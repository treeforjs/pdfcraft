//! Small ICC profiles (version 2, display class) written in code from published colorimetry.
//! [`srgb`] holds the published sRGB primaries (IEC 61966-2-1) adapted to D50, with the sRGB
//! tone curve sampled at 1024 points, and is the PDF/A output intent's destination profile.
//! [`rgb_matrix_trc`] and [`gray_trc`] build other RGB working spaces (from the chromaticities
//! of their primaries and white, Bradford-adapted to D50) and gray working spaces (a tone curve)
//! for the colour settings.

fn s15f16(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn xyz(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut t = b"XYZ \0\0\0\0".to_vec();
    t.extend(s15f16(x));
    t.extend(s15f16(y));
    t.extend(s15f16(z));
    t
}

fn desc(text: &str) -> Vec<u8> {
    let mut t = b"desc\0\0\0\0".to_vec();
    let ascii: Vec<u8> = text.bytes().chain(std::iter::once(0)).collect();
    t.extend((ascii.len() as u32).to_be_bytes());
    t.extend(&ascii);
    // No Unicode or ScriptCode descriptions.
    t.extend([0u8; 4 + 4]);
    t.extend([0u8; 2 + 1 + 67]);
    t
}

fn text(text: &str) -> Vec<u8> {
    let mut t = b"text\0\0\0\0".to_vec();
    t.extend(text.bytes());
    t.push(0);
    t
}

fn curve() -> Vec<u8> {
    const N: usize = 1024;
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend((N as u32).to_be_bytes());
    for i in 0..N {
        let v = i as f64 / (N - 1) as f64;
        let lin = if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        t.extend(((lin * 65535.0).round() as u16).to_be_bytes());
    }
    t
}

/// The profile's bytes.
pub fn srgb() -> Vec<u8> {
    // sRGB primaries, Bradford-adapted to the D50 profile connection space.
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", desc("sRGB (PdfCraft)")),
        (b"cprt", text("No copyright, use freely")),
        (b"wtpt", xyz(0.9642, 1.0, 0.8249)),
        (b"rXYZ", xyz(0.4361, 0.2225, 0.0139)),
        (b"gXYZ", xyz(0.3851, 0.7169, 0.0971)),
        (b"bXYZ", xyz(0.1431, 0.0606, 0.7141)),
        (b"rTRC", curve()),
    ];
    // gTRC and bTRC share rTRC's data.
    let count = tags.len() + 2;
    let table_len = 4 + count * 12;
    let mut offset = 128 + table_len;
    let mut table = (count as u32).to_be_bytes().to_vec();
    let mut data = Vec::new();
    let mut trc = (0, 0);
    for (sig, body) in &tags {
        let len = body.len();
        table.extend(*sig);
        table.extend((offset as u32).to_be_bytes());
        table.extend((len as u32).to_be_bytes());
        if *sig == b"rTRC" {
            trc = (offset, len);
        }
        data.extend(body);
        // Tags start on 4-byte boundaries.
        let pad = (4 - len % 4) % 4;
        data.extend(std::iter::repeat_n(0u8, pad));
        offset += len + pad;
    }
    for sig in [b"gTRC", b"bTRC"] {
        table.extend(sig);
        table.extend((trc.0 as u32).to_be_bytes());
        table.extend((trc.1 as u32).to_be_bytes());
    }
    let size = 128 + table.len() + data.len();
    let mut h = Vec::with_capacity(size);
    h.extend((size as u32).to_be_bytes());
    h.extend(b"\0\0\0\0"); // preferred CMM
    h.extend([2, 0x10, 0, 0]); // version 2.1
    h.extend(b"mntr");
    h.extend(b"RGB ");
    h.extend(b"XYZ ");
    h.extend([0u8; 12]); // date
    h.extend(b"acsp");
    h.extend(b"\0\0\0\0"); // platform
    h.extend([0u8; 4]); // flags
    h.extend([0u8; 4]); // manufacturer
    h.extend([0u8; 4]); // model
    h.extend([0u8; 8]); // attributes
    h.extend([0, 0, 0, 0]); // perceptual intent
    h.extend(s15f16(0.9642));
    h.extend(s15f16(1.0));
    h.extend(s15f16(0.8249));
    h.extend([0u8; 4]); // creator
    h.resize(128, 0);
    h.extend(table);
    h.extend(data);
    h
}

/// The sRGB tone curve (encoded value → linear light).
pub fn srgb_curve(v: f64) -> f64 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn sampled_curve(f: &dyn Fn(f64) -> f64) -> Vec<u8> {
    const N: usize = 1024;
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend((N as u32).to_be_bytes());
    for i in 0..N {
        let v = f(i as f64 / (N - 1) as f64);
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        t.extend(((v * 65535.0).round() as u16).to_be_bytes());
    }
    t
}

/// A profile of colour space `space` (`b"RGB "`, `b"GRAY"`) from its tags; tags named in
/// `aliases` share the data of the tag they name.
fn assemble(space: &[u8; 4], tags: &[(&[u8; 4], Vec<u8>)], aliases: &[(&[u8; 4], &[u8; 4])]) -> Vec<u8> {
    let count = tags.len() + aliases.len();
    let mut offset = 128 + 4 + count * 12;
    let mut table = (count as u32).to_be_bytes().to_vec();
    let mut data = Vec::new();
    let mut placed: Vec<([u8; 4], usize, usize)> = Vec::new();
    for (sig, body) in tags {
        let len = body.len();
        table.extend(*sig);
        table.extend((offset as u32).to_be_bytes());
        table.extend((len as u32).to_be_bytes());
        placed.push((**sig, offset, len));
        data.extend(body);
        let pad = (4 - len % 4) % 4;
        data.extend(std::iter::repeat_n(0u8, pad));
        offset += len + pad;
    }
    for (sig, target) in aliases {
        let (off, len) = placed.iter().find(|(s, _, _)| s == *target).map_or((0, 0), |(_, o, l)| (*o, *l));
        table.extend(*sig);
        table.extend((off as u32).to_be_bytes());
        table.extend((len as u32).to_be_bytes());
    }
    let size = 128 + table.len() + data.len();
    let mut h = Vec::with_capacity(size);
    h.extend((size as u32).to_be_bytes());
    h.extend(b"\0\0\0\0");
    h.extend([2, 0x10, 0, 0]);
    h.extend(b"mntr");
    h.extend(space);
    h.extend(b"XYZ ");
    h.extend([0u8; 12]);
    h.extend(b"acsp");
    h.extend([0u8; 4 + 4 + 4 + 4 + 8 + 4]);
    h.extend(s15f16(0.9642));
    h.extend(s15f16(1.0));
    h.extend(s15f16(0.8249));
    h.resize(128, 0);
    h.extend(table);
    h.extend(data);
    h
}

fn xy_to_xyz([x, y]: [f64; 2]) -> [f64; 3] {
    let y = if y.abs() < 1e-9 { 1e-9 } else { y };
    [x / y, 1.0, (1.0 - x - y) / y]
}

fn mul3(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

fn inverse3(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let c = |r1: usize, c1: usize, r2: usize, c2: usize| m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1];
    Some([
        [c(1, 1, 2, 2) / det, -c(0, 1, 2, 2) / det, c(0, 1, 1, 2) / det],
        [-c(1, 0, 2, 2) / det, c(0, 0, 2, 2) / det, -c(0, 0, 1, 2) / det],
        [c(1, 0, 2, 1) / det, -c(0, 0, 2, 1) / det, c(0, 0, 1, 1) / det],
    ])
}

/// The D50-adapted colorants (columns XYZ of red, green and blue) of an RGB space whose
/// primaries and white have chromaticities `primaries` and `white` (Bradford adaptation).
pub fn d50_colorants(primaries: [[f64; 2]; 3], white: [f64; 2]) -> Option<[[f64; 3]; 3]> {
    let [r, g, b] = primaries.map(xy_to_xyz);
    let p = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let w = xy_to_xyz(white);
    let s = mul3(&inverse3(&p)?, w);
    let scaled = [0, 1, 2].map(|row| [0, 1, 2].map(|col| p[row][col] * s[col]));
    const BRADFORD: [[f64; 3]; 3] = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let d50 = [0.9642, 1.0, 0.8249];
    let (cs, cd) = (mul3(&BRADFORD, w), mul3(&BRADFORD, d50));
    let ratio = [0, 1, 2].map(|i| if cs[i].abs() < 1e-12 { 1.0 } else { cd[i] / cs[i] });
    let inv = inverse3(&BRADFORD)?;
    // adapt = inv · diag(ratio) · BRADFORD
    let scaled_b = [0, 1, 2].map(|row| [0, 1, 2].map(|col| ratio[row] * BRADFORD[row][col]));
    let adapt = [0, 1, 2].map(|row| [0, 1, 2].map(|col| (0..3).map(|k| inv[row][k] * scaled_b[k][col]).sum::<f64>()));
    let out = [0, 1, 2].map(|row| [0, 1, 2].map(|col| (0..3).map(|k| adapt[row][k] * scaled[k][col]).sum::<f64>()));
    out.iter().flatten().all(|v| v.is_finite()).then_some(out)
}

/// An RGB matrix/TRC display profile described as `description`, with D50 `colorants` (from
/// [`d50_colorants`]) and the tone curve `curve` (encoded value → linear light, 0–1) for all
/// three channels.
pub fn rgb_matrix_trc(description: &str, colorants: [[f64; 3]; 3], curve: &dyn Fn(f64) -> f64) -> Vec<u8> {
    let column = |c: usize| xyz(colorants[0][c], colorants[1][c], colorants[2][c]);
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", desc(description)),
        (b"cprt", text("No copyright, use freely")),
        (b"wtpt", xyz(0.9642, 1.0, 0.8249)),
        (b"rXYZ", column(0)),
        (b"gXYZ", column(1)),
        (b"bXYZ", column(2)),
        (b"rTRC", sampled_curve(curve)),
    ];
    assemble(b"RGB ", &tags, &[(b"gTRC", b"rTRC"), (b"bTRC", b"rTRC")])
}

/// A gray display profile described as `description` whose tone curve `curve` maps the gray
/// value (0 black – 1 white) to luminance (0–1).
pub fn gray_trc(description: &str, curve: &dyn Fn(f64) -> f64) -> Vec<u8> {
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", desc(description)),
        (b"cprt", text("No copyright, use freely")),
        (b"wtpt", xyz(0.9642, 1.0, 0.8249)),
        (b"kTRC", sampled_curve(curve)),
    ];
    assemble(b"GRAY", &tags, &[])
}

#[cfg(test)]
mod tests {
    #[test]
    fn profile_is_well_formed() {
        let p = super::srgb();
        assert_eq!(u32::from_be_bytes(p[0..4].try_into().unwrap()) as usize, p.len());
        assert_eq!(&p[36..40], b"acsp");
        assert_eq!(&p[12..20], b"mntrRGB ");
        let n = u32::from_be_bytes(p[128..132].try_into().unwrap()) as usize;
        assert_eq!(n, 9);
        for i in 0..n {
            let e = 132 + i * 12;
            let off = u32::from_be_bytes(p[e + 4..e + 8].try_into().unwrap()) as usize;
            let len = u32::from_be_bytes(p[e + 8..e + 12].try_into().unwrap()) as usize;
            assert!(off.is_multiple_of(4) && off + len <= p.len(), "tag {i}");
        }
    }

    #[test]
    fn built_profiles_are_well_formed_and_srgb_colorants_match() {
        let srgb = super::d50_colorants([[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]], [0.3127, 0.3290]).unwrap();
        // The published D50-adapted sRGB colorants.
        let want = [[0.4361, 0.3851, 0.1431], [0.2225, 0.7169, 0.0606], [0.0139, 0.0971, 0.7141]];
        for r in 0..3 {
            for c in 0..3 {
                assert!((srgb[r][c] - want[r][c]).abs() < 2e-3, "{r},{c}: {}", srgb[r][c]);
            }
        }
        for p in [super::rgb_matrix_trc("x", srgb, &super::srgb_curve), super::gray_trc("g", &|v| v.powf(2.2))] {
            assert_eq!(u32::from_be_bytes(p[0..4].try_into().unwrap()) as usize, p.len());
            assert_eq!(&p[36..40], b"acsp");
            let n = u32::from_be_bytes(p[128..132].try_into().unwrap()) as usize;
            for i in 0..n {
                let e = 132 + i * 12;
                let off = u32::from_be_bytes(p[e + 4..e + 8].try_into().unwrap()) as usize;
                let len = u32::from_be_bytes(p[e + 8..e + 12].try_into().unwrap()) as usize;
                assert!(off.is_multiple_of(4) && len > 0 && off + len <= p.len(), "tag {i}");
            }
        }
        assert!(super::d50_colorants([[0.3, 0.3]; 3], [0.3127, 0.3290]).is_none(), "degenerate primaries");
    }
}
