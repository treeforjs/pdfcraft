use std::sync::Arc;

use printcraft_cos::{Document, Object, SaveOptions, write_full, write_incremental};

use super::*;

/// A 3-page document with a nested page tree. MediaBox and Rotate are inherited from the root,
/// Resources from an intermediate node; each page's content says which page it is.
fn fixture() -> Vec<u8> {
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),                                                  // 1
        b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 3 /MediaBox [0 0 300 400] /Rotate 90 >>".to_vec(), // 2
        b"<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 /Resources << /Font << /F1 10 0 R >> >> >>".to_vec(), // 3
        b"<< /Type /Page /Parent 3 0 R /Contents 7 0 R >>".to_vec(),                                    // 4
        b"<< /Type /Page /Parent 3 0 R /Contents 8 0 R >>".to_vec(),                                    // 5
        b"<< /Type /Page /Parent 2 0 R /Contents 9 0 R /Resources << /Font << /F1 10 0 R >> >> /MediaBox [0 0 500 500] >>".to_vec(), // 6
        stream(b"BT /F1 12 Tf 20 20 Td (Page 1) Tj ET"),                                                // 7
        stream(b"BT /F1 12 Tf 20 20 Td (Page 2) Tj ET"),                                                // 8
        stream(b"BT /F1 12 Tf 20 20 Td (Page 3) Tj ET"),                                                // 9
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),                             // 10
        b"<< /Title (Original) /Producer (fixture) >>".to_vec(),                                        // 11
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
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R /Info 11 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn stream(body: &[u8]) -> Vec<u8> {
    let mut v = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
    v.extend_from_slice(body);
    v.extend_from_slice(b"\nendstream");
    v
}

fn open(bytes: Vec<u8>) -> Document {
    Document::open(Arc::new(bytes)).expect("opens")
}

/// The text label of each page, via its content stream, in order.
fn labels(doc: &Document) -> Vec<String> {
    pages(doc)
        .unwrap()
        .iter()
        .map(|p| {
            let page = doc.get(p.obj);
            let c = page.as_dict().and_then(|d| d.get(b"Contents").cloned());
            match c.map(|c| doc.resolve(&c)).as_deref() {
                Some(Object::Stream(s)) => {
                    let data = s.decoded().unwrap();
                    let t = String::from_utf8_lossy(&data);
                    t.split('(').nth(1).and_then(|x| x.split(')').next()).unwrap_or("").to_string()
                }
                _ => "blank".into(),
            }
        })
        .collect()
}

/// Save incrementally, verify the prefix is untouched, and reopen — both with our reader and with
/// an independent parser (hayro-syntax) that must agree on the page count.
fn save_and_reopen(doc: &Document) -> Document {
    let original = doc.bytes().clone();
    let bytes = write_incremental(doc, &SaveOptions::default()).expect("saves");
    assert_eq!(&bytes[..original.len()], &original[..], "incremental save must not modify existing bytes");
    let reopened = open(bytes.clone());
    let independent = hayro_syntax::Pdf::new(bytes).expect("hayro opens our output");
    assert_eq!(independent.pages().len(), page_count(&reopened).unwrap(), "readers agree on page count");
    reopened
}

#[test]
fn walks_nested_tree_in_order() {
    let doc = open(fixture());
    assert_eq!(labels(&doc), ["Page 1", "Page 2", "Page 3"]);
}

#[test]
fn rotate_uses_inherited_rotation_and_round_trips() {
    let mut doc = open(fixture());
    rotate_pages(&mut doc, &[0, 2], 90).unwrap();
    rotate_pages(&mut doc, &[2], -270).unwrap(); // page 3: inherited 90 + 90 - 270 = -90 ≡ 270
    let doc = save_and_reopen(&doc);
    let rot = |i: usize| {
        let p = pages(&doc).unwrap()[i].obj;
        doc.get(p).as_dict().and_then(|d| d.int(b"Rotate"))
    };
    assert_eq!(rot(0), Some(180), "inherited 90 + 90");
    assert_eq!(rot(1), None, "untouched page keeps inheriting");
    assert_eq!(rot(2), Some(270), "normalised into 0..360");
}

#[test]
fn delete_keeps_inherited_attributes_on_survivors() {
    let mut doc = open(fixture());
    delete_pages(&mut doc, &[0]).unwrap();
    let doc = save_and_reopen(&doc);
    assert_eq!(labels(&doc), ["Page 2", "Page 3"]);
    // Page 2 inherited Resources (from the intermediate node) and MediaBox/Rotate (from the root);
    // the tree was flattened, so they must now be on the page itself.
    let p = doc.get(pages(&doc).unwrap()[0].obj);
    let d = p.as_dict().unwrap();
    assert!(d.contains(b"Resources") && d.contains(b"MediaBox"));
    assert_eq!(d.int(b"Rotate"), Some(90));
    let root = doc.get(doc.get(doc.root().unwrap()).as_dict().unwrap().reference(b"Pages").unwrap());
    assert_eq!(root.as_dict().unwrap().int(b"Count"), Some(2));
}

#[test]
fn cannot_delete_every_page_or_missing_pages() {
    let mut doc = open(fixture());
    assert_eq!(delete_pages(&mut doc, &[0, 1, 2]), Err(OrganizeError::WouldRemoveAllPages));
    assert_eq!(delete_pages(&mut doc, &[7]), Err(OrganizeError::NoSuchPage(7)));
    assert_eq!(rotate_pages(&mut doc, &[3], 90), Err(OrganizeError::NoSuchPage(3)));
    assert!(!doc.is_modified(), "failed operations leave the document untouched");
}

#[test]
fn move_pages_reorders() {
    let mut doc = open(fixture());
    move_pages(&mut doc, &[2], 0).unwrap();
    assert_eq!(labels(&doc), ["Page 3", "Page 1", "Page 2"]);
    move_pages(&mut doc, &[0, 1], 3).unwrap();
    assert_eq!(labels(&doc), ["Page 2", "Page 3", "Page 1"]);
    let doc = save_and_reopen(&doc);
    assert_eq!(labels(&doc), ["Page 2", "Page 3", "Page 1"]);
}

#[test]
fn inserts_blank_page() {
    let mut doc = open(fixture());
    insert_blank_page(&mut doc, 1, 612.0, 792.0).unwrap();
    let doc = save_and_reopen(&doc);
    assert_eq!(labels(&doc), ["Page 1", "blank", "Page 2", "Page 3"]);
    let hay = hayro_syntax::Pdf::new(doc.bytes().clone()).unwrap();
    let (w, h) = hay.pages().get(1).unwrap().render_dimensions();
    assert_eq!((w, h), (612.0, 792.0));
}

#[test]
fn document_info_edits_round_trip_with_unicode() {
    let mut doc = open(fixture());
    set_info(&mut doc, "Title", "Résumé — 履歴書").unwrap();
    set_info(&mut doc, "Author", "PrintCraft").unwrap();
    set_info(&mut doc, "Producer", "").unwrap(); // clearing removes the key
    let doc = save_and_reopen(&doc);
    assert_eq!(info(&doc, "Title").as_deref(), Some("Résumé — 履歴書"));
    assert_eq!(info(&doc, "Author").as_deref(), Some("PrintCraft"));
    assert_eq!(info(&doc, "Producer"), None);
}

#[test]
fn successive_incremental_saves_stack_revisions() {
    let mut doc = open(fixture());
    rotate_pages(&mut doc, &[0], 90).unwrap();
    let doc = save_and_reopen(&doc);
    assert_eq!(doc.revisions().len(), 2);
    let mut doc = doc;
    delete_pages(&mut doc, &[1]).unwrap();
    let doc = save_and_reopen(&doc);
    assert_eq!(doc.revisions().len(), 3);
    assert_eq!(labels(&doc), ["Page 1", "Page 3"]);
}

#[test]
fn full_save_drops_unreachable_objects() {
    let mut doc = open(fixture());
    delete_pages(&mut doc, &[0, 1]).unwrap();
    let bytes = write_full(&doc, &SaveOptions::default()).unwrap();
    let full = open(bytes.clone());
    assert_eq!(labels(&full), ["Page 3"]);
    // Deleted pages, their content streams and the orphaned intermediate node are gone.
    assert!(full.object_numbers().len() < doc.object_numbers().len());
    assert_eq!(info(&full, "Title").as_deref(), Some("Original"));
    assert_eq!(hayro_syntax::Pdf::new(bytes).unwrap().pages().len(), 1);
}

// ── Combine / extract / split ──────────────────────────────────────────────────────────────────

/// Build a classic-xref PDF from object bodies (object i+1 = bodies[i]).
fn build(bodies: &[String], trailer: &str) -> Vec<u8> {
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
    out.extend_from_slice(format!("trailer\n<< /Size {} {trailer} >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).as_bytes());
    out
}

fn body(text: &str) -> String {
    let s = format!("BT /F1 12 Tf 20 20 Td ({text}) Tj ET");
    format!("<< /Length {} >>\nstream\n{s}\nendstream", s.len())
}

/// Document A: pages A1–A3 sharing one font. A1 links to A3 (GoTo action), A2 links to A1
/// (/Dest) and carries a text field widget; A3 has a note with a popup (a reference cycle).
fn doc_a() -> Document {
    let b: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [12 0 R] >> >>".into(), // 1
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 300 400] >>".into(), // 2
        "<< /Type /Page /Parent 2 0 R /Contents 6 0 R /Resources << /Font << /F1 9 0 R >> >> /Annots [10 0 R] >>".into(), // 3
        "<< /Type /Page /Parent 2 0 R /Contents 7 0 R /Resources << /Font << /F1 9 0 R >> >> /Annots [11 0 R 12 0 R] >>".into(), // 4
        "<< /Type /Page /Parent 2 0 R /Contents 8 0 R /Resources << /Font << /F1 9 0 R >> >> /Annots [13 0 R] /StructParents 4 >>".into(), // 5
        body("A1"),                                                                  // 6
        body("A2"),                                                                  // 7
        body("A3"),                                                                  // 8
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),             // 9
        "<< /Type /Annot /Subtype /Link /Rect [0 0 50 50] /P 3 0 R /A << /S /GoTo /D [5 0 R /Fit] >> >>".into(), // 10
        "<< /Type /Annot /Subtype /Link /Rect [0 0 50 50] /P 4 0 R /Dest [3 0 R /XYZ 0 400 0] >>".into(), // 11
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (Ada) /Rect [60 60 200 80] /P 4 0 R >>".into(), // 12
        "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (note) /Popup 14 0 R /P 5 0 R /StructParent 7 >>".into(), // 13
        "<< /Type /Annot /Subtype /Popup /Rect [40 40 200 120] /Parent 13 0 R >>".into(), // 14
        "<< /Title (Doc A) /Author (Alice) >>".into(),                               // 15
    ];
    open(build(&b, "/Root 1 0 R /Info 15 0 R"))
}

/// Document B: pages B1, B2 in a nested tree with MediaBox and Rotate inherited from the root.
fn doc_b() -> Document {
    let b: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 2 /MediaBox [0 0 500 250] /Rotate 90 >>".into(),
        "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 >>".into(),
        "<< /Type /Page /Parent 3 0 R /Contents 6 0 R /Resources << /Font << /F1 8 0 R >> >> >>".into(),
        "<< /Type /Page /Parent 3 0 R /Contents 7 0 R /Resources << /Font << /F1 8 0 R >> >> >>".into(),
        body("B1"),
        body("B2"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>".into(),
    ];
    open(build(&b, "/Root 1 0 R"))
}

/// Write in full, reopen with our reader, and require hayro to agree on the page count.
fn full_roundtrip(doc: &Document) -> Document {
    let bytes = write_full(doc, &SaveOptions::default()).expect("writes");
    let reopened = open(bytes.clone());
    let hay = hayro_syntax::Pdf::new(bytes).expect("hayro opens");
    assert_eq!(hay.pages().len(), page_count(&reopened).unwrap());
    reopened
}

fn page_dict(doc: &Document, i: usize) -> Dict {
    doc.get(pages(doc).unwrap()[i].obj).as_dict().cloned().unwrap()
}

fn annots(doc: &Document, i: usize) -> Vec<Dict> {
    let p = page_dict(doc, i);
    let list = p.get(b"Annots").map(|a| doc.resolve(a).as_array().cloned().unwrap_or_default()).unwrap_or_default();
    list.iter().map(|a| doc.resolve(a).as_dict().cloned().unwrap()).collect()
}

fn count_fonts(doc: &Document) -> usize {
    doc.object_numbers().iter().filter(|n| doc.get(ObjRef::new(**n, 0)).as_dict().is_some_and(|d| d.name(b"Type") == Some(b"Font"))).count()
}

#[test]
fn combine_concatenates_with_a_bookmark_per_file() {
    let out = full_roundtrip(&combine(&[("Report A", &doc_a()), ("Report B", &doc_b())]).unwrap());
    assert_eq!(labels(&out), ["A1", "A2", "A3", "B1", "B2"]);
    // Shared resources are copied once per source: one Helvetica, one Times.
    assert_eq!(count_fonts(&out), 2);
    // Bookmarks: one per source file, pointing at its first page.
    let cat = out.get(out.root().unwrap()).as_dict().cloned().unwrap();
    let outlines = out.get(cat.reference(b"Outlines").unwrap()).as_dict().cloned().unwrap();
    let first = out.get(outlines.reference(b"First").unwrap()).as_dict().cloned().unwrap();
    assert_eq!(first.get(b"Title").and_then(|t| t.as_string()).map(|s| s.to_text()).as_deref(), Some("Report A"));
    let second = out.get(first.reference(b"Next").unwrap()).as_dict().cloned().unwrap();
    assert_eq!(second.get(b"Title").and_then(|t| t.as_string()).map(|s| s.to_text()).as_deref(), Some("Report B"));
    let dest_page = second.get(b"Dest").and_then(|d| d.as_array()).and_then(|a| a[0].as_ref()).unwrap();
    assert_eq!(dest_page, pages(&out).unwrap()[3].obj);
}

#[test]
fn inherited_attributes_travel_with_copied_pages() {
    let out = full_roundtrip(&extract_pages(&doc_b(), &[1]).unwrap());
    let p = page_dict(&out, 0);
    assert_eq!(p.int(b"Rotate"), Some(90));
    assert!(p.contains(b"MediaBox") && p.contains(b"Resources"));
    let hay = hayro_syntax::Pdf::new(write_full(&out, &SaveOptions::default()).unwrap()).unwrap();
    assert_eq!(hay.pages().first().unwrap().render_dimensions(), (250.0, 500.0), "rotated 90°");
}

#[test]
fn links_are_rewired_to_copied_pages_or_dropped() {
    let a = doc_a();
    // Extract A3, A1 (reordered): A1's GoTo A3 must now point at the copy of A3 (index 0).
    let out = full_roundtrip(&extract_pages(&a, &[2, 0]).unwrap());
    assert_eq!(labels(&out), ["A3", "A1"]);
    let link = &annots(&out, 1)[0];
    let action = out.resolve(link.get(b"A").unwrap());
    let target = action.as_dict().unwrap().get(b"D").and_then(|d| d.as_array()).and_then(|a| a[0].as_ref()).unwrap();
    assert_eq!(target, pages(&out).unwrap()[0].obj);
    assert_eq!(link.reference(b"P"), Some(pages(&out).unwrap()[1].obj), "/P points at the new page");
    // Extract only A2: its link to A1 would dangle, so the destination is dropped.
    let out = full_roundtrip(&extract_pages(&a, &[1]).unwrap());
    let link = annots(&out, 0).into_iter().find(|d| d.name(b"Subtype") == Some(b"Link")).unwrap();
    assert!(!link.contains(b"Dest") && !link.contains(b"A"));
}

#[test]
fn form_fields_come_along_and_are_registered() {
    let out = full_roundtrip(&extract_pages(&doc_a(), &[1]).unwrap());
    let cat = out.get(out.root().unwrap()).as_dict().cloned().unwrap();
    let form = out.resolve(cat.get(b"AcroForm").unwrap());
    let fields = out.resolve(form.as_dict().unwrap().get(b"Fields").unwrap()).as_array().cloned().unwrap();
    assert_eq!(fields.len(), 1);
    let field = out.resolve(&fields[0]).as_dict().cloned().unwrap();
    assert_eq!(field.get(b"V").and_then(|v| v.as_string()).map(|s| s.to_text()).as_deref(), Some("Ada"));
    assert_eq!(field.reference(b"P"), Some(pages(&out).unwrap()[0].obj));
}

#[test]
fn popup_cycles_copy_once_and_structure_links_are_removed() {
    let out = full_roundtrip(&extract_pages(&doc_a(), &[2]).unwrap());
    let p = page_dict(&out, 0);
    assert!(!p.contains(b"StructParents"));
    let note = &annots(&out, 0)[0];
    assert!(!note.contains(b"StructParent"));
    let popup = out.get(note.reference(b"Popup").unwrap()).as_dict().cloned().unwrap();
    assert_eq!(
        out.get(popup.reference(b"Parent").unwrap()).as_dict().unwrap().get(b"Contents").and_then(|c| c.as_string()).map(|s| s.to_text()).as_deref(),
        Some("note")
    );
    // Nothing unrelated was dragged in: no pages A1/A2, no other content streams.
    assert_eq!(labels(&out), ["A3"]);
}

#[test]
fn inserting_pages_from_another_file_is_an_incremental_edit() {
    let mut a = doc_a();
    import_pages(&mut a, &doc_b(), &[1, 0], 1).unwrap();
    let a = save_and_reopen(&a); // checks the prefix invariant and hayro agreement
    assert_eq!(labels(&a), ["A1", "B2", "B1", "A2", "A3"]);
    assert_eq!(info(&a, "Title").as_deref(), Some("Doc A"));
    assert_eq!(import_pages(&mut a.clone(), &doc_b(), &[5], 0), Err(OrganizeError::NoSuchPage(5)));
}

#[test]
fn split_ranges_cover_every_page_once() {
    assert_eq!(split_ranges(5, &SplitBy::PageCount(2)), [0..2, 2..4, 4..5]);
    assert_eq!(split_ranges(5, &SplitBy::PageCount(0)), [0..1, 1..2, 2..3, 3..4, 4..5]);
    assert_eq!(split_ranges(5, &SplitBy::Before(vec![3, 1, 1, 9, 0])), [0..1, 1..3, 3..5]);
    assert_eq!(split_ranges(3, &SplitBy::PageCount(10)), vec![0..3]);
}

#[test]
fn split_produces_standalone_documents_with_metadata() {
    let parts = split(&doc_a(), &SplitBy::PageCount(1)).unwrap();
    assert_eq!(parts.len(), 3);
    for (i, part) in parts.iter().enumerate() {
        let part = full_roundtrip(part);
        assert_eq!(labels(&part), [format!("A{}", i + 1)]);
        assert_eq!(info(&part, "Author").as_deref(), Some("Alice"));
    }
}

#[test]
fn duplicate_pages_become_independent_copies() {
    let out = full_roundtrip(&extract_pages(&doc_b(), &[0, 0]).unwrap());
    assert_eq!(labels(&out), ["B1", "B1"]);
    let ps = pages(&out).unwrap();
    assert_ne!(ps[0].obj, ps[1].obj, "a page object may appear only once in the tree");
}

/// Document C: two pages. Page 1 has a link to the named destination `chap2` (page 2); page 2's
/// content uses a layer that is off by default. It has a bookmark tree and one attachment.
fn doc_c() -> Document {
    let b: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 9 0 R /EmbeddedFiles 13 0 R >> /OCProperties << /OCGs [8 0 R] /D << /OFF [8 0 R] >> >> /Outlines 10 0 R >>".into(), // 1
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 200 200] >>".into(), // 2
        "<< /Type /Page /Parent 2 0 R /Contents 5 0 R /Resources << /Font << /F1 7 0 R >> >> /Annots [<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest (chap2) >>] >>".into(), // 3
        "<< /Type /Page /Parent 2 0 R /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> /Properties << /L1 8 0 R >> >> >>".into(), // 4
        body("C1"),                                                                   // 5
        body("C2"),                                                                   // 6
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),             // 7
        "<< /Type /OCG /Name (Draft marks) >>".into(),                                // 8
        "<< /Names [(chap2) [4 0 R /Fit]] >>".into(),                                 // 9
        "<< /Type /Outlines /First 11 0 R /Last 11 0 R /Count 1 >>".into(),          // 10
        "<< /Title (Chapter one) /Parent 10 0 R /Dest [3 0 R /Fit] /First 12 0 R /Last 12 0 R /Count 1 >>".into(), // 11
        "<< /Title (Section) /Parent 11 0 R /A << /S /GoTo /D (chap2) >> >>".into(),  // 12
        "<< /Names [(notes.txt) 14 0 R] >>".into(),                                   // 13
        "<< /Type /Filespec /F (notes.txt) /EF << /F 15 0 R >> >>".into(),            // 14
        "<< /Type /EmbeddedFile /Length 5 >>\nstream\nhello\nendstream".into(),       // 15
    ];
    open(build(&b, "/Root 1 0 R"))
}

fn catalog(doc: &Document) -> Dict {
    doc.get(doc.root().unwrap()).as_dict().cloned().unwrap()
}

#[test]
fn named_destinations_are_resolved_to_the_copies() {
    let out = full_roundtrip(&extract_pages(&doc_c(), &[0, 1]).unwrap());
    let link = &annots(&out, 0)[0];
    let dest = link.get(b"Dest").and_then(|d| d.as_array()).expect("explicit destination");
    assert_eq!(dest[0].as_ref(), Some(pages(&out).unwrap()[1].obj));
    // Without the target page, the link loses its destination instead of dangling.
    let alone = full_roundtrip(&extract_pages(&doc_c(), &[0]).unwrap());
    assert!(!annots(&alone, 0)[0].contains(b"Dest"));
}

#[test]
fn layers_are_registered_with_their_default_state() {
    let out = full_roundtrip(&extract_pages(&doc_c(), &[1]).unwrap());
    let props = out.resolve(catalog(&out).get(b"OCProperties").unwrap()).as_dict().cloned().unwrap();
    let ocgs = props.get(b"OCGs").and_then(|o| o.as_array()).unwrap().clone();
    assert_eq!(ocgs.len(), 1);
    let name = out.resolve(&ocgs[0]).as_dict().unwrap().get(b"Name").and_then(|n| n.as_string()).map(|s| s.to_text());
    assert_eq!(name.as_deref(), Some("Draft marks"));
    let d = props.get(b"D").and_then(|d| d.as_dict()).unwrap();
    assert_eq!(d.get(b"OFF").and_then(|o| o.as_array()).unwrap(), &ocgs, "stays off by default");
    // Page 1 uses no layer, so extracting it alone adds no OCProperties.
    let out = extract_pages(&doc_c(), &[0]).unwrap();
    assert!(!catalog(&out).contains(b"OCProperties"));
}

#[test]
fn combine_nests_source_bookmarks_and_keeps_attachments() {
    let out = full_roundtrip(&combine(&[("C", &doc_c()), ("C again", &doc_c())]).unwrap());
    assert_eq!(labels(&out), ["C1", "C2", "C1", "C2"]);
    let outlines = out.get(catalog(&out).reference(b"Outlines").unwrap()).as_dict().cloned().unwrap();
    let file = out.get(outlines.reference(b"First").unwrap()).as_dict().cloned().unwrap();
    assert!(file.int(b"Count").unwrap() < 0, "source bookmarks start collapsed");
    let chapter = out.get(file.reference(b"First").unwrap()).as_dict().cloned().unwrap();
    assert_eq!(chapter.get(b"Title").and_then(|t| t.as_string()).map(|s| s.to_text()).as_deref(), Some("Chapter one"));
    let section = out.get(chapter.reference(b"First").unwrap()).as_dict().cloned().unwrap();
    let dest = section.get(b"Dest").and_then(|d| d.as_array()).expect("GoTo (named) became an explicit destination");
    assert_eq!(dest[0].as_ref(), Some(pages(&out).unwrap()[1].obj));
    // The second copy's bookmarks point into the second copy.
    let file2 = out.get(file.reference(b"Next").unwrap()).as_dict().cloned().unwrap();
    let chapter2 = out.get(file2.reference(b"First").unwrap()).as_dict().cloned().unwrap();
    assert_eq!(chapter2.get(b"Dest").and_then(|d| d.as_array()).unwrap()[0].as_ref(), Some(pages(&out).unwrap()[2].obj));
    // Attachments from both sources, the duplicate name disambiguated.
    let names = out.resolve(catalog(&out).get(b"Names").unwrap()).as_dict().cloned().unwrap();
    let tree = out.resolve(names.get(b"EmbeddedFiles").unwrap()).as_dict().cloned().unwrap();
    let keys: Vec<String> = tree.get(b"Names").and_then(|n| n.as_array()).unwrap().chunks(2).map(|p| p[0].as_string().unwrap().to_text()).collect();
    assert_eq!(keys, ["notes.txt", "notes.txt (2)"]);
}
