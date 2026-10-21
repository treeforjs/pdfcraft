//! Keep list tags on edit: after an edit draws a bulleted or numbered list, give its marked
//! content list structure (`L` > `LI` > `Lbl` + `LBody`, a nested list inside the `LBody` of the
//! item it belongs to), as other viewers tag lists made in Edit PDF.
//!
//! Run after [`crate::keep_tags`], which gives every new label and paragraph its own MCID (in
//! a new `P`, or in the element of the content it replaced). The list's drawing wraps each label
//! and paragraph in `/PCList` marked content naming its role (`Lbl` or `LBody`), list, item and
//! level (`pdfcraft-edit`'s `lists`). Each list (a content stream's `/List` number) is then
//! rebuilt: its MCIDs leave the elements holding them, those elements (and list elements left
//! empty above them) go, and a new `L` takes the place of the first of them (or of the list it
//! was in). Every walk is bounded; content without an element is reported, never guessed.

use std::collections::{BTreeMap, HashMap, HashSet};

use pdfcraft_content::Pieces;
use pdfcraft_cos::{Dict, Document, ObjRef, Object};

use crate::keep::{AddedTag, MAX_CLIMB, MAX_OPS, TagUpdate, content_streams, insert_kid, kid_list, rebuild_parent_tree, root_ref, strip_mcids};
use crate::tags::{MAX_MCID, TagError, TagKid, tags};

/// Most marked-content sequences followed in one list.
const MAX_LIST_SEQS: usize = 100_000;
/// Marked-content nesting deeper than this is not followed.
const MAX_NEST: usize = 4096;
/// Element types that go when the list takes their content and leaves them empty.
const REPLACEABLE: &[&str] = &["P", "Span", "Lbl", "LBody", "LI", "L"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    Lbl,
    LBody,
}

/// One list marker: its role, list, item and level.
#[derive(Clone, Copy, Debug)]
struct Marker {
    role: Role,
    list: i64,
    item: i64,
    level: i64,
}

fn marker(props: &Dict) -> Option<Marker> {
    let role = match props.name(b"Role")? {
        b"Lbl" => Role::Lbl,
        b"LBody" => Role::LBody,
        _ => return None,
    };
    Some(Marker { role, list: props.int(b"List")?, item: props.int(b"Item")?, level: props.int(b"Level").unwrap_or(0).clamp(0, 64) })
}

/// One item of a list, in drawing order.
#[derive(Debug, Default)]
struct Item {
    level: i64,
    lbl: Vec<i64>,
    body: Vec<i64>,
}

/// A list's item numbers in drawing order, and its items.
type List = (Vec<i64>, HashMap<i64, Item>);

fn new_element(doc: &mut Document, role: &str, parent: ObjRef, page: ObjRef, mcids: &[i64]) -> ObjRef {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("StructElem"));
    d.set(b"S".to_vec(), Object::name(role));
    d.set(b"P".to_vec(), Object::Ref(parent));
    d.set(b"Pg".to_vec(), Object::Ref(page));
    match mcids {
        [] => {}
        [one] => d.set(b"K".to_vec(), Object::Int(*one)),
        many => d.set(b"K".to_vec(), Object::Array(many.iter().map(|m| Object::Int(*m)).collect())),
    }
    doc.add(Object::Dict(d))
}

fn push_kid(doc: &mut Document, holder: ObjRef, kid: ObjRef) -> Result<(), TagError> {
    let d = doc.get(holder).as_dict().cloned().ok_or_else(|| TagError::Invalid("a structure element is not a dictionary".into()))?;
    let mut list = kid_list(doc, &d);
    list.push(Object::Ref(kid));
    doc.update_dict(holder, |d| d.set(b"K".to_vec(), Object::Array(list)))?;
    Ok(())
}

fn parent_of(doc: &Document, e: ObjRef) -> Option<ObjRef> {
    doc.get(e).as_dict().and_then(|d| d.get(b"P").and_then(Object::as_ref))
}

/// Unlink `e`, which the list left empty, and the replaceable elements above it that it leaves
/// empty in turn. Returns how many went.
fn unlink_empty(
    doc: &mut Document,
    root: ObjRef,
    start: ObjRef,
    keep: &HashSet<ObjRef>,
    role: &dyn Fn(ObjRef) -> Option<String>,
) -> Result<usize, TagError> {
    let mut n = 0usize;
    let mut e = start;
    for _ in 0..MAX_CLIMB {
        if keep.contains(&e) || doc.get(e).as_dict().is_none_or(|d| !kid_list(doc, d).is_empty()) {
            break;
        }
        if !role(e).is_some_and(|r| REPLACEABLE.contains(&r.as_str())) {
            break;
        }
        let holder = parent_of(doc, e).unwrap_or(root);
        let Some(d) = doc.get(holder).as_dict().cloned() else { break };
        let kids = kid_list(doc, &d);
        let rest: Vec<Object> = kids.iter().filter(|k| k.as_ref() != Some(e)).cloned().collect();
        if rest.len() == kids.len() {
            break;
        }
        doc.update_dict(holder, |d| d.set(b"K".to_vec(), Object::Array(rest)))?;
        n = n.saturating_add(1);
        if holder == root {
            break;
        }
        e = holder;
    }
    Ok(n)
}

/// Give the lists drawn on page `page` (0-based) list structure; see the module docs. Changes
/// nothing and reports nothing for an untagged document or a page without list markers. On an
/// error the document is unchanged.
pub fn keep_list_tags(doc: &mut Document, page: usize) -> Result<TagUpdate, TagError> {
    let mut work = doc.clone();
    let report = keep_list_tags_inner(&mut work, page)?;
    if report.lists > 0 || !report.warnings.is_empty() {
        *doc = work;
    }
    Ok(report)
}

fn keep_list_tags_inner(doc: &mut Document, page: usize) -> Result<TagUpdate, TagError> {
    let mut report = TagUpdate::default();
    let Ok(root) = root_ref(doc) else { return Ok(report) };
    let Some(pg) = pdfcraft_model::pages(doc).into_iter().nth(page) else { return Ok(report) };
    let streams = content_streams(doc, &pg.dict);
    if !streams.iter().any(|(_, d)| d.windows(7).any(|w| w == b"/PCList")) {
        return Ok(report);
    }
    let pieces = Pieces::join(&streams.iter().map(|(_, d)| d.as_slice()).collect::<Vec<_>>());
    let ops = pieces.parse().ops;
    if ops.len() > MAX_OPS {
        report.warnings.push(format!("page {} has too many operators to compare; its list tags were not updated", page.saturating_add(1)));
        return Ok(report);
    }
    let props = crate::tags::sub_dict(doc, &crate::tags::sub_dict(doc, &pg.dict, b"Resources"), b"Properties");
    // (content stream, list) → items by number, in drawing order.
    let mut lists: BTreeMap<(usize, i64), List> = BTreeMap::new();
    let mut stack: Vec<Option<Marker>> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let mut followed = 0usize;
    for op in &ops {
        match op.op.as_slice() {
            b"BMC" => {
                if stack.len() < MAX_NEST {
                    stack.push(None);
                }
            }
            b"BDC" => {
                let p = match op.operands.get(1) {
                    Some(Object::Dict(d)) => Some(d.clone()),
                    Some(Object::Name(n)) => props.get(n).map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()),
                    _ => None,
                };
                let list_marker = (op.name(0) == Some(b"PCList")).then(|| p.as_ref().and_then(marker)).flatten();
                let mcid = p.as_ref().and_then(|d| d.get(b"MCID").and_then(Object::as_int)).filter(|m| (0..=MAX_MCID).contains(m));
                if let Some(m) = mcid
                    && let Some(mk) = stack.iter().rev().flatten().next().copied()
                    && seen.insert(m)
                    && followed < MAX_LIST_SEQS
                {
                    followed = followed.saturating_add(1);
                    let stream = pieces.piece_of(op.span.start);
                    let (order, items) = lists.entry((stream, mk.list)).or_default();
                    let item = items.entry(mk.item).or_insert_with(|| {
                        order.push(mk.item);
                        Item { level: mk.level, ..Item::default() }
                    });
                    match mk.role {
                        Role::Lbl => item.lbl.push(m),
                        Role::LBody => item.body.push(m),
                    }
                }
                if stack.len() < MAX_NEST {
                    stack.push(list_marker);
                }
            }
            b"EMC" => {
                stack.pop();
            }
            _ => {}
        }
    }
    if lists.is_empty() {
        return Ok(report);
    }

    let tree = tags(doc);
    let owner: HashMap<i64, ObjRef> = tree
        .nodes
        .iter()
        .filter_map(|n| n.obj.map(|o| (o, n)))
        .flat_map(|(o, n)| {
            n.kids.iter().filter_map(move |k| match k {
                TagKid::Content { page: Some(p), mcid, stream: None } if *p == page => Some((*mcid, o)),
                _ => None,
            })
        })
        .collect();
    let index: HashMap<ObjRef, usize> = tree.nodes.iter().enumerate().filter_map(|(i, n)| n.obj.map(|o| (o, i))).collect();
    let role_of = |e: ObjRef| index.get(&e).and_then(|i| tree.nodes.get(*i)).map(|n| n.role().to_owned());
    let tree_parent =
        |e: ObjRef| index.get(&e).and_then(|i| tree.nodes.get(*i)).and_then(|n| n.parent).and_then(|p| tree.nodes.get(p)).and_then(|n| n.obj);

    let mut made: HashSet<ObjRef> = HashSet::new();
    for ((_, _), (order, items)) in &lists {
        let mcids: Vec<i64> = order.iter().filter_map(|i| items.get(i)).flat_map(|it| it.lbl.iter().chain(&it.body).copied()).collect();
        let Some(first) = mcids.first().and_then(|m| owner.get(m)).copied() else {
            report.warnings.push(format!("a list on page {} has no tags to follow; it was left as it was", page.saturating_add(1)));
            continue;
        };
        if let Some(m) = mcids.iter().find(|m| !owner.contains_key(m)) {
            report
                .warnings
                .push(format!("marked content {m} of a list on page {} has no element; the list was left as it was", page.saturating_add(1)));
            continue;
        }
        // Where the list goes: in place of its first element, or of the outermost list that
        // element is in.
        let mut anchor = first;
        for _ in 0..MAX_CLIMB {
            match tree_parent(anchor) {
                Some(p)
                    if role_of(anchor).is_some_and(|r| matches!(r.as_str(), "Lbl" | "LBody" | "LI" | "L"))
                        && role_of(p).is_some_and(|r| matches!(r.as_str(), "LBody" | "LI" | "L")) =>
                {
                    anchor = p
                }
                _ => break,
            }
        }
        let parent = tree_parent(anchor);
        let holder = parent.unwrap_or(root);

        // The new structure, levels nesting inside the LBody of the item before.
        let top = new_element(doc, "L", holder, pg.obj, &[]);
        made.insert(top);
        let mut levels: Vec<(i64, ObjRef, Option<ObjRef>)> = vec![(order.first().and_then(|i| items.get(i)).map_or(0, |it| it.level), top, None)];
        for it in order.iter().filter_map(|i| items.get(i)) {
            while levels.len() > 1 && levels.last().is_some_and(|(l, _, _)| *l > it.level) {
                levels.pop();
            }
            if let Some(&(level, _, Some(body))) = levels.last()
                && it.level > level
                && levels.len() < 64
            {
                let nested = new_element(doc, "L", body, pg.obj, &[]);
                made.insert(nested);
                push_kid(doc, body, nested)?;
                levels.push((it.level, nested, None));
            }
            let Some(&(_, list, _)) = levels.last() else { break };
            let li = new_element(doc, "LI", list, pg.obj, &[]);
            push_kid(doc, list, li)?;
            made.insert(li);
            if !it.lbl.is_empty() {
                let lbl = new_element(doc, "Lbl", li, pg.obj, &it.lbl);
                push_kid(doc, li, lbl)?;
                made.insert(lbl);
                report.added.extend(it.lbl.iter().map(|m| AddedTag { page, element: lbl, role: "Lbl".into(), mcid: *m }));
            }
            let body = new_element(doc, "LBody", li, pg.obj, &it.body);
            push_kid(doc, li, body)?;
            made.insert(body);
            report.added.extend(it.body.iter().map(|m| AddedTag { page, element: body, role: "LBody".into(), mcid: *m }));
            if let Some(last) = levels.last_mut() {
                last.2 = Some(body);
            }
        }
        insert_kid(doc, root, parent, Some(anchor), false, top)?;

        // The MCIDs leave the elements that held them; elements left empty go.
        let gone: HashSet<i64> = mcids.iter().copied().collect();
        let holders: Vec<ObjRef> = {
            let mut v: Vec<ObjRef> = mcids.iter().filter_map(|m| owner.get(m).copied()).collect();
            let mut seen = HashSet::new();
            v.retain(|o| seen.insert(*o));
            v
        };
        for e in holders {
            let here = index.get(&e).and_then(|i| tree.nodes.get(*i)).and_then(|n| n.page) == Some(page);
            let (n, empty) = strip_mcids(doc, e, here, pg.obj, &gone)?;
            report.removed = report.removed.saturating_add(n);
            if empty {
                report.pruned = report.pruned.saturating_add(unlink_empty(doc, root, e, &made, &role_of)?);
            }
        }
        report.lists = report.lists.saturating_add(1);
    }
    if report.lists > 0 {
        rebuild_parent_tree(doc, root, page, pg.obj)?;
        report.parent_tree = 1;
        report.pages.push(page);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn doc(content: &str, tagged: bool) -> Document {
        let root = if tagged { " /StructTreeRoot 5 0 R" } else { "" };
        let bodies = [
            format!("<< /Type /Catalog /Pages 2 0 R{root} >>"),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /StructParents 0 >>".into(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /StructTreeRoot /K 6 0 R /ParentTree 7 0 R >>".into(),
            "<< /Type /StructElem /S /Document /P 5 0 R /K [8 0 R] >>".into(),
            "<< /Nums [0 [8 0 R 8 0 R]] >>".into(),
            "<< /Type /StructElem /S /P /P 6 0 R /Pg 3 0 R /K [0 1] >>".into(),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, b) in bodies.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{b}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).as_bytes());
        Document::open(Arc::new(out)).unwrap()
    }

    const LIST: &str = "BT /PCList << /Role /Lbl /List 0 /Item 0 /Level 0 >> BDC /P << /MCID 0 >> BDC (1.) Tj EMC EMC \
                        /PCList << /Role /LBody /List 0 /Item 0 /Level 0 >> BDC /P << /MCID 1 >> BDC (One) Tj EMC EMC ET";

    #[test]
    fn a_marked_list_becomes_list_structure() {
        let mut d = doc(LIST, true);
        let r = keep_list_tags(&mut d, 0).unwrap();
        assert_eq!((r.lists, r.added.len()), (1, 2), "{r:?}");
        let roles: Vec<String> = tags(&d).nodes.iter().map(|n| n.role().to_owned()).collect();
        assert_eq!(roles, ["Document", "L", "LI", "Lbl", "LBody"]);
    }

    #[test]
    fn untagged_or_hostile_markers_change_nothing() {
        let mut d = doc(LIST, false);
        assert!(keep_list_tags(&mut d, 0).unwrap().is_empty());
        for content in [
            "/PCList << /Role /Lbl >> BDC (x) Tj EMC",
            "/PCList << /Role /Nope /List 0 /Item 0 >> BDC /P << /MCID 0 >> BDC (x) Tj EMC EMC",
            "/PCList << /Role /Lbl /List 0 /Item 0 /Level -99999 >> BDC /P << /MCID 99 >> BDC (x) Tj EMC",
            "EMC EMC /PCList /Missing BDC /P << /MCID -4 >> BDC",
        ] {
            let mut d = doc(content, true);
            let r = keep_list_tags(&mut d, 0).unwrap();
            assert_eq!(r.lists, 0, "{content}: {r:?}");
        }
        assert!(keep_list_tags(&mut doc(LIST, true), 7).unwrap().is_empty(), "no such page");
    }
}
