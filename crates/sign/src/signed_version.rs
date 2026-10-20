//! Compare a signature's signed version with the current document (PDF: View Signed
//! Version, Compare Signed Version to Current Version).
//!
//! The signed version is the file's first [`SignatureInfo::signed_len`] bytes: the revision the
//! signature's `/ByteRange` ends in. [`compare_signed_version`] parses it, diffs every object the
//! current document changed or added since (the same diff signature validation uses), and lists
//! each change — page content, comments, form fields with their old and new values, pages added
//! or removed, signatures, metadata — with whether the signature's DocMDP/FieldMDP permissions
//! allow it. The verdict matches [`SignatureInfo::modification`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, ObjRef, Object};

use crate::SignError;
use crate::pdf::{SignatureInfo, effective_permissions, judged_changes, kind_permitted, object_pages, text};

/// At most this many changes are listed (a hostile file can change millions of objects).
pub const MAX_CHANGES: usize = 10_000;
/// Field values are cut to this many characters.
const MAX_VALUE_CHARS: usize = 200;
/// Deepest `/Parent` chain followed to name a field.
const MAX_FIELD_DEPTH: usize = 32;

/// How an object differs between the signed version and the current document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeAction {
    Added,
    Modified,
    Removed,
}

impl ChangeAction {
    pub fn label(self) -> &'static str {
        match self {
            ChangeAction::Added => "added",
            ChangeAction::Modified => "modified",
            ChangeAction::Removed => "removed",
        }
    }
}

/// One change made after signing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionChange {
    /// The category, as [`crate::Modification`] names it: "page content", "comments",
    /// "form fill", "changes to locked fields", "signature", "pages added or removed",
    /// "document structure", "links", "metadata", "document security store", "other changes".
    pub kind: String,
    pub action: ChangeAction,
    /// 0-based page: in the current document, or for something removed, in the signed version.
    pub page: Option<usize>,
    /// The object changed (in the current document; for something removed, in the signed one).
    pub object: Option<ObjRef>,
    /// What it is: "page", "page content", an annotation's subtype ("Text", "Widget"…), "form
    /// field", "catalog", "interactive form", "signature dictionary"…
    pub subject: String,
    /// For form fields: the fully qualified name.
    pub field: Option<String>,
    /// For form fields: the value in the signed version and now.
    pub before: Option<String>,
    pub after: Option<String>,
    /// Whether the signature's permissions allow it.
    pub permitted: bool,
}

/// What changed between a signature's signed version and the current document.
#[derive(Clone, Debug, PartialEq)]
pub struct SignedVersionComparison {
    pub field: String,
    /// 1-based revision the signature covers.
    pub revision: usize,
    /// Length of the signed version, and of the current file (bytes).
    pub signed_len: usize,
    pub current_len: usize,
    /// DocMDP level of a certification signature (1–3).
    pub certify: Option<u8>,
    /// The permissions applied: `certify` tightened by a field lock's (`None`: an approval
    /// signature, which permits form fill, comments and further signatures).
    pub permissions: Option<u8>,
    pub signed_pages: usize,
    pub current_pages: usize,
    /// Every change, in page order (document-level changes last).
    pub changes: Vec<VersionChange>,
    /// More changes than [`MAX_CHANGES`] were found; the rest are not listed.
    pub truncated: bool,
    /// The signed version could not be read: nothing can be compared, and nothing is permitted.
    pub unreadable: bool,
}

impl SignedVersionComparison {
    /// Whether anything changed after signing.
    pub fn modified(&self) -> bool {
        self.unreadable || !self.changes.is_empty()
    }

    /// Whether every change is one the signature permits.
    pub fn permitted(&self) -> bool {
        !self.unreadable && self.changes.iter().all(|c| c.permitted)
    }

    /// The categories of change, each with whether it is permitted, in first-seen order.
    pub fn kinds(&self) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> = Vec::new();
        for c in &self.changes {
            match out.iter_mut().find(|(k, _)| *k == c.kind) {
                Some(entry) => entry.1 &= c.permitted,
                None => out.push((c.kind.clone(), c.permitted)),
            }
        }
        out
    }
}

/// What a DocMDP level (as [`SignedVersionComparison::permissions`]) permits, in words.
pub fn permissions_label(p: Option<u8>) -> &'static str {
    match p {
        Some(1) => "No changes are permitted.",
        Some(2) => "Only filling in forms and signing are permitted.",
        Some(3) => "Only filling in forms, signing and commenting are permitted.",
        _ => "Filling in forms, signing and commenting are permitted (approval signature).",
    }
}

/// The signed version of `sig`: `bytes[..sig.signed_len]`. `bytes` is the file `sig` was
/// validated against.
pub fn signed_version<'a>(bytes: &'a [u8], sig: &SignatureInfo) -> Result<&'a [u8], SignError> {
    if !sig.signed {
        return Err(SignError::Pdf(format!("signature field {:?} is not signed", sig.field)));
    }
    if sig.signed_len == 0 {
        return Err(SignError::Pdf(format!("the signature in {:?} has no valid byte range, so its signed version is unknown", sig.field)));
    }
    bytes
        .get(..sig.signed_len)
        .ok_or_else(|| SignError::Pdf(format!("the signature in {:?} covers {} bytes but the file has {}", sig.field, sig.signed_len, bytes.len())))
}

/// Compare the signed version of `sig` with the current document `doc` (written as `bytes`,
/// the file `sig` was validated against). `password` opens an encrypted signed version.
pub fn compare_signed_version(
    doc: &Document,
    bytes: &[u8],
    sig: &SignatureInfo,
    password: Option<&str>,
) -> Result<SignedVersionComparison, SignError> {
    let signed = signed_version(bytes, sig)?;
    let permissions = effective_permissions(sig.certify, sig.lock.as_ref());
    let current_pages = pdfcraft_annot::page_refs(doc).unwrap_or_default();
    let mut out = SignedVersionComparison {
        field: sig.field.clone(),
        revision: sig.revision,
        signed_len: signed.len(),
        current_len: bytes.len(),
        certify: sig.certify,
        permissions,
        signed_pages: 0,
        current_pages: current_pages.len(),
        changes: Vec::new(),
        truncated: false,
        unreadable: false,
    };
    // Trailing whitespace after the signed bytes is not a revision.
    let rest = bytes.get(signed.len()..).unwrap_or_default();
    let Ok(old) = Document::open_with_password(Arc::new(signed.to_vec()), password) else {
        out.unreadable = true;
        return Ok(out);
    };
    let old_pages = pdfcraft_annot::page_refs(&old).unwrap_or_default();
    out.signed_pages = old_pages.len();
    if rest.iter().all(|b| b.is_ascii_whitespace() || *b == 0) {
        return Ok(out);
    }
    let on_page = object_pages(doc);
    let old_on_page = object_pages(&old);
    let page_of = |d: &Document, map: &HashMap<ObjRef, usize>, r: ObjRef| {
        map.get(&r).copied().or_else(|| d.get(r).as_dict().and_then(|x| x.reference(b"P")).and_then(|p| map.get(&p).copied()))
    };
    let old_page_set: HashSet<ObjRef> = old_pages.iter().copied().collect();
    let new_page_set: HashSet<ObjRef> = current_pages.iter().copied().collect();
    let pages_kind = "pages added or removed";
    let mut changes = Vec::new();
    let push = |changes: &mut Vec<VersionChange>, c: VersionChange| {
        if changes.len() < MAX_CHANGES {
            changes.push(c);
            true
        } else {
            false
        }
    };
    let mut truncated = false;
    // Pages added and removed (by page object).
    for (i, r) in current_pages.iter().enumerate().filter(|(_, r)| !old_page_set.contains(r)) {
        let c = change(pages_kind, ChangeAction::Added, Some(i), Some(*r), "page", permissions);
        truncated |= !push(&mut changes, c);
    }
    for (i, r) in old_pages.iter().enumerate().filter(|(_, r)| !new_page_set.contains(r)) {
        let c = change(pages_kind, ChangeAction::Removed, Some(i), Some(*r), "page", permissions);
        truncated |= !push(&mut changes, c);
    }
    for (r, kind, permitted) in judged_changes(doc, &old, sig.certify, sig.lock.as_ref()) {
        // New pages are listed above; the page tree that gained them is the same change.
        if new_page_set.contains(&r) && !old_page_set.contains(&r) {
            continue;
        }
        let now = doc.get(r);
        let before = old.try_get(r.num).ok().filter(|o| !matches!(**o, Object::Null));
        let action = if before.is_some() { ChangeAction::Modified } else { ChangeAction::Added };
        let page = page_of(doc, &on_page, r);
        let mut c = VersionChange { permitted, ..change(kind, action, page, Some(r), "", permissions) };
        match &*now {
            Object::Stream(_) if kind == "page content" => c.subject = "page content".into(),
            Object::Stream(s) => c.subject = s.dict.name(b"Type").map_or_else(|| "stream".into(), |t| String::from_utf8_lossy(t).into_owned()),
            Object::Dict(d) => {
                c.subject = subject(d);
                if is_field(d) {
                    c.field = field_name(doc, d);
                    c.after = field_value(doc, d);
                    c.before = before.as_deref().and_then(Object::as_dict).and_then(|b| field_value(&old, b));
                }
                // Annotations the page no longer lists.
                if d.name(b"Type") == Some(b"Page")
                    && let Some(was) = before.as_deref().and_then(Object::as_dict)
                {
                    let now_annots: HashSet<ObjRef> = refs(doc, d.get(b"Annots")).into_iter().collect();
                    for a in refs(&old, was.get(b"Annots")).into_iter().filter(|a| !now_annots.contains(a)) {
                        let Some(ad) = old.get(a).as_dict().cloned() else { continue };
                        let mut removed = VersionChange {
                            permitted,
                            ..change(kind, ChangeAction::Removed, page_of(&old, &old_on_page, r), Some(a), "", permissions)
                        };
                        removed.subject = subject(&ad);
                        if is_field(&ad) {
                            removed.field = field_name(&old, &ad);
                            removed.before = field_value(&old, &ad);
                        }
                        truncated |= !push(&mut changes, removed);
                    }
                }
            }
            _ => c.subject = "object".into(),
        }
        truncated |= !push(&mut changes, c);
    }
    // Page order, document-level changes last; stable otherwise.
    changes.sort_by_key(|c| (c.page.is_none(), c.page));
    out.changes = changes;
    out.truncated = truncated;
    Ok(out)
}

fn change(kind: &str, action: ChangeAction, page: Option<usize>, object: Option<ObjRef>, subject: &str, p: Option<u8>) -> VersionChange {
    VersionChange {
        kind: kind.to_string(),
        action,
        page,
        object,
        subject: subject.to_string(),
        field: None,
        before: None,
        after: None,
        permitted: kind_permitted(kind, p),
    }
}

fn refs(doc: &Document, o: Option<&Object>) -> Vec<ObjRef> {
    o.map(|o| doc.resolve(o)).and_then(|a| a.as_array().map(|a| a.iter().filter_map(Object::as_ref).collect())).unwrap_or_default()
}

/// What a changed dictionary is, in a word or two.
fn subject(d: &Dict) -> String {
    let name = |n: &[u8]| String::from_utf8_lossy(n).into_owned();
    match (d.name(b"Type"), d.name(b"Subtype")) {
        (Some(b"Page"), _) => "page".into(),
        (Some(b"Pages"), _) => "page tree".into(),
        (Some(b"Catalog"), _) => "catalog".into(),
        (Some(b"Sig" | b"DocTimeStamp"), _) => "signature dictionary".into(),
        (Some(b"SigRef" | b"TransformParams"), _) => "signature reference".into(),
        (_, Some(s)) if d.contains(b"Rect") || d.name(b"Type") == Some(b"Annot") => name(s),
        _ if d.contains(b"Fields") => "interactive form".into(),
        _ if is_field(d) => "form field".into(),
        _ if d.contains(b"Producer") || d.contains(b"ModDate") || d.contains(b"CreationDate") => "document information".into(),
        (Some(t), _) => name(t),
        _ => "object".into(),
    }
}

fn is_field(d: &Dict) -> bool {
    d.contains(b"FT") || d.contains(b"T") || (d.name(b"Subtype") == Some(b"Widget") && d.contains(b"Parent"))
}

/// The fully qualified name of the field `d` is (or is a widget of).
fn field_name(doc: &Document, d: &Dict) -> Option<String> {
    let mut parts = Vec::new();
    let mut cur = Some(d.clone());
    let mut seen = HashSet::new();
    for _ in 0..MAX_FIELD_DEPTH {
        let Some(x) = cur.take() else { break };
        if let Some(t) = text(doc, &x, b"T") {
            parts.push(t);
        }
        cur = x.reference(b"Parent").filter(|p| seen.insert(*p)).and_then(|p| doc.get(p).as_dict().cloned());
    }
    parts.reverse();
    (!parts.is_empty()).then(|| parts.join("."))
}

/// A field's value (`/V`, inherited from its parents), as text, cut to [`MAX_VALUE_CHARS`].
fn field_value(doc: &Document, d: &Dict) -> Option<String> {
    let mut cur = Some(d.clone());
    let mut seen = HashSet::new();
    for _ in 0..MAX_FIELD_DEPTH {
        let x = cur.take()?;
        if let Some(v) = x.get(b"V") {
            let v = doc.resolve(v);
            let s = match &*v {
                Object::String(s) => s.to_text(),
                Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
                Object::Array(a) => a
                    .iter()
                    .take(64)
                    .filter_map(|e| match &*doc.resolve(e) {
                        Object::String(s) => Some(s.to_text()),
                        Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                Object::Dict(_) => "(signature)".into(),
                Object::Int(i) => i.to_string(),
                _ => return None,
            };
            return Some(s.chars().take(MAX_VALUE_CHARS).collect());
        }
        cur = x.reference(b"Parent").filter(|p| seen.insert(*p)).and_then(|p| doc.get(p).as_dict().cloned());
    }
    None
}
