//! Form fields for Autotag Document: every widget annotation (radio and checkbox kids
//! included) becomes a `Form` element holding an object reference to it, with a
//! `/StructParent` on the widget and its entry in the parent tree.
//!
//! - **Placement.** A widget goes next to its label in reading order: after the label when the
//!   label is on its left (same row) or above it, before it when a checkbox or radio button has
//!   its label on the right. The `Form` element goes inside the element holding the label
//!   (a paragraph, list body or table cell), or after it at the top level when the label is a
//!   heading. A widget with no label nearby takes its own place: it is ordered with the page's
//!   blocks by the same row/column cuts as the text.
//! - **Alternate text.** `/Alt` comes from the field's tooltip (`/TU`), else its name (`/T`),
//!   else the label. A radio button adds its option (the label on its right, else its on
//!   state): `Payment: Card`.
//! - **Tooltips.** Fields without a tooltip are reported with one suggested from their label
//!   (else their name); [`FieldTooltips::Fill`] writes the suggestion as `/TU`.
//!
//! The field tree is walked up from each widget through `/Parent` with a seen-set and a depth
//! limit (it is never walked down through `/Kids`), and the number of widgets is capped.

use std::collections::{HashMap, HashSet};

use pdfcraft_cos::{Dict, Document, ObjRef, Object};

use super::{Annot, AutotagFieldTooltip, FieldTooltips, Kid, Layout, Rank, Tree, finite, norm, reading_order};
use crate::content::text_of;

/// `/Parent` links followed up from a widget.
const MAX_FIELD_DEPTH: usize = 32;
/// The most widgets one call tags; those beyond are left untagged and reported.
pub(super) const MAX_FIELDS: usize = 50_000;
/// Widgets × text pieces compared on one page when looking for labels.
const MAX_LABEL_WORK: usize = 20_000_000;
/// Widgets without a label that are ordered with the blocks (more are put after them).
const MAX_ORDERED: usize = 2_000;
/// Characters kept of a label, a tooltip suggestion or an alternate text.
const MAX_LABEL_CHARS: usize = 120;
/// How far a label on the left, on the right or above may be from its widget, in points.
const MAX_LEFT: f64 = 200.0;
const MAX_RIGHT: f64 = 40.0;

/// Where a piece of text went: the element holding it and its rank there.
#[derive(Clone, Copy, Debug)]
pub(super) struct Home {
    pub(super) elem: usize,
    pub(super) rank: Rank,
    /// The element is a heading (a field labelled by it goes after it, not inside).
    pub(super) heading: bool,
}

/// A tagged widget, waiting for its element's object number.
#[derive(Clone, Debug)]
pub(super) struct Pending {
    pub(super) page: usize,
    pub(super) widget: ObjRef,
    pub(super) field: ObjRef,
    pub(super) elem: usize,
    pub(super) name: String,
    pub(super) kind: &'static str,
    pub(super) rect: [f64; 4],
    pub(super) alt: Option<String>,
    pub(super) alt_source: Option<&'static str>,
    pub(super) label: Option<String>,
}

/// What the fields of the pages tagged so far came to.
#[derive(Debug, Default)]
pub(super) struct FieldState {
    widgets: HashSet<ObjRef>,
    /// Each field seen: the alternate text its widgets start from, and where it came from.
    fields: HashMap<ObjRef, (Option<String>, Option<&'static str>)>,
    pub(super) pending: Vec<Pending>,
    pub(super) needing: Vec<AutotagFieldTooltip>,
    /// Tooltips to write (`fill`): field, text.
    pub(super) fills: Vec<(ObjRef, String)>,
    /// Widgets left untagged: over the limit.
    pub(super) over_limit: usize,
    /// Pages where labels were not looked for (too many widgets and text pieces).
    pub(super) unlabelled_pages: Vec<usize>,
}

/// A widget's field: the nearest node with a name, going up from the widget.
struct Info {
    field: ObjRef,
    name: String,
    partial: Option<String>,
    tooltip: Option<String>,
    kind: &'static str,
    /// A checkbox's or radio button's on state.
    state: Option<String>,
}

fn info(doc: &Document, widget: ObjRef, wd: &Dict) -> Info {
    let mut chain: Vec<(ObjRef, Dict)> = vec![(widget, wd.clone())];
    let mut seen: HashSet<ObjRef> = HashSet::from([widget]);
    let mut next = wd.get(b"Parent").and_then(Object::as_ref);
    while let Some(r) = next {
        if chain.len() >= MAX_FIELD_DEPTH || !seen.insert(r) {
            break;
        }
        let Some(d) = doc.get(r).as_dict().cloned() else { break };
        next = d.get(b"Parent").and_then(Object::as_ref);
        chain.push((r, d));
    }
    let at = chain.iter().position(|(_, d)| d.contains(b"T")).unwrap_or(0);
    let (field, fd) = chain.get(at).cloned().unwrap_or((widget, wd.clone()));
    let mut parts: Vec<String> = chain.iter().skip(at).filter_map(|(_, d)| text_of(doc, d, b"T")).collect();
    parts.reverse();
    let inherited = |key: &[u8]| chain.iter().find_map(|(_, d)| d.get(key).map(|o| doc.resolve(o)));
    let ft = inherited(b"FT").and_then(|o| o.as_name().map(<[u8]>::to_vec));
    let ff = inherited(b"Ff").and_then(|o| o.as_int()).unwrap_or(0);
    let kind = match ft.as_deref() {
        Some(b"Tx") => "text",
        Some(b"Btn") if ff & (1 << 16) != 0 => "button",
        Some(b"Btn") if ff & (1 << 15) != 0 => "radio",
        Some(b"Btn") => "checkbox",
        Some(b"Ch") if ff & (1 << 17) != 0 => "combo",
        Some(b"Ch") => "list",
        Some(b"Sig") => "signature",
        _ => "unknown",
    };
    let state = if matches!(kind, "radio" | "checkbox") {
        let ap = wd.get(b"AP").map(|o| doc.resolve(o));
        let n = ap.as_ref().and_then(|a| a.as_dict()).and_then(|a| a.get(b"N")).map(|o| doc.resolve(o));
        n.as_ref()
            .and_then(|n| n.as_dict())
            .and_then(|n| n.iter().map(|(k, _)| String::from_utf8_lossy(k)).find(|k| k != "Off"))
            .and_then(|s| clean(&s))
    } else {
        None
    };
    Info { field, name: parts.join("."), partial: text_of(doc, &fd, b"T"), tooltip: text_of(doc, &fd, b"TU"), kind, state }
}

/// A label as text: single spaces, without the underscores of a drawn blank or a trailing
/// colon or asterisk; `None` when nothing readable is left.
fn clean(t: &str) -> Option<String> {
    let mut s = String::new();
    for w in t.split_whitespace() {
        if s.chars().count() >= MAX_LABEL_CHARS {
            break;
        }
        if !s.is_empty() {
            s.push(' ');
        }
        s.extend(w.chars().take(MAX_LABEL_CHARS));
    }
    let s: String = s.chars().take(MAX_LABEL_CHARS).collect();
    let s = s.trim_matches(|c: char| c == '_' || c.is_whitespace()).trim_end_matches(|c: char| c == ':' || c == '*' || c == '_' || c.is_whitespace());
    s.chars().any(char::is_alphanumeric).then(|| s.to_owned())
}

/// A field name as words: `first_name` → `First name`.
fn humanize(t: &str) -> Option<String> {
    let spaced: String = t.chars().map(|c| if matches!(c, '_' | '-' | '.') { ' ' } else { c }).collect();
    let s = clean(&spaced)?;
    let mut chars = s.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Side {
    Left,
    Right,
    Above,
}

/// The nearest text on each side of a widget: (piece, distance).
#[derive(Default)]
struct Near {
    left: Option<(usize, f64)>,
    right: Option<(usize, f64)>,
    above: Option<(usize, f64)>,
}

fn near(l: &Layout, home: &[Option<Home>], w: &[f64; 4]) -> Near {
    let mut n = Near::default();
    let better = |slot: &mut Option<(usize, f64)>, i: usize, d: f64| {
        if slot.is_none_or(|(_, b)| d < b) {
            *slot = Some((i, d));
        }
    };
    let wh = (w[3] - w[1]).max(0.1);
    for (i, p) in l.pieces.iter().enumerate() {
        if home.get(i).copied().flatten().is_none() {
            continue;
        }
        let r = &p.rect;
        let ph = (r[3] - r[1]).max(0.1);
        let v_overlap = r[3].min(w[3]) - r[1].max(w[1]);
        if v_overlap > 0.5 * ph.min(wh) {
            if r[2] <= w[0] + 2.0 {
                let d = w[0] - r[2];
                if d <= MAX_LEFT {
                    better(&mut n.left, i, d);
                }
            } else if r[0] >= w[2] - 2.0 {
                let d = r[0] - w[2];
                if d <= MAX_RIGHT {
                    better(&mut n.right, i, d);
                }
            }
            continue;
        }
        if r[3] <= w[1] + 2.0 && r[0] < w[2] && r[2] > w[0] - 4.0 {
            let d = w[1] - r[3];
            if d <= 2.5 * p.size + 6.0 {
                better(&mut n.above, i, d.max(0.0) + (r[0] - w[0]).abs() * 0.01);
            }
        }
    }
    n
}

/// Where a widget's element goes.
#[derive(Clone, Copy, Debug)]
enum Place {
    /// Inside this element, at this rank.
    In(usize, Rank),
    /// After the page's blocks (no position to go by).
    End(usize),
    /// Ordered with the blocks by position.
    Own(usize),
}

fn label_text(l: &Layout, piece: Option<(usize, f64)>) -> Option<String> {
    piece.and_then(|(i, _)| l.pieces.get(i)).and_then(|p| clean(&p.text))
}

/// Tag one page's widgets (in the order the page lists them).
#[allow(clippy::too_many_arguments)]
pub(super) fn tag_widgets(
    doc: &Document,
    l: &Layout,
    widgets: Vec<(ObjRef, Dict)>,
    home: &[Option<Home>],
    tree: &mut Tree,
    tooltips: FieldTooltips,
    state: &mut FieldState,
    annots: &mut Vec<Annot>,
    untag: &mut Vec<ObjRef>,
) {
    let page = l.page;
    let mut list: Vec<(ObjRef, Dict, Option<[f64; 4]>)> = Vec::new();
    for (r, d) in widgets {
        if state.widgets.contains(&r) {
            continue;
        }
        if state.widgets.len() >= MAX_FIELDS {
            state.over_limit = state.over_limit.saturating_add(1);
            if d.contains(b"StructParent") {
                untag.push(r);
            }
            continue;
        }
        state.widgets.insert(r);
        let rect: Vec<f64> = d
            .get(b"Rect")
            .map(|o| doc.resolve(o))
            .and_then(|o| o.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
            .unwrap_or_default();
        let view = match rect[..] {
            [a, b, c, e] => finite(l.view.bbox(norm([a, b, c, e]))),
            _ => None,
        };
        list.push((r, d, view));
    }
    // Top to bottom, left to right: a radio group's first button is its first in reading.
    list.sort_by(|a, b| match (a.2, b.2) {
        (Some(x), Some(y)) => x[1].total_cmp(&y[1]).then(x[0].total_cmp(&y[0])),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let search = list.len().saturating_mul(l.pieces.len()) <= MAX_LABEL_WORK;
    if !search {
        state.unlabelled_pages.push(page);
    }
    let mut placed: Vec<(Place, usize)> = Vec::with_capacity(list.len());
    let mut own: Vec<(usize, [f64; 4])> = Vec::new();
    for (k, (r, d, view)) in list.iter().enumerate() {
        let inf = info(doc, *r, d);
        let n = match view {
            Some(v) if search => near(l, home, v),
            _ => Near::default(),
        };
        let check = matches!(inf.kind, "radio" | "checkbox");
        // The label that places it, and the caption that names its field.
        let (side, at) = match (n.right, n.left, n.above) {
            (Some(x), _, _) if check => (Some(Side::Right), Some(x)),
            (_, Some(x), _) => (Some(Side::Left), Some(x)),
            (_, _, Some(x)) => (Some(Side::Above), Some(x)),
            _ => (None, None),
        };
        let label = label_text(l, at);
        let caption = match inf.kind {
            "radio" => label_text(l, n.left.or(n.above)),
            _ => label.clone(),
        };
        let (base, source) = state.fields.get(&inf.field).cloned().unwrap_or_else(|| {
            let suggested = if inf.tooltip.is_some() { None } else { caption.clone().or_else(|| inf.partial.as_deref().and_then(humanize)) };
            let fill = tooltips == FieldTooltips::Fill && suggested.is_some();
            if inf.tooltip.is_none() {
                state.needing.push(AutotagFieldTooltip {
                    page,
                    field: inf.field,
                    name: inf.name.clone(),
                    suggested: suggested.clone(),
                    filled: fill,
                });
                if fill && let Some(s) = &suggested {
                    state.fills.push((inf.field, s.clone()));
                }
            }
            let picked = match (&inf.tooltip, fill, &inf.partial, &caption) {
                (Some(t), _, _, _) => (Some(t.clone()), Some("tooltip")),
                (None, true, _, _) => (suggested, Some("suggested")),
                (None, false, Some(t), _) => (Some(t.clone()), Some("name")),
                (None, false, None, Some(c)) => (Some(c.clone()), Some("label")),
                _ => (None, None),
            };
            state.fields.insert(inf.field, picked.clone());
            picked
        });
        let alt = if inf.kind == "radio" {
            let option = if side == Some(Side::Right) { label.clone() } else { None }.or_else(|| inf.state.clone());
            match (base, option) {
                (Some(b), Some(o)) if b != o => Some(format!("{b}: {o}")),
                (b, o) => b.or(o),
            }
        } else {
            base
        };
        let place = match (side, at.and_then(|(i, _)| home.get(i).copied().flatten())) {
            (Some(Side::Left), Some(h)) => Place::In(h.elem, (h.rank.0, h.rank.1, h.rank.2, h.rank.3.saturating_add(1))),
            (Some(Side::Right), Some(h)) => Place::In(h.elem, h.rank),
            (Some(Side::Above), Some(h)) if h.heading => Place::In(0, (page, h.rank.1, 1, 0)),
            (Some(Side::Above), Some(h)) => Place::In(h.elem, (h.rank.0, h.rank.1, h.rank.2, usize::MAX)),
            _ => match view {
                Some(v) => {
                    own.push((k, *v));
                    Place::Own(k)
                }
                None => Place::End(k),
            },
        };
        let e = tree.elems.len();
        placed.push((place, e));
        let (rank, parent) = match place {
            Place::In(c, rank) => (rank, c),
            Place::End(k) | Place::Own(k) => ((page, l.order.len(), 1, k), 0),
        };
        tree.add("Form", parent, page, rank);
        if let Some(el) = tree.elems.get_mut(e) {
            el.kids.push((rank, Kid::Objr { obj: *r, page }));
            el.alt.clone_from(&alt);
        }
        annots.push(Annot { obj: *r, elem: e, contents: None });
        state.pending.push(Pending {
            page,
            widget: *r,
            field: inf.field,
            elem: e,
            name: inf.name,
            kind: inf.kind,
            rect: view.unwrap_or_default(),
            alt,
            alt_source: source,
            label,
        });
    }
    // Widgets with no label: their own place among the blocks.
    if own.is_empty() || own.len() > MAX_ORDERED {
        return;
    }
    let nb = l.blocks.len();
    let mut rects: Vec<[f64; 4]> = l.blocks.iter().map(|b| b.rect).collect();
    rects.extend(own.iter().map(|(_, r)| *r));
    let mut items = l.order.clone();
    items.extend((0..own.len()).map(|j| nb.saturating_add(j)));
    let pos_of: HashMap<usize, usize> = l.order.iter().enumerate().map(|(p, b)| (*b, p)).collect();
    let mut prev: Option<usize> = None;
    let mut front = 0usize;
    let by_k: HashMap<usize, usize> = placed.iter().filter_map(|(p, e)| matches!(p, Place::Own(_)).then_some(*e)).enumerate().collect();
    for (j, item) in reading_order(&rects, items, 0).into_iter().enumerate() {
        if item < nb {
            prev = pos_of.get(&item).copied().or(prev);
            continue;
        }
        let Some(&e) = item.checked_sub(nb).and_then(|w| by_k.get(&w)) else { continue };
        let rank = match prev {
            Some(p) => (page, p, 2, j),
            None => (page, 0, 0, 0),
        };
        if let Some(el) = tree.elems.get_mut(e) {
            for (r, _) in &mut el.kids {
                *r = rank;
            }
        }
        // The element's own entry among the top-level kids.
        if let Some(root) = tree.elems.get_mut(0)
            && let Some(at) = root.kids.iter().rposition(|(_, k)| matches!(k, Kid::Elem(x) if *x == e))
        {
            let mut kid = root.kids.remove(at);
            kid.0 = rank;
            // Before the page's first block: ahead of it among equal ranks.
            if prev.is_none() {
                root.kids.insert(front.min(root.kids.len()), kid);
                front = front.saturating_add(1);
            } else {
                root.kids.push(kid);
            }
        }
    }
}

/// A label as it would be used (for tests).
#[cfg(test)]
pub(super) fn clean_label(t: &str) -> Option<String> {
    clean(t)
}

#[cfg(test)]
pub(super) fn humanize_name(t: &str) -> Option<String> {
    humanize(t)
}
