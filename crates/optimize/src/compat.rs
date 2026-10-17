//! Make compatible with (Acrobat: PDF Optimizer ▸ Make compatible with): the copy is written as
//! a chosen PDF version. Object streams need PDF 1.5, so an older target writes a classic
//! cross-reference table ([`Settings::object_streams`](crate::Settings::object_streams)).
//! Features the target version doesn't define are found and reported (not converted):
//! transparency (1.4), JBIG2 (1.4), JPEG 2000 (1.5), 16-bit images (1.5), optional content
//! (1.5), AES encryption (1.6) and AES-256 (2.0).

use std::collections::BTreeMap;

use pdfcraft_cos::{Dict, Document, ObjRef, Object};

/// The versions a copy can be made compatible with, oldest first (Acrobat 4 to PDF 2.0).
pub const VERSIONS: [(u8, u8); 6] = [(1, 3), (1, 4), (1, 5), (1, 6), (1, 7), (2, 0)];

/// A feature the target version doesn't define: what it is, the version it needs, and how many
/// objects use it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewerFeature {
    pub feature: &'static str,
    pub needs: (u8, u8),
    pub count: usize,
}

/// `major.minor` as written in a header.
pub fn version_name((major, minor): (u8, u8)) -> String {
    format!("{major}.{minor}")
}

/// Parse `1.4` (or `PDF 1.4`) into one of [`VERSIONS`].
pub fn parse_version(s: &str) -> Option<(u8, u8)> {
    let s = s.trim();
    let s = s.strip_prefix("PDF").or_else(|| s.strip_prefix("pdf")).unwrap_or(s).trim();
    let (a, b) = s.split_once('.')?;
    let v = (a.parse().ok()?, b.parse().ok()?);
    VERSIONS.contains(&v).then_some(v)
}

/// Write the copy as version `target`: the header says so and a catalog `/Version` that says
/// otherwise is dropped.
pub(crate) fn apply(doc: &mut Document, target: (u8, u8)) -> Result<(), pdfcraft_cos::CosError> {
    doc.set_version(&version_name(target));
    if let Some(root) = doc.root()
        && doc.get(root).as_dict().is_some_and(|c| c.contains(b"Version"))
    {
        doc.update_dict(root, |c| {
            c.remove(b"Version");
        })?;
    }
    Ok(())
}

/// The features `doc` uses that `target` doesn't define, newest requirement last.
pub fn newer_features(doc: &Document, target: (u8, u8)) -> Vec<NewerFeature> {
    let mut found: BTreeMap<(&'static str, (u8, u8)), usize> = BTreeMap::new();
    let mut note = |feature: &'static str, needs: (u8, u8)| {
        if needs > target {
            *found.entry((feature, needs)).or_default() += 1;
        }
    };
    for num in doc.object_numbers() {
        let r = ObjRef { num, generation: doc.generation(num) };
        let obj = doc.get(r);
        match &*obj {
            Object::Stream(s) => {
                let d = &s.dict;
                for f in filters(doc, d) {
                    match f.as_slice() {
                        b"JPXDecode" => note("JPEG 2000 images", (1, 5)),
                        b"JBIG2Decode" => note("JBIG2 images", (1, 4)),
                        _ => {}
                    }
                }
                if d.name(b"Subtype") == Some(b"Image".as_slice()) {
                    if d.int(b"BitsPerComponent") == Some(16) {
                        note("16-bit images", (1, 5));
                    }
                    if d.get(b"SMask").and_then(Object::as_ref).is_some() || d.int(b"SMaskInData").is_some_and(|v| v != 0) {
                        note("transparency", (1, 4));
                    }
                }
                if d.name(b"Subtype") == Some(b"Form".as_slice()) && d.get(b"Group").is_some() {
                    note("transparency", (1, 4));
                }
            }
            Object::Dict(d) => {
                if (d.name(b"Type") == Some(b"ExtGState".as_slice()) || d.contains(b"BM") || d.contains(b"ca") || d.contains(b"CA"))
                    && transparent(doc, d)
                {
                    note("transparency", (1, 4));
                }
                if d.name(b"Type") == Some(b"Catalog".as_slice()) && d.contains(b"OCProperties") {
                    note("optional content (layers)", (1, 5));
                }
            }
            _ => {}
        }
    }
    if let Some(e) = doc.trailer().get(b"Encrypt").map(|e| doc.resolve(e))
        && let Some(e) = e.as_dict()
    {
        match e.int(b"V") {
            Some(5..) => note("AES-256 encryption", (2, 0)),
            Some(4) if uses_aes(doc, e) => note("AES encryption", (1, 6)),
            _ => {}
        }
    }
    let mut out: Vec<NewerFeature> = found.into_iter().map(|((feature, needs), count)| NewerFeature { feature, needs, count }).collect();
    out.sort_by(|a, b| a.needs.cmp(&b.needs).then(a.feature.cmp(b.feature)));
    out
}

/// A stream's filter names.
fn filters(doc: &Document, d: &Dict) -> Vec<Vec<u8>> {
    let Some(f) = d.get(b"Filter").map(|f| doc.resolve(f)) else { return Vec::new() };
    match &*f {
        Object::Name(n) => vec![n.clone()],
        Object::Array(a) => a.iter().filter_map(|x| doc.resolve(x).as_name().map(<[u8]>::to_vec)).collect(),
        _ => Vec::new(),
    }
}

/// Does a graphics state dictionary make anything transparent (a soft mask, a blend mode other
/// than Normal, or an alpha below 1)?
fn transparent(doc: &Document, d: &Dict) -> bool {
    let soft_mask = d.get(b"SMask").map(|m| doc.resolve(m)).is_some_and(|m| m.as_name() != Some(b"None".as_slice()));
    let blend = d.get(b"BM").map(|b| doc.resolve(b)).is_some_and(|b| {
        let normal = |n: &[u8]| n == b"Normal" || n == b"Compatible";
        match &*b {
            Object::Name(n) => !normal(n),
            Object::Array(a) => a.iter().any(|x| x.as_name().is_some_and(|n| !normal(n))),
            _ => false,
        }
    });
    let alpha = [b"ca".as_slice(), b"CA"].iter().any(|k| d.get(k).map(|v| doc.resolve(v)).and_then(|v| v.as_f64()).is_some_and(|v| v < 1.0));
    soft_mask || blend || alpha
}

/// Does a version 4 security handler use AES crypt filters?
fn uses_aes(doc: &Document, e: &Dict) -> bool {
    let Some(cf) = e.get(b"CF").map(|c| doc.resolve(c)) else { return false };
    let Some(cf) = cf.as_dict() else { return false };
    cf.iter().any(|(_, f)| doc.resolve(f).as_dict().is_some_and(|f| f.name(b"CFM") == Some(b"AESV2".as_slice())))
}
