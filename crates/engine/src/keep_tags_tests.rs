//! Keep tags on edit: real edits on a tagged document keep its structure tree in step.

use std::sync::Arc;

use super::*;

/// Page 1: a heading (H1, MCID 0), a two-line paragraph with one MCID per line inside one text
/// object (P, MCIDs 1 and 2), and a figure (MCID 3) with alternate text. Page 2: a paragraph
/// (MCID 0). Objects: 8 = H1, 9 = P, 12 = Figure, 15 = page 2's P.
fn tagged_pdf() -> Vec<u8> {
    let c1 = "/H1 << /MCID 0 >> BDC BT /F1 18 Tf 20 250 Td (Annual report) Tj ET EMC \
              BT /F1 12 Tf 20 200 Td /P << /MCID 1 >> BDC (The first line of text) Tj EMC 0 -14 Td /P << /MCID 2 >> BDC (and the second line) Tj EMC ET \
              /Figure << /MCID 3 >> BDC q 40 0 0 40 20 100 cm /Im1 Do Q EMC";
    let c2 = "/P << /MCID 0 >> BDC BT /F1 12 Tf 20 250 Td (Page two) Tj ET EMC";
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R /Lang (en-US) >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 13 0 R] /Count 2 /MediaBox [0 0 300 300] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /StructParents 0 /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 10 0 R >> >> >>"
            .to_vec(),
        format!("<< /Length {} >>\nstream\n{c1}\nendstream", c1.len()).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
        b"<< /Type /StructTreeRoot /K 7 0 R /ParentTree 11 0 R /ParentTreeNextKey 2 >>".to_vec(),
        b"<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R 12 0 R 15 0 R] >>".to_vec(),
        b"<< /Type /StructElem /S /H1 /P 7 0 R /Pg 3 0 R /K 0 >>".to_vec(),
        b"<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [1 2] /ActualText (The first line of text and the second line) >>".to_vec(),
        b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x7f\nendstream"
            .to_vec(),
        b"<< /Nums [0 [8 0 R 9 0 R 9 0 R 12 0 R] 1 [15 0 R]] >>".to_vec(),
        b"<< /Type /StructElem /S /Figure /P 7 0 R /Pg 3 0 R /K 3 /Alt (A grey square) >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 14 0 R /StructParents 1 /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{c2}\nendstream", c2.len()).into_bytes(),
        b"<< /Type /StructElem /S /P /P 7 0 R /Pg 13 0 R /K 0 >>".to_vec(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn open() -> (Session, DocId) {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("tagged.pdf", None, Arc::new(tagged_pdf()), None).expect("opens");
    (s, id)
}

fn cos(s: &Session, id: DocId) -> &pdfcraft_cos::Document {
    &s.get(id).unwrap().editor.as_ref().unwrap().cos
}

fn update(s: &Session, id: DocId) -> a11y::TagUpdate {
    s.get(id).unwrap().tag_update.clone().unwrap_or_default()
}

/// The Accessibility Checker's Tagged content rule (everything drawn is tagged or an artifact)
/// and, for every page, the parent tree agreeing with the tree.
fn assert_tagged(s: &Session, id: DocId) {
    let doc = cos(s, id);
    let r = a11y::check(doc, &a11y::Options { rules: [a11y::Rule::TaggedContent].into(), pages: None });
    assert_eq!(r.result(a11y::Rule::TaggedContent).unwrap().status, a11y::Status::Passed, "{:?}", r.result(a11y::Rule::TaggedContent));
    let tree = a11y::tags(doc);
    let root = doc.get(doc.root().unwrap()).as_dict().unwrap().reference(b"StructTreeRoot").unwrap();
    let pt = doc.get(root).as_dict().unwrap().reference(b"ParentTree").unwrap();
    let nums = doc.get(pt).as_dict().unwrap().get(b"Nums").unwrap().as_array().unwrap().clone();
    for (pi, page) in pdfcraft_model::pages(doc).iter().enumerate() {
        let key = page.dict.get(b"StructParents").and_then(pdfcraft_cos::Object::as_int).unwrap();
        let v = nums.chunks(2).find(|c| c[0].as_int() == Some(key)).unwrap()[1].clone();
        let arr: Vec<Option<pdfcraft_cos::ObjRef>> = doc.resolve(&v).as_array().unwrap().iter().map(|o| o.as_ref()).collect();
        for n in &tree.nodes {
            for k in &n.kids {
                if let a11y::TagKid::Content { page: Some(p), mcid, stream: None } = k
                    && *p == pi
                {
                    assert_eq!(arr.get(*mcid as usize).copied().flatten(), n.obj, "page {pi} MCID {mcid}");
                }
            }
        }
    }
}

fn types(s: &Session, id: DocId) -> Vec<String> {
    a11y::tags(cos(s, id)).nodes.iter().map(|n| n.ty.clone()).collect()
}

fn png() -> Arc<Vec<u8>> {
    let image = image::RgbaImage::from_fn(4, 4, |x, _| image::Rgba([x as u8 * 60, 20, 20, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    Arc::new(out.into_inner())
}

#[test]
fn the_fixture_is_tagged() {
    let (s, id) = open();
    assert_tagged(&s, id);
    assert!(s.keep_tags_on_edit());
}

#[test]
fn editing_a_line_keeps_its_tag_in_one_undo_step() {
    let (mut s, id) = open();
    let line = s.get(id).unwrap().text_lines(0).iter().position(|l| l.text == "The first line of text").unwrap();
    s.apply(id, Edit::EditTextLine { page: 0, line, text: "The very first line".into() }).unwrap();
    let u = update(&s, id);
    assert_eq!((u.kept, u.added.len(), u.removed), (1, 0, 0), "{u:?}");
    assert_eq!(u.actual_text_cleared, 1, "the paragraph's ActualText spelled the old words");
    assert_tagged(&s, id);
    assert_eq!(types(&s, id), ["Document", "H1", "P", "Figure", "P"]);
    assert_eq!(s.undo(id).unwrap(), "Edit text");
    assert!(s.get(id).unwrap().can_undo().is_none(), "one step");
    let p = cos(&s, id).get(pdfcraft_cos::ObjRef::new(9, 0));
    assert!(p.as_dict().unwrap().contains(b"ActualText"), "undo brings the tags back too");
}

#[test]
fn rewriting_a_paragraph_keeps_it_in_its_element() {
    let (mut s, id) = open();
    let block = s.get(id).unwrap().text_blocks(0).iter().position(|b| b.text.starts_with("The first line")).unwrap();
    s.apply(id, Edit::EditTextBlock { page: 0, block, text: "A rewritten paragraph that says something else".into(), style: BlockStyle::default() })
        .unwrap();
    let u = update(&s, id);
    assert!(u.added.is_empty(), "no new element: {u:?}");
    assert_eq!((u.adopted, u.removed), (1, 1), "the new text took the first line's MCID; the second's went: {u:?}");
    assert_tagged(&s, id);
    assert_eq!(types(&s, id), ["Document", "H1", "P", "Figure", "P"]);
    let tree = a11y::tags(cos(&s, id));
    let p = tree.nodes.iter().find(|n| n.obj == Some(pdfcraft_cos::ObjRef::new(9, 0))).unwrap();
    assert!(!p.kids.is_empty(), "the paragraph still owns its content");
    assert!(p.actual_text.is_none());
}

#[test]
fn added_text_and_images_are_tagged_and_follow_later_edits() {
    let (mut s, id) = open();
    let text = AddedText { rect: [20.0, 60.0, 200.0, 80.0], text: "A note added later".into(), ..AddedText::default() };
    s.apply(id, Edit::AddText { page: 0, text: text.clone() }).unwrap();
    let u = update(&s, id);
    assert_eq!(u.added.iter().map(|a| a.role.as_str()).collect::<Vec<_>>(), ["P"], "{u:?}");
    assert_tagged(&s, id);
    assert_eq!(types(&s, id), ["Document", "H1", "P", "Figure", "P", "P"], "after the figure, before page 2");
    // Retyped and moved: the same element.
    let moved = AddedText { rect: [30.0, 40.0, 200.0, 60.0], text: "The note, retyped".into(), ..text };
    s.apply(id, Edit::UpdateContent { page: 0, index: 0, content: AddedContent::Text(moved) }).unwrap();
    let u = update(&s, id);
    assert_eq!((u.adopted, u.added.len()), (1, 0), "{u:?}");
    assert_tagged(&s, id);
    assert_eq!(types(&s, id).len(), 6);
    // An image: a Figure, which needs alternate text.
    s.apply(id, Edit::AddImage { page: 0, rect: Some([150.0, 150.0, 190.0, 190.0]), name: "dot.png".into(), bytes: png() }).unwrap();
    let u = update(&s, id);
    assert_eq!(u.added.iter().map(|a| a.role.as_str()).collect::<Vec<_>>(), ["Figure"], "{u:?}");
    assert_tagged(&s, id);
    s.apply(id, Edit::ReplaceImage { page: 0, index: 1, name: "dot2.png".into(), bytes: png() }).unwrap();
    assert_eq!(update(&s, id).adopted, 1);
    assert_tagged(&s, id);
    // Deleted: their elements go.
    s.apply(id, Edit::DeleteContent { page: 0, index: 1 }).unwrap();
    s.apply(id, Edit::DeleteContent { page: 0, index: 0 }).unwrap();
    let u = update(&s, id);
    assert_eq!((u.removed, u.pruned), (1, 1), "{u:?}");
    assert_tagged(&s, id);
    assert_eq!(types(&s, id), ["Document", "H1", "P", "Figure", "P"]);
}

#[test]
fn page_images_deleted_or_replaced_update_their_figure() {
    let (mut s, id) = open();
    s.apply(id, Edit::EditPageImage { page: 0, index: 0, change: ImageEdit::Replace { name: "new.png".into(), bytes: png() } }).unwrap();
    let u = update(&s, id);
    assert_eq!(u.review_alt, [pdfcraft_cos::ObjRef::new(12, 0)], "{u:?}");
    assert_tagged(&s, id);
    s.apply(id, Edit::EditPageImage { page: 0, index: 0, change: ImageEdit::Move([100.0, 100.0, 140.0, 140.0]) }).unwrap();
    assert_tagged(&s, id);
    s.apply(id, Edit::EditPageImage { page: 0, index: 0, change: ImageEdit::Delete }).unwrap();
    let u = update(&s, id);
    assert_eq!((u.removed, u.pruned), (1, 1), "{u:?}");
    assert_eq!(types(&s, id), ["Document", "H1", "P", "P"]);
    assert_tagged(&s, id);
}

#[test]
fn deleting_a_page_removes_its_tags() {
    let (mut s, id) = open();
    s.apply(id, Edit::DeletePages { pages: vec![1] }).unwrap();
    let u = update(&s, id);
    assert_eq!((u.removed, u.pruned, u.parent_tree), (1, 1, 1), "{u:?}");
    assert_eq!(types(&s, id), ["Document", "H1", "P", "Figure"]);
    assert_tagged(&s, id);
}

#[test]
fn with_keep_tags_off_edits_leave_the_tree_alone() {
    let (mut s, id) = open();
    s.set_keep_tags_on_edit(false);
    let text = AddedText { rect: [20.0, 60.0, 200.0, 80.0], text: "Untagged note".into(), ..AddedText::default() };
    s.apply(id, Edit::AddText { page: 0, text }).unwrap();
    assert!(s.get(id).unwrap().tag_update.is_none());
    assert_eq!(types(&s, id), ["Document", "H1", "P", "Figure", "P"]);
    let r = a11y::check(cos(&s, id), &a11y::Options { rules: [a11y::Rule::TaggedContent].into(), pages: None });
    assert_eq!(r.result(a11y::Rule::TaggedContent).unwrap().status, a11y::Status::Failed);
}

#[test]
fn untagged_documents_get_no_tags_from_edits() {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("plain.pdf", None, Arc::new(super::tests::fixture(1)), None).unwrap();
    let text = AddedText { rect: [20.0, 60.0, 180.0, 80.0], text: "Note".into(), ..AddedText::default() };
    s.apply(id, Edit::AddText { page: 0, text }).unwrap();
    assert!(s.get(id).unwrap().tag_update.is_none());
    assert!(!a11y::tags(cos(&s, id)).exists);
}
