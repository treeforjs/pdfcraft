//! Just enough of a TrueType reader to embed the repository's OFL fonts (`assets/fonts/`) in the
//! fidelity fixtures: units per em, bounding box, ascent/descent, horizontal advances and the
//! Unicode `cmap` (format 4 or 12). Every read is bounds-checked; a malformed font is an error.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

pub struct Ttf {
    pub units_per_em: u16,
    pub bbox: [i16; 4],
    pub ascent: i16,
    pub descent: i16,
    advances: Vec<u16>,
    cmap: BTreeMap<u32, u16>,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    let b = d.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

fn i16_at(d: &[u8], at: usize) -> Option<i16> {
    u16_at(d, at).map(|v| v as i16)
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    let b = d.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn table<'a>(d: &'a [u8], tag: &[u8; 4]) -> Result<&'a [u8]> {
    let n = u16_at(d, 4).context("ttf: no table count")? as usize;
    for i in 0..n {
        let rec = 12 + 16 * i;
        if d.get(rec..rec + 4) == Some(tag.as_slice()) {
            let off = u32_at(d, rec + 8).context("ttf: table offset")? as usize;
            let len = u32_at(d, rec + 12).context("ttf: table length")? as usize;
            return d.get(off..off.checked_add(len).context("ttf: table overflow")?).context("ttf: table out of range");
        }
    }
    bail!("ttf: no `{}` table", String::from_utf8_lossy(tag))
}

/// Reads the Unicode subtable (3,10 format 12 preferred, else 3,1 / 0,x format 4) into a map.
fn read_cmap(cmap: &[u8]) -> Result<BTreeMap<u32, u16>> {
    let n = u16_at(cmap, 2).context("cmap: count")? as usize;
    let mut best: Option<(u8, usize)> = None;
    for i in 0..n {
        let rec = 4 + 8 * i;
        let (platform, encoding) = (u16_at(cmap, rec).context("cmap rec")?, u16_at(cmap, rec + 2).context("cmap rec")?);
        let off = u32_at(cmap, rec + 4).context("cmap rec")? as usize;
        let format = u16_at(cmap, off).context("cmap subtable")?;
        let rank = match (platform, encoding, format) {
            (3, 10, 12) | (0, 4, 12) => 2,
            (3, 1, 4) | (0, _, 4) => 1,
            _ => continue,
        };
        if best.is_none_or(|(r, _)| rank > r) {
            best = Some((rank, off));
        }
    }
    let (_, off) = best.context("cmap: no Unicode subtable")?;
    let sub = cmap.get(off..).context("cmap subtable")?;
    let mut map = BTreeMap::new();
    match u16_at(sub, 0) {
        Some(12) => {
            let groups = u32_at(sub, 12).context("cmap12")? as usize;
            for g in 0..groups.min(1 << 20) {
                let at = 16 + 12 * g;
                let (start, end, gid) = (u32_at(sub, at), u32_at(sub, at + 4), u32_at(sub, at + 8));
                let (Some(start), Some(end), Some(gid)) = (start, end, gid) else { bail!("cmap12: truncated") };
                // Only the Basic Multilingual Plane is needed for the fixtures; bound the work.
                for c in start..=end.min(0xFFFF) {
                    let g = gid.saturating_add(c.saturating_sub(start));
                    map.insert(c, u16::try_from(g).unwrap_or(0));
                }
            }
        }
        Some(4) => {
            let seg_x2 = u16_at(sub, 6).context("cmap4")? as usize;
            let ends = 14;
            let starts = ends + seg_x2 + 2;
            let deltas = starts + seg_x2;
            let ranges = deltas + seg_x2;
            for s in 0..seg_x2 / 2 {
                let end = u16_at(sub, ends + 2 * s).context("cmap4 end")?;
                let start = u16_at(sub, starts + 2 * s).context("cmap4 start")?;
                let delta = u16_at(sub, deltas + 2 * s).context("cmap4 delta")?;
                let range_at = ranges + 2 * s;
                let range = u16_at(sub, range_at).context("cmap4 range")? as usize;
                for c in start..=end {
                    if c == 0xFFFF {
                        break;
                    }
                    let gid = if range == 0 {
                        c.wrapping_add(delta)
                    } else {
                        let at = range_at + range + 2 * (c - start) as usize;
                        match u16_at(sub, at) {
                            Some(0) | None => 0,
                            Some(g) => g.wrapping_add(delta),
                        }
                    };
                    if gid != 0 {
                        map.insert(u32::from(c), gid);
                    }
                }
            }
        }
        other => bail!("cmap: unsupported format {other:?}"),
    }
    Ok(map)
}

impl Ttf {
    pub fn parse(d: &[u8]) -> Result<Self> {
        let head = table(d, b"head")?;
        let hhea = table(d, b"hhea")?;
        let hmtx = table(d, b"hmtx")?;
        let units_per_em = u16_at(head, 18).filter(|u| *u > 0).context("head: unitsPerEm")?;
        let bbox = [i16_at(head, 36), i16_at(head, 38), i16_at(head, 40), i16_at(head, 42)];
        let [Some(x0), Some(y0), Some(x1), Some(y1)] = bbox else { bail!("head: bbox") };
        let n_metrics = u16_at(hhea, 34).context("hhea: metrics")? as usize;
        let advances = (0..n_metrics).map(|i| u16_at(hmtx, 4 * i).context("hmtx")).collect::<Result<Vec<_>>>()?;
        if advances.is_empty() {
            bail!("hmtx: no metrics");
        }
        Ok(Self {
            units_per_em,
            bbox: [x0, y0, x1, y1],
            ascent: i16_at(hhea, 4).context("hhea: ascent")?,
            descent: i16_at(hhea, 6).context("hhea: descent")?,
            advances,
            cmap: read_cmap(table(d, b"cmap")?)?,
        })
    }

    pub fn glyph(&self, c: char) -> Option<u16> {
        self.cmap.get(&u32::from(c)).copied().filter(|g| *g != 0)
    }

    /// A design-unit value in PDF glyph space (1000 per em).
    pub fn scale(&self, v: i32) -> i32 {
        v.saturating_mul(1000) / i32::from(self.units_per_em)
    }

    /// Horizontal advance of `gid` in 1000ths of an em (glyphs past the metrics use the last one).
    pub fn advance(&self, gid: u16) -> i32 {
        let adv = self.advances.get(gid as usize).or(self.advances.last()).copied().unwrap_or(0);
        self.scale(i32::from(adv))
    }
}

#[cfg(test)]
mod tests {
    use super::Ttf;

    #[test]
    fn reads_the_repository_fonts() {
        for name in ["Inter-Regular.ttf", "JetBrainsMono-Regular.ttf", "DancingScript.ttf"] {
            let bytes = std::fs::read(crate::gates::root().join("assets/fonts").join(name)).expect("committed font");
            let ttf = Ttf::parse(&bytes).expect(name);
            let a = ttf.glyph('A').expect("A is mapped");
            assert!((300..1200).contains(&ttf.advance(a)), "{name}: advance of A = {}", ttf.advance(a));
            assert!(ttf.glyph('é').is_some(), "{name}: é");
            assert!(ttf.ascent > 0 && ttf.descent < 0, "{name}");
        }
        let mono = Ttf::parse(&std::fs::read(crate::gates::root().join("assets/fonts/JetBrainsMono-Regular.ttf")).expect("font")).expect("mono");
        let (i, m) = (mono.glyph('i').expect("i"), mono.glyph('M').expect("M"));
        assert_eq!(mono.advance(i), mono.advance(m), "a monospace font has one advance");
    }

    #[test]
    fn truncated_fonts_are_errors_not_panics() {
        let bytes = std::fs::read(crate::gates::root().join("assets/fonts/Inter-Regular.ttf")).expect("font");
        for cut in [0, 3, 12, 40, 200, 1000] {
            assert!(Ttf::parse(&bytes[..cut]).is_err(), "cut at {cut}");
        }
    }
}
