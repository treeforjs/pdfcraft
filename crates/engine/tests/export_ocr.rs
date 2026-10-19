//! Export ▸ Run OCR (`Session::with_export_ocr_using`) on original synthetic fixtures with a
//! deterministic stand-in recognizer: which pages are recognized in each mode, that the export
//! sees the recognized copy while the document itself never changes, and what happens without
//! models or permission.
use std::{
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use pdfcraft_engine::{
    DocId, Session,
    export::{Exporter, encode_png},
    export_ocr::{ExportOcr, ExportOcrMode},
    ocr::{EngineInfo, OcrEngine, Recognizer},
};
use pdfcraft_ocr::{Line, OcrError, Word};

struct Reader {
    calls: AtomicUsize,
}

impl Recognizer for Reader {
    fn recognize(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<Line>, OcrError> {
        pdfcraft_ocr::validate_image(pixels, width, height)?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(vec![Line {
            words: vec![
                Word { text: "Quarterly".into(), rect: [10.0, 10.0, 60.0, 25.0], confidence: None },
                Word { text: "harvest".into(), rect: [65.0, 10.0, 110.0, 25.0], confidence: None },
            ],
        }])
    }
}

/// A gray image-only page, then a page with typed text, then a blank page.
fn document() -> (Session, DocId) {
    let mut session = Session::new().with_clock(|| 1_700_000_000);
    let rgba: Vec<u8> = (0..120 * 90).flat_map(|i| if i % 7 == 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] }).collect();
    let png = encode_png(120, 90, &rgba).unwrap();
    let scan = session.create_from_images(&[("scan.png".into(), png)]).unwrap();
    let id = session.open("scan.pdf", None, scan, None).unwrap();
    let typed = session.create_from_text("Typed", "Lighthouse keepers logged calm seas.").unwrap();
    session
        .apply(id, pdfcraft_engine::Edit::InsertPagesFrom { name: "typed.pdf".into(), bytes: typed, pages: None, at: 1, merge_tags: true })
        .unwrap();
    session.apply(id, pdfcraft_engine::Edit::InsertBlankPage { at: 2, width: 120.0, height: 90.0 }).unwrap();
    let saved = session.save_bytes(id).unwrap();
    session.mark_saved(id, saved, None).unwrap();
    (session, id)
}

fn text(d: &pdfcraft_engine::Document) -> Result<String, String> {
    Exporter::new(d).text_of(&[0, 1, 2])
}

fn options(mode: ExportOcrMode) -> ExportOcr {
    ExportOcr { mode, ..Default::default() }
}

#[test]
fn auto_recognizes_only_scans_into_a_copy() {
    let (session, id) = document();
    let before = session.get(id).unwrap().bytes.clone();
    let (undo, generation) = (session.get(id).unwrap().can_undo().map(str::to_owned), session.get(id).unwrap().edit_generation());
    let reader = Arc::new(Reader { calls: AtomicUsize::new(0) });
    let (out, report) = session.with_export_ocr_using(id, &[], &ExportOcr::default(), || Ok(Some(reader.clone())), text).unwrap();
    assert_eq!(report.mode, ExportOcrMode::Auto);
    assert_eq!(report.pages_without_text, vec![0, 2]);
    assert_eq!(report.scanned_pages, vec![0]);
    assert_eq!(report.recognized, vec![0]);
    assert_eq!(report.words, 2);
    assert!(report.not_run.is_none());
    assert_eq!(reader.calls.load(Ordering::Relaxed), 1, "only the scan is read");
    let pages: Vec<&str> = out.split('\u{c}').collect();
    assert!(pages[0].contains("Quarterly harvest"), "{out:?}");
    assert!(pages[1].contains("Lighthouse"), "{out:?}");
    let doc = session.get(id).unwrap();
    assert!(doc.bytes == before && !doc.dirty, "the document is unchanged");
    assert_eq!((doc.can_undo().map(str::to_owned), doc.edit_generation()), (undo, generation), "and so is its history");
    assert!(!text(doc).unwrap().contains("Quarterly"));
}

#[test]
fn on_recognizes_every_page_without_text_and_off_none() {
    let (session, id) = document();
    let reader = Arc::new(Reader { calls: AtomicUsize::new(0) });
    let (out, report) = session.with_export_ocr_using(id, &[], &options(ExportOcrMode::On), || Ok(Some(reader.clone())), text).unwrap();
    assert_eq!(report.recognized, vec![0, 2]);
    assert_eq!(reader.calls.load(Ordering::Relaxed), 2);
    assert_eq!(out.matches("Quarterly").count(), 2, "{out:?}");

    let loaded = Cell::new(false);
    let load = || {
        loaded.set(true);
        Ok(Some(reader.clone()))
    };
    let (out, report) = session.with_export_ocr_using(id, &[], &options(ExportOcrMode::Off), load, text).unwrap();
    assert!(!loaded.get(), "off loads no recognizer");
    assert!(!report.ran() && report.pages_without_text.is_empty());
    assert!(!out.contains("Quarterly"));
}

#[test]
fn only_the_exported_pages_are_looked_at() {
    let (session, id) = document();
    let reader = Arc::new(Reader { calls: AtomicUsize::new(0) });
    let loaded = Cell::new(false);
    let load = || {
        loaded.set(true);
        Ok(Some(reader.clone()))
    };
    let (_, report) = session.with_export_ocr_using(id, &[1], &options(ExportOcrMode::On), load, |d| Exporter::new(d).text_of(&[1])).unwrap();
    assert!(report.pages_without_text.is_empty() && !report.ran());
    assert!(!loaded.get(), "nothing to recognize loads no recognizer");
    let missing = session.with_export_ocr_using(id, &[3], &ExportOcr::default(), || Ok(Some(reader.clone())), text);
    assert!(missing.unwrap_err().contains("page 4 does not exist"));
}

#[test]
fn without_models_auto_exports_as_is_and_on_fails() {
    let (session, id) = document();
    let none = || Ok(None::<Arc<Reader>>);
    let (out, report) = session.with_export_ocr_using(id, &[], &ExportOcr::default(), none, text).unwrap();
    assert!(!report.ran());
    assert!(report.not_run.as_deref().unwrap().contains("no OCR models"), "{report:?}");
    assert!(out.contains("Lighthouse") && !out.contains("Quarterly"));
    let exported = Cell::new(false);
    let err = session
        .with_export_ocr_using(id, &[], &options(ExportOcrMode::On), none, |d| {
            exported.set(true);
            text(d)
        })
        .unwrap_err();
    assert!(err.contains("no OCR models") && !exported.get(), "{err}");
    let bad = ExportOcr { language: "xx".into(), ..Default::default() };
    assert!(session.with_export_ocr_using(id, &[], &bad, none, text).unwrap_err().contains("unsupported recognition language"));
}

#[test]
fn a_document_that_forbids_changes_is_not_recognized_in_auto_and_refused_when_on() {
    let (mut session, id) = document();
    let protect = pdfcraft_engine::Edit::Protect(pdfcraft_engine::Protection {
        permissions_password: Some("owner".into()),
        changes: pdfcraft_engine::Changes::None,
        copy: true,
        ..Default::default()
    });
    session.apply(id, protect).unwrap();
    let bytes = session.save_bytes(id).unwrap();
    let mut restricted = Session::new();
    let rid = restricted.open("restricted.pdf", None, bytes, None).unwrap();
    let reader = Arc::new(Reader { calls: AtomicUsize::new(0) });
    let (_, report) = restricted.with_export_ocr_using(rid, &[], &ExportOcr::default(), || Ok(Some(reader.clone())), text).unwrap();
    assert!(!report.ran() && report.not_run.as_deref().unwrap().contains("does not allow changes"), "{report:?}");
    let err = restricted.with_export_ocr_using(rid, &[], &options(ExportOcrMode::On), || Ok(Some(reader.clone())), text).unwrap_err();
    assert!(err.contains("does not allow changes"), "{err}");
    assert_eq!(reader.calls.load(Ordering::Relaxed), 0, "nothing was read");
}

/// A registered engine that reads German, for the selected-engine path.
struct German {
    calls: AtomicUsize,
}

impl OcrEngine for German {
    fn id(&self) -> &str {
        "german"
    }
    fn name(&self) -> &str {
        "German test engine"
    }
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: "german".into(),
            name: self.name().into(),
            available: true,
            version: None,
            languages: vec!["de".into()],
            external: false,
            detail: String::new(),
        }
    }
    fn prepare(&self, _: &str) -> Result<(), OcrError> {
        Ok(())
    }
    fn recognize(&self, pixels: &[u8], width: u32, height: u32, language: &str) -> Result<Vec<Line>, OcrError> {
        pdfcraft_ocr::validate_image(pixels, width, height)?;
        assert_eq!(language, "de");
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(vec![Line { words: vec![Word { text: "Leuchtturm".into(), rect: [10.0, 10.0, 70.0, 25.0], confidence: Some(0.9) }] }])
    }
}

#[test]
fn exports_read_with_the_selected_engine_and_its_languages() {
    let (mut session, id) = document();
    let engine = Arc::new(German { calls: AtomicUsize::new(0) });
    session.register_ocr_engine(engine.clone()).unwrap();
    session.set_ocr_engine("german").unwrap();
    let german = ExportOcr { language: "de".into(), ..Default::default() };
    let (out, report) = session.with_export_ocr(id, &[], &german, text).unwrap();
    assert_eq!(report.recognized, vec![0], "{report:?}");
    assert!(out.contains("Leuchtturm") && out.contains("Lighthouse"), "{out}");
    assert_eq!(engine.calls.load(Ordering::Relaxed), 1);
    assert!(!session.get(id).unwrap().dirty, "the document itself is unchanged");

    // A language the selected engine doesn't read: Auto exports as is and says why, On fails.
    let french = ExportOcr { language: "fr".into(), ..Default::default() };
    let (out, report) = session.with_export_ocr(id, &[], &french, text).unwrap();
    assert!(!report.ran() && report.not_run.as_deref().unwrap().contains("the german engine reads German"), "{report:?}");
    assert!(!out.contains("Leuchtturm"));
    let on = ExportOcr { mode: ExportOcrMode::On, ..french };
    assert!(session.with_export_ocr(id, &[], &on, text).unwrap_err().contains("text recognition is on, but unsupported"));
    assert_eq!(engine.calls.load(Ordering::Relaxed), 1, "nothing more was read");
}

/// An engine that reads words with doubled letters, squeezed into narrow boxes as OCR finds them.
struct Doubled;

impl OcrEngine for Doubled {
    fn id(&self) -> &str {
        "doubled"
    }
    fn name(&self) -> &str {
        "Doubled-letter test engine"
    }
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: "doubled".into(),
            name: self.name().into(),
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
        let word = |text: &str, rect: [f32; 4]| Word { text: text.into(), rect, confidence: Some(0.9) };
        Ok(vec![
            Line { words: vec![word("letter", [10.0, 10.0, 32.0, 25.0]), word("settle", [40.0, 10.0, 62.0, 25.0])] },
            Line { words: vec![word("Leuchtturm", [10.0, 40.0, 70.0, 55.0])] },
        ])
    }
}

#[test]
fn doubled_letters_survive_recognize_text_and_export_ocr() {
    // A narrow letter of a word stretched to its box advanced less than the fake-bold
    // tolerance, so the second "t" of "tt" was dropped and read back as a space.
    let (mut session, id) = document();
    session.register_ocr_engine(Arc::new(Doubled)).unwrap();
    session.set_ocr_engine("doubled").unwrap();
    let (out, report) = session.with_export_ocr(id, &[], &ExportOcr::default(), text).unwrap();
    assert_eq!(report.recognized, vec![0], "{report:?}");
    let page = out.split('\u{c}').next().unwrap();
    assert!(page.contains("letter settle") && page.contains("Leuchtturm"), "{page:?}");

    let settings = pdfcraft_engine::ocr::OcrSettings { dpi: 72.0, ..Default::default() };
    session.recognize_text(id, &[0], settings).unwrap();
    let out = text(session.get(id).unwrap()).unwrap();
    let page = out.split('\u{c}').next().unwrap();
    assert!(page.contains("letter settle") && page.contains("Leuchtturm"), "{page:?}");
}
