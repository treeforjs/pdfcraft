//! Field locks (ISO 32000-2 §12.7.5.5 and §12.8.2.4, FieldMDP): a signature field's `/Lock`
//! names the form fields that become locked once it is signed, and signing records the lock in
//! the signature as a FieldMDP transform. A lock may also restrict later changes to the whole
//! document (`/P`, as DocMDP does: the viewer's "Lock document after signing").

use std::collections::HashSet;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString};

/// Most fields walked in one form (cyclic or huge field trees stop there).
const MAX_FIELDS: usize = 100_000;
/// Deepest field tree walked.
const MAX_DEPTH: usize = 64;

/// Which fields a lock covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockAction {
    /// Every field in the form.
    All,
    /// Only these fields (fully qualified names).
    Include(Vec<String>),
    /// Every field except these.
    Exclude(Vec<String>),
}

/// A field lock: what signing locks, and optionally what later changes the document allows
/// (DocMDP permissions 1–3: 1 none, 2 form fill and signing, 3 also comments).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldLock {
    pub action: LockAction,
    pub permissions: Option<u8>,
}

impl FieldLock {
    /// Lock every field, and the document (no changes after signing).
    pub fn document() -> Self {
        Self { action: LockAction::All, permissions: Some(1) }
    }

    /// Does the lock cover the field named `name`? A lock on a field also covers the fields
    /// under it (`a` covers `a.b`).
    pub fn covers(&self, name: &str) -> bool {
        let listed = |names: &[String]| names.iter().any(|n| name == n || name.strip_prefix(n.as_str()).is_some_and(|rest| rest.starts_with('.')));
        match &self.action {
            LockAction::All => true,
            LockAction::Include(names) => listed(names),
            LockAction::Exclude(names) => !listed(names),
        }
    }

    /// Read a `/Lock` (SigFieldLock) or FieldMDP `/TransformParams` dictionary. A missing or
    /// unknown `/Action` makes no lock.
    pub fn read(doc: &Document, d: &Dict) -> Option<Self> {
        let names = || -> Vec<String> {
            let list = d.get(b"Fields").map(|f| doc.resolve(f)).and_then(|f| f.as_array().cloned()).unwrap_or_default();
            list.iter().filter_map(|n| doc.resolve(n).as_string().map(PdfString::to_text)).take(MAX_FIELDS).collect()
        };
        let action = match d.name(b"Action")? {
            b"All" => LockAction::All,
            b"Include" => LockAction::Include(names()),
            b"Exclude" => LockAction::Exclude(names()),
            _ => return None,
        };
        let permissions = d.get(b"P").map(|p| doc.resolve(p)).and_then(|p| p.as_int()).filter(|p| (1..=3).contains(p)).map(|p| p as u8);
        Some(Self { action, permissions })
    }

    /// The lock as a dictionary of type `ty` (`SigFieldLock` or `TransformParams`).
    pub fn to_dict(&self, ty: &str) -> Dict {
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name(ty));
        let (action, names) = match &self.action {
            LockAction::All => ("All", None),
            LockAction::Include(n) => ("Include", Some(n)),
            LockAction::Exclude(n) => ("Exclude", Some(n)),
        };
        d.set(b"Action".to_vec(), Object::name(action));
        if let Some(names) = names {
            d.set(b"Fields".to_vec(), Object::Array(names.iter().map(|n| Object::from(PdfString::text(n))).collect()));
        }
        if let Some(p) = self.permissions {
            d.set(b"P".to_vec(), Object::Int(i64::from(p.clamp(1, 3))));
        }
        if ty == "TransformParams" {
            d.set(b"V".to_vec(), Object::name("1.2"));
        }
        d
    }
}

/// One terminal form field: its full name, its dictionary's reference, the objects that hold
/// its value and look (itself and its widgets), and whether it is a signature field.
#[derive(Clone, Debug)]
pub(crate) struct FormField {
    pub name: String,
    pub field: ObjRef,
    pub objects: Vec<ObjRef>,
    pub signature: bool,
}

/// Every terminal field of the form, walked depth-first in `/Fields` order.
pub(crate) fn form_fields(doc: &Document) -> Vec<FormField> {
    let mut out = Vec::new();
    let Some(root) = doc.root() else { return out };
    let Some(af) = doc.get(root).as_dict().and_then(|c| c.get(b"AcroForm").map(|a| doc.resolve(a))).and_then(|a| a.as_dict().cloned()) else {
        return out;
    };
    let Some(fields) = af.get(b"Fields").map(|f| doc.resolve(f)).and_then(|f| f.as_array().cloned()) else { return out };
    let mut seen = HashSet::new();
    let mut stack: Vec<(Object, String, bool, usize)> = fields.into_iter().rev().map(|f| (f, String::new(), false, 0)).collect();
    while let Some((o, prefix, inherited_sig, depth)) = stack.pop() {
        let Some(r) = o.as_ref() else { continue };
        if !seen.insert(r) || seen.len() > MAX_FIELDS || depth > MAX_DEPTH {
            continue;
        }
        let Some(d) = doc.get(r).as_dict().cloned() else { continue };
        let part = d.get(b"T").map(|t| doc.resolve(t)).and_then(|t| t.as_string().map(PdfString::to_text));
        let name = match (&part, prefix.is_empty()) {
            (Some(p), true) => p.clone(),
            (Some(p), false) => format!("{prefix}.{p}"),
            (None, _) => prefix.clone(),
        };
        let signature = d.name(b"FT").map_or(inherited_sig, |ft| ft == b"Sig");
        let kids: Vec<Object> = d.get(b"Kids").map(|k| doc.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default();
        let (sub, widgets): (Vec<Object>, Vec<Object>) =
            kids.into_iter().partition(|k| k.as_ref().is_some_and(|kr| doc.get(kr).as_dict().is_some_and(|kd| kd.contains(b"T"))));
        if sub.is_empty() {
            let mut objects = vec![r];
            objects.extend(widgets.iter().filter_map(Object::as_ref));
            out.push(FormField { name, field: r, objects, signature });
        } else {
            for k in sub.into_iter().rev() {
                stack.push((k, name.clone(), signature, depth + 1));
            }
        }
    }
    out
}

/// Mark every field `lock` covers read-only (field flag 1), signature fields excepted so they
/// can still be signed. Returns how many fields were locked.
pub(crate) fn lock_fields(doc: &mut Document, lock: &FieldLock) -> Result<usize, pdfcraft_cos::CosError> {
    let mut n = 0;
    for f in form_fields(doc) {
        if f.signature || !lock.covers(&f.name) {
            continue;
        }
        let flags = doc.get(f.field).as_dict().and_then(|d| d.int(b"Ff")).unwrap_or(0);
        if flags & 1 == 0 {
            doc.update_dict(f.field, |d| d.set(b"Ff".to_vec(), Object::Int(flags | 1)))?;
        }
        n += 1;
    }
    Ok(n)
}

/// The objects of the fields `lock` covers (signature fields excepted).
pub(crate) fn locked_objects(doc: &Document, lock: &FieldLock) -> HashSet<ObjRef> {
    form_fields(doc).into_iter().filter(|f| !f.signature && lock.covers(&f.name)).flat_map(|f| f.objects).collect()
}

/// Set (or with `None`, clear) the lock of the unsigned signature field `field` (Prepare Form ▸
/// signature field properties ▸ Signed: mark fields read-only when signed).
pub fn set_field_lock(doc: &mut Document, field: &str, lock: Option<&FieldLock>) -> Result<(), crate::SignError> {
    let f = form_fields(doc)
        .into_iter()
        .find(|f| f.signature && f.name == field)
        .ok_or_else(|| crate::SignError::Pdf(format!("there is no signature field {field:?}")))?;
    if doc.get(f.field).as_dict().is_some_and(|d| d.contains(b"V")) {
        return Err(crate::SignError::Pdf(format!("the field {field:?} is already signed")));
    }
    doc.update_dict(f.field, |d| match lock {
        Some(l) => d.set(b"Lock".to_vec(), Object::Dict(l.to_dict("SigFieldLock"))),
        None => {
            d.remove(b"Lock");
        }
    })?;
    Ok(())
}

/// The lock an unsigned signature field `field` will apply when signed.
pub fn field_lock(doc: &Document, field: &str) -> Option<FieldLock> {
    let f = form_fields(doc).into_iter().find(|f| f.signature && f.name == field)?;
    let d = doc.get(f.field).as_dict().cloned()?;
    let l = doc.resolve(d.get(b"Lock")?);
    FieldLock::read(doc, l.as_dict()?)
}
