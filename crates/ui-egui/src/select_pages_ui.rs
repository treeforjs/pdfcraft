//! Pages ▸ Select pages…: select pages by a typed range (`1-3, 7, 9-`, page labels too),
//! narrowed to odd or even pages and to landscape or portrait ones. The selection is the one the
//! Organize grid and the Pages panel act on.

use egui::{Align, Layout};
use pdfcraft_engine::{PageOrientation, PageParity};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, RightPanel, widgets};

/// The Select Pages dialog's state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelectPages {
    pub range: String,
    pub parity: PageParity,
    pub orientation: PageOrientation,
    pub error: Option<String>,
}

impl PdfCraftApp {
    /// Select the pages the dialog describes in the active view. Returns whether it worked.
    pub fn select_page_range(&mut self) -> bool {
        let Some((i, id)) = self.active_ids() else { return true };
        let Some(doc) = self.session.get(id) else { return true };
        let s = &self.select_pages;
        match pdfcraft_engine::select_page_range(&doc.info, &s.range, s.parity, s.orientation) {
            Ok(pages) if pages.is_empty() => {
                self.select_pages.error = Some(tl!("No pages match.").to_string());
                false
            }
            Ok(pages) => {
                let n = pages.len();
                if let Some(v) = self.views.get_mut(i) {
                    v.select_pages(&pages);
                    // The selection shows in the Organize grid, else in the Pages panel.
                    if !v.organize {
                        self.right = Some(RightPanel::Pages);
                    }
                }
                self.select_pages.error = None;
                let msg =
                    if n == 1 { tl!("1 page selected").to_string() } else { crate::i18n::fmt(tl!("{n} pages selected"), &[("n", &n.to_string())]) };
                self.notify(msg);
                true
            }
            Err(e) => {
                self.select_pages.error = Some(e);
                false
            }
        }
    }
}

/// The dialog. Returns `true` to close.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    ui.label(egui::RichText::new(tl!("Select pages")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    let s = &mut app.select_pages;
    let mut enter = false;
    egui::Grid::new("select-pages").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        let l = ui.label(tl!("Pages:"));
        let r = ui
            .add(
                egui::TextEdit::singleline(&mut s.range)
                    .hint_text(tl!("e.g. 1-3, 7 (empty: all pages)"))
                    .desired_width(260.0)
                    .id_salt("select-range"),
            )
            .labelled_by(l.id);
        enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.end_row();
        ui.label(tl!("Subset:"));
        ui.horizontal(|ui| {
            ui.radio_value(&mut s.parity, PageParity::Both, tl!("All"));
            ui.radio_value(&mut s.parity, PageParity::Odd, tl!("Odd pages"));
            ui.radio_value(&mut s.parity, PageParity::Even, tl!("Even pages"));
        });
        ui.end_row();
        ui.label(tl!("Orientation:"));
        ui.horizontal(|ui| {
            ui.radio_value(&mut s.orientation, PageOrientation::Both, tl!("All"));
            ui.radio_value(&mut s.orientation, PageOrientation::Landscape, tl!("Landscape pages"));
            ui.radio_value(&mut s.orientation, PageOrientation::Portrait, tl!("Portrait pages"));
        });
        ui.end_row();
    });
    if let Some(e) = &s.error {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(e).color(theme::DANGER));
    }
    ui.label(egui::RichText::new(tl!("Page labels work too, as in Print.")).small().color(t.text_faint));
    ui.add_space(10.0);
    let (mut ok, mut cancel) = (enter, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ok |= widgets::pill_button(ui, tl!("Select"), true).clicked();
        cancel = widgets::pill_button(ui, tl!("Cancel"), false).clicked();
    });
    if cancel {
        app.select_pages.error = None;
        return true;
    }
    ok && app.select_page_range()
}
