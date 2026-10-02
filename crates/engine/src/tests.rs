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

fn page_texts(s: &Session, id: DocId) -> Vec<String> {
    let doc = s.get(id).unwrap();
    let config = printcraft_render::RenderConfig { password: doc.password.as_deref().map(Arc::from), ..Default::default() };
    let mut r = printcraft_render::PageRenderer::new(doc.bytes.clone(), config);
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
    fn text(page: usize) -> printcraft_render::RenderRequest {
        printcraft_render::RenderRequest { page, kind: printcraft_render::RequestKind::Text, scale: 1.0, ..Default::default() }
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
    assert_eq!(err, EditError::Organize(printcraft_organize::OrganizeError::WouldRemoveAllPages));
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
    let reopened = printcraft_cos::Document::open(second).unwrap();
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
fn layer_choices_survive_edits() {
    // No layers in the fixture: just make sure refresh keeps working with an empty list and
    // that unknown layers are rejected.
    let (mut s, id) = session_with(1);
    assert!(!s.set_layer_visible(id, 0, false));
    s.apply(id, Edit::InsertBlankPage { at: 1, width: 100.0, height: 100.0 }).unwrap();
    assert_eq!(s.get(id).unwrap().info.pages.len(), 2);
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
    let parts = s.split(id, &printcraft_organize::SplitBy::PageCount(2)).unwrap();
    assert_eq!(parts.iter().map(|(a, b, _)| (*a, *b)).collect::<Vec<_>>(), [(1, 2), (3, 3)]);
    // Insert pages from a file, undoably, into the open document.
    s.apply(id, Edit::InsertPagesFrom { name: "b.pdf".into(), bytes: other, pages: Some(vec![1]), at: 1 }).unwrap();
    assert_eq!(page_texts(&s, id), ["Page 1", "Page 2", "Page 2", "Page 3"]);
    assert_eq!(s.undo(id).unwrap(), "Insert pages from b.pdf");
    assert_eq!(page_texts(&s, id).len(), 3);
    // Garbage sources fail cleanly.
    let bad = s.apply(id, Edit::InsertPagesFrom { name: "junk.pdf".into(), bytes: Arc::new(b"nope".to_vec()), pages: None, at: 0 });
    assert!(matches!(bad, Err(EditError::Source(_))));
}

fn protected(user: &str, owner: &str, permissions: i32) -> Arc<Vec<u8>> {
    let mut doc = printcraft_cos::Document::open(Arc::new(fixture(2))).unwrap();
    doc.set_encryption(&printcraft_cos::NewEncryption {
        algorithm: printcraft_cos::Algorithm::Aes256,
        user_password: user,
        owner_password: owner,
        permissions,
        encrypt_metadata: true,
        seed: [7; 32],
    })
    .unwrap();
    Arc::new(printcraft_cos::write_full(&doc, &Default::default()).unwrap())
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
    assert!(printcraft_cos::Document::open(saved.clone()).is_err(), "still protected after saving");
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
fn combining_protected_files_is_refused_clearly() {
    let s = Session::new();
    let err = s.combine(&[("a.pdf".into(), protected("pw", "o", -1)), ("b.pdf".into(), Arc::new(fixture(1)))]).unwrap_err();
    assert_eq!(err, EditError::Source("a.pdf: it is password-protected".into()));
    let err = s.combine(&[("c.pdf".into(), protected("", "o", 0b0100))]).unwrap_err();
    assert!(matches!(err, EditError::Source(m) if m.contains("don't allow copying pages")));
}

#[test]
fn owner_password_of_older_revisions_opens_the_viewer_too() {
    for alg in [printcraft_cos::Algorithm::Rc4_128, printcraft_cos::Algorithm::Aes128] {
        let mut doc = printcraft_cos::Document::open(Arc::new(fixture(1))).unwrap();
        doc.set_encryption(&printcraft_cos::NewEncryption {
            algorithm: alg,
            user_password: "u",
            owner_password: "o",
            permissions: 0,
            encrypt_metadata: true,
            seed: [1; 32],
        })
        .unwrap();
        let bytes = Arc::new(printcraft_cos::write_full(&doc, &Default::default()).unwrap());
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

fn outline_titles(items: &[printcraft_render::OutlineItem]) -> Vec<String> {
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
    use printcraft_organize::LabelStyle;
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
    let mut r = printcraft_render::PageRenderer::new(doc.bytes.clone(), printcraft_render::RenderConfig::default());
    let out = r.render(printcraft_render::RenderRequest { page, scale: 1.0, ..Default::default() });
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
    let mut cos = printcraft_cos::Document::open(Arc::new(fixture(1))).unwrap();
    // Owner "own", empty user password, everything allowed except commenting (bit 6).
    let params = printcraft_cos::NewEncryption {
        algorithm: printcraft_cos::Algorithm::Aes256,
        user_password: "",
        owner_password: "own",
        permissions: !(1 << 5),
        encrypt_metadata: true,
        seed: [7; 32],
    };
    cos.set_encryption(&params).unwrap();
    let bytes = printcraft_cos::write_full(&cos, &printcraft_cos::SaveOptions::default()).unwrap();
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
    assert!(printcraft_cos::Document::open(saved.clone()).is_err(), "needs the password");
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
    assert!(printcraft_cos::Document::open(plain).is_ok());
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
fn protection_is_validated_undoable_and_never_logged() {
    let (mut s, id) = session_with(1);
    assert!(matches!(s.apply(id, Edit::Protect(protection(None, None))), Err(EditError::Protection(_))));
    assert!(matches!(s.apply(id, Edit::Protect(protection(Some("same"), Some("same")))), Err(EditError::Protection(_))));
    let rc4 = Protection { algorithm: printcraft_cos::Algorithm::Rc4_128, ..protection(Some("pässword"), None) };
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
