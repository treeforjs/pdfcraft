//! Comment pop-up notes on the page (Acrobat: "Open Pop-up Note").
//!
//! An open pop-up (`/Popup` with `/Open true`) is drawn as a floating card over the page: a
//! header with the comment's colour, author and date, which drags the card, a close button,
//! the comment's text and replies, and a grip that resizes it. Pop-ups keep their size at every
//! zoom and stay upright on rotated pages (ISO 32000-2 NoZoom | NoRotate), so a card's top-left
//! corner is the pop-up rectangle's top-left corner in user space (`[x0, y1]`) and its size is
//! the rectangle's in points. Moving, resizing and closing are `Edit::SetCommentPopup`s, one undo
//! step each, applied by the app like every other comment edit.

use egui::{CornerRadius, Rect, Sense, Stroke, Vec2, pos2, vec2};
use pdfcraft_engine::Edit;
use pdfcraft_render::DocInfo;

use crate::canvas::{DocView, PageXform};
use crate::theme::{self, Tokens};

/// A card can be made no smaller than this on screen (room for the header and a line of text).
pub const MIN_CARD: Vec2 = vec2(140.0, 80.0);
/// Nor larger than this (the engine's limit for a pop-up's side, in points).
pub const MAX_CARD: f32 = 2000.0;
/// At most this many cards are drawn per frame (a hostile file can hold thousands of open pop-ups).
const MAX_CARDS: usize = 64;
const HEADER: f32 = 30.0;
const GRIP: f32 = 14.0;

/// An open pop-up on a page on screen: page, index, pop-up rectangle, page transform, the
/// page's comments.
type OpenCard<'a> = (usize, usize, [f32; 4], PageXform, &'a [pdfcraft_render::Annotation]);

/// A pop-up card being dragged or resized: the pointer's movement since the press.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopupDrag {
    pub page: usize,
    pub index: usize,
    pub resize: bool,
    pub delta: Vec2,
}

/// The card's rectangle on screen for a pop-up `rect` (user space) on `page`.
pub(crate) fn card_rect(info: &DocInfo, page: usize, xf: &PageXform, rect: [f32; 4]) -> Option<Rect> {
    let p = info.pages.get(page)?;
    if !rect.iter().all(|v| v.is_finite()) {
        return None;
    }
    let v = p.user_to_view(rect[0].min(rect[2]), rect[1].max(rect[3]));
    let anchor = xf.norm_to_screen(v[0] / xf.pw.max(1e-3), v[1] / xf.ph.max(1e-3));
    let size = vec2((rect[2] - rect[0]).abs().clamp(MIN_CARD.x, MAX_CARD), (rect[3] - rect[1]).abs().clamp(MIN_CARD.y, MAX_CARD));
    // Far off the page (where f32 screen coordinates lose precision): not drawn.
    (anchor.x.abs() < 1e6 && anchor.y.abs() < 1e6).then(|| Rect::from_min_size(anchor, size))
}

/// The pop-up rectangle (user space) for a card at `card` on screen: the inverse of [`card_rect`].
pub(crate) fn popup_rect(info: &DocInfo, page: usize, xf: &PageXform, card: Rect) -> Option<[f64; 4]> {
    let p = info.pages.get(page)?;
    let (vx, vy) = xf.screen_to_view(card.min);
    let [x, y] = p.view_to_user(vx, vy);
    let (w, h) = (card.width().clamp(MIN_CARD.x, MAX_CARD), card.height().clamp(MIN_CARD.y, MAX_CARD));
    let r = [x as f64, (y - h) as f64, (x + w) as f64, y as f64];
    r.iter().all(|v| v.is_finite()).then_some(r)
}

/// The card after a drag in progress (or finished) by `d`.
fn dragged(card: Rect, d: &PopupDrag) -> Rect {
    if d.resize {
        let size = (card.size() + d.delta).clamp(MIN_CARD, Vec2::splat(MAX_CARD));
        Rect::from_min_size(card.min, size)
    } else {
        card.translate(d.delta)
    }
}

/// Draw the open pop-ups of the pages on screen. Returns the edit a card asked for (moved,
/// resized or closed). `allowed`: the document allows comment changes; otherwise cards are
/// shown but can't be moved or closed.
pub(crate) fn show(ctx: &egui::Context, view: &mut DocView, info: &DocInfo, allowed: bool, hidden: bool) -> Option<Edit> {
    if hidden {
        view.comments.popup_drag = None;
        return None;
    }
    let t = Tokens::get(ctx);
    let viewport = view.viewport_rect();
    // Only the comments of the pages on screen are looked at: `annotations` is sorted by page,
    // so each page's run is found by binary search and the frame's cost doesn't grow with the
    // document's comment count.
    let mut open: Vec<OpenCard<'_>> = Vec::new();
    for (page, xf) in view.screen_pages() {
        let all = &info.annotations;
        let (lo, hi) = (all.partition_point(|a| a.page < page), all.partition_point(|a| a.page <= page));
        let Some(on_page) = all.get(lo..hi.max(lo)) else { continue };
        for a in on_page.iter().filter(|a| a.in_reply_to.is_none() && a.subtype != "Popup") {
            if let Some(p) = a.popup.filter(|p| p.open)
                && open.len() < MAX_CARDS
            {
                open.push((page, a.index, p.rect, xf, on_page));
            }
        }
    }
    let mut edit = None;
    for (page, index, rect, xf, on_page) in open {
        let Some(base) = card_rect(info, page, &xf, rect) else { continue };
        let drag = view.comments.popup_drag.filter(|d| (d.page, d.index) == (page, index));
        let card = drag.map_or(base, |d| dragged(base, &d));
        if !card.intersects(viewport) {
            continue;
        }
        let Some(a) = on_page.iter().find(|a| a.index == index && a.in_reply_to.is_none()) else { continue };
        // Replies share their comment's page.
        let replies: Vec<&pdfcraft_render::Annotation> =
            on_page.iter().filter(|r| a.name.is_some() && r.in_reply_to == a.name && r.state.is_none()).take(200).collect();
        let accent = a.color.map_or(t.accent, |c| crate::comments::color32(c.map(f64::from)));
        let id = egui::Id::new(("comment-popup", view.id.0, page, index));
        let mut close = false;
        let mut select = false;
        let mut started: Option<bool> = None;
        let mut moved = Vec2::ZERO;
        let mut stopped = false;
        egui::Area::new(id).order(egui::Order::Middle).fixed_pos(card.min).constrain_to(viewport).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).fill(t.card).stroke(Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(8)).inner_margin(0).show(
                ui,
                |ui| {
                    ui.set_width(card.width());
                    ui.set_height(card.height());
                    // The header drags the card; the close button, added later, sits on top of it.
                    let (header, head) =
                        ui.allocate_exact_size(vec2(card.width(), HEADER), if allowed { Sense::click_and_drag() } else { Sense::click() });
                    let move_label = tl!("Move pop-up");
                    head.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, allowed, move_label));
                    if allowed && head.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    }
                    if head.drag_started() {
                        started = Some(false);
                    }
                    if head.dragged() {
                        moved += head.drag_delta();
                    }
                    stopped |= head.drag_stopped();
                    select |= head.clicked();
                    let painter = ui.painter();
                    painter.rect_filled(
                        Rect::from_min_size(header.min, vec2(header.width(), 3.0)),
                        CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 },
                        accent,
                    );
                    painter.line_segment([pos2(header.left(), header.bottom()), pos2(header.right(), header.bottom())], Stroke::new(1.0, t.divider));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(header.shrink2(vec2(10.0, 3.0))), |ui| {
                        ui.horizontal_centered(|ui| {
                            let who = a.author.clone().unwrap_or_else(|| tl!("Unknown author").to_string());
                            ui.add(egui::Label::new(egui::RichText::new(who).font(theme::semibold(12.5)).color(t.text)).truncate().selectable(false));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_enabled_ui(allowed, |ui| {
                                    if crate::icons::button(ui, "x", 22.0, false, tl!("Close pop-up")).clicked() {
                                        close = true;
                                    }
                                });
                                if let Some(m) = &a.modified {
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(m).font(theme::regular(11.0)).color(t.text_faint))
                                            .truncate()
                                            .selectable(false),
                                    );
                                }
                            });
                        });
                    });
                    let body = Rect::from_min_max(pos2(card.left() + 10.0, header.bottom() + 6.0), pos2(card.right() - 10.0, card.bottom() - GRIP));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
                        egui::ScrollArea::vertical().id_salt(id.with("body")).auto_shrink([false, false]).show(ui, |ui| {
                            let text = a.contents.clone().unwrap_or_default();
                            if text.is_empty() {
                                ui.label(egui::RichText::new(tl!("No text")).font(theme::regular(12.0)).color(t.text_faint).italics());
                            } else {
                                ui.label(egui::RichText::new(text).font(theme::regular(12.5)).color(t.text));
                            }
                            for r in &replies {
                                ui.add_space(6.0);
                                let who = r.author.clone().unwrap_or_else(|| tl!("Unknown author").to_string());
                                ui.label(egui::RichText::new(who).font(theme::semibold(11.5)).color(t.text_muted));
                                ui.label(egui::RichText::new(r.contents.clone().unwrap_or_default()).font(theme::regular(12.0)).color(t.text));
                            }
                        });
                    });
                    if allowed {
                        let grip = Rect::from_min_size(card.right_bottom() - Vec2::splat(GRIP), Vec2::splat(GRIP));
                        let g = ui.interact(grip, id.with("resize"), Sense::drag());
                        let resize_label = tl!("Resize pop-up");
                        g.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, resize_label));
                        if g.hovered() || g.dragged() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                        }
                        if g.drag_started() {
                            started = Some(true);
                        }
                        if g.dragged() {
                            moved += g.drag_delta();
                        }
                        stopped |= g.drag_stopped();
                        let c = grip.right_bottom() - vec2(3.0, 3.0);
                        for k in [4.0, 8.0] {
                            ui.painter().line_segment([c - vec2(k, 0.0), c - vec2(0.0, k)], Stroke::new(1.0, t.text_faint));
                        }
                    }
                },
            );
        });
        let cv = &mut view.comments;
        if let Some(resize) = started {
            cv.popup_drag = Some(PopupDrag { page, index, resize, delta: Vec2::ZERO });
        }
        if let Some(d) = cv.popup_drag.as_mut().filter(|d| (d.page, d.index) == (page, index)) {
            d.delta += moved;
        }
        if stopped
            && let Some(d) = cv.popup_drag.take()
            && d.delta.length() >= 1.0
            && let Some(r) = popup_rect(info, page, &xf, dragged(base, &d))
        {
            edit = Some(Edit::SetCommentPopup { page, index, open: None, rect: Some(r) });
        }
        if select {
            cv.selected = Some((page, index));
            cv.reveal = true;
        }
        if close {
            edit = Some(Edit::SetCommentPopup { page, index, open: Some(false), rect: None });
        }
    }
    edit
}

/// Whether a comment of this subtype can show a pop-up (markup annotations other than text
/// boxes; keep in step with `pdfcraft_annot::popup::has_popup_subtype`).
pub(crate) fn can_have_popup(subtype: &str) -> bool {
    !matches!(
        subtype,
        "FreeText" | "Sound" | "Link" | "Widget" | "Popup" | "Screen" | "Movie" | "RichMedia" | "3D" | "PrinterMark" | "TrapNet" | "Watermark"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Pos2;

    fn page(rotation: u16) -> DocInfo {
        let mut info = DocInfo::default();
        let (width, height) = if rotation.is_multiple_of(180) { (600.0, 800.0) } else { (800.0, 600.0) };
        let p = pdfcraft_render::PageInfo { width, height, label: String::new(), crop: [0.0, 0.0, 600.0, 800.0], rotation };
        info.pages.push(p);
        info
    }

    #[test]
    fn cards_round_trip_through_pop_up_rectangles() {
        for rot in [0, 90, 180, 270] {
            let info = page(rot);
            let (w, h) = (info.pages[0].width, info.pages[0].height);
            let xf = PageXform { rect: Rect::from_min_size(pos2(50.0, 20.0), vec2(w * 1.5, h * 1.5)), rot: 0, pw: w, ph: h };
            let rect = [300.0, 500.0, 500.0, 620.0];
            let card = card_rect(&info, 0, &xf, rect).unwrap();
            assert_eq!(card.size(), vec2(200.0, 120.0), "NoZoom: the card keeps its size in points ({rot}°)");
            let back = popup_rect(&info, 0, &xf, card).unwrap();
            for (a, b) in back.iter().zip(rect) {
                assert!((a - b as f64).abs() < 0.01, "{rot}°: {back:?} vs {rect:?}");
            }
            let moved = popup_rect(&info, 0, &xf, card.translate(vec2(15.0, 0.0))).unwrap();
            assert!((moved[2] - moved[0] - 200.0).abs() < 0.01, "moving keeps the size");
        }
    }

    #[test]
    fn hostile_rectangles_are_clamped_or_refused() {
        let info = page(0);
        let xf = PageXform { rect: Rect::from_min_size(Pos2::ZERO, vec2(600.0, 800.0)), rot: 0, pw: 600.0, ph: 800.0 };
        assert!(card_rect(&info, 0, &xf, [f32::NAN, 0.0, 1.0, 1.0]).is_none());
        assert!(card_rect(&info, 3, &xf, [0.0, 0.0, 10.0, 10.0]).is_none(), "no such page");
        let tiny = card_rect(&info, 0, &xf, [10.0, 10.0, 10.0, 10.0]).unwrap();
        assert_eq!(tiny.size(), MIN_CARD);
        assert!(card_rect(&info, 0, &xf, [0.0, 0.0, 1e9, 1e9]).is_none(), "too far away to draw");
        let huge = card_rect(&info, 0, &xf, [0.0, 0.0, 1e5, 1e5]).unwrap();
        assert_eq!(huge.size(), Vec2::splat(MAX_CARD));
        let d = PopupDrag { page: 0, index: 0, resize: true, delta: vec2(-1e6, -1e6) };
        assert_eq!(dragged(tiny, &d).size(), MIN_CARD, "resizing stops at the minimum");
    }

    #[test]
    fn text_boxes_have_no_pop_up() {
        assert!(can_have_popup("Text") && can_have_popup("Highlight") && can_have_popup("Square"));
        assert!(!can_have_popup("FreeText") && !can_have_popup("Popup") && !can_have_popup("Widget"));
    }
}
