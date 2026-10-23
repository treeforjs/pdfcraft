//! Editing bookmarks (the document outline, ISO 32000-2 §12.3.3), execution plan M4.6.
//!
//! Bookmarks are addressed by their **path**: child indices from the top level, so `[0, 2]` is
//! the third child of the first top-level bookmark. Paths follow `/First`–`/Next` order, which
//! is also the order viewers (and our inspector) list them in.
//!
//! Edits are surgical: they relink only the affected siblings (`/Parent`, `/Prev`, `/Next`,
//! `/First`, `/Last`) and recompute `/Count` values, and an object is rewritten only when one of
//! its values changes. Everything else on an item (colour, style, actions, unknown keys) is kept.
//! Walks are cycle- and size-safe; a broken outline is never followed forever.

use std::collections::HashSet;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString};
use pdfcraft_model::dests;
pub use pdfcraft_model::dests::Target;

use crate::{OrganizeError, walk};

/// Hard cap on items visited (broken or hostile outlines).
const MAX_ITEMS: usize = 100_000;

/// A bookmark as stored in the file.
#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    pub obj: ObjRef,
    pub title: String,
    /// Shown expanded (a positive `/Count`) when it has children.
    pub open: bool,
    pub children: Vec<Bookmark>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OutlineError {
    #[error("there is no bookmark at {0:?}")]
    NoSuchBookmark(Vec<usize>),
    #[error("a bookmark cannot be moved inside itself")]
    IntoItself,
    #[error("the title must not be empty")]
    EmptyTitle,
    #[error("{0}")]
    Action(String),
    #[error("{0}")]
    Organize(#[from] OrganizeError),
    #[error("{0}")]
    Cos(#[from] pdfcraft_cos::CosError),
}

type Result<T> = std::result::Result<T, OutlineError>;

fn outline_root(doc: &Document) -> Option<ObjRef> {
    let root = doc.root()?;
    doc.get(root).as_dict()?.reference(b"Outlines")
}

/// The children of `parent` (an item or the outline root), in order. Cycle-safe.
fn children_of(doc: &Document, parent: ObjRef, seen: &mut HashSet<ObjRef>) -> Vec<ObjRef> {
    let mut out = Vec::new();
    let mut next = doc.get(parent).as_dict().and_then(|d| d.reference(b"First"));
    while let Some(r) = next {
        if seen.len() >= MAX_ITEMS || !seen.insert(r) {
            break;
        }
        let item = doc.get(r);
        let Some(d) = item.as_dict() else { break };
        out.push(r);
        next = d.reference(b"Next");
    }
    out
}

/// Every bookmark item, parents before their children. Cycle-safe.
pub(crate) fn items(doc: &Document) -> Vec<ObjRef> {
    let Some(root) = outline_root(doc) else { return Vec::new() };
    let (mut seen, mut out, mut stack) = (HashSet::new(), Vec::new(), vec![root]);
    while let Some(parent) = stack.pop() {
        let kids = children_of(doc, parent, &mut seen);
        stack.extend(kids.iter().copied());
        out.extend(kids);
    }
    out
}

/// The bookmark tree.
pub fn bookmarks(doc: &Document) -> Vec<Bookmark> {
    let Some(root) = outline_root(doc) else { return Vec::new() };
    let mut seen = HashSet::from([root]);
    fn build(doc: &Document, parent: ObjRef, seen: &mut HashSet<ObjRef>) -> Vec<Bookmark> {
        children_of(doc, parent, seen)
            .into_iter()
            .map(|r| {
                let item = doc.get(r);
                let d = item.as_dict().cloned().unwrap_or_default();
                let title = d.get(b"Title").map(|t| doc.resolve(t)).and_then(|t| t.as_string().map(|s| s.to_text())).unwrap_or_default();
                let open = d.int(b"Count").is_some_and(|c| c > 0);
                Bookmark { obj: r, title, open, children: build(doc, r, seen) }
            })
            .collect()
    }
    build(doc, root, &mut seen)
}

/// The object of the bookmark at `path`, and its parent object (the outline root for top-level).
pub(crate) fn resolve_path(doc: &Document, path: &[usize]) -> Result<(ObjRef, ObjRef)> {
    let root = outline_root(doc).ok_or_else(|| OutlineError::NoSuchBookmark(path.to_vec()))?;
    if path.is_empty() {
        return Err(OutlineError::NoSuchBookmark(Vec::new()));
    }
    let mut parent = root;
    let mut seen = HashSet::from([root]);
    for (depth, i) in path.iter().enumerate() {
        let kids = children_of(doc, parent, &mut seen);
        let r = *kids.get(*i).ok_or_else(|| OutlineError::NoSuchBookmark(path.to_vec()))?;
        if depth + 1 == path.len() {
            return Ok((r, parent));
        }
        parent = r;
    }
    // The loop returns on the last index.
    Err(OutlineError::NoSuchBookmark(path.to_vec()))
}

/// The object that holds children at `parent_path` (`[]` = the outline root, created if needed).
fn container(doc: &mut Document, parent_path: &[usize]) -> Result<ObjRef> {
    if parent_path.is_empty() {
        if let Some(r) = outline_root(doc) {
            return Ok(r);
        }
        let mut o = Dict::new();
        o.set(b"Type".to_vec(), Object::name("Outlines"));
        let r = doc.add(Object::Dict(o));
        let root = doc.root().ok_or(OrganizeError::NoPageTree)?;
        doc.update_dict(root, |c| c.set(b"Outlines".to_vec(), Object::Ref(r)))?;
        return Ok(r);
    }
    Ok(resolve_path(doc, parent_path)?.0)
}

/// Set `key` on the dictionary `r` (or remove it with `None`) only if it changes.
fn put(doc: &mut Document, r: ObjRef, key: &[u8], value: Option<Object>) -> Result<()> {
    let current = doc.get(r).as_dict().and_then(|d| d.get(key).cloned());
    if current == value {
        return Ok(());
    }
    doc.update_dict(r, |d| match value {
        Some(v) => d.set(key.to_vec(), v),
        None => {
            d.remove(key);
        }
    })?;
    Ok(())
}

/// Make `kids` the children of `parent`, in order: rewires `/Parent`, `/Prev`, `/Next`,
/// `/First` and `/Last`.
fn relink(doc: &mut Document, parent: ObjRef, kids: &[ObjRef]) -> Result<()> {
    for (i, k) in kids.iter().enumerate() {
        put(doc, *k, b"Parent", Some(Object::Ref(parent)))?;
        put(doc, *k, b"Prev", i.checked_sub(1).map(|p| Object::Ref(kids[p])))?;
        put(doc, *k, b"Next", kids.get(i + 1).map(|n| Object::Ref(*n)))?;
    }
    put(doc, parent, b"First", kids.first().map(|k| Object::Ref(*k)))?;
    put(doc, parent, b"Last", kids.last().map(|k| Object::Ref(*k)))?;
    Ok(())
}

/// Recompute every `/Count` (§12.3.3: open items count their visible descendants, closed items
/// the negative of what opening them would show; the root counts all visible items).
fn recount(doc: &mut Document) -> Result<()> {
    let Some(root) = outline_root(doc) else { return Ok(()) };
    let mut seen = HashSet::from([root]);
    // Returns the number of items visible below `node` when `node` is open.
    fn visit(doc: &mut Document, node: ObjRef, is_root: bool, seen: &mut HashSet<ObjRef>) -> Result<i64> {
        let kids = children_of(doc, node, seen);
        let mut visible = 0i64;
        for k in &kids {
            let below = visit(doc, *k, false, seen)?;
            let open = doc.get(*k).as_dict().and_then(|d| d.int(b"Count")).is_some_and(|c| c > 0);
            visible += 1 + if open { below } else { 0 };
        }
        let count = if kids.is_empty() {
            None
        } else if is_root || doc.get(node).as_dict().and_then(|d| d.int(b"Count")).is_some_and(|c| c > 0) {
            Some(visible)
        } else {
            Some(-visible)
        };
        put(doc, node, b"Count", count.map(Object::Int))?;
        Ok(visible)
    }
    visit(doc, root, true, &mut seen)?;
    Ok(())
}

fn destination(doc: &Document, page: usize) -> Result<Object> {
    let pages = walk(doc)?;
    let (p, _) = pages.get(page).ok_or(OrganizeError::NoSuchPage(page))?;
    // `/XYZ null null null`: go to the page, keeping the reader's zoom (what Acrobat writes for
    // a new bookmark when the view has no specific position).
    Ok(Object::Array(vec![Object::Ref(*p), Object::name("XYZ"), Object::Null, Object::Null, Object::Null]))
}

/// Add a bookmark titled `title` that goes to `page` (0-based), as child `index` of
/// `parent_path` (`[]` = top level; an index past the end appends). Returns its path.
pub fn add_bookmark(doc: &mut Document, parent_path: &[usize], index: usize, title: &str, page: usize) -> Result<Vec<usize>> {
    if title.trim().is_empty() {
        return Err(OutlineError::EmptyTitle);
    }
    let dest = destination(doc, page)?;
    let parent = container(doc, parent_path)?;
    let mut kids = children_of(doc, parent, &mut HashSet::from([parent]));
    let mut d = Dict::new();
    d.set(b"Title".to_vec(), Object::String(PdfString::text(title)));
    d.set(b"Dest".to_vec(), dest);
    let item = doc.add(Object::Dict(d));
    let at = index.min(kids.len());
    kids.insert(at, item);
    relink(doc, parent, &kids)?;
    // Show the new bookmark: open its parent (the root is always open).
    if !parent_path.is_empty() && doc.get(parent).as_dict().and_then(|d| d.int(b"Count")).is_none_or(|c| c <= 0) {
        put(doc, parent, b"Count", Some(Object::Int(1)))?;
    }
    recount(doc)?;
    let mut path = parent_path.to_vec();
    path.push(at);
    Ok(path)
}

/// Change a bookmark's title.
pub fn rename_bookmark(doc: &mut Document, path: &[usize], title: &str) -> Result<()> {
    if title.trim().is_empty() {
        return Err(OutlineError::EmptyTitle);
    }
    let (r, _) = resolve_path(doc, path)?;
    put(doc, r, b"Title", Some(Object::String(PdfString::text(title))))
}

/// Point a bookmark at `page` (0-based), replacing its destination or GoTo action.
pub fn set_bookmark_page(doc: &mut Document, path: &[usize], page: usize) -> Result<()> {
    let dest = destination(doc, page)?;
    let (r, _) = resolve_path(doc, path)?;
    put(doc, r, b"Dest", Some(dest))?;
    put(doc, r, b"A", None) // `/Dest` and `/A` must not both be present (§12.3.3)
}

/// What a bookmark does when clicked (§12.3.3: a `/Dest` or an `/A` action).
#[derive(Clone, Debug, PartialEq)]
pub enum BookmarkAction {
    /// Go to a page view or a named destination (`/Dest`, or a GoTo action).
    GoTo(Target),
    /// Open a web page (a URI action).
    Uri(String),
    /// Run a JavaScript (§12.6.4.17): the script.
    JavaScript(String),
    /// Another action (Launch, Named, a remote go-to…) or a go-to whose destination
    /// isn't in this document: shown by its action type and kept as is.
    Other(String),
    /// Does nothing.
    None,
}

fn action_of(doc: &Document, d: &Dict, pages: &[ObjRef]) -> BookmarkAction {
    if let Some(dest) = d.get(b"Dest") {
        return dests::read_target(doc, dest, pages).map_or_else(|| BookmarkAction::Other("GoTo".into()), BookmarkAction::GoTo);
    }
    let Some(a) = d.get(b"A").and_then(|a| doc.resolve(a).as_dict().cloned()) else { return BookmarkAction::None };
    match a.name(b"S") {
        Some(b"URI") => BookmarkAction::Uri(
            a.get(b"URI").and_then(|u| doc.resolve(u).as_string().map(|s| String::from_utf8_lossy(&s.bytes).into_owned())).unwrap_or_default(),
        ),
        Some(b"GoTo") => a
            .get(b"D")
            .and_then(|dest| dests::read_target(doc, dest, pages))
            .map_or_else(|| BookmarkAction::Other("GoTo".into()), BookmarkAction::GoTo),
        // A script that can't be read (a broken `/JS`) is kept, not offered for editing.
        Some(b"JavaScript") => pdfcraft_model::actions::script(doc, &Object::Dict(a.clone()))
            .map_or_else(|| BookmarkAction::Other("JavaScript".into()), BookmarkAction::JavaScript),
        Some(s) => BookmarkAction::Other(String::from_utf8_lossy(s).into_owned()),
        None => BookmarkAction::Other("unknown".into()),
    }
}

/// Every bookmark's path and action, parents before their children. Cycle- and size-safe, and
/// iterative (no recursion on hostile nesting).
pub fn bookmark_actions(doc: &Document) -> Vec<(Vec<usize>, BookmarkAction)> {
    let Some(root) = outline_root(doc) else { return Vec::new() };
    let pages = dests::page_refs(doc);
    let mut seen = HashSet::from([root]);
    let mut out = Vec::new();
    let mut stack = vec![(root, Vec::new())];
    while let Some((node, path)) = stack.pop() {
        let kids = children_of(doc, node, &mut seen);
        for (i, k) in kids.iter().enumerate().rev() {
            let mut p = path.clone();
            p.push(i);
            stack.push((*k, p));
        }
        if node != root {
            let item = doc.get(node);
            out.push((path, action_of(doc, item.as_dict().unwrap_or(&Dict::new()), &pages)));
        }
    }
    out
}

/// The action of the bookmark at `path`.
pub fn bookmark_action(doc: &Document, path: &[usize]) -> Result<BookmarkAction> {
    let (r, _) = resolve_path(doc, path)?;
    let item = doc.get(r);
    Ok(action_of(doc, item.as_dict().unwrap_or(&Dict::new()), &dests::page_refs(doc)))
}

/// Bookmark Properties ▸ Actions: make the bookmark go to a view or named destination, open a
/// web page or run a JavaScript. Replaces its `/Dest` or `/A` (never both, §12.3.3); everything
/// else on the item is kept. Other action types can't be set here.
pub fn set_bookmark_action(doc: &mut Document, path: &[usize], action: &BookmarkAction) -> Result<()> {
    let (dest, act) = match action {
        BookmarkAction::GoTo(t) => (Some(dests::target_object(doc, t).map_err(|e| OutlineError::Action(e.to_string()))?), None),
        BookmarkAction::Uri(u) => {
            let u = u.trim();
            if u.is_empty() {
                return Err(OutlineError::Action("type the web address first".into()));
            }
            let mut a = Dict::new();
            a.set(b"S".to_vec(), Object::name("URI"));
            a.set(b"URI".to_vec(), PdfString::literal(u.as_bytes().to_vec()));
            (None, Some(Object::Dict(a)))
        }
        BookmarkAction::JavaScript(js) => (None, Some(pdfcraft_model::actions::new_javascript(js).map_err(|e| OutlineError::Action(e.to_string()))?)),
        BookmarkAction::Other(_) | BookmarkAction::None => {
            return Err(OutlineError::Action("a bookmark action can be a page view, a named destination, a web address or a JavaScript".into()));
        }
    };
    let (r, _) = resolve_path(doc, path)?;
    put(doc, r, b"Dest", dest)?;
    put(doc, r, b"A", act)
}

/// Remove a bookmark and everything under it. The removed items are unlinked (a full save drops
/// them; an incremental save leaves the old objects unreferenced).
pub fn delete_bookmark(doc: &mut Document, path: &[usize]) -> Result<()> {
    let (r, parent) = resolve_path(doc, path)?;
    let mut kids = children_of(doc, parent, &mut HashSet::from([parent]));
    kids.retain(|k| *k != r);
    relink(doc, parent, &kids)?;
    recount(doc)
}

/// Move the bookmark at `from` to child `index` of `to_parent` (counted after removing it from
/// its old place; past the end appends). Returns its new path.
pub fn move_bookmark(doc: &mut Document, from: &[usize], to_parent: &[usize], index: usize) -> Result<Vec<usize>> {
    if to_parent.starts_with(from) {
        return Err(OutlineError::IntoItself);
    }
    let (r, old_parent) = resolve_path(doc, from)?;
    let mut old = children_of(doc, old_parent, &mut HashSet::from([old_parent]));
    old.retain(|k| *k != r);
    relink(doc, old_parent, &old)?;
    // Removing `from` shifts later siblings: adjust a target path that runs through them.
    let mut to_parent = to_parent.to_vec();
    let depth = from.len() - 1;
    if to_parent.len() > depth && to_parent[..depth] == from[..depth] && to_parent[depth] > from[depth] {
        to_parent[depth] -= 1;
    }
    let parent = container(doc, &to_parent)?;
    let mut kids = children_of(doc, parent, &mut HashSet::from([parent]));
    let at = index.min(kids.len());
    kids.insert(at, r);
    relink(doc, parent, &kids)?;
    recount(doc)?;
    to_parent.push(at);
    Ok(to_parent)
}

/// Expand or collapse a bookmark that has children.
pub fn set_bookmark_open(doc: &mut Document, path: &[usize], open: bool) -> Result<()> {
    let (r, _) = resolve_path(doc, path)?;
    let count = doc.get(r).as_dict().and_then(|d| d.int(b"Count")).unwrap_or(0);
    if count != 0 && (count > 0) != open {
        put(doc, r, b"Count", Some(Object::Int(-count)))?;
        recount(doc)?;
    }
    Ok(())
}

/// One bookmark of a generated outline (New Bookmarks from Structure): a title, its nesting
/// level (1 = top level of the new bookmarks), and where it goes.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineEntry {
    pub title: String,
    /// 1-based level; an entry nests under the nearest earlier entry with a lower level.
    pub level: usize,
    /// 0-based page.
    pub page: usize,
    /// The user-space y to scroll to (`/XYZ null top null`), or `None` for the top of the page.
    pub top: Option<f64>,
}

/// The deepest nesting [`add_bookmark_tree`] writes; deeper entries become children of the
/// bookmark at this depth.
pub const MAX_TREE_DEPTH: usize = 32;

/// Add `entries`, in order, as a bookmark tree after the existing top-level bookmarks, or in
/// place of every bookmark when `replace` is set (the old items are unlinked, as by
/// [`delete_bookmark`]). Levels need not be consecutive: an H3 right after an H1 nests under it.
/// New bookmarks with children are shown expanded. Returns the number of bookmarks added.
pub fn add_bookmark_tree(doc: &mut Document, entries: &[OutlineEntry], replace: bool) -> Result<usize> {
    let pages = walk(doc)?;
    // Resolve every destination first, so a bad page leaves the document untouched.
    let mut dests = Vec::with_capacity(entries.len().min(MAX_ITEMS));
    for e in entries.iter().take(MAX_ITEMS) {
        if e.title.trim().is_empty() {
            return Err(OutlineError::EmptyTitle);
        }
        let (p, _) = pages.get(e.page).ok_or(OrganizeError::NoSuchPage(e.page))?;
        let top = e.top.filter(|t| t.is_finite()).map_or(Object::Null, Object::Real);
        dests.push(Object::Array(vec![Object::Ref(*p), Object::name("XYZ"), Object::Null, top, Object::Null]));
    }
    let root = container(doc, &[])?;
    let mut top_level = if replace { Vec::new() } else { children_of(doc, root, &mut HashSet::from([root])) };
    // Parents (index into `made`, or `None` for the top level), in entry order.
    let mut made: Vec<ObjRef> = Vec::with_capacity(dests.len());
    let mut kids: Vec<Vec<usize>> = Vec::with_capacity(dests.len());
    let mut new_top: Vec<usize> = Vec::new();
    // Open ancestors: (level, index into `made`).
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for (i, (e, dest)) in entries.iter().zip(dests).enumerate() {
        while stack.last().is_some_and(|(level, _)| *level >= e.level) {
            stack.pop();
        }
        if stack.len() >= MAX_TREE_DEPTH {
            stack.truncate(MAX_TREE_DEPTH - 1);
        }
        let mut d = Dict::new();
        d.set(b"Title".to_vec(), Object::String(PdfString::text(e.title.trim())));
        d.set(b"Dest".to_vec(), dest);
        made.push(doc.add(Object::Dict(d)));
        kids.push(Vec::new());
        match stack.last() {
            Some((_, parent)) => {
                if let Some(k) = kids.get_mut(*parent) {
                    k.push(i);
                }
            }
            None => new_top.push(i),
        }
        stack.push((e.level, i));
    }
    // Link each new parent's children; every new parent is open, so its /Count is the number of
    // its descendants. Children come after their parent, so a reverse pass sees them first.
    let mut descendants = vec![0i64; made.len()];
    for i in (0..made.len()).rev() {
        let (Some(&item), Some(children)) = (made.get(i), kids.get(i)) else { continue };
        let refs: Vec<ObjRef> = children.iter().filter_map(|c| made.get(*c).copied()).collect();
        relink(doc, item, &refs)?;
        let below: i64 = children.iter().map(|c| 1 + descendants.get(*c).copied().unwrap_or(0)).sum();
        if let Some(slot) = descendants.get_mut(i) {
            *slot = below;
        }
        put(doc, item, b"Count", (below > 0).then_some(Object::Int(below)))?;
    }
    top_level.extend(new_top.iter().filter_map(|i| made.get(*i).copied()));
    relink(doc, root, &top_level)?;
    // The root counts every visible item: each top-level item plus, when it is open, what its
    // own (positive) /Count says is visible beneath it. Existing items keep their counts.
    let visible: i64 = top_level
        .iter()
        .map(|k| 1i64.saturating_add(doc.get(*k).as_dict().and_then(|d| d.int(b"Count")).unwrap_or(0).max(0)))
        .fold(0i64, i64::saturating_add);
    put(doc, root, b"Count", (visible > 0).then_some(Object::Int(visible)))?;
    Ok(made.len())
}
