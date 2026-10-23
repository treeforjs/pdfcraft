//! Accessibility ▸ Tags and Order panels, and the tag dialogs (Tag properties, New tag).
//!
//! The panels never walk the structure tree per frame: the tree is read once per edit
//! ([`TagsView::refresh`]), the visible rows are flattened only when the tree or the expanded
//! set changes, and only the rows inside the scroll viewport are laid out, so a 50 000-tag tree
//! costs a handful of rows per frame. Page reading orders (which need the page's text) are read
//! on demand, one page at a time, and kept until the next edit ([`OrderCache`]).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{Align, Align2, Color32, CornerRadius, Layout, Rect, Sense, Stroke, pos2, vec2};
use pdfcraft_engine::a11y::{ArtifactSelection, PageOrder, TagChange, TagKid, Tags};
use pdfcraft_engine::{DocId, Document, Edit};

use crate::theme::{self, Tokens};
use crate::{Dialog, PdfCraftApp, RightPanel, icons, widgets};

/// Height of one tree or order row.
pub const ROW_H: f32 = 26.0;
/// Rows the Tags panel lists at most (the rest is summarized).
pub const MAX_ROWS: usize = 200_000;
/// Elements "Expand all" opens at most.
const MAX_EXPAND_ALL: usize = 20_000;
/// Indentation stops growing past this depth, so deep trees stay readable.
const MAX_INDENT_DEPTH: u16 = 24;
/// Pages whose reading order is kept between edits.
const MAX_ORDER_PAGES: usize = 48;

/// A tree node that survives edits: its object number, or (for an element written inside its
/// parent) its position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Obj(u32),
    Node(usize),
}

/// One visible row: an element, or one of its marked-content / object kids (`kid`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub node: usize,
    pub kid: Option<usize>,
    pub depth: u16,
}

/// Where a dragged tag goes relative to the row it is dropped on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Before,
    After,
    Into,
}

/// The Tags panel's view model.
#[derive(Default)]
pub struct TagsView {
    pub doc: Option<DocId>,
    generation: u64,
    pub tree: Arc<Tags>,
    /// The structure types a tag may be given.
    pub types: Arc<Vec<String>>,
    pub expanded: HashSet<Key>,
    pub rows: Vec<Row>,
    pub rows_truncated: bool,
    rows_dirty: bool,
    pub selected: Option<usize>,
    selected_key: Option<Key>,
    /// The selected content row (a kid index of the selected element).
    pub selected_kid: Option<usize>,
    /// The row being dragged.
    drag: Option<usize>,
    /// Scroll the selection into view on the next frame.
    pub reveal: bool,
    /// The selected tag's content on its page: (page, rectangles in view space).
    pub highlight: Option<(usize, Vec<[f32; 4]>)>,
    /// How many times the tree was read (tests check it isn't per frame).
    pub reads: u64,
}

impl TagsView {
    fn key(&self, i: usize) -> Key {
        self.tree.nodes.get(i).and_then(|n| n.obj).map_or(Key::Node(i), |o| Key::Obj(o.num))
    }

    fn find_key(&self, k: Key) -> Option<usize> {
        match k {
            Key::Obj(n) => self.tree.nodes.iter().position(|x| x.obj.is_some_and(|o| o.num == n)),
            Key::Node(i) => (i < self.tree.nodes.len()).then_some(i),
        }
    }

    /// Read the tree again when the document changed (once per edit, never per frame).
    pub fn refresh(&mut self, doc: &Document) {
        let first = self.doc != Some(doc.id);
        if !first && self.generation == doc.edit_generation() {
            if self.rows_dirty {
                self.build_rows();
            }
            return;
        }
        self.doc = Some(doc.id);
        self.generation = doc.edit_generation();
        self.tree = Arc::new(doc.tags());
        self.types = Arc::new(doc.tag_types());
        self.reads = self.reads.saturating_add(1);
        self.drag = None;
        self.highlight = None;
        if first {
            self.expanded.clear();
            self.selected_key = None;
            // The top level is open, and a single Document element's kids show too.
            let roots: Vec<usize> = self.tree.roots.clone();
            for &r in &roots {
                let k = self.key(r);
                self.expanded.insert(k);
            }
        }
        self.selected = self.selected_key.and_then(|k| self.find_key(k));
        if self.selected.is_none() {
            self.selected_kid = None;
        }
        self.build_rows();
    }

    /// Flatten the expanded part of the tree into rows (bounded).
    fn build_rows(&mut self) {
        self.rows_dirty = false;
        self.rows.clear();
        self.rows_truncated = false;
        let tree = Arc::clone(&self.tree);
        let mut stack: Vec<Row> = tree.roots.iter().rev().map(|&r| Row { node: r, kid: None, depth: 0 }).collect();
        while let Some(row) = stack.pop() {
            if self.rows.len() >= MAX_ROWS {
                self.rows_truncated = true;
                break;
            }
            self.rows.push(row);
            if row.kid.is_some() || !self.expanded.contains(&self.key(row.node)) {
                continue;
            }
            let Some(n) = tree.nodes.get(row.node) else { continue };
            let room = MAX_ROWS.saturating_mul(2).saturating_sub(stack.len());
            if n.kids.len() > room {
                self.rows_truncated = true;
            }
            let depth = row.depth.saturating_add(1);
            for (k, kid) in n.kids.iter().enumerate().take(room).rev() {
                stack.push(match kid {
                    TagKid::Element(c) => Row { node: *c, kid: None, depth },
                    _ => Row { node: row.node, kid: Some(k), depth },
                });
            }
        }
    }

    pub fn toggle(&mut self, node: usize) {
        let k = self.key(node);
        if !self.expanded.remove(&k) {
            self.expanded.insert(k);
        }
        self.rows_dirty = true;
        self.build_rows();
    }

    pub fn expand_all(&mut self) -> bool {
        let mut n = 0usize;
        for i in 0..self.tree.nodes.len() {
            let has_kids = self.tree.nodes.get(i).is_some_and(|x| !x.kids.is_empty());
            if has_kids {
                if n >= MAX_EXPAND_ALL {
                    self.build_rows();
                    return false;
                }
                let k = self.key(i);
                self.expanded.insert(k);
                n += 1;
            }
        }
        self.build_rows();
        true
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.build_rows();
    }

    /// Select a node (and open its ancestors so it shows).
    pub fn select(&mut self, node: usize, kid: Option<usize>) {
        if node >= self.tree.nodes.len() {
            return;
        }
        let ancestors: Vec<usize> = self.tree.ancestors(node).collect();
        for a in ancestors {
            let k = self.key(a);
            self.expanded.insert(k);
        }
        if kid.is_some() {
            let k = self.key(node);
            self.expanded.insert(k);
        }
        self.selected = Some(node);
        self.selected_key = Some(self.key(node));
        self.selected_kid = kid;
        self.reveal = true;
        self.build_rows();
    }

    pub fn selected_obj(&self) -> Option<u32> {
        self.selected.and_then(|i| self.tree.nodes.get(i)).and_then(|n| n.obj).map(|o| o.num)
    }

    /// The page a node's content is on: its own, or its first content kid's.
    pub fn page_of(&self, node: usize) -> Option<usize> {
        let n = self.tree.nodes.get(node)?;
        n.page.or_else(|| {
            n.kids.iter().find_map(|k| match k {
                TagKid::Content { page, .. } | TagKid::Object { page, .. } => *page,
                TagKid::Element(_) => None,
            })
        })
    }
}

/// Page reading orders, read on demand and kept until the next edit.
#[derive(Default)]
pub struct OrderCache {
    doc: Option<DocId>,
    generation: u64,
    pages: HashMap<usize, Arc<Result<PageOrder, String>>>,
    /// Reads so far (tests check that frames don't read).
    pub reads: u64,
}

impl OrderCache {
    fn sync(&mut self, doc: &Document) {
        if self.doc != Some(doc.id) || self.generation != doc.edit_generation() {
            self.doc = Some(doc.id);
            self.generation = doc.edit_generation();
            self.pages.clear();
        }
    }

    /// The page's reading order, read now if it isn't known yet.
    pub fn get(&mut self, doc: &Document, page: usize) -> Arc<Result<PageOrder, String>> {
        self.sync(doc);
        if let Some(o) = self.pages.get(&page) {
            return Arc::clone(o);
        }
        if self.pages.len() >= MAX_ORDER_PAGES {
            self.pages.clear();
        }
        self.reads = self.reads.saturating_add(1);
        let o = Arc::new(doc.reading_order(page));
        self.pages.insert(page, Arc::clone(&o));
        o
    }

    /// The page's reading order if it is known.
    pub fn peek(&mut self, doc: &Document, page: usize) -> Option<Arc<Result<PageOrder, String>>> {
        self.sync(doc);
        self.pages.get(&page).cloned()
    }
}

/// Tag properties being edited.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TagPropsDraft {
    pub doc: Option<DocId>,
    pub obj: u32,
    pub ty: String,
    pub title: String,
    pub alt: String,
    pub actual: String,
    pub lang: String,
    /// The values when the dialog opened: only what changed is written.
    pub orig: [String; 5],
    pub types: Arc<Vec<String>>,
}

/// New tag: its type and where it goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewTagDraft {
    pub doc: Option<DocId>,
    pub parent: Option<u32>,
    pub parent_label: String,
    pub ty: String,
    pub types: Arc<Vec<String>>,
}

/// Everything the accessibility tools keep between frames.
#[derive(Default)]
pub struct A11yTools {
    pub tags: TagsView,
    pub orders: OrderCache,
    /// The Order panel's (and the Reading Order tool's) selected region: (page, region).
    pub region: Option<(usize, usize)>,
    pub props: Option<TagPropsDraft>,
    pub new_tag: Option<NewTagDraft>,
    pub reading: crate::reading_order_ui::ReadingState,
    pub autotag: crate::make_accessible_ui::AutotagDraft,
    pub summary: Option<crate::make_accessible_ui::AutotagSummary>,
    pub make: Option<crate::make_accessible_ui::MakeAccessible>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TagsAction {
    Select(usize, Option<usize>),
    Properties(usize),
    NewTag(Option<usize>),
    Delete(u32),
    Artifact(u32),
    Move { element: u32, target: u32, place: Place },
    ExpandAll,
    Command(&'static str),
}

/// The text shown after a tag's type: its title, alternate or actual text.
fn node_text(tree: &Tags, i: usize) -> String {
    let Some(n) = tree.nodes.get(i) else { return String::new() };
    let pick = [&n.title, &n.alt, &n.actual_text].into_iter().flatten().map(|s| s.trim()).find(|s| !s.is_empty()).unwrap_or("");
    pick.chars().take(80).collect()
}

/// A row's accessible label and its two visible parts (chip, text).
fn row_label(tree: &Tags, row: Row) -> (String, String, String) {
    let Some(n) = tree.nodes.get(row.node) else { return (String::new(), String::new(), String::new()) };
    match row.kid.and_then(|k| n.kids.get(k)) {
        Some(TagKid::Content { page, mcid, .. }) => {
            let text = match page {
                Some(p) => {
                    crate::i18n::fmt(tl!("Marked content {mcid} on page {page}"), &[("mcid", &mcid.to_string()), ("page", &(p + 1).to_string())])
                }
                None => crate::i18n::fmt(tl!("Marked content {mcid}"), &[("mcid", &mcid.to_string())]),
            };
            (text.clone(), "MC".into(), text)
        }
        Some(TagKid::Object { subtype, .. }) => {
            let s = subtype.clone().unwrap_or_else(|| tl!("Object").to_string());
            let text = crate::i18n::fmt(tl!("Annotation: {kind}"), &[("kind", &s)]);
            (text.clone(), "OBJR".into(), text)
        }
        _ => {
            let chip = match &n.standard {
                Some(s) if *s != n.ty => format!("{} → {s}", n.ty),
                _ => n.ty.clone(),
            };
            let text = node_text(tree, row.node);
            let label = if text.is_empty() { n.ty.clone() } else { format!("{} {text}", n.ty) };
            (label, chip, text)
        }
    }
}

/// The rows of a virtual list that are on screen: lay out only those. `paint` draws row `i`
/// in its rectangle.
pub(crate) fn virtual_rows(ui: &mut egui::Ui, count: usize, mut paint: impl FnMut(&mut egui::Ui, usize, Rect)) -> Rect {
    let w = ui.available_width();
    let total = ROW_H * count as f32;
    let (area, _) = ui.allocate_exact_size(vec2(w, total.max(1.0)), Sense::hover());
    let clip = ui.clip_rect();
    let first = (((clip.top() - area.top()) / ROW_H).floor().max(0.0) as usize).min(count);
    let last = ((((clip.bottom() - area.top()) / ROW_H).ceil().max(0.0) as usize).saturating_add(1)).min(count);
    for i in first..last {
        let r = Rect::from_min_size(pos2(area.left(), area.top() + ROW_H * i as f32), vec2(w, ROW_H));
        paint(ui, i, r);
    }
    area
}

fn chip(ui: &egui::Ui, t: &Tokens, at: egui::Pos2, text: &str, strong: bool) -> f32 {
    let font = theme::semibold(11.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, if strong { t.accent_text } else { t.text_muted });
    let r = Rect::from_min_size(at - vec2(0.0, 9.0), vec2(galley.size().x + 10.0, 18.0));
    ui.painter().rect_filled(r, CornerRadius::same(5), if strong { t.accent_soft } else { t.hover });
    ui.painter().galley(r.left_center() + vec2(5.0, -galley.size().y / 2.0), galley, t.text);
    r.width()
}

/// The Tags panel. `editable`: the document can be changed.
pub(crate) fn tags_panel(ui: &mut egui::Ui, t: &Tokens, v: &mut TagsView, editable: bool) -> Option<TagsAction> {
    let mut action = None;
    if !v.tree.exists {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.add(icons::image("tag", 32.0, t.text_faint));
            ui.add_space(6.0);
            ui.label(egui::RichText::new(tl!("This document has no tags.")).color(t.text_muted));
            ui.label(
                egui::RichText::new(tl!("Tags tell screen readers what each part of the page is and in which order to read it."))
                    .small()
                    .color(t.text_faint),
            );
            ui.add_space(8.0);
            ui.add_enabled_ui(editable, |ui| {
                if widgets::pill_button(ui, tl!("Autotag document"), true).clicked() {
                    action = Some(TagsAction::Command("a11y.autotag"));
                }
                ui.add_space(4.0);
                if widgets::pill_button(ui, tl!("Reading order tool"), false).clicked() {
                    action = Some(TagsAction::Command("a11y.reading_order"));
                }
            });
        });
        return action;
    }
    let sel_obj = v.selected_obj();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.add_enabled_ui(editable, |ui| {
            if icons::button(ui, "plus", 26.0, false, tl!("New tag…")).clicked() {
                action = Some(TagsAction::NewTag(v.selected));
            }
            ui.add_enabled_ui(sel_obj.is_some(), |ui| {
                if icons::button(ui, "settings-2", 26.0, false, tl!("Tag properties…")).clicked()
                    && let Some(s) = v.selected
                {
                    action = Some(TagsAction::Properties(s));
                }
                if icons::button(ui, "eye-off", 26.0, false, tl!("Change to artifact")).clicked()
                    && let Some(o) = sel_obj
                {
                    action = Some(TagsAction::Artifact(o));
                }
                if icons::button(ui, "trash-2", 26.0, false, tl!("Delete tag")).clicked()
                    && let Some(o) = sel_obj
                {
                    action = Some(TagsAction::Delete(o));
                }
            });
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icons::button(ui, "chevron-right", 26.0, false, tl!("Collapse all tags")).clicked() {
                v.collapse_all();
            }
            if icons::button(ui, "chevron-down", 26.0, false, tl!("Expand all tags")).clicked() {
                action = Some(TagsAction::ExpandAll);
            }
            ui.label(egui::RichText::new(v.tree.nodes.len().to_string()).font(theme::medium(12.0)).color(t.text_faint));
        });
    });
    if v.tree.truncated {
        ui.label(
            egui::RichText::new(tl!("This structure tree is very large: only part of it is shown, and it can't be edited."))
                .small()
                .color(t.text_faint),
        );
    }
    ui.add_space(4.0);
    let tree = Arc::clone(&v.tree);
    let rows = std::mem::take(&mut v.rows);
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let mut drop: Option<(usize, Place)> = None;
    let reveal = std::mem::take(&mut v.reveal);
    let selected_row = rows.iter().position(|r| Some(r.node) == v.selected && r.kid == v.selected_kid);
    let id = ui.id().with("tags-rows");
    let area = virtual_rows(ui, rows.len(), |ui, i, rect| {
        let Some(&row) = rows.get(i) else { return };
        let (label, chip_text, text) = row_label(&tree, row);
        let resp = ui.interact(rect, id.with(i), Sense::click_and_drag());
        let selected = Some(i) == selected_row;
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label));
        if selected {
            ui.painter().rect_filled(rect, CornerRadius::same(5), t.selected);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(5), t.hover);
        }
        let indent = 4.0 + 14.0 * f32::from(row.depth.min(MAX_INDENT_DEPTH));
        let n = tree.nodes.get(row.node);
        let has_kids = row.kid.is_none() && n.is_some_and(|n| !n.kids.is_empty());
        let open = has_kids && v.expanded.contains(&v.key(row.node));
        let toggle_rect = Rect::from_min_size(pos2(rect.left() + indent, rect.top() + 4.0), vec2(18.0, 18.0));
        if has_kids {
            icons::paint(ui, toggle_rect, if open { "chevron-down" } else { "chevron-right" }, 14.0, t.icon);
        }
        let mut x = toggle_rect.right() + 4.0;
        x += chip(ui, t, pos2(x, rect.center().y), &chip_text, row.kid.is_none()) + 6.0;
        if !text.is_empty() && row.kid.is_none() {
            widgets::row_text(ui, pos2(x, rect.center().y), text, theme::regular(12.5), t.text, (rect.right() - x - 4.0).max(0.0));
        } else if row.kid.is_some() {
            widgets::row_text(ui, pos2(x, rect.center().y), text, theme::regular(12.0), t.text_muted, (rect.right() - x - 4.0).max(0.0));
        }
        if resp.clicked() {
            if has_kids && pointer.is_some_and(|p| toggle_rect.expand(3.0).contains(p)) {
                v.toggle(row.node);
            } else {
                action = Some(TagsAction::Select(row.node, row.kid));
            }
        }
        if resp.double_clicked() && row.kid.is_none() {
            action = Some(TagsAction::Properties(row.node));
        }
        if resp.drag_started() && editable && row.kid.is_none() {
            v.drag = Some(i);
        }
        // Drop target: top quarter before, bottom quarter after, the middle inside.
        if let (Some(from), Some(p)) = (v.drag, pointer)
            && rect.contains(p)
            && from != i
            && row.kid.is_none()
        {
            let f = (p.y - rect.top()) / ROW_H;
            let place = if f < 0.25 {
                Place::Before
            } else if f > 0.75 {
                Place::After
            } else {
                Place::Into
            };
            let stroke = Stroke::new(2.0, t.accent);
            match place {
                Place::Before => {
                    ui.painter().hline(rect.x_range(), rect.top(), stroke);
                }
                Place::After => {
                    ui.painter().hline(rect.x_range(), rect.bottom(), stroke);
                }
                Place::Into => {
                    ui.painter().rect_stroke(rect.shrink(1.0), CornerRadius::same(5), stroke, egui::StrokeKind::Inside);
                }
            }
            drop = Some((i, place));
        }
        if row.kid.is_none() {
            resp.context_menu(|ui| {
                ui.set_min_width(190.0);
                let obj = n.and_then(|n| n.obj).map(|o| o.num);
                if ui.add_enabled(obj.is_some(), egui::Button::new(tl!("Tag properties…"))).clicked() {
                    action = Some(TagsAction::Properties(row.node));
                    ui.close();
                }
                if ui.add_enabled(editable, egui::Button::new(tl!("New tag…"))).clicked() {
                    action = Some(TagsAction::NewTag(Some(row.node)));
                    ui.close();
                }
                ui.separator();
                if ui.add_enabled(editable && obj.is_some(), egui::Button::new(tl!("Change to artifact"))).clicked()
                    && let Some(o) = obj
                {
                    action = Some(TagsAction::Artifact(o));
                    ui.close();
                }
                if ui.add_enabled(editable && obj.is_some(), egui::Button::new(tl!("Delete tag"))).clicked()
                    && let Some(o) = obj
                {
                    action = Some(TagsAction::Delete(o));
                    ui.close();
                }
            });
        }
    });
    // Reveal a selection that isn't laid out yet (off screen).
    if reveal && let Some(i) = selected_row {
        let r = Rect::from_min_size(pos2(area.left(), area.top() + ROW_H * i as f32), vec2(area.width(), ROW_H));
        ui.scroll_to_rect(r, Some(Align::Center));
    }
    let released = ui.input(|i| i.pointer.any_released());
    if released
        && let Some(from) = v.drag.take()
        && let Some((to, place)) = drop
    {
        let obj = |r: Option<&Row>| r.and_then(|r| tree.nodes.get(r.node)).and_then(|n| n.obj).map(|o| o.num);
        if let (Some(element), Some(target)) = (obj(rows.get(from)), obj(rows.get(to))) {
            action = Some(TagsAction::Move { element, target, place });
        }
    }
    if v.rows_truncated {
        ui.label(egui::RichText::new(tl!("Collapse some tags to see the rest.")).small().color(t.text_faint));
    }
    if v.rows.is_empty() {
        v.rows = rows;
    }
    action
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OrderAction {
    Select(usize),
    Move { element: u32, target: u32, after: bool },
    Clear(usize),
    Tool,
}

/// The Order panel: page `page`'s regions in reading order.
pub(crate) fn order_panel(ui: &mut egui::Ui, t: &Tokens, tools: &mut A11yTools, doc: &Document, page: usize, editable: bool) -> Option<OrderAction> {
    let mut action = None;
    let order = tools.orders.get(doc, page);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::fmt(tl!("Page {n}"), &[("n", &(page + 1).to_string())])).font(theme::medium(13.0)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_enabled_ui(editable, |ui| {
                if icons::button(ui, "square-dashed", 26.0, false, tl!("Reading order tool")).clicked() {
                    action = Some(OrderAction::Tool);
                }
                if icons::button(ui, "eraser", 26.0, false, tl!("Clear page structure")).clicked() {
                    action = Some(OrderAction::Clear(page));
                }
            });
        });
    });
    let o = match &*order {
        Ok(o) => o,
        Err(e) => {
            ui.label(egui::RichText::new(e).small().color(t.text_muted));
            return action;
        }
    };
    if !o.tagged || o.regions.is_empty() {
        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            ui.add(icons::image("list-ordered", 28.0, t.text_faint));
            ui.label(egui::RichText::new(tl!("Nothing on this page is tagged yet.")).color(t.text_muted));
            ui.label(
                egui::RichText::new(tl!("Use the Reading order tool: draw a box around content and choose what it is.")).small().color(t.text_faint),
            );
        });
    }
    if o.untagged > 0 {
        ui.label(
            egui::RichText::new(crate::i18n::fmt(tl!("{n} drawing operations on this page are not tagged."), &[("n", &o.untagged.to_string())]))
                .small()
                .color(t.text_faint),
        );
    }
    ui.add_space(4.0);
    let tree = Arc::clone(&tools.tags.tree);
    let selected = tools.region.filter(|(p, _)| *p == page).map(|(_, r)| r);
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let drag_id = ui.id().with("order-drag");
    let mut drag: Option<usize> = ui.data(|d| d.get_temp(drag_id)).flatten();
    let mut drop: Option<(usize, bool)> = None;
    let id = ui.id().with("order-rows");
    let n = o.regions.len();
    virtual_rows(ui, n, |ui, i, rect| {
        let Some(r) = o.regions.get(i) else { return };
        let text = r
            .obj
            .and_then(|obj| tree.find(obj))
            .map(|k| node_text(&tree, k))
            .filter(|s| !s.is_empty())
            .or_else(|| r.elements.iter().find_map(|e| e.alt.clone().or_else(|| e.actual_text.clone())))
            .unwrap_or_default();
        let label = format!("{} {}{}", i + 1, r.ty, if text.is_empty() { String::new() } else { format!(" {text}") });
        let resp = ui.interact(rect, id.with(i), Sense::click_and_drag());
        let is_sel = selected == Some(i);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, is_sel, &label));
        if is_sel {
            ui.painter().rect_filled(rect, CornerRadius::same(5), t.selected);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(5), t.hover);
        }
        let badge = Rect::from_center_size(pos2(rect.left() + 16.0, rect.center().y), vec2(24.0, 18.0));
        ui.painter().rect_filled(badge, CornerRadius::same(9), t.accent);
        ui.painter().text(badge.center(), Align2::CENTER_CENTER, (i + 1).to_string(), theme::semibold(11.0), Color32::WHITE);
        let mut x = badge.right() + 8.0;
        x += chip(ui, t, pos2(x, rect.center().y), &r.ty, true) + 6.0;
        widgets::row_text(ui, pos2(x, rect.center().y), text, theme::regular(12.5), t.text, (rect.right() - x - 56.0).max(0.0));
        if (is_sel || resp.hovered()) && editable && r.obj.is_some() {
            let up = Rect::from_min_size(pos2(rect.right() - 52.0, rect.top() + 2.0), vec2(22.0, 22.0));
            let down = Rect::from_min_size(pos2(rect.right() - 26.0, rect.top() + 2.0), vec2(22.0, 22.0));
            let target = |k: usize| o.regions.get(k).and_then(|x| x.obj).map(|x| x.num);
            for (b, icon, tip, to, after) in [
                (up, "chevron-up", tl!("Move up"), i.checked_sub(1), false),
                (down, "chevron-down", tl!("Move down"), i.checked_add(1).filter(|k| *k < n), true),
            ] {
                let br = ui.interact(b, id.with((i, icon)), Sense::click());
                br.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, to.is_some(), tip));
                if br.hovered() {
                    ui.painter().rect_filled(b, CornerRadius::same(4), t.pressed);
                }
                icons::paint(ui, b, icon, 13.0, if to.is_some() { t.icon } else { t.text_faint });
                if br.clicked()
                    && let (Some(element), Some(target)) = (r.obj.map(|x| x.num), to.and_then(target))
                {
                    action = Some(OrderAction::Move { element, target, after });
                }
            }
        }
        if resp.clicked() && action.is_none() {
            action = Some(OrderAction::Select(i));
        }
        if resp.drag_started() && editable && r.obj.is_some() {
            drag = Some(i);
        }
        if let (Some(from), Some(p)) = (drag, pointer)
            && rect.contains(p)
            && from != i
        {
            let after = p.y > rect.center().y;
            ui.painter().hline(rect.x_range(), if after { rect.bottom() } else { rect.top() }, Stroke::new(2.0, t.accent));
            drop = Some((i, after));
        }
    });
    if ui.input(|i| i.pointer.any_released())
        && let Some(from) = drag.take()
        && let Some((to, after)) = drop
        && let (Some(element), Some(target)) =
            (o.regions.get(from).and_then(|r| r.obj).map(|x| x.num), o.regions.get(to).and_then(|r| r.obj).map(|x| x.num))
    {
        action = Some(OrderAction::Move { element, target, after });
    }
    ui.data_mut(|d| d.insert_temp(drag_id, drag));
    action
}

/// Tag properties: type, title, alternate text, actual text, language.
pub(crate) fn props_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let Some(d) = app.a11y_tools.props.as_mut() else { return true };
    ui.label(egui::RichText::new(tl!("Tag properties")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    egui::Grid::new("tag-props").num_columns(2).spacing(vec2(12.0, 8.0)).show(ui, |ui| {
        let l = ui.label(tl!("Type"));
        egui::ComboBox::from_id_salt("tag-type")
            .selected_text(d.ty.clone())
            .width(300.0)
            .show_ui(ui, |ui| {
                for ty in d.types.iter() {
                    ui.selectable_value(&mut d.ty, ty.clone(), ty);
                }
            })
            .response
            .labelled_by(l.id);
        ui.end_row();
        for (label, value, hint, multi) in [
            (tl!("Title"), &mut d.title, "", false),
            (tl!("Alternate text"), &mut d.alt, tl!("What a figure or formula shows"), true),
            (tl!("Actual text"), &mut d.actual, tl!("The exact text the content spells"), true),
            (tl!("Language"), &mut d.lang, "en-US", false),
        ] {
            let l = ui.label(label);
            let edit = if multi { egui::TextEdit::multiline(value).desired_rows(2) } else { egui::TextEdit::singleline(value) };
            ui.add(edit.desired_width(300.0).hint_text(hint)).labelled_by(l.id);
            ui.end_row();
        }
    });
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Alternate and actual text replace everything inside the tag for screen readers.")).small().color(t.text_faint));
    ui.add_space(10.0);
    let (mut save, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            save = widgets::pill_button(ui, tl!("Save"), true).clicked();
            cancel = widgets::pill_button(ui, tl!("Cancel"), false).clicked();
        });
    });
    if save {
        let d = app.a11y_tools.props.take().unwrap_or_default();
        let now = [d.ty.trim().to_owned(), d.title.clone(), d.alt.clone(), d.actual.clone(), d.lang.trim().to_owned()];
        let changed = |k: usize| now.get(k) != d.orig.get(k);
        let text = |k: usize| changed(k).then(|| now.get(k).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()));
        let change =
            TagChange { ty: changed(0).then(|| now[0].clone()), title: text(1), alt: text(2), actual_text: text(3), lang: text(4), namespace: None };
        if !change.is_empty() {
            app.apply_edit(Edit::SetTag { element: d.obj, change });
        }
        return true;
    }
    if cancel {
        app.a11y_tools.props = None;
    }
    cancel
}

/// New tag: choose its type.
pub(crate) fn new_tag_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let Some(d) = app.a11y_tools.new_tag.as_mut() else { return true };
    ui.label(egui::RichText::new(tl!("New tag")).font(theme::semibold(18.0)));
    ui.add_space(6.0);
    ui.label(egui::RichText::new(&d.parent_label).color(t.text_muted));
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let l = ui.label(tl!("Type"));
        egui::ComboBox::from_id_salt("new-tag-type")
            .selected_text(d.ty.clone())
            .width(280.0)
            .show_ui(ui, |ui| {
                for ty in d.types.iter() {
                    ui.selectable_value(&mut d.ty, ty.clone(), ty);
                }
            })
            .response
            .labelled_by(l.id);
    });
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Drag tags onto the new tag to move them inside it.")).small().color(t.text_faint));
    ui.add_space(10.0);
    let (mut add, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            add = widgets::pill_button(ui, tl!("Add tag"), true).clicked();
            cancel = widgets::pill_button(ui, tl!("Cancel"), false).clicked();
        });
    });
    if add {
        let d = app.a11y_tools.new_tag.take().unwrap_or_default();
        app.add_tag(d.parent, &d.ty);
        return true;
    }
    if cancel {
        app.a11y_tools.new_tag = None;
    }
    cancel
}

impl PdfCraftApp {
    /// Refresh the tags view model for the active document (cheap when nothing changed).
    pub(crate) fn refresh_tags(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        if let Some(doc) = self.session.get(id) {
            self.a11y_tools.tags.refresh(doc);
        }
    }

    pub(crate) fn tags_action(&mut self, index: usize, action: TagsAction) {
        match action {
            TagsAction::Select(node, kid) => self.select_tag(index, node, kid),
            TagsAction::Properties(node) => self.open_tag_properties(node),
            TagsAction::NewTag(parent) => self.open_new_tag(parent),
            TagsAction::Delete(obj) => {
                if self.apply_edit(Edit::DeleteTag { element: obj }) {
                    self.a11y_tools.tags.selected = None;
                }
            }
            TagsAction::Artifact(obj) => {
                let selection = ArtifactSelection { elements: vec![obj], ..Default::default() };
                self.apply_edit(Edit::MarkArtifact { selection });
            }
            TagsAction::Move { element, target, place } => {
                let edit = match place {
                    Place::Before => Edit::MoveTag { element, target, after: false },
                    Place::After => Edit::MoveTag { element, target, after: true },
                    Place::Into => Edit::MoveTagInto { element, target },
                };
                if self.apply_edit(edit) {
                    self.refresh_tags();
                    if let Some(i) = self.a11y_tools.tags.tree.nodes.iter().position(|n| n.obj.is_some_and(|o| o.num == element)) {
                        self.a11y_tools.tags.select(i, None);
                    }
                }
            }
            TagsAction::ExpandAll => {
                if !self.a11y_tools.tags.expand_all() {
                    self.notify_tr("This tree is too large to expand completely; the first tags were expanded.");
                }
            }
            TagsAction::Command(id) => {
                self.execute(id);
            }
        }
    }

    /// Select a tag: show where its content is on its page.
    pub(crate) fn select_tag(&mut self, index: usize, node: usize, kid: Option<usize>) {
        let tools = &mut self.a11y_tools;
        tools.tags.select(node, kid);
        tools.tags.highlight = None;
        let Some(doc) = self.views.get(index).and_then(|v| self.session.get(v.id)) else { return };
        let tree = Arc::clone(&tools.tags.tree);
        let page = match kid.and_then(|k| tree.nodes.get(node).and_then(|n| n.kids.get(k))) {
            Some(TagKid::Content { page, .. } | TagKid::Object { page, .. }) => *page,
            _ => tools.tags.page_of(node),
        };
        let Some(page) = page.filter(|p| *p < doc.info.pages.len()) else { return };
        let order = tools.orders.get(doc, page);
        let mut rects = Vec::new();
        if let Ok(o) = &*order {
            let mcid = match kid.and_then(|k| tree.nodes.get(node).and_then(|n| n.kids.get(k))) {
                Some(TagKid::Content { mcid, .. }) => Some(*mcid),
                _ => None,
            };
            for (ri, r) in o.regions.iter().enumerate() {
                for e in &r.elements {
                    let inside = e.node == node || tree.ancestors(e.node).any(|a| a == node);
                    if inside && mcid.is_none_or(|m| e.mcids.contains(&m) && e.node == node) {
                        if let Some(b) = e.rect {
                            rects.push(b.map(|x| x as f32));
                        }
                        if r.node == node {
                            tools.region = Some((page, ri));
                        }
                    }
                }
            }
        }
        tools.tags.highlight = Some((page, rects));
        if let Some(v) = self.views.get_mut(index)
            && !v.visible_page_rects().iter().any(|(p, _)| *p == page)
        {
            v.go_to_page(page);
        }
    }

    /// Tag properties for tree node `node`.
    pub(crate) fn open_tag_properties(&mut self, node: usize) {
        self.refresh_tags();
        let tags = &self.a11y_tools.tags;
        let Some(n) = tags.tree.nodes.get(node) else { return };
        let Some(obj) = n.obj else {
            self.notify_tr("This tag is written inside its parent and can't be changed.");
            return;
        };
        let s = |o: &Option<String>| o.clone().unwrap_or_default();
        let orig = [n.ty.clone(), s(&n.title), s(&n.alt), s(&n.actual_text), s(&n.lang)];
        let mut types = (*tags.types).clone();
        if !types.contains(&n.ty) {
            types.insert(0, n.ty.clone());
        }
        self.a11y_tools.props = Some(TagPropsDraft {
            doc: tags.doc,
            obj: obj.num,
            ty: orig[0].clone(),
            title: orig[1].clone(),
            alt: orig[2].clone(),
            actual: orig[3].clone(),
            lang: orig[4].clone(),
            orig,
            types: Arc::new(types),
        });
        self.dialog = Some(Dialog::TagProperties);
    }

    /// New tag inside tree node `parent` (`None`: at the top level).
    pub(crate) fn open_new_tag(&mut self, parent: Option<usize>) {
        self.refresh_tags();
        let tags = &self.a11y_tools.tags;
        if !tags.tree.exists {
            self.notify_tr("This document has no tags yet: autotag it or use the Reading order tool first.");
            return;
        }
        let p = parent.and_then(|i| tags.tree.nodes.get(i));
        let parent_label = match p {
            Some(n) => crate::i18n::fmt(tl!("Inside {tag}"), &[("tag", &n.ty)]),
            None => tl!("At the top level").to_string(),
        };
        self.a11y_tools.new_tag = Some(NewTagDraft {
            doc: tags.doc,
            parent: p.and_then(|n| n.obj).map(|o| o.num),
            parent_label,
            ty: "P".into(),
            types: Arc::clone(&tags.types),
        });
        self.dialog = Some(Dialog::NewTag);
    }

    /// Add a tag and select it.
    pub(crate) fn add_tag(&mut self, parent: Option<u32>, ty: &str) {
        let before: HashSet<u32> = self.a11y_tools.tags.tree.nodes.iter().filter_map(|n| n.obj.map(|o| o.num)).collect();
        if !self.apply_edit(Edit::NewTag { parent, ty: ty.to_owned() }) {
            return;
        }
        self.refresh_tags();
        let tags = &mut self.a11y_tools.tags;
        if let Some(i) = tags.tree.nodes.iter().position(|n| n.obj.is_some_and(|o| !before.contains(&o.num))) {
            tags.select(i, None);
        }
    }

    pub(crate) fn order_action(&mut self, index: usize, page: usize, action: OrderAction) {
        match action {
            OrderAction::Select(r) => {
                self.a11y_tools.region = Some((page, r));
                let Some(doc) = self.views.get(index).and_then(|v| self.session.get(v.id)) else { return };
                self.a11y_tools.tags.refresh(doc);
                let order = self.a11y_tools.orders.get(doc, page);
                if let Ok(o) = &*order
                    && let Some(reg) = o.regions.get(r)
                {
                    let node = reg.obj.and_then(|obj| self.a11y_tools.tags.tree.find(obj)).unwrap_or(reg.node);
                    self.a11y_tools.tags.select(node, None);
                    self.a11y_tools.tags.highlight = Some((page, reg.rect.map(|b| vec![b.map(|x| x as f32)]).unwrap_or_default()));
                }
            }
            OrderAction::Move { element, target, after } => {
                if self.apply_edit(Edit::MoveTag { element, target, after })
                    && let Some((p, r)) = self.a11y_tools.region
                {
                    // The moved region stays selected, where it is now.
                    let to = if after { r.saturating_add(1) } else { r.saturating_sub(1) };
                    self.a11y_tools.region = Some((p, to));
                }
            }
            OrderAction::Clear(page) => {
                self.apply_edit(Edit::ClearPageStructure { page, keep_artifacts: true });
            }
            OrderAction::Tool => self.execute_reading_order_tool(),
        }
    }

    /// The tag commands (`a11y.tags`, `a11y.order`, `a11y.tag_properties`, `a11y.new_tag`,
    /// `a11y.delete_tag`, `a11y.artifact`).
    pub(crate) fn tags_command(&mut self, id: &str) {
        let Some((index, _)) = self.active_ids() else { return };
        self.refresh_tags();
        let selected = self.a11y_tools.tags.selected;
        let obj = self.a11y_tools.tags.selected_obj();
        match id {
            "a11y.tags" => self.choose_right_panel(Some(RightPanel::Tags)),
            "a11y.order" => self.choose_right_panel(Some(RightPanel::Order)),
            "a11y.new_tag" => self.open_new_tag(selected),
            _ => {
                let (Some(node), Some(obj)) = (selected, obj) else {
                    self.choose_right_panel(Some(RightPanel::Tags));
                    self.notify_tr("Select a tag in the Tags panel first.");
                    return;
                };
                let action = match id {
                    "a11y.tag_properties" => TagsAction::Properties(node),
                    "a11y.delete_tag" => TagsAction::Delete(obj),
                    _ => TagsAction::Artifact(obj),
                };
                self.tags_action(index, action);
            }
        }
    }
}
