//! The PDF Optimizer (execution plan M11.1; Acrobat: Reduce File Size and Optimize PDF ▸
//! Advanced optimization).
//!
//! - **Images:** each image's effective resolution is measured where pages draw it (the
//!   smallest over its uses, through form XObjects); colour and grayscale images above a
//!   threshold are resampled (bicubic) to a target resolution and recompressed as JPEG or Flate.
//!   A new image replaces the old one only if it is smaller. Images PdfCraft can't decode
//!   faithfully (CMYK and other colour spaces, masks, decode arrays, JPEG 2000, JBIG2, CCITT,
//!   more than 8 bits) are left alone.
//! - **Monochrome images** (one-bit samples or image masks) are recompressed losslessly as
//!   CCITT Group 4 (or Flate) when that is smaller, and optionally downsampled first (area
//!   average, majority with ties to ink). Only samples that decode completely through general
//!   filters are used; images already in CCITT or JBIG2 are left alone.
//! - **Discard objects:** page thumbnails, alternate images, document tags (structure tree),
//!   print settings.
//! - **Fonts:** unembed chosen embedded fonts where that is safe ([`fonts`]).
//! - **Clean up:** Flate-compress streams that have no filter; re-encode LZW streams as Flate
//!   (lossless, with the same complete-decoding rules as preflight's fix: see
//!   [`pdfcraft_cos::Stream::decoded_complete`]). Whether the copy packs objects into object
//!   streams is [`Settings::compress_structure`] (the engine's writer applies it).
//!
//! Metadata, attachments, comments, scripts, private data, hidden layers, bookmarks and form
//! fields are discarded by `pdfcraft-redact`'s Remove Hidden Information, which the engine
//! runs alongside.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod audit;
pub mod background;
pub mod compat;
pub mod despeckle;
pub mod fonts;
mod images;
mod links;

pub use audit::{SpaceAudit, SpaceCategory, SpaceUse, audit_space, audit_space_detailed};
pub use compat::{NewerFeature, VERSIONS, newer_features, parse_version, version_name};
pub use fonts::{EmbeddedFont, UnembedBlock, UnembedError, embedded_fonts};

use pdfcraft_cos::{Document, ObjRef, Object, Stream};

pub use images::{BackgroundReport, DespeckleReport, effective_resolutions};

#[derive(Debug, thiserror::Error)]
pub enum OptimizeError {
    #[error("the document has no page tree")]
    NoPages,
    #[error(transparent)]
    Cos(#[from] pdfcraft_cos::CosError),
    #[error("the optimization was cancelled")]
    Cancelled,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Fonts(#[from] fonts::UnembedError),
}

/// Where an optimization is, reported to [`optimize_with_progress`]'s callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// About to process image `done` (0-based) of `total`; `done == total` once all are done.
    Images { done: usize, total: usize },
    /// Discarding objects, cleaning up links and compressing unencoded streams.
    CleanUp,
}

/// How resampled (or recompressed) images are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// JPEG with a quality from 1 to 100.
    Jpeg(u8),
    /// Lossless (zlib).
    Flate,
    /// Keep the image's own compression (resampled images become Flate if they weren't JPEG).
    Retain,
}

/// Acrobat's JPEG quality steps.
pub const QUALITIES: [(&str, u8); 5] = [("Minimum", 25), ("Low", 45), ("Medium", 60), ("High", 80), ("Maximum", 95)];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageSettings {
    /// Resample images whose effective resolution is above `above_ppi` down to `target_ppi`.
    pub downsample: bool,
    pub target_ppi: f64,
    pub above_ppi: f64,
    pub compression: Compression,
}

impl ImageSettings {
    /// Reduce File Size: bicubic to 150 ppi above 225 ppi, JPEG medium quality.
    pub const REDUCE: ImageSettings = ImageSettings { downsample: true, target_ppi: 150.0, above_ppi: 225.0, compression: Compression::Jpeg(60) };
}

/// How monochrome (one-bit) images are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonoCompression {
    /// CCITT Group 4 (lossless).
    CcittG4,
    /// Flate (lossless).
    Flate,
    /// Keep the image's own compression (downsampled images become Flate).
    Retain,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonoSettings {
    /// Downsample images whose effective resolution is above `above_ppi` to `target_ppi`.
    pub downsample: bool,
    pub target_ppi: f64,
    pub above_ppi: f64,
    pub compression: MonoCompression,
}

impl MonoSettings {
    /// Reduce File Size: 300 ppi above 450 ppi, CCITT Group 4.
    pub const REDUCE: MonoSettings = MonoSettings { downsample: true, target_ppi: 300.0, above_ppi: 450.0, compression: MonoCompression::CcittG4 };
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub color: ImageSettings,
    pub gray: ImageSettings,
    /// One-bit images and image masks.
    pub mono: MonoSettings,
    pub discard_thumbnails: bool,
    pub discard_alternate_images: bool,
    pub discard_tags: bool,
    pub discard_print_settings: bool,
    /// Flate-compress streams stored without a filter.
    pub flate_unencoded: bool,
    /// Clean Up: re-encode every LZW stream as Flate ("In streams that use LZW encoding, use
    /// Flate instead"). Streams that don't decode completely are left as they are.
    pub lzw_to_flate: bool,
    /// Clean Up: pack the copy's objects into compressed object streams when it is written.
    pub compress_structure: bool,
    /// Clean Up: remove links (and bookmark destinations) that point nowhere.
    pub remove_invalid_links: bool,
    /// Clean Up: remove named destinations nothing refers to.
    pub remove_unreferenced_dests: bool,
    /// Fonts: unembed the embedded fonts with these `/BaseFont` names. Every name must match an
    /// embedded font that can safely be unembedded ([`fonts::unembed`]), or nothing is done.
    pub unembed_fonts: Vec<String>,
    /// Make compatible with this PDF version (one of [`VERSIONS`]); `None` retains the
    /// document's own.
    pub compatibility: Option<(u8, u8)>,
}

impl Settings {
    /// Whether the copy is written with object streams: asked for, and defined by the version
    /// it is made compatible with (PDF 1.5 and later).
    pub fn object_streams(&self) -> bool {
        self.compress_structure && self.compatibility.is_none_or(|v| v >= (1, 5))
    }
}

impl Default for Settings {
    /// Acrobat's Reduce File Size choices.
    fn default() -> Self {
        Self {
            color: ImageSettings::REDUCE,
            gray: ImageSettings::REDUCE,
            mono: MonoSettings::REDUCE,
            discard_thumbnails: true,
            discard_alternate_images: true,
            discard_tags: false,
            discard_print_settings: false,
            flate_unencoded: true,
            lzw_to_flate: true,
            compress_structure: true,
            remove_invalid_links: true,
            remove_unreferenced_dests: true,
            unembed_fonts: Vec::new(),
            compatibility: None,
        }
    }
}

/// What an optimization did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub images: usize,
    pub images_resampled: usize,
    pub images_recompressed: usize,
    /// Encoded image bytes before and after (images that changed).
    pub image_bytes_before: usize,
    pub image_bytes_after: usize,
    /// Monochrome images now stored as CCITT Group 4 (also counted above).
    pub images_ccitt: usize,
    pub thumbnails: usize,
    pub alternate_images: usize,
    pub tags_removed: bool,
    pub print_settings: usize,
    pub streams_compressed: usize,
    /// LZW streams re-encoded as Flate, and LZW streams left alone (incomplete or corrupt data,
    /// an indirect filter description, an image codec in the chain, or over the size limits).
    pub lzw_reencoded: usize,
    pub lzw_skipped: usize,
    /// Links removed and bookmarks whose destination was cleared (Clean Up).
    pub invalid_links: usize,
    pub invalid_bookmarks: usize,
    pub unreferenced_dests: usize,
    /// Font dictionaries whose embedded program was removed.
    pub fonts_unembedded: usize,
    /// Features the copy keeps that the version it was made compatible with doesn't define.
    pub newer_features: Vec<NewerFeature>,
}

/// Encoded size limit for one LZW stream, and decoded total for one run (the same limits as
/// preflight's LZW fix).
const LZW_STREAM_LIMIT: usize = 32 << 20;
const LZW_TOTAL_LIMIT: usize = 128 << 20;

/// Whether a stream's `/Filter` names LZW (resolving an indirect filter only to report it).
fn uses_lzw(doc: &Document, d: &pdfcraft_cos::Dict) -> bool {
    let is_lzw = |o: &Object| matches!(o.as_name(), Some(b"LZWDecode" | b"LZW"));
    match d.get(b"Filter").map(|f| doc.resolve(f)) {
        Some(f) => match &*f {
            Object::Array(a) => a.iter().take(64).any(|o| is_lzw(&doc.resolve(o))),
            o => is_lzw(o),
        },
        None => false,
    }
}

/// Clean Up: every LZW stream that decodes completely ([`Stream::decoded_complete`]: strict,
/// end-of-data code present, direct filter description, no external data) is replaced by a
/// Flate stream of the same decoded bytes; any other filters of the chain and their
/// parameters go with it. Acrobat documents this option as converting LZW streams regardless
/// of size. A stream that can't be decoded that way, or a chain with an image codec or
/// `/Crypt`, is left exactly as it was and counted as skipped.
fn lzw_to_flate(doc: &mut Document, report: &mut Report) {
    let mut total = 0usize;
    for num in doc.object_numbers() {
        let r = ObjRef { num, generation: doc.generation(num) };
        let obj = doc.get(r);
        let Object::Stream(s) = &*obj else { continue };
        if !uses_lzw(doc, &s.dict) {
            continue;
        }
        let crypt = matches!(s.dict.get(b"Filter"), Some(Object::Array(a)) if a.iter().any(|o| o.as_name() == Some(b"Crypt")));
        let room = LZW_TOTAL_LIMIT.saturating_sub(total).min(LZW_STREAM_LIMIT);
        let data = if crypt || s.raw.len() > LZW_STREAM_LIMIT || room == 0 { None } else { s.decoded_complete(room).ok() };
        let Some(data) = data else {
            report.lzw_skipped += 1;
            continue;
        };
        total = total.saturating_add(data.len());
        let mut d = s.dict.clone();
        d.remove(b"Length");
        let packed = Stream::flate(d, &data);
        doc.set(r, Object::Stream(packed));
        report.lzw_reencoded += 1;
    }
}

/// Optimize `doc` in place.
pub fn optimize(doc: &mut Document, settings: &Settings) -> Result<Report, OptimizeError> {
    optimize_with_progress(doc, settings, &mut |_| true)
}

/// [`optimize`], calling `progress` before each image and before the clean-up. Returning `false`
/// stops with [`OptimizeError::Cancelled`]; `doc` is then partly optimized and should be dropped.
/// Searchable Image: resample the images pages `pages` (0-based) draw above `ppi` (with 10 %
/// slack) down to `ppi`, keeping each image's own compression. Only the images change: an image
/// those pages share with others is smaller there too. Returns what was done.
pub fn downsample_page_images(doc: &mut Document, pages: &[usize], ppi: f64) -> Result<Report, OptimizeError> {
    let all = pdfcraft_annot::page_refs(doc).map_err(|_| OptimizeError::NoPages)?;
    let chosen: Vec<ObjRef> = pages.iter().filter_map(|p| all.get(*p).copied()).collect();
    if chosen.len() != pages.len() {
        return Err(OptimizeError::Invalid("downsampling pages must be in the document".into()));
    }
    if !(ppi.is_finite() && (36.0..=2400.0).contains(&ppi)) {
        return Err(OptimizeError::Invalid("the downsampling resolution must be between 36 and 2400 ppi".into()));
    }
    let image = ImageSettings { downsample: true, target_ppi: ppi, above_ppi: ppi * 1.1, compression: Compression::Retain };
    let mono = MonoSettings { downsample: true, target_ppi: ppi, above_ppi: ppi * 1.1, compression: MonoCompression::Retain };
    let settings = Settings { color: image, gray: image, mono, ..Settings::default() };
    let mut report = Report::default();
    images::run(doc, &chosen, &settings, &mut report, &mut |_| true)?;
    Ok(report)
}

/// Scan & OCR ▸ Enhance ▸ Despeckle: remove isolated specks from the images `pages` draw
/// ([`despeckle`]), each sized by its effective resolution where it is drawn. Images shared
/// with other pages change there too. One-bit images (including CCITT fax) keep one bit per
/// pixel (CCITT is re-encoded as Group 4); 8-bit gray and RGB images are rewritten with Flate,
/// or JPEG at quality 90 when they were JPEG. Images that cannot be decoded faithfully are
/// left alone and counted in [`DespeckleReport::images_skipped`].
pub fn despeckle_page_images(doc: &mut Document, pages: &[usize], options: despeckle::Options) -> Result<DespeckleReport, OptimizeError> {
    let all = pdfcraft_annot::page_refs(doc).map_err(|_| OptimizeError::NoPages)?;
    let chosen: Vec<ObjRef> = pages.iter().filter_map(|p| all.get(*p).copied()).collect();
    if chosen.len() != pages.len() {
        return Err(OptimizeError::Invalid("despeckle pages must be in the document".into()));
    }
    if !options.any() {
        return Ok(DespeckleReport::default());
    }
    Ok(images::despeckle(doc, &chosen, options))
}

/// Scan & OCR ▸ Enhance ▸ Remove background: whiten the paper background of the 8-bit gray
/// and RGB images `pages` draw ([`background`]). Images shared with other pages change there
/// too. Images are rewritten with Flate, or JPEG at quality 90 when they were JPEG; one-bit
/// images already have a white background and are left as they are. Images that cannot be
/// decoded faithfully are left alone and counted in [`BackgroundReport::images_skipped`].
pub fn remove_page_image_backgrounds(doc: &mut Document, pages: &[usize], options: background::Options) -> Result<BackgroundReport, OptimizeError> {
    let all = pdfcraft_annot::page_refs(doc).map_err(|_| OptimizeError::NoPages)?;
    let chosen: Vec<ObjRef> = pages.iter().filter_map(|p| all.get(*p).copied()).collect();
    if chosen.len() != pages.len() {
        return Err(OptimizeError::Invalid("background removal pages must be in the document".into()));
    }
    if let Some(w) = options.white_point
        && w < background::MIN_WHITE_POINT
    {
        return Err(OptimizeError::Invalid(format!("the white point must be between {} and 255", background::MIN_WHITE_POINT)));
    }
    if !options.any() {
        return Ok(BackgroundReport::default());
    }
    Ok(images::remove_backgrounds(doc, &chosen, options))
}

pub fn optimize_with_progress(doc: &mut Document, settings: &Settings, progress: &mut dyn FnMut(Stage) -> bool) -> Result<Report, OptimizeError> {
    let mut report = Report::default();
    let pages = pdfcraft_annot::page_refs(doc).map_err(|_| OptimizeError::NoPages)?;
    if settings.compatibility.is_some_and(|v| !VERSIONS.contains(&v)) {
        return Err(OptimizeError::Invalid(format!("PDF versions to make compatible with are {}", VERSIONS.map(version_name).join(", "))));
    }
    // First, so a font that can't be unembedded stops the run before anything changed.
    report.fonts_unembedded = fonts::unembed(doc, &settings.unembed_fonts)?;
    images::run(doc, &pages, settings, &mut report, progress)?;
    if !progress(Stage::CleanUp) {
        return Err(OptimizeError::Cancelled);
    }
    if settings.discard_thumbnails {
        for p in &pages {
            if doc.get(*p).as_dict().is_some_and(|d| d.contains(b"Thumb")) {
                doc.update_dict(*p, |d| {
                    d.remove(b"Thumb");
                })?;
                report.thumbnails += 1;
            }
        }
    }
    if settings.discard_alternate_images {
        for num in doc.object_numbers() {
            let r = ObjRef { num, generation: doc.generation(num) };
            let alt = matches!(&*doc.get(r), Object::Stream(s) if s.dict.name(b"Subtype") == Some(b"Image") && s.dict.contains(b"Alternates"));
            if alt {
                if let Object::Stream(s) = &*doc.get(r) {
                    let mut s = s.clone();
                    s.dict.remove(b"Alternates");
                    doc.set(r, Object::Stream(s));
                }
                report.alternate_images += 1;
            }
        }
    }
    let root = doc.root();
    if settings.discard_tags
        && let Some(root) = root
        && doc.get(root).as_dict().is_some_and(|c| c.contains(b"StructTreeRoot"))
    {
        doc.update_dict(root, |c| {
            c.remove(b"StructTreeRoot");
            c.remove(b"MarkInfo");
        })?;
        for p in &pages {
            if doc.get(*p).as_dict().is_some_and(|d| d.contains(b"StructParents")) {
                doc.update_dict(*p, |d| {
                    d.remove(b"StructParents");
                })?;
            }
        }
        report.tags_removed = true;
    }
    if settings.discard_print_settings
        && let Some(root) = root
    {
        const PRINT: [&[u8]; 7] = [b"PrintScaling", b"Duplex", b"PickTrayByPDFSize", b"PrintPageRange", b"NumCopies", b"PrintArea", b"PrintClip"];
        let vp = doc.get(root).as_dict().and_then(|c| c.get(b"ViewerPreferences").cloned());
        let edit = |d: &mut pdfcraft_cos::Dict| PRINT.iter().filter(|k| d.remove(k).is_some()).count();
        match vp {
            Some(Object::Ref(r)) => {
                let mut n = 0;
                doc.update_dict(r, |d| n = edit(d))?;
                report.print_settings += n;
            }
            Some(Object::Dict(_)) => {
                let mut n = 0;
                doc.update_dict(root, |c| {
                    if let Some(Object::Dict(d)) = c.get_mut(b"ViewerPreferences") {
                        n = edit(d);
                    }
                })?;
                report.print_settings += n;
            }
            _ => {}
        }
    }
    if settings.remove_invalid_links {
        let (l, b) = links::remove_invalid(doc, &pages)?;
        report.invalid_links = l;
        report.invalid_bookmarks = b;
    }
    if settings.remove_unreferenced_dests {
        report.unreferenced_dests = links::remove_unreferenced_dests(doc)?;
    }
    if settings.lzw_to_flate {
        lzw_to_flate(doc, &mut report);
    }
    if settings.flate_unencoded {
        for num in doc.object_numbers() {
            let r = ObjRef { num, generation: doc.generation(num) };
            let obj = doc.get(r);
            let Object::Stream(s) = &*obj else { continue };
            // XMP stays readable as plain text (PDF/A requires it); tiny streams don't gain.
            if s.dict.contains(b"Filter") || s.dict.name(b"Type") == Some(b"Metadata") || s.dict.name(b"Type") == Some(b"XRef") || s.raw.len() < 64 {
                continue;
            }
            let mut d = s.dict.clone();
            d.remove(b"Length");
            let packed = Stream::flate(d, &s.raw);
            if packed.raw.len() < s.raw.len() {
                doc.set(r, Object::Stream(packed));
                report.streams_compressed += 1;
            }
        }
    }
    if let Some(v) = settings.compatibility {
        compat::apply(doc, v)?;
        report.newer_features = newer_features(doc, v);
    }
    Ok(report)
}
