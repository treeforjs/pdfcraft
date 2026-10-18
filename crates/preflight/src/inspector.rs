//! Object Inspector (Acrobat: Output Preview ▸ Object Inspector): every object painted at one
//! point of a page, topmost first, with what a print technician checks: its kind (text, path,
//! image, shading, form XObject, annotation), its fill and stroke colour (space, components and
//! the inks it puts down, as the Ink Manager routes them), overprint (`OP`, `op`, `OPM`),
//! transparency (blend mode, constant alpha, soft mask), rendering intent and line width; the
//! font (name, type, size, embedded) of text; the size, bits per component, colour space and
//! effective resolution of images; and the ink of every plate at the point.
//!
//! The objects come from the ink rasterizer's content interpreter ([`crate::coverage`]), which
//! tests the point against each painted object's real geometry: fills with their winding rule,
//! strokes within half the line width (at least [`TOLERANCE`]), images by their unit square,
//! shadings by the clip. As in the separations, clipping paths count as their bounding boxes and
//! text as the box of each shown string (font widths, ascent and descent), curves are flattened,
//! and optional content is not hidden. Annotations are listed by their rectangle (their
//! appearances are not inspected). The ink values at the point are a separations sample at the
//! inspection's grid resolution.

use pdfcraft_cos::{Document, Object};

use crate::coverage::{Conversion, Probe, Raster, rasterize_probed};
use crate::separations::{Sample, sample};

/// Objects listed at most; deeper ones are dropped (`objects_truncated`).
pub const MAX_OBJECTS: usize = 512;
/// Annotations looked at, at most.
pub const MAX_ANNOTATIONS: usize = 4096;
/// The stroke hit tolerance: a point this close (in points) to a stroke's centre line is on it,
/// however thin the line.
pub const TOLERANCE: f64 = 0.5;

/// What kind of object was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Text,
    Path,
    Image,
    Shading,
    Form,
    Annotation,
}

impl ObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Path => "path",
            Self::Image => "image",
            Self::Shading => "shading",
            Self::Form => "form",
            Self::Annotation => "annotation",
        }
    }
}

/// A fill or stroke colour.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectColour {
    /// The colour space: `DeviceCMYK`, `ICCBased (4)`, `Separation (Gold)`, `DeviceN (Cyan,
    /// Teal)`, `Indexed (DeviceRGB)`, `Pattern` …
    pub space: String,
    /// The colour's components as set (`sc`/`scn`, `k`, `rg` …), rounded to 1/10000.
    pub components: Vec<f64>,
    /// The inks it puts down with their percentages (0–100), after the Ink Manager's routing;
    /// RGB and Lab through the conversion (`converted`). Empty when not measurable.
    pub inks: Vec<(String, f64)>,
    /// Its inks come from the RGB/Lab → CMYK conversion (or a spot ink converted to process).
    pub converted: bool,
    /// Its inks could be worked out (patterns and unknown colour spaces can't).
    pub measured: bool,
    /// A pattern colour: the pattern's resource name and type (`P1: shading pattern, axial,
    /// DeviceCMYK`).
    pub pattern: Option<String>,
}

/// The overprint parameters an object is painted with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Overprint {
    /// `op`: overprint for fills, text fills and images.
    pub fill: bool,
    /// `OP`: overprint for strokes.
    pub stroke: bool,
    /// `OPM` (0 or 1).
    pub mode: i64,
}

/// The transparency parameters an object is painted with.
#[derive(Clone, Debug, PartialEq)]
pub struct Transparency {
    /// `BM` (the first supported one of an array).
    pub blend_mode: String,
    /// `ca` and `CA`.
    pub fill_alpha: f64,
    pub stroke_alpha: f64,
    /// The soft mask: `Luminosity`/`Alpha` (ExtGState `SMask`), `image` (an image's `SMask`),
    /// `JPX data` (`SMaskInData`).
    pub soft_mask: Option<String>,
    /// `AIS`.
    pub alpha_is_shape: bool,
}

impl Default for Transparency {
    fn default() -> Self {
        Self { blend_mode: "Normal".into(), fill_alpha: 1.0, stroke_alpha: 1.0, soft_mask: None, alpha_is_shape: false }
    }
}

impl Transparency {
    /// Whether anything here makes the object transparent.
    pub fn is_transparent(&self) -> bool {
        !matches!(self.blend_mode.as_str(), "Normal" | "Compatible") || self.fill_alpha < 1.0 || self.stroke_alpha < 1.0 || self.soft_mask.is_some()
    }
}

/// A text object's font.
#[derive(Clone, Debug, PartialEq)]
pub struct TextInfo {
    /// `BaseFont` (with its subset prefix), empty when the font has none.
    pub font: String,
    /// The font resource's name (`F1`).
    pub resource: String,
    /// `Subtype`: `Type1`, `TrueType`, `Type0`, `Type3`, `MMType1`.
    pub font_type: String,
    /// The font program is embedded (`FontFile`, `FontFile2`, `FontFile3`; Type 3 fonts always).
    pub embedded: bool,
    /// The name has a subset prefix (`ABCDEF+`).
    pub subset: bool,
    /// The `Tf` size, and the size on the page (scaled by the text and current matrices).
    pub size: f64,
    pub effective_size: f64,
    /// `Tr` (0–7).
    pub render_mode: i64,
    /// Neither filled nor stroked (render modes 3 and 7: OCR text layers, clipping text).
    pub invisible: bool,
}

/// An image.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageInfo {
    /// The XObject resource name; `None` for an inline image.
    pub name: Option<String>,
    pub width: i64,
    pub height: i64,
    pub bits_per_component: i64,
    /// Its colour space (`stencil mask` for an image mask; `from the JPX data` when absent).
    pub colour_space: String,
    pub stencil: bool,
    /// Its filters (`DCTDecode` …).
    pub filters: Vec<String>,
    /// Effective resolution on the page, horizontally and vertically (pixels per inch of
    /// user space, 1/10 ppi; 0 when degenerate).
    pub ppi: [f64; 2],
    /// `Mask`: `colour key` or `stencil`.
    pub mask: Option<String>,
    pub interpolate: bool,
    /// Its own `Intent`.
    pub intent: Option<String>,
}

/// A shading painted with `sh`.
#[derive(Clone, Debug, PartialEq)]
pub struct ShadingInfo {
    pub name: String,
    /// `ShadingType` (1–7) and what it is (`axial` …).
    pub shading_type: Option<i64>,
    pub description: String,
    pub colour_space: String,
}

/// A form XObject whose box holds the point.
#[derive(Clone, Debug, PartialEq)]
pub struct FormInfo {
    pub name: String,
    /// Its transparency group: `isolated`, `knockout` and the group colour space.
    pub group: Option<String>,
}

/// An annotation whose rectangle holds the point.
#[derive(Clone, Debug, PartialEq)]
pub struct AnnotationInfo {
    pub subtype: String,
    /// `NM`.
    pub name: Option<String>,
    /// Flags: hidden (bit 2), print (bit 3), no view (bit 6).
    pub hidden: bool,
    pub printed: bool,
    pub has_appearance: bool,
}

/// One object at the point.
#[derive(Clone, Debug, PartialEq)]
pub struct Inspected {
    pub kind: ObjectKind,
    /// Its place in the page's painting order (1 is painted first, at the bottom).
    pub order: usize,
    /// Its bounds in user space `[x0, y0, x1, y1]` (rounded to 1/100 pt).
    pub bbox: [f64; 4],
    /// What of it covers the point: `fill`, `stroke` (empty for invisible text, forms and
    /// annotations).
    pub painted: Vec<&'static str>,
    pub fill: Option<ObjectColour>,
    pub stroke: Option<ObjectColour>,
    pub overprint: Overprint,
    pub transparency: Transparency,
    /// The rendering intent in force (`ri`, ExtGState `RI`; an image's own `Intent` overrides).
    pub rendering_intent: String,
    /// Line width on the page in points (stroked paths and text).
    pub line_width: Option<f64>,
    /// `nonzero` or `even-odd` (filled paths).
    pub fill_rule: Option<&'static str>,
    pub text: Option<TextInfo>,
    pub image: Option<ImageInfo>,
    pub shading: Option<ShadingInfo>,
    pub form: Option<FormInfo>,
    pub annotation: Option<AnnotationInfo>,
    /// The form XObjects it is drawn inside, outermost first (resource names).
    pub forms: Vec<String>,
}

/// What the Object Inspector found at a point.
#[derive(Clone, Debug)]
pub struct Inspection {
    /// 0-based page.
    pub page: usize,
    /// The user-space point inspected.
    pub point: [f64; 2],
    /// The objects there, topmost first (annotations above page content).
    pub objects: Vec<Inspected>,
    /// More than [`MAX_OBJECTS`] objects were there; the bottom ones are left out.
    pub objects_truncated: bool,
    /// Every ink at the point (`None` outside the crop box).
    pub sample: Option<Sample>,
    /// The page's separations the sample comes from (its inks, conversion, what wasn't
    /// measured, Ink Manager routes).
    pub raster: Raster,
}

/// Inspect user-space point (`x`, `y`) of 0-based `page`, separating at `dpi` (overprint
/// simulated when `overprint`) with `conversion` (and its Ink Manager settings).
pub fn inspect(doc: &Document, page: usize, x: f64, y: f64, dpi: f64, overprint: bool, conversion: &dyn Conversion) -> Result<Inspection, String> {
    if !(x.is_finite() && y.is_finite()) {
        return Err("the point must be finite".into());
    }
    let probe = Box::new(Probe::new((x, y)));
    let (raster, probe) = rasterize_probed(doc, page, dpi, overprint, conversion, Some(probe))?;
    let (objects, mut truncated) = probe
        .map(|p| {
            let p = *p;
            (p.objects, p.truncated)
        })
        .unwrap_or_default();
    // Painted bottom first: list the topmost first.
    let mut objects: Vec<Inspected> = objects.into_iter().rev().collect();
    let mut annotations = annotations_at(doc, page, x, y);
    annotations.reverse();
    if !annotations.is_empty() {
        let first = objects.iter().map(|o| o.order).max().unwrap_or(0).saturating_add(1);
        for (i, a) in annotations.iter_mut().rev().enumerate() {
            a.order = first.saturating_add(i);
        }
        annotations.extend(objects);
        objects = annotations;
    }
    if objects.len() > MAX_OBJECTS {
        objects.truncate(MAX_OBJECTS);
        truncated = true;
    }
    let sample = sample(&raster, x, y);
    Ok(Inspection { page, point: [x, y], objects, objects_truncated: truncated, sample, raster })
}

/// The annotations of `page` whose rectangle holds the point, in page order.
fn annotations_at(doc: &Document, page: usize, x: f64, y: f64) -> Vec<Inspected> {
    let pages = pdfcraft_model::pages(doc);
    let Some(p) = pages.get(page) else { return Vec::new() };
    let Some(list) = p.dict.get(b"Annots").map(|o| doc.resolve(o)) else { return Vec::new() };
    let Some(list) = list.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for entry in list.iter().take(MAX_ANNOTATIONS) {
        let a = doc.resolve(entry);
        let Some(d) = a.as_dict() else { continue };
        let rect: Vec<f64> = d
            .get(b"Rect")
            .map(|r| doc.resolve(r))
            .and_then(|r| r.as_array().map(|r| r.iter().filter_map(|v| doc.resolve(v).as_f64()).filter(|v| v.is_finite()).collect()))
            .unwrap_or_default();
        let [a0, b0, a1, b1] = rect.as_slice() else { continue };
        let bbox = [a0.min(*a1), b0.min(*b1), a0.max(*a1), b0.max(*b1)];
        if x < bbox[0] || x > bbox[2] || y < bbox[1] || y > bbox[3] {
            continue;
        }
        let flags = d.get(b"F").and_then(|f| doc.resolve(f).as_int()).unwrap_or(0);
        let name = d.get(b"NM").map(|n| doc.resolve(n)).and_then(|n| match n.as_ref() {
            Object::String(s) => Some(String::from_utf8_lossy(&s.bytes).chars().take(256).collect()),
            _ => None,
        });
        let round = |v: f64| (v * 100.0).round() / 100.0;
        out.push(Inspected {
            kind: ObjectKind::Annotation,
            order: 0,
            bbox: bbox.map(round),
            painted: Vec::new(),
            fill: None,
            stroke: None,
            overprint: Overprint::default(),
            transparency: Transparency::default(),
            rendering_intent: "RelativeColorimetric".into(),
            line_width: None,
            fill_rule: None,
            text: None,
            image: None,
            shading: None,
            form: None,
            annotation: Some(AnnotationInfo {
                subtype: d.name(b"Subtype").map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default(),
                name,
                hidden: flags & 2 != 0 || flags & 32 != 0,
                printed: flags & 4 != 0,
                has_appearance: d.get(b"AP").is_some(),
            }),
            forms: Vec::new(),
        });
        if out.len() >= MAX_OBJECTS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests;
