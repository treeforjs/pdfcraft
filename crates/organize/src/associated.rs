//! PDF 2.0 associated files (ISO 32000-2 §14.13): file specification dictionaries with an
//! `/AFRelationship`, listed in an `/AF` array on the object they belong to.
//!
//! Owners: the document (the catalog), a page, an annotation, a structure element or an XObject
//! (a form or an image). [`associated_files`] lists one owner's files, [`all_associated_files`]
//! the document's, every page's and every annotation's; [`add_associated_file`] embeds a file
//! (an `/EmbeddedFile` stream with `/Subtype` MIME type and `/Params` Size, CheckSum (MD5),
//! CreationDate and ModDate) and appends its file specification (`/F`, `/UF`, `/EF`, `/Desc`,
//! `/AFRelationship`); [`remove_associated_file`] takes one out again.
//!
//! **Document-level files are also attachments.** A file associated with the document is
//! registered in the `/Names /EmbeddedFiles` name tree too (the same file specification), as
//! PDF/A-3 expects and as Acrobat's Attachments panel shows them, and removing it takes it out
//! of that tree again. Files associated with pages, annotations, structure elements or XObjects
//! are only in their owner's `/AF` (they are not document attachments). Removing never frees
//! the file specification or its stream, since other objects may still use them; a full save
//! drops them when nothing does.
//!
//! Hostile input: `/AF` that is not an array is reported as an issue and treated as empty (an
//! add replaces it), entries that are not file specifications are listed with a `problem`, only
//! the first [`MAX_AF_ENTRIES`] entries are read, and the `/EmbeddedFiles` tree is walked with a
//! seen-set and a depth limit; when it can't be read completely, an edit that would rewrite it
//! is refused rather than dropping entries.

use std::collections::HashSet;
use std::sync::Arc;

use md5::{Digest as _, Md5};
use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::OrganizeError;

/// The largest file [`add_associated_file`] embeds (64 MiB).
pub const MAX_FILE_BYTES: usize = 64 << 20;
/// `/AF` entries read per owner; adding to an owner that has this many is refused.
pub const MAX_AF_ENTRIES: usize = 4096;
/// Entries read from the `/EmbeddedFiles` name tree.
const MAX_TREE_ENTRIES: usize = 100_000;
/// Depth limit for the `/EmbeddedFiles` name tree.
const MAX_TREE_DEPTH: usize = 32;
/// Annotations read per page by [`all_associated_files`].
const MAX_ANNOTS: usize = 10_000;
/// Longest file name (characters).
pub const MAX_NAME_CHARS: usize = 255;
/// Longest description (characters).
pub const MAX_DESC_CHARS: usize = 4096;

/// How an associated file relates to its owner (ISO 32000-2 Table 43, `/AFRelationship`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relationship {
    /// The original source material for the content.
    Source,
    /// Information used to derive a visual presentation, such as a table or graph.
    Data,
    /// An alternative representation of the content, such as audio.
    Alternative,
    /// A supplemental representation of the original source or data that may be more easily
    /// consumable.
    Supplement,
    /// An encrypted payload document (§7.6.7).
    EncryptedPayload,
    /// The data associated with a form (§12.7.8).
    FormData,
    /// A schema definition for the associated object.
    Schema,
    /// Not known or can't be described with the other values.
    Unspecified,
}

impl Relationship {
    pub const ALL: [Relationship; 8] = [
        Relationship::Source,
        Relationship::Data,
        Relationship::Alternative,
        Relationship::Supplement,
        Relationship::EncryptedPayload,
        Relationship::FormData,
        Relationship::Schema,
        Relationship::Unspecified,
    ];

    /// The PDF name (`Source`, `Data`, …).
    pub fn name(self) -> &'static str {
        match self {
            Relationship::Source => "Source",
            Relationship::Data => "Data",
            Relationship::Alternative => "Alternative",
            Relationship::Supplement => "Supplement",
            Relationship::EncryptedPayload => "EncryptedPayload",
            Relationship::FormData => "FormData",
            Relationship::Schema => "Schema",
            Relationship::Unspecified => "Unspecified",
        }
    }

    /// Parse a PDF name, ignoring case (`source`, `FormData`, …).
    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.name().eq_ignore_ascii_case(s.trim()))
    }

    /// Every name, for error messages.
    pub fn names() -> String {
        Self::ALL.map(Relationship::name).join(", ")
    }
}

/// The object an associated file belongs to. Page indices are 0-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AfTarget {
    /// The document (the catalog's `/AF`).
    Document,
    /// A page.
    Page(usize),
    /// An annotation: its page and its position in the page's `/Annots`.
    Annotation { page: usize, index: usize },
    /// Any other owner by object number: a structure element, an XObject (form or image) or an
    /// annotation.
    Object(u32),
}

/// A file to associate.
#[derive(Clone, Debug, PartialEq)]
pub struct NewAssociatedFile {
    /// File name (`/F` and `/UF`, and the attachment's name).
    pub name: String,
    pub data: Arc<Vec<u8>>,
    /// MIME type (`/Subtype` of the embedded file stream), e.g. `text/csv`.
    pub mime: Option<String>,
    pub relationship: Relationship,
    pub description: Option<String>,
}

impl NewAssociatedFile {
    /// Check the name, size, MIME type and description.
    pub fn validate(&self) -> Result<(), OrganizeError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(invalid("the associated file needs a name"));
        }
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(invalid(format!("the file name is longer than {MAX_NAME_CHARS} characters")));
        }
        if name.chars().any(char::is_control) {
            return Err(invalid("the file name contains control characters"));
        }
        if self.data.len() > MAX_FILE_BYTES {
            return Err(invalid(format!("the file is {} bytes; associated files can be at most {} MiB", self.data.len(), MAX_FILE_BYTES >> 20)));
        }
        if let Some(m) = &self.mime {
            validate_mime(m)?;
        }
        if let Some(d) = &self.description
            && d.chars().count() > MAX_DESC_CHARS
        {
            return Err(invalid(format!("the description is longer than {MAX_DESC_CHARS} characters")));
        }
        Ok(())
    }
}

/// A MIME type is `type/subtype` (RFC 2045 tokens, optional parameters are not allowed in a PDF
/// name here), at most 127 characters.
pub fn validate_mime(m: &str) -> Result<(), OrganizeError> {
    let bad = || invalid(format!("{m:?} is not a MIME type such as text/plain or application/xml"));
    if m.is_empty() || m.len() > 127 {
        return Err(bad());
    }
    let token = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b));
    match m.split_once('/') {
        Some((t, s)) if token(t) && token(s) => Ok(()),
        _ => Err(bad()),
    }
}

/// A MIME type guessed from a file name's extension (common types only).
pub fn guess_mime(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "txt" => "text/plain",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "htm" | "html" => "text/html",
        "md" => "text/markdown",
        "xml" => "application/xml",
        "xsd" => "application/xml",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => return None,
    })
}

/// Which associated file of an owner: its 0-based position in `/AF`, or its file name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AfSelector {
    Index(usize),
    Name(String),
}

/// One `/AF` entry as read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssociatedFile {
    /// 0-based position in the owner's `/AF`.
    pub index: usize,
    /// `/UF`, else `/F` (empty when neither is present).
    pub name: String,
    pub description: Option<String>,
    /// `/AFRelationship` as written (`None` when missing; PDF 2.0 then means Unspecified).
    pub relationship: Option<String>,
    /// The embedded file stream's `/Subtype`.
    pub mime: Option<String>,
    /// `/Params /Size`.
    pub size: Option<u64>,
    /// Whether the file data is embedded (`/EF`).
    pub embedded: bool,
    pub creation_date: Option<String>,
    pub mod_date: Option<String>,
    /// `/Params /CheckSum` as lowercase hex.
    pub checksum: Option<String>,
    /// Object number of the file specification (`None` for a direct dictionary).
    pub object: Option<u32>,
    /// Whether the same file specification is in the `/Names /EmbeddedFiles` tree.
    pub in_embedded_files: bool,
    /// Why the entry isn't a usable file specification, if it isn't.
    pub problem: Option<String>,
}

/// One owner's associated files and what was wrong with its `/AF`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssociatedFiles {
    pub files: Vec<AssociatedFile>,
    pub issues: Vec<String>,
}

fn invalid(s: impl Into<String>) -> OrganizeError {
    OrganizeError::Invalid(s.into())
}

fn describe(target: &AfTarget) -> String {
    match target {
        AfTarget::Document => "the document".into(),
        AfTarget::Page(p) => format!("page {}", p.saturating_add(1)),
        AfTarget::Annotation { page, index } => format!("annotation {} on page {}", index.saturating_add(1), page.saturating_add(1)),
        AfTarget::Object(n) => format!("object {n}"),
    }
}

fn page_ref(doc: &Document, page: usize) -> Result<ObjRef, OrganizeError> {
    let pages = crate::pages(doc)?;
    let n = pages.len();
    pages.get(page).map(|p| p.obj).ok_or_else(|| invalid(format!("there is no page {} (the document has {n})", page.saturating_add(1))))
}

/// Where a page's `/Annots` array lives: directly in the page, or in an indirect array object.
fn annots(doc: &Document, page: ObjRef) -> (Vec<Object>, Option<ObjRef>) {
    let pd = doc.get(page);
    let Some(v) = pd.as_dict().and_then(|d| d.get(b"Annots")) else { return (Vec::new(), None) };
    match v {
        Object::Ref(r) => (doc.get(*r).as_array().cloned().unwrap_or_default(), Some(*r)),
        Object::Array(a) => (a.clone(), None),
        _ => (Vec::new(), None),
    }
}

/// Whether a dictionary is something an `/AF` may sit on through an object target.
fn object_kind(d: &Dict, is_stream: bool) -> Option<&'static str> {
    let ty = d.name(b"Type");
    let sub = d.name(b"Subtype");
    if is_stream {
        return match sub {
            Some(b"Form") => Some("form XObject"),
            Some(b"Image") => Some("image XObject"),
            _ => None,
        };
    }
    if ty == Some(b"StructElem") || (ty.is_none() && d.name(b"S").is_some() && d.contains(b"P")) {
        return Some("structure element");
    }
    if ty == Some(b"Page") {
        return Some("page");
    }
    if (ty.is_none() || ty == Some(b"Annot")) && sub.is_some() && d.contains(b"Rect") {
        return Some("annotation");
    }
    if ty == Some(b"DPart") {
        return Some("document part");
    }
    None
}

fn object_ref(doc: &Document, num: u32) -> Result<ObjRef, OrganizeError> {
    let obj = doc.try_get(num).map_err(|_| invalid(format!("object {num} doesn't exist")))?;
    let not = || invalid(format!("object {num} is not an annotation, structure element or XObject; use the document or a page as the target"));
    let d = obj.as_dict().ok_or_else(not)?;
    if doc.root().is_some_and(|r| r.num == num) {
        return Err(invalid("that object is the catalog; use the document as the target"));
    }
    object_kind(d, matches!(*obj, Object::Stream(_))).ok_or_else(not)?;
    Ok(ObjRef::new(num, doc.generation(num)))
}

/// The owner's dictionary, for reading.
fn owner_dict(doc: &Document, target: &AfTarget) -> Result<Dict, OrganizeError> {
    match target {
        AfTarget::Document => {
            let root = doc.root().ok_or(OrganizeError::NoPageTree)?;
            doc.get(root).as_dict().cloned().ok_or_else(|| invalid("the document has no catalog"))
        }
        AfTarget::Page(p) => doc.get(page_ref(doc, *p)?).as_dict().cloned().ok_or_else(|| invalid("the page is not a dictionary")),
        AfTarget::Annotation { page, index } => {
            let (list, _) = annots(doc, page_ref(doc, *page)?);
            let entry = list.get(*index).ok_or_else(|| no_annotation(*page, *index, list.len()))?;
            doc.dict(entry).ok_or_else(|| invalid(format!("{} is not a dictionary", describe(target))))
        }
        AfTarget::Object(n) => doc.get(object_ref(doc, *n)?).as_dict().cloned().ok_or_else(|| invalid(format!("object {n} is not a dictionary"))),
    }
}

fn no_annotation(page: usize, index: usize, n: usize) -> OrganizeError {
    invalid(format!("there is no annotation {} on page {} (it has {n})", index.saturating_add(1), page.saturating_add(1)))
}

/// The owner as an indirect object, for writing. A direct annotation dictionary is made
/// indirect first (its `/Annots` entry then refers to it).
fn owner_ref(doc: &mut Document, target: &AfTarget) -> Result<ObjRef, OrganizeError> {
    match target {
        AfTarget::Document => {
            let root = doc.root().ok_or(OrganizeError::NoPageTree)?;
            if doc.get(root).as_dict().is_none() {
                return Err(invalid("the document has no catalog"));
            }
            Ok(root)
        }
        AfTarget::Page(p) => page_ref(doc, *p),
        AfTarget::Annotation { page, index } => {
            let pr = page_ref(doc, *page)?;
            let (mut list, at) = annots(doc, pr);
            let n = list.len();
            let entry = list.get_mut(*index).ok_or_else(|| no_annotation(*page, *index, n))?;
            match entry {
                Object::Ref(r) => {
                    let r = *r;
                    if doc.get(r).as_dict().is_none() {
                        return Err(invalid(format!("{} is not a dictionary", describe(target))));
                    }
                    Ok(r)
                }
                Object::Dict(d) => {
                    let r = doc.add(Object::Dict(d.clone()));
                    *entry = Object::Ref(r);
                    match at {
                        Some(a) => doc.set(a, Object::Array(list)),
                        None => doc.update_dict(pr, |p| p.set(b"Annots".to_vec(), Object::Array(list)))?,
                    }
                    Ok(r)
                }
                _ => Err(invalid(format!("{} is not a dictionary", describe(target)))),
            }
        }
        AfTarget::Object(n) => object_ref(doc, *n),
    }
}

/// The owner's `/AF` entries (at most [`MAX_AF_ENTRIES`]), where the array lives when it is an
/// indirect object, and problems found.
fn af_array(doc: &Document, owner: &Dict, issues: &mut Vec<String>) -> (Vec<Object>, Option<ObjRef>, bool) {
    let Some(v) = owner.get(b"AF") else { return (Vec::new(), None, true) };
    let (arr, at) = match v {
        Object::Array(a) => (a.clone(), None),
        Object::Ref(r) => match doc.get(*r).as_array() {
            Some(a) => (a.clone(), Some(*r)),
            None => {
                issues.push("/AF is not an array; it is ignored".into());
                return (Vec::new(), None, false);
            }
        },
        _ => {
            issues.push("/AF is not an array; it is ignored".into());
            return (Vec::new(), None, false);
        }
    };
    let mut arr = arr;
    if arr.len() > MAX_AF_ENTRIES {
        issues.push(format!("/AF has {} entries; only the first {MAX_AF_ENTRIES} are read", arr.len()));
        arr.truncate(MAX_AF_ENTRIES);
    }
    (arr, at, true)
}

fn text(d: &Dict, key: &[u8]) -> Option<String> {
    d.get(key).and_then(|o| o.as_string()).map(PdfString::to_text)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read one `/AF` entry.
fn read_entry(doc: &Document, index: usize, entry: &Object, tree: &HashSet<ObjRef>) -> AssociatedFile {
    let mut f = AssociatedFile { index, object: entry.as_ref().map(|r| r.num), ..Default::default() };
    f.in_embedded_files = entry.as_ref().is_some_and(|r| tree.contains(&r));
    let resolved = doc.resolve(entry);
    let Some(d) = resolved.as_dict() else {
        f.problem = Some(match &*resolved {
            Object::String(s) => {
                f.name = s.to_text();
                "a file specification string, not a dictionary (associated files need a dictionary)".into()
            }
            Object::Null => "missing object".into(),
            _ => "not a file specification dictionary".into(),
        });
        return f;
    };
    f.name = text(d, b"UF").or_else(|| text(d, b"F")).unwrap_or_default();
    f.description = text(d, b"Desc");
    f.relationship = d.name(b"AFRelationship").map(|n| String::from_utf8_lossy(n).into_owned());
    if let Some(ty) = d.name(b"Type")
        && ty != b"Filespec"
        && ty != b"F"
    {
        f.problem = Some(format!("/Type is /{}, not /Filespec", String::from_utf8_lossy(ty)));
    }
    let ef = d.get(b"EF").and_then(|e| doc.dict(e));
    let stream = ef.as_ref().and_then(|e| e.get(b"UF").or_else(|| e.get(b"F")).map(|s| doc.resolve(s)));
    if let Some(s) = stream.as_deref() {
        if let Object::Stream(s) = s {
            f.embedded = true;
            f.mime = s.dict.name(b"Subtype").map(|n| String::from_utf8_lossy(n).into_owned());
            if let Some(p) = s.dict.get(b"Params").and_then(|p| doc.dict(p)) {
                f.size = p.get(b"Size").and_then(|s| doc.resolve(s).as_int()).and_then(|s| u64::try_from(s).ok());
                f.creation_date = text(&p, b"CreationDate");
                f.mod_date = text(&p, b"ModDate");
                f.checksum = p.get(b"CheckSum").and_then(|c| c.as_string()).map(|c| hex(&c.bytes));
            }
        } else if f.problem.is_none() {
            f.problem = Some("/EF does not hold an embedded file stream".into());
        }
    }
    f
}

/// The `/Names /EmbeddedFiles` tree root, if any.
fn embedded_files_root(doc: &Document) -> Option<Object> {
    let root = doc.root()?;
    let cat = doc.get(root);
    let names = doc.dict(cat.as_dict()?.get(b"Names")?)?;
    names.get(b"EmbeddedFiles").cloned()
}

/// The `/EmbeddedFiles` entries, and whether they could all be read (no cycle, depth or size
/// limit hit, no malformed node).
fn tree_entries(doc: &Document) -> (Vec<(Vec<u8>, Object)>, bool) {
    let mut out = Vec::new();
    let Some(root) = embedded_files_root(doc) else { return (out, true) };
    let mut seen = HashSet::new();
    let mut complete = true;
    walk_tree(doc, &root, 0, &mut seen, &mut out, &mut complete);
    (out, complete)
}

fn walk_tree(doc: &Document, node: &Object, depth: usize, seen: &mut HashSet<u32>, out: &mut Vec<(Vec<u8>, Object)>, complete: &mut bool) {
    if depth > MAX_TREE_DEPTH {
        *complete = false;
        return;
    }
    if let Object::Ref(r) = node
        && !seen.insert(r.num)
    {
        *complete = false;
        return;
    }
    let node = doc.resolve(node);
    let Some(d) = node.as_dict() else {
        *complete = false;
        return;
    };
    if let Some(names) = d.get(b"Names") {
        let names = doc.resolve(names);
        let Some(names) = names.as_array() else {
            *complete = false;
            return;
        };
        if names.len() % 2 != 0 {
            *complete = false;
        }
        for [k, v] in names.as_chunks::<2>().0 {
            if out.len() >= MAX_TREE_ENTRIES {
                *complete = false;
                return;
            }
            if let Some(k) = doc.resolve(k).as_string() {
                out.push((k.bytes.clone(), v.clone()));
            } else {
                *complete = false;
            }
        }
    }
    if let Some(kids) = d.get(b"Kids") {
        let kids = doc.resolve(kids);
        let Some(kids) = kids.as_array() else {
            *complete = false;
            return;
        };
        for k in kids {
            walk_tree(doc, k, depth + 1, seen, out, complete);
        }
    }
}

/// Rewrite `/Names /EmbeddedFiles` as one flat, sorted node (removing it when empty). An
/// indirect tree root is replaced in place, so references to it stay valid.
fn write_tree(doc: &mut Document, mut entries: Vec<(Vec<u8>, Object)>) -> Result<(), OrganizeError> {
    let root = doc.root().ok_or(OrganizeError::NoPageTree)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let old = embedded_files_root(doc);
    let names_obj = doc.get(root).as_dict().and_then(|c| c.get(b"Names").cloned());
    let mut names = names_obj.as_ref().and_then(|n| doc.dict(n)).unwrap_or_default();
    if entries.is_empty() {
        names.remove(b"EmbeddedFiles");
    } else {
        let mut flat = Vec::with_capacity(entries.len().saturating_mul(2));
        for (k, v) in entries {
            flat.push(Object::String(PdfString::literal(k)));
            flat.push(v);
        }
        let mut tree = Dict::new();
        tree.set(b"Names".to_vec(), Object::Array(flat));
        match old {
            Some(Object::Ref(r)) => doc.set(r, Object::Dict(tree)),
            _ => {
                let r = doc.add(Object::Dict(tree));
                names.set(b"EmbeddedFiles".to_vec(), Object::Ref(r));
            }
        }
    }
    match names_obj {
        Some(Object::Ref(r)) if doc.get(r).as_dict().is_some() => doc.set(r, Object::Dict(names)),
        _ => doc.update_dict(root, |c| {
            if names.is_empty() {
                c.remove(b"Names");
            } else {
                c.set(b"Names".to_vec(), Object::Dict(names));
            }
        })?,
    }
    Ok(())
}

fn tree_refs(doc: &Document) -> HashSet<ObjRef> {
    tree_entries(doc).0.into_iter().filter_map(|(_, v)| v.as_ref()).collect()
}

fn list_owner(doc: &Document, owner: &Dict, tree: &HashSet<ObjRef>) -> AssociatedFiles {
    let mut out = AssociatedFiles::default();
    let (arr, _, _) = af_array(doc, owner, &mut out.issues);
    out.files = arr.iter().enumerate().map(|(i, e)| read_entry(doc, i, e, tree)).collect();
    out
}

/// One owner's associated files.
pub fn associated_files(doc: &Document, target: &AfTarget) -> Result<AssociatedFiles, OrganizeError> {
    let owner = owner_dict(doc, target)?;
    Ok(list_owner(doc, &owner, &tree_refs(doc)))
}

/// The document's associated files (always first, even when empty), then those of every page
/// and every annotation that has any. Structure elements and XObjects are not scanned; ask for
/// them with [`AfTarget::Object`].
pub fn all_associated_files(doc: &Document) -> Result<Vec<(AfTarget, AssociatedFiles)>, OrganizeError> {
    let tree = tree_refs(doc);
    let mut out = vec![(AfTarget::Document, list_owner(doc, &owner_dict(doc, &AfTarget::Document)?, &tree))];
    for (i, p) in crate::pages(doc)?.iter().enumerate() {
        let pd = doc.get(p.obj);
        if let Some(d) = pd.as_dict()
            && d.contains(b"AF")
        {
            out.push((AfTarget::Page(i), list_owner(doc, d, &tree)));
        }
        let (list, _) = annots(doc, p.obj);
        for (j, a) in list.iter().take(MAX_ANNOTS).enumerate() {
            if let Some(d) = doc.dict(a)
                && d.contains(b"AF")
            {
                out.push((AfTarget::Annotation { page: i, index: j }, list_owner(doc, &d, &tree)));
            }
        }
    }
    Ok(out)
}

/// Store the owner's new `/AF` (removing the key when empty): into the indirect array it
/// already uses, else directly in the owner.
fn write_af(doc: &mut Document, owner: ObjRef, at: Option<ObjRef>, arr: Vec<Object>) -> Result<(), OrganizeError> {
    match (at, arr.is_empty()) {
        (Some(a), false) => doc.set(a, Object::Array(arr)),
        (_, true) => doc.update_dict(owner, |d| {
            d.remove(b"AF");
        })?,
        (None, false) => doc.update_dict(owner, |d| d.set(b"AF".to_vec(), Object::Array(arr)))?,
    }
    Ok(())
}

/// Embed `file` and associate it with `target` (appended to its `/AF`; for the document also
/// registered in `/Names /EmbeddedFiles`, under a unique name). `date` is the PDF date for
/// `/CreationDate` and `/ModDate`. Returns the new entry's 0-based position in `/AF`.
pub fn add_associated_file(doc: &mut Document, target: &AfTarget, file: &NewAssociatedFile, date: Option<&str>) -> Result<usize, OrganizeError> {
    file.validate()?;
    let owner_d = owner_dict(doc, target)?;
    let mut issues = Vec::new();
    let (mut arr, at, _) = af_array(doc, &owner_d, &mut issues);
    if arr.len() >= MAX_AF_ENTRIES {
        return Err(invalid(format!("{} already has {MAX_AF_ENTRIES} associated files", describe(target))));
    }
    let tree = if *target == AfTarget::Document {
        let (entries, complete) = tree_entries(doc);
        if !complete {
            return Err(invalid("the document's EmbeddedFiles name tree is malformed, cyclic or too large to update safely"));
        }
        Some(entries)
    } else {
        None
    };
    let owner = owner_ref(doc, target)?;
    let name = file.name.trim();

    let mut params = Dict::new();
    params.set(b"Size".to_vec(), Object::Int(i64::try_from(file.data.len()).unwrap_or(i64::MAX)));
    params.set(b"CheckSum".to_vec(), Object::String(PdfString { bytes: Md5::digest(file.data.as_slice()).to_vec(), hex: true }));
    if let Some(date) = date {
        params.set(b"CreationDate".to_vec(), PdfString::literal(date.as_bytes().to_vec()));
        params.set(b"ModDate".to_vec(), PdfString::literal(date.as_bytes().to_vec()));
    }
    let mut ef = Dict::new();
    ef.set(b"Type".to_vec(), Object::name("EmbeddedFile"));
    if let Some(m) = &file.mime {
        ef.set(b"Subtype".to_vec(), Object::name(m));
    }
    ef.set(b"Params".to_vec(), Object::Dict(params));
    let stream = doc.add(Object::Stream(Stream::flate(ef, &file.data)));
    let mut efd = Dict::new();
    efd.set(b"F".to_vec(), Object::Ref(stream));
    efd.set(b"UF".to_vec(), Object::Ref(stream));
    let mut fs = Dict::new();
    fs.set(b"Type".to_vec(), Object::name("Filespec"));
    fs.set(b"F".to_vec(), PdfString::text(name));
    fs.set(b"UF".to_vec(), PdfString::text(name));
    fs.set(b"EF".to_vec(), Object::Dict(efd));
    if let Some(d) = file.description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        fs.set(b"Desc".to_vec(), PdfString::text(d));
    }
    fs.set(b"AFRelationship".to_vec(), Object::name(file.relationship.name()));
    let fs = doc.add(Object::Dict(fs));

    if !issues.is_empty() {
        doc.note_repair(format!("replaced the unreadable /AF of {}", describe(target)));
    }
    arr.push(Object::Ref(fs));
    let index = arr.len().saturating_sub(1);
    write_af(doc, owner, at, arr)?;

    if let Some(mut entries) = tree {
        let base = PdfString::text(name).bytes;
        let taken: HashSet<Vec<u8>> = entries.iter().map(|(k, _)| k.clone()).collect();
        let mut key = base.clone();
        let mut n = 2u32;
        while taken.contains(&key) {
            key = PdfString::text(&format!("{name} ({n})")).bytes;
            n = n.saturating_add(1);
        }
        entries.push((key, Object::Ref(fs)));
        write_tree(doc, entries)?;
    }
    Ok(index)
}

/// Find the entry `sel` names among `files`.
fn select(target: &AfTarget, files: &[AssociatedFile], sel: &AfSelector) -> Result<usize, OrganizeError> {
    match sel {
        AfSelector::Index(i) => {
            if *i < files.len() {
                Ok(*i)
            } else {
                Err(invalid(format!("{} has no associated file {} (it has {})", describe(target), i.saturating_add(1), files.len())))
            }
        }
        AfSelector::Name(n) => {
            let hits: Vec<usize> = files.iter().filter(|f| f.name == *n).map(|f| f.index).collect();
            match hits.as_slice() {
                [one] => Ok(*one),
                [] => {
                    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).take(20).collect();
                    Err(invalid(format!(
                        "{} has no associated file named {n:?} (it has: {})",
                        describe(target),
                        if names.is_empty() { "none".into() } else { names.join(", ") }
                    )))
                }
                many => Err(invalid(format!(
                    "{} has {} associated files named {n:?} (at {}); pass index instead",
                    describe(target),
                    many.len(),
                    many.iter().map(|i| i.saturating_add(1).to_string()).collect::<Vec<_>>().join(", ")
                ))),
            }
        }
    }
}

/// Remove one associated file from `target`'s `/AF` (and, for the document, its entries in
/// `/Names /EmbeddedFiles`). Returns what was removed.
pub fn remove_associated_file(doc: &mut Document, target: &AfTarget, sel: &AfSelector) -> Result<AssociatedFile, OrganizeError> {
    let owner_d = owner_dict(doc, target)?;
    let tree = tree_refs(doc);
    let mut issues = Vec::new();
    let (mut arr, at, ok) = af_array(doc, &owner_d, &mut issues);
    if !ok {
        return Err(invalid(format!("{} has a malformed /AF (not an array)", describe(target))));
    }
    let files: Vec<AssociatedFile> = arr.iter().enumerate().map(|(i, e)| read_entry(doc, i, e, &tree)).collect();
    let i = select(target, &files, sel)?;
    let removed = files.get(i).cloned().ok_or_else(|| invalid("no such associated file"))?;
    let entry = arr.get(i).cloned().ok_or_else(|| invalid("no such associated file"))?;
    let tree = match (target, entry.as_ref()) {
        (AfTarget::Document, Some(r)) if removed.in_embedded_files => {
            let (entries, complete) = tree_entries(doc);
            if !complete {
                return Err(invalid("the document's EmbeddedFiles name tree is malformed, cyclic or too large to update safely"));
            }
            Some(entries.into_iter().filter(|(_, v)| v.as_ref() != Some(r)).collect::<Vec<_>>())
        }
        _ => None,
    };
    let owner = owner_ref(doc, target)?;
    // The array may have been longer than what was read: keep the unread tail.
    if let Some(full) = owner_d.get(b"AF").map(|v| doc.resolve(v)).and_then(|v| v.as_array().cloned())
        && full.len() > arr.len()
    {
        arr = full;
    }
    if i < arr.len() {
        arr.remove(i);
    }
    write_af(doc, owner, at, arr)?;
    if let Some(entries) = tree {
        write_tree(doc, entries)?;
    }
    Ok(removed)
}

/// An associated file's information and its decoded data (at most [`MAX_FILE_BYTES`]
/// … or `limit`, whichever is smaller).
pub fn associated_file_data(doc: &Document, target: &AfTarget, sel: &AfSelector, limit: usize) -> Result<(AssociatedFile, Vec<u8>), OrganizeError> {
    let owner = owner_dict(doc, target)?;
    let tree = tree_refs(doc);
    let list = list_owner(doc, &owner, &tree);
    let i = select(target, &list.files, sel)?;
    let info = list.files.get(i).cloned().ok_or_else(|| invalid("no such associated file"))?;
    let mut issues = Vec::new();
    let (arr, _, _) = af_array(doc, &owner, &mut issues);
    let fs = arr
        .get(i)
        .and_then(|e| doc.dict(e))
        .ok_or_else(|| invalid(format!("associated file {} is not a file specification", i.saturating_add(1))))?;
    let ef = fs.get(b"EF").and_then(|e| doc.dict(e)).ok_or_else(|| invalid(format!("{:?} is not embedded (no /EF)", info.name)))?;
    let s = ef
        .get(b"UF")
        .or_else(|| ef.get(b"F"))
        .map(|s| doc.resolve(s))
        .ok_or_else(|| invalid(format!("{:?} has no embedded file stream", info.name)))?;
    let Object::Stream(s) = &*s else { return Err(invalid(format!("{:?} has no embedded file stream", info.name))) };
    let max = limit.min(MAX_FILE_BYTES.saturating_mul(4));
    let data = s.decoded_within(max).map_err(|e| invalid(format!("{:?} could not be decoded: {e}", info.name)))?;
    Ok((info, data))
}

#[cfg(test)]
mod tests;
