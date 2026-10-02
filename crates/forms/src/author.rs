//! Prepare a form: create, change and delete fields (execution plan M6.6).
//!
//! New fields follow Acrobat's conventions: names "Text1", "Check Box1", "Group1" (radio groups),
//! "Dropdown1", "List Box1", "Button1", "Signature1", "Date1"; 12 pt Helvetica (auto-size for
//! check boxes), a thin grey border and a white background, and appearance streams generated
//! immediately so every viewer shows the empty field.

use printcraft_cos::{Dict, Document, ObjRef, Object, PdfString, Stream};
use printcraft_fonts::{helvetica_width, literal, win_ansi};

use crate::{Field, FieldKind, FormError, Widget, appearance, fields, flags, page_refs};

/// The kind of field to add.
#[derive(Clone, Debug, PartialEq)]
pub enum NewField {
    Text {
        multiline: bool,
    },
    /// A date field: a text field with Acrobat's date format action (`AFDate_FormatEx`).
    Date,
    CheckBox,
    /// One radio button in `group` (a new group is created when none of that name exists),
    /// selected by `export`.
    Radio {
        group: Option<String>,
        export: String,
    },
    Combo {
        options: Vec<String>,
        editable: bool,
    },
    List {
        options: Vec<String>,
        multi: bool,
    },
    Button {
        caption: String,
    },
    Signature,
}

impl NewField {
    fn base_name(&self) -> &'static str {
        match self {
            NewField::Text { .. } => "Text",
            NewField::Date => "Date",
            NewField::CheckBox => "Check Box",
            NewField::Radio { .. } => "Group",
            NewField::Combo { .. } => "Dropdown",
            NewField::List { .. } => "List Box",
            NewField::Button { .. } => "Button",
            NewField::Signature => "Signature",
        }
    }
}

/// Field properties the Properties dialog edits (`None` leaves a value unchanged).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FieldProps {
    pub name: Option<String>,
    pub tooltip: Option<String>,
    pub read_only: Option<bool>,
    pub required: Option<bool>,
    pub multiline: Option<bool>,
    pub max_len: Option<Option<usize>>,
    /// Choice options (display texts; export values equal them).
    pub options: Option<Vec<String>>,
    /// Font size (0 = auto).
    pub font_size: Option<f64>,
    /// Position: move or resize one widget (its index in [`Field::widgets`]) to a new rect in
    /// user space.
    pub rect: Option<(usize, [f64; 4])>,
}

fn invalid<T>(m: impl Into<String>) -> Result<T, FormError> {
    Err(FormError::Invalid(m.into()))
}

fn rgb(c: [f64; 3]) -> Object {
    Object::Array(c.iter().map(|v| Object::Real(*v)).collect())
}

/// The AcroForm dictionary's reference, creating the form (with `/Helv` in `/DR`) if needed.
fn ensure_form(doc: &mut Document) -> Result<ObjRef, FormError> {
    let root = doc.root().ok_or(FormError::NoForm)?;
    let existing = doc.get(root).as_dict().and_then(|d| d.get(b"AcroForm").cloned());
    let r = match existing {
        Some(Object::Ref(r)) if doc.get(r).as_dict().is_some() => r,
        other => {
            let d = other.and_then(|o| o.as_dict().cloned()).unwrap_or_default();
            let r = doc.add(Object::Dict(d));
            doc.update_dict(root, |c| c.set(b"AcroForm".to_vec(), Object::Ref(r)))?;
            r
        }
    };
    let mut af = doc.get(r).as_dict().cloned().unwrap_or_default();
    if !af.contains(b"Fields") {
        af.set(b"Fields".to_vec(), Object::Array(Vec::new()));
    }
    if !af.contains(b"DA") {
        af.set(b"DA".to_vec(), PdfString::literal(b"/Helv 0 Tf 0 g".to_vec()));
    }
    let mut dr = af.get(b"DR").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned()).unwrap_or_default();
    let mut fonts = dr.get(b"Font").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned()).unwrap_or_default();
    if !fonts.contains(b"Helv") {
        let mut f = Dict::new();
        f.set(b"Type".to_vec(), Object::name("Font"));
        f.set(b"Subtype".to_vec(), Object::name("Type1"));
        f.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
        f.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
        fonts.set(b"Helv".to_vec(), Object::Dict(f));
        dr.set(b"Font".to_vec(), Object::Dict(fonts));
        af.set(b"DR".to_vec(), Object::Dict(dr));
    }
    doc.set(r, Object::Dict(af));
    Ok(r)
}

fn add_to_page(doc: &mut Document, page: ObjRef, widget: ObjRef) -> Result<(), FormError> {
    let existing = doc.get(page).as_dict().and_then(|d| d.get(b"Annots").cloned());
    match existing.as_ref().and_then(|o| o.as_ref()).filter(|r| doc.get(*r).as_array().is_some()) {
        Some(r) => {
            let mut a = doc.get(r).as_array().cloned().unwrap_or_default();
            a.push(Object::Ref(widget));
            doc.set(r, Object::Array(a));
        }
        None => {
            let mut a = existing.and_then(|o| o.as_array().cloned()).unwrap_or_default();
            a.push(Object::Ref(widget));
            doc.update_dict(page, |d| d.set(b"Annots".to_vec(), Object::Array(a)))?;
        }
    }
    Ok(())
}

fn add_to_fields(doc: &mut Document, af: ObjRef, field: ObjRef) -> Result<(), FormError> {
    let mut d = doc.get(af).as_dict().cloned().unwrap_or_default();
    let mut list = d.get(b"Fields").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()).unwrap_or_default();
    list.push(Object::Ref(field));
    d.set(b"Fields".to_vec(), Object::Array(list));
    doc.set(af, Object::Dict(d));
    Ok(())
}

/// The first free "<base>N" name.
fn free_name(existing: &[Field], base: &str) -> String {
    (1..).map(|n| format!("{base}{n}")).find(|c| !existing.iter().any(|f| f.name == *c || f.name.starts_with(&format!("{c}.")))).expect("unbounded")
}

fn widget_dict(page: ObjRef, rect: [f64; 4]) -> Dict {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("Annot"));
    d.set(b"Subtype".to_vec(), Object::name("Widget"));
    d.set(b"Rect".to_vec(), Object::Array(rect.iter().map(|v| Object::Real(*v)).collect()));
    d.set(b"P".to_vec(), Object::Ref(page));
    d.set(b"F".to_vec(), Object::Int(4));
    let mut mk = Dict::new();
    mk.set(b"BC".to_vec(), rgb([0.55, 0.55, 0.55]));
    mk.set(b"BG".to_vec(), rgb([1.0, 1.0, 1.0]));
    d.set(b"MK".to_vec(), Object::Dict(mk));
    let mut bs = Dict::new();
    bs.set(b"W".to_vec(), Object::Int(1));
    bs.set(b"S".to_vec(), Object::name("S"));
    d.set(b"BS".to_vec(), Object::Dict(bs));
    d
}

/// Add a field on `page` (0-based) in `rect` (user space). Returns its full name.
pub fn add_field(doc: &mut Document, page: usize, rect: [f64; 4], kind: &NewField, name: Option<&str>) -> Result<String, FormError> {
    let pages = page_refs(doc);
    let page_ref = *pages.get(page).ok_or_else(|| FormError::Invalid(format!("page {} does not exist", page + 1)))?;
    let rect = [rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3])];
    if !rect.iter().all(|v| v.is_finite()) || rect[2] - rect[0] < 4.0 || rect[3] - rect[1] < 4.0 {
        return invalid("the field is too small");
    }
    let existing = fields(doc);
    // A radio button names its group: a new group takes that name.
    let name = match (name, kind) {
        (None, NewField::Radio { group: Some(g), .. }) => Some(g.as_str()),
        _ => name,
    };
    let name = match name.map(str::trim) {
        Some(n) if !n.is_empty() => {
            if n.contains('.') {
                return invalid("field names can't contain a period");
            }
            n.to_string()
        }
        _ => free_name(&existing, kind.base_name()),
    };
    let radio_group = match kind {
        NewField::Radio { group, .. } => existing.iter().find(|f| f.kind == FieldKind::Radio && Some(&f.name) == group.as_ref()).cloned(),
        _ => None,
    };
    if radio_group.is_none() && existing.iter().any(|f| f.name == name) {
        return invalid(format!("a field named {name:?} already exists"));
    }
    let af = ensure_form(doc)?;
    let mut w = widget_dict(page_ref, rect);
    let field_name = match kind {
        NewField::Radio { group: Some(g), .. } if radio_group.is_some() => g.clone(),
        _ => name.clone(),
    };
    match kind {
        NewField::Text { multiline } => {
            w.set(b"FT".to_vec(), Object::name("Tx"));
            w.set(b"DA".to_vec(), PdfString::literal(b"/Helv 12 Tf 0 g".to_vec()));
            if *multiline {
                w.set(b"Ff".to_vec(), Object::Int(flags::MULTILINE as i64));
            }
        }
        NewField::Date => {
            w.set(b"FT".to_vec(), Object::name("Tx"));
            w.set(b"DA".to_vec(), PdfString::literal(b"/Helv 12 Tf 0 g".to_vec()));
            // Acrobat's format and keystroke actions for mm/dd/yyyy (run by its JavaScript engine).
            let js = |s: &str| {
                let mut a = Dict::new();
                a.set(b"S".to_vec(), Object::name("JavaScript"));
                a.set(b"JS".to_vec(), PdfString::literal(s.as_bytes().to_vec()));
                Object::Dict(a)
            };
            let mut aa = Dict::new();
            aa.set(b"F".to_vec(), js("AFDate_FormatEx(\"mm/dd/yyyy\");"));
            aa.set(b"K".to_vec(), js("AFDate_KeystrokeEx(\"mm/dd/yyyy\");"));
            w.set(b"AA".to_vec(), Object::Dict(aa));
        }
        NewField::CheckBox => {
            w.set(b"FT".to_vec(), Object::name("Btn"));
            w.set(b"V".to_vec(), Object::name("Off"));
            w.set(b"AS".to_vec(), Object::name("Off"));
            w.set(b"DA".to_vec(), PdfString::literal(b"/ZaDb 0 Tf 0 g".to_vec()));
        }
        NewField::Radio { export, .. } => {
            w.set(b"AS".to_vec(), Object::name("Off"));
            if export.trim().is_empty() || export == "Off" {
                return invalid("a radio button needs an export value other than Off");
            }
        }
        NewField::Combo { options, editable } | NewField::List { options, multi: editable } => {
            w.set(b"FT".to_vec(), Object::name("Ch"));
            w.set(b"DA".to_vec(), PdfString::literal(b"/Helv 12 Tf 0 g".to_vec()));
            let combo = matches!(kind, NewField::Combo { .. });
            let mut ff = 0;
            if combo {
                ff |= flags::COMBO;
                if *editable {
                    ff |= flags::EDIT;
                }
            } else if *editable {
                ff |= flags::MULTI_SELECT;
            }
            w.set(b"Ff".to_vec(), Object::Int(ff as i64));
            w.set(b"Opt".to_vec(), Object::Array(options.iter().map(|o| Object::String(PdfString::text(o))).collect()));
        }
        NewField::Button { caption } => {
            w.set(b"FT".to_vec(), Object::name("Btn"));
            w.set(b"Ff".to_vec(), Object::Int(flags::PUSH_BUTTON as i64));
            w.set(b"DA".to_vec(), PdfString::literal(b"/Helv 0 Tf 0 g".to_vec()));
            let mut mk = w.get(b"MK").and_then(|m| m.as_dict()).cloned().unwrap_or_default();
            mk.set(b"BG".to_vec(), rgb([0.86, 0.86, 0.86]));
            mk.set(b"CA".to_vec(), PdfString::text(caption));
            w.set(b"MK".to_vec(), Object::Dict(mk));
        }
        NewField::Signature => {
            w.set(b"FT".to_vec(), Object::name("Sig"));
        }
    }
    let widget = doc.add(Object::Dict(w));
    match kind {
        NewField::Radio { export, .. } => {
            let group = match &radio_group {
                Some(g) => g.obj,
                None => {
                    let mut g = Dict::new();
                    g.set(b"FT".to_vec(), Object::name("Btn"));
                    g.set(b"Ff".to_vec(), Object::Int((flags::RADIO | flags::NO_TOGGLE_TO_OFF) as i64));
                    g.set(b"T".to_vec(), PdfString::text(&field_name));
                    g.set(b"V".to_vec(), Object::name("Off"));
                    g.set(b"DA".to_vec(), PdfString::literal(b"/ZaDb 0 Tf 0 g".to_vec()));
                    g.set(b"Kids".to_vec(), Object::Array(Vec::new()));
                    let g = doc.add(Object::Dict(g));
                    add_to_fields(doc, af, g)?;
                    g
                }
            };
            doc.update_dict(widget, |d| d.set(b"Parent".to_vec(), Object::Ref(group)))?;
            doc.update_dict(group, |d| {
                let mut kids = d.get(b"Kids").and_then(|k| k.as_array().cloned()).unwrap_or_default();
                kids.push(Object::Ref(widget));
                d.set(b"Kids".to_vec(), Object::Array(kids));
            })?;
            let w = Widget { obj: widget, page: Some(page), rect, on_state: Some(export.clone()), state: Some("Off".into()) };
            let ap = appearance::check_box_states(doc, &w, FieldKind::Radio, export);
            doc.update_dict(widget, |d| d.set(b"AP".to_vec(), Object::Dict(ap)))?;
        }
        _ => {
            doc.update_dict(widget, |d| d.set(b"T".to_vec(), PdfString::text(&name)))?;
            add_to_fields(doc, af, widget)?;
        }
    }
    add_to_page(doc, page_ref, widget)?;
    redraw_field(doc, &field_name)?;
    Ok(field_name)
}

/// Regenerate every widget appearance of a field from its current value and settings.
pub fn redraw_field(doc: &mut Document, name: &str) -> Result<(), FormError> {
    let f = fields(doc).into_iter().find(|f| f.name == name).ok_or_else(|| FormError::NoSuchField(name.into()))?;
    for w in &f.widgets {
        let ap = match f.kind {
            FieldKind::Text | FieldKind::Combo | FieldKind::List => {
                let s = appearance::field_appearance(doc, &f, w, &f.value);
                let r = doc.add(Object::Stream(s));
                let mut d = Dict::new();
                d.set(b"N".to_vec(), Object::Ref(r));
                d
            }
            FieldKind::CheckBox | FieldKind::Radio => {
                let on = w.on_state.clone().unwrap_or_else(|| "Yes".into());
                appearance::check_box_states(doc, w, f.kind, &on)
            }
            FieldKind::PushButton => {
                let s = button_appearance(doc, w);
                let r = doc.add(Object::Stream(s));
                let mut d = Dict::new();
                d.set(b"N".to_vec(), Object::Ref(r));
                d
            }
            FieldKind::Signature => {
                let s = empty_box(doc, w);
                let r = doc.add(Object::Stream(s));
                let mut d = Dict::new();
                d.set(b"N".to_vec(), Object::Ref(r));
                d
            }
        };
        doc.update_dict(w.obj, |d| d.set(b"AP".to_vec(), Object::Dict(ap)))?;
    }
    Ok(())
}

fn frame_only(doc: &Document, w: &Widget) -> (String, f64, f64) {
    let (width, height) = ((w.rect[2] - w.rect[0]).max(1.0), (w.rect[3] - w.rect[1]).max(1.0));
    let wobj = doc.get(w.obj);
    let wd = wobj.as_dict().cloned().unwrap_or_default();
    let mk = wd.get(b"MK").and_then(|m| m.as_dict()).cloned().unwrap_or_default();
    let col = |k: &[u8]| -> Option<String> {
        let v: Vec<f64> = mk.get(k)?.as_array()?.iter().filter_map(|x| x.as_f64()).collect();
        (v.len() == 3).then(|| format!("{} {} {}", crate::appearance::fmt(v[0]), crate::appearance::fmt(v[1]), crate::appearance::fmt(v[2])))
    };
    let mut c = String::new();
    if let Some(bg) = col(b"BG") {
        c.push_str(&format!("{bg} rg 0 0 {width:.3} {height:.3} re f\n"));
    }
    if let Some(bc) = col(b"BC") {
        c.push_str(&format!("{bc} RG 1 w 0.5 0.5 {:.3} {:.3} re S\n", width - 1.0, height - 1.0));
    }
    (c, width, height)
}

fn form_stream(width: f64, height: f64, content: Vec<u8>, resources: Dict) -> Stream {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Form"));
    d.set(b"BBox".to_vec(), Object::Array([0.0, 0.0, width, height].iter().map(|v| Object::Real(*v)).collect()));
    d.set(b"Resources".to_vec(), Object::Dict(resources));
    Stream::flate(d, &content)
}

/// A push button: background, border and its centred caption.
fn button_appearance(doc: &Document, w: &Widget) -> Stream {
    let (mut c, width, height) = frame_only(doc, w);
    let wobj = doc.get(w.obj);
    let caption = wobj
        .as_dict()
        .and_then(|d| d.get(b"MK"))
        .and_then(|m| m.as_dict())
        .and_then(|m| m.get(b"CA"))
        .and_then(|c| c.as_string())
        .map(|s| s.to_text())
        .unwrap_or_default();
    let size = ((height - 4.0) * 0.6).clamp(4.0, 14.0);
    let tw = helvetica_width(&caption, size);
    let mut content = std::mem::take(&mut c).into_bytes();
    content.extend(format!("BT /Helv {size:.2} Tf 0 g 1 0 0 1 {:.3} {:.3} Tm ", (width - tw) / 2.0, (height - size * 0.7) / 2.0).bytes());
    content.extend(literal(&win_ansi(&caption)));
    content.extend_from_slice(b" Tj ET\n");
    let mut font = Dict::new();
    font.set(b"Type".to_vec(), Object::name("Font"));
    font.set(b"Subtype".to_vec(), Object::name("Type1"));
    font.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
    font.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
    let mut fonts = Dict::new();
    fonts.set(b"Helv".to_vec(), Object::Dict(font));
    let mut res = Dict::new();
    res.set(b"Font".to_vec(), Object::Dict(fonts));
    form_stream(width, height, content, res)
}

fn empty_box(doc: &Document, w: &Widget) -> Stream {
    let (c, width, height) = frame_only(doc, w);
    form_stream(width, height, c.into_bytes(), Dict::new())
}

/// Change a field's properties (General and Options tabs) and redraw it.
pub fn set_props(doc: &mut Document, name: &str, props: &FieldProps) -> Result<String, FormError> {
    let all = fields(doc);
    let f = all.iter().find(|f| f.name == name).ok_or_else(|| FormError::NoSuchField(name.into()))?.clone();
    let mut new_name = name.to_string();
    if let Some(n) = props.name.as_deref().map(str::trim).filter(|n| *n != f.name) {
        if n.is_empty() || n.contains('.') {
            return invalid("field names can't be empty or contain a period");
        }
        if all.iter().any(|x| x.name == n) {
            return invalid(format!("a field named {n:?} already exists"));
        }
        // Rename the terminal part (the field's own /T).
        let prefix = f.name.rsplit_once('.').map(|(p, _)| format!("{p}.")).unwrap_or_default();
        new_name = format!("{prefix}{n}");
        doc.update_dict(f.obj, |d| d.set(b"T".to_vec(), PdfString::text(n)))?;
    }
    let mut ff = f.flags;
    let mut set_flag = |flag: u32, on: Option<bool>| {
        if let Some(on) = on {
            if on {
                ff |= flag;
            } else {
                ff &= !flag;
            }
        }
    };
    set_flag(flags::READ_ONLY, props.read_only);
    set_flag(flags::REQUIRED, props.required);
    if f.kind == FieldKind::Text {
        set_flag(flags::MULTILINE, props.multiline);
    }
    if props.options.as_ref().is_some_and(|o| o.is_empty()) && matches!(f.kind, FieldKind::Combo | FieldKind::List) {
        return invalid("a list needs at least one option");
    }
    doc.update_dict(f.obj, |d| {
        d.set(b"Ff".to_vec(), Object::Int(ff as i64));
        if let Some(t) = &props.tooltip {
            if t.is_empty() {
                d.remove(b"TU");
            } else {
                d.set(b"TU".to_vec(), PdfString::text(t));
            }
        }
        if let Some(m) = props.max_len {
            match m {
                Some(m) => d.set(b"MaxLen".to_vec(), Object::Int(m as i64)),
                None => {
                    d.remove(b"MaxLen");
                }
            }
        }
        if let Some(opts) = &props.options {
            d.set(b"Opt".to_vec(), Object::Array(opts.iter().map(|o| Object::String(PdfString::text(o))).collect()));
        }
        if let Some(size) = props.font_size {
            d.set(b"DA".to_vec(), PdfString::literal(format!("/Helv {} Tf 0 g", crate::appearance::fmt(size.clamp(0.0, 100.0))).into_bytes()));
        }
    })?;
    if let Some((wi, r)) = props.rect {
        let w = f.widgets.get(wi).ok_or_else(|| FormError::Invalid(format!("{name} has no widget {}", wi + 1)))?;
        let r = [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])];
        if !r.iter().all(|v| v.is_finite()) || r[2] - r[0] < 4.0 || r[3] - r[1] < 4.0 {
            return invalid("the field is too small");
        }
        doc.update_dict(w.obj, |d| d.set(b"Rect".to_vec(), Object::Array(r.iter().map(|v| Object::Real(*v)).collect())))?;
    }
    // Widgets may carry their own /DA; keep them in step with the field.
    if let Some(size) = props.font_size {
        for w in &f.widgets {
            if w.obj != f.obj {
                doc.update_dict(w.obj, |d| {
                    if d.contains(b"DA") {
                        d.set(
                            b"DA".to_vec(),
                            PdfString::literal(format!("/Helv {} Tf 0 g", crate::appearance::fmt(size.clamp(0.0, 100.0))).into_bytes()),
                        );
                    }
                })?;
            }
        }
    }
    redraw_field(doc, &new_name)?;
    Ok(new_name)
}

/// Delete a field: its widgets leave their pages and the field leaves the form.
pub fn delete_field(doc: &mut Document, name: &str) -> Result<(), FormError> {
    let all = fields(doc);
    let f = all.iter().find(|f| f.name == name).ok_or_else(|| FormError::NoSuchField(name.into()))?.clone();
    let widgets: std::collections::HashSet<ObjRef> = f.widgets.iter().map(|w| w.obj).collect();
    for p in page_refs(doc) {
        let Some(a) = doc.get(p).as_dict().and_then(|d| d.get(b"Annots").cloned()) else { continue };
        let list = doc.resolve(&a).as_array().cloned().unwrap_or_default();
        let kept: Vec<Object> = list.iter().filter(|o| !o.as_ref().is_some_and(|r| widgets.contains(&r))).cloned().collect();
        if kept.len() != list.len() {
            match a.as_ref() {
                Some(r) if doc.get(r).as_array().is_some() => doc.set(r, Object::Array(kept)),
                _ => doc.update_dict(p, |d| d.set(b"Annots".to_vec(), Object::Array(kept)))?,
            }
        }
    }
    // Out of the field tree: from its parent's /Kids, or from /Fields.
    let parent = doc.get(f.obj).as_dict().and_then(|d| d.reference(b"Parent"));
    let remove_from = |doc: &mut Document, holder: ObjRef, key: &[u8]| -> Result<(), FormError> {
        let mut d = doc.get(holder).as_dict().cloned().unwrap_or_default();
        let list = d.get(key).map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()).unwrap_or_default();
        let kept: Vec<Object> = list.into_iter().filter(|o| o.as_ref() != Some(f.obj)).collect();
        d.set(key.to_vec(), Object::Array(kept));
        doc.set(holder, Object::Dict(d));
        Ok(())
    };
    match parent {
        Some(p) => remove_from(doc, p, b"Kids")?,
        None => {
            let root = doc.root().ok_or(FormError::NoForm)?;
            if let Some(af) = doc.get(root).as_dict().and_then(|d| d.reference(b"AcroForm")) {
                remove_from(doc, af, b"Fields")?;
            }
        }
    }
    Ok(())
}
