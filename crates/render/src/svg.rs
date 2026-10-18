//! Export a page as a standalone SVG document (Save As ▸ SVG).
//!
//! The page is interpreted by the bootstrap interpreter (hayro-interpret) into [`SvgDevice`], an
//! original `Device` that writes SVG elements instead of pixels, so everything the viewer
//! understands (fonts, colour spaces, forms, annotations, optional content) reaches the SVG the
//! same way it reaches the screen. The SVG user space is the page as displayed: the crop box
//! after `/Rotate`, in points, with the origin at the top left.
//!
//! - Paths keep their fill rule, line width, caps, joins, miter limit and dashes. Strokes keep
//!   the CTM as the element `transform`, so non-uniform scaling distorts the pen as in PDF.
//! - Colours are converted to sRGB by the interpreter's colour conversion (ICC, CMYK, Lab,
//!   separations …), with the constant alpha as `fill-opacity` / `stroke-opacity`.
//! - Clipping paths become nested `<clipPath>` groups; form XObjects are flattened by the
//!   interpreter (its nesting depth and paint budget bound looping forms).
//! - Text is drawn as glyph outlines: each glyph's outline is defined once and placed with `<use>`;
//!   fonts are not embedded. An optional transparent `<text>` layer, one element per line of the
//!   extracted reading-order text, keeps the page searchable and selectable.
//! - Images are decoded and re-encoded as base64 PNG, with their soft mask as alpha, downsampled
//!   above [`MAX_IMAGE_PIXELS`].
//! - Tiling patterns become SVG `<pattern>`s (vector). Shadings are rasterised over the painted
//!   region (at most 2 px/pt and [`MAX_SHADING_PIXELS`]) and masked by the painted shape.
//!   Soft masks become SVG `<mask>`s (luminosity or alpha); transfer functions are ignored.
//!   Blend modes become CSS `mix-blend-mode`.
//! - Output is bounded by [`MAX_SVG_BYTES`]; past it, further content is left out and the page
//!   is reported truncated (the document stays well-formed).

use std::collections::HashMap;
use std::fmt::Write as _;

use base64::Engine as _;
use hayro::hayro_interpret::TransformExt as _;
use hayro::hayro_interpret::font::Glyph;
use hayro::hayro_interpret::hayro_syntax::Pdf;
use hayro::hayro_interpret::pattern::{Pattern, ShadingPattern};
use hayro::hayro_interpret::{
    BlendMode, CacheKey, ClipPath, Context, Device, FillRule, GlyphDrawMode, Image, ImageData, InterpreterCache, InterpreterSettings, LumaData,
    MaskType, Paint, PathDrawMode, SoftMask, StrokeProps, interpret_page,
};
use kurbo::{Affine, BezPath, PathEl, Point, Rect, Shape};

/// The most SVG text one page may produce (bytes); later content is left out.
pub const MAX_SVG_BYTES: usize = 128 << 20;
/// Images with more pixels are downsampled (box filter) to at most this many.
pub const MAX_IMAGE_PIXELS: u64 = 4 << 20;
/// The most pixels one rasterised shading may have.
pub const MAX_SHADING_PIXELS: u64 = 1 << 20;
/// The most shading samples one page may compute.
const MAX_SHADING_SAMPLES: u64 = 16 << 20;
/// Patterns and masks nested deeper than this are left out.
const MAX_DEPTH: u32 = 8;
/// Clips and groups open at once beyond this are not written.
const MAX_OPEN: usize = 4096;
/// Dash arrays longer than this are drawn solid.
const MAX_DASHES: usize = 256;
/// Coordinates beyond this magnitude are not written (the element is skipped).
const MAX_COORDINATE: f64 = 1e9;

/// What to write.
#[derive(Clone, Debug)]
pub struct SvgOptions {
    /// Add a transparent `<text>` layer with the page's text, for searching and selecting.
    pub text_layer: bool,
    /// Leave the page background transparent instead of painting it white.
    pub transparent: bool,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self { text_layer: true, transparent: false }
    }
}

/// What a page's SVG holds, and what it could not express.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SvgStats {
    /// Filled or stroked paths (glyphs and shadings not included).
    pub paths: usize,
    /// Glyphs drawn as outlines.
    pub glyphs: usize,
    /// Embedded images (PNG).
    pub images: usize,
    /// Images downsampled to [`MAX_IMAGE_PIXELS`].
    pub images_downsampled: usize,
    pub clips: usize,
    /// Transparency groups (`<g>` with opacity, blend mode or mask).
    pub groups: usize,
    /// Tiling patterns written as SVG `<pattern>`.
    pub tiling_patterns: usize,
    /// Shadings drawn as a rasterised image of the painted region.
    pub shadings_rasterized: usize,
    pub soft_masks: usize,
    /// Elements drawn with a CSS blend mode.
    pub blend_modes: usize,
    /// Lines in the transparent text layer.
    pub text_lines: usize,
    /// Content that was left out: images that could not be decoded or encoded, stencil masks
    /// painted with a pattern, patterns or masks nested too deeply, shadings beyond the page's
    /// sampling budget.
    pub omitted: usize,
    /// Soft-mask transfer functions, which are ignored (the mask is used as is).
    pub mask_transfer_functions_ignored: usize,
    /// The page reached [`MAX_SVG_BYTES`]; content after that point is missing.
    pub truncated: bool,
}

/// One page as SVG.
#[derive(Clone, Debug)]
pub struct SvgPage {
    pub svg: String,
    /// The displayed page size in points (the SVG `viewBox`).
    pub width: f32,
    pub height: f32,
    pub stats: SvgStats,
}

/// State shared by the page device and the devices that write patterns and masks.
struct Shared {
    next_id: u64,
    bytes: usize,
    stats: SvgStats,
    /// Soft masks already written, by cache key.
    masks: HashMap<u128, String>,
    /// Glyph outlines already written, by glyph identity.
    glyphs: HashMap<u128, Option<String>>,
    page: Rect,
    depth: u32,
    samples: u64,
}

impl Shared {
    fn id(&mut self, prefix: &str) -> String {
        self.next_id = self.next_id.saturating_add(1);
        format!("{prefix}{}", self.next_id)
    }

    /// Reserve `len` bytes of output; `false` (and truncated) once the page budget is spent.
    fn reserve(&mut self, len: usize) -> bool {
        match self.bytes.checked_add(len) {
            Some(total) if total <= MAX_SVG_BYTES && !self.stats.truncated => {
                self.bytes = total;
                true
            }
            _ => {
                self.stats.truncated = true;
                false
            }
        }
    }
}

struct SvgDevice<'s> {
    shared: &'s mut Shared,
    out: String,
    /// For each clip or group pushed and not yet popped: whether its `<g>` was written (it is
    /// not once the budget is spent). Pushes beyond [`MAX_OPEN`] are only counted.
    opened: Vec<bool>,
    overflow: usize,
    /// The soft mask in effect for the next elements (`mask="url(#…)"`).
    mask: Option<String>,
    blend: BlendMode,
}

/// Which painting attribute a paint sets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Fill,
    Stroke,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Fill => "fill",
            Kind::Stroke => "stroke",
        }
    }
}

impl<'s> SvgDevice<'s> {
    fn new(shared: &'s mut Shared) -> Self {
        Self { shared, out: String::new(), opened: Vec::new(), overflow: 0, mask: None, blend: BlendMode::Normal }
    }

    /// Append `text` if the page budget allows.
    fn emit(&mut self, text: &str) -> bool {
        if self.shared.reserve(text.len()) {
            self.out.push_str(text);
            true
        } else {
            false
        }
    }

    /// Close every group still open and return the markup.
    fn finish(mut self) -> String {
        for opened in std::mem::take(&mut self.opened) {
            // Closing tags are always written so the document stays well-formed; their number
            // is bounded by the opening tags that fit the budget.
            if opened {
                self.out.push_str("</g>");
            }
        }
        self.out
    }

    /// `mask` and `mix-blend-mode` of the graphics state, for an element.
    fn state_attributes(&mut self) -> String {
        let mut s = String::new();
        if let Some(mask) = &self.mask {
            let _ = write!(s, " mask=\"url(#{mask})\"");
        }
        if let Some(mode) = blend_name(self.blend) {
            self.shared.stats.blend_modes = self.shared.stats.blend_modes.saturating_add(1);
            let _ = write!(s, " style=\"mix-blend-mode:{mode}\"");
        }
        s
    }

    /// Draw one shape (`tag` with geometry `attributes`) with `paint` as its `kind`.
    /// `transform` maps the element's coordinates to page space; `bounds` is the page-space
    /// box of what it paints.
    fn shape(&mut self, tag: &str, attributes: &str, kind: Kind, paint: &Paint<'_>, transform: Affine, bounds: Rect) {
        if !overlaps(bounds, self.shared.page) {
            return;
        }
        let other = match kind {
            Kind::Fill => "",
            Kind::Stroke => " fill=\"none\"",
        };
        match paint {
            Paint::Color(c) => {
                let state = self.state_attributes();
                let colour = colour_attributes(kind, c.to_rgba().components());
                self.emit(&format!("<{tag}{attributes}{other}{colour}{state}/>"));
            }
            Paint::Pattern(p) => match p.as_ref() {
                Pattern::Tiling(t) => {
                    let Some(id) = self.tiling(t, kind, transform) else { return };
                    let state = self.state_attributes();
                    self.emit(&format!("<{tag}{attributes}{other} {}=\"url(#{id})\"{state}/>", kind.name()));
                }
                Pattern::Shading(s) => {
                    let shape = format!("<{tag}{attributes}{other} {}=\"#fff\"/>", kind.name());
                    self.shading(s, &shape, bounds);
                }
            },
        }
    }

    /// Write a tiling pattern's `<pattern>` and return its id. `transform` is the element's.
    fn tiling(&mut self, t: &hayro::hayro_interpret::pattern::TilingPattern<'_>, kind: Kind, transform: Affine) -> Option<String> {
        let (w, h) = (f64::from(t.x_step).abs(), f64::from(t.y_step).abs());
        if self.shared.depth >= MAX_DEPTH || !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) || !finite_rect(t.bbox) {
            self.shared.stats.omitted = self.shared.stats.omitted.saturating_add(1);
            return None;
        }
        // Pattern space → page space is the pattern matrix; the element's user space is
        // `transform` from page space.
        let inverse = invertible(transform)?.inverse();
        let pattern_transform = inverse * t.matrix;
        if !finite_affine(pattern_transform) {
            self.shared.stats.omitted = self.shared.stats.omitted.saturating_add(1);
            return None;
        }
        let id = self.shared.id("p");
        self.shared.depth += 1;
        let body = {
            let mut cell = SvgDevice::new(self.shared);
            t.interpret(&mut cell, Affine::IDENTITY, kind == Kind::Stroke);
            cell.finish()
        };
        self.shared.depth -= 1;
        let head = format!(
            "<defs><pattern id=\"{id}\" patternUnits=\"userSpaceOnUse\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" patternTransform=\"{}\">",
            num(t.bbox.x0),
            num(t.bbox.y0),
            num(w),
            num(h),
            matrix(pattern_transform)
        );
        // The cell's markup was counted when it was written.
        if !self.emit(&head) {
            return None;
        }
        self.out.push_str(&body);
        self.out.push_str("</pattern></defs>");
        self.shared.stats.tiling_patterns = self.shared.stats.tiling_patterns.saturating_add(1);
        Some(id)
    }

    /// Rasterise shading `s` over `bounds` (page space) and draw it through `shape` as a mask.
    fn shading(&mut self, s: &ShadingPattern, shape: &str, bounds: Rect) {
        let area = bounds.intersect(self.shared.page);
        if area.is_zero_area() || !finite_rect(area) {
            return;
        }
        let (w, h) = (area.width(), area.height());
        let scale = (MAX_SHADING_PIXELS as f64 / (w * h)).sqrt().min(2.0);
        if !(scale.is_finite() && scale > 0.0) {
            return;
        }
        let px_w = ((w * scale).ceil() as u64).clamp(1, 4096);
        let px_h = ((h * scale).ceil() as u64).clamp(1, 4096);
        let pixels = px_w.saturating_mul(px_h);
        if pixels > MAX_SHADING_PIXELS.saturating_mul(2) || self.shared.samples.saturating_add(pixels) > MAX_SHADING_SAMPLES {
            self.shared.stats.omitted = self.shared.stats.omitted.saturating_add(1);
            return;
        }
        self.shared.samples = self.shared.samples.saturating_add(pixels);
        let (sx, sy) = (px_w as f64 / w, px_h as f64 / h);
        // Page space → raster pixels, prepended to the pattern matrix (pattern → page).
        let to_pixels = Affine::scale_non_uniform(sx, sy) * Affine::translate((-area.x0, -area.y0));
        let mut pattern = s.clone();
        pattern.matrix = to_pixels * s.matrix;
        let encoded = pattern.encode_within(Some(Rect::new(-1.0, -1.0, px_w as f64 + 1.0, px_h as f64 + 1.0)));
        let Ok(len) = usize::try_from(pixels.saturating_mul(4)) else { return };
        let mut rgba = Vec::with_capacity(len);
        for y in 0..px_h {
            for x in 0..px_w {
                let p = encoded.base_transform * Point::new(x as f64 + 0.5, y as f64 + 0.5);
                let c = encoded.sample(p);
                rgba.extend(c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
            }
        }
        let (Ok(pw), Ok(ph)) = (u32::try_from(px_w), u32::try_from(px_h)) else { return };
        let Some(png) = encode_png(pw, ph, &rgba) else {
            self.shared.stats.omitted = self.shared.stats.omitted.saturating_add(1);
            return;
        };
        let mask = self.shared.id("s");
        let page = self.shared.page;
        let state = self.state_attributes();
        let image_transform = Affine::translate((area.x0, area.y0)) * Affine::scale_non_uniform(1.0 / sx, 1.0 / sy);
        let markup = format!(
            "<mask id=\"{mask}\" maskUnits=\"userSpaceOnUse\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\">{shape}</mask><g{state}><image mask=\"url(#{mask})\" width=\"{pw}\" height=\"{ph}\" preserveAspectRatio=\"none\" transform=\"{}\" xlink:href=\"data:image/png;base64,{}\"/></g>",
            num(page.x0 - 1.0),
            num(page.y0 - 1.0),
            num(page.width() + 2.0),
            num(page.height() + 2.0),
            matrix(image_transform),
            base64::engine::general_purpose::STANDARD.encode(png),
        );
        if self.emit(&markup) {
            self.shared.stats.shadings_rasterized = self.shared.stats.shadings_rasterized.saturating_add(1);
        }
    }

    /// The id of soft mask `m`'s `<mask>`, written on first use.
    fn soft_mask(&mut self, m: &SoftMask<'_>) -> Option<String> {
        let key = m.cache_key();
        if let Some(id) = self.shared.masks.get(&key) {
            return Some(id.clone());
        }
        if self.shared.depth >= MAX_DEPTH {
            self.shared.stats.omitted = self.shared.stats.omitted.saturating_add(1);
            return None;
        }
        if m.transfer_function().is_some() {
            self.shared.stats.mask_transfer_functions_ignored = self.shared.stats.mask_transfer_functions_ignored.saturating_add(1);
        }
        let id = self.shared.id("m");
        let page = self.shared.page.inflate(1.0, 1.0);
        let (kind, backdrop) = match m.mask_type() {
            MaskType::Luminosity => {
                let [r, g, b, _] = m.background_color().to_rgba().components();
                // Outside the group the mask is the backdrop colour's luminosity; black (the
                // default) is what an SVG mask has there anyway.
                let backdrop = if r + g + b > 0.0 {
                    format!(
                        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\"/>",
                        num(page.x0),
                        num(page.y0),
                        num(page.width()),
                        num(page.height()),
                        hex([r, g, b])
                    )
                } else {
                    String::new()
                };
                ("", backdrop)
            }
            MaskType::Alpha => (" style=\"mask-type:alpha\"", String::new()),
        };
        self.shared.depth += 1;
        let body = {
            let mut content = SvgDevice::new(self.shared);
            m.interpret(&mut content);
            content.finish()
        };
        self.shared.depth -= 1;
        let head = format!(
            "<defs><mask id=\"{id}\" maskUnits=\"userSpaceOnUse\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" color-interpolation=\"sRGB\"{kind}>{backdrop}",
            num(page.x0),
            num(page.y0),
            num(page.width()),
            num(page.height())
        );
        if !self.emit(&head) {
            return None;
        }
        self.out.push_str(&body);
        self.out.push_str("</mask></defs>");
        self.shared.stats.soft_masks = self.shared.stats.soft_masks.saturating_add(1);
        self.shared.masks.insert(key, id.clone());
        Some(id)
    }

    /// The id of an outline glyph's `<path>` in glyph space, written on first use; `None` for
    /// a glyph without an outline (a space).
    fn glyph_outline(&mut self, glyph: &hayro::hayro_interpret::font::OutlineGlyph) -> Option<String> {
        let key = glyph.identifier().cache_key();
        if let Some(known) = self.shared.glyphs.get(&key) {
            return known.clone();
        }
        let outline = glyph.outline();
        let id = if outline.elements().is_empty() {
            None
        } else {
            let d = path_data(&outline, Affine::IDENTITY)?;
            let id = self.shared.id("g");
            if !self.emit(&format!("<defs><path id=\"{id}\" d=\"{d}\"/></defs>")) {
                return None;
            }
            Some(id)
        };
        self.shared.glyphs.insert(key, id.clone());
        id
    }

    fn stroke_attributes(&self, props: &StrokeProps, transform: Affine) -> String {
        let mut s = String::new();
        if props.line_width > 0.0 && props.line_width.is_finite() {
            let _ = write!(s, " stroke-width=\"{}\"", num(f64::from(props.line_width)));
        } else {
            // A zero width is the thinnest line the device can draw (ISO 32000-2 §8.4.3.2).
            s.push_str(" stroke-width=\"1\" vector-effect=\"non-scaling-stroke\"");
        }
        match props.line_cap {
            kurbo::Cap::Butt => {}
            kurbo::Cap::Round => s.push_str(" stroke-linecap=\"round\""),
            kurbo::Cap::Square => s.push_str(" stroke-linecap=\"square\""),
        }
        match props.line_join {
            kurbo::Join::Miter => {
                let limit = if props.miter_limit.is_finite() { props.miter_limit.clamp(1.0, 1000.0) } else { 10.0 };
                let _ = write!(s, " stroke-miterlimit=\"{}\"", num(f64::from(limit)));
            }
            kurbo::Join::Round => s.push_str(" stroke-linejoin=\"round\""),
            kurbo::Join::Bevel => s.push_str(" stroke-linejoin=\"bevel\""),
        }
        let dashes = &props.dash_array;
        if !dashes.is_empty()
            && dashes.len() <= MAX_DASHES
            && dashes.iter().all(|d| d.is_finite() && *d >= 0.0)
            && dashes.iter().any(|d| *d > 0.0)
            && props.dash_offset.is_finite()
        {
            let list: Vec<String> = dashes.iter().map(|d| num(f64::from(*d))).collect();
            let _ = write!(s, " stroke-dasharray=\"{}\"", list.join(" "));
            if props.dash_offset != 0.0 {
                let _ = write!(s, " stroke-dashoffset=\"{}\"", num(f64::from(props.dash_offset)));
            }
        }
        if transform != Affine::IDENTITY {
            let _ = write!(s, " transform=\"{}\"", matrix(transform));
        }
        s
    }

    /// Stroke `path` (user space) with pen `transform`.
    fn stroke(&mut self, tag: &str, geometry: &str, path_bounds: Rect, transform: Affine, paint: &Paint<'_>, props: &StrokeProps) {
        if !finite_affine(transform) {
            return;
        }
        let attributes = format!("{geometry}{}", self.stroke_attributes(props, transform));
        let (a, b) = axis_lengths(transform);
        let reach = f64::from(props.line_width.max(0.0)) * a.max(b) + 1.0;
        let bounds = transform.transform_rect_bbox(path_bounds).inflate(reach, reach);
        self.shape(tag, &attributes, Kind::Stroke, paint, transform, bounds);
    }

    fn image(&mut self, rgba: Vec<u8>, width: u32, height: u32, transform: Affine) {
        let (rgba, w, h, downsampled) = match downsample(rgba, width, height) {
            Some(v) => v,
            None => {
                self.shared.stats.omitted = self.shared.stats.omitted.saturating_add(1);
                return;
            }
        };
        let transform = transform * Affine::scale_non_uniform(f64::from(width) / f64::from(w), f64::from(height) / f64::from(h));
        if !finite_affine(transform) {
            return;
        }
        let bounds = transform.transform_rect_bbox(Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
        if !overlaps(bounds, self.shared.page) {
            return;
        }
        // An image's encoded size is unknown until it is encoded: check the budget first with
        // the smallest it could be, so a page past its budget does not keep encoding images.
        if self.shared.stats.truncated || self.shared.bytes.saturating_add(64) > MAX_SVG_BYTES {
            self.shared.stats.truncated = true;
            return;
        }
        let Some(png) = encode_png(w, h, &rgba) else {
            self.shared.stats.omitted = self.shared.stats.omitted.saturating_add(1);
            return;
        };
        let state = self.state_attributes();
        let markup = format!(
            "<image width=\"{w}\" height=\"{h}\" preserveAspectRatio=\"none\" transform=\"{}\"{state} xlink:href=\"data:image/png;base64,{}\"/>",
            matrix(transform),
            base64::engine::general_purpose::STANDARD.encode(png)
        );
        if self.emit(&markup) {
            self.shared.stats.images = self.shared.stats.images.saturating_add(1);
            if downsampled {
                self.shared.stats.images_downsampled = self.shared.stats.images_downsampled.saturating_add(1);
            }
        }
    }
}

impl<'a> Device<'a> for SvgDevice<'_> {
    fn set_soft_mask(&mut self, mask: Option<SoftMask<'a>>) {
        self.mask = mask.and_then(|m| self.soft_mask(&m));
    }

    fn set_blend_mode(&mut self, blend_mode: BlendMode) {
        self.blend = blend_mode;
    }

    fn draw_path(&mut self, path: &BezPath, transform: Affine, paint: &Paint<'a>, draw_mode: &PathDrawMode) {
        if path.elements().is_empty() || !finite_affine(transform) {
            return;
        }
        match draw_mode {
            PathDrawMode::Fill(rule) => {
                let Some(d) = path_data(path, transform) else { return };
                let rule = if *rule == FillRule::EvenOdd { " fill-rule=\"evenodd\"" } else { "" };
                let bounds = transform.transform_rect_bbox(path.bounding_box());
                self.shared.stats.paths = self.shared.stats.paths.saturating_add(1);
                self.shape("path", &format!(" d=\"{d}\"{rule}"), Kind::Fill, paint, Affine::IDENTITY, bounds);
            }
            PathDrawMode::Stroke(props) => {
                let Some(d) = path_data(path, Affine::IDENTITY) else { return };
                self.shared.stats.paths = self.shared.stats.paths.saturating_add(1);
                self.stroke("path", &format!(" d=\"{d}\""), path.bounding_box(), transform, paint, props);
            }
        }
    }

    fn push_clip_path(&mut self, clip_path: &ClipPath) {
        // An empty or unwritable clip clips everything away.
        let d = path_data(&clip_path.path, Affine::IDENTITY).unwrap_or_default();
        let id = self.shared.id("c");
        let rule = if clip_path.fill == FillRule::EvenOdd { " clip-rule=\"evenodd\"" } else { "" };
        let opened = self.emit(&format!("<clipPath id=\"{id}\"><path d=\"{d}\"{rule}/></clipPath><g clip-path=\"url(#{id})\">"));
        if opened {
            self.shared.stats.clips = self.shared.stats.clips.saturating_add(1);
        }
        self.push_group(opened);
    }

    fn push_transparency_group(&mut self, opacity: f32, mask: Option<SoftMask<'a>>, blend_mode: BlendMode) {
        let mut attributes = String::new();
        if opacity.is_finite() && opacity < 1.0 {
            let _ = write!(attributes, " opacity=\"{}\"", self::opacity(opacity));
        }
        if let Some(id) = mask.and_then(|m| self.soft_mask(&m)) {
            let _ = write!(attributes, " mask=\"url(#{id})\"");
        }
        if let Some(mode) = blend_name(blend_mode) {
            self.shared.stats.blend_modes = self.shared.stats.blend_modes.saturating_add(1);
            let _ = write!(attributes, " style=\"mix-blend-mode:{mode}\"");
        }
        let opened = self.emit(&format!("<g{attributes}>"));
        if opened {
            self.shared.stats.groups = self.shared.stats.groups.saturating_add(1);
        }
        self.push_group(opened);
    }

    fn draw_glyph(&mut self, glyph: &Glyph<'a>, transform: Affine, glyph_transform: Affine, paint: &Paint<'a>, draw_mode: &GlyphDrawMode) {
        if matches!(draw_mode, GlyphDrawMode::Invisible) || !finite_affine(transform) || !finite_affine(glyph_transform) {
            return;
        }
        match glyph {
            Glyph::Outline(o) => {
                let Some(id) = self.glyph_outline(o) else { return };
                let bounds = o.outline().bounding_box();
                self.shared.stats.glyphs = self.shared.stats.glyphs.saturating_add(1);
                match draw_mode {
                    GlyphDrawMode::Stroke(props) => {
                        // The outline in text space, stroked with the CTM as the pen.
                        let outline = glyph_transform * o.outline();
                        let Some(d) = path_data(&outline, Affine::IDENTITY) else { return };
                        self.stroke("path", &format!(" d=\"{d}\""), outline.bounding_box(), transform, paint, props);
                    }
                    _ => {
                        let full = transform * glyph_transform;
                        if !finite_affine(full) {
                            return;
                        }
                        let geometry = format!(" xlink:href=\"#{id}\" transform=\"{}\"", matrix(full));
                        self.shape("use", &geometry, Kind::Fill, paint, Affine::IDENTITY, full.transform_rect_bbox(bounds));
                    }
                }
            }
            Glyph::Type3(t) => {
                self.shared.stats.glyphs = self.shared.stats.glyphs.saturating_add(1);
                t.interpret(self, transform, glyph_transform, paint);
            }
        }
    }

    fn draw_image(&mut self, image: Image<'a, '_>, transform: Affine) {
        if !finite_affine(transform) {
            return;
        }
        match image {
            Image::Stencil(s) => {
                let mut drawn = None;
                s.with_stencil(
                    |stencil, paint| {
                        drawn = Some(match paint {
                            Paint::Color(c) => {
                                let [r, g, b, a] = c.to_rgba().components();
                                let colour = [r, g, b].map(unit_byte);
                                stencil_rgba(&stencil, colour, a).map(|rgba| (rgba, stencil.width, stencil.height, stencil.scale_factors))
                            }
                            Paint::Pattern(_) => None,
                        });
                    },
                    None,
                );
                match drawn {
                    Some(Some((rgba, w, h, (fx, fy)))) => {
                        self.image(rgba, w, h, transform * Affine::scale_non_uniform(f64::from(fx), f64::from(fy)));
                    }
                    _ => self.shared.stats.omitted = self.shared.stats.omitted.saturating_add(1),
                }
            }
            Image::Raster(r) => {
                let mut drawn = None;
                r.with_rgba(
                    |data, alpha| {
                        drawn = Some(raster_rgba(&data, alpha.as_ref()).map(|rgba| (rgba, data.width(), data.height(), data.scale_factors())))
                    },
                    None,
                );
                match drawn {
                    Some(Some((rgba, w, h, (fx, fy)))) => {
                        self.image(rgba, w, h, transform * Affine::scale_non_uniform(f64::from(fx), f64::from(fy)));
                    }
                    _ => self.shared.stats.omitted = self.shared.stats.omitted.saturating_add(1),
                }
            }
        }
    }

    fn pop_clip_path(&mut self) {
        self.close_group();
    }

    fn pop_transparency_group(&mut self) {
        self.close_group();
    }
}

impl SvgDevice<'_> {
    fn push_group(&mut self, opened: bool) {
        if self.opened.len() < MAX_OPEN {
            self.opened.push(opened);
        } else {
            // Too deep to write; the matching pop only uncounts it.
            if opened {
                self.out.push_str("</g>");
            }
            self.overflow = self.overflow.saturating_add(1);
        }
    }

    fn close_group(&mut self) {
        if self.overflow > 0 {
            self.overflow -= 1;
        } else if self.opened.pop() == Some(true) {
            self.out.push_str("</g>");
        }
    }
}

/// Interpret page `page` (0-based) into an SVG document. Panics inside the interpreter are the
/// caller's to catch.
pub(crate) fn page_svg(pdf: &Pdf, page: usize, settings: &InterpreterSettings, options: &SvgOptions) -> Result<SvgPage, String> {
    let pages = pdf.pages();
    let p = pages.get(page).ok_or_else(|| format!("page {} does not exist", page.saturating_add(1)))?;
    let (w, h) = p.render_dimensions();
    if !(w.is_finite() && h.is_finite()) || w < 0.5 || h < 0.5 || w > 14_400_000.0 || h > 14_400_000.0 {
        return Err(format!("page {} has an empty or invalid page box ({w}×{h} pt)", page.saturating_add(1)));
    }
    let page_rect = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
    let mut shared = Shared {
        next_id: 0,
        bytes: 0,
        stats: SvgStats::default(),
        masks: HashMap::new(),
        glyphs: HashMap::new(),
        page: page_rect,
        depth: 0,
        samples: 0,
    };
    let body = {
        let cache = InterpreterCache::new();
        let initial = p.initial_transform(true).to_kurbo();
        let mut ctx = Context::new(initial, page_rect, &cache, p.xref(), settings.clone());
        let mut device = SvgDevice::new(&mut shared);
        interpret_page(p, &mut ctx, &mut device);
        device.finish()
    };
    let mut svg = String::with_capacity(body.len().saturating_add(1024));
    let _ = write!(
        svg,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" version=\"1.1\" width=\"{0}pt\" height=\"{1}pt\" viewBox=\"0 0 {0} {1}\">\n",
        num(f64::from(w)),
        num(f64::from(h))
    );
    if !options.transparent {
        let _ = writeln!(svg, "<rect width=\"{}\" height=\"{}\" fill=\"#fff\"/>", num(f64::from(w)), num(f64::from(h)));
    }
    svg.push_str("<g>");
    svg.push_str(&body);
    svg.push_str("</g>\n");
    if options.text_layer
        && let Some(text) = crate::text::extract_page(pdf, page, settings)
    {
        let layer = text_layer(&text, &mut shared);
        svg.push_str(&layer);
    }
    svg.push_str("</svg>\n");
    Ok(SvgPage { svg, width: w, height: h, stats: shared.stats })
}

/// One transparent `<text>` per line, stretched over the line's box.
fn text_layer(text: &crate::text::PageText, shared: &mut Shared) -> String {
    let mut out = String::from("<g fill=\"#000\" fill-opacity=\"0\" font-family=\"sans-serif\" aria-hidden=\"false\">");
    let mut start = 0usize;
    let n = text.glyphs.len().min(text.line_of.len());
    while start < n {
        let line = text.line_of.get(start).copied();
        let mut end = start.saturating_add(1);
        while end < n && text.line_of.get(end).copied() == line {
            end = end.saturating_add(1);
        }
        let mut rect: Option<[f32; 4]> = None;
        for g in text.glyphs.get(start..end).unwrap_or_default() {
            let r = g.rect;
            rect = Some(match rect {
                None => r,
                Some(a) => [a[0].min(r[0]), a[1].min(r[1]), a[2].max(r[2]), a[3].max(r[3])],
            });
        }
        let content = text.text_of(start..end);
        start = end;
        let Some([x0, y0, x1, y1]) = rect else { continue };
        let content = content.trim();
        let (width, height) = (f64::from(x1 - x0), f64::from(y1 - y0));
        if content.is_empty() || !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
            continue;
        }
        let size = height * 0.8;
        let element = format!(
            "<text x=\"{}\" y=\"{}\" font-size=\"{}\" textLength=\"{}\" lengthAdjust=\"spacingAndGlyphs\">{}</text>",
            num(f64::from(x0)),
            num(f64::from(y1) - height * 0.2),
            num(size),
            num(width),
            escape(content)
        );
        if !shared.reserve(element.len()) {
            break;
        }
        out.push_str(&element);
        shared.stats.text_lines = shared.stats.text_lines.saturating_add(1);
    }
    out.push_str("</g>\n");
    out
}

/// XML character data / attribute text: markup characters escaped, characters XML 1.0 forbids
/// left out.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

fn blend_name(mode: BlendMode) -> Option<&'static str> {
    Some(match mode {
        BlendMode::Normal => return None,
        BlendMode::Multiply => "multiply",
        BlendMode::Screen => "screen",
        BlendMode::Overlay => "overlay",
        BlendMode::Darken => "darken",
        BlendMode::Lighten => "lighten",
        BlendMode::ColorDodge => "color-dodge",
        BlendMode::ColorBurn => "color-burn",
        BlendMode::HardLight => "hard-light",
        BlendMode::SoftLight => "soft-light",
        BlendMode::Difference => "difference",
        BlendMode::Exclusion => "exclusion",
        BlendMode::Hue => "hue",
        BlendMode::Saturation => "saturation",
        BlendMode::Color => "color",
        BlendMode::Luminosity => "luminosity",
    })
}

fn unit_byte(v: f32) -> u8 {
    if v.is_finite() { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8 } else { 0 }
}

fn hex(rgb: [f32; 3]) -> String {
    let [r, g, b] = rgb.map(unit_byte);
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn colour_attributes(kind: Kind, [r, g, b, a]: [f32; 4]) -> String {
    let name = kind.name();
    let mut s = format!(" {name}=\"{}\"", hex([r, g, b]));
    if a.is_finite() && a < 1.0 {
        let _ = write!(s, " {name}-opacity=\"{}\"", opacity(a));
    }
    s
}

/// An opacity to two decimals (the interpreter hands them over quantised to 8 bits, so 0.5
/// arrives as 128/255).
fn opacity(a: f32) -> String {
    num((f64::from(a.clamp(0.0, 1.0)) * 100.0).round() / 100.0)
}

/// A number for SVG: the shortest `f32` spelling (no exponent), `0` for non-finite values.
fn num(v: f64) -> String {
    let v = v as f32;
    if !v.is_finite() {
        return "0".into();
    }
    let v = if v == 0.0 { 0.0 } else { v };
    format!("{v}")
}

fn matrix(t: Affine) -> String {
    let [a, b, c, d, e, f] = t.as_coeffs();
    format!("matrix({} {} {} {} {} {})", num(a), num(b), num(c), num(d), num(e), num(f))
}

fn finite_affine(t: Affine) -> bool {
    t.as_coeffs().iter().all(|v| v.is_finite() && v.abs() <= MAX_COORDINATE)
}

fn finite_rect(r: Rect) -> bool {
    [r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite())
}

fn overlaps(a: Rect, b: Rect) -> bool {
    finite_rect(a) && a.x0 <= b.x1 + 1.0 && a.x1 >= b.x0 - 1.0 && a.y0 <= b.y1 + 1.0 && a.y1 >= b.y0 - 1.0
}

fn invertible(t: Affine) -> Option<Affine> {
    let det = t.determinant();
    (det.is_finite() && det.abs() > 1e-12).then_some(t)
}

/// The lengths of the transformed unit axes.
fn axis_lengths(t: Affine) -> (f64, f64) {
    let [a, b, c, d, _, _] = t.as_coeffs();
    (a.hypot(b), c.hypot(d))
}

/// SVG path data for `path` mapped by `transform`; `None` if a coordinate is out of range.
fn path_data(path: &BezPath, transform: Affine) -> Option<String> {
    let mut d = String::new();
    let point = |d: &mut String, p: Point| -> Option<()> {
        let p = transform * p;
        if !(p.x.is_finite() && p.y.is_finite()) || p.x.abs() > MAX_COORDINATE || p.y.abs() > MAX_COORDINATE {
            return None;
        }
        let _ = write!(d, "{} {}", num(p.x), num(p.y));
        Some(())
    };
    for el in path.elements() {
        if !d.is_empty() {
            d.push(' ');
        }
        match *el {
            PathEl::MoveTo(p) => {
                d.push('M');
                point(&mut d, p)?;
            }
            PathEl::LineTo(p) => {
                d.push('L');
                point(&mut d, p)?;
            }
            PathEl::QuadTo(a, b) => {
                d.push('Q');
                point(&mut d, a)?;
                d.push(' ');
                point(&mut d, b)?;
            }
            PathEl::CurveTo(a, b, c) => {
                d.push('C');
                point(&mut d, a)?;
                d.push(' ');
                point(&mut d, b)?;
                d.push(' ');
                point(&mut d, c)?;
            }
            PathEl::ClosePath => d.push('Z'),
        }
    }
    Some(d)
}

/// Straight RGBA of a decoded raster image with its alpha (resampled to the image's size when
/// the mask has another one). `None` when the data does not match its dimensions.
fn raster_rgba(data: &ImageData, alpha: Option<&LumaData>) -> Option<Vec<u8>> {
    let (w, h) = (data.width(), data.height());
    let pixels = usize::try_from(u64::from(w).checked_mul(u64::from(h))?).ok()?;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(pixels.checked_mul(4)?).ok()?;
    match data {
        ImageData::Rgb(d) => {
            if d.data.len() != pixels.checked_mul(3)? {
                return None;
            }
            for px in d.data.as_chunks::<3>().0 {
                rgba.extend_from_slice(px);
                rgba.push(255);
            }
        }
        ImageData::Luma(d) => {
            if d.data.len() != pixels {
                return None;
            }
            for v in &d.data {
                rgba.extend_from_slice(&[*v, *v, *v, 255]);
            }
        }
    }
    if let Some(a) = alpha {
        let a_pixels = usize::try_from(u64::from(a.width).checked_mul(u64::from(a.height))?).ok()?;
        if a.data.len() != a_pixels || a_pixels == 0 {
            return Some(rgba);
        }
        let (w64, h64) = (u64::from(w.max(1)), u64::from(h.max(1)));
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = ((i as u64) % w64, (i as u64) / w64);
            let ax = x.saturating_mul(u64::from(a.width)) / w64;
            let ay = y.saturating_mul(u64::from(a.height)) / h64;
            let index = usize::try_from(ay.saturating_mul(u64::from(a.width)).saturating_add(ax)).ok()?;
            if let (Some(v), Some(slot)) = (a.data.get(index), px.get_mut(3)) {
                *slot = *v;
            }
        }
    }
    Some(rgba)
}

/// A stencil mask painted in `colour` at opacity `alpha`: RGBA with the stencil as alpha.
fn stencil_rgba(stencil: &LumaData, colour: [u8; 3], alpha: f32) -> Option<Vec<u8>> {
    let pixels = usize::try_from(u64::from(stencil.width).checked_mul(u64::from(stencil.height))?).ok()?;
    if stencil.data.len() != pixels {
        return None;
    }
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(pixels.checked_mul(4)?).ok()?;
    let alpha = if alpha.is_finite() { alpha.clamp(0.0, 1.0) } else { 1.0 };
    for v in &stencil.data {
        rgba.extend_from_slice(&colour);
        rgba.push((f32::from(*v) * alpha + 0.5) as u8);
    }
    Some(rgba)
}

/// Box-filter `rgba` (`w` × `h`) down to at most [`MAX_IMAGE_PIXELS`]: the pixels, their size,
/// and whether they were reduced. `None` for an empty or inconsistent image.
fn downsample(rgba: Vec<u8>, w: u32, h: u32) -> Option<(Vec<u8>, u32, u32, bool)> {
    let pixels = u64::from(w).checked_mul(u64::from(h))?;
    if pixels == 0 || usize::try_from(pixels.checked_mul(4)?).ok()? != rgba.len() {
        return None;
    }
    if pixels <= MAX_IMAGE_PIXELS {
        return Some((rgba, w, h, false));
    }
    let factor = ((pixels as f64 / MAX_IMAGE_PIXELS as f64).sqrt().ceil() as u32).max(2);
    let (nw, nh) = (w.div_ceil(factor).max(1), h.div_ceil(factor).max(1));
    let mut out = Vec::new();
    out.try_reserve_exact(usize::try_from(u64::from(nw) * u64::from(nh) * 4).ok()?).ok()?;
    let stride = usize::try_from(w).ok()?.checked_mul(4)?;
    for ny in 0..nh {
        for nx in 0..nw {
            let mut sum = [0u64; 4];
            let mut count = 0u64;
            let (y0, x0) = (ny.saturating_mul(factor), nx.saturating_mul(factor));
            for y in y0..y0.saturating_add(factor).min(h) {
                for x in x0..x0.saturating_add(factor).min(w) {
                    let i = (y as usize).saturating_mul(stride).saturating_add((x as usize).saturating_mul(4));
                    if let Some(px) = rgba.get(i..i.saturating_add(4)) {
                        for (s, v) in sum.iter_mut().zip(px) {
                            *s += u64::from(*v);
                        }
                        count += 1;
                    }
                }
            }
            let count = count.max(1);
            out.extend(sum.map(|s| (s / count) as u8));
        }
    }
    Some((out, nw, nh, true))
}

/// Straight RGBA → PNG.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

#[cfg(test)]
mod tests;
