//! Export a PDF with text recognition: the viewer's "Run OCR" export setting (parity
//! `create.export-ocr`).
//!
//! Exporting a scan to a text-bearing format (Word, RTF, HTML, Excel, PowerPoint, plain text)
//! gives nothing to read unless its pages are recognized first. [`Session::with_export_ocr`]
//! recognizes the pages that need it on a **temporary copy** of the document (its working file,
//! so unsaved edits count), hands that copy to the exporter, and drops it: the document itself,
//! its history and its dirty state never change.
//!
//! [`ExportOcrMode::Auto`] (the default, as in PDF) recognizes only scanned pages, those that
//! draw an image and have no text, and only when the session's selected OCR engine
//! ([`Session::ocr_engine_for`]) can run and reads the language; without it, or when the document's permissions don't allow recognition, the export goes ahead
//! without text recognition and the report says why. [`ExportOcrMode::On`] recognizes every
//! exported page without text and fails the export when it can't. [`ExportOcrMode::Off`] never
//! recognizes.

use std::sync::Arc;

use pdfcraft_render::{PageRenderer, RenderRequest, RequestKind};

use crate::ocr::{DEFAULT_ENGINE, EngineRecognizer, KNOWN_LANGUAGES, LANGUAGES, OcrEngine, OcrError, OcrJob, OcrReview, OcrSettings, Recognizer};
use crate::{DocId, Document, Session};

/// When an export runs text recognition first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportOcrMode {
    /// Recognize scanned pages (an image and no text) when the models are installed.
    #[default]
    Auto,
    /// Recognize every exported page without text; fail when that isn't possible.
    On,
    /// Never recognize.
    Off,
}

impl ExportOcrMode {
    pub const ALL: [ExportOcrMode; 3] = [ExportOcrMode::Auto, ExportOcrMode::On, ExportOcrMode::Off];

    pub fn id(self) -> &'static str {
        match self {
            ExportOcrMode::Auto => "auto",
            ExportOcrMode::On => "on",
            ExportOcrMode::Off => "off",
        }
    }

    pub fn from_id(id: &str) -> Option<ExportOcrMode> {
        Self::ALL.into_iter().find(|m| m.id() == id)
    }
}

/// The export's text recognition settings.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportOcr {
    pub mode: ExportOcrMode,
    /// Recognition language, a code from [`KNOWN_LANGUAGES`] the selected engine reads.
    pub language: String,
}

impl Default for ExportOcr {
    fn default() -> Self {
        ExportOcr { mode: ExportOcrMode::Auto, language: "en".into() }
    }
}

impl ExportOcr {
    /// The language is one any engine could read ([`KNOWN_LANGUAGES`]); whether the selected
    /// engine reads it is checked when recognition is needed.
    pub fn validate(&self) -> Result<(), String> {
        if !KNOWN_LANGUAGES.iter().any(|(code, _, _)| *code == self.language) {
            let codes: Vec<&str> = KNOWN_LANGUAGES.iter().map(|(c, _, _)| *c).collect();
            return Err(format!("unsupported recognition language {:?} (one of {})", self.language, codes.join(", ")));
        }
        Ok(())
    }

    /// The language is one the built-in engine reads ([`LANGUAGES`]).
    fn validate_bundled(&self) -> Result<(), String> {
        if !LANGUAGES.iter().any(|(code, _)| *code == self.language) {
            let codes: Vec<&str> = LANGUAGES.iter().map(|(c, _)| *c).collect();
            return Err(format!("unsupported recognition language {:?} (one of {})", self.language, codes.join(", ")));
        }
        Ok(())
    }
}

/// What text recognition did for an export. Pages are 0-based.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportOcrReport {
    pub mode: ExportOcrMode,
    pub language: String,
    /// Exported pages with no text.
    pub pages_without_text: Vec<usize>,
    /// Exported pages with no text that draw an image (scans).
    pub scanned_pages: Vec<usize>,
    /// Pages whose words were recognized into the exported copy.
    pub recognized: Vec<usize>,
    /// Pages recognition was asked for but left alone, and why.
    pub skipped_pages: Vec<(usize, String)>,
    pub words: usize,
    /// Why recognition did not run at all (`None` when it ran or nothing needed it).
    pub not_run: Option<String>,
}

impl ExportOcrReport {
    /// Whether the export came from a recognized copy.
    pub fn ran(&self) -> bool {
        !self.recognized.is_empty()
    }
}

const NO_MODELS: &str = "no OCR models are installed (cargo xtask models, or set PDFCRAFT_MODELS)";

/// The recognizer an export runs its job with, or why there is none.
struct Ready<'a> {
    /// The engine the temporary copy reads with (`None`: the copy's default).
    engine: Option<Arc<dyn OcrEngine>>,
    run: Box<dyn FnOnce(OcrJob) -> Result<OcrReview, String> + 'a>,
}

impl Session {
    /// Run `export` on document `id`, or on a temporary copy with the text of `pages` (0-based;
    /// empty = all) recognized first, as `options` say (see the module notes). Reads with the
    /// session's selected OCR engine ([`Session::ocr_engine_for`]); an engine that can't run, or
    /// doesn't read the language, counts as no recognizer (Auto exports as is and says why).
    pub fn with_export_ocr<T>(
        &self,
        id: DocId,
        pages: &[usize],
        options: &ExportOcr,
        export: impl FnOnce(&Document) -> Result<T, String>,
    ) -> Result<(T, ExportOcrReport), String> {
        let settings = OcrSettings { language: options.language.clone(), ..Default::default() };
        let load = || -> Result<Result<Ready<'static>, String>, String> {
            let engine = match self.ocr_engine_for(&settings) {
                Ok(e) => e,
                Err(why) => return Ok(Err(why)),
            };
            let info = engine.info();
            if !info.available {
                return Ok(Err(if info.id == DEFAULT_ENGINE {
                    NO_MODELS.to_owned()
                } else {
                    format!("the {} OCR engine can't run: {}", info.id, info.detail)
                }));
            }
            let language = settings.language.clone();
            let ready = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine.prepare(&language)))
                .unwrap_or_else(|_| Err(OcrError::Recognize("the recognition engine failed unexpectedly".into())));
            if let Err(e) = ready {
                return Ok(Err(e.to_string()));
            }
            let reader = engine.clone();
            let run = move |job: OcrJob| job.run(&EngineRecognizer { engine: reader.as_ref(), language: &language }, |_, _| true);
            Ok(Ok(Ready { engine: Some(engine), run: Box::new(run) }))
        };
        self.export_ocr_with(id, pages, options, load, export)
    }

    /// [`Session::with_export_ocr`] with the recognizer `load` gives (`None`: no models
    /// installed) instead of the selected engine. `load` is called only when a page needs
    /// recognizing.
    pub fn with_export_ocr_using<T, R: Recognizer>(
        &self,
        id: DocId,
        pages: &[usize],
        options: &ExportOcr,
        load: impl FnOnce() -> Result<Option<Arc<R>>, String>,
        export: impl FnOnce(&Document) -> Result<T, String>,
    ) -> Result<(T, ExportOcrReport), String> {
        options.validate_bundled()?;
        let load = || {
            let Some(ocr) = load()? else { return Ok(Err(NO_MODELS.to_owned())) };
            let run = move |job: OcrJob| job.run(ocr.as_ref(), |_, _| true);
            Ok(Ok(Ready { engine: None, run: Box::new(run) }))
        };
        self.export_ocr_with(id, pages, options, load, export)
    }

    fn export_ocr_with<'a, T>(
        &self,
        id: DocId,
        pages: &[usize],
        options: &ExportOcr,
        load: impl FnOnce() -> Result<Result<Ready<'a>, String>, String>,
        export: impl FnOnce(&Document) -> Result<T, String>,
    ) -> Result<(T, ExportOcrReport), String> {
        options.validate()?;
        let doc = self.get(id).ok_or("the document is not open")?;
        let n = doc.info.pages.len();
        if let Some(p) = pages.iter().find(|p| **p >= n) {
            return Err(format!("page {} does not exist (the document has {n} pages)", p.saturating_add(1)));
        }
        let mut report = ExportOcrReport { mode: options.mode, language: options.language.clone(), ..Default::default() };
        if options.mode == ExportOcrMode::Off {
            return Ok((export(doc)?, report));
        }
        let mut wanted: Vec<usize> = if pages.is_empty() { (0..n).collect() } else { pages.to_vec() };
        wanted.sort_unstable();
        wanted.dedup();
        let (without_text, scanned) = pages_needing_text(doc, &wanted);
        report.pages_without_text = without_text.clone();
        report.scanned_pages = scanned.clone();
        let targets = if options.mode == ExportOcrMode::On { without_text } else { scanned };
        let auto = options.mode == ExportOcrMode::Auto;
        if targets.is_empty() {
            return Ok((export(doc)?, report));
        }
        // Permissions first, so a restricted document loads no model.
        if let Err(why) = self.ocr_authority(id) {
            if auto {
                report.not_run = Some(why);
                return Ok((export(doc)?, report));
            }
            return Err(why);
        }
        let ready = match load()? {
            Ok(r) => r,
            Err(why) if auto => {
                report.not_run = Some(format!("{} scanned page(s) need text recognition, but {why}", targets.len()));
                return Ok((export(doc)?, report));
            }
            Err(why) => return Err(format!("text recognition is on, but {why}")),
        };
        // The temporary copy: the working file (unsaved edits included), opened as the
        // document was, reading with the same engine.
        let mut copy = Session::new();
        if let Some(engine) = ready.engine {
            let engine_id = engine.id().to_owned();
            copy.register_ocr_engine(engine)?;
            copy.set_ocr_engine(&engine_id)?;
        }
        let cid = copy.open(doc.name.clone(), None, doc.bytes.clone(), doc.password.as_deref()).map_err(|e| e.to_string())?;
        let settings = OcrSettings { language: options.language.clone(), ..Default::default() };
        let job = copy.ocr_job_checked(cid, &targets, settings)?;
        let review = (ready.run)(job)?;
        for p in review.pages() {
            match &p.skipped {
                Some(why) => report.skipped_pages.push((p.page, why.clone())),
                None if p.word_count() > 0 => {
                    report.recognized.push(p.page);
                    report.words = report.words.saturating_add(p.word_count());
                }
                None => {}
            }
        }
        if !report.ran() {
            return Ok((export(doc)?, report));
        }
        copy.apply_ocr_review(&review)?;
        let recognized = copy.get(cid).ok_or("the recognized copy closed unexpectedly")?;
        Ok((export(recognized)?, report))
    }
}

fn pages_needing_text(doc: &Document, pages: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let mut renderer = PageRenderer::new(doc.bytes.clone(), doc.config.clone());
    let (mut without, mut scanned) = (Vec::new(), Vec::new());
    for &page in pages {
        let r = renderer.render(RenderRequest { page, kind: RequestKind::Text, scale: 1.0, ..Default::default() });
        // A page whose text can't be read is treated as having text: recognizing it would only
        // add a second layer over whatever it holds.
        let has_text = match &r.text {
            Some(t) => !t.plain_text().trim().is_empty(),
            None => r.error.is_some(),
        };
        if has_text {
            continue;
        }
        without.push(page);
        if !doc.page_images(page).is_empty() {
            scanned.push(page);
        }
    }
    (without, scanned)
}
