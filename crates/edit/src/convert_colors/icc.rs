//! Destination ICC profiles (ICC.1:2001-04 / ICC.1:2010) and the colour arithmetic of the
//! profile connection space. A destination profile is read for its device colour space, its
//! description and a transform from the PCS: the `BToA` table of the rendering intent (lut8 or
//! lut16, `mft1`/`mft2`, Lab or XYZ PCS), else a gray TRC or an RGB matrix/TRC inverted. A
//! profile with none of these (v4 `mBA` tables only) still tags the converted colours, but the
//! numbers come from the built-in conversion (reported as approximate). Every offset and size
//! read from the profile is checked; nothing here panics on a hostile profile.

/// Profiles larger than this are refused.
pub const MAX_PROFILE: usize = 32 << 20;
/// Lookup tables: grid points per input and table entries at most.
const MAX_GRID: usize = 255;
const MAX_TABLE: usize = 4096;
const MAX_CLUT: usize = 4 << 20;

/// D50, the PCS illuminant.
pub(super) const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

/// The device colour family of a profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Gray,
    Rgb,
    Cmyk,
}

impl Family {
    pub fn components(self) -> usize {
        match self {
            Family::Gray => 1,
            Family::Rgb => 3,
            Family::Cmyk => 4,
        }
    }

    pub(super) fn device_name(self) -> &'static str {
        match self {
            Family::Gray => "DeviceGray",
            Family::Rgb => "DeviceRGB",
            Family::Cmyk => "DeviceCMYK",
        }
    }
}

#[derive(Clone, Debug)]
enum Curve {
    Gamma(f64),
    /// Samples of the curve over 0–1.
    Table(Vec<f64>),
}

impl Curve {
    /// The input giving `y` (the curve is treated as monotonic).
    fn invert(&self, y: f64) -> f64 {
        let y = y.clamp(0.0, 1.0);
        match self {
            Curve::Gamma(g) if *g > 1e-6 => y.powf(1.0 / g),
            Curve::Gamma(_) => y,
            Curve::Table(t) => {
                let n = t.len();
                if n < 2 {
                    return y;
                }
                let rising = t.last().copied().unwrap_or(1.0) >= t.first().copied().unwrap_or(0.0);
                // Binary search for the segment holding y.
                let (mut lo, mut hi) = (0usize, n - 1);
                while hi - lo > 1 {
                    let mid = (lo + hi) / 2;
                    let v = t.get(mid).copied().unwrap_or(0.0);
                    if (v <= y) == rising {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                let (a, b) = (t.get(lo).copied().unwrap_or(0.0), t.get(hi).copied().unwrap_or(1.0));
                let f = if (b - a).abs() < 1e-12 { 0.0 } else { ((y - a) / (b - a)).clamp(0.0, 1.0) };
                ((lo as f64 + f) / (n - 1) as f64).clamp(0.0, 1.0)
            }
        }
    }
}

/// Linear interpolation in a table of samples over 0–1.
fn table(t: &[f64], x: f64) -> f64 {
    let n = t.len();
    if n == 0 {
        return x;
    }
    if n == 1 {
        return t.first().copied().unwrap_or(x);
    }
    let p = x.clamp(0.0, 1.0) * (n - 1) as f64;
    let i = (p.floor() as usize).min(n - 2);
    let f = p - i as f64;
    let (a, b) = (t.get(i).copied().unwrap_or(0.0), t.get(i + 1).copied().unwrap_or(0.0));
    a + f * (b - a)
}

#[derive(Clone, Debug)]
struct Lut {
    /// Applied to XYZ input before the input tables.
    matrix: Option<[f64; 9]>,
    inputs: Vec<Vec<f64>>,
    grid: usize,
    outputs: usize,
    clut: Vec<f64>,
    out_tables: Vec<Vec<f64>>,
    /// 16-bit legacy Lab encoding (mft2) rather than 8-bit (mft1).
    wide: bool,
}

#[derive(Clone, Debug)]
enum Transform {
    Lut(Lut),
    /// XYZ → linear RGB (the inverted colorant matrix), then the inverted TRCs.
    Matrix {
        inverse: [f64; 9],
        curves: [Curve; 3],
    },
    Gray(Curve),
}

/// A destination profile, read.
#[derive(Clone, Debug)]
pub struct Profile {
    pub family: Family,
    pub description: String,
    lab_pcs: bool,
    transform: Option<Transform>,
}

fn u32_at(p: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(p.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u16_at(p: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(p.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn s15f16(p: &[u8], at: usize) -> Option<f64> {
    Some(f64::from(u32_at(p, at)? as i32) / 65536.0)
}

impl Profile {
    /// Read `bytes` for the rendering `intent` (0 perceptual, 1 relative colorimetric,
    /// 2 saturation, 3 absolute colorimetric).
    pub fn parse(bytes: &[u8], intent: u8) -> Result<Profile, String> {
        if bytes.len() < 132 || bytes.len() > MAX_PROFILE {
            return Err(format!("an ICC profile is 132 bytes to {} MB", MAX_PROFILE >> 20));
        }
        let size = u32_at(bytes, 0).unwrap_or(0) as usize;
        if bytes.get(36..40) != Some(b"acsp".as_slice()) || size < 132 || size > bytes.len() {
            return Err("not an ICC profile (no acsp signature or a bad size)".into());
        }
        let p = bytes.get(..size).unwrap_or(bytes);
        let class = p.get(12..16).unwrap_or_default();
        if !matches!(class, b"prtr" | b"mntr" | b"scnr" | b"spac") {
            return Err(format!(
                "profile class {:?} can't be a destination (output, display, input or colour space profiles)",
                String::from_utf8_lossy(class)
            ));
        }
        let family = match p.get(16..20).unwrap_or_default() {
            b"GRAY" => Family::Gray,
            b"RGB " => Family::Rgb,
            b"CMYK" => Family::Cmyk,
            other => return Err(format!("profile colour space {:?} is not gray, RGB or CMYK", String::from_utf8_lossy(other))),
        };
        let lab_pcs = match p.get(20..24).unwrap_or_default() {
            b"Lab " => true,
            b"XYZ " => false,
            _ => return Err("profile connection space is neither Lab nor XYZ".into()),
        };
        let count = u32_at(p, 128).unwrap_or(0) as usize;
        if count > 1024 {
            return Err("profile has too many tags".into());
        }
        let mut tags: Vec<([u8; 4], &[u8])> = Vec::new();
        for i in 0..count {
            let e = 132 + i * 12;
            let (Some(sig), Some(off), Some(len)) = (p.get(e..e + 4), u32_at(p, e + 4), u32_at(p, e + 8)) else {
                return Err("profile tag table is truncated".into());
            };
            let (off, len) = (off as usize, len as usize);
            let Some(body) = off.checked_add(len).and_then(|end| p.get(off..end)) else { continue };
            let mut s = [0u8; 4];
            s.copy_from_slice(sig);
            tags.push((s, body));
        }
        let tag = |sig: &[u8; 4]| tags.iter().find(|(s, _)| s == sig).map(|(_, b)| *b);
        let description = tag(b"desc").and_then(description).unwrap_or_default();
        let wanted: [&[u8; 4]; 3] = match intent {
            0 => [b"B2A0", b"B2A1", b"B2A2"],
            2 => [b"B2A2", b"B2A0", b"B2A1"],
            _ => [b"B2A1", b"B2A0", b"B2A2"],
        };
        let mut transform = wanted.iter().filter_map(|s| tag(s)).find_map(|t| lut(t, family.components())).map(Transform::Lut);
        if transform.is_none() {
            transform = match family {
                Family::Gray => tag(b"kTRC").and_then(curve).map(Transform::Gray),
                Family::Rgb => matrix_trc(&tag),
                Family::Cmyk => None,
            };
        }
        Ok(Profile { family, description, lab_pcs, transform })
    }

    /// Whether colours are converted through the profile (not the built-in approximation).
    pub fn exact(&self) -> bool {
        self.transform.is_some()
    }

    /// CMYK (0–1) for an sRGB colour, through this CMYK profile's tables (`None` for another
    /// family or a profile without usable tables).
    pub fn cmyk_from_srgb(&self, rgb: [f64; 3]) -> Option<[f64; 4]> {
        self.cmyk_from_xyz(srgb_to_xyz(rgb.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })))
    }

    /// CMYK (0–1) for a D50 CIE L*a*b* colour, through this CMYK profile's tables.
    pub fn cmyk_from_lab(&self, lab: [f64; 3]) -> Option<[f64; 4]> {
        if !lab.iter().all(|v| v.is_finite()) {
            return None;
        }
        self.cmyk_from_xyz(lab_to_xyz(lab))
    }

    fn cmyk_from_xyz(&self, xyz: [f64; 3]) -> Option<[f64; 4]> {
        if self.family != Family::Cmyk {
            return None;
        }
        match self.device_from_xyz(xyz)?.as_slice() {
            [c, m, y, k] => Some([*c, *m, *y, *k].map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })),
            _ => None,
        }
    }

    /// Device values (0–1) for a PCS colour given as D50-relative XYZ.
    pub(super) fn device_from_xyz(&self, xyz: [f64; 3]) -> Option<Vec<f64>> {
        match self.transform.as_ref()? {
            Transform::Gray(c) => Some(vec![c.invert(xyz[1])]),
            Transform::Matrix { inverse, curves } => {
                let lin = mul(inverse, xyz);
                Some(lin.iter().zip(curves).map(|(v, c)| c.invert(*v)).collect())
            }
            Transform::Lut(l) => {
                let input = if self.lab_pcs {
                    let [ll, a, b] = xyz_to_lab(xyz);
                    if l.wide {
                        [ll * 652.8 / 65535.0, (a + 128.0) * 256.0 / 65535.0, (b + 128.0) * 256.0 / 65535.0]
                    } else {
                        [ll / 100.0, (a + 128.0) / 255.0, (b + 128.0) / 255.0]
                    }
                } else {
                    let v = match &l.matrix {
                        Some(m) => mul(m, xyz),
                        None => xyz,
                    };
                    let k = 32768.0 / 65535.0;
                    [v[0] * k, v[1] * k, v[2] * k]
                };
                Some(l.eval(input))
            }
        }
    }
}

impl Lut {
    fn eval(&self, input: [f64; 3]) -> Vec<f64> {
        let x: Vec<f64> =
            input.iter().enumerate().map(|(i, v)| table(self.inputs.get(i).map_or(&[][..], |t| t.as_slice()), v.clamp(0.0, 1.0))).collect();
        let g = self.grid;
        let last = g.saturating_sub(1) as f64;
        let mut cell = [(0usize, 0usize, 0.0f64); 3];
        for (i, c) in cell.iter_mut().enumerate() {
            let p = x.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0) * last;
            let lo = (p.floor() as usize).min(g.saturating_sub(1));
            let hi = (lo + 1).min(g.saturating_sub(1));
            *c = (lo, hi, p - lo as f64);
        }
        let mut out = vec![0.0; self.outputs];
        for corner in 0..8usize {
            let mut w = 1.0;
            let mut index = 0usize;
            // The first input varies slowest.
            for (i, (lo, hi, f)) in cell.iter().enumerate() {
                let up = corner >> (2 - i) & 1 == 1;
                w *= if up { *f } else { 1.0 - f };
                index = index * g + if up { *hi } else { *lo };
            }
            if w == 0.0 {
                continue;
            }
            for (j, o) in out.iter_mut().enumerate() {
                *o += w * self.clut.get(index * self.outputs + j).copied().unwrap_or(0.0);
            }
        }
        out.iter()
            .enumerate()
            .map(|(j, v)| table(self.out_tables.get(j).map_or(&[][..], |t| t.as_slice()), v.clamp(0.0, 1.0)).clamp(0.0, 1.0))
            .collect()
    }
}

fn description(t: &[u8]) -> Option<String> {
    match t.get(0..4)? {
        b"desc" => {
            let n = u32_at(t, 8)? as usize;
            let text = t.get(12..12usize.checked_add(n)?)?;
            Some(String::from_utf8_lossy(text).trim_end_matches('\0').trim().to_string())
        }
        b"mluc" => {
            let records = u32_at(t, 8)?;
            if records == 0 {
                return None;
            }
            let len = u32_at(t, 20)? as usize;
            let off = u32_at(t, 24)? as usize;
            let bytes = t.get(off..off.checked_add(len)?)?;
            let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
            Some(String::from_utf16_lossy(&units).trim_end_matches('\0').trim().to_string())
        }
        b"text" => Some(String::from_utf8_lossy(t.get(8..)?).trim_end_matches('\0').trim().to_string()),
        _ => None,
    }
    .map(|s| s.chars().take(256).collect())
}

fn curve(t: &[u8]) -> Option<Curve> {
    match t.get(0..4)? {
        b"curv" => {
            let n = u32_at(t, 8)? as usize;
            match n {
                0 => Some(Curve::Gamma(1.0)),
                1 => Some(Curve::Gamma(f64::from(u16_at(t, 12)?) / 256.0)),
                n if n <= 1 << 16 => {
                    let v: Option<Vec<f64>> = (0..n).map(|i| u16_at(t, 12 + 2 * i).map(|v| f64::from(v) / 65535.0)).collect();
                    v.map(Curve::Table)
                }
                _ => None,
            }
        }
        b"para" => {
            let kind = u16_at(t, 8)?;
            let n = match kind {
                0 => 1,
                1 => 3,
                2 => 4,
                3 => 5,
                4 => 7,
                _ => return None,
            };
            let p: Vec<f64> = (0..n).map(|i| s15f16(t, 12 + 4 * i)).collect::<Option<_>>()?;
            let g = *p.first()?;
            if kind == 0 {
                return Some(Curve::Gamma(g));
            }
            let at = |i: usize| p.get(i).copied().unwrap_or(0.0);
            let f = |x: f64| -> f64 {
                let (a, b, c, d, e, ff) = (at(1), at(2), at(3), at(4), at(5), at(6));
                let pw = |v: f64| if v > 0.0 { v.powf(g) } else { 0.0 };
                match kind {
                    1 => {
                        if a.abs() > 1e-12 && x >= -b / a {
                            pw(a * x + b)
                        } else {
                            0.0
                        }
                    }
                    2 => {
                        if a.abs() > 1e-12 && x >= -b / a {
                            pw(a * x + b) + c
                        } else {
                            c
                        }
                    }
                    3 => {
                        if x >= d {
                            pw(a * x + b)
                        } else {
                            c * x
                        }
                    }
                    _ => {
                        if x >= d {
                            pw(a * x + b) + e
                        } else {
                            c * x + ff
                        }
                    }
                }
            };
            Some(Curve::Table((0..1024).map(|i| f(i as f64 / 1023.0).clamp(0.0, 1.0)).collect()))
        }
        _ => None,
    }
}

fn matrix_trc<'a>(tag: &impl Fn(&[u8; 4]) -> Option<&'a [u8]>) -> Option<Transform> {
    let col = |sig: &[u8; 4]| -> Option<[f64; 3]> {
        let t = tag(sig)?;
        if t.get(0..4)? != b"XYZ " {
            return None;
        }
        Some([s15f16(t, 8)?, s15f16(t, 12)?, s15f16(t, 16)?])
    };
    let (r, g, b) = (col(b"rXYZ")?, col(b"gXYZ")?, col(b"bXYZ")?);
    let m = [r[0], g[0], b[0], r[1], g[1], b[1], r[2], g[2], b[2]];
    let inverse = invert(&m)?;
    let curves = [curve(tag(b"rTRC")?)?, curve(tag(b"gTRC")?)?, curve(tag(b"bTRC")?)?];
    Some(Transform::Matrix { inverse, curves })
}

/// A BToA lut8/lut16 with three PCS inputs and `outputs` device channels.
fn lut(t: &[u8], outputs: usize) -> Option<Lut> {
    let wide = match t.get(0..4)? {
        b"mft2" => true,
        b"mft1" => false,
        _ => return None,
    };
    let (inc, outc, grid) = (*t.get(8)? as usize, *t.get(9)? as usize, *t.get(10)? as usize);
    if inc != 3 || outc != outputs || !(2..=MAX_GRID).contains(&grid) {
        return None;
    }
    let mut m = [0.0; 9];
    for (i, v) in m.iter_mut().enumerate() {
        *v = s15f16(t, 12 + 4 * i)?;
    }
    let identity = m == [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let (in_n, out_n, mut at) = if wide { (u16_at(t, 48)? as usize, u16_at(t, 50)? as usize, 52usize) } else { (256, 256, 48usize) };
    if !(2..=MAX_TABLE).contains(&in_n) || !(2..=MAX_TABLE).contains(&out_n) {
        return None;
    }
    let width = if wide { 2 } else { 1 };
    let max = if wide { 65535.0 } else { 255.0 };
    let mut read = |count: usize| -> Option<Vec<f64>> {
        let bytes = t.get(at..at.checked_add(count.checked_mul(width)?)?)?;
        at = at.checked_add(count * width)?;
        Some(if wide {
            bytes.as_chunks::<2>().0.iter().map(|c| f64::from(u16::from_be_bytes(*c)) / max).collect()
        } else {
            bytes.iter().map(|b| f64::from(*b) / max).collect()
        })
    };
    let inputs: Vec<Vec<f64>> = (0..inc).map(|_| read(in_n)).collect::<Option<_>>()?;
    let points = grid.checked_mul(grid)?.checked_mul(grid)?.checked_mul(outc)?;
    if points > MAX_CLUT {
        return None;
    }
    let clut = read(points)?;
    let out_tables: Vec<Vec<f64>> = (0..outc).map(|_| read(out_n)).collect::<Option<_>>()?;
    Some(Lut { matrix: (!identity).then_some(m), inputs, grid, outputs: outc, clut, out_tables, wide })
}

// ---- PCS arithmetic -------------------------------------------------------------------------

pub(super) fn mul(m: &[f64; 9], v: [f64; 3]) -> [f64; 3] {
    [m[0] * v[0] + m[1] * v[1] + m[2] * v[2], m[3] * v[0] + m[4] * v[1] + m[5] * v[2], m[6] * v[0] + m[7] * v[1] + m[8] * v[2]]
}

pub(super) fn invert(m: &[f64; 9]) -> Option<[f64; 9]> {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let k = 1.0 / det;
    Some([
        (e * i - f * h) * k,
        (c * h - b * i) * k,
        (b * f - c * e) * k,
        (f * g - d * i) * k,
        (a * i - c * g) * k,
        (c * d - a * f) * k,
        (d * h - e * g) * k,
        (b * g - a * h) * k,
        (a * e - b * d) * k,
    ])
}

/// sRGB primaries adapted to D50 (Bradford), linear RGB → XYZ.
const SRGB_TO_XYZ: [f64; 9] = [0.4361, 0.3851, 0.1431, 0.2225, 0.7169, 0.0606, 0.0139, 0.0971, 0.7141];

fn srgb_linear(v: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn srgb_encode(v: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

pub(super) fn srgb_to_xyz(rgb: [f64; 3]) -> [f64; 3] {
    mul(&SRGB_TO_XYZ, [srgb_linear(rgb[0]), srgb_linear(rgb[1]), srgb_linear(rgb[2])])
}

pub(super) fn xyz_to_srgb(xyz: [f64; 3]) -> [f64; 3] {
    let inv = invert(&SRGB_TO_XYZ).unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let l = mul(&inv, xyz);
    [srgb_encode(l[0]), srgb_encode(l[1]), srgb_encode(l[2])]
}

pub(super) fn xyz_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (x, y, z) = (f(xyz[0] / D50[0]), f(xyz[1] / D50[1]), f(xyz[2] / D50[2]));
    [(116.0 * y - 16.0).clamp(0.0, 100.0), (500.0 * (x - y)).clamp(-128.0, 127.0), (200.0 * (y - z)).clamp(-128.0, 127.0)]
}

/// Lab → XYZ relative to D50. Lab is relative to its space's white point; scaling XYZ from
/// that white to D50 (von Kries on XYZ, as viewers do) gives the same as reading it against D50.
pub(super) fn lab_to_xyz(lab: [f64; 3]) -> [f64; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    let g = |t: f64| if t > 6.0 / 29.0 { t * t * t } else { 108.0 / 841.0 * (t - 4.0 / 29.0) };
    [g(fx) * D50[0], g(fy) * D50[1], g(fz) * D50[2]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lab_round_trips_through_xyz() {
        for lab in [[50.0, 20.0, -30.0], [100.0, 0.0, 0.0], [0.0, 0.0, 0.0], [75.0, -60.0, 70.0]] {
            let back = xyz_to_lab(lab_to_xyz(lab));
            for (a, b) in lab.iter().zip(back) {
                assert!((a - b).abs() < 1e-6, "{lab:?} → {back:?}");
            }
        }
        let white = xyz_to_srgb(srgb_to_xyz([1.0, 1.0, 1.0]));
        assert!(white.iter().all(|v| (v - 1.0).abs() < 1e-3), "{white:?}");
    }

    #[test]
    fn hostile_profiles_are_refused_without_panicking() {
        assert!(Profile::parse(&[], 1).is_err());
        assert!(Profile::parse(&[0u8; 200], 1).is_err());
        let mut p = vec![0u8; 200];
        p[0..4].copy_from_slice(&200u32.to_be_bytes());
        p[36..40].copy_from_slice(b"acsp");
        p[12..16].copy_from_slice(b"prtr");
        p[16..20].copy_from_slice(b"CMYK");
        p[20..24].copy_from_slice(b"Lab ");
        p[128..132].copy_from_slice(&9u32.to_be_bytes());
        // Nine tags declared, the table runs off the end: refused, never read past the bytes.
        assert!(Profile::parse(&p, 1).is_err());
        p[128..132].copy_from_slice(&1u32.to_be_bytes());
        p[132..136].copy_from_slice(b"B2A1");
        p[136..140].copy_from_slice(&150u32.to_be_bytes());
        p[140..144].copy_from_slice(&u32::MAX.to_be_bytes());
        let read = Profile::parse(&p, 1).unwrap();
        assert!(!read.exact(), "a tag running past the end is ignored");
    }
}
