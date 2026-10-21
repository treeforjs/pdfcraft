//! Scan & OCR ▸ Recognize text: render pages, read the words in them and add an invisible text
//! layer ([`Edit::AddOcrText`]), making scanned pages searchable and selectable.
//!
//! Recognition is slow (about a second a page), so it is split from the edit: [`Session::ocr_job`]
//! captures what it needs, [`OcrJob::run`] works anywhere (the UI runs it on a worker thread) and
//! [`Session::apply_ocr_review`] applies the reviewed result as one undoable step, provided the
//! original document revision is still current.
//!
//! Recognition engines are pluggable ([`OcrEngine`]): the session holds a [`Registry`] (the
//! built-in ocrs engine, the default, and an installed Tesseract program when there is one) and
//! the selected engine ([`Session::set_ocr_engine`]); [`OcrSettings::engine`] overrides it for one
//! job. Every recognition entry point resolves its engine through [`Session::ocr_engine_for`].

use std::sync::Arc;

use pdfcraft_render::{PageInfo, PageRenderer, RenderConfig, RenderRequest, RequestKind};

pub use pdfcraft_ocr::editable::{Family as FontFamily, Font as StandardFont, Suspect, SuspectReason, horizontal_scale};
pub use pdfcraft_ocr::engines::{DEFAULT_ENGINE, EngineInfo, KNOWN_LANGUAGES, OcrEngine, OcrsEngine, Registry};
pub use pdfcraft_ocr::orient::Orientation;
pub use pdfcraft_ocr::tesseract;
pub use pdfcraft_ocr::{LANGUAGES, Line, Models, Ocr, OcrError, PlacedWord, Word};
pub use pdfcraft_optimize::background::{Options as BackgroundOptions, Stats as BackgroundStats, Strength as BackgroundStrength};
pub use pdfcraft_optimize::despeckle::{Level as DespeckleLevel, Options as DespeckleOptions, Stats as DespeckleStats};
pub use pdfcraft_optimize::{BackgroundReport, DespeckleReport};

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

/// A registered engine reading one language.
pub(crate) struct EngineRecognizer<'a> {
    pub(crate) engine: &'a dyn OcrEngine,
    pub(crate) language: &'a str,
}

impl Recognizer for EngineRecognizer<'_> {
    fn recognize(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<pdfcraft_ocr::Line>, OcrError> {
        self.engine.recognize(pixels, width, height, self.language)
    }
}

/// The engines a session can use and the one chosen.
#[derive(Default)]
pub(crate) struct Engines {
    registry: Registry,
    selected: Option<String>,
}

/// Recognize Text settings.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrSettings {
    /// Resolution pages are rendered at for recognition (the viewer's "Downsample to" choices).
    pub dpi: f32,
    /// Language code from [`LANGUAGES`]; unsupported alphabets are rejected.
    pub language: String,
    /// Leave pages that already have text alone (PDF reports "page contains renderable
    /// text" and skips them).
    pub skip_text_pages: bool,
    /// Detect pages turned by a quarter or half turn and read them upright
    /// ([`pdfcraft_ocr::orient`]). Off by default: it costs up to four extra recognitions of a
    /// reduced page.
    pub auto_rotate: bool,
    /// Detect skew of up to ±15° and read the page straightened. Off by default.
    pub deskew: bool,
    /// Output Searchable Image instead of Searchable Image (Exact): the rotation and skew found
    /// by `auto_rotate`/`deskew` are also corrected on the page itself, which is turned (in its
    /// content and `/Rotate`, not resampled) so its text reads straight, and the recognized
    /// pages' images are downsampled to `dpi` when finer. Off by default.
    pub straighten: bool,
    /// Remove isolated specks before recognition ([`pdfcraft_optimize::despeckle`]), on the
    /// temporary recognition copy. With `straighten` (Searchable Image) the recognized pages'
    /// images are despeckled too, in the same undoable step. Off by default.
    pub despeckle: DespeckleOptions,
    /// Whiten the paper background before recognition ([`pdfcraft_optimize::background`]), on
    /// the temporary recognition copy (before despeckling). With `straighten` (Searchable Image)
    /// the recognized pages' images get a white background too, in the same undoable step.
    /// Off by default.
    pub background: BackgroundOptions,
    /// Output Editable Text and Images instead of a searchable image: recognized lines become
    /// visible text in a matching standard font, size and colour, and the text areas are
    /// cleaned out of the page image, which stays as the background ([`EditableOptions`]).
    /// Not with `straighten`. With `background`/`despeckle` the page images are cleaned too.
    /// Off by default.
    pub editable: Option<EditableOptions>,
    /// The engine to read with, by id ([`Session::ocr_engines`]); `None` uses the session's
    /// selected engine ([`Session::set_ocr_engine`], ocrs unless changed).
    pub engine: Option<String>,
}

/// Editable Text and Images options.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditableOptions {
    /// Words the backend is less sure of than this (0–1) stay as image with invisible text
    /// over them. When the backend reports no confidence (the bundled engine), words whose
    /// text doesn't look like a word are kept instead, unless this is 0.
    pub min_confidence: f32,
}

impl Default for EditableOptions {
    fn default() -> Self {
        EditableOptions { min_confidence: 0.5 }
    }
}

/// One word of an editable line: its index in [`OcrPage::words`], where it starts along the
/// line and how long it is (user units), and the area of the page image it was read from.
#[derive(Clone, Debug, PartialEq)]
pub struct EditableWord {
    pub index: usize,
    /// Kept as image (a suspect): written invisibly in its place in the line.
    pub image: bool,
    pub offset: f64,
    pub width: f64,
    pub region: [[f64; 2]; 4],
}

/// A recognized line to be written as visible text (user space): the start of its baseline,
/// unit vectors along and across it, and the font, size and colours measured for it.
#[derive(Clone, Debug, PartialEq)]
pub struct EditableLine {
    pub font: StandardFont,
    pub size: f64,
    pub origin: [f64; 2],
    pub direction: [f64; 2],
    pub up: [f64; 2],
    pub color: [u8; 3],
    pub paper: [u8; 3],
    pub words: Vec<EditableWord>,
}

/// Editable Text and Images for one page: the lines that become text, and the words kept as
/// image (with invisible text) and why.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditableLayout {
    pub lines: Vec<EditableLine>,
    pub suspects: Vec<Suspect>,
}

impl EditableLayout {
    /// How many lines use each standard font.
    pub fn fonts(&self) -> std::collections::BTreeMap<&'static str, usize> {
        let mut out = std::collections::BTreeMap::new();
        for l in &self.lines {
            *out.entry(l.font.base_font()).or_insert(0) += 1;
        }
        out
    }

    /// The visible lines as they will be written, with the words' current (corrected) text;
    /// words left empty by a correction stay as image. Also returns the image regions to clean
    /// and which words became visible.
    fn resolve(&self, words: &[PlacedWord]) -> (Vec<pdfcraft_ocr::editable::TextLine>, Vec<pdfcraft_ocr::editable::Region>, Vec<bool>) {
        let mut used = vec![false; words.len()];
        let (mut lines, mut regions) = (Vec::new(), Vec::new());
        for l in &self.lines {
            let kept: Vec<(&EditableWord, &str)> = l
                .words
                .iter()
                .filter_map(|w| Some((w, words.get(w.index)?.text.trim())))
                .filter(|(_, t)| !t.is_empty() && l.font.em_width(t).is_some())
                .collect();
            if kept.iter().all(|(w, _)| w.image) {
                continue;
            }
            let Some((first, _)) = kept.first() else { continue };
            let base = first.offset;
            let origin = [l.origin[0] + l.direction[0] * base, l.origin[1] + l.direction[1] * base];
            let mut line_words = Vec::new();
            for (w, text) in &kept {
                line_words.push(pdfcraft_ocr::editable::LineWord {
                    text: (*text).to_owned(),
                    offset: w.offset - base,
                    width: w.width,
                    hidden: w.image,
                });
                if !w.image {
                    regions.push(pdfcraft_ocr::editable::Region { quad: w.region, paper: l.paper });
                }
                if let Some(u) = used.get_mut(w.index) {
                    *u = true;
                }
            }
            lines.push(pdfcraft_ocr::editable::TextLine {
                font: l.font,
                size: l.size,
                origin,
                direction: l.direction,
                up: l.up,
                color: l.color,
                words: line_words,
            });
        }
        (lines, regions, used)
    }
}

/// Map a page's measured lines from recognition pixels to user space.
fn editable_layout(
    analysis: pdfcraft_ocr::editable::Analysis,
    words: &[pdfcraft_ocr::Word],
    to_user: &dyn Fn(f32, f32) -> [f64; 2],
) -> EditableLayout {
    let mut out = EditableLayout { lines: Vec::new(), suspects: analysis.suspects };
    let map = |x: f64, y: f64| to_user(x as f32, y as f32);
    for s in analysis.lines {
        let o = map(s.left, s.baseline);
        let e = map(s.right, s.baseline);
        let u = map(s.left, s.baseline - s.size);
        let along = [e[0] - o[0], e[1] - o[1]];
        let upv = [u[0] - o[0], u[1] - o[1]];
        let (length, size) = (along[0].hypot(along[1]), upv[0].hypot(upv[1]));
        let span = s.right - s.left;
        let usable = length.is_finite()
            && length > 0.0
            && span > 0.0
            && size.is_finite()
            && (pdfcraft_ocr::editable::MIN_SIZE..=pdfcraft_ocr::editable::MAX_SIZE).contains(&size)
            && o.iter().all(|v| v.is_finite());
        if !usable {
            out.suspects.extend(s.words.iter().map(|i| Suspect {
                word: *i,
                confidence: words.get(*i).and_then(|w| w.confidence),
                reason: SuspectReason::NoInk,
            }));
            continue;
        }
        let direction = [along[0] / length, along[1] / length];
        let up = [upv[0] / size, upv[1] / size];
        // Anti-aliased edges reach a little beyond the box the backend reports.
        let pad = (0.06 * s.size).max(1.0);
        let mut line_words = Vec::new();
        let mut members: Vec<(usize, bool)> = s.words.iter().map(|i| (*i, false)).chain(s.kept.iter().map(|i| (*i, true))).collect();
        members.sort_by(|a, b| {
            let x = |i: usize| words.get(i).map_or(0.0, |w| w.rect[0]);
            x(a.0).total_cmp(&x(b.0))
        });
        for (i, image) in &members {
            let Some(w) = words.get(*i) else { continue };
            let [l, t, r, b] = w.rect.map(f64::from);
            let (t, b) = (t.min(s.top) - pad, b.max(s.bottom) + pad);
            let (l2, r2) = (l - pad, r + pad);
            line_words.push(EditableWord {
                index: *i,
                image: *image,
                offset: (l - s.left) / span * length,
                width: (r - l).max(0.0) / span * length,
                region: [map(l2, t), map(r2, t), map(r2, b), map(l2, b)],
            });
        }
        out.lines.push(EditableLine { font: s.font, size, origin: o, direction, up, color: s.color, paper: s.paper, words: line_words });
    }
    out.suspects.sort_by_key(|s| s.word);
    out
}

/// The colour of the rendered page at a pixel (RGBA composited over white).
fn colour_at(rgba: &[u8], width: u32, height: u32, x: f64, y: f64) -> Option<[u8; 3]> {
    if !(x.is_finite() && y.is_finite() && x >= 0.0 && y >= 0.0 && x < f64::from(width) && y < f64::from(height)) {
        return None;
    }
    let i = (y as usize).checked_mul(width as usize)?.checked_add(x as usize)?.checked_mul(4)?;
    let p = rgba.get(i..i + 4)?;
    let alpha = u32::from(p[3]);
    let over = |c: u8| ((u32::from(c) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
    Some([over(p[0]), over(p[1]), over(p[2])])
}

impl Default for OcrSettings {
    fn default() -> Self {
        OcrSettings {
            dpi: 300.0,
            language: "en".into(),
            skip_text_pages: true,
            auto_rotate: false,
            deskew: false,
            straighten: false,
            despeckle: DespeckleOptions::default(),
            background: BackgroundOptions::default(),
            editable: None,
            engine: None,
        }
    }
}

impl OcrSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !self.dpi.is_finite() || !(72.0..=600.0).contains(&self.dpi) {
            return Err("recognition resolution must be finite and between 72 and 600 dpi".into());
        }
        if self.background.white_point.is_some_and(|w| w < pdfcraft_optimize::background::MIN_WHITE_POINT) {
            return Err(format!("the background white point must be between {} and 255", pdfcraft_optimize::background::MIN_WHITE_POINT));
        }
        if let Some(e) = self.editable {
            if self.straighten {
                return Err("choose either the Searchable Image (straighten) or the Editable Text and Images output, not both".into());
            }
            if !(e.min_confidence.is_finite() && (0.0..=1.0).contains(&e.min_confidence)) {
                return Err("the minimum confidence must be between 0 and 1".into());
            }
        }
        // Which of these the chosen engine reads is checked when it is resolved
        // ([`Session::ocr_engine_for`]).
        if pdfcraft_ocr::engines::language_name(&self.language).is_none() {
            return Err(format!("unsupported recognition language {:?}; the installed recognizer currently supports English", self.language));
        }
        if let Some(id) = &self.engine
            && !pdfcraft_ocr::engines::valid_id(id)
        {
            return Err(format!("invalid OCR engine id {id:?}"));
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
    /// Searchable Image: downsample the recognized pages' images to this resolution.
    downsample: Option<f32>,
    /// Searchable Image: despeckle the recognized pages' images first.
    despeckle: Option<DespeckleOptions>,
    /// Searchable Image: whiten the recognized pages' image backgrounds first of all.
    background: Option<BackgroundOptions>,
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
    /// was on. The page image itself is changed only with `straighten`.
    pub orientation: Option<Orientation>,
    /// Turn the page so that `orientation` reads straight when the text is applied
    /// ([`OcrSettings::straighten`]).
    pub straighten: bool,
    /// What despeckling the recognition copy removed, when despeckle was on.
    pub despeckle: Option<DespeckleStats>,
    /// What background removal found and changed on the recognition copy, when it was on.
    pub background: Option<BackgroundStats>,
    /// Editable Text and Images: the lines that become visible text and the words kept as
    /// image ([`OcrSettings::editable`]).
    pub editable: Option<EditableLayout>,
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

/// The edit that turns page `page` so text found running at `o` reads straight: the skew is
/// undone about the visible page's centre in its content, the quarter turns through `/Rotate`.
fn straighten(info: &PageInfo, page: usize, o: Orientation) -> Result<Edit, EditError> {
    if !o.skew.is_finite() || o.skew.abs() > pdfcraft_ocr::orient::MAX_SKEW as f32 || !o.rotation.is_multiple_of(90) {
        return Err(EditError::Invalid("the page orientation to correct is out of range".into()));
    }
    // In view space (y down), turn clockwise as seen by the skew, about the page centre.
    let (sin, cos) = f64::from(o.skew).to_radians().sin_cos();
    let (cx, cy) = (f64::from(info.width) / 2.0, f64::from(info.height) / 2.0);
    let map = |x: f64, y: f64| {
        let v = info.user_to_view(x as f32, y as f32);
        let (dx, dy) = (f64::from(v[0]) - cx, f64::from(v[1]) - cy);
        let u = info.view_to_user((cx + cos * dx - sin * dy) as f32, (cy + sin * dx + cos * dy) as f32);
        [f64::from(u[0]), f64::from(u[1])]
    };
    // The map is affine: read its matrix from three points, around the crop box for precision.
    let [x0, y0, x1, y1] = info.crop.map(f64::from);
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    let (p, px, py) = (map(x0, y0), map(x0 + w, y0), map(x0, y0 + h));
    let (a, b, c, d) = ((px[0] - p[0]) / w, (px[1] - p[1]) / w, (py[0] - p[0]) / h, (py[1] - p[1]) / h);
    let matrix = [a, b, c, d, p[0] - a * x0 - c * y0, p[1] - b * x0 - d * y0];
    Ok(Edit::StraightenPage { page, matrix, quarter_turns: i64::from(o.rotation / 90) })
}

/// The built-in recogniser, loaded once (it takes a moment) and shared. Recognition entry
/// points use the session's selected engine instead ([`Session::ocr_engine_for`]).
pub fn engine() -> Result<Arc<Ocr>, String> {
    OcrsEngine::shared().map_err(|e| e.to_string())
}

/// Whether the built-in engine's recognition models are installed.
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
    /// The engine resolved for the job when it was created.
    engine: Arc<dyn OcrEngine>,
}

impl OcrJob {
    /// The engine the job reads with ([`Self::run_selected`]).
    pub fn ocr_engine(&self) -> &Arc<dyn OcrEngine> {
        &self.engine
    }

    /// [`Self::run`] with the job's own engine: it is readied first (models loaded, program and
    /// language data found), so a missing engine fails the job before any page is rendered.
    pub fn run_selected(self, progress: impl FnMut(usize, usize) -> bool) -> Result<OcrReview, String> {
        let engine = self.engine.clone();
        let language = self.settings.language.clone();
        let ready = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine.prepare(&language)))
            .unwrap_or_else(|_| Err(OcrError::Recognize("the recognition engine failed unexpectedly".into())));
        ready.map_err(|e| e.to_string())?;
        self.run(&EngineRecognizer { engine: engine.as_ref(), language: &language }, progress)
    }

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
            let skip = |why: &str| OcrPage {
                page,
                words: Vec::new(),
                skipped: Some(why.into()),
                orientation: None,
                straighten: false,
                despeckle: None,
                background: None,
                editable: None,
            };
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
            // The background is whitened and specks are removed on a gray copy for recognition
            // (the page itself changes only with Searchable Image, through
            // `Edit::RemovePageImageBackgrounds` and `Edit::DespecklePageImages` when applied).
            let mut cleaned = None;
            let (background, despeckle) = (self.settings.background, self.settings.despeckle);
            if background.any() || despeckle.any() {
                let gray = pdfcraft_ocr::orient::Gray::from_pixels(&shot.rgba, shot.width, shot.height).map_err(|e| e.to_string());
                let done = gray.and_then(|mut g| {
                    let mut bg = None;
                    if background.any() {
                        let stats = pdfcraft_optimize::background::remove_background_pixels(&mut g.pixels, g.width, g.height, 1, background)
                            .map_err(|e| format!("background removal failed: {e}; recognize this page with background off"))?;
                        bg = Some(stats);
                    }
                    let mut specks = None;
                    if despeckle.any() {
                        let dpi = f64::from(shot.width) / f64::from(info.width.max(1e-3)) * 72.0;
                        let stats = pdfcraft_optimize::despeckle::despeckle_pixels(&mut g.pixels, g.width, g.height, 1, dpi, despeckle)
                            .map_err(|e| format!("despeckling failed: {e}; recognize this page with despeckle off"))?;
                        specks = Some(stats);
                    }
                    Ok((g, specks, bg))
                });
                match done {
                    Ok(d) => cleaned = Some(d),
                    Err(e) => {
                        out.push(skip(&e));
                        continue;
                    }
                }
            }
            let (input, input_w, input_h) = match &cleaned {
                Some((g, ..)) => (&g.pixels[..], g.width, g.height),
                None => (&shot.rgba[..], shot.width, shot.height),
            };
            // Orientation and skew are corrected on a gray copy for recognition only; words are
            // mapped back through `to_source`, so the text layer lies over the unchanged image.
            let options = pdfcraft_ocr::orient::Options { auto_rotate: self.settings.auto_rotate, deskew: self.settings.deskew };
            let corrected = match pdfcraft_ocr::orient::correct(input, input_w, input_h, options, |p, w, h| ocr.recognize(p, w, h)) {
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
                None => ocr.recognize(input, input_w, input_h),
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
            let editable = self.settings.editable.and_then(|options| {
                // Measured in the image that was read (the corrected copy, if any); colours come
                // from the rendered page through the same back-mapping as the words.
                let flat: Vec<pdfcraft_ocr::Word> = lines.iter().flat_map(|l| l.words.iter().cloned()).collect();
                let owned;
                let gray = match &corrected.image {
                    Some(image) => image,
                    None => {
                        owned = pdfcraft_ocr::orient::Gray::from_pixels(input, input_w, input_h).ok()?;
                        &owned
                    }
                };
                let rgb = |x: f64, y: f64| {
                    let [sx, sy] = corrected.to_source.apply(x, y);
                    colour_at(&shot.rgba, shot.width, shot.height, sx, sy)
                };
                let analysis = pdfcraft_ocr::editable::analyze(gray, &rgb, &flat, options.min_confidence);
                Some(editable_layout(analysis, &flat, &to_user))
            });
            let orientation = options.any().then_some(corrected.orientation);
            let straighten = self.settings.straighten && orientation.is_some_and(|o| o.rotation != 0 || o.skew != 0.0);
            let despeckle = cleaned.as_ref().and_then(|(_, stats, _)| *stats);
            let background = cleaned.as_ref().and_then(|(_, _, stats)| *stats);
            if self.settings.editable.is_some() && editable.is_none() {
                out.push(skip("the page could not be measured for editable text"));
                continue;
            }
            let found = OcrPage { page, words, skipped: None, orientation, straighten, despeckle, background, editable };
            if let Err(error) = pdfcraft_ocr::validate_words(&found.words) {
                out.push(skip(&error.to_string()));
            } else {
                out.push(found);
            }
        }
        if !progress(total, total) {
            return Err("text recognition canceled; no text was applied".into());
        }
        let downsample = self.settings.straighten.then_some(self.settings.dpi);
        let rewrite = self.settings.straighten || self.settings.editable.is_some();
        let despeckle = (rewrite && self.settings.despeckle.any()).then_some(self.settings.despeckle);
        let background = (rewrite && self.settings.background.any()).then_some(self.settings.background);
        Ok(OcrReview { source: self.source, pages: out, downsample, despeckle, background })
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
        let engine = self.ocr_engine_for(&settings)?;
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
        Ok(OcrJob {
            source,
            bytes: doc.bytes.clone(),
            password: doc.password.clone(),
            infos: doc.info.pages.clone(),
            native,
            pages,
            settings,
            engine,
        })
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
        let clean = Cleanup { background: review.background, despeckle: review.despeckle };
        self.apply_ocr_with(review.source.doc, &review.pages, review.downsample, clean).map_err(|e| e.to_string())
    }

    /// Add the words found by [`OcrJob::run`] as one undoable step. Returns the number of words.
    pub fn apply_ocr(&mut self, id: DocId, found: &[OcrPage]) -> Result<usize, EditError> {
        self.apply_ocr_with(id, found, None, Cleanup::default())
    }

    /// [`Self::apply_ocr`], also whitening the background of and despeckling the recognized
    /// pages' images and downsampling them to `downsample` dpi (Searchable Image).
    fn apply_ocr_with(&mut self, id: DocId, found: &[OcrPage], downsample: Option<f32>, clean: Cleanup) -> Result<usize, EditError> {
        let count = self.get(id).ok_or_else(|| EditError::Invalid("no such document".into()))?.info.pages.len();
        self.ocr_authority(id).map_err(EditError::Invalid)?;
        let mut pages = std::collections::BTreeSet::new();
        for page in found {
            if page.page >= count || !pages.insert(page.page) || (page.skipped.is_some() && (!page.words.is_empty() || page.editable.is_some())) {
                return Err(EditError::Invalid("recognized pages must be unique, present in the document, and empty when skipped".into()));
            }
            pdfcraft_ocr::validate_words(&page.words).map_err(|e| EditError::Invalid(e.to_string()))?;
            if let Some(layout) = &page.editable {
                if layout.lines.iter().flat_map(|l| &l.words).any(|w| w.index >= page.words.len())
                    || layout.suspects.iter().any(|s| s.word >= page.words.len())
                {
                    return Err(EditError::Invalid("editable lines must refer to the page's recognized words".into()));
                }
                let (lines, regions, _) = layout.resolve(&page.words);
                pdfcraft_ocr::editable::validate(&lines, &regions).map_err(|e| EditError::Invalid(e.to_string()))?;
            }
        }
        let infos = self.get(id).map(|d| d.info.pages.clone()).unwrap_or_default();
        let mut edits = Vec::new();
        for p in found {
            if let Some(layout) = &p.editable {
                // Visible lines; everything else recognized stays as image with invisible text.
                let (lines, regions, used) = layout.resolve(&p.words);
                let hidden: Vec<_> = p
                    .words
                    .iter()
                    .zip(used.iter().chain(std::iter::repeat(&false)))
                    .filter(|(w, u)| !**u && !w.text.trim().is_empty())
                    .map(|(w, _)| w.clone())
                    .collect();
                if !lines.is_empty() || !hidden.is_empty() {
                    edits.push(Edit::AddEditableOcrText { page: p.page, lines, regions, hidden });
                }
                continue;
            }
            let words: Vec<_> = p.words.iter().filter(|w| !w.text.trim().is_empty()).cloned().collect();
            if !words.is_empty() {
                edits.push(Edit::AddOcrText { page: p.page, words });
            }
            // After the text layer, so the words turn with the image they lie over.
            if let (true, Some(o), Some(info)) = (p.straighten, p.orientation, infos.get(p.page)) {
                edits.push(straighten(info, p.page, o)?);
            }
        }
        let read: Vec<usize> = found.iter().filter(|p| p.skipped.is_none()).map(|p| p.page).collect();
        // At the images' own resolution, before any downsampling; the background first, so
        // despeckling sees clean paper.
        if let Some(options) = clean.background.filter(|o| o.any() && !read.is_empty()) {
            edits.push(Edit::RemovePageImageBackgrounds { pages: read.clone(), options });
        }
        if let Some(options) = clean.despeckle.filter(|o| o.any() && !read.is_empty()) {
            edits.push(Edit::DespecklePageImages { pages: read.clone(), options });
        }
        if let Some(ppi) = downsample.filter(|_| !read.is_empty()) {
            edits.push(Edit::DownsamplePageImages { pages: read, ppi: f64::from(ppi) });
        }
        let words = found.iter().map(OcrPage::word_count).sum();
        if !edits.is_empty() {
            self.apply(id, Edit::Batch { label: "Recognize text".into(), edits })?;
        }
        Ok(words)
    }

    /// Recognise text on `pages` and add it, in one call (the CLI and agents use this).
    pub fn recognize_text(&mut self, id: DocId, pages: &[usize], settings: OcrSettings) -> Result<Vec<OcrPage>, String> {
        let job = self.ocr_job_checked(id, pages, settings)?;
        let review = job.run_selected(|_, _| true)?;
        self.apply_ocr_review(&review)?;
        Ok(review.pages)
    }

    /// The registered engines.
    pub fn ocr_engine_registry(&self) -> &Registry {
        &self.ocr_engines.registry
    }

    /// Every registered engine: whether it can run, its version and languages. Asks external
    /// engines afresh (it may run them briefly).
    pub fn ocr_engines(&self) -> Vec<EngineInfo> {
        self.ocr_engines.registry.engines().iter().map(|e| e.info()).collect()
    }

    /// The selected engine's id ([`DEFAULT_ENGINE`] unless changed).
    pub fn ocr_engine_id(&self) -> &str {
        self.ocr_engines.selected.as_deref().unwrap_or(DEFAULT_ENGINE)
    }

    /// Add an engine (or replace the one with its id). If the replaced engine was selected,
    /// the new one is.
    pub fn register_ocr_engine(&mut self, engine: Arc<dyn OcrEngine>) -> Result<(), String> {
        self.ocr_engines.registry.register(engine).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Run Tesseract from `path` (`None`: find `tesseract` on `PATH`) instead of where it was
    /// looked for so far.
    pub fn set_tesseract_path(&mut self, path: Option<std::path::PathBuf>) -> Result<(), String> {
        let config = tesseract::Config { path, ..tesseract::Config::default() };
        self.register_ocr_engine(Arc::new(tesseract::Tesseract::new(config)))
    }

    /// Choose the engine every recognition uses unless its settings name another. The engine
    /// must be registered and able to run now.
    pub fn set_ocr_engine(&mut self, id: &str) -> Result<EngineInfo, String> {
        let engine = self.ocr_engines.registry.get(id).ok_or_else(|| self.unknown_engine(id))?;
        let info = engine.info();
        if !info.available {
            return Err(format!("the {id} OCR engine can't run: {}", info.detail));
        }
        self.ocr_engines.selected = (id != DEFAULT_ENGINE).then(|| id.to_owned());
        Ok(info)
    }

    fn unknown_engine(&self, id: &str) -> String {
        format!("unknown OCR engine {id:?}; the registered engines are {}", self.ocr_engines.registry.ids().join(", "))
    }

    /// The engine `settings` read with ([`OcrSettings::engine`], else the selected one),
    /// checked to be registered and to read `settings.language`. Whether it can run now is
    /// checked when a job starts ([`OcrJob::run_selected`]).
    pub fn ocr_engine_for(&self, settings: &OcrSettings) -> Result<Arc<dyn OcrEngine>, String> {
        let id = settings.engine.as_deref().unwrap_or(self.ocr_engine_id());
        let engine = self.ocr_engines.registry.get(id).ok_or_else(|| self.unknown_engine(id))?;
        let info = engine.info();
        if info.languages.contains(&settings.language) {
            return Ok(engine);
        }
        if !info.available && info.languages.is_empty() {
            return Err(format!("the {id} OCR engine can't run: {}", info.detail));
        }
        let names: Vec<&str> = info.languages.iter().filter_map(|l| pdfcraft_ocr::engines::language_name(l)).collect();
        Err(format!(
            "unsupported recognition language {:?}; the {id} engine reads {}",
            settings.language,
            if names.is_empty() { "no language".to_owned() } else { names.join(", ") }
        ))
    }
}

/// Searchable Image clean-up applied with the recognized text.
#[derive(Clone, Copy, Default)]
struct Cleanup {
    background: Option<BackgroundOptions>,
    despeckle: Option<DespeckleOptions>,
}

impl Session {
    /// Scan & OCR ▸ Enhance scan ▸ Remove background: whiten the paper background of the
    /// images `pages` (0-based; empty = all) of document `id` draw, as one undoable step
    /// ([`Edit::RemovePageImageBackgrounds`]). Returns what changed; when nothing does, no step
    /// is added. The work is done on a copy first (to report it), then applied.
    pub fn remove_scan_backgrounds(&mut self, id: DocId, pages: &[usize], options: BackgroundOptions) -> Result<BackgroundReport, EditError> {
        let doc = self.get(id).ok_or_else(|| EditError::Invalid("no such document".into()))?;
        let all = doc.info.pages.len();
        let mut unique = std::collections::BTreeSet::new();
        if pages.iter().any(|p| *p >= all || !unique.insert(*p)) {
            return Err(EditError::Invalid(format!("background removal pages must be unique page indexes below {all}")));
        }
        if let Some(w) = options.white_point
            && w < pdfcraft_optimize::background::MIN_WHITE_POINT
        {
            return Err(EditError::Invalid(format!("the white point must be between {} and 255", pdfcraft_optimize::background::MIN_WHITE_POINT)));
        }
        if let Some(why) = &doc.read_only_reason {
            return Err(EditError::ReadOnly(why.clone()));
        }
        if !doc.allows_modification() {
            return Err(EditError::NotPermitted("changes to the document"));
        }
        let pages: Vec<usize> = if pages.is_empty() { (0..all).collect() } else { pages.to_vec() };
        if !options.any() {
            return Ok(BackgroundReport::default());
        }
        let mut copy = doc.editor.as_ref().ok_or_else(|| EditError::ReadOnly(String::new()))?.cos.clone();
        let report = crate::guard(|| pdfcraft_optimize::remove_page_image_backgrounds(&mut copy, &pages, options))
            .map_err(|m| EditError::Invalid(format!("background removal failed unexpectedly ({m}); the document was not changed")))?
            .map_err(|e| EditError::Optimize(e.to_string()))?;
        if report.images_changed > 0 {
            self.apply(id, Edit::RemovePageImageBackgrounds { pages, options })?;
        }
        Ok(report)
    }

    /// Scan & OCR ▸ Enhance scan ▸ Despeckle: remove isolated specks from the images `pages`
    /// (0-based; empty = all) of document `id` draw, as one undoable step
    /// ([`Edit::DespecklePageImages`]). Returns what changed; when nothing does, no step is
    /// added. The work is done on a copy first (to report it), then applied.
    pub fn despeckle_scans(&mut self, id: DocId, pages: &[usize], options: DespeckleOptions) -> Result<DespeckleReport, EditError> {
        let doc = self.get(id).ok_or_else(|| EditError::Invalid("no such document".into()))?;
        let all = doc.info.pages.len();
        let mut unique = std::collections::BTreeSet::new();
        if pages.iter().any(|p| *p >= all || !unique.insert(*p)) {
            return Err(EditError::Invalid(format!("despeckle pages must be unique page indexes below {all}")));
        }
        if let Some(why) = &doc.read_only_reason {
            return Err(EditError::ReadOnly(why.clone()));
        }
        if !doc.allows_modification() {
            return Err(EditError::NotPermitted("changes to the document"));
        }
        let pages: Vec<usize> = if pages.is_empty() { (0..all).collect() } else { pages.to_vec() };
        if !options.any() {
            return Ok(DespeckleReport::default());
        }
        let mut copy = doc.editor.as_ref().ok_or_else(|| EditError::ReadOnly(String::new()))?.cos.clone();
        let report = crate::guard(|| pdfcraft_optimize::despeckle_page_images(&mut copy, &pages, options))
            .map_err(|m| EditError::Invalid(format!("despeckling failed unexpectedly ({m}); the document was not changed")))?
            .map_err(|e| EditError::Optimize(e.to_string()))?;
        if report.images_changed > 0 {
            self.apply(id, Edit::DespecklePageImages { pages, options })?;
        }
        Ok(report)
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

/// Recognize text in multiple files: one PDF's bytes in, the searchable PDF out, read with
/// `engine` (resolve it with [`Session::ocr_engine_for`]). `progress` works as for
/// [`OcrJob::run`].
pub fn recognize_file(
    name: &str,
    bytes: Arc<Vec<u8>>,
    password: Option<&str>,
    mut settings: OcrSettings,
    engine: &Arc<dyn OcrEngine>,
    progress: impl FnMut(usize, usize) -> bool,
) -> Result<FileResult, String> {
    let mut s = Session::new();
    s.register_ocr_engine(engine.clone())?;
    settings.engine = Some(engine.id().to_owned());
    let id = s.open(name, None, bytes.clone(), password).map_err(|e| e.to_string())?;
    if let Some(why) = s.get(id).and_then(|d| d.read_only_reason.clone()) {
        return Err(why);
    }
    let job = s.ocr_job_checked(id, &[], settings)?;
    let review = job.run_selected(progress)?;
    let pages = review.pages.clone();
    if s.apply_ocr_review(&review)? == 0 {
        return Ok(FileResult { bytes, pages });
    }
    Ok(FileResult { bytes: s.save_bytes(id).map_err(|e| e.to_string())?, pages })
}
