//! Pluggable OCR engines: selection, per-job override, language and availability checks, and
//! every engine-crate entry point reading with the chosen engine. Uses a fake engine, so no
//! models or external programs are needed.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use pdfcraft_engine::{
    DocId, Session,
    ocr::{self, EngineInfo, OcrEngine, OcrSettings},
};
use pdfcraft_ocr::{Line, OcrError, Word};

struct Fake {
    id: &'static str,
    available: bool,
    languages: &'static [&'static str],
    panic: bool,
    prepared: AtomicUsize,
    calls: AtomicUsize,
    language: Mutex<String>,
}

impl Fake {
    fn new(id: &'static str) -> Arc<Fake> {
        Arc::new(Fake {
            id,
            available: true,
            languages: &["en", "de"],
            panic: false,
            prepared: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            language: Mutex::new(String::new()),
        })
    }
}

impl OcrEngine for Fake {
    fn id(&self) -> &str {
        self.id
    }
    fn name(&self) -> &str {
        "Fake engine"
    }
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: self.id.into(),
            name: self.name().into(),
            available: self.available,
            version: Some("1.2.3".into()),
            languages: self.languages.iter().map(|l| (*l).to_owned()).collect(),
            external: false,
            detail: if self.available { "in the test".into() } else { "the fake is switched off".into() },
        }
    }
    fn prepare(&self, language: &str) -> Result<(), OcrError> {
        assert!(!self.panic, "synthetic engine failure");
        self.prepared.fetch_add(1, Ordering::SeqCst);
        if !self.available {
            return Err(OcrError::Unavailable("the fake is switched off".into()));
        }
        *self.language.lock().unwrap() = language.to_owned();
        Ok(())
    }
    fn recognize(&self, pixels: &[u8], width: u32, height: u32, language: &str) -> Result<Vec<Line>, OcrError> {
        pdfcraft_ocr::validate_image(pixels, width, height)?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.language.lock().unwrap() = language.to_owned();
        let word = |text: &str, x: f32| Word { text: text.into(), rect: [x, 10.0, x + 30.0, 25.0], confidence: Some(0.9) };
        Ok(vec![Line { words: vec![word(self.id, 10.0), word("words", 45.0)] }])
    }
}

fn document() -> (Session, DocId) {
    let mut session = Session::new().with_clock(|| 1_700_000_000);
    let bytes = session.create_blank(120.0, 90.0, 1).unwrap();
    let id = session.open("scan.pdf", None, bytes, None).unwrap();
    (session, id)
}

fn settings() -> OcrSettings {
    OcrSettings { dpi: 72.0, ..Default::default() }
}

#[test]
fn the_built_in_engine_is_the_default_and_tesseract_is_listed() {
    let (mut session, _) = document();
    assert_eq!(session.ocr_engine_id(), ocr::DEFAULT_ENGINE);
    let engines = session.ocr_engines();
    let ids: Vec<&str> = engines.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["ocrs", "tesseract"]);
    assert_eq!(engines[0].languages, ["en"]);
    assert!(!engines[0].external && engines[1].external);
    // A Tesseract that isn't installed can't be chosen, and says why.
    session.set_tesseract_path(Some("/nonexistent/pdfcraft/tesseract".into())).unwrap();
    let tesseract = session.ocr_engines().into_iter().find(|e| e.id == "tesseract").unwrap();
    assert!(!tesseract.available && tesseract.detail.contains("not installed"), "{tesseract:?}");
    let refused = session.set_ocr_engine("tesseract").unwrap_err();
    assert!(refused.contains("not installed"), "{refused}");
    assert!(session.set_ocr_engine("nope").unwrap_err().contains("unknown OCR engine"));
    assert_eq!(session.ocr_engine_id(), "ocrs", "a refused choice changes nothing");
    let named = OcrSettings { engine: Some("tesseract".into()), ..settings() };
    assert!(session.ocr_engine_for(&named).is_err());
}

#[test]
fn the_selected_engine_reads_every_recognition() {
    let (mut session, id) = document();
    let fake = Fake::new("fake");
    session.register_ocr_engine(fake.clone()).unwrap();
    assert_eq!(session.set_ocr_engine("fake").unwrap().version.as_deref(), Some("1.2.3"));
    assert_eq!(session.ocr_engine_id(), "fake");

    // recognize_text (also what the accessibility and action paths call).
    let found = session.recognize_text(id, &[], OcrSettings { language: "de".into(), ..settings() }).unwrap();
    assert_eq!(found[0].text(), "fake words");
    assert_eq!(*fake.language.lock().unwrap(), "de");
    assert!(fake.prepared.load(Ordering::SeqCst) >= 1 && fake.calls.load(Ordering::SeqCst) >= 1);
    assert_eq!(session.get(id).unwrap().can_undo(), Some("Recognize text"));

    // A review draft (ocr_prepare and the dialog) and editable output read with it too.
    session.undo(id).unwrap();
    let job = session.ocr_job_checked(id, &[], settings()).unwrap();
    assert_eq!(job.ocr_engine().id(), "fake");
    let review = job.run_selected(|_, _| true).unwrap();
    assert_eq!(review.pages()[0].text(), "fake words");
    let editable = OcrSettings { editable: Some(Default::default()), ..settings() };
    let calls = fake.calls.load(Ordering::SeqCst);
    session.ocr_job_checked(id, &[], editable).unwrap().run_selected(|_, _| true).unwrap();
    assert!(fake.calls.load(Ordering::SeqCst) > calls, "editable output reads with the selected engine");

    // Recognize text in multiple files, with the engine resolved by the session.
    let bytes = session.get(id).unwrap().bytes.clone();
    let engine = session.ocr_engine_for(&settings()).unwrap();
    let file = ocr::recognize_file("scan.pdf", bytes, None, settings(), &engine, |_, _| true).unwrap();
    assert_eq!(file.words(), 2);
}

#[test]
fn a_job_can_name_another_engine_and_bad_choices_fail_before_reading() {
    let (mut session, id) = document();
    let (one, two) = (Fake::new("one"), Fake::new("two"));
    session.register_ocr_engine(one.clone()).unwrap();
    session.register_ocr_engine(two.clone()).unwrap();
    session.set_ocr_engine("one").unwrap();
    let found = session.recognize_text(id, &[], OcrSettings { engine: Some("two".into()), ..settings() }).unwrap();
    assert_eq!(found[0].text(), "two words");
    assert_eq!(one.calls.load(Ordering::SeqCst), 0);

    let before = session.get(id).unwrap().bytes.clone();
    let calls = two.calls.load(Ordering::SeqCst);
    for (bad, why) in [
        (OcrSettings { engine: Some("missing".into()), ..settings() }, "unknown OCR engine"),
        (OcrSettings { engine: Some("Bad Id".into()), ..settings() }, "invalid OCR engine id"),
        (OcrSettings { engine: Some("two".into()), language: "fr".into(), ..settings() }, "unsupported recognition language"),
        (OcrSettings { engine: Some("ocrs".into()), language: "de".into(), ..settings() }, "unsupported recognition language"),
        (OcrSettings { language: "xx".into(), ..settings() }, "unsupported recognition language"),
    ] {
        let error = session.ocr_job_checked(id, &[], bad.clone()).err().unwrap();
        assert!(error.contains(why), "{bad:?}: {error}");
        assert!(session.recognize_text(id, &[], bad).is_err());
    }
    assert_eq!(two.calls.load(Ordering::SeqCst), calls, "nothing was read");
    assert_eq!(session.get(id).unwrap().bytes, before);
}

#[test]
fn an_engine_that_cannot_run_fails_the_job_before_any_page() {
    let (mut session, id) = document();
    let off = Arc::new(Fake { available: false, ..Arc::into_inner(Fake::new("off")).unwrap() });
    session.register_ocr_engine(off.clone()).unwrap();
    assert!(session.set_ocr_engine("off").unwrap_err().contains("switched off"));
    let error = session.recognize_text(id, &[], OcrSettings { engine: Some("off".into()), ..settings() }).unwrap_err();
    assert!(error.contains("switched off"), "{error}");
    assert_eq!(off.calls.load(Ordering::SeqCst), 0);
    assert_eq!(session.get(id).unwrap().can_undo(), None);

    // A panic while readying the engine is contained.
    let panicking = Arc::new(Fake { panic: true, ..Arc::into_inner(Fake::new("boom")).unwrap() });
    session.register_ocr_engine(panicking).unwrap();
    let error = session.recognize_text(id, &[], OcrSettings { engine: Some("boom".into()), ..settings() }).unwrap_err();
    assert!(error.contains("unexpectedly"), "{error}");
    assert_eq!(session.get(id).unwrap().can_undo(), None);
}

#[test]
fn replacing_an_engine_keeps_it_selected() {
    let (mut session, id) = document();
    session.register_ocr_engine(Fake::new("fake")).unwrap();
    session.set_ocr_engine("fake").unwrap();
    let newer = Fake::new("fake");
    session.register_ocr_engine(newer.clone()).unwrap();
    session.recognize_text(id, &[], settings()).unwrap();
    assert!(newer.calls.load(Ordering::SeqCst) >= 1);
}
