//! Recognition paced for a single-threaded host (the web app): each page read in steps with a
//! pause between them, the same result as the unpaced run, cancellation within a page, and
//! failing or panicking engine steps ending in an error. Uses fake engines, so no models.
use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use pdfcraft_engine::ocr::{EngineInfo, OcrEngine, OcrSettings, Pace, Steps, Unpaced};
use pdfcraft_engine::{DocId, Session};
use pdfcraft_ocr::{Line, OcrError, Word};

#[derive(Clone, Copy, PartialEq)]
enum Fault {
    None,
    Fail,
    Panic,
}

/// Reads one line per step, `lines` of them; `begin` counts stepwise readings.
struct Stepper {
    lines: usize,
    fault: Fault,
    begun: AtomicUsize,
    whole: AtomicUsize,
    steps: Arc<AtomicUsize>,
}

fn stepper(lines: usize, fault: Fault) -> Arc<Stepper> {
    Arc::new(Stepper { lines, fault, begun: AtomicUsize::new(0), whole: AtomicUsize::new(0), steps: Arc::default() })
}

fn line(i: usize) -> Line {
    let x = 10.0 + i as f32;
    Line { words: vec![Word { text: format!("w{i}"), rect: [x, 10.0, x + 20.0, 25.0], confidence: None }] }
}

struct Reading {
    next: usize,
    lines: usize,
    fault: Fault,
    steps: Arc<AtomicUsize>,
}

impl Steps for Reading {
    fn step(&mut self) -> Result<Option<Vec<Line>>, OcrError> {
        self.steps.fetch_add(1, Ordering::SeqCst);
        self.next += 1;
        if self.next == 2 {
            match self.fault {
                Fault::Fail => return Err(OcrError::Recognize("synthetic step failure".into())),
                Fault::Panic => panic!("synthetic step panic"),
                Fault::None => {}
            }
        }
        Ok((self.next > self.lines).then(|| (0..self.lines).map(line).collect()))
    }
}

impl OcrEngine for Stepper {
    fn id(&self) -> &str {
        "stepper"
    }
    fn name(&self) -> &str {
        "Stepper"
    }
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: "stepper".into(),
            name: "Stepper".into(),
            available: true,
            version: None,
            languages: vec!["en".into()],
            external: false,
            detail: String::new(),
        }
    }
    fn prepare(&self, _: &str) -> Result<(), OcrError> {
        Ok(())
    }
    fn recognize(&self, pixels: &[u8], width: u32, height: u32, _: &str) -> Result<Vec<Line>, OcrError> {
        pdfcraft_ocr::validate_image(pixels, width, height)?;
        self.whole.fetch_add(1, Ordering::SeqCst);
        Ok((0..self.lines).map(line).collect())
    }
    fn begin<'a>(&'a self, pixels: &'a [u8], width: u32, height: u32, _: &'a str) -> Result<Box<dyn Steps + 'a>, OcrError> {
        pdfcraft_ocr::validate_image(pixels, width, height)?;
        self.begun.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Reading { next: 0, lines: self.lines, fault: self.fault, steps: self.steps.clone() }))
    }
}

/// Yields once at every pause, as the web app does when its frame budget is spent.
#[derive(Default)]
struct Yielding {
    pauses: usize,
}

struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

impl Pace for Yielding {
    fn stepwise(&self) -> bool {
        true
    }
    fn pause(&mut self) -> impl Future<Output = ()> {
        self.pauses += 1;
        YieldOnce(false)
    }
}

/// Poll to the end, counting how often the job gave the thread back.
fn drive<F: Future>(f: F) -> (F::Output, usize) {
    let mut f = pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    let mut pending = 0;
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return (v, pending);
        }
        pending += 1;
        assert!(pending < 100_000, "the job never finished");
    }
}

fn document(pages: usize, engine: Arc<Stepper>) -> (Session, DocId) {
    let mut session = Session::new().with_clock(|| 1_700_000_000);
    let bytes = session.create_blank(120.0, 90.0, pages).unwrap();
    let id = session.open("scan.pdf", None, bytes, None).unwrap();
    session.register_ocr_engine(engine).unwrap();
    session.set_ocr_engine("stepper").unwrap();
    (session, id)
}

fn settings() -> OcrSettings {
    OcrSettings { dpi: 72.0, ..Default::default() }
}

#[test]
fn paced_reading_steps_through_each_page_and_matches_the_unpaced_run() {
    let engine = stepper(5, Fault::None);
    let (session, id) = document(3, engine.clone());
    let unpaced = session.ocr_job_checked(id, &[], settings()).unwrap().run_selected(|_, _| true).unwrap();
    assert_eq!((engine.whole.load(Ordering::SeqCst), engine.begun.load(Ordering::SeqCst)), (3, 0), "desktop reads each page in one call");

    let mut pace = Yielding::default();
    let job = session.ocr_job_checked(id, &[], settings()).unwrap();
    let (paced, yielded) = drive(job.run_selected_paced(|_, _| true, &mut pace));
    let paced = paced.unwrap();
    assert_eq!(engine.begun.load(Ordering::SeqCst), 3, "each page read in steps");
    assert_eq!(engine.steps.load(Ordering::SeqCst), 3 * 6);
    // Five pauses between the six steps of each page, one after each render, one between pages.
    assert_eq!(pace.pauses, 3 * 5 + 3 + 2);
    assert_eq!(yielded, pace.pauses, "every pause gave the thread back");
    assert_eq!(paced.pages(), unpaced.pages());
    assert_eq!(paced.words(), 15);

    // The paced review applies like any other.
    let mut session = session;
    assert_eq!(session.apply_ocr_review(&paced).unwrap(), 15);
}

#[test]
fn an_unpaced_pace_is_the_plain_run() {
    let engine = stepper(2, Fault::None);
    let (session, id) = document(1, engine.clone());
    let job = session.ocr_job_checked(id, &[], settings()).unwrap();
    let (review, yielded) = drive(job.run_selected_paced(|_, _| true, &mut Unpaced));
    assert_eq!((review.unwrap().words(), yielded), (2, 0));
    assert_eq!(engine.begun.load(Ordering::SeqCst), 0);
}

#[test]
fn canceling_between_steps_stops_within_a_page_and_applies_nothing() {
    let engine = stepper(50, Fault::None);
    let (session, id) = document(2, engine.clone());
    let job = session.ocr_job_checked(id, &[], settings()).unwrap();
    let asked = AtomicUsize::new(0);
    // Cancel at the sixth question: inside the first page's reading.
    let progress = |_, _| asked.fetch_add(1, Ordering::SeqCst) < 5;
    let (result, _) = drive(job.run_selected_paced(progress, &mut Yielding::default()));
    assert!(result.err().unwrap_or_default().contains("canceled"));
    assert_eq!(engine.begun.load(Ordering::SeqCst), 1, "the second page was never started");
    assert!(engine.steps.load(Ordering::SeqCst) < 10, "stopped long before the page's 51 steps");
}

#[test]
fn a_failing_step_skips_the_page_with_its_reason() {
    let engine = stepper(3, Fault::Fail);
    let (session, id) = document(1, engine);
    let job = session.ocr_job_checked(id, &[], settings()).unwrap();
    let (review, _) = drive(job.run_selected_paced(|_, _| true, &mut Yielding::default()));
    let review = review.unwrap();
    assert_eq!(review.words(), 0);
    assert!(review.pages()[0].skipped.as_deref().unwrap_or_default().contains("synthetic step failure"));
}

#[test]
fn a_panicking_step_ends_the_job_with_an_error() {
    let engine = stepper(3, Fault::Panic);
    let (session, id) = document(1, engine);
    let job = session.ocr_job_checked(id, &[], settings()).unwrap();
    let (result, _) = drive(job.run_selected_paced(|_, _| true, &mut Yielding::default()));
    assert!(result.err().unwrap_or_default().contains("failed unexpectedly"));
}

/// An engine that can't split its work (Tesseract, or a registered one) reads each page in one
/// step under a stepwise pace.
#[test]
fn engines_without_steps_read_in_one_step() {
    struct Whole;
    impl OcrEngine for Whole {
        fn id(&self) -> &str {
            "whole"
        }
        fn name(&self) -> &str {
            "Whole"
        }
        fn info(&self) -> EngineInfo {
            EngineInfo {
                id: "whole".into(),
                name: "Whole".into(),
                available: true,
                version: None,
                languages: vec!["en".into()],
                external: false,
                detail: String::new(),
            }
        }
        fn prepare(&self, _: &str) -> Result<(), OcrError> {
            Ok(())
        }
        fn recognize(&self, _: &[u8], _: u32, _: u32, _: &str) -> Result<Vec<Line>, OcrError> {
            Ok(vec![line(0)])
        }
    }
    let mut session = Session::new();
    let bytes = session.create_blank(120.0, 90.0, 1).unwrap();
    let id = session.open("scan.pdf", None, bytes, None).unwrap();
    session.register_ocr_engine(Arc::new(Whole)).unwrap();
    let job = session.ocr_job_checked(id, &[], OcrSettings { engine: Some("whole".into()), ..settings() }).unwrap();
    let (review, _) = drive(job.run_selected_paced(|_, _| true, &mut Yielding::default()));
    assert_eq!(review.unwrap().words(), 1);
}

#[test]
fn files_recognize_paced_like_the_plain_batch() {
    let engine = stepper(3, Fault::None);
    let session = Session::new();
    let bytes = session.create_blank(120.0, 90.0, 2).unwrap();
    let engine: Arc<dyn OcrEngine> = engine;
    let plain = pdfcraft_engine::ocr::recognize_file("a.pdf", bytes.clone(), None, settings(), &engine, |_, _| true).unwrap();
    let mut pace = Yielding::default();
    let (paced, yielded) =
        drive(pdfcraft_engine::ocr::recognize_file_paced("a.pdf", bytes.clone(), None, settings(), &engine, |_, _| true, &mut pace));
    let paced = paced.unwrap();
    assert!(yielded > 0);
    assert_eq!((paced.words(), plain.words()), (6, 6));
    assert_eq!(paced.pages, plain.pages);
    // Not a PDF: an error, not a crash.
    let (bad, _) = drive(pdfcraft_engine::ocr::recognize_file_paced(
        "x.pdf",
        Arc::new(b"%PDF-1.7\n\xff\xff".to_vec()),
        None,
        settings(),
        &engine,
        |_, _| true,
        &mut Yielding::default(),
    ));
    assert!(bad.is_err());
}
