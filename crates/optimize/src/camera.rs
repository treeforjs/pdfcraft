//! Camera images (Scan & OCR ▸ Enhance ▸ Camera Image): find a photographed document's page in
//! a camera picture, undo its perspective and even out its lighting.
//!
//! Pure pixel code (8-bit gray or RGB, row-major, top row first); the PDF side is
//! [`crate::correct_camera_page`]. Coordinates are continuous pixel coordinates: pixel `(i, j)`
//! covers `[i, i + 1) × [j, j + 1)`, so the image's corners are `(0, 0)` and `(width, height)`,
//! y down.
//!
//! ## Method (original code)
//!
//! 1. **Edges.** The luma image is reduced by area averaging to at most [`WORK_SIDE`] pixels
//!    on its longer side, blurred (two 5×5 box passes), differentiated with Sobel kernels and
//!    thinned by non-maximum suppression along the gradient. Edge pixels are those above
//!    `max(12, 0.2 · p99)` of the thinned magnitudes.
//! 2. **Lines.** A Hough transform (1° by 1 px) where each edge pixel votes only within ±4°
//!    of its gradient direction; at most 16 peaks, at least 5° or 10 px apart.
//! 3. **Quadrilateral.** Pairs of nearly parallel lines (within 30°, at least 15 % of the
//!    image apart, not meeting inside the image) are combined with a second pair at least
//!    45° away into convex quadrilaterals covering at least 8 % of the image. Each side's
//!    *support* is the share of points along it with an edge pixel within 2 px whose gradient
//!    is within 15° of the side's normal; the score is `0.6 · mean + 0.4 · min` over the four
//!    sides, and the quadrilateral ranked best by `score · area^0.25` wins. Its score is the
//!    [`Detection::confidence`]; below [`MIN_CONFIDENCE`] no page was found and the whole
//!    image is returned.
//! 4. **Refinement.** At full resolution each side is re-measured: across ~100 points along
//!    it the strongest luma step of the side's dominant polarity is located to a sub-pixel
//!    (parabola) within a few pixels, a total-least-squares line is fitted with one round of
//!    outlier rejection, and the refined lines are intersected (twice, the second time with a
//!    narrower search).
//! 5. **Aspect ratio.** The page's width/height comes from the corners with Zhang & He's
//!    single-view rectangle method ("Whiteboard scanning and image enhancement", 2007),
//!    assuming square pixels and the principal point at the image centre: the focal length
//!    is solved from the two vanishing directions, then the ratio of the back-projected sides.
//!    When the sides are (nearly) parallel or the focal length is implausible, the ratio of
//!    the mean opposite side lengths is used instead.
//! 6. **Warp.** The rectangle is mapped onto the quadrilateral by Heckbert's closed-form
//!    square-to-quad homography and sampled bilinearly (edges clamped). The output keeps about
//!    the resolution of the photo's longer sides, capped at [`MAX_OUTPUT_PIXELS`].
//! 7. **Enhance.** Lighting is evened out per channel by dividing by a paper estimate: the
//!    90th percentile of each block (about 1/20 of the shorter side), dilated and smoothed
//!    over the 3×3 neighbouring blocks and interpolated bilinearly; this also neutralises a
//!    tinted light. Then a linear stretch maps the 1st luma percentile (at most 100) to black,
//!    and [`crate::background`]'s low-strength pass whitens what noise is left on the paper.

use thiserror::Error;

/// Four corners: top-left, top-right, bottom-right, bottom-left.
pub type Quad = [[f64; 2]; 4];

/// Images larger than this are refused (time and memory).
pub const MAX_INPUT_PIXELS: u64 = 80_000_000;
/// The corrected image never has more pixels than this.
pub const MAX_OUTPUT_PIXELS: u64 = 40_000_000;
/// Nor a longer side than this.
pub const MAX_OUTPUT_SIDE: u32 = 16_384;
/// Detections scoring lower than this are not trusted.
pub const MIN_CONFIDENCE: f64 = 0.45;
/// The longer side of the reduced image edges are found in.
pub const WORK_SIDE: usize = 640;
/// Hough lines considered.
const MAX_LINES: usize = 16;

#[derive(Clone, Debug, PartialEq, Error)]
pub enum CameraError {
    #[error("the image is empty")]
    Empty,
    #[error("the image is too large (at most {MAX_INPUT_PIXELS} pixels)")]
    TooLarge,
    #[error("{0}")]
    Invalid(String),
    #[error("the corners do not outline a page: {0}")]
    Degenerate(String),
}

/// Where the page is in a camera image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detection {
    /// The page's corners (pixels), or the image's own when nothing was found.
    pub quad: Quad,
    /// How well the edges support the outline (0–1).
    pub confidence: f64,
    /// Whether a page outline was found (`confidence` at least [`MIN_CONFIDENCE`]).
    pub found: bool,
}

/// The size of a corrected image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    pub width: u32,
    pub height: u32,
    /// Estimated width / height of the photographed page.
    pub aspect: f64,
}

/// What [`enhance`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EnhanceStats {
    /// The darkest and lightest paper (lighting) estimate, luma 0–255, before evening out.
    pub paper_min: f64,
    pub paper_max: f64,
    /// The luma mapped to black by the contrast stretch.
    pub black_point: u8,
    /// Pixels whose value changed.
    pub pixels_changed: u64,
}

fn check(px: &[u8], w: u32, h: u32, n: usize) -> Result<(usize, usize), CameraError> {
    if w == 0 || h == 0 {
        return Err(CameraError::Empty);
    }
    if n != 1 && n != 3 {
        return Err(CameraError::Invalid("only gray (1) and RGB (3) images are supported".into()));
    }
    if u64::from(w) * u64::from(h) > MAX_INPUT_PIXELS {
        return Err(CameraError::TooLarge);
    }
    let (w, h) = (usize::try_from(w).map_err(|_| CameraError::TooLarge)?, usize::try_from(h).map_err(|_| CameraError::TooLarge)?);
    let need = w.checked_mul(h).and_then(|p| p.checked_mul(n)).ok_or(CameraError::TooLarge)?;
    if px.len() != need {
        return Err(CameraError::Invalid(format!("the pixel buffer holds {} bytes, not {need}", px.len())));
    }
    Ok((w, h))
}

fn zeroed<T: Clone>(len: usize, v: T) -> Result<Vec<T>, CameraError> {
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| CameraError::TooLarge)?;
    out.resize(len, v);
    Ok(out)
}

fn luma(p: &[u8]) -> f64 {
    match p {
        [r, g, b, ..] => (77.0 * f64::from(*r) + 150.0 * f64::from(*g) + 29.0 * f64::from(*b)) / 256.0,
        [v, ..] => f64::from(*v),
        [] => 0.0,
    }
}

/// A float image with clamped reads.
struct Gray {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Gray {
    fn at(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.w.saturating_sub(1) as isize) as usize;
        let y = y.clamp(0, self.h.saturating_sub(1) as isize) as usize;
        self.v.get(y * self.w + x).copied().unwrap_or(0.0)
    }

    fn blur(&self) -> Gray {
        let r = 2isize;
        let mut tmp = vec![0f32; self.v.len()];
        for y in 0..self.h {
            for x in 0..self.w {
                let s: f32 = (-r..=r).map(|d| self.at(x as isize + d, y as isize)).sum();
                if let Some(t) = tmp.get_mut(y * self.w + x) {
                    *t = s / (2 * r + 1) as f32;
                }
            }
        }
        let t = Gray { w: self.w, h: self.h, v: tmp };
        let mut out = vec![0f32; self.v.len()];
        for y in 0..self.h {
            for x in 0..self.w {
                let s: f32 = (-r..=r).map(|d| t.at(x as isize, y as isize + d)).sum();
                if let Some(o) = out.get_mut(y * self.w + x) {
                    *o = s / (2 * r + 1) as f32;
                }
            }
        }
        Gray { w: self.w, h: self.h, v: out }
    }
}

/// The luma image reduced by an integer factor to at most [`WORK_SIDE`] on its longer side
/// (pixels beyond whole blocks are left out), and the factor.
fn reduce(px: &[u8], w: usize, h: usize, n: usize) -> Result<(Gray, usize), CameraError> {
    let s = w.max(h).div_ceil(WORK_SIDE).max(1);
    let (ws, hs) = ((w / s).max(1), (h / s).max(1));
    let mut v = zeroed(ws * hs, 0f32)?;
    let row = w * n;
    for y in 0..(hs * s).min(h) {
        let Some(line) = px.get(y * row..(y + 1) * row) else { continue };
        let wy = y / s;
        for (x, p) in line.chunks_exact(n).enumerate().take((ws * s).min(w)) {
            if let Some(c) = v.get_mut(wy * ws + x / s) {
                *c += luma(p) as f32;
            }
        }
    }
    let area = (s * s) as f32;
    v.iter_mut().for_each(|c| *c /= area);
    Ok((Gray { w: ws, h: hs, v }, s))
}

/// Thinned edges: per pixel the gradient's normal angle (radians, 0..π) or a negative value.
fn edges(g: &Gray) -> Vec<f32> {
    let (w, h) = (g.w, g.h);
    let mut mag = vec![0f32; w * h];
    let mut ang = vec![0f32; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let (xi, yi) = (x as isize, y as isize);
            let p = |dx: isize, dy: isize| g.at(xi + dx, yi + dy);
            let gx = p(1, -1) + 2.0 * p(1, 0) + p(1, 1) - p(-1, -1) - 2.0 * p(-1, 0) - p(-1, 1);
            let gy = p(-1, 1) + 2.0 * p(0, 1) + p(1, 1) - p(-1, -1) - 2.0 * p(0, -1) - p(1, -1);
            if let (Some(m), Some(a)) = (mag.get_mut(y * w + x), ang.get_mut(y * w + x)) {
                *m = gx.hypot(gy);
                *a = gy.atan2(gx);
            }
        }
    }
    let m = |x: isize, y: isize| -> f32 {
        if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
            return 0.0;
        }
        mag.get(y as usize * w + x as usize).copied().unwrap_or(0.0)
    };
    let mut thin = vec![-1f32; w * h];
    let mut kept = Vec::new();
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            let (v, a) = (mag.get(i).copied().unwrap_or(0.0), ang.get(i).copied().unwrap_or(0.0));
            if v <= 0.0 {
                continue;
            }
            // The neighbours along the gradient, in four sectors.
            let deg = a.to_degrees().rem_euclid(180.0);
            let (dx, dy) = match deg {
                d if !(22.5..157.5).contains(&d) => (1, 0),
                d if d < 67.5 => (1, 1),
                d if d < 112.5 => (0, 1),
                _ => (-1, 1),
            };
            let (xi, yi) = (x as isize, y as isize);
            if v >= m(xi + dx, yi + dy) && v >= m(xi - dx, yi - dy) {
                kept.push((i, v, a.rem_euclid(std::f32::consts::PI)));
            }
        }
    }
    if kept.is_empty() {
        return thin;
    }
    let mut sorted: Vec<f32> = kept.iter().map(|k| k.1).collect();
    sorted.sort_by(f32::total_cmp);
    let p99 = sorted.get((sorted.len() - 1) * 99 / 100).copied().unwrap_or(0.0);
    let t = (0.2 * p99).max(12.0);
    for (i, v, a) in kept {
        if v >= t
            && let Some(e) = thin.get_mut(i)
        {
            *e = a;
        }
    }
    thin
}

/// A line `nx·x + ny·y = rho` with a unit normal at angle `theta` (radians, 0..π).
#[derive(Clone, Copy, Debug)]
struct Line {
    nx: f64,
    ny: f64,
    rho: f64,
    theta: f64,
}

impl Line {
    fn from_normal(theta: f64, rho: f64) -> Line {
        Line { nx: theta.cos(), ny: theta.sin(), rho, theta }
    }

    /// The line through `c` along `d` (a unit vector).
    fn through(c: [f64; 2], d: [f64; 2]) -> Line {
        let (nx, ny) = (-d[1], d[0]);
        Line { nx, ny, rho: nx * c[0] + ny * c[1], theta: ny.atan2(nx).rem_euclid(std::f64::consts::PI) }
    }

    fn meet(&self, o: &Line) -> Option<[f64; 2]> {
        let det = self.nx * o.ny - self.ny * o.nx;
        if det.abs() < 1e-9 {
            return None;
        }
        let p = [(self.rho * o.ny - self.ny * o.rho) / det, (self.nx * o.rho - self.rho * o.nx) / det];
        (p[0].is_finite() && p[1].is_finite()).then_some(p)
    }

    fn distance(&self, p: [f64; 2]) -> f64 {
        (self.nx * p[0] + self.ny * p[1] - self.rho).abs()
    }
}

/// The angle between two undirected directions given as angles (radians), 0..π/2.
fn angle_between(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(std::f64::consts::PI);
    d.min(std::f64::consts::PI - d)
}

fn hough(edge: &[f32], w: usize, h: usize) -> Vec<Line> {
    let diag = ((w * w + h * h) as f64).sqrt().ceil() as usize + 1;
    let nr = 2 * diag + 1;
    let mut acc = vec![0u32; 180 * nr];
    let trig: Vec<(f64, f64)> = (0..180).map(|t| (f64::from(t)).to_radians().sin_cos()).map(|(s, c)| (c, s)).collect();
    for (i, a) in edge.iter().enumerate() {
        if *a < 0.0 {
            continue;
        }
        let (x, y) = ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
        let base = f64::from(a.to_degrees()).round() as i64;
        for dt in -4..=4i64 {
            let t = (base + dt).rem_euclid(180) as usize;
            let Some((c, s)) = trig.get(t) else { continue };
            let r = (x * c + y * s).round() as i64 + diag as i64;
            if r >= 0
                && let Some(v) = acc.get_mut(t * nr + r as usize)
            {
                *v += 1;
            }
        }
    }
    let min_votes = (0.1 * w.min(h) as f64).max(20.0) as u32;
    let get = |t: i64, r: i64| -> u32 {
        if r < 0 || r >= nr as i64 {
            return 0;
        }
        acc.get(t.rem_euclid(180) as usize * nr + r as usize).copied().unwrap_or(0)
    };
    let mut peaks = Vec::new();
    for t in 0..180i64 {
        for r in 0..nr as i64 {
            let v = get(t, r);
            if v < min_votes {
                continue;
            }
            let local = (-1..=1).all(|dt| (-1..=1).all(|dr| (dt == 0 && dr == 0) || get(t + dt, r + dr) <= v));
            if local {
                peaks.push((v, t, r - diag as i64));
            }
        }
    }
    peaks.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut lines: Vec<(i64, i64)> = Vec::new();
    for (_, t, r) in peaks {
        let near = lines.iter().any(|(lt, lr)| {
            let dt = (t - lt).abs();
            (dt <= 5 && (r - lr).abs() <= 10) || (180 - dt <= 5 && (r + lr).abs() <= 10)
        });
        if !near {
            lines.push((t, r));
            if lines.len() >= MAX_LINES {
                break;
            }
        }
    }
    lines.into_iter().map(|(t, r)| Line::from_normal((t as f64).to_radians(), r as f64)).collect()
}

fn cross(o: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn area(q: &Quad) -> f64 {
    let [a, b, c, d] = *q;
    0.5 * (cross(a, b, c) + cross(a, c, d)).abs()
}

/// The corners in the order top-left, top-right, bottom-right, bottom-left (y down): clockwise
/// on screen, starting from the corner nearest the top-left.
pub fn order_quad(q: Quad) -> Quad {
    let c = [q.iter().map(|p| p[0]).sum::<f64>() / 4.0, q.iter().map(|p| p[1]).sum::<f64>() / 4.0];
    let mut v = q;
    v.sort_by(|a, b| (a[1] - c[1]).atan2(a[0] - c[0]).total_cmp(&(b[1] - c[1]).atan2(b[0] - c[0])));
    let start = (0..4).min_by(|i, j| {
        let s = |k: usize| v.get(k).map_or(f64::INFINITY, |p| p[0] + p[1]);
        s(*i).total_cmp(&s(*j))
    });
    let start = start.unwrap_or(0);
    let mut out = v;
    for (k, o) in out.iter_mut().enumerate() {
        if let Some(p) = v.get((start + k) % 4) {
            *o = *p;
        }
    }
    out
}

/// Refuse corners that cannot outline a page of a `w`×`h` image: not finite, more than 2 % of
/// the image outside it, not a convex quadrilateral (in order), covering less than 1 % of it,
/// with a side shorter than 4 pixels or a corner sharper than 10° or flatter than 170°.
pub fn validate_quad(q: &Quad, w: f64, h: f64) -> Result<(), CameraError> {
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return Err(CameraError::Empty);
    }
    if q.iter().flatten().any(|v| !v.is_finite()) {
        return Err(CameraError::Degenerate("a coordinate is not a finite number".into()));
    }
    let m = 0.02 * w.max(h);
    if q.iter().any(|p| p[0] < -m || p[1] < -m || p[0] > w + m || p[1] > h + m) {
        return Err(CameraError::Degenerate("a corner lies outside the image".into()));
    }
    let turns: Vec<f64> = (0..4).map(|i| cross(q[i], q[(i + 1) % 4], q[(i + 2) % 4])).collect();
    if !(turns.iter().all(|t| *t > 0.0) || turns.iter().all(|t| *t < 0.0)) {
        return Err(CameraError::Degenerate("they do not form a convex quadrilateral".into()));
    }
    if area(q) < 0.01 * w * h {
        return Err(CameraError::Degenerate("the outlined area is less than 1 % of the image".into()));
    }
    for i in 0..4 {
        let (p, a, b) = (q[i], q[(i + 3) % 4], q[(i + 1) % 4]);
        let (la, lb) = (dist(p, a), dist(p, b));
        if la < 4.0 || lb < 4.0 {
            return Err(CameraError::Degenerate("a side is shorter than 4 pixels".into()));
        }
        let cos = ((a[0] - p[0]) * (b[0] - p[0]) + (a[1] - p[1]) * (b[1] - p[1])) / (la * lb);
        let deg = cos.clamp(-1.0, 1.0).acos().to_degrees();
        if !(10.0..=170.0).contains(&deg) {
            return Err(CameraError::Degenerate(format!("a corner's angle is {deg:.0}°, outside 10–170°")));
        }
    }
    Ok(())
}

/// The share of points along `a`→`b` with a matching edge pixel within 2 px.
fn support(edge: &[f32], w: usize, h: usize, a: [f64; 2], b: [f64; 2]) -> f64 {
    let len = dist(a, b);
    if len < 1.0 {
        return 0.0;
    }
    let (dx, dy) = ((b[0] - a[0]) / len, (b[1] - a[1]) / len);
    let (nx, ny) = (-dy, dx);
    let normal = ny.atan2(nx);
    let samples = ((len / 2.0) as usize).clamp(8, 4000);
    let tol = 15f64.to_radians();
    let mut hits = 0usize;
    for k in 0..samples {
        let t = 0.05 + 0.9 * (k as f64 + 0.5) / samples as f64;
        let (px, py) = (a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]));
        let hit = (-2..=2).any(|o| {
            let (x, y) = ((px + f64::from(o) * nx).floor(), (py + f64::from(o) * ny).floor());
            if x < 0.0 || y < 0.0 || x >= w as f64 || y >= h as f64 {
                return false;
            }
            edge.get(y as usize * w + x as usize).is_some_and(|e| *e >= 0.0 && angle_between(f64::from(*e), normal) <= tol)
        });
        hits += usize::from(hit);
    }
    hits as f64 / samples as f64
}

/// The best-supported page outline in the reduced image (corners and score).
fn best_quad(edge: &[f32], w: usize, h: usize, lines: &[Line]) -> Option<(Quad, f64)> {
    let (wf, hf) = (w as f64, h as f64);
    let small = wf.min(hf);
    let centre = [wf / 2.0, hf / 2.0];
    let margin = 0.05 * wf.max(hf);
    let inside = |p: [f64; 2]| p[0] > -margin && p[1] > -margin && p[0] < wf + margin && p[1] < hf + margin;
    let mut pairs = Vec::new();
    for (i, a) in lines.iter().enumerate() {
        for (j, b) in lines.iter().enumerate().skip(i + 1) {
            if angle_between(a.theta, b.theta) > 30f64.to_radians() || a.meet(b).is_some_and(inside) {
                continue;
            }
            let foot =
                [centre[0] - (a.nx * centre[0] + a.ny * centre[1] - a.rho) * a.nx, centre[1] - (a.nx * centre[0] + a.ny * centre[1] - a.rho) * a.ny];
            if b.distance(foot) < 0.15 * small {
                continue;
            }
            let mean = 0.5 * ((2.0 * a.theta).sin() + (2.0 * b.theta).sin()).atan2((2.0 * a.theta).cos() + (2.0 * b.theta).cos());
            pairs.push((i, j, mean));
        }
    }
    let mut best: Option<(Quad, f64, f64)> = None;
    for (k, p) in pairs.iter().enumerate() {
        for q in pairs.iter().skip(k + 1) {
            if p.0 == q.0 || p.0 == q.1 || p.1 == q.0 || p.1 == q.1 || angle_between(p.2, q.2) < 45f64.to_radians() {
                continue;
            }
            let (Some(pa), Some(pb), Some(qa), Some(qb)) = (lines.get(p.0), lines.get(p.1), lines.get(q.0), lines.get(q.1)) else { continue };
            let (Some(c0), Some(c1), Some(c2), Some(c3)) = (pa.meet(qa), pa.meet(qb), pb.meet(qb), pb.meet(qa)) else { continue };
            let quad = order_quad([c0, c1, c2, c3]);
            let a = area(&quad);
            if a < 0.08 * wf * hf || validate_quad(&quad, wf, hf).is_err() {
                continue;
            }
            let s: Vec<f64> = (0..4).map(|i| support(edge, w, h, quad[i], quad[(i + 1) % 4])).collect();
            let mean = s.iter().sum::<f64>() / 4.0;
            let min = s.iter().copied().fold(1.0, f64::min);
            let score = 0.6 * mean + 0.4 * min;
            let rank = score * (a / (wf * hf)).powf(0.25);
            if best.as_ref().is_none_or(|b| rank > b.2) {
                best = Some((quad, score, rank));
            }
        }
    }
    best.map(|(q, s, _)| (q, s))
}

/// Bilinear luma at a continuous point (edges clamped).
fn luma_at(px: &[u8], w: usize, h: usize, n: usize, x: f64, y: f64) -> f64 {
    let (fx, fy) = ((x - 0.5).clamp(0.0, (w - 1) as f64), (y - 0.5).clamp(0.0, (h - 1) as f64));
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (fx - x0 as f64, fy - y0 as f64);
    let p = |x: usize, y: usize| px.get((y * w + x) * n..(y * w + x) * n + n).map_or(0.0, luma);
    let top = p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx;
    let bottom = p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx;
    top * (1.0 - ty) + bottom * ty
}

/// A luma step across a side: (offset along the normal, strength).
type Step = (f64, f64);

/// Re-measure the side `a`→`b` at full resolution, searching `radius` pixels across it.
fn refine_side(px: &[u8], w: usize, h: usize, n: usize, a: [f64; 2], b: [f64; 2], radius: f64) -> Option<Line> {
    let len = dist(a, b);
    if len < 8.0 {
        return None;
    }
    let (dx, dy) = ((b[0] - a[0]) / len, (b[1] - a[1]) / len);
    let (nx, ny) = (-dy, dx);
    let samples = ((len / 4.0) as usize).clamp(16, 160);
    let steps = (radius.ceil() as i64).clamp(2, 64);
    // Per sample, the best step of each polarity: (offset, strength).
    let mut found: Vec<([f64; 2], Step, Step)> = Vec::new();
    for k in 0..samples {
        let t = 0.08 + 0.84 * (k as f64 + 0.5) / samples as f64;
        let c = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])];
        let profile: Vec<f64> = (-steps - 1..=steps + 1)
            .map(|o| {
                let o = o as f64;
                (-1..=1).map(|s| luma_at(px, w, h, n, c[0] + o * nx + f64::from(s) * dx, c[1] + o * ny + f64::from(s) * dy)).sum::<f64>() / 3.0
            })
            .collect();
        let grad: Vec<f64> = profile.windows(3).map(|v| v[2] - v[0]).collect();
        let pick = |sign: f64| -> (f64, f64) {
            let mut best = (0usize, 0.0f64);
            for (i, g) in grad.iter().enumerate() {
                if sign * g > best.1 {
                    best = (i, sign * g);
                }
            }
            let (i, v) = best;
            let g = |j: usize| grad.get(j).map_or(0.0, |g| sign * g);
            let sub = if i > 0 && i + 1 < grad.len() {
                let (l, r) = (g(i - 1), g(i + 1));
                let den = l - 2.0 * v + r;
                if den.abs() > 1e-9 { (0.5 * (l - r) / den).clamp(-0.5, 0.5) } else { 0.0 }
            } else {
                0.0
            };
            (i as f64 - steps as f64 + sub, v)
        };
        found.push((c, pick(1.0), pick(-1.0)));
    }
    let pos: f64 = found.iter().map(|f| f.1.1).sum();
    let neg: f64 = found.iter().map(|f| f.2.1).sum();
    let points: Vec<[f64; 2]> = found
        .iter()
        .filter_map(|(c, p, m)| {
            let (o, v) = if pos >= neg { *p } else { *m };
            (v > 4.0).then(|| [c[0] + o * nx, c[1] + o * ny])
        })
        .collect();
    let fit = fit_line(&points)?;
    let residual: Vec<f64> = points.iter().map(|p| fit.distance(*p)).collect();
    let mut sorted = residual.clone();
    sorted.sort_by(f64::total_cmp);
    let mad = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    let keep: Vec<[f64; 2]> = points.iter().zip(&residual).filter(|(_, r)| **r <= (2.5 * mad).max(1.0)).map(|(p, _)| *p).collect();
    fit_line(&keep)
}

/// Total least squares: the line through the points' centroid along their principal axis.
fn fit_line(points: &[[f64; 2]]) -> Option<Line> {
    if points.len() < 8 {
        return None;
    }
    let k = points.len() as f64;
    let c = [points.iter().map(|p| p[0]).sum::<f64>() / k, points.iter().map(|p| p[1]).sum::<f64>() / k];
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for p in points {
        let (x, y) = (p[0] - c[0], p[1] - c[1]);
        sxx += x * x;
        sxy += x * y;
        syy += y * y;
    }
    let angle = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let line = Line::through(c, [angle.cos(), angle.sin()]);
    (line.nx.is_finite() && line.rho.is_finite()).then_some(line)
}

fn refine(px: &[u8], w: usize, h: usize, n: usize, q: Quad, radius: f64) -> Option<Quad> {
    let sides: Vec<Line> = (0..4).map(|i| refine_side(px, w, h, n, q[i], q[(i + 1) % 4], radius)).collect::<Option<_>>()?;
    let corner = |i: usize| sides.get((i + 3) % 4)?.meet(sides.get(i)?);
    let out = [corner(0)?, corner(1)?, corner(2)?, corner(3)?];
    let moved = (0..4).map(|i| dist(out[i], q[i])).fold(0.0, f64::max);
    (moved <= 3.0 * radius && validate_quad(&out, w as f64, h as f64).is_ok()).then_some(out)
}

/// Find the photographed page in an 8-bit gray (`n` 1) or RGB (`n` 3) image. When no outline is
/// found well enough the whole image is returned with `found` false.
pub fn detect(px: &[u8], width: u32, height: u32, n: usize) -> Result<Detection, CameraError> {
    let (w, h) = check(px, width, height, n)?;
    let full: Quad = [[0.0, 0.0], [w as f64, 0.0], [w as f64, h as f64], [0.0, h as f64]];
    let none = Detection { quad: full, confidence: 0.0, found: false };
    if w < 16 || h < 16 {
        return Ok(none);
    }
    let (work, s) = reduce(px, w, h, n)?;
    let blurred = work.blur().blur();
    let edge = edges(&blurred);
    let lines = hough(&edge, blurred.w, blurred.h);
    let Some((quad, score)) = best_quad(&edge, blurred.w, blurred.h, &lines) else { return Ok(none) };
    if score < MIN_CONFIDENCE {
        return Ok(Detection { confidence: score, ..none });
    }
    let sf = s as f64;
    let mut q = quad.map(|p| [p[0] * sf, p[1] * sf]);
    if let Some(r) = refine(px, w, h, n, q, 4.0 * sf + 4.0) {
        q = r;
        if let Some(r) = refine(px, w, h, n, q, 3.0) {
            q = r;
        }
    }
    Ok(Detection { quad: q, confidence: score, found: true })
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The photographed page's width / height (see the module docs, step 5). `q` is ordered
/// top-left, top-right, bottom-right, bottom-left in a `w`×`h` image.
pub fn estimate_aspect(q: &Quad, w: f64, h: f64) -> f64 {
    let sides = (dist(q[0], q[1]) + dist(q[3], q[2])) / (dist(q[0], q[3]) + dist(q[1], q[2])).max(1e-9);
    let fallback = if sides.is_finite() { sides.clamp(0.05, 20.0) } else { 1.0 };
    let (cx, cy) = (w / 2.0, h / 2.0);
    let m = |p: [f64; 2]| [p[0] - cx, p[1] - cy, 1.0];
    let (m1, m2, m3, m4) = (m(q[0]), m(q[1]), m(q[3]), m(q[2]));
    let d2 = dot3(cross3(m2, m4), m3);
    let d3 = dot3(cross3(m3, m4), m2);
    if d2.abs() < 1e-9 || d3.abs() < 1e-9 {
        return fallback;
    }
    let k2 = dot3(cross3(m1, m4), m3) / d2;
    let k3 = dot3(cross3(m1, m4), m2) / d3;
    let n2 = sub([k2 * m2[0], k2 * m2[1], k2], m1);
    let n3 = sub([k3 * m3[0], k3 * m3[1], k3], m1);
    let size = w.max(h);
    // (Nearly) parallel sides: the focal length is undetermined and does not matter.
    let affine = n2[2].abs() * size < 1e-3 * n2[0].hypot(n2[1]) || n3[2].abs() * size < 1e-3 * n3[0].hypot(n3[1]);
    let ratio = if affine {
        ((n2[0] * n2[0] + n2[1] * n2[1]) / (n3[0] * n3[0] + n3[1] * n3[1])).sqrt()
    } else {
        let f2 = -(n2[0] * n3[0] + n2[1] * n3[1]) / (n2[2] * n3[2]);
        if !(f2.is_finite() && f2 > (0.2 * size).powi(2) && f2 < (20.0 * size).powi(2)) {
            return fallback;
        }
        ((n2[0] * n2[0] + n2[1] * n2[1] + f2 * n2[2] * n2[2]) / (n3[0] * n3[0] + n3[1] * n3[1] + f2 * n3[2] * n3[2])).sqrt()
    };
    // A wildly different answer means the camera model doesn't fit (a cropped photo, a
    // non-rectangular page): keep the plain estimate.
    if ratio.is_finite() && ratio > 0.0 && (ratio / fallback).ln().abs() < 0.7 { ratio.clamp(0.05, 20.0) } else { fallback }
}

/// The corrected image's size for corners `q` in a `w`×`h` image: about the resolution of the
/// photo's longer sides, at the estimated aspect ratio, within [`MAX_OUTPUT_PIXELS`] and
/// [`MAX_OUTPUT_SIDE`].
pub fn plan(q: &Quad, w: u32, h: u32) -> Result<Plan, CameraError> {
    validate_quad(q, f64::from(w), f64::from(h))?;
    let aspect = estimate_aspect(q, f64::from(w), f64::from(h));
    let w0 = dist(q[0], q[1]).max(dist(q[3], q[2]));
    let h0 = dist(q[0], q[3]).max(dist(q[1], q[2]));
    let mut ow = w0.max(h0 * aspect);
    let mut oh = ow / aspect;
    let max_side = f64::from(MAX_OUTPUT_SIDE);
    let shrink = (max_side / ow).min(max_side / oh).min((MAX_OUTPUT_PIXELS as f64 / (ow * oh)).sqrt()).min(1.0);
    if !(shrink.is_finite() && shrink > 0.0) {
        return Err(CameraError::Degenerate("the corrected size cannot be computed".into()));
    }
    ow = (ow * shrink).round().clamp(1.0, max_side);
    oh = (oh * shrink).round().clamp(1.0, max_side);
    Ok(Plan { width: ow as u32, height: oh as u32, aspect })
}

/// Heckbert's square-to-quad projective map: unit square corners (0,0), (1,0), (1,1), (0,1)
/// onto `q` (in order). `[a, b, c, d, e, f, g, h]` with
/// `x = (a·u + b·v + c) / (g·u + h·v + 1)`, `y = (d·u + e·v + f) / (g·u + h·v + 1)`.
pub fn square_to_quad(q: &Quad) -> Option<[f64; 8]> {
    let [[x0, y0], [x1, y1], [x2, y2], [x3, y3]] = *q;
    let (dx3, dy3) = (x0 - x1 + x2 - x3, y0 - y1 + y2 - y3);
    let m = if dx3.abs() < 1e-12 && dy3.abs() < 1e-12 {
        [x1 - x0, x3 - x0, x0, y1 - y0, y3 - y0, y0, 0.0, 0.0]
    } else {
        let (dx1, dx2, dy1, dy2) = (x1 - x2, x3 - x2, y1 - y2, y3 - y2);
        let den = dx1 * dy2 - dx2 * dy1;
        if den.abs() < 1e-12 {
            return None;
        }
        let g = (dx3 * dy2 - dx2 * dy3) / den;
        let hh = (dx1 * dy3 - dx3 * dy1) / den;
        [x1 - x0 + g * x1, x3 - x0 + hh * x3, x0, y1 - y0 + g * y1, y3 - y0 + hh * y3, y0, g, hh]
    };
    m.iter().all(|v| v.is_finite()).then_some(m)
}

/// Where unit-square point `(u, v)` lands under `m` ([`square_to_quad`]).
pub fn map_point(m: &[f64; 8], u: f64, v: f64) -> Option<[f64; 2]> {
    let den = m[6] * u + m[7] * v + 1.0;
    if den.abs() < 1e-12 {
        return None;
    }
    let p = [(m[0] * u + m[1] * v + m[2]) / den, (m[3] * u + m[4] * v + m[5]) / den];
    (p[0].is_finite() && p[1].is_finite()).then_some(p)
}

/// Resample the page outlined by `q` (ordered) into an upright `out_w`×`out_h` image, sampling
/// bilinearly (edges clamped).
pub fn warp(px: &[u8], width: u32, height: u32, n: usize, q: &Quad, out_w: u32, out_h: u32) -> Result<Vec<u8>, CameraError> {
    let (w, h) = check(px, width, height, n)?;
    validate_quad(q, w as f64, h as f64)?;
    if out_w == 0 || out_h == 0 || out_w > MAX_OUTPUT_SIDE || out_h > MAX_OUTPUT_SIDE || u64::from(out_w) * u64::from(out_h) > MAX_OUTPUT_PIXELS {
        return Err(CameraError::Invalid(format!(
            "the corrected image must be 1–{MAX_OUTPUT_SIDE} pixels a side and at most {MAX_OUTPUT_PIXELS} pixels"
        )));
    }
    let m = square_to_quad(q).ok_or_else(|| CameraError::Degenerate("no perspective maps a rectangle onto them".into()))?;
    let (ow, oh) = (out_w as usize, out_h as usize);
    let mut out = Vec::new();
    out.try_reserve_exact(ow * oh * n).map_err(|_| CameraError::TooLarge)?;
    let (maxx, maxy) = ((w - 1) as f64, (h - 1) as f64);
    let at = |x: usize, y: usize, c: usize| px.get((y * w + x) * n + c).map_or(0.0, |v| f64::from(*v));
    for y in 0..oh {
        let v = (y as f64 + 0.5) / oh as f64;
        for x in 0..ow {
            let u = (x as f64 + 0.5) / ow as f64;
            let [sx, sy] = map_point(&m, u, v).unwrap_or([0.0, 0.0]);
            let (fx, fy) = ((sx - 0.5).clamp(0.0, maxx), (sy - 0.5).clamp(0.0, maxy));
            let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (tx, ty) = (fx - x0 as f64, fy - y0 as f64);
            for c in 0..n {
                let top = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
                let bottom = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
                out.push((top * (1.0 - ty) + bottom * ty).round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    Ok(out)
}

/// Even out the lighting, stretch the contrast and whiten the paper of a corrected page image
/// in place (see the module docs, step 7).
pub fn enhance(px: &mut [u8], width: u32, height: u32, n: usize) -> Result<EnhanceStats, CameraError> {
    let (w, h) = check(px, width, height, n)?;
    let block = (w.min(h) / 20).clamp(8, 256);
    let (gw, gh) = (w.div_ceil(block), h.div_ceil(block));
    // Per channel: the paper estimate of each block.
    let mut grid = zeroed(gw * gh * n, 0f64)?;
    for c in 0..n {
        for by in 0..gh {
            for bx in 0..gw {
                let mut hist = [0u32; 256];
                let mut count = 0u32;
                for y in by * block..((by + 1) * block).min(h) {
                    for x in bx * block..((bx + 1) * block).min(w) {
                        if let Some(v) = px.get((y * w + x) * n + c) {
                            hist[usize::from(*v)] += 1;
                            count += 1;
                        }
                    }
                }
                let target = (f64::from(count) * 0.9).ceil() as u32;
                let mut acc = 0u32;
                let mut level = 255usize;
                for (i, k) in hist.iter().enumerate() {
                    acc += k;
                    if acc >= target.max(1) {
                        level = i;
                        break;
                    }
                }
                if let Some(g) = grid.get_mut((by * gw + bx) * n + c) {
                    *g = level as f64;
                }
            }
        }
    }
    // Dilate (blocks full of ink or pictures borrow their neighbours' paper), then smooth.
    let pass = |src: &[f64], max: bool| -> Vec<f64> {
        let mut out = src.to_vec();
        for c in 0..n {
            for by in 0..gh as isize {
                for bx in 0..gw as isize {
                    let mut vals = Vec::with_capacity(9);
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let (x, y) = (bx + dx, by + dy);
                            if x >= 0
                                && y >= 0
                                && (x as usize) < gw
                                && (y as usize) < gh
                                && let Some(v) = src.get((y as usize * gw + x as usize) * n + c)
                            {
                                vals.push(*v);
                            }
                        }
                    }
                    let v = if max { vals.iter().copied().fold(0.0, f64::max) } else { vals.iter().sum::<f64>() / vals.len().max(1) as f64 };
                    if let Some(o) = out.get_mut((by as usize * gw + bx as usize) * n + c) {
                        *o = v;
                    }
                }
            }
        }
        out
    };
    let grid = pass(&pass(&grid, true), false);
    let lum = |i: usize| -> f64 {
        if n == 3 {
            (77.0 * grid.get(i * 3).copied().unwrap_or(0.0)
                + 150.0 * grid.get(i * 3 + 1).copied().unwrap_or(0.0)
                + 29.0 * grid.get(i * 3 + 2).copied().unwrap_or(0.0))
                / 256.0
        } else {
            grid.get(i).copied().unwrap_or(0.0)
        }
    };
    let papers: Vec<f64> = (0..gw * gh).map(lum).collect();
    let mut stats = EnhanceStats {
        paper_min: papers.iter().copied().fold(255.0, f64::min),
        paper_max: papers.iter().copied().fold(0.0, f64::max),
        ..Default::default()
    };
    let original = {
        let mut v = Vec::new();
        v.try_reserve_exact(px.len()).map_err(|_| CameraError::TooLarge)?;
        v.extend_from_slice(px);
        v
    };
    let paper = |gx: f64, gy: f64, c: usize| -> f64 {
        let (fx, fy) = ((gx - 0.5).clamp(0.0, (gw - 1) as f64), (gy - 0.5).clamp(0.0, (gh - 1) as f64));
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(gw - 1), (y0 + 1).min(gh - 1));
        let (tx, ty) = (fx - x0 as f64, fy - y0 as f64);
        let g = |x: usize, y: usize| grid.get((y * gw + x) * n + c).copied().unwrap_or(255.0);
        let v = (g(x0, y0) * (1.0 - tx) + g(x1, y0) * tx) * (1.0 - ty) + (g(x0, y1) * (1.0 - tx) + g(x1, y1) * tx) * ty;
        v.max(32.0)
    };
    for y in 0..h {
        let gy = (y as f64 + 0.5) / block as f64;
        for x in 0..w {
            let gx = (x as f64 + 0.5) / block as f64;
            for c in 0..n {
                if let Some(v) = px.get_mut((y * w + x) * n + c) {
                    *v = (f64::from(*v) * 255.0 / paper(gx, gy, c)).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    // Contrast: the darkest 1 % becomes black (unless the page has no dark ink).
    let mut hist = [0u64; 256];
    for p in px.chunks_exact(n) {
        hist[luma(p).round().clamp(0.0, 255.0) as usize] += 1;
    }
    let total: u64 = hist.iter().sum();
    let mut acc = 0u64;
    let mut black = 0usize;
    for (i, k) in hist.iter().enumerate() {
        acc += k;
        if acc * 100 >= total {
            black = i;
            break;
        }
    }
    let black = black.min(100);
    stats.black_point = black as u8;
    if black > 0 {
        let lut: Vec<u8> =
            (0..256usize).map(|v| ((v.saturating_sub(black)) as f64 * 255.0 / (255 - black) as f64).round().clamp(0.0, 255.0) as u8).collect();
        for v in px.iter_mut() {
            *v = lut.get(usize::from(*v)).copied().unwrap_or(*v);
        }
    }
    let low = crate::background::Options { strength: crate::background::Strength::Low, white_point: None };
    // A page without a light paper level is left as it is; that is not an error here.
    let _ = crate::background::remove_background_pixels(px, width, height, n, low);
    stats.pixels_changed = px.chunks_exact(n).zip(original.chunks_exact(n)).filter(|(a, b)| a != b).count() as u64;
    Ok(stats)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A synthetic page: white paper (lit unevenly) with dark text bars and a title block, at
    /// page coordinates `(u, v)` in 0..1.
    pub(crate) fn page_luma(u: f64, v: f64) -> f64 {
        let ink = (0.12..0.88).contains(&u)
            && ((0.1..0.16).contains(&v) && u < 0.6 || (0.25..0.9).contains(&v) && (v * 40.0).fract() < 0.45 && (u * 13.0).fract() < 0.8);
        if ink { 30.0 } else { 245.0 }
    }

    /// A textured table: smooth value noise in browns.
    pub(crate) fn table_luma(x: f64, y: f64) -> f64 {
        let n = |x: f64, y: f64| ((x * 0.031).sin() * (y * 0.027).cos() + (x * 0.011 + y * 0.017).sin()) * 0.5;
        let fine = (((x as i64).wrapping_mul(73_856_093) ^ (y as i64).wrapping_mul(19_349_663)) & 15) as f64;
        90.0 + 30.0 * n(x, y) + fine
    }

    /// A pinhole camera looking at a page of `pw`×`ph` units tilted by `rx`, `ry` (radians)
    /// and turned by `rz`, at distance `z`, with focal length `f` pixels: the projective map
    /// from page `(u, v)` (0..1) to image pixels, and the page's projected corners.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn camera(w: f64, h: f64, pw: f64, ph: f64, rx: f64, ry: f64, rz: f64, z: f64, f: f64) -> (impl Fn(f64, f64) -> [f64; 2], Quad) {
        let (sx, cx) = rx.sin_cos();
        let (sy, cy) = ry.sin_cos();
        let (sz, cz) = rz.sin_cos();
        let rot = move |p: [f64; 3]| {
            let p = [p[0] * cz - p[1] * sz, p[0] * sz + p[1] * cz, p[2]];
            let p = [p[0], p[1] * cx - p[2] * sx, p[1] * sx + p[2] * cx];
            [p[0] * cy + p[2] * sy, p[1], -p[0] * sy + p[2] * cy]
        };
        let project = move |u: f64, v: f64| {
            let p = rot([(u - 0.5) * pw, (v - 0.5) * ph, 0.0]);
            let zz = p[2] + z;
            [w / 2.0 + f * p[0] / zz, h / 2.0 + f * p[1] / zz]
        };
        let q = [project(0.0, 0.0), project(1.0, 0.0), project(1.0, 1.0), project(0.0, 1.0)];
        (project, q)
    }

    /// Render the camera image (gray) by inverting the projection with a homography.
    pub(crate) fn photograph(w: usize, h: usize, q: &Quad) -> Vec<u8> {
        let m = square_to_quad(q).unwrap();
        // Invert numerically: the inverse homography is the square_to_quad of the image →
        // square, so solve per pixel with the adjugate.
        let hm = [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], 1.0]];
        let inv = invert3(hm);
        let mut px = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0.0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let (fx, fy) = (x as f64 + ox, y as f64 + oy);
                    let d = inv[2][0] * fx + inv[2][1] * fy + inv[2][2];
                    let (u, v) = ((inv[0][0] * fx + inv[0][1] * fy + inv[0][2]) / d, (inv[1][0] * fx + inv[1][1] * fy + inv[1][2]) / d);
                    sum += if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) {
                        // Shadowed towards one corner.
                        page_luma(u, v) * (1.0 - 0.25 * u * v)
                    } else {
                        table_luma(fx, fy)
                    };
                }
                px.push((sum / 4.0).round() as u8);
            }
        }
        px
    }

    pub(crate) fn invert3(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        let c = |a: usize, b: usize, c: usize, d: usize| (m[a][b] * m[c][d] - m[a][d] * m[c][b]) / det;
        [
            [c(1, 1, 2, 2), -c(0, 1, 2, 2), c(0, 1, 1, 2)],
            [-c(1, 0, 2, 2), c(0, 0, 2, 2), -c(0, 0, 1, 2)],
            [c(1, 0, 2, 1), -c(0, 0, 2, 1), c(0, 0, 1, 1)],
        ]
    }

    fn max_corner_error(a: &Quad, b: &Quad) -> f64 {
        (0..4).map(|i| dist(a[i], b[i])).fold(0.0, f64::max)
    }

    #[test]
    fn square_to_quad_maps_the_corners_exactly() {
        let q = [[10.0, 20.0], [300.0, 35.0], [280.0, 400.0], [5.0, 380.0]];
        let m = square_to_quad(&q).unwrap();
        for (i, (u, v)) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].into_iter().enumerate() {
            let p = map_point(&m, u, v).unwrap();
            assert!(dist(p, q[i]) < 1e-9, "{p:?} {:?}", q[i]);
        }
        // A parallelogram is affine.
        let m = square_to_quad(&[[0.0, 0.0], [4.0, 1.0], [5.0, 4.0], [1.0, 3.0]]).unwrap();
        assert_eq!((m[6], m[7]), (0.0, 0.0));
    }

    #[test]
    fn corners_are_ordered_from_the_top_left_clockwise() {
        let q = order_quad([[90.0, 95.0], [10.0, 12.0], [5.0, 80.0], [100.0, 8.0]]);
        assert_eq!(q, [[10.0, 12.0], [100.0, 8.0], [90.0, 95.0], [5.0, 80.0]]);
    }

    #[test]
    fn degenerate_corners_are_refused() {
        let (w, h) = (200.0, 100.0);
        let bad: [Quad; 7] = [
            [[0.0, 0.0], [f64::NAN, 0.0], [10.0, 10.0], [0.0, 10.0]],
            [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
            [[0.0, 0.0], [200.0, 0.0], [0.0, 100.0], [200.0, 100.0]],
            [[0.0, 0.0], [100.0, 0.0], [200.0, 0.0], [0.0, 100.0]],
            [[0.0, 0.0], [500.0, 0.0], [200.0, 100.0], [0.0, 100.0]],
            [[0.0, 0.0], [200.0, 0.0], [200.0, 100.0], [199.0, 100.0]],
            [[0.0, 0.0], [200.0, 0.0], [199.0, 100.0], [198.0, 0.5]],
        ];
        for q in &bad {
            assert!(validate_quad(q, w, h).is_err(), "{q:?}");
            assert!(plan(q, 200, 100).is_err(), "{q:?}");
        }
        assert!(validate_quad(&[[0.0, 0.0], [200.0, 0.0], [200.0, 100.0], [0.0, 100.0]], w, h).is_ok());
        assert!(validate_quad(&[[0.0, 0.0], [200.0, 0.0], [200.0, 100.0], [0.0, 100.0]], 0.0, h).is_err());
    }

    #[test]
    fn hostile_images_are_refused_without_panicking() {
        assert_eq!(detect(&[], 0, 0, 1), Err(CameraError::Empty));
        assert!(matches!(detect(&[0; 10], 3, 3, 1), Err(CameraError::Invalid(_))));
        assert!(matches!(detect(&[0; 9], 3, 3, 2), Err(CameraError::Invalid(_))));
        assert_eq!(detect(&[0; 4], u32::MAX, u32::MAX, 1), Err(CameraError::TooLarge));
        // Tiny, flat and noisy images find nothing and keep the whole image.
        let d = detect(&[7; 64], 8, 8, 1).unwrap();
        assert!(!d.found && d.quad[2] == [8.0, 8.0]);
        let flat = vec![128u8; 300 * 200 * 3];
        assert!(!detect(&flat, 300, 200, 3).unwrap().found);
        let noise: Vec<u8> = (0..300u32 * 200).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        assert!(!detect(&noise, 300, 200, 1).unwrap().found);
        let full = [[0.0, 0.0], [8.0, 0.0], [8.0, 8.0], [0.0, 8.0]];
        assert!(warp(&[7; 64], 8, 8, 1, &full, 0, 4).is_err());
        assert!(warp(&[7; 64], 8, 8, 1, &full, MAX_OUTPUT_SIDE, MAX_OUTPUT_SIDE).is_err());
        assert!(warp(&[7; 63], 8, 8, 1, &full, 4, 4).is_err());
        assert_eq!(warp(&[7; 64], 8, 8, 1, &full, 4, 4).unwrap(), vec![7; 16]);
        assert!(enhance(&mut [], 0, 0, 1).is_err());
        let mut one = [200u8];
        assert!(enhance(&mut one, 1, 1, 1).is_ok());
    }

    #[test]
    fn a_photographed_page_is_found_straightened_and_measured() {
        let (w, h) = (1000usize, 760usize);
        let f = 900.0;
        // An A4-like page (1 : 1.414) seen from the side and below, turned a little.
        let (project, truth) = camera(w as f64, h as f64, 210.0, 297.0, 0.35, -0.25, 0.08, 470.0, f);
        let px = photograph(w, h, &truth);
        let d = detect(&px, w as u32, h as u32, 1).unwrap();
        assert!(d.found && d.confidence > 0.7, "{d:?}");
        let err = max_corner_error(&d.quad, &truth);
        assert!(err < 2.0, "corners {:?} vs {truth:?}: {err}", d.quad);
        let p = plan(&d.quad, w as u32, h as u32).unwrap();
        assert!((p.aspect - 210.0 / 297.0).abs() < 0.02, "{p:?}");
        // The straightened page matches the page itself (lighting aside).
        let out = warp(&px, w as u32, h as u32, 1, &d.quad, p.width, p.height).unwrap();
        let (ow, oh) = (p.width as usize, p.height as usize);
        let mut diff = 0.0;
        let mut count = 0.0;
        for y in (oh / 20..oh - oh / 20).step_by(3) {
            for x in (ow / 20..ow - ow / 20).step_by(3) {
                let (u, v) = ((x as f64 + 0.5) / ow as f64, (y as f64 + 0.5) / oh as f64);
                let expect = page_luma(u, v) * (1.0 - 0.25 * u * v);
                diff += (f64::from(out[y * ow + x]) - expect).abs();
                count += 1.0;
            }
        }
        assert!(diff / count < 12.0, "mean difference {}", diff / count);
        let _ = project;
        // Enhancing removes the shadow and keeps the ink.
        let mut enhanced = out.clone();
        let stats = enhance(&mut enhanced, p.width, p.height, 1).unwrap();
        assert!(stats.paper_max - stats.paper_min > 30.0, "{stats:?}");
        let corner = |img: &[u8]| {
            let (x, y) = (ow * 9 / 10, oh * 97 / 100);
            img[y * ow + x]
        };
        assert!(corner(&out) < 200 && corner(&enhanced) > 245, "{} {}", corner(&out), corner(&enhanced));
        let dark = |img: &[u8]| img.iter().filter(|v| **v < 80).count();
        assert!(dark(&enhanced) as f64 > 0.8 * dark(&out) as f64);
    }

    #[test]
    fn a_frontal_page_has_its_plain_aspect_ratio() {
        let q = [[100.0, 50.0], [400.0, 50.0], [400.0, 250.0], [100.0, 250.0]];
        assert!((estimate_aspect(&q, 500.0, 300.0) - 1.5).abs() < 1e-9);
        let p = plan(&q, 500, 300).unwrap();
        assert_eq!((p.width, p.height), (300, 200));
    }
}
