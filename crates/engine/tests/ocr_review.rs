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
        assert!(
            session
                .apply_ocr(
                    id,
                    &[pdfcraft_engine::ocr::OcrPage {
                        page: 0,
                        words: vec![bad],
                        skipped: None,
                        orientation: None,
                        straighten: false,
                        despeckle: None,
                        background: None
                    }]
                )
                .is_err()
        );
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
        assert!(
            session
                .apply_ocr(
                    id,
                    &[pdfcraft_engine::ocr::OcrPage {
                        page: 0,
                        words,
                        skipped: None,
                        orientation: None,
                        straighten: false,
                        despeckle: None,
                        background: None
                    }]
                )
                .is_err()
        );
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

/// A 300 × 200 pt page with `/Rotate rotate` of dark bars (text lines), drawn turned `degrees`
/// counterclockwise in user space.
fn skewed_scan(degrees: f64, rotate: i64) -> Arc<Vec<u8>> {
    let (s, c) = degrees.to_radians().sin_cos();
    let mut content = format!("q {c:.6} {s:.6} {:.6} {c:.6} 150 100 cm 0 g\n", -s);
    for line in 0..7 {
        content.push_str(&format!("-85 {} {} 6 re f\n", 60 - line * 20, 150 + (line * 13) % 30));
    }
    content.push_str("Q\n");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Rotate {rotate} /Contents 4 0 R /Resources << >> >>"),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    Arc::new(out)
}

/// The skew `orient::correct` finds on a rendering of `bytes`.
fn skew_of(bytes: Arc<Vec<u8>>) -> f32 {
    let shot = PageRenderer::new(bytes, RenderConfig::default()).render(RenderRequest { scale: 2.0, ..Default::default() });
    let options = pdfcraft_ocr::orient::Options { auto_rotate: false, deskew: true };
    pdfcraft_ocr::orient::correct(&shot.rgba, shot.width, shot.height, options, |_, _, _| Ok(Vec::new())).unwrap().orientation.skew
}

#[test]
fn searchable_image_straightens_a_skewed_page_with_its_text_layer() {
    for rotate in [0, 90] {
        let mut session = Session::new().with_clock(|| 1_700_000_000);
        // Seen on the turned page, the lines rise 6° to the right.
        let bytes = skewed_scan(6.0 + rotate as f64, rotate);
        let found = skew_of(bytes.clone());
        assert!((found - 6.0).abs() < 1.0, "the fixture reads as skewed: {found}");
        let id = session.open("scan.pdf", None, bytes.clone(), None).unwrap();
        let straight = OcrSettings { deskew: true, straighten: true, ..settings() };
        let review = session.ocr_job_checked(id, &[], straight).unwrap().run(&Reader::new(), |_, _| true).unwrap();
        assert!(review.pages()[0].straighten, "{:?}", review.pages()[0].orientation);
        assert_eq!(session.apply_ocr_review(&review).unwrap(), 2);
        let doc = session.get(id).unwrap();
        assert_eq!(doc.can_undo(), Some("Recognize text"), "one step");
        assert_eq!(doc.info.pages[0].rotation, rotate as u16, "no quarter turn");
        let saved = session.save_bytes(id).unwrap();
        let after = skew_of(saved.clone());
        assert!(after.abs() < 0.5, "straightened (/Rotate {rotate}): {after}");
        assert_eq!(text(saved).split_whitespace().collect::<Vec<_>>(), ["Cafe", "extra"], "the text layer is kept");
        // Exact (the default) leaves the page as it was.
        let mut exact = Session::new().with_clock(|| 1_700_000_000);
        let id = exact.open("scan.pdf", None, bytes.clone(), None).unwrap();
        let review = exact.ocr_job_checked(id, &[], OcrSettings { deskew: true, ..settings() }).unwrap().run(&Reader::new(), |_, _| true).unwrap();
        assert!(!review.pages()[0].straighten);
        exact.apply_ocr_review(&review).unwrap();
        assert!((skew_of(exact.save_bytes(id).unwrap()) - found).abs() < 0.2);
    }
}

#[test]
fn straightening_a_page_turns_its_content_and_rotate() {
    let mut session = Session::new().with_clock(|| 1_700_000_000);
    let id = session.open("scan.pdf", None, skewed_scan(0.0, 0), None).unwrap();
    let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    session.apply(id, Edit::StraightenPage { page: 0, matrix: identity, quarter_turns: 3 }).unwrap();
    assert_eq!(session.get(id).unwrap().info.pages[0].rotation, 270);
    for bad in [[0.0; 6], [f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0]] {
        assert!(session.apply(id, Edit::StraightenPage { page: 0, matrix: bad, quarter_turns: 0 }).is_err());
    }
    assert!(session.apply(id, Edit::StraightenPage { page: 5, matrix: identity, quarter_turns: 0 }).is_err());
    session.undo(id).unwrap();
    assert_eq!(session.get(id).unwrap().info.pages[0].rotation, 0);
}

#[test]
fn searchable_image_downsamples_finer_page_images_and_exact_keeps_them() {
    // A 400 × 200 px picture shown 100 pt wide: 288 ppi.
    let mut png = Vec::new();
    image::RgbImage::from_fn(400, 200, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let make = || {
        let mut session = Session::new().with_clock(|| 1_700_000_000);
        let bytes = session.create_from_images(&[("scan.png".into(), png.clone())]).unwrap();
        let id = session.open("scan.pdf", None, bytes, None).unwrap();
        let change = pdfcraft_engine::ImageEdit::Move([0.0, 0.0, 100.0, 50.0]);
        session.apply(id, Edit::EditPageImage { page: 0, index: 0, change }).unwrap();
        let bytes = session.save_bytes(id).unwrap();
        let mut session = Session::new().with_clock(|| 1_700_000_000);
        let id = session.open("scan.pdf", None, bytes, None).unwrap();
        (session, id)
    };
    let pixels_of = |session: &Session, id| {
        let image = &session.get(id).unwrap().page_images(0)[0];
        (image.width, image.height)
    };
    let (mut session, id) = make();
    assert_eq!(pixels_of(&session, id), (400, 200));
    let review = session.ocr_job_checked(id, &[], OcrSettings { straighten: true, ..settings() }).unwrap().run(&Reader::new(), |_, _| true).unwrap();
    session.apply_ocr_review(&review).unwrap();
    assert_eq!(pixels_of(&session, id), (100, 50), "down to 72 ppi");
    assert_eq!(session.get(id).unwrap().can_undo(), Some("Recognize text"), "one step");
    session.undo(id).unwrap();
    assert_eq!(pixels_of(&session, id), (400, 200));
    let (mut exact, id) = make();
    let review = exact.ocr_job_checked(id, &[], settings()).unwrap().run(&Reader::new(), |_, _| true).unwrap();
    exact.apply_ocr_review(&review).unwrap();
    assert_eq!(pixels_of(&exact, id), (400, 200), "Exact keeps the image");
}
