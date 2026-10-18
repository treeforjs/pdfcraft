//! Convert to PDF/X (Acrobat: Preflight ▸ Convert to PDF/X): the document-level fixes.
//!
//! The whole conversion runs in the engine (`pdfcraft-engine`, `Session::pdfx_convert`), which
//! also needs the annotation flattener and Convert Colors from `pdfcraft-edit`. It is, in order:
//!
//! 1. [`pdfx_prepare`]: encryption removed; JavaScript (document-level, actions anywhere) and
//!    additional actions removed; annotations that don't print removed (links stay, with their
//!    print flag cleared), and printing annotations without an appearance removed; transfer
//!    functions and halftones outside the allowed types removed from graphics states; OPI
//!    dictionaries removed; unembedded standard-14 fonts embedded (the substitute programs the
//!    renderer draws them with, [`pdfcraft_render::standard_font_program`], with widths from
//!    the standard metrics); for PDF/X-4, optional content configurations named and without AS.
//! 2. (engine) printing annotations and form fields flattened into the page content.
//! 3. (engine, PDF/X-1a and PDF/X-3) transparency flattened ([`crate::flatten_transparency`]).
//! 4. (engine) colour converted to the output intent: for PDF/X-1a every colour that isn't
//!    CMYK, gray or spot; for PDF/X-3 and PDF/X-4 DeviceRGB when the intent isn't RGB (see
//!    [`XConvertOptions::colour_plan`]).
//! 5. [`pdfx_finish`]: the form (`AcroForm`) removed, the one `GTS_PDFX` output intent, TrimBox
//!    and BleedBox on every page, the PDF version, the document information (GTS_PDFXVersion,
//!    GTS_PDFXConformance for X-1a:2001, Title, CreationDate, ModDate, Trapped), the XMP
//!    metadata for PDF/X-4 and the file identifier.
//!
//! Every step changes only what needs changing, so converting a converted document again
//! changes nothing. What can't be fixed here (fonts other than the standard 14, PostScript,
//! reference XObjects, external streams, images Convert Colors can't decode, …) stays, and
//! [`crate::verify_pdfx`] reports it afterwards.

use std::collections::HashSet;
use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::flatten::FlattenSettings;
use crate::pdfx::XPart;

/// Largest output profile accepted.
pub const MAX_PROFILE_BYTES: usize = 64 << 20;
/// The registry a printing condition without a profile is looked up in.
pub const ICC_REGISTRY: &str = "http://www.color.org";
const MAX_OBJECTS: usize = 2_000_000;
const MAX_ARRAY: usize = 10_000;
const MAX_TEXT: usize = 256;

/// The output intent the conversion gives the document.
#[derive(Clone, Debug, PartialEq)]
pub struct XIntent {
    /// `OutputConditionIdentifier`: a registered printing condition (`FOGRA39`, `CGATS TR 001`,
    /// …) or a name for the profile's condition.
    pub condition: String,
    /// `Info`: a human-readable description (default: the condition).
    pub info: Option<String>,
    /// The ICC output profile (`DestOutputProfile`). Without one the condition must be
    /// registered (`RegistryName`); PDF/X-4 needs the profile.
    pub profile: Option<Arc<Vec<u8>>>,
}

/// The ICC header fields the conversion looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IccHeader {
    pub class: [u8; 4],
    pub space: [u8; 4],
    pub major: u8,
}

impl IccHeader {
    pub fn read(bytes: &[u8]) -> Result<IccHeader, String> {
        if bytes.len() > MAX_PROFILE_BYTES {
            return Err(format!("the profile is larger than {} MB", MAX_PROFILE_BYTES >> 20));
        }
        let (Some(class), Some(space), Some(major)) =
            (bytes.get(12..16).and_then(|b| <[u8; 4]>::try_from(b).ok()), bytes.get(16..20).and_then(|b| <[u8; 4]>::try_from(b).ok()), bytes.get(8))
        else {
            return Err("the profile is too short to be an ICC profile".into());
        };
        if bytes.get(36..40) != Some(b"acsp".as_slice()) {
            return Err("the profile has no ICC signature (acsp)".into());
        }
        Ok(IccHeader { class, space, major: *major })
    }

    /// Colour components of the profile's space (CMYK, gray or RGB only).
    pub fn components(&self) -> Option<i64> {
        match &self.space {
            b"CMYK" => Some(4),
            b"GRAY" => Some(1),
            b"RGB " => Some(3),
            _ => None,
        }
    }
}

/// What to convert to.
#[derive(Clone, Debug, PartialEq)]
pub struct XConvertOptions {
    pub part: XPart,
    /// The output intent; `None` keeps the document's own `GTS_PDFX` intent.
    pub intent: Option<XIntent>,
    /// TrimBox for every page, in default user space (default: the page's TrimBox, ArtBox,
    /// CropBox or MediaBox, in that order).
    pub trim: Option<[f64; 4]>,
    /// BleedBox for every page (default: the page's BleedBox, else its CropBox or MediaBox),
    /// always made to contain the TrimBox and to lie inside the MediaBox.
    pub bleed: Option<[f64; 4]>,
    /// The title written when the document has none (default: "Untitled").
    pub title: Option<String>,
    /// How transparency is flattened for PDF/X-1a and PDF/X-3.
    pub flatten: FlattenSettings,
    /// Convert even when the result won't conform (what remains is reported).
    pub allow_partial: bool,
}

impl XConvertOptions {
    pub fn new(part: XPart) -> Self {
        Self { part, intent: None, trim: None, bleed: None, title: None, flatten: FlattenSettings::default(), allow_partial: false }
    }

    /// Check the options against `doc` before anything changes.
    pub fn validate(&self, doc: &Document) -> Result<(), String> {
        let line = |what: &str, s: &str| -> Result<(), String> {
            if s.trim().is_empty() || s.chars().count() > MAX_TEXT || s.chars().any(char::is_control) {
                Err(format!("{what} must be one line of 1 to {MAX_TEXT} characters"))
            } else {
                Ok(())
            }
        };
        if let Some(t) = &self.title {
            line("the title", t)?;
        }
        for (what, r) in [("trim", self.trim), ("bleed", self.bleed)] {
            if let Some(r) = r
                && !(r.iter().all(|v| v.is_finite() && v.abs() < 1.0e7) && r[2] - r[0] > 0.0 && r[3] - r[1] > 0.0)
            {
                return Err(format!("{what} must be [left, bottom, right, top] in points with right > left and top > bottom"));
            }
        }
        if let (Some(t), Some(b)) = (self.trim, self.bleed)
            && !inside(b, t)
        {
            return Err("the bleed box must contain the trim box".into());
        }
        self.flatten.validate()?;
        match &self.intent {
            Some(i) => {
                line("output_condition", &i.condition)?;
                if let Some(info) = &i.info {
                    line("the output condition's description", info)?;
                }
                match &i.profile {
                    Some(p) => {
                        let h = IccHeader::read(p).map_err(|e| format!("profile: {e}"))?;
                        if &h.class != b"prtr" {
                            return Err(format!(
                                "profile: a PDF/X output intent needs an output (printer, prtr) profile, not a {:?} profile",
                                String::from_utf8_lossy(&h.class)
                            ));
                        }
                        if h.components().is_none() {
                            return Err(format!("profile: its colour space {:?} isn't CMYK, gray or RGB", String::from_utf8_lossy(&h.space)));
                        }
                        if self.part.is_x1a() && &h.space == b"RGB " {
                            return Err("profile: PDF/X-1a needs a CMYK or gray printing condition, not RGB".into());
                        }
                        let max = if self.part == XPart::X4 { 4 } else { 2 };
                        if h.major > max {
                            return Err(format!("profile: ICC version {} is newer than {} allows (version {max})", h.major, self.part.label()));
                        }
                    }
                    None if self.part == XPart::X4 => {
                        return Err("PDF/X-4 embeds the output profile: give profile (an ICC output profile), not only output_condition".into());
                    }
                    None => {}
                }
            }
            None => {
                if existing_intents(doc).is_empty() {
                    return Err(
                        "the document has no PDF/X output intent: give output_condition (a registered printing condition such as FOGRA39) or profile"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    /// The colour space the output intent stands for (`CMYK`, `GRAY`, `RGB `): the profile's,
    /// CMYK for a registered condition without one (the registry's conditions are CMYK print).
    pub fn intent_space(&self, doc: &Document) -> [u8; 4] {
        let from = |p: &[u8]| IccHeader::read(p).ok().filter(|h| h.components().is_some()).map(|h| h.space);
        match &self.intent {
            Some(i) => i.profile.as_deref().and_then(|p| from(p)).unwrap_or(*b"CMYK"),
            None => existing_intents(doc)
                .first()
                .and_then(|(_, d)| d.get(b"DestOutputProfile").map(|p| doc.resolve(p)))
                .and_then(|p| match &*p {
                    Object::Stream(s) => s.decoded_within(MAX_PROFILE_BYTES).ok().and_then(|b| from(&b)),
                    _ => None,
                })
                .unwrap_or(*b"CMYK"),
        }
    }

    /// The output profile's bytes (given, or the document's own intent's).
    pub fn intent_profile(&self, doc: &Document) -> Option<Arc<Vec<u8>>> {
        match &self.intent {
            Some(i) => i.profile.clone(),
            None => existing_intents(doc).first().and_then(|(_, d)| d.get(b"DestOutputProfile").map(|p| doc.resolve(p))).and_then(|p| match &*p {
                Object::Stream(s) => s.decoded_within(MAX_PROFILE_BYTES).ok().map(Arc::new),
                _ => None,
            }),
        }
    }

    /// Which colour conversion the part needs: the source spaces to convert (names as Convert
    /// Colors takes them) into the intent's space. Empty when none is needed.
    pub fn colour_plan(&self, doc: &Document) -> Vec<&'static str> {
        let space = self.intent_space(doc);
        if self.part.is_x1a() {
            // CMYK, gray and spot colour stay (PDF/X-1a allows them, whatever the intent).
            vec!["DeviceRGB", "ICCBased", "CalGray", "CalRGB", "Lab"]
        } else if &space == b"RGB " {
            Vec::new()
        } else {
            vec!["DeviceRGB"]
        }
    }
}

impl XPart {
    /// The part for a conversion level: `x1a` (PDF/X-1a:2003), `x3` (PDF/X-3:2003), `x4`, or
    /// any id [`XPart::from_id`] takes (`X-1a:2001`, `PDF/X-3:2002`, …).
    pub fn from_level(level: &str) -> Option<XPart> {
        let s: String = level.chars().filter(|c| !c.is_whitespace() && *c != '-' && *c != '_').collect::<String>().to_ascii_lowercase();
        let s = s.strip_prefix("pdf/").or_else(|| s.strip_prefix("pdf")).unwrap_or(&s);
        match s {
            "x1a" => Some(XPart::X1a2003),
            "x3" => Some(XPart::X3_2003),
            "x4" => Some(XPart::X4),
            _ => XPart::from_id(level),
        }
    }

    /// Whether the part allows transparency (only PDF/X-4).
    pub fn allows_transparency(self) -> bool {
        self == XPart::X4
    }
}

fn inside(outer: [f64; 4], inner: [f64; 4]) -> bool {
    const EPS: f64 = 0.01;
    inner[0] >= outer[0] - EPS && inner[1] >= outer[1] - EPS && inner[2] <= outer[2] + EPS && inner[3] <= outer[3] + EPS
}

fn catalog(doc: &Document) -> Dict {
    doc.root().and_then(|r| doc.get(r).as_dict().cloned()).unwrap_or_default()
}

fn dict_at(doc: &Document, d: &Dict, key: &[u8]) -> Option<Dict> {
    d.get(key).map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned())
}

fn text(doc: &Document, d: &Dict, key: &[u8]) -> Option<String> {
    let v = doc.resolve(d.get(key)?);
    match &*v {
        Object::String(s) => Some(s.to_text()),
        Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
        _ => None,
    }
    .map(|s| s.trim().to_owned())
    .filter(|s| !s.is_empty())
}

/// The catalog's `GTS_PDFX` output intents.
fn existing_intents(doc: &Document) -> Vec<(Object, Dict)> {
    let cat = catalog(doc);
    cat.get(b"OutputIntents")
        .map(|o| doc.resolve(o))
        .and_then(|o| o.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .take(MAX_ARRAY)
        .filter_map(|o| doc.resolve(&o).as_dict().cloned().map(|d| (o, d)))
        .filter(|(_, d)| d.name(b"S") == Some(b"GTS_PDFX".as_slice()))
        .collect()
}

fn no_catalog() -> String {
    "the document has no catalog".into()
}

fn set_catalog(doc: &mut Document, cat: Dict) -> Result<(), String> {
    let root = doc.root().ok_or_else(no_catalog)?;
    doc.update_dict(root, |c| *c = cat).map_err(|e| e.to_string())
}

/// Every indirect object, at most [`MAX_OBJECTS`] (with whether the limit was reached).
fn objects(doc: &Document) -> (Vec<(ObjRef, Arc<Object>)>, bool) {
    let mut v: Vec<(ObjRef, Arc<Object>)> = doc.scan_objects().take(MAX_OBJECTS.saturating_add(1)).collect();
    let limited = v.len() > MAX_OBJECTS;
    v.truncate(MAX_OBJECTS);
    (v, limited)
}

fn is_js_action(doc: &Document, o: &Object) -> bool {
    doc.resolve(o).as_dict().is_some_and(|a| a.name(b"S") == Some(b"JavaScript".as_slice()) && a.name(b"Type").is_none_or(|t| t == b"Action"))
}

/// Whether a graphics state needs fixing; the fixed copy when it does.
fn fix_gstate(doc: &Document, g: &Dict) -> Option<Dict> {
    let mut n = g.clone();
    let mut changed = false;
    if n.get(b"TR").is_some_and(|tr| !matches!(&*doc.resolve(tr), Object::Name(x) if x == b"Identity")) {
        n.remove(b"TR");
        changed = true;
    }
    if n.get(b"TR2").is_some_and(|tr| !matches!(&*doc.resolve(tr), Object::Name(x) if x == b"Default" || x == b"Identity")) {
        n.remove(b"TR2");
        changed = true;
    }
    if let Some(ht) = n.get(b"HT").map(|h| doc.resolve(h))
        && let Some(h) = ht.as_dict()
        && !matches!(h.get(b"HalftoneType").and_then(|t| doc.resolve(t).as_int()), Some(1 | 5 | 6 | 10 | 16))
    {
        n.remove(b"HT");
        changed = true;
    }
    changed.then_some(n)
}

/// Fix the graphics states written inline in a resource dictionary (`d` is one, or a page or
/// form whose `/Resources` is inline).
fn fix_inline_gstates(doc: &Document, d: &mut Dict, depth: usize) -> usize {
    if depth > 2 {
        return 0;
    }
    let mut n = 0;
    if let Some(Object::Dict(res)) = d.get(b"Resources").cloned() {
        let mut res = res;
        n += fix_inline_gstates(doc, &mut res, depth + 1);
        if n > 0 {
            d.set(b"Resources".to_vec(), Object::Dict(res));
        }
    }
    if let Some(Object::Dict(states)) = d.get(b"ExtGState").cloned() {
        let mut states = states;
        let keys: Vec<Vec<u8>> = states.iter().take(MAX_ARRAY).map(|(k, _)| k.clone()).collect();
        let mut here = 0;
        for k in keys {
            if let Some(Object::Dict(g)) = states.get(&k)
                && let Some(fixed) = fix_gstate(doc, g)
            {
                states.set(k, Object::Dict(fixed));
                here += 1;
            }
        }
        if here > 0 {
            d.set(b"ExtGState".to_vec(), Object::Dict(states));
            n += here;
        }
    }
    n
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// An optional content configuration with a Name and no AS; whether it changed.
fn fix_config(doc: &Document, c: &mut Dict, fallback: &str) -> bool {
    let mut changed = c.remove(b"AS").is_some();
    if text(doc, c, b"Name").is_none() {
        c.set(b"Name".to_vec(), PdfString::text(fallback));
        changed = true;
    }
    changed
}

/// Step 1 of the conversion (see the module documentation). Returns what was fixed.
pub fn pdfx_prepare(doc: &mut Document, opts: &XConvertOptions) -> Result<Vec<String>, String> {
    let mut fixed = Vec::new();
    if doc.trailer().get(b"Encrypt").is_some() {
        doc.remove_encryption();
        fixed.push("Removed encryption".into());
    }
    // Catalog: additional actions, a JavaScript open action, document-level JavaScript.
    let mut cat = catalog(doc);
    let mut cat_changed = false;
    if cat.remove(b"AA").is_some() {
        fixed.push("Removed the document's additional actions".into());
        cat_changed = true;
    }
    if cat.get(b"OpenAction").is_some_and(|a| is_js_action(doc, a)) {
        cat.remove(b"OpenAction");
        fixed.push("Removed the JavaScript open action".into());
        cat_changed = true;
    }
    if let Some(mut names) = dict_at(doc, &cat, b"Names")
        && names.remove(b"JavaScript").is_some()
    {
        match cat.get(b"Names").and_then(Object::as_ref) {
            Some(r) => doc.set(r, Object::Dict(names)),
            None => cat.set(b"Names".to_vec(), Object::Dict(names)),
        }
        fixed.push("Removed document-level JavaScript".into());
        cat_changed = true;
    }
    if opts.part == XPart::X4
        && let Some(oc) = dict_at(doc, &cat, b"OCProperties")
    {
        let mut oc_new = oc.clone();
        let mut n = 0usize;
        if let Some(mut d) = dict_at(doc, &oc, b"D")
            && fix_config(doc, &mut d, "Default")
        {
            match oc.get(b"D").and_then(Object::as_ref) {
                Some(r) => doc.set(r, Object::Dict(d)),
                None => oc_new.set(b"D".to_vec(), Object::Dict(d)),
            }
            n += 1;
        }
        let configs = oc.get(b"Configs").map(|c| doc.resolve(c)).and_then(|c| c.as_array().cloned()).unwrap_or_default();
        let mut new_configs = configs.clone();
        let mut inline_changed = false;
        for (i, c) in configs.iter().take(MAX_ARRAY).enumerate() {
            let Some(mut d) = doc.resolve(c).as_dict().cloned() else { continue };
            if fix_config(doc, &mut d, &format!("Configuration {}", i + 1)) {
                match c.as_ref() {
                    Some(r) => doc.set(r, Object::Dict(d)),
                    None => {
                        if let Some(slot) = new_configs.get_mut(i) {
                            *slot = Object::Dict(d);
                            inline_changed = true;
                        }
                    }
                }
                n += 1;
            }
        }
        if inline_changed {
            oc_new.set(b"Configs".to_vec(), Object::Array(new_configs));
        }
        if n > 0 {
            match cat.get(b"OCProperties").and_then(Object::as_ref) {
                Some(r) if oc_new != oc => doc.set(r, Object::Dict(oc_new)),
                Some(_) => {}
                None => {
                    cat.set(b"OCProperties".to_vec(), Object::Dict(oc_new));
                    cat_changed = true;
                }
            }
            fixed.push(format!("Named {} and removed AS", plural(n, "optional content configuration", "optional content configurations")));
        }
    }
    if cat_changed {
        set_catalog(doc, cat)?;
    }

    // Annotations: what doesn't print goes (links stay, not printing); printing annotations
    // without an appearance draw nothing and go too.
    let (mut removed, mut links) = (0usize, 0usize);
    for p in pdfcraft_model::pages(doc) {
        let Some(annots_obj) = p.dict.get(b"Annots").cloned() else { continue };
        let annots = doc.resolve(&annots_obj).as_array().cloned().unwrap_or_default();
        let mut kept = Vec::with_capacity(annots.len());
        let mut changed = false;
        for a in annots.into_iter().take(MAX_ARRAY) {
            let Some(d) = doc.resolve(&a).as_dict().cloned() else {
                changed = true;
                continue;
            };
            let flags = d.get(b"F").and_then(|f| doc.resolve(f).as_int()).unwrap_or(0);
            let prints = flags & 4 != 0 && flags & 2 == 0;
            let sub = d.name(b"Subtype").map(<[u8]>::to_vec).unwrap_or_default();
            let has_ap = dict_at(doc, &d, b"AP").is_some_and(|ap| ap.contains(b"N"));
            if sub == b"Link" {
                if prints {
                    let mut nd = d.clone();
                    nd.set(b"F".to_vec(), Object::Int(flags & !4));
                    match a.as_ref() {
                        Some(r) => doc.set(r, Object::Dict(nd)),
                        None => {
                            kept.push(Object::Dict(nd));
                            changed = true;
                            links += 1;
                            continue;
                        }
                    }
                    links += 1;
                }
                kept.push(a);
                continue;
            }
            if !prints || !has_ap {
                removed += 1;
                changed = true;
                continue;
            }
            kept.push(a);
        }
        if changed {
            match annots_obj.as_ref() {
                Some(r) => doc.set(r, Object::Array(kept)),
                None => doc.update_dict(p.obj, |d| d.set(b"Annots".to_vec(), Object::Array(kept))).map_err(|e| e.to_string())?,
            }
        }
    }
    if removed > 0 {
        fixed.push(format!("Removed {} that don't print or have no appearance", plural(removed, "annotation", "annotations")));
    }
    if links > 0 {
        fixed.push(format!("Set {} not to print", plural(links, "link", "links")));
    }

    // Every object: actions, graphics states, OPI, standard fonts.
    let (objs, _) = objects(doc);
    let (mut aa, mut js, mut states, mut opi) = (0usize, 0usize, 0usize, 0usize);
    let mut js_actions: Vec<ObjRef> = Vec::new();
    for (r, o) in &objs {
        match &**o {
            Object::Dict(d) => {
                if d.name(b"S") == Some(b"JavaScript".as_slice()) && d.name(b"Type").is_none_or(|t| t == b"Action") && !d.contains(b"K") {
                    js_actions.push(*r);
                    continue;
                }
                let mut nd = d.clone();
                let mut changed = false;
                if nd.remove(b"AA").is_some() {
                    aa += 1;
                    changed = true;
                }
                if nd.get(b"A").is_some_and(|a| is_js_action(doc, a)) {
                    nd.remove(b"A");
                    js += 1;
                    changed = true;
                }
                let is_gstate =
                    nd.name(b"Type") == Some(b"ExtGState".as_slice()) || (nd.name(b"Type").is_none() && (nd.contains(b"TR") || nd.contains(b"TR2")));
                if is_gstate && let Some(g) = fix_gstate(doc, &nd) {
                    nd = g;
                    states += 1;
                    changed = true;
                }
                let inline = fix_inline_gstates(doc, &mut nd, 0);
                if inline > 0 {
                    states += inline;
                    changed = true;
                }
                if changed {
                    doc.set(*r, Object::Dict(nd));
                }
            }
            Object::Stream(s) => {
                let mut ns = s.clone();
                let mut changed = false;
                if matches!(ns.dict.name(b"Subtype"), Some(b"Image" | b"Form")) && ns.dict.remove(b"OPI").is_some() {
                    opi += 1;
                    changed = true;
                }
                let inline = fix_inline_gstates(doc, &mut ns.dict, 0);
                if inline > 0 {
                    states += inline;
                    changed = true;
                }
                if changed {
                    doc.set(*r, Object::Stream(ns));
                }
            }
            _ => {}
        }
    }
    for r in &js_actions {
        doc.set(*r, Object::Null);
    }
    js = js.saturating_add(js_actions.len());
    for (n, what) in [
        (aa, plural(aa, "set of additional actions", "sets of additional actions")),
        (js, plural(js, "JavaScript action", "JavaScript actions")),
        (states, plural(states, "graphics state's transfer function or halftone", "graphics states' transfer functions or halftones")),
        (opi, plural(opi, "OPI dictionary", "OPI dictionaries")),
    ] {
        if n > 0 {
            fixed.push(format!("Removed {what}"));
        }
    }
    let fonts = embed_standard_fonts(doc)?;
    if !fonts.is_empty() {
        fixed.push(format!("Embedded {}: {}", plural(fonts.len(), "standard font", "standard fonts"), fonts.join(", ")));
    }
    Ok(fixed)
}

const STANDARD_14: [&str; 14] = [
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Courier",
    "Courier-Bold",
    "Courier-Oblique",
    "Courier-BoldOblique",
    "Times-Roman",
    "Times-Bold",
    "Times-Italic",
    "Times-BoldItalic",
    "Symbol",
    "ZapfDingbats",
];

fn embedded(fd: Option<&Dict>) -> bool {
    fd.is_some_and(|fd| fd.contains(b"FontFile") || fd.contains(b"FontFile2") || fd.contains(b"FontFile3"))
}

/// Fonts embedded so far: name → program object, and descriptors already rewritten.
#[derive(Default)]
struct Embedder {
    programs: Vec<(String, ObjRef)>,
    descriptors: HashSet<ObjRef>,
    done: Vec<String>,
}

impl Embedder {
    /// The font with its program embedded, when it is an unembedded standard-14 Type 1 font.
    fn fix(&mut self, doc: &mut Document, font: &Dict) -> Option<Dict> {
        if font.name(b"Subtype") != Some(b"Type1".as_slice()) || font.name(b"Type").is_some_and(|t| t != b"Font") {
            return None;
        }
        let base = font.name(b"BaseFont").map(|b| String::from_utf8_lossy(b).into_owned())?;
        if !STANDARD_14.contains(&base.as_str()) {
            return None;
        }
        let fd_obj = font.get(b"FontDescriptor").cloned();
        let fd = fd_obj.as_ref().and_then(|f| doc.resolve(f).as_dict().cloned());
        if embedded(fd.as_ref()) {
            return None;
        }
        let file = match self.programs.iter().find(|(n, _)| *n == base) {
            Some((_, f)) => *f,
            None => {
                let program = pdfcraft_render::standard_font_program(&base)?;
                let mut d = Dict::new();
                d.set(b"Subtype".to_vec(), Object::name("Type1C"));
                let f = doc.add(Object::Stream(Stream::flate(d, &program)));
                self.programs.push((base.clone(), f));
                f
            }
        };
        let mut nf = font.clone();
        if !nf.contains(b"Widths") {
            let m = pdfcraft_fonts::pdf::Metrics::from_dict(doc, font);
            let widths: Vec<Object> = (0u32..=255).map(|c| Object::Int((m.width(c) * 1000.0).round().clamp(0.0, 10_000.0) as i64)).collect();
            nf.set(b"FirstChar".to_vec(), Object::Int(0));
            nf.set(b"LastChar".to_vec(), Object::Int(255));
            nf.set(b"Widths".to_vec(), Object::Array(widths));
        }
        let mut desc = fd.unwrap_or_default();
        fill_descriptor(&mut desc, &base);
        desc.set(b"FontFile3".to_vec(), Object::Ref(file));
        match fd_obj.as_ref().and_then(Object::as_ref) {
            Some(dr) => {
                if self.descriptors.insert(dr) {
                    doc.set(dr, Object::Dict(desc));
                }
            }
            None => {
                let dr = doc.add(Object::Dict(desc));
                nf.set(b"FontDescriptor".to_vec(), Object::Ref(dr));
            }
        }
        if !self.done.contains(&base) {
            self.done.push(base);
        }
        Some(nf)
    }

    /// Fonts written inline: the direct font dictionaries in `d` (a font map), in its `/Font`
    /// and in its inline `/Resources`' `/Font`. Whether `d` changed.
    fn fix_inline(&mut self, doc: &mut Document, d: &mut Dict, depth: usize, font_map: bool) -> bool {
        if depth > 2 {
            return false;
        }
        let mut changed = false;
        let keys: Vec<Vec<u8>> = d.iter().take(MAX_ARRAY).map(|(k, _)| k.clone()).collect();
        for k in keys {
            match d.get(&k).cloned() {
                Some(Object::Dict(f)) if font_map || f.name(b"Type") == Some(b"Font".as_slice()) || k == b"Font" || k == b"Resources" => {
                    if let Some(nf) = self.fix(doc, &f) {
                        d.set(k, Object::Dict(nf));
                        changed = true;
                    } else if k == b"Font" || k == b"Resources" {
                        let mut inner = f;
                        if self.fix_inline(doc, &mut inner, depth + 1, k == b"Font") {
                            d.set(k, Object::Dict(inner));
                            changed = true;
                        }
                    }
                }
                _ => {}
            }
        }
        changed
    }
}

/// Embed the substitute programs of standard-14 Type 1 fonts that aren't embedded, whether
/// they are objects of their own or written inline in resources. The font keeps its name; it
/// gets a complete descriptor and, when it had none, widths from the standard metrics (so text
/// keeps its layout). Returns the fonts' names.
fn embed_standard_fonts(doc: &mut Document) -> Result<Vec<String>, String> {
    let (objs, _) = objects(doc);
    let mut e = Embedder::default();
    for (r, o) in objs {
        match &*o {
            Object::Dict(d) => {
                if let Some(nf) = e.fix(doc, d) {
                    doc.set(r, Object::Dict(nf));
                    continue;
                }
                let mut nd = d.clone();
                if e.fix_inline(doc, &mut nd, 0, false) {
                    doc.set(r, Object::Dict(nd));
                }
            }
            Object::Stream(s) => {
                let mut ns = s.clone();
                if e.fix_inline(doc, &mut ns.dict, 0, false) {
                    doc.set(r, Object::Stream(ns));
                }
            }
            _ => {}
        }
    }
    Ok(e.done)
}

/// The descriptor entries a standard font's program needs (kept where present). The metrics are
/// round approximations by family, not the fonts' own values.
fn fill_descriptor(d: &mut Dict, base: &str) {
    let symbolic = base == "Symbol" || base == "ZapfDingbats";
    let italic = base.contains("Oblique") || base.contains("Italic");
    let bold = base.contains("Bold");
    let fixed = base.starts_with("Courier");
    let serif = base.starts_with("Times");
    let mut flags = if symbolic { 4 } else { 32 };
    if fixed {
        flags |= 1;
    }
    if serif {
        flags |= 2;
    }
    if italic {
        flags |= 64;
    }
    let set = |d: &mut Dict, k: &[u8], v: Object| {
        if !d.contains(k) {
            d.set(k.to_vec(), v);
        }
    };
    d.set(b"Type".to_vec(), Object::name("FontDescriptor"));
    set(d, b"FontName", Object::name(base));
    set(d, b"Flags", Object::Int(flags));
    set(d, b"FontBBox", Object::Array([-200, -300, 1200, 1000].into_iter().map(Object::Int).collect()));
    set(d, b"ItalicAngle", Object::Int(if italic { -12 } else { 0 }));
    set(d, b"Ascent", Object::Int(750));
    set(d, b"Descent", Object::Int(-250));
    set(d, b"CapHeight", Object::Int(700));
    set(d, b"StemV", Object::Int(if bold { 140 } else { 80 }));
}

/// Step 5 of the conversion (see the module documentation). `date` is a PDF date
/// (`D:YYYYMMDDHHmmSSZ`) for dates the document lacks. Returns what was fixed.
pub fn pdfx_finish(doc: &mut Document, opts: &XConvertOptions, date: &str) -> Result<Vec<String>, String> {
    let mut fixed = Vec::new();
    let part = opts.part;
    let mut cat = catalog(doc);
    if cat.contains(b"AcroForm") {
        cat.remove(b"AcroForm");
        fixed.push("Removed the interactive form (its fields were flattened)".into());
    }
    // The output intent.
    let list: Vec<Object> = cat.get(b"OutputIntents").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()).unwrap_or_default();
    let is_x = |o: &Object| doc.resolve(o).as_dict().is_some_and(|d| d.name(b"S") == Some(b"GTS_PDFX".as_slice()));
    let others: Vec<Object> = list.iter().filter(|o| !is_x(o)).take(MAX_ARRAY).cloned().collect();
    let current: Vec<Object> = list.iter().filter(|o| is_x(o)).cloned().collect();
    let new_intent = match &opts.intent {
        Some(i) => {
            let same =
                current.len() == 1 && current.first().and_then(|o| doc.resolve(o).as_dict().cloned()).is_some_and(|d| intent_matches(doc, &d, i));
            if same { None } else { Some(Object::Dict(intent_dict(doc, i))) }
        }
        None => {
            let first = current.first().cloned();
            let mut d = first.as_ref().and_then(|o| doc.resolve(o).as_dict().cloned()).unwrap_or_default();
            let mut changed = current.len() > 1;
            if text(doc, &d, b"OutputConditionIdentifier").is_none() {
                let id = text(doc, &d, b"Info").unwrap_or_else(|| "Custom".into());
                d.set(b"OutputConditionIdentifier".to_vec(), PdfString::text(&id));
                changed = true;
            }
            match (changed, first.as_ref().and_then(Object::as_ref)) {
                (false, _) => None,
                (true, Some(r)) => {
                    doc.set(r, Object::Dict(d));
                    Some(Object::Ref(r))
                }
                (true, None) => Some(Object::Dict(d)),
            }
        }
    };
    if let Some(oi) = new_intent {
        let mut l = vec![oi];
        l.extend(others);
        cat.set(b"OutputIntents".to_vec(), Object::Array(l));
        fixed.push(match &opts.intent {
            Some(i) => format!("Set the PDF/X output intent to {}", i.condition),
            None => "Kept one PDF/X output intent, with its condition identifier".into(),
        });
    }
    // The PDF version: the catalog's may only raise it.
    let max = part.max_pdf_version();
    let max_text = format!("{}.{}", max.0, max.1);
    let version_of = |v: &str| -> Option<(u8, u8)> {
        let (a, b) = v.trim().split_once('.')?;
        let b: String = b.chars().take_while(char::is_ascii_digit).collect();
        Some((a.trim().parse().ok()?, b.parse().ok()?))
    };
    if cat.name(b"Version").and_then(|v| version_of(&String::from_utf8_lossy(v))).is_some_and(|v| v > max) {
        cat.remove(b"Version");
        fixed.push("Removed the catalog's PDF version".into());
    }
    set_catalog(doc, cat)?;
    if version_of(doc.version()).is_none_or(|v| v > max) {
        let old = doc.version().to_owned();
        doc.set_version(&max_text);
        doc.require_full_save();
        fixed.push(format!("Wrote the file as PDF {max_text} (was {old}); it is saved in full"));
    }
    // Page boxes.
    let boxes = page_boxes(doc, opts)?;
    if boxes > 0 {
        fixed.push(format!("Set the trim and bleed boxes of {}", plural(boxes, "page", "pages")));
    }
    // Document information.
    fixed.extend(document_info(doc, opts, date)?);
    if part == XPart::X4 {
        fixed.extend(xmp(doc, opts, date)?);
    }
    let id_ok = doc.trailer().get(b"ID").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()).is_some_and(|a| a.len() == 2);
    if !id_ok {
        let seed = format!("{date}{}{}", doc.object_numbers().len(), doc.bytes().len());
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut bytes = Vec::with_capacity(16);
        for round in 0u8..2 {
            for b in seed.bytes().chain([round]) {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
            bytes.extend(h.to_be_bytes());
        }
        let s = Object::String(PdfString { bytes, hex: true });
        doc.trailer_mut().set(b"ID".to_vec(), Object::Array(vec![s.clone(), s]));
        fixed.push("Added a file identifier".into());
    }
    Ok(fixed)
}

fn intent_matches(doc: &Document, d: &Dict, i: &XIntent) -> bool {
    if text(doc, d, b"OutputConditionIdentifier").as_deref() != Some(i.condition.trim()) {
        return false;
    }
    match (&i.profile, d.get(b"DestOutputProfile").map(|p| doc.resolve(p))) {
        (Some(p), Some(s)) => matches!(&*s, Object::Stream(s) if s.decoded_within(MAX_PROFILE_BYTES).ok().as_deref() == Some(p.as_slice())),
        (None, None) => text(doc, d, b"RegistryName").is_some(),
        _ => false,
    }
}

fn intent_dict(doc: &mut Document, i: &XIntent) -> Dict {
    let mut oi = Dict::new();
    oi.set(b"Type".to_vec(), Object::name("OutputIntent"));
    oi.set(b"S".to_vec(), Object::name("GTS_PDFX"));
    oi.set(b"OutputConditionIdentifier".to_vec(), PdfString::text(i.condition.trim()));
    oi.set(b"Info".to_vec(), PdfString::text(i.info.as_deref().unwrap_or(&i.condition).trim()));
    oi.set(b"RegistryName".to_vec(), PdfString::text(ICC_REGISTRY));
    if let Some(p) = &i.profile {
        let n = IccHeader::read(p).ok().and_then(|h| h.components()).unwrap_or(4);
        let mut d = Dict::new();
        d.set(b"N".to_vec(), Object::Int(n));
        let r = doc.add(Object::Stream(Stream::flate(d, p)));
        oi.set(b"DestOutputProfile".to_vec(), Object::Ref(r));
    }
    oi
}

fn rect(doc: &Document, d: &Dict, key: &[u8]) -> Option<[f64; 4]> {
    let a = doc.resolve(d.get(key)?);
    let v: Vec<f64> = a.as_array()?.iter().take(5).filter_map(|x| doc.resolve(x).as_f64()).collect();
    match v.as_slice() {
        [a, b, c, e] if v.iter().all(|x| x.is_finite()) => Some([a.min(*c), b.min(*e), a.max(*c), b.max(*e)]),
        _ => None,
    }
}

fn rect_obj(r: [f64; 4]) -> Object {
    let n = |v: f64| {
        let rounded = (v * 1000.0).round() / 1000.0;
        if rounded.fract() == 0.0 && rounded.abs() < 1.0e9 { Object::Int(rounded as i64) } else { Object::Real(rounded) }
    };
    Object::Array(r.into_iter().map(n).collect())
}

fn intersect(a: [f64; 4], b: [f64; 4]) -> Option<[f64; 4]> {
    let r = [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
    (r[2] > r[0] && r[3] > r[1]).then_some(r)
}

fn union(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

fn same(a: Option<[f64; 4]>, b: [f64; 4]) -> bool {
    a.is_some_and(|a| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.001))
}

/// TrimBox (unless the page uses an ArtBox and no trim was asked for) and BleedBox on every
/// page; returns how many pages changed.
fn page_boxes(doc: &mut Document, opts: &XConvertOptions) -> Result<usize, String> {
    let mut changed = 0;
    for (i, p) in pdfcraft_model::pages(doc).into_iter().enumerate() {
        let n = i + 1;
        let media = rect(doc, &p.dict, b"MediaBox").unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let crop = p.crop(doc);
        let had_trim = rect(doc, &p.dict, b"TrimBox");
        let art = rect(doc, &p.dict, b"ArtBox");
        let had_bleed = rect(doc, &p.dict, b"BleedBox");
        let trim = match opts.trim {
            Some(t) => {
                if !inside(media, t) {
                    return Err(format!("the trim box lies outside page {n}'s MediaBox ({media:?})"));
                }
                Some(t)
            }
            None => match (had_trim, art) {
                (Some(t), _) => Some(intersect(media, t).unwrap_or(crop)),
                (None, Some(_)) => None,
                (None, None) => Some(crop),
            },
        };
        let frame = trim.or(art).unwrap_or(crop);
        let bleed = match opts.bleed {
            Some(b) => {
                if !inside(media, b) {
                    return Err(format!("the bleed box lies outside page {n}'s MediaBox ({media:?})"));
                }
                b
            }
            None => {
                let base = had_bleed.unwrap_or(crop);
                intersect(media, union(base, frame)).unwrap_or(media)
            }
        };
        let mut page_changed = false;
        let mut set: Vec<(&[u8], [f64; 4])> = Vec::new();
        if let Some(t) = trim
            && !same(had_trim, t)
        {
            set.push((b"TrimBox", t));
        }
        if !same(had_bleed, bleed) {
            set.push((b"BleedBox", bleed));
        }
        if let Some(a) = art
            && !inside(media, a)
            && let Some(fixed) = intersect(media, a)
        {
            set.push((b"ArtBox", fixed));
        }
        if !set.is_empty() {
            doc.update_dict(p.obj, |d| {
                for (k, r) in set {
                    d.set(k.to_vec(), rect_obj(r));
                }
            })
            .map_err(|e| e.to_string())?;
            page_changed = true;
        }
        changed += usize::from(page_changed);
    }
    Ok(changed)
}

fn info_ref(doc: &mut Document) -> Result<(ObjRef, Dict), String> {
    match doc.trailer().get(b"Info").cloned() {
        Some(Object::Ref(r)) => {
            let d = doc.get(r).as_dict().cloned().unwrap_or_default();
            Ok((r, d))
        }
        other => {
            let d = other.and_then(|o| o.as_dict().cloned()).unwrap_or_default();
            let r = doc.add(Object::Dict(d.clone()));
            doc.trailer_mut().set(b"Info".to_vec(), Object::Ref(r));
            Ok((r, d))
        }
    }
}

fn document_info(doc: &mut Document, opts: &XConvertOptions, date: &str) -> Result<Vec<String>, String> {
    let part = opts.part;
    let (r, old) = info_ref(doc)?;
    let mut d = old.clone();
    let mut fixed = Vec::new();
    let want = part.version_key();
    if text(doc, &d, b"GTS_PDFXVersion").as_deref() != Some(want) {
        d.set(b"GTS_PDFXVersion".to_vec(), PdfString::text(want));
        fixed.push(format!("Identified the file as {} (GTS_PDFXVersion)", part.label()));
    }
    if part == XPart::X1a2001 {
        if text(doc, &d, b"GTS_PDFXConformance").as_deref() != Some("PDF/X-1a:2001") {
            d.set(b"GTS_PDFXConformance".to_vec(), PdfString::text("PDF/X-1a:2001"));
            fixed.push("Set GTS_PDFXConformance to PDF/X-1a:2001".into());
        }
    } else if d.remove(b"GTS_PDFXConformance").is_some() {
        fixed.push("Removed GTS_PDFXConformance (only PDF/X-1a:2001 has it)".into());
    }
    if text(doc, &d, b"Title").is_none() {
        let title = opts.title.clone().or_else(|| xmp_title(doc)).unwrap_or_else(|| "Untitled".into());
        d.set(b"Title".to_vec(), PdfString::text(&title));
        fixed.push(format!("Set the title to {title:?}"));
    }
    for key in ["CreationDate", "ModDate"] {
        if text(doc, &d, key.as_bytes()).is_none() {
            d.set(key.as_bytes().to_vec(), PdfString::literal(date.as_bytes().to_vec()));
            fixed.push(format!("Set {key}"));
        }
    }
    match text(doc, &d, b"Trapped").as_deref() {
        Some("True" | "False") => {
            // A name is what the standard asks for.
            if let Some(Object::String(s)) = d.get(b"Trapped").map(|t| (*doc.resolve(t)).clone()) {
                d.set(b"Trapped".to_vec(), Object::name(s.to_text().trim()));
                fixed.push("Wrote Trapped as a name".into());
            }
        }
        _ => {
            d.set(b"Trapped".to_vec(), Object::name("False"));
            fixed.push("Set Trapped to False (the file has not been trapped)".into());
        }
    }
    if d != old {
        doc.set(r, Object::Dict(d));
    }
    Ok(fixed)
}

fn xmp_title(doc: &Document) -> Option<String> {
    crate::xmp_metadata(doc).and_then(|x| crate::xmp_value(&x, "dc:title")).map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// PDF/X-4's XMP metadata: written anew when an entry the part needs is missing or differs.
fn xmp(doc: &mut Document, opts: &XConvertOptions, date: &str) -> Result<Vec<String>, String> {
    let info = doc.trailer().get(b"Info").map(|i| doc.resolve(i)).and_then(|i| i.as_dict().cloned()).unwrap_or_default();
    let get = |k: &[u8]| text(doc, &info, k);
    let title = get(b"Title").or_else(|| opts.title.clone()).unwrap_or_else(|| "Untitled".into());
    let trapped = get(b"Trapped").filter(|t| t == "True" || t == "False").unwrap_or_else(|| "False".into());
    let old = crate::xmp_metadata(doc);
    let v = |k: &str| old.as_deref().and_then(|x| crate::xmp_value(x, k)).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let ok = v("pdfxid:GTS_PDFXVersion").as_deref().and_then(|x| XPart::from_declared(x, None)) == Some(XPart::X4)
        && v("xmp:CreateDate").is_some()
        && v("xmp:ModifyDate").is_some()
        && v("dc:title").is_some()
        && v("pdf:Trapped").as_deref() == Some(trapped.as_str());
    if ok {
        return Ok(Vec::new());
    }
    let iso = |d: Option<String>| {
        d.and_then(|d| crate::xmp::iso_date(&d)).or_else(|| crate::xmp::iso_date(date)).unwrap_or_else(|| "1970-01-01T00:00:00Z".into())
    };
    let created = v("xmp:CreateDate").unwrap_or_else(|| iso(get(b"CreationDate")));
    let modified = v("xmp:ModifyDate").unwrap_or_else(|| iso(get(b"ModDate")));
    let esc = crate::xmp::esc;
    let mut props = String::new();
    props.push_str(&format!("   <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>\n", esc(&title)));
    if let Some(a) = get(b"Author") {
        props.push_str(&format!("   <dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>\n", esc(&a)));
    }
    if let Some(p) = get(b"Producer") {
        props.push_str(&format!("   <pdf:Producer>{}</pdf:Producer>\n", esc(&p)));
    }
    if let Some(c) = get(b"Creator") {
        props.push_str(&format!("   <xmp:CreatorTool>{}</xmp:CreatorTool>\n", esc(&c)));
    }
    // A PDF/A or PDF/UA identification the file had stays.
    let mut extra_ns = String::new();
    if let (Some(part), Some(conf)) = (v("pdfaid:part"), v("pdfaid:conformance")) {
        extra_ns.push_str("\n    xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\"");
        props.push_str(&format!("   <pdfaid:part>{}</pdfaid:part>\n   <pdfaid:conformance>{}</pdfaid:conformance>\n", esc(&part), esc(&conf)));
    }
    if let Some(ua) = v("pdfuaid:part") {
        extra_ns.push_str("\n    xmlns:pdfuaid=\"http://www.aiim.org/pdfua/ns/id/\"");
        props.push_str(&format!("   <pdfuaid:part>{}</pdfuaid:part>\n", esc(&ua)));
    }
    let packet = format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
 <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
  <rdf:Description rdf:about=\"\"\n\
    xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n\
    xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\"\n\
    xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n\
    xmlns:pdfxid=\"http://www.npes.org/pdfx/ns/id/\"{extra_ns}>\n\
   <pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion>\n\
   <pdf:Trapped>{trapped}</pdf:Trapped>\n\
   <xmp:CreateDate>{created}</xmp:CreateDate>\n\
   <xmp:ModifyDate>{modified}</xmp:ModifyDate>\n\
   <xmp:MetadataDate>{modified}</xmp:MetadataDate>\n\
{props}  </rdf:Description>\n\
 </rdf:RDF>\n\
</x:xmpmeta>\n\
<?xpacket end=\"w\"?>"
    );
    let mut md = Dict::new();
    md.set(b"Type".to_vec(), Object::name("Metadata"));
    md.set(b"Subtype".to_vec(), Object::name("XML"));
    // Metadata stays uncompressed so other tools can find it.
    let m = doc.add(Object::Stream(Stream::from_raw(md, packet.into_bytes())));
    let mut cat = catalog(doc);
    cat.set(b"Metadata".to_vec(), Object::Ref(m));
    set_catalog(doc, cat)?;
    Ok(vec!["Wrote the XMP metadata (pdfxid:GTS_PDFXVersion PDF/X-4, dates, title, Trapped)".into()])
}

#[cfg(test)]
#[path = "pdfx_convert_tests.rs"]
mod tests;
