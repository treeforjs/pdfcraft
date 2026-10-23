//! Optimize PDF ▸ Advanced optimization (Acrobat's PDF Optimizer): Images, Fonts, Discard
//! Objects, Discard User Data and Clean Up panels, and Audit space usage. The result is saved as a copy, like Reduce File Size.
//!
//! The optimization (this dialog's, or Reduce File Size's fixed choices) runs on a worker thread
//! and shows its progress in a notice with a bar and a Cancel button; the copy is saved once it
//! is done.

use std::sync::{Arc, Mutex};

use egui::{Align, Layout};
use pdfcraft_engine::optimize::{
    Compression, EmbeddedFont, ImageSettings, MonoCompression, MonoSettings, QUALITIES, Settings, SpaceAudit, SpaceCategory, UnembedBlock,
};
use pdfcraft_engine::optimizer::{OptimizeStage, Optimized};
use pdfcraft_engine::{DocId, EditError, Hidden, OptimizeReport};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptimizeTab {
    Images,
    Fonts,
    DiscardObjects,
    DiscardUserData,
    Transparency,
    CleanUp,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OptimizeDraft {
    pub tab: OptimizeTab,
    pub settings: Settings,
    /// Remove Hidden Information categories to discard.
    pub discard: Vec<Hidden>,
    /// "Audit space usage…" was pressed.
    pub audit: bool,
    /// Fonts panel: the active document's embedded fonts, listed when the dialog opens.
    pub fonts: Vec<EmbeddedFont>,
}

impl Default for OptimizeDraft {
    fn default() -> Self {
        Self { tab: OptimizeTab::Images, settings: Settings::default(), discard: Vec::new(), audit: false, fonts: Vec::new() }
    }
}

/// Progress of a background optimization: the stage, the result once finished, and a cancel
/// request.
#[derive(Default)]
pub struct OptimizeProgress {
    pub stage: OptimizeStage,
    pub result: Option<Result<Optimized, EditError>>,
    pub cancel: bool,
}

/// Which command started an optimization: they save under different names, and only the
/// Optimizer's notice lists what it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptimizeKind {
    /// Reduce File Size (Acrobat's defaults).
    Reduce,
    /// Optimize PDF ▸ Advanced optimization (the dialog's choices).
    Advanced,
}

impl OptimizeKind {
    /// The suffix of the saved copy's name ("notes (reduced).pdf").
    fn suffix(self) -> &'static str {
        match self {
            OptimizeKind::Reduce => "reduced",
            OptimizeKind::Advanced => "optimized",
        }
    }
}

pub struct OptimizeRun {
    pub doc: DocId,
    pub kind: OptimizeKind,
    pub progress: Arc<Mutex<OptimizeProgress>>,
}

/// The notice's text for a stage.
fn stage_label(stage: OptimizeStage) -> String {
    match stage {
        OptimizeStage::Discarding => tl!("Optimizing… removing user data").to_string(),
        OptimizeStage::Flattening => tl!("Optimizing… flattening transparency").to_string(),
        OptimizeStage::Images { done, total } => crate::i18n::fmt(
            tl!("Optimizing… image {d} of {t}"),
            &[("d", &(done + 1).min(total.max(1)).to_string()), ("t", &total.max(1).to_string())],
        ),
        OptimizeStage::CleaningUp => tl!("Optimizing… cleaning up").to_string(),
        OptimizeStage::Merging => tl!("Optimizing… merging identical objects").to_string(),
        OptimizeStage::Writing => tl!("Optimizing… writing the copy").to_string(),
    }
}

/// What the saved notice adds after the size: images optimized, fonts unembedded and items
/// discarded.
fn summary(r: &OptimizeReport) -> String {
    let o = &r.optimize;
    let mut parts = Vec::new();
    if o.images_resampled + o.images_recompressed > 0 {
        parts.push(format!(
            "{} image{} optimized",
            o.images_resampled + o.images_recompressed,
            if o.images_resampled + o.images_recompressed == 1 { "" } else { "s" }
        ));
    }
    if r.flattened > 0 {
        parts.push(format!("transparency flattened on {} page{}", r.flattened, if r.flattened == 1 { "" } else { "s" }));
    }
    if o.images_downgraded > 0 {
        parts.push(format!("{} image{} converted for the older PDF version", o.images_downgraded, if o.images_downgraded == 1 { "" } else { "s" }));
    }
    if o.fonts_unembedded > 0 {
        parts.push(format!("{} font{} unembedded", o.fonts_unembedded, if o.fonts_unembedded == 1 { "" } else { "s" }));
    }
    let discarded: usize = r.discarded.iter().map(|(_, n)| n).sum();
    if discarded > 0 {
        parts.push(format!("{discarded} item{} discarded", if discarded == 1 { "" } else { "s" }));
    }
    if parts.is_empty() { String::new() } else { format!("; {}", parts.join(", ")) }
}

fn image_row(ui: &mut egui::Ui, id: &str, title: &str, s: &mut ImageSettings) {
    ui.label(egui::RichText::new(tl!(title)).font(theme::semibold(13.0)));
    ui.horizontal(|ui| {
        ui.checkbox(&mut s.downsample, tl!("Bicubic downsampling to"));
        ui.add_enabled(s.downsample, egui::DragValue::new(&mut s.target_ppi).range(9.0..=2400.0).suffix(" ppi"));
        ui.label(tl!("for images above"));
        ui.add_enabled(s.downsample, egui::DragValue::new(&mut s.above_ppi).range(9.0..=2400.0).suffix(" ppi"));
    });
    s.above_ppi = s.above_ppi.max(s.target_ppi);
    ui.horizontal(|ui| {
        ui.label(tl!("Compression"));
        let label = match s.compression {
            Compression::Jpeg(_) => "JPEG",
            Compression::Flate => tl!("ZIP"),
            Compression::Retain => tl!("Retain existing"),
        };
        egui::ComboBox::from_id_salt((id, "compression")).selected_text(label).show_ui(ui, |ui| {
            let q = match s.compression {
                Compression::Jpeg(q) => q,
                _ => 60,
            };
            ui.selectable_value(&mut s.compression, Compression::Jpeg(q), "JPEG");
            ui.selectable_value(&mut s.compression, Compression::Flate, tl!("ZIP"));
            ui.selectable_value(&mut s.compression, Compression::Retain, tl!("Retain existing"));
        });
        if let Compression::Jpeg(q) = &mut s.compression {
            ui.label(tl!("Quality"));
            let name = QUALITIES.iter().min_by_key(|(_, v)| (*v as i32 - *q as i32).abs()).map_or(tl!("Medium"), |(n, _)| tl!(n));
            egui::ComboBox::from_id_salt((id, "quality")).selected_text(name).show_ui(ui, |ui| {
                for (n, v) in QUALITIES {
                    ui.selectable_value(q, v, tl!(n));
                }
            });
        }
    });
    ui.add_space(8.0);
}

/// Monochrome images: downsampling (area average) and CCITT Group 4, ZIP or the existing
/// compression.
fn mono_row(ui: &mut egui::Ui, s: &mut MonoSettings) {
    ui.label(egui::RichText::new(tl!("Monochrome Images")).font(theme::semibold(13.0)));
    ui.horizontal(|ui| {
        ui.checkbox(&mut s.downsample, tl!("Downsample to"));
        ui.add_enabled(s.downsample, egui::DragValue::new(&mut s.target_ppi).range(9.0..=2400.0).suffix(" ppi"));
        ui.label(tl!("for images above"));
        ui.add_enabled(s.downsample, egui::DragValue::new(&mut s.above_ppi).range(9.0..=2400.0).suffix(" ppi"));
    });
    s.above_ppi = s.above_ppi.max(s.target_ppi);
    ui.horizontal(|ui| {
        ui.label(tl!("Compression"));
        // "CCITT Group 4" is a standard name and stays untranslated, like "JPEG".
        // "JBIG2" is a standard name too.
        let label = match s.compression {
            MonoCompression::CcittG4 => "CCITT Group 4",
            MonoCompression::Jbig2 => "JBIG2",
            MonoCompression::Flate => tl!("ZIP"),
            MonoCompression::Retain => tl!("Retain existing"),
        };
        egui::ComboBox::from_id_salt(("mono", "compression")).selected_text(label).show_ui(ui, |ui| {
            ui.selectable_value(&mut s.compression, MonoCompression::CcittG4, "CCITT Group 4");
            ui.selectable_value(&mut s.compression, MonoCompression::Jbig2, "JBIG2");
            ui.selectable_value(&mut s.compression, MonoCompression::Flate, tl!("ZIP"));
            ui.selectable_value(&mut s.compression, MonoCompression::Retain, tl!("Retain existing"));
        });
    });
    ui.add_space(8.0);
}

/// Clean Up's object compression: the whole file (object streams and unencoded streams
/// compressed), the document structure only (object streams), or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObjectCompression {
    EntireFile,
    StructureOnly,
    Remove,
}

impl ObjectCompression {
    fn of(s: &Settings) -> Self {
        match (s.compress_structure, s.flate_unencoded) {
            (true, true) => ObjectCompression::EntireFile,
            (true, false) => ObjectCompression::StructureOnly,
            (false, _) => ObjectCompression::Remove,
        }
    }

    fn apply(self, s: &mut Settings) {
        let (structure, unencoded) = match self {
            ObjectCompression::EntireFile => (true, true),
            ObjectCompression::StructureOnly => (true, false),
            ObjectCompression::Remove => (false, false),
        };
        s.compress_structure = structure;
        s.flate_unencoded = unencoded;
    }

    fn label(self) -> &'static str {
        match self {
            ObjectCompression::EntireFile => tl!("Compress entire file"),
            ObjectCompression::StructureOnly => tl!("Compress document structure"),
            ObjectCompression::Remove => tl!("Remove compression"),
        }
    }
}

/// Transparency: flatten it in the copy, at a resolution preset and raster/vector balance.
fn transparency_panel(ui: &mut egui::Ui, s: &mut Settings, t: &Tokens) {
    let forced = s.compatibility.is_some_and(|v| v < (1, 4));
    let mut on = s.transparency.is_some() || forced;
    if ui.add_enabled(!forced, egui::Checkbox::new(&mut on, tl!("Flatten transparency"))).changed() {
        s.transparency = on.then(pdfcraft_engine::optimize::Transparency::default);
    }
    if forced {
        ui.label(egui::RichText::new(tl!("PDF 1.3 has no transparency: it is always flattened.")).small().color(t.text_muted));
    }
    let mut current = s.transparency.unwrap_or_default();
    let before = current;
    ui.add_enabled_ui(on, |ui| {
        ui.horizontal(|ui| {
            let l = ui.label(tl!("Resolution"));
            let name = |ppi: f64| {
                if ppi < 200.0 {
                    tl!("Low (150 ppi)")
                } else if ppi < 450.0 {
                    tl!("Medium (300 ppi)")
                } else {
                    tl!("High (600 ppi)")
                }
            };
            egui::ComboBox::from_id_salt("optimize-flatten-resolution")
                .selected_text(name(current.resolution))
                .show_ui(ui, |ui| {
                    for ppi in [150.0, 300.0, 600.0] {
                        ui.selectable_value(&mut current.resolution, ppi, name(ppi));
                    }
                })
                .response
                .labelled_by(l.id);
        });
        ui.horizontal(|ui| {
            let l = ui.label(tl!("Raster/vector balance"));
            ui.add(egui::Slider::new(&mut current.balance, 0..=100)).labelled_by(l.id);
        });
        ui.label(
            egui::RichText::new(tl!("Only the areas whose look depends on transparency become images; text and vectors elsewhere stay as they are."))
                .small()
                .color(t.text_muted),
        );
    });
    if current != before {
        s.transparency = Some(current);
    }
}

fn discard_box(ui: &mut egui::Ui, list: &mut Vec<Hidden>, h: Hidden, label: &str) {
    let mut on = list.contains(&h);
    if ui.checkbox(&mut on, tl!(label)).changed() {
        if on {
            list.push(h);
        } else {
            list.retain(|x| *x != h);
        }
    }
}

/// Why fonts named `name` can't be unembedded: unembedding goes by name, so one blocked font
/// blocks every font of that name.
fn name_block(fonts: &[EmbeddedFont], name: &str) -> Option<UnembedBlock> {
    fonts.iter().filter(|f| f.name == name).find_map(|f| f.blocked)
}

/// Keep only chosen names that are still embedded and can be unembedded in `fonts`.
pub(crate) fn prune_unembed(fonts: &[EmbeddedFont], chosen: &mut Vec<String>) {
    chosen.retain(|n| fonts.iter().any(|f| &f.name == n) && name_block(fonts, n).is_none());
}

/// Fonts: every embedded font, with a check box to unembed it or the reason it can't be.
fn fonts_panel(ui: &mut egui::Ui, fonts: &[EmbeddedFont], chosen: &mut Vec<String>, t: &Tokens) {
    if fonts.is_empty() {
        ui.label(tl!("This document has no embedded fonts."));
        return;
    }
    ui.add(
        egui::Label::new(
            egui::RichText::new(tl!(
                "Unembedding removes a font's program; viewers then draw its text with an installed or substitute font. Fonts that would not keep their character codes and widths can't be unembedded."
            ))
            .small()
            .color(t.text_muted),
        )
        .wrap(),
    );
    ui.add_space(6.0);
    egui::ScrollArea::vertical().max_height(280.0).auto_shrink([false, true]).show(ui, |ui| {
        for (i, f) in fonts.iter().enumerate() {
            let block = name_block(fonts, &f.name);
            let mut on = chosen.contains(&f.name);
            let r = ui.push_id(i, |ui| ui.add_enabled(block.is_none(), egui::Checkbox::new(&mut on, &f.name))).inner;
            if r.changed() {
                if on {
                    chosen.push(f.name.clone());
                } else {
                    chosen.retain(|n| *n != f.name);
                }
            }
            let detail = crate::i18n::fmt(
                tl!("{kind} · {program} · {n} bytes"),
                &[("kind", &f.subtype), ("program", f.program), ("n", &f.program_bytes.to_string())],
            );
            let status = match block {
                None => tl!("Can be unembedded"),
                Some(b) => tl!(b.reason()),
            };
            ui.indent(("font", i), |ui| {
                ui.label(egui::RichText::new(detail).small().color(t.text_muted));
                ui.add(egui::Label::new(egui::RichText::new(status).small().color(if block.is_some() { t.text_muted } else { t.text })).wrap());
            });
            ui.add_space(4.0);
        }
    });
    ui.add_space(4.0);
    ui.label(crate::i18n::fmt(tl!("Fonts to unembed: {n}"), &[("n", &chosen.len().to_string())]));
}

/// Draw the dialog; returns (ok, cancel).
pub(crate) fn body(ui: &mut egui::Ui, d: &mut OptimizeDraft, t: &Tokens) -> (bool, bool) {
    ui.label(egui::RichText::new(tl!("PDF Optimizer")).font(theme::semibold(18.0)));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let l = ui.label(tl!("Make compatible with"));
        let name = |v: Option<(u8, u8)>| {
            v.map_or_else(|| tl!("Retain existing").to_string(), |v| format!("PDF {}", pdfcraft_engine::optimize::version_name(v)))
        };
        egui::ComboBox::from_id_salt("optimize-compatibility")
            .selected_text(name(d.settings.compatibility))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut d.settings.compatibility, None, name(None));
                for v in pdfcraft_engine::optimize::VERSIONS {
                    ui.selectable_value(&mut d.settings.compatibility, Some(v), name(Some(v)));
                }
            })
            .response
            .labelled_by(l.id);
    });
    if d.settings.compatibility.is_some_and(|v| v < (1, 5)) {
        ui.label(
            egui::RichText::new(tl!(
                "Older versions: JPEG 2000 and 16-bit images are converted, JBIG2 becomes CCITT for PDF 1.3, transparency is flattened for PDF 1.3, and there are no object streams."
            ))
            .small()
            .color(t.text_muted),
        );
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        for (tab, label) in [
            (OptimizeTab::Images, tl!("Images")),
            (OptimizeTab::Fonts, tl!("Fonts")),
            (OptimizeTab::DiscardObjects, tl!("Discard Objects")),
            (OptimizeTab::DiscardUserData, tl!("Discard User Data")),
            (OptimizeTab::Transparency, tl!("Transparency")),
            (OptimizeTab::CleanUp, tl!("Clean Up")),
        ] {
            if widgets::mode_tab(ui, label, d.tab == tab).clicked() {
                d.tab = tab;
            }
        }
    });
    ui.separator();
    ui.add_space(6.0);
    let s = &mut d.settings;
    match d.tab {
        OptimizeTab::Images => {
            image_row(ui, "color", "Color Images", &mut s.color);
            image_row(ui, "gray", "Grayscale Images", &mut s.gray);
            mono_row(ui, &mut s.mono);
            ui.label(
                egui::RichText::new(tl!("Each image is measured where pages draw it; an image is replaced only if the result is smaller."))
                    .small()
                    .color(t.text_muted),
            );
        }
        OptimizeTab::Fonts => fonts_panel(ui, &d.fonts, &mut s.unembed_fonts, t),
        OptimizeTab::DiscardObjects => {
            discard_box(ui, &mut d.discard, Hidden::LinksActionsScripts, "Discard all links, actions and JavaScript");
            ui.checkbox(&mut s.discard_alternate_images, tl!("Discard alternate images"));
            ui.checkbox(&mut s.discard_thumbnails, tl!("Discard embedded page thumbnails"));
            ui.checkbox(&mut s.discard_tags, tl!("Discard document tags"));
            ui.checkbox(&mut s.discard_print_settings, tl!("Discard embedded print settings"));
            discard_box(ui, &mut d.discard, Hidden::Bookmarks, "Discard bookmarks");
            discard_box(ui, &mut d.discard, Hidden::FormFields, "Flatten form fields");
            discard_box(ui, &mut d.discard, Hidden::HiddenLayers, "Discard hidden layer content");
        }
        OptimizeTab::DiscardUserData => {
            discard_box(ui, &mut d.discard, Hidden::Comments, "Discard all comments, forms and multimedia");
            discard_box(ui, &mut d.discard, Hidden::Metadata, "Discard document information and metadata");
            discard_box(ui, &mut d.discard, Hidden::Attachments, "Discard all object data (file attachments)");
            discard_box(ui, &mut d.discard, Hidden::PrivateData, "Discard private data of other applications");
            discard_box(ui, &mut d.discard, Hidden::HiddenText, "Discard hidden text");
        }
        OptimizeTab::Transparency => transparency_panel(ui, s, t),
        OptimizeTab::CleanUp => {
            let current = ObjectCompression::of(s);
            let mut chosen = current;
            ui.horizontal(|ui| {
                let l = ui.label(tl!("Object compression"));
                egui::ComboBox::from_id_salt("optimize-object-compression")
                    .selected_text(current.label())
                    .show_ui(ui, |ui| {
                        for c in [ObjectCompression::EntireFile, ObjectCompression::StructureOnly, ObjectCompression::Remove] {
                            ui.selectable_value(&mut chosen, c, c.label());
                        }
                    })
                    .response
                    .labelled_by(l.id);
            });
            if chosen != current {
                chosen.apply(s);
            }
            ui.checkbox(&mut s.flate_unencoded, tl!("Use Flate to encode streams that are not encoded"));
            ui.checkbox(&mut s.lzw_to_flate, tl!("In streams that use LZW encoding, use Flate instead"));
            ui.checkbox(&mut s.remove_invalid_links, tl!("Remove invalid links and bookmarks"));
            ui.checkbox(&mut s.remove_unreferenced_dests, tl!("Remove unreferenced named destinations"));
            ui.add_enabled(false, egui::Checkbox::new(&mut true, tl!("Remove unused objects and merge identical ones")));
        }
    }
    ui.add_space(12.0);
    let (mut ok, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        if widgets::pill_button(ui, tl!("Audit space usage…"), false).clicked() {
            d.audit = true;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("OK"), true).clicked() {
                ok = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                cancel = true;
            }
        });
    });
    (ok, cancel)
}

/// The audit's name for a category. "Patterns" also names the redaction search patterns
/// ("模式"); the audit's PDF graphics objects need their own key.
fn category_label(c: SpaceCategory) -> &'static str {
    if c == SpaceCategory::Patterns { tl!("Patterns (graphics objects)") } else { tl!(c.label()) }
}

fn percent(bytes: u64, total: u64) -> String {
    if total == 0 { "0.00%".into() } else { format!("{:.2}%", bytes as f64 * 100.0 / total as f64) }
}

/// Audit Space Usage: objects, bytes and share of the file per kind of content, how the document
/// overhead breaks down, and how the total relates to the file size. Returns whether OK was
/// pressed.
pub(crate) fn audit_body(ui: &mut egui::Ui, audit: Option<&SpaceAudit>, t: &Tokens) -> bool {
    ui.label(egui::RichText::new(tl!("Space Audit")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    match audit {
        None => {
            ui.label(tl!("Couldn't audit this document."));
        }
        Some(a) => {
            let right = |ui: &mut egui::Ui, text: egui::RichText| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(text));
            };
            egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("space-audit").num_columns(4).striped(true).spacing([20.0, 4.0]).show(ui, |ui| {
                    for h in [tl!("Description"), tl!("Objects"), tl!("Bytes"), tl!("Percentage")] {
                        ui.label(egui::RichText::new(h).color(t.text_muted));
                    }
                    ui.end_row();
                    for r in a.rows.iter().filter(|r| r.bytes > 0) {
                        ui.label(category_label(r.category));
                        let objects = if r.category == SpaceCategory::DocumentOverhead { String::new() } else { r.objects.to_string() };
                        right(ui, egui::RichText::new(objects));
                        right(ui, egui::RichText::new(r.bytes.to_string()));
                        right(ui, egui::RichText::new(percent(r.bytes, a.total_bytes)));
                        ui.end_row();
                        if r.category == SpaceCategory::DocumentOverhead {
                            for (label, bytes) in [
                                (tl!("Cross-reference"), a.cross_reference_bytes),
                                (tl!("Unclaimed objects"), a.unclaimed_object_bytes),
                                (tl!("Other overhead"), a.other_overhead_bytes),
                            ] {
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label(egui::RichText::new(label).color(t.text_muted));
                                });
                                ui.label("");
                                right(ui, egui::RichText::new(bytes.to_string()).color(t.text_muted));
                                right(ui, egui::RichText::new(percent(bytes, a.total_bytes)).color(t.text_muted));
                                ui.end_row();
                            }
                        }
                    }
                    let objects: usize = a.rows.iter().map(|r| r.objects).fold(0, usize::saturating_add);
                    ui.label(egui::RichText::new(tl!("Total")).strong());
                    right(ui, egui::RichText::new(objects.to_string()).strong());
                    right(ui, egui::RichText::new(a.total_bytes.to_string()).strong());
                    right(ui, egui::RichText::new("100.00%").strong());
                    ui.end_row();
                });
            });
            ui.add_space(8.0);
            ui.label(crate::i18n::fmt(tl!("File size: {n} bytes"), &[("n", &a.file_bytes.to_string())]));
            let note = |ui: &mut egui::Ui, text: String| {
                ui.add(egui::Label::new(egui::RichText::new(text).small().color(t.text_muted)).wrap());
            };
            if a.estimated_objects > 0 {
                note(
                    ui,
                    crate::i18n::fmt(
                        tl!("{n} objects with unsaved changes are estimated, not measured in the file."),
                        &[("n", &a.estimated_objects.to_string())],
                    ),
                );
            }
            if a.apportioned_bytes > 0 {
                note(
                    ui,
                    crate::i18n::fmt(
                        tl!("{n} bytes of compressed object streams are shared out among their objects by decoded size."),
                        &[("n", &a.apportioned_bytes.to_string())],
                    ),
                );
            }
        }
    }
    ui.add_space(12.0);
    let mut ok = false;
    ui.horizontal(|ui| ui.with_layout(Layout::right_to_left(Align::Center), |ui| ok = widgets::pill_button(ui, tl!("OK"), true).clicked()));
    ok
}

impl PdfCraftApp {
    /// Open the PDF Optimizer, listing the active document's embedded fonts for the Fonts panel.
    pub(crate) fn open_optimizer(&mut self) {
        let fonts = self.active_ids().and_then(|(_, id)| self.session.get(id)).map(|d| d.embedded_fonts()).unwrap_or_default();
        prune_unembed(&fonts, &mut self.optimize_draft.settings.unembed_fonts);
        self.optimize_draft.fonts = fonts;
        self.dialog = Some(crate::Dialog::Optimize);
    }

    /// Measure the active document for Audit space usage. `from_optimizer`: OK returns to the
    /// PDF Optimizer instead of closing.
    pub(crate) fn load_space_audit(&mut self, from_optimizer: bool) {
        self.space_audit = self.active_ids().and_then(|(_, id)| self.session.get(id)).and_then(|d| d.audit_space_detailed());
        self.audit_from_optimizer = from_optimizer;
    }

    /// Optimize PDF with the dialog's choices on a worker thread; the copy is saved when done.
    pub fn optimize_with_draft(&mut self) {
        let (settings, discard) = (self.optimize_draft.settings.clone(), self.optimize_draft.discard.clone());
        self.start_optimize(OptimizeKind::Advanced, &settings, &discard);
    }

    /// Run an optimization of the active document on a worker thread (inline in tests and on
    /// the web), showing its progress; the copy is saved when it is done.
    pub(crate) fn start_optimize(&mut self, kind: OptimizeKind, settings: &Settings, discard: &[Hidden]) {
        // What's typed in a form field is part of the document (#166).
        if !self.commit_form_typing() {
            return;
        }
        let Some((_, id)) = self.active_ids() else { return };
        if self.optimize_run.is_some() {
            self.notify_tr("Optimization is already running");
            return;
        }
        let job = match self.session.optimize_job(id, settings, discard) {
            Ok(job) => job,
            Err(e) => return self.save_optimized(id, kind.suffix(), Err(e)),
        };
        let progress = Arc::new(Mutex::new(OptimizeProgress::default()));
        let p = progress.clone();
        let work = move || {
            let result = job.run(|stage| {
                let Ok(mut s) = p.lock() else { return false };
                s.stage = stage;
                !s.cancel
            });
            if let Ok(mut s) = p.lock() {
                s.result = Some(result);
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if self.run_inline {
            work();
        } else if let Err(e) = std::thread::Builder::new().name("pdfcraft-optimize".into()).spawn(work) {
            // No worker: report the failure instead of waiting for a result that never comes.
            if let Ok(mut s) = progress.lock() {
                s.result = Some(Err(EditError::Optimize(e.to_string())));
            }
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.optimize_run = Some(OptimizeRun { doc: id, kind, progress });
        self.poll_optimize();
    }

    /// Stop a running optimization (nothing is saved).
    pub fn cancel_optimize(&mut self) {
        if let Some(r) = &self.optimize_run
            && let Ok(mut s) = r.progress.lock()
        {
            s.cancel = true;
        }
    }

    /// Show progress; save the copy once the worker is done.
    pub(crate) fn poll_optimize(&mut self) {
        let Some(run) = self.optimize_run.as_ref() else { return };
        let (doc, kind, progress) = (run.doc, run.kind, run.progress.clone());
        let Ok(mut s) = progress.lock() else {
            // The worker panicked while holding the lock: it will never report back.
            self.optimize_run = None;
            self.progress_notice = None;
            self.notify_tr("Couldn't optimize the file");
            return;
        };
        let Some(result) = s.result.take() else {
            let cancelling = s.cancel;
            let stage = s.stage;
            drop(s);
            let label = if cancelling { tl!("Cancelling…").to_string() } else { stage_label(stage) };
            self.progress_notice = Some(crate::widgets::ProgressNotice { label, fraction: stage.fraction(), cancellable: !cancelling });
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            return;
        };
        drop(s);
        self.optimize_run = None;
        self.progress_notice = None;
        match result {
            Err(EditError::Cancelled) => self.notify_tr("Optimization cancelled"),
            result => {
                let result = result.map(|(b, r)| {
                    let detail = if kind == OptimizeKind::Advanced { summary(&r) } else { String::new() };
                    (b, detail)
                });
                self.save_optimized(doc, kind.suffix(), result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn the_progress_card_shows_the_stage_and_cancels() {
        let progress = Arc::new(Mutex::new(OptimizeProgress { stage: OptimizeStage::Images { done: 3, total: 10 }, ..Default::default() }));
        let p = progress.clone();
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(move |_cc| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.open_bytes("notes.txt", None, b"hello".to_vec()).unwrap();
            let (_, doc) = app.active_ids().unwrap();
            app.optimize_run = Some(OptimizeRun { doc, kind: OptimizeKind::Advanced, progress: p });
            app.notify("Saved notes.pdf");
            app
        });
        // 1.5 s (kittest steps a quarter second): the bar has eased in, the toast still shows.
        h.run_steps(6);
        if let Ok(path) = std::env::var("PDFCRAFT_OPTIMIZE_PROGRESS_SHOT") {
            h.render().unwrap().save(path).unwrap();
        }
        h.get_by_label("Optimizing… image 4 of 10");
        let shown = h.state().progress_notice.clone().unwrap();
        assert!((shown.fraction - 0.275).abs() < 1e-4, "{shown:?}");
        assert!(shown.cancellable);

        h.get_by_label("Cancel").click();
        h.run_steps(2);
        assert!(progress.lock().unwrap().cancel, "the worker is asked to stop");
        h.get_by_label("Cancelling…");
        assert!(h.query_by_label("Cancel").is_none(), "Cancel is not offered twice");

        // The worker stops and reports back.
        progress.lock().unwrap().result = Some(Err(EditError::Cancelled));
        h.run_steps(2);
        assert!(h.state().optimize_run.is_none() && h.state().progress_notice.is_none());
        assert_eq!(h.state().toast.as_ref().map(|t| t.0.as_str()), Some("Optimization cancelled"));
    }

    #[test]
    fn a_second_run_is_refused_while_one_is_going() {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("notes.txt", None, b"hello".to_vec()).unwrap();
        let (_, doc) = app.active_ids().unwrap();
        app.optimize_run = Some(OptimizeRun { doc, kind: OptimizeKind::Reduce, progress: Arc::default() });
        app.optimize_with_draft();
        assert_eq!(app.toast.as_ref().map(|t| t.0.as_str()), Some("Optimization is already running"));
    }
}
