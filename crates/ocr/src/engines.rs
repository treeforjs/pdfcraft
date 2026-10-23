//! Pluggable recognition engines.
//!
//! Every engine reads the same bounded image (gray, RGB or RGBA, checked by
//! [`crate::validate_image`]) and returns the same [`Line`]s of [`crate::Word`]s, so the rest of
//! the OCR workflow (orientation, review, searchable and editable output) does not depend on
//! which engine read the page. The built-in [`OcrsEngine`] (pure Rust) is the default;
//! [`crate::tesseract::Tesseract`] runs an installed `tesseract` executable as an external
//! process, never linked or shipped. A [`Registry`] holds the engines a session can choose
//! from; callers and tests may register their own.

use std::sync::{Arc, Mutex};

use crate::{LANGUAGES, Line, Models, Ocr, OcrError};

/// The engine used when none is chosen.
pub const DEFAULT_ENGINE: &str = "ocrs";

/// The ocrs crate version the built-in engine runs (kept in step with Cargo.toml by a test).
pub const OCRS_VERSION: &str = "0.13.1";

/// Languages an engine may offer (ISO 639-1 code, Tesseract's trained-data name, English
/// name). Limited to Latin-script languages whose letters the searchable-text font
/// (WinAnsi) can encode; which of them an engine actually reads is in [`EngineInfo::languages`].
pub const KNOWN_LANGUAGES: &[(&str, &str, &str)] = &[
    ("en", "eng", "English"),
    ("de", "deu", "German"),
    ("fr", "fra", "French"),
    ("es", "spa", "Spanish"),
    ("it", "ita", "Italian"),
    ("pt", "por", "Portuguese"),
    ("nl", "nld", "Dutch"),
    ("da", "dan", "Danish"),
    ("sv", "swe", "Swedish"),
    ("no", "nor", "Norwegian"),
    ("fi", "fin", "Finnish"),
    ("is", "isl", "Icelandic"),
    ("ca", "cat", "Catalan"),
];

/// The English name of a [`KNOWN_LANGUAGES`] code.
pub fn language_name(code: &str) -> Option<&'static str> {
    KNOWN_LANGUAGES.iter().find(|l| l.0 == code).map(|l| l.2)
}

/// What an engine is and whether it can run now.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineInfo {
    pub id: String,
    pub name: String,
    pub available: bool,
    /// The engine's own version, when known.
    pub version: Option<String>,
    /// ISO 639-1 codes (from [`KNOWN_LANGUAGES`]) it can read.
    pub languages: Vec<String>,
    /// Runs as a separate program on this machine rather than inside PdfCraft.
    pub external: bool,
    /// Where it was found, or why it is unavailable.
    pub detail: String,
}

/// A text recognition engine. Implementations must be safe to share between threads; a
/// recognition job may run on a worker thread.
pub trait OcrEngine: Send + Sync {
    /// Stable identifier: 1–32 characters of `a-z`, `0-9`, `-` and `_`.
    fn id(&self) -> &str;
    /// A name for people.
    fn name(&self) -> &str;
    /// Whether it can run, its version and languages. May briefly run the engine to ask.
    fn info(&self) -> EngineInfo;
    /// Ready it to read `language` (an ISO 639-1 code): load models, find the program and its
    /// language data. Called once before a job's pages are read, so a missing engine fails the
    /// job instead of every page.
    fn prepare(&self, language: &str) -> Result<(), OcrError>;
    /// Recognise the text in a gray, RGB or RGBA image of `width` × `height` pixels.
    fn recognize(&self, pixels: &[u8], width: u32, height: u32, language: &str) -> Result<Vec<Line>, OcrError>;
    /// [`Self::recognize`] in steps, so a single-threaded host (the web app) can let the page
    /// respond between them. Engines that can't split their work read in one step.
    fn begin<'a>(&'a self, pixels: &'a [u8], width: u32, height: u32, language: &'a str) -> Result<Box<dyn Steps + 'a>, OcrError> {
        Ok(Box::new(OneStep(Some(move || self.recognize(pixels, width, height, language)))))
    }
}

/// A recognition in progress ([`OcrEngine::begin`]).
pub trait Steps {
    /// Do the next step: `Some(lines)` once the image has been read.
    fn step(&mut self) -> Result<Option<Vec<Line>>, OcrError>;
}

/// A recognition done in one step.
pub struct OneStep<F>(pub Option<F>);

impl<F: FnOnce() -> Result<Vec<Line>, OcrError>> Steps for OneStep<F> {
    fn step(&mut self) -> Result<Option<Vec<Line>>, OcrError> {
        let read = self.0.take().ok_or_else(|| OcrError::Recognize("the reading has already finished".into()))?;
        read().map(Some)
    }
}

impl Steps for crate::Reading<'_> {
    fn step(&mut self) -> Result<Option<Vec<Line>>, OcrError> {
        crate::Reading::step(self)
    }
}

/// Whether `id` is a valid engine identifier.
pub fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The most engines a registry holds.
pub const MAX_ENGINES: usize = 64;

/// The engines a session can choose from, in registration order.
#[derive(Clone)]
pub struct Registry {
    engines: Vec<Arc<dyn OcrEngine>>,
}

impl Default for Registry {
    /// The built-in engine, then Tesseract (found on `PATH` or through `PDFCRAFT_TESSERACT`).
    fn default() -> Self {
        Registry { engines: vec![Arc::new(OcrsEngine), Arc::new(crate::tesseract::Tesseract::from_env())] }
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.engines.iter().map(|e| e.id().to_owned())).finish()
    }
}

impl Registry {
    /// A registry with no engines.
    pub fn empty() -> Registry {
        Registry { engines: Vec::new() }
    }

    /// Add `engine`, replacing one with the same id. Returns whether one was replaced.
    pub fn register(&mut self, engine: Arc<dyn OcrEngine>) -> Result<bool, OcrError> {
        let id = engine.id().to_owned();
        if !valid_id(&id) {
            return Err(OcrError::Unavailable(format!("invalid OCR engine id {id:?}: use 1-32 characters a-z, 0-9, - or _")));
        }
        if let Some(slot) = self.engines.iter_mut().find(|e| e.id() == id) {
            *slot = engine;
            return Ok(true);
        }
        if self.engines.len() >= MAX_ENGINES {
            return Err(OcrError::Unavailable(format!("at most {MAX_ENGINES} OCR engines can be registered")));
        }
        self.engines.push(engine);
        Ok(false)
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn OcrEngine>> {
        self.engines.iter().find(|e| e.id() == id).cloned()
    }

    pub fn engines(&self) -> &[Arc<dyn OcrEngine>] {
        &self.engines
    }

    pub fn ids(&self) -> Vec<String> {
        self.engines.iter().map(|e| e.id().to_owned()).collect()
    }
}

/// The built-in engine: ocrs with the pinned models ([`Models::find`]), loaded once per
/// process and shared.
#[derive(Clone, Copy, Debug, Default)]
pub struct OcrsEngine;

static SHARED: Mutex<Option<Arc<Ocr>>> = Mutex::new(None);

impl OcrsEngine {
    /// The loaded recogniser, shared by every session in the process.
    pub fn shared() -> Result<Arc<Ocr>, OcrError> {
        let mut slot = SHARED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(o) = slot.as_ref() {
            return Ok(o.clone());
        }
        let o = Arc::new(Ocr::find()?);
        *slot = Some(o.clone());
        Ok(o)
    }

    /// Use `ocr` as the shared recogniser from now on: the web app installs the models it
    /// downloaded ([`Ocr::from_bytes`]), since it has no model files to find.
    pub fn install(ocr: Ocr) -> Arc<Ocr> {
        let ocr = Arc::new(ocr);
        *SHARED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ocr.clone());
        ocr
    }

    /// Whether the recogniser is loaded (or installed) in this process.
    pub fn loaded() -> bool {
        SHARED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some()
    }
}

fn ocrs_language(language: &str) -> Result<(), OcrError> {
    if LANGUAGES.iter().any(|(code, _)| *code == language) {
        return Ok(());
    }
    Err(OcrError::Language(format!("unsupported recognition language {language:?}; the built-in recognizer currently supports English")))
}

impl OcrEngine for OcrsEngine {
    fn id(&self) -> &str {
        DEFAULT_ENGINE
    }

    fn name(&self) -> &str {
        "ocrs (built-in)"
    }

    fn info(&self) -> EngineInfo {
        let (available, detail) = if cfg!(target_arch = "wasm32") {
            if Self::loaded() { (true, "models downloaded into this browser".into()) } else { (false, OcrError::NotDownloaded.to_string()) }
        } else {
            match Models::find() {
                Some(m) => (true, format!("models in {}", m.recognition.parent().map_or_else(String::new, |p| p.display().to_string()))),
                None => (false, OcrError::NoModels.to_string()),
            }
        };
        EngineInfo {
            id: DEFAULT_ENGINE.into(),
            name: self.name().into(),
            available,
            version: Some(OCRS_VERSION.into()),
            languages: LANGUAGES.iter().map(|(c, _)| (*c).to_owned()).collect(),
            external: false,
            detail,
        }
    }

    fn prepare(&self, language: &str) -> Result<(), OcrError> {
        ocrs_language(language)?;
        Self::shared().map(|_| ())
    }

    fn recognize(&self, pixels: &[u8], width: u32, height: u32, language: &str) -> Result<Vec<Line>, OcrError> {
        ocrs_language(language)?;
        Self::shared()?.recognize(pixels, width, height)
    }

    fn begin<'a>(&'a self, pixels: &'a [u8], width: u32, height: u32, language: &'a str) -> Result<Box<dyn Steps + 'a>, OcrError> {
        ocrs_language(language)?;
        Ok(Box::new(Self::shared()?.begin(pixels, width, height)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Named(&'static str);

    impl OcrEngine for Named {
        fn id(&self) -> &str {
            self.0
        }
        fn name(&self) -> &str {
            "test"
        }
        fn info(&self) -> EngineInfo {
            EngineInfo {
                id: self.0.into(),
                name: "test".into(),
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
            Ok(Vec::new())
        }
    }

    #[test]
    fn the_default_registry_has_the_built_in_engine_first_and_tesseract() {
        let r = Registry::default();
        assert_eq!(r.ids(), ["ocrs", "tesseract"]);
        let ocrs = r.get(DEFAULT_ENGINE).unwrap().info();
        assert_eq!((ocrs.languages, ocrs.external), (vec!["en".to_owned()], false));
        assert!(r.get(DEFAULT_ENGINE).unwrap().prepare("de").is_err(), "the built-in models read English only");
    }

    #[test]
    fn engines_register_replace_and_reject_bad_ids() {
        let mut r = Registry::empty();
        assert!(!r.register(Arc::new(Named("fake"))).unwrap());
        assert!(r.register(Arc::new(Named("fake"))).unwrap(), "same id replaces");
        for bad in ["", "Fake", "has space", "../x", "x".repeat(33).leak()] {
            assert!(r.register(Arc::new(Named(bad))).is_err(), "{bad:?}");
        }
        assert_eq!(r.ids(), ["fake"]);
        for i in 1..MAX_ENGINES {
            r.register(Arc::new(Named(format!("e{i}").leak()))).unwrap();
        }
        assert!(r.register(Arc::new(Named("one-too-many"))).is_err());
    }

    #[test]
    fn engines_that_cannot_split_their_work_read_in_one_step() {
        let named = Named("one");
        let mut steps = named.begin(&[0], 1, 1, "en").unwrap();
        assert_eq!(steps.step().unwrap(), Some(Vec::new()));
        assert!(steps.step().is_err(), "a finished reading reports an error, not a crash");
    }

    #[test]
    fn the_ocrs_version_matches_the_dependency() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains(&format!("ocrs = {{ version = \"{OCRS_VERSION}\"")), "update OCRS_VERSION");
    }

    #[test]
    fn known_languages_are_unique_and_encodable() {
        let mut codes = std::collections::BTreeSet::new();
        for (iso, tess, name) in KNOWN_LANGUAGES {
            assert!(codes.insert(*iso) && iso.len() == 2 && tess.len() == 3 && !name.is_empty());
        }
        assert!(LANGUAGES.iter().all(|(c, _)| language_name(c).is_some()));
    }
}
