//! Edit a PDF ▸ Edit text: boxes around the paragraphs of existing text; click one to edit it in
//! place (⌘Enter or clicking away applies and rewraps it to the box, Esc cancels).

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke};
use printcraft_engine::Edit;
use printcraft_render::DocInfo;

use crate::canvas::{DocView, PageXform};

const ACCENT: Color32 = Color32::from_rgb(0x14, 0x73, 0xE6);

/// A paragraph being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct LineEditor {
    pub page: usize,
    pub block: usize,
    pub text: String,
    original: String,
    rect: Rect,
    size: f32,
    focus: bool,
}

/// Lines and their screen boxes for a page; hover outlines, click opens the editor. Returns
/// `true` when the pointer was used.
pub(crate) fn page_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    xf: &PageXform,
    page: usize,
    info: &DocInfo,
    lines: &[printcraft_engine::TextBlock],
    view: &mut DocView,
) -> bool {
    let boxes: Vec<Rect> = lines.iter().map(|l| xf.user_rect(info, page, l.rect.map(|v| v as f32)).expand(2.0)).collect();
    let painter = ui.painter();
    for b in &boxes {
        painter.rect_stroke(*b, CornerRadius::same(2), Stroke::new(0.75, ACCENT.gamma_multiply(0.35)), egui::StrokeKind::Outside);
    }
    let Some(p) = ui.input(|i| i.pointer.hover_pos()).filter(|p| xf.rect.contains(*p)) else { return false };
    let Some(hit) = boxes.iter().position(|b| b.contains(p)) else { return false };
    painter.rect_stroke(boxes[hit], CornerRadius::same(2), Stroke::new(1.5, ACCENT), egui::StrokeKind::Outside);
    ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    if resp.clicked() {
        let l = &lines[hit];
        // Screen pixels per point, from the box's width.
        let scale = (boxes[hit].width() - 4.0) / ((l.rect[2] - l.rect[0]).max(1.0) as f32);
        view.line_editor = Some(LineEditor {
            page,
            block: hit,
            text: l.text.clone(),
            original: l.text.clone(),
            rect: boxes[hit],
            size: (l.size as f32 * scale).clamp(8.0, 72.0),
            focus: true,
        });
    }
    true
}

/// The inline editor; returns the edit once the text is applied.
pub(crate) fn overlay(ctx: &egui::Context, view: &mut DocView) -> Option<Edit> {
    let ed = view.line_editor.as_mut()?;
    let mut done = None;
    egui::Area::new(egui::Id::new("edit-text-line")).order(egui::Order::Foreground).fixed_pos(Pos2::new(ed.rect.left(), ed.rect.top())).show(
        ctx,
        |ui| {
            egui::Frame::NONE.fill(Color32::WHITE).stroke(Stroke::new(1.5, ACCENT)).inner_margin(egui::Margin::symmetric(2, 0)).show(ui, |ui| {
                let r = ui.add(
                    egui::TextEdit::multiline(&mut ed.text)
                        .id(egui::Id::new("edit-text-line-input"))
                        .font(egui::FontId::proportional(ed.size))
                        .text_color(Color32::BLACK)
                        .frame(egui::Frame::NONE)
                        .desired_width(ed.rect.width().max(120.0))
                        .desired_rows(((ed.rect.height() / (ed.size * 1.2)).round() as usize).max(1)),
                );
                if ed.focus {
                    r.request_focus();
                    ed.focus = false;
                }
                let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                let apply = ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
                if esc {
                    done = Some(false);
                } else if apply || r.lost_focus() {
                    done = Some(true);
                }
            });
        },
    );
    match done {
        Some(apply) => {
            let ed = view.line_editor.take()?;
            (apply && ed.text != ed.original).then_some(Edit::EditTextBlock { page: ed.page, block: ed.block, text: ed.text })
        }
        None => None,
    }
}
