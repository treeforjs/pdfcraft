//! PDF 2.0 structure namespaces (ISO 32000-2 §14.7.4): the `/Namespaces` array of the
//! structure tree root, an element's `/NS`, namespace dictionaries (`/Type /Namespace`, `/NS`
//! URI, `/Schema`, `/RoleMapNS`) and role mapping across namespaces.
//!
//! An element without `/NS` is in the default namespace, the PDF 1.7 standard structure
//! namespace, whose role map is the root's `/RoleMap`. A type in the PDF 2.0 namespace is
//! standard when PDF 2.0 defines it; every MathML type is accepted as is; a type in any other
//! namespace must be role-mapped (through `/RoleMapNS`, possibly through several namespaces)
//! to a standard one. Chains are bounded and cycle-checked, and a missing or broken namespace
//! object is reported, never followed blindly.
//!
//! [`namespaces`] lists a document's namespaces, [`add_namespace`] adds one (or changes its
//! schema and role mappings); an element's namespace is changed through [`crate::set_tag`].

use std::collections::{HashMap, HashSet};

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString};

use crate::tags::{TagError, catalog};

/// The PDF 1.7 standard structure namespace (the default).
pub const NS_PDF17: &str = "http://iso.org/pdf/ssn";
/// The PDF 2.0 standard structure namespace.
pub const NS_PDF20: &str = "http://iso.org/pdf2/ssn";
/// MathML 3.0.
pub const NS_MATHML: &str = "http://www.w3.org/1998/Math/MathML";

/// Namespaces read from `/Namespaces` beyond this are ignored.
pub const MAX_NAMESPACES: usize = 10_000;
/// Role mappings one [`add_namespace`] call may set.
pub const MAX_NS_MAPPINGS: usize = 1_000;
/// Role-map steps followed before a chain is called circular.
const MAX_CHAIN: usize = 64;
/// Role-map entries read per namespace for listing.
const MAX_LISTED_MAPPINGS: usize = 10_000;
/// The longest namespace URI accepted.
const MAX_URI: usize = 2_048;
/// The longest structure type name accepted (ISO 32000-2 Annex C: names are at most 127 bytes).
const MAX_NAME: usize = 127;

/// Standard structure types of PDF 1.7 (ISO 32000-1 §14.8.4), the set PDF/UA-1 uses.
pub(crate) const STANDARD_17: &[&[u8]] = &[
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

pub(crate) fn in_pdf17(t: &[u8]) -> bool {
    STANDARD_17.contains(&t)
}

/// A type of the PDF 2.0 standard structure namespace (including `Hn` for any n ≥ 1).
pub(crate) fn in_pdf20(t: &[u8]) -> bool {
    if let [b'H', digits @ ..] = t
        && !digits.is_empty()
        && digits.iter().all(u8::is_ascii_digit)
        && digits.first() != Some(&b'0')
    {
        return true;
    }
    crate::structure::is_standard(t) && !NOT_IN_PDF20.contains(&t)
}

/// Which kind of namespace a URI names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NamespaceKind {
    /// The PDF 1.7 standard structure namespace (the default namespace).
    Pdf17,
    /// The PDF 2.0 standard structure namespace.
    Pdf20,
    MathMl,
    /// Any other namespace: its types must be role-mapped to a standard one.
    Other,
}

impl NamespaceKind {
    pub fn from_uri(uri: &str) -> NamespaceKind {
        match uri.trim() {
            NS_PDF17 => NamespaceKind::Pdf17,
            NS_PDF20 => NamespaceKind::Pdf20,
            NS_MATHML => NamespaceKind::MathMl,
            _ => NamespaceKind::Other,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            NamespaceKind::Pdf17 => "pdf-1.7",
            NamespaceKind::Pdf20 => "pdf-2.0",
            NamespaceKind::MathMl => "mathml",
            NamespaceKind::Other => "other",
        }
    }
}

/// The URI for a namespace given by URI or by short name (`pdf-2.0`/`pdf2`, `pdf-1.7`/`pdf1.7`,
/// `mathml`).
pub fn namespace_uri(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "pdf-2.0" | "pdf2" | "pdf2.0" | "pdf-2" | "2.0" => NS_PDF20.into(),
        "pdf-1.7" | "pdf1.7" | "pdf17" | "1.7" => NS_PDF17.into(),
        "mathml" => NS_MATHML.into(),
        _ => s.trim().to_owned(),
    }
}

/// One role mapping of a namespace: `from` (a type of the namespace) maps to `to` in
/// `namespace` (a URI; `None`: the default namespace).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct NsMapping {
    pub from: String,
    pub to: String,
    pub namespace: Option<String>,
}

/// A namespace dictionary of the document.
#[derive(Clone, Debug, PartialEq)]
pub struct Namespace {
    pub obj: ObjRef,
    pub uri: String,
    pub kind: NamespaceKind,
    /// The schema's file name (`/UF` or `/F` of `/Schema`), or a URL.
    pub schema: Option<String>,
    /// `/RoleMapNS`, sorted by `from`.
    pub role_map: Vec<NsMapping>,
    /// Listed in the root's `/Namespaces` (an element's `/NS` may name one that isn't).
    pub listed: bool,
}

/// Where an element's `/NS` points.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub(crate) enum NsRef {
    /// No `/NS`: the default (PDF 1.7) namespace.
    #[default]
    Default,
    Obj(ObjRef),
    /// `/NS` is there but isn't a reference (namespaces must be indirect).
    Broken,
}

/// The `/NS` of an element dictionary.
pub(crate) fn ns_of(d: &Dict) -> NsRef {
    match d.get(b"NS") {
        None | Some(Object::Null) => NsRef::Default,
        Some(Object::Ref(r)) => NsRef::Obj(*r),
        Some(_) => NsRef::Broken,
    }
}

/// How strictly types resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Any standard type is the end: PDF 1.7 and 2.0 types are both accepted in the default
    /// namespace (readers do, for files written before namespaces).
    Any,
    /// PDF/UA-2: the end must be a PDF 2.0 type (a PDF 1.7 type that PDF 2.0 also has
    /// counts) or MathML.
    Pdf20,
}

/// How a resolution ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Standard,
    /// Mapping ends at a type that isn't standard.
    Unmapped,
    Cycle,
    /// A namespace on the way is missing or isn't a namespace dictionary (`Mode::Pdf20` only;
    /// `Mode::Any` reads the type in the default namespace instead).
    MissingNamespace(NsRef),
    /// A PDF 1.7 type with no PDF 2.0 counterpart (`Mode::Pdf20`).
    NotInPdf20,
    /// A PDF 2.0-only type used in the default (PDF 1.7) namespace (`Mode::Pdf20`).
    Pdf20InDefault,
}

/// Where a type's role mapping ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolution {
    /// The last type reached (the standard type when `outcome` is `Standard`).
    pub ty: Vec<u8>,
    pub kind: NamespaceKind,
    /// The namespace URI of `ty` (`None`: the default namespace).
    pub uri: Option<String>,
    pub outcome: Outcome,
    /// A missing or broken namespace was read as the default namespace (`Mode::Any`).
    pub broken: bool,
}

impl Resolution {
    pub fn standard(&self) -> Option<&[u8]> {
        (self.outcome == Outcome::Standard).then_some(self.ty.as_slice())
    }
}

#[derive(Clone, Debug)]
struct NsInfo {
    uri: String,
    kind: NamespaceKind,
    map: Dict,
}

fn text_of(doc: &Document, o: Option<&Object>) -> Option<String> {
    o.map(|o| doc.resolve(o)).and_then(|o| o.as_string().map(PdfString::to_text))
}

/// A namespace dictionary's URI and role map, if `r` is one.
fn read_ns(doc: &Document, r: ObjRef) -> Option<NsInfo> {
    let obj = doc.get(r);
    let d = obj.as_dict()?;
    if d.name(b"Type").is_some_and(|t| t != b"Namespace") {
        return None;
    }
    let uri = text_of(doc, d.get(b"NS"))?.trim().to_owned();
    if uri.is_empty() {
        return None;
    }
    let map = d.get(b"RoleMapNS").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned()).unwrap_or_default();
    Some(NsInfo { kind: NamespaceKind::from_uri(&uri), uri, map })
}

/// The references in the root's `/Namespaces` (bounded).
pub(crate) fn listed(doc: &Document, root: &Dict) -> Vec<ObjRef> {
    let a = root.get(b"Namespaces").map(|o| doc.resolve(o));
    a.as_deref().and_then(Object::as_array).map(|a| a.iter().filter_map(Object::as_ref).take(MAX_NAMESPACES).collect()).unwrap_or_default()
}

/// A role-map value: a name in the default namespace, or `[name namespace]`.
fn target(doc: &Document, v: &Object) -> Option<(Vec<u8>, NsRef)> {
    match &*doc.resolve(v) {
        Object::Name(n) => Some((n.clone(), NsRef::Default)),
        Object::Array(a) => {
            let n = a.first().and_then(Object::as_name)?.to_vec();
            let ns = match a.get(1) {
                None | Some(Object::Null) => NsRef::Default,
                Some(Object::Ref(r)) => NsRef::Obj(*r),
                Some(_) => NsRef::Broken,
            };
            Some((n, ns))
        }
        _ => None,
    }
}

/// Resolves structure types to standard types through the role map and namespaces.
pub(crate) struct Resolver {
    role_map: Dict,
    spaces: HashMap<ObjRef, NsInfo>,
}

impl Resolver {
    /// A resolver for the structure tree root `root` (its `/RoleMap` and `/Namespaces`).
    pub fn new(doc: &Document, root: &Dict) -> Resolver {
        let role_map = root.get(b"RoleMap").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned()).unwrap_or_default();
        let spaces = listed(doc, root).into_iter().filter_map(|r| read_ns(doc, r).map(|i| (r, i))).collect();
        Resolver { role_map, spaces }
    }

    /// The resolver for the document's structure tree (empty when it has none).
    pub fn for_doc(doc: &Document) -> Resolver {
        let root =
            catalog(doc).and_then(|(_, c)| c.get(b"StructTreeRoot").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned())).unwrap_or_default();
        Resolver::new(doc, &root)
    }

    fn info(&self, doc: &Document, ns: NsRef) -> Option<NsInfo> {
        match ns {
            NsRef::Default => None,
            NsRef::Obj(r) => self.spaces.get(&r).cloned().or_else(|| read_ns(doc, r)),
            NsRef::Broken => None,
        }
    }

    /// The namespace URI and kind of `ns` (`None` for the default namespace or a broken one).
    pub fn uri(&self, doc: &Document, ns: NsRef) -> Option<(String, NamespaceKind)> {
        self.info(doc, ns).map(|i| (i.uri, i.kind))
    }

    /// Follow `ty` in namespace `ns` to a standard type.
    pub fn resolve(&self, doc: &Document, ty: &[u8], ns: NsRef, mode: Mode) -> Resolution {
        let (mut t, mut ns) = (ty.to_vec(), ns);
        let mut seen: HashSet<(Vec<u8>, NsRef)> = HashSet::new();
        let mut broken = false;
        let mut last = (NamespaceKind::Pdf17, None);
        for _ in 0..MAX_CHAIN {
            if !seen.insert((t.clone(), ns)) {
                return Resolution { ty: t, kind: last.0, uri: last.1, outcome: Outcome::Cycle, broken };
            }
            let info = match ns {
                NsRef::Default => None,
                other => match self.info(doc, other) {
                    Some(i) => Some(i),
                    None if mode == Mode::Pdf20 => {
                        return Resolution { ty: t, kind: last.0, uri: last.1, outcome: Outcome::MissingNamespace(other), broken };
                    }
                    None => {
                        broken = true;
                        None
                    }
                },
            };
            let kind = info.as_ref().map_or(NamespaceKind::Pdf17, |i| i.kind);
            let uri = info.as_ref().map(|i| i.uri.clone());
            last = (kind, uri.clone());
            let standard = match (kind, mode) {
                (NamespaceKind::MathMl, _) => true,
                (NamespaceKind::Pdf20, _) => in_pdf20(&t),
                (NamespaceKind::Pdf17, Mode::Any) => crate::structure::is_standard(&t),
                (NamespaceKind::Pdf17, Mode::Pdf20) => in_pdf17(&t) && in_pdf20(&t),
                (NamespaceKind::Other, _) => false,
            };
            if standard {
                return Resolution { ty: t, kind, uri, outcome: Outcome::Standard, broken };
            }
            // A namespace's own RoleMapNS; the PDF 1.7 namespace also uses the root RoleMap.
            let own = info.as_ref().and_then(|i| i.map.get(&t)).and_then(|v| target(doc, v));
            let next = match own {
                Some(n) => Some(n),
                None if kind == NamespaceKind::Pdf17 => {
                    self.role_map.get(&t).and_then(|v| doc.resolve(v).as_name().map(|n| (n.to_vec(), NsRef::Default)))
                }
                None => None,
            };
            match next {
                Some((n, next_ns)) => (t, ns) = (n, next_ns),
                None => {
                    let outcome = match (kind, mode) {
                        (NamespaceKind::Pdf17, Mode::Pdf20) if in_pdf17(&t) => Outcome::NotInPdf20,
                        (NamespaceKind::Pdf17, Mode::Pdf20) if in_pdf20(&t) => Outcome::Pdf20InDefault,
                        _ => Outcome::Unmapped,
                    };
                    return Resolution { ty: t, kind, uri, outcome, broken };
                }
            }
        }
        Resolution { ty: t, kind: last.0, uri: last.1, outcome: Outcome::Cycle, broken }
    }
}

fn schema_of(doc: &Document, d: &Dict) -> Option<String> {
    let s = d.get(b"Schema").map(|o| doc.resolve(o))?;
    match &*s {
        Object::String(s) => Some(s.to_text()),
        Object::Dict(f) => text_of(doc, f.get(b"UF")).or_else(|| text_of(doc, f.get(b"F"))),
        _ => None,
    }
}

fn namespace_from(doc: &Document, r: ObjRef, listed: bool) -> Option<Namespace> {
    let info = read_ns(doc, r)?;
    let d = doc.get(r).as_dict().cloned().unwrap_or_default();
    let mut role_map: Vec<NsMapping> = info
        .map
        .iter()
        .take(MAX_LISTED_MAPPINGS)
        .filter_map(|(k, v)| {
            let (to, ns) = target(doc, v)?;
            let namespace = match ns {
                NsRef::Default => None,
                NsRef::Obj(o) => Some(read_ns(doc, o).map_or_else(|| format!("(missing namespace object {})", o.num), |i| i.uri)),
                NsRef::Broken => Some("(not a namespace reference)".into()),
            };
            Some(NsMapping { from: String::from_utf8_lossy(k).into_owned(), to: String::from_utf8_lossy(&to).into_owned(), namespace })
        })
        .collect();
    role_map.sort_by(|a, b| a.from.cmp(&b.from));
    Some(Namespace { obj: r, uri: info.uri, kind: info.kind, schema: schema_of(doc, &d), role_map, listed })
}

/// The namespaces in the structure tree root's `/Namespaces`, in order, then any other
/// namespace an element names (from `extra`).
pub(crate) fn read_namespaces(doc: &Document, root: &Dict, extra: &[ObjRef]) -> Vec<Namespace> {
    let list = listed(doc, root);
    let mut seen: HashSet<ObjRef> = HashSet::new();
    let mut out = Vec::new();
    for r in &list {
        if seen.insert(*r)
            && let Some(n) = namespace_from(doc, *r, true)
        {
            out.push(n);
        }
    }
    for r in extra.iter().take(MAX_NAMESPACES) {
        if seen.insert(*r)
            && let Some(n) = namespace_from(doc, *r, false)
        {
            out.push(n);
        }
    }
    out
}

/// The document's namespaces (those in `/Namespaces`).
pub fn namespaces(doc: &Document) -> Vec<Namespace> {
    let Some(root) = catalog(doc).and_then(|(_, c)| c.get(b"StructTreeRoot").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned())) else {
        return Vec::new();
    };
    read_namespaces(doc, &root, &[])
}

/// Add a namespace, or change one: its schema and role mappings.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct NamespaceSpec {
    /// The namespace URI (or a short name, see [`namespace_uri`]).
    pub uri: String,
    /// A schema file name or URL (`/Schema`, written as a file specification).
    pub schema: Option<String>,
    /// Mappings to set; one with an empty `to` removes that type's mapping.
    pub role_map: Vec<NsMapping>,
}

fn invalid(s: impl Into<String>) -> TagError {
    TagError::Invalid(s.into())
}

/// A namespace URI, checked.
pub(crate) fn valid_uri(uri: &str) -> Result<String, TagError> {
    let uri = namespace_uri(uri);
    if uri.is_empty() {
        return Err(invalid("give the namespace URI (for example http://iso.org/pdf2/ssn)"));
    }
    if uri.len() > MAX_URI {
        return Err(invalid(format!("the namespace URI is longer than {MAX_URI} bytes")));
    }
    if uri.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(invalid(format!("{uri:?} is not a URI: it holds spaces or control characters")));
    }
    Ok(uri)
}

fn valid_name(n: &str, what: &str) -> Result<Vec<u8>, TagError> {
    let n = n.trim();
    if n.is_empty() {
        return Err(invalid(format!("give the {what} structure type")));
    }
    if n.len() > MAX_NAME {
        return Err(invalid(format!("the structure type {:?}… is longer than {MAX_NAME} bytes", n.chars().take(20).collect::<String>())));
    }
    if n.chars().any(|c| c.is_control() || c.is_whitespace() || matches!(c, '/' | '(' | ')' | '<' | '>' | '[' | ']' | '{' | '}' | '%')) {
        return Err(invalid(format!("{n:?} is not a structure type name (no spaces or PDF delimiters)")));
    }
    Ok(n.as_bytes().to_vec())
}

/// The structure tree root's reference; the root must be an indirect dictionary to be changed.
fn root_ref(doc: &Document) -> Result<ObjRef, TagError> {
    let (_, cat) = catalog(doc).ok_or(TagError::Untagged)?;
    match cat.get(b"StructTreeRoot") {
        None => Err(TagError::Untagged),
        Some(Object::Ref(r)) if doc.get(*r).as_dict().is_some() => Ok(*r),
        Some(Object::Ref(_)) => Err(TagError::Untagged),
        Some(_) => Err(invalid("the structure tree root is not an indirect dictionary; it can't be changed")),
    }
}

/// The namespace with `uri` in `/Namespaces`, creating it (and the array) when there is none.
pub(crate) fn find_or_add(doc: &mut Document, uri: &str) -> Result<ObjRef, TagError> {
    let root = root_ref(doc)?;
    let root_dict = doc.get(root).as_dict().cloned().unwrap_or_default();
    for r in listed(doc, &root_dict) {
        if read_ns(doc, r).is_some_and(|i| i.uri == uri) {
            return Ok(r);
        }
    }
    let ns = doc.add(Object::Dict({
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("Namespace"));
        d.set(b"NS".to_vec(), Object::String(PdfString::text(uri)));
        d
    }));
    let existing = root_dict.get(b"Namespaces").cloned();
    match existing {
        Some(Object::Ref(a)) if doc.get(a).as_array().is_some() => {
            let mut list = doc.get(a).as_array().cloned().unwrap_or_default();
            list.push(Object::Ref(ns));
            doc.set(a, Object::Array(list));
        }
        Some(Object::Array(mut list)) => {
            list.push(Object::Ref(ns));
            doc.update_dict(root, |d| d.set(b"Namespaces".to_vec(), Object::Array(list)))?;
        }
        _ => doc.update_dict(root, |d| d.set(b"Namespaces".to_vec(), Object::Array(vec![Object::Ref(ns)])))?,
    }
    Ok(ns)
}

fn describe(r: &Resolution, from: &str) -> String {
    let end = String::from_utf8_lossy(&r.ty);
    match &r.outcome {
        Outcome::Cycle => format!("the role mapping of {from} would be circular"),
        Outcome::MissingNamespace(_) => format!("{from} maps into a namespace that is missing"),
        _ if end == from => format!("{from} isn't a standard structure type and isn't role-mapped to one"),
        _ => format!("{from} maps to {end}, which isn't a standard structure type (nor mapped to one)"),
    }
}

/// Add the namespace `spec.uri` to the structure tree (or change the one there): set its
/// schema and role mappings. Each mapped type must then reach a standard type (no cycles, no
/// dead ends); standard PDF 2.0 types can't be remapped in the PDF 2.0 namespace. All or
/// nothing. Returns the namespace object.
pub fn add_namespace(doc: &mut Document, spec: &NamespaceSpec) -> Result<ObjRef, TagError> {
    let uri = valid_uri(&spec.uri)?;
    let kind = NamespaceKind::from_uri(&uri);
    if spec.role_map.len() > MAX_NS_MAPPINGS {
        return Err(invalid(format!("give at most {MAX_NS_MAPPINGS} role mappings at once")));
    }
    let schema = match spec.schema.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if s.chars().count() > MAX_URI => return Err(invalid(format!("the schema is longer than {MAX_URI} characters"))),
        other => other.map(str::to_owned),
    };
    let mut mappings = Vec::with_capacity(spec.role_map.len());
    for m in &spec.role_map {
        let from = valid_name(&m.from, "mapped")?;
        if kind == NamespaceKind::Pdf20 && in_pdf20(&from) {
            return Err(invalid(format!("{} is a standard PDF 2.0 type; it can't be remapped in the PDF 2.0 namespace", m.from.trim())));
        }
        let to = if m.to.trim().is_empty() { None } else { Some(valid_name(&m.to, "target")?) };
        let ns = match m.namespace.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(n) => Some(valid_uri(n)?),
        };
        mappings.push((from, to, ns));
    }
    root_ref(doc)?;
    let mut copy = doc.clone();
    let ns = find_or_add(&mut copy, &uri)?;
    let mut entries: Vec<(Vec<u8>, Option<Object>)> = Vec::with_capacity(mappings.len());
    for (from, to, target_ns) in &mappings {
        let value = match (to, target_ns) {
            (None, _) => None,
            (Some(t), None) => Some(Object::Name(t.clone())),
            (Some(t), Some(u)) if *u == uri => Some(Object::Array(vec![Object::Name(t.clone()), Object::Ref(ns)])),
            (Some(t), Some(u)) => Some(Object::Array(vec![Object::Name(t.clone()), Object::Ref(find_or_add(&mut copy, u)?)])),
        };
        entries.push((from.clone(), value));
    }
    let ns_dict = copy.get(ns).as_dict().cloned().unwrap_or_default();
    // The role map may be indirect: write the merged map back in the namespace directly.
    let mut map = ns_dict.get(b"RoleMapNS").map(|o| copy.resolve(o)).and_then(|o| o.as_dict().cloned()).unwrap_or_default();
    for (from, value) in entries {
        match value {
            Some(v) => map.set(from, v),
            None => {
                map.remove(&from);
            }
        }
    }
    copy.update_dict(ns, |d| {
        if map.is_empty() {
            d.remove(b"RoleMapNS");
        } else {
            d.set(b"RoleMapNS".to_vec(), Object::Dict(map));
        }
        if let Some(s) = &schema {
            let mut f = Dict::new();
            f.set(b"Type".to_vec(), Object::name("Filespec"));
            f.set(b"F".to_vec(), Object::String(PdfString::literal(s.chars().filter(char::is_ascii).collect::<String>().into_bytes())));
            f.set(b"UF".to_vec(), Object::String(PdfString::text(s)));
            d.set(b"Schema".to_vec(), Object::Dict(f));
        }
    })?;
    // Every mapping given must now reach a standard type.
    let resolver = Resolver::for_doc(&copy);
    for (from, to, _) in &mappings {
        if to.is_none() {
            continue;
        }
        let r = resolver.resolve(&copy, from, NsRef::Obj(ns), Mode::Any);
        if r.outcome != Outcome::Standard || r.broken {
            return Err(invalid(describe(&r, &String::from_utf8_lossy(from))));
        }
    }
    *doc = copy;
    Ok(ns)
}

#[cfg(test)]
#[path = "namespace_tests.rs"]
mod tests;
