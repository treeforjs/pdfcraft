//! Original, bounded PDF vector display-list compiler (ISO 32000-2 §8.4–8.5).
//!
//! Coordinates are mapped to default page space when path operators execute, and a stroke
//! keeps the CTM at painting as its pen. Conforming content cannot change the CTM between the
//! two (a page that does falls back). The current path is not part of q/Q state.
//! Unsupported content rejects the whole list; callers must never show a partially compiled
//! page. Text, images, forms, transparency and resource color spaces are subsequent stages.

use std::sync::Arc;

use kurbo::{Affine, BezPath, Cap, Join, PathEl, Point, Stroke};
use pdfcraft_content::Op;
use pdfcraft_cos::Object;

mod lexer;

pub(crate) const MAX_CONTENT: usize = 1024 * 1024;
const MAX_OPERATORS: usize = 100_000;
const MAX_SEGMENTS: usize = 100_000;
const MAX_DRAWS: usize = 25_000;
const MAX_STACK: usize = 256;
const MAX_CLIPS: usize = 256;
const MAX_CLIP_REFERENCES: usize = 65_536;

/// A whole-page fallback reason. No partially interpreted display list is returned.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("native vector interpretation: {0}")]
pub struct Unsupported(pub String);

type Result<T> = std::result::Result<T, Unsupported>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Clone, Debug)]
pub struct Clip {
    /// Geometry in default page user coordinates, independent of later CTM changes.
    pub path: Arc<BezPath>,
    pub rule: FillRule,
}

#[derive(Clone, Debug)]
pub enum Paint {
    Fill {
        rgb: [f32; 3],
        rule: FillRule,
    },
    /// `pen` is the CTM at the painting operator. A zero width is a device hairline.
    Stroke {
        rgb: [f32; 3],
        style: Stroke,
        pen: Affine,
    },
}

#[derive(Clone, Debug)]
pub struct Draw {
    pub path: Arc<BezPath>,
    pub paint: Paint,
    pub clips: Vec<Clip>,
}

#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    pub draws: Vec<Draw>,
    pub operators: usize,
}

#[derive(Clone)]
struct State {
    ctm: Affine,
    fill: [f32; 3],
    stroke: [f32; 3],
    style: Stroke,
    clips: Vec<Clip>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            ctm: Affine::IDENTITY,
            fill: [0.0; 3],
            stroke: [0.0; 3],
            style: Stroke::new(1.0).with_miter_limit(10.0).with_join(Join::Miter).with_start_cap(Cap::Butt).with_end_cap(Cap::Butt),
            clips: Vec::new(),
        }
    }
}

struct Compiler {
    state: State,
    stack: Vec<State>,
    path: BezPath,
    current: Option<Point>,
    subpath: Option<Point>,
    pending_clip: Option<FillRule>,
    /// Control-point box of the current path in user space (the CTM cannot change inside it).
    user_bounds: Option<[f64; 4]>,
    segments: usize,
    clip_count: usize,
    clip_references: usize,
    list: DisplayList,
}

impl Compiler {
    fn new() -> Self {
        Self {
            state: State::default(),
            stack: Vec::new(),
            path: BezPath::new(),
            current: None,
            subpath: None,
            pending_clip: None,
            user_bounds: None,
            segments: 0,
            clip_count: 0,
            clip_references: 0,
            list: DisplayList::default(),
        }
    }

    fn point(&mut self, x: f64, y: f64) -> Result<Point> {
        let p = self.state.ctm * Point::new(x, y);
        if [p.x, p.y].iter().all(|v| v.is_finite() && v.abs() <= 1e12) {
            let [x0, y0, x1, y1] = self.user_bounds.unwrap_or([x, y, x, y]);
            self.user_bounds = Some([x0.min(x), y0.min(y), x1.max(x), y1.max(y)]);
            Ok(p)
        } else {
            Err(Unsupported("path coordinates exceed the vector budget".into()))
        }
    }

    fn account_segments(&mut self, count: usize) -> Result<()> {
        self.segments = self.segments.saturating_add(count);
        if self.segments > MAX_SEGMENTS { Err(Unsupported("too many path segments".into())) } else { Ok(()) }
    }

    /// The current point for a new segment. After `h` the current point is the closed subpath's
    /// start and the next segment begins a new subpath there (ISO 32000-2 §8.5.2.1); kurbo needs
    /// that `MoveTo` spelled out instead of continuing from a `ClosePath`.
    fn continue_subpath(&mut self) -> Result<Point> {
        let current = self.current.ok_or_else(|| Unsupported("segment without a current point".into()))?;
        if matches!(self.path.elements().last(), Some(PathEl::ClosePath)) {
            self.account_segments(1)?;
            self.path.move_to(current);
            self.subpath = Some(current);
        }
        Ok(current)
    }

    /// ISO 32000-2 §8.5.1 allows no `cm` (nor a `Q` restoring another CTM) between path
    /// construction and painting, and readers disagree about such content: the bootstrap
    /// renderer and Poppler map the whole path with the CTM at painting, others with the CTM
    /// at each construction operator. The page uses the bootstrap renderer instead of
    /// choosing a third behaviour.
    fn ctm_change_outside_path(&self) -> Result<()> {
        if self.path.elements().is_empty() { Ok(()) } else { Err(Unsupported("transformation changed inside a path object".into())) }
    }

    fn close(&mut self) -> Result<()> {
        if let Some(start) = self.subpath {
            self.account_segments(1)?;
            self.path.close_path();
            self.current = Some(start);
        }
        Ok(())
    }

    fn paint(&mut self, fill: Option<FillRule>, stroke: bool) -> Result<()> {
        let path = Arc::new(std::mem::take(&mut self.path));
        let user_bounds = self.user_bounds.take();
        if !path.elements().is_empty() {
            if let Some(rule) = fill {
                match user_bounds.and_then(|b| self.degenerate_fill(b)) {
                    Some(line) => self.draw(line, Paint::Stroke { rgb: self.state.fill, style: degenerate_fill_style(), pen: self.state.ctm })?,
                    None => self.draw(path.clone(), Paint::Fill { rgb: self.state.fill, rule })?,
                }
            }
            if stroke {
                self.draw(path.clone(), Paint::Stroke { rgb: self.state.stroke, style: self.state.style.clone(), pen: self.state.ctm })?;
            }
        }
        // W/W* change clipping only after the path-painting operator. A combined fill/stroke
        // therefore paints under the previous clip, then installs the new clip (§8.5.4).
        if let Some(rule) = self.pending_clip.take() {
            self.clip_count = self.clip_count.saturating_add(1);
            if self.clip_count > MAX_CLIPS {
                return Err(Unsupported("too many clipping paths".into()));
            }
            self.state.clips.push(Clip { path, rule });
        }
        self.current = None;
        self.subpath = None;
        Ok(())
    }

    /// A fill whose path has no width or no height in user space paints nothing by area, but
    /// such paths are how many producers draw table rules (pdf.js issue 4260). The bootstrap
    /// renderer, pdf.js and Poppler paint them as a hairline: the diagonal of the path's
    /// user-space control box, stroked one device pixel wide. `None` for a path with area.
    /// The tolerance is the bootstrap renderer's (`f32` within 1/4096 of zero).
    fn degenerate_fill(&self, [x0, y0, x1, y1]: [f64; 4]) -> Option<Arc<BezPath>> {
        let nearly_zero = |v: f64| (v as f32).abs() <= 1.0 / 4096.0;
        if !nearly_zero(x1 - x0) && !nearly_zero(y1 - y0) {
            return None;
        }
        let mut line = BezPath::new();
        line.move_to(self.state.ctm * Point::new(x0, y0));
        line.line_to(self.state.ctm * Point::new(x1, y1));
        Some(Arc::new(line))
    }

    fn draw(&mut self, path: Arc<BezPath>, paint: Paint) -> Result<()> {
        if self.list.draws.len() >= MAX_DRAWS {
            return Err(Unsupported("too many painted paths".into()));
        }
        self.clip_references = self.clip_references.saturating_add(self.state.clips.len());
        if self.clip_references > MAX_CLIP_REFERENCES {
            return Err(Unsupported("too many retained clip references".into()));
        }
        self.list.draws.push(Draw { path, paint, clips: self.state.clips.clone() });
        Ok(())
    }

    fn operator(&mut self, op: &Op) -> Result<()> {
        if matches!(op.op.as_slice(), b"q" | b"Q" | b"h" | b"W" | b"W*" | b"n" | b"f" | b"F" | b"f*" | b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*") {
            numbers::<0>(op)?;
        }
        match op.op.as_slice() {
            b"q" => {
                if self.stack.len() >= MAX_STACK {
                    return Err(Unsupported("graphics state stack is too deep".into()));
                }
                self.stack.push(self.state.clone());
            }
            b"Q" => {
                let state = self.stack.pop().ok_or_else(|| Unsupported("unbalanced graphics state".into()))?;
                if state.ctm != self.state.ctm {
                    self.ctm_change_outside_path()?;
                }
                self.state = state;
            }
            b"cm" => {
                self.ctm_change_outside_path()?;
                let matrix = self.state.ctm * Affine::new(numbers(op)?);
                if matrix.as_coeffs().iter().any(|v| !v.is_finite() || v.abs() > 1e12) {
                    return Err(Unsupported("transformation exceeds the vector budget".into()));
                }
                self.state.ctm = matrix;
            }
            b"w" => {
                let [width] = numbers(op)?;
                if !(0.0..=1e9).contains(&width) {
                    return Err(Unsupported("invalid stroke width".into()));
                }
                self.state.style.width = width;
            }
            b"J" => {
                let cap = match numbers::<1>(op)? {
                    [0.0] => Cap::Butt,
                    [1.0] => Cap::Round,
                    [2.0] => Cap::Square,
                    _ => return Err(Unsupported("invalid line cap".into())),
                };
                self.state.style.start_cap = cap;
                self.state.style.end_cap = cap;
            }
            b"j" => {
                self.state.style.join = match numbers::<1>(op)? {
                    [0.0] => Join::Miter,
                    [1.0] => Join::Round,
                    [2.0] => Join::Bevel,
                    _ => return Err(Unsupported("invalid line join".into())),
                }
            }
            b"M" => {
                let [limit] = numbers(op)?;
                if !(1.0..=1000.0).contains(&limit) {
                    return Err(Unsupported("invalid miter limit".into()));
                }
                self.state.style.miter_limit = limit;
            }
            b"d" => {
                let Some(Object::Array(array)) = op.operands.first() else {
                    return Err(Unsupported("dash pattern is not an array".into()));
                };
                if array.len() > 256 || op.operands.len() != 2 {
                    return Err(Unsupported("invalid dash pattern length".into()));
                }
                let dash: Option<Vec<_>> = array.iter().map(Object::as_f64).collect();
                let dash = dash.ok_or_else(|| Unsupported("non-numeric dash pattern".into()))?;
                let phase = op.num(1).ok_or_else(|| Unsupported("invalid dash phase".into()))?;
                if !phase.is_finite()
                    || phase.abs() > 1e9
                    || dash.iter().any(|v| !v.is_finite() || !(0.0..=1e9).contains(v))
                    || (!dash.is_empty() && dash.iter().all(|v| *v == 0.0))
                {
                    return Err(Unsupported("invalid dash values".into()));
                }
                // Keep the phase within one period (an odd-length pattern repeats twice per
                // period) so the device never searches far for the starting dash. kurbo 0.13
                // reduces it the same way; this keeps the bound independent of the backend.
                let sum: f64 = dash.iter().sum();
                let period = if dash.len() % 2 == 1 { 2.0 * sum } else { sum };
                let phase = if dash.is_empty() { 0.0 } else { phase.rem_euclid(period) };
                if !phase.is_finite() {
                    return Err(Unsupported("invalid dash phase".into()));
                }
                self.state.style = self.state.style.clone().with_dashes(phase, dash);
            }
            b"g" | b"G" => {
                let [gray] = numbers(op)?;
                let rgb = [gray.clamp(0.0, 1.0) as f32; 3];
                if op.is("g") {
                    self.state.fill = rgb;
                } else {
                    self.state.stroke = rgb;
                }
            }
            b"rg" | b"RG" => {
                let rgb = numbers::<3>(op)?.map(|v| v.clamp(0.0, 1.0) as f32);
                if op.is("rg") {
                    self.state.fill = rgb;
                } else {
                    self.state.stroke = rgb;
                }
            }
            b"m" => {
                let [x, y] = numbers(op)?;
                let p = self.point(x, y)?;
                self.account_segments(1)?;
                self.path.move_to(p);
                self.current = Some(p);
                self.subpath = Some(p);
            }
            b"l" => {
                let [x, y] = numbers(op)?;
                let p = self.point(x, y)?;
                self.continue_subpath()?;
                self.account_segments(1)?;
                self.path.line_to(p);
                self.current = Some(p);
            }
            b"c" | b"v" | b"y" => {
                let previous = self.continue_subpath()?;
                let (p1, p2, p3) = if op.is("c") {
                    let [x1, y1, x2, y2, x3, y3] = numbers(op)?;
                    (self.point(x1, y1)?, self.point(x2, y2)?, self.point(x3, y3)?)
                } else {
                    let [x1, y1, x2, y2] = numbers(op)?;
                    let a = self.point(x1, y1)?;
                    let b = self.point(x2, y2)?;
                    if op.is("v") { (previous, a, b) } else { (a, b, b) }
                };
                self.account_segments(1)?;
                self.path.curve_to(p1, p2, p3);
                self.current = Some(p3);
            }
            b"h" => self.close()?,
            b"re" => {
                let [x, y, w, h] = numbers(op)?;
                let a = self.point(x, y)?;
                let b = self.point(x + w, y)?;
                let c = self.point(x + w, y + h)?;
                let d = self.point(x, y + h)?;
                self.account_segments(5)?;
                self.path.move_to(a);
                self.path.line_to(b);
                self.path.line_to(c);
                self.path.line_to(d);
                self.path.close_path();
                self.current = Some(a);
                self.subpath = Some(a);
            }
            b"W" => self.pending_clip = Some(FillRule::NonZero),
            b"W*" => self.pending_clip = Some(FillRule::EvenOdd),
            b"n" => self.paint(None, false)?,
            b"f" | b"F" => self.paint(Some(FillRule::NonZero), false)?,
            b"f*" => self.paint(Some(FillRule::EvenOdd), false)?,
            b"S" => self.paint(None, true)?,
            b"s" => {
                self.close()?;
                self.paint(None, true)?;
            }
            b"B" => self.paint(Some(FillRule::NonZero), true)?,
            b"B*" => self.paint(Some(FillRule::EvenOdd), true)?,
            b"b" => {
                self.close()?;
                self.paint(Some(FillRule::NonZero), true)?;
            }
            b"b*" => {
                self.close()?;
                self.paint(Some(FillRule::EvenOdd), true)?;
            }
            // Operators are at most three bytes (the lexer's token limit).
            _ => return Err(Unsupported(format!("unsupported content operator {}", String::from_utf8_lossy(&op.op)))),
        }
        Ok(())
    }
}

/// The pen of a degenerate fill's hairline (see `Compiler::degenerate_fill`): as thin as the
/// device allows, butt caps, bevel joins, no dashes.
fn degenerate_fill_style() -> Stroke {
    Stroke::new(0.001).with_join(Join::Bevel).with_start_cap(Cap::Butt).with_end_cap(Cap::Butt)
}

fn numbers<const N: usize>(op: &Op) -> Result<[f64; N]> {
    if op.operands.len() != N {
        return Err(Unsupported(format!("invalid operand count for {}", String::from_utf8_lossy(&op.op))));
    }
    op.nums().filter(|v| v.iter().all(|n| n.is_finite())).ok_or_else(|| Unsupported("non-finite or non-numeric operand".into()))
}

/// Compile decoded content into a complete vector display list. A caller-provided cancellation
/// probe is checked during lexical reading and every operator; input and retained data are bounded.
pub fn compile(content: &[u8], cancelled: impl Fn() -> bool) -> Result<DisplayList> {
    if content.len() > MAX_CONTENT || cancelled() {
        return Err(Unsupported("content budget exceeded or request cancelled".into()));
    }
    let mut compiler = Compiler::new();
    let mut reader = lexer::Reader::new(content, &cancelled);
    while let Some(op) = reader.next()? {
        compiler.list.operators += 1;
        if compiler.list.operators > MAX_OPERATORS {
            return Err(Unsupported("operator budget exceeded".into()));
        }
        compiler.operator(&op)?;
    }
    if !compiler.stack.is_empty() || compiler.pending_clip.is_some() {
        return Err(Unsupported("unfinished graphics state or clipping path".into()));
    }
    Ok(compiler.list)
}

#[cfg(test)]
mod tests;
