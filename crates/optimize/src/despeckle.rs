//! Despeckle: remove isolated specks from scanned page images (Scan & OCR ▸ Recognize text ▸
//! despeckle, and the standalone Enhance scan ▸ Despeckle).
//!
//! Pure pixel code shared by OCR preprocessing (on a temporary gray copy of the rendered page)
//! and by [`crate::despeckle_page_images`] (which rewrites the page's image XObjects as one
//! undoable engine edit). Nothing here allocates more than a bit mask, a run list and a coarse
//! grid, all bounded by [`MAX_PIXELS`] and [`MAX_RUNS`].
//!
//! ## Method (original code)
//!
//! 1. **Ink mask.** Bilevel images use their samples directly (which value is ink is the
//!    caller's: `0` unless a `/Decode [1 0]` inverts it). Gray and colour images are binarized
//!    at Otsu's threshold over luma; a page without contrasting ink (the two class means less
//!    than 40 levels apart) or that is mostly dark (photos, inverted scans: more than half the
//!    pixels below the threshold) is left alone.
//! 2. **Connected components** of ink (8-connectivity), found from horizontal runs with a
//!    union-find over runs, so no per-pixel label image is needed.
//! 3. **Speck size from resolution.** A component is a speck candidate when it fits in a
//!    `speck × speck` pixel square, `speck = round(dpi × size)` (at least 1 px) where `size` is
//!    1/200 in (0.13 mm) for [`Level::Low`], 1/120 in (0.21 mm) for [`Level::Medium`] and
//!    1/75 in (0.34 mm) for [`Level::High`]. At 300 dpi that is 2, 3 and 4 px. A 10 pt period
//!    is about 0.35 mm across (≈ 4 px at 300 dpi), so Low and Medium never even consider it,
//!    and High relies on the next rule.
//! 4. **Punctuation is kept by proximity to text.** A component is *text-sized* when its
//!    longer side is at least `max(2·speck + 1, dpi/40)` px (0.64 mm, about the x-height of
//!    5 pt type). A speck candidate is kept when any text-sized ink lies within `reach =
//!    dpi/24` px (3 pt, 1.06 mm) of its bounding box. Periods, commas, colons and the dots of
//!    i and j sit within about 0.5–2 pt of the letter they belong to in 6–14 pt type, while
//!    scanner dust in margins and between lines is farther than 3 pt from any letter. Proximity
//!    is tested on a coarse grid of `reach`-sized cells, so the effective distance is between
//!    `reach` and `2·reach`: the rule errs towards keeping. Noise right beside letters is
//!    therefore kept too, by design.
//! 5. **Light specks** (optional): small holes of background inside solid ink (pinholes in
//!    black areas). A non-ink component that fits the speck square, does not touch the image
//!    border, and whose surrounding box (expanded by `speck + 1` px) is at least 90 % ink is
//!    filled. The counters of letters (`e`, `o`, `a`) fail the 90 % test because a stroke is
//!    thinner than that and background lies just beyond it.
//! 6. **Repainting.** Bilevel samples are flipped. Gray and colour specks are painted with the
//!    mean of the background pixels on a ring two pixels outside the speck (white when there are
//!    none), including the speck's anti-aliased halo within one pixel; holes are filled with
//!    the mean of the ink around them.
//! 7. **Impulse median** (optional, gray/colour only): before the component pass, a pixel whose
//!    luma is at least 64 levels darker than all eight neighbours (or, with light specks,
//!    lighter) takes the per-channel median of its 3×3 neighbourhood. Strokes one pixel wide
//!    survive (pixels along them have similar neighbours). It does not apply the punctuation
//!    rule, so it is opt-in; it can remove a one-pixel period at very low resolutions.

use thiserror::Error;

/// Images larger than this are refused (memory and time).
pub const MAX_PIXELS: u64 = 80_000_000;
/// Ink (or background) runs analysed per image; a page fragmented past this is refused rather
/// than left to run away.
pub const MAX_RUNS: usize = 8_000_000;
/// Resolutions accepted for sizing specks.
pub const MIN_DPI: f64 = 36.0;
pub const MAX_DPI: f64 = 4800.0;

/// How large a speck may be removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl Level {
    pub const NAMES: [&'static str; 4] = ["off", "low", "medium", "high"];

    pub fn parse(name: &str) -> Option<Level> {
        match name {
            "off" => Some(Level::Off),
            "low" => Some(Level::Low),
            "medium" => Some(Level::Medium),
            "high" => Some(Level::High),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Low => "low",
            Level::Medium => "medium",
            Level::High => "high",
        }
    }

    /// The side of the square a speck must fit in, in inches (`None` when off).
    pub fn speck_inches(self) -> Option<f64> {
        match self {
            Level::Off => None,
            Level::Low => Some(1.0 / 200.0),
            Level::Medium => Some(1.0 / 120.0),
            Level::High => Some(1.0 / 75.0),
        }
    }
}

/// What to remove. The default does nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Dark specks up to this size are removed (and light ones, with `light`).
    pub level: Level,
    /// Also fill small light holes inside solid ink.
    pub light: bool,
    /// Gray and colour images: also replace single-pixel impulses by their 3×3 median.
    pub median: bool,
}

impl Options {
    /// Whether these options change anything.
    pub fn any(&self) -> bool {
        self.level != Level::Off || self.median
    }
}

/// Pixel sizes derived from [`Options`] and a resolution (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    /// A speck fits in a square of this side.
    pub speck: u32,
    /// A component whose longer side is at least this is text-sized.
    pub glyph: u32,
    /// Specks with text-sized ink within this distance are kept.
    pub reach: u32,
}

impl Thresholds {
    /// The thresholds for `level` at `dpi`, or `None` when the level is off.
    pub fn new(level: Level, dpi: f64) -> Result<Option<Thresholds>> {
        if !dpi.is_finite() || !(MIN_DPI..=MAX_DPI).contains(&dpi) {
            return Err(DespeckleError::Invalid(format!("the despeckle resolution must be between {MIN_DPI} and {MAX_DPI} dpi")));
        }
        let Some(inches) = level.speck_inches() else { return Ok(None) };
        // dpi ≤ 4800, so every value below is small and the casts are exact.
        let speck = ((dpi * inches + 0.5).floor() as u32).max(1);
        let glyph = (2 * speck + 1).max((dpi / 40.0).ceil() as u32);
        let reach = ((dpi / 24.0).ceil() as u32).max(1);
        Ok(Some(Thresholds { speck, glyph, reach }))
    }
}

/// What a despeckle pass changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Dark specks removed.
    pub dark_specks: usize,
    /// Light holes filled.
    pub light_specks: usize,
    /// Impulse pixels replaced by their median.
    pub impulses: usize,
    /// Pixels whose value changed.
    pub pixels_changed: u64,
}

impl Stats {
    pub fn changed(&self) -> bool {
        self.pixels_changed > 0
    }

    pub fn add(&mut self, other: &Stats) {
        self.dark_specks = self.dark_specks.saturating_add(other.dark_specks);
        self.light_specks = self.light_specks.saturating_add(other.light_specks);
        self.impulses = self.impulses.saturating_add(other.impulses);
        self.pixels_changed = self.pixels_changed.saturating_add(other.pixels_changed);
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DespeckleError {
    #[error("the image to despeckle is empty")]
    Empty,
    #[error("despeckling is limited to images of {MAX_PIXELS} pixels")]
    TooLarge,
    #[error("the image is too fragmented to despeckle (more than {MAX_RUNS} runs)")]
    TooComplex,
    #[error("{0}")]
    Invalid(String),
}

type Result<T> = std::result::Result<T, DespeckleError>;

fn checked_size(width: u32, height: u32) -> Result<(usize, usize)> {
    if width == 0 || height == 0 {
        return Err(DespeckleError::Empty);
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(DespeckleError::TooLarge);
    }
    let w = usize::try_from(width).map_err(|_| DespeckleError::TooLarge)?;
    let h = usize::try_from(height).map_err(|_| DespeckleError::TooLarge)?;
    w.checked_mul(h).ok_or(DespeckleError::TooLarge)?;
    Ok((w, h))
}

/// A one-bit-per-pixel mask, rows padded to whole 64-bit words.
struct Mask {
    w: usize,
    h: usize,
    stride: usize,
    words: Vec<u64>,
}

impl Mask {
    fn new(w: usize, h: usize) -> Result<Mask> {
        let stride = w.div_ceil(64);
        let len = stride.checked_mul(h).ok_or(DespeckleError::TooLarge)?;
        Ok(Mask { w, h, stride, words: vec![0; len] })
    }

    fn get(&self, x: usize, y: usize) -> bool {
        x < self.w && y < self.h && self.words.get(y * self.stride + x / 64).is_some_and(|w| (w >> (x % 64)) & 1 == 1)
    }

    fn set(&mut self, x: usize, y: usize, v: bool) {
        if x >= self.w || y >= self.h {
            return;
        }
        if let Some(w) = self.words.get_mut(y * self.stride + x / 64) {
            if v {
                *w |= 1 << (x % 64);
            } else {
                *w &= !(1 << (x % 64));
            }
        }
    }
}

/// A horizontal run `[x0, x1)` on row `y`.
#[derive(Clone, Copy, Debug)]
struct Run {
    y: u32,
    x0: u32,
    x1: u32,
}

/// A connected component's area and inclusive bounding box.
#[derive(Clone, Copy, Debug)]
struct Comp {
    area: u64,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Comp {
    fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0).saturating_add(1)
    }
    fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0).saturating_add(1)
    }
}

fn find(parent: &mut [u32], mut i: u32) -> u32 {
    // Path halving; parents always point to smaller or equal indices, so this terminates.
    loop {
        let Some(&p) = parent.get(i as usize) else { return i };
        if p == i {
            return i;
        }
        let gp = parent.get(p as usize).copied().unwrap_or(p);
        if let Some(slot) = parent.get_mut(i as usize) {
            *slot = gp;
        }
        i = gp;
    }
}

fn union(parent: &mut [u32], a: u32, b: u32) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
    if lo != hi
        && let Some(slot) = parent.get_mut(hi as usize)
    {
        *slot = lo;
    }
}

/// Runs of pixels whose mask value is `value`, their component index, and the components
/// (8-connectivity).
fn components(mask: &Mask, value: bool) -> Result<(Vec<Run>, Vec<u32>, Vec<Comp>)> {
    let mut runs: Vec<Run> = Vec::new();
    let mut parent: Vec<u32> = Vec::new();
    let mut prev = 0..0;
    for y in 0..mask.h {
        let start = runs.len();
        let mut x = 0;
        while x < mask.w {
            if mask.get(x, y) != value {
                x += 1;
                continue;
            }
            let x0 = x;
            while x < mask.w && mask.get(x, y) == value {
                x += 1;
            }
            if runs.len() >= MAX_RUNS {
                return Err(DespeckleError::TooComplex);
            }
            // w, h ≤ MAX_PIXELS < 2^32, and runs.len() < MAX_RUNS < 2^32.
            let id = runs.len() as u32;
            runs.push(Run { y: y as u32, x0: x0 as u32, x1: x as u32 });
            parent.push(id);
        }
        // Join with the previous row's runs that touch (8-connectivity).
        let mut j = prev.start;
        for i in start..runs.len() {
            let Some(cur) = runs.get(i).copied() else { continue };
            while runs.get(j).is_some_and(|p| j < prev.end && p.x1 < cur.x0) {
                j += 1;
            }
            let mut k = j;
            while k < prev.end {
                let Some(p) = runs.get(k).copied() else { break };
                if p.x0 > cur.x1 {
                    break;
                }
                union(&mut parent, k as u32, i as u32);
                k += 1;
            }
        }
        prev = start..runs.len();
    }
    let mut index = vec![u32::MAX; runs.len()];
    let mut comps: Vec<Comp> = Vec::new();
    let mut of_run = Vec::with_capacity(runs.len());
    for (i, run) in runs.iter().enumerate() {
        let root = find(&mut parent, i as u32) as usize;
        let slot = index.get(root).copied().unwrap_or(u32::MAX);
        let c = if slot == u32::MAX {
            let c = comps.len() as u32;
            if let Some(s) = index.get_mut(root) {
                *s = c;
            }
            comps.push(Comp { area: 0, x0: run.x0, y0: run.y, x1: run.x1.saturating_sub(1), y1: run.y });
            c
        } else {
            slot
        };
        if let Some(comp) = comps.get_mut(c as usize) {
            comp.area = comp.area.saturating_add(u64::from(run.x1 - run.x0));
            comp.x0 = comp.x0.min(run.x0);
            comp.x1 = comp.x1.max(run.x1.saturating_sub(1));
            comp.y0 = comp.y0.min(run.y);
            comp.y1 = comp.y1.max(run.y);
        }
        of_run.push(c);
    }
    Ok((runs, of_run, comps))
}

/// Runs, the component of each run, the components, and which components are chosen.
type Chosen = (Vec<Run>, Vec<u32>, Vec<Comp>, Vec<bool>);

/// Which components to change: dark specks to remove and light holes to fill.
struct Plan {
    dark: Chosen,
    light: Option<Chosen>,
}

fn plan(mask: &Mask, t: Thresholds, light: bool) -> Result<Plan> {
    let (runs, of_run, comps) = components(mask, true)?;
    let glyph: Vec<bool> = comps.iter().map(|c| c.width().max(c.height()) >= t.glyph).collect();
    // Coarse grid of cells holding text-sized ink.
    let cell = t.reach.max(1) as usize;
    let (gw, gh) = (mask.w.div_ceil(cell), mask.h.div_ceil(cell));
    let mut grid = vec![false; gw.checked_mul(gh).ok_or(DespeckleError::TooLarge)?];
    for (run, c) in runs.iter().zip(&of_run) {
        if !glyph.get(*c as usize).copied().unwrap_or(false) {
            continue;
        }
        let gy = run.y as usize / cell;
        for gx in run.x0 as usize / cell..=(run.x1.saturating_sub(1) as usize) / cell {
            if let Some(g) = grid.get_mut(gy * gw + gx) {
                *g = true;
            }
        }
    }
    let near_text = |c: &Comp| {
        let r = t.reach;
        let (x0, y0) = (c.x0.saturating_sub(r) as usize / cell, c.y0.saturating_sub(r) as usize / cell);
        let x1 = (c.x1.saturating_add(r) as usize / cell).min(gw.saturating_sub(1));
        let y1 = (c.y1.saturating_add(r) as usize / cell).min(gh.saturating_sub(1));
        (y0..=y1).any(|gy| (x0..=x1).any(|gx| grid.get(gy * gw + gx).copied().unwrap_or(false)))
    };
    let small = |c: &Comp| c.width() <= t.speck && c.height() <= t.speck;
    let remove: Vec<bool> = comps.iter().map(|c| small(c) && !near_text(c)).collect();
    let light = if light {
        let (lruns, lof, lcomps) = components(mask, false)?;
        let fill: Vec<bool> = lcomps
            .iter()
            .map(|c| {
                if !small(c) || c.x0 == 0 || c.y0 == 0 || c.x1 as usize + 1 >= mask.w || c.y1 as usize + 1 >= mask.h {
                    return false;
                }
                let r = t.speck.saturating_add(1);
                let (x0, y0) = (c.x0.saturating_sub(r) as usize, c.y0.saturating_sub(r) as usize);
                let x1 = (c.x1.saturating_add(r) as usize).min(mask.w - 1);
                let y1 = (c.y1.saturating_add(r) as usize).min(mask.h - 1);
                let mut ink = 0u64;
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        ink += u64::from(mask.get(x, y));
                    }
                }
                let around = ((x1 - x0 + 1) as u64 * (y1 - y0 + 1) as u64).saturating_sub(c.area);
                around > 0 && ink * 10 >= around * 9
            })
            .collect();
        Some((lruns, lof, lcomps, fill))
    } else {
        None
    };
    Ok(Plan { dark: (runs, of_run, comps, remove), light })
}

/// Despeckle packed one-bit rows (most significant bit first, rows padded to whole bytes;
/// exactly `ceil(width/8) × height` bytes). `ink_is_one` says which sample value is ink.
pub fn despeckle_bits(bits: &mut [u8], width: u32, height: u32, ink_is_one: bool, dpi: f64, options: Options) -> Result<Stats> {
    let (w, h) = checked_size(width, height)?;
    let stride = w.div_ceil(8);
    if stride.checked_mul(h) != Some(bits.len()) {
        return Err(DespeckleError::Invalid("expected one bit per pixel, rows padded to whole bytes".into()));
    }
    let Some(t) = Thresholds::new(options.level, dpi)? else { return Ok(Stats::default()) };
    let bit = |bits: &[u8], x: usize, y: usize| bits.get(y * stride + x / 8).is_some_and(|b| (b >> (7 - x % 8)) & 1 == 1);
    let mut mask = Mask::new(w, h)?;
    let mut ink = 0u64;
    for y in 0..h {
        for x in 0..w {
            if bit(bits, x, y) == ink_is_one {
                mask.set(x, y, true);
                ink += 1;
            }
        }
    }
    // Mostly ink: an inverted scan or the wrong polarity; nothing here is a speck on paper.
    if ink == 0 || ink.saturating_mul(2) > (w as u64).saturating_mul(h as u64) {
        return Ok(Stats::default());
    }
    let plan = plan(&mask, t, options.light)?;
    let mut stats = Stats::default();
    let mut paint = |runs: &[Run], of_run: &[u32], chosen: &[bool], to: bool, count: &mut usize, stats_pixels: &mut u64| {
        for (run, c) in runs.iter().zip(of_run) {
            if !chosen.get(*c as usize).copied().unwrap_or(false) {
                continue;
            }
            for x in run.x0 as usize..run.x1 as usize {
                if let Some(b) = bits.get_mut(run.y as usize * stride + x / 8) {
                    let m = 0x80u8 >> (x % 8);
                    if to {
                        *b |= m;
                    } else {
                        *b &= !m;
                    }
                    *stats_pixels += 1;
                }
            }
        }
        *count += chosen.iter().filter(|c| **c).count();
    };
    let (runs, of_run, _, remove) = &plan.dark;
    let mut pixels = 0u64;
    paint(runs, of_run, remove, !ink_is_one, &mut stats.dark_specks, &mut pixels);
    if let Some((runs, of_run, _, fill)) = &plan.light {
        paint(runs, of_run, fill, ink_is_one, &mut stats.light_specks, &mut pixels);
    }
    stats.pixels_changed = pixels;
    Ok(stats)
}

fn luma_of(p: &[u8]) -> u8 {
    match p {
        [r, g, b, ..] => ((77 * u32::from(*r) + 150 * u32::from(*g) + 29 * u32::from(*b) + 128) >> 8) as u8,
        [v, ..] => *v,
        [] => 255,
    }
}

/// Otsu's threshold over a luma histogram, or `None` when the image has no contrasting ink or
/// is mostly dark (module docs, step 1).
fn document_threshold(hist: &[u64; 256]) -> Option<u8> {
    let total: u64 = hist.iter().sum();
    let sum_all: f64 = hist.iter().enumerate().map(|(i, c)| i as f64 * *c as f64).sum();
    let (mut best, mut threshold, mut weight_b, mut sum_b, mut means) = (0.0f64, 0usize, 0u64, 0.0f64, (0.0, 0.0));
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
    if means.1 - means.0 < 40.0 || dark == 0 || dark * 2 > total {
        return None;
    }
    u8::try_from(threshold).ok()
}

/// Replace impulse pixels by their 3×3 median (module docs, step 7). Reads original values from
/// a three-row window, so it needs only two row copies.
fn impulse_median(px: &mut [u8], w: usize, h: usize, n: usize, light: bool) -> Stats {
    let mut stats = Stats::default();
    if w < 3 || h < 3 {
        return stats;
    }
    let row = w * n;
    let mut above: Vec<u8> = px.get(..row).map(<[u8]>::to_vec).unwrap_or_default();
    let mut here: Vec<u8> = Vec::with_capacity(row);
    for y in 1..h - 1 {
        here.clear();
        here.extend_from_slice(px.get(y * row..(y + 1) * row).unwrap_or_default());
        let Some(below) = px.get((y + 1) * row..(y + 2) * row).map(<[u8]>::to_vec) else { break };
        for x in 1..w - 1 {
            let rows = [&above, &here, &below];
            let at = |r: usize, dx: usize| rows.get(r).and_then(|row| row.get((x + dx - 1) * n..(x + dx) * n)).unwrap_or_default();
            let centre = luma_of(at(1, 1));
            let (mut lo, mut hi) = (255u8, 0u8);
            for r in 0..3 {
                for dx in 0..3 {
                    if r == 1 && dx == 1 {
                        continue;
                    }
                    let l = luma_of(at(r, dx));
                    lo = lo.min(l);
                    hi = hi.max(l);
                }
            }
            let dark = u16::from(centre) + 64 <= u16::from(lo);
            let bright = light && u16::from(centre) >= u16::from(hi) + 64;
            if !dark && !bright {
                continue;
            }
            let mut changed = false;
            for c in 0..n {
                let mut v = [0u8; 9];
                for (i, slot) in v.iter_mut().enumerate() {
                    *slot = at(i / 3, i % 3).get(c).copied().unwrap_or(255);
                }
                v.sort_unstable();
                if let Some(p) = px.get_mut(y * row + x * n + c) {
                    if *p != v[4] {
                        changed = true;
                    }
                    *p = v[4];
                }
            }
            if changed {
                stats.impulses += 1;
                stats.pixels_changed += 1;
            }
        }
        std::mem::swap(&mut above, &mut here);
    }
    stats
}

/// Despeckle packed 8-bit gray (`channels` 1) or RGB (`channels` 3) pixels in place
/// (exactly `width × height × channels` bytes). `dpi` is the image's resolution.
pub fn despeckle_pixels(px: &mut [u8], width: u32, height: u32, channels: usize, dpi: f64, options: Options) -> Result<Stats> {
    let (w, h) = checked_size(width, height)?;
    if !matches!(channels, 1 | 3) || (w * h).checked_mul(channels) != Some(px.len()) {
        return Err(DespeckleError::Invalid("expected exactly one gray or three RGB bytes per pixel".into()));
    }
    let n = channels;
    let thresholds = Thresholds::new(options.level, dpi)?;
    let mut stats = Stats::default();
    if options.median {
        stats.add(&impulse_median(px, w, h, n, options.light));
    }
    let Some(t) = thresholds else { return Ok(stats) };
    let mut hist = [0u64; 256];
    for p in px.chunks_exact(n) {
        if let Some(c) = hist.get_mut(usize::from(luma_of(p))) {
            *c += 1;
        }
    }
    let Some(threshold) = document_threshold(&hist) else { return Ok(stats) };
    let luma = |px: &[u8], x: usize, y: usize| px.get((y * w + x) * n..(y * w + x + 1) * n).map_or(255, luma_of);
    let mut mask = Mask::new(w, h)?;
    for y in 0..h {
        for x in 0..w {
            if luma(px, x, y) <= threshold {
                mask.set(x, y, true);
            }
        }
    }
    let plan = plan(&mask, t, options.light)?;
    // The mean colour of pixels on the rectangle `margin` px outside `c` whose ink-ness is `ink`.
    let ring_mean = |px: &[u8], c: &Comp, margin: u32, ink: bool| -> Option<[u8; 3]> {
        let (x0, y0) = (c.x0.saturating_sub(margin) as usize, c.y0.saturating_sub(margin) as usize);
        let x1 = (c.x1.saturating_add(margin) as usize).min(w - 1);
        let y1 = (c.y1.saturating_add(margin) as usize).min(h - 1);
        let (mut sum, mut count) = ([0u64; 3], 0u64);
        for y in y0..=y1 {
            for x in x0..=x1 {
                if (y != y0 && y != y1 && x != x0 && x != x1) || mask.get(x, y) != ink {
                    continue;
                }
                let Some(p) = px.get((y * w + x) * n..(y * w + x + 1) * n) else { continue };
                for (s, v) in sum.iter_mut().zip(p.iter().cycle()) {
                    *s += u64::from(*v);
                }
                count += 1;
            }
        }
        (count > 0).then(|| sum.map(|s| ((s + count / 2) / count) as u8))
    };
    let mut changed = 0u64;
    let mut set = |px: &mut [u8], x: usize, y: usize, colour: [u8; 3]| {
        if let Some(p) = px.get_mut((y * w + x) * n..(y * w + x + 1) * n) {
            if p.iter().zip(colour).any(|(a, b)| *a != b) {
                changed += 1;
            }
            for (a, b) in p.iter_mut().zip(colour) {
                *a = b;
            }
        }
    };
    let (runs, of_run, comps, remove) = &plan.dark;
    let colours: Vec<Option<[u8; 3]>> = comps.iter().zip(remove).map(|(c, r)| r.then(|| ring_mean(px, c, 2, false).unwrap_or([255; 3]))).collect();
    for (run, c) in runs.iter().zip(of_run) {
        let Some(Some(colour)) = colours.get(*c as usize) else { continue };
        for x in run.x0 as usize..run.x1 as usize {
            set(px, x, run.y as usize, *colour);
        }
    }
    // The anti-aliased halo within one pixel of each removed speck (background-class pixels).
    for (c, colour) in comps.iter().zip(&colours) {
        let Some(colour) = colour else { continue };
        let (x0, y0) = (c.x0.saturating_sub(1) as usize, c.y0.saturating_sub(1) as usize);
        let (x1, y1) = ((c.x1 as usize + 1).min(w - 1), (c.y1 as usize + 1).min(h - 1));
        for y in y0..=y1 {
            for x in x0..=x1 {
                if !mask.get(x, y) {
                    set(px, x, y, *colour);
                }
            }
        }
    }
    stats.dark_specks += remove.iter().filter(|r| **r).count();
    if let Some((runs, of_run, comps, fill)) = &plan.light {
        let colours: Vec<Option<[u8; 3]>> = comps.iter().zip(fill).map(|(c, f)| f.then(|| ring_mean(px, c, 1, true).unwrap_or([0; 3]))).collect();
        for (run, c) in runs.iter().zip(of_run) {
            let Some(Some(colour)) = colours.get(*c as usize) else { continue };
            for x in run.x0 as usize..run.x1 as usize {
                set(px, x, run.y as usize, *colour);
            }
        }
        stats.light_specks += fill.iter().filter(|f| **f).count();
    }
    stats.pixels_changed = stats.pixels_changed.saturating_add(changed);
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: u8 = 255;

    /// A white gray image with `rects` (x, y, w, h) painted black.
    fn page(w: u32, h: u32, rects: &[(u32, u32, u32, u32)]) -> Vec<u8> {
        let mut px = vec![WHITE; (w * h) as usize];
        for &(x0, y0, rw, rh) in rects {
            for y in y0..y0 + rh {
                for x in x0..x0 + rw {
                    px[(y * w + x) as usize] = 0;
                }
            }
        }
        px
    }

    fn dark_at(px: &[u8], w: u32, x: u32, y: u32) -> bool {
        px[(y * w + x) as usize] < 128
    }

    fn opts(level: Level) -> Options {
        Options { level, ..Default::default() }
    }

    /// A line of "text" at 300 dpi: letter-sized stems (4 × 30 px), a period (4 × 4) 3 px after
    /// the last stem, an i with its dot (4 × 4) 4 px above the stem, plus 1–3 px specks far away.
    type Points = Vec<(u32, u32)>;

    fn text_page() -> (u32, u32, Vec<u8>, Points, Points) {
        let (w, h) = (400, 200);
        let mut rects = vec![];
        for i in 0..6 {
            rects.push((40 + i * 14, 80, 4, 30));
        }
        // i: stem and its dot.
        rects.push((130, 88, 4, 22));
        rects.push((130, 80, 4, 4));
        // Period after the line.
        rects.push((137, 106, 4, 4));
        // Specks: one pixel, two by two, three by three, far from the text.
        let specks = vec![(20, 20), (300, 30), (350, 160), (200, 170)];
        rects.push((20, 20, 1, 1));
        rects.push((300, 30, 2, 2));
        rects.push((350, 160, 3, 3));
        rects.push((200, 170, 2, 1));
        let kept = vec![(41, 90), (131, 81), (138, 107), (131, 100)];
        (w, h, page(w, h, &rects), specks, kept)
    }

    #[test]
    fn thresholds_scale_with_resolution() {
        let at = |level, dpi| Thresholds::new(level, dpi).unwrap().unwrap();
        assert_eq!(at(Level::Low, 300.0).speck, 2);
        assert_eq!(at(Level::Medium, 300.0).speck, 3);
        assert_eq!(at(Level::High, 300.0).speck, 4);
        assert_eq!(at(Level::High, 600.0).speck, 8);
        assert_eq!(at(Level::Low, 72.0).speck, 1);
        assert_eq!(at(Level::High, 300.0).reach, 13);
        assert!(at(Level::High, 600.0).glyph > at(Level::High, 300.0).glyph);
        assert_eq!(Thresholds::new(Level::Off, 300.0).unwrap(), None);
        for bad in [f64::NAN, f64::INFINITY, 0.0, -300.0, 10.0, 1e9] {
            assert!(Thresholds::new(Level::Low, bad).is_err(), "{bad}");
        }
        for (i, name) in Level::NAMES.iter().enumerate() {
            assert_eq!(Level::parse(name).unwrap().name(), Level::NAMES[i]);
        }
        assert_eq!(Level::parse("extreme"), None);
    }

    #[test]
    fn specks_are_removed_and_text_strokes_and_dots_kept() {
        let (w, h, mut px, specks, kept) = text_page();
        let stats = despeckle_pixels(&mut px, w, h, 1, 300.0, opts(Level::High)).unwrap();
        assert_eq!(stats.dark_specks, 4, "{stats:?}");
        for (x, y) in specks {
            assert!(!dark_at(&px, w, x, y), "speck at {x},{y} kept");
        }
        for (x, y) in kept {
            assert!(dark_at(&px, w, x, y), "text at {x},{y} removed");
        }
    }

    #[test]
    fn a_lone_dot_far_from_text_is_a_speck_but_the_same_dot_beside_text_is_kept() {
        let (w, h) = (300, 100);
        let mut px = page(w, h, &[(20, 20, 4, 30), (27, 46, 4, 4), (200, 46, 4, 4)]);
        despeckle_pixels(&mut px, w, h, 1, 300.0, opts(Level::High)).unwrap();
        assert!(dark_at(&px, w, 28, 47), "period beside the stem removed");
        assert!(!dark_at(&px, w, 201, 47), "lone dot kept");
    }

    #[test]
    fn levels_remove_only_specks_up_to_their_size() {
        let (w, h) = (300, 100);
        let base = page(w, h, &[(20, 20, 1, 1), (60, 20, 2, 2), (100, 20, 3, 3), (140, 20, 4, 4), (180, 20, 5, 5)]);
        let survivors = |level| {
            let mut px = base.clone();
            despeckle_pixels(&mut px, w, h, 1, 300.0, opts(level)).unwrap();
            [20, 60, 100, 140, 180].iter().filter(|x| dark_at(&px, w, **x, 20)).count()
        };
        assert_eq!(survivors(Level::Off), 5);
        assert_eq!(survivors(Level::Low), 3);
        assert_eq!(survivors(Level::Medium), 2);
        assert_eq!(survivors(Level::High), 1);
        // The same 3 px speck is larger than Medium's limit at 150 dpi (1 px)...
        let mut px = base.clone();
        despeckle_pixels(&mut px, w, h, 1, 150.0, opts(Level::Medium)).unwrap();
        assert!(dark_at(&px, w, 100, 20));
        // ...and within it at 600 dpi (5 px).
        let mut px = base.clone();
        despeckle_pixels(&mut px, w, h, 1, 600.0, opts(Level::Medium)).unwrap();
        assert!(!dark_at(&px, w, 100, 20));
        assert!(!dark_at(&px, w, 180, 20) && !dark_at(&px, w, 140, 20));
    }

    #[test]
    fn bilevel_samples_follow_the_ink_polarity() {
        // 64 × 16, one 1-pixel speck at (40, 8); ink is 0 (white is 1) in one copy and 1 in the other.
        let (w, h) = (64u32, 16u32);
        let stride = 8usize;
        let mut white_is_1 = vec![0xFFu8; stride * 16];
        white_is_1[8 * stride + 5] &= !0x80;
        let mut ink_is_1: Vec<u8> = white_is_1.iter().map(|b| !b).collect();
        let s = despeckle_bits(&mut white_is_1, w, h, false, 300.0, opts(Level::Low)).unwrap();
        assert_eq!((s.dark_specks, s.pixels_changed), (1, 1));
        assert!(white_is_1.iter().all(|b| *b == 0xFF));
        let s = despeckle_bits(&mut ink_is_1, w, h, true, 300.0, opts(Level::Low)).unwrap();
        assert_eq!(s.dark_specks, 1);
        assert!(ink_is_1.iter().all(|b| *b == 0));
        // Wrong buffer length.
        assert!(despeckle_bits(&mut [0u8; 3], w, h, true, 300.0, opts(Level::Low)).is_err());
    }

    #[test]
    fn light_holes_in_solid_ink_are_filled_but_letter_counters_kept() {
        let (w, h) = (200, 100);
        // A solid block with a 2 × 2 pinhole, and an "o": 7 × 7, strokes 2 px, a 3 × 3 counter.
        let mut px = page(w, h, &[(20, 20, 40, 40), (120, 20, 7, 7)]);
        for (x, y) in [(30, 30), (31, 30), (30, 31), (31, 31)] {
            px[(y * w + x) as usize] = WHITE;
        }
        for y in 22..25 {
            for x in 122..125 {
                px[(y * w + x) as usize] = WHITE;
            }
        }
        let mut no_light = px.clone();
        despeckle_pixels(&mut no_light, w, h, 1, 300.0, opts(Level::Medium)).unwrap();
        assert!(!dark_at(&no_light, w, 30, 30), "holes are left alone unless asked");
        let s = despeckle_pixels(&mut px, w, h, 1, 300.0, Options { level: Level::Medium, light: true, median: false }).unwrap();
        assert_eq!(s.light_specks, 1, "{s:?}");
        assert!(dark_at(&px, w, 30, 30) && dark_at(&px, w, 31, 31), "pinhole not filled");
        assert!(!dark_at(&px, w, 123, 23), "counter filled");
        assert!(dark_at(&px, w, 120, 20), "the o was removed");
    }

    #[test]
    fn colour_specks_take_the_surrounding_paper_colour_including_their_halo() {
        let (w, h) = (100u32, 60u32);
        let paper = [250u8, 240, 220];
        let mut px: Vec<u8> = (0..w * h).flat_map(|_| paper).collect();
        let put = |px: &mut Vec<u8>, x: u32, y: u32, c: [u8; 3]| px[((y * w + x) * 3) as usize..][..3].copy_from_slice(&c);
        // Some text so the page has ink, then a dark speck with a gray halo.
        for y in 10..40 {
            for x in 10..14 {
                put(&mut px, x, y, [0, 0, 0]);
            }
        }
        put(&mut px, 70, 30, [10, 10, 10]);
        put(&mut px, 71, 30, [200, 190, 170]);
        let s = despeckle_pixels(&mut px, w, h, 3, 300.0, opts(Level::Low)).unwrap();
        assert_eq!(s.dark_specks, 1);
        assert_eq!(&px[((30 * w + 70) * 3) as usize..][..3], &paper);
        assert_eq!(&px[((30 * w + 71) * 3) as usize..][..3], &paper);
        assert_eq!(&px[((20 * w + 11) * 3) as usize..][..3], &[0, 0, 0]);
    }

    #[test]
    fn impulse_median_removes_single_pixels_and_keeps_thin_lines() {
        let (w, h) = (40u32, 20u32);
        let mut px = vec![200u8; (w * h) as usize];
        for x in 5..35 {
            px[(5 * w + x) as usize] = 20; // a one-pixel line
        }
        px[(12 * w + 20) as usize] = 30; // an impulse
        let s = despeckle_pixels(&mut px, w, h, 1, 300.0, Options { median: true, ..Default::default() }).unwrap();
        assert_eq!(s.impulses, 1);
        assert_eq!(px[(12 * w + 20) as usize], 200);
        assert!((5..35).all(|x| px[(5 * w + x) as usize] == 20));
    }

    #[test]
    fn photos_and_blank_pages_are_left_alone() {
        let (w, h) = (64u32, 64u32);
        let mut blank = vec![WHITE; (w * h) as usize];
        assert_eq!(despeckle_pixels(&mut blank, w, h, 1, 300.0, opts(Level::High)).unwrap(), Stats::default());
        // Mostly dark: an inverted scan with white specks is not treated as a document.
        let mut dark = vec![0u8; (w * h) as usize];
        dark[100] = WHITE;
        let before = dark.clone();
        despeckle_pixels(&mut dark, w, h, 1, 300.0, opts(Level::High)).unwrap();
        assert_eq!(dark, before);
    }

    #[test]
    fn empty_tiny_and_hostile_inputs_are_errors_or_no_ops() {
        assert_eq!(despeckle_pixels(&mut [], 0, 0, 1, 300.0, opts(Level::High)), Err(DespeckleError::Empty));
        assert_eq!(despeckle_pixels(&mut [0], 1, 0, 1, 300.0, opts(Level::High)), Err(DespeckleError::Empty));
        // A single pixel: it is all ink (mostly dark) or all paper, so nothing changes.
        for v in [0u8, 255] {
            let mut one = [v];
            despeckle_pixels(&mut one, 1, 1, 1, 300.0, Options { level: Level::High, light: true, median: true }).unwrap();
            assert_eq!(one, [v]);
            let mut bit = [v];
            despeckle_bits(&mut bit, 1, 1, true, 300.0, Options { level: Level::High, light: true, median: true }).unwrap();
        }
        // Huge dimensions are refused before anything is allocated.
        assert_eq!(despeckle_pixels(&mut [0; 4], u32::MAX, u32::MAX, 1, 300.0, opts(Level::Low)), Err(DespeckleError::TooLarge));
        assert_eq!(despeckle_bits(&mut [0; 4], 100_000, 100_000, true, 300.0, opts(Level::Low)), Err(DespeckleError::TooLarge));
        // Mismatched lengths and channel counts.
        assert!(despeckle_pixels(&mut [0; 5], 2, 2, 1, 300.0, opts(Level::Low)).is_err());
        assert!(despeckle_pixels(&mut [0; 8], 2, 2, 2, 300.0, opts(Level::Low)).is_err());
        assert!(despeckle_pixels(&mut [0; 4], 2, 2, 1, f64::NAN, opts(Level::Low)).is_err());
        // Off with a bad dpi is still a refusal (the caller passed nonsense).
        assert!(despeckle_pixels(&mut [0; 4], 2, 2, 1, -1.0, opts(Level::Off)).is_err());
    }

    #[test]
    fn work_is_capped_on_pathological_noise() {
        // A checkerboard has w·h/2 single-pixel runs; past MAX_RUNS the image is refused, unchanged.
        let (w, h) = (4096u32, 4000u32);
        let stride = (w / 8) as usize;
        let mut bits = vec![0u8; stride * h as usize];
        for (y, row) in bits.chunks_mut(stride).enumerate() {
            row.fill(if y % 2 == 0 { 0xAA } else { 0x55 });
        }
        let before = bits.clone();
        assert_eq!(despeckle_bits(&mut bits, w, h, true, 300.0, opts(Level::Low)), Err(DespeckleError::TooComplex));
        assert_eq!(bits, before);
    }

    #[test]
    fn components_join_diagonally_and_across_runs() {
        // A diagonal line of single pixels is one 20 px component (text-sized), not 20 specks.
        let (w, h) = (40u32, 40u32);
        let mut px = vec![WHITE; (w * h) as usize];
        for i in 5..25 {
            px[(i * w + i) as usize] = 0;
        }
        let before = px.clone();
        let s = despeckle_pixels(&mut px, w, h, 1, 300.0, opts(Level::High)).unwrap();
        assert_eq!(s.dark_specks, 0);
        assert_eq!(px, before);
    }
}
