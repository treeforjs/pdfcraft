//! Print colour warnings: overprint use, white objects set to overprint, rich black, small
//! rich-black text and the registration colour outside printer marks (Acrobat's Output
//! Preview "Show" warnings and the matching Preflight checks).
//!
//! [`check`] reads the document only. It interprets each page's content stream (and the forms
//! and coloured tiling patterns it draws) with a small, bounded graphics-state tracker: the
//! fill and stroke colours, the overprint parameters set by `gs` (`OP`, `op`, `OPM`), the
//! current transformation and text matrices and the font size, and the marked-content stack.
//! Each painting operator (fill, stroke, text, stencil-mask image) is then classified:
//!
//! - **overprint-state** (info): an ExtGState that turns overprint on (`OP`/`op` true) or sets
//!   overprint mode 1 is applied;
//! - **overprint-paint** (info): an object is filled or stroked while overprint is on;
//! - **overprint-white** (warning): a white or zero-tint object is painted with overprint on. In
//!   DeviceCMYK with `OPM 1`, and for Separation and DeviceN colours, the zero components leave
//!   the plates below untouched, so the object disappears on press; for other spaces it knocks
//!   out per ISO 32000 but is a classic RIP-dependent hazard;
//! - **rich-black** (info): a DeviceCMYK colour (also ICCBased with four components, Indexed over
//!   those, DeviceN process colorants, and Separation/DeviceN spot colours whose tint transform
//!   is a Type 0 one-input or Type 2 function into DeviceCMYK or DeviceGray) has K at or above
//!   the black threshold and C+M+Y at or above the CMY threshold; the total ink is reported;
//! - **rich-black-small-text** (warning): text in rich black below the small-text size, from the
//!   font size × the text and current transformation matrices (registration risk);
//! - **registration-colour** (warning): `/Separation /All` (or a DeviceN `All` colorant) painted
//!   outside printer marks. Marks are recognised by marked content (an `Artifact` whose
//!   properties name `PrinterMarks`, or an optional content group named like "Printer marks")
//!   or by lying entirely outside the page's TrimBox.
//!
//! Findings are de-duplicated per (kind, page, object, paint, colour, text size) with an
//! occurrence count. All work is bounded: decoded bytes, operators, nesting depth (seen-set
//! for forms and patterns), marked-content and q/Q stacks, and kept findings.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use pdfcraft_content::{Matrix, Op, parse};
use pdfcraft_cos::{Dict, Document, ObjRef, Object};

#[cfg(test)]
mod tests;

/// Default black threshold, percent K at or above which a colour counts as black.
pub const DEFAULT_RICH_BLACK_K: f64 = 95.0;
/// Default CMY threshold, percent C+M+Y at or above which a black is rich.
pub const DEFAULT_RICH_BLACK_CMY: f64 = 10.0;
/// Default small-text size, in points: rich-black text below it is a registration risk.
pub const DEFAULT_SMALL_TEXT_PT: f64 = 12.0;
pub const DEFAULT_MAX_FINDINGS: usize = 100;
pub const MAX_FINDINGS: usize = 10_000;

/// Decoded content bytes per stream, and per check.
const MAX_STREAM: usize = 16 << 20;
const MAX_TOTAL: usize = 256 << 20;
const MAX_OPERATORS: usize = 10_000_000;
/// Nesting of forms and patterns.
const MAX_DEPTH: usize = 16;
const MAX_STACK: usize = 256;
const MAX_MARKED: usize = 256;
const MAX_CS_DEPTH: usize = 8;
const MAX_COMPONENTS: usize = 32;
const MAX_SAMPLES: usize = 4096;
const MAX_STREAMS: usize = 4096;
/// De-duplication keys remembered beyond the kept findings.
const MAX_KEYS: usize = 200_000;

/// What isn't checked, or only approximately, for reports.
pub const NOT_CHECKED: &[&str] = &[
    "RGB, Lab, CalRGB and three-component ICC colours: whether they separate into a rich black needs an output profile (counted under unresolved)",
    "Image pixels (only stencil masks, which paint the fill colour, and overprint on images are checked) and shadings, including shading patterns",
    "Spot colours whose tint transform is not a Type 0 one-input or Type 2 function into DeviceCMYK or DeviceGray (counted under unresolved)",
    "Annotation appearances (including PrinterMark annotations), Type 3 glyph procedures and whether optional content is visible",
    "What overlaps what: objects later covered by others are still reported, and overprint is not simulated",
    "Text size is the font size scaled by the text and current matrices; UserUnit and glyph outlines are ignored, and thin rich-black strokes are not flagged",
    "Printer marks are recognised only by marked content (Artifact with PrinterMarks, an optional content group named like \"Printer marks\") or by lying outside the TrimBox (text by its origin)",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    OverprintState,
    OverprintPaint,
    OverprintWhite,
    RichBlack,
    RichBlackSmallText,
    Registration,
}

impl Kind {
    pub const ALL: [Kind; 6] =
        [Kind::OverprintState, Kind::OverprintPaint, Kind::OverprintWhite, Kind::RichBlack, Kind::RichBlackSmallText, Kind::Registration];

    pub fn id(self) -> &'static str {
        match self {
            Kind::OverprintState => "overprint-state",
            Kind::OverprintPaint => "overprint-paint",
            Kind::OverprintWhite => "overprint-white",
            Kind::RichBlack => "rich-black",
            Kind::RichBlackSmallText => "rich-black-small-text",
            Kind::Registration => "registration-colour",
        }
    }

    pub fn severity(self) -> Severity {
        match self {
            Kind::OverprintState | Kind::OverprintPaint | Kind::RichBlack => Severity::Info,
            Kind::OverprintWhite | Kind::RichBlackSmallText | Kind::Registration => Severity::Warning,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Warning,
}

impl Severity {
    pub fn id(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
        }
    }
}

/// What was painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaintKind {
    Fill,
    Stroke,
    Text,
    /// Text drawn with a stroking render mode.
    TextStroke,
    Image,
}

impl PaintKind {
    pub fn id(self) -> &'static str {
        match self {
            PaintKind::Fill => "fill",
            PaintKind::Stroke => "stroke",
            PaintKind::Text => "text",
            PaintKind::TextStroke => "text-stroke",
            PaintKind::Image => "image",
        }
    }

    fn strokes(self) -> bool {
        matches!(self, PaintKind::Stroke | PaintKind::TextStroke)
    }
}

/// A painted colour.
#[derive(Clone, Debug, PartialEq)]
pub struct Colour {
    /// `DeviceCMYK`, `Separation /Gold`, `DeviceN [/Cyan /Black]`, …
    pub space: String,
    /// The colour's components as given.
    pub components: Vec<f64>,
    /// Its process inks C, M, Y, K in percent, when they could be determined.
    pub cmyk: Option<[f64; 4]>,
    /// The CMYK came from a spot colour's alternate (it prints so only if converted to process).
    pub via_alternate: bool,
}

impl Colour {
    /// C+M+Y+K in percent.
    pub fn total(&self) -> Option<f64> {
        self.cmyk.map(|c| round(c.iter().sum()))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub kind: Kind,
    pub severity: Severity,
    /// 0-based page.
    pub page: usize,
    /// The ExtGState for overprint-state; else the form, pattern or page whose content paints.
    pub object: Option<ObjRef>,
    pub message: String,
    pub paint: Option<PaintKind>,
    pub colour: Option<Colour>,
    /// The effective text size in points, for text.
    pub text_size: Option<f64>,
    /// The overprint mode in effect, for overprint findings.
    pub overprint_mode: Option<i64>,
    /// The painted objects' bounding box in page user space, where known (text: origins).
    pub bbox: Option<[f64; 4]>,
    /// How many painted objects (or `gs` operators) this finding stands for.
    pub occurrences: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// 0-based pages to check; empty checks every page.
    pub pages: Vec<usize>,
    /// Percent K at or above which a colour is black (1–100).
    pub rich_black_k: f64,
    /// Percent C+M+Y at or above which a black is rich (1–300).
    pub rich_black_cmy: f64,
    /// Text below this size (points) in rich black is a warning (1–144).
    pub small_text_pt: f64,
    /// Findings kept per kind (1–[`MAX_FINDINGS`]); counts include the rest.
    pub max_findings: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            pages: Vec::new(),
            rich_black_k: DEFAULT_RICH_BLACK_K,
            rich_black_cmy: DEFAULT_RICH_BLACK_CMY,
            small_text_pt: DEFAULT_SMALL_TEXT_PT,
            max_findings: DEFAULT_MAX_FINDINGS,
        }
    }
}

impl Options {
    pub fn validate(&self) -> Result<(), String> {
        let within = |v: f64, lo: f64, hi: f64| v.is_finite() && (lo..=hi).contains(&v);
        if !within(self.rich_black_k, 1.0, 100.0) {
            return Err("rich_black_k must be a percentage between 1 and 100".into());
        }
        if !within(self.rich_black_cmy, 1.0, 300.0) {
            return Err("rich_black_cmy_total must be a percentage between 1 and 300".into());
        }
        if !within(self.small_text_pt, 1.0, 144.0) {
            return Err("small_text_pt must be between 1 and 144 points".into());
        }
        if !(1..=MAX_FINDINGS).contains(&self.max_findings) {
            return Err(format!("max_findings must be between 1 and {MAX_FINDINGS}"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Report {
    /// The 0-based pages checked.
    pub pages: Vec<usize>,
    /// Findings, at most `max_findings` per kind, in page order.
    pub findings: Vec<Finding>,
    /// Kind → distinct findings (including those not kept).
    pub counts: BTreeMap<&'static str, usize>,
    /// Kind → painted objects (or `gs` operators) behind them.
    pub occurrences: BTreeMap<&'static str, usize>,
    /// Some findings were counted but not kept.
    pub findings_truncated: bool,
    /// Painted objects whose colour could not be classified, by reason.
    pub unresolved: BTreeMap<&'static str, usize>,
    /// Limits reached (the check did not read everything).
    pub limits: Vec<String>,
}

impl Report {
    pub fn count(&self, kind: Kind) -> usize {
        self.counts.get(kind.id()).copied().unwrap_or(0)
    }

    pub fn warnings(&self) -> usize {
        Kind::ALL.iter().filter(|k| k.severity() == Severity::Warning).map(|k| self.count(*k)).sum()
    }
}

/// Check `options.pages` (all pages when empty).
pub fn check(doc: &Document, options: &Options) -> Result<Report, String> {
    options.validate()?;
    let pages = pdfcraft_model::pages(doc);
    let selected: Vec<usize> = if options.pages.is_empty() { (0..pages.len()).collect() } else { options.pages.clone() };
    if let Some(bad) = selected.iter().find(|p| **p >= pages.len()) {
        return Err(format!("page {} does not exist; the document has {} pages", bad.saturating_add(1), pages.len()));
    }
    let mut walk = Walk {
        doc,
        options,
        out: Out {
            cap: options.max_findings,
            findings: Vec::new(),
            index: HashMap::new(),
            counts: BTreeMap::new(),
            occurrences: BTreeMap::new(),
            truncated: false,
        },
        seen: HashSet::new(),
        bytes: 0,
        streams: 0,
        ops: 0,
        stopped: false,
        limits: Vec::new(),
        unresolved: BTreeMap::new(),
        page: 0,
        trim: None,
        depth: 0,
    };
    for &index in &selected {
        let Some(page) = pages.get(index) else { continue };
        if walk.stopped {
            break;
        }
        walk.page(index, page);
    }
    let mut counts = BTreeMap::new();
    let mut occurrences = BTreeMap::new();
    for k in Kind::ALL {
        counts.insert(k.id(), walk.out.counts.get(&k).copied().unwrap_or(0));
        occurrences.insert(k.id(), walk.out.occurrences.get(&k).copied().unwrap_or(0));
    }
    Ok(Report {
        pages: selected,
        findings: walk.out.findings,
        counts,
        occurrences,
        findings_truncated: walk.out.truncated,
        unresolved: walk.unresolved,
        limits: walk.limits,
    })
}

fn round(v: f64) -> f64 {
    if v.is_finite() { (v * 100.0).round() / 100.0 } else { 0.0 }
}

fn clamp01(v: f64) -> f64 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

fn finite(m: &Matrix) -> bool {
    m.0.iter().all(|v| v.is_finite() && v.abs() < 1e12)
}

fn union(a: Option<[f64; 4]>, b: Option<[f64; 4]>) -> Option<[f64; 4]> {
    match (a, b) {
        (Some(a), Some(b)) => Some([a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]),
        (a, None) => a,
        (None, b) => b,
    }
}

fn name_text(n: &[u8]) -> String {
    String::from_utf8_lossy(n).chars().take(64).collect()
}

// ---- findings -------------------------------------------------------------------------------

struct Out {
    cap: usize,
    findings: Vec<Finding>,
    /// Key → index of the kept finding (None: counted, not kept).
    index: HashMap<String, Option<usize>>,
    counts: BTreeMap<Kind, usize>,
    occurrences: BTreeMap<Kind, usize>,
    truncated: bool,
}

impl Out {
    fn add(&mut self, kind: Kind, key: String, finding: impl FnOnce() -> Finding) {
        let n = self.occurrences.entry(kind).or_insert(0);
        *n = n.saturating_add(1);
        match self.index.get(&key) {
            Some(Some(i)) => {
                let new = finding();
                if let Some(f) = self.findings.get_mut(*i) {
                    f.occurrences = f.occurrences.saturating_add(1);
                    f.bbox = union(f.bbox, new.bbox);
                }
                return;
            }
            Some(None) => return,
            None => {}
        }
        let distinct = self.counts.entry(kind).or_insert(0);
        *distinct = distinct.saturating_add(1);
        if *distinct <= self.cap {
            self.findings.push(finding());
            if self.index.len() < MAX_KEYS {
                self.index.insert(key, Some(self.findings.len().saturating_sub(1)));
            }
        } else {
            self.truncated = true;
            if self.index.len() < MAX_KEYS {
                self.index.insert(key, None);
            }
        }
    }
}

// ---- colour ---------------------------------------------------------------------------------

/// A tint transform into the alternate space.
#[derive(Clone, Debug)]
enum Tint {
    /// Type 2: `C0 + t^N (C1 − C0)`.
    Exponential { domain: [f64; 2], c0: Vec<f64>, c1: Vec<f64>, n: f64 },
    /// Type 0 with one input, linearly interpolated.
    Sampled { domain: [f64; 2], encode: [f64; 2], samples: Vec<Vec<f64>> },
}

impl Tint {
    fn eval(&self, t: f64) -> Vec<f64> {
        match self {
            Tint::Exponential { domain, c0, c1, n } => {
                let t = if t.is_finite() { t.clamp(domain[0].min(domain[1]), domain[0].max(domain[1])) } else { domain[0] };
                let p = t.powf(*n);
                let p = if p.is_finite() { p } else { 0.0 };
                c0.iter().zip(c1).map(|(a, b)| clamp01(a + p * (b - a))).collect()
            }
            Tint::Sampled { domain, encode, samples } => {
                let t = if t.is_finite() { t.clamp(domain[0].min(domain[1]), domain[0].max(domain[1])) } else { domain[0] };
                let span = domain[1] - domain[0];
                let e = if span.abs() > 1e-12 { encode[0] + (t - domain[0]) * (encode[1] - encode[0]) / span } else { encode[0] };
                let last = samples.len().saturating_sub(1) as f64;
                let e = if e.is_finite() { e.clamp(0.0, last) } else { 0.0 };
                let (lo, hi) = (e.floor() as usize, e.ceil() as usize);
                let f = e - e.floor();
                let (Some(a), Some(b)) = (samples.get(lo), samples.get(hi)) else { return Vec::new() };
                a.iter().zip(b).map(|(a, b)| clamp01(a + f * (b - a))).collect()
            }
        }
    }
}

/// How a spot colour's alternate gives process inks.
#[derive(Clone, Debug)]
struct Alternate {
    tint: Tint,
    /// The alternate is DeviceGray (one output, K = 1 − gray), else CMYK.
    gray: bool,
}

#[derive(Clone, Debug)]
enum Cs {
    Gray,
    Rgb,
    Cmyk,
    /// ICCBased with four components (treated as CMYK inks).
    IccCmyk,
    Lab,
    Separation {
        name: Vec<u8>,
        alternate: Option<Alternate>,
    },
    DeviceN {
        names: Vec<Vec<u8>>,
        alternate: Option<Alternate>,
    },
    Indexed {
        base: Box<Cs>,
        hival: usize,
        lookup: Vec<u8>,
    },
    Pattern(Option<Box<Cs>>),
    Unknown(usize),
}

fn process_plate(name: &[u8]) -> Option<usize> {
    match name {
        b"Cyan" => Some(0),
        b"Magenta" => Some(1),
        b"Yellow" => Some(2),
        b"Black" => Some(3),
        _ => None,
    }
}

impl Cs {
    fn components(&self) -> usize {
        match self {
            Cs::Gray | Cs::Separation { .. } | Cs::Indexed { .. } => 1,
            Cs::Rgb | Cs::Lab => 3,
            Cs::Cmyk | Cs::IccCmyk => 4,
            Cs::DeviceN { names, .. } => names.len(),
            Cs::Pattern(under) => under.as_ref().map_or(0, |u| u.components()),
            Cs::Unknown(n) => *n,
        }
    }

    fn initial(&self) -> Vec<f64> {
        match self {
            Cs::Cmyk | Cs::IccCmyk => vec![0.0, 0.0, 0.0, 1.0],
            Cs::Separation { .. } | Cs::DeviceN { .. } => vec![1.0; self.components()],
            _ => vec![0.0; self.components()],
        }
    }

    fn label(&self) -> String {
        match self {
            Cs::Gray => "DeviceGray".into(),
            Cs::Rgb => "DeviceRGB".into(),
            Cs::Cmyk => "DeviceCMYK".into(),
            Cs::IccCmyk => "ICCBased (CMYK)".into(),
            Cs::Lab => "Lab".into(),
            Cs::Separation { name, .. } => format!("Separation /{}", name_text(name)),
            Cs::DeviceN { names, .. } => {
                let names: Vec<String> = names.iter().take(8).map(|n| format!("/{}", name_text(n))).collect();
                format!("DeviceN [{}]", names.join(" "))
            }
            Cs::Indexed { base, .. } => format!("Indexed {}", base.label()),
            Cs::Pattern(_) => "Pattern".into(),
            Cs::Unknown(_) => "unknown".into(),
        }
    }

    /// Process inks C, M, Y, K (0–1) for components `c`, and whether they came from a spot
    /// colour's alternate; `Err` names why they couldn't be found.
    fn cmyk(&self, c: &[f64]) -> Result<([f64; 4], bool), &'static str> {
        let at = |i: usize| clamp01(c.get(i).copied().unwrap_or(0.0));
        match self {
            Cs::Cmyk | Cs::IccCmyk => Ok(([at(0), at(1), at(2), at(3)], false)),
            Cs::Gray => Ok(([0.0, 0.0, 0.0, 1.0 - at(0)], false)),
            Cs::Separation { name, alternate } => {
                if name == b"None" || name == b"All" {
                    return Ok(([0.0; 4], false));
                }
                if let Some(p) = process_plate(name) {
                    let mut out = [0.0; 4];
                    if let Some(slot) = out.get_mut(p) {
                        *slot = at(0);
                    }
                    return Ok((out, false));
                }
                let alt = alternate.as_ref().ok_or("spot colours with an unevaluated tint transform")?;
                alt.apply(&[at(0)]).map(|v| (v, true))
            }
            Cs::DeviceN { names, alternate } => {
                let spots = names.iter().any(|n| process_plate(n).is_none() && n != b"None");
                if spots && let Some(alt) = alternate {
                    let comps: Vec<f64> = (0..names.len()).map(at).collect();
                    if let Ok(v) = alt.apply(&comps) {
                        return Ok((v, true));
                    }
                }
                // The process colorants are exact; spot colorants aren't C, M, Y or K.
                let mut out = [0.0; 4];
                for (i, n) in names.iter().enumerate() {
                    if let Some(slot) = process_plate(n).and_then(|p| out.get_mut(p)) {
                        *slot = at(i);
                    }
                }
                Ok((out, false))
            }
            Cs::Indexed { base, hival, lookup } => {
                let index = c.first().copied().unwrap_or(0.0);
                let index = if index.is_finite() { index.round().clamp(0.0, *hival as f64) as usize } else { 0 };
                let n = base.components();
                let start = index.checked_mul(n).ok_or("indexed colours outside their lookup table")?;
                let entry = lookup.get(start..start.saturating_add(n)).ok_or("indexed colours outside their lookup table")?;
                let comps: Vec<f64> = entry.iter().map(|b| f64::from(*b) / 255.0).collect();
                base.cmyk(&comps)
            }
            Cs::Rgb | Cs::Lab => Err("RGB and Lab colours (need an output profile)"),
            Cs::Pattern(_) => Err("pattern colours"),
            Cs::Unknown(_) => Err("unknown colour spaces"),
        }
    }

    /// The colour paints nothing: white, or zero tint of every colorant.
    fn white(&self, c: &[f64]) -> bool {
        let at = |i: usize| c.get(i).copied().unwrap_or(0.0);
        let zero = |v: f64| v.is_finite() && v <= 1e-6;
        match self {
            Cs::Cmyk | Cs::IccCmyk => (0..4).all(|i| zero(at(i))),
            Cs::Gray => at(0) >= 1.0 - 1e-6,
            Cs::Rgb => (0..3).all(|i| at(i) >= 1.0 - 1e-6),
            Cs::Lab => at(0) >= 100.0 - 1e-6 && at(1).abs() < 1e-6 && at(2).abs() < 1e-6,
            Cs::Separation { name, .. } => name != b"None" && zero(at(0)),
            Cs::DeviceN { names, .. } => names.iter().any(|n| n != b"None") && (0..names.len()).all(|i| zero(at(i))),
            Cs::Indexed { .. } => self.cmyk(c).is_ok_and(|(v, _)| v.iter().all(|x| zero(*x))),
            Cs::Pattern(_) | Cs::Unknown(_) => false,
        }
    }

    /// Whether a white overprinted object leaves the plates below untouched (disappears).
    fn white_disappears(&self, opm: i64) -> bool {
        match self {
            Cs::Cmyk => opm == 1,
            Cs::Separation { name, .. } => name != b"All",
            Cs::DeviceN { names, .. } => !names.iter().any(|n| n == b"All"),
            _ => false,
        }
    }

    /// The registration colour's tint, when this colour uses it.
    fn registration(&self, c: &[f64]) -> Option<f64> {
        let at = |i: usize| clamp01(c.get(i).copied().unwrap_or(0.0));
        match self {
            Cs::Separation { name, .. } if name == b"All" => Some(at(0)).filter(|t| *t > 0.0),
            Cs::DeviceN { names, .. } => names.iter().position(|n| n == b"All").map(at).filter(|t| *t > 0.0),
            _ => None,
        }
    }
}

impl Alternate {
    fn apply(&self, comps: &[f64]) -> Result<[f64; 4], &'static str> {
        let t = match comps {
            [t] => *t,
            _ => return Err("spot colours with an unevaluated tint transform"),
        };
        let out = self.tint.eval(t);
        match (self.gray, out.as_slice()) {
            (true, [g]) => Ok([0.0, 0.0, 0.0, 1.0 - clamp01(*g)]),
            (false, [c, m, y, k]) => Ok([*c, *m, *y, *k]),
            _ => Err("spot colours with an unevaluated tint transform"),
        }
    }
}

#[derive(Clone, Debug)]
struct Paint {
    cs: Rc<Cs>,
    comps: Vec<f64>,
    /// The pattern a `/P scn` selects.
    pattern: Option<Object>,
}

impl Paint {
    fn new(cs: Cs, comps: Vec<f64>) -> Self {
        Self { cs: Rc::new(cs), comps, pattern: None }
    }

    fn colour(&self) -> Colour {
        let cmyk = self.cs.cmyk(&self.comps).ok();
        Colour {
            space: self.cs.label(),
            components: self.comps.iter().map(|v| round(*v * 10_000.0) / 10_000.0).collect(),
            cmyk: cmyk.map(|(v, _)| v.map(|x| round(x * 100.0))),
            via_alternate: cmyk.is_some_and(|(_, a)| a),
        }
    }

    /// A short, stable fingerprint for de-duplication.
    fn key(&self) -> String {
        let comps: Vec<String> = self.comps.iter().map(|v| format!("{:.4}", v)).collect();
        format!("{}|{}", self.cs.label(), comps.join(","))
    }
}

// ---- interpreter ----------------------------------------------------------------------------

#[derive(Clone)]
struct Gs {
    ctm: Matrix,
    fill: Paint,
    stroke: Paint,
    op_fill: bool,
    op_stroke: bool,
    opm: i64,
    /// The ExtGState that last set overprint.
    op_source: Option<ObjRef>,
    size: f64,
    render: i64,
    rise: f64,
    leading: f64,
}

impl Gs {
    fn new(ctm: Matrix) -> Self {
        Self {
            ctm,
            fill: Paint::new(Cs::Gray, vec![0.0]),
            stroke: Paint::new(Cs::Gray, vec![0.0]),
            op_fill: false,
            op_stroke: false,
            opm: 0,
            op_source: None,
            size: 0.0,
            render: 0,
            rise: 0.0,
            leading: 0.0,
        }
    }
}

/// Where content runs: the object that owns it, and whether it is a printer mark.
#[derive(Clone, Copy)]
struct At {
    owner: Option<ObjRef>,
    mark: bool,
}

struct Walk<'a> {
    doc: &'a Document,
    options: &'a Options,
    out: Out,
    /// Forms and patterns being interpreted (cycle guard).
    seen: HashSet<ObjRef>,
    bytes: usize,
    streams: usize,
    ops: usize,
    stopped: bool,
    limits: Vec<String>,
    unresolved: BTreeMap<&'static str, usize>,
    page: usize,
    trim: Option<[f64; 4]>,
    /// Nesting of the content being interpreted.
    depth: usize,
}

fn rect_of(doc: &Document, d: &Dict, key: &[u8]) -> Option<[f64; 4]> {
    let o = doc.resolve(d.get(key)?);
    let v: Vec<f64> = o.as_array()?.iter().filter_map(|x| doc.resolve(x).as_f64()).collect();
    match v.as_slice() {
        [a, b, c, d] if v.iter().all(|x| x.is_finite()) => Some([a.min(*c), b.min(*d), a.max(*c), b.max(*d)]),
        _ => None,
    }
}

fn resource(doc: &Document, res: &Dict, category: &[u8], name: &[u8]) -> Option<Object> {
    res.get(category).map(|d| doc.resolve(d)).and_then(|d| d.as_dict().and_then(|d| d.get(name).cloned()))
}

impl Walk<'_> {
    fn limit(&mut self, what: String) {
        if !self.limits.contains(&what) && self.limits.len() < 32 {
            self.limits.push(what);
        }
    }

    fn unresolved(&mut self, why: &'static str) {
        let n = self.unresolved.entry(why).or_insert(0);
        *n = n.saturating_add(1);
    }

    fn page(&mut self, index: usize, page: &pdfcraft_model::Page) {
        self.page = index;
        self.trim = rect_of(self.doc, &page.dict, b"TrimBox");
        let resources = page.dict.get(b"Resources").map(|o| self.doc.resolve(o));
        let empty = Dict::new();
        let resources = resources.as_ref().and_then(|o| o.as_dict()).unwrap_or(&empty);
        let Some(contents) = page.dict.get(b"Contents") else { return };
        let contents = self.doc.resolve(contents);
        let parts: Vec<Object> = match contents.as_ref() {
            Object::Array(a) => a.iter().take(MAX_STREAMS).cloned().collect(),
            other => vec![other.clone()],
        };
        let mut data = Vec::new();
        for part in parts {
            let s = self.doc.resolve(&part);
            let Object::Stream(s) = s.as_ref() else { continue };
            match self.decode(s) {
                Some(d) => {
                    data.extend(d);
                    data.push(b'\n');
                }
                None => break,
            }
        }
        self.run(&data, resources, Gs::new(Matrix::IDENTITY), At { owner: Some(page.obj), mark: false }, 0);
    }

    fn decode(&mut self, s: &pdfcraft_cos::Stream) -> Option<Vec<u8>> {
        self.streams = self.streams.saturating_add(1);
        let room = MAX_TOTAL.saturating_sub(self.bytes);
        if room == 0 || self.streams > MAX_STREAMS.saturating_mul(16) {
            self.limit(format!("Only the first {} MB of content were checked", MAX_TOTAL >> 20));
            self.stopped = true;
            return None;
        }
        match s.decoded_within(MAX_STREAM.min(room)) {
            Ok(d) => {
                self.bytes = self.bytes.saturating_add(d.len());
                Some(d)
            }
            Err(_) => {
                self.unresolved("unreadable content streams");
                None
            }
        }
    }

    fn colour_space(&mut self, o: &Object, res: &Dict, depth: usize) -> Cs {
        if depth > MAX_CS_DEPTH {
            return Cs::Unknown(1);
        }
        let o = self.doc.resolve(o);
        match o.as_ref() {
            Object::Name(n) => match n.as_slice() {
                b"DeviceGray" | b"G" | b"CalGray" => Cs::Gray,
                b"DeviceRGB" | b"RGB" | b"CalRGB" => Cs::Rgb,
                b"DeviceCMYK" | b"CMYK" => Cs::Cmyk,
                b"Pattern" => Cs::Pattern(None),
                other => match resource(self.doc, res, b"ColorSpace", other) {
                    Some(v) => self.colour_space(&v, &Dict::new(), depth + 1),
                    None => Cs::Unknown(1),
                },
            },
            Object::Array(a) => {
                let family = a.first().map(|f| self.doc.resolve(f)).and_then(|f| f.as_name().map(<[u8]>::to_vec)).unwrap_or_default();
                let arg = |i: usize| a.get(i).map(|o| self.doc.resolve(o));
                match family.as_slice() {
                    b"DeviceGray" | b"CalGray" | b"G" => Cs::Gray,
                    b"DeviceRGB" | b"CalRGB" | b"RGB" => Cs::Rgb,
                    b"DeviceCMYK" | b"CMYK" => Cs::Cmyk,
                    b"Lab" => Cs::Lab,
                    b"ICCBased" => {
                        let n = arg(1).and_then(|s| s.as_dict().and_then(|d| d.get(b"N")).and_then(|n| self.doc.resolve(n).as_int()));
                        match n {
                            Some(1) => Cs::Gray,
                            Some(3) => Cs::Rgb,
                            Some(4) => Cs::IccCmyk,
                            Some(n) => Cs::Unknown(usize::try_from(n).unwrap_or(1).clamp(1, MAX_COMPONENTS)),
                            None => Cs::Unknown(1),
                        }
                    }
                    b"Separation" => {
                        let name = arg(1).and_then(|n| n.as_name().map(<[u8]>::to_vec)).unwrap_or_default();
                        let alternate = self.alternate(a.get(2), a.get(3), res, depth);
                        Cs::Separation { name, alternate }
                    }
                    b"DeviceN" => match arg(1).as_deref().and_then(Object::as_array) {
                        Some(names) if !names.is_empty() && names.len() <= MAX_COMPONENTS => {
                            let names: Vec<Vec<u8>> =
                                names.iter().map(|n| self.doc.resolve(n).as_name().map(<[u8]>::to_vec).unwrap_or_default()).collect();
                            let alternate = if names.len() == 1 { self.alternate(a.get(2), a.get(3), res, depth) } else { None };
                            Cs::DeviceN { names, alternate }
                        }
                        _ => Cs::Unknown(1),
                    },
                    b"Indexed" | b"I" => {
                        let base = a.get(1).map_or(Cs::Unknown(1), |b| self.colour_space(b, res, depth + 1));
                        if matches!(base, Cs::Indexed { .. } | Cs::Pattern(_) | Cs::Unknown(_)) {
                            return Cs::Unknown(1);
                        }
                        let hival = arg(2).and_then(|h| h.as_int()).unwrap_or(0).clamp(0, 255) as usize;
                        let want = (hival + 1).saturating_mul(base.components());
                        let lookup = match arg(3).as_deref() {
                            Some(Object::String(s)) => s.bytes.iter().take(want).copied().collect(),
                            Some(Object::Stream(s)) => {
                                s.decoded_within(want.max(1).saturating_mul(2)).unwrap_or_default().into_iter().take(want).collect()
                            }
                            _ => Vec::new(),
                        };
                        Cs::Indexed { base: Box::new(base), hival, lookup }
                    }
                    b"Pattern" => {
                        let under = a.get(1).map(|u| self.colour_space(u, res, depth + 1));
                        Cs::Pattern(under.map(Box::new))
                    }
                    _ => Cs::Unknown(1),
                }
            }
            _ => Cs::Unknown(1),
        }
    }

    /// A spot colour's alternate, when it is DeviceCMYK or DeviceGray with a tint transform
    /// we evaluate.
    fn alternate(&mut self, space: Option<&Object>, function: Option<&Object>, res: &Dict, depth: usize) -> Option<Alternate> {
        let space = self.colour_space(space?, res, depth + 1);
        let (gray, outputs) = match space {
            Cs::Cmyk | Cs::IccCmyk => (false, 4),
            Cs::Gray => (true, 1),
            _ => return None,
        };
        let f = self.doc.resolve(function?);
        let d = f.as_dict()?;
        let nums = |key: &[u8]| -> Option<Vec<f64>> {
            let a = self.doc.resolve(d.get(key)?);
            let v: Vec<f64> = a.as_array()?.iter().take(64).filter_map(|x| self.doc.resolve(x).as_f64()).filter(|x| x.is_finite()).collect();
            Some(v)
        };
        let domain = match nums(b"Domain").as_deref() {
            Some([a, b, ..]) => [*a, *b],
            _ => [0.0, 1.0],
        };
        let tint = match d.get(b"FunctionType").and_then(|t| self.doc.resolve(t).as_int()) {
            Some(2) => {
                let c0 = nums(b"C0").unwrap_or_else(|| vec![0.0]);
                let c1 = nums(b"C1").unwrap_or_else(|| vec![1.0]);
                let n = d.get(b"N").and_then(|n| self.doc.resolve(n).as_f64()).filter(|n| n.is_finite())?;
                if c0.len() != outputs || c1.len() != outputs {
                    return None;
                }
                Tint::Exponential { domain, c0, c1, n }
            }
            Some(0) => {
                let Object::Stream(s) = f.as_ref() else { return None };
                let size = match nums(b"Size").as_deref() {
                    Some([n]) if *n >= 1.0 && *n <= MAX_SAMPLES as f64 => *n as usize,
                    _ => return None,
                };
                let bps = d.get(b"BitsPerSample").and_then(|b| self.doc.resolve(b).as_int())?;
                let bytes = match bps {
                    8 => 1usize,
                    16 => 2,
                    _ => return None,
                };
                let range = nums(b"Range")?;
                if range.len() != outputs.saturating_mul(2) {
                    return None;
                }
                let decode = nums(b"Decode").filter(|v| v.len() == range.len()).unwrap_or_else(|| range.clone());
                let encode = match nums(b"Encode").as_deref() {
                    Some([a, b]) => [*a, *b],
                    _ => [0.0, size.saturating_sub(1) as f64],
                };
                let need = size.checked_mul(outputs)?.checked_mul(bytes)?;
                let data = s.decoded_within(need.saturating_add(16)).ok()?;
                let max = if bytes == 1 { 255.0 } else { 65535.0 };
                let mut samples = Vec::with_capacity(size);
                for i in 0..size {
                    let mut row = Vec::with_capacity(outputs);
                    for j in 0..outputs {
                        let at = i.checked_mul(outputs)?.checked_add(j)?.checked_mul(bytes)?;
                        let raw = if bytes == 1 {
                            f64::from(*data.get(at)?)
                        } else {
                            f64::from(u16::from_be_bytes([*data.get(at)?, *data.get(at.checked_add(1)?)?]))
                        };
                        let (d0, d1) = (decode.get(2 * j).copied().unwrap_or(0.0), decode.get(2 * j + 1).copied().unwrap_or(1.0));
                        row.push(d0 + raw * (d1 - d0) / max);
                    }
                    samples.push(row);
                }
                Tint::Sampled { domain, encode, samples }
            }
            _ => return None,
        };
        Some(Alternate { tint, gray })
    }

    /// Whether a marked-content sequence starting with `op` (BDC) marks printer marks.
    fn marks(&self, op: &Op, res: &Dict) -> bool {
        let tag = op.name(0).unwrap_or_default();
        let props = match op.operands.get(1) {
            Some(Object::Name(n)) => resource(self.doc, res, b"Properties", n).map(|p| self.doc.resolve(&p)),
            Some(other) => Some(self.doc.resolve(other)),
            None => None,
        };
        let Some(props) = props else { return false };
        let Some(d) = props.as_dict() else { return false };
        if tag == b"Artifact" {
            return d.iter().take(64).any(|(_, v)| matches!(self.doc.resolve(v).as_name(), Some(b"PrinterMarks" | b"PrinterMark")));
        }
        if tag == b"OC" && d.name(b"Type") != Some(b"OCMD") {
            let name = d.get(b"Name").map(|n| self.doc.resolve(n)).and_then(|n| n.as_string().map(|s| s.bytes.clone())).unwrap_or_default();
            let name = String::from_utf8_lossy(&name).to_lowercase();
            return name.contains("printer mark") || name.contains("printer's mark") || name.contains("printers mark");
        }
        false
    }

    /// Classify one painted object.
    fn paint(&mut self, gs: &Gs, kind: PaintKind, bbox: Option<[f64; 4]>, at: At, text_size: Option<f64>) {
        let paint = if kind.strokes() { gs.stroke.clone() } else { gs.fill.clone() };
        // A coloured tiling pattern paints its own content; an uncoloured one paints in the
        // colour given with it.
        if let Some(pattern) = &paint.pattern
            && kind != PaintKind::Image
            && self.pattern(pattern.clone(), gs, at)
        {
            return;
        }
        let cs = match paint.cs.as_ref() {
            Cs::Pattern(Some(under)) => Rc::new((**under).clone()),
            _ => paint.cs.clone(),
        };
        let paint = Paint { cs, comps: paint.comps, pattern: None };
        let page = self.page;
        let colour_key = paint.key();
        let size_key = text_size.map(|s| format!("{:.1}", s)).unwrap_or_default();
        let overprint = if kind.strokes() { gs.op_stroke } else { gs.op_fill };

        // Registration colour outside printer marks.
        if kind != PaintKind::Image
            && let Some(tint) = paint.cs.registration(&paint.comps)
        {
            let outside_trim = match (self.trim, bbox) {
                (Some(t), Some(b)) => b[2] <= t[0] || b[0] >= t[2] || b[3] <= t[1] || b[1] >= t[3],
                _ => false,
            };
            if !at.mark && !outside_trim {
                let colour = paint.colour();
                self.out.add(Kind::Registration, format!("{page}|{:?}|{}|{colour_key}", at.owner, kind.id()), || Finding {
                    kind: Kind::Registration,
                    severity: Kind::Registration.severity(),
                    page,
                    object: at.owner,
                    message: format!(
                        "A {} in the registration colour (Separation /All, {:.0}% on every plate) outside printer marks",
                        kind.id(),
                        tint * 100.0
                    ),
                    paint: Some(kind),
                    colour: Some(colour),
                    text_size,
                    overprint_mode: None,
                    bbox,
                    occurrences: 1,
                });
            }
        }

        // Overprint.
        if overprint {
            let opm = gs.opm;
            let white = kind != PaintKind::Image && paint.cs.white(&paint.comps);
            let k = if white { Kind::OverprintWhite } else { Kind::OverprintPaint };
            let colour = (kind != PaintKind::Image).then(|| paint.colour());
            let disappears = paint.cs.white_disappears(opm);
            let key =
                format!("{page}|{:?}|{}|{}|{opm}|{:?}", at.owner, kind.id(), if kind == PaintKind::Image { "" } else { &colour_key }, gs.op_source);
            let label = paint.cs.label();
            self.out.add(k, key, || Finding {
                kind: k,
                severity: k.severity(),
                page,
                object: at.owner,
                message: if white {
                    if disappears {
                        format!("A white (zero-tint) {} in {label} is set to overprint (OPM {opm}): it disappears on press", kind.id())
                    } else {
                        format!(
                            "A white {} in {label} is set to overprint (OPM {opm}): it knocks out per ISO 32000, but RIPs that apply overprint to it drop it",
                            kind.id()
                        )
                    }
                } else {
                    format!("A {} painted with overprint on (OPM {opm}){}", kind.id(), colour.as_ref().map(|c| format!(" in {}", c.space)).unwrap_or_default())
                },
                paint: Some(kind),
                colour,
                text_size,
                overprint_mode: Some(opm),
                bbox,
                occurrences: 1,
            });
        }

        // Rich black.
        if kind == PaintKind::Image {
            return;
        }
        match paint.cs.cmyk(&paint.comps) {
            Ok(([c, m, y, k], _)) => {
                let (k, cmy) = (k * 100.0, (c + m + y) * 100.0);
                // Small tolerance so 95 % stored as 0.95 is "at" 95 %.
                if k + 1e-6 >= self.options.rich_black_k && cmy + 1e-6 >= self.options.rich_black_cmy && paint.cs.registration(&paint.comps).is_none()
                {
                    let small = text_size.is_some_and(|s| s < self.options.small_text_pt);
                    let kind_id = if small { Kind::RichBlackSmallText } else { Kind::RichBlack };
                    let colour = paint.colour();
                    let total = colour.total().unwrap_or(0.0);
                    let alt = if colour.via_alternate { " (from the spot colour's alternate, if converted to process)" } else { "" };
                    let key = format!("{page}|{:?}|{}|{colour_key}|{size_key}", at.owner, kind.id());
                    let cmyk = colour.cmyk.unwrap_or_default();
                    let message = if small {
                        format!(
                            "{:.1} pt text in rich black C{:.0} M{:.0} Y{:.0} K{:.0} ({total:.0}% ink){alt}: small text on four plates is a registration risk (below {} pt)",
                            text_size.unwrap_or(0.0),
                            cmyk[0],
                            cmyk[1],
                            cmyk[2],
                            cmyk[3],
                            self.options.small_text_pt
                        )
                    } else {
                        format!(
                            "A {} in rich black C{:.0} M{:.0} Y{:.0} K{:.0} ({total:.0}% ink){alt}",
                            kind.id(),
                            cmyk[0],
                            cmyk[1],
                            cmyk[2],
                            cmyk[3]
                        )
                    };
                    self.out.add(kind_id, key, || Finding {
                        kind: kind_id,
                        severity: kind_id.severity(),
                        page,
                        object: at.owner,
                        message,
                        paint: Some(kind),
                        colour: Some(colour),
                        text_size,
                        overprint_mode: None,
                        bbox,
                        occurrences: 1,
                    });
                }
            }
            Err(why) => self.unresolved(why),
        }
    }

    /// Interpret a coloured tiling pattern's content. Returns whether the pattern was handled
    /// (`false` for an uncoloured pattern, whose colour the caller classifies).
    fn pattern(&mut self, pattern: Object, gs: &Gs, at: At) -> bool {
        let reference = pattern.as_ref();
        if reference.is_some_and(|r| self.seen.contains(&r)) {
            return true;
        }
        let object = self.doc.resolve(&pattern);
        let Object::Stream(s) = object.as_ref() else {
            if object.as_dict().is_some_and(|d| d.get(b"Shading").is_some()) {
                self.unresolved("shading patterns");
            }
            return true;
        };
        if s.dict.get(b"PatternType").and_then(|p| self.doc.resolve(p).as_int()) != Some(1) {
            return true;
        }
        if s.dict.get(b"PaintType").and_then(|p| self.doc.resolve(p).as_int()) == Some(2) {
            return false;
        }
        let depth = self.depth.saturating_add(1);
        if depth >= MAX_DEPTH {
            self.limit(format!("Forms and patterns nested more than {MAX_DEPTH} deep were not checked"));
            return true;
        }
        let matrix =
            s.dict.get(b"Matrix").map(|m| self.doc.resolve(m)).and_then(|o| o.as_array().and_then(|a| Matrix::from_operands(a))).unwrap_or_default();
        if !finite(&matrix) {
            return true;
        }
        let Some(data) = self.decode(s) else { return true };
        let own = s.dict.get(b"Resources").map(|o| self.doc.resolve(o));
        let empty = Dict::new();
        let res = own.as_ref().and_then(|o| o.as_dict()).unwrap_or(&empty);
        let mut inner = Gs::new(matrix);
        (inner.op_fill, inner.op_stroke, inner.opm, inner.op_source) = (gs.op_fill, gs.op_stroke, gs.opm, gs.op_source);
        if let Some(r) = reference {
            self.seen.insert(r);
        }
        self.run(&data, res, inner, At { owner: reference.or(at.owner), mark: at.mark }, depth);
        if let Some(r) = reference {
            self.seen.remove(&r);
        }
        true
    }

    fn ext_gstate(&mut self, name: &[u8], res: &Dict, gs: &mut Gs, at: At) {
        let Some(entry) = resource(self.doc, res, b"ExtGState", name) else { return };
        let reference = entry.as_ref();
        let d = self.doc.resolve(&entry);
        let Some(d) = d.as_dict() else { return };
        let flag = |k: &[u8]| d.get(k).map(|v| matches!(self.doc.resolve(v).as_ref(), Object::Bool(true)));
        let (stroke, fill) = (flag(b"OP"), flag(b"op"));
        if let Some(s) = stroke {
            gs.op_stroke = s;
            // Without `op`, `OP` sets both.
            if fill.is_none() {
                gs.op_fill = s;
            }
        }
        if let Some(f) = fill {
            gs.op_fill = f;
        }
        let opm = d.get(b"OPM").and_then(|v| self.doc.resolve(v).as_int());
        if let Some(m) = opm {
            gs.opm = if m == 1 { 1 } else { 0 };
        }
        if let Some(size) = d.get(b"Font").map(|f| self.doc.resolve(f)).and_then(|f| f.as_array().and_then(|a| a.get(1).and_then(Object::as_f64)))
            && size.is_finite()
        {
            gs.size = size.clamp(-1e5, 1e5);
        }
        let sets_overprint = stroke == Some(true) || fill == Some(true);
        if sets_overprint {
            gs.op_source = reference;
        }
        if sets_overprint || opm == Some(1) {
            let page = self.page;
            let object = reference.or(at.owner);
            let what: Vec<&str> = [
                (fill == Some(true) || (fill.is_none() && stroke == Some(true))).then_some("fills (op)"),
                (stroke == Some(true)).then_some("strokes (OP)"),
                (opm == Some(1)).then_some("overprint mode 1 (OPM)"),
            ]
            .into_iter()
            .flatten()
            .collect();
            let label = name_text(name);
            self.out.add(Kind::OverprintState, format!("{page}|{object:?}|{label}"), || Finding {
                kind: Kind::OverprintState,
                severity: Kind::OverprintState.severity(),
                page,
                object,
                message: format!("Graphics state /{label} sets overprint for {}", what.join(", ")),
                paint: None,
                colour: None,
                text_size: None,
                overprint_mode: opm,
                bbox: None,
                occurrences: 1,
            });
        }
    }

    fn xobject(&mut self, name: &[u8], res: &Dict, gs: &Gs, at: At, depth: usize) {
        let Some(entry) = resource(self.doc, res, b"XObject", name) else { return };
        let reference = entry.as_ref();
        if reference.is_some_and(|r| self.seen.contains(&r)) {
            return;
        }
        let object = self.doc.resolve(&entry);
        let Object::Stream(s) = object.as_ref() else { return };
        let unit = gs.ctm.bbox([0.0, 0.0, 1.0, 1.0]);
        match s.dict.name(b"Subtype") {
            Some(b"Image") => {
                let mask = s.dict.get(b"ImageMask").is_some_and(|o| matches!(self.doc.resolve(o).as_ref(), Object::Bool(true)));
                if mask {
                    self.paint(gs, PaintKind::Fill, Some(unit), at, None);
                } else {
                    self.unresolved("images (pixels are not checked)");
                    if gs.op_fill {
                        self.paint(gs, PaintKind::Image, Some(unit), at, None);
                    }
                }
            }
            Some(b"Form") => {
                if depth + 1 >= MAX_DEPTH {
                    self.limit(format!("Forms and patterns nested more than {MAX_DEPTH} deep were not checked"));
                    return;
                }
                let matrix = s
                    .dict
                    .get(b"Matrix")
                    .map(|m| self.doc.resolve(m))
                    .and_then(|o| o.as_array().and_then(|a| Matrix::from_operands(a)))
                    .unwrap_or_default();
                let ctm = matrix.then(&gs.ctm);
                if !finite(&ctm) {
                    return;
                }
                let Some(data) = self.decode(s) else { return };
                let own = s.dict.get(b"Resources").map(|o| self.doc.resolve(o));
                let inner_res = own.as_ref().and_then(|o| o.as_dict()).unwrap_or(res);
                let mut inner = gs.clone();
                inner.ctm = ctm;
                if let Some(r) = reference {
                    self.seen.insert(r);
                }
                self.run(&data, inner_res, inner, At { owner: reference.or(at.owner), mark: at.mark }, depth + 1);
                if let Some(r) = reference {
                    self.seen.remove(&r);
                }
            }
            _ => {}
        }
    }

    fn run(&mut self, data: &[u8], res: &Dict, initial: Gs, at: At, depth: usize) {
        let mut gs = initial;
        let mut stack: Vec<Gs> = Vec::new();
        let mut marked: Vec<bool> = Vec::new();
        let mut path: Option<[f64; 4]> = None;
        let mut tm = Matrix::IDENTITY;
        let mut tlm = Matrix::IDENTITY;
        let mut spaces: HashMap<Vec<u8>, Rc<Cs>> = HashMap::new();
        for op in parse(data).ops {
            self.ops = self.ops.saturating_add(1);
            if self.ops > MAX_OPERATORS {
                self.limit(format!("Only the first {MAX_OPERATORS} content operators were checked"));
                self.stopped = true;
            }
            if self.stopped {
                return;
            }
            self.depth = depth;
            let here = At { owner: at.owner, mark: at.mark || marked.iter().any(|m| *m) };
            let point = |gs: &Gs, x: f64, y: f64| {
                let (x, y) = gs.ctm.apply(x, y);
                (x.is_finite() && y.is_finite()).then_some([x, y, x, y])
            };
            match op.op.as_slice() {
                b"q" => {
                    if stack.len() >= MAX_STACK {
                        self.limit(format!("Graphics states nested more than {MAX_STACK} deep were not checked"));
                        return;
                    }
                    stack.push(gs.clone());
                }
                b"Q" => {
                    if let Some(g) = stack.pop() {
                        gs = g;
                    }
                }
                b"cm" => {
                    if let Some(m) = Matrix::from_operands(&op.operands) {
                        let next = m.then(&gs.ctm);
                        if finite(&next) {
                            gs.ctm = next;
                        }
                    }
                }
                b"gs" => {
                    if let Some(name) = op.name(0) {
                        self.ext_gstate(name, res, &mut gs, here);
                    }
                }
                b"m" | b"l" => {
                    if let Some([x, y]) = op.nums::<2>() {
                        path = union(path, point(&gs, x, y));
                    }
                }
                b"c" => {
                    if let Some(v) = op.nums::<6>() {
                        for p in v.chunks(2) {
                            if let [x, y] = p {
                                path = union(path, point(&gs, *x, *y));
                            }
                        }
                    }
                }
                b"v" | b"y" => {
                    if let Some(v) = op.nums::<4>() {
                        for p in v.chunks(2) {
                            if let [x, y] = p {
                                path = union(path, point(&gs, *x, *y));
                            }
                        }
                    }
                }
                b"re" => {
                    if let Some([x, y, w, h]) = op.nums::<4>() {
                        path = union(path, point(&gs, x, y));
                        path = union(path, point(&gs, x + w, y + h));
                        path = union(path, point(&gs, x + w, y));
                        path = union(path, point(&gs, x, y + h));
                    }
                }
                b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"S" | b"s" | b"n" => {
                    let name = op.op.as_slice();
                    if path.is_some() {
                        if matches!(name, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*") {
                            self.paint(&gs, PaintKind::Fill, path, here, None);
                        }
                        if matches!(name, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*") {
                            self.paint(&gs, PaintKind::Stroke, path, here, None);
                        }
                    }
                    path = None;
                }
                b"g" | b"G" | b"rg" | b"RG" | b"k" | b"K" => {
                    let (cs, n) = match op.op.as_slice() {
                        b"g" | b"G" => (Cs::Gray, 1),
                        b"rg" | b"RG" => (Cs::Rgb, 3),
                        _ => (Cs::Cmyk, 4),
                    };
                    let comps: Vec<f64> = op.operands.iter().take(n).map(|o| o.as_f64().unwrap_or(0.0)).collect();
                    let paint = Paint::new(cs, comps);
                    if op.op.first().is_some_and(u8::is_ascii_lowercase) {
                        gs.fill = paint;
                    } else {
                        gs.stroke = paint;
                    }
                }
                b"cs" | b"CS" => {
                    if let Some(o) = op.operands.first() {
                        let cs = match o.as_name() {
                            Some(n) => match spaces.get(n) {
                                Some(cs) => cs.clone(),
                                None => {
                                    let cs = Rc::new(self.colour_space(o, res, 0));
                                    if spaces.len() < 1024 {
                                        spaces.insert(n.to_vec(), cs.clone());
                                    }
                                    cs
                                }
                            },
                            None => Rc::new(self.colour_space(o, res, 0)),
                        };
                        let comps = cs.initial();
                        let paint = Paint { cs, comps, pattern: None };
                        if op.is("cs") {
                            gs.fill = paint;
                        } else {
                            gs.stroke = paint;
                        }
                    }
                }
                b"sc" | b"scn" | b"SC" | b"SCN" => {
                    let target = if op.op.first() == Some(&b's') { &mut gs.fill } else { &mut gs.stroke };
                    let n = target.cs.components().min(MAX_COMPONENTS);
                    target.comps = op.operands.iter().filter_map(Object::as_f64).take(n).collect();
                    target.pattern = match op.operands.last().and_then(Object::as_name) {
                        Some(name) if matches!(*target.cs, Cs::Pattern(_)) => resource(self.doc, res, b"Pattern", name),
                        _ => None,
                    };
                }
                b"BT" => {
                    tm = Matrix::IDENTITY;
                    tlm = Matrix::IDENTITY;
                }
                b"Tf" => {
                    if let Some(size) = op.num(1).filter(|s| s.is_finite()) {
                        gs.size = size.clamp(-1e5, 1e5);
                    }
                }
                b"Tr" => gs.render = op.operands.first().and_then(Object::as_int).unwrap_or(0),
                b"Ts" => gs.rise = op.num(0).filter(|v| v.is_finite()).unwrap_or(0.0).clamp(-1e5, 1e5),
                b"TL" => gs.leading = op.num(0).filter(|v| v.is_finite()).unwrap_or(0.0).clamp(-1e5, 1e5),
                b"Td" | b"TD" => {
                    if let Some([x, y]) = op.nums::<2>() {
                        if op.is("TD") {
                            gs.leading = -y;
                        }
                        let next = Matrix::translate(x, y).then(&tlm);
                        if finite(&next) {
                            tlm = next;
                            tm = tlm;
                        }
                    }
                }
                b"Tm" => {
                    if let Some(m) = Matrix::from_operands(&op.operands).filter(finite) {
                        tlm = m;
                        tm = m;
                    }
                }
                b"T*" => {
                    tlm = Matrix::translate(0.0, -gs.leading).then(&tlm);
                    tm = tlm;
                }
                b"Tj" | b"TJ" | b"'" | b"\"" => {
                    if op.is("'") || op.is("\"") {
                        tlm = Matrix::translate(0.0, -gs.leading).then(&tlm);
                        tm = tlm;
                    }
                    let strings: Vec<&Object> = match op.op.as_slice() {
                        b"TJ" => op.operands.first().and_then(Object::as_array).map(|a| a.iter().collect()).unwrap_or_default(),
                        b"\"" => op.operands.get(2).into_iter().collect(),
                        _ => op.operands.first().into_iter().collect(),
                    };
                    let shown = strings.iter().any(|s| matches!(s, Object::String(s) if !s.bytes.is_empty()));
                    if !shown {
                        continue;
                    }
                    let trm = Matrix::translate(0.0, gs.rise).then(&tm).then(&gs.ctm);
                    let [_, _, c, d, x, y] = trm.0;
                    let size = (gs.size * c.hypot(d)).abs();
                    let size = if size.is_finite() { round(size) } else { 0.0 };
                    let origin = (x.is_finite() && y.is_finite()).then_some([x, y, x, y]);
                    let (fill, stroke) = match gs.render.rem_euclid(8) {
                        0 | 4 => (true, false),
                        1 | 5 => (false, true),
                        2 | 6 => (true, true),
                        _ => (false, false),
                    };
                    if fill {
                        self.paint(&gs, PaintKind::Text, origin, here, Some(size));
                    }
                    if stroke {
                        self.paint(&gs, PaintKind::TextStroke, origin, here, Some(size));
                    }
                }
                b"Do" => {
                    if let Some(name) = op.name(0) {
                        self.xobject(name, res, &gs, here, depth);
                    }
                }
                b"BI" => {
                    if let Some((dict, _)) = &op.inline {
                        let mask = dict.get(b"IM").or_else(|| dict.get(b"ImageMask")).is_some_and(|o| matches!(o, Object::Bool(true)));
                        let unit = gs.ctm.bbox([0.0, 0.0, 1.0, 1.0]);
                        if mask {
                            self.paint(&gs, PaintKind::Fill, Some(unit), here, None);
                        } else {
                            self.unresolved("images (pixels are not checked)");
                            if gs.op_fill {
                                self.paint(&gs, PaintKind::Image, Some(unit), here, None);
                            }
                        }
                    }
                }
                b"sh" => self.unresolved("shadings"),
                b"BMC" => {
                    if marked.len() >= MAX_MARKED {
                        self.limit(format!("Marked content nested more than {MAX_MARKED} deep was not checked"));
                        return;
                    }
                    marked.push(false);
                }
                b"BDC" => {
                    if marked.len() >= MAX_MARKED {
                        self.limit(format!("Marked content nested more than {MAX_MARKED} deep was not checked"));
                        return;
                    }
                    let mark = self.marks(&op, res);
                    marked.push(mark);
                }
                b"EMC" => {
                    marked.pop();
                }
                _ => {}
            }
        }
    }
}
