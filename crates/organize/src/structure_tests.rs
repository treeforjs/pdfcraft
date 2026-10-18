//! Structure-tree merging on synthetic tagged documents.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, write_full};

use crate::{
    FieldConflicts, ImportOptions, combine_with_options, delete_pages, duplicate_pages, extract_pages_with, import_pages_with_options, pages,
    replace_pages,
};

fn build(bodies: &[String]) -> Document {
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
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

fn stream(dict: &str, data: &str) -> String {
    format!("<< {dict} /Length {} >>\nstream\n{data}\nendstream", data.len())
}

/// A two-page tagged document. Page 1: a heading (custom type `Custom`, ID h1), a paragraph
/// (class Bold) whose content is on the page and in a form XObject, and a link (OBJR). Page 2: a
/// paragraph (ID p2) in the PDF 2.0 namespace. `custom` is what Custom maps to; `color` the
/// Bold class's colour; `lang` the document language.
fn tagged(custom: &str, color: &str, lang: &str) -> Document {
    let c1 = "/Custom << /MCID 0 >> BDC BT /F1 12 Tf 20 180 Td (Title) Tj ET EMC \
              /P << /MCID 1 >> BDC BT /F1 12 Tf 20 150 Td (Body) Tj ET EMC /Fm Do";
    let c2 = "/P << /MCID 0 >> BDC BT /F1 12 Tf 20 180 Td (Second) Tj ET EMC";
    let fm = "/P << /MCID 0 >> BDC BT /F1 10 Tf 20 100 Td (Inside) Tj ET EMC";
    let b: Vec<String> = vec![
        format!("<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 3 0 R /Lang ({lang}) >>"), // 1
        "<< /Type /Pages /Kids [4 0 R 5 0 R] /Count 2 /MediaBox [0 0 200 200] >>".into(),                               // 2
        format!(
            "<< /Type /StructTreeRoot /K 6 0 R /ParentTree 7 0 R /ParentTreeNextKey 4 /RoleMap << /Custom /{custom} >> \
             /ClassMap << /Bold << /O /Layout /Color [{color}] >> >> /IDTree 8 0 R /Namespaces [18 0 R] >>"
        ), // 3
        "<< /Type /Page /Parent 2 0 R /Contents 9 0 R /StructParents 0 /Annots [12 0 R] \
         /Resources << /Font << /F1 10 0 R >> /XObject << /Fm 11 0 R >> >> >>"
            .into(), // 4
        "<< /Type /Page /Parent 2 0 R /Contents 13 0 R /StructParents 1 /Resources << /Font << /F1 10 0 R >> >> >>".into(), // 5
        "<< /Type /StructElem /S /Document /P 3 0 R /K [14 0 R 15 0 R 17 0 R 16 0 R] >>".into(),                        // 6
        "<< /Nums [0 [14 0 R 15 0 R] 1 [16 0 R] 2 17 0 R 3 [15 0 R]] >>".into(),                                        // 7
        "<< /Names [(h1) 14 0 R (p2) 16 0 R] >>".into(),                                                                // 8
        stream("", c1),                                                                                                 // 9
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),                                                // 10
        stream("/Type /XObject /Subtype /Form /BBox [0 0 200 200] /StructParents 3 /Resources << /Font << /F1 10 0 R >> >>", fm), // 11
        "<< /Type /Annot /Subtype /Link /Rect [20 20 80 40] /Contents (Site) /StructParent 2 /A << /S /URI /URI (https://example.com) >> >>".into(), // 12
        stream("", c2),                                                                                                 // 13
        "<< /Type /StructElem /S /Custom /P 6 0 R /Pg 4 0 R /K 0 /ID (h1) >>".into(),                                   // 14
        "<< /Type /StructElem /S /P /P 6 0 R /Pg 4 0 R /C /Bold /K [1 << /Type /MCR /Stm 11 0 R /MCID 0 >>] >>".into(), // 15
        "<< /Type /StructElem /S /P /P 6 0 R /Pg 5 0 R /K 0 /ID (p2) /NS 18 0 R >>".into(),                             // 16
        "<< /Type /StructElem /S /Link /P 6 0 R /Pg 4 0 R /K << /Type /OBJR /Obj 12 0 R >> /Alt (Site) >>".into(),      // 17
        "<< /Type /Namespace /NS (http://iso.org/pdf2/ssn) >>".into(),                                                  // 18
    ];
    build(&b)
}

/// An untagged one-page document.
fn untagged() -> Document {
    let b: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << >> >>".into(),
        stream("", "0 0 10 10 re f"),
    ];
    build(&b)
}

fn catalog(doc: &Document) -> Dict {
    doc.get(doc.root().unwrap()).as_dict().cloned().unwrap()
}

fn dict(doc: &Document, o: &Object) -> Dict {
    doc.resolve(o).as_dict().cloned().unwrap()
}

/// What a structure tree holds, after checking that it is consistent: every element's /P is its
/// parent, every marked-content kid is found through the page's (or stream's) parent-tree entry,
/// every OBJR through the object's /StructParent, and /ParentTreeNextKey is above every key.
#[derive(Debug, Default)]
struct Facts {
    /// (depth, /S, /ID) of every element in document order (depth 0: the Document element).
    elements: Vec<(usize, String, Option<String>)>,
    /// (page index, mcid) per element, in order.
    content: Vec<(usize, i64)>,
    marked: bool,
}

fn number_tree(doc: &Document, node: &Object, out: &mut HashMap<i64, Object>) {
    let d = dict(doc, node);
    if let Some(n) = d.get(b"Nums").map(|n| doc.resolve(n)).and_then(|n| n.as_array().cloned()) {
        for p in n.chunks(2) {
            out.insert(p[0].as_int().unwrap(), p[1].clone());
        }
    }
    for k in d.get(b"Kids").and_then(|k| doc.resolve(k).as_array().cloned()).unwrap_or_default() {
        number_tree(doc, &k, out);
    }
}

fn check(doc: &Document) -> Facts {
    let cat = catalog(doc);
    let mut f = Facts { marked: cat.get(b"MarkInfo").is_some_and(|m| dict(doc, m).get(b"Marked") == Some(&Object::Bool(true))), ..Facts::default() };
    let root_ref = cat.reference(b"StructTreeRoot").expect("a structure tree");
    let root = doc.get(root_ref).as_dict().cloned().unwrap();
    let page_list: Vec<ObjRef> = pages(doc).unwrap().into_iter().map(|p| p.obj).collect();
    let page_index = |r: ObjRef| page_list.iter().position(|p| *p == r).expect("Pg is a page of the document");
    let mut tree = HashMap::new();
    number_tree(doc, root.get(b"ParentTree").unwrap(), &mut tree);
    let next = root.int(b"ParentTreeNextKey").unwrap();
    assert!(tree.keys().all(|k| *k < next), "ParentTreeNextKey {next} is above every key");
    let entry =
        |owner: &Dict, key: &[u8]| -> Object { tree.get(&owner.int(key).expect("has a parent-tree key")).cloned().expect("the key has an entry") };
    let mut stack: Vec<(ObjRef, ObjRef, usize)> = match root.get(b"K") {
        Some(Object::Ref(r)) => vec![(*r, root_ref, 0)],
        other => panic!("one top-level element, not {other:?}"),
    };
    let mut seen = HashSet::new();
    while let Some((e, parent, depth)) = stack.pop() {
        assert!(seen.insert(e), "element {e:?} reached twice");
        let d = doc.get(e).as_dict().cloned().unwrap();
        assert_eq!(d.reference(b"P"), Some(parent), "element {e:?} /P");
        let id = d.get(b"ID").and_then(|i| i.as_string()).map(|s| s.to_text());
        f.elements.push((depth, String::from_utf8_lossy(d.name(b"S").unwrap()).into_owned(), id));
        let kids = match d.get(b"K").cloned() {
            Some(Object::Array(a)) => a,
            Some(o) => vec![o],
            None => vec![],
        };
        let mut sub = Vec::new();
        for k in kids {
            match k {
                Object::Ref(r) => sub.push((r, e, depth + 1)),
                Object::Int(mcid) => {
                    let pg = d.reference(b"Pg").expect("an element with MCIDs has /Pg");
                    let arr = entry(&doc.get(pg).as_dict().cloned().unwrap(), b"StructParents");
                    assert_eq!(dict_arr(doc, &arr).get(mcid as usize), Some(&Object::Ref(e)), "parent tree of page for MCID {mcid}");
                    f.content.push((page_index(pg), mcid));
                }
                Object::Dict(k) if k.name(b"Type") == Some(b"MCR") => {
                    let mcid = k.int(b"MCID").unwrap();
                    let pg = k.reference(b"Pg").or(d.reference(b"Pg")).unwrap();
                    let owner = match k.reference(b"Stm") {
                        Some(s) => doc.get(s).as_dict().cloned().unwrap(),
                        None => doc.get(pg).as_dict().cloned().unwrap(),
                    };
                    let arr = entry(&owner, b"StructParents");
                    assert_eq!(dict_arr(doc, &arr).get(mcid as usize), Some(&Object::Ref(e)));
                    f.content.push((page_index(pg), mcid));
                }
                Object::Dict(k) if k.name(b"Type") == Some(b"OBJR") => {
                    let obj = doc.get(k.reference(b"Obj").unwrap()).as_dict().cloned().unwrap();
                    assert_eq!(entry(&obj, b"StructParent"), Object::Ref(e));
                }
                other => panic!("unexpected kid {other:?}"),
            }
        }
        stack.extend(sub.into_iter().rev());
    }
    // Every ID in the IDTree is unique and names its element.
    if let Some(t) = root.get(b"IDTree") {
        let names = dict(doc, t).get(b"Names").and_then(|n| doc.resolve(n).as_array().cloned()).unwrap();
        let mut ids = HashSet::new();
        for p in names.chunks(2) {
            let id = p[0].as_string().unwrap().to_text();
            assert!(ids.insert(id.clone()), "ID {id} is unique");
            assert_eq!(dict(doc, &p[1]).get(b"ID").and_then(|i| i.as_string()).map(|s| s.to_text()), Some(id));
        }
    }
    f
}

fn dict_arr(doc: &Document, o: &Object) -> Vec<Object> {
    doc.resolve(o).as_array().cloned().unwrap()
}

fn roundtrip(doc: &Document) -> Document {
    Document::open(Arc::new(write_full(doc, &SaveOptions::default()).unwrap())).unwrap()
}

fn types(f: &Facts) -> Vec<(usize, &str)> {
    f.elements.iter().map(|(d, s, _)| (*d, s.as_str())).collect()
}

fn root_dict(doc: &Document) -> Dict {
    dict(doc, catalog(doc).get(b"StructTreeRoot").unwrap())
}

#[test]
fn combining_two_tagged_documents_merges_their_trees() {
    let (a, b) = (tagged("H1", "1 0 0", "en-US"), tagged("P", "0 0 1", "de-DE"));
    let (out, report) =
        combine_with_options(&[("a", &a, None), ("b", &b, None)], &ImportOptions { fields: FieldConflicts::Rename, merge_tags: true }).unwrap();
    let out = roundtrip(&out);
    let f = check(&out);
    // One Document element; each source's Document dissolved into it, in page order.
    assert_eq!(types(&f), [(0, "Document"), (1, "Custom"), (1, "P"), (1, "Link"), (1, "P"), (1, "Custom"), (1, "P"), (1, "Link"), (1, "P")]);
    assert_eq!(f.content, [(0, 0), (0, 1), (0, 0), (1, 0), (2, 0), (2, 1), (2, 0), (3, 0)]);
    assert!(f.marked && report.tags.marked && !report.tags.partially_tagged);
    assert_eq!((report.tags.tagged_sources, report.tags.untagged_sources, report.tags.elements), (2, 0, 8));
    // Role map: the first mapping wins, the conflict is reported.
    let root = root_dict(&out);
    assert_eq!(dict(&out, root.get(b"RoleMap").unwrap()).name(b"Custom"), Some(&b"H1"[..]));
    assert_eq!(report.tags.role_map_conflicts.len(), 1);
    let c = &report.tags.role_map_conflicts[0];
    assert_eq!((c.role.as_str(), c.kept.as_str(), c.ignored.as_str(), c.source.as_str()), ("Custom", "H1", "P", "b"));
    assert!(report.tags.warnings.iter().any(|w| w.contains("/Custom")));
    // IDs stay unique; the second file's are renamed.
    let ids: Vec<Option<&str>> = f.elements.iter().map(|(_, _, id)| id.as_deref()).collect();
    assert_eq!(ids, [None, Some("h1"), None, None, Some("p2"), Some("h1-2"), None, None, Some("p2-2")]);
    assert_eq!(report.tags.ids_renamed, [("h1".to_owned(), "h1-2".to_owned()), ("p2".to_owned(), "p2-2".to_owned())]);
    // A different class under the same name is renamed, and the element follows.
    let classes = dict(&out, root.get(b"ClassMap").unwrap());
    assert!(classes.contains(b"Bold") && classes.contains(b"Bold_2"));
    assert_eq!(report.tags.classes_renamed, [("Bold".to_owned(), "Bold_2".to_owned())]);
    // The shared PDF 2.0 namespace is listed once.
    assert_eq!(dict_arr(&out, root.get(b"Namespaces").unwrap()).len(), 1);
    assert_eq!(report.tags.namespaces_added, 1);
    // The catalog keeps the first language; the second file's elements carry theirs.
    let lang = |d: &Dict| d.get(b"Lang").and_then(|l| l.as_string()).map(|s| s.to_text());
    assert_eq!(lang(&catalog(&out)).as_deref(), Some("en-US"));
    let doc_elem = dict(&out, root.get(b"K").unwrap());
    let kids = dict_arr(&out, doc_elem.get(b"K").unwrap());
    assert_eq!(lang(&dict(&out, &kids[0])), None);
    assert_eq!(lang(&dict(&out, &kids[4])).as_deref(), Some("de-DE"));
}

#[test]
fn identical_class_maps_and_namespaces_are_shared() {
    let a = tagged("H1", "1 0 0", "en-US");
    let (out, report) = combine_with_options(&[("a", &a, None), ("again", &a, None)], &ImportOptions::default()).unwrap();
    check(&out);
    assert!(report.tags.classes_renamed.is_empty() && report.tags.role_map_conflicts.is_empty());
    let root = root_dict(&out);
    assert_eq!(dict(&out, root.get(b"ClassMap").unwrap()).len(), 1);
    assert_eq!(dict_arr(&out, root.get(b"Namespaces").unwrap()).len(), 1);
}

#[test]
fn tagged_with_untagged_is_partially_tagged() {
    let a = tagged("H1", "1 0 0", "en-US");
    let u = untagged();
    let (out, report) = combine_with_options(&[("plain", &u, None), ("a", &a, None)], &ImportOptions::default()).unwrap();
    let f = check(&roundtrip(&out));
    assert_eq!(f.elements.len(), 5);
    assert_eq!(f.content.first(), Some(&(1, 0)), "the tagged file's pages come second");
    assert!(!f.marked);
    let t = &report.tags;
    assert!(t.partially_tagged && !t.marked && t.tree);
    assert_eq!((t.tagged_sources, t.untagged_sources), (1, 1));
    assert!(t.warnings.iter().any(|w| w.contains("partially tagged") && w.contains("plain")), "{:?}", t.warnings);
    // Untagged only: no tree at all.
    let (out, report) = combine_with_options(&[("x", &u, None), ("y", &u, None)], &ImportOptions::default()).unwrap();
    assert!(!catalog(&out).contains(b"StructTreeRoot") && !catalog(&out).contains(b"MarkInfo"));
    assert!(!report.tags.tree && !report.tags.partially_tagged);
}

#[test]
fn merging_can_be_turned_off() {
    let a = tagged("H1", "1 0 0", "en-US");
    let (out, report) =
        combine_with_options(&[("a", &a, None), ("b", &a, None)], &ImportOptions { merge_tags: false, ..ImportOptions::default() }).unwrap();
    assert!(!catalog(&out).contains(b"StructTreeRoot"));
    assert_eq!(report.tags, crate::TagMergeReport::default());
    for p in pages(&out).unwrap() {
        assert!(!out.get(p.obj).as_dict().unwrap().contains(b"StructParents"));
    }
    // The form XObject's stale key went too.
    let fm = out
        .object_numbers()
        .into_iter()
        .map(|n| out.get(ObjRef::new(n, 0)))
        .find(|o| o.as_dict().is_some_and(|d| d.name(b"Subtype") == Some(b"Form")));
    assert!(!fm.unwrap().as_dict().unwrap().contains(b"StructParents"));
}

#[test]
fn extracting_keeps_only_the_elements_on_the_kept_pages() {
    let a = tagged("H1", "1 0 0", "en-US");
    let (out, report) = extract_pages_with(&a, &[1], true).unwrap();
    let f = check(&roundtrip(&out));
    assert_eq!(types(&f), [(0, "Document"), (1, "P")]);
    assert_eq!(f.elements[1].2.as_deref(), Some("p2"));
    assert_eq!(f.content, [(0, 0)]);
    assert!(f.marked);
    assert_eq!((report.elements, report.elements_dropped), (1, 3));
    let ids = dict(&out, root_dict(&out).get(b"IDTree").unwrap());
    assert_eq!(dict_arr(&out, ids.get(b"Names").unwrap()).len(), 2, "only p2");

    let (out, report) = extract_pages_with(&a, &[0], true).unwrap();
    let f = check(&roundtrip(&out));
    assert_eq!(types(&f), [(0, "Document"), (1, "Custom"), (1, "P"), (1, "Link")]);
    assert_eq!(f.content, [(0, 0), (0, 1), (0, 0)], "the form XObject's content stays linked");
    assert_eq!(report.elements_dropped, 1);
    // Split parts are subsets too.
    let parts = crate::split(&a, &crate::SplitBy::PageCount(1)).unwrap();
    assert_eq!(check(&parts[1]).elements.len(), 2);
}

#[test]
fn inserting_places_the_elements_by_page_and_keeps_the_tree_valid() {
    let mut a = tagged("H1", "1 0 0", "en-US");
    let b = tagged("H1", "1 0 0", "en-US");
    let r = import_pages_with_options(&mut a, &b, &[1, 0], 1, &ImportOptions::default()).unwrap();
    let f = check(&roundtrip(&a));
    // a's page-1 elements, then b's (inserted as pages 2–3), then a's page-2 paragraph (now page 4).
    assert_eq!(f.content, [(0, 0), (0, 1), (0, 0), (2, 0), (2, 1), (2, 0), (1, 0), (3, 0)]);
    assert!(f.marked && r.tags.marked);
    assert_eq!((r.tags.tagged_sources, r.tags.elements), (2, 4));

    // Into an untagged document: the inserted pages bring their tags, the result is partial.
    let mut u = untagged();
    let r = import_pages_with_options(&mut u, &b, &[0], 1, &ImportOptions::default()).unwrap();
    let f = check(&u);
    assert_eq!(f.elements.len(), 4);
    assert!(!f.marked && r.tags.partially_tagged);
    // An untagged file into a tagged one: the tree stays, but it is no longer marked.
    let mut t = tagged("H1", "1 0 0", "en-US");
    let r = import_pages_with_options(&mut t, &untagged(), &[0], 0, &ImportOptions::default()).unwrap();
    let f = check(&t);
    assert_eq!(f.elements.len(), 5);
    assert!(!f.marked && r.tags.partially_tagged && r.tags.untagged_sources == 1);
}

#[test]
fn duplicate_and_replace_keep_a_valid_tree() {
    let mut a = tagged("H1", "1 0 0", "en-US");
    duplicate_pages(&mut a, &[1]).unwrap();
    let f = check(&roundtrip(&a));
    assert_eq!(f.content.last(), Some(&(2, 0)), "the copy of page 2 is tagged too");
    assert!(f.elements.iter().any(|e| e.2.as_deref() == Some("p2-2")));

    let mut a = tagged("H1", "1 0 0", "en-US");
    replace_pages(&mut a, &[1], &tagged("H1", "1 0 0", "en-US"), &[0]).unwrap();
    let f = check(&roundtrip(&a));
    assert_eq!(f.elements.len(), 5, "replacing pages adds no elements");
    delete_pages(&mut a, &[0]).unwrap();
}

#[test]
fn preview_reports_conflicts_without_changing_anything() {
    let a = tagged("H1", "1 0 0", "en-US");
    let b = tagged("P", "1 0 0", "en-US");
    let p = crate::tag_merge_preview(&a, &b);
    assert_eq!(p.role_map_conflicts.len(), 1);
    assert!(p.marked && !p.partially_tagged);
    let p = crate::tag_merge_preview(&a, &untagged());
    assert!(p.partially_tagged && !p.marked);
}

/// A tagged one-page document whose structure is `elems` (objects 6 onward) under root K
/// `root_k`, with `extra` root entries.
fn hostile(root_k: &str, extra: &str, elems: &[String]) -> Document {
    let mut b: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 3 0 R >>".into(),
        "<< /Type /Pages /Kids [4 0 R] /Count 1 /MediaBox [0 0 200 200] >>".into(),
        format!("<< /Type /StructTreeRoot /K {root_k} {extra} >>"),
        "<< /Type /Page /Parent 2 0 R /Contents 5 0 R /StructParents 0 /Resources << >> >>".into(),
        stream("", "/P << /MCID 0 >> BDC 0 0 10 10 re f EMC"),
    ];
    b.extend(elems.iter().cloned());
    build(&b)
}

#[test]
fn hostile_trees_merge_without_panicking() {
    let mut cases = Vec::new();
    // Cycles: an element whose kid is its parent, and one that is its own kid.
    cases.push(hostile(
        "6 0 R",
        "",
        &[
            "<< /Type /StructElem /S /Document /P 3 0 R /K [7 0 R 6 0 R] >>".into(),
            "<< /Type /StructElem /S /P /P 6 0 R /Pg 4 0 R /K [0 6 0 R 7 0 R] >>".into(),
        ],
    ));
    // Deep nesting: a chain of 3000 elements.
    let n = 3000;
    let deep: Vec<String> = (0..n)
        .map(|i| {
            let o = 6 + i;
            if i + 1 == n {
                format!("<< /Type /StructElem /S /Span /Pg 4 0 R /K 0 /P {} 0 R >>", o - 1)
            } else {
                format!("<< /Type /StructElem /S /Div /K {} 0 R /P 3 0 R >>", o + 1)
            }
        })
        .collect();
    cases.push(hostile("6 0 R", "", &deep));
    // Huge and negative MCIDs, kids pointing at pages, a cyclic parent tree, a role-map loop,
    // a cyclic ID tree, namespaces that refer to each other, a junk class map.
    cases.push(hostile(
        "[6 0 R 4 0 R 9 0 R]",
        "/ParentTree 7 0 R /ParentTreeNextKey 9223372036854775807 /RoleMap << /A /B /B /A /Document /A >> /IDTree 8 0 R \
         /Namespaces [10 0 R 11 0 R 12] /ClassMap 13",
        &[
            "<< /Type /StructElem /S /A /Pg 4 0 R /K [0 -5 999999999999 << /Type /MCR /MCID 1 /Stm 4 0 R >> << /Type /OBJR /Obj 2 0 R >>] /ID (x) /Ref [6 0 R 3 0 R] /NS 10 0 R /A << /O /Layout /Ref 4 0 R >> >>"
                .into(),
            "<< /Kids [7 0 R] /Nums [0 7 0 R] >>".into(),
            "<< /Kids [8 0 R 8 0 R] /Names [(x) 6 0 R] >>".into(),
            "<< /Type /StructElem /S /B /K 6 0 R /ID (x) >>".into(),
            "<< /Type /Namespace /NS (urn:a) /RoleMapNS << /A [/P 11 0 R] >> >>".into(),
            "<< /Type /Namespace /NS (urn:b) /RoleMapNS << /B [/P 10 0 R] >> >>".into(),
        ],
    ));
    // A root whose K is not an element at all.
    cases.push(hostile("(junk)", "/ParentTree 5", &[]));
    for (i, src) in cases.iter().enumerate() {
        let (out, report) =
            combine_with_options(&[("h", src, None), ("again", src, None)], &ImportOptions::default()).unwrap_or_else(|e| panic!("case {i}: {e}"));
        let out = roundtrip(&out);
        if catalog(&out).contains(b"StructTreeRoot") && root_dict(&out).get(b"K").is_some() {
            let f = check(&out);
            assert!(f.elements.len() <= 2 * n + 1, "case {i}");
        }
        let (part, _) = extract_pages_with(src, &[0], true).unwrap_or_else(|e| panic!("case {i}: {e}"));
        roundtrip(&part);
        let mut into = tagged("H1", "1 0 0", "en-US");
        import_pages_with_options(&mut into, src, &[0], 1, &ImportOptions::default()).unwrap_or_else(|e| panic!("case {i}: {e}"));
        check(&roundtrip(&into));
        if i == 1 {
            assert!(report.tags.truncated, "the deep chain is cut at the depth limit");
        }
    }
}
