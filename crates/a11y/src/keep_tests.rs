//! Keep tags on edit: synthetic tagged pages edited directly, then brought back in step.

use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use super::*;

fn pdf(bodies: &[String]) -> Document {
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

fn stream(data: &str) -> String {
    format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len())
}

const HEADING: &str = "/P << /MCID 0 >> BDC BT /F1 12 Tf 20 180 Td (Hello) Tj ET EMC";
const PARAGRAPH: &str = "BT /F1 12 Tf 20 150 Td /P << /MCID 1 >> BDC (Line one) Tj EMC 0 -14 Td /P << /MCID 2 >> BDC (Line two) Tj EMC ET";
const FIGURE: &str = "/Figure << /MCID 3 >> BDC q 10 0 0 10 20 100 cm /Im Do Q EMC";

/// Page 1: a P with ActualText (MCID 0), a two-line P (MCIDs 1 and 2) and a Figure with alt
/// (MCID 3), all in a Document element. Objects: 8 = first P, 9 = paragraph, 12 = Figure.
fn tagged(content: &str) -> Document {
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /StructParents 0 \
         /Resources << /Font << /F1 5 0 R >> /XObject << /Im 11 0 R /Im2 13 0 R >> >> >>"
            .into(),
        stream(content),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        "<< /Type /StructTreeRoot /K 7 0 R /ParentTree 10 0 R /ParentTreeNextKey 1 >>".into(),
        "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R 12 0 R] >>".into(),
        "<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K 0 /ActualText (Hello) >>".into(),
        "<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [1 2] >>".into(),
        "<< /Nums [0 [8 0 R 9 0 R 9 0 R 12 0 R]] >>".into(),
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x7f\nendstream"
            .into(),
        "<< /Type /StructElem /S /Figure /P 7 0 R /Pg 3 0 R /K 3 /Alt (A dot) >>".into(),
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x20\nendstream"
            .into(),
    ])
}

fn original() -> String {
    format!("{HEADING} {PARAGRAPH} {FIGURE}")
}

fn r(n: u32) -> ObjRef {
    ObjRef::new(n, 0)
}

/// Replace page 1's content, as an edit would.
fn set_content(doc: &mut Document, content: &str) {
    let s = doc.add(Object::Stream(Stream::flate(Dict::new(), content.as_bytes())));
    doc.update_dict(r(3), |d| d.set(b"Contents".to_vec(), Object::Ref(s))).unwrap();
}

fn page_content(doc: &Document) -> String {
    let page = doc.get(r(3));
    let c = page.as_dict().unwrap().get(b"Contents").unwrap().clone();
    let data = match &*doc.resolve(&c) {
        Object::Stream(s) => s.decoded().unwrap(),
        Object::Array(a) => a
            .iter()
            .flat_map(|x| match &*doc.resolve(x) {
                Object::Stream(s) => s.decoded().unwrap(),
                _ => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    };
    // Whitespace collapsed: operators are copied byte for byte, insertions add newlines.
    String::from_utf8_lossy(&data).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The page's parent-tree array, as object numbers (0 for null).
fn parent_tree(doc: &Document) -> Vec<u32> {
    let key = doc.get(r(3)).as_dict().unwrap().get(b"StructParents").and_then(Object::as_int).unwrap();
    let root = doc.get(r(6));
    let pt = root.as_dict().unwrap().get(b"ParentTree").and_then(Object::as_ref).unwrap();
    let nums = doc.get(pt).as_dict().unwrap().get(b"Nums").unwrap().as_array().unwrap().clone();
    let v = nums.chunks(2).find(|c| c[0].as_int() == Some(key)).unwrap()[1].clone();
    doc.resolve(&v).as_array().unwrap().iter().map(|o| o.as_ref().map_or(0, |r| r.num)).collect()
}

/// Every (page, MCID) → element in the tree, which must agree with the parent tree.
fn assert_consistent(doc: &Document) {
    let t = tags(doc);
    let pt = parent_tree(doc);
    let mut seen = 0;
    for n in &t.nodes {
        for k in &n.kids {
            if let TagKid::Content { page: Some(0), mcid, stream: None } = k {
                assert_eq!(pt.get(*mcid as usize).copied(), n.obj.map(|o| o.num), "MCID {mcid}");
                seen += 1;
            }
        }
    }
    assert_eq!(pt.iter().filter(|x| **x != 0).count(), seen, "parent tree {pt:?} has entries the tree doesn't");
    // Every MCID in the content is in the tree once.
    let content = page_content(doc);
    for (m, _) in content.match_indices("/MCID ") {
        let id: i64 = content[m + 6..].split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap();
        assert!(t.nodes.iter().any(|n| n.kids.contains(&TagKid::Content { page: Some(0), mcid: id, stream: None })), "MCID {id} is untagged");
    }
}

fn kids(doc: &Document, n: u32) -> Vec<Object> {
    match doc.get(r(n)).as_dict().unwrap().get(b"K").cloned() {
        Some(Object::Array(a)) => a,
        Some(o) => vec![o],
        None => Vec::new(),
    }
}

#[test]
fn an_untagged_document_has_no_snapshot() {
    let doc = pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".into(),
        stream("BT (x) Tj ET"),
    ]);
    assert!(snapshot(&doc, 0).is_none());
    assert!(pages_snapshot(&doc, &[0]).is_none());
}

#[test]
fn nothing_changes_when_the_marked_content_is_the_same() {
    let mut doc = tagged(&original());
    let snap = snapshot(&doc, 0).unwrap();
    set_content(&mut doc, &original());
    let u = keep_tags(&mut doc, &snap).unwrap();
    assert!(u.is_empty(), "{u:?}");
}

#[test]
fn text_changed_inside_its_marked_content_keeps_its_tag_and_loses_stale_actual_text() {
    let mut doc = tagged(&original());
    let snap = snapshot(&doc, 0).unwrap();
    set_content(&mut doc, &original().replace("(Hello)", "(Howdy)"));
    let u = keep_tags(&mut doc, &snap).unwrap();
    assert_eq!((u.kept, u.adopted, u.removed, u.actual_text_cleared), (1, 0, 0, 1), "{u:?}");
    assert_eq!(u.pages, [0]);
    assert!(!doc.get(r(8)).as_dict().unwrap().contains(b"ActualText"));
    assert_eq!(parent_tree(&doc), [8, 9, 9, 12]);
    assert_consistent(&doc);
}

#[test]
fn a_rewritten_paragraph_takes_over_the_tag_of_the_lines_it_replaced() {
    let mut doc = tagged(&original());
    let snap = snapshot(&doc, 0).unwrap();
    // As paragraph editing does: the new text is drawn at the BT, the old lines' marked content
    // is left empty.
    let rewritten =
        "q BT /F1 12 Tf 20 150 Td (The new paragraph) Tj ET Q BT /F1 12 Tf 20 150 Td /P << /MCID 1 >> BDC EMC 0 -14 Td /P << /MCID 2 >> BDC EMC ET";
    set_content(&mut doc, &format!("{HEADING} {rewritten} {FIGURE}"));
    let u = keep_tags(&mut doc, &snap).unwrap();
    assert_eq!((u.adopted, u.removed, u.pruned, u.added.len()), (1, 1, 0, 0), "{u:?}");
    assert_eq!(kids(&doc, 9), [Object::Int(1)]);
    let c = page_content(&doc);
    assert!(c.contains("/P <</MCID 1>> BDC (The new paragraph) Tj EMC"), "{c}");
    assert_eq!(c.matches("BDC").count(), 3, "the empty sequences are gone: {c}");
    assert_eq!(c.matches("EMC").count(), 3, "{c}");
    assert_eq!(parent_tree(&doc), [8, 9, 0, 12]);
    assert_consistent(&doc);
}

#[test]
fn added_text_and_images_get_new_elements_after_the_nearest_tagged_content() {
    let mut doc = tagged(&original());
    let snap = snapshot(&doc, 0).unwrap();
    let added = format!("{} q BT /F1 10 Tf 20 60 Td (Added) Tj 0 -12 Td (two lines) Tj ET Q q 5 0 0 5 20 20 cm /Im2 Do Q", original());
    set_content(&mut doc, &added);
    let u = keep_tags(&mut doc, &snap).unwrap();
    let roles: Vec<(&str, i64)> = u.added.iter().map(|a| (a.role.as_str(), a.mcid)).collect();
    assert_eq!(roles, [("P", 4), ("Figure", 5)], "{u:?}");
    let (p, f) = (u.added[0].element, u.added[1].element);
    assert_eq!(kids(&doc, 7), [Object::Ref(r(8)), Object::Ref(r(9)), Object::Ref(r(12)), Object::Ref(p), Object::Ref(f)]);
    let pd = doc.get(p);
    let pd = pd.as_dict().unwrap();
    assert_eq!(pd.get(b"P").and_then(Object::as_ref), Some(r(7)));
    assert_eq!(pd.get(b"Pg").and_then(Object::as_ref), Some(r(3)));
    let c = page_content(&doc);
    assert!(c.contains("/P <</MCID 4>> BDC (Added) Tj 0 -12 Td (two lines) Tj EMC"), "one sequence per text object: {c}");
    assert!(c.contains("/Figure <</MCID 5>> BDC /Im2 Do EMC"), "{c}");
    assert_eq!(parent_tree(&doc), [8, 9, 9, 12, p.num, f.num]);
    assert_consistent(&doc);
}

#[test]
fn new_content_before_all_tagged_content_goes_first() {
    let mut doc = tagged(&original());
    let snap = snapshot(&doc, 0).unwrap();
    set_content(&mut doc, &format!("BT /F1 10 Tf 20 190 Td (Top) Tj ET {}", original()));
    let u = keep_tags(&mut doc, &snap).unwrap();
    let p = u.added[0].element;
    assert_eq!(kids(&doc, 7).first(), Some(&Object::Ref(p)));
    assert_consistent(&doc);
}

#[test]
fn a_deleted_image_leaves_the_tree_and_its_empty_figure_goes() {
    let mut doc = tagged(&original());
    let snap = snapshot(&doc, 0).unwrap();
    set_content(&mut doc, &original().replace("/Im Do", ""));
    let u = keep_tags(&mut doc, &snap).unwrap();
    assert_eq!((u.removed, u.pruned), (1, 1), "{u:?}");
    assert_eq!(kids(&doc, 7), [Object::Ref(r(8)), Object::Ref(r(9))]);
    assert!(!page_content(&doc).contains("MCID 3"));
    assert_eq!(parent_tree(&doc), [8, 9, 9]);
    assert_consistent(&doc);
}

#[test]
fn a_replaced_image_keeps_its_figure_and_asks_for_its_alt_to_be_checked() {
    let mut doc = tagged(&original());
    let snap = snapshot(&doc, 0).unwrap();
    set_content(&mut doc, &original().replace("/Im Do", "/Im2 Do"));
    let u = keep_tags(&mut doc, &snap).unwrap();
    assert_eq!((u.kept, u.removed), (1, 0), "{u:?}");
    assert_eq!(u.review_alt, [r(12)]);
    assert!(doc.get(r(12)).as_dict().unwrap().contains(b"Alt"));
    assert_consistent(&doc);
}

#[test]
fn an_image_drawn_again_in_new_content_takes_over_the_figure() {
    // Added images are rewritten whole when they move: the old sequence goes with the stream.
    let mut doc = tagged(&original());
    let snap = snapshot(&doc, 0).unwrap();
    set_content(&mut doc, &format!("{HEADING} {PARAGRAPH} q 20 0 0 20 50 50 cm /Im Do Q"));
    let u = keep_tags(&mut doc, &snap).unwrap();
    assert_eq!((u.adopted, u.removed, u.pruned), (1, 0, 0), "{u:?}");
    assert_eq!(u.review_alt, [r(12)]);
    assert!(page_content(&doc).contains("/Figure <</MCID 3>> BDC /Im Do EMC"));
    assert_eq!(parent_tree(&doc), [8, 9, 9, 12]);
    assert_consistent(&doc);
}

#[test]
fn new_underlines_become_artifacts() {
    let mut doc = tagged(&original());
    let snap = snapshot(&doc, 0).unwrap();
    set_content(&mut doc, &format!("{} q 0 0 0 RG 20 148 m 80 148 l S Q", original()));
    let u = keep_tags(&mut doc, &snap).unwrap();
    assert_eq!((u.artifacts, u.added.len()), (1, 0), "{u:?}");
    assert!(page_content(&doc).contains("/Artifact BMC 20 148 m 80 148 l S EMC"));
}

#[test]
fn content_already_untagged_is_left_alone() {
    let base = format!("{} BT /F1 8 Tf 5 5 Td (stray) Tj ET", original());
    let mut doc = tagged(&base);
    let snap = snapshot(&doc, 0).unwrap();
    set_content(&mut doc, &base.replace("(Hello)", "(Hi)"));
    let u = keep_tags(&mut doc, &snap).unwrap();
    assert!(u.added.is_empty(), "{u:?}");
    assert!(page_content(&doc).contains("(stray) Tj"));
    assert!(!page_content(&doc).contains("MCID 4"));
}

#[test]
fn a_page_without_a_parent_tree_entry_gets_one() {
    let mut doc = tagged(&original());
    doc.update_dict(r(3), |d| {
        d.remove(b"StructParents");
    })
    .unwrap();
    let snap = snapshot(&doc, 0).unwrap();
    set_content(&mut doc, &original().replace("(Hello)", "(Hey)"));
    keep_tags(&mut doc, &snap).unwrap();
    assert_eq!(doc.get(r(3)).as_dict().unwrap().get(b"StructParents").and_then(Object::as_int), Some(1));
    assert_eq!(doc.get(r(6)).as_dict().unwrap().get(b"ParentTreeNextKey").and_then(Object::as_int), Some(2));
    assert_eq!(parent_tree(&doc), [8, 9, 9, 12]);
}

#[test]
fn deleted_pages_leave_the_tree_and_the_parent_tree() {
    // Page 2 (object 20) holds MCID 0 of the paragraph and a link annotation tagged by OBJR.
    let mut bodies: Vec<String> = (1..=13).map(tagged_body).collect();
    bodies[1] = "<< /Type /Pages /Kids [3 0 R 14 0 R] /Count 2 >>".into();
    bodies[8] = "<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [1 2 << /Type /MCR /Pg 14 0 R /MCID 0 >>] >>".into();
    bodies[6] = "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R 12 0 R 16 0 R 17 0 R] >>".into();
    bodies[9] = "<< /Nums [0 [8 0 R 9 0 R 9 0 R 12 0 R] 1 [9 0 R 17 0 R] 2 17 0 R] >>".into();
    bodies.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 15 0 R /StructParents 1 /Annots [18 0 R] >>".into());
    bodies.push(stream("/P << /MCID 0 >> BDC BT (p2) Tj ET EMC /Span << /MCID 1 >> BDC BT (x) Tj ET EMC"));
    bodies.push("<< /Type /StructElem /S /Link /P 7 0 R /Pg 14 0 R /K << /Type /OBJR /Obj 18 0 R >> >>".into());
    bodies.push("<< /Type /StructElem /S /Span /P 7 0 R /Pg 14 0 R /K 1 >>".into());
    bodies.push("<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /StructParent 2 >>".into());
    let mut doc = pdf(&bodies);
    let snap = pages_snapshot(&doc, &[1]).unwrap();
    // As page deletion does.
    doc.update_dict(r(2), |d| {
        d.set(b"Kids".to_vec(), Object::Array(vec![Object::Ref(r(3))]));
        d.set(b"Count".to_vec(), Object::Int(1));
    })
    .unwrap();
    doc.free(r(14));
    let u = drop_page_tags(&mut doc, &snap).unwrap();
    assert_eq!((u.removed, u.pruned, u.parent_tree), (3, 2, 2), "{u:?}");
    assert_eq!(u.pages, [1]);
    assert_eq!(kids(&doc, 7), [Object::Ref(r(8)), Object::Ref(r(9)), Object::Ref(r(12))]);
    assert_eq!(kids(&doc, 9), [Object::Int(1), Object::Int(2)]);
    let nums = doc.get(r(10)).as_dict().unwrap().get(b"Nums").unwrap().as_array().unwrap().clone();
    assert_eq!(nums.len(), 2, "only page 1's entry is left: {nums:?}");
    assert_consistent(&doc);
}

fn tagged_body(i: usize) -> String {
    let bodies = [
        "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /StructParents 0 \
         /Resources << /Font << /F1 5 0 R >> /XObject << /Im 11 0 R /Im2 13 0 R >> >> >>"
            .into(),
        stream(&original()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        "<< /Type /StructTreeRoot /K 7 0 R /ParentTree 10 0 R /ParentTreeNextKey 3 >>".into(),
        "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R 12 0 R] >>".into(),
        "<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K 0 /ActualText (Hello) >>".into(),
        "<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [1 2] >>".into(),
        "<< /Nums [0 [8 0 R 9 0 R 9 0 R 12 0 R]] >>".into(),
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x7f\nendstream"
            .into(),
        "<< /Type /StructElem /S /Figure /P 7 0 R /Pg 3 0 R /K 3 /Alt (A dot) >>".into(),
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x20\nendstream"
            .into(),
    ];
    bodies[i - 1].clone()
}

#[test]
fn hostile_trees_and_ids_do_not_crash() {
    // A tree whose elements are their own kids and parents, MCIDs out of range, an unclosed
    // sequence and a parent tree that is not a dictionary.
    let mut bodies: Vec<String> = (1..=13).map(tagged_body).collect();
    bodies[6] = "<< /Type /StructElem /S /Document /P 7 0 R /K [7 0 R 8 0 R 9 0 R 12 0 R 9 0 R] >>".into();
    bodies[8] = "<< /Type /StructElem /S /P /P 9 0 R /Pg 3 0 R /K [1 2 9 0 R 99999999999 -5] >>".into();
    bodies[9] = "(not a tree)".into();
    bodies[3] = stream(&format!("{} /P << /MCID 99999999999 >> BDC BT (big) Tj ET EMC /P << /MCID 7 >> BDC", original()));
    let mut doc = pdf(&bodies);
    let snap = snapshot(&doc, 0).unwrap();
    set_content(&mut doc, &format!("{HEADING} BT (new) Tj ET {FIGURE} /P << /MCID 7 >> BDC (x) Tj"));
    let _ = keep_tags(&mut doc, &snap);
    let snap = pages_snapshot(&doc, &[0, 5]).unwrap();
    let _ = drop_page_tags(&mut doc, &snap);
}
