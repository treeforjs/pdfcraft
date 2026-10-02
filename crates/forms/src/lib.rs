//! printcraft-forms — interactive forms (AcroForm, ISO 32000-2 §12.7), execution plan M6.1–M6.2.
//!
//! - [`fields`]: the field tree flattened to terminal fields, each with its widgets (page,
//!   rectangle, on-state), inherited attributes (`/FT`, `/Ff`, `/V`, `/DV`, `/DA`, `/Q`,
//!   `/MaxLen`) and choice options.
//! - [`set_value`]: fill a field. Text and choice fields get new appearance streams
//!   ([`appearance`]); check boxes and radio buttons switch `/V` and each widget's `/AS` between
//!   the states their appearances already define.
//! - [`reset`]: Acrobat's Clear form (back to `/DV`).
//!
//! Not yet: JavaScript actions (format, keystroke, validate, calculate — M6.4/M6.5) and rich text
//! values (`/RV`, which is removed when a value is set so it can't contradict `/V`).

use printcraft_cos::{Dict, Document, ObjRef, Object, PdfString};

pub mod appearance;

#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum FormError {
    #[error("the document has no form fields")]
    NoForm,
    #[error("there is no field named {0:?}")]
    NoSuchField(String),
    #[error("{0:?} is read-only")]
    ReadOnly(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Cos(#[from] printcraft_cos::CosError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    CheckBox,
    Radio,
    PushButton,
    Combo,
    List,
    Signature,
}

/// Field flags (§12.7.4, Tables 226, 228, 229, 231).
pub mod flags {
    pub const READ_ONLY: u32 = 1;
    pub const REQUIRED: u32 = 1 << 1;
    pub const MULTILINE: u32 = 1 << 12;
    pub const PASSWORD: u32 = 1 << 13;
    pub const NO_TOGGLE_TO_OFF: u32 = 1 << 14;
    pub const RADIO: u32 = 1 << 15;
    pub const PUSH_BUTTON: u32 = 1 << 16;
    pub const COMBO: u32 = 1 << 17;
    pub const EDIT: u32 = 1 << 18;
    pub const MULTI_SELECT: u32 = 1 << 21;
    pub const DO_NOT_SCROLL: u32 = 1 << 23;
    pub const COMB: u32 = 1 << 24;
    pub const RADIOS_IN_UNISON: u32 = 1 << 25;
}

/// One widget (the field's appearance on a page).
#[derive(Clone, Debug, PartialEq)]
pub struct Widget {
    pub obj: ObjRef,
    /// 0-based page index, when the widget is on a page.
    pub page: Option<usize>,
    /// In user space, normalized.
    pub rect: [f64; 4],
    /// Check boxes and radio buttons: the name of the "on" appearance state.
    pub on_state: Option<String>,
    /// The current appearance state (`/AS`).
    pub state: Option<String>,
}

/// A terminal form field.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// Fully qualified name (`parent.child`).
    pub name: String,
    pub obj: ObjRef,
    pub kind: FieldKind,
    /// Text: the text. Check box / radio: the selected state (none when off). Choice: the
    /// selected export values.
    pub value: Vec<String>,
    pub default: Vec<String>,
    pub flags: u32,
    pub max_len: Option<usize>,
    /// Choice options: (export value, display text).
    pub options: Vec<(String, String)>,
    /// Default appearance string (`/DA`, inherited from the form if absent).
    pub da: String,
    /// Quadding: 0 left, 1 centred, 2 right.
    pub quadding: i64,
    pub tooltip: Option<String>,
    pub widgets: Vec<Widget>,
}

impl Field {
    pub fn has(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    pub fn read_only(&self) -> bool {
        self.has(flags::READ_ONLY)
    }

    /// The value as one string: text, the state name, or the selected display texts.
    pub fn display_value(&self) -> String {
        match self.kind {
            FieldKind::Combo | FieldKind::List => self
                .value
                .iter()
                .map(|v| self.options.iter().find(|(e, _)| e == v).map_or(v.clone(), |(_, d)| d.clone()))
                .collect::<Vec<_>>()
                .join(", "),
            _ => self.value.join(", "),
        }
    }
}

/// A value to put in a field.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Text(String),
    /// Check box: checked or not.
    Check(bool),
    /// Radio group: the on-state to select (`None`: none selected).
    Radio(Option<String>),
    /// Combo box or list box: export values (several only for multi-select lists).
    Choice(Vec<String>),
}

// ── reading ─────────────────────────────────────────────────────────────────────────────────

fn text_of(o: &Object) -> Option<String> {
    match o {
        Object::String(s) => Some(s.to_text()),
        Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
        _ => None,
    }
}

fn values_of(doc: &Document, o: Option<&Object>) -> Vec<String> {
    let Some(o) = o else { return Vec::new() };
    match &*doc.resolve(o) {
        Object::Array(a) => a.iter().filter_map(|x| text_of(&doc.resolve(x))).collect(),
        Object::Stream(s) => s.decoded().ok().map(|b| PdfString::literal(b).to_text()).into_iter().collect(),
        other => text_of(other).filter(|s| s != "Off" && !s.is_empty()).into_iter().collect(),
    }
}

fn nums(doc: &Document, o: Option<&Object>) -> Option<Vec<f64>> {
    let o = doc.resolve(o?);
    o.as_array()?.iter().map(|x| doc.resolve(x).as_f64()).collect()
}

/// Leaf page objects in order (a small walker; `organize` and `annot` have their own).
fn page_refs(doc: &Document) -> Vec<ObjRef> {
    let Some(root) = doc.root() else { return Vec::new() };
    let Some(pages) = doc.get(root).as_dict().and_then(|d| d.reference(b"Pages")) else { return Vec::new() };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![pages];
    while let Some(node) = stack.pop() {
        if !seen.insert(node) || seen.len() > 1_000_000 {
            continue;
        }
        let obj = doc.get(node);
        let Some(d) = obj.as_dict() else { continue };
        match d.get(b"Kids").map(|k| doc.resolve(k)) {
            Some(kids) if d.name(b"Type") != Some(b"Page") => {
                if let Some(a) = kids.as_array() {
                    stack.extend(a.iter().rev().filter_map(|k| k.as_ref()));
                }
            }
            _ => out.push(node),
        }
    }
    out
}

/// The AcroForm dictionary.
pub fn acroform(doc: &Document) -> Option<Dict> {
    let root = doc.root()?;
    let cat = doc.get(root);
    let af = cat.as_dict()?.get(b"AcroForm")?.clone();
    doc.resolve(&af).as_dict().cloned()
}

#[derive(Clone, Default)]
struct Inherited {
    name: String,
    ft: Option<Vec<u8>>,
    ff: Option<u32>,
    v: Option<Object>,
    dv: Option<Object>,
    da: Option<String>,
    q: Option<i64>,
    max_len: Option<usize>,
}

/// Every terminal field, in tree order.
pub fn fields(doc: &Document) -> Vec<Field> {
    let Some(af) = acroform(doc) else { return Vec::new() };
    let mut page_of = std::collections::HashMap::new();
    for (i, p) in page_refs(doc).iter().enumerate() {
        if let Some(a) = doc.get(*p).as_dict().and_then(|d| d.get(b"Annots").cloned()) {
            for e in doc.resolve(&a).as_array().into_iter().flatten() {
                if let Some(r) = e.as_ref() {
                    page_of.entry(r).or_insert(i);
                }
            }
        }
    }
    let base = Inherited {
        da: af.get(b"DA").and_then(|o| text_of(&doc.resolve(o))),
        q: af.get(b"Q").and_then(|o| doc.resolve(o).as_int()),
        ..Default::default()
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in af.get(b"Fields").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()).unwrap_or_default() {
        if let Some(r) = f.as_ref() {
            walk(doc, r, &base, &page_of, &mut seen, &mut out, 0);
        }
    }
    out
}

fn walk(
    doc: &Document,
    r: ObjRef,
    parent: &Inherited,
    page_of: &std::collections::HashMap<ObjRef, usize>,
    seen: &mut std::collections::HashSet<ObjRef>,
    out: &mut Vec<Field>,
    depth: usize,
) {
    if depth > 64 || !seen.insert(r) {
        return;
    }
    let obj = doc.get(r);
    let Some(d) = obj.as_dict() else { return };
    let mut inh = parent.clone();
    if let Some(t) = d.get(b"T").and_then(|o| text_of(&doc.resolve(o))) {
        inh.name = if inh.name.is_empty() { t } else { format!("{}.{t}", inh.name) };
    }
    if let Some(ft) = d.name(b"FT") {
        inh.ft = Some(ft.to_vec());
    }
    if let Some(ff) = d.get(b"Ff").and_then(|o| doc.resolve(o).as_int()) {
        inh.ff = Some(ff as u32);
    }
    for (k, slot) in [(&b"V"[..], &mut inh.v), (b"DV", &mut inh.dv)] {
        if let Some(v) = d.get(k) {
            *slot = Some(v.clone());
        }
    }
    if let Some(da) = d.get(b"DA").and_then(|o| text_of(&doc.resolve(o))) {
        inh.da = Some(da);
    }
    if let Some(q) = d.get(b"Q").and_then(|o| doc.resolve(o).as_int()) {
        inh.q = Some(q);
    }
    if let Some(m) = d.get(b"MaxLen").and_then(|o| doc.resolve(o).as_int()) {
        inh.max_len = usize::try_from(m).ok();
    }
    let kids: Vec<ObjRef> =
        d.get(b"Kids").map(|k| doc.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default().iter().filter_map(|k| k.as_ref()).collect();
    // Kids with /T are fields; kids without are this field's widgets.
    let field_kids: Vec<ObjRef> = kids.iter().copied().filter(|k| doc.get(*k).as_dict().is_some_and(|kd| kd.contains(b"T"))).collect();
    if !field_kids.is_empty() {
        for k in field_kids {
            walk(doc, k, &inh, page_of, seen, out, depth + 1);
        }
        return;
    }
    let widget_refs: Vec<ObjRef> = if d.contains(b"Rect") { vec![r] } else { kids };
    let ff = inh.ff.unwrap_or(0);
    let kind = match inh.ft.as_deref() {
        Some(b"Tx") => FieldKind::Text,
        Some(b"Btn") if ff & flags::PUSH_BUTTON != 0 => FieldKind::PushButton,
        Some(b"Btn") if ff & flags::RADIO != 0 => FieldKind::Radio,
        Some(b"Btn") => FieldKind::CheckBox,
        Some(b"Ch") if ff & flags::COMBO != 0 => FieldKind::Combo,
        Some(b"Ch") => FieldKind::List,
        Some(b"Sig") => FieldKind::Signature,
        _ => return,
    };
    let widgets = widget_refs
        .into_iter()
        .filter_map(|w| {
            let wo = doc.get(w);
            let wd = wo.as_dict()?;
            let rect = nums(doc, wd.get(b"Rect")).filter(|r| r.len() == 4).unwrap_or_else(|| vec![0.0; 4]);
            let on_state = wd
                .get(b"AP")
                .map(|ap| doc.resolve(ap))
                .and_then(|ap| ap.as_dict().and_then(|a| a.get(b"N").cloned()))
                .and_then(|n| doc.resolve(&n).as_dict().cloned())
                .and_then(|n| n.iter().map(|(k, _)| String::from_utf8_lossy(k).into_owned()).find(|k| k != "Off"));
            Some(Widget {
                obj: w,
                page: page_of.get(&w).copied(),
                rect: [rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3])],
                on_state: on_state.filter(|_| matches!(kind, FieldKind::CheckBox | FieldKind::Radio)),
                state: wd.name(b"AS").map(|s| String::from_utf8_lossy(s).into_owned()),
            })
        })
        .collect();
    let options = d
        .get(b"Opt")
        .map(|o| doc.resolve(o))
        .and_then(|o| o.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|o| match &*doc.resolve(o) {
            Object::Array(pair) if pair.len() >= 2 => Some((text_of(&doc.resolve(&pair[0]))?, text_of(&doc.resolve(&pair[1]))?)),
            other => text_of(other).map(|t| (t.clone(), t)),
        })
        .collect();
    out.push(Field {
        name: inh.name.clone(),
        obj: r,
        kind,
        value: values_of(doc, inh.v.as_ref()),
        default: values_of(doc, inh.dv.as_ref()),
        flags: ff,
        max_len: inh.max_len,
        options,
        da: inh.da.clone().unwrap_or_else(|| "/Helv 0 Tf 0 g".into()),
        quadding: inh.q.unwrap_or(0),
        tooltip: d.get(b"TU").and_then(|o| text_of(&doc.resolve(o))),
        widgets,
    });
}

// ── writing ─────────────────────────────────────────────────────────────────────────────────

/// Fill the field `name`.
pub fn set_value(doc: &mut Document, name: &str, value: &FieldValue) -> Result<(), FormError> {
    let all = fields(doc);
    if all.is_empty() {
        return Err(FormError::NoForm);
    }
    let f = all.iter().find(|f| f.name == name).ok_or_else(|| FormError::NoSuchField(name.into()))?;
    if f.read_only() {
        return Err(FormError::ReadOnly(name.into()));
    }
    write_value(doc, f, value)
}

fn invalid<T>(m: impl Into<String>) -> Result<T, FormError> {
    Err(FormError::Invalid(m.into()))
}

fn write_value(doc: &mut Document, f: &Field, value: &FieldValue) -> Result<(), FormError> {
    match (f.kind, value) {
        (FieldKind::Text, FieldValue::Text(t)) => {
            if let Some(max) = f.max_len
                && t.chars().count() > max
            {
                return invalid(format!("{:?} takes at most {max} characters", f.name));
            }
            doc.update_dict(f.obj, |d| {
                if t.is_empty() {
                    d.remove(b"V");
                } else {
                    d.set(b"V".to_vec(), PdfString::text(t));
                }
                d.remove(b"RV");
            })?;
            redraw(doc, f, std::slice::from_ref(t))
        }
        (FieldKind::CheckBox, v) => {
            let state = f.widgets.iter().find_map(|w| w.on_state.clone()).unwrap_or_else(|| "Yes".into());
            let on = match v {
                FieldValue::Check(on) => *on,
                // The on-state name or a yes/no word also work (agents, FDF-style data).
                FieldValue::Text(t) | FieldValue::Radio(Some(t)) => {
                    let t = t.trim();
                    if t == state || ["yes", "true", "on", "1", "x", "checked"].contains(&t.to_lowercase().as_str()) {
                        true
                    } else if t.is_empty() || ["no", "false", "off", "0", "unchecked"].contains(&t.to_lowercase().as_str()) {
                        false
                    } else {
                        return invalid(format!("{:?} is a check box: use true or false", f.name));
                    }
                }
                FieldValue::Radio(None) => false,
                FieldValue::Choice(_) => return invalid(format!("{:?} is a check box: use true or false", f.name)),
            };
            set_states(doc, f, on.then_some(state.as_str()))
        }
        (FieldKind::Radio, v) => {
            let choice = match v {
                FieldValue::Radio(c) => c.clone(),
                FieldValue::Text(t) if t.is_empty() => None,
                FieldValue::Text(t) => Some(t.clone()),
                FieldValue::Check(false) => None,
                _ => return invalid(format!("{:?} is a radio group: choose one of its options", f.name)),
            };
            if let Some(c) = &choice
                && !f.widgets.iter().any(|w| w.on_state.as_deref() == Some(c.as_str()))
            {
                let opts: Vec<&str> = f.widgets.iter().filter_map(|w| w.on_state.as_deref()).collect();
                return invalid(format!("{:?} has no option {c:?} (options: {})", f.name, opts.join(", ")));
            }
            if choice.is_none() && f.has(flags::NO_TOGGLE_TO_OFF) && !f.value.is_empty() {
                return invalid(format!("{:?} must keep one option selected", f.name));
            }
            set_states(doc, f, choice.as_deref())
        }
        (FieldKind::Combo | FieldKind::List, FieldValue::Choice(_) | FieldValue::Text(_)) => {
            let vals: Vec<String> = match value {
                FieldValue::Choice(v) => v.clone(),
                FieldValue::Text(t) if t.is_empty() => Vec::new(),
                FieldValue::Text(t) => vec![t.clone()],
                _ => unreachable!("matched above"),
            };
            if vals.len() > 1 && !(f.kind == FieldKind::List && f.has(flags::MULTI_SELECT)) {
                return invalid(format!("{:?} takes a single value", f.name));
            }
            // Accept export values or display texts; free text only in editable combo boxes.
            let mut exports = Vec::new();
            for v in &vals {
                match f.options.iter().find(|(e, d)| e == v || d == v) {
                    Some((e, _)) => exports.push(e.clone()),
                    None if f.kind == FieldKind::Combo && f.has(flags::EDIT) => exports.push(v.clone()),
                    None if f.options.is_empty() => exports.push(v.clone()),
                    None => {
                        let opts: Vec<&str> = f.options.iter().map(|(_, d)| d.as_str()).collect();
                        return invalid(format!("{:?} has no option {v:?} (options: {})", f.name, opts.join(", ")));
                    }
                }
            }
            let indices: Vec<Object> =
                exports.iter().filter_map(|e| f.options.iter().position(|(x, _)| x == e)).map(|i| Object::Int(i as i64)).collect();
            doc.update_dict(f.obj, |d| {
                match exports.as_slice() {
                    [] => {
                        d.remove(b"V");
                    }
                    [one] => d.set(b"V".to_vec(), PdfString::text(one)),
                    many => d.set(b"V".to_vec(), Object::Array(many.iter().map(|e| Object::String(PdfString::text(e))).collect())),
                }
                if indices.is_empty() {
                    d.remove(b"I");
                } else {
                    d.set(b"I".to_vec(), Object::Array(indices));
                }
            })?;
            redraw(doc, f, &exports)
        }
        (FieldKind::PushButton, _) => invalid(format!("{:?} is a button; it has no value", f.name)),
        (FieldKind::Signature, _) => invalid(format!("{:?} is a signature field; sign it with Fill & Sign or a digital ID", f.name)),
        (kind, v) => invalid(format!("{:?} is a {kind:?} field and can't take {v:?}", f.name)),
    }
}

/// Check boxes and radio buttons: `/V` on the field, `/AS` on each widget.
fn set_states(doc: &mut Document, f: &Field, on: Option<&str>) -> Result<(), FormError> {
    let v = on.unwrap_or("Off");
    doc.update_dict(f.obj, |d| d.set(b"V".to_vec(), Object::name(v)))?;
    for w in &f.widgets {
        let state = match (on, w.on_state.as_deref()) {
            (Some(c), Some(s)) if c == s => s,
            _ => "Off",
        };
        // A widget without appearances for its states gets PrintCraft's own.
        let has_ap = doc.get(w.obj).as_dict().and_then(|d| d.get(b"AP").cloned()).is_some();
        if !has_ap {
            let on_name = w.on_state.clone().unwrap_or_else(|| on.unwrap_or("Yes").to_string());
            let ap = appearance::check_box_states(doc, w, f.kind, &on_name);
            doc.update_dict(w.obj, |d| d.set(b"AP".to_vec(), Object::Dict(ap)))?;
        }
        doc.update_dict(w.obj, |d| d.set(b"AS".to_vec(), Object::name(state)))?;
    }
    Ok(())
}

/// Regenerate the normal appearance of every widget of a text or choice field.
fn redraw(doc: &mut Document, f: &Field, values: &[String]) -> Result<(), FormError> {
    for w in &f.widgets {
        let stream = appearance::field_appearance(doc, f, w, values);
        let ap = doc.add(Object::Stream(stream));
        let mut apd = Dict::new();
        apd.set(b"N".to_vec(), Object::Ref(ap));
        doc.update_dict(w.obj, |d| {
            d.set(b"AP".to_vec(), Object::Dict(apd));
            d.remove(b"AS");
        })?;
    }
    Ok(())
}

/// Acrobat's Clear form: every field (or the named ones) back to its default value.
pub fn reset(doc: &mut Document, names: Option<&[String]>) -> Result<usize, FormError> {
    let all = fields(doc);
    if all.is_empty() {
        return Err(FormError::NoForm);
    }
    if let Some(n) = names
        && let Some(missing) = n.iter().find(|n| !all.iter().any(|f| &f.name == *n))
    {
        return Err(FormError::NoSuchField(missing.clone()));
    }
    let mut changed = 0;
    for f in all.iter().filter(|f| names.is_none_or(|n| n.contains(&f.name))) {
        if f.value == f.default {
            continue;
        }
        let v = match f.kind {
            FieldKind::Text => FieldValue::Text(f.default.first().cloned().unwrap_or_default()),
            FieldKind::CheckBox => FieldValue::Check(!f.default.is_empty()),
            FieldKind::Radio => FieldValue::Radio(f.default.first().cloned()),
            FieldKind::Combo | FieldKind::List => FieldValue::Choice(f.default.clone()),
            FieldKind::PushButton | FieldKind::Signature => continue,
        };
        // A reset is not blocked by NoToggleToOff: go through the writer directly.
        let mut tmp = f.clone();
        tmp.flags &= !flags::NO_TOGGLE_TO_OFF;
        write_value(doc, &tmp, &v)?;
        changed += 1;
    }
    Ok(changed)
}
