use std::sync::Arc;

use super::*;

/// A minimal `n`-page document; page `i` shows "Page i+1".
pub(crate) fn fixture(n: usize) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {n} /MediaBox [0 0 200 300] >>", kids.join(" ")).into_bytes());
    objs.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    for i in 0..n {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + 2 * i).into_bytes());
        let body = format!("BT /F1 24 Tf 20 150 Td (Page {}) Tj ET", i + 1);
        objs.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()).into_bytes());
    }
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

fn session_with(n: usize) -> (Session, DocId) {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("fixture.pdf", None, Arc::new(fixture(n)), None).expect("opens");
    (s, id)
}

#[test]
fn custom_image_stamp_keeps_displayed_orientation_on_rotated_pages() {
    let colours = [[240, 20, 20, 255], [20, 180, 20, 255], [20, 20, 240, 255], [230, 180, 20, 255]];
    let image = image::RgbaImage::from_fn(80, 40, |x, y| image::Rgba(colours[usize::from(y >= 20) * 2 + usize::from(x >= 40)]));
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap();
    let bytes = Arc::new(png.into_inner());
    for degrees in [0, 90, 180, 270] {
        let (mut session, id) = session_with(1);
        session.apply(id, Edit::RotatePages { pages: vec![0], degrees }).unwrap();
        let at = session.get(id).unwrap().info.pages[0].view_to_user(100.0, 100.0).map(f64::from);
        session
            .apply(
                id,
                Edit::AddCustomStamp {
                    page: 0,
                    rect: [at[0], at[1], at[0], at[1]],
                    name: "Original quadrants".into(),
                    file: MarkFile { name: "quadrants.png".into(), bytes: bytes.clone(), page: 0 },
                    author: "Test".into(),
                },
            )
            .unwrap();
        let check = |session: &Session, points: [(u32, u32); 4]| {
            let doc = session.get(id).unwrap();
            let mut renderer = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), doc.config.clone());
            let rendered = renderer.render(pdfcraft_render::RenderRequest { page: 0, scale: 1.0, ..Default::default() });
            assert!(rendered.error.is_none(), "{:?}", rendered.error);
            for ((x, y), colour) in points.into_iter().zip(colours) {
                let offset = ((y * rendered.width + x) * 4) as usize;
                assert_eq!(&rendered.rgba[offset..offset + 4], &colour, "page rotation {degrees}; displayed point ({x}, {y})");
            }
        };
        let points = [(80, 90), (120, 90), (80, 110), (120, 110)];
        check(&session, points);
        let info = session.get(id).unwrap().info.pages[0].clone();
        let rect = session.get(id).unwrap().info.annotations[0].rect;
        let a = info.user_to_view(rect[0], rect[1]);
        let b = info.user_to_view(rect[2], rect[3]);
        let bounds = [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])];
        for (actual, expected) in bounds.into_iter().zip([60.0, 80.0, 140.0, 120.0]) {
            assert!((actual - expected).abs() < 0.0001, "displayed bounds {bounds:?}");
        }
        session.apply(id, Edit::StyleAnnotation { page: 0, index: 0, color: None, opacity: Some(1.0), width: None, endings: None }).unwrap();
        check(&session, points);
        let resized = info.view_rect_to_user([40.0, 70.0, 160.0, 130.0]).map(f64::from);
        session.apply(id, Edit::ResizeAnnotation { page: 0, index: 0, rect: resized }).unwrap();
        let resized_points = [(70, 85), (130, 85), (70, 115), (130, 115)];
        check(&session, resized_points);
        session.undo(id).unwrap();
        check(&session, points);
        session.redo(id).unwrap();
        check(&session, resized_points);
        let from = info.view_to_user(100.0, 100.0);
        let to = info.view_to_user(110.0, 115.0);
        session.apply(id, Edit::MoveAnnotation { page: 0, index: 0, dx: f64::from(to[0] - from[0]), dy: f64::from(to[1] - from[1]) }).unwrap();
        check(&session, [(80, 100), (140, 100), (80, 130), (140, 130)]);
        session.undo(id).unwrap();
        check(&session, resized_points);
        session.redo(id).unwrap();
        check(&session, [(80, 100), (140, 100), (80, 130), (140, 130)]);
    }
}

fn page_texts(s: &Session, id: DocId) -> Vec<String> {
    let doc = s.get(id).unwrap();
    let config = pdfcraft_render::RenderConfig { password: doc.password.as_deref().map(Arc::from), ..Default::default() };
    let mut r = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), config);
    (0..doc.info.pages.len())
        .map(|p| {
            let r = r.render(RenderRequestFor::text(p));
            r.text.map(|t| t.plain_text().trim().to_string()).unwrap_or_default()
        })
        .collect()
}

/// Shorthand for building text-extraction requests.
struct RenderRequestFor;
impl RenderRequestFor {
    fn text(page: usize) -> pdfcraft_render::RenderRequest {
        pdfcraft_render::RenderRequest { page, kind: pdfcraft_render::RequestKind::Text, scale: 1.0, ..Default::default() }
    }
}

#[test]
fn edits_update_view_data_and_mark_dirty() {
    let (mut s, id) = session_with(3);
    assert!(s.get(id).unwrap().editable());
    assert!(!s.get(id).unwrap().dirty);
    s.apply(id, Edit::DeletePages { pages: vec![1] }).unwrap();
    let doc = s.get(id).unwrap();
    assert!(doc.dirty);
    assert_eq!(doc.info.pages.len(), 2, "inspection refreshed");
    assert_eq!(doc.can_undo(), Some("Delete page"));
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 3"], "renderer refreshed");
}

#[test]
fn undo_and_redo_restore_exact_states() {
    let (mut s, id) = session_with(3);
    let original = s.get(id).unwrap().bytes.clone();
    s.apply(id, Edit::MovePages { pages: vec![2], to: 0 }).unwrap();
    s.apply(id, Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
    assert_eq!(s.get(id).unwrap().info.pages[0].rotation, 90);
    assert_eq!(s.undo(id).unwrap(), "Rotate page");
    assert_eq!(s.get(id).unwrap().info.pages[0].rotation, 0);
    assert_eq!(s.undo(id).unwrap(), "Move page");
    assert_eq!(s.get(id).unwrap().bytes, original, "undoing everything returns the original bytes");
    assert_eq!(s.undo(id), Err(EditError::NothingToUndo));
    assert_eq!(s.redo(id).unwrap(), "Move page");
    assert_eq!(page_texts(&s, id), ["Page 3", "Page 1", "Page 2"]);
    // A new edit clears the redo stack.
    s.apply(id, Edit::InsertBlankPage { at: 0, width: 612.0, height: 792.0 }).unwrap();
    assert_eq!(s.redo(id), Err(EditError::NothingToRedo));
    assert_eq!(s.get(id).unwrap().info.pages.len(), 4);
}

#[test]
fn failed_edit_changes_nothing() {
    let (mut s, id) = session_with(2);
    let before = s.get(id).unwrap().bytes.clone();
    let err = s.apply(id, Edit::DeletePages { pages: vec![0, 1] }).unwrap_err();
    assert_eq!(err, EditError::Organize(pdfcraft_organize::OrganizeError::WouldRemoveAllPages));
    let doc = s.get(id).unwrap();
    assert!(!doc.dirty);
    assert_eq!(doc.can_undo(), None);
    assert_eq!(doc.bytes, before);
}

#[test]
fn save_is_incremental_stamps_mod_date_and_rebases() {
    let (mut s, id) = session_with(2);
    let original = s.get(id).unwrap().bytes.clone();
    s.apply(id, Edit::SetInfo { key: "Title".into(), value: "Quarterly report".into() }).unwrap();
    let saved = s.save_bytes(id).unwrap();
    assert_eq!(&saved[..original.len()], &original[..], "incremental save keeps the original bytes");
    let text = String::from_utf8_lossy(&saved[original.len()..]);
    assert!(text.contains("/ModDate (D:20231114221320Z)"), "mod date stamped: {text}");
    s.mark_saved(id, saved.clone(), Some("/tmp/out/report.pdf".into())).unwrap();
    let doc = s.get(id).unwrap();
    assert!(!doc.dirty);
    assert_eq!(doc.name, "report.pdf");
    assert_eq!(doc.info_value("Title").as_deref(), Some("Quarterly report"));
    assert_eq!(doc.info.title.as_deref(), Some("Quarterly report"));
    // Saving again without edits writes the same bytes; a later edit appends just one revision.
    assert_eq!(s.save_bytes(id).unwrap(), saved);
    s.apply(id, Edit::RotatePages { pages: vec![1], degrees: -90 }).unwrap();
    let second = s.save_bytes(id).unwrap();
    assert_eq!(&second[..saved.len()], &saved[..]);
    let reopened = pdfcraft_cos::Document::open(second).unwrap();
    assert_eq!(reopened.revisions().len(), 3);
}

#[test]
fn undo_still_works_after_save() {
    let (mut s, id) = session_with(2);
    s.apply(id, Edit::DeletePages { pages: vec![0] }).unwrap();
    let saved = s.save_bytes(id).unwrap();
    s.mark_saved(id, saved, None).unwrap();
    s.undo(id).unwrap();
    let doc = s.get(id).unwrap();
    assert!(doc.dirty, "undoing past a save makes the document dirty again");
    assert_eq!(doc.info.pages.len(), 2);
}

#[test]
fn full_save_is_readable_and_smaller_after_deletes() {
    let (mut s, id) = session_with(5);
    s.apply(id, Edit::DeletePages { pages: vec![0, 1, 2] }).unwrap();
    let full = s.save_full_bytes(id).unwrap();
    assert!(full.len() < s.save_bytes(id).unwrap().len());
    let mut s2 = Session::new();
    let id2 = s2.open("full.pdf", None, full, None).unwrap();
    assert_eq!(page_texts(&s2, id2), ["Page 4", "Page 5"]);
}

#[test]
fn linearized_save_is_fast_web_view_until_saved_incrementally() {
    let (mut s, id) = session_with(4);
    assert!(!s.get(id).unwrap().linearization().linearized);
    s.apply(id, Edit::DeletePages { pages: vec![1] }).unwrap();
    let lin = s.save_linearized_bytes(id).unwrap();
    let report = pdfcraft_cos::check_linearization(&lin, None);
    assert!(report.valid, "{:?}", report.problems);
    assert_eq!(report.params.unwrap().page_count, Some(3));
    s.mark_saved(id, lin.clone(), None).unwrap();
    let doc = s.get(id).unwrap();
    assert!(doc.fast_web_view() && doc.linearization().valid);
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 3", "Page 4"]);
    // An unsaved edit doesn't change what the file is; saving it incrementally ends Fast Web View.
    s.apply(id, Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
    assert!(s.get(id).unwrap().linearization().valid);
    let updated = s.save_bytes(id).unwrap();
    s.mark_saved(id, updated, None).unwrap();
    let doc = s.get(id).unwrap();
    assert!(!doc.fast_web_view());
    let report = doc.linearization();
    assert!(report.linearized && !report.valid);
}

#[test]
fn layer_choices_survive_edits() {
    // No layers in the fixture: just make sure refresh keeps working with an empty list and
    // that unknown layers are rejected.
    let (mut s, id) = session_with(1);
    assert!(!s.set_layer_visible(id, 0, false));
    s.apply(id, Edit::InsertBlankPage { at: 1, width: 100.0, height: 100.0 }).unwrap();
    assert_eq!(s.get(id).unwrap().info.pages.len(), 2);
}

#[test]
fn set_layer_actions_toggle_in_order_and_keep_radio_groups() {
    use pdfcraft_render::LayerOp::{Off, On, Toggle};
    let layer = |n: u32, visible: bool| Layer { id: (n, 0), name: format!("L{n}"), visible };
    let run = |changes: &[(LayerOp, u32)], preserve_rb: bool| {
        // L1 and L2 are a radio-button group; L3 is on its own.
        let mut layers = vec![layer(1, true), layer(2, false), layer(3, true)];
        let changes: Vec<_> = changes.iter().map(|&(op, n)| (op, (n, 0))).collect();
        let changed = apply_layer_state(&mut layers, &[vec![(1, 0), (2, 0)]], &changes, preserve_rb);
        (changed, layers.iter().map(|l| l.visible).collect::<Vec<_>>())
    };
    // Turning a layer on turns off the rest of its group, unless the action says not to.
    assert_eq!(run(&[(On, 2)], true), (true, vec![false, true, true]));
    assert_eq!(run(&[(On, 2)], false), (true, vec![true, true, true]));
    // A toggle that turns a layer on does the same.
    assert_eq!(run(&[(Toggle, 2)], true), (true, vec![false, true, true]));
    // Turning a layer off leaves its group alone.
    assert_eq!(run(&[(Off, 1)], true), (true, vec![false, false, true]));
    // Each toggle sees the state the changes before it left: twice is no change.
    assert_eq!(run(&[(Toggle, 3), (Toggle, 3)], true), (false, vec![true, false, true]));
    // Later changes win, and groups that aren't layers are skipped.
    assert_eq!(run(&[(On, 1), (On, 2), (Off, 99)], true), (true, vec![false, true, true]));
    // What counts is the end state: on and back off again is no change.
    assert_eq!(run(&[(On, 2), (On, 1), (Off, 99)], true), (false, vec![true, false, true]));
}

#[test]
fn set_layer_actions_change_what_renders() {
    // A red left half on layer Red (on) and a green right half on layer Green (off); the two
    // are a radio-button group.
    let content = "/OC /R BDC 1 0 0 rg 0 0 50 100 re f EMC /OC /G BDC 0 1 0 rg 50 0 50 100 re f EMC";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R 6 0 R] /D << /OFF [6 0 R] /RBGroups [[5 0 R 6 0 R]] >> >> >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /Properties << /R 5 0 R /G 6 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /OCG /Name (Red) >>".into(),
        "<< /Type /OCG /Name (Green) >>".into(),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    let mut s = Session::new();
    let id = s.open("layers.pdf", None, Arc::new(pdf), None).unwrap();
    // The colours at the middle of the left and right halves, as the document draws now.
    let halves = |s: &Session| {
        let doc = s.get(id).unwrap();
        let mut r = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), doc.config.clone());
        let p = r.render(pdfcraft_render::RenderRequest { page: 0, kind: pdfcraft_render::RequestKind::Pixels, scale: 1.0, ..Default::default() });
        let at = |x: usize| {
            let i = (50 * p.width as usize + x) * 4;
            p.rgba[i..i + 3].to_vec()
        };
        (at(25), at(75))
    };
    let (red, green, white) = (vec![255, 0, 0], vec![0, 255, 0], vec![255, 255, 255]);
    assert_eq!(halves(&s), (red.clone(), white.clone()));
    // Showing Green hides Red, its radio-button partner.
    assert!(s.set_layer_state(id, &[(LayerOp::On, (6, 0))], true));
    let visible: Vec<_> = s.get(id).unwrap().info.layers.iter().map(|l| (l.name.as_str(), l.visible)).collect();
    assert_eq!(visible, [("Red", false), ("Green", true)]);
    assert_eq!(halves(&s), (white.clone(), green));
    // Running it again changes nothing.
    assert!(!s.set_layer_state(id, &[(LayerOp::On, (6, 0))], true));
    // Turning Green off brings nothing back on.
    assert!(s.set_layer_state(id, &[(LayerOp::Off, (6, 0))], true));
    assert_eq!(halves(&s), (white.clone(), white));
    assert!(!s.set_layer_state(DocId(999), &[(LayerOp::On, (5, 0))], true));
}

#[test]
fn edits_to_unknown_documents_are_rejected() {
    let (mut s, _) = session_with(1);
    assert_eq!(s.apply(DocId(999), Edit::DeletePages { pages: vec![0] }), Err(EditError::NoDocument));
    assert_eq!(s.undo(DocId(999)), Err(EditError::NoDocument));
    assert!(s.save_bytes(DocId(999)).is_err());
}

#[test]
fn batch_is_one_undo_step_and_all_or_nothing() {
    let (mut s, id) = session_with(3);
    let batch = Edit::Batch {
        label: "Change properties".into(),
        edits: vec![Edit::SetInfo { key: "Title".into(), value: "T".into() }, Edit::SetInfo { key: "Author".into(), value: "A".into() }],
    };
    s.apply(id, batch).unwrap();
    assert_eq!(s.get(id).unwrap().info_value("Author").as_deref(), Some("A"));
    assert_eq!(s.undo(id).unwrap(), "Change properties");
    assert_eq!(s.get(id).unwrap().info_value("Title"), None);
    assert_eq!(s.get(id).unwrap().can_undo(), None, "one step");
    // A failing member rolls back the whole batch.
    let bad = Edit::Batch { label: "x".into(), edits: vec![Edit::RotatePages { pages: vec![0], degrees: 90 }, Edit::DeletePages { pages: vec![9] }] };
    assert!(s.apply(id, bad).is_err());
    assert_eq!(s.get(id).unwrap().info.pages[0].rotation, 0);
}

#[test]
fn insert_blank_between_pages_keeps_both_neighbours() {
    let (mut s, id) = session_with(2);
    s.apply(id, Edit::InsertBlankPage { at: 1, width: 200.0, height: 300.0 }).unwrap();
    assert_eq!(page_texts(&s, id), ["Page 1", "", "Page 2"]);
}

#[test]
fn combine_extract_split_and_insert_from_file() {
    let (mut s, id) = session_with(3);
    let other = Arc::new(fixture(2));
    // Combine: the first file then the second.
    let combined = s.combine(&[("a.pdf".into(), Arc::new(fixture(3))), ("b.pdf".into(), other.clone())]).unwrap();
    let cid = s.open_new("Combined.pdf", combined).unwrap();
    assert_eq!(page_texts(&s, cid), ["Page 1", "Page 2", "Page 3", "Page 1", "Page 2"]);
    assert!(s.get(cid).unwrap().dirty, "a new document starts unsaved");
    assert_eq!(s.get(cid).unwrap().info.outline.len(), 2, "one bookmark per file");
    // Extract pages 3 and 1 into a new document.
    let ex = s.extract(id, &[2, 0]).unwrap();
    let eid = s.open_new("Extract.pdf", ex).unwrap();
    assert_eq!(page_texts(&s, eid), ["Page 3", "Page 1"]);
    // Split every 2 pages.
    let parts = s.split(id, &pdfcraft_organize::SplitBy::PageCount(2)).unwrap();
    assert_eq!(parts.iter().map(|(a, b, _)| (*a, *b)).collect::<Vec<_>>(), [(1, 2), (3, 3)]);
    // Insert pages from a file, undoably, into the open document.
    s.apply(id, Edit::InsertPagesFrom { name: "b.pdf".into(), bytes: other, pages: Some(vec![1]), at: 1, merge_tags: true }).unwrap();
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 2", "Page 2", "Page 3"]);
    assert_eq!(s.undo(id).unwrap(), "Insert pages from b.pdf");
    assert_eq!(page_texts(&s, id).len(), 3);
    // Garbage sources fail cleanly.
    let bad = s.apply(id, Edit::InsertPagesFrom { name: "junk.pdf".into(), bytes: Arc::new(b"nope".to_vec()), pages: None, at: 0, merge_tags: true });
    assert!(matches!(bad, Err(EditError::Source(_))));
}

/// A minimal baseline JPEG (headers only; the data is embedded as is).
fn jpeg_bytes() -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8];
    v.extend_from_slice(&[0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 1, 0x01, 0x2C, 0x01, 0x2C, 0, 0]);
    v.extend_from_slice(&[0xFF, 0xC0, 0, 17, 8, 0, 2, 0, 3, 3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

#[test]
fn mixed_files_convert_and_combine_in_order() {
    let mut s = Session::new();
    let pdf = Arc::new(fixture(2));
    let (kind, same) = s.convert_to_pdf("a.pdf", &pdf).unwrap();
    assert_eq!(kind, SourceKind::Pdf);
    assert!(Arc::ptr_eq(&same, &pdf), "a PDF is passed through untouched");
    let (kind, image) = s.convert_to_pdf("scan.jpg", &Arc::new(jpeg_bytes())).unwrap();
    assert_eq!(kind, SourceKind::Image);
    let (kind, text) = s.convert_to_pdf("notes.txt", &Arc::new(b"hello".to_vec())).unwrap();
    assert_eq!(kind, SourceKind::Text);
    let combined = s.combine_ranges(&[("notes".into(), text, None), ("a".into(), pdf, Some("2".into())), ("scan".into(), image, None)]).unwrap();
    let id = s.open_new("Combined.pdf", combined).unwrap();
    assert_eq!(page_texts(&s, id), ["hello", "Page 2", ""]);
    assert_eq!(outline_titles(&s.get(id).unwrap().info.outline), ["notes→1", "a→2", "scan→3"]);
}

#[test]
fn files_that_cannot_be_converted_are_refused_clearly() {
    let s = Session::new();
    let err = s.convert_to_pdf("report.docx", &Arc::new(b"PK\x03\x04".to_vec())).unwrap_err();
    assert_eq!(err, EditError::Source("report.docx: this file type can't be converted; use a PDF, an image or a .txt file".into()));
    assert!(matches!(s.convert_to_pdf("empty", &Arc::new(Vec::new())), Err(EditError::Source(_))));
    // Damaged inputs of a known type fail with an error, not a panic.
    assert!(s.convert_to_pdf("bad.png", &Arc::new(b"\x89PNG\r\n\x1a\nnope".to_vec())).is_err());
    assert!(s.convert_to_pdf("bad.jpg", &Arc::new(vec![0xFF, 0xD8, 0xFF])).is_err());
    assert!(s.convert_to_pdf("bad.pdf", &Arc::new(b"%PDF-1.7 nope".to_vec())).is_err());
    let err = s.convert_to_pdf("locked.pdf", &protected("pw", "o", -1)).unwrap_err();
    assert_eq!(err, EditError::Source("locked.pdf: it is password-protected".into()));
}

fn protected(user: &str, owner: &str, permissions: i32) -> Arc<Vec<u8>> {
    let mut doc = pdfcraft_cos::Document::open(Arc::new(fixture(2))).unwrap();
    doc.set_encryption(&pdfcraft_cos::NewEncryption {
        algorithm: pdfcraft_cos::Algorithm::Aes256,
        user_password: user,
        owner_password: owner,
        permissions,
        encrypt_metadata: true,
        seed: [7; 32],
    })
    .unwrap();
    Arc::new(pdfcraft_cos::write_full(&doc, &Default::default()).unwrap())
}

#[test]
fn encrypted_documents_open_edit_and_save_encrypted() {
    let bytes = protected("pw", "owner", -1);
    let mut s = Session::new();
    assert!(s.open("x.pdf", None, bytes.clone(), None).is_err(), "needs a password");
    let id = s.open("x.pdf", None, bytes.clone(), Some("pw")).unwrap();
    assert!(s.get(id).unwrap().editable(), "{:?}", s.get(id).unwrap().read_only_reason);
    assert_eq!(s.get(id).unwrap().security_summary().unwrap().method, "AES, 256-bit");
    assert!(s.get(id).unwrap().info.encrypted, "reported as encrypted");
    s.apply(id, Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
    let saved = s.save_bytes(id).unwrap();
    assert_eq!(&saved[..bytes.len()], &bytes[..], "incremental");
    assert!(pdfcraft_cos::Document::open(saved.clone()).is_err(), "still protected after saving");
    let mut s2 = Session::new();
    let id2 = s2.open("x.pdf", None, saved, Some("pw")).unwrap();
    assert_eq!(s2.get(id2).unwrap().info.pages[0].rotation, 90);
}

#[test]
fn restricted_documents_refuse_changes_unless_opened_by_the_owner() {
    let bytes = protected("", "owner", 0b0100); // print only, no password to open
    let mut s = Session::new();
    let id = s.open("r.pdf", None, bytes.clone(), None).unwrap();
    let d = s.get(id).unwrap();
    assert!(!d.allows_assembly() && !d.allows_modification());
    let summary = d.security_summary().unwrap();
    assert!(!summary.owner && summary.permissions.print() && !summary.permissions.copy());
    assert_eq!(s.apply(id, Edit::DeletePages { pages: vec![0] }), Err(EditError::NotPermitted("page changes")));
    assert_eq!(s.apply(id, Edit::SetInfo { key: "Title".into(), value: "x".into() }), Err(EditError::NotPermitted("changes to the document")));
    assert!(matches!(s.extract(id, &[0]), Err(EditError::NotPermitted(_))));
    assert!(!s.get(id).unwrap().dirty);
    // The owner password lifts the restrictions.
    let oid = s.open("r.pdf", None, bytes, Some("owner")).unwrap();
    assert!(s.get(oid).unwrap().allows_assembly());
    s.apply(oid, Edit::DeletePages { pages: vec![0] }).unwrap();
}

#[test]
fn invalid_encryption_permissions_never_fall_back_to_an_unrestricted_viewer() {
    let original = protected("user", "owner", 0);
    let mut cos = pdfcraft_cos::Document::open_with_password(original.clone(), Some("owner")).unwrap();
    let encryption = cos.trailer().reference(b"Encrypt").unwrap();
    cos.update_dict(encryption, |dict| dict.set(b"P".to_vec(), pdfcraft_cos::Object::Int(-4))).unwrap();
    let tampered = Arc::new(pdfcraft_cos::write_incremental(&cos, &Default::default()).unwrap());
    let mut session = Session::new();
    let original_id = session.open("original.pdf", None, original, Some("user")).unwrap();
    assert!(!session.get(original_id).unwrap().allows_printing());
    for password in ["user", "owner"] {
        let error = session.open("tampered.pdf", None, tampered.clone(), Some(password)).unwrap_err();
        assert!(error.to_string().contains("/Perms"), "{error}");
        assert_eq!(session.docs().len(), 1, "a rejected document must not enter the session");
        assert!(!session.get(original_id).unwrap().allows_printing());
    }
}

#[test]
fn combining_protected_files_is_refused_clearly() {
    let s = Session::new();
    let err = s.combine(&[("a.pdf".into(), protected("pw", "o", -1)), ("b.pdf".into(), Arc::new(fixture(1)))]).unwrap_err();
    assert_eq!(err, EditError::Source("a.pdf: it is password-protected".into()));
    let err = s.combine(&[("c.pdf".into(), protected("", "o", 0b0100))]).unwrap_err();
    assert!(matches!(err, EditError::Source(m) if m.contains("don't allow copying pages")));
}

#[test]
fn owner_password_of_older_revisions_opens_the_viewer_too() {
    for alg in [pdfcraft_cos::Algorithm::Rc4_128, pdfcraft_cos::Algorithm::Aes128] {
        let mut doc = pdfcraft_cos::Document::open(Arc::new(fixture(1))).unwrap();
        doc.set_encryption(&pdfcraft_cos::NewEncryption {
            algorithm: alg,
            user_password: "u",
            owner_password: "o",
            permissions: 0,
            encrypt_metadata: true,
            seed: [1; 32],
        })
        .unwrap();
        let bytes = Arc::new(pdfcraft_cos::write_full(&doc, &Default::default()).unwrap());
        let mut s = Session::new();
        let id = s.open("x.pdf", None, bytes, Some("o")).unwrap_or_else(|e| panic!("{alg:?}: {e}"));
        let d = s.get(id).unwrap();
        assert!(d.security_summary().unwrap().owner && d.allows_modification(), "{alg:?}");
        assert_eq!(page_texts(&s, id), ["Page 1"], "{alg:?}: the renderer reads it");
    }
}

#[test]
fn autosave_snapshots_only_changed_documents() {
    let (mut s, id) = session_with(2);
    assert!(s.autosave_snapshots().is_empty(), "clean documents are not snapshotted");
    s.apply(id, Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
    let snaps = s.autosave_snapshots();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].bytes, s.get(id).unwrap().bytes, "the working file");
    assert!(s.autosave_snapshots().is_empty(), "nothing new since the last snapshot");
    s.undo(id).unwrap();
    assert_eq!(s.autosave_snapshots().len(), 1, "undo is a change too");
    let saved = s.save_bytes(id).unwrap();
    s.mark_saved(id, saved, None).unwrap();
    assert!(s.autosave_snapshots().is_empty(), "saved documents need no recovery");
}

#[test]
fn failed_autosave_can_retry_without_another_edit() {
    let (mut session, id) = session_with(2);
    session.apply(id, Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
    let first = session.autosave_snapshots();
    assert!(session.autosave_snapshots().is_empty());
    session.retry_autosave(id);
    let retry = session.autosave_snapshots();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].bytes, first[0].bytes);
    assert!(session.autosave_snapshots().is_empty());
}

#[test]
fn recovered_documents_reopen_unsaved_at_their_original_path() {
    let (mut s, id) = session_with(2);
    s.apply(id, Edit::DeletePages { pages: vec![1] }).unwrap();
    let snap = s.autosave_snapshots().remove(0);
    // A new session after a crash.
    let mut s2 = Session::new();
    let rid = s2.open(snap.name.clone(), None, snap.bytes.clone(), None).unwrap();
    s2.mark_recovered(rid, Some("/docs/report.pdf".into()));
    let d = s2.get(rid).unwrap();
    assert!(d.dirty);
    assert_eq!(d.path.as_deref(), Some("/docs/report.pdf"));
    assert_eq!(d.info.pages.len(), 1, "the edit survived");
    assert!(s2.autosave_snapshots().is_empty(), "already in the recovery store");
}

fn outline_titles(items: &[pdfcraft_render::OutlineItem]) -> Vec<String> {
    items
        .iter()
        .map(|o| {
            if o.children.is_empty() {
                format!("{}→{}", o.title, o.page.map_or(0, |p| p + 1))
            } else {
                format!("{}→{}[{}]", o.title, o.page.map_or(0, |p| p + 1), outline_titles(&o.children).join(","))
            }
        })
        .collect()
}

#[test]
fn bookmark_edits_show_in_the_viewer_undo_and_save() {
    let (mut s, id) = session_with(3);
    let titles = |s: &Session| outline_titles(&s.get(id).unwrap().info.outline);
    s.apply(id, Edit::AddBookmark { parent: vec![], index: 0, title: "Start".into(), page: 0 }).unwrap();
    s.apply(id, Edit::AddBookmark { parent: vec![], index: 1, title: "End".into(), page: 2 }).unwrap();
    s.apply(id, Edit::AddBookmark { parent: vec![1], index: 0, title: "Détail".into(), page: 1 }).unwrap();
    // The inspector (an independent parser) sees the same tree and destinations.
    assert_eq!(titles(&s), ["Start→1", "End→3[Détail→2]"]);
    s.apply(id, Edit::MoveBookmark { from: vec![1, 0], to_parent: vec![], index: 0 }).unwrap();
    s.apply(id, Edit::RenameBookmark { path: vec![2], title: "Finish".into() }).unwrap();
    s.apply(id, Edit::SetBookmarkPage { path: vec![1], page: 1 }).unwrap();
    assert_eq!(titles(&s), ["Détail→2", "Start→2", "Finish→3"]);
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Set bookmark destination"));
    s.undo(id).unwrap();
    assert_eq!(titles(&s), ["Détail→2", "Start→1", "Finish→3"]);
    s.apply(id, Edit::DeleteBookmark { path: vec![0] }).unwrap();
    assert!(matches!(s.apply(id, Edit::DeleteBookmark { path: vec![7] }), Err(EditError::Bookmark(_))));

    let saved = s.save_bytes(id).unwrap();
    let mut again = Session::new();
    let id2 = again.open("again.pdf", None, saved, None).unwrap();
    assert_eq!(outline_titles(&again.get(id2).unwrap().info.outline), ["Start→1", "Finish→3"]);
}

#[test]
fn number_pages_shows_in_the_viewer_and_undoes() {
    let (mut s, id) = session_with(5);
    let labels = |s: &Session| s.get(id).unwrap().info.pages.iter().map(|p| p.label.clone()).collect::<Vec<_>>();
    use pdfcraft_organize::LabelStyle;
    s.apply(id, Edit::NumberPages { from: 0, to: 1, style: LabelStyle::LowerRoman, prefix: String::new(), first: 1 }).unwrap();
    s.apply(id, Edit::NumberPages { from: 2, to: 4, style: LabelStyle::Decimal, prefix: "§".into(), first: 10 }).unwrap();
    // The inspector (lopdf-based, independent) formats them the same way.
    assert_eq!(labels(&s), ["i", "ii", "§10", "§11", "§12"]);
    s.undo(id).unwrap();
    assert_eq!(labels(&s), ["i", "ii", "3", "4", "5"]);
    assert!(s.apply(id, Edit::NumberPages { from: 3, to: 9, style: LabelStyle::Decimal, prefix: String::new(), first: 1 }).is_err());
}

/// RGBA of the pixel at PDF point (x, y) on `page`, rendered at 1 px/pt (page height 300).
fn pixel(s: &Session, id: DocId, page: usize, x: u32, y: u32) -> [u8; 4] {
    let doc = s.get(id).unwrap();
    let mut r = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), doc.config.clone());
    let out = r.render(pdfcraft_render::RenderRequest { page, scale: 1.0, ..Default::default() });
    assert!(out.error.is_none(), "{:?}", out.error);
    let i = (((300 - y) * out.width + x) * 4) as usize;
    out.rgba[i..i + 4].try_into().unwrap()
}

fn rect_comment(page: usize, rect: [f64; 4]) -> Edit {
    Edit::AddAnnotation(NewAnnotation {
        page,
        shape: Shape::Rectangle { rect },
        style: Style { color: [1.0, 0.0, 0.0], opacity: 1.0, width: 2.0, fill: Some([1.0, 0.0, 0.0]) },
        contents: "Look here".into(),
        author: "Reviewer".into(),
    })
}

#[test]
fn comments_are_added_drawn_threaded_and_undone() {
    let (mut s, id) = session_with(2);
    assert_eq!(pixel(&s, id, 1, 100, 100), [255, 255, 255, 255]);
    s.apply(id, rect_comment(1, [80.0, 80.0, 120.0, 120.0])).unwrap();
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Add rectangle"));
    let px = pixel(&s, id, 1, 100, 100);
    assert!(px[0] > 200 && px[1] < 40 && px[2] < 40, "the rectangle is drawn: {px:?}");
    let a = &s.get(id).unwrap().info.annotations;
    assert_eq!(a.len(), 1);
    assert_eq!((a[0].page, a[0].index, a[0].author.as_deref(), a[0].contents.as_deref()), (1, 0, Some("Reviewer"), Some("Look here")));
    assert_eq!(a[0].modified.as_deref().map(|m| m.contains("2023")), Some(true), "dated by the session clock");
    let nm = a[0].name.clone().expect("has an /NM");
    assert_eq!(nm.len(), 36);

    s.apply(id, Edit::ReplyToAnnotation { page: 1, index: 0, text: "Done".into(), author: "Ada".into() }).unwrap();
    s.apply(id, Edit::SetAnnotationStatus { page: 1, index: 0, state: ReviewState::Completed, author: "Ada".into() }).unwrap();
    let a = &s.get(id).unwrap().info.annotations;
    assert_eq!(a.len(), 3);
    assert!(a.iter().filter(|r| r.in_reply_to.as_deref() == Some(nm.as_str())).count() == 2);
    assert!(a.iter().any(|r| r.state.as_deref() == Some("Completed")));
    assert_ne!(a[1].name, a[2].name, "every comment gets its own id");

    s.apply(id, Edit::MoveAnnotation { page: 1, index: 0, dx: 50.0, dy: 0.0 }).unwrap();
    assert_eq!(pixel(&s, id, 1, 100, 100), [255, 255, 255, 255]);
    let px = pixel(&s, id, 1, 150, 100);
    assert!(px[0] > 200 && px[1] < 40, "moved: {px:?}");

    s.apply(id, Edit::DeleteAnnotation { page: 1, index: 0 }).unwrap();
    assert!(s.get(id).unwrap().info.annotations.is_empty(), "replies go with their parent");
    for _ in 0..4 {
        s.undo(id).unwrap();
    }
    assert_eq!(s.get(id).unwrap().info.annotations.len(), 1);
    let px = pixel(&s, id, 1, 100, 100);
    assert!(px[0] > 200 && px[1] < 40, "back where it was: {px:?}");
}

#[test]
fn highlight_multiplies_over_text() {
    let (mut s, id) = session_with(1);
    // "Page 1" is drawn at 20,150 in 24 pt Helvetica.
    let quad = [18.0, 172.0, 100.0, 172.0, 18.0, 145.0, 100.0, 145.0];
    s.apply(
        id,
        Edit::AddAnnotation(NewAnnotation {
            page: 0,
            shape: Shape::TextMarkup { kind: Markup::Highlight, quads: vec![quad] },
            style: Style { color: [1.0, 1.0, 0.0], ..Style::default() },
            contents: String::new(),
            author: String::new(),
        }),
    )
    .unwrap();
    let a = &s.get(id).unwrap().info.annotations[0];
    assert_eq!(a.subtype, "Highlight");
    assert_eq!(a.quads, vec![quad.map(|v| v as f32)]);
    // Background inside the quad turns yellow; text stays dark (multiply).
    let bg = pixel(&s, id, 0, 19, 170);
    assert!(bg[0] > 240 && bg[1] > 240 && bg[2] < 30, "{bg:?}");
}

#[test]
fn comment_permission_is_enforced() {
    let mut cos = pdfcraft_cos::Document::open(Arc::new(fixture(1))).unwrap();
    // Owner "own", empty user password, everything allowed except commenting (bit 6).
    let params = pdfcraft_cos::NewEncryption {
        algorithm: pdfcraft_cos::Algorithm::Aes256,
        user_password: "",
        owner_password: "own",
        permissions: !(1 << 5),
        encrypt_metadata: true,
        seed: [7; 32],
    };
    cos.set_encryption(&params).unwrap();
    let bytes = pdfcraft_cos::write_full(&cos, &pdfcraft_cos::SaveOptions::default()).unwrap();
    let mut s = Session::new();
    let id = s.open("locked.pdf", None, Arc::new(bytes), None).unwrap();
    let err = s.apply(id, rect_comment(0, [10.0, 10.0, 50.0, 50.0])).unwrap_err();
    assert_eq!(err, EditError::NotPermitted("comments"));
}

#[test]
fn saving_a_password_protected_document_rebases_on_it() {
    let bytes = protected("pw", "owner", -1);
    let mut s = Session::new();
    let id = s.open("x.pdf", None, bytes, Some("pw")).unwrap();
    s.apply(id, Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
    let saved = s.save_bytes(id).unwrap();
    s.mark_saved(id, saved.clone(), None).expect("rebases on the encrypted file");
    assert!(!s.get(id).unwrap().dirty);
    s.apply(id, Edit::RotatePages { pages: vec![1], degrees: 90 }).unwrap();
    let again = s.save_bytes(id).unwrap();
    assert_eq!(&again[..saved.len()], &saved[..], "the next save appends");
}

fn protection(open: Option<&str>, perms: Option<&str>) -> Protection {
    Protection { open_password: open.map(Into::into), permissions_password: perms.map(Into::into), ..Protection::default() }
}

#[test]
fn protect_with_an_open_password_then_save_reopen_and_undo() {
    let (mut s, id) = session_with(2);
    s.apply(id, Edit::Protect(protection(Some("secret"), None))).unwrap();
    let d = s.get(id).unwrap();
    assert!(d.info.encrypted, "the working file is encrypted: {:?}", d.info.warnings);
    assert_eq!(d.security_summary().unwrap().method, "AES, 256-bit");
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 2"], "still viewable in this session");
    let saved = s.save_bytes(id).unwrap();
    assert!(pdfcraft_cos::Document::open(saved.clone()).is_err(), "needs the password");
    s.mark_saved(id, saved.clone(), None).unwrap();
    // Further edits keep working and saving stays encrypted.
    s.apply(id, Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
    let again = s.save_bytes(id).unwrap();
    let mut s2 = Session::new();
    assert!(s2.open("p.pdf", None, again.clone(), None).is_err());
    let id2 = s2.open("p.pdf", None, again, Some("secret")).unwrap();
    assert_eq!(s2.get(id2).unwrap().info.pages[0].rotation, 90);
    // Nothing is restricted without a permissions password: the opener may remove security.
    assert!(s2.get(id2).unwrap().allows_security_change());
    s2.apply(id2, Edit::RemoveProtection).unwrap();
    let plain = s2.save_bytes(id2).unwrap();
    assert!(pdfcraft_cos::Document::open(plain).is_ok());
}

#[test]
fn permissions_password_restricts_others_but_not_this_session() {
    let (mut s, id) = session_with(1);
    let p = Protection { printing: Printing::Low, changes: Changes::CommentFillSign, copy: false, ..protection(None, Some("boss")) };
    s.apply(id, Edit::Protect(p.clone())).unwrap();
    assert!(s.get(id).unwrap().allows_modification(), "the author keeps full rights");
    let saved = s.save_bytes(id).unwrap();
    s.mark_saved(id, saved.clone(), None).unwrap();
    assert!(s.get(id).unwrap().allows_modification(), "…also after saving (re-opened as owner)");
    // Someone else opens it without a password: restricted as chosen.
    let mut s2 = Session::new();
    let id2 = s2.open("r.pdf", None, saved.clone(), None).unwrap();
    let perm = s2.get(id2).unwrap().permissions().unwrap();
    assert!(perm.print() && !perm.print_high_quality() && perm.annotate() && perm.fill_forms() && !perm.copy() && !perm.modify());
    assert!(perm.extract_for_accessibility());
    assert!(!s2.get(id2).unwrap().allows_security_change());
    assert_eq!(s2.apply(id2, Edit::RemoveProtection), Err(EditError::NotPermitted("changing security")));
    assert!(s2.get(id2).unwrap().allows_annotation(), "commenting was allowed");
    // The owner password lifts everything.
    let id3 = s2.open("r.pdf", None, saved, Some("boss")).unwrap();
    assert!(s2.get(id3).unwrap().allows_security_change());
}

#[test]
fn combine_source_check_says_why_a_file_cannot_be_combined() {
    let (mut s, id) = session_with(1);
    let plain = s.get(id).unwrap().bytes.clone();
    assert_eq!(crate::combine_source_check(&plain, None), Ok(()));
    // Page assembly withheld (Changes::None) without a password to open it.
    let p = Protection { changes: Changes::None, ..protection(None, Some("boss")) };
    s.apply(id, Edit::Protect(p)).unwrap();
    let locked = s.save_bytes(id).unwrap();
    assert_eq!(crate::combine_source_check(&locked, None), Err(crate::SourceProblem::NotPermitted));
    let (mut s, id) = session_with(1);
    s.apply(id, Edit::Protect(protection(Some("pw"), Some("owner")))).unwrap();
    let secret = s.save_bytes(id).unwrap();
    assert_eq!(crate::combine_source_check(&secret, None), Err(crate::SourceProblem::Password));
    assert_eq!(crate::combine_source_check(&secret, Some("nope")), Err(crate::SourceProblem::WrongPassword));
    // Its open password reads it, but its permissions withhold assembling pages: the owner's
    // password allows it.
    assert_eq!(crate::combine_source_check(&secret, Some("pw")), Err(crate::SourceProblem::NotPermitted));
    assert_eq!(crate::combine_source_check(&secret, Some("owner")), Ok(()));
    // The permissions password lifts the restriction on copying pages.
    assert_eq!(crate::combine_source_check(&locked, Some("boss")), Ok(()));
    // Combining with the passwords: the result opens without any.
    let s = Session::new();
    let sources = vec![("secret".to_string(), secret, None), ("locked".to_string(), locked, None)];
    assert!(s.combine_ranges(&sources).is_err(), "no passwords, no combining");
    let out = s.combine_unlocked(&sources, &[Some("owner"), Some("boss")]).unwrap();
    let combined = pdfcraft_cos::Document::open(out).unwrap();
    assert!(combined.permissions().is_none(), "the combined file is not encrypted");
    assert_eq!(pdfcraft_organize::page_count(&combined).unwrap(), 2);
    assert!(matches!(crate::combine_source_check(&Arc::new(b"not a pdf".to_vec()), None), Err(crate::SourceProblem::Unreadable(_))));
}

#[test]
fn protection_is_validated_undoable_and_never_logged() {
    let (mut s, id) = session_with(1);
    assert!(matches!(s.apply(id, Edit::Protect(protection(None, None))), Err(EditError::Protection(_))));
    assert!(matches!(s.apply(id, Edit::Protect(protection(Some("same"), Some("same")))), Err(EditError::Protection(_))));
    let rc4 = Protection { algorithm: pdfcraft_cos::Algorithm::Rc4_128, ..protection(Some("pässword"), None) };
    assert!(matches!(s.apply(id, Edit::Protect(rc4)), Err(EditError::Protection(_))));
    assert!(!format!("{:?}", Edit::Protect(protection(Some("hunter2"), Some("x")))).contains("hunter2"));
    s.apply(id, Edit::Protect(protection(Some("pw"), Some("owner")))).unwrap();
    assert!(s.get(id).unwrap().info.encrypted);
    s.undo(id).unwrap();
    assert!(!s.get(id).unwrap().info.encrypted, "undo removes the pending protection");
    assert!(s.get(id).unwrap().security_summary().is_none());
    s.redo(id).unwrap();
    assert!(s.get(id).unwrap().info.encrypted);
    assert_eq!(page_texts(&s, id), ["Page 1"]);
}

/// A one-page form: a text field and a check box (with appearance states).
fn form_fixture() -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 6 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [20 250 180 270] /P 3 0 R /MK << /BC [0 0 0] >> >>",
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (ok) /V /Off /AS /Off /Rect [20 200 35 215] /P 3 0 R /AP << /N << /Yes 7 0 R /Off 7 0 R >> >> >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        "<< /Length 0 >>\nstream\n\nendstream",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

#[test]
fn filling_a_form_shows_saves_and_undoes() {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("form.pdf", None, Arc::new(form_fixture()), None).unwrap();
    assert_eq!(s.get(id).unwrap().form.len(), 2);
    s.apply(id, Edit::SetFieldValue { name: "name".into(), value: FieldValue::Text("Ada Lovelace".into()) }).unwrap();
    s.apply(id, Edit::SetFieldValue { name: "ok".into(), value: FieldValue::Check(true) }).unwrap();
    let d = s.get(id).unwrap();
    assert_eq!(d.can_undo(), Some("Fill in ok"));
    assert_eq!(d.form[0].value, ["Ada Lovelace"]);
    assert_eq!(d.form[1].value, ["Yes"]);
    assert_eq!(page_texts(&s, id), ["Ada Lovelace"], "the new appearance shows the text");
    let err = s.apply(id, Edit::SetFieldValue { name: "missing".into(), value: FieldValue::Text("x".into()) }).unwrap_err();
    assert_eq!(err.to_string(), "there is no field named \"missing\"");
    s.apply(id, Edit::ResetForm { names: None }).unwrap();
    assert!(s.get(id).unwrap().form.iter().all(|f| f.value.is_empty()));
    s.undo(id).unwrap();
    assert_eq!(s.get(id).unwrap().form[0].value, ["Ada Lovelace"]);
    let saved = s.save_bytes(id).unwrap();
    let mut s2 = Session::new();
    let id2 = s2.open("f.pdf", None, saved, None).unwrap();
    assert_eq!(s2.get(id2).unwrap().form[1].value, ["Yes"]);
}

#[test]
fn comment_edits_refresh_the_list_exactly_as_a_full_inspection_would() {
    let (mut s, id) = session_with(3);
    let add = |page: usize, shape: Shape, contents: &str| {
        Edit::AddAnnotation(NewAnnotation {
            page,
            shape,
            style: Style::default_for(&Shape::Rectangle { rect: [0.0; 4] }),
            contents: contents.into(),
            author: "Ada".into(),
        })
    };
    s.apply(id, add(0, Shape::Rectangle { rect: [10.0, 10.0, 60.0, 60.0] }, "box")).unwrap();
    s.apply(id, add(1, Shape::TextMarkup { kind: Markup::Highlight, quads: vec![[20.0, 170.0, 90.0, 170.0, 20.0, 150.0, 90.0, 150.0]] }, ""))
        .unwrap();
    s.apply(id, add(1, Shape::TextBox { rect: [10.0, 200.0, 150.0, 240.0], font_size: 11.0 }, "Text box")).unwrap();
    s.apply(id, add(2, Shape::Note { at: [100.0, 280.0], icon: NoteIcon::Comment }, "Sticky")).unwrap();
    s.apply(id, Edit::ReplyToAnnotation { page: 2, index: 0, text: "Reply".into(), author: "Bob".into() }).unwrap();
    s.apply(id, Edit::SetAnnotationStatus { page: 2, index: 0, state: ReviewState::Accepted, author: "Bob".into() }).unwrap();
    s.apply(id, Edit::MarkAnnotation { page: 2, index: 0, marked: true, author: "Bob".into() }).unwrap();
    s.apply(id, Edit::LockAnnotation { page: 1, index: 1, locked: true }).unwrap();
    s.apply(id, Edit::MoveAnnotation { page: 0, index: 0, dx: 5.0, dy: 5.0 }).unwrap();
    s.apply(id, Edit::DeleteAnnotation { page: 1, index: 0 }).unwrap();
    s.undo(id).unwrap();
    let d = s.get(id).unwrap();
    let full = pdfcraft_render::inspect(d.bytes.clone(), None).unwrap();
    assert_eq!(format!("{:?}", d.info.annotations), format!("{:?}", full.annotations));
    assert_eq!(d.info.file_size, full.file_size);
    assert_eq!(d.info.annotations.len(), 7);
    assert!(d.info.annotations.iter().any(|a| a.locked));
}

#[test]
fn form_edits_refresh_field_values_without_a_full_inspection() {
    let mut s = Session::new();
    let id = s.open("form.pdf", None, Arc::new(form_fixture()), None).unwrap();
    s.apply(id, Edit::SetFieldValue { name: "name".into(), value: FieldValue::Text("Ada".into()) }).unwrap();
    s.apply(id, Edit::SetFieldValue { name: "ok".into(), value: FieldValue::Check(true) }).unwrap();
    let d = s.get(id).unwrap();
    let full = pdfcraft_render::inspect(d.bytes.clone(), None).unwrap();
    let values = |fs: &[pdfcraft_render::Field]| fs.iter().map(|f| (f.name.clone(), f.value.clone())).collect::<Vec<_>>();
    assert_eq!(values(&d.info.fields), values(&full.fields));
    assert_eq!(values(&d.info.fields), [("name".to_string(), Some("Ada".to_string())), ("ok".to_string(), Some("Yes".to_string()))]);
}

#[test]
fn preparing_a_form_adds_renames_and_deletes_fields() {
    let (mut s, id) = session_with(1);
    assert!(s.get(id).unwrap().form.is_empty());
    let add = |kind: NewField, y: f64| Edit::AddField { page: 0, rect: [20.0, y, 180.0, y + 22.0], kind, name: None };
    s.apply(id, add(NewField::Text { multiline: false }, 250.0)).unwrap();
    s.apply(id, add(NewField::CheckBox, 200.0)).unwrap();
    s.apply(id, add(NewField::Text { multiline: false }, 150.0)).unwrap();
    let names = |s: &Session| s.get(id).unwrap().form.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&s), ["Text1", "Check Box1", "Text2"]);
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Add field"));
    let props = FieldProps { name: Some("email".into()), tooltip: Some("Your e-mail".into()), required: Some(true), ..Default::default() };
    s.apply(id, Edit::SetFieldProps { name: "Text2".into(), props: Box::new(props) }).unwrap();
    s.apply(id, Edit::DeleteField { name: "Check Box1".into() }).unwrap();
    assert_eq!(names(&s), ["Text1", "email"]);
    s.apply(id, Edit::SetFieldValue { name: "email".into(), value: FieldValue::Text("ada@example.org".into()) }).unwrap();
    let saved = s.save_bytes(id).unwrap();
    let mut s2 = Session::new();
    let id2 = s2.open("f.pdf", None, saved, None).unwrap();
    let d = s2.get(id2).unwrap();
    assert_eq!(d.form.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["Text1", "email"]);
    assert_eq!(d.form[1].value, ["ada@example.org"]);
    assert!(page_texts(&s2, id2).iter().any(|t| t.contains("ada@example.org")));
    s.undo(id).unwrap();
    s.undo(id).unwrap();
    assert_eq!(names(&s), ["Text1", "Check Box1", "email"]);
}

#[test]
fn redaction_marks_apply_for_good_and_undo() {
    let (mut s, id) = session_with(2);
    // "Page 1" at 24 pt from x 20: the "1" starts near x 82.7 (Helvetica widths).
    let shape =
        Shape::Redact { quads: vec![pdfcraft_annot::rect_quad([80.0, 140.0, 100.0, 180.0])], overlay: String::new(), look: Default::default() };
    let mark =
        Edit::AddAnnotation(NewAnnotation { page: 0, style: Style::default_for(&shape), shape, contents: String::new(), author: "Ada".into() });
    s.apply(id, mark).unwrap();
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Add redaction mark"));
    assert_eq!(s.get(id).unwrap().redaction_marks(), 1);
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 2"], "marking alone changes nothing");
    s.apply(id, Edit::ApplyRedactions { pages: None }).unwrap();
    let d = s.get(id).unwrap();
    assert_eq!((d.can_undo(), d.redaction_marks()), (Some("Apply redactions"), 0));
    // An independent extractor (the renderer's) no longer finds the "1".
    assert_eq!(page_texts(&s, id), ["Page", "Page 2"]);
    let saved = s.save_bytes(id).unwrap();
    assert!(!saved.windows(8).any(|w| w == b"(Page 1)"), "a full rewrite: the old revision is gone");
    assert!(saved.windows(8).any(|w| w == b"(Page 2)"));
    let mut s2 = Session::new();
    let id2 = s2.open("r.pdf", None, saved, None).unwrap();
    assert_eq!(page_texts(&s2, id2), ["Page", "Page 2"]);
    s.undo(id).unwrap();
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 2"]);
    assert_eq!(s.apply(id, Edit::ClearRedactions).map(|_| s.get(id).unwrap().redaction_marks()), Ok(0));
    assert!(matches!(s.apply(id, Edit::ApplyRedactions { pages: None }), Err(EditError::Redact(_))));
}

#[test]
fn printing_lays_out_sheets_that_render() {
    let (s, id) = session_with(5);
    let settings = print::Settings {
        pages: print::select_pages(5, Some("2-5"), &[], print::Subset::All, false).unwrap(),
        layout: print::Layout::multiple(2),
        ..Default::default()
    };
    let bytes = s.print_pdf(id, &settings).unwrap();
    let mut s2 = Session::new();
    let id2 = s2.open("print.pdf", None, Arc::new(bytes), None).unwrap();
    assert_eq!(s2.get(id2).unwrap().info.pages.len(), 2, "four pages, two per sheet");
    let texts = page_texts(&s2, id2);
    assert!(texts[0].contains("Page 2") && texts[0].contains("Page 3") && texts[1].contains("Page 5"), "{texts:?}");
    // Printing honours the permissions.
    let p =
        Protection { open_password: Some("pw".into()), permissions_password: Some("owner".into()), printing: Printing::None, ..Default::default() };
    let (mut s3, id3) = session_with(1);
    s3.apply(id3, Edit::Protect(p)).unwrap();
    let saved = s3.save_bytes(id3).unwrap();
    let mut s4 = Session::new();
    let id4 = s4.open("locked.pdf", None, saved, Some("pw")).unwrap();
    assert_eq!(s4.print_pdf(id4, &print::Settings { pages: vec![0], ..Default::default() }), Err(EditError::NotPermitted("printing")));
}

#[test]
fn printing_as_an_image_leaves_one_picture_per_sheet() {
    let (s, id) = session_with(3);
    let settings = print::Settings { pages: vec![0, 1, 2], layout: print::Layout::Size(print::SizeMode::Actual), ..Default::default() };
    let plain = s.print_pdf(id, &settings).unwrap();
    let (bytes, report) = s.print_pdf_as_image(id, &settings, 100.0).unwrap();
    assert_eq!(report.len(), 3);
    let mut s2 = Session::new();
    let id2 = s2.open("print.pdf", None, Arc::new(bytes.clone()), None).unwrap();
    let mut s3 = Session::new();
    let id3 = s3.open("plain.pdf", None, Arc::new(plain), None).unwrap();
    let (img, vec) = (&s2.get(id2).unwrap().info.pages, &s3.get(id3).unwrap().info.pages);
    assert_eq!(img.len(), 3);
    for (a, b) in img.iter().zip(vec.iter()) {
        assert_eq!((a.width, a.height), (b.width, b.height));
    }
    // Letter sheets at 100 dpi.
    for r in &report {
        assert_eq!((r.width, r.height, r.images), (850, 1100, 1), "{r:?}");
        assert!((r.resolution - 100.0).abs() < 1e-9);
    }
    // No text is left to extract.
    assert!(page_texts(&s2, id2).iter().all(|t| t.trim().is_empty()), "{:?}", page_texts(&s2, id2));
    assert!(page_texts(&s3, id3)[1].contains("Page 2"));
    let cos = pdfcraft_cos::Document::open(Arc::new(bytes)).unwrap();
    for p in pdfcraft_model::pages(&cos) {
        let c = cos.resolve(p.dict.get(b"Contents").unwrap());
        let pdfcraft_cos::Object::Stream(st) = &*c else { panic!("{c:?}") };
        let text = String::from_utf8_lossy(&st.decoded().unwrap()).into_owned();
        assert!(!text.contains("BT") && !text.contains("Tf"), "{text}");
    }
    for bad in [0.0, 35.0, 1201.0, f64::NAN] {
        assert!(matches!(s.print_pdf_as_image(id, &settings, bad), Err(EditError::Invalid(_))), "{bad}");
    }
}

#[test]
fn crop_and_duplicate_pages_show_in_the_viewer() {
    let (mut s, id) = session_with(2);
    s.apply(id, Edit::SetPageBox { pages: vec![0], which: PageBox::Crop, spec: BoxSpec::Margins([10.0, 20.0, 30.0, 40.0]) }).unwrap();
    let d = s.get(id).unwrap();
    assert_eq!(d.can_undo(), Some("Crop page"));
    assert_eq!(d.info.pages[0].crop, [10.0, 20.0, 170.0, 260.0]);
    assert_eq!((d.info.pages[0].width, d.info.pages[0].height), (160.0, 240.0), "the viewer shows the cropped size");
    assert_eq!(d.page_boxes()[1][1], [0.0, 0.0, 200.0, 300.0]);
    s.apply(id, Edit::DuplicatePages { pages: vec![1] }).unwrap();
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 2", "Page 2"]);
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Duplicate page"));
}

#[test]
fn headers_footers_watermarks_and_backgrounds_show_update_and_remove() {
    let (mut s, id) = session_with(2);
    let mut hf = HeaderFooter::default();
    hf.text[4] = "Page <<1>> of <<n>> · <<yyyy-mm-dd>>".into();
    s.apply(id, Edit::AddHeaderFooter { pages: vec![0, 1], settings: hf.clone(), replace: false }).unwrap();
    // The session clock is 2023-11-14.
    assert_eq!(page_texts(&s, id)[1], "Page 2\nPage 2 of 2 · 2023-11-14");
    assert_eq!(s.get(id).unwrap().marks, [MarkKind::HeaderFooter]);
    hf.text[4] = "Draft".into();
    s.apply(id, Edit::AddHeaderFooter { pages: vec![0, 1], settings: hf, replace: true }).unwrap();
    assert_eq!(page_texts(&s, id)[0], "Page 1\nDraft");
    s.apply(
        id,
        Edit::AddWatermark { pages: vec![0], settings: Watermark { text: "SECRET".into(), ..Watermark::default() }, file: None, replace: false },
    )
    .unwrap();
    s.apply(
        id,
        Edit::AddBackground {
            pages: vec![1],
            settings: Background { color: [1.0, 0.9, 0.9], opacity: 1.0, ..Background::default() },
            replace: false,
            file: None,
        },
    )
    .unwrap();
    assert_eq!(s.get(id).unwrap().marks.len(), 3);
    assert!(page_texts(&s, id)[0].contains("SECRET"));
    s.apply(id, Edit::RemoveMarks { kind: MarkKind::HeaderFooter }).unwrap();
    s.apply(id, Edit::RemoveMarks { kind: MarkKind::Watermark }).unwrap();
    s.apply(id, Edit::RemoveMarks { kind: MarkKind::Background }).unwrap();
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 2"]);
    assert!(s.get(id).unwrap().marks.is_empty());
    assert!(matches!(s.apply(id, Edit::RemoveMarks { kind: MarkKind::Watermark }), Err(EditError::Edit(_))));
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Remove background"));
}

#[test]
fn export_images_and_text_follow_the_working_file() {
    let (mut s, id) = session_with(2);
    s.apply(id, Edit::RotatePages { pages: vec![1], degrees: 90 }).unwrap();
    let doc = s.get(id).unwrap();
    let mut ex = export::Exporter::new(doc);
    let png = ex.png(1, 144.0).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    // 200×300 pt rotated → 300×200 pt at 2 px/pt.
    let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((w, h), (600, 400));
    assert_eq!(ex.text_of(&[0, 1]).unwrap(), "Page 1\n\u{c}Page 2\n");
    assert!(ex.png(5, 72.0).is_err());
}

#[test]
fn svg_export_follows_the_working_file() {
    let (mut s, id) = session_with(2);
    s.apply(id, Edit::RotatePages { pages: vec![1], degrees: 90 }).unwrap();
    let doc = s.get(id).unwrap();
    let mut ex = export::Exporter::new(doc);
    let page = ex.svg(1, &pdfcraft_render::SvgOptions::default()).unwrap();
    // The unsaved rotation: 200×300 pt shown as 300×200.
    assert!(page.svg.contains("viewBox=\"0 0 300 200\""), "{}", &page.svg[..300.min(page.svg.len())]);
    assert!(page.stats.glyphs == 5 && page.svg.contains(">Page 2</text>"), "{:?}", page.stats);
    assert!(ex.svg(5, &pdfcraft_render::SvgOptions::default()).unwrap_err().contains("does not exist"));
}

#[test]
fn create_and_reduce() {
    let mut s = Session::new();
    let blank = s.create_blank(612.0, 792.0, 2).unwrap();
    let id = s.open_new("Untitled.pdf", blank).unwrap();
    assert_eq!(s.get(id).unwrap().info.pages.len(), 2);
    let text = s.create_from_text("notes", "hello\nworld").unwrap();
    let t = s.open_new("notes.pdf", text).unwrap();
    assert_eq!(page_texts(&s, t), ["hello\nworld"]);
    // A file with the same page imported twice shares nothing until reduced.
    let (mut s2, id2) = session_with(1);
    let src = Arc::new(fixture(1));
    s2.apply(id2, Edit::InsertPagesFrom { name: "x".into(), bytes: src.clone(), pages: None, at: 1, merge_tags: true }).unwrap();
    let full = s2.save_full_bytes(id2).unwrap();
    let (reduced, _) = s2.reduced_bytes(id2).unwrap();
    assert!(reduced.len() <= full.len(), "{} > {}", reduced.len(), full.len());
    let mut s3 = Session::new();
    let r = s3.open("r.pdf", None, reduced, None).unwrap();
    assert_eq!(page_texts(&s3, r), ["Page 1", "Page 1"]);
}

#[test]
fn an_optimize_job_reports_its_stages_and_can_be_cancelled() {
    use crate::optimizer::OptimizeStage;
    let (s, id) = session_with(2);
    let settings = optimize::Settings::default();
    let mut stages = Vec::new();
    let (bytes, _) = s
        .optimize_job(id, &settings, &[])
        .unwrap()
        .run(|st| {
            stages.push(st);
            true
        })
        .unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    assert_eq!(stages.first(), Some(&OptimizeStage::Discarding));
    assert_eq!(&stages[stages.len() - 3..], [OptimizeStage::CleaningUp, OptimizeStage::Merging, OptimizeStage::Writing]);
    let fractions: Vec<f32> = stages.iter().map(|st| st.fraction()).collect();
    assert!(fractions.windows(2).all(|w| w[0] <= w[1]), "the bar never goes back: {fractions:?}");
    assert!(fractions.iter().all(|f| (0.0..=1.0).contains(f)));
    assert_eq!(
        OptimizeStage::Images { done: 0, total: 0 }.fraction(),
        OptimizeStage::Images { done: 5, total: 5 }.fraction(),
        "no images: no division by zero"
    );

    // Cancelled at the merge: nothing is written, the open document is untouched.
    let before = s.get(id).unwrap().bytes.clone();
    let r = s.optimize_job(id, &settings, &[]).unwrap().run(|st| st != OptimizeStage::Merging);
    assert!(matches!(r, Err(EditError::Cancelled)), "{r:?}");
    assert_eq!(s.get(id).unwrap().bytes, before);
}

#[test]
fn flattening_keeps_the_look_and_drops_the_objects() {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("form.pdf", None, Arc::new(form_fixture()), None).unwrap();
    s.apply(id, Edit::SetFieldValue { name: "name".into(), value: FieldValue::Text("Ada".into()) }).unwrap();
    s.apply(id, rect_comment(0, [80.0, 80.0, 120.0, 120.0])).unwrap();
    let red_before = pixel(&s, id, 0, 100, 100);
    s.apply(id, Edit::Flatten { comments: true, fields: true }).unwrap();
    let d = s.get(id).unwrap();
    assert!(d.info.annotations.is_empty() && d.form.is_empty(), "no comments or fields remain");
    assert_eq!(pixel(&s, id, 0, 100, 100), red_before, "the rectangle is now page content");
    assert_eq!(page_texts(&s, id), ["Ada"], "the field's text is now page content");
    s.undo(id).unwrap();
    assert_eq!(s.get(id).unwrap().form.len(), 2);
}

fn fill_item(shape: Shape, contents: &str) -> Edit {
    let style = Style::default_for(&shape);
    Edit::AddAnnotation(NewAnnotation { page: 0, shape, style, contents: contents.into(), author: "Ada".into() })
}

fn comment_text(s: &Session, id: DocId) -> Vec<String> {
    let mut t: Vec<String> = s.get(id).unwrap().info.annotations.iter().map(|a| a.contents.clone().unwrap_or_default()).collect();
    t.sort();
    t
}

#[test]
fn flattening_fill_and_sign_keeps_other_comments_fields_and_undoes() {
    let (mut s, id) = session_with(1);
    s.apply(id, fill_item(Shape::Typewriter { rect: [20.0, 200.0, 120.0, 220.0], font_size: 10.0 }, "Hello")).unwrap();
    s.apply(id, fill_item(Shape::Mark { rect: [140.0, 200.0, 160.0, 220.0], mark: FillMark::Check }, "")).unwrap();
    s.apply(id, fill_item(Shape::Signature { strokes: vec![vec![[10.0, 40.0], [40.0, 70.0], [70.0, 40.0]]] }, "")).unwrap();
    s.apply(id, fill_item(Shape::TypedSignature { rect: [90.0, 40.0, 150.0, 70.0], contours: vec![vec![[0.1, 0.2], [0.5, 0.9], [0.9, 0.2]]] }, ""))
        .unwrap();
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 8, 8);
        enc.set_color(png::ColorType::Rgba);
        let pixel = [40u8, 60, 80, 200];
        enc.write_header().unwrap().write_image_data(&pixel.repeat(64)).unwrap();
    }
    let image = SignatureImage::from_bytes(&png).unwrap();
    let info = s.get(id).unwrap().info.pages[0].clone();
    s.apply(id, image.edit(0, &info, [20.0, 120.0], false, "Ada").unwrap()).unwrap();
    s.apply(id, fill_item(Shape::TextMarkup { kind: Markup::Highlight, quads: vec![[20.0, 280.0, 80.0, 280.0, 20.0, 270.0, 80.0, 270.0]] }, "keep"))
        .unwrap();
    s.apply(id, fill_item(Shape::TextBox { rect: [20.0, 150.0, 120.0, 180.0], font_size: 12.0 }, "Note")).unwrap();
    s.apply(id, Edit::AddField { page: 0, rect: [20.0, 230.0, 120.0, 250.0], kind: NewField::Text { multiline: false }, name: Some("email".into()) })
        .unwrap();

    s.apply(id, Edit::FlattenFillSign).unwrap();
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Flatten Fill & Sign"));
    assert_eq!(comment_text(&s, id), ["Note", "keep"]);
    assert_eq!(s.get(id).unwrap().form.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["email"]);
    s.undo(id).unwrap();
    assert!(comment_text(&s, id).contains(&"Hello".to_string()), "undo restores the typewriter");

    s.apply(id, Edit::FlattenFillSign).unwrap();
    let saved = s.save_bytes(id).unwrap();
    s.mark_saved(id, saved.clone(), None).unwrap();
    let mut again = Session::new();
    let id2 = again.open("saved.pdf", None, saved, None).unwrap();
    assert_eq!(comment_text(&again, id2), ["Note", "keep"]);
    assert_eq!(again.get(id2).unwrap().form.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["email"]);
}

#[test]
fn flattening_fill_and_sign_does_nothing_when_there_are_none() {
    let (mut s, id) = session_with(1);
    s.apply(id, fill_item(Shape::TextMarkup { kind: Markup::Highlight, quads: vec![[20.0, 200.0, 80.0, 200.0, 20.0, 190.0, 80.0, 190.0]] }, "keep"))
        .unwrap();
    let saved = s.save_bytes(id).unwrap();
    s.mark_saved(id, saved, None).unwrap();
    assert!(!s.get(id).unwrap().dirty);
    s.apply(id, Edit::FlattenFillSign).unwrap();
    assert!(!s.get(id).unwrap().dirty, "nothing was baked in, so the document stays saved");
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Add highlight"), "no undo step for a flatten that changed nothing");
    assert_eq!(comment_text(&s, id), ["keep"]);
}

#[test]
fn flattening_fill_and_sign_is_refused_without_permission_to_change() {
    let (mut s, id) = session_with(1);
    s.apply(id, fill_item(Shape::Typewriter { rect: [20.0, 200.0, 120.0, 220.0], font_size: 10.0 }, "Hello")).unwrap();
    let p = Protection { printing: Printing::Low, changes: Changes::CommentFillSign, copy: false, ..protection(None, Some("boss")) };
    s.apply(id, Edit::Protect(p)).unwrap();
    let saved = s.save_bytes(id).unwrap();
    let mut other = Session::new();
    let id2 = other.open("locked.pdf", None, saved, None).unwrap();
    let before = other.get(id2).unwrap().bytes.clone();
    assert_eq!(other.apply(id2, Edit::FlattenFillSign), Err(EditError::NotPermitted("changes to the document")));
    assert_eq!(other.get(id2).unwrap().bytes, before);
    assert!(!other.get(id2).unwrap().dirty);
    assert!(comment_text(&other, id2).contains(&"Hello".to_string()));
}

#[test]
fn flattening_fill_and_sign_on_a_signed_file_stays_incremental() {
    let p12 = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../sign/tests/data/ec-p256.p12")).unwrap();
    let digital_id = sign::pkcs12::open(&p12, "test").unwrap();
    let mut s = Session::new().with_clock(|| 1_800_000_000);
    let id = s.open("fixture.pdf", None, Arc::new(fixture(1)), None).unwrap();
    let opts = SignOptions { page: 0, rect: Some([20.0, 20.0, 160.0, 50.0]), ..SignOptions::default() };
    let signed = s.sign(id, &digital_id, opts).unwrap();
    s.mark_signed(id, signed.clone(), None).unwrap();
    s.apply(id, fill_item(Shape::Typewriter { rect: [20.0, 200.0, 120.0, 220.0], font_size: 10.0 }, "Hello")).unwrap();
    s.apply(id, Edit::FlattenFillSign).unwrap();
    let saved = s.save_bytes(id).unwrap();
    assert!(saved.starts_with(signed.as_ref()), "the signature's bytes stay an incremental update");
    assert!(!comment_text(&s, id).contains(&"Hello".to_string()));
}

#[test]
fn flattening_fill_and_sign_reopens_an_encrypted_file() {
    let (mut s, id) = session_with(1);
    s.apply(id, fill_item(Shape::Typewriter { rect: [20.0, 200.0, 120.0, 220.0], font_size: 10.0 }, "Hello")).unwrap();
    s.apply(id, Edit::Protect(protection(Some("secret"), None))).unwrap();
    s.apply(id, Edit::FlattenFillSign).unwrap();
    let saved = s.save_bytes(id).unwrap();
    let mut again = Session::new();
    assert!(again.open("p.pdf", None, saved.clone(), None).is_err());
    let id2 = again.open("p.pdf", None, saved, Some("secret")).unwrap();
    assert!(!comment_text(&again, id2).contains(&"Hello".to_string()));
}

#[test]
fn pages_export_as_png_jpeg_and_tiff() {
    use crate::export::{Exporter, ImageFormat};
    let (s, id) = session_with(1);
    let mut ex = Exporter::new(s.get(id).unwrap());
    let png = ex.image(0, 72.0, ImageFormat::Png).unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    let jpg = ex.image(0, 72.0, ImageFormat::Jpeg { quality: 85 }).unwrap();
    let decoded = image::load_from_memory_with_format(&jpg, image::ImageFormat::Jpeg).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (200, 300), "the page at 72 dpi");
    let corner = decoded.to_rgb8().get_pixel(2, 2).0;
    assert!(corner.iter().all(|c| *c > 240), "white paper, not black: {corner:?}");
    let tif = ex.image(0, 144.0, ImageFormat::Tiff).unwrap();
    let decoded = image::load_from_memory_with_format(&tif, image::ImageFormat::Tiff).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (400, 600));
}

#[test]
fn added_text_and_images_render_and_stay_editable() {
    let (mut s, id) = session_with(1);
    let text = AddedText { rect: [20.0, 250.0, 180.0, 280.0], text: "Added note".into(), size: 12.0, ..AddedText::default() };
    s.apply(id, Edit::AddText { page: 0, text: text.clone() }).unwrap();
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Add text"));
    assert!(page_texts(&s, id)[0].contains("Added note"), "the renderer's extractor sees page content");
    // A PNG, centred at its natural size.
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 20, 10);
        enc.set_color(png::ColorType::Rgb);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[0u8; 600]).unwrap();
    }
    s.apply(id, Edit::AddImage { page: 0, rect: None, name: "dot.png".into(), bytes: Arc::new(png) }).unwrap();
    let d = s.get(id).unwrap();
    assert_eq!(d.added.len(), 2);
    assert_eq!(d.added[1].content.rect(), [90.0, 145.0, 110.0, 155.0], "20×10 px at 72 dpi, centred on 200×300");
    let moved = d.added[0].content.with_rect([30.0, 100.0, 190.0, 130.0]);
    s.apply(id, Edit::UpdateContent { page: 0, index: 0, content: moved }).unwrap();
    assert_eq!(s.get(id).unwrap().added[0].content.rect()[3], 130.0);
    s.apply(id, Edit::DeleteContent { page: 0, index: 1 }).unwrap();
    assert_eq!(s.get(id).unwrap().added.len(), 1);
    s.undo(id).unwrap();
    assert_eq!(s.get(id).unwrap().added.len(), 2);
}

#[test]
fn added_images_rotate_flip_and_crop_as_drawn() {
    // A 20×10 image: left half red, right half blue.
    let mut px = Vec::new();
    for _y in 0..10 {
        for x in 0..20 {
            px.extend_from_slice(if x < 10 { &[255, 0, 0] } else { &[0, 0, 255] });
        }
    }
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 20, 10);
        enc.set_color(png::ColorType::Rgb);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
    }
    let (mut s, id) = session_with(1);
    // A 100×100 box at (50, 100) on the 200×300 page.
    s.apply(id, Edit::AddImage { page: 0, rect: Some([50.0, 100.0, 150.0, 200.0]), name: "rb.png".into(), bytes: Arc::new(png) }).unwrap();
    let colour_at = |s: &Session, x: u32, y_from_top: u32| -> [u8; 3] {
        let doc = s.get(id).unwrap();
        let mut r = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), Default::default());
        let out = r.render(pdfcraft_render::RenderRequest { page: 0, scale: 1.0, ..Default::default() });
        let i = ((y_from_top * out.width + x) * 4) as usize;
        [out.rgba[i], out.rgba[i + 1], out.rgba[i + 2]]
    };
    let red = |c: [u8; 3]| c[0] > 200 && c[2] < 60;
    let blue = |c: [u8; 3]| c[2] > 200 && c[0] < 60;
    // Display y 100–200 is rows 100–200 from the top on a 300 pt page.
    assert!(red(colour_at(&s, 70, 150)) && blue(colour_at(&s, 130, 150)), "as placed: red left, blue right");
    let item = |s: &Session| s.get(id).unwrap().added[0].content.clone();
    let AddedContent::Image(mut img) = item(&s) else { panic!() };
    img.rotation = 1;
    s.apply(id, Edit::UpdateContent { page: 0, index: 0, content: AddedContent::Image(img.clone()) }).unwrap();
    assert!(red(colour_at(&s, 100, 180)) && blue(colour_at(&s, 100, 120)), "a quarter turn left: red at the bottom");
    img.rotation = 0;
    img.flip_h = true;
    s.apply(id, Edit::UpdateContent { page: 0, index: 0, content: AddedContent::Image(img.clone()) }).unwrap();
    assert!(blue(colour_at(&s, 70, 150)) && red(colour_at(&s, 130, 150)), "flipped");
    img.flip_h = false;
    img.crop = [0.0, 0.0, 0.5, 0.0];
    s.apply(id, Edit::UpdateContent { page: 0, index: 0, content: AddedContent::Image(img) }).unwrap();
    assert!(red(colour_at(&s, 130, 150)), "the right half cropped away: red fills the box");
}

#[test]
fn revert_goes_back_to_the_saved_version() {
    let (mut s, id) = session_with(2);
    s.apply(id, Edit::DeletePages { pages: vec![0] }).unwrap();
    let saved = s.save_bytes(id).unwrap();
    s.mark_saved(id, saved, None).unwrap();
    s.apply(id, Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
    s.apply(id, Edit::InsertBlankPage { at: 0, width: 200.0, height: 300.0 }).unwrap();
    assert!(s.get(id).unwrap().dirty);
    s.revert(id).unwrap();
    let d = s.get(id).unwrap();
    assert!(!d.dirty && d.can_undo().is_none() && d.can_redo().is_none());
    assert_eq!(page_texts(&s, id), ["Page 2"], "the saved state, not the opened one");
    assert_eq!(d.info.pages[0].rotation, 0);
}

#[test]
fn split_by_size_and_bookmarks_and_page_filters() {
    let (mut s, id) = session_with(6);
    // Every page alone is a few hundred bytes: a limit of about two pages gives three parts.
    let one = s.split(id, &pdfcraft_organize::SplitBy::PageCount(1)).unwrap()[0].2.len();
    let parts = s.split_by_size(id, one * 2 + one / 2).unwrap();
    assert!(parts.len() >= 2 && parts.len() <= 6, "{}", parts.len());
    assert_eq!(parts.last().unwrap().1, 6, "every page is in a part");
    assert_eq!(s.split_by_size(id, 1).unwrap().len(), 6, "pages larger than the limit stand alone");
    // Top-level bookmarks name the parts.
    s.apply(id, Edit::AddBookmark { parent: vec![], index: 0, title: "Intro".into(), page: 0 }).unwrap();
    s.apply(id, Edit::AddBookmark { parent: vec![], index: 1, title: "Results".into(), page: 3 }).unwrap();
    assert_eq!(s.bookmark_splits(id), [(0, "Intro".to_string()), (3, "Results".to_string())]);
    // Filters: page numbers 2, 4, 6 are even.
    let info = &s.get(id).unwrap().info;
    let all: Vec<usize> = (0..6).collect();
    assert_eq!(filter_pages(info, &all, PageParity::Even, PageOrientation::Both), [1, 3, 5]);
    assert_eq!(filter_pages(info, &all, PageParity::Odd, PageOrientation::Landscape), Vec::<usize>::new());
    assert_eq!(filter_pages(info, &all, PageParity::Odd, PageOrientation::Portrait), [0, 2, 4]);
}

#[test]
fn links_from_urls_and_link_edits() {
    // One page whose text contains a web address.
    let mut objs: Vec<Vec<u8>> =
        vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(), b"<< /Type /Pages /Kids [4 0 R] /Count 1 /MediaBox [0 0 300 200] >>".to_vec()];
    objs.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    objs.push(b"<< /Type /Page /Parent 2 0 R /Contents 5 0 R /Resources << /Font << /F1 3 0 R >> >> >>".to_vec());
    let body = b"BT /F1 12 Tf 20 100 Td (Visit www.example.org today) Tj ET";
    objs.push([format!("<< /Length {} >>\nstream\n", body.len()).into_bytes(), body.to_vec(), b"\nendstream".to_vec()].concat());
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    let mut s = Session::new();
    let id = s.open("u.pdf", None, Arc::new(out), None).unwrap();
    let found = s.find_urls(id);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].2, "http://www.example.org");
    s.apply(id, Edit::AddLinks { links: found, style: LinkStyle::default() }).unwrap();
    let d = s.get(id).unwrap();
    assert_eq!(d.links.len(), 1);
    assert_eq!(d.info.links.len(), 1, "the viewer follows it");
    assert!(s.find_urls(id).is_empty(), "already linked");
    // Link Properties and delete.
    let l = s.get(id).unwrap().links[0].clone();
    s.apply(id, Edit::SetLink { page: 0, index: l.index, rect: None, action: Some(LinkAction::Page(0)), style: None }).unwrap();
    assert_eq!(s.get(id).unwrap().links[0].action, LinkAction::Page(0));
    s.apply(id, Edit::DeleteLink { page: 0, index: l.index }).unwrap();
    assert!(s.get(id).unwrap().links.is_empty());
    assert!(s.apply(id, Edit::RemoveLinks { pages: None }).is_err(), "nothing left to remove");
}

#[test]
fn comments_lock_take_checkmarks_hide_and_summarize() {
    let (mut s, id) = session_with(2);
    s.apply(id, rect_comment(1, [80.0, 80.0, 120.0, 120.0])).unwrap();
    s.apply(id, Edit::ReplyToAnnotation { page: 1, index: 0, text: "Agreed".into(), author: "Ada".into() }).unwrap();
    s.apply(id, Edit::LockAnnotation { page: 1, index: 0, locked: true }).unwrap();
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Lock comment"));
    assert!(s.get(id).unwrap().info.annotations[0].locked);
    assert!(s.apply(id, Edit::MoveAnnotation { page: 1, index: 0, dx: 5.0, dy: 0.0 }).is_err(), "locked comments stay put");
    assert!(s.apply(id, Edit::DeleteAnnotation { page: 1, index: 0 }).is_err(), "and can't be deleted");
    s.apply(id, Edit::SetAnnotationContents { page: 1, index: 0, text: "Still editable".into() }).unwrap();
    s.apply(id, Edit::LockAnnotation { page: 1, index: 0, locked: false }).unwrap();
    s.apply(id, Edit::MoveAnnotation { page: 1, index: 0, dx: 5.0, dy: 0.0 }).unwrap();

    s.apply(id, Edit::MarkAnnotation { page: 1, index: 0, marked: true, author: "Ada".into() }).unwrap();
    let a = &s.get(id).unwrap().info.annotations;
    let mark = a.iter().find(|r| r.is_mark()).expect("a checkmark reply");
    assert_eq!(mark.state.as_deref(), Some("Marked"));

    // Hiding comments drops the rectangle but keeps the page.
    let px = pixel(&s, id, 1, 105, 100);
    assert!(px[0] > 200 && px[1] < 40, "drawn: {px:?}");
    assert!(s.set_hide_comments(id, true));
    assert!(!s.set_hide_comments(id, true));
    assert_eq!(pixel(&s, id, 1, 105, 100), [255, 255, 255, 255]);
    assert!(s.set_hide_comments(id, false));
    assert!(pixel(&s, id, 1, 105, 100)[1] < 40);

    let text = comment_summary("doc.pdf", &s.get(id).unwrap().info.annotations, SummarySort::Page);
    assert!(text.starts_with("Summary of Comments on doc.pdf"));
    assert!(text.contains("Page: 2\nNumber: 1  Author: Reviewer  Subject: Rectangle"), "{text}");
    assert!(text.contains("Still editable") && text.contains("    Agreed"), "{text}");
    assert!(!text.contains("Marked"), "checkmarks aren't listed: {text}");
    let pdf = s.summarize_comments(id, SummarySort::Author).unwrap();
    let n = s.open_new("summary.pdf", pdf).unwrap();
    assert!(!s.get(n).unwrap().info.pages.is_empty());
    assert_eq!(comment_summary("x", &[], SummarySort::Page), "Summary of Comments on x\n\nThis document has no comments.\n");
}

#[test]
fn signing_saving_trusting_and_commenting_afterwards() {
    let p12 = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../sign/tests/data/ec-p256.p12")).unwrap();
    let digital_id = sign::pkcs12::open(&p12, "test").unwrap();
    // After the test certificate's start date (2026-10-02).
    let mut s = Session::new().with_clock(|| 1_800_000_000);
    let id = s.open("fixture.pdf", None, Arc::new(fixture(2)), None).unwrap();
    assert!(s.get(id).unwrap().signatures.is_empty());
    let opts = SignOptions { page: 1, rect: Some([20.0, 20.0, 180.0, 60.0]), reason: Some("Approved".into()), ..SignOptions::default() };
    let signed = s.sign(id, &digital_id, opts).unwrap();
    s.mark_signed(id, signed, Some("/tmp/signed.pdf".into())).unwrap();
    let doc = s.get(id).unwrap();
    assert_eq!(doc.name, "signed.pdf");
    assert!(doc.is_signed() && !doc.dirty && doc.can_undo().is_none(), "signing can't be undone");
    let sig = &doc.signatures[0];
    assert_eq!((sig.status, sig.page, sig.date.as_deref()), (SignatureStatus::Unknown, Some(1), Some("D:20270115080000Z")));
    assert_eq!(s.save_full_bytes(id).unwrap_err(), EditError::Signed);
    assert_eq!(s.save_linearized_bytes(id).unwrap_err(), EditError::Signed);
    // Trusting the signer makes it valid; a later comment is permitted.
    s.set_trusted_certificates(vec![digital_id.certificate.clone()]);
    assert_eq!(s.get(id).unwrap().signatures[0].status, SignatureStatus::Valid);
    s.apply(id, rect_comment(0, [80.0, 80.0, 120.0, 120.0])).unwrap();
    let sig = &s.get(id).unwrap().signatures[0];
    assert_eq!((sig.status, sig.modification.clone()), (SignatureStatus::Valid, sign::Modification::Allowed(vec!["comments".into()])));
    // Saving keeps the signature (incremental).
    let saved = s.save_bytes(id).unwrap();
    s.mark_saved(id, saved, None).unwrap();
    assert_eq!(s.get(id).unwrap().signatures[0].status, SignatureStatus::Valid);
    // Edits that would need a full rewrite are refused, and change nothing.
    let before = s.get(id).unwrap().can_undo().map(str::to_owned);
    assert!(matches!(s.apply(id, Edit::Sanitize), Err(EditError::SignedRewrite(_))));
    assert!(!s.get(id).unwrap().dirty && s.get(id).unwrap().can_undo().map(str::to_owned) == before);
}

#[test]
#[ignore = "timing probe"]
fn probe_edit_latency_on_a_large_signed_document() {
    // ~120 MB: one page whose content stream is large.
    let big: Vec<u8> = (0..120_000_000u32).map(|i| b"0123456789 "[(i % 11) as usize]).collect();
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offs = vec![];
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R >>".to_vec(),
        [format!("<< /Length {} >>\nstream\n", big.len() + 3).into_bytes(), b"%  ".to_vec(), big, b"\nendstream".to_vec()].concat(),
    ];
    for (i, o) in objs.iter().enumerate() {
        offs.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n", i + 1).bytes());
        pdf.extend(o);
        pdf.extend(b"\nendobj\n");
    }
    let x = pdf.len();
    pdf.extend(b"xref\n0 5\n0000000000 65535 f \n");
    for o in offs {
        pdf.extend(format!("{o:010} 00000 n \n").bytes());
    }
    pdf.extend(format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").bytes());
    let p12 = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../sign/tests/data/ec-p256.p12")).unwrap();
    let digital_id = sign::pkcs12::open(&p12, "test").unwrap();
    let mut s = Session::new().with_clock(|| 1_800_000_000);
    let t = std::time::Instant::now();
    let id = s.open("big.pdf", None, Arc::new(pdf), None).unwrap();
    eprintln!("open: {:?}", t.elapsed());
    let t = std::time::Instant::now();
    s.apply(id, rect_comment(0, [5.0, 10.0, 50.0, 50.0])).unwrap();
    eprintln!("comment edit before signing: {:?}", t.elapsed());
    s.undo(id).unwrap();
    let t = std::time::Instant::now();
    let signed = s.sign(id, &digital_id, SignOptions { rect: None, ..SignOptions::default() }).unwrap();
    eprintln!("sign: {:?}", t.elapsed());
    let t = std::time::Instant::now();
    s.mark_signed(id, signed, None).unwrap();
    eprintln!("mark_signed (reopen + validate): {:?}", t.elapsed());
    for k in 0..3 {
        let t = std::time::Instant::now();
        s.apply(id, rect_comment(0, [10.0 + k as f64, 10.0, 50.0, 50.0])).unwrap();
        eprintln!("comment edit {k}: {:?}", t.elapsed());
    }
}

/// A tiny uncompressed BMP of one colour.
fn bmp(w: u32, h: u32, bgr: [u8; 3]) -> Vec<u8> {
    let row = (w * 3).div_ceil(4) * 4;
    let size = 54 + row * h;
    let mut b = b"BM".to_vec();
    b.extend(size.to_le_bytes());
    b.extend([0u8; 4]);
    b.extend(54u32.to_le_bytes());
    b.extend(40u32.to_le_bytes());
    b.extend((w as i32).to_le_bytes());
    b.extend((h as i32).to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(24u16.to_le_bytes());
    b.extend([0u8; 24]);
    for _ in 0..h {
        for _ in 0..w {
            b.extend(bgr);
        }
        b.extend(vec![0u8; (row - w * 3) as usize]);
    }
    b
}

#[test]
fn backgrounds_and_watermarks_from_files() {
    let (mut s, id) = session_with(1);
    // A source PDF whose second page is solid blue.
    let blue = {
        let mut src = Session::new();
        let sid = src.open("src.pdf", None, Arc::new(fixture(2)), None).unwrap();
        src.apply(
            sid,
            Edit::AddBackground {
                pages: vec![1],
                settings: Background { color: [0.0, 0.0, 1.0], ..Background::default() },
                replace: false,
                file: None,
            },
        )
        .unwrap();
        src.save_bytes(sid).unwrap()
    };
    let file = MarkFile { name: "src.pdf".into(), bytes: blue, page: 1 };
    s.apply(id, Edit::AddBackground { pages: vec![0], settings: Background::default(), replace: false, file: Some(file) }).unwrap();
    let corner = pixel(&s, id, 0, 3, 3);
    assert!(corner[2] > 200 && corner[0] < 40, "the blue page fills the background: {corner:?}");
    // The page's own text still draws on top (at "Page 1", 20,150).
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Add background"));
    // A red image watermark at half the page, on top, unrotated.
    let red = MarkFile { name: "stamp.bmp".into(), bytes: Arc::new(bmp(4, 4, [0, 0, 255])), page: 0 };
    let wm = Watermark { rotation: 0.0, opacity: 1.0, scale: 0.5, ..Watermark::default() };
    s.apply(id, Edit::AddWatermark { pages: vec![0], settings: wm, replace: false, file: Some(red) }).unwrap();
    let centre = pixel(&s, id, 0, 100, 150);
    assert!(centre[0] > 200 && centre[2] < 40, "the red picture sits in the middle: {centre:?}");
    let edge = pixel(&s, id, 0, 10, 150);
    assert!(edge[2] > 200, "outside it the background shows: {edge:?}");
    // A bad file is refused clearly.
    let junk = MarkFile { name: "x.png".into(), bytes: Arc::new(b"not an image".to_vec()), page: 0 };
    assert!(s.apply(id, Edit::AddBackground { pages: vec![0], settings: Background::default(), replace: true, file: Some(junk) }).is_err());
}

/// Scan & OCR ▸ Recognize text on a page that is only a picture of text (needs the models:
/// `cargo xtask models`; skipped without them).
#[test]
fn recognize_text_makes_a_scanned_page_searchable() {
    if !ocr::Models::required_for_tests().unwrap() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let text = s.create_from_text("t", "The quick brown fox jumps over the lazy dog.").unwrap();
    let id = s.open("text.pdf", None, text, None).unwrap();
    // Scanned at 300 dpi, the usual scanning resolution: at 150 dpi the pinned model reads this
    // Helvetica "quick" as "auick" (observed 2026-10-12), a recognition-accuracy limit
    // rather than a placement or text-layer defect.
    let png = export::Exporter::new(s.get(id).unwrap()).png(0, 300.0).unwrap();
    let scan = s.create_from_images(&[("scan.png".into(), png)]).unwrap();
    let id = s.open("scan.pdf", None, scan, None).unwrap();
    assert_eq!(page_texts(&s, id), [""], "a picture has no text");

    let found = s.recognize_text(id, &[], ocr::OcrSettings::default()).unwrap();
    assert_eq!(found.len(), 1);
    let text = page_texts(&s, id)[0].to_lowercase();
    for w in ["quick", "brown", "fox", "lazy"] {
        assert!(text.contains(w), "{text}");
    }
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Recognize text"));
    // The text sits over the words: "quick" is left of "lazy" and on the same line.
    let words = &found[0].words;
    let q = words.iter().find(|w| w.text.to_lowercase().contains("quick")).unwrap();
    let l = words.iter().find(|w| w.text.to_lowercase().contains("lazy")).unwrap();
    // The image becomes a page at one point per pixel, so the tolerance scales with the text:
    // tops within a fifth of the word height (3 units at the former 150 dpi scan).
    let top = |w: &ocr::PlacedWord| w.origin[1] + w.up[1];
    let tolerance = 0.2 * q.up[1].abs().max(l.up[1].abs());
    assert!(q.origin[0] < l.origin[0] && (top(q) - top(l)).abs() < tolerance, "{q:?} {l:?}");

    // A second pass skips the page: it has text now.
    let again = s.recognize_text(id, &[], ocr::OcrSettings::default()).unwrap();
    assert!(again[0].skipped.is_some());
}

/// A form whose scripts are custom JavaScript: total = price × qty (calculate, through a
/// document-level function), shown with a custom format; qty is validated; a button script.
fn scripted_form() -> Vec<u8> {
    let text = |name: &str, y: u32, aa: &str| {
        format!("<< /Type /Annot /Subtype /Widget /FT /Tx /T ({name}) /Rect [20 {y} 180 {}] /P 3 0 R /F 4 {aa} >>", y + 20)
    };
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /Names << /JavaScript << /Names [(helpers) 10 0 R] >> >> /AcroForm << /Fields [4 0 R 5 0 R 6 0 R 7 0 R] /CO [6 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 8 0 R >> >> >> >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R 6 0 R 7 0 R] >>".to_string(),
        text("price", 250, "/V (2.5)"),
        text("qty", 220, "/AA << /V << /S /JavaScript /JS (if (event.value > 100) { app.alert('At most 100'); event.rc = false; }) >> >>"),
        text("total", 190, "/AA << /C << /S /JavaScript /JS (event.value = times\\(getField('price').value, getField('qty').value\\);) >> /F << /S /JavaScript /JS (event.value = 'USD ' + util.printf('%.2f', event.value);) >> >>"),
        "<< /Type /Annot /Subtype /Widget /FT /Btn /Ff 65536 /T (go) /Rect [20 150 80 170] /P 3 0 R /F 4 >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        "<< >>".to_string(),
        "<< /S /JavaScript /JS (function times\\(a, b\\) { console.println('times'); return a * b; }) >>".to_string(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

#[test]
fn form_javascript_validates_calculates_and_formats() {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("order.pdf", None, Arc::new(scripted_form()), None).unwrap();
    let value = |s: &Session, n: &str| s.get(id).unwrap().form.iter().find(|f| f.name == n).unwrap().value.clone();
    s.apply(id, Edit::SetFieldValue { name: "qty".into(), value: FieldValue::Text("4".into()) }).unwrap();
    assert_eq!(value(&s, "total"), ["10"], "calculated by the script");
    assert!(page_texts(&s, id)[0].contains("USD 10.00"), "{:?}", page_texts(&s, id));
    let out = s.take_js_output(id);
    assert!(out.console.contains(&"times".to_string()), "{out:?}");

    let err = s.apply(id, Edit::SetFieldValue { name: "qty".into(), value: FieldValue::Text("500".into()) }).unwrap_err();
    assert_eq!(err.to_string(), "At most 100");
    assert_eq!(value(&s, "qty"), ["4"]);

    // A button script changes fields in one undoable step, and asks for a page.
    let o =
        s.run_javascript(id, "getField('price').value = 3; getField('total').readonly = true; app.alert('Done'); this.print();", Some("go")).unwrap();
    assert_eq!(o.alerts, ["Done"]);
    assert_eq!(o.requests, [js::Request::Print]);
    assert_eq!(value(&s, "total"), ["12"], "recalculated after the change");
    assert!(s.get(id).unwrap().form.iter().find(|f| f.name == "total").unwrap().read_only());
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Run JavaScript"));

    // JavaScript off: scripts don't run.
    s.set_javascript(false);
    s.apply(id, Edit::SetFieldValue { name: "qty".into(), value: FieldValue::Text("500".into()) }).unwrap();
    assert_eq!(value(&s, "total"), ["12"]);
    assert!(s.run_javascript(id, "1", None).is_err());
}

#[test]
fn detecting_fields_on_a_printed_form() {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let text = s.create_from_text("t", "Name: ______________________\n\nEmail address: ____________________\n\nPlain text without blanks.").unwrap();
    let id = s.open("paper.pdf", None, text, None).unwrap();
    let found = s.detect_fields(id, &[]);
    let names: Vec<&str> = found.iter().map(|(_, c)| c.name.as_str()).collect();
    assert_eq!(names, ["Name", "Email address"], "{found:?}");
    let added = s.auto_detect_fields(id, &[]).unwrap();
    assert_eq!(added, ["Name", "Email address"]);
    let d = s.get(id).unwrap();
    assert_eq!(d.form.len(), 2);
    assert_eq!(d.can_undo(), Some("Detect form fields"));
    // The field sits on the blank: right of the label, at its height.
    let f = &d.form[0];
    assert!(f.widgets[0].rect[0] > 72.0 + 20.0, "{:?}", f.widgets[0].rect);
    assert!(s.detect_fields(id, &[]).is_empty(), "nothing left to detect");
}

/// Field names come from label words, which are page text: a document that allows changes
/// but not copying gets the same fields with generic names; the owner password lifts it.
#[test]
fn detected_field_names_withhold_label_text_when_copying_is_denied() {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let text = s.create_from_text("t", "Name: ______________________\n\nEmail address: ____________________").unwrap();
    let form = s.open("form.pdf", None, text, None).unwrap();
    let p = Protection { changes: Changes::AnyExceptExtract, copy: false, ..protection(Some("user"), Some("owner")) };
    s.apply(form, Edit::Protect(p)).unwrap();
    let bytes = s.save_bytes(form).unwrap();
    let user = s.open("restricted.pdf", None, bytes.clone(), Some("user")).unwrap();
    let d = s.get(user).unwrap();
    assert!(d.allows_modification() && !d.allows_copying());
    let found = s.detect_fields(user, &[]);
    let names: Vec<&str> = found.iter().map(|(_, c)| c.name.as_str()).collect();
    assert_eq!(names, ["Text1", "Text2"], "{found:?}");
    let owner = s.open("owner.pdf", None, bytes, Some("owner")).unwrap();
    let owner_found = s.detect_fields(owner, &[]);
    let names: Vec<&str> = owner_found.iter().map(|(_, c)| c.name.as_str()).collect();
    assert_eq!(names, ["Name", "Email address"]);
    // The same fields in the same places: only the names differ.
    let rects = |f: &[(usize, pdfcraft_forms::detect::Candidate)]| f.iter().map(|(p, c)| (*p, c.rect)).collect::<Vec<_>>();
    assert_eq!(rects(&found), rects(&owner_found));
    assert_eq!(s.auto_detect_fields(user, &[]).unwrap(), ["Text1", "Text2"]);
    assert!(s.get(user).unwrap().form.iter().all(|f| !f.name.contains("Name") && !f.name.contains("Email")));
}

/// The renderer can open files the editor cannot read. Such an encrypted document has no
/// established permissions, so printing and every change fail closed (as copying does);
/// an unencrypted document without an editor still prints.
#[test]
fn encrypted_documents_without_an_editor_deny_printing_and_changes() {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let plain = s.open("plain.pdf", None, Arc::new(fixture(2)), None).unwrap();
    let owner = s.open("owner.pdf", None, protected("user", "owner", -1), Some("owner")).unwrap();
    let settings = print::Settings { pages: vec![0], ..Default::default() };
    let d = s.get(owner).unwrap();
    assert!(d.allows_printing() && d.allows_modification() && d.allows_security_change(), "the owner may do anything");
    assert!(s.print_pdf(owner, &settings).is_ok());
    for id in [plain, owner] {
        let doc = s.doc_mut(id).unwrap();
        doc.editor = None;
        doc.read_only_reason = Some("synthetic: the document structure could not be read for editing".into());
    }
    let d = s.get(plain).unwrap();
    assert!(!d.info.encrypted && d.permissions().is_none());
    assert!(d.allows_printing(), "an unencrypted document has no restrictions to lose");
    assert!(s.print_pdf(plain, &settings).is_ok());
    let d = s.get(owner).unwrap();
    assert!(d.info.encrypted && d.permissions().is_none());
    assert!(!d.allows_printing(), "unknown permissions on an encrypted document must not allow printing");
    assert!(!d.allows_modification() && !d.allows_assembly() && !d.allows_annotation());
    assert!(!d.allows_form_filling() && !d.allows_security_change() && !d.allows_copying());
    assert_eq!(s.print_pdf(owner, &settings), Err(EditError::NotPermitted("printing")));
    for edit in [Edit::SetInfo { key: "Title".into(), value: "x".into() }, Edit::DeletePages { pages: vec![0] }, Edit::RemoveProtection] {
        assert!(matches!(s.apply(owner, edit), Err(EditError::ReadOnly(_))));
    }
    assert!(s.extract(owner, &[0]).is_err());
    assert!(s.split(owner, &pdfcraft_organize::SplitBy::PageCount(1)).is_err());
    for id in ["page.cut", "form.detect", "edit.edit_text", "measure.distance", "protect.password", "protect.remove"] {
        let spec = commands::command(id).unwrap();
        assert!(!commands::is_enabled(spec, &s, Some(owner)), "{id}");
        assert!(!commands::is_enabled(spec, &s, Some(plain)), "{id}: without an editor nothing changes");
    }
    assert!(!s.get(owner).unwrap().dirty);
}

#[test]
fn comparing_two_versions_of_a_document() {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let v1 = s.create_from_text("t", "The contract starts on Monday.\n\nPayment is due within thirty days.\n\nSigned by both parties.").unwrap();
    let v2 = s
        .create_from_text("t", "The contract starts on Tuesday.\n\nPayment is due within thirty days of invoice.\n\nSigned by both parties.")
        .unwrap();
    let old = s.open("v1.pdf", None, v1, None).unwrap();
    let new = s.open("v2.pdf", None, v2, None).unwrap();
    let c = s.compare(old, new).unwrap();
    let got: Vec<(compare::Kind, &str, &str)> = c.changes.iter().map(|x| (x.kind, x.old.text.as_str(), x.new.text.as_str())).collect();
    assert_eq!(got, [(compare::Kind::Replaced, "Monday.", "Tuesday."), (compare::Kind::Replaced, "days.", "days of invoice.")], "{got:?}");
    assert!(s.compare(old, old).unwrap().identical());

    let report = s.compare_report(old, new).unwrap();
    let rid = s.open("report.pdf", None, report, None).unwrap();
    let text = page_texts(&s, rid).join(" ");
    assert!(text.contains("2 changes: 2 replaced") && text.contains("Tuesday."), "{text}");

    // Visually, only the changed words' places differ (and the page is otherwise the same).
    let vis = s.compare_visual(old, new, 72.0).unwrap();
    assert!(!vis.is_empty() && vis.iter().all(|(p, r)| *p == 0 && r[3] > 600.0), "{vis:?}");
    assert!(s.compare_visual(old, old, 72.0).unwrap().is_empty());

    assert_eq!(s.mark_differences(old, new).unwrap(), 2);
    let d = s.get(new).unwrap();
    assert_eq!(d.can_undo(), Some("Mark differences"));
    let marks: Vec<_> = d.info.annotations.iter().filter(|a| a.author.as_deref() == Some("Compare")).collect();
    assert_eq!(marks.len(), 2);
    assert!(marks[0].contents.as_deref().unwrap().starts_with("Replaced: \"Monday.\""));
}

#[test]
fn comparing_files_finds_moved_inserted_deleted_and_shifted_pages() {
    use compare::{CompareOptions, PageStatus};
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let a = "Alpha page about the lease terms, the monthly rent, the deposit and the dates it is due.";
    let b = "Bravo page about parking spaces, bicycle storage, the shared laundry room and its hours.";
    let c = "Charlie page about pets, noise after ten at night, smoking and guests staying over weekends.";
    let d = "Delta page with the signatures of both parties and the date the agreement was signed.";
    let v1 = s.create_from_text("t", &[a, b, c, d].join("\u{c}")).unwrap();
    // Charlie moved to the front with one word changed, Bravo deleted, Echo inserted, and
    // Delta's text pushed down the page (same words, different look).
    let c2 = c.replace("pets", "cats");
    let e = "Echo page about the new garden behind the building and who may use it in summer.";
    let d2 = format!("\n\n\n\n{d}");
    let v2 = s.create_from_text("t", &[c2.as_str(), a, e, d2.as_str()].join("\u{c}")).unwrap();
    let old = s.open("v1.pdf", None, v1.clone(), None).unwrap();
    let new = s.open("v2.pdf", None, v2, None).unwrap();

    let opts = CompareOptions { visual: true, ..Default::default() };
    let r = s.compare_files(old, new, &opts).unwrap();
    let map: Vec<(PageStatus, Option<usize>, Option<usize>)> = r.pages.iter().map(|p| (p.status, p.old, p.new)).collect();
    assert_eq!(
        map,
        [
            (PageStatus::Moved, Some(2), Some(0)),
            (PageStatus::Same, Some(0), Some(1)),
            (PageStatus::Deleted, Some(1), None),
            (PageStatus::Inserted, None, Some(2)),
            (PageStatus::Same, Some(3), Some(3)),
        ]
    );
    // The moved page is compared with its counterpart: one word, not a whole page twice.
    let got: Vec<(compare::Kind, &str, usize)> = r.text.changes.iter().map(|x| (x.kind, x.old.text.as_str(), x.new.page)).collect();
    assert_eq!(got[0], (compare::Kind::Replaced, "pets,", 0), "{got:?}");
    assert_eq!(r.text.changes[0].new.text, "cats,");
    // The rest is Bravo's words against Echo's (word by word), all on the inserted page; the
    // unchanged Alpha and Delta pages contribute nothing.
    assert!(got.len() > 1 && got[1..].iter().all(|g| g.2 == 2), "{got:?}");
    assert_eq!(got[1].1, "Bravo");
    assert_eq!(r.text.changes[1].new.text, "Echo");
    // Visually: the edited moved page and the shifted page differ; the unchanged one does not.
    let looks: Vec<(usize, usize)> = r.visual.iter().map(|v| (v.old_page, v.new_page)).collect();
    assert_eq!(looks, [(2, 0), (3, 3)], "{:?}", r.visual);
    assert!(r.visual.iter().all(|v| v.fraction > 0.0 && v.fraction < 0.5 && !v.regions.is_empty()));
    // A threshold above the shares found ignores them.
    let high = s.compare_files(old, new, &CompareOptions { visual: true, threshold: 0.5, ..Default::default() }).unwrap();
    assert!(high.visual.is_empty());
    assert!(!r.identical());

    // Page ranges: old page 1 against new page 2 (the same Alpha page).
    let ranged =
        s.compare_files(old, new, &CompareOptions { old_pages: Some((0, 0)), new_pages: Some((1, 1)), visual: true, ..Default::default() }).unwrap();
    assert!(ranged.identical(), "{ranged:?}");
    assert_eq!(ranged.pages[0].old, Some(0));
    assert_eq!(ranged.pages[0].new, Some(1));
    assert!(s.compare_files(old, new, &CompareOptions { old_pages: Some((2, 9)), ..Default::default() }).is_err());

    // The report lists the page map.
    let report = s.compare_files_report(old, new, &r).unwrap();
    let rid = s.open("report.pdf", None, report, None).unwrap();
    let text = page_texts(&s, rid).join(" ");
    assert!(text.contains("1 inserted, 1 deleted, 1 moved") && text.contains("looks different"), "{text}");

    // The results overlay: a marked-up copy; the open document is untouched.
    let overlay = s.compare_overlay(new, &r).unwrap();
    assert!(!s.get(new).unwrap().dirty);
    assert!(s.get(new).unwrap().info.annotations.is_empty());
    let oid = s.open("overlay.pdf", None, overlay, None).unwrap();
    let marks: Vec<String> =
        s.get(oid).unwrap().info.annotations.iter().filter(|a| a.author.as_deref() == Some("Compare")).filter_map(|a| a.contents.clone()).collect();
    for want in ["Moved page: page 3", "Deleted page: page 2", "Inserted page", "Replaced: \"pets,\"", "Looks different"] {
        assert!(marks.iter().any(|m| m.starts_with(want)), "{want} missing from {marks:?}");
    }

    // A blank page (no text) inserted at the front is found by its picture.
    let blank = s.open("blank.pdf", None, v1, None).unwrap();
    s.apply(blank, Edit::InsertBlankPage { at: 0, width: 612.0, height: 792.0 }).unwrap();
    let bytes = s.save_bytes(blank).unwrap();
    let blank2 = s.open("blank2.pdf", None, bytes, None).unwrap();
    let r = s.compare_files(old, blank2, &CompareOptions::default()).unwrap();
    let map: Vec<PageStatus> = r.pages.iter().map(|p| p.status).collect();
    assert_eq!(map, [PageStatus::Inserted, PageStatus::Same, PageStatus::Same, PageStatus::Same, PageStatus::Same]);
    assert!(r.text.identical());
}

#[test]
fn compare_jobs_report_progress_stop_and_give_the_json_report() {
    use compare::{CompareOptions, report_json};
    let mut s = Session::new();
    let pages: Vec<String> = (0..6).map(|i| format!("Page {i} of the handbook about topic number {i}.")).collect();
    let v1 = s.create_from_text("t", &pages.join("\u{c}")).unwrap();
    let mut changed = pages.clone();
    changed[4] = "Page 4 of the handbook about topic number 44.".into();
    let v2 = s.create_from_text("t", &changed.join("\u{c}")).unwrap();
    let old = s.open("v1.pdf", None, v1, None).unwrap();
    let new = s.open("v2.pdf", None, v2, None).unwrap();
    let opts = CompareOptions { visual: true, ..Default::default() };
    let job = s.compare_job(old, new, &opts).unwrap();
    // Off the session: on another thread, with progress up to the total.
    let (result, seen) = std::thread::spawn(move || {
        let mut seen = Vec::new();
        let r = job.run(|d, t| {
            seen.push((d, t));
            true
        });
        (r, seen)
    })
    .join()
    .unwrap();
    let r = result.unwrap();
    // 6 + 6 pages read, then 6 page pairs rendered.
    assert_eq!(seen.first(), Some(&(0, 18)));
    assert_eq!(seen.last(), Some(&(18, 18)));
    assert!(seen.iter().all(|s| s.1 == 18) && seen.windows(2).all(|w| w[0].0 + 1 == w[1].0), "{seen:?}");
    assert_eq!(r, s.compare_files(old, new, &opts).unwrap());
    assert_eq!(r.visual.iter().map(|v| v.new_page).collect::<Vec<_>>(), [4]);
    // Stopped after three pages: nothing.
    let job = s.compare_job(old, new, &opts).unwrap();
    let mut calls = 0;
    assert!(
        job.run(|d, _| {
            calls += 1;
            d < 3
        })
        .is_none()
    );
    assert_eq!(calls, 4);
    // Bad ranges are refused before any work.
    assert!(s.compare_job(old, new, &CompareOptions { new_pages: Some((3, 6)), ..Default::default() }).is_err());
    // The structured report, with the files' names.
    let v = s.compare_json(old, new, &r, &opts, usize::MAX);
    assert_eq!(v["old"]["name"], "v1.pdf");
    assert_eq!(v["new"]["pages"], 6);
    assert_eq!(v["summary"]["pages_modified"], 1);
    assert_eq!(v["pages"][4]["status"], "modified");
    assert_eq!(v["visual"][0]["new_page"], 5);
    assert_eq!(report_json(&r, &opts, 0)["truncated"], true);
    // Marking: the visual regions the text changes explain aren't marked twice.
    let n = s.mark_file_differences(new, &r).unwrap();
    let explained = r.visual[0].regions.iter().filter(|b| compare::explained_by_text(&r.text, 4, b)).count();
    assert_eq!(n, r.text.changes.len() + r.visual[0].regions.len() - explained);
}

#[test]
fn actions_run_their_steps_on_files() {
    let s = Session::new();
    let src = s.create_from_text("t", "Quarterly numbers").unwrap();
    let built = actions::builtin();
    let numbers = built.iter().find(|a| a.name == "Add Page Numbers").unwrap();
    let r = actions::run_on(numbers, "q.pdf", src.clone(), |_, _| {}).unwrap();
    let mut s2 = Session::new();
    let id = s2.open("out.pdf", None, r.bytes, None).unwrap();
    assert!(page_texts(&s2, id)[0].contains("Page 1 of 1"), "{:?}", page_texts(&s2, id));
    assert_eq!(r.log, ["Add footer"]);

    let custom = actions::Action {
        name: "Mine".into(),
        description: String::new(),
        steps: vec![
            actions::Step::from_id("set_title", "Q3").unwrap(),
            actions::Step::AddWatermark("DRAFT".into()),
            actions::Step::ReduceFileSize,
            actions::Step::FlattenComments,
        ],
        builtin: false,
    };
    let mut steps = Vec::new();
    let r = actions::run_on(&custom, "q.pdf", src, |i, n| steps.push((i, n))).unwrap();
    assert_eq!(steps, [(0, 4), (1, 4), (2, 4), (3, 4)]);
    assert_eq!(r.log.len(), 4, "{:?}", r.log);
    let id = s2.open("out2.pdf", None, r.bytes, None).unwrap();
    let d = s2.get(id).unwrap();
    assert_eq!(d.info.title.as_deref(), Some("Q3"));
    assert!(page_texts(&s2, id)[0].contains("DRAFT"));
    assert!(actions::run_on(&custom, "bad.pdf", Arc::new(b"nope".to_vec()), |_, _| {}).is_err());
}

#[test]
fn the_archive_action_saves_as_pdfa() {
    let s = Session::new();
    let src = s.create_from_text("t", "Board minutes").unwrap();
    let built = actions::builtin();
    let archive = built.iter().find(|a| a.name == "Archive Documents").unwrap();
    let r = actions::run_on(archive, "minutes.pdf", src.clone(), |_, _| {}).unwrap();
    assert_eq!(r.log, ["Save as PDF/A-2b"]);
    let doc = pdfcraft_cos::Document::open(r.bytes.clone()).unwrap();
    assert_eq!(pdfcraft_preflight::declared(&doc).pdfa, Some((2, "B".to_string())));
    // Fix hairlines as a step reports what it found; text pages have none.
    let custom = actions::Action {
        name: "Print".into(),
        description: String::new(),
        steps: vec![actions::Step::from_id("fix_hairlines", "").unwrap()],
        builtin: false,
    };
    assert_eq!(actions::run_on(&custom, "m.pdf", src, |_, _| {}).unwrap().log, ["Fix hairlines: 0"]);
}

#[test]
fn exporting_office_files_keeps_images() {
    let mut s = Session::new();
    let text = s.create_from_text("t", "Picture page").unwrap();
    let id = s.open("t.pdf", None, text, None).unwrap();
    let png = export::Exporter::new(s.get(id).unwrap()).png(0, 20.0).unwrap();
    let pic = s.create_from_images(&[("p.png".into(), png)]).unwrap();
    let pid = s.open("p.pdf", None, pic, None).unwrap();
    let d = s.get(pid).unwrap();
    assert_eq!(d.export_pages()[0].images.len(), 1);
    let docx = d.export_office(compare::OfficeFormat::Docx).unwrap();
    assert!(docx.windows(16).any(|w| w == b"word/media/image"));
    if let Ok(dir) = std::env::var("PDFCRAFT_EXPORT_DIR") {
        std::fs::write(format!("{dir}/pic.docx"), &docx).unwrap();
    }
    assert!(String::from_utf8(d.export_office(compare::OfficeFormat::Html).unwrap()).unwrap().contains("data:image/png;base64,"));
}

/// #314: a page whose content is drawn through a form XObject exports its text (the file from
/// the report: no xref table, wrong stream lengths).
#[test]
fn exporting_office_files_reads_form_xobjects() {
    let pdf = b"%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]/Resources<</XObject<</Fm1 7 0 R>>>>/Contents 5 0 R>> endobj
4 0 obj <</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>> endobj
5 0 obj <</Length 22>> stream
q 1 0 0 1 0 0 cm /Fm1 Do Q
endstream endobj
7 0 obj <</Type/XObject/Subtype/Form/BBox[0 0 595 842]/Resources<</Font<</F1 4 0 R>>>>/Length 60>> stream
BT /F1 18 Tf 72 760 Td (Hello from a test invoice) Tj ET
endstream endobj
trailer <</Root 1 0 R>>
%%EOF
";
    let mut s = Session::new();
    let id = s.open("form.pdf", None, Arc::new(pdf.to_vec()), None).unwrap();
    let d = s.get(id).unwrap();
    let blocks: Vec<String> = d.export_pages()[0].blocks.iter().map(|b| b.text.clone()).collect();
    assert_eq!(blocks, ["Hello from a test invoice"]);
    assert!(String::from_utf8(d.export_office(compare::OfficeFormat::Html).unwrap()).unwrap().contains("Hello from a test invoice"));
    // Edit text still offers only what the page's own streams show.
    assert!(d.text_blocks(0).is_empty());
}

#[test]
fn exporting_office_files_keeps_text_colour() {
    // #526: a dark green heading came out black in Word.
    let content = "BT /F1 24 Tf 0.05 0.23 0.18 rg 72 700 Td (Annual Report) Tj ET \
BT /F1 11 Tf 0 g 72 650 Td (Black body text that is long enough to be the body size.) Tj ET";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".into(),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    let mut s = Session::new();
    let id = s.open("c.pdf", None, Arc::new(pdf), None).unwrap();
    let d = s.get(id).unwrap();
    let html = String::from_utf8(d.export_office(compare::OfficeFormat::Html).unwrap()).unwrap();
    assert!(html.contains("style=\"color:#0D3B2E\">Annual Report"), "{html}");
    let blocks = &d.export_pages()[0].blocks;
    assert!(blocks.iter().any(|b| b.text == "Annual Report" && b.color == [0.05, 0.23, 0.18]), "{blocks:?}");
}

#[test]
fn guard_turns_a_panic_into_an_error() {
    assert_eq!(guard(|| 7), Ok(7));
    assert_eq!(guard(|| -> u8 { panic!("boom") }), Err("boom".to_string()));
    let n = 3;
    assert_eq!(guard(|| -> u8 { panic!("page {n} is bad") }), Err("page 3 is bad".to_string()));
}

#[test]
fn measurement_edits_are_transactional_and_honour_annotation_permissions() {
    let mut session = Session::default();
    let id = session.open("plan.pdf", None, Arc::new(fixture(1)), None).unwrap();
    let m = measure::NewMeasurement {
        page: 0,
        kind: measure::Kind::Distance,
        points: vec![[10.0, 20.0], [40.0, 60.0]],
        scale: measure::Scale::new(0.1, "m", 2).unwrap(),
        style: Style::default(),
        label: "Wall".into(),
        author: "Tester".into(),
    };
    session.apply(id, Edit::AddMeasurement(m.clone())).unwrap();
    let before = session.get(id).unwrap().bytes.clone();
    assert_eq!(session.get(id).unwrap().measurements().unwrap().measurements[0].reading.label, "5.00 m");
    let mut invalid = m.clone();
    invalid.points[1][0] = f64::INFINITY;
    assert!(session.apply(id, Edit::AddMeasurement(invalid)).is_err());
    assert_eq!(session.get(id).unwrap().bytes, before);
    session.undo(id).unwrap();
    assert!(session.get(id).unwrap().measurements().unwrap().measurements.is_empty());
    session.redo(id).unwrap();
    assert_eq!(session.get(id).unwrap().bytes, before);
    let p = pdfcraft_cos::Permissions { bits: 0, owner: false };
    assert_eq!(check_permission(&Edit::AddMeasurement(m), &p), Err(EditError::NotPermitted("comments")));
    assert_eq!(
        check_permission(
            &Edit::SetMeasurementScale { page: 0, bbox: [0.0, 0.0, 100.0, 100.0], name: "Detail".into(), scale: measure::Scale::default() },
            &p
        ),
        Err(EditError::NotPermitted("comments"))
    );
}

#[test]
fn dynamic_xfa_forms_are_laid_out_on_open_filled_and_saved_incrementally() {
    let original = Arc::new(pdfcraft_xfa::fixtures::shell(&pdfcraft_xfa::fixtures::template(3)));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("form.pdf", None, original.clone(), None).expect("opens");
    let doc = s.get(id).unwrap();
    let x = doc.xfa.as_ref().expect("laid out from the template");
    assert_eq!((x.pages, x.fields), (2, 14), "{x:?}");
    assert!(x.warnings.is_empty(), "{:?}", x.warnings);
    assert_eq!(doc.info.pages.len(), 2, "the placeholder page is gone");
    assert_eq!(doc.info.xfa, Some(pdfcraft_render::Xfa::Dynamic), "still an XFA form for other XFA viewers");
    assert!(!doc.dirty, "laying out is not an edit");
    assert!(doc.bytes.starts_with(&original[..]), "the layout is an appended revision; the original bytes stay");
    let names: Vec<&str> = doc.form.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"familyName") && names.contains(&"answer") && names.contains(&"what2"), "{names:?}");
    let family = doc.form.iter().find(|f| f.name == "familyName").unwrap();
    assert_eq!((family.tooltip.as_deref(), family.max_len), (Some("Your family name"), Some(30)));
    let answer = doc.form.iter().find(|f| f.name == "answer").unwrap();
    assert_eq!(answer.widgets.iter().filter_map(|w| w.on_state.clone()).collect::<Vec<_>>(), ["Y", "N"]);
    assert!(page_texts(&s, id)[0].contains("Sample Form"), "{:?}", page_texts(&s, id));
    // Fill it in and save: one more incremental revision on top.
    s.apply(id, Edit::SetFieldValue { name: "familyName".into(), value: FieldValue::Text("Singh".into()) }).unwrap();
    s.apply(id, Edit::SetFieldValue { name: "answer".into(), value: FieldValue::Radio(Some("N".into())) }).unwrap();
    assert!(s.get(id).unwrap().dirty);
    let saved = s.save_bytes(id).unwrap();
    assert!(saved.starts_with(&original[..]));
    // The datasets packet carries the values too, for other XFA viewers.
    let cos = pdfcraft_cos::Document::open(saved.clone()).unwrap();
    let data = pdfcraft_xfa::parse_datasets(&pdfcraft_xfa::read_packets(&cos).unwrap().unwrap().xdp).expect("a datasets packet");
    assert_eq!(data.text_at(&pdfcraft_xfa::som_to_path("form[0].head[0].familyName[0]")), Some("Singh"));
    assert_eq!(data.text_at(&pdfcraft_xfa::som_to_path("form[0].head[0].answer[0]")), Some("N"));
    let id2 = s.open("saved.pdf", None, saved, None).unwrap();
    let d2 = s.get(id2).unwrap();
    assert_eq!(d2.info.pages.len(), 2, "a saved form is not laid out a second time");
    assert_eq!(d2.xfa.as_ref().map(|x| (x.pages, x.fields)), Some((2, 14)), "the layout is remembered");
    assert_eq!(d2.form.iter().find(|f| f.name == "familyName").unwrap().value, vec!["Singh".to_string()]);
    assert_eq!(d2.form.iter().find(|f| f.name == "answer").unwrap().value, vec!["N".to_string()]);
}

#[test]
fn an_xfa_form_without_a_template_opens_with_its_placeholder_and_a_warning() {
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell("<xdp:xdp xmlns:xdp=\"http://ns.adobe.com/xdp/\"></xdp:xdp>"));
    let mut s = Session::new();
    let id = s.open("form.pdf", None, bytes, None).expect("opens");
    let doc = s.get(id).unwrap();
    assert!(doc.xfa.is_none());
    assert_eq!(doc.info.pages.len(), 1);
    assert!(doc.info.warnings.iter().any(|w| w.contains("could not be laid out") && w.contains("no template")), "{:?}", doc.info.warnings);
}

#[test]
fn dynamic_xfa_forms_open_with_the_values_and_rows_of_their_data() {
    let data = "<form><head><familyName>Kaur</familyName><born>2001-02-03</born><answer>Y</answer></head><table><row><what0>a</what0></row><row><what0>b</what0></row><row><what0>c</what0></row></table></form>";
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell(&pdfcraft_xfa::fixtures::template_with_data(1, data)));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("form.pdf", None, bytes, None).expect("opens");
    let doc = s.get(id).unwrap();
    let value = |n: &str| doc.form.iter().find(|f| f.name == n).unwrap_or_else(|| panic!("no field {n}")).value.clone();
    assert_eq!(value("familyName"), vec!["Kaur".to_string()]);
    assert_eq!(value("born"), vec!["2001-02-03".to_string()]);
    assert_eq!(value("answer"), vec!["Y".to_string()]);
    assert_eq!(value("what0_3"), vec!["c".to_string()], "three rows from the data: {:?}", doc.form.iter().map(|f| &f.name).collect::<Vec<_>>());
    assert!(!doc.dirty);
}

#[test]
fn static_xfa_forms_take_their_values_from_the_datasets_and_write_them_back() {
    let original = Arc::new(pdfcraft_xfa::fixtures::static_shell("<form1><page1><name>Ada</name><agree>1</agree></page1></form1>"));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("static.pdf", None, original.clone(), None).expect("opens");
    let doc = s.get(id).unwrap();
    assert_eq!(doc.info.xfa, Some(pdfcraft_render::Xfa::Static));
    assert!(doc.xfa.is_none(), "nothing was laid out");
    assert_eq!(doc.form.iter().find(|f| f.name == "form1[0].page1[0].name[0]").unwrap().value, vec!["Ada".to_string()]);
    assert_eq!(doc.form.iter().find(|f| f.name == "form1[0].page1[0].agree[0]").unwrap().value, vec!["1".to_string()]);
    assert!(!doc.dirty && doc.bytes.starts_with(&original[..]), "the values are an appended revision");
    // Rewriting the fields from the XFA data is recorded, not silent.
    let note = doc.info.warnings.iter().find(|w| w.contains("taken from this form's XFA data")).expect("a warning");
    assert!(note.starts_with("2 form field values") && note.contains("form1[0].page1[0].name[0]"), "{note}");
    s.apply(id, Edit::SetFieldValue { name: "form1[0].page1[0].name[0]".into(), value: FieldValue::Text("Grace".into()) }).unwrap();
    s.apply(id, Edit::SetFieldValue { name: "form1[0].page1[0].agree[0]".into(), value: FieldValue::Check(false) }).unwrap();
    let saved = s.save_bytes(id).unwrap();
    let cos = pdfcraft_cos::Document::open(saved.clone()).unwrap();
    let data = pdfcraft_xfa::parse_datasets(&pdfcraft_xfa::read_packets(&cos).unwrap().unwrap().xdp).unwrap();
    assert_eq!(data.text_at(&pdfcraft_xfa::som_to_path("form1[0].page1[0].name[0]")), Some("Grace"));
    assert_eq!(data.text_at(&pdfcraft_xfa::som_to_path("form1[0].page1[0].agree[0]")), Some(""), "unchecked, with no off value");
    // Reopening agrees with itself: nothing to apply, nothing dirty.
    let id2 = s.open("again.pdf", None, saved, None).unwrap();
    let d2 = s.get(id2).unwrap();
    assert_eq!(d2.form.iter().find(|f| f.name == "form1[0].page1[0].name[0]").unwrap().value, vec!["Grace".to_string()]);
    assert!(!d2.dirty);
    assert!(!d2.info.warnings.iter().any(|w| w.contains("taken from this form's XFA data")), "nothing was rewritten");
}

#[test]
fn xfa_data_no_field_binds_to_survives_an_edit_and_save() {
    // Regression: editing a field replaced the whole data element with the fields' values.
    let data = concat!(
        "<form1><page1><name>Ada</name><agree>1</agree><extra xmlns:my=\"urn:my\"><my:note id=\"7\">keep</my:note></extra></page1>",
        "<history><entry><when>2020</when></entry></history></form1><my:other xmlns:my=\"urn:my\"><my:x>unbound</my:x></my:other>"
    );
    let original = Arc::new(pdfcraft_xfa::fixtures::static_shell(data));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("static.pdf", None, original, None).expect("opens");
    s.apply(id, Edit::SetFieldValue { name: "form1[0].page1[0].name[0]".into(), value: FieldValue::Text("Grace".into()) }).unwrap();
    let saved = s.save_bytes(id).unwrap();
    let cos = pdfcraft_cos::Document::open(saved.clone()).unwrap();
    let xdp = pdfcraft_xfa::read_packets(&cos).unwrap().unwrap().xdp;
    for kept in [
        "<extra xmlns:my=\"urn:my\"><my:note id=\"7\">keep</my:note></extra>",
        "<history><entry><when>2020</when></entry></history>",
        "<my:other xmlns:my=\"urn:my\"><my:x>unbound</my:x></my:other>",
    ] {
        assert!(xdp.contains(kept), "lost {kept:?}: {xdp}");
    }
    let data = pdfcraft_xfa::parse_datasets(&xdp).unwrap();
    assert_eq!(data.text_at(&pdfcraft_xfa::som_to_path("form1[0].page1[0].name[0]")), Some("Grace"));
    assert_eq!(data.text_at(&pdfcraft_xfa::som_to_path("form1[0].page1[0].agree[0]")), Some("1"));
    // A value that can't be written is reported on the document.
    let original = Arc::new(pdfcraft_xfa::fixtures::static_shell("<form1><page1><name><b>rich</b></name></page1></form1>"));
    let id = s.open("rich.pdf", None, original, None).expect("opens");
    s.apply(id, Edit::SetFieldValue { name: "form1[0].page1[0].name[0]".into(), value: FieldValue::Text("plain".into()) }).unwrap();
    let doc = s.get(id).unwrap();
    assert!(doc.info.warnings.iter().any(|w| w.contains("structured content")), "{:?}", doc.info.warnings);
    assert!(doc.xfa_warnings.iter().any(|w| w.contains("structured content")));
}

#[test]
fn xfa_choice_lists_signature_and_picture_fields_open_fill_script_and_save() {
    let data = "<form><page1><country>Japan</country><langs>English\nSpanish</langs></page1></form>";
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell(&pdfcraft_xfa::fixtures::fields_template(data)));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("fields.pdf", None, bytes, None).expect("opens");
    let field = |s: &Session, n: &str| s.get(id).unwrap().form.iter().find(|f| f.name == n).cloned().unwrap_or_else(|| panic!("no field {n}"));
    use pdfcraft_forms::FieldKind as K;
    // The forms layer sees a combo box with its options and the data's saved value, a
    // multi-select list with both picks, an editable combo, a password field and a signature.
    let country = field(&s, "country");
    assert_eq!((country.kind, country.value.clone()), (K::Combo, vec!["JP".to_string()]));
    assert_eq!(country.options, vec![("CA".into(), "Canada".into()), ("FR".into(), "France".into()), ("JP".into(), "Japan".into())]);
    let langs = field(&s, "langs");
    assert_eq!(langs.kind, K::List);
    assert!(langs.has(pdfcraft_forms::flags::MULTI_SELECT));
    assert_eq!(langs.value, vec!["English".to_string(), "Spanish".to_string()]);
    assert!(field(&s, "other").has(pdfcraft_forms::flags::EDIT));
    assert!(field(&s, "pin").has(pdfcraft_forms::flags::PASSWORD));
    assert_eq!(field(&s, "sign").kind, K::Signature);
    // Scripts read a choice list's saved value.
    assert_eq!(field(&s, "summary").value, vec!["Country: JP".to_string()], "{:?}", s.take_js_output(id));
    // Choosing another item recalculates; the list takes several values; a typed value is kept
    // in the editable list and refused in the fixed one.
    s.apply(id, Edit::SetFieldValue { name: "country".into(), value: FieldValue::Choice(vec!["CA".into()]) }).unwrap();
    assert_eq!(field(&s, "summary").value, vec!["Country: CA".to_string()]);
    s.apply(id, Edit::SetFieldValue { name: "langs".into(), value: FieldValue::Choice(vec!["French".into(), "Spanish".into()]) }).unwrap();
    s.apply(id, Edit::SetFieldValue { name: "other".into(), value: FieldValue::Text("typed".into()) }).unwrap();
    assert!(s.apply(id, Edit::SetFieldValue { name: "country".into(), value: FieldValue::Choice(vec!["Mars".into()]) }).is_err());
    // Saved: the datasets hold the saved values, several on separate lines; reopened, the
    // fields and the calculation are what they were.
    let saved = s.save_bytes(id).unwrap();
    let cos = pdfcraft_cos::Document::open(saved.clone()).unwrap();
    let d = pdfcraft_xfa::data_of(&cos).unwrap();
    assert_eq!(d.text_at(&pdfcraft_xfa::som_to_path("form[0].page1[0].country[0]")), Some("CA"));
    assert_eq!(d.text_at(&pdfcraft_xfa::som_to_path("form[0].page1[0].langs[0]")), Some("French\nSpanish"));
    assert_eq!(d.text_at(&pdfcraft_xfa::som_to_path("form[0].page1[0].other[0]")), Some("typed"));
    let id2 = s.open("again.pdf", None, saved.clone(), None).unwrap();
    let d2 = s.get(id2).unwrap();
    let f2 = |n: &str| d2.form.iter().find(|f| f.name == n).unwrap().value.clone();
    assert_eq!(f2("country"), vec!["CA".to_string()]);
    assert_eq!(f2("langs"), vec!["French".to_string(), "Spanish".to_string()]);
    assert_eq!(f2("summary"), vec!["Country: CA".to_string()]);
    // The PNG of the image field and the GIF of the draw are pictures on the page.
    assert_eq!(s.get(id).unwrap().page_images(0).len(), 2);
    // Filled in by another viewer since: saved values, several on separate lines, and shown
    // text (an older viewer's), are taken on reopen; shown text that means the current value
    // is not a change.
    let mut cos = pdfcraft_cos::Document::open(saved).unwrap();
    pdfcraft_xfa::write_data_value(&mut cos, &pdfcraft_xfa::som_to_path("form[0].page1[0].langs[0]"), "English\nSpanish").unwrap();
    pdfcraft_xfa::write_data_value(&mut cos, &pdfcraft_xfa::som_to_path("form[0].page1[0].country[0]"), "Canada").unwrap();
    let edited = Arc::new(pdfcraft_cos::write_incremental(&cos, &pdfcraft_cos::SaveOptions::default()).unwrap());
    let id3 = s.open("elsewhere.pdf", None, edited, None).unwrap();
    let d3 = s.get(id3).unwrap();
    let f3 = |n: &str| d3.form.iter().find(|f| f.name == n).unwrap().value.clone();
    assert_eq!(f3("langs"), vec!["English".to_string(), "Spanish".to_string()]);
    assert_eq!(f3("country"), vec!["CA".to_string()]);
    let taken: Vec<&String> = d3.info.warnings.iter().filter(|w| w.contains("another viewer")).collect();
    assert_eq!(taken.len(), 1, "{:?}", d3.info.warnings);
    assert!(taken[0].contains("langs") && !taken[0].contains("country"), "{}", taken[0]);
}

#[test]
fn xfa_scripts_initialize_calculate_validate_toggle_and_add_rows() {
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell(&pdfcraft_xfa::fixtures::scripted_template()));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("scripted.pdf", None, bytes, None).expect("opens");
    let value = |s: &Session, n: &str| {
        s.get(id).unwrap().form.iter().find(|f| f.name == n).map(|f| f.value.clone()).unwrap_or_else(|| panic!("no field {n}"))
    };
    let names = |s: &Session| s.get(id).unwrap().form.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
    // On open: the initialize script set qty, the calculations ran, nothing is dirty.
    assert_eq!(value(&s, "qty"), vec!["2".to_string()]);
    assert_eq!(value(&s, "total"), vec!["10".to_string()], "{:?}", s.take_js_output(id));
    assert_eq!(value(&s, "grand"), vec!["0".to_string()]);
    assert!(!s.get(id).unwrap().dirty);
    assert!(!names(&s).iter().any(|n| n == "note"), "details is hidden");
    // A change recalculates.
    s.apply(id, Edit::SetFieldValue { name: "qty".into(), value: FieldValue::Text("3".into()) }).unwrap();
    assert_eq!(value(&s, "total"), vec!["15".to_string()], "{:?}", s.take_js_output(id));
    // A failing validate script shows its message; the value stays, as in other viewers.
    s.apply(id, Edit::SetFieldValue { name: "qty".into(), value: FieldValue::Text("500".into()) }).unwrap();
    let out = s.take_js_output(id);
    assert!(out.alerts.iter().any(|a| a == "Quantity must be 100 or less"), "{out:?}");
    assert_eq!(value(&s, "total"), vec!["2500".to_string()]);
    // A button adds a row: new fields, same page, one undo step.
    let before = names(&s).len();
    let o = s.run_javascript(id, "", Some("addRow")).unwrap();
    assert_eq!(o.error, None);
    let after = names(&s);
    assert_eq!(after.len(), before + 2, "{after:?}");
    assert!(after.iter().any(|n| n == "amount_2"));
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Run form script"));
    s.apply(id, Edit::SetFieldValue { name: "amount".into(), value: FieldValue::Text("10".into()) }).unwrap();
    s.apply(id, Edit::SetFieldValue { name: "amount_2".into(), value: FieldValue::Text("32".into()) }).unwrap();
    assert_eq!(value(&s, "grand"), vec!["42".to_string()], "{:?}", s.take_js_output(id));
    // Message boxes reach the viewer.
    let o = s.run_javascript(id, "", Some("hello")).unwrap();
    assert_eq!(o.alerts, vec!["Hello 500".to_string()]);
    // The check box shows the hidden subform; undo hides it again.
    s.apply(id, Edit::SetFieldValue { name: "more".into(), value: FieldValue::Check(true) }).unwrap();
    assert!(names(&s).iter().any(|n| n == "note"), "{:?} {:?}", names(&s), s.take_js_output(id));
    s.undo(id).unwrap();
    assert!(!names(&s).iter().any(|n| n == "note"));
    s.redo(id).unwrap();
    assert!(names(&s).iter().any(|n| n == "note"));
    // Removing the row takes its amount out of the sum.
    s.run_javascript(id, "", Some("removeRow")).unwrap();
    assert!(!names(&s).iter().any(|n| n == "amount_2"));
    assert_eq!(value(&s, "grand"), vec!["10".to_string()]);
    // FormCalc scripts run too.
    let o = s.run_javascript(id, "", Some("legacy")).unwrap();
    assert_eq!(o.error, None);
    assert_eq!(o.alerts, vec!["FormCalc".to_string()]);
    // Saved and reopened: the visible subform, the values and the rows are what they were.
    let saved = s.save_bytes(id).unwrap();
    let id2 = s.open("again.pdf", None, saved, None).unwrap();
    let d2 = s.get(id2).unwrap();
    let n2: Vec<&str> = d2.form.iter().map(|f| f.name.as_str()).collect();
    assert!(n2.contains(&"note") && !n2.contains(&"amount_2"), "{n2:?}");
    assert_eq!(d2.form.iter().find(|f| f.name == "grand").unwrap().value, vec!["10".to_string()]);
    assert_eq!(d2.form.iter().find(|f| f.name == "qty").unwrap().value, vec!["500".to_string()]);
    assert!(!d2.dirty);
}

#[test]
fn dynamic_xfa_forms_run_formcalc_scripts() {
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell(&pdfcraft_xfa::fixtures::formcalc_template()));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("formcalc.pdf", None, bytes, None).expect("opens");
    let value = |s: &Session, n: &str| s.get(id).unwrap().form.iter().find(|f| f.name == n).unwrap().value.clone();
    let names = |s: &Session| s.get(id).unwrap().form.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
    // Initialize set qty; the calculates ran from it, including the English words.
    assert_eq!(value(&s, "qty"), vec!["2".to_string()], "{:?}", s.take_js_output(id));
    assert_eq!(value(&s, "total"), vec!["10".to_string()]);
    assert_eq!(value(&s, "words"), vec!["Ten".to_string()]);
    assert!(s.take_js_output(id).is_empty());
    // Editing recalculates and validates.
    s.apply(id, Edit::SetFieldValue { name: "qty".into(), value: FieldValue::Text("7".into()) }).unwrap();
    assert_eq!(value(&s, "total"), vec!["35".to_string()]);
    assert_eq!(value(&s, "words"), vec!["Thirty-Five".to_string()]);
    s.apply(id, Edit::SetFieldValue { name: "qty".into(), value: FieldValue::Text("500".into()) }).unwrap();
    let out = s.take_js_output(id);
    assert!(out.alerts.iter().any(|a| a == "Quantity must be 100 or less"), "{out:?}");
    assert_eq!(value(&s, "qty"), vec!["500".to_string()]);
    // Buttons: a row is added, summed and removed; the message box reaches the viewer.
    let before = names(&s).len();
    let o = s.run_javascript(id, "", Some("addRow")).unwrap();
    assert_eq!(o.error, None, "{o:?}");
    assert_eq!(names(&s).len(), before + 2, "{:?}", names(&s));
    s.apply(id, Edit::SetFieldValue { name: "amount".into(), value: FieldValue::Text("10".into()) }).unwrap();
    s.apply(id, Edit::SetFieldValue { name: "amount_2".into(), value: FieldValue::Text("32".into()) }).unwrap();
    assert_eq!(value(&s, "grand"), vec!["42".to_string()], "{:?}", s.take_js_output(id));
    let o = s.run_javascript(id, "", Some("hello")).unwrap();
    assert_eq!(o.alerts, vec!["Hello 500".to_string()]);
    s.apply(id, Edit::SetFieldValue { name: "more".into(), value: FieldValue::Check(true) }).unwrap();
    assert!(names(&s).iter().any(|n| n == "note"), "{:?} {:?}", names(&s), s.take_js_output(id));
    s.run_javascript(id, "", Some("removeRow")).unwrap();
    assert!(!names(&s).iter().any(|n| n == "amount_2"));
    assert_eq!(value(&s, "grand"), vec!["10".to_string()]);
    // Saved and reopened, the form is what it was.
    let saved = s.save_bytes(id).unwrap();
    let id2 = s.open("again.pdf", None, saved, None).unwrap();
    let d2 = s.get(id2).unwrap();
    assert_eq!(d2.form.iter().find(|f| f.name == "words").unwrap().value, vec!["Two Thousand Five Hundred".to_string()]);
    assert!(d2.form.iter().any(|f| f.name == "note"));
}

#[test]
fn hostile_formcalc_scripts_cannot_hang_or_bloat_the_document() {
    let tpl = pdfcraft_xfa::fixtures::formcalc_template()
        .replace(
            "<calculate><script>qty * price</script></calculate>",
            "<calculate><script>table._row.addInstance(1)
table._row.count</script></calculate>",
        )
        .replace("<script>table._row.addInstance(1)</script>", "<script>while (1) do table._row.addInstance(1) endwhile</script>")
        .replace(
            "<script>WordNum(total)</script>",
            "<script>func f(n) do f(n + 1) endfunc
f(1)</script>",
        );
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell(&tpl));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let started = std::time::Instant::now();
    let id = s.open("hostile.pdf", None, bytes, None).expect("opens");
    let rows = |s: &Session| s.get(id).unwrap().form.iter().filter(|f| f.name.starts_with("amount")).count();
    assert!(rows(&s) <= 50, "{} rows", rows(&s));
    let out = s.take_js_output(id);
    assert!(out.errors.iter().any(|l| l.contains("nested")), "{out:?}");
    let o = s.run_javascript(id, "", Some("addRow")).unwrap();
    assert!(o.error.is_some() || rows(&s) <= 50, "{o:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(60), "{:?}", started.elapsed());
}

#[test]
fn a_formcalc_calculate_result_is_cut_before_it_is_saved() {
    let tpl = pdfcraft_xfa::fixtures::formcalc_template()
        .replace("<script>WordNum(total)</script>", "<script>Replace(Space(100000), \" \", \"x\")</script>");
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell(&tpl));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("long.pdf", None, bytes, None).expect("opens");
    let words = |s: &Session, id| s.get(id).unwrap().form.iter().find(|f| f.name == "words").unwrap().value.concat();
    assert_eq!(words(&s, id).chars().count(), 65_536);
    let out = s.take_js_output(id);
    assert!(out.errors.iter().any(|e| e.contains("cut")), "{out:?}");
    let saved = s.save_bytes(id).unwrap();
    let id2 = s.open("again.pdf", None, saved, None).unwrap();
    assert!(words(&s, id2).chars().count() <= 65_536);
}

#[test]
fn xfa_scripts_stay_off_with_javascript_off() {
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell(&pdfcraft_xfa::fixtures::scripted_template()));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    s.set_javascript(false);
    let id = s.open("scripted.pdf", None, bytes, None).expect("opens");
    let qty = s.get(id).unwrap().form.iter().find(|f| f.name == "qty").unwrap().value.clone();
    assert!(qty.is_empty(), "no initialize script ran");
    assert!(s.run_javascript(id, "", Some("addRow")).is_err());
}

#[test]
fn hostile_xfa_scripts_cannot_hang_or_bloat_the_document() {
    // A calculate script that adds a row every time it runs, a click script that loops on
    // addInstance, and one that nests setInstances: each ends quickly with a bounded form.
    let tpl = pdfcraft_xfa::fixtures::scripted_template()
        .replace(
            r#"<calculate><script contentType="application/x-javascript">qty.rawValue * price.rawValue</script></calculate>"#,
            r#"<calculate><script contentType="application/x-javascript">table._row.addInstance(1); table._row.count</script></calculate>"#,
        )
        .replace(
            r#"table._row.addInstance(1);</script>"#,
            r#"for (var i = 0; i < 100000; i++) { table._row.addInstance(1); table._row.setInstances(0); }</script>"#,
        );
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell(&tpl));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let started = std::time::Instant::now();
    let id = s.open("hostile.pdf", None, bytes, None).expect("opens");
    let rows = |s: &Session| s.get(id).unwrap().form.iter().filter(|f| f.name.starts_with("amount")).count();
    assert!(rows(&s) <= 50, "{} rows", rows(&s));
    let o = s.run_javascript(id, "", Some("addRow")).unwrap();
    assert!(o.error.is_some() || rows(&s) <= 50, "{o:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(60), "{:?}", started.elapsed());
}

#[test]
fn xfa_scripts_can_remove_rows_twice_and_reset_fields_by_name() {
    let tpl = pdfcraft_xfa::fixtures::scripted_template()
        .replace(r#"if (table._row.count > 1) { table._row.removeInstance(table._row.count - 1); }"#, r#"table._row.setInstances(1);"#)
        .replace(r#"xfa.host.messageBox("Hello " + qty.rawValue);"#, r#"xfa.host.resetData("qty, table");"#);
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell(&tpl));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("rows.pdf", None, bytes, None).expect("opens");
    let rows = |s: &Session| s.get(id).unwrap().form.iter().filter(|f| f.name.starts_with("amount")).count();
    s.run_javascript(id, "", Some("addRow")).unwrap();
    s.run_javascript(id, "", Some("addRow")).unwrap();
    assert_eq!(rows(&s), 3);
    s.apply(id, Edit::SetFieldValue { name: "amount".into(), value: FieldValue::Text("7".into()) }).unwrap();
    // setInstances(1) removes two rows in one script: both go, nothing comes back.
    s.run_javascript(id, "", Some("removeRow")).unwrap();
    assert_eq!(rows(&s), 1, "{:?}", s.get(id).unwrap().form.iter().map(|f| &f.name).collect::<Vec<_>>());
    let cos = pdfcraft_cos::Document::open(s.get(id).unwrap().bytes.clone()).unwrap();
    let data = pdfcraft_xfa::data_of(&cos).unwrap();
    assert_eq!(data.count(&pdfcraft_xfa::som_to_path("form[0].page1[0].table[0]"), "row"), 1);
    // resetData with names clears those fields (the row's amount too) and nothing else.
    s.apply(id, Edit::SetFieldValue { name: "price".into(), value: FieldValue::Text("9".into()) }).unwrap();
    let o = s.run_javascript(id, "", Some("hello")).unwrap();
    let value = |s: &Session, n: &str| s.get(id).unwrap().form.iter().find(|f| f.name == n).unwrap().value.clone();
    assert!(value(&s, "qty").is_empty(), "{:?} {o:?}", value(&s, "qty"));
    assert!(value(&s, "amount").is_empty());
    assert_eq!(value(&s, "price"), vec!["9".to_string()]);
}

/// Streams in `bytes` (every revision) that hold an XFA datasets packet.
fn datasets_streams(bytes: &Arc<Vec<u8>>) -> usize {
    let cos = pdfcraft_cos::Document::open(bytes.clone()).unwrap();
    cos.object_numbers()
        .into_iter()
        .filter(|&n| {
            cos.try_get(n).ok().is_some_and(|o| match &*o {
                pdfcraft_cos::Object::Stream(s) => s.decoded_within(1 << 24).is_ok_and(|b| b.windows(9).any(|w| w == b"datasets>")),
                _ => false,
            })
        })
        .count()
}

#[test]
fn hostile_xfa_open_scripts_are_merged_capped_kept_from_printing_and_noted() {
    // On open: a calculate setting its own value 50 000 times, an initialize script showing a
    // thousand messages and asking to print, open a link and save.
    let tpl = pdfcraft_xfa::fixtures::scripted_template()
        .replace(
            r#"<calculate><script contentType="application/x-javascript">qty.rawValue * price.rawValue</script></calculate>"#,
            r#"<calculate><script contentType="application/x-javascript">for (var i = 0; i &lt; 50000; i++) this.rawValue = i;</script></calculate>"#,
        )
        .replace(
            r#"if (qty.rawValue === null) qty.rawValue = 2;"#,
            r#"for (var i = 0; i &lt; 1000; i++) xfa.host.messageBox("m" + i); xfa.host.print(); xfa.host.gotoURL("https://example.com/"); app.execMenuItem("SaveAs"); if (qty.rawValue === null) qty.rawValue = 2;"#,
        );
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell(&tpl));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let started = std::time::Instant::now();
    let id = s.open("hostile.pdf", None, bytes, None).expect("opens");
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
    let doc = s.get(id).unwrap();
    assert_eq!(doc.form.iter().find(|f| f.name == "total").unwrap().value, vec!["49999".to_string()]);
    // One datasets stream for the scripts' revision, not one per value set.
    assert!(datasets_streams(&doc.bytes) <= 3, "{} datasets streams", datasets_streams(&doc.bytes));
    // What opening changed is noted with the form's other XFA notes.
    assert!(doc.xfa_warnings.iter().any(|w| w.contains("scripts changed it on opening")), "{:?}", doc.xfa_warnings);
    let out = s.take_js_output(id);
    assert_eq!(out.alerts.len(), 100, "message boxes are capped");
    assert!(out.errors.iter().any(|e| e.contains("more than 100 messages")), "{:?}", out.errors);
    assert!(out.requests.is_empty(), "only a click may print, save or open links: {:?}", out.requests);
    assert!(out.console.iter().any(|c| c.contains("only print when a button is clicked")), "{:?}", out.console);
}

#[test]
fn runaway_xfa_calculations_at_open_end_quickly_with_a_report() {
    // Nested loops in one frame stop at the loop limit; loops in nested calls (that limit is per
    // call) are abandoned at the time limit and turn the form's scripts off.
    let tpl = pdfcraft_xfa::fixtures::scripted_template()
        .replace(
            r#"<calculate><script contentType="application/x-javascript">qty.rawValue * price.rawValue</script></calculate>"#,
            r#"<calculate><script contentType="application/x-javascript">for (var i = 0; i &lt; 1e9; i++) { for (var j = 0; j &lt; 1e9; j++) {} }</script></calculate>"#,
        )
        .replace(
            r#"var t = 0; var rows"#,
            r#"function f() { for (var j = 0; j &lt; 99999; j++) {} } for (var k = 0; k &lt; 5000; k++) f(); var t = 0; var rows"#,
        );
    let bytes = Arc::new(pdfcraft_xfa::fixtures::shell(&tpl));
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let started = std::time::Instant::now();
    let id = s.open("runaway.pdf", None, bytes, None).expect("opens");
    assert!(started.elapsed() < std::time::Duration::from_secs(4), "{:?}", started.elapsed());
    let out = s.take_js_output(id);
    assert!(out.errors.iter().any(|e| e.contains("total") && e.contains("maximum number of iteration")), "{:?}", out.errors);
    assert!(out.errors.iter().any(|e| e.contains("abandoned")), "{:?}", out.errors);
    assert!(out.errors.iter().any(|e| e.contains("scripts are off")), "{:?}", out.errors);
    // The scripts stay off: a change runs nothing more.
    let started = std::time::Instant::now();
    s.apply(id, Edit::SetFieldValue { name: "qty".into(), value: FieldValue::Text("3".into()) }).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(s.take_js_output(id).errors.is_empty());
}

/// The page view's raster of `page` at 1 px/pt (the document's own render pool).
fn shown(s: &Session, id: DocId, page: usize) -> pdfcraft_render::RenderedPage {
    let doc = s.get(id).unwrap();
    doc.renderer.set_queue(vec![pdfcraft_render::RenderRequest { page, scale: 1.0, ..Default::default() }]);
    let start = std::time::Instant::now();
    loop {
        if let Some(p) = doc.renderer.try_recv() {
            assert!(p.error.is_none(), "{:?}", p.error);
            return p;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(20), "no render");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// #260 (page 7): comments without an appearance stream, here a FreeText box (`/C` background,
/// blue `/DA` text) and an Ink stroke, weren't drawn. The page view now draws them from
/// appearances made for display only; the working file and what Save writes stay as they were.
#[test]
fn comments_without_appearances_are_drawn_but_not_saved() {
    let objs: Vec<&[u8]> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>",
        b"<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R] >>",
        b"<< /Type /Annot /Subtype /FreeText /Rect [20 200 180 260] /Contents (Box) /DA (/Helv 10 Tf 0 0 1 rg) /Border [0 0 1] /C [.85 .47 .02] >>",
        b"<< /Type /Annot /Subtype /Ink /Rect [20 20 180 120] /InkList [[30 30 100 110 170 30]] /Border [0 0 4] /C [0 .6 0] >>",
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        bytes.extend_from_slice(o);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        bytes.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    let original = Arc::new(bytes);
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("comments.pdf", None, original.clone(), None).expect("opens");
    let check = |s: &Session| {
        let p = shown(s, id, 0);
        let px = |x: u32, y: u32| p.rgba[((y * p.width + x) * 4) as usize..][..4].to_vec();
        // y down: the FreeText box spans rows 40..100, the Ink apex is at (100, 190).
        assert_eq!(px(100, 90), vec![217, 120, 5, 255], "the FreeText background");
        // On the apex row: Ink is drawn as a curve through its points (#374), level at the apex.
        assert_eq!(px(100, 190), vec![0, 153, 0, 255], "the Ink stroke");
    };
    check(&s);
    let doc = s.get(id).unwrap();
    assert!(Arc::ptr_eq(&doc.bytes, &original), "the working file is the file as opened");
    assert!(!Arc::ptr_eq(&doc.display, &doc.bytes) && Arc::ptr_eq(&doc.export_source().bytes, &doc.display), "page images show it too");
    // After an edit (a scoped refresh) they are still drawn, and Save writes no appearances.
    s.apply(id, rect_comment(0, [150.0, 270.0, 190.0, 290.0])).unwrap();
    check(&s);
    let saved = s.save_bytes(id).unwrap();
    let reopened = pdfcraft_cos::Document::open(saved).unwrap();
    for r in [pdfcraft_cos::ObjRef::new(4, 0), pdfcraft_cos::ObjRef::new(5, 0)] {
        assert!(reopened.get(r).as_dict().is_some_and(|d| !d.contains(b"AP")), "{r:?} is saved without /AP");
    }
}

/// Titles a browser writes for a blank page or a pop-up name no document; real titles stay.
#[test]
fn placeholder_titles_are_recognised() {
    for t in ["about:blank", "About:Blank", " about:srcdoc ", "blob:https://bank.example/3f2a", "data:text/html,x", "Untitled", "untitled"] {
        assert!(is_placeholder_title(t), "{t:?}");
    }
    for t in ["Quarterly report", "Untitled report", "About: our company", "Statement", "blank"] {
        assert!(!is_placeholder_title(t), "{t:?}");
    }
}

/// A form whose fields are only page widgets (the `/Fields` list is empty): they are adopted, and
/// the leniency is recorded in the repair log rather than applied silently.
#[test]
fn adopting_page_only_fields_is_noted_as_a_repair() {
    let objs: Vec<&str> = vec![
        "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>",                                     // 1
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>",                     // 2
        "<< /Type /Page /Parent 2 0 R /Annots [5 0 R 6 0 R] >>",                                 // 3
        "<< /Fields [] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 7 0 R >> >> >>",               // 4
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (alpha) /Rect [10 200 90 220] /P 3 0 R >>", // 5
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (beta) /Rect [10 150 90 170] /P 3 0 R >>",  // 6
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",                                // 7
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        bytes.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("orphan.pdf", None, Arc::new(bytes), None).expect("opens");
    let doc = s.get(id).unwrap();
    let names: Vec<&str> = doc.form.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta"]);
    assert!(doc.repair_log().iter().any(|l| l.contains("2 fields") && l.contains("page annotations")), "{:?}", doc.repair_log());
}

/// A page too large for the renderer at the asked resolution exports at the most it allows (and
/// says at what), as File ▸ Export does; a source that asks for strictness refuses it instead.
#[test]
fn oversized_exports_clamp_and_report_their_dpi_unless_strict() {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    // 14,400 pt (200 in) wide: 600 dpi would be 120,000 px.
    let bytes = s.create_blank(14_400.0, 792.0, 1).unwrap();
    let id = s.open("wide.pdf", None, bytes, None).unwrap();
    let src = s.get(id).unwrap().export_source();
    let mut ex = export::Exporter::from_source(src.clone());
    let used = ex.dpi_used(0, 600.0);
    assert!(used < 600.0 && used > 0.0, "{used}");
    let png = ex.png(0, 600.0).expect("clamped, not refused");
    assert!(png.starts_with(b"\x89PNG"));
    assert!((ex.dpi_used(0, 18.0) - 18.0).abs() < 0.01, "a small enough request (3,600 px) is drawn as asked");
    let mut strict = src;
    strict.config.reject_oversize = true;
    let err = export::Exporter::from_source(strict).png(0, 600.0).unwrap_err();
    assert!(err.contains("exceeds renderer limits"), "{err}");
}

#[test]
fn page_transitions_are_one_undo_step_saved_and_permission_checked() {
    let (mut s, id) = session_with(3);
    let mut t = Transition::new(TransitionStyle::Split);
    t.dimension = Dimension::Vertical;
    t.motion = Motion::Outward;
    t.duration = 1.5;
    let settings = PageTransition { transition: Some(t), advance: Some(3.0) };
    s.apply(id, Edit::SetPageTransition { pages: vec![0, 2], settings }).unwrap();
    let d = s.get(id).unwrap();
    assert_eq!(d.can_undo(), Some("Set page transitions"));
    assert_eq!(d.page_transitions(), [settings, PageTransition::default(), settings]);
    // A refused edit leaves the document as it was.
    let bad = PageTransition { transition: None, advance: Some(f64::INFINITY) };
    assert!(s.apply(id, Edit::SetPageTransition { pages: vec![1], settings: bad }).is_err());
    assert_eq!(s.get(id).unwrap().page_transitions()[1], PageTransition::default());
    let saved = s.save_bytes(id).unwrap();
    let mut s2 = Session::new();
    let id2 = s2.open("t.pdf", None, saved, None).unwrap();
    assert_eq!(s2.get(id2).unwrap().page_transitions()[2], settings);
    s.undo(id).unwrap();
    assert!(s.get(id).unwrap().page_transitions().iter().all(|p| *p == PageTransition::default()));
    s.redo(id).unwrap();
    assert_eq!(s.get(id).unwrap().page_transitions()[0], settings);
    // Restricted documents refuse it like other changes to the document (Table 22 bit 4).
    let mut s3 = Session::new();
    let id3 = s3.open("r.pdf", None, protected("", "owner", 0b0100), None).unwrap();
    assert_eq!(s3.apply(id3, Edit::SetPageTransition { pages: vec![0], settings }), Err(EditError::NotPermitted("changes to the document")));
}

/// RGBA at PDF point (x, y) of `page`, rendered at 1 px/pt, on a page `height` points high.
fn pixel_on(s: &Session, id: DocId, page: usize, height: u32, x: u32, y: u32) -> [u8; 4] {
    let doc = s.get(id).unwrap();
    let mut r = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), doc.config.clone());
    let out = r.render(pdfcraft_render::RenderRequest { page, scale: 1.0, ..Default::default() });
    assert!(out.error.is_none(), "{:?}", out.error);
    let i = (((height - y) * out.width + x) * 4) as usize;
    out.rgba[i..i + 4].try_into().unwrap()
}

#[test]
fn comment_summaries_lay_out_the_pages_with_connector_lines_or_sequence_numbers() {
    let (mut s, id) = session_with(3);
    s.apply(id, rect_comment(1, [80.0, 80.0, 120.0, 120.0])).unwrap();
    s.apply(id, Edit::ReplyToAnnotation { page: 1, index: 0, text: "Agreed".into(), author: "Ada".into() }).unwrap();
    let red = Style { color: [0.89, 0.13, 0.13], opacity: 1.0, width: 1.0, fill: None };
    let blue = Style { color: [0.0, 0.47, 0.84], opacity: 1.0, width: 1.0, fill: None };
    let quads = vec![[20.0, 172.0, 100.0, 172.0, 20.0, 148.0, 100.0, 148.0]];
    s.apply(id, Edit::ReplaceText { page: 2, quads, text: "Sheet 3".into(), author: "Bo".into(), strike: red, caret: blue }).unwrap();

    // Comments only: the replacement is listed under its proposal.
    let text = comment_summary("doc.pdf", &s.get(id).unwrap().info.annotations, SummarySort::Page);
    assert!(text.contains("Subject: Replace Text") && text.contains("Subject: Replacement Text") && text.contains("    Sheet 3"), "{text}");

    // Connector lines: one landscape page per commented page (page 1 has none).
    let pdf = s.summarize_comments_with(id, SummarySort::Page, SummaryLayout::ConnectorLines).unwrap();
    let n = s.open_new("summary.pdf", pdf).unwrap();
    let pages = &s.get(n).unwrap().info.pages;
    assert_eq!(pages.len(), 2);
    assert!(pages.iter().all(|p| (p.width, p.height) == (792.0, 612.0)), "{pages:?}");
    let texts = page_texts(&s, n);
    for want in ["Summary of Comments on fixture.pdf", "Page 2", "Rectangle", "Reviewer", "Look here", "Reply", "Agreed"] {
        assert!(texts[0].contains(want), "{want:?} in {:?}", texts[0]);
    }
    for want in ["Page 3", "Replace Text", "Replacement Text", "Sheet 3"] {
        assert!(texts[1].contains(want), "{want:?} in {:?}", texts[1]);
    }
    // The page is drawn at 1.76× in a 360 × 528 area at (40, 36), its comments flattened in:
    // the rectangle's centre (100, 100) lands at (216, 212).
    let px = pixel_on(&s, n, 0, 612, 216, 205);
    assert!(px[0] > 200 && px[1] < 60 && px[2] < 60, "the commented page is drawn with its comment: {px:?}");
    // The comment box: a light tint of the comment's red at the top of the right column.
    let px = pixel_on(&s, n, 0, 612, 750, 556);
    assert!(px[0] > 240 && (200..240).contains(&px[1]) && (200..240).contains(&px[2]), "tinted comment box: {px:?}");
    // The connector line runs from the marker to the box (red, between the page and the column).
    let doc = s.get(n).unwrap();
    let mut r = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), doc.config.clone());
    let out = r.render(pdfcraft_render::RenderRequest { page: 0, scale: 1.0, ..Default::default() });
    let column_x = 420u32;
    let reds = (0..out.height)
        .filter(|y| {
            let i = ((y * out.width + column_x) * 4) as usize;
            let p = &out.rgba[i..i + 4];
            p[0] > 150 && p[1] < 120 && p[2] < 120
        })
        .count();
    assert!(reds >= 1, "a connector crosses the gap between page and comments");

    // Sequence numbers: the page at its own size with markers, then its comments.
    let pdf = s.summarize_comments_with(id, SummarySort::Author, SummaryLayout::SequenceNumbers).unwrap();
    let q = s.open_new("summary2.pdf", pdf).unwrap();
    let pages = &s.get(q).unwrap().info.pages;
    let sizes: Vec<(f32, f32)> = pages.iter().map(|p| (p.width, p.height)).collect();
    assert_eq!(sizes, [(200.0, 300.0), (612.0, 792.0), (200.0, 300.0), (612.0, 792.0)]);
    let texts = page_texts(&s, q);
    assert!(texts[0].contains("Page 2") && texts[0].contains("1"), "{:?}", texts[0]);
    assert!(texts[1].contains("Look here") && texts[3].contains("Sheet 3"), "{texts:?}");
    assert!(s.summarize_comments_with(id, SummarySort::Page, SummaryLayout::CommentsOnly).is_ok());

    // Long comments continue on the next summary page.
    let long = (0..120).map(|i| format!("line {i} of a long remark")).collect::<Vec<_>>().join("\n");
    s.apply(id, Edit::SetAnnotationContents { page: 1, index: 0, text: long }).unwrap();
    let pdf = s.summarize_comments_with(id, SummarySort::Page, SummaryLayout::ConnectorLines).unwrap();
    let l = s.open_new("summary3.pdf", pdf).unwrap();
    let texts = page_texts(&s, l);
    assert_eq!(texts.len(), 4, "page 2's comment needs three columns, then page 3");
    assert!(texts[1].contains("1 (continued)") && texts[2].contains("line 119"), "{texts:?}");

    // No comments: the comments-only summary says so.
    let (s2, id2) = session_with(1);
    let mut s2 = s2;
    let pdf = s2.summarize_comments_with(id2, SummarySort::Page, SummaryLayout::ConnectorLines).unwrap();
    let e = s2.open_new("empty.pdf", pdf).unwrap();
    assert!(page_texts(&s2, e)[0].contains("This document has no comments."));
    assert_eq!(SummaryLayout::parse("Connector-Lines"), Some(SummaryLayout::ConnectorLines));
}

// ── Certificate (public-key) security, ISO 32000-2 §7.6.5 ────────────────────────────────────

/// A throwaway RSA digital ID generated at run time (no real identity).
fn test_digital_id(cn: &str, serial: u8) -> pdfcraft_sign::DigitalId {
    let key = pdfcraft_sign::PrivateKey::generate_rsa(2048).unwrap();
    let name = pdfcraft_sign::Name::build(cn, "", "PdfCraft test identities", "", "");
    let from = pdfcraft_sign::Time { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
    let certificate = pdfcraft_sign::Certificate::self_signed(&name, &key, from, 2, &[serial]).unwrap();
    pdfcraft_sign::DigitalId { key, certificate, chain: Vec::new(), friendly_name: None }
}

fn recipient(id: &pdfcraft_sign::DigitalId, access: RecipientAccess) -> CertificateRecipient {
    CertificateRecipient { certificate: id.certificate.clone(), access }
}

#[test]
fn certificate_security_protects_saves_and_opens_with_each_recipients_permissions() {
    let boss = test_digital_id("Boss", 1);
    let reader = test_digital_id("Reader", 2);
    let stranger = test_digital_id("Stranger", 3);
    let (mut s, id) = session_with(2);
    let reader_access = RecipientAccess { printing: Printing::Low, changes: Changes::CommentFillSign, ..RecipientAccess::default() };
    let p = CertificateProtection {
        recipients: vec![recipient(&boss, RecipientAccess::FULL), recipient(&reader, reader_access)],
        algorithm: Algorithm::Aes256,
        encrypt_metadata: true,
    };
    s.apply(id, Edit::ProtectCertificates(p.clone())).unwrap();
    let d = s.get(id).unwrap();
    assert!(d.certificate_secured() && d.info.encrypted);
    assert_eq!(d.security_summary().unwrap().method, "Certificate security (AES, 256-bit)");
    assert!(d.allows_security_change(), "the author keeps full rights");
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 2"], "viewable through the decrypted copy");
    let saved = s.save_bytes(id).unwrap();
    let raw = String::from_utf8_lossy(&saved).into_owned();
    assert!(raw.contains("/Adobe.PubSec") && !raw.contains("(Page 1)"), "encrypted on disk");
    assert!(pdfcraft_cos::Document::open(saved.clone()).is_err());
    s.mark_saved(id, saved.clone(), None).expect("reopens with the file key, without a private key");
    s.apply(id, Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
    // Crash-recovery snapshots never hold the decrypted copy.
    let snaps = s.autosave_snapshots();
    assert_eq!(snaps.len(), 1);
    assert!(snaps[0].encrypted && !String::from_utf8_lossy(&snaps[0].bytes).contains("(Page 1)"));
    assert!(s.open_revision(id, 1).is_err(), "no unencrypted copy of a revision");
    let again = s.save_bytes(id).unwrap();
    assert_eq!(&again[..saved.len()], &saved[..], "the next save appends");
    // Another session: passwords and strangers can't open it.
    let mut s2 = Session::new();
    assert!(matches!(s2.open("c.pdf", None, again.clone(), None), Err(OpenError::Unsupported(m)) if m.contains("digital ID")));
    assert!(matches!(s2.open("c.pdf", None, again.clone(), Some("guess")), Err(OpenError::Unsupported(_))));
    assert!(
        matches!(s2.open_with_certificate("c.pdf", None, again.clone(), &stranger), Err(OpenError::Unsupported(m)) if m.contains("not one of the recipients"))
    );
    // The reader gets exactly its permissions.
    let r = s2.open_with_certificate("c.pdf", None, again.clone(), &reader).unwrap();
    assert_eq!(page_texts(&s2, r), ["Page 1", "Page 2"]);
    let rd = s2.get(r).unwrap();
    assert_eq!(rd.info.pages[0].rotation, 90);
    assert!(rd.certificate_secured() && rd.info.encrypted);
    let perm = rd.permissions().unwrap();
    assert!(perm.print() && !perm.print_high_quality() && perm.annotate() && perm.fill_forms() && !perm.modify() && !perm.copy() && !perm.assemble());
    assert!(!rd.allows_security_change() && !rd.allows_assembly());
    assert_eq!(s2.apply(r, Edit::RemoveProtection), Err(EditError::NotPermitted("changing security")));
    assert!(s2.apply(r, Edit::DeletePages { pages: vec![0] }).is_err());
    // The boss is owner-equivalent: may remove the security.
    let b = s2.open_with_certificate("c.pdf", None, again.clone(), &boss).unwrap();
    assert!(s2.get(b).unwrap().allows_security_change());
    s2.apply(b, Edit::RemoveProtection).unwrap();
    assert!(!s2.get(b).unwrap().certificate_secured());
    let plain = s2.save_bytes(b).unwrap();
    assert!(pdfcraft_cos::Document::open(plain).unwrap().security().is_none());
    // Undo history survives the save: undoing the rotation and then the protection gives the
    // unprotected working state back.
    s.undo(id).unwrap();
    s.undo(id).unwrap();
    assert!(!s.get(id).unwrap().certificate_secured());
}

#[test]
fn certificate_protection_is_validated() {
    let boss = test_digital_id("Boss", 1);
    let (mut s, id) = session_with(1);
    let none = CertificateProtection { recipients: vec![], algorithm: Algorithm::Aes256, encrypt_metadata: true };
    assert!(matches!(s.apply(id, Edit::ProtectCertificates(none)), Err(EditError::Protection(_))));
    let twice = CertificateProtection {
        recipients: vec![recipient(&boss, RecipientAccess::FULL), recipient(&boss, RecipientAccess::default())],
        algorithm: Algorithm::Aes256,
        encrypt_metadata: true,
    };
    assert!(matches!(s.apply(id, Edit::ProtectCertificates(twice)), Err(EditError::Protection(m)) if m.contains("twice")));
    let rc4 =
        CertificateProtection { recipients: vec![recipient(&boss, RecipientAccess::FULL)], algorithm: Algorithm::Rc4_128, encrypt_metadata: true };
    assert!(matches!(s.apply(id, Edit::ProtectCertificates(rc4)), Err(EditError::Protection(_))));
    // Non-RSA certificates can't be recipients (no key transport for them here).
    let ec = pdfcraft_sign::PrivateKey::generate_p256().unwrap();
    let from = pdfcraft_sign::Time { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
    let ec_cert = pdfcraft_sign::Certificate::self_signed(&pdfcraft_sign::Name::build("EC", "", "", "", ""), &ec, from, 1, &[7]).unwrap();
    let ec_only = CertificateProtection {
        recipients: vec![CertificateRecipient { certificate: ec_cert, access: RecipientAccess::FULL }],
        algorithm: Algorithm::Aes256,
        encrypt_metadata: true,
    };
    assert!(matches!(s.apply(id, Edit::ProtectCertificates(ec_only)), Err(EditError::Protection(m)) if m.contains("RSA")));
    assert!(s.get(id).unwrap().can_undo().is_none(), "failed edits leave no undo step");
    // AES-128 works too, and the recipients' bits are what the envelope carries.
    let aes128 =
        CertificateProtection { recipients: vec![recipient(&boss, RecipientAccess::FULL)], algorithm: Algorithm::Aes128, encrypt_metadata: false };
    s.apply(id, Edit::ProtectCertificates(aes128)).unwrap();
    assert_eq!(s.get(id).unwrap().security_summary().unwrap().method, "Certificate security (AES, 128-bit)");
    let saved = s.save_bytes(id).unwrap();
    let mut s2 = Session::new();
    let b = s2.open_with_certificate("c.pdf", None, saved, &boss).unwrap();
    assert_eq!(page_texts(&s2, b), ["Page 1"]);
    assert!(s2.get(b).unwrap().permissions().unwrap().owner);
}

/// Independent oracles: `openssl cms -decrypt` recovers the seed from the saved file's
/// `/Recipients`, and the file key derived from it (§7.6.5.3) is the one PdfCraft uses;
/// `pdfinfo` (poppler) sees an encrypted file. Each is skipped when not installed.
#[test]
fn openssl_recovers_the_seed_and_the_file_key_matches() {
    let Ok(v) = std::process::Command::new("openssl").arg("version").output() else {
        eprintln!("openssl not installed; skipping");
        return;
    };
    eprintln!("oracle: {}", String::from_utf8_lossy(&v.stdout).trim());
    let boss = test_digital_id("Boss", 1);
    let reader = test_digital_id("Reader", 2);
    let (mut s, id) = session_with(1);
    let reader_access = RecipientAccess { printing: Printing::High, changes: Changes::None, copy: true, ..RecipientAccess::default() };
    let p = CertificateProtection {
        recipients: vec![recipient(&boss, RecipientAccess::FULL), recipient(&reader, reader_access)],
        algorithm: Algorithm::Aes256,
        encrypt_metadata: true,
    };
    s.apply(id, Edit::ProtectCertificates(p)).unwrap();
    let saved = s.save_bytes(id).unwrap();
    let dir = std::env::temp_dir().join(format!("pdfcraft-certsec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pdf = dir.join("cert.pdf");
    std::fs::write(&pdf, saved.as_slice()).unwrap();
    // Our reader of the saved file, for the /Recipients bytes and the key it derived.
    let opened = pdfcraft_cos::Document::open_with_credential(saved.clone(), pdfcraft_cos::Credential::Recipient(&RecipientId(&reader))).unwrap();
    let handler = opened.security().unwrap();
    let recipients = handler.dict().recipients.clone();
    assert_eq!(recipients.len(), 2, "one envelope per permission set");
    let cert = dir.join("reader.pem");
    let key = dir.join("reader.key.der");
    std::fs::write(&cert, pdfcraft_sign::x509::to_pem(&reader.certificate)).unwrap();
    std::fs::write(&key, reader.key.pkcs8()).unwrap();
    let mut contents = Vec::new();
    for (i, envelope) in recipients.iter().enumerate() {
        let path = dir.join(format!("r{i}.der"));
        std::fs::write(&path, envelope).unwrap();
        let out = std::process::Command::new("openssl")
            .args(["cms", "-decrypt", "-binary", "-inform", "DER", "-keyform", "DER"])
            .arg("-in")
            .arg(&path)
            .arg("-recip")
            .arg(&cert)
            .arg("-inkey")
            .arg(&key)
            .output()
            .unwrap();
        if out.status.success() {
            contents.push(out.stdout);
        }
    }
    assert_eq!(contents.len(), 1, "exactly one envelope is addressed to the reader");
    let content = &contents[0];
    assert_eq!(content.len(), 24, "20-byte seed + 4 permission bytes");
    let bits = i32::from_be_bytes([content[20], content[21], content[22], content[23]]);
    assert_eq!(bits & 0xF3E, reader_access.permission_bits(), "the reader's permissions, big-endian");
    let mut parts: Vec<&[u8]> = vec![&content[..20]];
    parts.extend(recipients.iter().map(Vec::as_slice));
    let expected = pdfcraft_sign::DigestAlg::Sha256.digest(&parts);
    assert_eq!(expected.as_slice(), handler.file_key(), "SHA-256(seed ‖ recipients) is the AES-256 file key");
    if let Ok(info) = std::process::Command::new("pdfinfo").arg(&pdf).output() {
        let text = format!("{}{}", String::from_utf8_lossy(&info.stdout), String::from_utf8_lossy(&info.stderr));
        eprintln!("pdfinfo: {text}");
        assert!(
            text.contains("Encrypted:") && text.contains("yes")
                || text.to_lowercase().contains("security handler")
                || text.to_lowercase().contains("encrypt"),
            "{text}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn scripts_change_comments_and_layers_and_wait_for_answers() {
    use pdfcraft_annot::{NewAnnotation, NoteIcon, Shape, Style};
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R 6 0 R] /D << /OFF [6 0 R] >> >> >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R /Resources << /Properties << /R 5 0 R /G 6 0 R >> >> >>".into(),
        "<< /Length 0 >>\nstream\n\nendstream".into(),
        "<< /Type /OCG /Name (Prices) >>".into(),
        "<< /Type /OCG /Name (Notes) >>".into(),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("layers.pdf", None, Arc::new(pdf), None).unwrap();
    let shape = Shape::Note { at: [20.0, 280.0], icon: NoteIcon::Comment };
    let note = NewAnnotation { page: 0, style: Style::default_for(&shape), shape, contents: "first".into(), author: "Ann".into() };
    s.apply(id, Edit::AddAnnotation(note)).unwrap();
    let comments = |s: &Session| {
        s.get(id)
            .unwrap()
            .info
            .annotations
            .iter()
            .map(|a| (a.subtype.clone(), a.contents.clone().unwrap_or_default()))
            .collect::<std::collections::BTreeSet<_>>()
    };

    // Change a comment, add two and hide a layer, as one undoable step (the layer aside).
    let o = s
        .run_javascript(
            id,
            "var a = this.getAnnots(0)[0]; a.contents = a.contents + ' (seen)';
             this.addAnnot({ type: 'Square', rect: [50, 50, 120, 90], contents: 'box' });
             this.addAnnot({ type: 'FreeText', rect: [50, 150, 200, 190], contents: 'Hello', textSize: 14 });
             var l = this.getOCGs(); l[0].state = false; l[1].state = true; l.length",
            None,
        )
        .unwrap();
    assert_eq!((o.error.as_deref(), o.result.as_deref()), (None, Some("2")));
    let want = [("FreeText", "Hello"), ("Square", "box"), ("Text", "first (seen)")];
    assert_eq!(comments(&s), want.map(|(a, b)| (a.to_string(), b.to_string())).into());
    assert_eq!(s.get(id).unwrap().info.layers.iter().map(|l| l.visible).collect::<Vec<_>>(), [false, true]);
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Run JavaScript"));
    s.undo(id).unwrap();
    assert_eq!(comments(&s), [("Text".to_string(), "first".to_string())].into());

    // Deleting a comment.
    s.run_javascript(id, "this.getAnnots()[0].destroy()", None).unwrap();
    assert!(comments(&s).is_empty());

    // A question stops the script with nothing done; the answer runs it to the end.
    let script = "if (app.alert('Add a note?', 2, 2) == 4) this.addAnnot({ type: 'Text', point: [10, 290], contents: app.response('Text?') });";
    let dialogs = |answers: Vec<js::Answer>| js::Dialogs { interactive: true, answers };
    let o = s.run_javascript_with(id, script, None, &dialogs(vec![])).unwrap();
    assert!(matches!(o.question, Some(js::Question::Alert { buttons: 2, .. })), "{o:?}");
    let o = s.run_javascript_with(id, script, None, &dialogs(vec![js::Answer::Button(4)])).unwrap();
    assert!(matches!(o.question, Some(js::Question::Response { .. })), "{o:?}");
    assert!(comments(&s).is_empty());
    let o = s.run_javascript_with(id, script, None, &dialogs(vec![js::Answer::Button(4), js::Answer::Text(Some("Call back".into()))])).unwrap();
    assert_eq!(o.question, None);
    assert_eq!(comments(&s), [("Text".to_string(), "Call back".to_string())].into());
}

/// One page (turned by `rotate`) whose structure tree holds a single element of type `ty` over
/// the text "Rotated  heading" (MCID 0).
fn tagged_one(ty: &str, rotate: i64) -> Vec<u8> {
    let body = "/P << /MCID 0 >> BDC BT /F1 20 Tf 20 150 Td (Rotated  heading) Tj ET EMC";
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R >>".into(),
        format!("<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 300] /Rotate {rotate} >>"),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        "<< /Type /StructTreeRoot /K 7 0 R >>".into(),
        format!("<< /Type /StructElem /S /{ty} /P 6 0 R /Pg 3 0 R /K 0 >>"),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

#[test]
fn heading_bookmarks_come_from_tagged_headings_only() {
    let (mut s, plain) = session_with(2);
    let err = s.get(plain).unwrap().heading_bookmarks(None).unwrap_err();
    assert!(err.contains("not tagged"), "{err}");
    let p = s.open("p.pdf", None, Arc::new(tagged_one("P", 0)), None).unwrap();
    let err = s.get(p).unwrap().heading_bookmarks(None).unwrap_err();
    assert!(err.contains("no headings"), "{err}");

    // An H2 on a page turned a quarter: its text (spaces collapsed), level, and the page alone
    // (a user-space top would not be the displayed top).
    let id = s.open("h.pdf", None, Arc::new(tagged_one("H2", 90)), None).unwrap();
    let found = s.get(id).unwrap().heading_bookmarks(None).unwrap();
    assert_eq!(found.entries, vec![OutlineEntry { title: "Rotated heading".into(), level: 2, page: 0, top: None }]);
    assert_eq!((found.untitled, found.unplaced, found.truncated), (0, 0, false));
    let err = s.get(id).unwrap().heading_bookmarks(Some(1)).unwrap_err();
    assert!(err.contains("level 1"), "{err}");
    s.apply(id, Edit::AddBookmarkTree { entries: found.entries, replace: false }).unwrap();
    let d = s.get(id).unwrap();
    assert_eq!(d.info.outline.iter().map(|o| (o.title.as_str(), o.page)).collect::<Vec<_>>(), [("Rotated heading", Some(0))]);
    assert_eq!(s.undo(id).unwrap(), "New bookmarks from structure");
    assert!(s.get(id).unwrap().info.outline.is_empty());

    // Upright, the bookmark goes to the heading's top (20 pt text on a 150 pt baseline).
    let id = s.open("u.pdf", None, Arc::new(tagged_one("H1", 0)), None).unwrap();
    let top = s.get(id).unwrap().heading_bookmarks(None).unwrap().entries[0].top.unwrap();
    assert!((160.0..=172.0).contains(&top), "{top}");
}

/// One page painted out of logical order (body, then title), with a running header and a page
/// number marked as artifacts, a list and a figure; `tree` is the /StructTreeRoot (object 6),
/// `elements` objects 7….
fn accessible_pdf(tree: &str, elements: &[&str]) -> Vec<u8> {
    let body = "/Artifact << /Type /Pagination >> BDC BT /F1 10 Tf 20 280 Td (Running header) Tj ET EMC \
                /P << /MCID 1 >> BDC BT /F1 12 Tf 20 200 Td (Body   comes) Tj 0 -14 Td (after the title.) Tj ET EMC \
                /H1 << /MCID 0 >> BDC BT /F1 18 Tf 20 240 Td (Title) Tj ET EMC \
                /Lbl << /MCID 2 >> BDC BT /F1 12 Tf 20 150 Td (1.) Tj ET EMC \
                /LBody << /MCID 3 >> BDC BT /F1 12 Tf 40 150 Td (First) Tj ET EMC \
                /LI << /MCID 4 >> BDC BT /F1 12 Tf 20 135 Td (Second) Tj ET EMC \
                /Figure << /MCID 5 >> BDC q 10 0 0 10 20 100 cm 0 0 1 1 re f Q EMC \
                /Artifact BMC BT /F1 10 Tf 90 20 Td (7) Tj ET EMC";
    let mut objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 300] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /StructParents 0 /Resources << /Font << /F1 5 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        tree.into(),
    ];
    objs.extend(elements.iter().map(|e| e.to_string()));
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

#[test]
fn accessible_text_reads_the_structure_tree_in_logical_order() {
    let (mut s, plain) = session_with(1);
    let err = s.get(plain).unwrap().accessible_text().unwrap_err();
    assert!(err.contains("not tagged"), "{err}");
    let empty = s.open("e.pdf", None, Arc::new(accessible_pdf("<< /Type /StructTreeRoot >>", &[])), None).unwrap();
    let err = s.get(empty).unwrap().accessible_text().unwrap_err();
    assert!(err.contains("empty"), "{err}");

    let elements = [
        "<< /Type /StructElem /S /Document /P 6 0 R /Pg 3 0 R /K [8 0 R 9 0 R 10 0 R 13 0 R] >>", // 7
        "<< /Type /StructElem /S /H1 /P 7 0 R /K 0 >>",                                           // 8
        "<< /Type /StructElem /S /P /P 7 0 R /K 1 >>",                                            // 9
        "<< /Type /StructElem /S /L /P 7 0 R /K [11 0 R 12 0 R] >>",                              // 10
        "<< /Type /StructElem /S /LI /P 10 0 R /K [<< /S /Lbl /K 2 >> << /S /LBody /K 3 >>] >>",  // 11
        "<< /Type /StructElem /S /LI /P 10 0 R /K [4 7 0 R] >>",                                  // 12: loops back to the root
        "<< /Type /StructElem /S /Figure /P 7 0 R /Alt (A blue square) /K 5 >>",                  // 13
    ];
    let id = s.open("t.pdf", None, Arc::new(accessible_pdf("<< /Type /StructTreeRoot /K 7 0 R >>", &elements)), None).unwrap();
    let got = s.get(id).unwrap().accessible_text().unwrap();
    assert_eq!(got.text, "Title\n\nBody comes after the title.\n\n1. First\nSecond\n\nA blue square\n");
    assert_eq!((got.blocks, got.headings, got.list_items, got.figures, got.table_rows), (5, 1, 2, 1, 0));
    assert_eq!((got.pages_read, got.unreadable_pages, got.truncated), (1, 0, false));
}

#[test]
fn change_to_artifact_takes_content_out_of_the_accessible_text_and_undoes() {
    let (mut s, _) = session_with(1);
    let elements = [
        "<< /Type /StructElem /S /Document /P 6 0 R /Pg 3 0 R /K [8 0 R 9 0 R 10 0 R 13 0 R] >>", // 7
        "<< /Type /StructElem /S /H1 /P 7 0 R /K 0 >>",                                           // 8
        "<< /Type /StructElem /S /P /P 7 0 R /K 1 >>",                                            // 9
        "<< /Type /StructElem /S /L /P 7 0 R /K [11 0 R 12 0 R] >>",                              // 10
        "<< /Type /StructElem /S /LI /P 10 0 R /K [<< /S /Lbl /K 2 >> << /S /LBody /K 3 >>] >>",  // 11
        "<< /Type /StructElem /S /LI /P 10 0 R /K [4 7 0 R] >>",                                  // 12: loops back to the root
        "<< /Type /StructElem /S /Figure /P 7 0 R /Alt (A blue square) /K 5 >>",                  // 13
    ];
    let id = s.open("t.pdf", None, Arc::new(accessible_pdf("<< /Type /StructTreeRoot /K 7 0 R >>", &elements)), None).unwrap();
    let before = s.get(id).unwrap().accessible_text().unwrap().text;
    assert!(before.contains("Second"), "{before}");
    // The second list item's line (baseline 135), as a layout artifact.
    let selection =
        a11y::ArtifactSelection { page: Some(0), rect: Some([0.0, 130.0, 300.0, 145.0]), ty: Some(a11y::ArtifactType::Layout), ..Default::default() };
    let preview = s.get(id).unwrap().artifact_preview(&selection).unwrap();
    assert_eq!((preview.marked.clone(), preview.untagged), (vec![(0, 4)], 0));
    assert_eq!(s.get(id).unwrap().accessible_text().unwrap().text, before, "a preview changes nothing");
    s.apply(id, Edit::MarkArtifact { selection: selection.clone() }).unwrap();
    let after = s.get(id).unwrap().accessible_text().unwrap().text;
    assert_eq!(after, "Title\n\nBody comes after the title.\n\n1. First\n\nA blue square\n");
    assert!(s.get(id).unwrap().tags().nodes.iter().filter(|n| n.ty == "LI").count() == 1);
    assert_eq!(s.undo(id).unwrap(), "Change to artifact");
    assert_eq!(s.get(id).unwrap().accessible_text().unwrap().text, before);
    // A refused selection is an error and no undo step.
    let bad = a11y::ArtifactSelection { page: Some(0), mcids: vec![42], ..Default::default() };
    assert!(s.get(id).unwrap().artifact_preview(&bad).unwrap_err().contains("MCID 42"));
    assert!(s.apply(id, Edit::MarkArtifact { selection: bad }).is_err());
    assert!(s.undo(id).is_err(), "the refused edit left no undo step");
}

#[test]
fn printer_marks_are_one_undo_step_and_keep_the_text() {
    let (mut s, id) = session_with(2);
    let before = s.get(id).unwrap().info.pages.clone();
    let settings = PrinterMarks { file_name: "fixture.pdf".into(), ..Default::default() };
    let plan = s.printer_marks_preview(id, &[1], &settings).unwrap();
    assert_eq!(plan.len(), 1);
    assert!(!s.get(id).unwrap().dirty, "the preview changes nothing");
    s.apply(id, Edit::AddPrinterMarks { pages: vec![1], settings }).unwrap();
    let doc = s.get(id).unwrap();
    assert_eq!(doc.can_undo(), Some("Add printer marks"));
    let m = plan[0].margin;
    assert!((doc.info.pages[1].width as f64 - (200.0 + 2.0 * m)).abs() < 0.01);
    assert_eq!(doc.info.pages[0].width, before[0].width, "page 1 untouched");
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 2"], "the page information is not text");
    s.undo(id).unwrap();
    assert_eq!(s.get(id).unwrap().info.pages[1].width, before[1].width);
    // Bad settings are refused before anything changes.
    let bad = PrinterMarks { offset: f64::NAN, ..Default::default() };
    assert!(s.apply(id, Edit::AddPrinterMarks { pages: vec![0], settings: bad }).is_err());
    assert_eq!(s.get(id).unwrap().can_undo(), None);
}

#[test]
fn associated_files_are_one_undo_step_each_and_survive_save() {
    let (mut s, id) = session_with(2);
    let file = NewAssociatedFile {
        name: "data.csv".into(),
        data: Arc::new(b"a,b\n1,2\n".to_vec()),
        mime: Some("text/csv".into()),
        relationship: AfRelationship::Data,
        description: Some("The figures".into()),
    };
    s.apply(id, Edit::AddAssociatedFile { target: AfTarget::Document, file: file.clone() }).unwrap();
    s.apply(
        id,
        Edit::AddAssociatedFile { target: AfTarget::Page(1), file: NewAssociatedFile { relationship: AfRelationship::Source, ..file.clone() } },
    )
    .unwrap();
    let doc = s.get(id).unwrap();
    assert_eq!(doc.can_undo(), Some("Add associated file"));
    let list = doc.associated_files(&AfTarget::Document).unwrap();
    assert_eq!(list.files.len(), 1);
    assert_eq!(list.files[0].mod_date.as_deref(), Some(pdfcraft_cos::pdf_date(1_700_000_000).as_str()));
    assert!(doc.info.attachments.iter().any(|a| a.name == "data.csv"), "a document-level file is an attachment");
    assert_eq!(doc.all_associated_files().unwrap().len(), 2);
    // Save and reopen.
    let bytes = s.save_bytes(id).unwrap();
    let mut s2 = Session::new();
    let id2 = s2.open("saved.pdf", None, bytes, None).unwrap();
    let page = s2.get(id2).unwrap().associated_files(&AfTarget::Page(1)).unwrap();
    assert_eq!(page.files[0].relationship.as_deref(), Some("Source"));
    let (_, data) = s2.get(id2).unwrap().associated_file_data(&AfTarget::Page(1), &AfSelector::Index(0), 1 << 20).unwrap();
    assert_eq!(data, b"a,b\n1,2\n");
    // Remove, undo, redo.
    s.apply(id, Edit::RemoveAssociatedFile { target: AfTarget::Document, select: AfSelector::Name("data.csv".into()) }).unwrap();
    let doc = s.get(id).unwrap();
    assert_eq!(doc.can_undo(), Some("Remove associated file"));
    assert!(doc.associated_files(&AfTarget::Document).unwrap().files.is_empty());
    assert!(doc.info.attachments.is_empty(), "the attachment goes too");
    s.undo(id).unwrap();
    assert_eq!(s.get(id).unwrap().associated_files(&AfTarget::Document).unwrap().files.len(), 1);
    s.redo(id).unwrap();
    assert!(s.get(id).unwrap().associated_files(&AfTarget::Document).unwrap().files.is_empty());
    s.undo(id).unwrap();
    s.undo(id).unwrap();
    assert!(s.get(id).unwrap().associated_files(&AfTarget::Page(1)).unwrap().files.is_empty());
    // A refused edit leaves no undo step.
    let before = s.get(id).unwrap().can_undo().map(str::to_owned);
    assert!(s.apply(id, Edit::AddAssociatedFile { target: AfTarget::Page(7), file: file.clone() }).is_err());
    assert!(s.apply(id, Edit::RemoveAssociatedFile { target: AfTarget::Page(0), select: AfSelector::Index(0) }).is_err());
    assert_eq!(s.get(id).unwrap().can_undo().map(str::to_owned), before);
}

#[test]
fn convert_colors_is_one_undo_step_and_a_noop_records_none() {
    let (mut s, id) = session_with(2);
    let settings = ColorConversion::default();
    let preview = s.convert_colors_preview(id, &[1], &settings).unwrap();
    assert!(preview.changed(), "{preview:?}");
    assert!(!s.get(id).unwrap().dirty, "the preview changes nothing");
    let report = s.convert_colors(id, vec![1], settings.clone()).unwrap();
    assert_eq!(report.pages, vec![1]);
    let doc = s.get(id).unwrap();
    assert_eq!(doc.can_undo(), Some("Convert colors"));
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 2"], "the text stays");
    // Again: the text is already CMYK; nothing changes and no second undo step is recorded.
    let again = s.convert_colors(id, vec![1], settings.clone()).unwrap();
    assert!(!again.changed(), "{again:?}");
    s.undo(id).unwrap();
    assert_eq!(s.get(id).unwrap().can_undo(), None);
    assert!(s.convert_colors_preview(id, &[1], &settings).unwrap().changed(), "undo put the gray back");
    s.redo(id).unwrap();
    assert!(!s.convert_colors_preview(id, &[1], &settings).unwrap().changed(), "redo converted it again");
    // Bad settings are refused before anything changes.
    let bad = ColorConversion { output_intent: true, ..Default::default() };
    assert!(s.convert_colors(id, vec![0], bad).is_err());
    assert!(s.convert_colors(id, vec![5], ColorConversion::default()).is_err());
}

/// Two pages: page 1 has half-transparent red over text and a Multiply square, page 2 is opaque.
pub(crate) fn transparency_fixture() -> Vec<u8> {
    let page1 = "BT /F1 24 Tf 20 200 Td (Flatten me) Tj ET /A gs 1 0 0 rg 10 190 180 40 re f /M gs 0 1 0 rg 120 20 60 60 re f";
    let page2 = "0 0 1 rg 20 20 100 100 re f";
    let bodies = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /MediaBox [0 0 200 300] >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Group << /S /Transparency >> /Resources << /Font << /F1 7 0 R >> \
         /ExtGState << /A << /ca 0.5 >> /M << /BM /Multiply >> >> >> >>"
            .to_string(),
        format!("<< /Length {} >>\nstream\n{page1}\nendstream", page1.len()),
        "<< /Type /Page /Parent 2 0 R /Contents 6 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{page2}\nendstream", page2.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
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
    out
}

fn x1a_transparency(bytes: &Arc<Vec<u8>>) -> usize {
    let doc = pdfcraft_cos::Document::open(bytes.clone()).unwrap();
    let options = pdfcraft_preflight::XOptions { part: Some(pdfcraft_preflight::XPart::X1a2001), max_findings: 0 };
    pdfcraft_preflight::verify_pdfx(&doc, &options).count("transparency")
}

fn page_pixels(bytes: &Arc<Vec<u8>>, page: usize) -> Vec<u8> {
    let mut r = pdfcraft_render::PageRenderer::new(bytes.clone(), pdfcraft_render::RenderConfig::default());
    let out = r.render(pdfcraft_render::RenderRequest { page, scale: 1.0, ..Default::default() });
    assert!(out.error.is_none(), "{:?}", out.error);
    out.rgba.to_vec()
}

#[test]
fn flattening_transparency_is_one_undo_step_and_survives_saving() {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let original = Arc::new(transparency_fixture());
    assert!(x1a_transparency(&original) > 0);
    let id = s.open("t.pdf", None, original.clone(), None).unwrap();
    let edit = |pages: Vec<usize>| Edit::FlattenTransparency { pages, settings: FlattenSettings::default() };
    // Only the opaque page: nothing to do, no undo step.
    s.apply(id, edit(vec![1])).unwrap();
    let d = s.get(id).unwrap();
    assert!(d.can_undo().is_none() && !d.dirty);
    assert_eq!(d.flatten_report.as_ref().map(|r| r.untouched.clone()), Some(vec![1]));

    s.apply(id, edit(Vec::new())).unwrap();
    let d = s.get(id).unwrap();
    assert_eq!(d.can_undo(), Some("Flatten transparency"));
    let report = d.flatten_report.clone().unwrap();
    assert_eq!(report.pages.len(), 1);
    assert_eq!(report.pages[0].action, PageAction::Patched, "{report:?}");
    assert_eq!(report.untouched, vec![1]);

    let saved = s.save_bytes(id).unwrap();
    assert_eq!(x1a_transparency(&saved), 0, "PDF/X-1a's transparency rule passes");
    let reopened = pdfcraft_cos::Document::open(saved.clone()).unwrap();
    assert!(pdfcraft_preflight::transparent_pages(&reopened, &[]).is_empty());
    // The look is kept (within the patch edges' resampling), and the opaque page is identical.
    for page in 0..2 {
        let (a, b) = (page_pixels(&original, page), page_pixels(&saved, page));
        let off = a
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.as_chunks::<4>().0.iter())
            .filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > 40))
            .count();
        assert!(off * 100 < a.len() / 4, "page {}: {off} pixels differ", page + 1);
        if page == 1 {
            assert_eq!(a, b);
        }
    }
    // The text stays text.
    let id2 = s.open("saved.pdf", None, saved, None).unwrap();
    assert!(s.get(id2).unwrap().text_lines(0).iter().any(|l| l.text.contains("Flatten me")));

    s.undo(id).unwrap();
    assert!(x1a_transparency(&s.get(id).unwrap().bytes) > 0);
    s.redo(id).unwrap();
    assert_eq!(x1a_transparency(&s.get(id).unwrap().bytes), 0);
    // Bad settings and pages are refused and change nothing.
    for bad in [
        Edit::FlattenTransparency { pages: vec![5], settings: FlattenSettings::default() },
        Edit::FlattenTransparency { pages: Vec::new(), settings: FlattenSettings { resolution: 0.0, balance: 50 } },
        Edit::FlattenTransparency { pages: Vec::new(), settings: FlattenSettings { resolution: 300.0, balance: 200 } },
    ] {
        assert!(s.apply(id, bad).is_err());
    }
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Flatten transparency"));
}

#[test]
fn structure_namespaces_edit_undo_redo_and_save_round_trip() {
    let mut s = Session::new();
    let id = s.open("ns.pdf", None, Arc::new(tagged_one("Callout", 0)), None).unwrap();
    assert!(s.get(id).unwrap().heading_bookmarks(None).unwrap_err().contains("no headings"));
    let custom = "http://example.com/report";
    let spec = a11y::NamespaceSpec {
        uri: custom.into(),
        schema: None,
        role_map: vec![a11y::NsMapping { from: "Callout".into(), to: "H1".into(), namespace: Some("pdf-2.0".into()) }],
    };
    s.apply(id, Edit::AddNamespace { spec }).unwrap();
    let element = s.get(id).unwrap().tags().nodes[0].obj.unwrap().num;
    let change = a11y::TagChange { namespace: Some(Some(custom.into())), ..Default::default() };
    s.apply(id, Edit::SetTag { element, change }).unwrap();
    let check = |s: &Session| {
        let t = s.get(id).unwrap().tags();
        let n = &t.nodes[0];
        assert_eq!(
            (n.standard.as_deref(), n.namespace.as_deref(), n.standard_namespace.as_deref()),
            (Some("H1"), Some(custom), Some(a11y::NS_PDF20))
        );
        assert_eq!(t.namespaces.iter().map(|n| n.uri.as_str()).collect::<Vec<_>>(), [custom, a11y::NS_PDF20]);
        assert_eq!(s.get(id).unwrap().heading_bookmarks(None).unwrap().entries[0].title, "Rotated heading");
    };
    check(&s);

    // A refused namespace change leaves the document and its history alone.
    let bad = a11y::TagChange { namespace: Some(Some("http://example.com/unmapped".into())), ..Default::default() };
    assert!(matches!(s.apply(id, Edit::SetTag { element, change: bad }), Err(EditError::Accessibility(_))));
    let cycle = a11y::NamespaceSpec {
        uri: custom.into(),
        schema: None,
        role_map: vec![
            a11y::NsMapping { from: "X".into(), to: "Y".into(), namespace: Some(custom.into()) },
            a11y::NsMapping { from: "Y".into(), to: "X".into(), namespace: Some(custom.into()) },
        ],
    };
    assert!(matches!(s.apply(id, Edit::AddNamespace { spec: cycle }), Err(EditError::Accessibility(_))));
    check(&s);

    assert_eq!(s.undo(id).unwrap(), "Change tag properties");
    let t = s.get(id).unwrap().tags();
    assert_eq!((t.nodes[0].standard.as_deref(), t.nodes[0].namespace.as_deref()), (None, None));
    assert_eq!(s.undo(id).unwrap(), "Add structure namespace");
    assert!(s.get(id).unwrap().tags().namespaces.is_empty());
    s.redo(id).unwrap();
    s.redo(id).unwrap();
    check(&s);

    let saved = s.save_full_bytes(id).unwrap();
    let re = s.open("re.pdf", None, saved, None).unwrap();
    let t = s.get(re).unwrap().tags();
    assert_eq!((t.nodes[0].standard.as_deref(), t.nodes[0].namespace.as_deref()), (Some("H1"), Some(custom)));
    assert_eq!(t.namespaces[0].role_map, [a11y::NsMapping { from: "Callout".into(), to: "H1".into(), namespace: Some(a11y::NS_PDF20.into()) }]);
}

/// A header-only ICC output profile (no conversion tables: colours use the approximation).
fn output_profile(major: u8, space: &[u8; 4]) -> Arc<Vec<u8>> {
    let mut h = vec![0u8; 256];
    h[0..4].copy_from_slice(&256u32.to_be_bytes());
    h[8] = major;
    h[12..16].copy_from_slice(b"prtr");
    h[16..20].copy_from_slice(space);
    h[20..24].copy_from_slice(b"Lab ");
    h[36..40].copy_from_slice(b"acsp");
    Arc::new(h)
}

fn pdfx_options(level: &str) -> pdfcraft_preflight::XConvertOptions {
    let part = pdfcraft_preflight::XPart::from_level(level).unwrap();
    let major = if part == pdfcraft_preflight::XPart::X4 { 4 } else { 2 };
    pdfcraft_preflight::XConvertOptions {
        intent: Some(pdfcraft_preflight::XIntent { condition: "FOGRA39".into(), info: None, profile: Some(output_profile(major, b"CMYK")) }),
        ..pdfcraft_preflight::XConvertOptions::new(part)
    }
}

fn pdfx_report(bytes: &Arc<Vec<u8>>, part: pdfcraft_preflight::XPart) -> pdfcraft_preflight::XReport {
    let doc = pdfcraft_cos::Document::open(bytes.clone()).unwrap();
    pdfcraft_preflight::verify_pdfx(&doc, &pdfcraft_preflight::XOptions { part: Some(part), max_findings: 100 })
}

#[test]
fn convert_to_pdfx_reaches_each_level_as_one_undo_step_and_is_idempotent() {
    for level in ["x1a", "x3", "x4"] {
        let mut s = Session::new().with_clock(|| 1_700_000_000);
        let original = Arc::new(transparency_fixture());
        let id = s.open("t.pdf", None, original.clone(), None).unwrap();
        let options = pdfx_options(level);
        let part = options.part;
        let r = s.pdfx_convert(id, options.clone()).unwrap();
        assert!(!r.before.conforms(), "{level}: {:?}", r.before.findings);
        assert!(r.converted && r.after.conforms(), "{level}: {:?} {:?} {:?}", r.after.findings, r.fixed, r.notes);
        assert!(r.unfixable().is_empty());
        assert!(r.fixed.iter().any(|f| f.contains("Embedded 1 standard font: Helvetica")), "{level}: {:?}", r.fixed);
        assert!(r.fixed.iter().any(|f| f.starts_with("Converted ")), "{level}: {:?}", r.fixed);
        assert_eq!(r.fixed.iter().any(|f| f.starts_with("Flattened transparency")), level != "x4", "{level}: {:?}", r.fixed);
        assert!(r.notes.iter().any(|n| n.contains("built-in approximation")), "{level}: {:?}", r.notes);
        let label = format!("Convert to {}", part.label());
        assert_eq!(s.get(id).unwrap().can_undo(), Some(label.as_str()));
        // Saved and reopened, it still conforms (PDF/X-1a and X-3 stay at PDF 1.4).
        let saved = s.save_bytes(id).unwrap();
        let reread = pdfx_report(&saved, part);
        assert!(reread.conforms(), "{level}: {:?}", reread.findings);
        let max = part.max_pdf_version();
        assert!(saved.starts_with(format!("%PDF-{}.{}", max.0, max.1).as_bytes()), "{level}");
        // The text stays text.
        let id2 = s.open("saved.pdf", None, saved, None).unwrap();
        assert!(s.get(id2).unwrap().text_lines(0).iter().any(|l| l.text.contains("Flatten me")), "{level}");
        // Again: nothing to do, no new undo step.
        let generation = s.get(id).unwrap().generation;
        let again = s.pdfx_convert(id, options.clone()).unwrap();
        assert!(again.converted && again.fixed.is_empty() && again.before.conforms(), "{level}: {:?}", again.fixed);
        assert_eq!(s.get(id).unwrap().generation, generation);
        // Undo restores the original.
        s.undo(id).unwrap();
        assert!(s.get(id).unwrap().can_undo().is_none());
        assert!(!pdfx_report(&s.get(id).unwrap().bytes, part).conforms());
        assert_eq!(page_pixels(&s.get(id).unwrap().bytes, 1), page_pixels(&original, 1), "{level}");
    }
}

#[test]
fn convert_to_pdfx_refuses_what_it_cannot_fix_unless_partial() {
    let page = "BT /F1 12 Tf 20 20 Td (Hi) Tj ET 0 0 0 1 k 0 0 10 10 re f";
    let bodies = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{page}\nendstream", page.len()),
        "<< /Type /Font /Subtype /TrueType /BaseFont /Arial /FirstChar 32 /LastChar 32 /Widths [278] >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
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
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("arial.pdf", None, Arc::new(out), None).unwrap();
    let r = s.pdfx_convert(id, pdfx_options("x1a")).unwrap();
    assert!(!r.converted, "{:?}", r.after.findings);
    assert_eq!(r.unfixable().iter().map(|f| f.rule).collect::<Vec<_>>(), vec!["fonts-embedded"]);
    assert!(s.get(id).unwrap().can_undo().is_none() && !s.get(id).unwrap().dirty, "nothing changed");
    let partial = pdfcraft_preflight::XConvertOptions { allow_partial: true, ..pdfx_options("x1a") };
    let r = s.pdfx_convert(id, partial).unwrap();
    assert!(r.converted && !r.after.conforms());
    assert_eq!(r.after.failed(), vec!["fonts-embedded"]);
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Convert to PDF/X-1a:2003"));
    // Bad options are errors and change nothing more.
    let generation = s.get(id).unwrap().generation;
    for bad in [
        pdfcraft_preflight::XConvertOptions {
            intent: Some(pdfcraft_preflight::XIntent { condition: "FOGRA39".into(), info: None, profile: None }),
            ..pdfx_options("x4")
        },
        pdfcraft_preflight::XConvertOptions { trim: Some([0.0, 0.0, 500.0, 500.0]), ..pdfx_options("x3") },
        pdfcraft_preflight::XConvertOptions { bleed: Some([0.0, 0.0, -1.0, 5.0]), ..pdfx_options("x3") },
    ] {
        assert!(matches!(s.pdfx_convert(id, bad), Err(EditError::Invalid(_))));
    }
    assert_eq!(s.get(id).unwrap().generation, generation);
}

#[test]
fn convert_to_pdfx_survives_hostile_documents() {
    let docs = [
        "%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 1 0 R /AcroForm 1 0 R /OutputIntents [1 0 R] >> endobj\ntrailer << /Root 1 0 R /Info 1 0 R >>\n%%EOF\n".to_string(),
        "%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >> endobj\n\
         3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 -5 1e308] /Annots [3 0 R] /Resources << /Font << /F 3 0 R >> >> /Contents 3 0 R >> endobj\n\
         trailer << /Root 1 0 R >>\n%%EOF\n"
            .to_string(),
    ];
    for d in docs {
        for level in ["x1a", "x3", "x4"] {
            let mut s = Session::new().with_clock(|| 1_700_000_000);
            let Ok(id) = s.open("h.pdf", None, Arc::new(d.clone().into_bytes()), None) else { continue };
            let _ = s.pdfx_convert(id, pdfx_options(level));
            let _ = s.pdfx_convert(id, pdfcraft_preflight::XConvertOptions { allow_partial: true, ..pdfx_options(level) });
        }
    }
}
