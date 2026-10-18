//! Despeckle through the engine: as Recognize text's preprocessing (Exact and Searchable Image)
//! and as the standalone undoable edit. Original synthetic fixtures; a deterministic recognizer
//! stands in for the models.
use std::cell::RefCell;
use std::sync::Arc;

use pdfcraft_engine::{
    DocId, Edit, Session,
    ocr::{DespeckleLevel, DespeckleOptions, OcrSettings, Recognizer},
};
use pdfcraft_ocr::{Line, OcrError, Word};
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest};

/// Remembers the image it was asked to read.
struct Spy(RefCell<Option<(Vec<u8>, u32, u32)>>);
impl Recognizer for Spy {
    fn recognize(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<Line>, OcrError> {
        pdfcraft_ocr::validate_image(pixels, width, height)?;
        *self.0.borrow_mut() = Some((pixels.to_vec(), width, height));
        Ok(vec![Line { words: vec![Word { text: "Scan".into(), rect: [10.0, 10.0, 50.0, 25.0], confidence: None }] }])
    }
}

const SPECKS: [(u32, u32); 3] = [(30, 30), (450, 60), (520, 240)];
const TEXT: [(u32, u32); 3] = [(81, 140), (201, 121), (208, 147)];

/// A 600 × 300 px gray scan (text-like stems, an i with its dot, a period; 3 × 3 specks far
/// away) shown 144 × 72 pt: 300 ppi.
fn scan() -> (Session, DocId) {
    let img = image::GrayImage::from_fn(600, 300, |x, y| {
        let stem = (0..8).any(|i| (80 + i * 14..84 + i * 14).contains(&x)) && (120..150).contains(&y);
        let i_dot = (200..204).contains(&x) && ((128..150).contains(&y) || (120..124).contains(&y));
        let period = (207..211).contains(&x) && (146..150).contains(&y);
        let speck = SPECKS.iter().any(|(sx, sy)| (*sx..sx + 3).contains(&x) && (*sy..sy + 3).contains(&y));
        image::Luma([if stem || i_dot || period || speck { 0 } else { 255 }])
    });
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let mut session = Session::new().with_clock(|| 1_700_000_000);
    let bytes = session.create_from_images(&[("scan.png".into(), png)]).unwrap();
    let id = session.open("scan.pdf", None, bytes, None).unwrap();
    session.apply(id, Edit::EditPageImage { page: 0, index: 0, change: pdfcraft_engine::ImageEdit::Move([0.0, 0.0, 144.0, 72.0]) }).unwrap();
    let bytes = session.save_bytes(id).unwrap();
    let mut session = Session::new().with_clock(|| 1_700_000_000);
    let id = session.open("scan.pdf", None, bytes, None).unwrap();
    (session, id)
}

/// The page image drawn at its own resolution: luma of image pixel (x, y), y down.
fn luma_at(bytes: Arc<Vec<u8>>, points: &[(u32, u32)]) -> Vec<u8> {
    let shot = PageRenderer::new(bytes, RenderConfig::default()).render(RenderRequest { scale: 300.0 / 72.0, ..Default::default() });
    assert!(shot.error.is_none(), "{:?}", shot.error);
    // The image fills the bottom-left 144 × 72 pt of the page.
    let top = shot.height - 300;
    points.iter().map(|(x, y)| shot.rgba[(((top + y + 1) * shot.width + x + 1) * 4) as usize]).collect()
}

fn high() -> DespeckleOptions {
    DespeckleOptions { level: DespeckleLevel::High, ..Default::default() }
}

#[test]
fn recognition_reads_a_despeckled_copy_and_exact_keeps_the_page() {
    let (mut session, id) = scan();
    let original = session.get(id).unwrap().bytes.clone();
    assert!(luma_at(original.clone(), &SPECKS).iter().all(|l| *l < 128), "the fixture has specks");
    let spy = Spy(RefCell::new(None));
    let settings = OcrSettings { despeckle: high(), ..Default::default() };
    let review = session.ocr_job_checked(id, &[], settings).unwrap().run(&spy, |_, _| true).unwrap();
    let page = &review.pages()[0];
    assert!(page.skipped.is_none(), "{:?}", page.skipped);
    let stats = page.despeckle.expect("despeckle stats");
    assert!(stats.dark_specks >= 3, "{stats:?}");
    let (seen, w, _) = spy.0.borrow().clone().unwrap();
    let h = seen.len() as u32 / w;
    assert_eq!(seen.len(), (w * h) as usize, "a gray copy");
    let top = h - 300;
    for (x, y) in SPECKS {
        assert_eq!(seen[((top + y + 1) * w + x + 1) as usize], 255, "speck at {x},{y} was read");
    }
    for (x, y) in TEXT {
        assert!(seen[((top + y) * w + x) as usize] < 128, "text at {x},{y} was removed from the copy");
    }
    session.apply_ocr_review(&review).unwrap();
    let saved = session.save_bytes(id).unwrap();
    assert!(luma_at(saved, &SPECKS).iter().all(|l| *l < 128), "Exact leaves the page image alone");
    // Without despeckle the specks reach the recognizer.
    let (session, id) = scan();
    let spy = Spy(RefCell::new(None));
    let review = session.ocr_job_checked(id, &[], OcrSettings::default()).unwrap().run(&spy, |_, _| true).unwrap();
    assert_eq!(review.pages()[0].despeckle, None);
}

#[test]
fn searchable_image_despeckles_the_page_in_the_recognition_step() {
    let (mut session, id) = scan();
    let original = session.get(id).unwrap().bytes.clone();
    let settings = OcrSettings { despeckle: high(), straighten: true, ..Default::default() };
    let review = session.ocr_job_checked(id, &[], settings).unwrap().run(&Spy(RefCell::new(None)), |_, _| true).unwrap();
    session.apply_ocr_review(&review).unwrap();
    assert_eq!(session.get(id).unwrap().can_undo(), Some("Recognize text"), "one step");
    let saved = session.save_bytes(id).unwrap();
    assert!(luma_at(saved.clone(), &SPECKS).iter().all(|l| *l > 200), "specks left on the page");
    assert!(luma_at(saved, &TEXT).iter().all(|l| *l < 128), "text removed from the page");
    session.undo(id).unwrap();
    assert_eq!(session.get(id).unwrap().bytes, original);
}

#[test]
fn despeckle_scans_is_one_undoable_step_reporting_what_changed() {
    let (mut session, id) = scan();
    let original = session.get(id).unwrap().bytes.clone();
    // Nothing to do: no step.
    assert_eq!(session.despeckle_scans(id, &[], DespeckleOptions::default()).unwrap().images_changed, 0);
    assert_eq!(session.get(id).unwrap().can_undo(), None);
    assert!(session.despeckle_scans(id, &[3], high()).is_err());
    assert!(session.despeckle_scans(id, &[0, 0], high()).is_err());
    let report = session.despeckle_scans(id, &[0], high()).unwrap();
    assert_eq!((report.images, report.images_changed, report.stats.dark_specks), (1, 1, 3), "{report:?}");
    assert_eq!(session.get(id).unwrap().can_undo(), Some("Despeckle"));
    let saved = session.save_bytes(id).unwrap();
    assert!(saved.starts_with(&original), "incremental save");
    assert!(luma_at(saved, &SPECKS).iter().all(|l| *l > 200));
    // A second pass finds nothing more and adds no step.
    assert_eq!(session.despeckle_scans(id, &[], high()).unwrap().images_changed, 0);
    session.undo(id).unwrap();
    assert_eq!(session.get(id).unwrap().bytes, original);
    assert_eq!(session.get(id).unwrap().can_undo(), None);
}
