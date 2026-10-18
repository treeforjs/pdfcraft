//! Convert Colors (Acrobat: Print Production ▸ Convert Colors): rewrite the colours of chosen
//! pages from chosen source spaces into one destination space — DeviceCMYK, DeviceGray,
//! DeviceRGB, or an ICC profile the caller supplies.
//!
//! **What is converted.** Fill and stroke colours of page content, the Form XObjects it draws
//! (each once, however many pages share it), coloured tiling patterns, shadings (their colour
//! functions resampled, their backgrounds), image XObjects and inline images (pixels re-encoded;
//! Indexed images get a new palette), and annotation appearances. The object types to touch are
//! a choice (text, vector art, images); colours are tracked through the graphics state, so text
//! can be converted while the vector art drawn with the same colour is not.
//!
//! **How content is rewritten.** Colour operators whose space is converted are taken out, and
//! the colour each painting operator needs is set just before it (before the path it paints, so
//! no operator lands inside a path object): converted when that object type is chosen, as it
//! was otherwise. Everything else is copied byte for byte. Colours in other spaces keep their
//! operators. Pages and forms that change get new streams; their resources gain the colour spaces
//! the new operators name (the destination profile, converted Indexed palettes).
//!
//! **The colour arithmetic.** With an ICC destination its `BToA` table for the rendering intent
//! (lut8/lut16) or its gray/RGB matrix-TRC is used, from CIE XYZ: DeviceRGB and 3-component
//! ICC colours are read as sRGB, CalRGB/CalGray through their gamma and matrix, Lab through its
//! white point. Without one (Device destinations, or a v4-only profile) a documented built-in
//! approximation is used: RGB → CMYK with full black generation and 70% under-colour removal,
//! CMYK → RGB naively, gray by luma. Gray always goes to the black plate (K only) and CMYK
//! numbers carry over between CMYK spaces. Preserve black turns pure black (RGB 0 0 0, gray 0,
//! K-only 100%, Lab L* 0) into the destination's pure black (100% K for CMYK). Spot colours
//! (Separation, DeviceN) stay unless they are to be converted to process, through their tint
//! transforms (Types 0, 2, 3 and 4). `/All` (registration) and `/None` are never converted.
//!
//! Every input-derived size is capped and the object graph is walked with seen-sets and depth
//! limits. Images too large, in codecs PdfCraft can't decode (JPX, JBIG2, CCITT, CMYK JPEG) or
//! with colour-key masks are left as they were and listed in the report.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use pdfcraft_content::{Op, Pieces, num};
use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::text::{content_streams, rewritten_contents, splice};
use crate::{EditError, check, page_list};

mod function;
pub mod icc;
mod space;
#[cfg(test)]
mod tests;

pub use function::Function;
use function::{Func, sampled_stream};
pub use icc::{Family, Profile};
use space::{Dest, Space};

/// What a colour in colour space `cs` (a family name, an array, or the name of a `/ColorSpace`
/// resource of `res`) is made of: each component's valid range (Indexed: 0 to `hival`; Lab: its
/// `/Range`). `None` for patterns and spaces that can't be read.
pub(crate) fn color_ranges(doc: &Document, cs: &Object, res: &Dict) -> Option<Vec<[f64; 2]>> {
    let space = Space::parse(doc, cs, res, 0, None);
    match &space {
        Space::Pattern(_) | Space::Unknown(_) => None,
        Space::Indexed { hival, .. } => Some(vec![[0.0, *hival as f64]]),
        _ => Some(space.ranges()),
    }
}

/// Forms drawn inside forms, at most this deep.
const MAX_DEPTH: usize = 16;
const MAX_STACK: usize = 256;
/// One content stream, and all content together, decoded at most.
const MAX_STREAM: usize = 64 << 20;
const MAX_TOTAL: usize = 1 << 30;
/// Images: pixels and decoded bytes at most (larger ones are left as they are).
pub const MAX_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_IMAGE_BYTES: usize = 512 << 20;
const MAX_INLINE_PIXELS: u64 = 1 << 20;
/// Distinct colours remembered per image while converting its pixels.
const MAX_CACHE: usize = 1 << 16;
/// Reasons listed in the report at most.
const MAX_SKIPPED: usize = 64;
/// Shading functions are resampled at this many points (two-input functions per side).
const SHADING_SAMPLES: usize = 256;
const SHADING_GRID: usize = 33;

/// The colour spaces to convert from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSpaces {
    pub device_gray: bool,
    pub device_rgb: bool,
    pub device_cmyk: bool,
    pub icc_based: bool,
    pub cal_gray: bool,
    pub cal_rgb: bool,
    pub lab: bool,
}

impl SourceSpaces {
    /// The names [`SourceSpaces::set`] takes.
    pub const NAMES: [&'static str; 7] = ["DeviceGray", "DeviceRGB", "DeviceCMYK", "ICCBased", "CalGray", "CalRGB", "Lab"];

    pub const ALL: SourceSpaces =
        SourceSpaces { device_gray: true, device_rgb: true, device_cmyk: true, icc_based: true, cal_gray: true, cal_rgb: true, lab: true };
    pub const NONE: SourceSpaces =
        SourceSpaces { device_gray: false, device_rgb: false, device_cmyk: false, icc_based: false, cal_gray: false, cal_rgb: false, lab: false };

    /// Turn on the space called `name` (one of [`SourceSpaces::NAMES`]); false if unknown.
    pub fn set(&mut self, name: &str) -> bool {
        let slot = match name {
            "DeviceGray" => &mut self.device_gray,
            "DeviceRGB" => &mut self.device_rgb,
            "DeviceCMYK" => &mut self.device_cmyk,
            "ICCBased" => &mut self.icc_based,
            "CalGray" => &mut self.cal_gray,
            "CalRGB" => &mut self.cal_rgb,
            "Lab" => &mut self.lab,
            _ => return false,
        };
        *slot = true;
        true
    }

    pub fn names(&self) -> Vec<&'static str> {
        let on = [self.device_gray, self.device_rgb, self.device_cmyk, self.icc_based, self.cal_gray, self.cal_rgb, self.lab];
        Self::NAMES.iter().zip(on).filter(|(_, on)| *on).map(|(n, _)| *n).collect()
    }

    fn any(&self) -> bool {
        *self != Self::NONE
    }
}

impl Default for SourceSpaces {
    fn default() -> Self {
        Self::ALL
    }
}

/// The space colours are converted to.
#[derive(Clone, Debug, PartialEq)]
pub enum Destination {
    DeviceCmyk,
    DeviceGray,
    DeviceRgb,
    /// An ICC profile (its bytes): gray, RGB or CMYK, output, display, input or colour-space class.
    Icc(Arc<Vec<u8>>),
}

impl Destination {
    pub fn name(&self) -> &'static str {
        match self {
            Destination::DeviceCmyk => "DeviceCMYK",
            Destination::DeviceGray => "DeviceGray",
            Destination::DeviceRgb => "DeviceRGB",
            Destination::Icc(_) => "ICCBased",
        }
    }
}

/// The object types whose colours are converted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectKinds {
    pub text: bool,
    /// Paths (fills and strokes), shadings and patterns.
    pub vector: bool,
    /// Image XObjects, inline images and stencil masks.
    pub images: bool,
}

impl ObjectKinds {
    pub const ALL: ObjectKinds = ObjectKinds { text: true, vector: true, images: true };
}

impl Default for ObjectKinds {
    fn default() -> Self {
        Self::ALL
    }
}

/// Rendering intent (ISO 32000-2 §8.6.5.8): which of the destination profile's tables is used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Intent {
    Perceptual,
    #[default]
    RelativeColorimetric,
    Saturation,
    /// Converted as relative colorimetric (no white-point scaling).
    AbsoluteColorimetric,
}

impl Intent {
    pub const NAMES: [&'static str; 4] = ["perceptual", "relative_colorimetric", "saturation", "absolute_colorimetric"];

    pub fn from_name(s: &str) -> Option<Intent> {
        match s {
            "perceptual" => Some(Intent::Perceptual),
            "relative_colorimetric" => Some(Intent::RelativeColorimetric),
            "saturation" => Some(Intent::Saturation),
            "absolute_colorimetric" => Some(Intent::AbsoluteColorimetric),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Intent::Perceptual => "perceptual",
            Intent::RelativeColorimetric => "relative_colorimetric",
            Intent::Saturation => "saturation",
            Intent::AbsoluteColorimetric => "absolute_colorimetric",
        }
    }

    fn icc(self) -> u8 {
        match self {
            Intent::Perceptual => 0,
            Intent::RelativeColorimetric => 1,
            Intent::Saturation => 2,
            Intent::AbsoluteColorimetric => 3,
        }
    }
}

/// Convert Colors settings.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorConversion {
    pub sources: SourceSpaces,
    pub destination: Destination,
    pub objects: ObjectKinds,
    pub intent: Intent,
    /// Pure black becomes the destination's pure black (100% K for CMYK).
    pub preserve_black: bool,
    /// Spot colours (Separation, DeviceN) are converted to the destination too.
    pub convert_spots: bool,
    /// With `convert_spots`, only these spot colorants (Ink Manager ▸ convert to process): a
    /// Separation is converted when its colorant is listed, a DeviceN when every colorant of it
    /// that isn't a process ink or `None` is. `None` converts every spot colour.
    pub spot_names: Option<BTreeSet<Vec<u8>>>,
    /// Make the destination profile the document's PDF/X output intent (ICC destinations).
    pub output_intent: bool,
    /// The output intent's printing condition (default: the profile's description).
    pub output_condition: Option<String>,
    /// With an ICC destination, compute colours through the profile but write them in its
    /// device space (DeviceCMYK, DeviceGray, DeviceRGB) rather than as ICCBased colour: what a
    /// PDF/X output intent stands for (PDF/X-1a allows no ICCBased colour).
    pub device_output: bool,
}

impl Default for ColorConversion {
    fn default() -> Self {
        Self {
            sources: SourceSpaces::ALL,
            destination: Destination::DeviceCmyk,
            objects: ObjectKinds::ALL,
            intent: Intent::default(),
            preserve_black: true,
            convert_spots: false,
            spot_names: None,
            output_intent: false,
            output_condition: None,
            device_output: false,
        }
    }
}

impl ColorConversion {
    /// Check the settings; reads the destination profile.
    pub fn validate(&self) -> Result<Option<Profile>, EditError> {
        if !self.sources.any() && !self.convert_spots {
            return Err(EditError::Invalid("choose at least one source colour space (or convert spot colours)".into()));
        }
        if !(self.objects.text || self.objects.vector || self.objects.images) {
            return Err(EditError::Invalid("choose at least one object type (text, vector, images)".into()));
        }
        if let Some(c) = &self.output_condition
            && (c.chars().count() > 256 || c.chars().any(char::is_control))
        {
            return Err(EditError::Invalid("the output condition is one line of at most 256 characters".into()));
        }
        match &self.destination {
            Destination::Icc(bytes) => {
                Profile::parse(bytes, self.intent.icc()).map(Some).map_err(|e| EditError::Invalid(format!("destination profile: {e}")))
            }
            _ if self.output_intent => Err(EditError::Invalid("an output intent needs an ICC destination profile".into())),
            _ => Ok(None),
        }
    }
}

/// What a conversion changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConversionReport {
    /// Pages whose content (or annotations) changed, 0-based.
    pub pages: Vec<usize>,
    /// Colours set in the destination space (operators written).
    pub colors: usize,
    pub images: usize,
    pub inline_images: usize,
    pub shadings: usize,
    pub forms: usize,
    pub patterns: usize,
    pub annotations: usize,
    /// Colour numbers came from the built-in approximation, not an ICC profile.
    pub approximate: bool,
    pub output_intent: bool,
    /// What was left as it was, and why.
    pub skipped: Vec<String>,
}

impl ConversionReport {
    /// Whether anything changed.
    pub fn changed(&self) -> bool {
        self.colors + self.images + self.inline_images + self.shadings + self.forms + self.patterns + self.annotations > 0
            || !self.pages.is_empty()
            || self.output_intent
    }
}

/// Convert colours on `pages` (0-based). On error the document may be half converted: callers
/// run this on a copy (the engine does).
pub fn convert_colors(doc: &mut Document, pages: &[usize], settings: &ColorConversion) -> Result<ConversionReport, EditError> {
    let profile = settings.validate()?;
    let all = page_list(doc);
    check(pages, all.len())?;
    let family = match (&settings.destination, &profile) {
        (Destination::DeviceCmyk, _) => Family::Cmyk,
        (Destination::DeviceGray, _) => Family::Gray,
        (Destination::DeviceRgb, _) => Family::Rgb,
        (Destination::Icc(_), Some(p)) => p.family,
        (Destination::Icc(_), None) => return Err(EditError::Invalid("destination profile could not be read".into())),
    };
    let profile_bytes = match &settings.destination {
        Destination::Icc(b) => Some(b.clone()),
        _ => None,
    };
    let approximate = profile.as_ref().is_none_or(|p| !p.exact());
    let mut cx = Cx {
        s: settings,
        dest: Dest { family, profile, preserve_black: settings.preserve_black },
        profile_bytes,
        icc: None,
        forms: HashSet::new(),
        images: HashSet::new(),
        shadings: HashSet::new(),
        patterns: HashSet::new(),
        report: ConversionReport { approximate, ..Default::default() },
        skipped: BTreeSet::new(),
        bytes: 0,
        stopped: false,
        changed: false,
    };
    let mut done = HashSet::new();
    for &i in pages {
        if !done.insert(i) {
            continue;
        }
        let Some(page) = all.get(i) else { continue };
        cx.changed = false;
        cx.page(doc, page)?;
        if cx.changed {
            cx.report.pages.push(i);
        }
    }
    if settings.output_intent {
        let icc = cx.icc_ref(doc);
        let condition = settings
            .output_condition
            .clone()
            .or_else(|| cx.dest.profile.as_ref().map(|p| p.description.clone()).filter(|d| !d.is_empty()))
            .unwrap_or_else(|| "Custom".into());
        add_output_intent(doc, icc, &condition)?;
        cx.report.output_intent = true;
    }
    cx.report.pages.sort_unstable();
    cx.report.skipped = cx.skipped.into_iter().take(MAX_SKIPPED).map(String::from).collect();
    Ok(cx.report)
}

/// The object type a painting operator draws.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Vector,
    Image,
    /// A form drawn with the current colour: its content may use it for anything.
    Form,
}

/// How a colour was set, to set it again as it was.
#[derive(Clone, Debug, PartialEq)]
enum Form {
    /// `g`, `rg` or `k` (lower case).
    Device(&'static str),
    /// `<operand> cs`, then `scn`.
    Named(Object),
    /// A form's colour comes from whoever draws it.
    Inherited,
}

#[derive(Clone, Debug)]
struct Colour {
    form: Form,
    space: Rc<Space>,
    comps: Vec<f64>,
    pattern: Option<Vec<u8>>,
}

impl PartialEq for Colour {
    fn eq(&self, o: &Self) -> bool {
        self.form == o.form && Rc::ptr_eq(&self.space, &o.space) && self.comps == o.comps && self.pattern == o.pattern
    }
}

#[derive(Clone, Debug)]
struct Channel {
    cur: Colour,
    /// What the rewritten stream has set: (converted, colour); `None` when unknown.
    emitted: Option<(bool, Colour)>,
}

#[derive(Clone, Debug)]
struct Gs {
    fill: Channel,
    stroke: Channel,
    render: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Init {
    /// The default graphics state: DeviceGray black (pages, patterns, appearances).
    Default,
    /// A form's: colours come from its caller.
    Inherited,
}

/// The resources of one stream being rewritten, and the colour spaces it adds.
struct Res {
    dict: Dict,
    adds: Vec<(Vec<u8>, Object)>,
    names: HashMap<String, Vec<u8>>,
    spaces: HashMap<Vec<u8>, Rc<Space>>,
    gray: Rc<Space>,
    rgb: Rc<Space>,
    cmyk: Rc<Space>,
    inherited: Rc<Space>,
}

impl Res {
    fn new(dict: Dict) -> Self {
        Res {
            dict,
            adds: Vec::new(),
            names: HashMap::new(),
            spaces: HashMap::new(),
            gray: Rc::new(Space::Gray),
            rgb: Rc::new(Space::Rgb),
            cmyk: Rc::new(Space::Cmyk),
            inherited: Rc::new(Space::Unknown(0)),
        }
    }

    fn colour_spaces(&self, doc: &Document) -> Dict {
        self.dict.get(b"ColorSpace").map(|c| doc.resolve(c)).and_then(|c| c.as_dict().cloned()).unwrap_or_default()
    }

    /// A resource name for colour space `obj` (`key` identifies it within this stream).
    fn name(&mut self, doc: &Document, key: String, obj: Object) -> Vec<u8> {
        if let Some(n) = self.names.get(&key) {
            return n.clone();
        }
        let existing = self.colour_spaces(doc);
        let name = match existing.iter().find(|(_, v)| **v == obj) {
            Some((n, _)) => n.clone(),
            None => {
                let mut i = 0usize;
                loop {
                    let n = format!("PCcs{i}").into_bytes();
                    if !existing.contains(&n) && !self.adds.iter().any(|(a, _)| *a == n) {
                        break n;
                    }
                    i += 1;
                }
            }
        };
        if !existing.contains(&name) {
            self.adds.push((name.clone(), obj));
        }
        self.names.insert(key, name.clone());
        name
    }
}

struct Cx<'a> {
    s: &'a ColorConversion,
    dest: Dest,
    profile_bytes: Option<Arc<Vec<u8>>>,
    icc: Option<ObjRef>,
    forms: HashSet<ObjRef>,
    images: HashSet<ObjRef>,
    shadings: HashSet<ObjRef>,
    patterns: HashSet<ObjRef>,
    report: ConversionReport,
    skipped: BTreeSet<&'static str>,
    bytes: usize,
    stopped: bool,
    /// Whether the page being converted changed.
    changed: bool,
}

fn op_name(op: &Op) -> &str {
    std::str::from_utf8(&op.op).unwrap_or("")
}

fn hex(data: &[u8]) -> Vec<u8> {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = Vec::with_capacity(data.len().saturating_mul(2).saturating_add(data.len() / 32 + 2));
    for (i, b) in data.iter().enumerate() {
        if i > 0 && i % 32 == 0 {
            out.push(b'\n');
        }
        out.push(DIGITS[usize::from(b >> 4)]);
        out.push(DIGITS[usize::from(b & 15)]);
    }
    out.push(b'>');
    out
}

fn to_byte(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

impl Cx<'_> {
    fn skip(&mut self, why: &'static str) {
        self.skipped.insert(why);
    }

    fn selected(&self, kind: Kind) -> bool {
        let o = &self.s.objects;
        match kind {
            Kind::Text => o.text,
            Kind::Vector => o.vector,
            Kind::Image => o.images,
            Kind::Form => o.text || o.vector,
        }
    }

    /// Whether converted colours are written as ICCBased colour in the destination profile.
    fn icc_dest(&self) -> bool {
        self.profile_bytes.is_some() && !self.s.device_output
    }

    /// Whether colours in `sp` are converted.
    fn managed(&self, sp: &Space) -> bool {
        let s = &self.s.sources;
        let same = |f: Family| self.dest.family == f && !self.icc_dest();
        match sp {
            Space::Gray => s.device_gray && !same(Family::Gray),
            Space::Rgb => s.device_rgb && !same(Family::Rgb),
            Space::Cmyk => s.device_cmyk && !same(Family::Cmyk),
            Space::Icc { dest, .. } => s.icc_based && !dest,
            Space::CalGray { .. } => s.cal_gray,
            Space::CalRgb { .. } => s.cal_rgb,
            Space::Lab { .. } => s.lab,
            Space::Separation { .. } | Space::DeviceN { .. } => self.s.convert_spots && self.spot_chosen(sp) && sp.spot_convertible(),
            Space::Indexed { base, .. } => self.managed(base),
            Space::Pattern(Some(base)) => self.managed(base),
            Space::Pattern(None) | Space::Unknown(_) => false,
        }
    }

    /// Whether spot space `sp` is among the spot colours chosen for conversion.
    fn spot_chosen(&self, sp: &Space) -> bool {
        let Some(chosen) = &self.s.spot_names else { return true };
        const PROCESS: [&[u8]; 5] = [b"Cyan", b"Magenta", b"Yellow", b"Black", b"None"];
        match sp {
            Space::Separation { name, .. } => chosen.contains(name),
            Space::DeviceN { names, .. } => {
                let mut spots = names.iter().filter(|n| !PROCESS.contains(&n.as_slice())).peekable();
                spots.peek().is_some() && spots.all(|n| chosen.contains(n))
            }
            _ => false,
        }
    }

    /// Note spot colours that were to be converted but can't be.
    fn note(&mut self, sp: &Space) {
        if self.s.convert_spots
            && self.s.spot_names.is_some()
            && let Space::DeviceN { names, .. } = sp
            && !self.spot_chosen(sp)
            && names.iter().any(|n| self.s.spot_names.as_ref().is_some_and(|c| c.contains(n)))
        {
            self.skip("DeviceN colours mixing spot inks converted to process with inks kept (left as they were)");
        }
        if self.s.convert_spots && sp.is_spot() && self.spot_chosen(sp) && !sp.spot_convertible() {
            let registration = match sp {
                Space::Separation { name, .. } => name == b"All" || name == b"None",
                Space::DeviceN { names, .. } => names.iter().any(|n| n == b"All"),
                _ => false,
            };
            if !registration {
                self.skip("spot colours whose alternate space or tint transform can't be evaluated");
            }
        }
    }

    /// The destination profile's stream, added once.
    fn icc_ref(&mut self, doc: &mut Document) -> ObjRef {
        if let Some(r) = self.icc {
            return r;
        }
        let bytes = self.profile_bytes.clone().unwrap_or_default();
        let mut d = Dict::new();
        d.set(b"N".to_vec(), Object::Int(self.dest.family.components() as i64));
        d.set(b"Alternate".to_vec(), Object::name(self.dest.family.device_name()));
        let r = doc.add(Object::Stream(Stream::flate(d, &bytes)));
        self.icc = Some(r);
        r
    }

    /// The destination colour space as an object.
    fn dest_space(&mut self, doc: &mut Document) -> Object {
        if self.icc_dest() {
            Object::Array(vec![Object::name("ICCBased"), Object::Ref(self.icc_ref(doc))])
        } else {
            Object::name(self.dest.family.device_name())
        }
    }

    fn convert(&self, sp: &Space, comps: &[f64]) -> Option<Vec<f64>> {
        sp.value(comps).map(|v| self.dest.convert(v))
    }

    /// A converted Indexed palette.
    fn palette(&self, base: &Space, hival: usize, lookup: &[u8]) -> Option<Vec<u8>> {
        let n = base.components();
        let ranges = base.ranges();
        let mut out = Vec::with_capacity((hival + 1).saturating_mul(self.dest.family.components()));
        for i in 0..=hival {
            let start = i.checked_mul(n)?;
            let entry = lookup.get(start..start.checked_add(n)?)?;
            let comps: Vec<f64> = entry
                .iter()
                .enumerate()
                .map(|(j, b)| {
                    let r = ranges.get(j).copied().unwrap_or([0.0, 1.0]);
                    r[0] + f64::from(*b) / 255.0 * (r[1] - r[0])
                })
                .collect();
            out.extend(self.convert(base, &comps)?.into_iter().map(to_byte));
        }
        Some(out)
    }

    fn indexed_space(&mut self, doc: &mut Document, sp: &Space) -> Option<Object> {
        let Space::Indexed { base, hival, lookup } = sp else { return None };
        let lut = self.palette(base, *hival, lookup)?;
        let dest = self.dest_space(doc);
        Some(Object::Array(vec![Object::name("Indexed"), dest, Object::Int(*hival as i64), Object::String(PdfString { bytes: lut, hex: true })]))
    }

    fn space_of(&self, doc: &Document, res: &mut Res, operand: &Object) -> Rc<Space> {
        let key = match operand {
            Object::Name(n) => n.clone(),
            _ => return Rc::new(Space::Unknown(1)),
        };
        match key.as_slice() {
            b"DeviceGray" => return res.gray.clone(),
            b"DeviceRGB" => return res.rgb.clone(),
            b"DeviceCMYK" => return res.cmyk.clone(),
            _ => {}
        }
        if let Some(s) = res.spaces.get(&key) {
            return s.clone();
        }
        let sp = Rc::new(Space::parse(doc, operand, &res.dict, 0, self.profile_bytes.as_deref().map(Vec::as_slice)));
        if res.spaces.len() < 4096 {
            res.spaces.insert(key, sp.clone());
        }
        sp
    }

    /// Operators that set `c` (converted or as it was) for fill or `stroke`.
    fn emit(&mut self, doc: &mut Document, res: &mut Res, c: &Colour, converted: bool, stroke: bool, out: &mut Vec<Op>) {
        let case = |s: &str| if stroke { s.to_uppercase() } else { s.to_string() };
        let nums = |v: &[f64]| v.iter().map(|x| num((x * 1e5).round() / 1e5)).collect::<Vec<_>>();
        let set = |out: &mut Vec<Op>, cs: Object, mut comps: Vec<Object>, pattern: Option<&Vec<u8>>| {
            out.push(Op::new(&case("cs"), vec![cs]));
            if let Some(p) = pattern {
                comps.push(Object::Name(p.clone()));
            }
            if !comps.is_empty() {
                out.push(Op::new(&case("scn"), comps));
            }
        };
        if !converted {
            match &c.form {
                Form::Device(op) => out.push(Op::new(&case(op), nums(&c.comps))),
                Form::Named(cs) => set(out, cs.clone(), nums(&c.comps), c.pattern.as_ref()),
                Form::Inherited => {}
            }
            return;
        }
        let key = match &c.form {
            Form::Named(Object::Name(n)) => String::from_utf8_lossy(n).into_owned(),
            _ => String::new(),
        };
        match &*c.space {
            Space::Indexed { .. } => {
                let Some(obj) = self.indexed_space(doc, &c.space) else {
                    return self.emit(doc, res, c, false, stroke, out);
                };
                let name = res.name(doc, format!("idx:{key}"), obj);
                set(out, Object::Name(name), nums(&c.comps), None);
            }
            Space::Pattern(Some(base)) => {
                let Some(comps) = self.convert(base, &c.comps) else {
                    return self.emit(doc, res, c, false, stroke, out);
                };
                let dest = self.dest_space(doc);
                let name = res.name(doc, "pattern".into(), Object::Array(vec![Object::name("Pattern"), dest]));
                set(out, Object::Name(name), nums(&comps), c.pattern.as_ref());
            }
            sp => {
                let Some(comps) = self.convert(sp, &c.comps) else {
                    self.skip("colours whose components can't be read");
                    return self.emit(doc, res, c, false, stroke, out);
                };
                if self.icc_dest() {
                    let dest = self.dest_space(doc);
                    let name = res.name(doc, "dest".into(), dest);
                    set(out, Object::Name(name), nums(&comps), None);
                } else {
                    let op = match self.dest.family {
                        Family::Gray => "g",
                        Family::Rgb => "rg",
                        Family::Cmyk => "k",
                    };
                    out.push(Op::new(&case(op), nums(&comps)));
                }
            }
        }
        self.report.colors += 1;
    }

    /// Make the rewritten stream's colour right for painting a `kind` object with the fill
    /// and/or stroke colour.
    #[allow(clippy::too_many_arguments)]
    fn prepare(&mut self, doc: &mut Document, res: &mut Res, gs: &mut Gs, kind: Kind, fill: bool, stroke: bool, out: &mut Vec<Op>, depth: usize) {
        for (use_it, is_stroke) in [(fill, false), (stroke, true)] {
            if !use_it {
                continue;
            }
            let ch = if is_stroke { &mut gs.stroke } else { &mut gs.fill };
            let cur = ch.cur.clone();
            if matches!(&*cur.space, Space::Pattern(_))
                && self.selected(kind)
                && let Some(p) = &cur.pattern
            {
                self.pattern(doc, &res.dict.clone(), p, depth);
            }
            if !self.managed(&cur.space) {
                continue;
            }
            let want = self.selected(kind);
            if ch.emitted.as_ref().is_some_and(|(c, col)| *c == want && *col == cur) {
                continue;
            }
            let ch_emitted = Some((want, cur.clone()));
            self.emit(doc, res, &cur, want, is_stroke, out);
            let ch = if is_stroke { &mut gs.stroke } else { &mut gs.fill };
            ch.emitted = ch_emitted;
        }
    }

    fn decode(&mut self, s: &Stream) -> Option<Vec<u8>> {
        let room = MAX_TOTAL.saturating_sub(self.bytes);
        if room == 0 || self.stopped {
            self.stopped = true;
            self.skip("content beyond the first 1 GB");
            return None;
        }
        match s.decoded_within(MAX_STREAM.min(room)) {
            Ok(d) => {
                self.bytes = self.bytes.saturating_add(d.len());
                Some(d)
            }
            Err(_) => {
                self.skip("content streams that can't be decoded or are larger than 64 MB");
                None
            }
        }
    }

    /// [`Cx::plan`], or no plan when it converts nothing: a stream whose colours are only set
    /// again as they were stays byte for byte.
    fn planned(&mut self, doc: &mut Document, ops: &[Op], res: &mut Res, init: Init, depth: usize) -> HashMap<usize, (Vec<Op>, bool)> {
        let before = self.report.colors + self.report.inline_images;
        let plan = self.plan(doc, ops, res, init, depth);
        if self.report.colors + self.report.inline_images == before { HashMap::new() } else { plan }
    }

    /// Plan the rewrite of one stream: operator index → (operators to put before it, keep it).
    fn plan(&mut self, doc: &mut Document, ops: &[Op], res: &mut Res, init: Init, depth: usize) -> HashMap<usize, (Vec<Op>, bool)> {
        let mut plan: HashMap<usize, (Vec<Op>, bool)> = HashMap::new();
        let start = |res: &Res| -> Channel {
            let cur = match init {
                Init::Default => Colour { form: Form::Device("g"), space: res.gray.clone(), comps: vec![0.0], pattern: None },
                Init::Inherited => Colour { form: Form::Inherited, space: res.inherited.clone(), comps: Vec::new(), pattern: None },
            };
            Channel { emitted: Some((false, cur.clone())), cur }
        };
        let mut gs = Gs { fill: start(res), stroke: start(res), render: 0 };
        let mut stack: Vec<Gs> = Vec::new();
        let mut path_start: Option<usize> = None;
        for (i, op) in ops.iter().enumerate() {
            let k = op_name(op);
            let mut before: Vec<Op> = Vec::new();
            let mut keep = true;
            let mut at = i;
            match k {
                "q" => {
                    if stack.len() < MAX_STACK {
                        stack.push(gs.clone());
                    }
                }
                "Q" => {
                    if let Some(g) = stack.pop() {
                        gs = g;
                    }
                }
                "cs" | "CS" => {
                    let operand = op.operands.last().cloned().unwrap_or(Object::Null);
                    let space = self.space_of(doc, res, &operand);
                    self.note(&space);
                    let cur = Colour { form: Form::Named(operand), comps: space.initial(), space, pattern: None };
                    let ch = if k == "CS" { &mut gs.stroke } else { &mut gs.fill };
                    keep = !self.managed(&cur.space);
                    if keep {
                        ch.emitted = Some((false, cur.clone()));
                    }
                    ch.cur = cur;
                }
                "sc" | "scn" | "SC" | "SCN" => {
                    let stroke = k.starts_with('S');
                    let ch = if stroke { &mut gs.stroke } else { &mut gs.fill };
                    ch.cur.comps = op.operands.iter().filter_map(Object::as_f64).take(32).collect();
                    ch.cur.pattern = op.operands.last().and_then(Object::as_name).map(<[u8]>::to_vec);
                    keep = !self.managed(&ch.cur.space);
                    if keep {
                        ch.emitted = Some((false, ch.cur.clone()));
                    }
                }
                "g" | "G" | "rg" | "RG" | "k" | "K" => {
                    let stroke = k.chars().next().is_some_and(|c| c.is_ascii_uppercase());
                    let (form, space) = match k {
                        "g" | "G" => ("g", res.gray.clone()),
                        "rg" | "RG" => ("rg", res.rgb.clone()),
                        _ => ("k", res.cmyk.clone()),
                    };
                    let cur = Colour {
                        form: Form::Device(form),
                        comps: op.operands.iter().filter_map(Object::as_f64).take(4).collect(),
                        space,
                        pattern: None,
                    };
                    keep = !self.managed(&cur.space);
                    let ch = if stroke { &mut gs.stroke } else { &mut gs.fill };
                    if keep {
                        ch.emitted = Some((false, cur.clone()));
                    }
                    ch.cur = cur;
                }
                "m" | "l" | "c" | "v" | "y" | "h" | "re" => {
                    path_start.get_or_insert(i);
                }
                "f" | "F" | "f*" | "S" | "s" | "B" | "B*" | "b" | "b*" => {
                    at = path_start.take().unwrap_or(i);
                    let fill = !matches!(k, "S" | "s");
                    let stroke = matches!(k, "S" | "s" | "B" | "B*" | "b" | "b*");
                    self.prepare(doc, res, &mut gs, Kind::Vector, fill, stroke, &mut before, depth);
                }
                "n" => path_start = None,
                "Tr" => gs.render = op.num(0).filter(|v| v.is_finite()).map_or(0, |v| v as i64),
                "Tj" | "TJ" | "'" | "\"" => {
                    let fill = matches!(gs.render, 0 | 2 | 4 | 6);
                    let stroke = matches!(gs.render, 1 | 2 | 5 | 6);
                    self.prepare(doc, res, &mut gs, Kind::Text, fill, stroke, &mut before, depth);
                }
                "sh" => {
                    if self.selected(Kind::Vector)
                        && let Some(entry) = op.name(0).and_then(|n| resource(doc, &res.dict, b"Shading", n))
                    {
                        match entry {
                            Object::Ref(r) => self.shading_ref(doc, r),
                            _ => self.skip("shadings written directly in resource dictionaries"),
                        }
                    }
                }
                "Do" => {
                    let Some(Object::Ref(r)) = op.name(0).and_then(|n| resource(doc, &res.dict, b"XObject", n)) else {
                        continue;
                    };
                    let obj = doc.get(r);
                    let Object::Stream(s) = &*obj else { continue };
                    match s.dict.name(b"Subtype") {
                        Some(b"Image") => {
                            if matches!(s.dict.get(b"ImageMask"), Some(Object::Bool(true))) {
                                self.prepare(doc, res, &mut gs, Kind::Image, true, false, &mut before, depth);
                            } else if self.selected(Kind::Image) {
                                self.image(doc, r);
                            }
                        }
                        Some(b"Form") => {
                            self.prepare(doc, res, &mut gs, Kind::Form, true, true, &mut before, depth);
                            if depth < MAX_DEPTH {
                                let caller = res.dict.clone();
                                self.form(doc, r, &caller, Init::Inherited, depth + 1);
                            } else {
                                self.skip("forms nested more than 16 deep");
                            }
                        }
                        _ => {}
                    }
                }
                "BI" => {
                    if let Some((dict, data)) = &op.inline {
                        let mask = matches!(dict.get(b"IM").or_else(|| dict.get(b"ImageMask")), Some(Object::Bool(true)));
                        if mask {
                            self.prepare(doc, res, &mut gs, Kind::Image, true, false, &mut before, depth);
                        } else if self.selected(Kind::Image)
                            && let Some(new) = self.inline_image(doc, res, dict, data)
                        {
                            before.push(new);
                            keep = false;
                        }
                    }
                }
                _ => {}
            }
            if !before.is_empty() || !keep {
                let entry = plan.entry(at).or_insert_with(|| (Vec::new(), true));
                entry.0.extend(before);
                if !keep {
                    let entry = plan.entry(i).or_insert_with(|| (Vec::new(), true));
                    entry.1 = false;
                }
            }
        }
        plan
    }

    fn page(&mut self, doc: &mut Document, page: &pdfcraft_model::Page) -> Result<(), EditError> {
        let res_dict = page.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default();
        let mut res = Res::new(res_dict);
        let streams = content_streams(doc, &page.dict);
        let pieces = Pieces::join(&streams.iter().map(|(_, d)| d.as_slice()).collect::<Vec<_>>());
        let total: usize = streams.iter().map(|(_, d)| d.len()).sum();
        self.bytes = self.bytes.saturating_add(total);
        if self.bytes > MAX_TOTAL {
            self.stopped = true;
            self.skip("content beyond the first 1 GB");
            return Ok(());
        }
        let ops = pieces.parse().ops;
        let plan = self.planned(doc, &ops, &mut res, Init::Default, 0);
        if !plan.is_empty() {
            let new = splice(&pieces, &ops, |k| plan.get(&k).cloned().unwrap_or((Vec::new(), true)));
            let contents = rewritten_contents(doc, &streams, new);
            doc.update_dict(page.obj, |d| d.set(b"Contents".to_vec(), contents))?;
            self.changed = true;
        }
        let raw = doc.get(page.obj).as_dict().cloned().unwrap_or_default();
        if let Some(entry) = with_colour_spaces(doc, raw.get(b"Resources"), &res.dict, &res.adds)? {
            doc.update_dict(page.obj, |d| d.set(b"Resources".to_vec(), entry))?;
        }
        // The page's transparency group blends in the destination space.
        if let Some(group) = self.group(doc, raw.get(b"Group")) {
            doc.update_dict(page.obj, |d| d.set(b"Group".to_vec(), group))?;
        }
        self.annotations(doc, &raw)?;
        Ok(())
    }

    /// A transparency group dictionary with its blending space converted, if it is.
    fn group(&mut self, doc: &mut Document, group: Option<&Object>) -> Option<Object> {
        let g = doc.resolve(group?);
        let d = g.as_dict()?;
        if d.name(b"S") != Some(b"Transparency".as_slice()) {
            return None;
        }
        let space = Space::parse(doc, d.get(b"CS")?, &Dict::new(), 0, self.profile_bytes.as_deref().map(Vec::as_slice));
        if !self.managed(&space) || space.is_spot() || matches!(space, Space::Indexed { .. } | Space::Pattern(_)) {
            return None;
        }
        let mut d = d.clone();
        d.set(b"CS".to_vec(), self.dest_space(doc));
        self.changed = true;
        match group? {
            Object::Ref(r) => {
                doc.set(*r, Object::Dict(d));
                None
            }
            _ => Some(Object::Dict(d)),
        }
    }

    fn annotations(&mut self, doc: &mut Document, page: &Dict) -> Result<(), EditError> {
        let Some(annots) = page.get(b"Annots").map(|a| doc.resolve(a)) else { return Ok(()) };
        let list = annots.as_array().cloned().unwrap_or_default();
        for a in list.iter().take(10_000) {
            let Some(ap) = doc.dict(a).and_then(|d| d.get(b"AP").cloned()) else { continue };
            let Some(ap) = doc.dict(&ap) else { continue };
            let mut streams = Vec::new();
            for key in [b"N".as_slice(), b"R", b"D"] {
                match ap.get(key) {
                    Some(Object::Ref(r)) => match &*doc.get(*r) {
                        Object::Stream(_) => streams.push(*r),
                        Object::Dict(states) => streams.extend(states.iter().filter_map(|(_, v)| v.as_ref()).take(64)),
                        _ => {}
                    },
                    Some(Object::Dict(states)) => streams.extend(states.iter().filter_map(|(_, v)| v.as_ref()).take(64)),
                    _ => {}
                }
            }
            let before = self.report.forms;
            for r in streams {
                self.form(doc, r, &Dict::new(), Init::Default, 1);
            }
            if self.report.forms > before {
                self.report.annotations += 1;
                self.changed = true;
            }
        }
        Ok(())
    }

    /// Rewrite a form XObject (or a coloured tiling pattern) in place, once.
    fn form(&mut self, doc: &mut Document, r: ObjRef, caller: &Dict, init: Init, depth: usize) {
        if !self.forms.insert(r) {
            return;
        }
        let obj = doc.get(r);
        let Object::Stream(s) = &*obj else { return };
        let is_pattern = s.dict.int(b"PatternType") == Some(1);
        if !is_pattern && s.dict.name(b"Subtype") != Some(b"Form".as_slice()) {
            return;
        }
        let Some(data) = self.decode(s) else { return };
        let mut dict = s.dict.clone();
        let own = dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned());
        let mut res = Res::new(own.clone().unwrap_or_else(|| caller.clone()));
        let pieces = Pieces::join(&[data.as_slice()]);
        let ops = pieces.parse().ops;
        let plan = self.planned(doc, &ops, &mut res, init, depth);
        let group = self.group(doc, dict.get(b"Group"));
        if plan.is_empty() && res.adds.is_empty() && group.is_none() {
            return;
        }
        let entry = match with_colour_spaces(doc, dict.get(b"Resources"), &res.dict, &res.adds) {
            Ok(e) => e,
            Err(_) => return,
        };
        if let Some(e) = entry {
            dict.set(b"Resources".to_vec(), e);
        }
        if let Some(g) = group {
            dict.set(b"Group".to_vec(), g);
        }
        let new = splice(&pieces, &ops, |k| plan.get(&k).cloned().unwrap_or((Vec::new(), true)));
        let data = new.into_iter().next().unwrap_or(data);
        dict.remove(b"Length");
        doc.set(r, Object::Stream(Stream::flate(dict, &data)));
        if is_pattern {
            self.report.patterns += 1;
        } else {
            self.report.forms += 1;
        }
    }

    /// A pattern a paint uses (resource `name`).
    fn pattern(&mut self, doc: &mut Document, res: &Dict, name: &[u8], depth: usize) {
        let Some(Object::Ref(r)) = resource(doc, res, b"Pattern", name) else { return };
        let obj = doc.get(r);
        let Some(d) = obj.as_dict() else { return };
        match d.int(b"PatternType") {
            Some(1) if d.int(b"PaintType") == Some(1) => {
                if depth < MAX_DEPTH {
                    self.form(doc, r, &Dict::new(), Init::Default, depth + 1);
                }
            }
            Some(2) => {
                if !self.patterns.insert(r) {
                    return;
                }
                match d.get(b"Shading").cloned() {
                    Some(Object::Ref(sr)) => self.shading_ref(doc, sr),
                    Some(direct) => {
                        if let Some(new) = self.shading(doc, &direct) {
                            let mut d = d.clone();
                            d.set(b"Shading".to_vec(), new);
                            doc.set(r, Object::Dict(d));
                            self.report.patterns += 1;
                            self.report.shadings += 1;
                        }
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn shading_ref(&mut self, doc: &mut Document, r: ObjRef) {
        if !self.shadings.insert(r) {
            return;
        }
        let obj = (*doc.get(r)).clone();
        if let Some(new) = self.shading(doc, &obj) {
            doc.set(r, new);
            self.report.shadings += 1;
            self.changed = true;
        }
    }

    /// A shading with its colours converted, or `None` when it stays.
    fn shading(&mut self, doc: &mut Document, obj: &Object) -> Option<Object> {
        let d = obj.as_dict()?;
        let cs = d.get(b"ColorSpace")?;
        let space = Space::parse(doc, cs, &Dict::new(), 0, self.profile_bytes.as_deref().map(Vec::as_slice));
        self.note(&space);
        if !self.managed(&space) {
            return None;
        }
        let kind = d.get(b"ShadingType").and_then(|t| doc.resolve(t).as_int())?;
        let mut new = d.clone();
        if let Some(bg) = d.get(b"Background").map(|b| doc.resolve(b)).and_then(|b| b.as_array().cloned()) {
            let comps: Vec<f64> = bg.iter().filter_map(|v| doc.resolve(v).as_f64()).collect();
            match self.convert(&space, &comps) {
                Some(out) => new.set(b"Background".to_vec(), Object::Array(out.into_iter().map(Object::Real).collect())),
                None => {
                    new.remove(b"Background");
                }
            }
        }
        match d.get(b"Function") {
            Some(f) => {
                let Some(func) = Func::parse(doc, f, 0) else {
                    self.skip("shadings whose colour function can't be evaluated");
                    return None;
                };
                if func.outputs().is_some_and(|n| n != space.components()) {
                    self.skip("shadings whose colour function can't be evaluated");
                    return None;
                }
                let nums = |key: &[u8]| -> Option<Vec<f64>> {
                    let a = doc.resolve(d.get(key)?);
                    a.as_array()?.iter().map(|x| doc.resolve(x).as_f64().filter(|v| v.is_finite())).collect()
                };
                let range = vec![[0.0, 1.0]; self.dest.family.components()];
                let (domain, samples) = if kind == 1 {
                    let dom = match nums(b"Domain").as_deref() {
                        Some([a, b, c, e]) => [[*a, *b], [*c, *e]],
                        _ => [[0.0, 1.0], [0.0, 1.0]],
                    };
                    let n = SHADING_GRID;
                    let mut samples = Vec::with_capacity(n * n);
                    for j in 0..n {
                        for i in 0..n {
                            let x = dom[0][0] + (dom[0][1] - dom[0][0]) * i as f64 / (n - 1) as f64;
                            let y = dom[1][0] + (dom[1][1] - dom[1][0]) * j as f64 / (n - 1) as f64;
                            samples.push(func.eval(&[x, y]).and_then(|c| self.convert(&space, &c)));
                        }
                    }
                    (dom.to_vec(), samples)
                } else {
                    let dom = if kind <= 3 {
                        match nums(b"Domain").as_deref() {
                            Some([a, b]) => [*a, *b],
                            _ => [0.0, 1.0],
                        }
                    } else {
                        func.domain()
                    };
                    let n = SHADING_SAMPLES;
                    let samples = (0..n)
                        .map(|i| {
                            let t = dom[0] + (dom[1] - dom[0]) * i as f64 / (n - 1) as f64;
                            func.eval(&[t]).and_then(|c| self.convert(&space, &c))
                        })
                        .collect();
                    (vec![dom], samples)
                };
                let Some(samples) = samples.into_iter().collect::<Option<Vec<_>>>() else {
                    self.skip("shadings whose colour function can't be evaluated");
                    return None;
                };
                let size = if kind == 1 { SHADING_GRID } else { SHADING_SAMPLES };
                let f = doc.add(Object::Stream(sampled_stream(&domain, size, &range, &samples)));
                new.set(b"Function".to_vec(), Object::Ref(f));
            }
            None if kind >= 4 => {
                self.skip("mesh shadings with colours in their vertices");
                return None;
            }
            None => return None,
        }
        new.set(b"ColorSpace".to_vec(), self.dest_space(doc));
        Some(match obj {
            Object::Stream(s) => Object::Stream(Stream::from_raw(new, s.raw.to_vec())),
            _ => Object::Dict(new),
        })
    }

    /// Convert an image XObject in place, once.
    fn image(&mut self, doc: &mut Document, r: ObjRef) {
        if !self.images.insert(r) {
            return;
        }
        let obj = doc.get(r);
        let Object::Stream(s) = &*obj else { return };
        let Some(cs) = s.dict.get(b"ColorSpace") else { return };
        let space = Space::parse(doc, cs, &Dict::new(), 0, self.profile_bytes.as_deref().map(Vec::as_slice));
        self.note(&space);
        if !self.managed(&space) {
            return;
        }
        let dict = s.dict.clone();
        if let Space::Indexed { .. } = &space {
            let Some(cs) = self.indexed_space(doc, &space) else { return };
            let mut d = dict;
            d.set(b"ColorSpace".to_vec(), cs);
            doc.set(r, Object::Stream(Stream::from_raw(d, s.raw.to_vec())));
            self.report.images += 1;
            self.changed = true;
            return;
        }
        let filters: Vec<pdfcraft_filters::Filter> = s.filters().into_iter().map(|(f, _)| f).collect();
        use pdfcraft_filters::Filter as F;
        if filters.iter().any(|f| matches!(f, F::Jpx | F::Jbig2 | F::CcittFax | F::Unknown(_))) {
            self.skip("JPEG 2000, JBIG2, CCITT and unknown-filter images");
            return;
        }
        if matches!(dict.get(b"Mask"), Some(Object::Array(_))) {
            self.skip("images with colour-key masks");
            return;
        }
        let dct = filters.last() == Some(&F::Dct);
        if dct && filters.len() > 1 {
            self.skip("JPEG images inside other filters");
            return;
        }
        let w = dict.get(b"Width").and_then(|v| doc.resolve(v).as_int()).unwrap_or(0);
        let h = dict.get(b"Height").and_then(|v| doc.resolve(v).as_int()).unwrap_or(0);
        let bpc = dict.get(b"BitsPerComponent").and_then(|v| doc.resolve(v).as_int()).unwrap_or(8);
        let decode: Option<Vec<f64>> = dict
            .get(b"Decode")
            .map(|d| doc.resolve(d))
            .and_then(|d| d.as_array().map(|a| a.iter().filter_map(|v| doc.resolve(v).as_f64()).collect()));
        let Some(px) = Pixels::check(w, h, bpc, space.components(), MAX_IMAGE_PIXELS) else {
            self.skip("images larger than 40 million pixels or with unusual sample sizes");
            return;
        };
        let samples = if dct {
            match jpeg_samples(&s.raw, &px) {
                Some(v) => v,
                None => {
                    self.skip("JPEG images that can't be decoded (CMYK JPEGs among them)");
                    return;
                }
            }
        } else {
            match s.decoded_within(px.bytes().min(MAX_IMAGE_BYTES).saturating_add(64)) {
                Ok(v) => v,
                Err(_) => {
                    self.skip("images whose data can't be decoded");
                    return;
                }
            }
        };
        let Some(out) = self.pixels(&space, &px, &samples, decode.as_deref()) else {
            self.skip("images whose data is shorter than their size");
            return;
        };
        let mut d = dict;
        for k in [&b"Filter"[..], b"DecodeParms", b"Decode", b"Length", b"DL"] {
            d.remove(k);
        }
        d.set(b"ColorSpace".to_vec(), self.dest_space(doc));
        d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
        let n = self.dest.family.components();
        let stream = match (dct, n) {
            (true, 1 | 3) => match encode_jpeg(&out, n, px.w, px.h) {
                Some(jpg) => {
                    let mut d = d;
                    d.set(b"Filter".to_vec(), Object::name("DCTDecode"));
                    Stream::from_raw(d, jpg)
                }
                None => Stream::flate(d, &out),
            },
            _ => Stream::flate(d, &out),
        };
        doc.set(r, Object::Stream(stream));
        self.report.images += 1;
        self.changed = true;
    }

    /// Destination samples (8-bit) for an image's samples.
    fn pixels(&self, space: &Space, px: &Pixels, data: &[u8], decode: Option<&[f64]>) -> Option<Vec<u8>> {
        let n = px.n;
        let ranges = space.ranges();
        let max = ((1u32 << px.bpc) - 1) as f64;
        let dec: Vec<[f64; 2]> = (0..n)
            .map(|i| match decode {
                Some(d) if d.len() >= 2 * n => [d.get(2 * i).copied().unwrap_or(0.0), d.get(2 * i + 1).copied().unwrap_or(1.0)],
                _ => ranges.get(i).copied().unwrap_or([0.0, 1.0]),
            })
            .collect();
        let row_bytes = px.row_bytes();
        if data.len() < row_bytes.checked_mul(px.h)? {
            return None;
        }
        let out_n = self.dest.family.components();
        let mut out = Vec::with_capacity(px.w.checked_mul(px.h)?.checked_mul(out_n)?);
        let mut cache: HashMap<Vec<u16>, Vec<u8>> = HashMap::new();
        let mut raw = vec![0u16; n];
        let mut comps = vec![0.0; n];
        for y in 0..px.h {
            let row = data.get(y * row_bytes..(y + 1) * row_bytes)?;
            let mut bit = 0usize;
            for _ in 0..px.w {
                for slot in raw.iter_mut() {
                    *slot = read_sample(row, bit, px.bpc)?;
                    bit += px.bpc;
                }
                if let Some(hit) = cache.get(&raw) {
                    out.extend_from_slice(hit);
                    continue;
                }
                for (i, c) in comps.iter_mut().enumerate() {
                    let d = dec.get(i).copied().unwrap_or([0.0, 1.0]);
                    *c = d[0] + f64::from(raw.get(i).copied().unwrap_or(0)) / max * (d[1] - d[0]);
                }
                let v: Vec<u8> = match space.value(&comps) {
                    Some(v) => self.dest.convert(v).into_iter().map(to_byte).collect(),
                    None => vec![0; out_n],
                };
                out.extend_from_slice(&v);
                if cache.len() < MAX_CACHE {
                    cache.insert(raw.clone(), v);
                }
            }
        }
        Some(out)
    }

    /// The inline image `BI dict ID data EI` converted, as a new operator.
    fn inline_image(&mut self, doc: &mut Document, res: &mut Res, dict: &Dict, data: &[u8]) -> Option<Op> {
        let get = |short: &[u8], long: &[u8]| dict.get(short).or_else(|| dict.get(long)).cloned();
        let cs = get(b"CS", b"ColorSpace")?;
        let space = Space::parse(doc, &cs, &res.dict, 0, self.profile_bytes.as_deref().map(Vec::as_slice));
        self.note(&space);
        if !self.managed(&space) {
            return None;
        }
        let mut out = Dict::new();
        let w = get(b"W", b"Width").and_then(|v| v.as_int()).unwrap_or(0);
        let h = get(b"H", b"Height").and_then(|v| v.as_int()).unwrap_or(0);
        let bpc = get(b"BPC", b"BitsPerComponent").and_then(|v| v.as_int()).unwrap_or(8);
        if let Space::Indexed { .. } = &space {
            let obj = self.indexed_space(doc, &space)?;
            let key = format!("inline-idx:{}", String::from_utf8_lossy(&pdfcraft_content::serialize_ops(&[Op::new("cs", vec![cs.clone()])])));
            let name = res.name(doc, key, obj);
            for (k, v) in dict.iter() {
                out.set(k.clone(), v.clone());
            }
            out.remove(b"CS");
            out.remove(b"ColorSpace");
            out.set(b"CS".to_vec(), Object::Name(name));
            self.report.inline_images += 1;
            return Some(Op { op: b"BI".to_vec(), operands: Vec::new(), inline: Some((out, data.to_vec())), span: 0..0 });
        }
        let filter = get(b"F", b"Filter");
        let parms = get(b"DP", b"DecodeParms");
        let px = Pixels::check(w, h, bpc, space.components(), MAX_INLINE_PIXELS)?;
        let mut sd = Dict::new();
        if let Some(f) = filter {
            sd.set(b"Filter".to_vec(), f);
        }
        if let Some(p) = parms {
            sd.set(b"DecodeParms".to_vec(), p);
        }
        let stream = Stream::from_raw(sd, data.to_vec());
        if stream.filters().iter().any(|(f, _)| {
            matches!(
                f,
                pdfcraft_filters::Filter::Dct
                    | pdfcraft_filters::Filter::Jpx
                    | pdfcraft_filters::Filter::CcittFax
                    | pdfcraft_filters::Filter::Jbig2
                    | pdfcraft_filters::Filter::Unknown(_)
            )
        }) {
            self.skip("inline images in image codecs");
            return None;
        }
        let samples = stream.decoded_within(px.bytes().saturating_add(64)).ok()?;
        let decode: Option<Vec<f64>> = get(b"D", b"Decode").and_then(|d| d.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()));
        let pixels = self.pixels(&space, &px, &samples, decode.as_deref())?;
        out.set(b"W".to_vec(), Object::Int(w));
        out.set(b"H".to_vec(), Object::Int(h));
        let dest_cs = if self.icc_dest() {
            let dest = self.dest_space(doc);
            Object::Name(res.name(doc, "dest".into(), dest))
        } else {
            Object::name(match self.dest.family {
                Family::Gray => "G",
                Family::Rgb => "RGB",
                Family::Cmyk => "CMYK",
            })
        };
        out.set(b"CS".to_vec(), dest_cs);
        out.set(b"BPC".to_vec(), Object::Int(8));
        out.set(b"F".to_vec(), Object::name("AHx"));
        for k in [&b"I"[..], b"Interpolate", b"Intent"] {
            if let Some(v) = dict.get(k) {
                out.set(k.to_vec(), v.clone());
            }
        }
        self.report.inline_images += 1;
        Some(Op { op: b"BI".to_vec(), operands: Vec::new(), inline: Some((out, hex(&pixels))), span: 0..0 })
    }
}

/// The size of an image's samples.
struct Pixels {
    w: usize,
    h: usize,
    bpc: usize,
    n: usize,
}

impl Pixels {
    fn check(w: i64, h: i64, bpc: i64, n: usize, max: u64) -> Option<Pixels> {
        if !(1..=1 << 16).contains(&w) || !(1..=1 << 16).contains(&h) || ![1, 2, 4, 8, 16].contains(&bpc) || n == 0 || n > 32 {
            return None;
        }
        let px = Pixels { w: w as usize, h: h as usize, bpc: bpc as usize, n };
        ((w as u64) * (h as u64) <= max && px.bytes() <= MAX_IMAGE_BYTES).then_some(px)
    }

    fn row_bytes(&self) -> usize {
        self.w.saturating_mul(self.n).saturating_mul(self.bpc).div_ceil(8)
    }

    fn bytes(&self) -> usize {
        self.row_bytes().saturating_mul(self.h)
    }
}

fn read_sample(row: &[u8], bit: usize, bpc: usize) -> Option<u16> {
    match bpc {
        8 => row.get(bit / 8).map(|b| u16::from(*b)),
        16 => {
            let i = bit / 8;
            Some(u16::from_be_bytes([*row.get(i)?, *row.get(i + 1)?]))
        }
        _ => {
            let byte = *row.get(bit / 8)?;
            let shift = 8 - (bit % 8) - bpc;
            Some(u16::from((byte >> shift) & ((1u8 << bpc) - 1)))
        }
    }
}

/// The 8-bit samples of a baseline gray or RGB JPEG of the image's size.
fn jpeg_samples(raw: &[u8], px: &Pixels) -> Option<Vec<u8>> {
    if px.bpc != 8 || !(px.n == 1 || px.n == 3) {
        return None;
    }
    let img = image::load_from_memory_with_format(raw, image::ImageFormat::Jpeg).ok()?;
    let ok = matches!((img.color(), px.n), (image::ColorType::L8, 1) | (image::ColorType::Rgb8, 3));
    if !ok || img.width() as usize != px.w || img.height() as usize != px.h {
        return None;
    }
    let v = if px.n == 1 { img.into_luma8().into_raw() } else { img.into_rgb8().into_raw() };
    (v.len() == px.bytes()).then_some(v)
}

fn encode_jpeg(px: &[u8], n: usize, w: usize, h: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92);
    let ty = if n == 1 { image::ExtendedColorType::L8 } else { image::ExtendedColorType::Rgb8 };
    enc.encode(px, u32::try_from(w).ok()?, u32::try_from(h).ok()?, ty).ok()?;
    Some(out)
}

fn resource(doc: &Document, res: &Dict, category: &[u8], name: &[u8]) -> Option<Object> {
    let group = doc.resolve(res.get(category)?);
    group.as_dict()?.get(name).cloned()
}

/// Add colour spaces to a resource dictionary stored as `entry` (a reference, a direct
/// dictionary, or absent: then `current` is the inherited one). Returns the new value of the
/// holder's `/Resources` when the holder must change; references are updated in place.
fn with_colour_spaces(doc: &mut Document, entry: Option<&Object>, current: &Dict, adds: &[(Vec<u8>, Object)]) -> Result<Option<Object>, EditError> {
    if adds.is_empty() {
        return Ok(None);
    }
    match entry {
        Some(Object::Ref(r)) if matches!(&*doc.get(*r), Object::Dict(_)) => {
            let d = doc.get(*r).as_dict().cloned().unwrap_or_default();
            if let Some(cs) = merged(doc, d.get(b"ColorSpace"), adds)? {
                doc.update_dict(*r, |d| d.set(b"ColorSpace".to_vec(), cs))?;
            }
            Ok(None)
        }
        Some(Object::Dict(d)) => {
            let mut d = d.clone();
            if let Some(cs) = merged(doc, d.get(b"ColorSpace"), adds)? {
                d.set(b"ColorSpace".to_vec(), cs);
            }
            Ok(Some(Object::Dict(d)))
        }
        _ => {
            let mut d = current.clone();
            if let Some(cs) = merged(doc, d.get(b"ColorSpace"), adds)? {
                d.set(b"ColorSpace".to_vec(), cs);
            }
            Ok(Some(Object::Dict(d)))
        }
    }
}

fn merged(doc: &mut Document, entry: Option<&Object>, adds: &[(Vec<u8>, Object)]) -> Result<Option<Object>, EditError> {
    match entry {
        Some(Object::Ref(r)) if matches!(&*doc.get(*r), Object::Dict(_)) => {
            doc.update_dict(*r, |d| {
                for (k, v) in adds {
                    d.set(k.clone(), v.clone());
                }
            })?;
            Ok(None)
        }
        Some(Object::Dict(d)) => {
            let mut d = d.clone();
            for (k, v) in adds {
                d.set(k.clone(), v.clone());
            }
            Ok(Some(Object::Dict(d)))
        }
        _ => Ok(Some(Object::Dict(adds.iter().cloned().collect()))),
    }
}

/// Make the destination profile the catalog's PDF/X output intent (replacing an earlier one).
fn add_output_intent(doc: &mut Document, profile: ObjRef, condition: &str) -> Result<(), EditError> {
    let root = doc.root().ok_or_else(|| EditError::Invalid("the document has no catalog".into()))?;
    let catalog = doc.get(root).as_dict().cloned().unwrap_or_default();
    let mut list: Vec<Object> = catalog.get(b"OutputIntents").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()).unwrap_or_default();
    list.retain(|oi| doc.dict(oi).is_none_or(|d| d.name(b"S") != Some(b"GTS_PDFX".as_slice())));
    let text = || Object::String(PdfString::literal(condition.as_bytes().to_vec()));
    let mut oi = Dict::new();
    oi.set(b"Type".to_vec(), Object::name("OutputIntent"));
    oi.set(b"S".to_vec(), Object::name("GTS_PDFX"));
    oi.set(b"OutputConditionIdentifier".to_vec(), text());
    oi.set(b"Info".to_vec(), text());
    oi.set(b"DestOutputProfile".to_vec(), Object::Ref(profile));
    list.insert(0, Object::Dict(oi));
    doc.update_dict(root, |d| d.set(b"OutputIntents".to_vec(), Object::Array(list)))?;
    Ok(())
}
