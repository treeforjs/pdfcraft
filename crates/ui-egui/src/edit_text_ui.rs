//! Edit a PDF ▸ Edit text & images: boxes around the paragraphs and images already on the page.
//! Click a paragraph to edit it in place (⌘Enter or clicking away applies and rewraps it to the
//! box, Esc cancels). Click an image to select it: drag to move, drag a corner to resize (keeping
//! its proportions), right-click for rotate, flip, replace, save and delete; Delete removes it.

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

/// A selected page image, and what the pointer is doing to it.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageSelection {
    pub page: usize,
    pub index: usize,
    /// Dragging: the start point and, for a corner, the opposite corner (screen).
    drag: Option<(Pos2, Option<Pos2>)>,
}

/// What a right-click on a selected image asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ImageAction {
    Replace(usize, usize),
    Save(usize, usize),
}

fn user_box(xf: &PageXform, info: &DocInfo, page: usize, r: Rect) -> [f64; 4] {
    let p = &info.pages[page];
    let (a, b) = (xf.screen_to_view(r.min), xf.screen_to_view(r.max));
    let (u, v) = (p.view_to_user(a.0, a.1), p.view_to_user(b.0, b.1));
    [u[0].min(v[0]) as f64, u[1].min(v[1]) as f64, u[0].max(v[0]) as f64, u[1].max(v[1]) as f64]
}

/// Images on a page: select, move, resize, right-click. Returns `true` when the pointer was used.
#[allow(clippy::too_many_arguments)]
pub(crate) fn image_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    xf: &PageXform,
    page: usize,
    info: &DocInfo,
    images: &[printcraft_engine::PageImage],
    view: &mut DocView,
    action: &mut Option<ImageAction>,
) -> bool {
    let boxes: Vec<Rect> = images.iter().map(|im| xf.user_rect(info, page, im.rect.map(|v| v as f32))).collect();
    let painter = ui.painter();
    for b in &boxes {
        painter.rect_stroke(*b, CornerRadius::ZERO, Stroke::new(0.75, ACCENT.gamma_multiply(0.35)), egui::StrokeKind::Outside);
    }
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let selected = view.image_selection.as_ref().filter(|s| s.page == page).map(|s| s.index).filter(|i| *i < boxes.len());
    // The selected image: frame, corner handles, dragging.
    if let Some(i) = selected {
        let b = boxes[i];
        painter.rect_stroke(b, CornerRadius::ZERO, Stroke::new(1.5, ACCENT), egui::StrokeKind::Outside);
        let corners = [b.left_top(), b.right_top(), b.left_bottom(), b.right_bottom()];
        for c in corners {
            painter.rect(
                Rect::from_center_size(c, egui::vec2(8.0, 8.0)),
                CornerRadius::ZERO,
                Color32::WHITE,
                Stroke::new(1.0, ACCENT),
                egui::StrokeKind::Middle,
            );
        }
        let origin = ui.input(|i| i.pointer.press_origin());
        if resp.drag_started()
            && let Some(o) = origin
        {
            let corner = corners.iter().position(|c| c.distance(o) < 8.0);
            if corner.is_some() || b.contains(o) {
                let opposite = corner.map(|k| corners[3 - k]);
                if let Some(s) = view.image_selection.as_mut() {
                    s.drag = Some((o, opposite));
                }
            }
        }
        if let Some((start, opposite)) = view.image_selection.as_ref().and_then(|s| s.drag)
            && let Some(p) = pointer
        {
            let preview = match opposite {
                // Resize from the opposite corner, keeping the aspect ratio.
                Some(fixed) => {
                    let (w0, h0) = (b.width().max(1.0), b.height().max(1.0));
                    let k = ((p.x - fixed.x).abs() / w0).max((p.y - fixed.y).abs() / h0).max(0.05);
                    let (w, h) = (w0 * k, h0 * k);
                    let x = if p.x < fixed.x { fixed.x - w } else { fixed.x };
                    let y = if p.y < fixed.y { fixed.y - h } else { fixed.y };
                    Rect::from_min_size(Pos2::new(x, y), egui::vec2(w, h))
                }
                None => b.translate(p - start),
            };
            painter.rect_stroke(preview, CornerRadius::ZERO, Stroke::new(1.0, ACCENT), egui::StrokeKind::Middle);
            if resp.drag_stopped() {
                if let Some(s) = view.image_selection.as_mut() {
                    s.drag = None;
                }
                if preview != b {
                    view.pending_edit =
                        Some(Edit::EditPageImage { page, index: i, change: printcraft_engine::ImageEdit::Move(user_box(xf, info, page, preview)) });
                }
            }
            return true;
        }
        if ui.input(|inp| inp.key_pressed(egui::Key::Delete) || inp.key_pressed(egui::Key::Backspace)) && !ui.ctx().egui_wants_keyboard_input() {
            view.image_selection = None;
            view.pending_edit = Some(Edit::EditPageImage { page, index: i, change: printcraft_engine::ImageEdit::Delete });
            return true;
        }
    }
    let Some(p) = pointer.filter(|p| xf.rect.contains(*p)) else { return false };
    let Some(hit) = boxes.iter().rposition(|b| b.contains(p)) else { return false };
    if selected != Some(hit) {
        painter.rect_stroke(boxes[hit], CornerRadius::ZERO, Stroke::new(1.5, ACCENT.gamma_multiply(0.7)), egui::StrokeKind::Outside);
    }
    ui.ctx().set_cursor_icon(if selected == Some(hit) { egui::CursorIcon::Move } else { egui::CursorIcon::PointingHand });
    if resp.clicked() || resp.secondary_clicked() {
        view.image_selection = Some(ImageSelection { page, index: hit, drag: None });
    }
    if selected == Some(hit) {
        resp.context_menu(|ui| {
            use printcraft_engine::ImageEdit as E;
            let items: [(&str, Option<E>); 4] = [
                ("Rotate Clockwise", Some(E::Rotate(1))),
                ("Rotate Counterclockwise", Some(E::Rotate(3))),
                ("Flip Horizontal", Some(E::Flip { horizontal: true })),
                ("Flip Vertical", Some(E::Flip { horizontal: false })),
            ];
            for (label, change) in items {
                if ui.button(label).clicked() {
                    view.pending_edit = change.map(|c| Edit::EditPageImage { page, index: hit, change: c });
                    ui.close();
                }
            }
            if ui.button("Replace Image…").clicked() {
                *action = Some(ImageAction::Replace(page, hit));
                ui.close();
            }
            if ui.button("Save Image As…").clicked() {
                *action = Some(ImageAction::Save(page, hit));
                ui.close();
            }
            ui.separator();
            if ui.button("Delete").clicked() {
                view.image_selection = None;
                view.pending_edit = Some(Edit::EditPageImage { page, index: hit, change: E::Delete });
                ui.close();
            }
        });
    }
    true
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
