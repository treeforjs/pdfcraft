//! Colour spaces as Convert Colors reads them, their colours in a common form, and the
//! conversion to the destination.

use pdfcraft_cos::{Dict, Document, Object};

use super::function::Func;
use super::icc::{self, D50, Family, Profile};

/// Colour spaces nested in colour spaces (Indexed, alternates, patterns), at most this deep.
const MAX_CS_DEPTH: usize = 8;
const MAX_COMPONENTS: usize = 32;

#[derive(Clone, Debug)]
pub(super) enum Space {
    Gray,
    Rgb,
    Cmyk,
    /// ICCBased with `n` components; `dest` when its profile is the destination profile.
    Icc {
        n: usize,
        dest: bool,
    },
    CalGray {
        gamma: f64,
    },
    CalRgb {
        white: [f64; 3],
        gamma: [f64; 3],
        matrix: [f64; 9],
    },
    Lab {
        range: [f64; 4],
    },
    Separation {
        name: Vec<u8>,
        alt: Box<Space>,
        tint: Option<Func>,
    },
    DeviceN {
        names: Vec<Vec<u8>>,
        alt: Box<Space>,
        tint: Option<Func>,
    },
    Indexed {
        base: Box<Space>,
        hival: usize,
        lookup: Vec<u8>,
    },
    Pattern(Option<Box<Space>>),
    Unknown(usize),
}

/// A colour in a form every destination can be reached from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Value {
    Gray(f64),
    Rgb([f64; 3]),
    Cmyk([f64; 4]),
    /// CIE XYZ relative to D50.
    Xyz([f64; 3]),
}

fn clamp01(v: f64) -> f64 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

fn nums(doc: &Document, o: Option<&Object>, n: usize) -> Option<Vec<f64>> {
    let o = doc.resolve(o?);
    let v: Vec<f64> = o.as_array()?.iter().take(n).filter_map(|x| doc.resolve(x).as_f64()).filter(|v| v.is_finite()).collect();
    (v.len() == n).then_some(v)
}

fn three(v: Option<Vec<f64>>, default: [f64; 3]) -> [f64; 3] {
    match v.as_deref() {
        Some([a, b, c]) => [*a, *b, *c],
        _ => default,
    }
}

impl Space {
    /// Read a colour space (`o` may name a resource of `res`). `dest_profile` is the
    /// destination ICC profile, to recognise colours already in it.
    pub(super) fn parse(doc: &Document, o: &Object, res: &Dict, depth: usize, dest_profile: Option<&[u8]>) -> Space {
        if depth > MAX_CS_DEPTH {
            return Space::Unknown(1);
        }
        let o = doc.resolve(o);
        match o.as_ref() {
            Object::Name(n) => match n.as_slice() {
                b"DeviceGray" | b"G" => Space::Gray,
                b"DeviceRGB" | b"RGB" => Space::Rgb,
                b"DeviceCMYK" | b"CMYK" => Space::Cmyk,
                b"Pattern" => Space::Pattern(None),
                other => {
                    let entry = res.get(b"ColorSpace").map(|c| doc.resolve(c)).and_then(|c| c.as_dict().and_then(|d| d.get(other).cloned()));
                    match entry {
                        // A resource names an array or a device space, never another resource.
                        Some(v) => Space::parse(doc, &v, &Dict::new(), depth + 1, dest_profile),
                        None => Space::Unknown(1),
                    }
                }
            },
            Object::Array(a) => {
                let family = a.first().map(|f| doc.resolve(f)).and_then(|f| f.as_name().map(<[u8]>::to_vec)).unwrap_or_default();
                let arg = |i: usize| a.get(i).map(|o| doc.resolve(o));
                match family.as_slice() {
                    b"DeviceGray" | b"G" => Space::Gray,
                    b"DeviceRGB" | b"RGB" => Space::Rgb,
                    b"DeviceCMYK" | b"CMYK" => Space::Cmyk,
                    b"CalGray" => {
                        let d = arg(1).and_then(|d| d.as_dict().cloned()).unwrap_or_default();
                        let gamma =
                            d.get(b"Gamma").and_then(|g| doc.resolve(g).as_f64()).filter(|g| g.is_finite() && *g > 0.0 && *g < 16.0).unwrap_or(1.0);
                        Space::CalGray { gamma }
                    }
                    b"CalRGB" => {
                        let d = arg(1).and_then(|d| d.as_dict().cloned()).unwrap_or_default();
                        let white = three(nums(doc, d.get(b"WhitePoint"), 3), D50);
                        let gamma = three(nums(doc, d.get(b"Gamma"), 3), [1.0; 3]).map(|g| if g > 0.0 && g < 16.0 { g } else { 1.0 });
                        let matrix = match nums(doc, d.get(b"Matrix"), 9) {
                            Some(m) => {
                                let mut out = [0.0; 9];
                                for (o, v) in out.iter_mut().zip(m) {
                                    *o = v;
                                }
                                out
                            }
                            None => [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                        };
                        Space::CalRgb { white, gamma, matrix }
                    }
                    b"Lab" => {
                        let d = arg(1).and_then(|d| d.as_dict().cloned()).unwrap_or_default();
                        let range = match nums(doc, d.get(b"Range"), 4).as_deref() {
                            Some([a, b, c, e]) if a < b && c < e => [*a, *b, *c, *e],
                            _ => [-100.0, 100.0, -100.0, 100.0],
                        };
                        Space::Lab { range }
                    }
                    b"ICCBased" => {
                        let stream = arg(1);
                        let n = stream.as_deref().and_then(|s| s.as_dict()).and_then(|d| d.get(b"N")).and_then(|n| doc.resolve(n).as_int());
                        let dest = match (stream.as_deref(), dest_profile) {
                            (Some(Object::Stream(s)), Some(p)) => s.decoded_within(icc::MAX_PROFILE).is_ok_and(|d| d == p),
                            _ => false,
                        };
                        match n {
                            Some(n @ (1 | 3 | 4)) => Space::Icc { n: n as usize, dest },
                            Some(n) => Space::Unknown(usize::try_from(n).unwrap_or(1).clamp(1, MAX_COMPONENTS)),
                            None => Space::Unknown(1),
                        }
                    }
                    b"Separation" => {
                        let name = arg(1).and_then(|n| n.as_name().map(<[u8]>::to_vec)).unwrap_or_default();
                        let alt = a.get(2).map_or(Space::Unknown(1), |s| Space::parse(doc, s, res, depth + 1, dest_profile));
                        let tint = a.get(3).and_then(|f| Func::parse(doc, f, 0));
                        Space::Separation { name, alt: Box::new(alt), tint }
                    }
                    b"DeviceN" => match arg(1).as_deref().and_then(Object::as_array) {
                        Some(names) if !names.is_empty() && names.len() <= MAX_COMPONENTS => {
                            let names: Vec<Vec<u8>> =
                                names.iter().map(|n| doc.resolve(n).as_name().map(<[u8]>::to_vec).unwrap_or_default()).collect();
                            let alt = a.get(2).map_or(Space::Unknown(1), |s| Space::parse(doc, s, res, depth + 1, dest_profile));
                            let tint = a.get(3).and_then(|f| Func::parse(doc, f, 0));
                            Space::DeviceN { names, alt: Box::new(alt), tint }
                        }
                        _ => Space::Unknown(1),
                    },
                    b"Indexed" | b"I" => {
                        let base = a.get(1).map_or(Space::Unknown(1), |b| Space::parse(doc, b, res, depth + 1, dest_profile));
                        if matches!(base, Space::Indexed { .. } | Space::Pattern(_) | Space::Unknown(_)) {
                            return Space::Unknown(1);
                        }
                        let hival = arg(2).and_then(|h| h.as_int()).unwrap_or(0).clamp(0, 255) as usize;
                        let want = (hival + 1).saturating_mul(base.components());
                        let lookup: Vec<u8> = match arg(3).as_deref() {
                            Some(Object::String(s)) => s.bytes.iter().take(want).copied().collect(),
                            Some(Object::Stream(s)) => {
                                s.decoded_within(want.max(1).saturating_mul(2)).unwrap_or_default().into_iter().take(want).collect()
                            }
                            _ => Vec::new(),
                        };
                        if lookup.len() < want {
                            return Space::Unknown(1);
                        }
                        Space::Indexed { base: Box::new(base), hival, lookup }
                    }
                    b"Pattern" => Space::Pattern(a.get(1).map(|u| Box::new(Space::parse(doc, u, res, depth + 1, dest_profile)))),
                    _ => Space::Unknown(1),
                }
            }
            _ => Space::Unknown(1),
        }
    }

    pub(super) fn components(&self) -> usize {
        match self {
            Space::Gray | Space::CalGray { .. } | Space::Separation { .. } | Space::Indexed { .. } => 1,
            Space::Rgb | Space::CalRgb { .. } | Space::Lab { .. } => 3,
            Space::Cmyk => 4,
            Space::Icc { n, .. } => *n,
            Space::DeviceN { names, .. } => names.len(),
            Space::Pattern(under) => under.as_ref().map_or(0, |u| u.components()),
            Space::Unknown(n) => *n,
        }
    }

    /// The colour `cs`/`CS` selects (ISO 32000-2 Table 73).
    pub(super) fn initial(&self) -> Vec<f64> {
        match self {
            Space::Cmyk | Space::Icc { n: 4, .. } => vec![0.0, 0.0, 0.0, 1.0],
            Space::Separation { .. } | Space::DeviceN { .. } => vec![1.0; self.components()],
            Space::Lab { range } => vec![0.0, 0.0f64.clamp(range[0], range[1]), 0.0f64.clamp(range[2], range[3])],
            _ => vec![0.0; self.components()],
        }
    }

    /// The default decode range of each component (images, lookup tables).
    pub(super) fn ranges(&self) -> Vec<[f64; 2]> {
        match self {
            Space::Lab { range } => vec![[0.0, 100.0], [range[0], range[1]], [range[2], range[3]]],
            _ => vec![[0.0, 1.0]; self.components()],
        }
    }

    /// Whether the space is a spot colour (Separation or DeviceN) that can be converted:
    /// not `/All` or `/None`, with a tint transform that evaluates.
    pub(super) fn is_spot(&self) -> bool {
        matches!(self, Space::Separation { .. } | Space::DeviceN { .. })
    }

    pub(super) fn spot_convertible(&self) -> bool {
        match self {
            Space::Separation { name, tint, alt } => name != b"All" && name != b"None" && tint.is_some() && alt.process(),
            Space::DeviceN { names, tint, alt } => !names.iter().any(|n| n == b"All") && tint.is_some() && alt.process(),
            _ => false,
        }
    }

    /// Spaces a colour can be read from directly (alternates of spot colours).
    fn process(&self) -> bool {
        matches!(self, Space::Gray | Space::Rgb | Space::Cmyk | Space::CalGray { .. } | Space::CalRgb { .. } | Space::Lab { .. } | Space::Icc { .. })
    }

    /// The colour of components `c`, when it can be read.
    pub(super) fn value(&self, c: &[f64]) -> Option<Value> {
        let at = |i: usize| clamp01(c.get(i).copied().unwrap_or(0.0));
        match self {
            Space::Gray | Space::Icc { n: 1, .. } => Some(Value::Gray(at(0))),
            Space::Rgb | Space::Icc { n: 3, .. } => Some(Value::Rgb([at(0), at(1), at(2)])),
            Space::Cmyk | Space::Icc { n: 4, .. } => Some(Value::Cmyk([at(0), at(1), at(2), at(3)])),
            Space::Icc { .. } => None,
            Space::CalGray { gamma, .. } => {
                let y = at(0).powf(*gamma);
                Some(Value::Xyz([D50[0] * y, D50[1] * y, D50[2] * y]))
            }
            Space::CalRgb { white, gamma, matrix } => {
                let lin = [at(0).powf(gamma[0]), at(1).powf(gamma[1]), at(2).powf(gamma[2])];
                // The matrix lists XA YA ZA XB YB ZB XC YC ZC.
                let m = [matrix[0], matrix[3], matrix[6], matrix[1], matrix[4], matrix[7], matrix[2], matrix[5], matrix[8]];
                let xyz = icc::mul(&m, lin);
                let scale = |i: usize| {
                    let w = white.get(i).copied().unwrap_or(1.0);
                    if w.is_finite() && w > 1e-6 { D50.get(i).copied().unwrap_or(1.0) / w } else { 1.0 }
                };
                Some(Value::Xyz([xyz[0] * scale(0), xyz[1] * scale(1), xyz[2] * scale(2)]))
            }
            Space::Lab { range } => {
                let f = |i: usize, lo: f64, hi: f64| {
                    let v = c.get(i).copied().unwrap_or(0.0);
                    if v.is_finite() { v.clamp(lo, hi) } else { 0.0 }
                };
                Some(Value::Xyz(icc::lab_to_xyz([f(0, 0.0, 100.0), f(1, range[0], range[1]), f(2, range[2], range[3])])))
            }
            Space::Separation { tint, alt, .. } | Space::DeviceN { tint, alt, .. } => {
                let n = self.components();
                let input: Vec<f64> = (0..n).map(at).collect();
                let out = tint.as_ref()?.eval(&input)?;
                if out.len() < alt.components() {
                    return None;
                }
                alt.value(&out)
            }
            Space::Indexed { base, hival, lookup } => {
                let index = c.first().copied().unwrap_or(0.0);
                let index = if index.is_finite() { index.round().clamp(0.0, *hival as f64) as usize } else { 0 };
                let n = base.components();
                let start = index.checked_mul(n)?;
                let entry = lookup.get(start..start.checked_add(n)?)?;
                let ranges = base.ranges();
                let comps: Vec<f64> = entry
                    .iter()
                    .enumerate()
                    .map(|(i, b)| {
                        let r = ranges.get(i).copied().unwrap_or([0.0, 1.0]);
                        r[0] + f64::from(*b) / 255.0 * (r[1] - r[0])
                    })
                    .collect();
                base.value(&comps)
            }
            Space::Pattern(_) | Space::Unknown(_) => None,
        }
    }
}

/// The destination of a conversion.
#[derive(Clone, Debug)]
pub(super) struct Dest {
    pub family: Family,
    pub profile: Option<Profile>,
    pub preserve_black: bool,
}

/// RGB → CMYK without a press profile: full black generation with 70% under-colour removal,
/// so neutrals print mostly black and the darkest colours stay below 200% total ink.
fn rgb_to_cmyk([r, g, b]: [f64; 3]) -> [f64; 4] {
    let (c, m, y) = (1.0 - clamp01(r), 1.0 - clamp01(g), 1.0 - clamp01(b));
    let k = c.min(m).min(y);
    let ucr = 0.7 * k;
    [clamp01(c - ucr), clamp01(m - ucr), clamp01(y - ucr), k]
}

fn cmyk_to_rgb([c, m, y, k]: [f64; 4]) -> [f64; 3] {
    [(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)]
}

fn luma([r, g, b]: [f64; 3]) -> f64 {
    clamp01(0.299 * r + 0.587 * g + 0.114 * b)
}

fn black(v: Value) -> bool {
    let z = |x: f64| x <= 1e-6;
    match v {
        Value::Gray(g) => z(g),
        Value::Rgb(c) | Value::Xyz(c) => c.iter().all(|x| z(*x)),
        Value::Cmyk([c, m, y, k]) => z(c) && z(m) && z(y) && k >= 1.0 - 1e-6,
    }
}

impl Dest {
    /// Destination components (0–1) for `v`.
    pub(super) fn convert(&self, v: Value) -> Vec<f64> {
        if self.preserve_black && black(v) {
            return match self.family {
                Family::Gray => vec![0.0],
                Family::Rgb => vec![0.0; 3],
                Family::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
            };
        }
        let out = match (&self.profile, self.family, v) {
            // Same model: the numbers carry over (gray prints on the black plate).
            (_, Family::Gray, Value::Gray(g)) => vec![g],
            (_, Family::Rgb, Value::Rgb(c)) if self.profile.as_ref().is_none_or(|p| !p.exact()) => c.to_vec(),
            (_, Family::Cmyk, Value::Cmyk(c)) => c.to_vec(),
            (_, Family::Cmyk, Value::Gray(g)) => vec![0.0, 0.0, 0.0, 1.0 - g],
            (Some(p), _, v) if p.exact() => {
                let xyz = match v {
                    Value::Gray(g) => icc::srgb_to_xyz([g, g, g]),
                    Value::Rgb(c) => icc::srgb_to_xyz(c),
                    Value::Cmyk(c) => icc::srgb_to_xyz(cmyk_to_rgb(c)),
                    Value::Xyz(x) => x,
                };
                match p.device_from_xyz(xyz) {
                    Some(out) => out,
                    None => self.builtin(v),
                }
            }
            _ => self.builtin(v),
        };
        out.into_iter().map(clamp01).collect()
    }

    fn builtin(&self, v: Value) -> Vec<f64> {
        let rgb = |v: Value| match v {
            Value::Gray(g) => [g, g, g],
            Value::Rgb(c) => c,
            Value::Cmyk(c) => cmyk_to_rgb(c),
            Value::Xyz(x) => icc::xyz_to_srgb(x),
        };
        match self.family {
            Family::Gray => vec![match v {
                Value::Gray(g) => g,
                Value::Cmyk([c, m, y, k]) => 1.0 - (0.3 * c + 0.59 * m + 0.11 * y + k).min(1.0),
                other => luma(rgb(other)),
            }],
            Family::Rgb => rgb(v).to_vec(),
            Family::Cmyk => match v {
                Value::Cmyk(c) => c.to_vec(),
                Value::Gray(g) => vec![0.0, 0.0, 0.0, 1.0 - g],
                other => rgb_to_cmyk(rgb(other)).to_vec(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dest(family: Family, preserve_black: bool) -> Dest {
        Dest { family, profile: None, preserve_black }
    }

    #[test]
    fn built_in_conversions() {
        let cmyk = dest(Family::Cmyk, false);
        assert_eq!(cmyk.convert(Value::Rgb([1.0, 0.0, 0.0])), vec![0.0, 1.0, 1.0, 0.0]);
        let rich = cmyk.convert(Value::Rgb([0.0, 0.0, 0.0]));
        assert!((rich[0] - 0.3).abs() < 1e-9 && rich[3] == 1.0, "{rich:?}");
        assert_eq!(dest(Family::Cmyk, true).convert(Value::Rgb([0.0, 0.0, 0.0])), vec![0.0, 0.0, 0.0, 1.0]);
        assert_eq!(cmyk.convert(Value::Gray(0.25)), vec![0.0, 0.0, 0.0, 0.75]);
        let gray = dest(Family::Gray, true);
        assert!((gray.convert(Value::Rgb([1.0, 1.0, 1.0]))[0] - 1.0).abs() < 1e-9);
        assert_eq!(gray.convert(Value::Cmyk([0.0, 0.0, 0.0, 1.0])), vec![0.0]);
        let rgb = dest(Family::Rgb, false);
        assert_eq!(rgb.convert(Value::Cmyk([0.0, 0.0, 0.0, 0.0])), vec![1.0, 1.0, 1.0]);
        // Lab white is white, whatever the destination.
        let white = Space::Lab { range: [-100.0, 100.0, -100.0, 100.0] }.value(&[100.0, 0.0, 0.0]).unwrap();
        assert!(rgb.convert(white).iter().all(|v| (v - 1.0).abs() < 1e-3));
        assert!(cmyk.convert(white).iter().all(|v| v.abs() < 1e-3));
        assert!((gray.convert(white)[0] - 1.0).abs() < 1e-3);
    }
}
