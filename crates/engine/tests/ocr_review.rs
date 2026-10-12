//! Original synthetic fixtures: review authority, cancellation, corrections and image fidelity.
use std::{cell::Cell, sync::Arc};

use pdfcraft_engine::{
    DocId, Edit, Session,
    ocr::{OcrReview, OcrSettings, Recognizer},
};
use pdfcraft_ocr::{Line, OcrError, PlacedWord, Word};
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind};

struct Reader {
    calls: Cell<usize>,
    panic: bool,
}
impl Reader {
    fn new() -> Self {
        Self { calls: Cell::new(0), panic: false }
    }
}
impl Recognizer for Reader {
    fn recognize(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<Line>, OcrError> {
        pdfcraft_ocr::validate_image(pixels, width, height)?;
        self.calls.set(self.calls.get() + 1);
        assert!(!self.panic, "synthetic backend failure");
        Ok(vec![Line {
            words: vec![Word { text: "Cafe".into(), rect: [10.0, 10.0, 50.0, 25.0] }, Word { text: "extra".into(), rect: [55.0, 10.0, 90.0, 25.0] }],
        }])
    }
}
fn document(pages: usize) -> (Session, DocId) {
    let mut session = Session::new().with_clock(|| 1_700_000_000);
    let bytes = session.create_blank(120.0, 90.0, pages).unwrap();
    let id = session.open("scan.pdf", None, bytes, None).unwrap();
    (session, id)
}
fn settings() -> OcrSettings {
    OcrSettings { dpi: 72.0, ..Default::default() }
}
fn draft(session: &Session, id: DocId) -> OcrReview {
    session.ocr_job_checked(id, &[], settings()).unwrap().run(&Reader::new(), |_, _| true).unwrap()
}
fn pixels(bytes: Arc<Vec<u8>>) -> Vec<u8> {
    let shot = PageRenderer::new(bytes, RenderConfig::default()).render(RenderRequest { scale: 1.0, ..Default::default() });
    assert!(shot.error.is_none(), "{:?}", shot.error);
    shot.rgba.to_vec()
}
fn text(bytes: Arc<Vec<u8>>) -> String {
    PageRenderer::new(bytes, RenderConfig::default())
        .render(RenderRequest { kind: RequestKind::Text, scale: 1.0, ..Default::default() })
        .text
        .unwrap()
        .plain_text()
}
fn visible_text() -> Edit {
    Edit::AddText {
        page: 0,
        text: pdfcraft_edit::AddedText { rect: [10.0, 40.0, 115.0, 70.0], text: "VISIBLE".into(), size: 18.0, ..Default::default() },
    }
}

#[test]
fn review_correct_apply_save_reopen_preserves_pixels_and_is_one_undo() {
    let (mut session, id) = document(1);
    let original = session.get(id).unwrap().bytes.clone();
    let original_pixels = pixels(original.clone());
    let mut review = draft(&session, id);
    let geometry = review.pages()[0].words[0].clone();
    assert_eq!(session.get(id).unwrap().bytes, original);
    assert_eq!(session.get(id).unwrap().can_undo(), None);
    review.correct(0, 0, "café").unwrap();
    review.correct(0, 1, "").unwrap();
    let corrected = &review.pages()[0].words[0];
    assert_eq!((corrected.origin, corrected.across, corrected.up), (geometry.origin, geometry.across, geometry.up));
    assert_eq!(review.words(), 1);
    assert_eq!(session.apply_ocr_review(&review).unwrap(), 1);
    let saved = session.save_bytes(id).unwrap();
    assert!(saved.starts_with(&original));
    assert_eq!(text(saved.clone()).trim(), "café");
    assert_eq!(pixels(saved.clone()), original_pixels);
    assert_eq!(session.get(id).unwrap().can_undo(), Some("Recognize text"));
    session.undo(id).unwrap();
    assert_eq!(session.get(id).unwrap().bytes, original);
    assert_eq!(session.get(id).unwrap().can_undo(), None);
    session.redo(id).unwrap();
    let reopened = session.open("saved.pdf", None, saved, None).unwrap();
    assert_eq!(text(session.get(reopened).unwrap().bytes.clone()).trim(), "café");
    assert_eq!(pixels(session.get(reopened).unwrap().bytes.clone()), original_pixels);
}

#[test]
fn cancel_at_every_boundary_discards_all_pages_including_completed_pages() {
    let (session, id) = document(2);
    let original = session.get(id).unwrap().bytes.clone();
    // Before render, after render, after inference; then the same on page two, then completion.
    for cancel_at in 0..7 {
        let reader = Reader::new();
        let mut checkpoint = 0;
        let result = session.ocr_job_checked(id, &[], settings()).unwrap().run(&reader, |_, _| {
            let proceed = checkpoint != cancel_at;
            checkpoint += 1;
            proceed
        });
        assert!(result.err().unwrap().contains("canceled"), "boundary {cancel_at}");
        assert_eq!(session.get(id).unwrap().bytes, original);
        assert_eq!(session.get(id).unwrap().can_undo(), None);
    }
}

#[test]
fn stale_review_rejects_edits_undo_redo_save_and_close() {
    for mutation in 0..5 {
        let (mut session, id) = document(1);
        let review = draft(&session, id);
        match mutation {
            0 => session.apply(id, visible_text()).unwrap(),
            1 => {
                session.apply(id, visible_text()).unwrap();
                session.undo(id).unwrap();
            }
            2 => {
                session.apply(id, visible_text()).unwrap();
                session.undo(id).unwrap();
                session.redo(id).unwrap();
            }
            3 => {
                let bytes = session.save_bytes(id).unwrap();
                session.mark_saved(id, bytes, None).unwrap();
            }
            _ => session.close(id),
        }
        let before = session.get(id).map(|d| (d.bytes.clone(), d.can_undo().map(str::to_owned), d.can_redo().map(str::to_owned)));
        assert!(session.apply_ocr_review(&review).is_err(), "mutation {mutation}");
        let after = session.get(id).map(|d| (d.bytes.clone(), d.can_undo().map(str::to_owned), d.can_redo().map(str::to_owned)));
        assert_eq!(after, before);
    }
}

#[test]
fn bad_settings_pages_words_and_corrections_fail_before_mutation() {
    let (mut session, id) = document(1);
    for dpi in [f32::NAN, f32::INFINITY, -1.0, 0.0, 71.9, 600.1] {
        assert!(session.ocr_job_checked(id, &[], OcrSettings { dpi, ..settings() }).is_err());
    }
    assert!(session.ocr_job_checked(id, &[], OcrSettings { language: "fr".into(), ..settings() }).is_err());
    for pages in [vec![1], vec![0, 0]] {
        assert!(session.ocr_job_checked(id, &pages, settings()).is_err());
    }
    let mut review = draft(&session, id);
    let before = review.pages().to_vec();
    for bad in ["文字".to_owned(), "line\nbreak".into(), "x".repeat(16_385)] {
        assert!(review.correct(0, 0, &bad).is_err());
    }
    assert!(review.correct(1, 0, "valid").is_err());
    assert!(review.correct(0, usize::MAX, "valid").is_err());
    assert_eq!(review.pages(), before);
    let word = PlacedWord { text: "good".into(), origin: [10.0, 10.0], across: [40.0, 0.0], up: [0.0, 12.0] };
    for bad in [
        PlacedWord { origin: [f64::NAN, 0.0], ..word.clone() },
        PlacedWord { up: [0.0; 2], ..word.clone() },
        PlacedWord { text: "文字".into(), ..word },
    ] {
        assert!(session.apply_ocr(id, &[pdfcraft_engine::ocr::OcrPage { page: 0, words: vec![bad], skipped: None, orientation: None }]).is_err());
    }
    assert_eq!(session.get(id).unwrap().can_undo(), None);
}

#[test]
fn oversized_pages_do_not_render_or_call_backend_and_backend_panics_are_contained() {
    let mut session = Session::new();
    let bytes = session.create_blank(14_400.0, 14_400.0, 1).unwrap();
    let id = session.open("huge.pdf", None, bytes, None).unwrap();
    let reader = Reader::new();
    let review = session.ocr_job_checked(id, &[], OcrSettings::default()).unwrap().run(&reader, |_, _| true).unwrap();
    assert_eq!(reader.calls.get(), 0);
    assert!(review.pages()[0].skipped.as_ref().unwrap().contains("dimensions"));
    assert_eq!(session.apply_ocr_review(&review).unwrap(), 0);
    assert_eq!(session.get(id).unwrap().can_undo(), None);
    let (session, id) = document(1);
    let result = session.ocr_job_checked(id, &[], settings()).unwrap().run(&Reader { calls: Cell::new(0), panic: true }, |_, _| true);
    assert!(result.err().unwrap().contains("unexpectedly"));
    assert_eq!(session.get(id).unwrap().can_undo(), None);
}

#[test]
fn invisible_ocr_does_not_leak_text_rendering_mode_to_later_visible_content() {
    let (mut session, id) = document(1);
    let review = draft(&session, id);
    session.apply_ocr_review(&review).unwrap();
    session.apply(id, visible_text()).unwrap();
    let actual = pixels(session.save_bytes(id).unwrap());
    let (mut expected, expected_id) = document(1);
    let blank = pixels(expected.get(expected_id).unwrap().bytes.clone());
    expected.apply(expected_id, visible_text()).unwrap();
    assert_ne!(actual, blank, "later visible text must draw");
    assert_eq!(actual, pixels(expected.save_bytes(expected_id).unwrap()));
}

#[test]
fn rotated_page_words_are_mapped_back_to_the_existing_page_geometry() {
    let (mut session, id) = document(1);
    session.apply(id, Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
    let review = draft(&session, id);
    let info = &session.get(id).unwrap().info.pages[0];
    let word = &review.pages()[0].words[0];
    let origin = info.view_to_user(10.0, 25.0);
    let corner = info.view_to_user(50.0, 25.0);
    assert_eq!(word.origin, [f64::from(origin[0]), f64::from(origin[1])]);
    assert_eq!(word.across, [f64::from(corner[0] - origin[0]), f64::from(corner[1] - origin[1])]);
}

/// An original one-page document protected through the engine with permissions password
/// `owner`, saved and returned as bytes (no open password, so anyone can open it restricted).
fn protected(copy: bool, modify: bool) -> Arc<Vec<u8>> {
    let (mut session, id) = document(1);
    let changes = if modify { pdfcraft_engine::Changes::AnyExceptExtract } else { pdfcraft_engine::Changes::None };
    let protection = pdfcraft_engine::Protection { permissions_password: Some("owner".into()), changes, copy, ..Default::default() };
    session.apply(id, Edit::Protect(protection)).unwrap();
    session.save_bytes(id).unwrap()
}
fn state(session: &Session, id: DocId) -> (Arc<Vec<u8>>, Option<String>, Option<String>, bool, u64) {
    let d = session.get(id).unwrap();
    (d.bytes.clone(), d.can_undo().map(str::to_owned), d.can_redo().map(str::to_owned), d.dirty, d.edit_generation())
}

#[test]
fn restricted_documents_refuse_recognition_before_rendering_or_backend_inference() {
    for (copy, modify, why) in [(false, true, "copying"), (true, false, "changes"), (false, false, "copying")] {
        let bytes = protected(copy, modify);
        let mut session = Session::new();
        let id = session.open("restricted.pdf", None, bytes.clone(), None).unwrap();
        let d = session.get(id).unwrap();
        assert!(d.info.encrypted && d.allows_copying() == copy && d.allows_modification() == modify, "fixture {copy} {modify}");
        let before = state(&session, id);
        let reader = Reader::new();
        let error = session.ocr_job_checked(id, &[], settings()).map(|job| job.run(&reader, |_, _| true)).err().unwrap();
        assert!(error.contains(why) && error.contains("permissions password"), "{copy} {modify}: {error}");
        assert!(session.ocr_job(id, &[], settings()).is_none());
        assert_eq!(reader.calls.get(), 0, "the backend must not run");
        assert_eq!(session.ocr_authority(id).unwrap_err(), error);
        let words = vec![PlacedWord { text: "x".into(), origin: [10.0, 10.0], across: [20.0, 0.0], up: [0.0, 10.0] }];
        assert!(session.apply_ocr(id, &[pdfcraft_engine::ocr::OcrPage { page: 0, words, skipped: None, orientation: None }]).is_err());
        assert_eq!(state(&session, id), before, "no text, data or history change");
        assert_eq!(session.get(id).unwrap().bytes, bytes);
    }
}

#[test]
fn owner_password_and_unrestricted_permissions_allow_recognition() {
    for (bytes, password) in [(protected(false, false), Some("owner")), (protected(true, true), None)] {
        let mut session = Session::new();
        let id = session.open("protected.pdf", None, bytes, password).unwrap();
        session.ocr_authority(id).unwrap();
        let reader = Reader::new();
        let review = session.ocr_job_checked(id, &[], settings()).unwrap().run(&reader, |_, _| true).unwrap();
        assert_eq!(reader.calls.get(), 1);
        assert_eq!(session.apply_ocr_review(&review).unwrap(), 2);
        assert_eq!(session.get(id).unwrap().can_undo(), Some("Recognize text"));
        let saved = session.save_bytes(id).unwrap();
        let mut reopened = Session::new();
        let rid = reopened.open("saved.pdf", None, saved, Some("owner")).unwrap();
        let text =
            PageRenderer::new(reopened.get(rid).unwrap().bytes.clone(), RenderConfig { password: Some(Arc::from("owner")), ..Default::default() })
                .render(RenderRequest { kind: RequestKind::Text, scale: 1.0, ..Default::default() })
                .text
                .unwrap()
                .plain_text();
        assert!(text.contains("Cafe"), "{text}");
    }
}

#[test]
fn allowed_drafts_are_refused_when_permissions_or_revision_later_change() {
    // Prepared with owner authority; the same document version without it (same id, revision
    // and bytes) must not see, correct or apply the draft.
    let bytes = protected(false, true);
    let mut owner = Session::new();
    let id = owner.open("p.pdf", None, bytes.clone(), Some("owner")).unwrap();
    let review = draft(&owner, id);
    owner.validate_ocr_review(&review).unwrap();
    let mut restricted = Session::new();
    let rid = restricted.open("p.pdf", None, bytes, None).unwrap();
    assert_eq!((rid, restricted.get(rid).unwrap().edit_generation()), (review.document(), review.generation()));
    let before = state(&restricted, rid);
    let error = restricted.validate_ocr_review(&review).unwrap_err();
    assert!(error.contains("copying"), "{error}");
    assert!(restricted.apply_ocr_review(&review).is_err());
    assert_eq!(state(&restricted, rid), before);

    // The owner changes the security (a new revision): the earlier draft is stale.
    let tighter = pdfcraft_engine::Protection { permissions_password: Some("owner2".into()), ..Default::default() };
    owner.apply(id, Edit::Protect(tighter)).unwrap();
    let before = state(&owner, id);
    assert!(owner.validate_ocr_review(&review).unwrap_err().contains("changed"));
    assert!(owner.apply_ocr_review(&review).is_err());
    assert_eq!(state(&owner, id), before);
}
