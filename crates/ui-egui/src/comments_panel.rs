//! The Comments panel (Acrobat's right-hand comment list; 11-visual-spec §3, audit `a15-*`).
//!
//! Header "Comments N" with search; an "Add a comment" box that posts a sticky note on the
//! current page; cards grouped by page. Selecting a card selects the comment on the page and
//! opens its reply box and "…" menu (Edit, Set status, Delete). Status replies show as a badge.

use std::collections::HashMap;

use egui::{Align, Color32, CornerRadius, Layout, Sense, Stroke, vec2};
use pdfcraft_engine::{Edit, NoteIcon, ReviewState, Shape};
use pdfcraft_render::{Annotation, DocInfo};

use crate::canvas::DocView;
use crate::comments::{CommentPrefs, CommentTool, CommentView, PanelStats, SortBy, color32, status_badge};
use crate::icons;
use crate::panels::Nav;
use crate::theme::{self, Tokens};

/// Accent bar of the selected card (11-visual-spec §3: `#0165DD`).
const CARD_ACCENT: Color32 = Color32::from_rgb(0x01, 0x65, 0xDD);

/// Reader-friendly names for annotation subtypes (the PDF names are shown in tooltips).
pub fn subtype_label(s: &str) -> &str {
    match s {
        "Text" => "Note",
        "FreeText" => "Text box",
        "StrikeOut" => "Strikethrough",
        "Square" => "Rectangle",
        "Circle" => "Oval",
        "Ink" => "Drawing",
        "PolyLine" => "Polyline",
        "FileAttachment" => "Attachment",
        "Caret" => "Insert text",
        other => other,
    }
}

/// The comment type as the panel names it, telling callouts and clouds apart by `/IT`.
pub fn kind_label(a: &Annotation) -> &str {
    match a.intent.as_deref() {
        Some("FreeTextCallout") => "Callout",
        Some("PolygonCloud") => "Cloud",
        Some("FreeTextTypeWriter") => "Typewriter",
        _ => subtype_label(&a.subtype),
    }
}

pub fn subtype_icon(s: &str) -> &'static str {
    match s {
        "Text" => "message-square-text",
        "Highlight" => "highlighter",
        "Underline" => "underline",
        "StrikeOut" => "strikethrough",
        "Squiggly" => "spline",
        "FreeText" => "type",
        "Ink" => "pencil",
        "Square" => "square",
        "Circle" => "circle",
        "Line" => "move-right",
        "Polygon" | "PolyLine" => "pen-tool",
        "Stamp" => "stamp",
        "FileAttachment" => "paperclip",
        "Caret" => "text-select",
        "Redact" => "rectangle-horizontal",
        _ => "message-square-text",
    }
}

fn initials(name: &str) -> String {
    let s: String = name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect();
    if s.is_empty() { "?".into() } else { s.to_uppercase() }
}

/// A comment's colour as `RRGGBB` (empty: none), the key the colour sort and filter use.
pub fn color_key(a: &Annotation) -> String {
    a.color.map(|c| format!("{:02X}{:02X}{:02X}", (c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8)).unwrap_or_default()
}

/// The filter's name for a checkmark state.
fn check_label(marked: bool) -> &'static str {
    if marked { "Checked" } else { "Unchecked" }
}

/// The latest checkmark reply in a thread says "Marked".
pub fn is_marked(thread: &[&Annotation]) -> bool {
    thread.iter().rev().find(|r| r.is_mark()).is_some_and(|r| r.state.as_deref() == Some("Marked"))
}

fn matches(a: &Annotation, q: &str) -> bool {
    let q = q.to_lowercase();
    [a.author.as_deref(), a.contents.as_deref(), Some(kind_label(a))].into_iter().flatten().any(|s| s.to_lowercase().contains(&q))
}

// ─────────────────────────────────────────────────────────────── index and card list
//
// A document can carry thousands of comments. Every frame the panel used to filter all of them,
// look up each one's replies with a scan of all annotations, count each page's comments with
// another scan, and lay out every card. Now the facts that only change with the document are
// indexed once per annotation revision, the filtered and sorted card list is rebuilt only when
// the revision, the search or a filter changes, and only the cards in view are laid out (their
// heights are measured as they are drawn and kept in a Fenwick tree, so finding the first visible
// card and the space above and below it is O(log n)).

/// Which annotation list an index was built from: the document's edit generation, and the
/// list's length and address (a new list after any refresh).
type Revision = (u64, usize, usize);

fn revision(info: &DocInfo, generation: u64) -> Revision {
    (generation, info.annotations.len(), info.annotations.as_ptr() as usize)
}

/// What the panel needs about a document's comments, built once per annotation revision.
#[derive(Clone, Debug, Default)]
struct CommentIndex {
    revision: Revision,
    /// Comments and replies, not status changes (the header number).
    total: usize,
    /// Top-level comments (indexes into `info.annotations`), in document order.
    roots: Vec<usize>,
    /// Replies (status changes and checkmarks included) to each annotation name, in order.
    replies: HashMap<String, Vec<usize>>,
    /// Top-level comments on each page.
    per_page: Vec<usize>,
    /// The filter menu's choices: comment types, authors and colours (key, colour).
    types: Vec<String>,
    authors: Vec<String>,
    colors: Vec<(String, Option<[f32; 3]>)>,
}

impl CommentIndex {
    fn build(info: &DocInfo, revision: Revision) -> Self {
        let mut ix = CommentIndex { revision, per_page: vec![0; info.pages.len()], ..Default::default() };
        for (i, a) in info.annotations.iter().enumerate() {
            if a.state.is_none() {
                ix.total += 1;
            }
            match &a.in_reply_to {
                Some(parent) => ix.replies.entry(parent.clone()).or_default().push(i),
                None => {
                    ix.roots.push(i);
                    if let Some(n) = ix.per_page.get_mut(a.page) {
                        *n += 1;
                    }
                }
            }
        }
        let roots = || ix.roots.iter().filter_map(|&i| info.annotations.get(i));
        let mut types: Vec<String> = roots().map(|a| kind_label(a).to_string()).collect();
        types.sort();
        types.dedup();
        let mut authors: Vec<String> = roots().map(|a| a.author.clone().unwrap_or_default()).collect();
        authors.sort();
        authors.dedup();
        let mut colors: Vec<(String, Option<[f32; 3]>)> = roots().map(|a| (color_key(a), a.color)).collect();
        colors.sort_by(|a, b| a.0.cmp(&b.0));
        colors.dedup_by(|a, b| a.0 == b.0);
        (ix.types, ix.authors, ix.colors) = (types, authors, colors);
        ix
    }

    /// The replies to `a` (none if it has no name), in document order.
    fn thread<'a>(&self, info: &'a DocInfo, a: &Annotation) -> Vec<&'a Annotation> {
        a.name.as_deref().and_then(|nm| self.replies.get(nm)).map(|r| r.iter().filter_map(|&i| info.annotations.get(i)).collect()).unwrap_or_default()
    }
}

/// A group header above a card.
#[derive(Clone, Debug)]
enum Header {
    /// "Page N" with that page's number of comments.
    Page(usize),
    /// An author, type or colour; `translate` unless it is an author's own name.
    Group { title: String, translate: bool },
}

/// One card of the list (and the header that starts its group, if any).
#[derive(Clone, Debug)]
struct Row {
    /// Index into `info.annotations`.
    annotation: usize,
    header: Option<Header>,
}

/// What the card list was built for: the revision, the search and the filters.
#[derive(Clone, Debug, Default, PartialEq)]
struct ListKey {
    revision: Revision,
    query: Option<String>,
    hidden_types: Vec<String>,
    hidden_authors: Vec<String>,
    hidden_statuses: Vec<String>,
    hidden_colors: Vec<String>,
    hidden_checks: Vec<String>,
    sort: SortBy,
}

impl ListKey {
    fn of(cv: &CommentView, revision: Revision) -> Self {
        ListKey {
            revision,
            query: query_of(cv).map(str::to_string),
            hidden_types: cv.hidden_types.clone(),
            hidden_authors: cv.hidden_authors.clone(),
            hidden_statuses: cv.hidden_statuses.clone(),
            hidden_colors: cv.hidden_colors.clone(),
            hidden_checks: cv.hidden_checks.clone(),
            sort: cv.sort,
        }
    }

    /// Compares without allocating (this runs every frame).
    fn is_for(&self, cv: &CommentView, revision: Revision) -> bool {
        self.revision == revision
            && self.query.as_deref() == query_of(cv)
            && self.hidden_types == cv.hidden_types
            && self.hidden_authors == cv.hidden_authors
            && self.hidden_statuses == cv.hidden_statuses
            && self.hidden_colors == cv.hidden_colors
            && self.hidden_checks == cv.hidden_checks
            && self.sort == cv.sort
    }
}

fn query_of(cv: &CommentView) -> Option<&str> {
    cv.search.as_deref().filter(|q| !q.trim().is_empty())
}

/// Row heights with O(log n) update, prefix sum and lookup by offset (a Fenwick tree).
#[derive(Clone, Debug, Default)]
struct Heights {
    tree: Vec<f64>,
    value: Vec<f64>,
}

impl Heights {
    fn new(value: Vec<f64>) -> Self {
        let n = value.len();
        let mut tree = value.clone();
        for i in 1..=n {
            let parent = i + (i & i.wrapping_neg());
            if parent <= n {
                let v = tree[i - 1];
                tree[parent - 1] += v;
            }
        }
        Heights { tree, value }
    }

    fn len(&self) -> usize {
        self.value.len()
    }

    fn get(&self, i: usize) -> f64 {
        self.value.get(i).copied().unwrap_or(0.0)
    }

    fn set(&mut self, i: usize, h: f64) {
        let Some(old) = self.value.get_mut(i) else { return };
        let delta = h - *old;
        *old = h;
        let mut j = i + 1;
        while let Some(t) = self.tree.get_mut(j - 1) {
            *t += delta;
            j += j & j.wrapping_neg();
        }
    }

    /// The total height of rows `0..i`.
    fn prefix(&self, i: usize) -> f64 {
        let mut s = 0.0;
        let mut j = i.min(self.len());
        while j > 0 {
            s += self.tree.get(j - 1).copied().unwrap_or(0.0);
            j -= j & j.wrapping_neg();
        }
        s
    }

    fn total(&self) -> f64 {
        self.prefix(self.len())
    }

    /// The row at offset `y` from the top of the list (the last row when `y` is past the end).
    fn row_at(&self, y: f64) -> usize {
        let n = self.len();
        let (mut pos, mut rest) = (0, y);
        let mut step = if n == 0 { 0 } else { 1 << (usize::BITS - 1 - n.leading_zeros()) };
        while step > 0 {
            if let Some(&t) = self.tree.get(pos + step - 1)
                && t <= rest
            {
                pos += step;
                rest -= t;
            }
            step >>= 1;
        }
        pos.min(n.saturating_sub(1))
    }
}

/// The panel's cached index, card list and measured card heights (part of a document's
/// [`CommentView`]).
#[derive(Clone, Debug, Default)]
pub struct PanelCache {
    index: CommentIndex,
    list: Option<ListKey>,
    rows: Vec<Row>,
    /// Where each listed comment is: (page, index in `/Annots`) → row.
    row_of: HashMap<(usize, usize), usize>,
    heights: Heights,
    stats: PanelStats,
}

impl PanelCache {
    pub fn stats(&self) -> PanelStats {
        self.stats
    }

    /// Comments and replies in the document (the panel header's number).
    pub fn total(&self) -> usize {
        self.index.total
    }
}

/// Bring the index up to date with the document; nearly free when nothing changed.
pub(crate) fn refresh(view: &mut DocView, info: &DocInfo, generation: u64) {
    let cache = &mut view.comments.panel;
    let rev = revision(info, generation);
    if cache.stats.index_builds == 0 || cache.index.revision != rev {
        cache.index = CommentIndex::build(info, rev);
        cache.stats.index_builds += 1;
    }
}

/// Rebuild the filtered, sorted card list when the revision, search or filters changed.
fn refresh_list(cache: &mut PanelCache, info: &DocInfo, cv: &CommentView) {
    let rev = cache.index.revision;
    if cache.list.as_ref().is_some_and(|k| k.is_for(cv, rev)) {
        return;
    }
    let ix = &cache.index;
    let query = query_of(cv);
    let status_of =
        |thread: &[&Annotation]| thread.iter().rev().filter(|r| !r.is_mark()).find_map(|r| r.state.clone()).unwrap_or_else(|| "None".into());
    let mut roots: Vec<(usize, &Annotation)> = ix
        .roots
        .iter()
        .filter_map(|&i| info.annotations.get(i).map(|a| (i, a)))
        .filter(|(_, a)| {
            let thread = ix.thread(info, a);
            query.is_none_or(|q| matches(a, q) || thread.iter().any(|r| matches(r, q)))
                && !cv.hidden_types.iter().any(|t| t == kind_label(a))
                && !cv.hidden_authors.contains(&a.author.clone().unwrap_or_default())
                && !cv.hidden_statuses.contains(&status_of(&thread))
                && !cv.hidden_colors.contains(&color_key(a))
                && !cv.hidden_checks.contains(&check_label(is_marked(&thread)).to_string())
        })
        .collect();
    let sort = cv.sort;
    match sort {
        SortBy::Page => {}
        SortBy::Author => roots.sort_by_key(|(_, a)| a.author.clone().unwrap_or_default().to_lowercase()),
        SortBy::Date => roots.sort_by(|(_, a), (_, b)| b.modified.cmp(&a.modified)),
        SortBy::Type => roots.sort_by_key(|(_, a)| kind_label(a).to_string()),
        SortBy::Color => roots.sort_by_key(|(_, a)| color_key(a)),
    }
    // Group headers follow the sort key (no headers when sorted by date).
    let group_of = |a: &Annotation| -> Option<(String, bool)> {
        match sort {
            SortBy::Author => Some(match &a.author {
                Some(name) => (name.clone(), false),
                None => ("Unknown author".into(), true),
            }),
            SortBy::Type => Some((kind_label(a).to_string(), true)),
            SortBy::Color => Some((if a.color.is_some() { format!("#{}", color_key(a)) } else { "No colour".into() }, true)),
            SortBy::Page | SortBy::Date => None,
        }
    };
    let mut rows = Vec::with_capacity(roots.len());
    let mut estimates = Vec::with_capacity(roots.len());
    let mut row_of = HashMap::with_capacity(roots.len());
    let mut page = usize::MAX;
    let mut group: Option<String> = None;
    for (i, a) in roots {
        let header = if sort == SortBy::Page {
            (a.page != page).then(|| {
                page = a.page;
                Header::Page(a.page)
            })
        } else {
            match group_of(a) {
                Some((title, translate)) if group.as_deref() != Some(title.as_str()) => {
                    group = Some(title.clone());
                    Some(Header::Group { title, translate })
                }
                _ => None,
            }
        };
        // A first guess until the card is drawn and measured.
        let replies = a.name.as_deref().and_then(|nm| ix.replies.get(nm)).map_or(0, Vec::len);
        estimates.push(CARD_ESTIMATE + if header.is_some() { HEADER_ESTIMATE } else { 0.0 } + REPLY_ESTIMATE * replies.min(20) as f64);
        row_of.insert((a.page, a.index), rows.len());
        rows.push(Row { annotation: i, header });
    }
    cache.heights = Heights::new(estimates);
    cache.stats.listed = rows.len();
    cache.stats.list_builds += 1;
    cache.rows = rows;
    cache.row_of = row_of;
    cache.list = Some(ListKey::of(cv, rev));
}

/// Height guesses for cards not drawn yet (measured heights replace them).
const CARD_ESTIMATE: f64 = 84.0;
const HEADER_ESTIMATE: f64 = 26.0;
const REPLY_ESTIMATE: f64 = 50.0;

/// Draw the panel body inside the panel's scroll area (`viewport`: its visible part, in content
/// coordinates). Returns an edit to apply (posting, replying, deleting…).
#[allow(clippy::too_many_arguments)]
pub(crate) fn show(
    ui: &mut egui::Ui,
    viewport: egui::Rect,
    t: &Tokens,
    info: &DocInfo,
    generation: u64,
    view: &mut DocView,
    prefs: &CommentPrefs,
    allowed: bool,
    nav: &mut Option<Nav>,
) -> Option<Edit> {
    refresh(view, info, generation);
    // The cache is set aside while cards (which change the view) are drawn.
    let mut cache = std::mem::take(&mut view.comments.panel);
    let edit = show_cached(ui, viewport, t, info, &mut cache, view, prefs, allowed, nav);
    view.comments.panel = cache;
    edit
}

#[allow(clippy::too_many_arguments)]
fn show_cached(
    ui: &mut egui::Ui,
    viewport: egui::Rect,
    t: &Tokens,
    info: &DocInfo,
    cache: &mut PanelCache,
    view: &mut DocView,
    prefs: &CommentPrefs,
    allowed: bool,
    nav: &mut Option<Nav>,
) -> Option<Edit> {
    let origin = ui.max_rect().top();
    cache.stats.drawn = 0;
    let mut edit = None;
    // Search (the header's magnifier toggles it).
    if let Some(q) = view.comments.search.as_mut() {
        let r = ui.add(egui::TextEdit::singleline(q).hint_text(tl!("Search comments")).desired_width(f32::INFINITY).id_salt("comment-search"));
        if view.comments.search_focus {
            r.request_focus();
            view.comments.search_focus = false;
        }
        ui.add_space(6.0);
    }
    // "Add a comment": a sticky note on the current page, near its top-right corner.
    if allowed {
        let r = ui.add(
            egui::TextEdit::singleline(&mut view.comments.add_box)
                .hint_text(tl!("Add a comment"))
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(8, 6))
                .id_salt("comment-add-box"),
        );
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !view.comments.add_box.trim().is_empty() {
            let page = view.current.min(info.pages.len().saturating_sub(1));
            if let Some(p) = info.pages.get(page) {
                let at = p.view_to_user(p.width - 44.0, 24.0);
                let text = std::mem::take(&mut view.comments.add_box);
                edit = Some(Edit::AddAnnotation(pdfcraft_engine::NewAnnotation {
                    page,
                    shape: Shape::Note { at: [at[0] as f64, at[1] as f64], icon: NoteIcon::Comment },
                    style: prefs.style(CommentTool::Note),
                    contents: text.trim().to_string(),
                    author: prefs.author.clone(),
                }));
            }
        }
        ui.add_space(8.0);
    }
    if info.annotations.is_empty() {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.add(icons::image("message-square-text", 36.0, t.text_faint));
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tl!("No comments yet.")).color(t.text_muted));
            if allowed {
                ui.label(egui::RichText::new(tl!("Pick a tool in the quick bar, or type above.")).color(t.text_faint).small());
            }
        });
        return edit;
    }
    refresh_list(cache, info, &view.comments);
    if cache.rows.is_empty() {
        ui.label(egui::RichText::new(tl!("No comments match.")).color(t.text_muted));
        return edit;
    }
    // Only the cards in view are laid out; the space above and below stands in for the rest.
    let top = f64::from(ui.cursor().top() - origin);
    let first = cache.heights.row_at(f64::from(viewport.top()) - top);
    ui.add_space(cache.heights.prefix(first) as f32);
    let mut row = first;
    while let Some(r) = cache.rows.get(row) {
        let start = ui.cursor().top();
        if f64::from(start - origin) > f64::from(viewport.bottom()) && row > first {
            break;
        }
        let Some(a) = info.annotations.get(r.annotation) else { break };
        if let Some(h) = &r.header {
            draw_header(ui, t, info, h, cache.index.per_page.get(a.page).copied().unwrap_or(0));
        }
        let thread = cache.index.thread(info, a);
        if let Some(e) = card(ui, t, a, &thread, view, prefs, allowed, nav) {
            edit = Some(e);
        }
        let measured = f64::from(ui.cursor().top() - start);
        if (measured - cache.heights.get(row)).abs() > 0.5 {
            cache.heights.set(row, measured);
            // The space below (and the scroll bar) follow the measured height next frame.
            ui.ctx().request_repaint();
        }
        cache.stats.drawn += 1;
        row += 1;
    }
    ui.add_space((cache.heights.total() - cache.heights.prefix(row)).max(0.0) as f32);
    // Scroll a selected card that is out of view to where it will be (the card itself centres
    // it precisely once drawn).
    if view.comments.reveal
        && let Some(&target) = view.comments.selected.and_then(|key| cache.row_of.get(&key))
        && !(first..row).contains(&target)
    {
        let y = origin + (top + cache.heights.prefix(target)) as f32;
        let rect = egui::Rect::from_min_size(egui::pos2(ui.max_rect().left(), y), vec2(ui.max_rect().width(), cache.heights.get(target) as f32));
        ui.scroll_to_rect(rect, Some(Align::Center));
    }
    edit
}

fn draw_header(ui: &mut egui::Ui, t: &Tokens, info: &DocInfo, header: &Header, count: usize) {
    match header {
        Header::Group { title, translate } => {
            ui.add_space(6.0);
            let shown = if *translate { tl!(title) } else { title.as_str() };
            ui.label(egui::RichText::new(shown).font(theme::semibold(12.5)).color(t.text_muted));
        }
        Header::Page(page) => {
            let label = info.pages.get(*page).map_or("?", |p| p.label.as_str());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(icons::image("chevron-down", 14.0, t.text_muted));
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(tl!("Page {label}"), &[("label", label)])).font(theme::semibold(12.5)).color(t.text_muted),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(egui::RichText::new(count.to_string()).color(t.text_faint).small())
                });
            });
            ui.add_space(2.0);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn card(
    ui: &mut egui::Ui,
    t: &Tokens,
    a: &Annotation,
    thread: &[&Annotation],
    view: &mut DocView,
    prefs: &CommentPrefs,
    allowed: bool,
    nav: &mut Option<Nav>,
) -> Option<Edit> {
    let mut edit = None;
    let key = (a.page, a.index);
    let selected = view.comments.selected == Some(key);
    let color = a.color.map(|c| color32(c.map(f64::from))).unwrap_or(t.accent);
    let status = thread.iter().rev().filter(|r| !r.is_mark()).find_map(|r| r.state.as_deref());
    let marked = is_marked(thread);
    let replies: Vec<&&Annotation> = thread.iter().filter(|r| r.state.is_none()).collect();
    let editing = view.comments.editing.as_ref().is_some_and(|(p, i, _)| (*p, *i) == key);
    let fill = if selected { t.hover.gamma_multiply(0.7) } else { Color32::TRANSPARENT };
    let mut open_menu = false;
    let mut more_rect = None;
    let frame = egui::Frame::NONE
        .fill(fill)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin { left: 10, right: 8, top: 10, bottom: 10 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut buttons_left = f32::INFINITY;
            let head = ui.horizontal(|ui| {
                avatar(ui, t, Some(subtype_icon(&a.subtype)), None, color);
                ui.label(egui::RichText::new(a.author.as_deref().unwrap_or_else(|| tl!("Unknown author"))).font(theme::semibold(12.5)).color(t.text));
                if let Some(m) = &a.modified {
                    ui.add(egui::Label::new(egui::RichText::new(m).font(theme::regular(11.0)).color(t.text_faint)).truncate());
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if selected && allowed {
                        let more = icons::button(ui, "ellipsis", 22.0, false, tl!("More"));
                        open_menu = more.clicked();
                        more_rect = Some(more.rect);
                    }
                    // Acrobat's per-comment checkmark: a toggle on the selected card, a mark otherwise.
                    let tip = if marked { tl!("Remove checkmark") } else { tl!("Mark with checkmark") };
                    if selected && allowed {
                        if icons::button(ui, "circle-check", 22.0, marked, tip).clicked() {
                            edit = Some(Edit::MarkAnnotation { page: a.page, index: a.index, marked: !marked, author: prefs.author.clone() });
                        }
                    } else if marked {
                        ui.add(icons::image("circle-check", 14.0, t.accent)).on_hover_text(tl!("Marked"));
                    }
                    if a.locked {
                        ui.add(icons::image("lock", 13.0, t.text_faint)).on_hover_text(tl!("Locked"));
                    }
                    buttons_left = ui.min_rect().left();
                });
            });
            egui::Frame::NONE.inner_margin(egui::Margin { left: 34, right: 0, top: 4, bottom: 0 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if editing && let Some((_, _, text)) = view.comments.editing.as_mut() {
                    let r = ui.add(egui::TextEdit::multiline(text).desired_rows(2).desired_width(f32::INFINITY).id_salt(("comment-edit", key)));
                    if !r.has_focus() && !ui.memory(|m| m.focused().is_some()) {
                        r.request_focus();
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::Button::new(egui::RichText::new(tl!("Save")).color(Color32::WHITE)).fill(t.accent).corner_radius(12))
                            .clicked()
                            && let Some((page, index, text)) = view.comments.editing.take()
                        {
                            edit = Some(Edit::SetAnnotationContents { page, index, text });
                        }
                        if ui.button(tl!("Cancel")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            view.comments.editing = None;
                        }
                    });
                } else {
                    let body = a.contents.as_deref().unwrap_or("");
                    let text = if body.is_empty() {
                        egui::RichText::new(tl!(kind_label(a))).italics().color(t.text_faint)
                    } else {
                        egui::RichText::new(body).color(t.text)
                    };
                    let mut label = egui::Label::new(text.font(theme::regular(13.0))).wrap();
                    if !selected {
                        label = label.truncate();
                    }
                    ui.add(label);
                }
                if let Some((icon, label, c)) = status.and_then(status_badge) {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add(icons::image(icon, 13.0, c));
                        ui.label(egui::RichText::new(tl!(label)).font(theme::medium(11.5)).color(c));
                    });
                }
            });
            // Replies, joined to the parent by a thread line.
            for r in &replies {
                ui.add_space(8.0);
                let resp = ui.horizontal(|ui| {
                    avatar(ui, t, None, Some(&initials(r.author.as_deref().unwrap_or("?"))), t.text_faint);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(r.author.as_deref().unwrap_or_else(|| tl!("Reply"))).font(theme::semibold(12.0)));
                            if let Some(m) = &r.modified {
                                ui.add(egui::Label::new(egui::RichText::new(m).font(theme::regular(11.0)).color(t.text_faint)).truncate());
                            }
                        });
                        ui.add(
                            egui::Label::new(egui::RichText::new(r.contents.as_deref().unwrap_or("")).font(theme::regular(12.5)).color(t.text))
                                .wrap(),
                        );
                    });
                });
                let x = resp.response.rect.left() + 12.0;
                ui.painter().vline(x, (resp.response.rect.top() - 14.0)..=(resp.response.rect.top() - 2.0), Stroke::new(1.5, t.border));
            }
            if selected && allowed {
                ui.add_space(8.0);
                egui::Frame::NONE.inner_margin(egui::Margin { left: 34, right: 0, top: 0, bottom: 0 }).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut view.comments.reply)
                                .hint_text(tl!("Add a reply"))
                                .desired_width(ui.available_width() - 56.0)
                                .id_salt(("comment-reply", key)),
                        );
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let ok = !view.comments.reply.trim().is_empty();
                        if (ui.add_enabled(ok, egui::Button::new(tl!("Post")).corner_radius(12)).clicked() || enter) && ok {
                            let text = std::mem::take(&mut view.comments.reply).trim().to_string();
                            edit = Some(Edit::ReplyToAnnotation { page: a.page, index: a.index, text, author: prefs.author.clone() });
                        }
                    });
                });
            }
            // The header selects the card, except where its buttons are.
            let mut r = head.response.rect;
            r.max.x = r.max.x.min(buttons_left - 2.0);
            r
        });
    let rect = frame.response.rect;
    if selected {
        let bar = egui::Rect::from_min_size(rect.min, vec2(4.0, rect.height()));
        ui.painter().rect_filled(bar, CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 }, CARD_ACCENT);
        if std::mem::take(&mut view.comments.reveal) {
            frame.response.scroll_to_me(Some(Align::Center));
        }
    }
    ui.painter().hline(rect.x_range().shrink(10.0), rect.bottom() + 2.0, Stroke::new(1.0, t.divider));
    ui.add_space(5.0);
    // The header row selects the card (and shows the comment on the page); unselected cards are
    // clickable everywhere.
    let hit = if selected { frame.inner } else { rect };
    let click = ui
        .interact(hit, ui.id().with(("card", key)), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(crate::i18n::fmt(tl!("{kind} — click to show it on the page"), &[("kind", tl!(kind_label(a)))]));
    if click.clicked() {
        view.comments.selected = Some(key);
        view.comments.reply.clear();
        *nav = Some(Nav::Flash(a.page, a.rect));
    }
    if click.double_clicked() && allowed {
        view.comments.editing = Some((a.page, a.index, a.contents.clone().unwrap_or_default()));
    }
    let menu_id = ui.id().with(("card-menu", key));
    if open_menu {
        egui::Popup::open_id(ui.ctx(), menu_id);
    }
    // Hang the menu from the "…" button (a pointer anchor would follow the mouse to the items).
    let anchor = more_rect.map_or(egui::PopupAnchor::PointerFixed, egui::PopupAnchor::ParentRect);
    egui::Popup::new(menu_id, ui.ctx().clone(), anchor, ui.layer_id())
        .open_memory(None)
        .kind(egui::PopupKind::Menu)
        .layout(Layout::top_down_justified(Align::Min))
        .show(|ui| {
            ui.set_min_width(160.0);
            if ui.button(tl_ctx!("comment menu", "Edit")).clicked() {
                view.comments.editing = Some((a.page, a.index, a.contents.clone().unwrap_or_default()));
                ui.close();
            }
            ui.menu_button(tl!("Set status"), |ui| {
                for s in [ReviewState::None, ReviewState::Accepted, ReviewState::Cancelled, ReviewState::Completed, ReviewState::Rejected] {
                    if ui.button(tl!(s.name())).clicked() {
                        edit = Some(Edit::SetAnnotationStatus { page: a.page, index: a.index, state: s, author: prefs.author.clone() });
                        ui.close();
                    }
                }
            });
            if ui.button(tl!(if marked { "Remove checkmark" } else { "Mark with checkmark" })).clicked() {
                edit = Some(Edit::MarkAnnotation { page: a.page, index: a.index, marked: !marked, author: prefs.author.clone() });
                ui.close();
            }
            if ui.button(tl!("Copy text")).clicked() {
                ui.ctx().copy_text(a.contents.clone().unwrap_or_default());
                ui.close();
            }
            if ui.button(tl!("Properties…")).clicked() {
                view.comments.props_request = Some((a.page, a.index));
                ui.close();
            }
            if ui.add_enabled(crate::comments::tool_for(a).is_some(), egui::Button::new(tl!("Make Current Properties Default"))).clicked() {
                view.comments.default_request = Some((a.page, a.index));
                ui.close();
            }
            ui.separator();
            if ui.add_enabled(!a.locked, egui::Button::new(tl!("Delete"))).clicked() {
                view.comments.selected = None;
                edit = Some(Edit::DeleteAnnotation { page: a.page, index: a.index });
                ui.close();
            }
        });
    edit
}

/// A 26 pt round badge: a type glyph (comments) or initials (replies).
fn avatar(ui: &mut egui::Ui, t: &Tokens, icon: Option<&str>, text: Option<&str>, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
    let bg = if t.dark() { Color32::from_gray(0x55) } else { Color32::from_gray(0xD8) };
    ui.painter().circle_filled(r.center(), 12.0, bg);
    if let Some(i) = icon {
        icons::paint(ui, r, i, 13.0, legible(color, t));
    }
    if let Some(s) = text {
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, s, theme::semibold(10.5), t.text);
    }
}

/// Darken very light annotation colours (e.g. note yellow) so their glyph stays legible.
fn legible(c: Color32, t: &Tokens) -> Color32 {
    let lum = 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
    if !t.dark() && lum > 150.0 {
        Color32::from_rgb((c.r() as f32 * 0.55) as u8, (c.g() as f32 * 0.55) as u8, (c.b() as f32 * 0.55) as u8)
    } else {
        c
    }
}

/// The Comments panel header's filter and sort controls (Acrobat: filter funnel and "…").
/// The header's "…" (sort, hide all, summary, data exchange) and filter menus; returns a
/// command to run.
pub(crate) fn header_controls(ui: &mut egui::Ui, info: &DocInfo, generation: u64, view: &mut DocView, hidden: bool) -> Option<&'static str> {
    refresh(view, info, generation);
    // The filter's choices come from the index (set aside while the menus change the view).
    let cache = std::mem::take(&mut view.comments.panel);
    let command = header_menus(ui, &cache.index, view, hidden);
    view.comments.panel = cache;
    command
}

fn header_menus(ui: &mut egui::Ui, index: &CommentIndex, view: &mut DocView, hidden: bool) -> Option<&'static str> {
    let mut command = None;
    let cv = &mut view.comments;
    let (types, authors, colors) = (&index.types, &index.authors, &index.colors);
    let active = !(cv.hidden_types.is_empty()
        && cv.hidden_authors.is_empty()
        && cv.hidden_statuses.is_empty()
        && cv.hidden_colors.is_empty()
        && cv.hidden_checks.is_empty());
    let more = icons::button(ui, "ellipsis", 26.0, false, tl!("More options"));
    egui::Popup::menu(&more).show(|ui| {
        ui.set_min_width(220.0);
        ui.label(egui::RichText::new(tl!("Sort by")).small());
        for (s, label) in [
            (SortBy::Page, tl!("Page")),
            (SortBy::Author, tl!("Author")),
            (SortBy::Date, tl!("Date")),
            (SortBy::Type, tl!("Type")),
            (SortBy::Color, tl!("Colour")),
        ] {
            if ui.radio(cv.sort == s, label).clicked() {
                cv.sort = s;
                ui.close();
            }
        }
        ui.separator();
        for (id, label) in [
            ("comment.hide_all", if hidden { tl!("Show all comments") } else { tl!("Hide all comments") }),
            ("comment.summarize", tl!("Create PDF comment summary…")),
            ("comment.export", tl!("Export all to data file…")),
            ("comment.import", tl!("Import comments…")),
        ] {
            if ui.button(tl!(label)).clicked() {
                command = Some(id);
                ui.close();
            }
        }
    });
    let filter = icons::button(ui, "filter", 26.0, active, tl!("Filter comments"));
    egui::Popup::menu(&filter).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(200.0);
        let toggle = |ui: &mut egui::Ui, list: &mut Vec<String>, value: &str, label: &str| {
            let mut shown = !list.iter().any(|x| x == value);
            if ui.checkbox(&mut shown, label).changed() {
                if shown {
                    list.retain(|x| x != value);
                } else {
                    list.push(value.to_string());
                }
            }
        };
        ui.label(egui::RichText::new(tl!("Type")).small());
        for ty in types {
            toggle(ui, &mut cv.hidden_types, ty, tl!(ty));
        }
        ui.separator();
        ui.label(egui::RichText::new(tl!("Author")).small());
        for a in authors {
            toggle(ui, &mut cv.hidden_authors, a, if a.is_empty() { tl!("Unknown author") } else { a });
        }
        ui.separator();
        ui.label(egui::RichText::new(tl!("Status")).small());
        for s in ["None", "Accepted", "Rejected", "Cancelled", "Completed"] {
            toggle(ui, &mut cv.hidden_statuses, s, tl!(s));
        }
        ui.separator();
        ui.label(egui::RichText::new(tl!("Colour")).small());
        for (key, c) in colors {
            ui.horizontal(|ui| {
                let name = crate::comments::SWATCHES
                    .iter()
                    .find(|(_, s)| c.is_some_and(|c| (0..3).all(|i| (s[i] as f32 - c[i]).abs() < 0.02)))
                    .map(|(n, _)| tl!(n).to_string())
                    .unwrap_or_else(|| if key.is_empty() { tl!("No colour").to_string() } else { format!("#{key}") });
                toggle(ui, &mut cv.hidden_colors, key, &name);
                if let Some(c) = c {
                    let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                    ui.painter().rect_filled(
                        r,
                        CornerRadius::same(2),
                        Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8),
                    );
                }
            });
        }
        ui.separator();
        ui.label(egui::RichText::new(tl!("Checkmark")).small());
        for s in ["Checked", "Unchecked"] {
            toggle(ui, &mut cv.hidden_checks, s, tl!(s));
        }
        if active && ui.button(tl!("Show all")).clicked() {
            cv.hidden_types.clear();
            cv.hidden_authors.clear();
            cv.hidden_statuses.clear();
            cv.hidden_colors.clear();
            cv.hidden_checks.clear();
        }
    });
    command
}

#[cfg(test)]
mod tests {
    use super::Heights;

    #[test]
    fn heights_find_rows_and_sum_like_a_plain_scan() {
        let mut values: Vec<f64> = (0..37).map(|i| 10.0 + (i % 5) as f64 * 7.0).collect();
        let mut h = Heights::new(values.clone());
        h.set(3, 100.0);
        values[3] = 100.0;
        h.set(36, 1.0);
        values[36] = 1.0;
        for i in 0..=values.len() {
            assert!((h.prefix(i) - values[..i].iter().sum::<f64>()).abs() < 1e-9, "prefix {i}");
        }
        assert!((h.total() - values.iter().sum::<f64>()).abs() < 1e-9);
        for (i, _) in values.iter().enumerate() {
            let top = values[..i].iter().sum::<f64>();
            assert_eq!(h.row_at(top), i, "a row's top is in it");
            assert_eq!(h.row_at(top + values[i] - 0.5), i, "a row's bottom is in it");
        }
        assert_eq!(h.row_at(-5.0), 0);
        assert_eq!(h.row_at(1e9), values.len() - 1);
        assert_eq!(Heights::new(Vec::new()).row_at(3.0), 0);
        h.set(99, 5.0); // out of range: ignored
        assert_eq!(h.len(), values.len());
    }
}
