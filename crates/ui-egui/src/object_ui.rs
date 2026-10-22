//! Edit a PDF ▸ Edit objects: select any object the page's own content draws (a text block, an
//! image, a vector path, a shading, a Form XObject) and change it.
//!
//! Click an object to select it (⇧-click adds or removes it), or drag around several from an
//! empty spot. The selection shows each object's box and, around them all, handles: drag inside
//! to move, a corner to scale (in proportion; ⇧ frees it), an edge to stretch one way, and the
//! round handle above to turn (⇧ snaps to 15°). Delete removes, arrow keys nudge (⇧: 10 pt),
//! ⌘] / ⌘[ bring forward / send backward (with ⇧: to front / to back), Esc deselects. The
//! right-click menu arranges, turns and flips; the Edit panel shows the selection's fill and
//! stroke colour, line width and opacity. Each change is one edit (one undo step) through
//! `Edit::EditPageObjects`, which rewrites only the selected objects.
//!
//! Hit testing uses an [`ObjectIndex`] built once per page and document generation, only for
//! the page under the pointer or holding the selection: hovering and clicking never rescan the
//! content.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke};
use pdfcraft_engine::{Anchor, Arrange, Edit, ObjectIndex, ObjectKind, ObjectSize, ObjectsEdit, PageObject, Paint, PathStyle};
use pdfcraft_render::DocInfo;

use crate::canvas::{DocView, PageXform};
use crate::theme::{self, Tokens};

const ACCENT: Color32 = Color32::from_rgb(0x14, 0x73, 0xE6);
/// How close (screen pixels) the pointer must be to a handle to take it.
const HANDLE_REACH: f32 = 7.0;
/// How far above the selection the rotation handle sits (screen pixels).
const ROTATE_OFFSET: f32 = 24.0;
/// Pages whose objects are kept indexed at once.
const MAX_CACHED: usize = 8;
/// How far (screen pixels) a click may miss an object's box and still select it.
const CLICK_PIXELS: f32 = 3.0;

/// A page's objects and their hit-testing index.
pub(crate) struct PageObjects {
    pub objects: Vec<PageObject>,
    pub index: ObjectIndex,
}

/// What the pointer is doing to the selection.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Gesture {
    /// Dragging a selection rectangle from `start` (screen); `add` keeps the selection.
    Marquee {
        page: usize,
        start: Pos2,
        add: bool,
    },
    Move {
        page: usize,
        start: Pos2,
    },
    /// Dragging handle `handle` (see [`handles`]) of the selection's box `bounds` (screen).
    Resize {
        page: usize,
        bounds: Rect,
        handle: usize,
    },
    /// Turning the selection about `centre` (screen), from where the drag started.
    Rotate {
        page: usize,
        centre: Pos2,
        start: Pos2,
    },
}

/// The Edit object tool's state for one document view.
#[derive(Default)]
pub struct ObjectView {
    cache: HashMap<usize, (u64, Arc<PageObjects>)>,
    /// The page the selection is on, and the selected objects (indexes into its objects).
    pub page: usize,
    pub selected: Vec<usize>,
    /// The document generation the selection refers to.
    generation: u64,
    /// After an arrange: the selected objects' kinds and boxes, to find them in the new order.
    follow: Option<Vec<(ObjectKind, Option<[f64; 4]>)>>,
    gesture: Option<Gesture>,
    /// The Edit panel's line width and opacity while their widgets are dragged.
    pub(crate) width_draft: Option<f64>,
    pub(crate) opacity_draft: Option<f64>,
}

impl ObjectView {
    /// The objects of `page`, indexed: from the cache while the document is unchanged.
    pub(crate) fn objects(&mut self, doc: &pdfcraft_engine::Document, page: usize) -> Arc<PageObjects> {
        let generation = doc.edit_generation();
        if let Some((g, cached)) = self.cache.get(&page)
            && *g == generation
        {
            return cached.clone();
        }
        self.cache.retain(|_, (g, _)| *g == generation);
        if self.cache.len() >= MAX_CACHED {
            self.cache.clear();
        }
        let objects = doc.page_objects(page);
        let index = ObjectIndex::new(&objects);
        let built = Arc::new(PageObjects { objects, index });
        self.cache.insert(page, (generation, built.clone()));
        built
    }

    /// Select `objects` of `page` (0-based indexes), replacing the selection.
    pub fn select(&mut self, page: usize, objects: Vec<usize>, generation: u64) {
        let mut objects = objects;
        objects.sort_unstable();
        objects.dedup();
        self.page = page;
        self.selected = objects;
        self.generation = generation;
        self.follow = None;
        self.gesture = None;
    }

    pub fn clear(&mut self) {
        self.selected.clear();
        self.follow = None;
        self.gesture = None;
        self.width_draft = None;
        self.opacity_draft = None;
    }

    /// Drop everything when the tool is put down.
    pub(crate) fn release(&mut self) {
        if !self.selected.is_empty() || self.gesture.is_some() || !self.cache.is_empty() {
            self.clear();
            self.cache.clear();
        }
    }

    /// Bring the selection up to date with the document: after an arrange the same objects are
    /// found again; after other edits (and undo) objects keep their numbers while they exist.
    pub(crate) fn sync(&mut self, doc: &pdfcraft_engine::Document) {
        let generation = doc.edit_generation();
        if self.generation == generation {
            return;
        }
        self.generation = generation;
        if self.selected.is_empty() {
            self.follow = None;
            return;
        }
        let page = self.page;
        let list = self.objects(doc, page);
        match self.follow.take() {
            Some(follow) => {
                let same = |a: Option<[f64; 4]>, b: Option<[f64; 4]>| match (a, b) {
                    (Some(a), Some(b)) => a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01),
                    (None, None) => true,
                    _ => false,
                };
                self.selected = list
                    .objects
                    .iter()
                    .enumerate()
                    .filter(|(_, o)| follow.iter().any(|(k, r)| *k == o.kind && same(*r, o.rect)))
                    .map(|(i, _)| i)
                    .collect();
            }
            None => self.selected.retain(|k| *k < list.objects.len()),
        }
    }

    /// The edit for `change` on the selection, remembering the selection across an arrange.
    fn edit(&mut self, change: ObjectsEdit) -> Option<Edit> {
        if self.selected.is_empty() {
            return None;
        }
        if matches!(change, ObjectsEdit::Arrange(_)) {
            let list = self.cache.get(&self.page).map(|(_, l)| l.clone());
            self.follow = list.map(|l| self.selected.iter().filter_map(|k| l.objects.get(*k)).map(|o| (o.kind, o.rect)).collect());
        }
        let edit = Edit::EditPageObjects { page: self.page, objects: self.selected.clone(), change: change.clone() };
        if change == ObjectsEdit::Delete {
            self.clear();
        }
        Some(edit)
    }

    /// The selected objects, when they are on `page` of the cached objects.
    fn chosen<'a>(&self, list: &'a PageObjects) -> Vec<&'a PageObject> {
        self.selected.iter().filter_map(|k| list.objects.get(*k)).collect()
    }
}

fn screen_box(xf: &PageXform, info: &DocInfo, page: usize, r: [f64; 4]) -> Rect {
    xf.user_rect(info, page, [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32])
}

/// A screen point in the page's user space.
fn to_user(xf: &PageXform, info: &DocInfo, page: usize, p: Pos2) -> Option<(f64, f64)> {
    let (vx, vy) = xf.screen_to_view(p);
    let [x, y] = info.pages.get(page)?.view_to_user(vx, vy);
    Some((f64::from(x), f64::from(y)))
}

/// A screen rectangle in the page's user space.
fn user_box(xf: &PageXform, info: &DocInfo, page: usize, r: Rect) -> Option<[f64; 4]> {
    let (a, b) = (to_user(xf, info, page, r.min)?, to_user(xf, info, page, r.max)?);
    Some([a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)])
}

/// The resize handles of screen box `b`: corners (top-left, top-right, bottom-left,
/// bottom-right), then edge midpoints (left, top, right, bottom).
fn handles(b: Rect) -> [Pos2; 8] {
    [b.left_top(), b.right_top(), b.left_bottom(), b.right_bottom(), b.left_center(), b.center_top(), b.right_center(), b.center_bottom()]
}

/// The sides handle `h` moves: left, top, right, bottom.
fn sides(h: usize) -> [bool; 4] {
    match h {
        0 => [true, true, false, false],
        1 => [false, true, true, false],
        2 => [true, false, false, true],
        3 => [false, false, true, true],
        4 => [true, false, false, false],
        5 => [false, true, false, false],
        6 => [false, false, true, false],
        _ => [false, false, false, true],
    }
}

fn rotate_handle(b: Rect) -> Pos2 {
    b.center_top() - egui::vec2(0.0, ROTATE_OFFSET)
}

/// Box `b` with handle `h` dragged to `p`: never flipped, never under 4 px; corners keep the
/// proportions unless `free`.
fn resized(b: Rect, h: usize, p: Pos2, free: bool) -> Rect {
    let [l, t, r, bo] = sides(h);
    let mut n = b;
    if l {
        n.min.x = p.x.min(b.max.x - 4.0);
    }
    if r {
        n.max.x = p.x.max(b.min.x + 4.0);
    }
    if t {
        n.min.y = p.y.min(b.max.y - 4.0);
    }
    if bo {
        n.max.y = p.y.max(b.min.y + 4.0);
    }
    if h < 4 && !free {
        let k = (n.width() / b.width().max(1.0)).max(n.height() / b.height().max(1.0)).max(0.01);
        let (w, hh) = (b.width() * k, b.height() * k);
        let x = if l { b.max.x - w } else { b.min.x };
        let y = if t { b.max.y - hh } else { b.min.y };
        n = Rect::from_min_size(Pos2::new(x, y), egui::vec2(w, hh));
    }
    n
}

/// The angle (degrees, clockwise on screen) from `start` to `p` about `centre`; ⇧ snaps to 15°.
fn turn(centre: Pos2, start: Pos2, p: Pos2, snap: bool) -> f32 {
    let a = |q: Pos2| (q.y - centre.y).atan2(q.x - centre.x);
    let mut d = (a(p) - a(start)).to_degrees();
    if d > 180.0 {
        d -= 360.0;
    } else if d < -180.0 {
        d += 360.0;
    }
    if snap { (d / 15.0).round() * 15.0 } else { d }
}

/// `r`'s corners turned `degrees` clockwise about `c` (screen).
fn turned(r: Rect, c: Pos2, degrees: f32) -> Vec<Pos2> {
    let (s, co) = degrees.to_radians().sin_cos();
    [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()]
        .into_iter()
        .map(|p| {
            let (x, y) = (p.x - c.x, p.y - c.y);
            Pos2::new(c.x + x * co - y * s, c.y + x * s + y * co)
        })
        .collect()
}

/// The user-space matrix turning `degrees` clockwise as displayed about user point `c`.
fn turn_matrix(c: (f64, f64), degrees: f64) -> [f64; 6] {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let (a, b, cc, d) = (cos, -sin, sin, cos);
    [a, b, cc, d, c.0 - a * c.0 - cc * c.1, c.1 - b * c.0 - d * c.1]
}

/// The user-space matrix taking box `from` onto box `to`.
fn box_matrix(from: [f64; 4], to: [f64; 4]) -> Option<[f64; 6]> {
    let (w, h) = (from[2] - from[0], from[3] - from[1]);
    if !(w > 1e-6 && h > 1e-6) {
        return None;
    }
    let (sx, sy) = ((to[2] - to[0]) / w, (to[3] - to[1]) / h);
    Some([sx, 0.0, 0.0, sy, to[0] - sx * from[0], to[1] - sy * from[1]]).filter(|m| m.iter().all(|v| v.is_finite()))
}

/// The page's objects for the Edit object tool: hover, select, move, resize, turn. Returns
/// `true` when the pointer was used.
pub(crate) fn page_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    xf: &PageXform,
    page: usize,
    info: &DocInfo,
    doc: &pdfcraft_engine::Document,
    view: &mut DocView,
) -> bool {
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let here = view.objects.page == page;
    let gesture_here = view.objects.gesture.is_some_and(|g| match g {
        Gesture::Marquee { page: p, .. } | Gesture::Move { page: p, .. } | Gesture::Resize { page: p, .. } | Gesture::Rotate { page: p, .. } => {
            p == page
        }
    });
    let over = pointer.is_some_and(|p| xf.rect.expand(ROTATE_OFFSET + HANDLE_REACH).contains(p));
    // Pages without the pointer, the selection or a gesture cost nothing.
    if !over && !gesture_here && !(here && !view.objects.selected.is_empty()) {
        return false;
    }
    view.objects.sync(doc);
    let list = view.objects.objects(doc, page);
    let generation = doc.edit_generation();
    let painter = ui.painter();
    let selected: Vec<usize> = if here { view.objects.selected.clone() } else { Vec::new() };
    let boxes: Vec<Rect> = selected.iter().filter_map(|k| list.objects.get(*k)?.rect).map(|r| screen_box(xf, info, page, r)).collect();
    let bounds = boxes.iter().copied().reduce(|a, b| a.union(b));
    let shift = ui.input(|i| i.modifiers.shift);

    // What is under the pointer.
    // A few screen pixels of slop, in points.
    let per_point = (xf.rect.width().max(xf.rect.height()) / xf.pw.max(xf.ph).max(1.0)).max(0.01);
    let slop = (f64::from(CLICK_PIXELS / per_point)).clamp(0.5, 20.0);
    let hit = pointer.filter(|p| xf.rect.contains(*p)).and_then(|p| to_user(xf, info, page, p)).and_then(|(x, y)| list.index.hit(x, y, slop));

    // Drawing: the hovered object, each selected object's box, and the handles.
    if let Some(h) = hit.filter(|h| !selected.contains(h))
        && let Some(r) = list.objects.get(h).and_then(|o| o.rect)
        && view.objects.gesture.is_none()
    {
        painter.rect_stroke(
            screen_box(xf, info, page, r),
            CornerRadius::ZERO,
            Stroke::new(1.0, ACCENT.gamma_multiply(0.6)),
            egui::StrokeKind::Outside,
        );
    }
    for b in &boxes {
        painter.rect_stroke(*b, CornerRadius::ZERO, Stroke::new(1.0, ACCENT), egui::StrokeKind::Outside);
    }
    if let Some(b) = bounds
        && view.objects.gesture.is_none()
    {
        painter.rect_stroke(b, CornerRadius::ZERO, Stroke::new(1.0, ACCENT.gamma_multiply(0.5)), egui::StrokeKind::Outside);
        painter.line_segment([b.center_top(), rotate_handle(b)], Stroke::new(1.0, ACCENT));
        painter.circle(rotate_handle(b), 4.5, Color32::WHITE, Stroke::new(1.0, ACCENT));
        for c in handles(b) {
            painter.rect(
                Rect::from_center_size(c, egui::vec2(7.0, 7.0)),
                CornerRadius::ZERO,
                Color32::WHITE,
                Stroke::new(1.0, ACCENT),
                egui::StrokeKind::Middle,
            );
        }
    }

    // Cursor over handles and objects.
    if let Some(p) = pointer
        && view.objects.gesture.is_none()
    {
        let near = bounds.and_then(|b| handles(b).iter().position(|c| c.distance(p) < HANDLE_REACH));
        if bounds.is_some_and(|b| rotate_handle(b).distance(p) < HANDLE_REACH) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Alias);
        } else if let Some(h) = near {
            ui.ctx().set_cursor_icon(match h {
                0 | 3 => egui::CursorIcon::ResizeNwSe,
                1 | 2 => egui::CursorIcon::ResizeNeSw,
                4 | 6 => egui::CursorIcon::ResizeHorizontal,
                _ => egui::CursorIcon::ResizeVertical,
            });
        } else if hit.is_some() || bounds.is_some_and(|b| b.contains(p)) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
        } else if xf.rect.contains(p) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    }

    // Starting a gesture.
    if resp.drag_started()
        && let Some(o) = ui.input(|i| i.pointer.press_origin())
        && xf.rect.expand(ROTATE_OFFSET + HANDLE_REACH).contains(o)
    {
        let start_hit = to_user(xf, info, page, o).and_then(|(x, y)| list.index.hit(x, y, slop));
        let handle = bounds.and_then(|b| handles(b).iter().position(|c| c.distance(o) < HANDLE_REACH));
        let gesture = if let Some(b) = bounds.filter(|b| rotate_handle(*b).distance(o) < HANDLE_REACH) {
            Some(Gesture::Rotate { page, centre: b.center(), start: o })
        } else if let (Some(b), Some(h)) = (bounds, handle) {
            Some(Gesture::Resize { page, bounds: b, handle: h })
        } else if boxes.iter().any(|r| r.contains(o)) {
            Some(Gesture::Move { page, start: o })
        } else if !xf.rect.contains(o) {
            None
        } else if let Some(k) = start_hit {
            // Drag an unselected object: it becomes the selection and moves.
            view.objects.select(page, vec![k], generation);
            Some(Gesture::Move { page, start: o })
        } else {
            Some(Gesture::Marquee { page, start: o, add: shift })
        };
        view.objects.gesture = gesture;
    }

    // A gesture in progress: preview, and commit on release.
    if let Some(g) = view.objects.gesture.filter(|_| gesture_here || here)
        && let Some(p) = pointer.or_else(|| ui.input(|i| i.pointer.interact_pos()))
    {
        let released = resp.drag_stopped() || !ui.input(|i| i.pointer.primary_down());
        let mut edit = None;
        let chosen = view.objects.chosen(&list).len();
        match g {
            Gesture::Marquee { start, add, .. } => {
                let r = Rect::from_two_pos(start, p);
                painter.rect(r, CornerRadius::ZERO, ACCENT.gamma_multiply(0.08), Stroke::new(1.0, ACCENT), egui::StrokeKind::Middle);
                if released {
                    let mut found = user_box(xf, info, page, r).map(|u| list.index.touching(u)).unwrap_or_default();
                    if add && here {
                        found.extend(view.objects.selected.iter().copied());
                    }
                    view.objects.select(page, found, generation);
                }
            }
            Gesture::Move { start, .. } => {
                let delta = p - start;
                for b in &boxes {
                    painter.rect_stroke(b.translate(delta), CornerRadius::ZERO, Stroke::new(1.0, ACCENT), egui::StrokeKind::Middle);
                }
                if released && delta.length() >= 1.0 {
                    let ((x0, y0), (x1, y1)) = (xf.screen_to_view(start), xf.screen_to_view(p));
                    let (dx, dy) = (f64::from(x1 - x0), f64::from(y1 - y0));
                    // View space runs down the page; display space runs up.
                    edit = view.objects.edit(ObjectsEdit::Move { dx, dy: -dy });
                }
            }
            Gesture::Resize { bounds: b, handle, .. } => {
                let n = resized(b, handle, p, shift);
                painter.rect_stroke(n, CornerRadius::ZERO, Stroke::new(1.0, ACCENT), egui::StrokeKind::Middle);
                if released && n != b {
                    let matrix = user_box(xf, info, page, b).zip(user_box(xf, info, page, n)).and_then(|(from, to)| box_matrix(from, to));
                    if let Some(m) = matrix {
                        edit = view.objects.edit(if chosen == 1 {
                            // One object: scale it about the opposite side, a "Resize objects" step.
                            let [fx, fy] = [m[0], m[3]];
                            let anchor = anchor_for(xf, b, handle);
                            ObjectsEdit::Resize { size: display_scale(info, page, fx, fy), anchor }
                        } else {
                            ObjectsEdit::Matrix(m)
                        });
                    }
                }
            }
            Gesture::Rotate { centre, start, .. } => {
                let d = turn(centre, start, p, shift);
                if let Some(b) = bounds {
                    painter.add(egui::Shape::closed_line(turned(b, centre, d), Stroke::new(1.0, ACCENT)));
                }
                painter.text(p + egui::vec2(12.0, 12.0), egui::Align2::LEFT_TOP, format!("{d:.0}°"), theme::regular(12.0), ACCENT);
                if released && d.abs() >= 0.5 {
                    edit = if chosen == 1 {
                        view.objects.edit(ObjectsEdit::RotateBy(f64::from(d)))
                    } else {
                        to_user(xf, info, page, centre).and_then(|c| view.objects.edit(ObjectsEdit::Matrix(turn_matrix(c, f64::from(d)))))
                    };
                }
            }
        }
        if released {
            view.objects.gesture = None;
        }
        if edit.is_some() {
            view.pending_edit = edit;
        }
        return true;
    }

    // Clicks select.
    if (resp.clicked() || resp.secondary_clicked()) && pointer.is_some_and(|p| xf.rect.contains(p)) {
        let keep = resp.secondary_clicked() && hit.is_some_and(|h| here && selected.contains(&h));
        if !keep {
            match (hit, shift && here) {
                (Some(h), true) => {
                    let mut s = view.objects.selected.clone();
                    if let Some(at) = s.iter().position(|k| *k == h) {
                        s.remove(at);
                    } else {
                        s.push(h);
                    }
                    view.objects.select(page, s, generation);
                }
                (Some(h), false) => view.objects.select(page, vec![h], generation),
                (None, _) => view.objects.clear(),
            }
        }
    }
    if here && !view.objects.selected.is_empty() {
        let mut picked = None;
        resp.context_menu(|ui| {
            picked = context_menu(ui);
        });
        if let Some(change) = picked {
            view.pending_edit = view.objects.edit(change);
        }
    }
    pointer.is_some_and(|p| xf.rect.contains(p) || bounds.is_some_and(|b| b.expand(ROTATE_OFFSET + HANDLE_REACH).contains(p)))
}

/// The display-space scale for screen factors `fx`, `fy` of user space (a quarter-turned page
/// swaps them).
fn display_scale(info: &DocInfo, page: usize, fx: f64, fy: f64) -> ObjectSize {
    let turned = info.pages.get(page).is_some_and(|p| p.rotation % 180 == 90);
    if turned { ObjectSize::Scale { sx: fy, sy: fx } } else { ObjectSize::Scale { sx: fx, sy: fy } }
}

/// The anchor of a one-object resize: the point of its displayed box opposite handle `h`.
fn anchor_for(xf: &PageXform, b: Rect, h: usize) -> Anchor {
    let [l, t, r, bo] = sides(h);
    // The fixed point on screen, then where it sits in the displayed (view-space) box.
    let x = if l {
        b.max.x
    } else if r {
        b.min.x
    } else {
        b.center().x
    };
    let y = if t {
        b.max.y
    } else if bo {
        b.min.y
    } else {
        b.center().y
    };
    let (fx, fy) = xf.screen_to_view(Pos2::new(x, y));
    let (v0, v1) = (xf.screen_to_view(b.min), xf.screen_to_view(b.max));
    let (x0, x1, y0, y1) = (v0.0.min(v1.0), v0.0.max(v1.0), v0.1.min(v1.1), v0.1.max(v1.1));
    let at = |v: f32, lo: f32, hi: f32| {
        let w = (hi - lo).max(1e-3);
        let k = (v - lo) / w;
        if k < 0.25 {
            0
        } else if k > 0.75 {
            2
        } else {
            1
        }
    };
    // View y runs down: 0 is the top.
    match (at(fx, x0, x1), at(fy, y0, y1)) {
        (0, 0) => Anchor::TopLeft,
        (1, 0) => Anchor::Top,
        (2, 0) => Anchor::TopRight,
        (0, 1) => Anchor::Left,
        (2, 1) => Anchor::Right,
        (0, 2) => Anchor::BottomLeft,
        (1, 2) => Anchor::Bottom,
        (2, 2) => Anchor::BottomRight,
        _ => Anchor::Center,
    }
}

/// The right-click menu on a selection.
fn context_menu(ui: &mut egui::Ui) -> Option<ObjectsEdit> {
    let items: [(&str, ObjectsEdit); 8] = [
        (tl!("Bring to Front"), ObjectsEdit::Arrange(Arrange::Front)),
        (tl!("Bring Forward"), ObjectsEdit::Arrange(Arrange::Forward)),
        (tl!("Send Backward"), ObjectsEdit::Arrange(Arrange::Backward)),
        (tl!("Send to Back"), ObjectsEdit::Arrange(Arrange::Back)),
        (tl!("Rotate Clockwise"), ObjectsEdit::Rotate(1)),
        (tl!("Rotate Counterclockwise"), ObjectsEdit::Rotate(3)),
        (tl!("Flip Horizontal"), ObjectsEdit::Flip { horizontal: true }),
        (tl!("Flip Vertical"), ObjectsEdit::Flip { horizontal: false }),
    ];
    let mut picked = None;
    for (k, (label, change)) in items.into_iter().enumerate() {
        if k == 4 {
            ui.separator();
        }
        if ui.button(label).clicked() {
            picked = Some(change);
            ui.close();
        }
    }
    ui.separator();
    if ui.button(tl!("Delete")).clicked() {
        picked = Some(ObjectsEdit::Delete);
        ui.close();
    }
    picked
}

/// Keys for a selection: Delete, arrows (nudge), ⌘] / ⌘[ (arrange), Esc. Returns `true` when a
/// key was used, so the view's own keys (page turns, history) leave it alone.
pub(crate) fn keys(view: &mut DocView, ctx: &egui::Context) -> bool {
    use egui::{Key, Modifiers};
    if view.objects.selected.is_empty() {
        return false;
    }
    let mut change = None;
    let mut used = false;
    ctx.input_mut(|i| {
        if i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace) {
            change = Some(ObjectsEdit::Delete);
        } else if i.consume_key(Modifiers::NONE, Key::Escape) {
            used = true;
        } else if i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::CloseBracket) {
            change = Some(ObjectsEdit::Arrange(Arrange::Front));
        } else if i.consume_key(Modifiers::COMMAND, Key::CloseBracket) {
            change = Some(ObjectsEdit::Arrange(Arrange::Forward));
        } else if i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::OpenBracket) {
            change = Some(ObjectsEdit::Arrange(Arrange::Back));
        } else if i.consume_key(Modifiers::COMMAND, Key::OpenBracket) {
            change = Some(ObjectsEdit::Arrange(Arrange::Backward));
        } else {
            for shift in [false, true] {
                let m = if shift { Modifiers::SHIFT } else { Modifiers::NONE };
                let step = if shift { 10.0 } else { 1.0 };
                for (key, dx, dy) in
                    [(Key::ArrowLeft, -step, 0.0), (Key::ArrowRight, step, 0.0), (Key::ArrowUp, 0.0, step), (Key::ArrowDown, 0.0, -step)]
                {
                    if change.is_none() && i.consume_key(m, key) {
                        change = Some(ObjectsEdit::Move { dx, dy });
                    }
                }
            }
        }
    });
    if used {
        view.objects.clear();
        return true;
    }
    match change {
        Some(c) => {
            view.pending_edit = view.objects.edit(c);
            true
        }
        None => false,
    }
}

/// Swatches offered for fill and stroke: none, then device colours.
const SWATCHES: [Option<[u8; 3]>; 10] = [
    None,
    Some([0, 0, 0]),
    Some([255, 255, 255]),
    Some([128, 128, 128]),
    Some([0xE0, 0x31, 0x31]),
    Some([0xF0, 0x8C, 0x00]),
    Some([0xF5, 0xC5, 0x18]),
    Some([0x2F, 0x9E, 0x44]),
    Some([0x14, 0x73, 0xE6]),
    Some([0x86, 0x3B, 0xD9]),
];

/// A paint as an on-screen colour (approximate for CMYK and other spaces).
fn shown(p: &Paint) -> Option<Color32> {
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    match p {
        Paint::None => None,
        Paint::Gray(g) => Some(Color32::from_gray(byte(*g))),
        Paint::Rgb([r, g, b]) => Some(Color32::from_rgb(byte(*r), byte(*g), byte(*b))),
        Paint::Cmyk([c, m, y, k]) => Some(Color32::from_rgb(byte((1.0 - c) * (1.0 - k)), byte((1.0 - m) * (1.0 - k)), byte((1.0 - y) * (1.0 - k)))),
        Paint::Current(_) => Some(Color32::from_gray(160)),
    }
}

/// A row of colour swatches; returns the one clicked.
fn swatches(ui: &mut egui::Ui, t: &Tokens, current: Option<&Paint>, id: &str) -> Option<Paint> {
    let mut picked = None;
    let now = current.and_then(shown);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for s in &SWATCHES {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
            let label = match s {
                None => tl!("No color").to_string(),
                Some([r, g, b]) => format!("#{r:02X}{g:02X}{b:02X}"),
            };
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{id} {label}")));
            let c = rect.center();
            match s {
                None => {
                    ui.painter().circle(c, 8.0, t.card, Stroke::new(1.0, t.border));
                    ui.painter()
                        .line_segment([c + egui::vec2(-5.5, 5.5), c + egui::vec2(5.5, -5.5)], Stroke::new(1.5, Color32::from_rgb(0xE0, 0x31, 0x31)));
                }
                Some([r, g, b]) => {
                    ui.painter().circle(c, 8.0, Color32::from_rgb(*r, *g, *b), Stroke::new(1.0, t.border));
                }
            }
            let active = match (s, current) {
                (None, Some(Paint::None)) => true,
                (Some([r, g, b]), _) => now == Some(Color32::from_rgb(*r, *g, *b)),
                _ => false,
            };
            if active || resp.hovered() {
                ui.painter().circle_stroke(c, 10.0, Stroke::new(1.5, if active { t.accent } else { t.text_faint }));
            }
            if resp.on_hover_text(label).clicked() {
                picked = Some(match s {
                    None => Paint::None,
                    Some([r, g, b]) => Paint::Rgb([f64::from(*r) / 255.0, f64::from(*g) / 255.0, f64::from(*b) / 255.0]),
                });
            }
        }
    });
    picked
}

/// The Edit panel's object properties for the selection: what it is, arrange and turn buttons,
/// and the paths' fill, stroke and line width and every object's opacity.
pub(crate) fn panel(app: &mut crate::PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if app.quick_tool != crate::QuickTool::EditObject {
        return;
    }
    let Some((index, id)) = app.active_ids() else { return };
    let Some(doc) = app.session.get(id) else { return };
    let Some(view) = app.views.get_mut(index) else { return };
    view.objects.sync(doc);
    crate::widgets::section_title(ui, tl!("Object properties"));
    if view.objects.selected.is_empty() {
        ui.label(egui::RichText::new(tl!("Click an object to select it, or drag around several")).small().color(t.text_faint));
        ui.add_space(6.0);
        ui.separator();
        return;
    }
    let page = view.objects.page;
    let list = view.objects.objects(doc, page);
    let chosen = view.objects.chosen(&list);
    let count = chosen.len();
    let kind = match chosen.as_slice() {
        [one] => match one.kind {
            ObjectKind::Text => tl!("Text"),
            ObjectKind::Path => tl!("Path"),
            ObjectKind::Image | ObjectKind::InlineImage => tl!("Image"),
            ObjectKind::Form => tl!("Form XObject"),
            ObjectKind::Shading => tl!("Shading"),
            ObjectKind::Group => tl!("Tagged content"),
        }
        .to_string(),
        _ => crate::i18n::fmt(tl!("{n} objects selected"), &[("n", &count.to_string())]),
    };
    ui.label(egui::RichText::new(kind).color(t.text).font(theme::semibold(13.0)));
    if let Some(r) = chosen.iter().filter_map(|o| o.rect).reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]) {
        ui.label(
            egui::RichText::new(format!("{} {:.1} × {} {:.1} pt", tl!("Width"), r[2] - r[0], tl!("Height"), r[3] - r[1])).small().color(t.text_muted),
        );
    }
    let mut change: Option<ObjectsEdit> = None;
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Arrange")).small().color(t.text_muted));
    ui.horizontal_wrapped(|ui| {
        let arrange: [(&str, Arrange); 4] = [
            (tl!("Bring to Front"), Arrange::Front),
            (tl!("Bring Forward"), Arrange::Forward),
            (tl!("Send Backward"), Arrange::Backward),
            (tl!("Send to Back"), Arrange::Back),
        ];
        for (label, a) in arrange {
            if ui.small_button(label).clicked() {
                change = Some(ObjectsEdit::Arrange(a));
            }
        }
    });
    ui.horizontal(|ui| {
        let buttons: [(&str, &str, ObjectsEdit); 3] = [
            ("rotate-cw", tl!("Rotate Clockwise"), ObjectsEdit::Rotate(1)),
            ("flip-horizontal-2", tl!("Flip Horizontal"), ObjectsEdit::Flip { horizontal: true }),
            ("trash-2", tl!("Delete"), ObjectsEdit::Delete),
        ];
        for (icon, label, c) in buttons {
            if crate::icons::button(ui, icon, 28.0, false, label).clicked() {
                change = Some(c);
            }
        }
    });
    // Fill, stroke and line width: the first selected path shows its own.
    let look = chosen.iter().find_map(|o| o.appearance.clone());
    let opacity = chosen.first().map_or(1.0, |o| o.opacity);
    let paths = chosen.iter().any(|o| o.appearance.is_some());
    let style = |f: &dyn Fn(&mut PathStyle)| {
        let mut s = PathStyle::default();
        f(&mut s);
        ObjectsEdit::Style(s)
    };
    if paths {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("Fill color")).small().color(t.text_muted));
        if let Some(p) = swatches(ui, t, look.as_ref().map(|a| &a.fill), "fill") {
            change = Some(style(&|s| s.fill = Some(p.clone())));
        }
        ui.label(egui::RichText::new(tl!("Stroke color")).small().color(t.text_muted));
        if let Some(p) = swatches(ui, t, look.as_ref().map(|a| &a.stroke), "stroke") {
            change = Some(style(&|s| s.stroke = Some(p.clone())));
        }
        let mut width = view.objects.width_draft.unwrap_or_else(|| look.as_ref().map_or(1.0, |a| a.line_width));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tl!("Line width")).small().color(t.text_muted));
            let r = ui.add(egui::DragValue::new(&mut width).range(0.0..=100.0).speed(0.1).max_decimals(2).suffix(" pt"));
            if r.changed() {
                view.objects.width_draft = Some(width);
            }
            if (r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus()))
                && let Some(w) = view.objects.width_draft.take()
            {
                change = Some(style(&|s| s.line_width = Some(w)));
            }
        });
    }
    ui.add_space(4.0);
    let mut alpha = view.objects.opacity_draft.unwrap_or(opacity) * 100.0;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Opacity")).small().color(t.text_muted));
        let r = ui.add(egui::Slider::new(&mut alpha, 0.0..=100.0).suffix("%").max_decimals(0));
        if r.changed() {
            view.objects.opacity_draft = Some(alpha / 100.0);
        }
        if (r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus()))
            && let Some(o) = view.objects.opacity_draft.take()
        {
            change = Some(style(&|s| s.opacity = Some(o)));
        }
    });
    ui.add_space(6.0);
    ui.separator();
    if let Some(c) = change
        && let Some(edit) = view.objects.edit(c)
    {
        app.apply_edit(edit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_keep_proportions_and_never_flip() {
        let b = Rect::from_min_size(Pos2::new(10.0, 10.0), egui::vec2(100.0, 50.0));
        let r = resized(b, 3, Pos2::new(210.0, 70.0), false);
        assert_eq!((r.min, r.width(), r.height()), (b.min, 200.0, 100.0));
        let r = resized(b, 3, Pos2::new(210.0, 70.0), true);
        assert_eq!((r.width(), r.height()), (200.0, 60.0));
        let r = resized(b, 0, Pos2::new(500.0, 500.0), true);
        assert!(r.width() >= 4.0 && r.height() >= 4.0 && r.max == b.max, "{r:?}");
        let r = resized(b, 4, Pos2::new(f32::NAN, 0.0), false);
        assert!(r.max == b.max);
    }

    #[test]
    fn turning_snaps_and_wraps() {
        let c = Pos2::new(0.0, 0.0);
        assert!((turn(c, Pos2::new(10.0, 0.0), Pos2::new(0.0, 10.0), false) - 90.0).abs() < 1e-3, "clockwise on screen");
        assert_eq!(turn(c, Pos2::new(10.0, 0.0), Pos2::new(10.0, 2.0), true), 15.0);
        assert!(turn(c, Pos2::new(-10.0, 1.0), Pos2::new(-10.0, -1.0), false).abs() < 20.0);
    }

    #[test]
    fn box_matrices_map_boxes_and_refuse_degenerate_ones() {
        let m = box_matrix([0.0, 0.0, 10.0, 10.0], [5.0, 5.0, 25.0, 15.0]).unwrap();
        assert_eq!(m, [2.0, 0.0, 0.0, 1.0, 5.0, 5.0]);
        assert!(box_matrix([0.0, 0.0, 0.0, 10.0], [0.0, 0.0, 1.0, 1.0]).is_none());
        assert!(box_matrix([0.0, 0.0, 1e-300, 1e-300], [0.0, 0.0, 1e300, 1e300]).is_none());
        let t = turn_matrix((5.0, 5.0), 90.0);
        assert!((t[0]).abs() < 1e-12 && (t[1] + 1.0).abs() < 1e-12 && (t[2] - 1.0).abs() < 1e-12);
    }
}
