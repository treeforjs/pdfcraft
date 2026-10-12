//! pdfcraft-ocr — Scan & OCR ▸ Recognize text (L4).
//!
//! Recognition runs the ocrs engine (MIT/Apache-2.0) with its pre-trained models (CC-BY-SA-4.0,
//! fetched by `cargo xtask models`; see ATTRIBUTION.toml). The caller renders a page to pixels;
//! [`Ocr::recognize`] finds the words in them, and [`text_layer`] turns words placed in user space
//! into page content: invisible text (rendering mode 3) over each word, so the page becomes a
//! searchable image (Acrobat's "Searchable Image (Exact)": the image is left untouched).
//!
//! The bundled recognition model has an ASCII alphabet. English is the validated language;
//! corrected searchable text supports WinAnsi accents, not arbitrary Unicode.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::{Path, PathBuf};

pub mod orient;

pub use pdfcraft_fonts::helvetica_width;

/// The model files, as named in ATTRIBUTION.toml.
pub const DETECTION_MODEL: &str = "text-detection.rten";
pub const RECOGNITION_MODEL: &str = "text-recognition.rten";

/// Languages validated against the bundled model (ISO 639-1).
pub const LANGUAGES: &[(&str, &str)] = &[("en", "English")];

/// Recognition input limits, checked before allocating channel-conversion buffers.
pub const MAX_IMAGE_PIXELS: u64 = 40_000_000;
pub const MAX_IMAGE_SIDE: u32 = 16_384;

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    /// Shown to end users (release packages ship the models; a source build fetches them with
    /// `cargo xtask models`). The UI catalogs key this exact text.
    #[error("the text recognition models are not installed (reinstall PdfCraft, or set PDFCRAFT_MODELS to the folder that holds them)")]
    NoModels,
    #[error("loading {0}: {1}")]
    Load(String, String),
    #[error("text recognition failed: {0}")]
    Recognize(String),
    #[error("the image is empty")]
    EmptyImage,
    #[error("invalid recognition image: {0}")]
    InvalidImage(String),
    #[error("invalid recognized text: {0}")]
    InvalidText(String),
}

/// Validate dimensions and packed channel layout without allocating from untrusted sizes.
pub fn validate_image(pixels: &[u8], width: u32, height: u32) -> Result<usize, OcrError> {
    if width == 0 || height == 0 || pixels.is_empty() {
        return Err(OcrError::EmptyImage);
    }
    let count = u64::from(width) * u64::from(height);
    if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE || count > MAX_IMAGE_PIXELS {
        return Err(OcrError::InvalidImage(format!("at most {MAX_IMAGE_PIXELS} pixels and {MAX_IMAGE_SIDE} pixels per side are supported")));
    }
    let count = usize::try_from(count).map_err(|_| OcrError::InvalidImage("image dimensions exceed this platform's limits".into()))?;
    [1, 3, 4]
        .into_iter()
        .find(|channels| count.checked_mul(*channels) == Some(pixels.len()))
        .ok_or_else(|| OcrError::InvalidImage("expected exactly one gray, three RGB, or four RGBA bytes per pixel".into()))
}

fn recognition_pixels(pixels: &[u8], width: u32, height: u32) -> Result<std::borrow::Cow<'_, [u8]>, OcrError> {
    if validate_image(pixels, width, height)? != 4 {
        return Ok(pixels.into());
    }
    // Transparent image colors are not visible text. Composite RGBA over the white page
    // used for recognition; this changes only the temporary OCR input, never the PDF image.
    Ok(pixels
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let alpha = u32::from(p[3]);
            [p[0], p[1], p[2]].map(|c| ((u32::from(c) * alpha + 255 * (255 - alpha) + 127) / 255) as u8)
        })
        .collect::<Vec<_>>()
        .into())
}

/// Where the two model files are.
#[derive(Clone, Debug, PartialEq)]
pub struct Models {
    pub detection: PathBuf,
    pub recognition: PathBuf,
}

impl Models {
    /// The models in `dir`, if both files are there.
    pub fn in_dir(dir: &Path) -> Option<Models> {
        let m = Models { detection: dir.join(DETECTION_MODEL), recognition: dir.join(RECOGNITION_MODEL) };
        (m.detection.is_file() && m.recognition.is_file()).then_some(m)
    }

    /// Look for the models: `$PDFCRAFT_MODELS`, then where the release packages install them
    /// beside the executable ([`Models::dirs_beside_exe`]), then the source tree's `assets/models/`.
    pub fn find() -> Option<Models> {
        Self::search_dirs().iter().find_map(|d| Self::in_dir(d))
    }

    /// For model-backed tests: whether to run them. `Ok(false)` means the models are missing
    /// and the test may skip; with `PDFCRAFT_MODELS_REQUIRED` set (CI sets it after
    /// `cargo xtask models`), missing models are an error instead, so a skipped test can never
    /// be mistaken for executed evidence.
    pub fn required_for_tests() -> Result<bool, String> {
        match (Self::find(), std::env::var_os("PDFCRAFT_MODELS_REQUIRED")) {
            (Some(_), _) => Ok(true),
            (None, None) => Ok(false),
            (None, Some(_)) => Err(format!(
                "PDFCRAFT_MODELS_REQUIRED is set but the OCR models are in none of {:?}; run `cargo xtask models` or set PDFCRAFT_MODELS",
                Self::search_dirs()
            )),
        }
    }

    /// The directories [`Models::find`] looks in, in order.
    pub fn search_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(d) = std::env::var_os("PDFCRAFT_MODELS") {
            dirs.push(PathBuf::from(d));
        }
        if let Some(exe) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
            dirs.extend(Self::dirs_beside_exe(&exe));
        }
        dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/models"));
        dirs
    }

    /// Where the release packages put the models, relative to the directory holding the
    /// executable (packaging/*):
    /// - `models/` beside it: the Windows MSI and portable zip;
    /// - `../Resources/models`: the macOS app bundle (`Contents/MacOS` → `Contents/Resources`);
    /// - `../share/pdfcraft/models`: the FHS-style trees (`bin/` → `share/`) of the Linux
    ///   deb/rpm/tar.gz/AppImage/Flatpak and the FreeBSD tarball, wherever they are installed.
    pub fn dirs_beside_exe(exe_dir: &Path) -> Vec<PathBuf> {
        vec![exe_dir.join("models"), exe_dir.join("../Resources/models"), exe_dir.join("../share/pdfcraft/models")]
    }
}

/// A recognised word: its text and its box in image pixels `[left, top, right, bottom]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub rect: [f32; 4],
}

/// A recognised line of words, in reading order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Line {
    pub words: Vec<Word>,
}

impl Line {
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

/// A loaded recogniser. Loading takes a moment; keep one and reuse it.
pub struct Ocr {
    engine: ocrs::OcrEngine,
}

impl Ocr {
    pub fn load(models: &Models) -> Result<Ocr, OcrError> {
        let load = |p: &Path| rten::Model::load_file(p).map_err(|e| OcrError::Load(p.display().to_string(), e.to_string()));
        let params = ocrs::OcrEngineParams {
            detection_model: Some(load(&models.detection)?),
            recognition_model: Some(load(&models.recognition)?),
            ..Default::default()
        };
        let engine = ocrs::OcrEngine::new(params).map_err(|e| OcrError::Load("ocr engine".into(), e.to_string()))?;
        Ok(Ocr { engine })
    }

    /// Load the models found by [`Models::find`].
    pub fn find() -> Result<Ocr, OcrError> {
        Self::load(&Models::find().ok_or(OcrError::NoModels)?)
    }

    /// Recognise the text in an RGBA (or RGB, or grey) image, `width` × `height` pixels.
    pub fn recognize(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<Line>, OcrError> {
        let rgb = recognition_pixels(pixels, width, height)?;
        let err = |e: &dyn std::fmt::Display| OcrError::Recognize(e.to_string());
        let source = ocrs::ImageSource::from_bytes(&rgb, (width, height)).map_err(|e| err(&e))?;
        let input = self.engine.prepare_input(source).map_err(|e| err(&e))?;
        let found = self.engine.detect_words(&input).map_err(|e| err(&e))?;
        let lines = self.engine.find_text_lines(&input, &found);
        let read = self.engine.recognize_text(&input, &lines).map_err(|e| err(&e))?;
        use ocrs::TextItem;
        Ok(read
            .into_iter()
            .flatten()
            .map(|line| Line {
                words: line
                    .words()
                    .map(|w| {
                        let r = w.bounding_rect();
                        Word { text: w.to_string(), rect: [r.left() as f32, r.top() as f32, r.right() as f32, r.bottom() as f32] }
                    })
                    .filter(|w| !w.text.trim().is_empty())
                    .collect(),
            })
            .filter(|l| !l.words.is_empty())
            .collect())
    }
}

/// A word placed on a page, in PDF user space: the bottom-left corner of its box, and the
/// vectors along its bottom edge (`across`, left → right as read) and its left edge (`up`).
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedWord {
    pub text: String,
    pub origin: [f64; 2],
    pub across: [f64; 2],
    pub up: [f64; 2],
}

/// Validate a correction before replacing the recognized word. Unsupported characters must
/// never turn silently into question marks in the current WinAnsi searchable-text writer.
pub fn validate_text(text: &str) -> Result<(), OcrError> {
    if text.len() > 16_384 {
        return Err(OcrError::InvalidText("a word or correction must be at most 16384 UTF-8 bytes".into()));
    }
    if let Some(c) = text.chars().find(|c| c.is_control() || !pdfcraft_fonts::win_ansi_encodable(*c)) {
        return Err(OcrError::InvalidText(format!("character U+{:04X} is not supported by the searchable-text font", u32::from(c))));
    }
    Ok(())
}

/// Validate public OCR word data before producing PDF syntax or making any document edit.
pub fn validate_words(words: &[PlacedWord]) -> Result<(), OcrError> {
    if words.len() > 200_000 {
        return Err(OcrError::InvalidText("at most 200000 words per page are supported".into()));
    }
    let mut bytes = 0usize;
    for word in words {
        validate_text(&word.text)?;
        bytes = bytes.saturating_add(word.text.len());
        if bytes > 16 * 1024 * 1024 {
            return Err(OcrError::InvalidText("a page's recognized text must be at most 16 MiB".into()));
        }
        if word.origin.iter().chain(&word.across).chain(&word.up).any(|v| !v.is_finite() || v.abs() > 10_000_000.0)
            || word.across[0].hypot(word.across[1]) <= 0.0
            || word.up[0].hypot(word.up[1]) <= 0.0
        {
            return Err(OcrError::InvalidText("word boxes must have finite, nonzero edges and coordinates within ten million points".into()));
        }
    }
    Ok(())
}

impl PlacedWord {
    /// Place a pixel-space word with `to_user`, which maps an image pixel (x right, y down) to
    /// user space.
    pub fn place(word: &Word, to_user: impl Fn(f32, f32) -> [f64; 2]) -> PlacedWord {
        let [l, t, r, b] = word.rect;
        let o = to_user(l, b);
        let br = to_user(r, b);
        let tl = to_user(l, t);
        PlacedWord { text: word.text.clone(), origin: o, across: [br[0] - o[0], br[1] - o[1]], up: [tl[0] - o[0], tl[1] - o[1]] }
    }
}

/// Helvetica's ascender and descender (em fractions): a word box spans about this much.
const BOX_EM: f64 = 0.93;
const DESCENT: f64 = 0.21;

fn num(v: f64) -> String {
    let s = format!("{:.4}", if v.abs() < 1e-9 { 0.0 } else { v });
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// Page content that writes `words` as invisible text in `/PCHelv` (standard Helvetica,
/// WinAnsiEncoding), each word stretched to its box so selection and search highlight the
/// right place. Marked content `/OCR` so it can be told apart from the page's own text.
pub fn text_layer(words: &[PlacedWord]) -> Result<Vec<u8>, OcrError> {
    validate_words(words)?;
    // Text rendering mode is graphics state: restore it so later added text stays visible.
    let mut out = b"q\n/OCR BMC\nBT\n3 Tr\n/PCHelv 1 Tf\n".to_vec();
    for w in words {
        let width = helvetica_width(&w.text, 1.0);
        let height = w.up[0].hypot(w.up[1]);
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        // The em square: its height spans the box; its width is stretched to fit the word.
        let em = [w.up[0] / BOX_EM, w.up[1] / BOX_EM];
        let ax = [w.across[0] / width, w.across[1] / width];
        let o = [w.origin[0] + em[0] * DESCENT, w.origin[1] + em[1] * DESCENT];
        let m = [ax[0], ax[1], em[0], em[1], o[0], o[1]];
        let nums: Vec<String> = m.iter().map(|v| num(*v)).collect();
        out.extend_from_slice(nums.join(" ").as_bytes());
        out.extend_from_slice(b" Tm ");
        out.extend(pdfcraft_fonts::literal(&pdfcraft_fonts::win_ansi(&w.text)));
        out.extend_from_slice(b" Tj\n");
    }
    out.extend_from_slice(b"ET\nEMC\nQ\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_image_dimensions_and_channel_lengths_are_rejected_before_allocation() {
        for (pixels, width, height) in [(&[0][..], u32::MAX, u32::MAX), (&[0][..], 10_000, 10_000), (&[0][..], 1, 20_000), (&[0, 1][..], 1, 1)] {
            assert!(matches!(validate_image(pixels, width, height), Err(OcrError::InvalidImage(_))));
        }
        assert!(matches!(validate_image(&[], 0, 1), Err(OcrError::EmptyImage)));
        for channels in [1, 3, 4] {
            assert_eq!(validate_image(&vec![0; 6 * channels], 2, 3).unwrap(), channels);
        }
    }

    #[test]
    fn transparent_pixels_are_composited_over_white_only_for_recognition() {
        let pixels = [0, 0, 0, 0, 10, 20, 30, 255, 0, 0, 0, 128];
        let original = pixels;
        assert_eq!(recognition_pixels(&pixels, 3, 1).unwrap().as_ref(), &[255, 255, 255, 10, 20, 30, 127, 127, 127]);
        assert_eq!(pixels, original);
        assert!(matches!(recognition_pixels(&[5, 6, 7], 1, 1).unwrap(), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn text_and_geometry_are_validated_before_writing_pdf_syntax() {
        let word = PlacedWord { text: "café (é) \\".into(), origin: [10.0, 20.0], across: [30.0, 0.0], up: [0.0, 9.3] };
        let stream = text_layer(std::slice::from_ref(&word)).unwrap();
        assert!(stream.contains(&0xe9), "WinAnsi accented corrections are encoded without replacement");
        for text in ["漢字".to_owned(), "bad\0text".into(), "x".repeat(16_385)] {
            assert!(text_layer(&[PlacedWord { text, ..word.clone() }]).is_err());
        }
        for origin in [[f64::NAN, 0.0], [f64::INFINITY, 0.0], [10_000_001.0, 0.0]] {
            assert!(text_layer(&[PlacedWord { origin, ..word.clone() }]).is_err());
        }
        assert!(text_layer(&[PlacedWord { across: [0.0; 2], ..word }]).is_err());
    }

    #[test]
    fn places_words_through_the_mapping() {
        // 2 pixels per point, page 612 × 792, y flipped.
        let to_user = |x: f32, y: f32| [x as f64 / 2.0, 792.0 - y as f64 / 2.0];
        let w = PlacedWord::place(&Word { text: "Hello".into(), rect: [100.0, 200.0, 300.0, 240.0] }, to_user);
        assert_eq!(w.origin, [50.0, 672.0]);
        assert_eq!(w.across, [100.0, 0.0]);
        assert_eq!(w.up, [0.0, 20.0]);
    }

    #[test]
    fn text_layer_is_invisible_text_fitted_to_each_box() {
        let w = PlacedWord { text: "Hi".into(), origin: [10.0, 20.0], across: [30.0, 0.0], up: [0.0, 9.3] };
        let s = String::from_utf8(text_layer(&[w]).unwrap()).unwrap();
        assert!(s.contains("3 Tr") && s.contains("/PCHelv 1 Tf"), "{s}");
        let width = helvetica_width("Hi", 1.0);
        assert!(s.contains(&format!("{} 0 0 10 10 22.1 Tm (Hi) Tj", num(30.0 / width))), "{s}");
        assert!(s.starts_with("q\n/OCR BMC") && s.ends_with("EMC\nQ\n"));
    }

    #[test]
    fn rotated_pages_keep_the_reading_direction() {
        // A page turned 90°: image x runs up the page.
        let to_user = |x: f32, y: f32| [y as f64, x as f64];
        let w = PlacedWord::place(&Word { text: "Up".into(), rect: [0.0, 0.0, 50.0, 10.0] }, to_user);
        assert_eq!(w.across, [0.0, 50.0]);
        assert_eq!(w.up, [-10.0, 0.0]);
    }

    /// #103: every release package's layout puts the models where the installed app looks.
    /// Mirrors packaging/{windows/package.ps1,macos/package.sh,linux/package.sh,freebsd/package.sh}.
    #[test]
    fn finds_the_models_in_every_release_package_layout() {
        let base = std::env::temp_dir().join(format!("pdfcraft-ocr-layouts-{}", std::process::id()));
        let layouts = [
            ("windows", "PdfCraft", "PdfCraft/models"),
            ("macos", "PdfCraft.app/Contents/MacOS", "PdfCraft.app/Contents/Resources/models"),
            ("linux-deb-rpm", "usr/bin", "usr/share/pdfcraft/models"),
            ("linux-tar", "pdfcraft-1.0.0-linux-x86_64/bin", "pdfcraft-1.0.0-linux-x86_64/share/pdfcraft/models"),
            ("appimage", "PdfCraft.AppDir/usr/bin", "PdfCraft.AppDir/usr/share/pdfcraft/models"),
            ("flatpak", "app/bin", "app/share/pdfcraft/models"),
            ("freebsd", "usr/local/bin", "usr/local/share/pdfcraft/models"),
        ];
        for (name, exe_dir, models_dir) in layouts {
            let root = base.join(name);
            let (exe_dir, models_dir) = (root.join(exe_dir), root.join(models_dir));
            std::fs::create_dir_all(&exe_dir).unwrap();
            std::fs::create_dir_all(&models_dir).unwrap();
            let found = || Models::dirs_beside_exe(&exe_dir).iter().find_map(|d| Models::in_dir(d));
            assert_eq!(found(), None, "{name}: no models yet");
            for file in [DETECTION_MODEL, RECOGNITION_MODEL] {
                std::fs::write(models_dir.join(file), b"model").unwrap();
            }
            let models = found().unwrap_or_else(|| panic!("{name}: models in {} not found", models_dir.display()));
            assert_eq!(models.detection.canonicalize().unwrap(), models_dir.join(DETECTION_MODEL).canonicalize().unwrap(), "{name}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The message tells end users what to do; release builds have no cargo.
    #[test]
    fn missing_models_message_is_for_end_users() {
        let message = OcrError::NoModels.to_string();
        assert!(!message.contains("cargo") && message.contains("PDFCRAFT_MODELS"), "{message}");
    }

    /// End to end on rendered text, when the models are installed (`cargo xtask models`).
    #[test]
    fn reads_rendered_text() {
        if !Models::required_for_tests().unwrap() {
            eprintln!("skipped: OCR models not installed");
            return;
        }
        let models = Models::find().unwrap();
        eprintln!("running actual OCR models from {}", models.detection.display());
        let ocr = Ocr::load(&models).unwrap();
        let (w, h, px) = test_image::hello();
        let lines = ocr.recognize(&px, w, h).unwrap();
        let text: Vec<String> = lines.iter().map(Line::text).collect();
        assert!(text.iter().any(|t| t.contains("HELL") && t.contains("WOR")), "{text:?}");
        let word = lines.iter().flat_map(|l| &l.words).find(|w| w.text.contains("HELL")).unwrap();
        assert!(word.rect[0] >= 10.0 && word.rect[0] < 60.0, "{word:?}");
    }

    mod test_image {
        /// "HELLO WORLD" drawn with blocky 5×7 letters, 4 pixels per dot, black on white.
        pub fn hello() -> (u32, u32, Vec<u8>) {
            const GLYPHS: &[(char, [&str; 7])] = &[
                ('H', ["X...X", "X...X", "X...X", "XXXXX", "X...X", "X...X", "X...X"]),
                ('E', ["XXXXX", "X....", "X....", "XXXX.", "X....", "X....", "XXXXX"]),
                ('L', ["X....", "X....", "X....", "X....", "X....", "X....", "XXXXX"]),
                ('O', [".XXX.", "X...X", "X...X", "X...X", "X...X", "X...X", ".XXX."]),
                ('W', ["X...X", "X...X", "X...X", "X.X.X", "X.X.X", "XX.XX", "X...X"]),
                ('R', ["XXXX.", "X...X", "X...X", "XXXX.", "X.X..", "X..X.", "X...X"]),
                ('D', ["XXXX.", "X...X", "X...X", "X...X", "X...X", "X...X", "XXXX."]),
            ];
            let text = "HELLO WORLD";
            let (dot, x0, y0) = (4u32, 40u32, 40u32);
            let (w, h) = (x0 * 2 + text.len() as u32 * 6 * dot, y0 * 2 + 7 * dot);
            let mut px = vec![255u8; (w * h * 4) as usize];
            for (i, c) in text.chars().enumerate() {
                let Some((_, rows)) = GLYPHS.iter().find(|g| g.0 == c) else { continue };
                for (ry, row) in rows.iter().enumerate() {
                    for (rx, b) in row.bytes().enumerate() {
                        if b != b'X' {
                            continue;
                        }
                        for dy in 0..dot {
                            for dx in 0..dot {
                                let x = x0 + (i as u32 * 6 + rx as u32) * dot + dx;
                                let y = y0 + ry as u32 * dot + dy;
                                let o = ((y * w + x) * 4) as usize;
                                px[o..o + 3].copy_from_slice(&[0, 0, 0]);
                            }
                        }
                    }
                }
            }
            (w, h, px)
        }
    }
}
