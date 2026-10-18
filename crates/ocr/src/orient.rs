//! Automatic page orientation and deskew for recognition (Recognize text ▸ "Detect page rotation"
//! and "Deskew pages").
//!
//! **Exact mode.** Nothing here changes a page image. [`correct`] builds a corrected gray *copy*
//! of the rendered page for recognition only; every recognized word box is then mapped back to
//! the original image pixels through [`Corrected::to_source`], so the invisible text lies over
//! the visible, unchanged words.
//!
//! ## Method (original code)
//!
//! 1. A gray copy (RGBA composited over white, then luma) is area-averaged to at most
//!    [`ANALYSIS_SIDE`] pixels per side and binarized with Otsu's threshold. At most
//!    [`MAX_INK_SAMPLES`] dark pixels are kept (an even stride over the image).
//! 2. **Projection profiles.** For a line direction φ (degrees counterclockwise as seen on the
//!    page, y down), every dark pixel is binned by `y' = x·sin φ + y·cos φ`, which is constant
//!    along a baseline drawn at φ. The profile's concentration `extent · Σc² / N²` is 1 for ink
//!    spread evenly over its extent and grows when the ink falls into separated text lines, so it
//!    peaks at the text-line direction. φ is searched over ±15° around horizontal (0°) and around
//!    vertical (90°) in 0.5° steps, then refined in 0.1° steps.
//! 3. **Skew.** The best φ is used as the skew only when it is at least 0.25° from the axis and
//!    its concentration beats the axis-aligned one by at least 3 %; otherwise the page is
//!    treated as straight (no resampling).
//! 4. **Orientation.** A line axis leaves two readings (φ and φ + 180°). The axis whose best
//!    concentration is at least 10 % higher is chosen; when neither is, all four readings are
//!    tried. Each reading is corrected at most [`CANDIDATE_SIDE`] pixels per side and recognized
//!    with the caller's recognizer; the reading with the highest [`plausibility`] wins. Upright
//!    is kept unless another reading scores at least 25 % (and 4 characters) more, and nothing is
//!    turned when no reading is plausible. At most four candidate recognitions run per page.
//! 5. The chosen correction is applied to the full-resolution gray copy (bilinear sampling;
//!    quarter turns sample exact pixel centers, so they are lossless) for the final recognition.
//!
//! ## The transform
//!
//! With the text direction θ = rotation + skew (counterclockwise), the corrected image turns the
//! page clockwise by θ about its center into a canvas just large enough to hold it (`W × H`, white
//! outside the page). Coordinates are continuous pixel coordinates (pixel `i` spans `[i, i+1]`).
//! A corrected point `(x', y')` maps back to the source image as
//!
//! ```text
//! x = cos θ · (x' − W/2) + sin θ · (y' − H/2) + w/2
//! y = −sin θ · (x' − W/2) + cos θ · (y' − H/2) + h/2
//! ```
//!
//! which is [`Corrected::to_source`]. A word's corrected box corners therefore become a rotated
//! parallelogram in the source image, and [`crate::PlacedWord::place`] keeps its reading direction.

use crate::{Line, MAX_IMAGE_PIXELS, MAX_IMAGE_SIDE, OcrError, validate_image};

/// Longest side of the image used for projection profiles.
pub const ANALYSIS_SIDE: u32 = 1000;
/// Longest side of the images recognized to compare orientations.
pub const CANDIDATE_SIDE: u32 = 1600;
/// Dark pixels sampled for projection profiles.
pub const MAX_INK_SAMPLES: usize = 120_000;
/// Largest skew searched, in degrees either way.
pub const MAX_SKEW: f64 = 15.0;

/// Which corrections to look for. Both default to off (conservative): recognition then reads
/// the page exactly as rendered, as before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Detect pages turned by a quarter or half turn (0/90/180/270°).
    pub auto_rotate: bool,
    /// Detect and straighten pages skewed by up to ±15°.
    pub deskew: bool,
}

impl Options {
    pub fn any(&self) -> bool {
        self.auto_rotate || self.deskew
    }
}

/// The text direction found on a page (and corrected for recognition).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Orientation {
    /// Quarter-turn part of the text direction, degrees counterclockwise: 0 upright, 90 reads
    /// bottom to top, 180 upside down, 270 top to bottom.
    pub rotation: u16,
    /// Remaining skew in degrees, counterclockwise (positive: baselines rise to the right),
    /// within ±15°.
    pub skew: f32,
}

impl Orientation {
    /// The whole text direction θ = rotation + skew, in degrees counterclockwise.
    pub fn angle(&self) -> f64 {
        f64::from(self.rotation) + f64::from(self.skew)
    }
}

/// An affine map `[a, b, c, d, e, f]` (PDF order): `x = a·x' + c·y' + e`, `y = b·x' + d·y' + f`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine(pub [f64; 6]);

impl Affine {
    pub const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn apply(&self, x: f64, y: f64) -> [f64; 2] {
        let [a, b, c, d, e, f] = self.0;
        [a * x + c * y + e, b * x + d * y + f]
    }
}

/// An 8-bit gray image.
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

fn luma(r: u8, g: u8, b: u8) -> u8 {
    // Weights sum to 256, so the result is at most 255.
    ((77 * u32::from(r) + 150 * u32::from(g) + 29 * u32::from(b) + 128) >> 8) as u8
}

impl Gray {
    /// A gray copy of a packed gray, RGB or RGBA image (RGBA composited over white).
    pub fn from_pixels(pixels: &[u8], width: u32, height: u32) -> Result<Gray, OcrError> {
        let pixels = match validate_image(pixels, width, height)? {
            1 => pixels.to_vec(),
            3 => pixels.as_chunks::<3>().0.iter().map(|p| luma(p[0], p[1], p[2])).collect(),
            _ => pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| {
                    let alpha = u32::from(p[3]);
                    let over = |c: u8| ((u32::from(c) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
                    luma(over(p[0]), over(p[1]), over(p[2]))
                })
                .collect(),
        };
        Ok(Gray { width, height, pixels })
    }

    fn at(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return 255;
        }
        // Both are in range, so the product fits (validated at most 40 million pixels).
        self.pixels.get((y as usize) * (self.width as usize) + x as usize).copied().unwrap_or(255)
    }

    /// Area-average down by a whole factor so neither side exceeds `max_side` (never enlarges).
    pub fn downscale(&self, max_side: u32) -> Gray {
        let longest = self.width.max(self.height);
        let k = longest.div_ceil(max_side.max(1)).max(1);
        if k == 1 || self.width == 0 || self.height == 0 {
            return self.clone();
        }
        let (w, h) = (self.width.div_ceil(k), self.height.div_ceil(k));
        let mut sums = vec![0u64; (w as usize) * (h as usize)];
        let mut counts = vec![0u64; sums.len()];
        for (y, row) in self.pixels.chunks(self.width as usize).enumerate() {
            let base = (y / k as usize) * w as usize;
            for (x, v) in row.iter().enumerate() {
                let i = base + x / k as usize;
                if let (Some(s), Some(c)) = (sums.get_mut(i), counts.get_mut(i)) {
                    *s += u64::from(*v);
                    *c += 1;
                }
            }
        }
        let pixels = sums.iter().zip(&counts).map(|(s, c)| if *c == 0 { 255 } else { ((s + c / 2) / c) as u8 }).collect();
        Gray { width: w, height: h, pixels }
    }

    /// Turn the image clockwise by `angle` degrees (undoing text drawn `angle`° counterclockwise)
    /// into a canvas that holds all of it. Returns the copy and the map from its pixels back to
    /// this image's (see the module docs), or `None` when the canvas would exceed the
    /// recognition limits.
    pub fn turned(&self, angle: f64) -> Option<(Gray, Affine)> {
        let (c, s) = cos_sin(angle);
        let (w, h) = (f64::from(self.width), f64::from(self.height));
        let ow = (w * c.abs() + h * s.abs() - 1e-6).ceil().max(1.0);
        let oh = (w * s.abs() + h * c.abs() - 1e-6).ceil().max(1.0);
        if !ow.is_finite() || !oh.is_finite() || ow > f64::from(MAX_IMAGE_SIDE) || oh > f64::from(MAX_IMAGE_SIDE) || ow * oh > MAX_IMAGE_PIXELS as f64
        {
            return None;
        }
        let (ow, oh) = (ow as u32, oh as u32);
        let (cx, cy) = (f64::from(ow) / 2.0, f64::from(oh) / 2.0);
        let to_source = Affine([c, -s, s, c, -c * cx - s * cy + w / 2.0, s * cx - c * cy + h / 2.0]);
        let mut pixels = Vec::with_capacity((ow as usize) * (oh as usize));
        for j in 0..oh {
            for i in 0..ow {
                let [x, y] = to_source.apply(f64::from(i) + 0.5, f64::from(j) + 0.5);
                pixels.push(self.sample(x - 0.5, y - 0.5));
            }
        }
        Some((Gray { width: ow, height: oh, pixels }, to_source))
    }

    /// Bilinear sample at index coordinates (pixel centers at integers); white outside.
    fn sample(&self, x: f64, y: f64) -> u8 {
        if !x.is_finite() || !y.is_finite() || x < -1.0 || y < -1.0 || x > f64::from(self.width) || y > f64::from(self.height) {
            return 255;
        }
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (xi, yi) = (x0 as i64, y0 as i64);
        let p = |dx: i64, dy: i64| f64::from(self.at(xi + dx, yi + dy));
        let top = p(0, 0) * (1.0 - fx) + p(1, 0) * fx;
        let bottom = p(0, 1) * (1.0 - fx) + p(1, 1) * fx;
        (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8
    }
}

/// cos and sin of `degrees`, exact for quarter turns so they copy pixels without resampling.
fn cos_sin(degrees: f64) -> (f64, f64) {
    let d = degrees.rem_euclid(360.0);
    for (q, cs) in [(0.0, (1.0, 0.0)), (90.0, (0.0, 1.0)), (180.0, (-1.0, 0.0)), (270.0, (0.0, -1.0)), (360.0, (1.0, 0.0))] {
        if (d - q).abs() < 1e-9 {
            return cs;
        }
    }
    let r = d.to_radians();
    (r.cos(), r.sin())
}

/// Otsu's threshold over the histogram; `None` for a page without contrasting ink.
fn ink(gray: &Gray) -> Option<Vec<(f32, f32)>> {
    let mut hist = [0u64; 256];
    for v in &gray.pixels {
        hist[usize::from(*v)] += 1;
    }
    let total: u64 = hist.iter().sum();
    let sum_all: f64 = hist.iter().enumerate().map(|(i, c)| i as f64 * *c as f64).sum();
    let (mut best, mut threshold, mut weight_b, mut sum_b) = (0.0f64, 0usize, 0u64, 0.0f64);
    let mut means = (0.0, 0.0);
    for (t, count) in hist.iter().enumerate() {
        weight_b += count;
        sum_b += t as f64 * *count as f64;
        let weight_f = total - weight_b;
        if weight_b == 0 || weight_f == 0 {
            continue;
        }
        let (mb, mf) = (sum_b / weight_b as f64, (sum_all - sum_b) / weight_f as f64);
        let between = weight_b as f64 * weight_f as f64 * (mb - mf) * (mb - mf);
        if between > best {
            (best, threshold, means) = (between, t, (mb, mf));
        }
    }
    let dark: u64 = hist.iter().take(threshold + 1).sum();
    // Faint or flat pages, and pages that are mostly dark (photos, inverted scans), have no text
    // lines this method can measure.
    if means.1 - means.0 < 40.0 || dark == 0 || dark * 2 > total || dark * 1000 < total {
        return None;
    }
    let stride = usize::try_from(dark.div_ceil(MAX_INK_SAMPLES as u64)).unwrap_or(usize::MAX).max(1);
    let width = gray.width.max(1) as usize;
    Some(
        gray.pixels
            .iter()
            .enumerate()
            .filter(|(_, v)| usize::from(**v) <= threshold)
            .step_by(stride)
            .map(|(i, _)| ((i % width) as f32 + 0.5, (i / width) as f32 + 0.5))
            .collect(),
    )
}

/// Concentration of the projection profile across direction `phi` (see the module docs).
fn concentration(points: &[(f32, f32)], phi: f64) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    let (s, c) = (phi.to_radians().sin() as f32, phi.to_radians().cos() as f32);
    let project = |p: &(f32, f32)| p.0 * s + p.1 * c;
    let (lo, hi) = points.iter().map(project).fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let extent = (hi - lo).floor() as usize + 1;
    // Points come from an image at most ANALYSIS_SIDE pixels per side, so the extent is small.
    let mut bins = vec![0u32; extent.min(8 * ANALYSIS_SIDE as usize) + 1];
    for p in points {
        let i = ((project(p) - lo).floor() as usize).min(bins.len() - 1);
        if let Some(b) = bins.get_mut(i) {
            *b += 1;
        }
    }
    let n = points.len() as f64;
    let squares: f64 = bins.iter().map(|c| f64::from(*c) * f64::from(*c)).sum();
    extent as f64 * squares / (n * n)
}

/// The text-line direction within ±15° of `axis` (0 or 90): `(phi, its concentration, the
/// axis-aligned concentration)`.
fn best_direction(points: &[(f32, f32)], axis: f64) -> (f64, f64, f64) {
    let at_axis = concentration(points, axis);
    let mut best = (axis, at_axis);
    let coarse = (MAX_SKEW / 0.5) as i32;
    for k in -coarse..=coarse {
        let phi = axis + f64::from(k) * 0.5;
        let score = concentration(points, phi);
        if score > best.1 {
            best = (phi, score);
        }
    }
    let center = best.0;
    for k in -5..=5 {
        let phi = center + f64::from(k) * 0.1;
        if (phi - axis).abs() > MAX_SKEW + 1e-9 {
            continue;
        }
        let score = concentration(points, phi);
        if score > best.1 {
            best = (phi, score);
        }
    }
    (best.0, best.1, at_axis)
}

/// Dictionary-free text plausibility, used to compare readings of one page: the sum, over
/// recognized words that look like words, of their ASCII letters and digits, minus half the
/// length of the words that don't. A word looks like a word when at least 60 % of it is letters
/// or digits, its letters are all lower case, all upper case or lower case after one initial
/// capital, letters and digits change at most once ("M10", not "l0l"), and a word of four or
/// more letters has a vowel. Upside-down or sideways readings of Latin text mostly fail these.
pub fn plausibility(lines: &[Line]) -> f64 {
    let mut score = 0.0;
    for word in lines.iter().flat_map(|l| &l.words) {
        let chars = word.text.trim().chars().count();
        if chars == 0 {
            continue;
        }
        if word_shaped(&word.text) {
            score += word.text.chars().filter(char::is_ascii_alphanumeric).count() as f64;
        } else {
            score -= chars as f64 / 2.0;
        }
    }
    score
}

/// Whether recognized text looks like a word by the rules [`plausibility`] documents (at least
/// 60 % letters or digits, consistent case, at most one letter/digit switch, a vowel in words of
/// four or more letters). Empty text is not word-shaped.
pub fn word_shaped(text: &str) -> bool {
    let chars: Vec<char> = text.trim().chars().collect();
    let core: Vec<char> = chars.iter().copied().filter(char::is_ascii_alphanumeric).collect();
    let letters: Vec<char> = core.iter().copied().filter(char::is_ascii_alphabetic).collect();
    let case_ok = letters.iter().all(char::is_ascii_lowercase)
        || letters.iter().all(char::is_ascii_uppercase)
        || letters.iter().skip(1).all(char::is_ascii_lowercase);
    let switches = core.windows(2).filter(|p| p[0].is_ascii_alphabetic() != p[1].is_ascii_alphabetic()).count();
    let vowel = letters.len() < 4 || letters.iter().any(|c| "aeiouyAEIOUY".contains(*c));
    !core.is_empty() && core.len() * 10 >= chars.len() * 6 && case_ok && switches <= 1 && vowel
}

/// A corrected copy of a page for recognition, and how to map its pixels back.
#[derive(Clone, Debug, PartialEq)]
pub struct Corrected {
    /// What was found and corrected.
    pub orientation: Orientation,
    /// The corrected gray copy, or `None` when the page is read as it is.
    pub image: Option<Gray>,
    /// Maps corrected-image pixels to the source image's (identity when `image` is `None`).
    pub to_source: Affine,
}

impl Corrected {
    fn unchanged() -> Corrected {
        Corrected { orientation: Orientation::default(), image: None, to_source: Affine::IDENTITY }
    }
}

/// Find a page's text direction and build the corrected copy to recognize (see the module
/// docs). `recognize` reads a gray image; it runs at most four times on images of at most
/// [`CANDIDATE_SIDE`] pixels per side, and only when `options.auto_rotate` is set. The source
/// pixels are never modified.
pub fn correct(
    pixels: &[u8],
    width: u32,
    height: u32,
    options: Options,
    mut recognize: impl FnMut(&[u8], u32, u32) -> Result<Vec<Line>, OcrError>,
) -> Result<Corrected, OcrError> {
    if !options.any() {
        return Ok(Corrected::unchanged());
    }
    let gray = Gray::from_pixels(pixels, width, height)?;
    let analysis = gray.downscale(ANALYSIS_SIDE);
    let Some(points) = ink(&analysis) else { return Ok(Corrected::unchanged()) };
    let skew_of = |(phi, score, at_axis): (f64, f64, f64), axis: f64| {
        if options.deskew && (phi - axis).abs() >= 0.25 && score >= at_axis * 1.03 { phi } else { axis }
    };
    let horizontal = best_direction(&points, 0.0);
    let mut angle = skew_of(horizontal, 0.0);
    if options.auto_rotate {
        let vertical = best_direction(&points, 90.0);
        let (h, v) = (skew_of(horizontal, 0.0), skew_of(vertical, 90.0));
        let candidates: Vec<f64> = if horizontal.1 >= vertical.1 * 1.1 {
            vec![h, h + 180.0]
        } else if vertical.1 >= horizontal.1 * 1.1 {
            vec![v, v + 180.0]
        } else {
            vec![h, v, h + 180.0, v + 180.0]
        };
        let medium = gray.downscale(CANDIDATE_SIDE);
        let mut scored = Vec::with_capacity(candidates.len());
        for theta in candidates {
            let score = match medium.turned(theta) {
                Some((image, _)) => plausibility(&recognize(&image.pixels, image.width, image.height)?),
                None => f64::NEG_INFINITY,
            };
            scored.push((theta, score));
        }
        let upright = scored.iter().find(|(t, _)| t.rem_euclid(360.0) < 45.0 || t.rem_euclid(360.0) > 315.0).copied();
        let best = scored.iter().copied().fold(None::<(f64, f64)>, |b, c| if b.is_none_or(|b| c.1 > b.1) { Some(c) } else { b });
        angle = match (best, upright) {
            (Some(best), Some(up)) if best.1 >= up.1.max(0.0) * 1.25 + 4.0 => best.0,
            (_, Some(up)) => up.0,
            (Some(best), None) if best.1 > 4.0 => best.0,
            _ => 0.0,
        };
    }
    let theta = angle.rem_euclid(360.0);
    let quarter = ((theta / 90.0).round() as i64).rem_euclid(4);
    let rotation = (quarter * 90) as u16;
    let mut skew = (theta - f64::from(rotation)) as f32;
    if skew > 180.0 {
        skew -= 360.0;
    }
    if rotation == 0 && skew == 0.0 {
        return Ok(Corrected::unchanged());
    }
    let (image, to_source, orientation) = match gray.turned(theta) {
        Some((image, to)) => (image, to, Orientation { rotation, skew }),
        // A straightened canvas would exceed the recognition limits: keep the quarter turn,
        // which never enlarges the image.
        None => match gray.turned(f64::from(rotation)) {
            Some((image, to)) if rotation != 0 => (image, to, Orientation { rotation, skew: 0.0 }),
            _ => return Ok(Corrected::unchanged()),
        },
    };
    Ok(Corrected { orientation, image: Some(image), to_source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Word;

    /// Black bars standing in for text lines: `lines` rows of words on a white page.
    fn page(width: u32, height: u32) -> Gray {
        let mut pixels = vec![255u8; (width * height) as usize];
        for line in 0..12u32 {
            let y0 = 60 + line * 40;
            for word in 0..6u32 {
                let x0 = 50 + word * 70 + (line * 17) % 30;
                for y in y0..y0 + 14 {
                    for x in x0..x0 + 50 + (line * 7 + word * 13) % 15 {
                        pixels[(y * width + x) as usize] = 0;
                    }
                }
            }
        }
        Gray { width, height, pixels }
    }

    #[test]
    fn quarter_turns_copy_pixels_exactly_and_map_back() {
        let g = Gray { width: 3, height: 2, pixels: vec![1, 2, 3, 4, 5, 6] };
        let (t, to) = g.turned(90.0).unwrap();
        // Text drawn reading upward, turned clockwise: the right column becomes the top row.
        assert_eq!((t.width, t.height), (2, 3));
        assert_eq!(t.pixels, vec![4, 1, 5, 2, 6, 3]);
        let [x, y] = to.apply(0.5, 0.5);
        assert!((x - 0.5).abs() < 1e-9 && (y - 1.5).abs() < 1e-9, "{x} {y}");
        let (u, _) = g.turned(180.0).unwrap();
        assert_eq!(u.pixels, vec![6, 5, 4, 3, 2, 1]);
        let (back, _) = t.turned(270.0).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn transform_maps_corrected_points_onto_the_source() {
        let g = Gray { width: 200, height: 100, pixels: vec![255; 20_000] };
        for angle in [7.0, -3.0, 90.0, 183.0, 266.5] {
            let (t, to) = g.turned(angle).unwrap();
            // The corrected center is the source center, and lengths are preserved.
            let [cx, cy] = to.apply(f64::from(t.width) / 2.0, f64::from(t.height) / 2.0);
            assert!((cx - 100.0).abs() < 1e-6 && (cy - 50.0).abs() < 1e-6);
            let [ax, ay] = to.apply(0.0, 0.0);
            let [bx, by] = to.apply(10.0, 0.0);
            assert!(((bx - ax).hypot(by - ay) - 10.0).abs() < 1e-9);
            // A corrected horizontal step runs along the text direction in the source.
            let dir = (by - ay).atan2(bx - ax).to_degrees();
            assert!(((-dir - angle).rem_euclid(360.0) + 1e-6).rem_euclid(360.0) < 1e-3, "{angle}: {dir}");
        }
    }

    #[test]
    fn skew_is_found_on_synthetic_lines() {
        let upright = page(560, 600);
        for angle in [-7.0f64, -3.0, 3.0, 7.0, 12.0] {
            // Draw the page skewed: turning by -angle draws text at +angle.
            let (skewed, _) = upright.turned(-angle).unwrap();
            let points = ink(&skewed).unwrap();
            let (phi, score, at_axis) = best_direction(&points, 0.0);
            assert!((phi - angle).abs() <= 0.3, "{angle}: found {phi}");
            assert!(score > at_axis * 1.03);
        }
        let points = ink(&upright).unwrap();
        assert!(best_direction(&points, 0.0).0.abs() < 0.25);
        let (sideways, _) = upright.turned(90.0).unwrap();
        let points = ink(&sideways).unwrap();
        assert!(best_direction(&points, 90.0).1 > best_direction(&points, 0.0).1 * 1.1);
    }

    #[test]
    fn deskew_only_straightens_without_recognition() {
        let (skewed, _) = page(560, 600).turned(-5.0).unwrap();
        let options = Options { deskew: true, auto_rotate: false };
        let c = correct(&skewed.pixels, skewed.width, skewed.height, options, |_, _, _| panic!("no recognition without auto-rotate")).unwrap();
        assert!((c.orientation.skew - 5.0).abs() <= 0.3 && c.orientation.rotation == 0, "{:?}", c.orientation);
        assert!(c.image.is_some());
        let off = correct(&skewed.pixels, skewed.width, skewed.height, Options::default(), |_, _, _| panic!("off")).unwrap();
        assert_eq!(off, Corrected::unchanged());
    }

    #[test]
    fn orientation_follows_the_most_plausible_reading() {
        let (turned, _) = page(560, 600).turned(180.0).unwrap();
        let options = Options { auto_rotate: true, deskew: true };
        let mut seen = Vec::new();
        // A recognizer that reads words only when the bars start at the left margin.
        let c = correct(&turned.pixels, turned.width, turned.height, options, |px, w, _| {
            let left_ink = px.chunks(w as usize).any(|row| row.get(55).is_some_and(|v| *v < 128));
            seen.push(left_ink);
            let text = if left_ink { "Plain words" } else { "pIq" };
            Ok(vec![Line { words: vec![Word { text: text.into(), rect: [0.0, 0.0, 1.0, 1.0], confidence: None }; 10] }])
        })
        .unwrap();
        assert_eq!(seen.len(), 2, "one axis: two candidate readings");
        assert_eq!(c.orientation.rotation, 180, "{:?}", c.orientation);
        // Nothing plausible anywhere: the page is left as it is.
        let c = correct(&turned.pixels, turned.width, turned.height, options, |_, _, _| Ok(Vec::new())).unwrap();
        assert_eq!(c.orientation, Orientation::default());
    }

    #[test]
    fn blank_dark_and_hostile_images_are_left_unchanged() {
        let options = Options { auto_rotate: true, deskew: true };
        for pixels in [vec![255u8; 100 * 80], vec![0u8; 100 * 80]] {
            let c = correct(&pixels, 100, 80, options, |_, _, _| panic!("no recognition")).unwrap();
            assert_eq!(c, Corrected::unchanged());
        }
        assert!(correct(&[0, 1], 1, 1, options, |_, _, _| Ok(Vec::new())).is_err());
        assert!(correct(&[0], u32::MAX, u32::MAX, options, |_, _, _| Ok(Vec::new())).is_err());
    }

    #[test]
    fn plausibility_prefers_word_shaped_text() {
        let line = |words: &[&str]| Line { words: words.iter().map(|t| Word { text: (*t).into(), rect: [0.0; 4], confidence: None }).collect() };
        let good = plausibility(&[line(&["Searchable", "scans", "with", "M10", "words."])]);
        let bad = plausibility(&[line(&["sMoP", "l0l", "pxqrst", "'!:"])]);
        assert!(good > 20.0 && bad < 0.0, "{good} {bad}");
    }
}
