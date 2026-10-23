//! Print production dialogs: the Ink Manager (spot inks aliased or converted to process for the
//! session, or made permanent as one undo step), Convert Colors and Add Printer Marks. Each
//! builds the same engine settings as its automation tool (`print_ink_manager_*`,
//! `print_convert_colors`, `print_add_printer_marks`).

use egui::{Align, CornerRadius, Layout, Sense, Stroke, StrokeKind, vec2};
use pdfcraft_engine::preflight::{InkManager, InkRoute};
use pdfcraft_engine::{ColorConversion, ColorDestination, ColorObjects, ColorSources, DocId, Edit, MarkStyle, PrinterMarks, RenderingIntent};
use serde_json::{Value, json};

use crate::output_preview_ui::{Key, Task};
use crate::theme::{self, Tokens};
use crate::{Dialog, PdfCraftApp, widgets};

/// Pages the Ink Manager lists inks from at most (the separations list's cap).
const MAX_INK_PAGES: usize = pdfcraft_engine::pdfa::separations::MAX_LIST_PAGES;

/// One ink in the Ink Manager.
#[derive(Clone, Debug, PartialEq)]
pub struct InkRow {
    pub name: String,
    pub process: bool,
    pub preview: [f64; 3],
}

/// The Ink Manager dialog: the document's inks and the settings being edited.
#[derive(Default)]
pub struct InkManagerDraft {
    pub doc: Option<DocId>,
    pub inks: Vec<InkRow>,
    pub settings: InkManager,
    /// Only the first [`MAX_INK_PAGES`] pages were listed.
    pub pages_cut: bool,
    pub error: Option<String>,
    listing: Option<Task<pdfcraft_engine::preflight::InkList>>,
}

impl InkManagerDraft {
    pub fn listing(&self) -> bool {
        self.listing.is_some()
    }

    fn spots(&self) -> impl Iterator<Item = &InkRow> {
        self.inks.iter().filter(|i| !i.process)
    }
}

/// Which pages a dialog acts on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageChoice {
    #[default]
    All,
    Current,
}

/// Convert Colors.
#[derive(Clone, Debug, PartialEq)]
pub struct ConvertDraft {
    /// `DeviceCMYK`, `DeviceGray` or `DeviceRGB` (through the working space when it isn't the
    /// factory one).
    pub destination: &'static str,
    pub sources: ColorSources,
    pub objects: ColorObjects,
    pub intent: RenderingIntent,
    pub preserve_black: bool,
    pub convert_spots: bool,
    pub pages: PageChoice,
}

impl Default for ConvertDraft {
    fn default() -> Self {
        ConvertDraft {
            destination: "DeviceCMYK",
            sources: ColorSources::ALL,
            objects: ColorObjects { text: true, vector: true, images: true },
            intent: RenderingIntent::default(),
            preserve_black: true,
            convert_spots: false,
            pages: PageChoice::All,
        }
    }
}

/// Add Printer Marks.
#[derive(Clone, Debug, PartialEq)]
pub struct PrinterMarksDraft {
    pub settings: PrinterMarks,
    pub pages: PageChoice,
}

impl Default for PrinterMarksDraft {
    fn default() -> Self {
        PrinterMarksDraft { settings: PrinterMarks::default(), pages: PageChoice::All }
    }
}

impl PdfCraftApp {
    fn dialog_pages(&self, choice: PageChoice) -> Vec<usize> {
        let Some((vi, id)) = self.active_ids() else { return Vec::new() };
        match choice {
            PageChoice::All => (0..self.session.get(id).map_or(0, |d| d.info.pages.len())).collect(),
            PageChoice::Current => vec![self.views.get(vi).map_or(0, |v| v.current)],
        }
    }

    /// The view shows a changed document (an edit made through the session).
    fn prepress_changed(&mut self, id: DocId) {
        if let Some(info) = self.session.get(id).map(|d| d.info.clone()) {
            for v in self.views.iter_mut().filter(|v| v.id == id) {
                v.document_changed(&info);
            }
        }
    }

    /// Print production ▸ Ink manager: list the document's inks off the frame, then edit.
    pub fn open_ink_manager(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let pages: Vec<usize> = (0..doc.info.pages.len().min(MAX_INK_PAGES)).collect();
        let pages_cut = doc.info.pages.len() > MAX_INK_PAGES;
        let settings = doc.ink_manager().clone();
        let generation = doc.edit_generation();
        let listing = match doc.press_snapshot() {
            Ok(snapshot) => {
                let ctx = (!self.run_inline).then(|| self.ctx.clone()).flatten();
                Some(Task::start(Key::document(id, generation), self.run_inline, ctx, move || {
                    snapshot.document_inks(&pages).map_err(|e| e.to_string())
                }))
            }
            Err(e) => {
                self.notify_error(e);
                return;
            }
        };
        self.ink_manager = InkManagerDraft { doc: Some(id), inks: Vec::new(), settings, pages_cut, error: None, listing };
        self.poll_ink_manager();
        self.dialog = Some(Dialog::InkManager);
    }

    fn poll_ink_manager(&mut self) {
        let d = &mut self.ink_manager;
        let Some(task) = &d.listing else { return };
        let Some(done) = task.take() else { return };
        d.listing = None;
        match done {
            Ok(list) => {
                d.inks = list.inks.iter().map(|u| InkRow { name: u.info.name.clone(), process: u.info.process, preview: u.info.preview }).collect();
                d.error = None;
            }
            Err(e) => d.error = Some(e),
        }
    }

    /// Keep the Ink Manager's settings for this session (the previews follow them).
    pub fn ink_manager_ok(&mut self) -> bool {
        let Some(id) = self.ink_manager.doc else { return false };
        let s = self.ink_manager.settings.clone();
        if let Err(e) = s.validate() {
            self.ink_manager.error = Some(e);
            return false;
        }
        match self.session.set_ink_manager(id, s) {
            Ok(()) => true,
            Err(e) => {
                self.ink_manager.error = Some(e.to_string());
                false
            }
        }
    }

    /// Make the Ink Manager's settings permanent in the document (one undo step).
    pub fn ink_manager_apply(&mut self) -> bool {
        if !self.ink_manager_ok() {
            return false;
        }
        let Some(id) = self.ink_manager.doc else { return false };
        if !self.ink_manager.settings.moves_inks() {
            self.ink_manager.error = Some(tl!("Set an alias or convert a spot ink to process first.").to_string());
            return false;
        }
        match self.session.apply_ink_manager(id) {
            Ok(report) if report.changed() => {
                self.prepress_changed(id);
                self.notify(tl!("Inks changed in the document"));
                true
            }
            Ok(_) => {
                self.notify(tl!("No ink needed changing"));
                true
            }
            Err(e) => {
                self.ink_manager.error = Some(e.to_string());
                false
            }
        }
    }

    /// Convert Colors with the dialog's settings. Returns whether the dialog can close.
    pub fn convert_colors_now(&mut self) -> bool {
        let Some((_, id)) = self.active_ids() else { return false };
        let d = self.convert_draft.clone();
        let pages = self.dialog_pages(d.pages);
        let colour = self.session.color_management();
        let mut settings = ColorConversion {
            sources: d.sources,
            objects: d.objects,
            intent: d.intent,
            preserve_black: d.preserve_black,
            convert_spots: d.convert_spots,
            ..Default::default()
        };
        let family = match d.destination {
            "DeviceGray" => {
                settings.destination = ColorDestination::DeviceGray;
                pdfcraft_engine::color_settings::ProfileFamily::Gray
            }
            "DeviceRGB" => {
                settings.destination = ColorDestination::DeviceRgb;
                pdfcraft_engine::color_settings::ProfileFamily::Rgb
            }
            _ => {
                settings.destination = ColorDestination::DeviceCmyk;
                pdfcraft_engine::color_settings::ProfileFamily::Cmyk
            }
        };
        // As print_convert_colors: a device destination goes through its working space when that
        // isn't the factory one.
        if let Some((bytes, _)) = colour.working_destination(family) {
            settings.destination = ColorDestination::Icc(bytes);
            settings.device_output = true;
        }
        if let Err(e) = settings.validate() {
            self.notify_error(e);
            return false;
        }
        match self.session.convert_colors(id, pages, settings) {
            Ok(report) if report.changed() => {
                self.prepress_changed(id);
                let n = report.colors.saturating_add(report.images).saturating_add(report.inline_images).saturating_add(report.shadings);
                self.notify(crate::i18n::fmt(tl!("Colors converted: {n}"), &[("n", &n.to_string())]));
                true
            }
            Ok(_) => {
                self.notify(tl!("Nothing to convert"));
                true
            }
            Err(e) => {
                self.notify_error(e);
                false
            }
        }
    }

    /// Add Printer Marks with the dialog's settings.
    pub fn printer_marks_now(&mut self) -> bool {
        let Some((_, id)) = self.active_ids() else { return false };
        let mut settings = self.printer_marks_draft.settings.clone();
        // The page information line takes one line of at most 256 characters.
        let name = self.session.get(id).map(|d| d.name.clone()).unwrap_or_default();
        settings.file_name = name.chars().filter(|c| !c.is_control()).take(256).collect();
        if !(settings.trim || settings.bleed || settings.registration || settings.color_bars || settings.page_info) {
            self.notify(tl!("Choose at least one mark."));
            return false;
        }
        if let Err(e) = settings.validate() {
            self.notify_error(e);
            return false;
        }
        let pages = self.dialog_pages(self.printer_marks_draft.pages);
        if let Err(e) = self.session.printer_marks_preview(id, &pages, &settings) {
            self.notify_error(e);
            return false;
        }
        self.apply_edit(Edit::AddPrinterMarks { pages, settings })
    }

    /// `ui.state`'s `ink_manager` (while the dialog is open).
    pub fn ink_manager_state(&self) -> Value {
        let d = &self.ink_manager;
        json!({
            "listing": d.listing(),
            "inks": d.inks.iter().map(|i| {
                let route = match d.settings.route(&i.name) {
                    InkRoute::Own => "own".to_string(),
                    InkRoute::Ink(to) => format!("alias:{to}"),
                    InkRoute::Process => "process".to_string(),
                };
                json!({ "name": i.name, "process": i.process, "route": route })
            }).collect::<Vec<_>>(),
            "all_to_process": d.settings.all_to_process,
            "use_lab": d.settings.use_lab,
            "pages_cut": d.pages_cut,
            "error": d.error,
        })
    }
}

fn header(ui: &mut egui::Ui, title: &str) {
    ui.label(egui::RichText::new(title).font(theme::semibold(18.0)));
    ui.add_space(6.0);
}

fn pages_choice(ui: &mut egui::Ui, choice: &mut PageChoice) {
    ui.horizontal(|ui| {
        ui.label(tl!("Pages:"));
        ui.radio_value(choice, PageChoice::All, tl!("All pages"));
        ui.radio_value(choice, PageChoice::Current, tl!("Current page"));
    });
}

/// OK / Cancel at the foot of a dialog: (ok, cancel).
fn buttons(ui: &mut egui::Ui, ok: &str, enabled: bool) -> (bool, bool) {
    let mut out = (false, false);
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add_enabled_ui(enabled, |ui| widgets::pill_button(ui, ok, true)).inner.clicked() {
                out.0 = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                out.1 = true;
            }
        });
    });
    out
}

/// The Ink Manager. Returns whether the dialog closes.
pub(crate) fn ink_manager_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    app.poll_ink_manager();
    ui.set_width(600.0);
    header(ui, tl!("Ink manager"));
    if app.ink_manager.listing() {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(egui::RichText::new(tl!("Analyzing…")).color(t.text_muted));
        });
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
        let (_, cancel) = buttons(ui, tl!("OK"), false);
        return cancel;
    }
    let d = &mut app.ink_manager;
    if d.pages_cut {
        let n = MAX_INK_PAGES.to_string();
        ui.label(egui::RichText::new(crate::i18n::fmt(tl!("Inks of the first {n} pages."), &[("n", &n)])).small().color(t.text_faint));
    }
    let names: Vec<String> = d.inks.iter().map(|i| i.name.clone()).collect();
    let spots: Vec<InkRow> = d.spots().cloned().collect();
    if spots.is_empty() {
        ui.label(egui::RichText::new(tl!("This document uses only the process inks.")).color(t.text_muted));
    }
    let row_h = 30.0;
    egui::ScrollArea::vertical().id_salt("ink-manager").max_height(row_h * 11.0).auto_shrink([false, true]).show_rows(
        ui,
        row_h,
        d.inks.len(),
        |ui, range| {
            for ink in d.inks.get(range).unwrap_or_default().to_vec() {
                ui.horizontal(|ui| {
                    ui.set_height(row_h - 4.0);
                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    ui.painter().rect_filled(r, CornerRadius::same(3), crate::output_preview_ui::swatch(ink.preview));
                    ui.painter().rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, t.border), StrokeKind::Inside);
                    let shown = if ink.process { tl!(ink.name.as_str()).to_string() } else { ink.name.clone() };
                    ui.add_sized([180.0, row_h - 6.0], egui::Label::new(shown).truncate());
                    if ink.process {
                        ui.label(egui::RichText::new(tl!("Process")).small().color(t.text_faint));
                        return;
                    }
                    let mut process = d.settings.all_to_process || d.settings.to_process.contains(&ink.name);
                    let resp = ui.add_enabled(!d.settings.all_to_process, egui::Checkbox::new(&mut process, tl!("To process")));
                    if resp.changed() {
                        if process {
                            d.settings.to_process.insert(ink.name.clone());
                        } else {
                            d.settings.to_process.remove(&ink.name);
                        }
                    }
                    let alias = d.settings.aliases.get(&ink.name).cloned();
                    let none = tl!("No alias").to_string();
                    egui::ComboBox::from_id_salt(("ink-alias", &ink.name))
                        .selected_text(alias.clone().unwrap_or_else(|| none.clone()))
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(alias.is_none(), &none).clicked() {
                                d.settings.aliases.remove(&ink.name);
                            }
                            for other in names.iter().filter(|n| **n != ink.name) {
                                if ui.selectable_label(alias.as_deref() == Some(other.as_str()), other).clicked() {
                                    d.settings.aliases.insert(ink.name.clone(), other.clone());
                                }
                            }
                        });
                });
            }
        },
    );
    ui.add_space(4.0);
    ui.checkbox(&mut d.settings.all_to_process, tl!("All spots to process"));
    ui.checkbox(&mut d.settings.use_lab, tl!("Use Lab values for spot colors"));
    ui.label(egui::RichText::new(tl!("OK keeps these settings for this session; Apply to document changes the file.")).small().color(t.text_faint));
    if let Some(e) = &d.error {
        ui.label(egui::RichText::new(e.clone()).color(egui::Color32::from_rgb(0xC8, 0x3A, 0x3A)));
    }
    let mut close = false;
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if widgets::pill_button(ui, tl!("Reset"), false).clicked() {
            app.ink_manager.settings = InkManager::default();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("OK"), true).clicked() && app.ink_manager_ok() {
                close = true;
            }
            let editable = app.ink_manager.doc.and_then(|id| app.session.get(id)).is_some_and(|d| d.allows_modification());
            if ui.add_enabled_ui(editable, |ui| widgets::pill_button(ui, tl!("Apply to document"), false)).inner.clicked() && app.ink_manager_apply()
            {
                close = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                close = true;
            }
        });
    });
    close
}

/// Convert Colors. Returns whether the dialog closes.
pub(crate) fn convert_colors_body(ui: &mut egui::Ui, app: &mut PdfCraftApp) -> bool {
    header(ui, tl!("Convert colors"));
    let d = &mut app.convert_draft;
    egui::Grid::new("convert-colors").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(tl!("Convert to:"));
        egui::ComboBox::from_id_salt("convert-destination").selected_text(d.destination).width(160.0).show_ui(ui, |ui| {
            for v in ["DeviceCMYK", "DeviceGray", "DeviceRGB"] {
                ui.selectable_value(&mut d.destination, v, v);
            }
        });
        ui.end_row();
        ui.label(tl!("Rendering intent:"));
        let intents = [
            (RenderingIntent::Perceptual, tl!("Perceptual")),
            (RenderingIntent::RelativeColorimetric, tl!("Relative colorimetric")),
            (RenderingIntent::Saturation, tl!("Saturation")),
            (RenderingIntent::AbsoluteColorimetric, tl!("Absolute colorimetric")),
        ];
        let shown = intents.iter().find(|(v, _)| *v == d.intent).map_or("", |(_, l)| *l);
        egui::ComboBox::from_id_salt("convert-intent").selected_text(shown).width(160.0).show_ui(ui, |ui| {
            for (v, l) in intents {
                ui.selectable_value(&mut d.intent, v, l);
            }
        });
        ui.end_row();
    });
    widgets::section_title(ui, tl!("Colors to convert"));
    ui.horizontal_wrapped(|ui| {
        let s = &mut d.sources;
        ui.checkbox(&mut s.device_gray, "DeviceGray");
        ui.checkbox(&mut s.device_rgb, "DeviceRGB");
        ui.checkbox(&mut s.device_cmyk, "DeviceCMYK");
        ui.checkbox(&mut s.icc_based, "ICCBased");
        ui.checkbox(&mut s.cal_gray, "CalGray");
        ui.checkbox(&mut s.cal_rgb, "CalRGB");
        ui.checkbox(&mut s.lab, "Lab");
    });
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut d.objects.text, tl!("Text"));
        ui.checkbox(&mut d.objects.vector, tl!("Vector objects"));
        ui.checkbox(&mut d.objects.images, tl!("Images"));
    });
    ui.checkbox(&mut d.preserve_black, tl!("Preserve black"));
    ui.checkbox(&mut d.convert_spots, tl!("Convert spot colors to process"));
    pages_choice(ui, &mut d.pages);
    let any = (d.sources != ColorSources::NONE || d.convert_spots) && (d.objects.text || d.objects.vector || d.objects.images);
    let editable = app.active_ids().and_then(|(_, id)| app.session.get(id)).is_some_and(|d| d.allows_modification());
    let (ok, cancel) = buttons(ui, tl!("Convert"), any && editable);
    if ok {
        return app.convert_colors_now();
    }
    cancel
}

/// Add Printer Marks. Returns whether the dialog closes.
pub(crate) fn printer_marks_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    header(ui, tl!("Add printer marks"));
    let d = &mut app.printer_marks_draft;
    let s = &mut d.settings;
    ui.columns(2, |cols| {
        let ui = &mut cols[0];
        ui.checkbox(&mut s.trim, tl!("Trim marks"));
        ui.checkbox(&mut s.bleed, tl!("Bleed marks"));
        ui.checkbox(&mut s.registration, tl!("Registration marks"));
        ui.checkbox(&mut s.color_bars, tl!("Color bars"));
        ui.checkbox(&mut s.page_info, tl!("Page information"));
        let ui = &mut cols[1];
        egui::Grid::new("marks-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label(tl!("Style:"));
            egui::ComboBox::from_id_salt("marks-style")
                .selected_text(if s.style == MarkStyle::Eastern { tl!("Eastern") } else { tl!("Western") })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.style, MarkStyle::Western, tl!("Western"));
                    ui.selectable_value(&mut s.style, MarkStyle::Eastern, tl!("Eastern"));
                });
            ui.end_row();
            ui.label(tl!("Line weight:"));
            ui.add(egui::DragValue::new(&mut s.line_weight).range(0.125..=2.0).speed(0.05).suffix(" pt"));
            ui.end_row();
            ui.label(tl!("Offset from trim:"));
            ui.add(egui::DragValue::new(&mut s.offset).range(0.0..=72.0).speed(0.5).suffix(" pt"));
            ui.end_row();
        });
    });
    pages_choice(ui, &mut d.pages);
    ui.label(egui::RichText::new(tl!("The page grows to make room for the marks.")).small().color(t.text_faint));
    let editable = app.active_ids().and_then(|(_, id)| app.session.get(id)).is_some_and(|d| d.allows_modification());
    let (ok, cancel) = buttons(ui, tl!("Add marks"), editable);
    if ok {
        return app.printer_marks_now();
    }
    cancel
}
