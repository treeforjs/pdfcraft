//! What redaction needs from a font: how to split a string into character codes, each code's
//! advance width, and the glyph height (ascent, descent). Widths come from the font dictionary
//! (`/Widths`, `/W`, `/DW`, `/MissingWidth`); the standard 14 fonts without widths use
//! approximations (a slightly wrong width moves a glyph box by a fraction of a glyph, which the
//! overlap rule tolerates).

use std::collections::HashMap;

use printcraft_cos::{Dict, Document, Object};

/// How the bytes of a string map to codes.
#[derive(Clone, Debug, PartialEq)]
enum Codes {
    One,
    Two,
    /// Codespace ranges `(length, low, high)` from an embedded CMap.
    Ranges(Vec<(usize, Vec<u8>, Vec<u8>)>),
}

#[derive(Clone, Debug, PartialEq)]
enum Std14 {
    Helvetica,
    Times,
    Courier,
    Symbolic,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Metrics {
    codes: Codes,
    /// Simple fonts: `/FirstChar` and `/Widths`.
    first: u32,
    widths: Vec<f64>,
    /// Composite fonts: CID → width, and CID ranges with one width.
    cid_widths: HashMap<u32, f64>,
    cid_ranges: Vec<(u32, u32, f64)>,
    /// Code → CID for embedded non-identity CMaps (`cidrange` / `cidchar`).
    cid_map: Vec<(u32, u32, u32)>,
    default: f64,
    std14: Option<Std14>,
    /// Glyph units → text space (0.001, or `/FontMatrix[0]` for Type 3).
    pub scale: f64,
    /// Glyph box in text space per unit of font size.
    pub ascent: f64,
    pub descent: f64,
    pub composite: bool,
}

fn nums(doc: &Document, o: Option<&Object>) -> Vec<f64> {
    o.map(|o| doc.resolve(o))
        .and_then(|a| a.as_array().map(|a| a.iter().map(|x| doc.resolve(x).as_f64().unwrap_or(0.0)).collect()))
        .unwrap_or_default()
}

fn dict(doc: &Document, o: Option<&Object>) -> Option<Dict> {
    o.and_then(|o| doc.resolve(o).as_dict().cloned())
}

fn hex_bytes(tok: &str) -> Option<Vec<u8>> {
    let t = tok.trim_start_matches('<').trim_end_matches('>');
    if !t.len().is_multiple_of(2) || t.is_empty() {
        return None;
    }
    (0..t.len()).step_by(2).map(|i| u8::from_str_radix(&t[i..i + 2], 16).ok()).collect()
}

fn be(b: &[u8]) -> u32 {
    b.iter().fold(0u32, |a, x| (a << 8) | u32::from(*x))
}

/// Codespace ranges and CID mappings from an embedded CMap stream.
#[allow(clippy::type_complexity)]
fn parse_cmap(data: &[u8]) -> (Vec<(usize, Vec<u8>, Vec<u8>)>, Vec<(u32, u32, u32)>) {
    let text = String::from_utf8_lossy(data);
    let toks: Vec<&str> = text.split_whitespace().collect();
    let (mut spaces, mut cids) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < toks.len() {
        match toks[i] {
            "begincodespacerange" => {
                i += 1;
                while i + 1 < toks.len() && toks[i] != "endcodespacerange" {
                    if let (Some(lo), Some(hi)) = (hex_bytes(toks[i]), hex_bytes(toks[i + 1]))
                        && lo.len() == hi.len()
                    {
                        spaces.push((lo.len(), lo, hi));
                    }
                    i += 2;
                }
            }
            "begincidrange" => {
                i += 1;
                while i + 2 < toks.len() && toks[i] != "endcidrange" {
                    if let (Some(lo), Some(hi), Ok(c)) = (hex_bytes(toks[i]), hex_bytes(toks[i + 1]), toks[i + 2].parse::<u32>()) {
                        cids.push((be(&lo), be(&hi), c));
                    }
                    i += 3;
                }
            }
            "begincidchar" => {
                i += 1;
                while i + 1 < toks.len() && toks[i] != "endcidchar" {
                    if let (Some(code), Ok(c)) = (hex_bytes(toks[i]), toks[i + 1].parse::<u32>()) {
                        cids.push((be(&code), be(&code), c));
                    }
                    i += 2;
                }
            }
            _ => {}
        }
        i += 1;
    }
    spaces.sort_by_key(|s| s.0);
    (spaces, cids)
}

impl Metrics {
    /// Metrics for text without a usable font (Helvetica-like).
    pub fn fallback() -> Self {
        Metrics {
            codes: Codes::One,
            first: 0,
            widths: Vec::new(),
            cid_widths: HashMap::new(),
            cid_ranges: Vec::new(),
            cid_map: Vec::new(),
            default: 500.0,
            std14: Some(Std14::Helvetica),
            scale: 0.001,
            ascent: 0.9,
            descent: -0.25,
            composite: false,
        }
    }

    pub fn from_dict(doc: &Document, font: &Dict) -> Self {
        let mut m = Metrics::fallback();
        m.std14 = None;
        let subtype = font.name(b"Subtype").unwrap_or(b"Type1").to_vec();
        let descriptor;
        if subtype == b"Type0" {
            m.composite = true;
            m.default = 1000.0;
            m.codes = Codes::Two;
            if let Some(e) = font.get(b"Encoding").map(|e| doc.resolve(e))
                && let Object::Stream(s) = &*e
                && let Ok(data) = s.decoded()
            {
                let (spaces, cids) = parse_cmap(&data);
                if !spaces.is_empty() {
                    m.codes = Codes::Ranges(spaces);
                }
                m.cid_map = cids;
            }
            let desc = font
                .get(b"DescendantFonts")
                .map(|d| doc.resolve(d))
                .and_then(|a| a.as_array().and_then(|a| a.first().cloned()))
                .and_then(|d| doc.resolve(&d).as_dict().cloned())
                .unwrap_or_default();
            if let Some(dw) = desc.get(b"DW").and_then(|d| doc.resolve(d).as_f64()) {
                m.default = dw;
            }
            if let Some(w) = desc.get(b"W").map(|w| doc.resolve(w)).and_then(|w| w.as_array().cloned()) {
                let mut i = 0;
                while i < w.len() {
                    let Some(c0) = doc.resolve(&w[i]).as_f64().map(|v| v as u32) else { break };
                    match w.get(i + 1).map(|o| doc.resolve(o)) {
                        Some(a) if a.as_array().is_some() => {
                            for (k, x) in a.as_array().into_iter().flatten().enumerate() {
                                if let Some(v) = doc.resolve(x).as_f64() {
                                    m.cid_widths.insert(c0 + k as u32, v);
                                }
                            }
                            i += 2;
                        }
                        Some(c1) => {
                            if let (Some(c1), Some(v)) = (c1.as_f64(), w.get(i + 2).and_then(|x| doc.resolve(x).as_f64())) {
                                m.cid_ranges.push((c0, c1 as u32, v));
                            }
                            i += 3;
                        }
                        None => break,
                    }
                }
            }
            descriptor = dict(doc, desc.get(b"FontDescriptor"));
        } else {
            m.first = font.get(b"FirstChar").and_then(|f| doc.resolve(f).as_f64()).unwrap_or(0.0).max(0.0) as u32;
            m.widths = nums(doc, font.get(b"Widths"));
            descriptor = dict(doc, font.get(b"FontDescriptor"));
            m.default = descriptor.as_ref().and_then(|d| d.get(b"MissingWidth")).and_then(|w| doc.resolve(w).as_f64()).unwrap_or(0.0);
            if subtype == b"Type3" {
                let fm = nums(doc, font.get(b"FontMatrix"));
                if fm.len() == 6 {
                    m.scale = fm[0];
                    let bbox = nums(doc, font.get(b"FontBBox"));
                    if bbox.len() == 4 && fm[3] != 0.0 {
                        m.ascent = (bbox[3] * fm[3]).max(0.5);
                        m.descent = (bbox[1] * fm[3]).min(-0.1);
                    }
                }
                return m;
            }
            if m.widths.is_empty() {
                let base = String::from_utf8_lossy(font.name(b"BaseFont").unwrap_or(b"")).to_ascii_lowercase();
                m.std14 = Some(if base.contains("courier") {
                    Std14::Courier
                } else if base.contains("times") {
                    Std14::Times
                } else if base.contains("symbol") || base.contains("dingbats") {
                    Std14::Symbolic
                } else {
                    Std14::Helvetica
                });
            }
        }
        if let Some(d) = descriptor {
            let a = d.get(b"Ascent").and_then(|v| doc.resolve(v).as_f64()).unwrap_or(0.0) / 1000.0;
            let de = d.get(b"Descent").and_then(|v| doc.resolve(v).as_f64()).unwrap_or(0.0) / 1000.0;
            // Fonts often claim 0; never shrink the glyph box below a sensible minimum.
            m.ascent = if a > 0.3 { a.min(1.5) } else { 0.9 };
            m.descent = if de < -0.05 { de.max(-0.6) } else { -0.25 };
        }
        m
    }

    /// Split a string into `(code, byte length)`.
    pub fn codes(&self, s: &[u8]) -> Vec<(u32, usize)> {
        let mut out = Vec::with_capacity(s.len());
        let mut i = 0;
        while i < s.len() {
            let len = match &self.codes {
                Codes::One => 1,
                Codes::Two => 2,
                Codes::Ranges(r) => r
                    .iter()
                    .find(|(len, lo, hi)| s.get(i..i + len).is_some_and(|b| b.iter().zip(lo.iter().zip(hi)).all(|(x, (l, h))| x >= l && x <= h)))
                    .map_or(r.first().map_or(1, |x| x.0), |x| x.0),
            };
            let len = len.min(s.len() - i).max(1);
            out.push((be(&s[i..i + len]), len));
            i += len;
        }
        out
    }

    fn cid(&self, code: u32) -> u32 {
        if self.cid_map.is_empty() {
            return code;
        }
        self.cid_map.iter().find(|(lo, hi, _)| code >= *lo && code <= *hi).map_or(0, |(lo, _, c)| c + (code - lo))
    }

    /// The advance of `code` in text space per unit of font size.
    pub fn width(&self, code: u32) -> f64 {
        if self.composite {
            let cid = self.cid(code);
            let w = self.cid_widths.get(&cid).copied().or_else(|| self.cid_ranges.iter().find(|(a, b, _)| cid >= *a && cid <= *b).map(|r| r.2));
            return w.unwrap_or(self.default) * self.scale;
        }
        if let Some(w) = code.checked_sub(self.first).and_then(|i| self.widths.get(i as usize)) {
            return w * self.scale;
        }
        match &self.std14 {
            Some(Std14::Courier) => 0.6,
            Some(Std14::Symbolic) => 0.75,
            Some(f) => {
                let c = char::from_u32(code).filter(|c| !c.is_control()).unwrap_or('n');
                let w = printcraft_fonts::helvetica_width(&c.to_string(), 1.0);
                if *f == Std14::Times { w * 0.92 } else { w }
            }
            None => self.default * self.scale,
        }
    }

    /// Word spacing applies to the single-byte code 32 (§9.3.3).
    pub fn is_space(&self, code: u32, len: usize) -> bool {
        code == 32 && len == 1
    }
}
