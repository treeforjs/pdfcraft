//! File ▸ Page Setup (⇧⌘P): the paper, orientation and margins every print starts from. They are
//! session settings in the engine (`Session::page_setup`), so the Print dialog, its preview, Save
//! as PDF and `doc_print` all use the same ones. The operating system's own page setup sheet is
//! not shown; the printer driver's settings stay under Print ▸ Properties….

use egui::{Color32, Rect, Stroke, pos2, vec2};
use pdfcraft_engine::print::{self, MAX_PAPER, MIN_PAPER, Margins, Orientation, PAPERS, PageSetup};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// The unit sizes and margins are shown in. Values are kept in points.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Unit {
    Points,
    #[default]
    Inches,
    Millimetres,
}

impl Unit {
    /// Points per unit.
    fn points(self) -> f64 {
        match self {
            Unit::Points => 1.0,
            Unit::Inches => 72.0,
            Unit::Millimetres => 72.0 / 25.4,
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Unit::Points => " pt",
            Unit::Inches => " in",
            Unit::Millimetres => " mm",
        }
    }

    fn decimals(self) -> usize {
        match self {
            Unit::Points => 1,
            Unit::Inches => 2,
            Unit::Millimetres => 1,
        }
    }
}

/// Quick margin choices (points on every side).
const MARGIN_PRESETS: [(f64, &str); 4] = [(0.0, "None"), (18.0, "Narrow"), (36.0, "Normal"), (72.0, "Wide")];

/// The dialog's state, edited in place and applied with OK.
#[derive(Clone, Debug, PartialEq)]
pub struct PageSetupDraft {
    /// Portrait (width, height) in points.
    pub paper: (f64, f64),
    /// Custom size chosen (the width and height fields are editable).
    pub custom: bool,
    pub orientation: Orientation,
    pub margins: Margins,
    pub unit: Unit,
    /// Opened from the Print dialog, which comes back when this one closes.
    pub back_to_print: bool,
}

impl Default for PageSetupDraft {
    fn default() -> Self {
        PageSetupDraft::from_setup(&PageSetup::default(), false)
    }
}

impl PageSetupDraft {
    pub fn from_setup(s: &PageSetup, back_to_print: bool) -> Self {
        PageSetupDraft {
            paper: s.paper,
            custom: s.paper_name().is_none(),
            orientation: s.orientation,
            margins: s.margins,
            unit: Unit::default(),
            back_to_print,
        }
    }

    /// The page setup this draft describes, or why it can't be used.
    pub fn setup(&self) -> Result<PageSetup, String> {
        PageSetup::new(self.paper, self.orientation, self.margins).map_err(|e| e.to_string())
    }
}

impl PdfCraftApp {
    /// File ▸ Page Setup…. `back_to_print`: return to the Print dialog afterwards.
    pub fn open_page_setup(&mut self, back_to_print: bool) {
        let mut d = PageSetupDraft::from_setup(self.session.page_setup(), back_to_print);
        // Keep the unit chosen last time.
        d.unit = self.page_setup_draft.unit;
        self.page_setup_draft = d;
        self.dialog = Some(crate::Dialog::PageSetup);
    }

    /// OK: store the draft as the session's page setup (and in the Print dialog when it opened
    /// this one). Returns `false`, telling the user why, when the values can't be used.
    pub fn apply_page_setup(&mut self) -> bool {
        let setup = match self.page_setup_draft.setup() {
            Ok(s) => s,
            Err(e) => {
                self.notify_error(e);
                return false;
            }
        };
        if let Err(e) = self.session.set_page_setup(setup) {
            self.notify_error(e);
            return false;
        }
        self.print_draft.paper = setup.paper;
        self.print_draft.orientation = setup.orientation;
        self.print_draft.margins = setup.margins;
        true
    }
}

/// A length field in `unit` for a value kept in points.
fn length(ui: &mut egui::Ui, pts: &mut f64, unit: Unit, range: std::ops::RangeInclusive<f64>, enabled: bool) -> bool {
    let k = unit.points();
    let mut v = *pts / k;
    let (lo, hi) = (*range.start() / k, *range.end() / k);
    let speed = match unit {
        Unit::Points => 1.0,
        Unit::Inches => 0.01,
        Unit::Millimetres => 0.5,
    };
    let r = ui.add_enabled(
        enabled,
        egui::DragValue::new(&mut v).range(lo..=hi).speed(speed).max_decimals(unit.decimals()).suffix(unit.suffix()).min_decimals(0),
    );
    if r.changed() && v.is_finite() {
        *pts = (v * k).clamp(*range.start(), *range.end());
        return true;
    }
    false
}

/// Draw the dialog. Returns (ok, cancel).
pub(crate) fn body(ui: &mut egui::Ui, d: &mut PageSetupDraft, t: &Tokens) -> (bool, bool) {
    ui.label(egui::RichText::new(tl!("Page Setup")).font(theme::semibold(18.0)));
    ui.label(egui::RichText::new(tl!("The paper and margins Print and Save as PDF use until you change them.")).small().color(t.text_muted));
    ui.add_space(10.0);
    let checked = d.setup();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(300.0);
            widgets::section_title(ui, tl!("Paper"));
            let name = if d.custom { tl!("Custom size").to_string() } else { print::paper_name(d.paper).unwrap_or_default().to_string() };
            egui::ComboBox::from_id_salt("page-setup-paper").selected_text(name).width(220.0).show_ui(ui, |ui| {
                for (n, size) in PAPERS {
                    let on = !d.custom && print::paper_name(d.paper) == Some(n);
                    if ui.selectable_label(on, n).clicked() {
                        d.paper = size;
                        d.custom = false;
                    }
                }
                if ui.selectable_label(d.custom, tl!("Custom size")).clicked() {
                    d.custom = true;
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("page-setup-size").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label(tl!("Width:"));
                let mut changed = length(ui, &mut d.paper.0, d.unit, MIN_PAPER..=MAX_PAPER, d.custom);
                ui.end_row();
                ui.label(tl!("Height:"));
                changed |= length(ui, &mut d.paper.1, d.unit, MIN_PAPER..=MAX_PAPER, d.custom);
                ui.end_row();
                if changed {
                    d.custom = true;
                }
                ui.label(tl!("Units:"));
                let shown = d.unit.suffix().trim();
                egui::ComboBox::from_id_salt("page-setup-unit").selected_text(shown).width(70.0).show_ui(ui, |ui| {
                    for u in [Unit::Inches, Unit::Millimetres, Unit::Points] {
                        ui.selectable_value(&mut d.unit, u, u.suffix().trim());
                    }
                });
                ui.end_row();
            });
            ui.add_space(8.0);
            widgets::section_title(ui, tl!("Orientation"));
            ui.horizontal(|ui| {
                for (o, label) in [
                    (Orientation::Auto, tl!("Auto portrait/landscape")),
                    (Orientation::Portrait, tl!("Portrait")),
                    (Orientation::Landscape, tl!("Landscape")),
                ] {
                    ui.selectable_value(&mut d.orientation, o, label);
                }
            });
            ui.add_space(8.0);
            widgets::section_title(ui, tl!("Margins"));
            ui.horizontal(|ui| {
                for (v, label) in MARGIN_PRESETS {
                    let on = d.margins == Margins::uniform(v);
                    if ui.selectable_label(on, tl!(label)).clicked() {
                        d.margins = Margins::uniform(v);
                    }
                }
            });
            ui.add_space(4.0);
            let max = MAX_PAPER / 2.0;
            egui::Grid::new("page-setup-margins").num_columns(4).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label(tl!("Top:"));
                length(ui, &mut d.margins.top, d.unit, 0.0..=max, true);
                ui.label(tl!("Bottom:"));
                length(ui, &mut d.margins.bottom, d.unit, 0.0..=max, true);
                ui.end_row();
                ui.label(tl!("Left:"));
                length(ui, &mut d.margins.left, d.unit, 0.0..=max, true);
                ui.label(tl!("Right:"));
                length(ui, &mut d.margins.right, d.unit, 0.0..=max, true);
                ui.end_row();
            });
        });
        ui.add_space(16.0);
        ui.vertical(|ui| {
            ui.set_width(200.0);
            preview(ui, d, t, checked.as_ref().err());
        });
    });
    ui.add_space(12.0);
    let (mut ok, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        if ui.button(tl!("Restore defaults")).clicked() {
            let unit = d.unit;
            *d = PageSetupDraft { unit, back_to_print: d.back_to_print, ..PageSetupDraft::default() };
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add_enabled_ui(checked.is_ok(), |ui| widgets::pill_button(ui, tl!("OK"), true)).inner.clicked() {
                ok = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                cancel = true;
            }
        });
    });
    (ok, cancel)
}

/// The sheet with its printable area, drawn to scale (no page rendering).
fn preview(ui: &mut egui::Ui, d: &PageSetupDraft, t: &Tokens, error: Option<&String>) {
    let (area, _) = ui.allocate_exact_size(vec2(200.0, 220.0), egui::Sense::hover());
    ui.painter().rect_filled(area, 6.0, t.hover);
    let (w, h) = if d.orientation == Orientation::Landscape { (d.paper.1, d.paper.0) } else { d.paper };
    let ok = w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0;
    if ok {
        let k = ((area.width() - 32.0) as f64 / w).min((area.height() - 32.0) as f64 / h) as f32;
        let paper = Rect::from_center_size(area.center(), vec2(w as f32 * k, h as f32 * k));
        ui.painter().rect_filled(paper, 0.0, Color32::WHITE);
        ui.painter().rect_stroke(paper, 0.0, Stroke::new(1.0, t.border), egui::StrokeKind::Outside);
        let m = d.margins;
        let inner = Rect::from_min_max(
            pos2(paper.left() + m.left as f32 * k, paper.top() + m.top as f32 * k),
            pos2(paper.right() - m.right as f32 * k, paper.bottom() - m.bottom as f32 * k),
        );
        if error.is_none() && inner.is_positive() {
            ui.painter().rect_filled(inner, 0.0, t.accent_soft);
            dashed_rect(ui.painter(), inner, Stroke::new(1.0, t.accent));
        }
    }
    let (wi, hi) = (w / 72.0, h / 72.0);
    let size = format!("{wi:.2} × {hi:.2} in  ·  {:.0} × {:.0} mm", wi * 25.4, hi * 25.4);
    ui.label(egui::RichText::new(size).small().color(t.text_muted));
    if d.orientation == Orientation::Auto {
        ui.label(egui::RichText::new(tl!("Each sheet turns to suit its page.")).small().color(t.text_muted));
    }
    if let Some(e) = error {
        ui.label(egui::RichText::new(e).small().color(t.text));
    }
}

/// A dashed outline (the printable area).
pub(crate) fn dashed_rect(painter: &egui::Painter, r: Rect, stroke: Stroke) {
    let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    painter.extend(egui::Shape::dashed_line(&corners, stroke, 4.0, 3.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_round_trips_and_refuses_what_the_engine_refuses() {
        let s = PageSetup::new((595.28, 841.89), Orientation::Landscape, Margins::uniform(36.0)).unwrap();
        let d = PageSetupDraft::from_setup(&s, true);
        assert!(!d.custom && d.back_to_print);
        assert_eq!(d.setup(), Ok(s));
        let custom = PageSetupDraft::from_setup(&PageSetup { paper: (400.0, 500.0), ..PageSetup::default() }, false);
        assert!(custom.custom);
        for bad in [
            PageSetupDraft { paper: (f64::NAN, 792.0), ..d.clone() },
            PageSetupDraft { paper: (0.0, 0.0), ..d.clone() },
            PageSetupDraft { margins: Margins::uniform(-1.0), ..d.clone() },
            PageSetupDraft { margins: Margins::uniform(f64::INFINITY), ..d.clone() },
            PageSetupDraft { margins: Margins { left: 400.0, right: 400.0, ..Margins::default() }, ..d.clone() },
        ] {
            assert!(bad.setup().is_err(), "{bad:?}");
        }
    }
}
