//! Scan & OCR ▸ Enhance scans: correct camera photos (find the page, straighten its perspective,
//! even out the lighting), straighten skewed scans, whiten the paper background and remove
//! specks, on the pages chosen. Each correction is its own undoable step, in that order; the
//! dialog says what changed. The pixel work is in `pdfcraft_optimize` (camera, background,
//! despeckle) and `pdfcraft_ocr::orient` (skew), reached through the session's scan methods that
//! the `scan_*` tools use too.

use egui::{Align, Layout};
use pdfcraft_engine::camera::CameraOptions;
use pdfcraft_engine::ocr::{BackgroundOptions, BackgroundStrength, DespeckleLevel, DespeckleOptions};

use crate::marks_ui::PageRange;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// The dialog's choices (kept between runs).
#[derive(Clone, Debug, PartialEq)]
pub struct EnhanceDraft {
    pub range: PageRange,
    /// Camera photos: find the page, correct its perspective and lighting.
    pub camera: bool,
    /// Straighten skewed pages.
    pub straighten: bool,
    /// Whiten the paper background (Off: leave it).
    pub background: BackgroundStrength,
    /// Remove specks (Off: leave them).
    pub despeckle: DespeckleLevel,
}

impl Default for EnhanceDraft {
    fn default() -> Self {
        EnhanceDraft {
            range: PageRange::default(),
            camera: false,
            straighten: true,
            background: BackgroundStrength::Medium,
            despeckle: DespeckleLevel::Low,
        }
    }
}

impl EnhanceDraft {
    fn any(&self) -> bool {
        self.camera || self.straighten || self.background != BackgroundStrength::Off || self.despeckle != DespeckleLevel::Off
    }
}

fn level_label(i: usize) -> &'static str {
    match i {
        1 => tl!("Low"),
        2 => tl!("Medium"),
        3 => tl!("High"),
        _ => tl!("Off"),
    }
}

pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> (bool, bool) {
    let count = app.active_ids().and_then(|(_, id)| app.session.get(id)).map_or(0, |d| d.info.pages.len());
    let d = &mut app.enhance_draft;
    ui.label(egui::RichText::new(tl!("Enhance Scans")).font(theme::semibold(18.0)));
    ui.add_space(4.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(tl!("Cleans up the images on scanned or photographed pages. Each correction can be undone."))
                .small()
                .color(t.text_faint),
        )
        .wrap(),
    );
    ui.add_space(8.0);
    egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.checkbox(&mut d.camera, tl!("Correct camera photos"))
            .on_hover_text(tl!("Finds the page in a photo, corrects its perspective and evens out the lighting"));
        ui.checkbox(&mut d.straighten, tl!("Straighten skewed pages")).on_hover_text(tl!("Turns pages scanned at a slant (up to 15°) straight"));
        egui::Grid::new("enhance-levels").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(tl!("Remove background"));
            let names = [BackgroundStrength::Off, BackgroundStrength::Low, BackgroundStrength::Medium, BackgroundStrength::High];
            let at = names.iter().position(|s| *s == d.background).unwrap_or(0);
            egui::ComboBox::from_id_salt("enhance-background").selected_text(level_label(at)).width(160.0).show_ui(ui, |ui| {
                for (i, s) in names.iter().enumerate() {
                    ui.selectable_value(&mut d.background, *s, level_label(i));
                }
            });
            ui.end_row();
            ui.label(tl!("Despeckle"));
            let names = [DespeckleLevel::Off, DespeckleLevel::Low, DespeckleLevel::Medium, DespeckleLevel::High];
            let at = names.iter().position(|s| *s == d.despeckle).unwrap_or(0);
            egui::ComboBox::from_id_salt("enhance-despeckle").selected_text(level_label(at)).width(160.0).show_ui(ui, |ui| {
                for (i, s) in names.iter().enumerate() {
                    ui.selectable_value(&mut d.despeckle, *s, level_label(i));
                }
            });
            ui.end_row();
        });
    });
    ui.add_space(8.0);
    ui.label(egui::RichText::new(tl!("Pages")).font(theme::semibold(12.5)));
    d.range.ui(ui, count);
    ui.add_space(12.0);
    let (mut go, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let ok = d.any() && !d.range.pages(count).is_empty();
        if ui.add_enabled_ui(ok, |ui| widgets::pill_button(ui, tl!("Enhance"), true)).inner.clicked() {
            go = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            cancel = true;
        }
    });
    (go, cancel)
}

impl PdfCraftApp {
    /// Run the corrections chosen in the Enhance Scans dialog on the active document and say
    /// what changed. Stops at the first correction that fails (those before it stay applied,
    /// each undoable).
    pub fn enhance_scans(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let count = self.session.get(id).map_or(0, |d| d.info.pages.len());
        let d = self.enhance_draft.clone();
        let pages = if d.range.all { Vec::new() } else { d.range.pages(count) };
        let mut done: Vec<String> = Vec::new();
        let fail = |app: &mut Self, done: &[String], e: String| {
            let mut msg = crate::i18n::fmt(tl!("Couldn't enhance the scans: {e}"), &[("e", &e)]);
            if !done.is_empty() {
                msg.push_str(&format!(" ({})", done.join(", ")));
            }
            app.notify(msg);
        };
        if d.camera {
            match self.session.correct_camera_images(id, &pages, &CameraOptions::default(), false) {
                Ok(found) => {
                    let n = found.iter().filter(|p| p.skipped.is_none()).count();
                    if n > 0 {
                        done.push(crate::i18n::fmt(tl!("{n} camera photos corrected"), &[("n", &n.to_string())]));
                    }
                }
                Err(e) => return fail(self, &done, e.to_string()),
            }
        }
        // Camera correction replaces pages, so later steps read the corrected ones.
        if d.straighten {
            match self.session.straighten_scans(id, &pages, 0.3, false) {
                Ok(found) => {
                    let n = found.iter().filter(|p| p.straightened).count();
                    if n > 0 {
                        done.push(crate::i18n::fmt(tl!("{n} pages straightened"), &[("n", &n.to_string())]));
                    }
                }
                Err(e) => return fail(self, &done, e.to_string()),
            }
        }
        if d.background != BackgroundStrength::Off {
            match self.session.remove_scan_backgrounds(id, &pages, BackgroundOptions { strength: d.background, white_point: None }) {
                Ok(r) if r.images_changed > 0 => {
                    done.push(crate::i18n::fmt(tl!("background whitened in {n} images"), &[("n", &r.images_changed.to_string())]))
                }
                Ok(_) => {}
                Err(e) => return fail(self, &done, e.to_string()),
            }
        }
        if d.despeckle != DespeckleLevel::Off {
            match self.session.despeckle_scans(id, &pages, DespeckleOptions { level: d.despeckle, ..Default::default() }) {
                Ok(r) if r.images_changed > 0 => {
                    done.push(crate::i18n::fmt(tl!("specks removed from {n} images"), &[("n", &r.images_changed.to_string())]))
                }
                Ok(_) => {}
                Err(e) => return fail(self, &done, e.to_string()),
            }
        }
        if let Some(info) = self.session.get(id).map(|d| d.info.clone()) {
            for view in self.views.iter_mut().filter(|v| v.id == id) {
                view.document_changed(&info);
            }
        }
        if done.is_empty() {
            self.notify_tr("Nothing needed enhancing on these pages");
        } else {
            self.notify(crate::i18n::fmt(tl!("Enhanced: {list}"), &[("list", &done.join(", "))]));
        }
    }
}
