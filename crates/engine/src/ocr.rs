//! Scan & OCR ▸ Recognize text: render pages, read the words in them and add an invisible text
//! layer ([`Edit::AddOcrText`]), making scanned pages searchable and selectable.
//!
//! Recognition is slow (about a second a page), so it is split from the edit: [`Session::ocr_job`]
//! captures what it needs, [`OcrJob::run`] works anywhere (the UI runs it on a worker thread) and
//! [`Session::apply_ocr_review`] applies the reviewed result as one undoable step, provided the
//! original document revision is still current.

use std::sync::{Arc, Mutex};

use pdfcraft_render::{PageInfo, PageRenderer, RenderConfig, RenderRequest, RequestKind};

pub use pdfcraft_ocr::orient::Orientation;
pub use pdfcraft_ocr::{LANGUAGES, Models, Ocr, OcrError, PlacedWord};

use crate::{DocId, Edit, EditError, Session};

/// Native recognition backends share the same bounded image and review workflow.
pub trait Recognizer {
    fn recognize(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<pdfcraft_ocr::Line>, OcrError>;
}

impl Recognizer for Ocr {
    fn recognize(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<pdfcraft_ocr::Line>, OcrError> {
        Ocr::recognize(self, pixels, width, height)
    }
}

/// Recognize Text settings.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrSettings {
    /// Resolution pages are rendered at for recognition (Acrobat's "Downsample to" choices).
    pub dpi: f32,
    /// Language code from [`LANGUAGES`]; unsupported alphabets are rejected.
    pub language: String,
    /// Leave pages that already have text alone (Acrobat reports "page contains renderable
    /// text" and skips them).
    pub skip_text_pages: bool,
    /// Detect pages turned by a quarter or half turn and read them upright
    /// ([`pdfcraft_ocr::orient`]). Off by default: it costs up to four extra recognitions of a
    /// reduced page.
    pub auto_rotate: bool,
    /// Detect skew of up to ±15° and read the page straightened. Off by default.
    pub deskew: bool,
}

impl Default for OcrSettings {
    fn default() -> Self {
        OcrSettings { dpi: 300.0, language: "en".into(), skip_text_pages: true, auto_rotate: false, deskew: false }
    }
}

impl OcrSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !self.dpi.is_finite() || !(72.0..=600.0).contains(&self.dpi) {
            return Err("recognition resolution must be finite and between 72 and 600 dpi".into());
        }
        if !LANGUAGES.iter().any(|(code, _)| *code == self.language) {
            return Err(format!("unsupported recognition language {:?}; the installed recognizer currently supports English", self.language));
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Source {
    doc: DocId,
    generation: u64,
    bytes: Arc<Vec<u8>>,
}

/// Recognized text remains a private draft until explicitly applied to the same document
/// version. Corrections change words only; source pixels, geometry and history stay intact.
#[derive(Clone)]
pub struct OcrReview {
    source: Source,
    pages: Vec<OcrPage>,
}

impl OcrReview {
    pub fn document(&self) -> DocId {
        self.source.doc
    }

    pub fn generation(&self) -> u64 {
        self.source.generation
    }

    pub fn pages(&self) -> &[OcrPage] {
        &self.pages
    }

    pub fn words(&self) -> usize {
        self.pages.iter().map(OcrPage::word_count).sum()
    }

    /// Recognized word slots, including words a correction emptied (memory accounting).
    pub fn word_slots(&self) -> usize {
        self.pages.iter().map(|p| p.words.len()).fold(0, usize::saturating_add)
    }

    /// UTF-8 bytes of recognized text held by the draft (memory accounting).
    pub fn text_bytes(&self) -> usize {
        self.pages.iter().flat_map(|p| &p.words).fold(0, |sum, w| sum.saturating_add(w.text.len()))
    }

    /// `page` is a document page index; `word` indexes that page's reviewed word list.
    /// Empty text removes that word from the searchable layer without changing the image.
    pub fn correct(&mut self, page: usize, word: usize, text: &str) -> Result<(), String> {
        pdfcraft_ocr::validate_text(text).map_err(|e| e.to_string())?;
        let slot = self.pages.iter().position(|p| p.page == page).ok_or("no such recognized page")?;
        let page = self.pages.get(slot).ok_or("no such recognized page")?;
        let old = page.words.get(word).ok_or("no such recognized word")?;
        let bytes = page.words.iter().map(|w| w.text.len()).sum::<usize>().saturating_sub(old.text.len()).saturating_add(text.len());
        if bytes > 16 * 1024 * 1024 {
            return Err("a page's recognized text must be at most 16 MiB".into());
        }
        let total_bytes = self
            .pages
            .iter()
            .flat_map(|p| &p.words)
            .fold(0usize, |sum, w| sum.saturating_add(w.text.len()))
            .saturating_sub(old.text.len())
            .saturating_add(text.len());
        if total_bytes > 64 * 1024 * 1024 {
            return Err("recognized text must be at most 64 MiB; recognize a smaller page range".into());
        }
        let target = self.pages.get_mut(slot).and_then(|p| p.words.get_mut(word)).ok_or("no such recognized word")?;
        target.text = text.to_owned();
        Ok(())
    }
}

/// What recognition found on one page.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrPage {
    pub page: usize,
    pub words: Vec<PlacedWord>,
    /// Why the page was not read, if it was skipped.
    pub skipped: Option<String>,
    /// The text direction found and corrected for recognition, when auto-rotate or deskew
    /// was on. The page image itself is never changed.
    pub orientation: Option<Orientation>,
}

impl OcrPage {
    /// The searchable text this page will receive: words left empty by a correction are
    /// omitted, as [`Session::apply_ocr`] omits them.
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ")
    }

    /// The number of words that will be added (non-empty after correction).
    pub fn word_count(&self) -> usize {
        self.words.iter().filter(|w| !w.text.trim().is_empty()).count()
    }
}

/// The recogniser, loaded once (it takes a moment) and shared.
pub fn engine() -> Result<Arc<Ocr>, String> {
    static OCR: Mutex<Option<Arc<Ocr>>> = Mutex::new(None);
    let mut slot = OCR.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = slot.as_ref() {
        return Ok(o.clone());
    }
    let o = Arc::new(Ocr::find().map_err(|e| e.to_string())?);
    *slot = Some(o.clone());
    Ok(o)
}

/// Whether the recognition models are installed.
pub fn available() -> bool {
    Models::find().is_some()
}

/// Everything recognition needs from a document, detached from the session.
pub struct OcrJob {
    source: Source,
    bytes: Arc<Vec<u8>>,
    password: Option<String>,
    infos: Vec<PageInfo>,
    /// Per page: the render scale (pixels per point) that shows its images at their own
    /// resolution, if it has any.
    native: Vec<Option<f32>>,
    pub pages: Vec<usize>,
    pub settings: OcrSettings,
}

impl OcrJob {
    /// Read the pages. `progress(done, total)` is called before each page; returning `false`
    /// cancels the entire draft. Checks also run after rendering and inference. Inference
    /// itself currently finishes before cancellation is observed; no partial result commits.
    pub fn run(self, ocr: &impl Recognizer, mut progress: impl FnMut(usize, usize) -> bool) -> Result<OcrReview, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_inner(ocr, &mut progress)))
            .unwrap_or_else(|_| Err("text recognition failed unexpectedly; no text was applied".into()))
    }

    fn run_inner(self, ocr: &impl Recognizer, progress: &mut impl FnMut(usize, usize) -> bool) -> Result<OcrReview, String> {
        self.settings.validate()?;
        let unique: std::collections::BTreeSet<_> = self.pages.iter().copied().collect();
        if unique.len() != self.pages.len() || self.pages.iter().any(|p| *p >= self.infos.len()) {
            return Err("recognition pages must be unique and present in the document".into());
        }
        let config = RenderConfig { password: self.password.as_deref().map(Arc::from), ..Default::default() };
        let mut r = PageRenderer::new(self.bytes.clone(), config);
        let mut out = Vec::new();
        let mut word_count = 0usize;
        let mut text_bytes = 0usize;
        let total = self.pages.len();
        for (done, &page) in self.pages.iter().enumerate() {
            if !progress(done, total) {
                return Err("text recognition canceled; no text was applied".into());
            }
            let Some(info) = self.infos.get(page) else { continue };
            let skip = |why: &str| OcrPage { page, words: Vec::new(), skipped: Some(why.into()), orientation: None };
            if self.settings.skip_text_pages {
                let t = r.render(RenderRequest { page, kind: RequestKind::Text, scale: 1.0, ..Default::default() });
                if t.text.as_ref().is_some_and(|t| !t.plain_text().trim().is_empty()) {
                    out.push(skip("the page already contains text"));
                    continue;
                }
            }
            // A scan is read at its own resolution (enlarging it only blurs the letters), at most
            // the chosen one.
            let wanted = self.settings.dpi / 72.0;
            let scale = self.native.get(page).copied().flatten().filter(|n| n.is_finite() && *n > 0.0).map_or(wanted, |n| n.clamp(1.0, wanted));
            let (width, height) = ((f64::from(info.width) * f64::from(scale)).ceil(), (f64::from(info.height) * f64::from(scale)).ceil());
            if !width.is_finite()
                || !height.is_finite()
                || width <= 0.0
                || height <= 0.0
                || width > f64::from(pdfcraft_ocr::MAX_IMAGE_SIDE)
                || height > f64::from(pdfcraft_ocr::MAX_IMAGE_SIDE)
                || width * height > pdfcraft_ocr::MAX_IMAGE_PIXELS as f64
            {
                out.push(skip("the recognition image exceeds the supported dimensions; choose a lower recognition resolution"));
                continue;
            }
            let shot = r.render(RenderRequest { page, scale, ..Default::default() });
            if !progress(done, total) {
                return Err("text recognition canceled; no text was applied".into());
            }
            if let Some(e) = shot.error {
                out.push(skip(&e));
                continue;
            }
            if let Err(error) = pdfcraft_ocr::validate_image(&shot.rgba, shot.width, shot.height) {
                out.push(skip(&error.to_string()));
                continue;
            }
            // Orientation and skew are corrected on a gray copy for recognition only; words are
            // mapped back through `to_source`, so the text layer lies over the unchanged image.
            let options = pdfcraft_ocr::orient::Options { auto_rotate: self.settings.auto_rotate, deskew: self.settings.deskew };
            let corrected = match pdfcraft_ocr::orient::correct(&shot.rgba, shot.width, shot.height, options, |p, w, h| ocr.recognize(p, w, h)) {
                Ok(c) => c,
                Err(e) => {
                    out.push(skip(&e.to_string()));
                    continue;
                }
            };
            if !progress(done, total) {
                return Err("text recognition canceled; no text was applied".into());
            }
            let read = match &corrected.image {
                Some(image) => ocr.recognize(&image.pixels, image.width, image.height),
                None => ocr.recognize(&shot.rgba, shot.width, shot.height),
            };
            let lines = match read {
                Ok(l) => l,
                Err(e) => {
                    out.push(skip(&e.to_string()));
                    continue;
                }
            };
            for word in lines.iter().flat_map(|line| &line.words) {
                word_count = word_count.saturating_add(1);
                text_bytes = text_bytes.saturating_add(word.text.len());
            }
            if word_count > 1_000_000 || text_bytes > 64 * 1024 * 1024 {
                return Err("recognition results exceed one million words or 64 MiB of text; recognize a smaller page range".into());
            }
            if !progress(done, total) {
                return Err("text recognition canceled; no text was applied".into());
            }
            let scale = shot.width as f32 / info.width.max(1e-3);
            let to_user = |x: f32, y: f32| {
                let [x, y] = corrected.to_source.apply(f64::from(x), f64::from(y));
                let [u, v] = info.view_to_user(x as f32 / scale, y as f32 / scale);
                [u as f64, v as f64]
            };
            let words = lines.iter().flat_map(|l| &l.words).map(|w| PlacedWord::place(w, to_user)).collect();
            let orientation = options.any().then_some(corrected.orientation);
            let found = OcrPage { page, words, skipped: None, orientation };
            if let Err(error) = pdfcraft_ocr::validate_words(&found.words) {
                out.push(skip(&error.to_string()));
            } else {
                out.push(found);
            }
        }
        if !progress(total, total) {
            return Err("text recognition canceled; no text was applied".into());
        }
        Ok(OcrReview { source: self.source, pages: out })
    }
}

impl Session {
    /// Capture what recognising `pages` (0-based; empty = all) of document `id` needs.
    pub fn ocr_job(&self, id: DocId, pages: &[usize], settings: OcrSettings) -> Option<OcrJob> {
        self.ocr_job_checked(id, pages, settings).ok()
    }

    /// Whether document `id` may be recognized now: recognition both reads its content (the
    /// recognized words are a copy of it) and adds a text layer, so it needs the same copy
    /// authority as text extraction ([`crate::Document::allows_copying`]) and the same
    /// modification authority as other content edits ([`crate::Document::allows_modification`]).
    /// Opening with the permissions (owner) password lifts both. Checked before rendering or
    /// inference and again whenever a draft is shown, corrected or applied.
    pub fn ocr_authority(&self, id: DocId) -> Result<(), String> {
        let doc = self.get(id).ok_or("the recognition document is not open")?;
        if let Some(why) = &doc.read_only_reason {
            return Err(why.clone());
        }
        if !doc.allows_copying() {
            return Err("this document does not allow copying text, so its text can't be recognized; open it with the permissions password".into());
        }
        if !doc.allows_modification() {
            return Err("this document does not allow changes, so recognized text can't be added; open it with the permissions password".into());
        }
        Ok(())
    }

    /// Checked job creation for user-facing callers, before model loading or page rendering.
    pub fn ocr_job_checked(&self, id: DocId, pages: &[usize], settings: OcrSettings) -> Result<OcrJob, String> {
        settings.validate()?;
        self.ocr_authority(id)?;
        let doc = self.get(id).ok_or("no such document")?;
        let all = doc.info.pages.len();
        if pages.iter().any(|p| *p >= all) || pages.len() > all {
            return Err(format!("recognition pages must be unique page indexes below {all}"));
        }
        let mut unique = std::collections::BTreeSet::new();
        if pages.iter().any(|p| !unique.insert(*p)) {
            return Err("recognition pages must not contain duplicates".into());
        }
        let pages: Vec<usize> = if pages.is_empty() { (0..all).collect() } else { pages.to_vec() };
        let native = (0..all)
            .map(|p| {
                if !pages.contains(&p) {
                    return None;
                }
                doc.page_images(p)
                    .iter()
                    .filter(|i| i.width > 1 && i.height > 1)
                    .map(|i| {
                        let m = i.matrix;
                        let (w, h) = (m[0].hypot(m[1]), m[2].hypot(m[3]));
                        (i.width as f64 / w.max(1e-6)).max(i.height as f64 / h.max(1e-6)) as f32
                    })
                    .reduce(f32::max)
            })
            .collect();
        let source = Source { doc: id, generation: doc.edit_generation(), bytes: doc.bytes.clone() };
        Ok(OcrJob { source, bytes: doc.bytes.clone(), password: doc.password.clone(), infos: doc.info.pages.clone(), native, pages, settings })
    }

    /// Refuse closed, restricted or stale targets before an OCR draft is shown, corrected or
    /// applied. Authority is checked against the document as it is now, then its revision.
    pub fn validate_ocr_review(&self, review: &OcrReview) -> Result<(), String> {
        let doc = self.get(review.source.doc).ok_or("the recognition document was closed")?;
        self.ocr_authority(review.source.doc)?;
        if doc.edit_generation() != review.source.generation || doc.bytes != review.source.bytes {
            return Err("the document changed after recognition started; recognize the current version again".into());
        }
        Ok(())
    }

    pub fn apply_ocr_review(&mut self, review: &OcrReview) -> Result<usize, String> {
        self.validate_ocr_review(review)?;
        self.apply_ocr(review.source.doc, &review.pages).map_err(|e| e.to_string())
    }

    /// Add the words found by [`OcrJob::run`] as one undoable step. Returns the number of words.
    pub fn apply_ocr(&mut self, id: DocId, found: &[OcrPage]) -> Result<usize, EditError> {
        let count = self.get(id).ok_or_else(|| EditError::Invalid("no such document".into()))?.info.pages.len();
        self.ocr_authority(id).map_err(EditError::Invalid)?;
        let mut pages = std::collections::BTreeSet::new();
        for page in found {
            if page.page >= count || !pages.insert(page.page) || (page.skipped.is_some() && !page.words.is_empty()) {
                return Err(EditError::Invalid("recognized pages must be unique, present in the document, and empty when skipped".into()));
            }
            pdfcraft_ocr::validate_words(&page.words).map_err(|e| EditError::Invalid(e.to_string()))?;
        }
        let edits: Vec<Edit> = found
            .iter()
            .filter_map(|p| {
                let words: Vec<_> = p.words.iter().filter(|w| !w.text.trim().is_empty()).cloned().collect();
                (!words.is_empty()).then_some(Edit::AddOcrText { page: p.page, words })
            })
            .collect();
        let words = found.iter().map(OcrPage::word_count).sum();
        if !edits.is_empty() {
            self.apply(id, Edit::Batch { label: "Recognize text".into(), edits })?;
        }
        Ok(words)
    }

    /// Recognise text on `pages` and add it, in one call (the CLI and agents use this).
    pub fn recognize_text(&mut self, id: DocId, pages: &[usize], settings: OcrSettings) -> Result<Vec<OcrPage>, String> {
        let job = self.ocr_job_checked(id, pages, settings)?;
        let ocr = engine()?;
        let review = job.run(ocr.as_ref(), |_, _| true)?;
        self.apply_ocr_review(&review)?;
        Ok(review.pages)
    }
}

/// What [`recognize_file`] did to one file.
#[derive(Clone, Debug, PartialEq)]
pub struct FileResult {
    /// The new file (incrementally saved), or the original when nothing was recognised.
    pub bytes: Arc<Vec<u8>>,
    pub pages: Vec<OcrPage>,
}

impl FileResult {
    pub fn words(&self) -> usize {
        self.pages.iter().map(OcrPage::word_count).sum()
    }
}

/// Recognize text in multiple files: one PDF's bytes in, the searchable PDF out. `progress`
/// works as for [`OcrJob::run`].
pub fn recognize_file(
    name: &str,
    bytes: Arc<Vec<u8>>,
    password: Option<&str>,
    settings: OcrSettings,
    ocr: &Ocr,
    progress: impl FnMut(usize, usize) -> bool,
) -> Result<FileResult, String> {
    let mut s = Session::new();
    let id = s.open(name, None, bytes.clone(), password).map_err(|e| e.to_string())?;
    if let Some(why) = s.get(id).and_then(|d| d.read_only_reason.clone()) {
        return Err(why);
    }
    let job = s.ocr_job_checked(id, &[], settings)?;
    let review = job.run(ocr, progress)?;
    let pages = review.pages.clone();
    if s.apply_ocr_review(&review)? == 0 {
        return Ok(FileResult { bytes, pages });
    }
    Ok(FileResult { bytes: s.save_bytes(id).map_err(|e| e.to_string())?, pages })
}
