//! Compare side by side: the older document on the left, the newer on the right, row by row
//! as the comparison's page map lines them up (a moved page sits beside its counterpart, an
//! inserted or deleted page beside a gap). Scrolling and zoom are synchronised unless turned
//! off; every change is shaded on both sides; Next / Previous change scroll both panes to it;
//! the change list beside the panes selects and follows the current change.
//!
//! Only rows on screen (and one screenful either way, as prefetch) are looked at and asked
//! for: whole-page rasters from each document's render pool through its own view's cache, the
//! two views splitting the application's raster allowance between them
//! ([`crate::canvas::DocView::raster_share`]). The row layout is computed once per zoom.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use pdfcraft_engine::DocId;
use pdfcraft_engine::compare::{FileComparison, Kind, PageStatus};
use pdfcraft_engine::compare_view::{CompareView, ViewChange, ViewKind};
use pdfcraft_render::DocInfo;

use crate::canvas::{PT, PageXform};
use crate::compare_ui::{VISUAL, colour};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

const MARGIN: f32 = 20.0;
const GAP: f32 = 14.0;
const TOOLBAR_H: f32 = 40.0;
const HEADER_H: f32 = 26.0;
const LIST_W: f32 = 260.0;
const LIST_ROW_H: f32 = 40.0;
/// Pages are drawn from one whole-page raster at most this many device pixels on a side (a
/// deeper zoom shows it enlarged, as the single view does before its tiles arrive).
const MAX_SIDE: f32 = 4096.0;
const ZOOM: std::ops::RangeInclusive<f32> = 0.08..=16.0;
const ZOOM_STEP: f32 = 1.25;
/// The current change is put this far down the pane.
const FOCUS_AT: f32 = 0.3;

/// Which pane: the older document or the newer one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Old = 0,
    New = 1,
}

impl Pane {
    const BOTH: [Pane; 2] = [Pane::Old, Pane::New];

    fn i(self) -> usize {
        self as usize
    }

    fn other(self) -> Pane {
        match self {
            Pane::Old => Pane::New,
            Pane::New => Pane::Old,
        }
    }
}

/// Row geometry of one pane at one zoom (content coordinates, points).
#[derive(Default)]
struct Rows {
    zoom: f32,
    pages: (usize, usize),
    tops: Vec<f32>,
    heights: Vec<f32>,
    width: f32,
    height: f32,
}

/// The side-by-side view's state.
pub struct SideBySide {
    pub old: DocId,
    pub new: DocId,
    pub model: CompareView,
    /// Scrolling and zoom follow each other.
    pub sync: bool,
    /// Zoom of each pane (equal while synchronised).
    pub zoom: [f32; 2],
    /// The current change (index into the model's changes).
    pub current: Option<usize>,
    /// Show the change list beside the panes.
    pub show_list: bool,
    /// Scroll offsets seen last frame, and offsets to apply next.
    offset: [Vec2; 2],
    force: [Option<Vec2>; 2],
    /// Scroll both panes to the current change once the panes are laid out.
    goto: bool,
    /// Bring the current change into view in the list.
    list_follow: bool,
    fit_pending: bool,
    rows: [Rows; 2],
    /// Pages on screen last frame, per pane: (document page, screen rect).
    pub(crate) on_screen: [Vec<(usize, Rect)>; 2],
    panes: [Rect; 2],
}

impl SideBySide {
    pub fn new(old: DocId, new: DocId, result: &FileComparison) -> Self {
        SideBySide {
            old,
            new,
            model: CompareView::new(result),
            sync: true,
            zoom: [1.0; 2],
            current: None,
            show_list: true,
            offset: [Vec2::ZERO; 2],
            force: [None; 2],
            goto: false,
            list_follow: false,
            fit_pending: true,
            rows: Default::default(),
            on_screen: Default::default(),
            panes: [Rect::NOTHING; 2],
        }
    }

    /// Make change `i` current and scroll both panes (and the list) to it.
    pub fn go_to(&mut self, i: usize) {
        if i < self.model.changes.len() {
            self.current = Some(i);
            self.goto = true;
            self.list_follow = true;
        }
    }

    /// Next (or previous) change, wrapping around. Returns whether there is one.
    pub fn step(&mut self, forward: bool) -> bool {
        match self.model.step(self.current, forward) {
            Some(i) => {
                self.go_to(i);
                true
            }
            None => false,
        }
    }

    pub fn set_sync(&mut self, on: bool) {
        self.sync = on;
        if on {
            // The old pane follows the new one.
            self.zoom[Pane::Old.i()] = self.zoom[Pane::New.i()];
            self.force[Pane::Old.i()] = Some(self.offset[Pane::New.i()]);
        }
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        let z = if zoom.is_finite() { zoom.clamp(*ZOOM.start(), *ZOOM.end()) } else { 1.0 };
        self.zoom = [z; 2];
        self.fit_pending = false;
        if self.current.is_some() {
            self.goto = true;
        }
    }

    fn zoom_pane(&mut self, pane: Pane, factor: f32) {
        let z = (self.zoom[pane.i()] * factor).clamp(*ZOOM.start(), *ZOOM.end());
        if self.sync {
            self.zoom = [z; 2];
        } else {
            self.zoom[pane.i()] = z;
        }
        self.fit_pending = false;
    }

    /// The old and the new pane on screen last frame (tests and automation).
    pub fn pane_rects(&self) -> [Rect; 2] {
        self.panes
    }

    /// For automation and the control channel.
    pub fn state(&self) -> serde_json::Value {
        let spot = |s: &Option<pdfcraft_engine::compare_view::Spot>| s.as_ref().map(|s| serde_json::json!({ "page": s.page + 1, "row": s.row + 1 }));
        let screen = |p: Pane| {
            self.on_screen[p.i()]
                .iter()
                .map(|(page, r)| serde_json::json!({ "page": page + 1, "rect": [r.min.x, r.min.y, r.max.x, r.max.y] }))
                .collect::<Vec<_>>()
        };
        serde_json::json!({
            "old_doc": self.old.0,
            "new_doc": self.new.0,
            "sync": self.sync,
            "zoom_percent": [(self.zoom[0] * 100.0).round(), (self.zoom[1] * 100.0).round()],
            "rows": self.model.rows.len(),
            "changes": self.model.changes.len(),
            "current": self.current.map(|i| i + 1),
            "current_change": self.current.and_then(|i| self.model.changes.get(i)).map(|c| serde_json::json!({
                "kind": c.kind.name(), "old": spot(&c.old), "new": spot(&c.new),
            })),
            "change_list": self.show_list,
            // Scroll offsets of the old and the new pane (points), equal while synchronised.
            "scroll": [[self.offset[0].x, self.offset[0].y], [self.offset[1].x, self.offset[1].y]],
            "old_pages_on_screen": screen(Pane::Old),
            "new_pages_on_screen": screen(Pane::New),
        })
    }

    fn rows_for(&mut self, pane: Pane, old: &DocInfo, new: &DocInfo) {
        let zoom = self.zoom[pane.i()];
        let r = &self.rows[pane.i()];
        if r.zoom == zoom && r.pages == (old.pages.len(), new.pages.len()) && r.tops.len() == self.model.rows.len() {
            return;
        }
        let s = zoom * PT;
        let size = |info: &DocInfo, p: Option<usize>| p.and_then(|p| info.pages.get(p)).map(|p| (p.width.max(1.0), p.height.max(1.0)));
        let mut rows = Rows { zoom, pages: (old.pages.len(), new.pages.len()), ..Default::default() };
        let mut y = MARGIN;
        let mut w = 0.0f32;
        for row in &self.model.rows {
            let (a, b) = (size(old, row.old), size(new, row.new));
            let h = a.map_or(0.0, |a| a.1).max(b.map_or(0.0, |b| b.1)).max(72.0) * s;
            // This pane's page, or the other side's size for its gap.
            let mine = if pane == Pane::Old { a.or(b) } else { b.or(a) };
            w = w.max(mine.map_or(612.0, |m| m.0) * s);
            rows.tops.push(y);
            rows.heights.push(h);
            y += h + GAP;
        }
        rows.width = w + 2.0 * MARGIN;
        rows.height = y - GAP + MARGIN;
        self.rows[pane.i()] = rows;
    }

    fn fit_width(&mut self, old: &DocInfo, new: &DocInfo, pane_w: f32) {
        let widest =
            |info: &DocInfo, pages: &mut dyn Iterator<Item = usize>| pages.filter_map(|p| info.pages.get(p)).map(|p| p.width).fold(1.0f32, f32::max);
        let w = widest(old, &mut self.model.rows.iter().filter_map(|r| r.old)).max(widest(new, &mut self.model.rows.iter().filter_map(|r| r.new)));
        let z = ((pane_w - 2.0 * MARGIN - 12.0).max(80.0) / (w * PT)).clamp(*ZOOM.start(), *ZOOM.end());
        self.zoom = [z; 2];
    }

    /// Scroll offsets that put the current change `FOCUS_AT` down each pane.
    fn scroll_to_current(&mut self, old: &DocInfo, new: &DocInfo) {
        let Some(ch) = self.current.and_then(|i| self.model.changes.get(i)).cloned() else { return };
        let mut target = [None, None];
        for pane in Pane::BOTH {
            let (spot, info) = if pane == Pane::Old { (&ch.old, old) } else { (&ch.new, new) };
            let Some(spot) = spot else { continue };
            let rows = &self.rows[pane.i()];
            let (Some(&top), Some(p)) = (rows.tops.get(spot.row), info.pages.get(spot.page)) else { continue };
            let s = self.zoom[pane.i()] * PT;
            let page = Rect::from_min_size(pos2(page_x(rows.width, p.width * s), top), vec2(p.width * s, p.height * s));
            let focus = spot.focus.filter(|r| r.iter().all(|v| v.is_finite())).map(|r| user_rect(info, spot.page, page, r));
            let view = self.panes[pane.i()].size() - vec2(0.0, HEADER_H);
            let y = focus.map_or(page.top() - GAP, |f| f.top() - view.y.max(0.0) * FOCUS_AT);
            let x = match focus {
                Some(f) if f.right() > self.offset[pane.i()].x + view.x || f.left() < self.offset[pane.i()].x => f.center().x - view.x / 2.0,
                _ => self.offset[pane.i()].x,
            };
            let max = vec2((rows.width - view.x).max(0.0), (rows.height - view.y).max(0.0));
            target[pane.i()] = Some(vec2(x.clamp(0.0, max.x), y.clamp(0.0, max.y)));
        }
        if self.sync {
            // Both panes share the rows: the new side leads, the old side when it has none.
            if let Some(t) = target[Pane::New.i()].or(target[Pane::Old.i()]) {
                self.force = [Some(t); 2];
            }
        } else {
            for pane in Pane::BOTH {
                if let Some(t) = target[pane.i()] {
                    self.force[pane.i()] = Some(t);
                }
            }
        }
    }
}

/// Where a page of `w` points sits horizontally in a pane `content_w` wide.
fn page_x(content_w: f32, w: f32) -> f32 {
    ((content_w - w) / 2.0).max(MARGIN)
}

/// A user-space box on `page` drawn at `screen` → screen rect (crop box and /Rotate applied).
fn user_rect(info: &DocInfo, page: usize, screen: Rect, r: [f64; 4]) -> Rect {
    let Some(p) = info.pages.get(page) else { return Rect::NOTHING };
    let xf = PageXform { rect: screen, rot: 0, pw: p.width.max(1.0), ph: p.height.max(1.0) };
    xf.user_rect(info, page, r.map(|v| v as f32))
}

/// A change's line in the list: what changed, and on which pages.
fn change_text(ch: &ViewChange, result: &FileComparison) -> (String, String) {
    let short = |s: &str| {
        let mut out: String = s.chars().take(60).collect();
        if s.chars().nth(60).is_some() {
            out.push('…');
        }
        out
    };
    let title = match ch.kind {
        ViewKind::Text(_) => match ch.text.and_then(|i| result.text.changes.get(i)) {
            Some(c) => match c.kind {
                Kind::Replaced => format!("\"{}\" → \"{}\"", short(&c.old.text), short(&c.new.text)),
                Kind::Inserted => format!("+ \"{}\"", short(&c.new.text)),
                Kind::Deleted => format!("− \"{}\"", short(&c.old.text)),
            },
            None => String::new(),
        },
        ViewKind::Visual => tl!("Looks different").to_string(),
        ViewKind::Page(PageStatus::Inserted) => tl!("Page inserted").to_string(),
        ViewKind::Page(PageStatus::Deleted) => tl!("Page deleted").to_string(),
        ViewKind::Page(_) => tl!("Page moved").to_string(),
    };
    let num = |s: &Option<pdfcraft_engine::compare_view::Spot>| s.as_ref().map_or_else(|| "—".to_string(), |s| (s.page + 1).to_string());
    let pages = crate::i18n::fmt(tl!("Old page {o} · new page {n}"), &[("o", &num(&ch.old)), ("n", &num(&ch.new))]);
    (title, pages)
}

fn kind_colour(kind: ViewKind, t: &Tokens) -> Color32 {
    match kind {
        ViewKind::Text(k) => colour(k),
        ViewKind::Visual => VISUAL,
        ViewKind::Page(PageStatus::Inserted) => colour(Kind::Inserted),
        ViewKind::Page(PageStatus::Deleted) => colour(Kind::Deleted),
        ViewKind::Page(_) => t.accent,
    }
}

/// What the view asks the app for this frame.
enum Action {
    Close,
}

/// Draw the side-by-side view in the document area.
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let Some(mut v) = app.side_by_side.take() else { return };
    let action = draw(app, &mut v, ui);
    match action {
        Some(Action::Close) => {}
        None => app.side_by_side = Some(v),
    }
}

fn draw(app: &mut PdfCraftApp, v: &mut SideBySide, ui: &mut egui::Ui) -> Option<Action> {
    let t = Tokens::get(ui.ctx());
    let (Some(oi), Some(ni)) = (app.views.iter().position(|x| x.id == v.old), app.views.iter().position(|x| x.id == v.new)) else {
        return Some(Action::Close);
    };
    let (Some(old_doc), Some(new_doc)) = (app.session.get(v.old), app.session.get(v.new)) else { return Some(Action::Close) };
    let (old_info, new_info) = (&old_doc.info, &new_doc.info);
    let names = [old_doc.name.clone(), new_doc.name.clone()];
    let full = ui.available_rect_before_wrap();
    ui.painter().rect_filled(full, CornerRadius::ZERO, t.pasteboard);

    // Toolbar.
    let bar = Rect::from_min_size(full.min, vec2(full.width(), TOOLBAR_H));
    ui.painter().rect_filled(bar, CornerRadius::ZERO, t.chrome);
    ui.painter().hline(bar.x_range(), bar.bottom(), Stroke::new(1.0, t.divider));
    let mut close = false;
    let mut bar_ui =
        ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(10.0, 6.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    {
        let ui = &mut bar_ui;
        ui.label(egui::RichText::new(tl!("Compare side by side")).font(theme::semibold(13.5)).color(t.text));
        ui.add_space(12.0);
        if icons::button(ui, "list", 28.0, v.show_list, tl!("Change list")).clicked() {
            v.show_list = !v.show_list;
        }
        if icons::button(ui, "link", 28.0, v.sync, tl!("Synchronise scrolling")).clicked() {
            let on = !v.sync;
            v.set_sync(on);
        }
        ui.add_space(8.0);
        if icons::button(ui, "zoom-out", 28.0, false, tl!("Zoom out")).clicked() {
            v.zoom_pane(Pane::New, 1.0 / ZOOM_STEP);
            if !v.sync {
                v.zoom_pane(Pane::Old, 1.0 / ZOOM_STEP);
            }
        }
        let z = if v.sync || v.zoom[0] == v.zoom[1] {
            format!("{:.0}%", v.zoom[1] * 100.0)
        } else {
            format!("{:.0}% | {:.0}%", v.zoom[0] * 100.0, v.zoom[1] * 100.0)
        };
        ui.label(egui::RichText::new(z).color(t.text_muted));
        if icons::button(ui, "zoom-in", 28.0, false, tl!("Zoom in")).clicked() {
            v.zoom_pane(Pane::New, ZOOM_STEP);
            if !v.sync {
                v.zoom_pane(Pane::Old, ZOOM_STEP);
            }
        }
        if icons::button(ui, "arrow-left-right", 28.0, false, tl!("Fit width")).clicked() {
            v.fit_pending = true;
        }
        ui.add_space(12.0);
        let n = v.model.changes.len();
        if icons::button(ui, "chevron-up", 28.0, false, tl!("Previous change")).clicked() {
            v.step(false);
        }
        let counter = match v.current {
            _ if n == 0 => tl!("No changes").to_string(),
            Some(i) => crate::i18n::fmt(tl!("Change {i} of {n}"), &[("i", &(i + 1).to_string()), ("n", &n.to_string())]),
            None => crate::i18n::fmt(tl!("Changes: {n}"), &[("n", &n.to_string())]),
        };
        ui.label(egui::RichText::new(counter).color(t.text));
        if icons::button(ui, "chevron-down", 28.0, false, tl!("Next change")).clicked() {
            v.step(true);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icons::button(ui, "x", 28.0, false, tl!("Close side-by-side view")).clicked() {
                close = true;
            }
        });
    }
    if close {
        return Some(Action::Close);
    }

    // The change list, then the two panes.
    let mut body = Rect::from_min_max(pos2(full.left(), bar.bottom() + 1.0), full.max);
    if v.show_list {
        let list = Rect::from_min_max(body.min, pos2((body.left() + LIST_W).min(body.right()), body.bottom()));
        body.min.x = list.right() + 1.0;
        let result = app.compare.as_ref().map(|c| &c.result);
        change_list(ui, v, list, result, &t);
    }
    let half = (body.width() / 2.0).floor();
    v.panes =
        [Rect::from_min_size(body.min, vec2(half - 0.5, body.height())), Rect::from_min_max(pos2(body.left() + half + 0.5, body.top()), body.max)];
    ui.painter().vline(body.left() + half, body.y_range(), Stroke::new(1.0, t.divider));
    if v.fit_pending && body.width() > 0.0 {
        v.fit_width(old_info, new_info, half);
        v.fit_pending = false;
    }
    for pane in Pane::BOTH {
        v.rows_for(pane, old_info, new_info);
    }
    if std::mem::take(&mut v.goto) {
        v.scroll_to_current(old_info, new_info);
    }
    let ppp = ui.ctx().pixels_per_point();
    let max_tex = ui.ctx().input(|i| i.max_texture_side) as f32;
    let mut clicked = None;
    // The pane under the pointer goes first, so the other one follows its scrolling in the
    // same frame.
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let order = if pointer.is_some_and(|p| v.panes[Pane::Old.i()].contains(p)) { [Pane::Old, Pane::New] } else { [Pane::New, Pane::Old] };
    for pane in order {
        let (view, info) = if pane == Pane::Old { (&mut app.views[oi], old_info) } else { (&mut app.views[ni], new_info) };
        let rect = v.panes[pane.i()];
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_H));
        let label = if pane == Pane::Old { tl!("Old: {name}") } else { tl!("New: {name}") };
        let text = crate::i18n::fmt(label, &[("name", &names[pane.i()])]);
        ui.painter().rect_filled(header, CornerRadius::ZERO, t.panel);
        widget_text(ui, header.left_center() + vec2(12.0, 0.0), &text, theme::medium(12.5), t.text_muted, header.width() - 24.0);
        let area = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).id_salt(("compare-pane", pane.i())));
        child.set_clip_rect(area);
        let mut scroll = egui::ScrollArea::both().id_salt(("compare-pane-scroll", pane.i())).auto_shrink([false, false]);
        if let Some(o) = v.force[pane.i()].take() {
            scroll = scroll.scroll_offset(o);
        }
        let rows = &v.rows[pane.i()];
        let zoom = v.zoom[pane.i()];
        let model = &v.model;
        let current = v.current;
        let mut on_screen = Vec::new();
        let out = scroll.show_viewport(&mut child, |ui, viewport| {
            let (content, resp) = ui.allocate_exact_size(vec2(rows.width.max(viewport.width()), rows.height), Sense::click());
            let origin = content.min;
            let s = zoom * PT;
            let painter = ui.painter();
            // Rows on screen, plus a screenful either way to prefetch.
            let ahead = viewport.height();
            // The last row starting above the range may reach into it; earlier ones end before.
            let first = rows.tops.partition_point(|&top| top < viewport.top() - ahead).saturating_sub(1);
            for (row_i, row) in model.rows.iter().enumerate().skip(first) {
                let (Some(&top), Some(&h)) = (rows.tops.get(row_i), rows.heights.get(row_i)) else { break };
                if top > viewport.bottom() + ahead {
                    break;
                }
                if top + h < viewport.top() - ahead {
                    continue;
                }
                let visible = top <= viewport.bottom() && top + h >= viewport.top();
                let (mine, theirs, their_info) = if pane == Pane::Old { (row.old, row.new, new_info) } else { (row.new, row.old, old_info) };
                let Some(page) = mine.filter(|&p| p < info.pages.len()) else {
                    // A gap beside an inserted (or deleted) page, the other side's size.
                    let (w, gh) = theirs.and_then(|p| their_info.pages.get(p)).map_or((612.0, 792.0), |p| (p.width, p.height));
                    let r = Rect::from_min_size(origin + vec2(page_x(rows.width, w * s), top), vec2(w * s, gh * s));
                    if visible {
                        painter.rect(r, CornerRadius::same(4), t.card.gamma_multiply(0.5), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
                        let msg = if pane == Pane::Old { tl!("Not in the old file") } else { tl!("Not in the new file") };
                        painter.text(r.center(), Align2::CENTER_CENTER, msg, theme::regular(12.5), t.text_muted);
                    }
                    continue;
                };
                let Some(p) = info.pages.get(page) else { continue };
                let (pw, ph) = (p.width.max(1.0), p.height.max(1.0));
                let r = Rect::from_min_size(origin + vec2(page_x(rows.width, pw * s), top), vec2(pw * s, ph * s));
                let r = Rect::from_min_size(pos2((r.min.x * ppp).round() / ppp, (r.min.y * ppp).round() / ppp), r.size());
                let scale = (s * ppp).min(MAX_SIDE.min(max_tex) / pw.max(ph));
                let tex = view.side_page(page, scale, visible);
                if !visible {
                    continue;
                }
                on_screen.push((page, r));
                painter.add(egui::epaint::Shadow { offset: [0, 3], blur: 14, spread: 0, color: t.page_shadow }.as_shape(r, CornerRadius::ZERO));
                painter.rect_filled(r, CornerRadius::ZERO, Color32::WHITE);
                match tex {
                    Ok(Some((id, size, sharp))) => {
                        // A sharp raster maps texel for texel; an older one is stretched.
                        let dst = if sharp { Rect::from_min_size(r.min, vec2(size[0] as f32, size[1] as f32) / ppp) } else { r };
                        let dst = if (dst.size() - r.size()).length() > 2.0 { r } else { dst };
                        painter.image(id, dst, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    Ok(None) => {
                        painter.text(r.center(), Align2::CENTER_CENTER, tl!("Rendering…"), theme::regular(12.0), t.text_faint);
                    }
                    Err(e) => {
                        let msg = crate::i18n::fmt(tl!("This page couldn't be displayed.\n{e}"), &[("e", &e)]);
                        painter.text(r.center(), Align2::CENTER_CENTER, msg, theme::regular(12.0), t.text_muted);
                    }
                }
                painter.rect_stroke(r, CornerRadius::ZERO, Stroke::new(0.5, t.border.gamma_multiply(0.8)), egui::StrokeKind::Outside);
                // The changes on this page, the current one outlined.
                let new_side = pane == Pane::New;
                for &ci in model.marks(new_side, page) {
                    let Some(ch) = model.changes.get(ci) else { continue };
                    let Some(spot) = (if new_side { &ch.new } else { &ch.old }) else { continue };
                    let c = kind_colour(ch.kind, &t);
                    let is_current = current == Some(ci);
                    if let ViewKind::Page(_) = ch.kind {
                        if is_current {
                            painter.rect_stroke(r.expand(4.0), CornerRadius::same(3), Stroke::new(2.5, c), egui::StrokeKind::Outside);
                        }
                        continue;
                    }
                    for b in spot.rects.iter().filter(|b| b.iter().all(|x| x.is_finite())) {
                        let sr = user_rect(info, page, r, *b).expand(1.5);
                        painter.rect_filled(sr, CornerRadius::same(2), c.gamma_multiply(0.28));
                        if is_current {
                            painter.rect_stroke(sr.expand(2.0), CornerRadius::same(3), Stroke::new(2.0, c), egui::StrokeKind::Outside);
                        }
                    }
                    // Words missing on this side: a caret where they would be.
                    if spot.rects.is_empty()
                        && let Some(f) = spot.focus.filter(|b| b.iter().all(|x| x.is_finite()))
                    {
                        let sr = user_rect(info, page, r, f);
                        let x = sr.right() + 1.0;
                        painter.vline(x, sr.y_range(), Stroke::new(if is_current { 3.0 } else { 2.0 }, c));
                    }
                }
                // What happened to this row's page.
                if row.status != PageStatus::Same {
                    let label = crate::compare_ui::status_label(row.status);
                    let font = theme::medium(11.0);
                    let g = painter.layout_no_wrap(label.to_string(), font, Color32::WHITE);
                    let pill = Rect::from_min_size(r.min + vec2(8.0, 8.0), g.size() + vec2(14.0, 6.0));
                    painter.rect_filled(pill, CornerRadius::same(9), crate::compare_ui::status_colour(row.status, &t));
                    painter.galley(pill.min + vec2(7.0, 3.0), g, Color32::WHITE);
                }
            }
            // A click on a shaded change makes it current.
            if resp.clicked()
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let new_side = pane == Pane::New;
                'pages: for (page, r) in &on_screen {
                    if !r.contains(pos) {
                        continue;
                    }
                    for &ci in model.marks(new_side, *page) {
                        let Some(spot) = model.changes.get(ci).and_then(|ch| if new_side { ch.new.as_ref() } else { ch.old.as_ref() }) else {
                            continue;
                        };
                        if spot.rects.iter().chain(spot.focus.iter()).any(|b| user_rect(info, *page, *r, *b).expand(3.0).contains(pos)) {
                            clicked = Some(ci);
                            break 'pages;
                        }
                    }
                }
            }
            // Pinch or ⌘/Ctrl + wheel over a pane zooms.
            if resp.hovered() {
                let dz = ui.input(|i| i.zoom_delta());
                if dz.is_finite() && (dz - 1.0).abs() > 1e-3 {
                    return Some(dz);
                }
            }
            None
        });
        v.on_screen[pane.i()] = on_screen;
        if let Some(dz) = out.inner {
            v.zoom_pane(pane, dz);
        }
        // Scrolling one pane scrolls the other.
        let seen = out.state.offset;
        if seen != v.offset[pane.i()] {
            v.offset[pane.i()] = seen;
            if v.sync && v.force[pane.other().i()].is_none() {
                v.force[pane.other().i()] = Some(seen);
                if pane == order[1] {
                    ui.ctx().request_repaint();
                }
            }
        }
    }
    if let Some(ci) = clicked {
        v.current = Some(ci);
        v.list_follow = true;
    }
    None
}

/// Text cut to `max_w` (the pane headers show long file names).
fn widget_text(ui: &egui::Ui, pos: Pos2, text: &str, font: egui::FontId, color: Color32, max_w: f32) {
    crate::widgets::row_text(ui, pos, text, font, color, max_w.max(20.0));
}

fn change_list(ui: &mut egui::Ui, v: &mut SideBySide, rect: Rect, result: Option<&FileComparison>, t: &Tokens) {
    ui.painter().rect_filled(rect, CornerRadius::ZERO, t.panel);
    ui.painter().vline(rect.right() + 0.5, rect.y_range(), Stroke::new(1.0, t.divider));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 8.0))).id_salt("compare-change-list"));
    let ui = &mut child;
    ui.label(egui::RichText::new(tl!("Changes")).font(theme::semibold(13.0)).color(t.text));
    ui.add_space(4.0);
    let n = v.model.changes.len();
    if n == 0 {
        ui.label(egui::RichText::new(tl!("No changes")).color(t.text_muted));
        return;
    }
    let Some(result) = result else { return };
    let spacing = ui.spacing().item_spacing.y;
    let mut scroll = egui::ScrollArea::vertical().id_salt("compare-change-list-scroll").auto_shrink([false, false]);
    if std::mem::take(&mut v.list_follow)
        && let Some(i) = v.current
    {
        let h = ui.available_height();
        scroll = scroll.vertical_scroll_offset((i as f32 * (LIST_ROW_H + spacing) - h / 2.0).max(0.0));
    }
    let mut picked = None;
    scroll.show_rows(ui, LIST_ROW_H, n, |ui, range| {
        for i in range {
            let Some(ch) = v.model.changes.get(i) else { break };
            let (title, pages) = change_text(ch, result);
            let w = ui.available_width();
            let (r, resp) = ui.allocate_exact_size(vec2(w, LIST_ROW_H), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, v.current == Some(i), &title));
            if v.current == Some(i) {
                ui.painter().rect_filled(r, CornerRadius::same(6), t.selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(6), t.hover);
            }
            let bar = Rect::from_min_size(r.min + vec2(4.0, 5.0), vec2(4.0, LIST_ROW_H - 10.0));
            ui.painter().rect_filled(bar, CornerRadius::same(2), kind_colour(ch.kind, t));
            crate::widgets::row_text(ui, r.min + vec2(16.0, 12.0), pages, theme::regular(11.0), t.text_muted, w - 22.0);
            crate::widgets::row_text(ui, r.min + vec2(16.0, 28.0), title, theme::regular(12.5), t.text, w - 22.0);
            if resp.clicked() {
                picked = Some(i);
            }
        }
    });
    if let Some(i) = picked {
        v.go_to(i);
        // The list stays where it is: the row was clicked there.
        v.list_follow = false;
    }
}

impl PdfCraftApp {
    /// Whether the side-by-side compare view replaces the document area this frame.
    pub fn side_by_side_showing(&self) -> bool {
        self.side_by_side.as_ref().is_some_and(|v| self.active_ids().is_some_and(|(_, id)| id == v.old || id == v.new))
    }

    /// The other document the side-by-side view shows (its rasters are kept, not suspended).
    pub(crate) fn side_by_side_partner(&self) -> Option<DocId> {
        let v = self.side_by_side.as_ref().filter(|_| self.side_by_side_showing())?;
        let active = self.active_ids()?.1;
        Some(if active == v.old { v.new } else { v.old })
    }

    /// Open (or close) the side-by-side view of the last comparison.
    pub fn toggle_side_by_side(&mut self) {
        if self.side_by_side_showing() {
            self.side_by_side = None;
            return;
        }
        self.open_side_by_side();
    }

    /// Open the side-by-side view of the last comparison, with the newer document active.
    /// Returns false (and says why) when there is nothing to show.
    pub fn open_side_by_side(&mut self) -> bool {
        let Some(c) = self.compare.as_ref() else {
            self.notify(tl!("Compare two files first (Compare files…)").to_string());
            return false;
        };
        let (Some(oi), Some(ni)) = (self.views.iter().position(|x| x.id == c.old), self.views.iter().position(|x| x.id == c.new)) else {
            self.notify(tl!("Both compared files must be open").to_string());
            return false;
        };
        if self.side_by_side.as_ref().is_none_or(|v| v.old != c.old || v.new != c.new) {
            self.side_by_side = Some(SideBySide::new(c.old, c.new, &c.result));
        }
        for i in [oi, ni] {
            if let Some(v) = self.views.get_mut(i) {
                v.enter_side_by_side();
            }
        }
        self.active = Some(ni);
        true
    }

    /// Next / previous change in the side-by-side view (opening it first).
    pub fn side_by_side_step(&mut self, forward: bool) {
        if !self.side_by_side_showing() && !self.open_side_by_side() {
            return;
        }
        if let Some(v) = self.side_by_side.as_mut()
            && !v.step(forward)
        {
            self.notify(tl!("No changes").to_string());
        }
    }

    /// Synchronised scrolling on or off (`None` toggles).
    pub fn side_by_side_sync(&mut self, on: Option<bool>) {
        if !self.side_by_side_showing() && !self.open_side_by_side() {
            return;
        }
        if let Some(v) = self.side_by_side.as_mut() {
            let on = on.unwrap_or(!v.sync);
            v.set_sync(on);
        }
    }

    /// The Compare panel's change `text_index` selected: the side-by-side view goes there too.
    pub(crate) fn side_by_side_follow_text(&mut self, text_index: usize) {
        if let Some(v) = self.side_by_side.as_mut()
            && let Some(i) = v.model.changes.iter().position(|c| c.text == Some(text_index))
        {
            v.go_to(i);
        }
    }
}
