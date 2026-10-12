//! PDF Optimizer ▸ Fonts: list the embedded fonts and unembed the ones that can safely do
//! without their font program.
//!
//! Unembedding removes `/FontFile`, `/FontFile2` or `/FontFile3` from a font's descriptor; the
//! viewer then draws the text with an installed or substitute font of that name, using the
//! widths and encoding the font dictionary still records. So it is offered only for **simple,
//! complete, non-symbolic fonts with an explicit standard-based encoding and a full widths
//! array**: character codes keep their meaning and text keeps its layout. Everything else is
//! refused with the reason:
//!
//! - composite (Type 0) fonts: codes select glyphs by number inside the embedded program;
//! - subsets (`ABCDEF+Name`): glyph choice and encoding are specific to the subset;
//! - symbolic fonts: glyphs are chosen through the program's own built-in encoding;
//! - no `/Encoding`, or one based on a non-standard encoding: the program supplies the mapping;
//! - no complete `/Widths`: text would reflow with the substitute's metrics;
//! - documents that claim PDF/A or PDF/X conformance, which require embedded fonts.
//!
//! Type 3 fonts have no program to remove and are not listed. The glyph shapes shown afterwards
//! depend on the fonts installed where the file is viewed; that is the point of unembedding.

use std::collections::HashMap;

use pdfcraft_cos::{Dict, Document, ObjRef, Object};

/// Why a font can't be unembedded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnembedBlock {
    /// The document claims PDF/A or PDF/X conformance, which requires embedded fonts.
    Conformance,
    /// A composite (Type 0 / CID) font.
    Composite,
    /// A subset font.
    Subset,
    /// The font is flagged symbolic.
    Symbolic,
    /// No explicit encoding based on a standard one.
    Encoding,
    /// No complete widths array.
    Widths,
}

impl UnembedBlock {
    pub fn reason(self) -> &'static str {
        match self {
            UnembedBlock::Conformance => "the document claims PDF/A or PDF/X conformance, which requires embedded fonts",
            UnembedBlock::Composite => "composite (Type 0) fonts select glyphs by number inside the embedded font program",
            UnembedBlock::Subset => "subset fonts contain only some glyphs, chosen and encoded for this document",
            UnembedBlock::Symbolic => "symbolic fonts choose glyphs through the embedded program's own encoding",
            UnembedBlock::Encoding => "the font has no explicit standard-based encoding; its program supplies the character mapping",
            UnembedBlock::Widths => "the font has no complete widths array, so text would reflow with another font's metrics",
        }
    }
}

/// One embedded font (a font dictionary whose descriptor holds a font program).
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddedFont {
    /// The font dictionary.
    pub font: ObjRef,
    /// `/BaseFont`.
    pub name: String,
    /// `/Subtype`: Type1, MMType1, TrueType or Type0.
    pub subtype: String,
    /// The program's format: FontFile (Type 1), FontFile2 (TrueType) or FontFile3 (CFF/OpenType).
    pub program: &'static str,
    /// Stored (compressed) size of the font program.
    pub program_bytes: u64,
    pub subset: bool,
    /// Why it can't be unembedded (`None`: it can).
    pub blocked: Option<UnembedBlock>,
}

const PROGRAMS: [&str; 3] = ["FontFile", "FontFile2", "FontFile3"];
/// Decoded metadata larger than this is not searched for conformance claims.
const MAX_XMP: usize = 4 << 20;

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn is_subset_name(name: &[u8]) -> bool {
    name.len() > 7 && name.get(6) == Some(&b'+') && name.get(..6).is_some_and(|p| p.iter().all(u8::is_ascii_uppercase))
}

/// Does the document claim PDF/A or PDF/X conformance (XMP identification or the PDF/X info key)?
pub fn claims_conformance_requiring_embedding(doc: &Document) -> bool {
    let Some(cat) = doc.root().and_then(|r| doc.get(r).as_dict().cloned()) else { return false };
    let info = doc.trailer().get(b"Info").and_then(|i| doc.dict(i));
    if info.is_some_and(|i| i.contains(b"GTS_PDFXVersion")) {
        return true;
    }
    let Some(m) = cat.get(b"Metadata") else { return false };
    let Object::Stream(s) = &*doc.resolve(m) else { return false };
    let Ok(xmp) = s.decoded_within(MAX_XMP) else { return false };
    let has = |n: &[u8]| xmp.windows(n.len()).any(|w| w == n);
    has(b"pdfaid:part") || has(b"GTS_PDFXVersion")
}

/// The font program a descriptor holds, with its key.
fn program_of(doc: &Document, descriptor: &Dict) -> Option<(&'static str, u64)> {
    PROGRAMS.iter().find_map(|k| {
        let v = descriptor.get(k.as_bytes())?;
        let size = match &*doc.resolve(v) {
            Object::Stream(s) => s.raw.len() as u64,
            _ => return None,
        };
        Some((*k, size))
    })
}

fn standard_encoding(name: &[u8], type1: bool) -> bool {
    matches!(name, b"WinAnsiEncoding" | b"MacRomanEncoding") || (type1 && name == b"StandardEncoding")
}

/// Why the simple font `f` (with `descriptor`) can't be unembedded, if it can't.
fn simple_block(doc: &Document, f: &Dict, descriptor: &Dict, name: &[u8]) -> Option<UnembedBlock> {
    if is_subset_name(name) {
        return Some(UnembedBlock::Subset);
    }
    if descriptor.int(b"Flags").is_some_and(|fl| fl & 4 != 0) {
        return Some(UnembedBlock::Symbolic);
    }
    let type1 = matches!(f.name(b"Subtype"), Some(b"Type1" | b"MMType1"));
    let encoding_ok = match f.get(b"Encoding").map(|e| doc.resolve(e)) {
        Some(e) => match &*e {
            Object::Name(n) => standard_encoding(n, type1),
            // Differences on top of a standard base (an absent base is the standard encoding
            // only for non-symbolic fonts, which this one is).
            Object::Dict(d) => d.name(b"BaseEncoding").is_none_or(|b| standard_encoding(b, type1)),
            _ => false,
        },
        None => false,
    };
    if !encoding_ok {
        return Some(UnembedBlock::Encoding);
    }
    let first = f.get(b"FirstChar").and_then(|x| doc.resolve(x).as_int());
    let last = f.get(b"LastChar").and_then(|x| doc.resolve(x).as_int());
    let widths = f.get(b"Widths").and_then(|w| doc.resolve(w).as_array().map(Vec::len));
    match (first, last, widths) {
        (Some(a), Some(b), Some(n)) if (0..=255).contains(&a) && (a..=255).contains(&b) && usize::try_from(b - a + 1).ok() == Some(n) => None,
        _ => Some(UnembedBlock::Widths),
    }
}

/// Every embedded font in the document (font dictionaries stored as their own objects), in
/// object order.
pub fn embedded_fonts(doc: &Document) -> Vec<EmbeddedFont> {
    let conformance = claims_conformance_requiring_embedding(doc);
    let mut out = Vec::new();
    for (r, obj) in doc.scan_objects() {
        let Some(f) = obj.as_dict() else { continue };
        if f.name(b"Type") != Some(b"Font") {
            continue;
        }
        let subtype = f.name(b"Subtype").unwrap_or_default();
        let name = f.name(b"BaseFont").unwrap_or_default();
        let (descriptor, composite) = match subtype {
            b"Type1" | b"MMType1" | b"TrueType" => (f.get(b"FontDescriptor").and_then(|d| doc.dict(d)), false),
            b"Type0" => {
                let first = f.get(b"DescendantFonts").map(|d| doc.resolve(d)).and_then(|a| a.as_array().and_then(|a| a.first().cloned()));
                let cid = first.and_then(|c| doc.dict(&c));
                (cid.and_then(|c| c.get(b"FontDescriptor").and_then(|d| doc.dict(d))), true)
            }
            _ => continue,
        };
        let Some(descriptor) = descriptor else { continue };
        let Some((program, program_bytes)) = program_of(doc, &descriptor) else { continue };
        let blocked = if conformance {
            Some(UnembedBlock::Conformance)
        } else if composite {
            Some(UnembedBlock::Composite)
        } else {
            simple_block(doc, f, &descriptor, name)
        };
        out.push(EmbeddedFont { font: r, name: text(name), subtype: text(subtype), program, program_bytes, subset: is_subset_name(name), blocked });
    }
    out
}

/// What [`unembed`] could not do (nothing was changed).
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum UnembedError {
    #[error("no embedded font is named {0:?}")]
    NotFound(String),
    #[error("{name} can't be unembedded: {reason}")]
    Blocked { name: String, reason: &'static str },
    #[error(transparent)]
    Cos(#[from] pdfcraft_cos::CosError),
}

/// Unembed every embedded font whose `/BaseFont` is one of `names`. All-or-nothing: if any name
/// matches no embedded font, or any matching font can't safely be unembedded, nothing changes.
/// Returns how many font dictionaries were unembedded.
pub fn unembed(doc: &mut Document, names: &[String]) -> Result<usize, UnembedError> {
    if names.is_empty() {
        return Ok(0);
    }
    let all = embedded_fonts(doc);
    let mut chosen: Vec<ObjRef> = Vec::new();
    for n in names {
        let hits: Vec<&EmbeddedFont> = all.iter().filter(|f| &f.name == n).collect();
        if hits.is_empty() {
            return Err(UnembedError::NotFound(n.clone()));
        }
        for f in hits {
            if let Some(b) = f.blocked {
                return Err(UnembedError::Blocked { name: f.name.clone(), reason: b.reason() });
            }
            if !chosen.contains(&f.font) {
                chosen.push(f.font);
            }
        }
    }
    // Descriptors shared with fonts that keep their program are copied, not edited.
    let mut users: HashMap<ObjRef, Vec<ObjRef>> = HashMap::new();
    for (r, obj) in doc.scan_objects() {
        if let Some(d) = obj.as_dict().filter(|d| d.name(b"Type") == Some(b"Font"))
            && let Some(Object::Ref(fd)) = d.get(b"FontDescriptor")
        {
            users.entry(*fd).or_default().push(r);
        }
    }
    let strip = |d: &mut Dict| {
        for k in PROGRAMS {
            d.remove(k.as_bytes());
        }
    };
    for font in &chosen {
        let fd = doc.get(*font).as_dict().and_then(|d| d.get(b"FontDescriptor").cloned());
        match fd {
            Some(Object::Ref(fd)) => {
                let shared = users.get(&fd).is_some_and(|u| u.iter().any(|x| !chosen.contains(x)));
                if shared {
                    let Some(mut copy) = doc.get(fd).as_dict().cloned() else { continue };
                    strip(&mut copy);
                    let new = doc.add(Object::Dict(copy));
                    doc.update_dict(*font, |d| d.set(b"FontDescriptor".to_vec(), Object::Ref(new)))?;
                } else {
                    doc.update_dict(fd, strip)?;
                }
            }
            Some(Object::Dict(_)) => doc.update_dict(*font, |d| {
                if let Some(Object::Dict(fd)) = d.get_mut(b"FontDescriptor") {
                    strip(fd);
                }
            })?,
            _ => {}
        }
    }
    Ok(chosen.len())
}
