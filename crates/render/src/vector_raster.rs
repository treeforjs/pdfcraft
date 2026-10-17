//! Native vector interpretation and a CPU device. The bootstrap syntax reader still supplies
//! page dictionaries/streams; every unsupported feature falls back for the entire page.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::util::{RectExt, TransformExt};
use hayro::hayro_syntax::{
    object::{Array, Name, Object, Stream},
    page::Page,
};
use hayro::vello_cpu::{RenderContext, RenderMode, Resources, color::AlphaColor, color::Srgb, peniko::Fill};
use kurbo::{Affine, BezPath, Join, PathEl, Point, Rect, Shape, Stroke};

use crate::Pixels;
use crate::display_list::{self, Clip, DisplayList, Draw, FillRule, Paint, Unsupported};

mod decode;

/// Device work one native raster may plan, in device pixels of outline: control-polygon length
/// of fills, of every clip each time it is pushed and of stroke outlines (both sides, joins,
/// caps and every dash piece). Over it, the page uses the bootstrap renderer.
const MAX_DEVICE_WORK: f64 = 4_000_000.0;
/// Largest coordinate, in device pixels (or the stroke's uniformly scaled pen space), handed
/// to the rasterizer. Paths beyond it fall back instead of meeting f32 precision limits.
const MAX_COORDINATE: f64 = 1e6;
/// Each dash piece becomes an open subpath of its own, with two caps and its own outline.
const DASH_PIECE_COST: f64 = 16.0;
/// Pixels added around a path's control-point box before deciding it misses the canvas:
/// antialiasing and flattening never reach that far past the outline.
const CULL_SLACK: f64 = 2.0;

/// One bounded display list per worker/parser generation. A negative result is cached too,
/// so zooming or rendering successive tiles of a text/image page does not reinterpret it.
#[derive(Default)]
pub(crate) struct Cache {
    page: RefCell<Option<(usize, Option<DisplayList>)>>,
    rasterized: Cell<u64>,
    /// Requests that reached the native path at all (tests prove the default never does).
    attempted: Cell<u64>,
}

type Result<T> = std::result::Result<T, Unsupported>;

fn refused(reason: &str) -> Unsupported {
    Unsupported(reason.into())
}

fn cancelled(settings: &InterpreterSettings) -> bool {
    settings.cancelled.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
}

fn compile_page(page: &Page<'_>, settings: &InterpreterSettings) -> Result<DisplayList> {
    let raw = page.raw();
    // Annotations (including widgets), transparency groups and replacement device spaces
    // require more than the vector content stream. Never quietly omit their appearances.
    if [b"Annots".as_slice(), b"Group", b"UserUnit"].iter().any(|k| raw.contains_key(k)) {
        return Err(refused("page needs annotation, group or user-unit handling"));
    }
    for name in [b"DefaultGray".as_slice(), b"DefaultRGB"] {
        if page.resources().get_color_space(&Name::new_unescaped(name)).is_some() {
            return Err(refused("page replaces a device color space"));
        }
    }
    let mut content = Vec::new();
    let mut encoded_left = decode::MAX_ENCODED_PAGE;
    let mut append = |stream: Stream<'_>| -> Result<()> {
        if cancelled(settings) {
            return Err(refused("request cancelled"));
        }
        let left = display_list::MAX_CONTENT.saturating_sub(content.len());
        let data = decode::stream(&stream, left, &mut encoded_left)?;
        if data.len().saturating_add(1) > left {
            return Err(refused("decoded content budget exceeded"));
        }
        content.extend_from_slice(&data);
        content.push(b'\n');
        Ok(())
    };
    if let Some(stream) = raw.get::<Stream<'_>>(b"Contents") {
        append(stream)?;
    } else if let Some(array) = raw.get::<Array<'_>>(b"Contents") {
        let expected = array.raw_iter().take(257).count();
        if expected > 256 {
            return Err(refused("too many content streams"));
        }
        let mut read = 0;
        for object in array.iter::<Object<'_>>().take(256) {
            append(object.into_stream().ok_or_else(|| refused("invalid content stream array"))?)?;
            read += 1;
        }
        // `iter` skips entries that do not resolve; a page missing one must not look complete.
        if read != expected {
            return Err(refused("unresolved content stream array entry"));
        }
    } else if raw.contains_key(b"Contents") {
        return Err(refused("invalid page content"));
    }
    display_list::compile(&content, || cancelled(settings))
}

fn fill(rule: FillRule) -> Fill {
    match rule {
        FillRule::NonZero => Fill::NonZero,
        FillRule::EvenOdd => Fill::EvenOdd,
    }
}

/// A path's control polygon under a transform. The polygon bounds each Bézier's arc length
/// and its box bounds the curve, so both are safe upper bounds for flattening work and extent.
struct Extent {
    length: f64,
    elements: usize,
    /// `MoveTo`s and `ClosePath`s: kurbo restarts a dash pattern at each.
    restarts: usize,
    bounds: Rect,
}

fn measure(path: &BezPath, transform: Affine) -> Result<Extent> {
    if transform.as_coeffs().iter().any(|x| !x.is_finite()) {
        return Err(refused("invalid raster transform"));
    }
    let mut extent =
        Extent { length: 0.0, elements: 0, restarts: 0, bounds: Rect::new(f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY) };
    let mut previous = Point::ZERO;
    let mut start = Point::ZERO;
    for element in path.elements() {
        extent.elements += 1;
        let mut point = |p: Point, edge: bool| -> Result<()> {
            let p = transform * p;
            if !p.x.is_finite() || !p.y.is_finite() || p.x.abs() > MAX_COORDINATE || p.y.abs() > MAX_COORDINATE {
                return Err(refused("raster coordinates exceed the device budget"));
            }
            if edge {
                extent.length += previous.distance(p);
            }
            extent.bounds = extent.bounds.union_pt(p);
            previous = p;
            Ok(())
        };
        match *element {
            PathEl::MoveTo(p) => {
                point(p, false)?;
                start = previous;
                extent.restarts += 1;
            }
            PathEl::LineTo(p) => point(p, true)?,
            PathEl::QuadTo(a, b) => {
                point(a, true)?;
                point(b, true)?;
            }
            PathEl::CurveTo(a, b, c) => {
                point(a, true)?;
                point(b, true)?;
                point(c, true)?;
            }
            PathEl::ClosePath => {
                extent.length += previous.distance(start);
                previous = start;
                extent.restarts += 1;
            }
        }
        if extent.length > MAX_DEVICE_WORK {
            return Err(refused("raster path work exceeds the device budget"));
        }
    }
    Ok(extent)
}

fn misses(bounds: Rect, slack: f64, canvas: Rect) -> bool {
    // An empty path has an inverted box and paints nothing.
    !(bounds.x0 - slack < canvas.x1 && bounds.x1 + slack > canvas.x0 && bounds.y0 - slack < canvas.y1 && bounds.y1 + slack > canvas.y0)
}

/// How a stroke reaches the rasterizer. vello expands strokes in the coordinates it is given,
/// to a tolerance of `0.25 / max(|a|, |d|, 1)` of the transform it is given (vello_common's
/// `flatten::stroke`), which in the PDF pen space itself depends on how the pen happens to be
/// scaled. The path is mapped into pen space scaled uniformly by the pen's larger column
/// length `k` instead: the outline is the same (stroking commutes with uniform scaling, with
/// width, dashes and phase scaled by `k`), the expansion tolerance is at most 0.25 device
/// pixels whatever the pen, and widths, dash pieces and the work estimate are all measured in
/// device-sized units. (kurbo approximates joins, caps and offsets with cubics, so stroking in
/// pen space was measured not to blow up either: a 1e-6 pen with 1e9-unit round joins took
/// about 1.5 ms on both devices.)
struct StrokeSetup {
    /// Default page space → scaled pen space.
    to_pen: Affine,
    /// Scaled pen space → device.
    to_device: Affine,
    style: Stroke,
}

fn stroke_setup(style: &Stroke, pen: Affine, transform: Affine, canvas: Rect) -> Result<StrokeSetup> {
    let device = transform * pen;
    let [a, b, c, d, _, _] = device.as_coeffs();
    let k = a.hypot(b).max(c.hypot(d));
    let pen_inverse = pen.inverse();
    if !k.is_finite()
        || k < 1e-9
        || !device.determinant().is_finite()
        || device.determinant().abs() < 1e-12
        || pen_inverse.as_coeffs().iter().any(|x| !x.is_finite())
    {
        return Err(refused("singular or extreme stroke pen"));
    }
    let to_pen = Affine::scale(k) * pen_inverse;
    let to_device = device * Affine::scale(1.0 / k);
    let mut scaled = style.clone();
    // As the bootstrap renderer does (and ISO 32000-2 §8.4.3.2 asks of a zero width): no
    // stroke is thinner than one device pixel, and one far wider than the canvas looks the
    // same as one four canvases wide.
    let widest = 4.0 * (canvas.width() + canvas.height());
    scaled.width = (style.width * k).max(1.0).min(widest);
    scaled.dash_offset = style.dash_offset * k;
    scaled.dash_pattern = style.dash_pattern.iter().map(|v| v * k).collect();
    if !scaled.width.is_finite() || !scaled.dash_offset.is_finite() || scaled.dash_pattern.iter().any(|v| !v.is_finite()) {
        return Err(refused("stroke pen exceeds the device budget"));
    }
    if !scaled.dash_pattern.is_empty() && scaled.dash_pattern.iter().sum::<f64>() <= 0.0 {
        return Err(refused("dash pattern vanishes on the device"));
    }
    if !scaled.dash_pattern.is_empty() {
        // A zero-length dash is a dot with round or square caps (ISO 32000-2 §8.4.3.6), but
        // kurbo emits it only when rounding happens to leave it a nonzero length. Give each
        // one a thousandth of a device pixel, taken from the gap after it so the period and
        // phase stay the same. An odd-length pattern is spelled out over its full period.
        const DOT: f64 = 1e-3;
        let mut pattern: Vec<f64> = scaled.dash_pattern.to_vec();
        if pattern.len() % 2 == 1 {
            pattern.extend_from_within(..);
        }
        for pair in pattern.as_chunks_mut::<2>().0 {
            if let [on, off] = pair
                && *on < DOT
                && *off > 2.0 * DOT
            {
                *on += DOT;
                *off -= DOT;
            }
        }
        scaled.dash_pattern = pattern.into_iter().collect();
    }
    Ok(StrokeSetup { to_pen, to_device, style: scaled })
}

/// Work for stroking a path whose control polygon in scaled pen space is `extent`.
fn stroke_work(extent: &Extent, style: &Stroke) -> f64 {
    let width = style.width;
    // Each vertex can carry a join (a miter reaches `limit × width / 2` each side; round joins
    // and caps are flattened arcs about `π × width / 2` long) and each subpath two caps.
    let join = if style.join == Join::Miter { style.miter_limit.max(2.0) } else { 2.0 };
    let mut work = 2.0 * extent.length + extent.elements as f64 * (1.0 + width * join);
    if !style.dash_pattern.is_empty() {
        let len = style.dash_pattern.len();
        let sum: f64 = style.dash_pattern.iter().sum();
        let (period, per_period) = if len % 2 == 1 { (2.0 * sum, 2 * len) } else { (sum, len) };
        let pieces = (extent.length / period + extent.restarts as f64) * per_period as f64;
        work += pieces * (DASH_PIECE_COST + 2.0 * width);
    }
    work
}

/// One rasterizer call, in order. Clips are pushed only when they differ from the ones already
/// pushed for the previous draw, as the PDF clipping path changes only at `W n`/`Q`.
enum Step<'a> {
    Pop,
    Push(&'a Clip),
    Fill(&'a Draw, FillRule),
    Stroke(&'a Draw, StrokeSetup),
}

/// Plans the whole raster and charges its work before anything is rasterized, so an
/// over-budget page falls back without having spent rasterization time.
fn plan<'a>(list: &'a DisplayList, transform: Affine, canvas: Rect, cancelled: impl Fn() -> bool) -> Result<Vec<Step<'a>>> {
    let mut work = 0.0;
    let mut charge = |amount: f64| -> Result<()> {
        work += amount;
        if !work.is_finite() || work > MAX_DEVICE_WORK { Err(refused("raster work exceeds the device budget")) } else { Ok(()) }
    };
    let mut steps = Vec::new();
    let mut active: Vec<&Clip> = Vec::new();
    // At most `MAX_CLIPS` distinct clipping paths exist, but each may be referenced by
    // thousands of draws: measure each once, by identity.
    let mut clip_extents: HashMap<*const BezPath, (f64, Rect)> = HashMap::new();
    for draw in &list.draws {
        if cancelled() {
            return Err(refused("request cancelled"));
        }
        let (paint, paint_work) = match &draw.paint {
            Paint::Fill { rule, .. } => {
                let extent = measure(&draw.path, transform)?;
                if misses(extent.bounds, CULL_SLACK, canvas) {
                    continue;
                }
                (Step::Fill(draw, *rule), extent.length + extent.elements as f64)
            }
            Paint::Stroke { style, pen, .. } => {
                let setup = stroke_setup(style, *pen, transform, canvas)?;
                let extent = measure(&draw.path, setup.to_pen)?;
                // The outline stays within half the width times the larger of the miter limit
                // and √2 (square caps) of the path, plus the 0.25-unit expansion tolerance.
                let reach = setup.style.width / 2.0 * setup.style.miter_limit.max(std::f64::consts::SQRT_2) + 1.0;
                let device = setup.to_device.transform_rect_bbox(extent.bounds.inflate(reach, reach));
                if extent.elements == 0 || misses(device, CULL_SLACK, canvas) {
                    continue;
                }
                let work = stroke_work(&extent, &setup.style);
                (Step::Stroke(draw, setup), work)
            }
        };
        let mut clip_work = Vec::with_capacity(draw.clips.len());
        let mut hidden = false;
        for clip in &draw.clips {
            let key = Arc::as_ptr(&clip.path);
            let (cost, bounds) = match clip_extents.get(&key) {
                Some(known) => *known,
                None => {
                    let extent = measure(&clip.path, transform)?;
                    let known = (extent.length + extent.elements as f64, extent.bounds);
                    clip_extents.insert(key, known);
                    known
                }
            };
            // Nothing inside a clip that misses the canvas can paint.
            hidden |= misses(bounds, CULL_SLACK, canvas);
            clip_work.push(cost);
        }
        if hidden {
            continue;
        }
        let keep = active.iter().zip(&draw.clips).take_while(|(a, b)| Arc::ptr_eq(&a.path, &b.path) && a.rule == b.rule).count();
        while active.len() > keep {
            active.pop();
            steps.push(Step::Pop);
        }
        for (clip, cost) in draw.clips.iter().zip(clip_work).skip(keep) {
            charge(cost)?;
            active.push(clip);
            steps.push(Step::Push(clip));
        }
        charge(paint_work)?;
        steps.push(paint);
    }
    steps.extend(active.iter().map(|_| Step::Pop));
    Ok(steps)
}

fn color(rgb: [f32; 3]) -> AlphaColor<Srgb> {
    AlphaColor::<Srgb>::new([rgb[0], rgb[1], rgb[2], 1.0])
}

fn raster(list: &DisplayList, page: &Page<'_>, settings: &InterpreterSettings, rs: &hayro::RenderSettings) -> Result<Pixels> {
    let (width, height) = hayro::render_size(page, rs);
    if width == 0 || height == 0 || width > 8192 || height > 8192 || u64::from(width) * u64::from(height) > 64_000_000 {
        return Err(refused("invalid raster dimensions"));
    }
    let transform = Affine::translate((-f64::from(rs.x_offset), -f64::from(rs.y_offset)))
        * Affine::scale_non_uniform(f64::from(rs.x_scale), f64::from(rs.y_scale))
        * page.initial_transform(true).to_kurbo();
    let canvas = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    let steps = plan(list, transform, canvas, || cancelled(settings))?;
    let mut context = RenderContext::new_with(width, height, hayro::vello_cpu::RenderSettings { num_threads: 0, ..Default::default() });
    context.set_paint(rs.bg_color);
    context.fill_rect(&canvas);
    context.set_transform(transform);
    context.set_fill_rule(Fill::NonZero);
    context.push_clip_path(&page.intersected_crop_box().to_kurbo().to_path(0.1));
    for step in steps {
        if cancelled(settings) {
            return Err(refused("request cancelled"));
        }
        match step {
            Step::Pop => context.pop_clip_path(),
            Step::Push(clip) => {
                context.set_transform(transform);
                context.set_fill_rule(fill(clip.rule));
                context.push_clip_path(&clip.path);
            }
            Step::Fill(draw, rule) => {
                let Paint::Fill { rgb, .. } = &draw.paint else { continue };
                context.set_transform(transform);
                context.set_paint(color(*rgb));
                context.set_fill_rule(fill(rule));
                context.fill_path(&draw.path);
            }
            Step::Stroke(draw, setup) => {
                let Paint::Stroke { rgb, .. } = &draw.paint else { continue };
                context.set_transform(setup.to_device);
                context.set_paint(color(*rgb));
                context.set_fill_rule(Fill::NonZero);
                context.set_stroke(setup.style);
                context.stroke_path(&(setup.to_pen * draw.path.as_ref().clone()));
            }
        }
    }
    context.pop_clip_path();
    context.flush();
    if cancelled(settings) {
        return Err(refused("request cancelled"));
    }
    let mut pixels = Pixels::zeroed(usize::from(width), usize::from(height)).ok_or_else(|| refused("raster allocation overflow"))?;
    context.render_to_buffer(&mut Resources::new(), pixels.bytes_mut(), width, height, RenderMode::OptimizeSpeed);
    Ok(pixels)
}

impl Cache {
    /// Native pixels for this page, or `None` to render it with the bootstrap renderer. A
    /// panic inside the native path is contained here: the page is remembered as unsupported
    /// and the bootstrap renderer still draws it.
    pub(crate) fn render(&self, page_index: usize, page: &Page<'_>, settings: &InterpreterSettings, rs: &hayro::RenderSettings) -> Option<Pixels> {
        self.attempted.set(self.attempted.get().saturating_add(1));
        if cancelled(settings) {
            return None;
        }
        match catch_unwind(AssertUnwindSafe(|| self.render_inner(page_index, page, settings, rs))) {
            Ok(pixels) => {
                if pixels.is_some() {
                    self.rasterized.set(self.rasterized.get().saturating_add(1));
                }
                pixels
            }
            Err(_) => {
                log::warn!("native vector rendering crashed on page {}; using the bootstrap renderer", page_index + 1);
                if let Ok(mut cached) = self.page.try_borrow_mut() {
                    *cached = Some((page_index, None));
                }
                None
            }
        }
    }

    fn render_inner(&self, page_index: usize, page: &Page<'_>, settings: &InterpreterSettings, rs: &hayro::RenderSettings) -> Option<Pixels> {
        let mut cached = self.page.borrow_mut();
        if cached.as_ref().is_none_or(|(index, _)| *index != page_index) {
            let list = compile_page(page, settings);
            // Do not retain a cancellation as a permanent unsupported-page decision.
            if cancelled(settings) {
                return None;
            }
            if let Err(reason) = &list {
                log::debug!("page {} uses the bootstrap renderer: {reason}", page_index + 1);
            }
            *cached = Some((page_index, list.ok()));
        }
        let list = cached.as_ref()?.1.as_ref()?;
        match raster(list, page, settings, rs) {
            Ok(pixels) => Some(pixels),
            Err(reason) => {
                log::debug!("page {} raster uses the bootstrap renderer: {reason}", page_index + 1);
                None
            }
        }
    }

    /// Pages this cache rasterized natively.
    pub(crate) fn rasterized(&self) -> u64 {
        self.rasterized.get()
    }

    /// Requests that reached the native path, whether or not it drew them.
    #[cfg(test)]
    pub(crate) fn attempted(&self) -> u64 {
        self.attempted.get()
    }
}

#[cfg(test)]
mod tests;
