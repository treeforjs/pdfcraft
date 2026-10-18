//! Background removal: whiten the off-white, grey or yellowed paper of scanned page images
//! (Scan & OCR ▸ Enhance scan ▸ Remove background, and Recognize text's `background` option).
//!
//! Pure pixel code shared by OCR preprocessing (on a temporary gray copy of the rendered page)
//! and by [`crate::remove_page_image_backgrounds`] (which rewrites the page's image XObjects as
//! one undoable engine edit). It allocates only a 256-entry histogram and lookup tables.
//!
//! ## Method (original code)
//!
//! 1. **Paper level.** A luma histogram (Rec. 601 weights, as the OCR crate uses) is smoothed
//!    over ±2 levels and its highest peak among the light levels (at least
//!    [`MIN_BACKGROUND`]) is the paper. When less than [`MIN_PAPER_SHARE`] of the pixels lie
//!    within 16 levels of that peak the image is not a page on paper (a photograph, a dark or
//!    inverted scan) and it is left alone.
//! 2. **Paper noise.** The spread σ of the paper is measured on the light side of the peak
//!    (up to 32 levels above it), where no ink is mixed in; when the peak is at or near 255 the
//!    16 levels below it are used instead.
//! 3. **White point.** `white = paper − (base + k·σ)`, with `base`/`k` 6/1.5 for
//!    [`Strength::Low`], 12/2.5 for [`Strength::Medium`] and 20/3.5 for [`Strength::High`],
//!    the margin capped at 64 levels and the white point never below [`MIN_WHITE_POINT`], so
//!    mid-greys (pencil, halftone, shading) are never whitened. An explicit
//!    [`Options::white_point`] replaces the estimate.
//! 4. **Colour.** For RGB images the paper's own colour (the mean of the pixels at the paper
//!    level) gives each channel its white point, `paper_c − margin`, so a yellowed or tinted
//!    sheet becomes neutral white too. An explicit white point is used for every channel.
//! 5. **Levels curve.** Each channel goes through a curve that keeps `[0, white − 64]`
//!    unchanged, stretches `(white − 64, white)` linearly up to 255 and maps everything at or
//!    above `white` to 255. Ink and dark detail keep their values; anti-aliased edges stay smooth instead of
//!    getting a hard threshold halo.

use thiserror::Error;

/// Images larger than this are refused (time).
pub const MAX_PIXELS: u64 = 80_000_000;
/// The paper must be at least this light (luma) to be whitened.
pub const MIN_BACKGROUND: u8 = 128;
/// The white point (estimated or explicit) is never below this.
pub const MIN_WHITE_POINT: u8 = 128;
/// The share of pixels that must lie within 16 levels of the paper level.
pub const MIN_PAPER_SHARE: f64 = 0.30;

/// How far below the paper level pixels are pushed to white.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strength {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl Strength {
    pub const NAMES: [&'static str; 4] = ["off", "low", "medium", "high"];

    pub fn parse(name: &str) -> Option<Strength> {
        match name {
            "off" => Some(Strength::Off),
            "low" => Some(Strength::Low),
            "medium" => Some(Strength::Medium),
            "high" => Some(Strength::High),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Strength::Off => "off",
            Strength::Low => "low",
            Strength::Medium => "medium",
            Strength::High => "high",
        }
    }

    /// `(base, k)` of the margin `base + k·σ` below the paper level (`None` when off).
    fn margin(self) -> Option<(f64, f64)> {
        match self {
            Strength::Off => None,
            Strength::Low => Some((6.0, 1.5)),
            Strength::Medium => Some((12.0, 2.5)),
            Strength::High => Some((20.0, 3.5)),
        }
    }
}

/// What to remove. The default does nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// The margin below the estimated paper level that still becomes white.
    pub strength: Strength,
    /// An explicit white point (luma, [`MIN_WHITE_POINT`]–255): pixels at least this light
    /// become white, whatever the estimate. It still requires a light paper background.
    pub white_point: Option<u8>,
}

impl Options {
    /// Whether these options change anything.
    pub fn any(&self) -> bool {
        self.strength != Strength::Off || self.white_point.is_some()
    }
}

/// What a background-removal pass found and changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// The estimated paper level (luma), or `None` when the image has no light paper
    /// background (and was left alone).
    pub background: Option<u8>,
    /// The white point used (luma for gray; the luma white point for colour).
    pub white_point: Option<u8>,
    /// Pixels that were not white and now are.
    pub pixels_whitened: u64,
    /// Pixels whose value changed.
    pub pixels_changed: u64,
}

impl Stats {
    pub fn changed(&self) -> bool {
        self.pixels_changed > 0
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BackgroundError {
    #[error("the image to clean is empty")]
    Empty,
    #[error("background removal is limited to images of {MAX_PIXELS} pixels")]
    TooLarge,
    #[error("{0}")]
    Invalid(String),
}

type Result<T> = std::result::Result<T, BackgroundError>;

fn luma_of(p: &[u8]) -> u8 {
    match p {
        // Weights sum to 256, so the result is at most 255.
        [r, g, b, ..] => ((77 * u32::from(*r) + 150 * u32::from(*g) + 29 * u32::from(*b) + 128) >> 8) as u8,
        [v, ..] => *v,
        [] => 255,
    }
}

/// The paper level and noise of a luma histogram, or `None` when there is no light paper.
pub fn estimate_background(hist: &[u64; 256]) -> Option<(u8, f64)> {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return None;
    }
    let at = |l: usize| hist.get(l).copied().unwrap_or(0);
    let smoothed = |l: usize| (l.saturating_sub(2)..=l.saturating_add(2).min(255)).map(at).sum::<u64>();
    // The highest smoothed peak among light levels (then the highest raw count; remaining ties
    // go to the lighter level).
    let mut best = (0u64, 0u64, usize::from(MIN_BACKGROUND));
    for l in usize::from(MIN_BACKGROUND)..=255 {
        let key = (smoothed(l), at(l));
        if key >= (best.0, best.1) {
            best = (key.0, key.1, l);
        }
    }
    let paper = best.2;
    let near: u64 = (paper.saturating_sub(16)..=paper.saturating_add(16).min(255)).map(at).sum();
    if best.0 == 0 || (near as f64) < MIN_PAPER_SHARE * total as f64 {
        return None;
    }
    let spread = |range: std::ops::RangeInclusive<usize>| {
        let (mut n, mut sq) = (0u64, 0f64);
        for l in range {
            let c = at(l);
            let d = l as f64 - paper as f64;
            n = n.saturating_add(c);
            sq += c as f64 * d * d;
        }
        (n, if n > 0 { (sq / n as f64).sqrt() } else { 0.0 })
    };
    // The light side holds paper only; it needs enough pixels beyond the peak itself.
    let (above, sigma_above) = spread(paper.saturating_add(1).min(255)..=paper.saturating_add(32).min(255));
    let sigma = if paper < 255 && above.saturating_mul(20) >= near { sigma_above } else { spread(paper.saturating_sub(16)..=paper).1 };
    Some((u8::try_from(paper).unwrap_or(255), if sigma.is_finite() { sigma } else { 0.0 }))
}

/// Levels below the white point that the curve stretches (darker values are kept).
const RAMP: u32 = 64;

/// The levels curve that keeps `[0, white − RAMP]`, stretches up to `white` and whitens above.
fn curve(white: u8) -> [u8; 256] {
    let mut lut = [0u8; 256];
    let w = u32::from(white.max(2));
    let lo = w.saturating_sub(RAMP);
    for (v, out) in (0u32..).zip(lut.iter_mut()) {
        *out = if v >= w {
            255
        } else if v <= lo {
            v as u8
        } else {
            // w − lo ≥ 1 (w ≥ 2), and the result is below 255.
            (lo + (v - lo) * (255 - lo) / (w - lo)).min(255) as u8
        };
    }
    lut
}

/// Remove the paper background from packed 8-bit gray (`channels` 1) or RGB (`channels` 3)
/// pixels in place (exactly `width × height × channels` bytes).
pub fn remove_background_pixels(px: &mut [u8], width: u32, height: u32, channels: usize, options: Options) -> Result<Stats> {
    if width == 0 || height == 0 {
        return Err(BackgroundError::Empty);
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err(BackgroundError::TooLarge);
    }
    let count = usize::try_from(pixels).map_err(|_| BackgroundError::TooLarge)?;
    if !matches!(channels, 1 | 3) || count.checked_mul(channels) != Some(px.len()) {
        return Err(BackgroundError::Invalid("expected exactly one gray or three RGB bytes per pixel".into()));
    }
    if let Some(w) = options.white_point
        && w < MIN_WHITE_POINT
    {
        return Err(BackgroundError::Invalid(format!("the white point must be between {MIN_WHITE_POINT} and 255")));
    }
    let mut stats = Stats::default();
    let Some(margin) = options.strength.margin().map(Some).or(options.white_point.map(|_| None)) else { return Ok(stats) };
    let mut hist = [0u64; 256];
    for p in px.chunks_exact(channels) {
        if let Some(c) = hist.get_mut(usize::from(luma_of(p))) {
            *c += 1;
        }
    }
    let Some((paper, sigma)) = estimate_background(&hist) else { return Ok(stats) };
    stats.background = Some(paper);
    // Levels below the paper level that still become white.
    let drop = match (options.white_point, margin) {
        (Some(w), _) => i32::from(paper) - i32::from(w),
        (None, Some((base, k))) => (base + k * sigma).round().clamp(0.0, 64.0) as i32,
        (None, None) => return Ok(stats),
    };
    let white = |level: u8| -> u8 {
        let w = options.white_point.map_or(i32::from(level) - drop, i32::from);
        w.clamp(i32::from(MIN_WHITE_POINT), 255) as u8
    };
    stats.white_point = Some(white(paper));
    let luts: Vec<[u8; 256]> = if channels == 1 {
        vec![curve(white(paper))]
    } else {
        // The paper's own colour: the mean of the pixels at the paper level (±σ, at least 2).
        let band = (sigma.ceil() as i32).clamp(2, 16);
        let (mut sum, mut n) = ([0u64; 3], 0u64);
        for p in px.as_chunks::<3>().0 {
            if (i32::from(luma_of(p)) - i32::from(paper)).abs() <= band {
                for (s, v) in sum.iter_mut().zip(p) {
                    *s += u64::from(*v);
                }
                n += 1;
            }
        }
        let colour = sum.map(|s| s.saturating_add(n / 2).checked_div(n).map_or(paper, |m| m.min(255) as u8));
        colour.iter().map(|c| curve(white(*c))).collect()
    };
    for p in px.chunks_exact_mut(channels) {
        let was_white = p.iter().all(|v| *v == 255);
        let mut changed = false;
        for (v, lut) in p.iter_mut().zip(luts.iter().cycle()) {
            let new = lut.get(usize::from(*v)).copied().unwrap_or(*v);
            changed |= new != *v;
            *v = new;
        }
        if changed {
            stats.pixels_changed += 1;
            if !was_white && p.iter().all(|v| *v == 255) {
                stats.pixels_whitened += 1;
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 200;
    const H: u32 = 100;

    /// A gray "scan": paper at `paper` with ±`noise` dither, black text stems and a mid-grey
    /// pencil mark.
    fn page(paper: u8, noise: u8) -> Vec<u8> {
        (0..W * H)
            .map(|i| {
                let (x, y) = (i % W, i / W);
                if (20..24).contains(&x) && (20..80).contains(&y) || (40..44).contains(&x) && (20..80).contains(&y) {
                    12
                } else if (100..140).contains(&x) && (40..44).contains(&y) {
                    96
                } else {
                    let d = ((x * 7 + y * 13) % (2 * u32::from(noise) + 1)) as i32 - i32::from(noise);
                    (i32::from(paper) + d).clamp(0, 255) as u8
                }
            })
            .collect()
    }

    fn medium() -> Options {
        Options { strength: Strength::Medium, ..Default::default() }
    }

    #[test]
    fn grey_paper_becomes_white_and_ink_and_pencil_stay() {
        let mut px = page(200, 6);
        let stats = remove_background_pixels(&mut px, W, H, 1, medium()).unwrap();
        assert!(stats.background.is_some_and(|b| (196..=204).contains(&b)), "{stats:?}");
        assert!(stats.white_point.is_some_and(|w| w < 194), "{stats:?}");
        let paper = |px: &[u8]| (0..W * H).filter(|i| i % W > 150).map(|i| px[i as usize]).collect::<Vec<_>>();
        assert!(paper(&px).iter().all(|v| *v == 255), "paper left grey");
        assert_eq!(px[(50 * W + 21) as usize], 12, "ink changed");
        assert_eq!(px[(42 * W + 120) as usize], 96, "pencil changed");
        assert!(stats.pixels_whitened > u64::from(W * H) * 8 / 10, "{stats:?}");
        // A second pass finds white paper and changes nothing more.
        let again = remove_background_pixels(&mut px, W, H, 1, medium()).unwrap();
        assert_eq!(again.background, Some(255));
        assert!(!again.changed(), "{again:?}");
    }

    #[test]
    fn strengths_reach_further_below_the_paper() {
        let whites: Vec<u8> = [Strength::Low, Strength::Medium, Strength::High]
            .into_iter()
            .map(|strength| {
                remove_background_pixels(&mut page(210, 4), W, H, 1, Options { strength, ..Default::default() }).unwrap().white_point.unwrap()
            })
            .collect();
        assert!(whites[0] > whites[1] && whites[1] > whites[2], "{whites:?}");
        assert!(whites.iter().all(|w| *w >= MIN_WHITE_POINT));
        // Off does nothing.
        let mut px = page(210, 4);
        let before = px.clone();
        assert_eq!(remove_background_pixels(&mut px, W, H, 1, Options::default()).unwrap(), Stats::default());
        assert_eq!(px, before);
    }

    #[test]
    fn explicit_white_point_is_used_and_validated() {
        let mut px = page(180, 0);
        let stats = remove_background_pixels(&mut px, W, H, 1, Options { white_point: Some(175), ..Default::default() }).unwrap();
        assert_eq!(stats.white_point, Some(175));
        assert_eq!(px[(10 * W + 190) as usize], 255);
        let mut px = page(180, 0);
        let stats = remove_background_pixels(&mut px, W, H, 1, Options { white_point: Some(220), ..Default::default() }).unwrap();
        assert_ne!(px[(10 * W + 190) as usize], 255, "paper below the white point is only lightened: {stats:?}");
        assert!(px[(10 * W + 190) as usize] > 180);
        let bad = Options { white_point: Some(40), ..Default::default() };
        assert!(matches!(remove_background_pixels(&mut page(180, 0), W, H, 1, bad), Err(BackgroundError::Invalid(_))));
    }

    #[test]
    fn yellowed_colour_paper_turns_neutral_white_and_colour_ink_stays() {
        let mut px = Vec::new();
        for i in 0..W * H {
            let (x, y) = (i % W, i / W);
            if (20..60).contains(&x) && (20..30).contains(&y) {
                px.extend([200, 20, 20]); // red ink
            } else {
                px.extend([238, 228, 190]); // yellowed paper
            }
        }
        let stats = remove_background_pixels(&mut px, W, H, 3, medium()).unwrap();
        assert!(stats.changed());
        assert_eq!(&px[((80 * W + 150) * 3) as usize..][..3], &[255, 255, 255], "paper not neutral white");
        let red = &px[((25 * W + 30) * 3) as usize..][..3];
        assert!(red[1] < 40 && red[2] < 40, "red ink washed out: {red:?}");
    }

    #[test]
    fn photographs_and_dark_pages_are_left_alone() {
        // A smooth gradient has no paper peak.
        let mut px: Vec<u8> = (0..W * H).map(|i| (i % W * 255 / (W - 1)) as u8).collect();
        let before = px.clone();
        let stats = remove_background_pixels(&mut px, W, H, 1, medium()).unwrap();
        assert_eq!((stats.background, stats.changed()), (None, false));
        assert_eq!(px, before);
        // An inverted scan: dark paper, light text.
        let mut px: Vec<u8> = page(200, 4).iter().map(|v| 255 - v).collect();
        let before = px.clone();
        assert!(!remove_background_pixels(&mut px, W, H, 1, medium()).unwrap().changed());
        assert_eq!(px, before);
    }

    #[test]
    fn hostile_sizes_are_refused() {
        assert_eq!(remove_background_pixels(&mut [], 0, 5, 1, medium()), Err(BackgroundError::Empty));
        assert!(matches!(remove_background_pixels(&mut [0; 5], 2, 2, 1, medium()), Err(BackgroundError::Invalid(_))));
        assert!(matches!(remove_background_pixels(&mut [0; 4], 2, 2, 2, medium()), Err(BackgroundError::Invalid(_))));
        assert_eq!(remove_background_pixels(&mut [0; 4], u32::MAX, u32::MAX, 1, medium()), Err(BackgroundError::TooLarge));
        // One pixel, and all-white and all-black images.
        assert!(remove_background_pixels(&mut [200], 1, 1, 1, medium()).is_ok());
        let mut white = vec![255u8; 64];
        assert!(!remove_background_pixels(&mut white, 8, 8, 1, medium()).unwrap().changed());
        let mut black = vec![0u8; 64];
        assert!(!remove_background_pixels(&mut black, 8, 8, 1, medium()).unwrap().changed());
        assert_eq!(estimate_background(&[0; 256]), None);
    }

    #[test]
    fn curve_is_monotonic_and_keeps_dark_values() {
        for white in [MIN_WHITE_POINT, 180, 230, 255] {
            let lut = curve(white);
            assert!(lut.windows(2).all(|w| w[0] <= w[1]), "{white}");
            assert_eq!(lut[0], 0);
            assert_eq!(lut[usize::from(white) - 64], white - 64);
            assert_eq!(lut[255], 255);
            assert_eq!(lut[usize::from(white)], 255);
        }
    }
}
