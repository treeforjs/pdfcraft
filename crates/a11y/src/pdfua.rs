//! PDF/UA validation: the machine-checkable requirements of PDF/UA-1 (ISO 14289-1) and
//! PDF/UA-2 (ISO 14289-2), reported by Matterhorn Protocol checkpoint where one applies.
//!
//! [`verify_pdfua`] reads the document only. It reuses the full check's structure tree and
//! content scan (tags, artifacts, MCIDs, fonts) and adds the PDF/UA-specific rules:
//! identification, role mapping, namespaces, the parent tree, annotations, optional content,
//! embedded files and encryption. Checkpoints that need a person (reading order, whether alt
//! text is appropriate, colour, …) are listed in [`UA_NOT_CHECKED`] and never reported.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use pdfcraft_cos::{Dict, Document, ObjRef, Object};

use crate::Rule;
use crate::content::{Scan, catalog, full_name, maps_to_unicode, text_of};
use crate::structure::Tree;

/// The PDF/UA part to verify.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UaPart {
    /// ISO 14289-1 (PDF 1.7).
    Ua1,
    /// ISO 14289-2 (PDF 2.0).
    Ua2,
}

impl UaPart {
    pub fn number(self) -> u8 {
        match self {
            UaPart::Ua1 => 1,
            UaPart::Ua2 => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            UaPart::Ua1 => "PDF/UA-1",
            UaPart::Ua2 => "PDF/UA-2",
        }
    }

    pub fn from_number(n: i64) -> Option<UaPart> {
        match n {
            1 => Some(UaPart::Ua1),
            2 => Some(UaPart::Ua2),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UaSeverity {
    /// Breaks a requirement: the document does not conform.
    Error,
    /// The check could not be completed (limits reached); conformance is unaffected.
    Warning,
}

impl UaSeverity {
    pub fn id(self) -> &'static str {
        match self {
            UaSeverity::Error => "error",
            UaSeverity::Warning => "warning",
        }
    }
}

/// One checkpoint the validator decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UaCheckpoint {
    /// The Matterhorn Protocol number (`06-002`) or, where none applies or the number is not
    /// certain, a short code (`struct-tree-root`).
    pub id: &'static str,
    pub description: &'static str,
    pub ua1: bool,
    pub ua2: bool,
}

const fn cp(id: &'static str, ua1: bool, ua2: bool, description: &'static str) -> UaCheckpoint {
    UaCheckpoint { id, description, ua1, ua2 }
}

/// Every checkpoint [`verify_pdfua`] decides, in report order.
pub const UA_CHECKPOINTS: &[UaCheckpoint] = &[
    cp("06-001", true, true, "The catalog has an XMP metadata stream"),
    cp("06-002", true, true, "The XMP metadata holds the PDF/UA identifier (pdfuaid:part) for the part checked"),
    cp("ua2-rev", false, true, "The XMP metadata holds the PDF/UA revision year (pdfuaid:rev)"),
    cp("06-003", true, true, "The XMP metadata holds a dc:title"),
    cp("07-001", true, true, "ViewerPreferences has a DisplayDocTitle entry"),
    cp("07-002", true, true, "DisplayDocTitle is true"),
    cp("ua2-version", false, true, "The file is PDF 2.0"),
    cp("struct-tree-root", true, true, "The catalog has a structure tree (StructTreeRoot)"),
    cp("mark-info", true, true, "MarkInfo Marked is true"),
    cp("01-007", true, false, "MarkInfo Suspects is not true"),
    cp("struct-tree-shape", true, true, "Every structure element has one parent (no loops or shared elements)"),
    cp("01-005", true, true, "All content is tagged or marked as an artifact"),
    cp("01-003", true, true, "No artifact is marked inside tagged content"),
    cp("01-004", true, true, "No tagged content is marked inside an artifact"),
    cp("mcid-unreferenced", true, true, "Every marked-content id on a page belongs to a structure element"),
    cp("struct-missing-content", true, true, "Every marked-content id the structure references exists on its page"),
    cp("parent-tree", true, true, "The parent tree maps every page's MCIDs and every tagged annotation to a structure element"),
    cp("02-001", true, true, "Non-standard structure types are role-mapped to a standard type"),
    cp("02-002", true, true, "Role mapping has no cycles"),
    cp("02-003", true, false, "Standard structure types are not remapped"),
    cp("ua2-namespace", false, true, "Structure types belong to (or map to) the PDF 2.0 or MathML namespace"),
    cp("09-004", true, true, "Table elements follow the table structure (Table > TR > TH/TD, THead/TBody/TFoot, Caption)"),
    cp("09-005", true, true, "List elements follow the list structure (L > LI > Lbl/LBody)"),
    cp("09-006", true, true, "TOCI elements are inside a TOC"),
    cp("10-001", true, true, "Every font maps its character codes to Unicode"),
    cp("fonts-embedded", true, true, "Fonts that paint visible text are embedded"),
    cp("11-001", true, true, "The natural language of the content can be determined (Lang)"),
    cp("13-003", true, true, "Figures have alternate text (Alt) or replacement text (ActualText)"),
    cp("17-002", true, false, "Formulas have alternate text"),
    cp("14-002", true, false, "With numbered headings, the first heading is H1"),
    cp("14-003", true, false, "Numbered heading levels are not skipped"),
    cp("14-006", true, false, "No element holds more than one H"),
    cp("14-007", true, false, "The document doesn't mix H and numbered headings"),
    cp("15-003", true, true, "TH cells have a Scope where the table doesn't use Headers"),
    cp("19-003", true, false, "Note elements have an ID"),
    cp("19-004", true, false, "Note IDs are unique"),
    cp("20-001", true, true, "Optional content configurations (Configs) have a Name"),
    cp("20-002", true, true, "The default optional content configuration (D) has a Name"),
    cp("20-003", true, true, "No optional content configuration has an AS entry"),
    cp("21-001", true, true, "Embedded file specifications have F and UF"),
    cp("25-001", true, true, "No XFA form requires dynamic rendering"),
    cp("26-001", true, true, "An encryption dictionary has a P entry"),
    cp("26-002", true, true, "Encryption allows assistive technology to read the content (P bit 10)"),
    cp("28-002", true, true, "Annotations other than Widget, Link and PrinterMark are inside an Annot element"),
    cp("28-004", true, true, "Annotations other than Widget and Link have Contents or alternate text"),
    cp("28-005", true, true, "Form fields have a TU (tooltip) or alternate text"),
    cp("28-007", true, false, "There are no TrapNet annotations"),
    cp("28-008", true, true, "Pages with annotations have a Tabs entry"),
    cp("28-009", true, true, "Pages with annotations tab in structure order (Tabs /S)"),
    cp("28-010", true, true, "Widget annotations are inside a Form element"),
    cp("28-011", true, true, "Link annotations are inside a Link element"),
    cp("28-012", true, true, "Link annotations have Contents (an alternate description)"),
    cp("30-001", true, true, "There are no reference XObjects"),
    cp("ps-xobject", true, true, "There are no PostScript XObjects"),
    cp("limits", true, true, "The validator read the whole document (limits not reached)"),
];

/// What a person must judge, or what isn't checked yet; never reported as passed or failed.
pub const UA_NOT_CHECKED: &[&str] = &[
    "01-001, 01-002, 01-006: whether artifacts and real content are told apart correctly, and tags fit their content",
    "02-004, 09-001 to 09-003: whether tags are semantically appropriate and in logical reading order",
    "03 flickering, 04 colour and contrast, 05 sound, 12 stretchable characters, 18 headers and footers",
    "06-004: whether dc:title identifies the document; 11-007: whether the language is right",
    "08: OCR text quality",
    "13-001, 13-002, 13-004 to 13-008: whether graphics are tagged as figures and their alternate text is appropriate",
    "14-001, 14-004, 14-005, 15-001, 15-002, 15-004, 16, 17-001, 17-003, 19-001, 19-002: whether headings, tables, lists, formulas and notes are tagged where they should be",
    "22 article threads, 23 signatures, 24 non-interactive forms, 27 navigation, 29 actions",
    "Not yet machine-checked: 09-007/09-008 Ruby and Warichu, 11-002 to 11-006 language of Alt, outlines, annotations and fields, 15-005 ambiguous headers, 30-002 shared form XObjects with MCIDs, the remaining 31 font checks (CMaps, widths, glyphs), the 28 rules for other annotation details, the UA-2 heading and formula (MathML) rules",
];

/// Findings beyond this many per checkpoint are only counted.
pub const MAX_UA_FINDINGS: usize = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct UaFinding {
    pub checkpoint: &'static str,
    pub severity: UaSeverity,
    pub message: String,
    /// 0-based page, when the problem is on a page.
    pub page: Option<usize>,
    /// The object at fault, when there is one.
    pub object: Option<ObjRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UaOptions {
    pub part: UaPart,
    /// Findings kept per checkpoint (1 to [`MAX_UA_FINDINGS`]); the rest are counted.
    pub max_findings: usize,
}

impl Default for UaOptions {
    fn default() -> Self {
        Self { part: UaPart::Ua1, max_findings: 100 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UaReport {
    pub part: UaPart,
    /// The `pdfuaid:part` the document declares.
    pub declared: Option<u8>,
    /// Findings, at most `max_findings` per checkpoint, in checkpoint order.
    pub findings: Vec<UaFinding>,
    /// Checkpoint → how many problems were found (including those not kept).
    pub counts: BTreeMap<&'static str, usize>,
    /// The checkpoints that were run for this part.
    pub checked: Vec<&'static str>,
}

impl UaReport {
    /// No requirement checked here is broken. Checkpoints that need a person
    /// ([`UA_NOT_CHECKED`]) are not part of this answer.
    pub fn conforms(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == UaSeverity::Error)
    }

    pub fn count(&self, checkpoint: &str) -> usize {
        self.counts.get(checkpoint).copied().unwrap_or(0)
    }

    /// The checkpoints with at least one problem.
    pub fn failed(&self) -> Vec<&'static str> {
        self.counts.iter().filter(|(_, n)| **n > 0).map(|(id, _)| *id).collect()
    }
}

struct Out {
    cap: usize,
    part: UaPart,
    findings: Vec<UaFinding>,
    counts: BTreeMap<&'static str, usize>,
}

impl Out {
    fn applies(&self, id: &str) -> bool {
        UA_CHECKPOINTS.iter().any(|c| c.id == id && if self.part == UaPart::Ua1 { c.ua1 } else { c.ua2 })
    }

    fn add(&mut self, id: &'static str, page: Option<usize>, object: Option<ObjRef>, message: impl FnOnce() -> String) {
        if !self.applies(id) {
            return;
        }
        let n = self.counts.entry(id).or_insert(0);
        *n = n.saturating_add(1);
        if *n <= self.cap {
            let severity = if id == "limits" { UaSeverity::Warning } else { UaSeverity::Error };
            self.findings.push(UaFinding { checkpoint: id, severity, message: message(), page, object });
        }
    }
}

/// Standard structure types of PDF 1.7 (ISO 32000-1 §14.8.4), the set PDF/UA-1 uses.
const STANDARD_17: &[&[u8]] = &[
    b"Document",
    b"Part",
    b"Art",
    b"Sect",
    b"Div",
    b"BlockQuote",
    b"Caption",
    b"TOC",
    b"TOCI",
    b"Index",
    b"NonStruct",
    b"Private",
    b"P",
    b"H",
    b"H1",
    b"H2",
    b"H3",
    b"H4",
    b"H5",
    b"H6",
    b"L",
    b"LI",
    b"Lbl",
    b"LBody",
    b"Table",
    b"TR",
    b"TH",
    b"TD",
    b"THead",
    b"TBody",
    b"TFoot",
    b"Span",
    b"Quote",
    b"Note",
    b"Reference",
    b"BibEntry",
    b"Code",
    b"Link",
    b"Annot",
    b"Ruby",
    b"RB",
    b"RT",
    b"RP",
    b"Warichu",
    b"WT",
    b"WP",
    b"Figure",
    b"Formula",
    b"Form",
];

/// PDF 1.7 types with no PDF 2.0 counterpart (ISO 32000-2 dropped them from its namespace).
const NOT_IN_PDF20: &[&[u8]] = &[b"Art", b"BlockQuote", b"Private", b"Quote", b"Note", b"Reference", b"BibEntry", b"Code"];

const NS_PDF17: &[u8] = b"http://iso.org/pdf/ssn";
const NS_PDF20: &[u8] = b"http://iso.org/pdf2/ssn";
const NS_MATHML: &[u8] = b"http://www.w3.org/1998/Math/MathML";

/// A type of the PDF 2.0 standard structure namespace.
fn in_pdf20(t: &[u8]) -> bool {
    if let [b'H', digits @ ..] = t
        && !digits.is_empty()
        && digits.iter().all(u8::is_ascii_digit)
        && digits.first() != Some(&b'0')
    {
        return true;
    }
    crate::structure::is_standard(t) && !NOT_IN_PDF20.contains(&t)
}

fn name_text(n: &[u8]) -> String {
    String::from_utf8_lossy(n).into_owned()
}

fn dict_of(doc: &Document, d: &Dict, key: &[u8]) -> Option<Dict> {
    d.get(key).map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned())
}

fn array_of(doc: &Document, d: &Dict, key: &[u8]) -> Vec<Object> {
    d.get(key).map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()).unwrap_or_default()
}

fn is_true(doc: &Document, d: &Dict, key: &[u8]) -> Option<bool> {
    d.get(key).map(|o| matches!(&*doc.resolve(o), Object::Bool(true)))
}

/// Verify `doc` against the machine-checkable requirements of PDF/UA-1 or PDF/UA-2.
pub fn verify_pdfua(doc: &Document, options: &UaOptions) -> UaReport {
    let part = options.part;
    let mut out = Out { cap: options.max_findings.clamp(1, MAX_UA_FINDINGS), part, findings: Vec::new(), counts: BTreeMap::new() };
    let pages = pdfcraft_model::pages(doc);
    let all: Vec<usize> = (0..pages.len()).collect();
    let tree = Tree::read(doc, &pages);
    let scan = Scan::run(doc, &pages, &all);
    let cat = catalog(doc);

    let declared = metadata(doc, part, &mut out);
    document(doc, &cat, part, &mut out);
    if tree.exists {
        structure(doc, &cat, &tree, part, &mut out);
        marked_content(doc, &pages, &tree, &scan, &cat, &mut out);
    }
    page_content(doc, &tree, &scan, &mut out);
    language(doc, &cat, &tree, &scan, &mut out);
    annotations(doc, &pages, &tree, &mut out);
    objects(doc, &mut out);

    let checked = UA_CHECKPOINTS.iter().filter(|c| if part == UaPart::Ua1 { c.ua1 } else { c.ua2 }).map(|c| c.id).collect::<Vec<_>>();
    let order = |id: &str| UA_CHECKPOINTS.iter().position(|c| c.id == id).unwrap_or(usize::MAX);
    out.findings.sort_by_key(|f| order(f.checkpoint));
    UaReport { part, declared, findings: out.findings, counts: out.counts, checked }
}

/// Identification and title in the XMP metadata (Matterhorn 06).
fn metadata(doc: &Document, part: UaPart, out: &mut Out) -> Option<u8> {
    let Some(x) = pdfcraft_preflight::xmp_metadata(doc) else {
        out.add("06-001", None, None, || "The catalog has no XMP metadata stream (Metadata)".into());
        return None;
    };
    let declared = pdfcraft_preflight::xmp_value(&x, "pdfuaid:part").and_then(|p| p.trim().parse::<u8>().ok());
    match declared {
        None => out.add("06-002", None, None, || "The XMP metadata has no PDF/UA identifier (pdfuaid:part)".into()),
        Some(n) if n != part.number() => out.add("06-002", None, None, || format!("The XMP metadata declares PDF/UA-{n}, not {}", part.label())),
        _ => {}
    }
    if part == UaPart::Ua2 {
        let rev = pdfcraft_preflight::xmp_value(&x, "pdfuaid:rev").map(|r| r.trim().to_owned());
        if !rev.as_deref().is_some_and(|r| r.len() == 4 && r.bytes().all(|b| b.is_ascii_digit())) {
            out.add("ua2-rev", None, None, || "The XMP metadata has no PDF/UA revision year (pdfuaid:rev, e.g. 2024)".into());
        }
    }
    if !pdfcraft_preflight::xmp_value(&x, "dc:title").is_some_and(|t| !t.trim().is_empty()) {
        out.add("06-003", None, None, || "The XMP metadata has no dc:title".into());
    }
    declared
}

/// Catalog-level requirements: title display, tagging marks, version, encryption, optional
/// content and XFA.
fn document(doc: &Document, cat: &Dict, part: UaPart, out: &mut Out) {
    match dict_of(doc, cat, b"ViewerPreferences").and_then(|v| is_true(doc, &v, b"DisplayDocTitle")) {
        None => out.add("07-001", None, None, || "ViewerPreferences has no DisplayDocTitle entry".into()),
        Some(false) => out.add("07-002", None, None, || "DisplayDocTitle isn't true: the window shows the file name, not the title".into()),
        Some(true) => {}
    }
    if !cat.contains(b"StructTreeRoot") || dict_of(doc, cat, b"StructTreeRoot").is_none() {
        out.add("struct-tree-root", None, None, || "The document has no structure tree (StructTreeRoot): it isn't tagged".into());
    }
    let mark = dict_of(doc, cat, b"MarkInfo").unwrap_or_default();
    if is_true(doc, &mark, b"Marked") != Some(true) {
        out.add("mark-info", None, None, || "MarkInfo Marked isn't true: the document isn't marked as tagged".into());
    }
    if is_true(doc, &mark, b"Suspects") == Some(true) {
        out.add("01-007", None, None, || "MarkInfo Suspects is true: the tags are marked as unreliable".into());
    }
    if part == UaPart::Ua2 {
        let header = doc.version().trim().starts_with('2');
        let catalog = cat.get(b"Version").map(|v| doc.resolve(v)).and_then(|v| v.as_name().map(|n| n.starts_with(b"2"))).unwrap_or(false);
        if !header && !catalog {
            let v = doc.version().to_owned();
            out.add("ua2-version", None, None, || format!("The file is PDF {v}; PDF/UA-2 needs PDF 2.0"));
        }
    }
    if doc.security().is_some() {
        let enc = doc.trailer().get(b"Encrypt").map(|e| doc.resolve(e)).and_then(|e| e.as_dict().cloned());
        let p = match &enc {
            Some(e) => e.get(b"P").map(|p| doc.resolve(p)).and_then(|p| p.as_int()),
            None => doc.permissions().map(|p| i64::from(p.bits)),
        };
        match p {
            None => out.add("26-001", None, None, || "The file is encrypted but its encryption dictionary has no P (permissions) entry".into()),
            Some(p) if (p >> 9) & 1 == 0 => {
                out.add("26-002", None, None, || "The security settings don't allow assistive technology to read the content (P bit 10)".into())
            }
            _ => {}
        }
    }
    if let Some(oc) = dict_of(doc, cat, b"OCProperties") {
        let named = |d: &Dict| text_of(doc, d, b"Name").is_some();
        if let Some(d) = dict_of(doc, &oc, b"D") {
            if !named(&d) {
                out.add("20-002", None, None, || "The default optional content configuration (D) has no Name".into());
            }
            if d.contains(b"AS") {
                out.add("20-003", None, None, || "The default optional content configuration (D) has an AS entry".into());
            }
        }
        for (i, c) in array_of(doc, &oc, b"Configs").iter().take(10_000).enumerate() {
            let Some(c) = doc.resolve(c).as_dict().cloned() else { continue };
            if !named(&c) {
                out.add("20-001", None, None, || format!("Optional content configuration {} in Configs has no Name", i + 1));
            }
            if c.contains(b"AS") {
                out.add("20-003", None, None, || format!("Optional content configuration {} in Configs has an AS entry", i + 1));
            }
        }
    }
    if let Some(form) = dict_of(doc, cat, b"AcroForm")
        && let Some(xfa) = form.get(b"XFA")
        && xfa_requires_dynamic_render(doc, xfa)
    {
        out.add("25-001", None, None, || "The XFA form requires dynamic rendering (dynamicRender is required)".into());
    }
}

/// XFA packets beyond this many bytes are not searched.
const MAX_XFA: usize = 32 << 20;

fn xfa_requires_dynamic_render(doc: &Document, xfa: &Object) -> bool {
    let streams: Vec<Object> = match &*doc.resolve(xfa) {
        Object::Array(a) => a.iter().skip(1).step_by(2).cloned().collect(),
        _ => vec![xfa.clone()],
    };
    let mut total = 0usize;
    for s in streams.iter().take(256) {
        let Object::Stream(st) = &*doc.resolve(s) else { continue };
        let Ok(data) = st.decoded_within(MAX_XFA.saturating_sub(total)) else { continue };
        total = total.saturating_add(data.len());
        let text: String = String::from_utf8_lossy(&data).chars().filter(|c| !c.is_whitespace()).collect();
        if text.contains("<dynamicRender>required</dynamicRender>") {
            return true;
        }
        if total >= MAX_XFA {
            break;
        }
    }
    false
}

/// How a structure type resolves through role maps.
enum Resolved {
    Ok,
    /// A PDF 1.7 type with no PDF 2.0 counterpart (UA-2).
    NotInPdf20(Vec<u8>),
    /// Mapping ends at a type that isn't standard.
    Dead(Vec<u8>),
    Cycle,
}

/// PDF/UA-1: follow the role map to a PDF 1.7 standard type.
fn resolve_17(doc: &Document, role_map: &Dict, ty: &[u8]) -> Resolved {
    let mut t = ty.to_vec();
    let mut seen = HashSet::new();
    for _ in 0..64 {
        if STANDARD_17.contains(&t.as_slice()) {
            return Resolved::Ok;
        }
        if !seen.insert(t.clone()) {
            return Resolved::Cycle;
        }
        match role_map.get(&t).map(|o| doc.resolve(o)).and_then(|o| o.as_name().map(<[u8]>::to_vec)) {
            Some(n) => t = n,
            None => return Resolved::Dead(t),
        }
    }
    Resolved::Cycle
}

/// A namespace dictionary: its URI and role map.
fn namespace(doc: &Document, r: ObjRef) -> (Vec<u8>, Dict) {
    let d = doc.get(r).as_dict().cloned().unwrap_or_default();
    let uri = d.get(b"NS").map(|o| doc.resolve(o)).and_then(|o| o.as_string().map(|s| s.bytes.clone())).unwrap_or_default();
    let map = dict_of(doc, &d, b"RoleMapNS").unwrap_or_default();
    (uri, map)
}

/// PDF/UA-2: follow namespace role maps (and the root role map for the default namespace) to
/// the PDF 2.0 or MathML namespace.
fn resolve_20(doc: &Document, role_map: &Dict, ty: &[u8], ns: Option<ObjRef>) -> Resolved {
    let (mut t, mut ns) = (ty.to_vec(), ns);
    let mut seen = HashSet::new();
    for _ in 0..64 {
        if !seen.insert((t.clone(), ns)) {
            return Resolved::Cycle;
        }
        let (uri, map) = match ns {
            Some(r) => namespace(doc, r),
            None => (NS_PDF17.to_vec(), role_map.clone()),
        };
        if uri == NS_MATHML || (uri == NS_PDF20 && in_pdf20(&t)) {
            return Resolved::Ok;
        }
        let pdf17 = uri == NS_PDF17;
        if pdf17 && in_pdf20(&t) {
            return Resolved::Ok;
        }
        let next = map.get(&t).map(|o| doc.resolve(o)).and_then(|o| match &*o {
            Object::Name(n) => Some((n.to_vec(), None)),
            Object::Array(a) => {
                let n = a.first().and_then(Object::as_name).map(<[u8]>::to_vec)?;
                Some((n, a.get(1).and_then(Object::as_ref)))
            }
            _ => None,
        });
        match next {
            Some((n, next_ns)) => (t, ns) = (n, next_ns),
            None if pdf17 && STANDARD_17.contains(&t.as_slice()) => return Resolved::NotInPdf20(t),
            None => return Resolved::Dead(t),
        }
    }
    Resolved::Cycle
}

fn elem_label(tree: &Tree, i: usize) -> String {
    let Some(e) = tree.elems.get(i) else { return "element".into() };
    let ty = name_text(&e.raw);
    let at = e.page.map(|p| format!(" on page {}", p + 1)).unwrap_or_default();
    match e.obj {
        Some(r) => format!("{ty} element (object {}){at}", r.num),
        None => format!("{ty} element{at}"),
    }
}

/// Structure tree rules: shape, role mapping, namespaces, tables, lists, TOC, headings,
/// figures, formulas and notes.
fn structure(doc: &Document, cat: &Dict, tree: &Tree, part: UaPart, out: &mut Out) {
    if tree.revisits > 0 {
        let n = tree.revisits;
        out.add("struct-tree-shape", None, tree.first_revisit, || {
            format!("{n} structure reference{} lead back to an element already in the tree (a loop or an element with two parents)", plural(n))
        });
    }
    if tree.truncated {
        out.add("limits", None, None, || "The structure tree is too deep or too large to read completely".into());
    }
    let root = dict_of(doc, cat, b"StructTreeRoot").unwrap_or_default();
    let role_map = dict_of(doc, &root, b"RoleMap").unwrap_or_default();

    // Role mapping, once per distinct type (and namespace).
    if part == UaPart::Ua1 {
        for (k, _) in role_map.iter().take(10_000) {
            if STANDARD_17.contains(&k.as_slice()) {
                let k = name_text(k);
                out.add("02-003", None, None, || format!("The role map remaps the standard type {k}"));
            }
        }
    }
    let mut types: BTreeMap<(Vec<u8>, Option<ObjRef>), usize> = BTreeMap::new();
    for (i, e) in tree.elems.iter().enumerate() {
        let ns = if part == UaPart::Ua2 { e.ns } else { None };
        types.entry((e.raw.clone(), ns)).or_insert(i);
    }
    for ((ty, ns), first) in &types {
        let r = match part {
            UaPart::Ua1 => resolve_17(doc, &role_map, ty),
            UaPart::Ua2 => resolve_20(doc, &role_map, ty, *ns),
        };
        let e = tree.elems.get(*first);
        let (page, obj) = (e.and_then(|e| e.page), e.and_then(|e| e.obj));
        let t = name_text(ty);
        match r {
            Resolved::Ok => {}
            Resolved::Cycle => out.add("02-002", page, obj, || format!("The role mapping of {t} is circular")),
            Resolved::Dead(end) => {
                let end = name_text(&end);
                let what = if end == t {
                    format!("{t} isn't a standard structure type and isn't role-mapped to one")
                } else {
                    format!("{t} maps to {end}, which isn't a standard structure type")
                };
                out.add("02-001", page, obj, || what);
            }
            Resolved::NotInPdf20(end) => {
                let end = name_text(&end);
                out.add("ua2-namespace", page, obj, || format!("{t} resolves to {end}, a PDF 1.7 type with no PDF 2.0 counterpart"));
            }
        }
    }

    let order = tree.preorder();
    let ty = |i: usize| tree.elems.get(i).map(|e| e.ty.as_slice()).unwrap_or(b"");
    let parent_ty = |i: usize| tree.elems.get(i).and_then(|e| e.parent).map(ty).unwrap_or(b"");
    let at = |i: usize| tree.elems.get(i).map(|e| (e.page, e.obj)).unwrap_or((None, None));

    // Tables (09-004) and lists (09-005): the full check's rules, plus the allowed children.
    for rule in [Rule::TableRows, Rule::TableCells] {
        for f in tree.rule(rule) {
            out.add("09-004", f.page, None, || f.message);
        }
    }
    for &i in &order {
        let (page, obj) = at(i);
        match ty(i) {
            b"Table" => {
                for &k in tree.elems.get(i).map(|e| e.children.as_slice()).unwrap_or(&[]) {
                    if !matches!(ty(k), b"TR" | b"THead" | b"TBody" | b"TFoot" | b"Caption") {
                        let (what, inside) = (elem_label(tree, k), elem_label(tree, i));
                        out.add("09-004", page, obj, || format!("{what} is inside {inside}; a table holds only TR, THead, TBody, TFoot and Caption"));
                    }
                }
            }
            b"TR" => {
                for &k in tree.elems.get(i).map(|e| e.children.as_slice()).unwrap_or(&[]) {
                    if !matches!(ty(k), b"TH" | b"TD") {
                        let what = elem_label(tree, k);
                        out.add("09-004", page, obj, || format!("{what} is inside a TR; a row holds only TH and TD"));
                    }
                }
            }
            b"TOCI" if !matches!(parent_ty(i), b"TOC") => {
                let what = elem_label(tree, i);
                out.add("09-006", page, obj, || format!("{what} isn't inside a TOC"));
            }
            _ => {}
        }
    }
    for rule in [Rule::ListItems, Rule::LblLBody] {
        for f in tree.rule(rule) {
            out.add("09-005", f.page, None, || f.message);
        }
    }

    // Alternate text (13-003, 17-002).
    for f in tree.rule(Rule::FiguresAltText) {
        out.add("13-003", f.page, None, || f.message);
    }
    for f in tree.rule(Rule::OtherElementsAltText) {
        out.add("17-002", f.page, None, || f.message);
    }

    // Headings (14-002, 14-003, 14-006, 14-007).
    let level = |i: usize| match ty(i) {
        [b'H', d @ b'1'..=b'6'] => Some(d - b'0'),
        _ => None,
    };
    let mut prev = 0u8;
    let (mut uses_h, mut uses_hn) = (false, false);
    for &i in &order {
        let (page, obj) = at(i);
        if ty(i) == b"H" {
            uses_h = true;
        }
        let Some(l) = level(i) else { continue };
        uses_hn = true;
        let what = elem_label(tree, i);
        if prev == 0 && l != 1 {
            out.add("14-002", page, obj, || format!("The first numbered heading is {what}, not an H1"));
        } else if prev > 0 && l > prev.saturating_add(1) {
            out.add("14-003", page, obj, || format!("{what} follows an H{prev}: heading levels are skipped"));
        }
        prev = l;
    }
    for &i in &order {
        let n = tree.elems.get(i).map(|e| e.children.iter().filter(|k| ty(**k) == b"H").count()).unwrap_or(0);
        if n > 1 {
            let (page, obj) = at(i);
            let what = elem_label(tree, i);
            out.add("14-006", page, obj, || format!("{what} holds {n} H elements; use one per section"));
        }
    }
    if uses_h && uses_hn {
        out.add("14-007", None, None, || "The document uses both H and numbered headings (H1–H6)".into());
    }

    // Table headers (15-003): without Headers attributes, every TH needs a Scope.
    for &i in &order {
        if ty(i) != b"Table" {
            continue;
        }
        let cells = tree.table_cells(i);
        if cells.iter().any(|(_, c)| tree.attr(*c, b"Headers").is_some()) {
            continue;
        }
        for (_, c) in cells {
            if ty(c) == b"TH" && tree.attr(c, b"Scope").is_none() {
                let (page, obj) = at(c);
                let what = elem_label(tree, c);
                out.add("15-003", page, obj, || format!("{what} has no Scope attribute and the table doesn't use Headers"));
            }
        }
    }

    // Notes (19-003, 19-004).
    let mut ids: HashMap<Vec<u8>, usize> = HashMap::new();
    for &i in &order {
        if ty(i) != b"Note" {
            continue;
        }
        let (page, obj) = at(i);
        let what = elem_label(tree, i);
        match tree.elems.get(i).and_then(|e| e.id.clone()) {
            None => out.add("19-003", page, obj, || format!("{what} has no ID")),
            Some(id) => {
                let n = ids.entry(id.clone()).or_insert(0);
                *n += 1;
                if *n == 2 {
                    let id = name_text(&id);
                    out.add("19-004", page, obj, || format!("{what} has the ID {id:?}, which another Note already uses"));
                }
            }
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Parent-tree entries read beyond this many are ignored.
const MAX_PARENT_TREE: usize = 2_000_000;

/// The entries of a number tree (bounded and cycle-safe).
fn number_tree(doc: &Document, node: &Object, out: &mut HashMap<i64, Object>, seen: &mut HashSet<ObjRef>, depth: usize) {
    if depth > 32 || out.len() >= MAX_PARENT_TREE {
        return;
    }
    if let Some(r) = node.as_ref()
        && !seen.insert(r)
    {
        return;
    }
    let Some(d) = doc.resolve(node).as_dict().cloned() else { return };
    let nums = array_of(doc, &d, b"Nums");
    for pair in nums.chunks(2) {
        if let [k, v] = pair
            && let Some(k) = doc.resolve(k).as_int()
        {
            if out.len() >= MAX_PARENT_TREE {
                return;
            }
            out.insert(k, v.clone());
        }
    }
    for kid in array_of(doc, &d, b"Kids") {
        number_tree(doc, &kid, out, seen, depth + 1);
    }
}

/// Whether an object is (a reference to) a structure element.
fn is_elem(doc: &Document, o: &Object) -> bool {
    doc.resolve(o).as_dict().is_some_and(|d| d.contains(b"S"))
}

/// MCIDs: page content against the structure tree and the parent tree.
fn marked_content(doc: &Document, pages: &[pdfcraft_model::Page], tree: &Tree, scan: &Scan, cat: &Dict, out: &mut Out) {
    let mut referenced: BTreeMap<usize, BTreeSet<i64>> = BTreeMap::new();
    for e in &tree.elems {
        for (page, mcid) in &e.mcids {
            let Some(p) = page else { continue };
            referenced.entry(*p).or_default().insert(*mcid);
            if !scan.mcids.get(p).is_some_and(|s| s.contains(mcid)) {
                let (p, m, obj) = (*p, *mcid, e.obj);
                let ty = name_text(&e.raw);
                out.add("struct-missing-content", Some(p), obj, || {
                    format!("{ty} element refers to MCID {m} on page {}, which the page doesn't have", p + 1)
                });
            }
        }
    }
    if !tree.truncated {
        for (p, ids) in &scan.mcids {
            let missing: Vec<i64> = ids.iter().filter(|m| !referenced.get(p).is_some_and(|r| r.contains(m))).copied().collect();
            if let Some(first) = missing.first() {
                let (p, n, first) = (*p, missing.len(), *first);
                out.add("mcid-unreferenced", Some(p), None, || {
                    format!("Page {}: {n} marked-content id{} (first MCID {first}) belong to no structure element", p + 1, plural(n))
                });
            }
        }
    }

    let root = dict_of(doc, cat, b"StructTreeRoot").unwrap_or_default();
    let mut parents = HashMap::new();
    if let Some(pt) = root.get(b"ParentTree") {
        number_tree(doc, pt, &mut parents, &mut HashSet::new(), 0);
    }
    for (p, ids) in &scan.mcids {
        let Some(page) = pages.get(*p) else { continue };
        let key = page.dict.get(b"StructParents").map(|o| doc.resolve(o)).and_then(|o| o.as_int());
        let Some(key) = key else {
            let p = *p;
            out.add("parent-tree", Some(p), Some(page.obj), || format!("Page {} has marked content but no StructParents entry", p + 1));
            continue;
        };
        let Some(entry) = parents.get(&key) else {
            let p = *p;
            out.add("parent-tree", Some(p), Some(page.obj), || format!("Page {}: the parent tree has no entry {key} for its marked content", p + 1));
            continue;
        };
        let list = doc.resolve(entry).as_array().cloned().unwrap_or_default();
        let bad: Vec<i64> =
            ids.iter().filter(|m| usize::try_from(**m).ok().and_then(|m| list.get(m)).is_none_or(|o| !is_elem(doc, o))).copied().collect();
        if let Some(first) = bad.first() {
            let (p, n, first) = (*p, bad.len(), *first);
            out.add("parent-tree", Some(p), Some(page.obj), || {
                format!("Page {}: {n} MCID{} (first {first}) have no structure element in the parent tree", p + 1, plural(n))
            });
        }
    }
    for (pi, page) in pages.iter().enumerate() {
        for (r, a) in page_annots(doc, &page.dict) {
            let Some(key) = a.get(b"StructParent").map(|o| doc.resolve(o)).and_then(|o| o.as_int()) else { continue };
            if !parents.get(&key).is_some_and(|o| is_elem(doc, o)) {
                let st = name_text(a.name(b"Subtype").unwrap_or(b"Unknown"));
                out.add("parent-tree", Some(pi), r, || {
                    format!("{st} annotation on page {} has StructParent {key}, which the parent tree doesn't map to an element", pi + 1)
                });
            }
        }
    }
}

/// Page content: untagged content, artifact nesting, fonts.
fn page_content(doc: &Document, tree: &Tree, scan: &Scan, out: &mut Out) {
    for (p, n) in &scan.untagged {
        let (p, n) = (*p, *n);
        let why = if tree.exists { "" } else { " (the document isn't tagged)" };
        out.add("01-005", Some(p), None, || format!("Page {}: {n} content item{} neither tagged nor marked as an artifact{why}", p + 1, plural(n)));
    }
    for (p, n) in &scan.artifact_in_tagged {
        let (p, n) = (*p, *n);
        out.add("01-003", Some(p), None, || format!("Page {}: {n} artifact{} marked inside tagged content", p + 1, plural(n)));
    }
    for (p, n) in &scan.tagged_in_artifact {
        let (p, n) = (*p, *n);
        out.add("01-004", Some(p), None, || format!("Page {}: {n} tagged content item{} marked inside an artifact", p + 1, plural(n)));
    }
    let font_name = |f: &Dict| f.name(b"BaseFont").map(name_text).unwrap_or_else(|| "(unnamed)".into());
    let mut seen = HashSet::new();
    for (p, fonts) in &scan.fonts {
        for r in fonts {
            if seen.insert(*r)
                && let Some(f) = doc.get(*r).as_dict()
                && !maps_to_unicode(doc, f)
            {
                let (p, name) = (*p, font_name(f));
                out.add("10-001", Some(p), Some(*r), || format!("Font {name} on page {} doesn't map its character codes to Unicode", p + 1));
            }
        }
    }
    let mut seen = HashSet::new();
    for (p, fonts) in &scan.visible_fonts {
        for r in fonts {
            if seen.insert(*r)
                && let Some(f) = doc.get(*r).as_dict()
                && !embedded(doc, f)
            {
                let (p, name) = (*p, font_name(f));
                out.add("fonts-embedded", Some(p), Some(*r), || format!("Font {name} on page {} isn't embedded", p + 1));
            }
        }
    }
}

/// Whether a font's program is embedded (Type 3 glyphs are content, so always).
fn embedded(doc: &Document, f: &Dict) -> bool {
    let has_file =
        |d: &Dict| dict_of(doc, d, b"FontDescriptor").is_some_and(|fd| [&b"FontFile"[..], b"FontFile2", b"FontFile3"].iter().any(|k| fd.contains(k)));
    match f.name(b"Subtype") {
        Some(b"Type3") => true,
        Some(b"Type0") => array_of(doc, f, b"DescendantFonts").first().and_then(|d| doc.resolve(d).as_dict().cloned()).is_some_and(|d| has_file(&d)),
        _ => has_file(f),
    }
}

/// The natural language (11-001): the catalog's Lang, or a Lang on each element with content.
fn language(doc: &Document, cat: &Dict, tree: &Tree, scan: &Scan, out: &mut Out) {
    if text_of(doc, cat, b"Lang").is_some() {
        return;
    }
    let mut found = false;
    // Parents are read before their children, so one pass in index order inherits Lang.
    let mut covered = vec![false; tree.elems.len()];
    for (i, e) in tree.elems.iter().enumerate() {
        let inherited = e.parent.and_then(|p| covered.get(p).copied()).unwrap_or(false);
        let has = e.lang || inherited;
        if let Some(c) = covered.get_mut(i) {
            *c = has;
        }
        if e.mcids.is_empty() || has {
            continue;
        }
        found = true;
        let what = elem_label(tree, i);
        out.add("11-001", e.page, e.obj, || format!("The language of {what} can't be determined: no Lang on it, its parents or the catalog"));
    }
    if !found && scan.has_text && !tree.elems.iter().any(|e| e.lang) {
        out.add("11-001", None, None, || "No document language is set (catalog Lang)".into());
    }
}

/// A page's annotations (not Popup), with their object when they are indirect.
fn page_annots(doc: &Document, page: &Dict) -> Vec<(Option<ObjRef>, Dict)> {
    array_of(doc, page, b"Annots")
        .iter()
        .take(100_000)
        .filter_map(|x| doc.resolve(x).as_dict().cloned().map(|d| (x.as_ref(), d)))
        .filter(|(_, d)| d.name(b"Subtype") != Some(b"Popup"))
        .collect()
}

/// Annotations (Matterhorn 28): tags, descriptions, tab order.
fn annotations(doc: &Document, pages: &[pdfcraft_model::Page], tree: &Tree, out: &mut Out) {
    let mut owner: HashMap<ObjRef, usize> = HashMap::new();
    for (i, e) in tree.elems.iter().enumerate() {
        for a in &e.annots {
            owner.entry(*a).or_insert(i);
        }
    }
    let mut fields = HashSet::new();
    for (p, page) in pages.iter().enumerate() {
        let annots = page_annots(doc, &page.dict);
        if !annots.is_empty() {
            match page.dict.get(b"Tabs").map(|t| doc.resolve(t)).map(|t| t.as_name().map(<[u8]>::to_vec)) {
                None => out.add("28-008", Some(p), Some(page.obj), || format!("Page {} has annotations but no Tabs entry", p + 1)),
                Some(t) if t.as_deref() != Some(b"S") => out.add("28-009", Some(p), Some(page.obj), || {
                    format!("Page {} has annotations but doesn't tab in structure order (Tabs /S)", p + 1)
                }),
                _ => {}
            }
        }
        for (r, a) in annots {
            let st = a.name(b"Subtype").unwrap_or(b"").to_vec();
            let label = name_text(&st);
            if st == b"TrapNet" {
                out.add("28-007", Some(p), r, || format!("TrapNet annotation on page {}", p + 1));
                continue;
            }
            let hidden = a.get(b"F").map(|f| doc.resolve(f)).and_then(|f| f.as_int()).is_some_and(|f| f & 2 != 0);
            if st == b"PrinterMark" || hidden {
                continue;
            }
            // A direct annotation can't be referenced from the structure, so it is never tagged.
            let elem = r.and_then(|r| owner.get(&r)).and_then(|i| tree.elems.get(*i));
            let elem_ty = elem.map(|e| e.ty.as_slice());
            let elem_alt = elem.is_some_and(|e| e.alt);
            let has_contents = text_of(doc, &a, b"Contents").is_some();
            let where_ = |want: &str| match elem {
                None => format!("{label} annotation on page {} isn't tagged (it needs a {want} element)", p + 1),
                Some(e) => format!("{label} annotation on page {} is inside a {} element, not a {want}", p + 1, name_text(&e.raw)),
            };
            match st.as_slice() {
                b"Widget" => {
                    if elem_ty != Some(b"Form") {
                        let m = where_("Form");
                        out.add("28-010", Some(p), r, || m);
                    }
                    let field = if a.contains(b"T") { r } else { a.get(b"Parent").and_then(Object::as_ref) };
                    if let Some(field) = field
                        && fields.insert(field)
                    {
                        let fd = doc.get(field).as_dict().cloned().unwrap_or_default();
                        if text_of(doc, &fd, b"TU").is_none() && !elem_alt {
                            let name = full_name(doc, field);
                            out.add("28-005", Some(p), Some(field), || {
                                format!("Field \"{name}\" on page {} has no tooltip (TU) or alternate text", p + 1)
                            });
                        }
                    }
                }
                b"Link" => {
                    if elem_ty != Some(b"Link") {
                        let m = where_("Link");
                        out.add("28-011", Some(p), r, || m);
                    }
                    if !has_contents {
                        out.add("28-012", Some(p), r, || format!("Link annotation on page {} has no Contents (alternate description)", p + 1));
                    }
                }
                _ => {
                    if elem_ty != Some(b"Annot") {
                        let m = where_("Annot");
                        out.add("28-002", Some(p), r, || m);
                    }
                    if !has_contents && !elem_alt {
                        out.add("28-004", Some(p), r, || format!("{label} annotation on page {} has no Contents or alternate text", p + 1));
                    }
                }
            }
            if let Some(fs) = a.get(b"FS")
                && let Some(d) = doc.resolve(fs).as_dict()
                && fs.as_ref().is_none()
            {
                filespec(d, Some(p), r, out);
            }
        }
    }
}

fn filespec(d: &Dict, page: Option<usize>, obj: Option<ObjRef>, out: &mut Out) {
    if d.contains(b"EF") && !(d.contains(b"F") && d.contains(b"UF")) {
        let missing = match (d.contains(b"F"), d.contains(b"UF")) {
            (false, false) => "F and UF",
            (false, true) => "F",
            _ => "UF",
        };
        out.add("21-001", page, obj, || format!("An embedded file's specification has no {missing}"));
    }
}

/// Objects scanned for embedded files and forbidden XObjects beyond this many are skipped.
const MAX_OBJECTS: usize = 2_000_000;

/// Whole-file rules: embedded file specifications, reference and PostScript XObjects.
fn objects(doc: &Document, out: &mut Out) {
    let mut n = 0usize;
    for (r, o) in doc.scan_objects() {
        n += 1;
        if n > MAX_OBJECTS {
            out.add("limits", None, None, || format!("Only the first {MAX_OBJECTS} objects were checked for embedded files and XObjects"));
            break;
        }
        match &*o {
            Object::Dict(d) => filespec(d, None, Some(r), out),
            Object::Stream(s) => {
                let st = s.dict.name(b"Subtype");
                if st == Some(b"PS") || (st == Some(b"Form") && s.dict.name(b"Subtype2") == Some(b"PS")) {
                    out.add("ps-xobject", None, Some(r), || format!("Object {} is a PostScript XObject", r.num));
                } else if st == Some(b"Form") && s.dict.contains(b"Ref") {
                    out.add("30-001", None, Some(r), || format!("Object {} is a reference XObject", r.num));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "pdfua_tests.rs"]
mod tests;
