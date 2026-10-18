//! PDF/X validation: the machine-checkable requirements of PDF/X-1a:2001, PDF/X-1a:2003,
//! PDF/X-3:2002, PDF/X-3:2003 and PDF/X-4 (ISO 15930-1, -4, -3, -6 and -7), reported by rule.
//!
//! [`verify_pdfx`] reads the document only. It checks the identification (`GTS_PDFXVersion` in
//! the document information or, for PDF/X-4, the XMP metadata), the PDF version, the document
//! information and XMP entries, trapping, the file identifier, encryption, the `GTS_PDFX`
//! output intent and its ICC profile, page boxes, annotations in the print area, and then walks
//! every page's content (forms, patterns, Type 3 glyphs, annotation appearances) through its
//! resources for colour spaces, transparency, fonts, transfer functions, halftones and
//! PostScript. A last pass over every object finds PostScript and reference XObjects, OPI,
//! external streams, embedded files and JavaScript.
//!
//! Where the standard's wording is not certain to us the rule reports a warning (which does not
//! affect [`XReport::conforms`]) rather than an error; [`X_NOT_CHECKED`] lists what is not
//! decided at all. All work is bounded: decoded bytes, operators, nesting depth and objects.

use std::collections::{BTreeMap, HashSet};

use pdfcraft_content::parse;
use pdfcraft_cos::{Dict, Document, ObjRef, Object};

/// The PDF/X part to verify.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum XPart {
    /// ISO 15930-1:2001 (PDF 1.3).
    X1a2001,
    /// ISO 15930-4:2003 (PDF 1.4).
    X1a2003,
    /// ISO 15930-3:2002 (PDF 1.3).
    X3_2002,
    /// ISO 15930-6:2003 (PDF 1.4).
    X3_2003,
    /// ISO 15930-7 (PDF 1.6).
    X4,
}

impl XPart {
    pub const ALL: [XPart; 5] = [XPart::X1a2001, XPart::X1a2003, XPart::X3_2002, XPart::X3_2003, XPart::X4];

    /// The short id the tool takes (`X-1a:2001`).
    pub fn id(self) -> &'static str {
        match self {
            XPart::X1a2001 => "X-1a:2001",
            XPart::X1a2003 => "X-1a:2003",
            XPart::X3_2002 => "X-3:2002",
            XPart::X3_2003 => "X-3:2003",
            XPart::X4 => "X-4",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            XPart::X1a2001 => "PDF/X-1a:2001",
            XPart::X1a2003 => "PDF/X-1a:2003",
            XPart::X3_2002 => "PDF/X-3:2002",
            XPart::X3_2003 => "PDF/X-3:2003",
            XPart::X4 => "PDF/X-4",
        }
    }

    /// `X-1a:2001`, `PDF/X-1a:2001`, `x-4`, … (case and spaces ignored).
    pub fn from_id(id: &str) -> Option<XPart> {
        let s: String = id.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
        let s = s.strip_prefix("pdf/").unwrap_or(&s);
        XPart::ALL.into_iter().find(|p| p.id().to_ascii_lowercase() == s)
    }

    /// The part a document declares by its `GTS_PDFXVersion` (and, for PDF/X-1a:2001,
    /// `GTS_PDFXConformance`).
    pub fn from_declared(version: &str, conformance: Option<&str>) -> Option<XPart> {
        match version.trim() {
            "PDF/X-1:2001" | "PDF/X-1a:2001" => {
                (conformance.is_none_or(|c| c.trim() == "PDF/X-1a:2001") || version.trim() == "PDF/X-1a:2001").then_some(XPart::X1a2001)
            }
            "PDF/X-1a:2003" => Some(XPart::X1a2003),
            "PDF/X-3:2002" => Some(XPart::X3_2002),
            "PDF/X-3:2003" => Some(XPart::X3_2003),
            "PDF/X-4" | "PDF/X-4:2008" | "PDF/X-4:2010" => Some(XPart::X4),
            _ => None,
        }
    }

    fn bit(self) -> u8 {
        match self {
            XPart::X1a2001 => X1A01,
            XPart::X1a2003 => X1A03,
            XPart::X3_2002 => X3_02,
            XPart::X3_2003 => X3_03,
            XPart::X4 => X4,
        }
    }

    fn is_x1a(self) -> bool {
        matches!(self, XPart::X1a2001 | XPart::X1a2003)
    }

    /// The highest PDF version the part is based on.
    pub fn max_pdf_version(self) -> (u8, u8) {
        match self {
            XPart::X1a2001 | XPart::X3_2002 => (1, 3),
            XPart::X1a2003 | XPart::X3_2003 => (1, 4),
            XPart::X4 => (1, 6),
        }
    }

    /// The `GTS_PDFXVersion` value the part is identified by.
    pub fn version_key(self) -> &'static str {
        match self {
            XPart::X1a2001 => "PDF/X-1:2001",
            XPart::X1a2003 => "PDF/X-1a:2003",
            XPart::X3_2002 => "PDF/X-3:2002",
            XPart::X3_2003 => "PDF/X-3:2003",
            XPart::X4 => "PDF/X-4",
        }
    }
}

const X1A01: u8 = 1;
const X1A03: u8 = 2;
const X3_02: u8 = 4;
const X3_03: u8 = 8;
const X4: u8 = 16;
const X1A: u8 = X1A01 | X1A03;
const X3: u8 = X3_02 | X3_03;
const ALL: u8 = X1A | X3 | X4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum XSeverity {
    /// Breaks a requirement: the document does not conform.
    Error,
    /// Worth fixing, but either the standard's wording isn't certain to us or the check could
    /// not be completed (limits); conformance is unaffected.
    Warning,
}

impl XSeverity {
    pub fn id(self) -> &'static str {
        match self {
            XSeverity::Error => "error",
            XSeverity::Warning => "warning",
        }
    }
}

/// One rule the validator decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XRule {
    pub id: &'static str,
    pub description: &'static str,
    /// Parts the rule applies to.
    parts: u8,
    /// Parts for which a breach is only a warning.
    warn: u8,
}

impl XRule {
    pub fn applies(&self, part: XPart) -> bool {
        self.parts & part.bit() != 0
    }

    pub fn severity(&self, part: XPart) -> XSeverity {
        if self.warn & part.bit() != 0 { XSeverity::Warning } else { XSeverity::Error }
    }
}

const fn rule(id: &'static str, parts: u8, warn: u8, description: &'static str) -> XRule {
    XRule { id, description, parts, warn }
}

/// Every rule [`verify_pdfx`] decides, in report order.
pub const X_RULES: &[XRule] = &[
    rule("version-key", ALL, 0, "GTS_PDFXVersion identifies the part (document information; XMP pdfxid:GTS_PDFXVersion for PDF/X-4)"),
    rule("conformance-key", X1A01, 0, "GTS_PDFXConformance is PDF/X-1a:2001"),
    rule("version-consistent", X4, X4, "A GTS_PDFXVersion in the document information agrees with the XMP metadata"),
    rule("pdf-version", ALL, 0, "The PDF version is no higher than the part allows (1.3, 1.4 or 1.6)"),
    rule("info-keys", X1A | X3, 0, "The document information has Title, CreationDate and ModDate"),
    rule("xmp-metadata", X4, 0, "The catalog has an XMP metadata stream"),
    rule("xmp-dates", X4, 0, "The XMP metadata has xmp:CreateDate and xmp:ModifyDate"),
    rule("xmp-title", X4, X4, "The XMP metadata has a dc:title"),
    rule("trapped", ALL, 0, "Trapped is True or False (document information; XMP pdf:Trapped for PDF/X-4, agreeing with the information)"),
    rule("document-id", ALL, X1A01 | X3_02, "The trailer has a file identifier (ID)"),
    rule("encryption", ALL, 0, "The file is not encrypted"),
    rule("output-intent", ALL, 0, "The catalog has exactly one output intent with S /GTS_PDFX"),
    rule("output-condition", ALL, 0, "The PDF/X output intent has an OutputConditionIdentifier"),
    rule("output-profile", ALL, 0, "The PDF/X output intent has a DestOutputProfile, or names a registered condition (RegistryName)"),
    rule(
        "output-profile-class",
        ALL,
        0,
        "The output profile is an ICC output (prtr) profile whose colour space matches N (CMYK or gray for PDF/X-1a)",
    ),
    rule("output-profile-version", ALL, ALL, "The output profile's ICC version is supported by the part's PDF version"),
    rule("page-trim", ALL, 0, "Every page has a TrimBox or an ArtBox"),
    rule("page-trim-art", ALL, ALL, "No page has both a TrimBox and an ArtBox"),
    rule("page-boxes", ALL, 0, "TrimBox, BleedBox and ArtBox lie inside the MediaBox, and the BleedBox contains the TrimBox"),
    rule("annotations", ALL, 0, "Annotations that print inside the BleedBox (or TrimBox) are PrinterMark or TrapNet annotations"),
    rule("fonts-embedded", ALL, 0, "Every font the content uses is embedded"),
    rule("colour-x1a", X1A, 0, "Colour is only DeviceCMYK, DeviceGray, Separation or DeviceN (no RGB, ICC-based, calibrated or Lab colour)"),
    rule("colour-alternate", X1A, X1A, "Separation and DeviceN alternate spaces are DeviceCMYK or DeviceGray"),
    rule("colour-rgb", X3 | X4, 0, "DeviceRGB is used only with an RGB output intent"),
    rule("colour-cmyk", X3 | X4, X3 | X4, "DeviceCMYK is used only with a CMYK output intent"),
    rule("transparency", X1A | X3, 0, "No transparency: soft masks, constant alpha below 1, blend modes other than Normal, transparency groups"),
    rule("optional-content", X4, 0, "Optional content configurations have a Name and no AS entry"),
    rule("transfer-functions", ALL, 0, "No transfer functions (TR, TR2 other than /Default) in graphics states"),
    rule("halftones", ALL, 0, "Halftones have a HalftoneType of 1, 5, 6, 10 or 16"),
    rule("halftone-type", ALL, ALL, "Halftones have a HalftoneType of 1 or 5"),
    rule("postscript", ALL, 0, "No PostScript XObjects and no PS operators"),
    rule("opi", ALL, 0, "No OPI dictionaries"),
    rule("external-content", ALL, 0, "No external streams (F, FFilter) and no reference XObjects"),
    rule("embedded-files", ALL, ALL, "No embedded files"),
    rule("actions", ALL, ALL, "No JavaScript and no additional actions"),
    rule("limits", ALL, ALL, "The validator read the whole document (limits not reached)"),
];

/// What isn't decided, or only approximately.
pub const X_NOT_CHECKED: &[&str] = &[
    "Whether a printing condition without a DestOutputProfile is really in the ICC characterization registry (only RegistryName is required)",
    "The colour space of an output intent without a profile, so DeviceRGB and DeviceCMYK are not compared with it",
    "Font programs themselves (completeness of subsets, glyph widths), and fonts used only by annotations' appearance streams that don't print",
    "Image details: 16-bit images, JPEG 2000 and JBIG2 beyond what the PDF version allows, Alternates",
    "LZW compression (not decided: we are not certain the PDF/X parts forbid it)",
    "TrapNet annotations' consistency with Trapped, page-level output intents, PDF/X-4p and PDF/X-5 (external profiles and graphics)",
    "Optional content beyond configuration names and AS (Order and the OCGs listed)",
    "Overprint mode details (OPM) and the exact XMP schema and extension-schema rules",
    "Content not reached from pages or printing annotations (unreferenced objects are only searched for PostScript, OPI, reference XObjects, external streams, embedded files and JavaScript)",
];

/// Findings beyond this many per rule are only counted.
pub const MAX_X_FINDINGS: usize = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct XFinding {
    pub rule: &'static str,
    pub severity: XSeverity,
    pub message: String,
    /// 0-based page, when the problem is on a page.
    pub page: Option<usize>,
    /// The object at fault, when there is one.
    pub object: Option<ObjRef>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct XOptions {
    /// The part to check; `None` checks the declared one, else PDF/X-4.
    pub part: Option<XPart>,
    /// Findings kept per rule (1 to [`MAX_X_FINDINGS`]); the rest are counted. 0 means 100.
    pub max_findings: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct XReport {
    /// The part checked.
    pub part: XPart,
    /// `GTS_PDFXVersion` as the document declares it (information, else XMP).
    pub declared: Option<String>,
    /// The part that declaration names, when it is one of [`XPart::ALL`].
    pub declared_part: Option<XPart>,
    /// Findings, at most `max_findings` per rule, in rule order.
    pub findings: Vec<XFinding>,
    /// Rule → how many problems were found (including those not kept).
    pub counts: BTreeMap<&'static str, usize>,
    /// The rules that were run for this part.
    pub checked: Vec<&'static str>,
}

impl XReport {
    /// No requirement checked here is broken (warnings don't count).
    pub fn conforms(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == XSeverity::Error)
    }

    pub fn count(&self, rule: &str) -> usize {
        self.counts.get(rule).copied().unwrap_or(0)
    }

    /// The rules with at least one problem, in rule order.
    pub fn failed(&self) -> Vec<&'static str> {
        X_RULES.iter().map(|r| r.id).filter(|id| self.count(id) > 0).collect()
    }
}

/// Decoded bytes per content stream, and in all.
const MAX_STREAM: usize = 16 << 20;
const MAX_TOTAL: usize = 256 << 20;
const MAX_OPERATORS: usize = 5_000_000;
/// Nesting of forms, patterns and glyphs.
const MAX_DEPTH: usize = 32;
/// Nesting of colour spaces (Indexed, Separation alternates, …).
const MAX_CS_DEPTH: usize = 8;
const MAX_OBJECTS: usize = 2_000_000;
const MAX_ARRAY: usize = 10_000;
const MAX_GLYPHS: usize = 4_096;
const MAX_PROFILE: usize = 64 << 20;

struct Out {
    cap: usize,
    part: XPart,
    findings: Vec<XFinding>,
    counts: BTreeMap<&'static str, usize>,
    /// (rule, page, object, kind) already reported, for content findings that repeat.
    seen: HashSet<(&'static str, Option<usize>, Option<ObjRef>, &'static str)>,
}

impl Out {
    fn rule(&self, id: &str) -> Option<&'static XRule> {
        X_RULES.iter().find(|r| r.id == id && r.applies(self.part))
    }

    fn add(&mut self, id: &'static str, page: Option<usize>, object: Option<ObjRef>, message: impl FnOnce() -> String) {
        let Some(rule) = self.rule(id) else { return };
        let severity = rule.severity(self.part);
        let n = self.counts.entry(id).or_insert(0);
        *n = n.saturating_add(1);
        if *n <= self.cap {
            self.findings.push(XFinding { rule: id, severity, message: message(), page, object });
        }
    }

    /// Report once per (rule, page, object, kind).
    fn once(&mut self, id: &'static str, page: Option<usize>, object: Option<ObjRef>, kind: &'static str, message: impl FnOnce() -> String) {
        if self.rule(id).is_some() && self.seen.insert((id, page, object, kind)) {
            self.add(id, page, object, message);
        }
    }
}

fn dict_of(doc: &Document, d: &Dict, key: &[u8]) -> Option<Dict> {
    d.get(key).map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned())
}

fn array_of(doc: &Document, d: &Dict, key: &[u8]) -> Vec<Object> {
    d.get(key).map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()).unwrap_or_default()
}

fn name_of(doc: &Document, d: &Dict, key: &[u8]) -> Option<Vec<u8>> {
    d.get(key).map(|o| doc.resolve(o)).and_then(|o| o.as_name().map(<[u8]>::to_vec))
}

fn num_of(doc: &Document, d: &Dict, key: &[u8]) -> Option<f64> {
    d.get(key).map(|o| doc.resolve(o)).and_then(|o| o.as_f64())
}

/// A text value given as a string or a name.
fn text_of(doc: &Document, d: &Dict, key: &[u8]) -> Option<String> {
    let v = doc.resolve(d.get(key)?);
    match &*v {
        Object::String(s) => Some(s.to_text()),
        Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
        _ => None,
    }
    .map(|s| s.trim().to_owned())
    .filter(|s| !s.is_empty())
}

fn rect(doc: &Document, d: &Dict, key: &[u8]) -> Option<[f64; 4]> {
    let a = doc.resolve(d.get(key)?);
    let v: Vec<f64> = a.as_array()?.iter().take(5).filter_map(|x| doc.resolve(x).as_f64()).collect();
    match v.as_slice() {
        [a, b, c, e] if v.iter().all(|x| x.is_finite()) => Some([a.min(*c), b.min(*e), a.max(*c), b.max(*e)]),
        _ => None,
    }
}

fn inside(outer: [f64; 4], inner: [f64; 4]) -> bool {
    const EPS: f64 = 0.01;
    inner[0] >= outer[0] - EPS && inner[1] >= outer[1] - EPS && inner[2] <= outer[2] + EPS && inner[3] <= outer[3] + EPS
}

fn overlaps(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn name_text(n: &[u8]) -> String {
    String::from_utf8_lossy(n).into_owned()
}

/// `"1.4"` → (1, 4).
fn parse_version(v: &str) -> Option<(u8, u8)> {
    let v = v.trim();
    let (a, b) = v.split_once('.')?;
    let b: String = b.chars().take_while(char::is_ascii_digit).collect();
    Some((a.trim().parse().ok()?, b.parse().ok()?))
}

fn catalog(doc: &Document) -> Dict {
    doc.root().and_then(|r| doc.get(r).as_dict().cloned()).unwrap_or_default()
}

fn info(doc: &Document) -> Dict {
    doc.trailer().get(b"Info").map(|i| doc.resolve(i)).and_then(|i| i.as_dict().cloned()).unwrap_or_default()
}

/// What the document declares: `GTS_PDFXVersion` and `GTS_PDFXConformance` (information first,
/// then XMP).
pub fn declared_pdfx(doc: &Document) -> (Option<String>, Option<String>) {
    let i = info(doc);
    let x = crate::xmp_metadata(doc);
    let get = |info_key: &[u8], xmp_key: &str| {
        text_of(doc, &i, info_key)
            .or_else(|| x.as_deref().and_then(|x| crate::xmp_value(x, xmp_key)).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()))
    };
    (get(b"GTS_PDFXVersion", "pdfxid:GTS_PDFXVersion"), get(b"GTS_PDFXConformance", "pdfxid:GTS_PDFXConformance"))
}

/// Verify `doc` against the machine-checkable requirements of a PDF/X part.
pub fn verify_pdfx(doc: &Document, options: &XOptions) -> XReport {
    let (declared, conformance) = declared_pdfx(doc);
    let declared_part = declared.as_deref().and_then(|v| XPart::from_declared(v, conformance.as_deref()));
    let part = options.part.or(declared_part).unwrap_or(XPart::X4);
    let cap = if options.max_findings == 0 { 100 } else { options.max_findings.min(MAX_X_FINDINGS) };
    let mut out = Out { cap, part, findings: Vec::new(), counts: BTreeMap::new(), seen: HashSet::new() };
    let cat = catalog(doc);

    identification(doc, &cat, part, &mut out);
    let intent_cs = output_intent(doc, &cat, part, &mut out);
    if part == XPart::X4 {
        optional_content(doc, &cat, &mut out);
    }
    let pages = pdfcraft_model::pages(doc);
    let mut walk = Walk { doc, part, intent_cs, out: &mut out, seen: HashSet::new(), fonts: HashSet::new(), bytes: 0, ops: 0, limited: false };
    for (i, p) in pages.iter().enumerate() {
        walk.page(i, p);
    }
    objects(doc, &cat, &pages, &mut out);

    let checked = X_RULES.iter().filter(|r| r.applies(part)).map(|r| r.id).collect::<Vec<_>>();
    let order = |id: &str| X_RULES.iter().position(|r| r.id == id).unwrap_or(usize::MAX);
    out.findings.sort_by_key(|f| order(f.rule));
    XReport { part, declared, declared_part, findings: out.findings, counts: out.counts, checked }
}

/// Version keys, PDF version, document information and XMP, trapping, ID, encryption.
fn identification(doc: &Document, cat: &Dict, part: XPart, out: &mut Out) {
    let i = info(doc);
    let xmp = crate::xmp_metadata(doc);
    let xv = |key: &str| xmp.as_deref().and_then(|x| crate::xmp_value(x, key)).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let want = part.version_key();
    if part == XPart::X4 {
        match xv("pdfxid:GTS_PDFXVersion") {
            None => out.add("version-key", None, None, || "The XMP metadata has no pdfxid:GTS_PDFXVersion".into()),
            Some(v) if XPart::from_declared(&v, None) != Some(XPart::X4) => {
                out.add("version-key", None, None, || format!("The XMP metadata declares {v:?}, not PDF/X-4"))
            }
            Some(v) => {
                if let Some(iv) = text_of(doc, &i, b"GTS_PDFXVersion")
                    && iv != v
                {
                    out.add("version-consistent", None, None, || {
                        format!("The document information's GTS_PDFXVersion is {iv:?}, the XMP metadata's {v:?}")
                    });
                }
            }
        }
    } else {
        match text_of(doc, &i, b"GTS_PDFXVersion") {
            None => out.add("version-key", None, None, || format!("The document information has no GTS_PDFXVersion (expected {want:?})")),
            Some(v) if v != want => out.add("version-key", None, None, || format!("GTS_PDFXVersion is {v:?}; {} expects {want:?}", part.label())),
            _ => {}
        }
        if part == XPart::X1a2001 && text_of(doc, &i, b"GTS_PDFXConformance").as_deref() != Some("PDF/X-1a:2001") {
            out.add("conformance-key", None, None, || "The document information has no GTS_PDFXConformance of \"PDF/X-1a:2001\"".into());
        }
    }

    let header = parse_version(doc.version());
    let in_catalog = name_of(doc, cat, b"Version").and_then(|v| parse_version(&name_text(&v)));
    let version = match (header, in_catalog) {
        (Some(h), Some(c)) => Some(h.max(c)),
        (h, c) => h.or(c),
    };
    let max = part.max_pdf_version();
    if let Some(v) = version
        && v > max
    {
        out.add("pdf-version", None, None, || format!("The file is PDF {}.{}; {} allows at most PDF {}.{}", v.0, v.1, part.label(), max.0, max.1));
    }

    if part == XPart::X4 {
        if xmp.is_none() {
            out.add("xmp-metadata", None, None, || "The catalog has no XMP metadata stream (Metadata)".into());
        } else {
            for key in ["xmp:CreateDate", "xmp:ModifyDate"] {
                if xv(key).is_none() {
                    out.add("xmp-dates", None, None, || format!("The XMP metadata has no {key}"));
                }
            }
            if xv("dc:title").is_none() {
                out.add("xmp-title", None, None, || "The XMP metadata has no dc:title".into());
            }
        }
        let info_trapped = text_of(doc, &i, b"Trapped");
        match xv("pdf:Trapped") {
            Some(t) if t == "True" || t == "False" => {
                if let Some(it) = info_trapped
                    && it != t
                {
                    out.add("trapped", None, None, || format!("Trapped is {it} in the document information but {t} in the XMP metadata"));
                }
            }
            Some(t) => out.add("trapped", None, None, || format!("pdf:Trapped is {t:?}; it must be True or False")),
            None if xmp.is_some() => out.add("trapped", None, None, || "The XMP metadata has no pdf:Trapped (True or False)".into()),
            None => {}
        }
    } else {
        let missing: Vec<&str> = ["Title", "CreationDate", "ModDate"].into_iter().filter(|k| text_of(doc, &i, k.as_bytes()).is_none()).collect();
        if !missing.is_empty() {
            out.add("info-keys", None, None, || format!("The document information has no {}", missing.join(", ")));
        }
        match text_of(doc, &i, b"Trapped") {
            Some(t) if t == "True" || t == "False" => {}
            Some(t) => out.add("trapped", None, None, || format!("Trapped is {t:?}; it must be True or False")),
            None => out.add("trapped", None, None, || "The document information has no Trapped entry (True or False)".into()),
        }
    }

    let id_ok = doc.trailer().get(b"ID").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()).is_some_and(|a| a.len() == 2);
    if !id_ok {
        out.add("document-id", None, None, || "The trailer has no file identifier (ID with two strings)".into());
    }
    if doc.trailer().get(b"Encrypt").is_some() || doc.security().is_some() {
        out.add("encryption", None, None, || "The file is encrypted".into());
    }
}

/// The output intent; returns the output profile's colour space (`CMYK`, `GRAY`, `RGB `).
fn output_intent(doc: &Document, cat: &Dict, part: XPart, out: &mut Out) -> Option<[u8; 4]> {
    let intents: Vec<(Option<ObjRef>, Dict)> = array_of(doc, cat, b"OutputIntents")
        .iter()
        .take(MAX_ARRAY)
        .filter_map(|o| doc.resolve(o).as_dict().cloned().map(|d| (o.as_ref(), d)))
        .filter(|(_, d)| name_of(doc, d, b"S").as_deref() == Some(b"GTS_PDFX"))
        .collect();
    let (obj, oi) = match intents.as_slice() {
        [] => {
            out.add("output-intent", None, None, || "The catalog has no output intent with S /GTS_PDFX".into());
            return None;
        }
        [one] => one.clone(),
        [first, ..] => {
            let n = intents.len();
            out.add("output-intent", None, None, || format!("The catalog has {n} GTS_PDFX output intents; only one is allowed"));
            first.clone()
        }
    };
    if text_of(doc, &oi, b"OutputConditionIdentifier").is_none() {
        out.add("output-condition", None, obj, || "The PDF/X output intent has no OutputConditionIdentifier".into());
    }
    let Some(profile_obj) = oi.get(b"DestOutputProfile") else {
        if text_of(doc, &oi, b"RegistryName").is_none() {
            out.add("output-profile", None, obj, || {
                "The PDF/X output intent has no DestOutputProfile and no RegistryName: the printing condition isn't characterized".into()
            });
        }
        return None;
    };
    let pref = profile_obj.as_ref().or(obj);
    let resolved = doc.resolve(profile_obj);
    let Object::Stream(s) = &*resolved else {
        out.add("output-profile", None, pref, || "The output intent's DestOutputProfile isn't an ICC profile stream".into());
        return None;
    };
    let Ok(data) = s.decoded_within(MAX_PROFILE) else {
        out.add("output-profile-class", None, pref, || "The output profile can't be decoded".into());
        return None;
    };
    let (Some(class), Some(space), Some(major)) =
        (data.get(12..16), data.get(16..20).and_then(|b| <[u8; 4]>::try_from(b).ok()), data.get(8).copied())
    else {
        out.add("output-profile-class", None, pref, || "The output profile is too short to be an ICC profile".into());
        return None;
    };
    if data.get(36..40) != Some(b"acsp".as_slice()) {
        out.add("output-profile-class", None, pref, || "The output profile has no ICC signature (acsp)".into());
        return None;
    }
    if class != b"prtr" {
        let c = String::from_utf8_lossy(class).into_owned();
        out.add("output-profile-class", None, pref, || format!("The output profile is a {c:?} profile, not an output (prtr) profile"));
    }
    let channels = match &space {
        b"CMYK" => Some(4),
        b"GRAY" => Some(1),
        b"RGB " => Some(3),
        _ => None,
    };
    let sp = String::from_utf8_lossy(&space).trim().to_owned();
    if let (Some(c), Some(n)) = (channels, s.dict.get(b"N").and_then(|n| doc.resolve(n).as_int()))
        && c != n
    {
        out.add("output-profile-class", None, pref, || format!("The output profile is {sp} but its N is {n}"));
    }
    if part.is_x1a() && !matches!(&space, b"CMYK" | b"GRAY") {
        out.add("output-profile-class", None, pref, || format!("The output profile is {sp}; PDF/X-1a needs a CMYK or gray printing condition"));
    }
    let max_major = if part == XPart::X4 { 4 } else { 2 };
    if major > max_major {
        out.add("output-profile-version", None, pref, || {
            format!("The output profile is ICC version {major}; {} expects version {max_major} at most", part.label())
        });
    }
    Some(space)
}

/// PDF/X-4: optional content configurations.
fn optional_content(doc: &Document, cat: &Dict, out: &mut Out) {
    let Some(oc) = dict_of(doc, cat, b"OCProperties") else { return };
    if let Some(d) = dict_of(doc, &oc, b"D") {
        if text_of(doc, &d, b"Name").is_none() {
            out.add("optional-content", None, None, || "The default optional content configuration (D) has no Name".into());
        }
        if d.contains(b"AS") {
            out.add("optional-content", None, None, || "The default optional content configuration (D) has an AS entry".into());
        }
    }
    for (i, c) in array_of(doc, &oc, b"Configs").iter().take(MAX_ARRAY).enumerate() {
        let Some(c) = doc.resolve(c).as_dict().cloned() else { continue };
        if text_of(doc, &c, b"Name").is_none() {
            out.add("optional-content", None, None, || format!("Optional content configuration {} in Configs has no Name", i + 1));
        }
        if c.contains(b"AS") {
            out.add("optional-content", None, None, || format!("Optional content configuration {} in Configs has an AS entry", i + 1));
        }
    }
}

/// The content walk: pages, their annotations' appearances, and what they draw.
struct Walk<'a> {
    doc: &'a Document,
    part: XPart,
    intent_cs: Option<[u8; 4]>,
    out: &'a mut Out,
    /// Forms, patterns, images, shadings and Type 3 fonts already checked.
    seen: HashSet<ObjRef>,
    fonts: HashSet<ObjRef>,
    bytes: usize,
    ops: usize,
    limited: bool,
}

/// Where a finding is.
#[derive(Clone, Copy)]
struct At {
    page: Option<usize>,
    object: Option<ObjRef>,
}

impl Walk<'_> {
    fn limit(&mut self, at: At, what: &str) {
        if !self.limited {
            self.limited = true;
            let what = what.to_owned();
            self.out.add("limits", at.page, at.object, || what);
        }
    }

    fn page(&mut self, i: usize, p: &pdfcraft_model::Page) {
        let doc = self.doc;
        let at = At { page: Some(i), object: Some(p.obj) };
        let n = i + 1;
        // Boxes.
        let media = rect(doc, &p.dict, b"MediaBox");
        let trim = rect(doc, &p.dict, b"TrimBox");
        let art = rect(doc, &p.dict, b"ArtBox");
        let bleed = rect(doc, &p.dict, b"BleedBox");
        match (trim, art) {
            (None, None) => self.out.add("page-trim", at.page, at.object, || format!("Page {n} has neither a TrimBox nor an ArtBox")),
            (Some(_), Some(_)) => self.out.add("page-trim-art", at.page, at.object, || format!("Page {n} has both a TrimBox and an ArtBox")),
            _ => {}
        }
        if let Some(m) = media {
            for (name, b) in [("TrimBox", trim), ("BleedBox", bleed), ("ArtBox", art)] {
                if let Some(b) = b
                    && !inside(m, b)
                {
                    self.out.add("page-boxes", at.page, at.object, || format!("Page {n}: the {name} extends beyond the MediaBox"));
                }
            }
        }
        if let (Some(b), Some(t)) = (bleed, trim)
            && !inside(b, t)
        {
            self.out.add("page-boxes", at.page, at.object, || format!("Page {n}: the BleedBox doesn't contain the TrimBox"));
        }
        // Page-level additional actions, transparency group.
        if p.dict.contains(b"AA") {
            self.out.add("actions", at.page, at.object, || format!("Page {n} has additional actions (AA)"));
        }
        if let Some(g) = dict_of(doc, &p.dict, b"Group") {
            self.group(&g, at, &format!("Page {n}"));
        }
        let res = dict_of(doc, &p.dict, b"Resources").unwrap_or_default();
        // Content.
        let contents: Vec<Object> = match p.dict.get(b"Contents").map(|c| (c.clone(), doc.resolve(c))) {
            Some((_, a)) if matches!(&*a, Object::Array(_)) => a.as_array().cloned().unwrap_or_default(),
            Some((c, _)) => vec![c],
            None => Vec::new(),
        };
        let mut data = Vec::new();
        for c in contents.iter().take(MAX_ARRAY) {
            if let Object::Stream(s) = &*doc.resolve(c) {
                match self.decode(s, at) {
                    Some(d) => {
                        data.extend_from_slice(&d);
                        data.push(b'\n');
                    }
                    None => break,
                }
            }
        }
        self.content(&data, &res, at, 0);
        // Annotations.
        let print_area = bleed.or(trim).or(art);
        for a in array_of(doc, &p.dict, b"Annots").iter().take(MAX_ARRAY) {
            let Some(d) = doc.resolve(a).as_dict().cloned() else { continue };
            let aat = At { page: Some(i), object: a.as_ref() };
            let sub = name_of(doc, &d, b"Subtype").unwrap_or_default();
            if d.contains(b"AA") {
                self.out.add("actions", aat.page, aat.object, || format!("A {} annotation on page {n} has additional actions", name_text(&sub)));
            }
            let flags = d.get(b"F").and_then(|f| doc.resolve(f).as_int()).unwrap_or(0);
            let prints = flags & 4 != 0 && flags & 2 == 0;
            if !prints {
                continue;
            }
            if !matches!(sub.as_slice(), b"PrinterMark" | b"TrapNet")
                && let (Some(area), Some(r)) = (print_area, rect(doc, &d, b"Rect"))
                && overlaps(area, r)
            {
                let which = if bleed.is_some() {
                    "BleedBox"
                } else if trim.is_some() {
                    "TrimBox"
                } else {
                    "ArtBox"
                };
                self.out.add("annotations", aat.page, aat.object, || {
                    format!("A {} annotation on page {n} prints inside the {which}; only PrinterMark and TrapNet annotations may", name_text(&sub))
                });
            }
            // What a printing annotation draws.
            if let Some(ap) = dict_of(doc, &d, b"AP")
                && let Some(normal) = ap.get(b"N")
            {
                let streams: Vec<Object> = match &*doc.resolve(normal) {
                    Object::Stream(_) => vec![normal.clone()],
                    Object::Dict(states) => states.iter().take(16).map(|(_, v)| v.clone()).collect(),
                    _ => Vec::new(),
                };
                for s in streams {
                    self.form(&s, &res, aat, 1);
                }
            }
        }
    }

    fn decode(&mut self, s: &pdfcraft_cos::Stream, at: At) -> Option<Vec<u8>> {
        let room = MAX_TOTAL.saturating_sub(self.bytes);
        if room == 0 {
            self.limit(at, &format!("Only the first {} MB of content were checked", MAX_TOTAL >> 20));
            return None;
        }
        let d = s.decoded_within(MAX_STREAM.min(room)).ok()?;
        self.bytes = self.bytes.saturating_add(d.len());
        Some(d)
    }

    /// Transparency group attributes (and the group's colour space).
    fn group(&mut self, g: &Dict, at: At, what: &str) {
        if name_of(self.doc, g, b"S").as_deref() == Some(b"Transparency") {
            let what = what.to_owned();
            self.out.once("transparency", at.page, at.object, "group", || format!("{what} is a transparency group"));
        }
        if let Some(cs) = g.get(b"CS") {
            let res = Dict::new();
            self.colour_space(cs, &res, at, 0, false);
        }
    }

    fn content(&mut self, data: &[u8], res: &Dict, at: At, depth: usize) {
        if data.is_empty() {
            return;
        }
        let doc = self.doc;
        let parsed = parse(data);
        let sub = |key: &[u8]| dict_of(doc, res, key).unwrap_or_default();
        let (xobjects, states, patterns, shadings, fonts) = (sub(b"XObject"), sub(b"ExtGState"), sub(b"Pattern"), sub(b"Shading"), sub(b"Font"));
        for op in &parsed.ops {
            self.ops = self.ops.saturating_add(1);
            if self.ops > MAX_OPERATORS {
                self.limit(at, &format!("Only the first {MAX_OPERATORS} content operators were checked"));
                return;
            }
            match op.op.as_slice() {
                b"rg" | b"RG" => self.device(b"DeviceRGB", res, at, 0, false),
                b"k" | b"K" => self.device(b"DeviceCMYK", res, at, 0, false),
                b"g" | b"G" => self.device(b"DeviceGray", res, at, 0, false),
                b"cs" | b"CS" => {
                    if let Some(n) = op.name(0) {
                        let n = Object::Name(n.to_vec());
                        self.colour_space(&n, res, at, 0, false);
                    }
                }
                b"scn" | b"SCN" => {
                    if let Some(n) = op.operands.last().and_then(Object::as_name)
                        && let Some(p) = patterns.get(n)
                    {
                        self.pattern(p, res, at, depth);
                    }
                }
                b"sh" => {
                    if let Some(sh) = op.name(0).and_then(|n| shadings.get(n)) {
                        self.shading(sh, res, at);
                    }
                }
                b"gs" => {
                    if let Some(g) = op.name(0).and_then(|n| states.get(n)).map(|g| doc.resolve(g)).and_then(|g| g.as_dict().cloned()) {
                        self.ext_gstate(&g, res, at, depth);
                    }
                }
                b"Do" => {
                    if let Some(x) = op.name(0).and_then(|n| xobjects.get(n)) {
                        self.xobject(x, res, at, depth);
                    }
                }
                b"Tf" => {
                    if let Some(f) = op.name(0).and_then(|n| fonts.get(n)) {
                        self.font(f, at, depth);
                    }
                }
                b"PS" => self.out.once("postscript", at.page, at.object, "op", || "The content uses the PS operator (PostScript)".into()),
                b"BI" => {
                    if let Some((d, _)) = &op.inline
                        && let Some(cs) = d.get(b"CS").or_else(|| d.get(b"ColorSpace"))
                    {
                        self.inline_cs(cs, res, at);
                    }
                }
                _ => {}
            }
        }
    }

    /// An inline image's colour space: abbreviations, a resource name, or an array.
    fn inline_cs(&mut self, cs: &Object, res: &Dict, at: At) {
        let expand = |n: &[u8]| -> Option<&'static [u8]> {
            match n {
                b"G" => Some(b"DeviceGray"),
                b"RGB" => Some(b"DeviceRGB"),
                b"CMYK" => Some(b"DeviceCMYK"),
                _ => None,
            }
        };
        match cs {
            Object::Name(n) => match expand(n) {
                Some(full) => self.device(full, res, at, 0, false),
                None => self.colour_space(cs, res, at, 0, false),
            },
            Object::Array(a) => {
                // [/I base hival lookup]
                if a.first().and_then(Object::as_name).is_some_and(|n| n == b"I" || n == b"Indexed")
                    && let Some(base) = a.get(1)
                {
                    match base.as_name().and_then(expand) {
                        Some(full) => self.device(full, res, at, 1, false),
                        None => self.colour_space(base, res, at, 1, false),
                    }
                } else {
                    self.colour_space(cs, res, at, 0, false);
                }
            }
            _ => {}
        }
    }

    /// A device colour space, through the resources' Default colour space when there is one.
    fn device(&mut self, family: &[u8], res: &Dict, at: At, depth: usize, alternate: bool) {
        let doc = self.doc;
        let default_key: &[u8] = match family {
            b"DeviceRGB" => b"DefaultRGB",
            b"DeviceCMYK" => b"DefaultCMYK",
            b"DeviceGray" => b"DefaultGray",
            _ => b"",
        };
        if !alternate
            && let Some(d) = dict_of(doc, res, b"ColorSpace").and_then(|c| c.get(default_key).cloned())
            && depth < MAX_CS_DEPTH
        {
            // The default space stands for the device space (and is never itself replaced).
            if !matches!(&*doc.resolve(&d), Object::Name(_)) {
                self.colour_space(&d, &Dict::new(), at, depth + 1, false);
                return;
            }
        }
        let x1a = self.part.is_x1a();
        match family {
            b"DeviceRGB" if x1a => {
                let rule = if alternate { "colour-alternate" } else { "colour-x1a" };
                self.out.once(rule, at.page, at.object, "rgb", || {
                    if alternate {
                        "A spot colour's alternate space is DeviceRGB".into()
                    } else {
                        "DeviceRGB colour is used; PDF/X-1a allows only CMYK, gray and spot colour".into()
                    }
                });
            }
            b"DeviceRGB" if !alternate && self.intent_cs.is_some_and(|c| &c != b"RGB ") => {
                self.out.once("colour-rgb", at.page, at.object, "rgb", || "DeviceRGB colour is used, but the output intent isn't RGB".into());
            }
            b"DeviceCMYK" if !alternate && !x1a && self.intent_cs.is_some_and(|c| &c != b"CMYK") => {
                self.out.once("colour-cmyk", at.page, at.object, "cmyk", || "DeviceCMYK colour is used, but the output intent isn't CMYK".into());
            }
            _ => {}
        }
    }

    fn colour_space(&mut self, cs: &Object, res: &Dict, at: At, depth: usize, alternate: bool) {
        if depth > MAX_CS_DEPTH {
            return;
        }
        let doc = self.doc;
        let resolved = doc.resolve(cs);
        match &*resolved {
            Object::Name(n) => match n.as_slice() {
                b"DeviceRGB" | b"DeviceCMYK" | b"DeviceGray" => self.device(n, res, at, depth, alternate),
                b"Pattern" => {}
                other => {
                    if let Some(c) = dict_of(doc, res, b"ColorSpace").and_then(|c| c.get(other).cloned()) {
                        self.colour_space(&c, res, at, depth + 1, alternate);
                    }
                }
            },
            Object::Array(a) => {
                let family = a.first().map(|f| doc.resolve(f)).and_then(|f| f.as_name().map(<[u8]>::to_vec)).unwrap_or_default();
                match family.as_slice() {
                    b"ICCBased" | b"CalRGB" | b"CalGray" | b"Lab" if self.part.is_x1a() => {
                        let kind: &'static str = match family.as_slice() {
                            b"ICCBased" => "ICCBased",
                            b"CalRGB" => "CalRGB",
                            b"CalGray" => "CalGray",
                            _ => "Lab",
                        };
                        let rule = if alternate { "colour-alternate" } else { "colour-x1a" };
                        self.out.once(rule, at.page, at.object, kind, || {
                            if alternate {
                                format!("A spot colour's alternate space is {kind}")
                            } else {
                                format!("{kind} colour is used; PDF/X-1a allows only CMYK, gray and spot colour")
                            }
                        });
                    }
                    b"Indexed" | b"I" | b"Pattern" => {
                        if let Some(base) = a.get(1) {
                            self.colour_space(base, res, at, depth + 1, alternate);
                        }
                    }
                    b"Separation" | b"DeviceN" => {
                        if self.part.is_x1a()
                            && let Some(alt) = a.get(2)
                        {
                            self.colour_space(alt, res, at, depth + 1, true);
                        }
                    }
                    b"DeviceRGB" | b"DeviceCMYK" | b"DeviceGray" => self.device(&family, res, at, depth, alternate),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn ext_gstate(&mut self, g: &Dict, res: &Dict, at: At, depth: usize) {
        let doc = self.doc;
        if let Some(sm) = g.get(b"SMask")
            && !matches!(&*doc.resolve(sm), Object::Name(n) if n == b"None")
        {
            self.out.once("transparency", at.page, at.object, "smask", || "A graphics state sets a soft mask (SMask)".into());
        }
        for key in [b"CA".as_slice(), b"ca"] {
            if num_of(doc, g, key).is_some_and(|v| v < 1.0) {
                self.out.once("transparency", at.page, at.object, "alpha", || "A graphics state sets constant alpha (CA or ca) below 1".into());
            }
        }
        if let Some(bm) = g.get(b"BM") {
            let modes: Vec<Vec<u8>> = match &*doc.resolve(bm) {
                Object::Name(n) => vec![n.clone()],
                Object::Array(a) => a.iter().take(64).filter_map(|m| doc.resolve(m).as_name().map(<[u8]>::to_vec)).collect(),
                _ => Vec::new(),
            };
            if let Some(m) = modes.iter().find(|m| !matches!(m.as_slice(), b"Normal" | b"Compatible")) {
                let m = name_text(m);
                self.out.once("transparency", at.page, at.object, "blend", || format!("A graphics state uses the {m} blend mode"));
            }
        }
        if let Some(tr) = g.get(b"TR")
            && !matches!(&*doc.resolve(tr), Object::Name(n) if n == b"Identity")
        {
            self.out.once("transfer-functions", at.page, at.object, "tr", || "A graphics state sets a transfer function (TR)".into());
        }
        if let Some(tr) = g.get(b"TR2")
            && !matches!(&*doc.resolve(tr), Object::Name(n) if n == b"Default" || n == b"Identity")
        {
            self.out.once("transfer-functions", at.page, at.object, "tr2", || {
                "A graphics state sets a transfer function (TR2 other than /Default)".into()
            });
        }
        if let Some(ht) = g.get(b"HT") {
            let ht = doc.resolve(ht);
            if let Some(h) = ht.as_dict() {
                match h.get(b"HalftoneType").and_then(|t| doc.resolve(t).as_int()) {
                    Some(1 | 5) => {}
                    Some(t @ (6 | 10 | 16)) => {
                        self.out.once("halftone-type", at.page, at.object, "ht", || format!("A halftone has HalftoneType {t}"))
                    }
                    Some(t) => self.out.once("halftones", at.page, at.object, "ht", || format!("A halftone has HalftoneType {t}")),
                    None => self.out.once("halftones", at.page, at.object, "ht", || "A halftone has no valid HalftoneType".into()),
                }
            }
        }
        // A soft mask's group draws content too.
        if let Some(sm) = g.get(b"SMask").map(|s| doc.resolve(s)).and_then(|s| s.as_dict().cloned())
            && let Some(form) = sm.get(b"G")
        {
            self.form(form, res, at, depth + 1);
        }
    }

    fn shading(&mut self, sh: &Object, res: &Dict, at: At) {
        let doc = self.doc;
        if let Some(r) = sh.as_ref()
            && !self.seen.insert(r)
        {
            return;
        }
        if let Some(d) = doc.resolve(sh).as_dict()
            && let Some(cs) = d.get(b"ColorSpace")
        {
            let cs = cs.clone();
            let at = At { page: at.page, object: sh.as_ref().or(at.object) };
            self.colour_space(&cs, res, at, 0, false);
        }
    }

    fn pattern(&mut self, p: &Object, res: &Dict, at: At, depth: usize) {
        let doc = self.doc;
        let resolved = doc.resolve(p);
        let Some(d) = resolved.as_dict().cloned() else { return };
        match d.get(b"PatternType").and_then(|t| doc.resolve(t).as_int()) {
            Some(1) => self.form(p, res, at, depth + 1),
            Some(2) => {
                if let Some(r) = p.as_ref()
                    && !self.seen.insert(r)
                {
                    return;
                }
                let at = At { page: at.page, object: p.as_ref().or(at.object) };
                if let Some(sh) = d.get(b"Shading") {
                    self.shading(sh, res, at);
                }
                if let Some(g) = dict_of(doc, &d, b"ExtGState") {
                    self.ext_gstate(&g, res, at, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn xobject(&mut self, x: &Object, res: &Dict, at: At, depth: usize) {
        let doc = self.doc;
        let resolved = doc.resolve(x);
        let Object::Stream(s) = &*resolved else { return };
        match s.dict.name(b"Subtype") {
            Some(b"Image") => {
                if let Some(r) = x.as_ref()
                    && !self.seen.insert(r)
                {
                    return;
                }
                let at = At { page: at.page, object: x.as_ref().or(at.object) };
                let mask = s.dict.get(b"ImageMask").is_some_and(|m| matches!(&*doc.resolve(m), Object::Bool(true)));
                if !mask && let Some(cs) = s.dict.get(b"ColorSpace") {
                    self.colour_space(cs, res, at, 0, false);
                }
                if s.dict.get(b"SMask").is_some_and(|m| matches!(&*doc.resolve(m), Object::Stream(_))) {
                    self.out.once("transparency", at.page, at.object, "image-smask", || "An image has a soft mask (SMask)".into());
                }
                if s.dict.get(b"SMaskInData").and_then(|v| doc.resolve(v).as_int()).is_some_and(|v| v > 0) {
                    self.out.once("transparency", at.page, at.object, "image-smask", || "A JPEG 2000 image has soft-mask data (SMaskInData)".into());
                }
            }
            Some(b"Form") => self.form(x, res, at, depth + 1),
            _ => {}
        }
    }

    /// A form XObject, a tiling pattern or an appearance stream: its group and content, with its
    /// own resources (else the caller's).
    fn form(&mut self, f: &Object, res: &Dict, at: At, depth: usize) {
        let doc = self.doc;
        if let Some(r) = f.as_ref()
            && !self.seen.insert(r)
        {
            // Checked already, or a form that draws itself: never followed twice.
            return;
        }
        if depth > MAX_DEPTH {
            self.limit(at, &format!("Forms nested more than {MAX_DEPTH} deep were not checked"));
            return;
        }
        let resolved = doc.resolve(f);
        let Object::Stream(s) = &*resolved else { return };
        let at = At { page: at.page, object: f.as_ref().or(at.object) };
        if let Some(g) = dict_of(doc, &s.dict, b"Group") {
            self.group(&g, at, "A form");
        }
        let own = dict_of(doc, &s.dict, b"Resources");
        let res = own.as_ref().unwrap_or(res).clone();
        if let Some(data) = self.decode(s, at) {
            self.content(&data, &res, at, depth);
        }
    }

    fn font(&mut self, f: &Object, at: At, depth: usize) {
        let doc = self.doc;
        if let Some(r) = f.as_ref()
            && !self.fonts.insert(r)
        {
            return;
        }
        let resolved = doc.resolve(f);
        let Some(d) = resolved.as_dict().cloned() else { return };
        let at = At { page: at.page, object: f.as_ref().or(at.object) };
        let sub = name_of(doc, &d, b"Subtype").unwrap_or_default();
        if sub == b"Type3" {
            if depth >= MAX_DEPTH {
                self.limit(at, &format!("Type 3 glyphs nested more than {MAX_DEPTH} deep were not checked"));
                return;
            }
            let res = dict_of(doc, &d, b"Resources").unwrap_or_default();
            if let Some(procs) = dict_of(doc, &d, b"CharProcs") {
                for (_, g) in procs.iter().take(MAX_GLYPHS) {
                    self.form(g, &res, at, depth + 1);
                }
            }
            return;
        }
        let descriptor = if sub == b"Type0" {
            array_of(doc, &d, b"DescendantFonts")
                .first()
                .map(|x| doc.resolve(x))
                .and_then(|x| x.as_dict().and_then(|fd| dict_of(doc, fd, b"FontDescriptor")))
        } else {
            dict_of(doc, &d, b"FontDescriptor")
        };
        let embedded = descriptor.is_some_and(|fd| fd.contains(b"FontFile") || fd.contains(b"FontFile2") || fd.contains(b"FontFile3"));
        if !embedded {
            let name = name_of(doc, &d, b"BaseFont").map(|n| name_text(&n)).unwrap_or_else(|| "a font".into());
            self.out.add("fonts-embedded", at.page, at.object, || format!("The font {name} isn't embedded"));
        }
    }
}

/// Whether a dictionary with an `/S` entry is an action (not a structure element).
fn is_action(d: &Dict) -> bool {
    match d.name(b"Type") {
        Some(t) => t == b"Action",
        None => !(d.contains(b"P") && d.contains(b"K")),
    }
}

/// Catalog-level actions and a pass over every object.
fn objects(doc: &Document, cat: &Dict, pages: &[pdfcraft_model::Page], out: &mut Out) {
    if cat.contains(b"AA") {
        out.add("actions", None, None, || "The document has additional actions (AA)".into());
    }
    let names = dict_of(doc, cat, b"Names").unwrap_or_default();
    if names.contains(b"JavaScript") {
        out.add("actions", None, None, || "The document has document-level JavaScript".into());
    }
    if names.contains(b"EmbeddedFiles") {
        out.add("embedded-files", None, None, || "The document has embedded files (Names EmbeddedFiles)".into());
    }
    let page_of: std::collections::HashMap<ObjRef, usize> = pages
        .iter()
        .enumerate()
        .flat_map(|(i, p)| array_of(doc, &p.dict, b"Annots").into_iter().take(MAX_ARRAY).filter_map(move |a| a.as_ref().map(|r| (r, i))))
        .collect();
    let mut n = 0usize;
    for (r, o) in doc.scan_objects() {
        n += 1;
        if n > MAX_OBJECTS {
            out.add("limits", None, None, || format!("Only the first {MAX_OBJECTS} objects were searched"));
            break;
        }
        let page = page_of.get(&r).copied();
        let (d, stream): (&Dict, bool) = match &*o {
            Object::Dict(d) => (d, false),
            Object::Stream(s) => (&s.dict, true),
            _ => continue,
        };
        if stream {
            let st = d.name(b"Subtype");
            if st == Some(b"PS") || (st == Some(b"Form") && d.name(b"Subtype2") == Some(b"PS")) {
                out.add("postscript", None, Some(r), || format!("Object {} is a PostScript XObject", r.num));
            }
            if st == Some(b"Form") && d.contains(b"Ref") {
                out.add("external-content", None, Some(r), || format!("Object {} is a reference XObject (external page content)", r.num));
            }
            if matches!(st, Some(b"Image") | Some(b"Form")) && d.contains(b"OPI") {
                out.add("opi", None, Some(r), || format!("Object {} has an OPI dictionary", r.num));
            }
            if d.contains(b"F") || d.contains(b"FFilter") {
                out.add("external-content", None, Some(r), || format!("Object {} keeps its data in an external file", r.num));
            }
            continue;
        }
        if d.contains(b"EF") && (d.name(b"Type") == Some(b"Filespec") || d.contains(b"F") || d.contains(b"UF")) {
            out.add("embedded-files", page, Some(r), || format!("Object {} is an embedded file", r.num));
        }
        if d.name(b"Subtype") == Some(b"FileAttachment") {
            out.add("embedded-files", page, Some(r), || format!("Object {} is a file attachment annotation", r.num));
        }
        if d.name(b"S") == Some(b"JavaScript") && is_action(d) {
            out.add("actions", page, Some(r), || format!("Object {} is a JavaScript action", r.num));
        }
    }
}

#[cfg(test)]
#[path = "pdfx_tests.rs"]
mod tests;
