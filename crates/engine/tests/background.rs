//! Background removal through the engine: as Recognize text's preprocessing (Exact and
//! Searchable Image) and as the standalone undoable edit. Original synthetic fixtures; a
//! deterministic recognizer stands in for the models.
use std::cell::RefCell;
use std::sync::Arc;

use pdfcraft_engine::{
    DocId, Session,
    ocr::{BackgroundOptions, BackgroundStrength, OcrSettings, Recognizer},
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

const PAPER: [(u32, u32); 3] = [(30, 30), (450, 60), (520, 240)];
const TEXT: [(u32, u32); 2] = [(81, 140), (201, 130)];

/// A 600 × 300 px gray scan on grey paper (luma about 200, dithered) with text-like stems,
/// shown 144 × 72 pt: 300 ppi.
fn scan() -> (Session, DocId) {
    let img = image::GrayImage::from_fn(600, 300, |x, y| {
        let stem = (0..8).any(|i| (80 + i * 14..84 + i * 14).contains(&x)) && (120..150).contains(&y);
        let i = (200..204).contains(&x) && (120..150).contains(&y);
        image::Luma([if stem || i { 10 } else { (196 + (x * 3 + y * 5) % 9) as u8 }])
    });
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let mut session = Session::new().with_clock(|| 1_700_000_000);
    // The scan fills its page, as a scanner's page does.
    let bytes = session.create_from_images_with_resolution(&[("scan.png".into(), png)], pdfcraft_engine::ImageResolution::Dpi(300.0)).unwrap();
    let id = session.open("scan.pdf", None, bytes, None).unwrap();
    (session, id)
}

/// The page drawn at the image's resolution: luma of image pixel (x, y), y down.
fn luma_at(bytes: Arc<Vec<u8>>, points: &[(u32, u32)]) -> Vec<u8> {
    let shot = PageRenderer::new(bytes, RenderConfig::default()).render(RenderRequest { scale: 300.0 / 72.0, ..Default::default() });
    assert!(shot.error.is_none(), "{:?}", shot.error);
    let top = shot.height - 300;
    points.iter().map(|(x, y)| shot.rgba[(((top + y + 1) * shot.width + x + 1) * 4) as usize]).collect()
}

fn medium() -> BackgroundOptions {
    BackgroundOptions { strength: BackgroundStrength::Medium, ..Default::default() }
}

#[test]
fn recognition_reads_a_whitened_copy_and_exact_keeps_the_page() {
    let (mut session, id) = scan();
    let original = session.get(id).unwrap().bytes.clone();
    assert!(luma_at(original, &PAPER).iter().all(|l| (190..215).contains(l)), "the fixture has grey paper");
    let spy = Spy(RefCell::new(None));
    let settings = OcrSettings { background: medium(), ..Default::default() };
    let review = session.ocr_job_checked(id, &[], settings).unwrap().run(&spy, |_, _| true).unwrap();
    let page = &review.pages()[0];
    assert!(page.skipped.is_none(), "{:?}", page.skipped);
    let stats = page.background.expect("background stats");
    assert!(stats.background.is_some_and(|b| (190..215).contains(&b)), "{stats:?}");
    assert!(stats.pixels_whitened > 0, "{stats:?}");
    assert_eq!(page.despeckle, None);
    let (seen, w, _) = spy.0.borrow().clone().unwrap();
    let h = seen.len() as u32 / w;
    assert_eq!(seen.len(), (w * h) as usize, "a gray copy");
    let top = h - 300;
    for (x, y) in PAPER {
        assert_eq!(seen[((top + y + 1) * w + x + 1) as usize], 255, "paper at {x},{y} was read grey");
    }
    for (x, y) in TEXT {
        assert!(seen[((top + y) * w + x) as usize] < 64, "text at {x},{y} was washed out");
    }
    session.apply_ocr_review(&review).unwrap();
    let saved = session.save_bytes(id).unwrap();
    assert!(luma_at(saved, &PAPER).iter().all(|l| *l < 230), "Exact leaves the page image alone");
    // Without the option the grey paper reaches the recognizer.
    let (session, id) = scan();
    let review = session.ocr_job_checked(id, &[], OcrSettings::default()).unwrap().run(&Spy(RefCell::new(None)), |_, _| true).unwrap();
    assert_eq!(review.pages()[0].background, None);
    // A white point below the minimum is refused before any page is read.
    let bad = OcrSettings { background: BackgroundOptions { white_point: Some(10), ..Default::default() }, ..Default::default() };
    assert!(session.ocr_job_checked(id, &[], bad).is_err());
}

#[test]
fn searchable_image_whitens_the_page_in_the_recognition_step() {
    let (mut session, id) = scan();
    let original = session.get(id).unwrap().bytes.clone();
    let settings = OcrSettings { background: medium(), straighten: true, ..Default::default() };
    let review = session.ocr_job_checked(id, &[], settings).unwrap().run(&Spy(RefCell::new(None)), |_, _| true).unwrap();
    session.apply_ocr_review(&review).unwrap();
    assert_eq!(session.get(id).unwrap().can_undo(), Some("Recognize text"), "one step");
    let saved = session.save_bytes(id).unwrap();
    assert!(luma_at(saved.clone(), &PAPER).iter().all(|l| *l > 245), "paper left grey on the page");
    assert!(luma_at(saved, &TEXT).iter().all(|l| *l < 64), "text washed out on the page");
    session.undo(id).unwrap();
    assert_eq!(session.get(id).unwrap().bytes, original);
}

#[test]
fn remove_scan_backgrounds_is_one_undoable_step_reporting_what_changed() {
    let (mut session, id) = scan();
    let original = session.get(id).unwrap().bytes.clone();
    // Nothing to do: no step.
    assert_eq!(session.remove_scan_backgrounds(id, &[], BackgroundOptions::default()).unwrap().images_changed, 0);
    assert_eq!(session.get(id).unwrap().can_undo(), None);
    assert!(session.remove_scan_backgrounds(id, &[3], medium()).is_err());
    assert!(session.remove_scan_backgrounds(id, &[0, 0], medium()).is_err());
    assert!(session.remove_scan_backgrounds(id, &[0], BackgroundOptions { white_point: Some(50), ..Default::default() }).is_err());
    let report = session.remove_scan_backgrounds(id, &[0], medium()).unwrap();
    assert_eq!((report.images, report.images_changed, report.images_skipped), (1, 1, 0), "{report:?}");
    assert!(report.pixels_whitened > 150_000, "{report:?}");
    assert_eq!(session.get(id).unwrap().can_undo(), Some("Remove background"));
    let saved = session.save_bytes(id).unwrap();
    assert!(saved.starts_with(&original), "incremental save");
    assert!(luma_at(saved.clone(), &PAPER).iter().all(|l| *l > 245));
    assert!(luma_at(saved, &TEXT).iter().all(|l| *l < 64));
    // A second pass finds white paper and adds no step.
    assert_eq!(session.remove_scan_backgrounds(id, &[], medium()).unwrap().images_changed, 0);
    session.undo(id).unwrap();
    assert_eq!(session.get(id).unwrap().bytes, original);
    assert_eq!(session.get(id).unwrap().can_undo(), None);
}
