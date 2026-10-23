//! New Tag, Delete Tag and moving a tag into another, on synthetic trees, hostile ones included.

use std::sync::Arc;

use pdfcraft_cos::{Document, ObjRef};

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

/// Document > Sect(8) > [P(9) mcid 0, P(10) mcid 1]; a link annotation (11) in a Link (12).
fn doc() -> Document {
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 5 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /StructParents 0 /Annots [11 0 R] >>".into(),
        stream("/P << /MCID 0 >> BDC 0 0 m 10 10 l S EMC /P << /MCID 1 >> BDC 0 20 m 10 30 l S EMC"),
        "<< /Type /StructTreeRoot /K 6 0 R /ParentTree 7 0 R /ParentTreeNextKey 2 >>".into(),
        "<< /Type /StructElem /S /Document /P 5 0 R /K [8 0 R 12 0 R] >>".into(),
        "<< /Nums [0 [9 0 R 10 0 R] 1 12 0 R] >>".into(),
        "<< /Type /StructElem /S /Sect /P 6 0 R /Pg 3 0 R /K [9 0 R 10 0 R] >>".into(),
        "<< /Type /StructElem /S /P /P 8 0 R /Pg 3 0 R /K 0 >>".into(),
        "<< /Type /StructElem /S /P /P 8 0 R /Pg 3 0 R /K 1 >>".into(),
        "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /StructParent 1 >>".into(),
        "<< /Type /StructElem /S /Link /P 6 0 R /Pg 3 0 R /K << /Type /OBJR /Obj 11 0 R >> >>".into(),
    ])
}

fn r(n: u32) -> ObjRef {
    ObjRef::new(n, 0)
}

fn types(d: &Document) -> Vec<(String, usize)> {
    tags(d).nodes.iter().map(|n| (n.ty.clone(), n.depth)).collect()
}

#[test]
fn a_new_tag_goes_last_inside_its_parent_and_can_receive_moved_tags() {
    let mut d = doc();
    let h = new_element(&mut d, Some(r(8)), "H1").unwrap();
    let t = tags(&d);
    let sect = t.find(r(8)).unwrap();
    let new = t.find(h).unwrap();
    assert_eq!(t.nodes[new].parent, Some(sect));
    assert_eq!(t.nodes[sect].kids.last(), Some(&TagKid::Element(new)));
    // Move the first paragraph into the empty heading.
    move_element(&mut d, r(9), OrderPlace::Into(h)).unwrap();
    let t = tags(&d);
    let new = t.find(h).unwrap();
    assert_eq!(t.nodes[new].kids, vec![TagKid::Element(t.find(r(9)).unwrap())]);
    assert_eq!(d.get(r(9)).as_dict().unwrap().get(b"P").and_then(pdfcraft_cos::Object::as_ref), Some(h));
    // A top-level tag too.
    let top = new_element(&mut d, None, "Part").unwrap();
    assert!(tags(&d).roots.contains(&tags(&d).find(top).unwrap()));
}

#[test]
fn bad_new_tags_and_moves_change_nothing() {
    let d0 = doc();
    let mut d = d0.clone();
    assert!(new_element(&mut d, Some(r(8)), "NotAType").is_err());
    assert!(new_element(&mut d, Some(r(8)), "Artifact").is_err());
    assert!(new_element(&mut d, Some(r(8)), "").is_err());
    assert!(new_element(&mut d, Some(r(99)), "P").is_err());
    assert!(move_element(&mut d, r(8), OrderPlace::Into(r(9))).is_err(), "into its own kid");
    assert!(move_element(&mut d, r(8), OrderPlace::Into(r(8))).is_err(), "into itself");
    assert_eq!(tags(&d), tags(&d0));
    let mut untagged = pdf(&["<< /Type /Catalog /Pages 2 0 R >>".into(), "<< /Type /Pages /Kids [] /Count 0 >>".into()]);
    assert_eq!(new_element(&mut untagged, None, "P"), Err(TagError::Untagged));
    assert_eq!(delete_element(&mut untagged, r(1)).map(|_| ()), Err(TagError::Untagged));
}

#[test]
fn deleting_a_tag_moves_its_kids_up_and_keeps_the_content_tagged() {
    let mut d = doc();
    let res = delete_element(&mut d, r(8)).unwrap();
    assert_eq!((res.lifted, res.parent), (2, Some(r(6))));
    assert_eq!(types(&d), vec![("Document".into(), 0), ("P".into(), 1), ("P".into(), 1), ("Link".into(), 1)]);
    // The paragraphs point at the Document now; the parent tree still reaches them.
    assert_eq!(d.get(r(9)).as_dict().unwrap().get(b"P").and_then(pdfcraft_cos::Object::as_ref), Some(r(6)));
    // Delete a paragraph: its MCID moves up with its page, and the parent tree follows.
    let res = delete_element(&mut d, r(9)).unwrap();
    assert_eq!(res.pages, vec![0]);
    let t = tags(&d);
    let docu = t.find(r(6)).unwrap();
    assert!(t.nodes[docu].kids.contains(&TagKid::Content { page: Some(0), mcid: 0, stream: None }));
    let pt = d.get(r(7));
    let nums = format!("{:?}", pt);
    assert!(nums.contains("6"), "parent tree points MCID 0 at the Document: {nums}");
    // The Link's annotation goes to the Document too.
    delete_element(&mut d, r(12)).unwrap();
    let t = tags(&d);
    assert!(t.nodes[t.find(r(6)).unwrap()].kids.iter().any(|k| matches!(k, TagKid::Object { obj, .. } if *obj == r(11))));
}

#[test]
fn a_top_level_tag_holding_content_is_not_deleted() {
    let mut d = doc();
    delete_element(&mut d, r(6)).unwrap();
    // The Sect and the Link are top-level now; the Link holds an annotation itself.
    let before = tags(&d);
    assert!(delete_element(&mut d, r(12)).is_err());
    assert_eq!(tags(&d), before);
    assert!(delete_element(&mut d, r(99)).is_err());
}

#[test]
fn hostile_trees_never_panic() {
    // A cycle: 6 > 7 > 6; a deep chain; kids that aren't elements.
    let mut bodies: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 4 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".into(),
        "<< /Type /StructTreeRoot /K [5 0 R 3 0 R (x) 7] >>".into(),
        "<< /Type /StructElem /S /Div /P 4 0 R /K [6 0 R] >>".into(),
        "<< /Type /StructElem /S /P /P 5 0 R /K [5 0 R 8 0 R] >>".into(),
    ];
    let deep = 2000;
    for i in 0..deep {
        let n = 7 + i;
        bodies.push(format!("<< /Type /StructElem /S /Span /P {} 0 R /K [{} 0 R] >>", n - 1, n + 1));
    }
    bodies.push("<< /Type /StructElem /S /Span >>".into());
    let d0 = pdf(&bodies);
    for target in [5, 6, 7, 8, 100, 2000, 9999] {
        let mut d = d0.clone();
        let _ = delete_element(&mut d, r(target));
        let _ = new_element(&mut d, Some(r(target)), "P");
        let _ = move_element(&mut d, r(target), OrderPlace::Into(r(5)));
        let _ = move_element(&mut d, r(6), OrderPlace::Into(r(target)));
    }
}
