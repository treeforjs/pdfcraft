//! View ▸ Zoom ▸ Marquee Zoom (drag a rectangle to fill the window with it; click to zoom in)
//! and Edit ▸ Take a Snapshot (drag a rectangle to copy that area as an image).

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke};
use printcraft_render::{RenderConfig, RenderRequest, RequestKind, Tile};

use crate::canvas::{DocView, PageXform};
use crate::{PrintCraftApp, QuickTool};

/// A finished gesture: (page, the rectangle on screen, the same in page view points
/// [x0, y0, x1, y1]); a plain click has an empty rectangle.
pub type Marquee = (usize, Rect, [f32; 4]);

/// Drag a rectangle on page `page` (Marquee Zoom, Snapshot).
pub(crate) fn page_input(ui: &egui::Ui, resp: &egui::Response, xf: &PageXform, page: usize, view: &mut DocView) {
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let origin = ui.input(|i| i.pointer.press_origin());
    if pointer.is_some_and(|p| xf.rect.contains(p)) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if resp.drag_started()
        && let Some(o) = origin.filter(|o| xf.rect.contains(*o))
    {
        view.marquee = Some((page, o));
    }
    if let Some((p, start)) = view.marquee
        && p == page
        && let Some(end) = pointer
    {
        let end = Pos2::new(end.x.clamp(xf.rect.left(), xf.rect.right()), end.y.clamp(xf.rect.top(), xf.rect.bottom()));
        let r = Rect::from_two_pos(start, end);
        ui.painter().rect_stroke(r, CornerRadius::ZERO, Stroke::new(1.0, Color32::from_rgb(0x14, 0x73, 0xE6)), egui::StrokeKind::Middle);
        if resp.drag_stopped() {
            view.marquee = None;
            let (a, b) = (xf.screen_to_view(r.left_top()), xf.screen_to_view(r.right_bottom()));
            view.marquee_done = Some((page, r, [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]));
        }
    }
    if resp.clicked()
        && let Some(p) = pointer.filter(|p| xf.rect.contains(*p))
    {
        let v = xf.screen_to_view(p);
        view.marquee_done = Some((page, Rect::from_min_size(p, egui::Vec2::ZERO), [v.0, v.1, v.0, v.1]));
    }
}

impl PrintCraftApp {
    /// Finish a marquee gesture for the current tool.
    pub(crate) fn finish_marquee(&mut self, index: usize, done: Marquee) {
        let (page, rect, view_rect) = done;
        match self.quick_tool {
            QuickTool::MarqueeZoom => {
                let view = &mut self.views[index];
                if rect.width() < 6.0 || rect.height() < 6.0 {
                    // A click zooms in one step around the point.
                    let z = (view.zoom * 1.5).min(64.0);
                    view.zoom_at(z, rect.center());
                } else {
                    view.zoom_to_rect(rect);
                }
            }
            QuickTool::Snapshot => {
                if view_rect[2] - view_rect[0] < 2.0 || view_rect[3] - view_rect[1] < 2.0 {
                    return;
                }
                match self.snapshot(index, page, view_rect) {
                    Ok((w, h)) => self.notify(format!("The selected area has been copied ({w} × {h} pixels)")),
                    Err(e) => self.notify(format!("Couldn't take the snapshot: {e}")),
                }
            }
            _ => {}
        }
    }

    /// Render `view_rect` (page view points) of `page` at the current zoom and copy it to the
    /// clipboard (and keep it in `last_snapshot`).
    pub fn snapshot(&mut self, index: usize, page: usize, view_rect: [f32; 4]) -> Result<(u32, u32), String> {
        let view = &self.views[index];
        let doc = self.session.get(view.id).ok_or("no document")?;
        let ppp = self.ctx.as_ref().map_or(2.0, |c| c.pixels_per_point());
        let scale = (view.zoom * ppp).clamp(0.5, 8.0);
        let tile = Tile {
            x: (view_rect[0] * scale).floor().max(0.0) as u32,
            y: (view_rect[1] * scale).floor().max(0.0) as u32,
            w: ((view_rect[2] - view_rect[0]) * scale).ceil().max(1.0) as u32,
            h: ((view_rect[3] - view_rect[1]) * scale).ceil().max(1.0) as u32,
        };
        if (tile.w as u64) * (tile.h as u64) > 64_000_000 {
            return Err("the area is too large at this zoom".into());
        }
        let config = RenderConfig { password: doc.password.as_deref().map(std::sync::Arc::from), ..RenderConfig::default() };
        let mut r = printcraft_render::PageRenderer::new(doc.bytes.clone(), config);
        let out = r.render(RenderRequest { page, kind: RequestKind::Pixels, tile: Some(tile), scale, tag: 0 });
        if let Some(e) = out.error {
            return Err(e);
        }
        let (w, h) = (out.width, out.height);
        #[cfg(not(target_arch = "wasm32"))]
        if self.system_clipboard {
            let img = arboard::ImageData { width: w as usize, height: h as usize, bytes: std::borrow::Cow::Borrowed(&out.rgba) };
            arboard::Clipboard::new().and_then(|mut c| c.set_image(img)).map_err(|e| e.to_string())?;
        }
        self.last_snapshot = Some((w, h, out.rgba));
        Ok((w, h))
    }
}
