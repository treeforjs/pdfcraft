//! Form fields arriving with copied pages (Combine Files, Insert Pages from File, Create PDF from
//! multiple files).
//!
//! Field values are keyed by fully qualified name: two top-level fields called `name` from two
//! different files would, once both are listed in one `/AcroForm`, be one field to most viewers
//! (filling one fills the other, or only the first is reachable). So when an incoming top-level
//! field's partial name is already taken in the destination:
//!
//! - [`FieldConflicts::Rename`] (the default) renames the incoming hierarchy by suffixing its
//!   top-level partial name (`name` → `name#2`, then `name#3`…; deterministic, in import order).
//!   Only `/T` of the top-level field changes, so widget ↔ parent links, kids and their partial
//!   names stay as they were, and every fully qualified name below it moves along.
//! - [`FieldConflicts::Merge`] makes the two one field with several widgets, but only when they
//!   are compatible: both terminal (no named kids), the same field type and type flags (radio,
//!   push button, combo, list multi-select, file select), equal options and maximum length, and
//!   not signature fields. The destination's value wins. Anything incompatible is renamed.
//!
//! Alongside, the source form's defaults come along without disturbing the destination's:
//! - `/DR` resources are merged per category. A same-named resource that differs is added under a
//!   new name (`Helv` → `Helv_2`) and every incoming `/DA` that named it is rewritten;
//! - the source's form-level `/DA` and `/Q` are pushed down onto incoming top-level fields that
//!   relied on them when the destination's defaults differ;
//! - the calculation order (`/CO`) of copied fields is appended;
//! - `/NeedAppearances` is set when the source had it (or merged widgets show a stale value), and
//!   `/SigFlags` gains SignaturesExist when a signed signature field came along. AppendOnly is not
//!   carried: a copied signature cannot validate in the new file anyway (its `/ByteRange` covers
//!   the source file), and renaming its ancestors does not change that.
//!
//! JavaScript in the copied objects (field and widget actions, page actions) is updated where the
//! name is a string literal: the argument of `getField(…)`, and the names listed in
//! `AFSimple_Calculate(…)` and `resetForm(…)`. Names built at run time (`"total" + i`), literals
//! with escape sequences, UTF-16 script streams and document-level scripts (which are not copied)
//! are not updated; computed `getField` calls are counted in [`FieldReport::scripts_dynamic`].
//! A signature's `/Lock` or FieldMDP field lists are not rewritten.

use std::collections::{HashMap, HashSet};

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::OrganizeError;

/// What to do when an incoming top-level form field has a name already used in the destination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FieldConflicts {
    /// Rename the incoming field (`name#2`), keeping its value independent.
    #[default]
    Rename,
    /// Join compatible fields into one (shared value); rename incompatible ones.
    Merge,
}

/// What happened to incoming form fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldReport {
    /// Top-level fields renamed: (old partial name, new partial name).
    pub renamed: Vec<(String, String)>,
    /// Names whose incoming field was joined with the existing one.
    pub merged: Vec<String>,
    /// Default resources added under a new name: ("Font/Helv", "Font/Helv_2").
    pub resources_renamed: Vec<(String, String)>,
    /// String literals in copied JavaScript updated to a new field name.
    pub scripts_updated: usize,
    /// `getField(…)` calls in copied JavaScript whose name is computed, so could not be updated.
    pub scripts_dynamic: usize,
}

impl FieldReport {
    pub(crate) fn absorb(&mut self, other: FieldReport) {
        self.renamed.extend(other.renamed);
        self.merged.extend(other.merged);
        self.resources_renamed.extend(other.resources_renamed);
        self.scripts_updated = self.scripts_updated.saturating_add(other.scripts_updated);
        self.scripts_dynamic = self.scripts_dynamic.saturating_add(other.scripts_dynamic);
    }
}

/// What one import brought along, all already copied into the destination.
pub(crate) struct Incoming<'a> {
    /// The source's `/AcroForm` (source-document objects; only scalars are read from it).
    pub form: Option<Dict>,
    /// The source's `/DR`, copied into the destination.
    pub dr: Option<Object>,
    /// The source's `/CO` entries mapped to their copies.
    pub co: Vec<ObjRef>,
    /// Copied top-level fields, in the order they were reached.
    pub fields: &'a [ObjRef],
    /// Every destination object created by the import (for script updates), sorted.
    pub created: Vec<ObjRef>,
}

/// Bound on nodes visited in one field hierarchy.
const MAX_NODES: usize = 100_000;
/// Bound on hierarchy depth.
const MAX_DEPTH: usize = 64;

/// Field-dictionary keys (the rest of a merged field/widget dictionary belongs to the widget).
const FIELD_KEYS: &[&[u8]] = &[b"T", b"TU", b"TM", b"FT", b"Ff", b"V", b"DV", b"Opt", b"TI", b"I", b"MaxLen", b"RV", b"Lock", b"SV", b"Kids"];
/// `/AA` triggers that belong to the field (keystroke, format, validate, calculate).
const FIELD_TRIGGERS: &[&[u8]] = &[b"K", b"F", b"V", b"C"];
/// `/Ff` bits that make a different kind of field of the same `/FT`.
const KIND_FLAGS: i64 = (1 << 15) | (1 << 16) | (1 << 17) | (1 << 20) | (1 << 21);

/// Register incoming top-level fields in the destination's `/AcroForm`, resolving name
/// conflicts per `policy` and merging the form's defaults (see the module documentation).
pub(crate) fn register(dst: &mut Document, inc: &Incoming<'_>, policy: FieldConflicts) -> Result<FieldReport, OrganizeError> {
    let mut report = FieldReport::default();
    if inc.fields.is_empty() {
        return Ok(report);
    }
    let root = dst.root().ok_or(OrganizeError::NoPageTree)?;
    let catalog = dst.get(root).as_dict().cloned().ok_or(OrganizeError::NoPageTree)?;
    let (form_ref, mut form) = match catalog.get(b"AcroForm") {
        Some(Object::Ref(r)) => (Some(*r), dst.get(*r).as_dict().cloned().unwrap_or_default()),
        Some(Object::Dict(d)) => (None, d.clone()),
        _ => (None, Dict::new()),
    };
    let array = |doc: &Document, d: &Dict, k: &[u8]| d.get(k).map(|f| doc.resolve(f)).and_then(|f| f.as_array().cloned()).unwrap_or_default();
    let mut list = array(dst, &form, b"Fields");
    let mut co = array(dst, &form, b"CO");
    let fresh = list.is_empty();

    // Default resources, then the defaults that name them.
    let mut dr = form.get(b"DR").map(|d| dst.resolve(d)).and_then(|d| d.as_dict().cloned()).unwrap_or_default();
    let fonts = merge_dr(dst, &mut dr, inc.dr.as_ref(), &mut report);
    if !dr.is_empty() {
        match form.get(b"DR") {
            Some(Object::Ref(r)) => dst.set(*r, Object::Dict(dr)),
            _ => form.set(b"DR".to_vec(), Object::Dict(dr)),
        }
    }
    let src_form = inc.form.clone().unwrap_or_default();
    let src_da = da_of(&src_form).map(|d| rewrite_da(&d, &fonts));
    let src_q = src_form.int(b"Q");
    if fresh {
        if !form.contains(b"DA")
            && let Some(da) = &src_da
        {
            form.set(b"DA".to_vec(), Object::String(PdfString::literal(da.clone())));
        }
        if !form.contains(b"Q")
            && let Some(q) = src_q
        {
            form.set(b"Q".to_vec(), Object::Int(q));
        }
    }
    let dst_da = da_of(&form);
    let dst_q = form.int(b"Q").unwrap_or(0);

    let mut signed = false;
    for &f in inc.fields {
        let nodes = subtree(dst, f);
        for &n in &nodes {
            let Some(d) = dst.get(n).as_dict().cloned() else { continue };
            if d.name(b"FT") == Some(b"Sig") && d.get(b"V").is_some_and(|v| !matches!(v, Object::Null)) {
                signed = true;
            }
            if !fonts.is_empty()
                && let Some(da) = da_of(&d)
            {
                let new = rewrite_da(&da, &fonts);
                if new != da {
                    dst.update_dict(n, |d| d.set(b"DA".to_vec(), Object::String(PdfString::literal(new))))?;
                }
            }
        }
        let Some(d) = dst.get(f).as_dict().cloned() else { continue };
        if !d.contains(b"DA")
            && let Some(da) = &src_da
            && dst_da.as_ref() != Some(da)
        {
            dst.update_dict(f, |d| d.set(b"DA".to_vec(), Object::String(PdfString::literal(da.clone()))))?;
        }
        if !d.contains(b"Q") && src_q.unwrap_or(0) != dst_q {
            dst.update_dict(f, |d| d.set(b"Q".to_vec(), Object::Int(src_q.unwrap_or(0))))?;
        }
    }

    // Names.
    let mut taken: HashMap<String, ObjRef> = HashMap::new();
    for o in &list {
        if let Some(r) = o.as_ref()
            && let Some(name) = partial_name(dst, r)
        {
            taken.entry(name).or_insert(r);
        }
    }
    let mut renames: Vec<(String, String)> = Vec::new();
    let mut merged_into: HashMap<ObjRef, ObjRef> = HashMap::new();
    let mut stale = false;
    for &f in inc.fields {
        let Some(name) = partial_name(dst, f) else {
            push_unique(&mut list, f);
            continue;
        };
        let existing = match taken.get(&name) {
            None => {
                taken.insert(name, f);
                push_unique(&mut list, f);
                continue;
            }
            Some(e) if *e == f => continue,
            Some(e) => *e,
        };
        if policy == FieldConflicts::Merge && compatible(dst, existing, f) {
            let field = merge(dst, existing, f, &mut list, &mut co, &mut stale)?;
            taken.insert(name.clone(), field);
            merged_into.insert(f, field);
            report.merged.push(name);
            continue;
        }
        let mut n: usize = 2;
        let mut new = format!("{name}#{n}");
        while taken.contains_key(&new) {
            n = n.saturating_add(1);
            new = format!("{name}#{n}");
        }
        dst.update_dict(f, |d| d.set(b"T".to_vec(), Object::String(PdfString::text(&new))))?;
        taken.insert(new.clone(), f);
        push_unique(&mut list, f);
        renames.push((name.clone(), new.clone()));
        report.renamed.push((name, new));
    }

    // Calculation order.
    for r in &inc.co {
        let r = merged_into.get(r).copied().unwrap_or(*r);
        if !co.contains(&Object::Ref(r)) && dst.get(r).as_dict().is_some() {
            co.push(Object::Ref(r));
        }
    }

    if !renames.is_empty() {
        update_scripts(dst, &inc.created, &renames, &mut report)?;
    }

    if src_form.get(b"NeedAppearances").is_some_and(|v| matches!(v, Object::Bool(true))) || stale {
        form.set(b"NeedAppearances".to_vec(), Object::Bool(true));
    }
    if signed {
        let flags = form.int(b"SigFlags").unwrap_or(0);
        form.set(b"SigFlags".to_vec(), Object::Int(flags | 1));
    }
    form.set(b"Fields".to_vec(), Object::Array(list));
    if !co.is_empty() {
        form.set(b"CO".to_vec(), Object::Array(co));
    }
    match form_ref {
        Some(r) => dst.set(r, Object::Dict(form)),
        None => {
            let r = dst.add(form);
            dst.update_dict(root, |c| c.set(b"AcroForm".to_vec(), Object::Ref(r)))?;
        }
    }
    Ok(report)
}

fn push_unique(list: &mut Vec<Object>, r: ObjRef) {
    if !list.contains(&Object::Ref(r)) {
        list.push(Object::Ref(r));
    }
}

fn partial_name(doc: &Document, r: ObjRef) -> Option<String> {
    let obj = doc.get(r);
    let t = obj.as_dict()?.get(b"T")?;
    Some(doc.resolve(t).as_string()?.to_text())
}

fn da_of(d: &Dict) -> Option<Vec<u8>> {
    d.get(b"DA").and_then(|v| v.as_string()).map(|s| s.bytes.clone())
}

/// `top` and every node below it through `/Kids` (fields and widgets), cycle- and size-safe.
fn subtree(doc: &Document, top: ObjRef) -> Vec<ObjRef> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![(top, 0usize)];
    while let Some((r, depth)) = stack.pop() {
        if depth > MAX_DEPTH || out.len() >= MAX_NODES || !seen.insert(r) {
            continue;
        }
        out.push(r);
        let obj = doc.get(r);
        let Some(kids) = obj.as_dict().and_then(|d| d.get(b"Kids")).map(|k| doc.resolve(k)) else { continue };
        if let Some(a) = kids.as_array() {
            stack.extend(a.iter().rev().filter_map(Object::as_ref).map(|k| (k, depth + 1)));
        }
    }
    out
}

/// The kids of a field that are fields themselves (have a partial name).
fn named_kids(doc: &Document, d: &Dict) -> bool {
    let kids = d.get(b"Kids").map(|k| doc.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default();
    kids.iter().any(|k| doc.resolve(k).as_dict().is_some_and(|kd| kd.contains(b"T")))
}

/// Whether two top-level fields can be one field (see [`FieldConflicts::Merge`]).
fn compatible(doc: &Document, a: ObjRef, b: ObjRef) -> bool {
    let (Some(da), Some(db)) = (doc.get(a).as_dict().cloned(), doc.get(b).as_dict().cloned()) else { return false };
    let ft = da.name(b"FT");
    if ft.is_none() || ft != db.name(b"FT") || ft == Some(b"Sig") {
        return false;
    }
    if named_kids(doc, &da) || named_kids(doc, &db) {
        return false;
    }
    let kind = |d: &Dict| d.get(b"Ff").and_then(|f| doc.resolve(f).as_int()).unwrap_or(0) & KIND_FLAGS;
    if kind(&da) != kind(&db) {
        return false;
    }
    let mut budget = 10_000usize;
    [&b"Opt"[..], b"MaxLen"].iter().all(|k| match (da.get(k), db.get(k)) {
        (None, None) => true,
        (Some(x), Some(y)) => same(doc, x, y, 0, &mut budget),
        _ => false,
    })
}

/// Split a merged field/widget dictionary into (field part, widget part).
fn split_keys(d: &Dict) -> (Dict, Dict) {
    let mut field = Dict::new();
    let mut widget = Dict::new();
    for (k, v) in d.iter() {
        let k = k.as_slice();
        if k == b"AA" {
            let Some(aa) = v.as_dict() else {
                widget.set(k.to_vec(), v.clone());
                continue;
            };
            let (mut fa, mut wa) = (Dict::new(), Dict::new());
            for (t, a) in aa.iter() {
                if FIELD_TRIGGERS.contains(&t.as_slice()) { fa.set(t.clone(), a.clone()) } else { wa.set(t.clone(), a.clone()) }
            }
            if !fa.is_empty() {
                field.set(b"AA".to_vec(), Object::Dict(fa));
            }
            if !wa.is_empty() {
                widget.set(b"AA".to_vec(), Object::Dict(wa));
            }
        } else if k == b"DA" || k == b"Q" {
            // Variable-text defaults: on the field for inheritance, and on the widget for its look.
            field.set(k.to_vec(), v.clone());
            widget.set(k.to_vec(), v.clone());
        } else if FIELD_KEYS.contains(&k) {
            field.set(k.to_vec(), v.clone());
        } else {
            widget.set(k.to_vec(), v.clone());
        }
    }
    (field, widget)
}

/// Join incoming field `f` into existing field `e`; returns the field that now holds both.
fn merge(dst: &mut Document, e: ObjRef, f: ObjRef, list: &mut [Object], co: &mut [Object], stale: &mut bool) -> Result<ObjRef, OrganizeError> {
    let ed = dst.get(e).as_dict().cloned().unwrap_or_default();
    // The existing field must be a parent of its widgets.
    let field = if ed.contains(b"Kids") {
        e
    } else {
        let (mut fd, mut wd) = split_keys(&ed);
        fd.set(b"Kids".to_vec(), Object::Array(vec![Object::Ref(e)]));
        let field = dst.add(fd);
        wd.set(b"Parent".to_vec(), Object::Ref(field));
        dst.set(e, Object::Dict(wd));
        for o in list.iter_mut().chain(co.iter_mut()) {
            if *o == Object::Ref(e) {
                *o = Object::Ref(field);
            }
        }
        field
    };
    let fdict = dst.get(f).as_dict().cloned().unwrap_or_default();
    let mut widgets = Vec::new();
    if let Some(kids) = fdict.get(b"Kids").map(|k| dst.resolve(k)).and_then(|k| k.as_array().cloned()) {
        for k in kids.iter().filter_map(Object::as_ref) {
            dst.update_dict(k, |w| {
                w.set(b"Parent".to_vec(), Object::Ref(field));
                for key in [&b"DA"[..], b"Q"] {
                    if !w.contains(key)
                        && let Some(v) = fdict.get(key)
                    {
                        w.set(key.to_vec(), v.clone());
                    }
                }
            })?;
            widgets.push(k);
        }
        dst.set(f, Object::Null);
    } else {
        let (_, mut wd) = split_keys(&fdict);
        wd.set(b"Parent".to_vec(), Object::Ref(field));
        dst.set(f, Object::Dict(wd));
        widgets.push(f);
    }
    let held = dst.get(field).as_dict().cloned().unwrap_or_default();
    let mut kids = held.get(b"Kids").map(|k| dst.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default();
    kids.extend(widgets.iter().map(|w| Object::Ref(*w)));
    dst.update_dict(field, |d| d.set(b"Kids".to_vec(), Object::Array(kids)))?;
    // The existing value wins; make the incoming widgets show it.
    let value = held.get(b"V").map(|v| dst.resolve(v).as_ref().clone());
    let incoming = fdict.get(b"V").map(|v| dst.resolve(v).as_ref().clone());
    if value != incoming {
        if held.name(b"FT") == Some(b"Btn") {
            let state = value.as_ref().and_then(|v| v.as_name().map(<[u8]>::to_vec)).unwrap_or_else(|| b"Off".to_vec());
            for w in &widgets {
                let wd = dst.get(*w).as_dict().cloned().unwrap_or_default();
                let has = wd
                    .get(b"AP")
                    .map(|a| dst.resolve(a))
                    .and_then(|a| a.as_dict().and_then(|a| a.get(b"N")).map(|n| dst.resolve(n)))
                    .is_some_and(|n| n.as_dict().is_some_and(|n| n.contains(&state)));
                let s = if has { state.clone() } else { b"Off".to_vec() };
                dst.update_dict(*w, |d| d.set(b"AS".to_vec(), Object::Name(s)))?;
            }
        } else {
            *stale = true;
        }
    }
    Ok(field)
}

/// Merge the incoming (already copied) `/DR` into `dr`. Returns the font renames.
fn merge_dr(dst: &Document, dr: &mut Dict, incoming: Option<&Object>, report: &mut FieldReport) -> HashMap<Vec<u8>, Vec<u8>> {
    let mut fonts = HashMap::new();
    let Some(inc) = incoming.map(|i| dst.resolve(i)).and_then(|i| i.as_dict().cloned()) else { return fonts };
    let mut budget = 100_000usize;
    for (cat, v) in inc.iter() {
        let Some(entries) = dst.resolve(v).as_dict().cloned() else {
            if !dr.contains(cat) {
                dr.set(cat.clone(), v.clone());
            }
            continue;
        };
        let mut mine = dr.get(cat).map(|m| dst.resolve(m)).and_then(|m| m.as_dict().cloned()).unwrap_or_default();
        for (name, value) in entries.iter() {
            match mine.get(name) {
                None => mine.set(name.clone(), value.clone()),
                Some(existing) if same(dst, existing, value, 0, &mut budget) => {}
                Some(_) => {
                    let mut n: usize = 2;
                    let mut new = [name.as_slice(), format!("_{n}").as_bytes()].concat();
                    while mine.contains(&new) {
                        n = n.saturating_add(1);
                        new = [name.as_slice(), format!("_{n}").as_bytes()].concat();
                    }
                    mine.set(new.clone(), value.clone());
                    let cat_s = String::from_utf8_lossy(cat);
                    report
                        .resources_renamed
                        .push((format!("{cat_s}/{}", String::from_utf8_lossy(name)), format!("{cat_s}/{}", String::from_utf8_lossy(&new))));
                    if cat.as_slice() == b"Font" {
                        fonts.insert(name.clone(), new);
                    }
                }
            }
        }
        dr.set(cat.clone(), Object::Dict(mine));
    }
    fonts
}

/// Structural equality of two objects of `doc` (references followed), bounded; when the bound is
/// reached the objects count as different (so the caller keeps both).
fn same(doc: &Document, a: &Object, b: &Object, depth: usize, budget: &mut usize) -> bool {
    if a == b {
        return true;
    }
    if depth > 12 || *budget == 0 {
        return false;
    }
    *budget -= 1;
    let (ra, rb) = (doc.resolve(a), doc.resolve(b));
    let dicts = |x: &Dict, y: &Dict, budget: &mut usize| {
        let keys = |d: &Dict| d.iter().filter(|(k, _)| k.as_slice() != b"Length").count();
        keys(x) == keys(y)
            && x.iter().filter(|(k, _)| k.as_slice() != b"Length").all(|(k, v)| y.get(k).is_some_and(|w| same(doc, v, w, depth + 1, budget)))
    };
    match (ra.as_ref(), rb.as_ref()) {
        (Object::Dict(x), Object::Dict(y)) => dicts(x, y, budget),
        (Object::Stream(x), Object::Stream(y)) => x.raw == y.raw && dicts(&x.dict, &y.dict, budget),
        (Object::Array(x), Object::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(v, w)| same(doc, v, w, depth + 1, budget)),
        (x, y) => x == y,
    }
}

/// Replace font names in a `/DA` string (`/Helv 12 Tf 0 g`).
fn rewrite_da(da: &[u8], fonts: &HashMap<Vec<u8>, Vec<u8>>) -> Vec<u8> {
    if fonts.is_empty() {
        return da.to_vec();
    }
    let delim = |b: u8| b.is_ascii_whitespace() || b"/()<>[]{}%".contains(&b);
    let mut out = Vec::with_capacity(da.len());
    let mut i = 0;
    while let Some(&b) = da.get(i) {
        if b != b'/' {
            out.push(b);
            i += 1;
            continue;
        }
        let rest = da.get(i + 1..).unwrap_or_default();
        let len = rest.iter().position(|c| delim(*c)).unwrap_or(rest.len());
        let name = rest.get(..len).unwrap_or_default();
        out.push(b'/');
        out.extend_from_slice(fonts.get(name).map_or(name, Vec::as_slice));
        i += 1 + len;
    }
    out
}

/// Update field names in the JavaScript of the created objects.
fn update_scripts(dst: &mut Document, created: &[ObjRef], renames: &[(String, String)], report: &mut FieldReport) -> Result<(), OrganizeError> {
    let mut streams = HashSet::new();
    for &r in created {
        let obj = dst.get(r);
        let mut changed = false;
        let mut updated = obj.as_ref().clone();
        let mut stream_refs = Vec::new();
        visit_actions(&mut updated, 0, &mut |action| match action.get(b"JS") {
            Some(Object::String(s)) => {
                let utf16 = s.bytes.starts_with(&[0xFE, 0xFF]);
                let text = if utf16 { s.to_text().into_bytes() } else { s.bytes.clone() };
                let (new, n, dynamic) = rewrite_script(&text, renames);
                report.scripts_dynamic = report.scripts_dynamic.saturating_add(dynamic);
                if n > 0 {
                    let new = if utf16 { PdfString::text(&String::from_utf8_lossy(&new)) } else { PdfString::literal(new) };
                    action.set(b"JS".to_vec(), Object::String(new));
                    report.scripts_updated = report.scripts_updated.saturating_add(n);
                    changed = true;
                }
            }
            Some(Object::Ref(s)) => stream_refs.push(*s),
            _ => {}
        });
        if changed {
            dst.set(r, updated);
        }
        for s in stream_refs {
            if !streams.insert(s) || created.binary_search(&s).is_err() {
                continue;
            }
            let Object::Stream(st) = dst.get(s).as_ref().clone() else { continue };
            let Ok(data) = st.decoded() else { continue };
            if data.starts_with(&[0xFE, 0xFF]) {
                continue;
            }
            let (new, n, dynamic) = rewrite_script(&data, renames);
            report.scripts_dynamic = report.scripts_dynamic.saturating_add(dynamic);
            if n > 0 {
                let mut d = st.dict.clone();
                d.remove(b"Length");
                dst.set(s, Object::Stream(Stream::flate(d, &new)));
                report.scripts_updated = report.scripts_updated.saturating_add(n);
            }
        }
    }
    Ok(())
}

/// Call `f` on every JavaScript action dictionary held directly in `o` (not through references:
/// referenced objects are visited on their own).
fn visit_actions(o: &mut Object, depth: usize, f: &mut dyn FnMut(&mut Dict)) {
    if depth > 16 {
        return;
    }
    let d = match o {
        Object::Dict(d) => d,
        Object::Stream(s) => &mut s.dict,
        Object::Array(a) => {
            for x in a.iter_mut() {
                visit_actions(x, depth + 1, f);
            }
            return;
        }
        _ => return,
    };
    if d.name(b"S") == Some(b"JavaScript") {
        f(d);
    }
    let keys: Vec<Vec<u8>> = d.iter().map(|(k, _)| k.clone()).collect();
    for k in keys {
        if let Some(v) = d.get_mut(&k) {
            visit_actions(v, depth + 1, f);
        }
    }
}

/// Calls whose string arguments are field names.
const NAME_LIST_CALLS: &[&[u8]] = &[b"AFSimple_Calculate", b"resetForm"];
/// `AFSimple_Calculate` operations, which are not field names.
const OPERATIONS: &[&[u8]] = &[b"SUM", b"AVG", b"PRD", b"MIN", b"MAX"];

/// Rewrite field-name string literals in `js`. Returns (script, literals changed, computed
/// `getField` calls seen).
fn rewrite_script(js: &[u8], renames: &[(String, String)]) -> (Vec<u8>, usize, usize) {
    let mut out: Vec<u8> = Vec::with_capacity(js.len());
    let (mut changed, mut dynamic) = (0usize, 0usize);
    // For each open parenthesis: whether it belongs to a call listing field names.
    let mut parens: Vec<bool> = Vec::new();
    let mut i = 0;
    let trimmed = |o: &[u8]| -> usize { o.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(0, |p| p + 1) };
    while let Some(&c) = js.get(i) {
        let next = js.get(i + 1).copied();
        if c == b'/' && (next == Some(b'/') || next == Some(b'*')) {
            let rest = js.get(i..).unwrap_or_default();
            let len = if next == Some(b'/') {
                rest.iter().position(|b| *b == b'\n' || *b == b'\r').unwrap_or(rest.len())
            } else {
                rest.windows(2).skip(2).position(|w| w == b"*/").map_or(rest.len(), |p| p + 4)
            };
            out.extend_from_slice(rest.get(..len).unwrap_or(rest));
            i += len.max(1);
            continue;
        }
        if c == b'(' {
            let end = trimmed(&out);
            let head = out.get(..end).unwrap_or_default();
            parens.push(parens.last().copied().unwrap_or(false) || NAME_LIST_CALLS.iter().any(|n| head.ends_with(n)));
            if head.ends_with(b"getField") {
                let after = js.get(i + 1..).unwrap_or_default();
                let first = after.iter().find(|b| !b.is_ascii_whitespace());
                if !matches!(first, Some(b'"' | b'\'')) {
                    dynamic += 1;
                }
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == b')' {
            parens.pop();
            out.push(c);
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' || c == b'`' {
            let mut j = i + 1;
            let mut escaped = false;
            let mut closed = false;
            while let Some(&b) = js.get(j) {
                if b == b'\\' {
                    escaped = true;
                    j += 2;
                    continue;
                }
                if b == c {
                    closed = true;
                    break;
                }
                if b == b'\n' && c != b'`' {
                    break;
                }
                j += 1;
            }
            if !closed {
                out.extend_from_slice(js.get(i..).unwrap_or_default());
                break;
            }
            let content = js.get(i + 1..j).unwrap_or_default();
            let end = trimmed(&out);
            let head = out.get(..end).unwrap_or_default();
            let after_get_field = head.ends_with(b"(")
                && head.get(..head.len().saturating_sub(1)).is_some_and(|h| h.get(..trimmed(h)).is_some_and(|h| h.ends_with(b"getField")));
            // `getField("a" + x)`: the name is computed. A leading literal that names a renamed
            // field (or its descendants, `"a."`) is still updated; the call is counted.
            if after_get_field && js.get(j + 1..).and_then(|r| r.iter().find(|b| !b.is_ascii_whitespace())) != Some(&b')') {
                dynamic += 1;
            }
            let in_list = parens.last().copied().unwrap_or(false) && !OPERATIONS.contains(&content);
            let replacement = (c != b'`' && !escaped && (after_get_field || in_list)).then(|| renamed(content, renames)).flatten();
            match replacement {
                Some(new) => {
                    out.push(c);
                    out.extend_from_slice(&new);
                    out.push(c);
                    changed += 1;
                }
                None => out.extend_from_slice(js.get(i..=j).unwrap_or_default()),
            }
            i = j + 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    (out, changed, dynamic)
}

/// `name` with a renamed top-level prefix replaced, if it has one.
fn renamed(name: &[u8], renames: &[(String, String)]) -> Option<Vec<u8>> {
    renames.iter().find_map(|(old, new)| {
        let old = old.as_bytes();
        if name == old {
            Some(new.as_bytes().to_vec())
        } else if name.len() > old.len() && name.starts_with(old) && name.get(old.len()) == Some(&b'.') {
            Some([new.as_bytes(), name.get(old.len()..).unwrap_or_default()].concat())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renames() -> Vec<(String, String)> {
        vec![("total".into(), "total#2".into()), ("person".into(), "person#2".into())]
    }

    #[test]
    fn scripts_update_literal_field_names_only() {
        let js = br#"var t = this.getField("total"); var n = getField( 'person.name' ); event.value = "total"; // getField("total")
AFSimple_Calculate("SUM", new Array ("total", "other")); var x = getField("tot" + "al"); resetForm(["person"]);"#;
        let (out, n, dynamic) = rewrite_script(js, &renames());
        let out = String::from_utf8(out).unwrap();
        assert_eq!(n, 4, "{out}");
        assert_eq!(dynamic, 1);
        assert!(out.contains(r#"this.getField("total#2")"#));
        assert!(out.contains("getField( 'person#2.name' )"));
        assert!(out.contains(r#"event.value = "total";"#), "a plain string is not a field reference");
        assert!(out.contains(r#"// getField("total")"#), "comments stay");
        assert!(out.contains(r#"AFSimple_Calculate("SUM", new Array ("total#2", "other"))"#));
        assert!(out.contains(r#"resetForm(["person#2"])"#));
        assert!(out.contains(r#"getField("tot" + "al")"#));
        // A computed name that starts with a renamed field is still updated (and counted).
        let (out, n, dynamic) = rewrite_script(br#"getField("person." + key)"#, &renames());
        assert_eq!((String::from_utf8(out).unwrap().as_str(), n, dynamic), (r#"getField("person#2." + key)"#, 1, 1));
    }

    #[test]
    fn scripts_survive_unterminated_and_odd_input() {
        for js in [&b"getField(\"total"[..], b"/*", b"(((", b")))\"", b"getField(", b"'\\", b"\"a\\\""] {
            let (out, _, _) = rewrite_script(js, &renames());
            assert_eq!(out, js);
        }
    }

    #[test]
    fn da_font_names_are_rewritten_as_tokens() {
        let fonts: HashMap<Vec<u8>, Vec<u8>> = [(b"Helv".to_vec(), b"Helv_2".to_vec())].into_iter().collect();
        assert_eq!(rewrite_da(b"/Helv 12 Tf 0 g", &fonts), b"/Helv_2 12 Tf 0 g");
        assert_eq!(rewrite_da(b"/HelvB 12 Tf", &fonts), b"/HelvB 12 Tf");
        assert_eq!(rewrite_da(b"0 g /Helv", &fonts), b"0 g /Helv_2");
        assert_eq!(rewrite_da(b"/", &fonts), b"/");
    }
}
