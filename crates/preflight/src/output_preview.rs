//! Output Preview (Acrobat: Print Production ▸ Output Preview): how a page will look printed,
//! simulated on screen from its ink plates.
//!
//! The page is separated by the ink rasterizer of [`crate::coverage`] (overprint simulated or
//! not), keeping only the objects the **Show** filter selects ([`Show`]: all, DeviceCMYK, not
//! DeviceCMYK, spot colour, RGB, gray, ICC-based, images, solid colour, text, line art). The
//! **separations** that are switched on are composited back to sRGB: the process inks through a
//! [`Display`] — the **simulation profile** (a CMYK ICC profile: the output intent's or one the
//! caller chose) read through its `AToB` table by moxcms, or the conversion pages display CMYK
//! with ([`Conversion::display_cmyk`]) when there is none — and each spot ink multiplies the result by its preview colour.
//!
//! With a simulation profile the display follows Acrobat's three modes:
//! - neither box: relative colorimetric with black point compensation (the profile's paper is
//!   screen white, its darkest ink screen black);
//! - **Simulate Black Ink**: relative colorimetric without black point compensation (black ink
//!   shows as the dark gray it prints);
//! - **Simulate Paper Color** (implies black ink): absolute colorimetric, scaling by the
//!   profile's media white point (the paper shows its own tint).
//!
//! **Total area coverage**: the sum of every ink of each cell (shown or not), its maximum and
//! where it is, the share of the page over a limit, and optionally those cells painted in a
//! warning colour. Every ink's maximum, inked area and mean coverage are reported too.
//!
//! What the ink rasterizer can't see (patterns, shadings, DCT/JPX/JBIG2/CCITT images,
//! annotations) is missing from the preview and counted as unmeasured; geometry is at the grid's
//! resolution (cell centres decide coverage, clips are bounding boxes, text is string boxes).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, Object};

use crate::coverage::{Conversion, IMAGE, InkInfo, PROCESS_INKS, Raster, TEXT, VECTOR, rasterize_shown};

/// Hidden inks remembered at most, and the longest ink name.
pub const MAX_HIDDEN_INKS: usize = 256;
pub const MAX_INK_NAME: usize = 127;
/// The default total area coverage limit (percent), and the range accepted.
pub const DEFAULT_LIMIT: f64 = crate::coverage::DEFAULT_LIMIT;
pub const MIN_LIMIT: f64 = crate::coverage::MIN_LIMIT;
pub const MAX_LIMIT: f64 = crate::coverage::MAX_LIMIT;
/// The default warning colour for areas over the limit.
pub const DEFAULT_WARNING_COLOUR: [u8; 3] = [0, 255, 0];
/// Distinct ink mixtures whose display colour is remembered while compositing.
const MAX_CACHE: usize = 1 << 16;
const MAX_PROFILE: usize = 32 << 20;
/// D50, the profile connection space's white.
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

/// Colour space family bits, as the Show filter sees them.
pub mod family {
    /// One component of gray: DeviceGray, CalGray, ICCBased with 1 component.
    pub const GRAY: u16 = 1;
    /// RGB: DeviceRGB, CalRGB, ICCBased with 3 components.
    pub const RGB: u16 = 2;
    /// Four components: DeviceCMYK, ICCBased with 4 components.
    pub const CMYK: u16 = 4;
    /// A device space itself (`DeviceGray`, `DeviceRGB`, `DeviceCMYK`).
    pub const DEVICE: u16 = 8;
    /// CalGray or CalRGB.
    pub const CAL: u16 = 16;
    pub const ICC: u16 = 32;
    pub const LAB: u16 = 64;
    /// A Separation or DeviceN space naming at least one spot colorant (not a process ink,
    /// `All` or `None`).
    pub const SPOT: u16 = 128;
    pub const INDEXED: u16 = 256;
    pub const SEPARATION: u16 = 512;
    pub const DEVICE_N: u16 = 1024;
    pub const PATTERN: u16 = 2048;

    /// The family of a device colour set with `g`, `rg` or `k`.
    pub fn device(gray: bool, rgb: bool, cmyk: bool) -> u16 {
        match (gray, rgb, cmyk) {
            (true, _, _) => GRAY | DEVICE,
            (_, true, _) => RGB | DEVICE,
            (_, _, true) => CMYK | DEVICE,
            _ => 0,
        }
    }
}

/// Output Preview's Show filter: which objects the preview shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Show {
    #[default]
    All,
    /// Objects in DeviceCMYK (Indexed over DeviceCMYK included).
    DeviceCmyk,
    /// Everything not in DeviceCMYK.
    NotDeviceCmyk,
    /// Separation and DeviceN colours with a spot colorant.
    Spot,
    /// DeviceRGB, CalRGB and three-component ICC colours.
    Rgb,
    /// DeviceGray, CalGray and one-component ICC colours.
    Gray,
    /// ICCBased colours of any kind.
    IccBased,
    /// Images (and stencil masks).
    Images,
    /// Everything that isn't an image: text and line art.
    SolidColor,
    Text,
    /// Paths: fills and strokes.
    LineArt,
}

impl Show {
    pub const ALL: [Show; 11] = [
        Show::All,
        Show::DeviceCmyk,
        Show::NotDeviceCmyk,
        Show::Spot,
        Show::Rgb,
        Show::Gray,
        Show::IccBased,
        Show::Images,
        Show::SolidColor,
        Show::Text,
        Show::LineArt,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Show::All => "all",
            Show::DeviceCmyk => "device_cmyk",
            Show::NotDeviceCmyk => "not_device_cmyk",
            Show::Spot => "spot",
            Show::Rgb => "rgb",
            Show::Gray => "gray",
            Show::IccBased => "icc_based",
            Show::Images => "images",
            Show::SolidColor => "solid_color",
            Show::Text => "text",
            Show::LineArt => "line_art",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Show::All => "All",
            Show::DeviceCmyk => "DeviceCMYK",
            Show::NotDeviceCmyk => "Not DeviceCMYK",
            Show::Spot => "Spot Color",
            Show::Rgb => "RGB",
            Show::Gray => "Gray",
            Show::IccBased => "ICC-Based",
            Show::Images => "Images",
            Show::SolidColor => "Solid Color",
            Show::Text => "Text",
            Show::LineArt => "Line Art",
        }
    }

    pub fn from_id(id: &str) -> Option<Show> {
        let id = id.trim();
        Show::ALL.into_iter().find(|s| s.id().eq_ignore_ascii_case(id) || s.label().eq_ignore_ascii_case(id))
    }

    /// Whether the filter leaves anything out.
    pub fn filters(self) -> bool {
        self != Show::All
    }

    /// Whether an object of colour space `family` ([`family`] bits) and `kind`
    /// ([`TEXT`], [`VECTOR`], [`IMAGE`]) is shown.
    pub fn shows(self, family: u16, kind: u8) -> bool {
        let device_cmyk = family & family::CMYK != 0 && family & family::DEVICE != 0;
        match self {
            Show::All => true,
            Show::DeviceCmyk => device_cmyk,
            Show::NotDeviceCmyk => !device_cmyk,
            Show::Spot => family & family::SPOT != 0,
            Show::Rgb => family & family::RGB != 0,
            Show::Gray => family & family::GRAY != 0,
            Show::IccBased => family & family::ICC != 0,
            Show::Images => kind & IMAGE != 0,
            Show::SolidColor => kind & (TEXT | VECTOR) != 0,
            Show::Text => kind & TEXT != 0,
            Show::LineArt => kind & VECTOR != 0,
        }
    }
}

/// The [`family`] bits of colour space `o` (a name, possibly of `resources`' `ColorSpace`, or
/// an array; inline-image abbreviations included).
pub fn space_family(doc: &Document, o: &Object, resources: &Dict, depth: usize) -> u16 {
    use family::*;
    if depth > 4 {
        return 0;
    }
    let o = doc.resolve(o);
    let named = |n: &[u8]| -> Option<u16> {
        Some(match n {
            b"DeviceGray" | b"G" => GRAY | DEVICE,
            b"CalGray" => GRAY | CAL,
            b"DeviceRGB" | b"RGB" => RGB | DEVICE,
            b"CalRGB" => RGB | CAL,
            b"DeviceCMYK" | b"CMYK" => CMYK | DEVICE,
            b"Lab" => LAB,
            b"Pattern" => PATTERN,
            _ => return None,
        })
    };
    let spot = |name: &[u8]| !matches!(name, b"Cyan" | b"Magenta" | b"Yellow" | b"Black" | b"All" | b"None");
    match o.as_ref() {
        Object::Name(n) => named(n).unwrap_or_else(|| {
            resources
                .get(b"ColorSpace")
                .map(|d| doc.resolve(d))
                .and_then(|d| d.as_dict().and_then(|d| d.get(n).cloned()))
                .map_or(0, |v| space_family(doc, &v, &Dict::new(), depth + 1))
        }),
        Object::Array(a) => {
            let first = a.first().map(|f| doc.resolve(f)).and_then(|f| f.as_name().map(<[u8]>::to_vec)).unwrap_or_default();
            let arg = |i: usize| a.get(i).map(|o| doc.resolve(o));
            match first.as_slice() {
                b"ICCBased" => {
                    let n = arg(1).and_then(|s| match s.as_ref() {
                        Object::Stream(s) => s.dict.get(b"N").and_then(|n| doc.resolve(n).as_int()),
                        _ => None,
                    });
                    ICC | match n {
                        Some(1) => GRAY,
                        Some(3) => RGB,
                        Some(4) => CMYK,
                        _ => 0,
                    }
                }
                b"Separation" => {
                    let name = arg(1).and_then(|n| n.as_name().map(<[u8]>::to_vec)).unwrap_or_default();
                    SEPARATION | if spot(&name) { SPOT } else { 0 }
                }
                b"DeviceN" => {
                    let any_spot = arg(1)
                        .as_deref()
                        .and_then(Object::as_array)
                        .is_some_and(|names| names.iter().take(64).any(|n| doc.resolve(n).as_name().is_some_and(spot)));
                    DEVICE_N | if any_spot { SPOT } else { 0 }
                }
                b"Indexed" | b"I" => INDEXED | a.get(1).map_or(0, |base| space_family(doc, base, resources, depth + 1)),
                other => named(other).unwrap_or(0),
            }
        }
        _ => 0,
    }
}

/// What the display simulates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Simulate {
    /// Simulate Paper Color (absolute colorimetric; implies black ink).
    pub paper: bool,
    /// Simulate Black Ink (no black point compensation).
    pub black: bool,
}

/// How process inks are shown on screen: through a CMYK simulation profile, or the renderer's
/// own CMYK conversion.
#[derive(Clone)]
pub struct Display {
    transform: Option<Arc<moxcms::TransformF64Executor>>,
    /// The destination's linear RGB → D50 XYZ matrix (rows), and its tone curve's gamma.
    to_xyz: [[f64; 3]; 3],
    /// What displays the inks, for reports.
    pub description: String,
    /// The profile's media white point (D50-relative XYZ; D50 itself without a profile).
    pub media_white: [f64; 3],
    /// The darkest colour the profile prints (relative XYZ: 100 % of every process ink).
    pub black_point: [f64; 3],
}

impl std::fmt::Debug for Display {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Display")
            .field("description", &self.description)
            .field("profile", &self.transform.is_some())
            .field("media_white", &self.media_white)
            .field("black_point", &self.black_point)
            .finish()
    }
}

impl Display {
    /// No simulation profile: process inks display as pages render them
    /// ([`Conversion::display_cmyk`], which follows the engine's colour settings); paper and
    /// black ink are not simulated.
    pub fn renderer() -> Display {
        Display {
            transform: None,
            to_xyz: SRGB_TO_XYZ,
            description: "the display conversion pages render with (no simulation profile)".into(),
            media_white: D50,
            black_point: [0.0; 3],
        }
    }

    /// Display through the CMYK ICC profile `bytes` (its `AToB` table, relative
    /// colorimetric), described as `label`.
    pub fn from_profile(bytes: &[u8], label: &str) -> Result<Display, String> {
        if bytes.len() < 132 || bytes.len() > MAX_PROFILE {
            return Err(format!("an ICC profile is 132 bytes to {} MB", MAX_PROFILE >> 20));
        }
        type Made = (Arc<moxcms::TransformF64Executor>, Option<moxcms::Xyzd>, [[f64; 3]; 3]);
        let made = std::panic::catch_unwind(|| -> Result<Made, String> {
            let profile = moxcms::ColorProfile::new_from_slice(bytes).map_err(|e| format!("the profile could not be read ({e:?})"))?;
            if profile.color_space != moxcms::DataColorSpace::Cmyk {
                return Err("the simulation profile is not a CMYK profile".into());
            }
            // Into a wide-gamut linear-light space (ProPhoto RGB's primaries, D50), so the
            // XYZ comes back unclipped for every printable colour.
            let wide = moxcms::ColorProfile::new_pro_photo_rgb();
            // Relative colorimetric, else the table the profile has (a display-only profile may
            // hold just the perceptual one).
            let mut last = None;
            let mut transform = None;
            for intent in [moxcms::RenderingIntent::RelativeColorimetric, moxcms::RenderingIntent::Perceptual] {
                let options = moxcms::TransformOptions { rendering_intent: intent, ..Default::default() };
                match profile.create_transform_f64(moxcms::Layout::Rgba, &wide, moxcms::Layout::Rgb, options) {
                    Ok(t) => {
                        transform = Some(t);
                        break;
                    }
                    Err(e) => last = Some(e),
                }
            }
            let transform = transform.ok_or_else(|| format!("the profile has no table to show CMYK with ({last:?})"))?;
            let (r, g, b) = (wide.red_colorant, wide.green_colorant, wide.blue_colorant);
            let to_xyz = [[r.x, g.x, b.x], [r.y, g.y, b.y], [r.z, g.z, b.z]];
            Ok((transform, profile.media_white_point, to_xyz))
        })
        .map_err(|_| "the colour engine failed on the profile".to_string())??;
        let (transform, white, to_xyz) = made;
        let mut display = Display { transform: Some(transform), to_xyz, description: label.to_string(), media_white: D50, black_point: [0.0; 3] };
        if let Some(w) = white {
            let w = [w.x, w.y, w.z];
            if w.iter().all(|v| v.is_finite() && *v > 0.05 && *v < 2.0) {
                display.media_white = w;
            }
        }
        let black = display.xyz([1.0; 4]).ok_or("the profile could not convert its black")?;
        display.black_point = black;
        Ok(display)
    }

    /// Whether a simulation profile displays the inks (paper and black ink can be simulated).
    pub fn has_profile(&self) -> bool {
        self.transform.is_some()
    }

    /// Relative D50 XYZ of `cmyk` (0–1) through the profile.
    fn xyz(&self, cmyk: [f64; 4]) -> Option<[f64; 3]> {
        let t = self.transform.as_ref()?;
        let mut rgb = [0.0f64; 3];
        let src = cmyk.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.transform(&src, &mut rgb))).ok()?.ok()?;
        // ProPhoto RGB's tone curve is gamma 1.8.
        let lin = rgb.map(|v| if v.is_finite() { v.max(0.0).powf(1.8) } else { 0.0 });
        let xyz = mul(&self.to_xyz, lin);
        xyz.iter().all(|v| v.is_finite()).then_some(xyz)
    }

    /// The sRGB colour (0–1) of process inks `cmyk` (0–1), simulating as `sim` says.
    pub fn srgb(&self, cmyk: [f64; 4], sim: Simulate, conversion: &dyn Conversion) -> [f64; 3] {
        if let Some(xyz) = self.xyz(cmyk) {
            let xyz = if sim.paper {
                [0, 1, 2].map(|i| xyz[i] * self.media_white[i] / D50[i])
            } else if sim.black {
                xyz
            } else {
                // Black point compensation: the profile's black is screen black.
                let yb = self.black_point[1].clamp(0.0, 0.99);
                [0, 1, 2].map(|i| (xyz[i] - yb * D50[i]) / (1.0 - yb))
            };
            return xyz_to_srgb(xyz);
        }
        let q = cmyk.map(|v| (if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 } * 255.0).round() as u8);
        match conversion.display_cmyk(&q).as_deref() {
            Some([r, g, b]) => [*r, *g, *b].map(|v| f64::from(v) / 255.0),
            _ => {
                let [c, m, y, k] = cmyk.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
                [(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)]
            }
        }
    }
}

fn lab_to_xyz([l, a, b]: [f64; 3]) -> [f64; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let g = |t: f64| if t > 6.0 / 29.0 { t * t * t } else { 108.0 / 841.0 * (t - 4.0 / 29.0) };
    [g(fx) * D50[0], g(fy) * D50[1], g(fz) * D50[2]]
}

fn xyz_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (x, y, z) = (f(xyz[0] / D50[0]), f(xyz[1] / D50[1]), f(xyz[2] / D50[2]));
    [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
}

/// The sRGB primaries adapted to D50 (Bradford): linear sRGB → XYZ, and back.
const SRGB_TO_XYZ: [[f64; 3]; 3] = [[0.4361, 0.3851, 0.1431], [0.2225, 0.7169, 0.0606], [0.0139, 0.0971, 0.7141]];
const XYZ_TO_SRGB: [[f64; 3]; 3] = [[3.1339, -1.6169, -0.4906], [-0.9788, 1.9161, 0.0335], [0.0719, -0.2290, 1.4052]];

fn mul(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

fn xyz_to_srgb(xyz: [f64; 3]) -> [f64; 3] {
    mul(&XYZ_TO_SRGB, xyz).map(|v| {
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    })
}

/// What Output Preview shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub show: Show,
    /// Simulate Overprinting.
    pub overprint: bool,
    pub simulate: Simulate,
    /// Separations switched off (ink names).
    pub hidden_inks: BTreeSet<String>,
    /// The total area coverage limit (percent) when its warning is on.
    pub coverage_warning: Option<f64>,
    /// The colour areas over the limit are painted with.
    pub warning_colour: [u8; 3],
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            show: Show::All,
            overprint: true,
            simulate: Simulate::default(),
            hidden_inks: BTreeSet::new(),
            coverage_warning: None,
            warning_colour: DEFAULT_WARNING_COLOUR,
        }
    }
}

impl Settings {
    /// Check the settings: ink names 1–127 bytes and at most [`MAX_HIDDEN_INKS`], the limit
    /// within [`MIN_LIMIT`]–[`MAX_LIMIT`], black ink simulated whenever paper is.
    pub fn validate(&self) -> Result<(), String> {
        if self.hidden_inks.len() > MAX_HIDDEN_INKS {
            return Err(format!("at most {MAX_HIDDEN_INKS} inks can be hidden"));
        }
        if let Some(n) = self.hidden_inks.iter().find(|n| n.is_empty() || n.len() > MAX_INK_NAME) {
            return Err(format!("ink names are 1 to {MAX_INK_NAME} bytes ({n:?})"));
        }
        if let Some(limit) = self.coverage_warning
            && !(limit.is_finite() && (MIN_LIMIT..=MAX_LIMIT).contains(&limit))
        {
            return Err(format!("the total area coverage limit must be between {MIN_LIMIT} and {MAX_LIMIT} percent"));
        }
        if self.simulate.paper && !self.simulate.black {
            return Err("simulating paper colour also simulates black ink".into());
        }
        Ok(())
    }
}

/// One ink of the previewed page.
#[derive(Clone, Debug, PartialEq)]
pub struct InkStat {
    pub info: InkInfo,
    /// Whether its separation is switched on.
    pub shown: bool,
    /// Its highest percentage, the share of the page it inks (percent of cells) and its mean
    /// coverage over the page (percent).
    pub max: f64,
    pub inked_percent: f64,
    pub mean: f64,
}

/// Total area coverage of the previewed page.
#[derive(Clone, Debug, PartialEq)]
pub struct Coverage {
    /// The highest total ink (percent) and the user-space centre of a cell where it is.
    pub max: f64,
    pub max_at: Option<[f64; 2]>,
    /// The limit checked (the warning's, else [`DEFAULT_LIMIT`]) and whether the warning is on.
    pub limit: f64,
    pub warning: bool,
    /// Cells over the limit, their share of the page and their area in points².
    pub over_cells: usize,
    pub over_percent: f64,
    pub over_area: f64,
    /// Pixels painted in the warning colour.
    pub warned_pixels: usize,
}

/// A rendered Output Preview.
#[derive(Clone, Debug)]
pub struct Preview {
    /// 8-bit RGB, `width × height`, top row first (the crop box, before `/Rotate`).
    pub rgb: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// The page's inks with their statistics, process inks first.
    pub inks: Vec<InkStat>,
    pub coverage: Coverage,
    /// What the display simulated (paper and black ink need a simulation profile).
    pub simulated: Simulate,
    /// What displayed the inks.
    pub display: String,
    /// The separated page (shown objects only), for ink readouts at a point.
    pub raster: Raster,
}

fn round(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Render Output Preview for 0-based `page`, separated at `dpi` (lowered on large pages or
/// with many inks) with `conversion` (RGB and Lab to process inks, Ink Manager settings) and
/// shown through `display`.
pub fn render(doc: &Document, page: usize, dpi: f64, settings: &Settings, conversion: &dyn Conversion, display: &Display) -> Result<Preview, String> {
    settings.validate()?;
    let raster = rasterize_shown(doc, page, dpi, settings.overprint, conversion, settings.show)?;
    let (w, h) = (raster.width, raster.height);
    let cells = w.checked_mul(h).ok_or("the preview is too large")?;
    let simulated = if display.has_profile() { settings.simulate } else { Simulate::default() };
    let shown: Vec<bool> = raster.inks.iter().map(|i| !settings.hidden_inks.contains(&i.name)).collect();
    let process_shown: [bool; 4] = [0, 1, 2, 3].map(|i| shown.get(i).copied().unwrap_or(false));
    // Spot inks multiply the paper by their preview colour: a CMYK alternate through the
    // display (relative to the paper), else the ink's own preview.
    let relative = Simulate { paper: false, black: simulated.black };
    let spots: Vec<(usize, [f64; 3])> = raster
        .inks
        .iter()
        .enumerate()
        .skip(PROCESS_INKS.len())
        .filter(|(i, _)| shown.get(*i).copied().unwrap_or(false))
        .map(|(i, info)| {
            let preview = match info.alternate_value.as_deref() {
                Some([c, m, y, k]) => display.srgb([*c, *m, *y, *k], relative, conversion),
                _ => info.preview,
            };
            (i, preview.map(|v| v.clamp(0.0, 1.0)))
        })
        .collect();
    let mut rgb: Vec<u8> = Vec::new();
    rgb.try_reserve_exact(cells.saturating_mul(3)).map_err(|_| "not enough memory for the output preview".to_string())?;
    let totals = raster.totals();
    let limit = settings.coverage_warning.unwrap_or(DEFAULT_LIMIT);
    let mut cache: HashMap<[u16; 4], [f64; 3]> = HashMap::new();
    let mut warned = 0usize;
    for row in (0..h).rev() {
        for col in 0..w {
            let at = row.saturating_mul(w).saturating_add(col);
            let total = totals.get(at).copied().map_or(0.0, f64::from);
            if settings.coverage_warning.is_some() && total > limit {
                rgb.extend(settings.warning_colour);
                warned += 1;
                continue;
            }
            let quad: [u16; 4] =
                [0, 1, 2, 3].map(|i| if process_shown[i] { (raster.value(i, col, row) * 100.0).round().clamp(0.0, 10_000.0) as u16 } else { 0 });
            let mut c = match cache.get(&quad) {
                Some(c) => *c,
                None => {
                    let c = display.srgb(quad.map(|v| f64::from(v) / 10_000.0), simulated, conversion);
                    if cache.len() >= MAX_CACHE {
                        cache.clear();
                    }
                    cache.insert(quad, c);
                    c
                }
            };
            for (ink, preview) in &spots {
                let t = (raster.value(*ink, col, row) / 100.0).clamp(0.0, 1.0);
                if t > 0.0 {
                    for (v, p) in c.iter_mut().zip(preview) {
                        *v *= 1.0 - t * (1.0 - p);
                    }
                }
            }
            rgb.extend(c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8));
        }
    }
    // Statistics.
    let all = cells.max(1) as f64;
    let inks = raster
        .inks
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let (mut max, mut inked, mut sum) = (0.0f64, 0usize, 0.0f64);
            for row in 0..h {
                for col in 0..w {
                    let v = raster.value(i, col, row);
                    max = max.max(v);
                    sum += v;
                    if v > 0.0 {
                        inked += 1;
                    }
                }
            }
            InkStat {
                info: info.clone(),
                shown: shown.get(i).copied().unwrap_or(true),
                max: round(max),
                inked_percent: round(inked as f64 / all * 100.0),
                mean: round(sum / all),
            }
        })
        .collect();
    let (mut max, mut max_cell, mut over) = (0.0f64, None, 0usize);
    for (at, t) in totals.iter().enumerate() {
        let t = f64::from(*t);
        if t > max {
            max = t;
            max_cell = Some(at);
        }
        if t > limit {
            over += 1;
        }
    }
    let cell = raster.cell();
    let coverage = Coverage {
        max: round(max),
        max_at: max_cell.map(|at| raster.centre(at % w.max(1), at / w.max(1))),
        limit,
        warning: settings.coverage_warning.is_some(),
        over_cells: over,
        over_percent: round(over as f64 / all * 100.0),
        over_area: round(over as f64 * cell * cell),
        warned_pixels: warned,
    };
    Ok(Preview { rgb, width: w, height: h, inks, coverage, simulated, display: display.description.clone(), raster })
}

// ---- a synthetic press profile --------------------------------------------------------------

/// A small CMYK printer profile written in code (ICC v2, Lab PCS, 16-bit `AToB0`/`AToB1` and
/// `BToA0`/`BToA1` tables, no asset): a simple subtractive press whose paper has the D50 XYZ
/// colour `paper` (its media white point) and whose 100 % C+M+Y+K prints at lightness `black_l`
/// (L*, relative to the paper). For tests and examples of simulation profiles.
pub fn synthetic_cmyk_profile(paper: [f64; 3], black_l: f64) -> Vec<u8> {
    let black_l = if black_l.is_finite() { black_l.clamp(0.0, 90.0) } else { 10.0 };
    // The darkest linear reflectance (relative to the paper).
    let floor = lab_to_xyz([black_l, 0.0, 0.0])[1].clamp(0.0, 0.9);
    let to_lab = |cmyk: [f64; 4]| -> [f64; 3] {
        let [c, m, y, k] = cmyk;
        let v = [(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)].map(|v| floor + (1.0 - floor) * v);
        xyz_to_lab(mul(&SRGB_TO_XYZ, v))
    };
    let from_lab = |lab: [f64; 3]| -> [f64; 4] {
        let lin = mul(&XYZ_TO_SRGB, lab_to_xyz(lab));
        let v = lin.map(|l| ((l - floor) / (1.0 - floor)).clamp(0.0, 1.0));
        let cmy = v.map(|v| 1.0 - v);
        let k = cmy[0].min(cmy[1]).min(cmy[2]);
        if k >= 1.0 - 1e-9 {
            return [0.0, 0.0, 0.0, 1.0];
        }
        [(cmy[0] - k) / (1.0 - k), (cmy[1] - k) / (1.0 - k), (cmy[2] - k) / (1.0 - k), k]
    };
    let lab16 = |lab: [f64; 3]| -> [u16; 3] {
        [
            (lab[0].clamp(0.0, 100.0) / 100.0 * 65280.0).round() as u16,
            ((lab[1].clamp(-128.0, 127.99) + 128.0) * 256.0).round() as u16,
            ((lab[2].clamp(-128.0, 127.99) + 128.0) * 256.0).round() as u16,
        ]
    };
    let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let mft2 = |inputs: usize, outputs: usize, grid: usize, clut: &[u16]| -> Vec<u8> {
        let mut t = b"mft2\0\0\0\0".to_vec();
        t.extend([inputs as u8, outputs as u8, grid as u8, 0]);
        for v in identity {
            t.extend(((v * 65536.0) as i32).to_be_bytes());
        }
        t.extend(2u16.to_be_bytes());
        t.extend(2u16.to_be_bytes());
        for _ in 0..inputs {
            t.extend([0u8, 0, 0xFF, 0xFF]);
        }
        for v in clut {
            t.extend(v.to_be_bytes());
        }
        for _ in 0..outputs {
            t.extend([0u8, 0, 0xFF, 0xFF]);
        }
        t
    };
    // AToB: CMYK → Lab on a 5-point grid, the first input varying slowest.
    let g = 5usize;
    let step = |i: usize| i as f64 / (g - 1) as f64;
    let mut a2b = Vec::new();
    for c in 0..g {
        for m in 0..g {
            for y in 0..g {
                for k in 0..g {
                    a2b.extend(lab16(to_lab([step(c), step(m), step(y), step(k)])));
                }
            }
        }
    }
    // BToA: Lab (legacy 16-bit encoding) → CMYK on a 9-point grid.
    let gb = 9usize;
    let mut b2a = Vec::new();
    for l in 0..gb {
        for a in 0..gb {
            for b in 0..gb {
                let s = |i: usize| i as f64 / (gb - 1) as f64 * 65535.0;
                let lab = [s(l) / 65280.0 * 100.0, s(a) / 256.0 - 128.0, s(b) / 256.0 - 128.0];
                b2a.extend(from_lab(lab).map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16));
            }
        }
    }
    let a2b = mft2(4, 3, g, &a2b);
    let b2a = mft2(3, 4, gb, &b2a);
    let s15 = |v: f64| ((v * 65536.0).round() as i32).to_be_bytes();
    let mut wtpt = b"XYZ \0\0\0\0".to_vec();
    for v in paper {
        wtpt.extend(s15(if v.is_finite() { v.clamp(0.1, 1.5) } else { 1.0 }));
    }
    let mut desc = b"desc\0\0\0\0".to_vec();
    let text = b"PdfCraft synthetic press\0";
    desc.extend((text.len() as u32).to_be_bytes());
    desc.extend(text);
    desc.extend([0u8; 4 + 4 + 2 + 1 + 67]);
    let mut cprt = b"text\0\0\0\0".to_vec();
    cprt.extend(b"No copyright, use freely\0");
    let tags: Vec<(&[u8; 4], &[u8])> =
        vec![(b"desc", &desc), (b"cprt", &cprt), (b"wtpt", &wtpt), (b"A2B0", &a2b), (b"A2B1", &a2b), (b"B2A0", &b2a), (b"B2A1", &b2a)];
    let mut offset = 128 + 4 + tags.len() * 12;
    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut data = Vec::new();
    for (sig, body) in &tags {
        table.extend(*sig);
        table.extend((offset as u32).to_be_bytes());
        table.extend((body.len() as u32).to_be_bytes());
        data.extend(*body);
        let pad = (4 - body.len() % 4) % 4;
        data.extend(std::iter::repeat_n(0u8, pad));
        offset += body.len() + pad;
    }
    let size = 128 + table.len() + data.len();
    let mut out = Vec::with_capacity(size);
    out.extend((size as u32).to_be_bytes());
    out.extend(b"\0\0\0\0");
    out.extend([2, 0x10, 0, 0]);
    out.extend(b"prtr");
    out.extend(b"CMYK");
    out.extend(b"Lab ");
    out.extend([0u8; 12]);
    out.extend(b"acsp");
    out.extend([0u8; 4 + 4 + 4 + 4 + 8]);
    out.extend(1u32.to_be_bytes());
    for v in D50 {
        out.extend(s15(v));
    }
    out.resize(128, 0);
    out.extend(table);
    out.extend(data);
    out
}

#[cfg(test)]
mod tests;
