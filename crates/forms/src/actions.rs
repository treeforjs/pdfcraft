//! Field Properties ▸ Actions: what a field does on each trigger (§12.6.3, Table 197: the
//! widget's `/A` for Mouse Up and its `/AA` D, E, X, Fo, Bl for the others).
//!
//! A trigger can run several actions in turn: the first is the trigger's entry and the rest
//! follow through `/Next` (§12.6.2: a dictionary or an array, run depth first). They are listed
//! here in the order they run; writing them back links them as a single `/Next` chain. Actions
//! that are kept unchanged keep their original dictionary (unknown types, extra keys and
//! destinations PdfCraft doesn't edit included); a trigger whose list didn't change is left as it
//! is in the file.

use std::collections::HashSet;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString};

use crate::{FormError, fields};

/// The Actions tab's triggers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Trigger {
    MouseUp,
    MouseDown,
    MouseEnter,
    MouseExit,
    OnFocus,
    OnBlur,
}

impl Trigger {
    pub const ALL: [Trigger; 6] = [Trigger::MouseUp, Trigger::MouseDown, Trigger::MouseEnter, Trigger::MouseExit, Trigger::OnFocus, Trigger::OnBlur];

    pub fn label(self) -> &'static str {
        match self {
            Trigger::MouseUp => "Mouse Up",
            Trigger::MouseDown => "Mouse Down",
            Trigger::MouseEnter => "Mouse Enter",
            Trigger::MouseExit => "Mouse Exit",
            Trigger::OnFocus => "On Focus",
            Trigger::OnBlur => "On Blur",
        }
    }

    /// Snake-case id (agents).
    pub fn id(self) -> &'static str {
        match self {
            Trigger::MouseUp => "mouse_up",
            Trigger::MouseDown => "mouse_down",
            Trigger::MouseEnter => "mouse_enter",
            Trigger::MouseExit => "mouse_exit",
            Trigger::OnFocus => "on_focus",
            Trigger::OnBlur => "on_blur",
        }
    }

    pub fn from_id(id: &str) -> Option<Trigger> {
        Trigger::ALL.into_iter().find(|t| t.id() == id)
    }

    /// `event.name` for a script this trigger runs (PDF JavaScript's field events).
    pub fn event_name(self) -> &'static str {
        match self {
            Trigger::MouseUp => "Mouse Up",
            Trigger::MouseDown => "Mouse Down",
            Trigger::MouseEnter => "Mouse Enter",
            Trigger::MouseExit => "Mouse Exit",
            Trigger::OnFocus => "Focus",
            Trigger::OnBlur => "Blur",
        }
    }

    /// The `/AA` key (`None`: Mouse Up lives in `/A`).
    fn key(self) -> Option<&'static [u8]> {
        match self {
            Trigger::MouseUp => None,
            Trigger::MouseDown => Some(b"D"),
            Trigger::MouseEnter => Some(b"E"),
            Trigger::MouseExit => Some(b"X"),
            Trigger::OnFocus => Some(b"Fo"),
            Trigger::OnBlur => Some(b"Bl"),
        }
    }
}

/// The menu items an Execute a menu item action can be given: the ones PdfCraft runs, none of
/// which changes or sends the document. Other named actions already in a file are kept, but
/// can't be added.
pub const MENU_ITEMS: [&str; 5] = ["Print", "NextPage", "PrevPage", "FirstPage", "LastPage"];

/// At most this many actions run on one trigger (longer chains are cut here when read and
/// refused when written).
pub const MAX_ACTIONS_PER_TRIGGER: usize = 64;

/// The Actions tab's "Select Action" choices.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldAction {
    /// Run a JavaScript.
    JavaScript(String),
    /// Open a web link.
    Uri(String),
    /// Reset a form: the listed fields, or every field when empty.
    Reset(Vec<String>),
    /// Execute a menu item (a named action; new ones only from [`MENU_ITEMS`]).
    Named(String),
    /// Go to a page view (0-based page).
    GoTo(usize),
    /// Show/hide a field.
    ShowHide { fields: Vec<String>, hide: bool },
    /// Submit a form.
    Submit(String),
    /// An action PdfCraft doesn't edit (by its `/S` type), kept as it is. Passing one back to
    /// [`set_field_actions`] keeps the existing action of that type; it can't create one.
    Other(String),
}

impl FieldAction {
    /// How the Actions tab lists it.
    pub fn describe(&self) -> String {
        match self {
            FieldAction::JavaScript(_) => "Run a JavaScript".into(),
            FieldAction::Uri(u) => format!("Open a web link: {u}"),
            FieldAction::Reset(f) if f.is_empty() => "Reset a form".into(),
            FieldAction::Reset(f) => format!("Reset a form: {}", f.join(", ")),
            FieldAction::Named(n) => format!("Execute a menu item: {n}"),
            FieldAction::GoTo(p) => format!("Go to a page view: page {}", p.saturating_add(1)),
            FieldAction::ShowHide { fields, hide } => format!("{} a field: {}", if *hide { "Hide" } else { "Show" }, fields.join(", ")),
            FieldAction::Submit(u) => format!("Submit a form: {u}"),
            FieldAction::Other(s) => s.clone(),
        }
    }
}

fn texts(doc: &Document, o: &Object) -> Vec<String> {
    match &*doc.resolve(o) {
        Object::Array(a) => a.iter().filter_map(|x| doc.resolve(x).as_string().map(|s| s.to_text())).collect(),
        Object::String(s) => vec![s.to_text()],
        _ => Vec::new(),
    }
}

/// What an action dictionary does, as the tab shows it (`None`: not an action dictionary).
fn read(doc: &Document, d: &Dict, pages: &[ObjRef]) -> Option<FieldAction> {
    let kind = d.name(b"S")?;
    let other = || FieldAction::Other(String::from_utf8_lossy(kind).into_owned());
    Some(match kind {
        b"JavaScript" => FieldAction::JavaScript(crate::script(doc, &Object::Dict(d.clone())).unwrap_or_default()),
        b"URI" => FieldAction::Uri(d.get(b"URI").and_then(|u| doc.resolve(u).as_string().map(|s| s.to_text())).unwrap_or_default()),
        b"ResetForm" => FieldAction::Reset(d.get(b"Fields").map(|f| texts(doc, f)).unwrap_or_default()),
        b"Named" => FieldAction::Named(d.name(b"N").map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default()),
        b"GoTo" => {
            // A named destination or a page in another file is kept, not edited.
            let page = d
                .get(b"D")
                .map(|x| doc.resolve(x))
                .and_then(|dest| dest.as_array().and_then(|a| a.first()).and_then(Object::as_ref))
                .and_then(|target| pages.iter().position(|p| *p == target));
            match page {
                Some(p) => FieldAction::GoTo(p),
                None => other(),
            }
        }
        b"Hide" => FieldAction::ShowHide {
            fields: d.get(b"T").map(|t| texts(doc, t)).unwrap_or_default(),
            hide: !matches!(d.get(b"H"), Some(Object::Bool(false))),
        },
        b"SubmitForm" => FieldAction::Submit(
            d.get(b"F")
                .map(|f| doc.resolve(f))
                .and_then(|f| {
                    f.as_string()
                        .map(|s| s.to_text())
                        .or_else(|| f.as_dict().and_then(|fd| fd.get(b"F")).and_then(|x| doc.resolve(x).as_string().map(|s| s.to_text())))
                })
                .unwrap_or_default(),
        ),
        _ => other(),
    })
}

/// One action of a trigger as read: what it does and its own dictionary (without `/Next`).
struct Read {
    action: FieldAction,
    original: Object,
}

/// The actions `entry` runs, in order: it, then its `/Next` actions depth first. Cycles and
/// chains longer than [`MAX_ACTIONS_PER_TRIGGER`] are cut.
fn chain(doc: &Document, entry: &Object, pages: &[ObjRef]) -> Vec<Read> {
    let mut out = Vec::new();
    let mut seen: HashSet<ObjRef> = HashSet::new();
    // A stack of objects still to visit; arrays push their items in reverse.
    let mut stack = vec![entry.clone()];
    let mut steps = 0usize;
    while let Some(o) = stack.pop() {
        steps += 1;
        if out.len() >= MAX_ACTIONS_PER_TRIGGER || steps > MAX_ACTIONS_PER_TRIGGER * 4 {
            break;
        }
        if let Object::Ref(r) = &o
            && !seen.insert(*r)
        {
            continue;
        }
        let resolved = doc.resolve(&o);
        match &*resolved {
            Object::Array(items) => {
                stack.extend(items.iter().take(MAX_ACTIONS_PER_TRIGGER).rev().cloned());
            }
            obj => {
                let Some(d) = obj.as_dict() else { continue };
                let next = d.get(b"Next").cloned();
                let mut original = obj.clone();
                if let Some(od) = original.as_dict_mut() {
                    od.remove(b"Next");
                }
                if let Some(a) = original.as_dict().and_then(|od| read(doc, od, pages)) {
                    out.push(Read { action: a, original });
                }
                if let Some(n) = next {
                    stack.push(n);
                }
            }
        }
    }
    out
}

/// A widget's entry for `t` (Mouse Up: `/A`, or `/AA /U` when there is no `/A`).
fn entry(wd: &Dict, aa: &Dict, t: Trigger) -> Option<Object> {
    match t.key() {
        None => wd.get(b"A").cloned().or_else(|| aa.get(b"U").cloned()),
        Some(k) => aa.get(k).cloned(),
    }
}

/// A widget's actions per trigger, in the tab's order.
fn widget_actions(doc: &Document, w: ObjRef, pages: &[ObjRef]) -> Vec<(Trigger, Vec<Read>)> {
    let wd = doc.get(w).as_dict().cloned().unwrap_or_default();
    let aa = wd.get(b"AA").map(|a| doc.resolve(a)).and_then(|a| a.as_dict().cloned()).unwrap_or_default();
    Trigger::ALL.into_iter().map(|t| (t, entry(&wd, &aa, t).map(|e| chain(doc, &e, pages)).unwrap_or_default())).collect()
}

/// The actions of field `name` (its first widget): each trigger's actions in the order they
/// run, triggers in the tab's order.
pub fn field_actions(doc: &Document, name: &str) -> Result<Vec<(Trigger, FieldAction)>, FormError> {
    let all = fields(doc);
    let f = all.iter().find(|f| f.name == name).ok_or_else(|| FormError::NoSuchField(name.into()))?;
    let pages = crate::page_refs(doc);
    let Some(w) = f.widgets.first() else { return Ok(Vec::new()) };
    Ok(widget_actions(doc, w.obj, &pages).into_iter().flat_map(|(t, list)| list.into_iter().map(move |r| (t, r.action))).collect())
}

/// A new action dictionary for `a` (not [`FieldAction::Other`]).
fn fresh(a: &FieldAction, pages: &[ObjRef]) -> Result<Object, FormError> {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("Action"));
    let list = |v: &[String]| Object::Array(v.iter().map(|s| Object::String(PdfString::text(s))).collect());
    match a {
        FieldAction::JavaScript(js) => {
            d.set(b"S".to_vec(), Object::name("JavaScript"));
            d.set(b"JS".to_vec(), PdfString::text(js));
        }
        FieldAction::Uri(u) => {
            d.set(b"S".to_vec(), Object::name("URI"));
            d.set(b"URI".to_vec(), PdfString::literal(u.as_bytes().to_vec()));
        }
        FieldAction::Reset(f) => {
            d.set(b"S".to_vec(), Object::name("ResetForm"));
            if !f.is_empty() {
                d.set(b"Fields".to_vec(), list(f));
            }
        }
        FieldAction::Named(n) => {
            if !MENU_ITEMS.contains(&n.as_str()) {
                return Err(FormError::Invalid(format!("the menu item {n:?} can't be added (choose one of {})", MENU_ITEMS.join(", "))));
            }
            d.set(b"S".to_vec(), Object::name("Named"));
            d.set(b"N".to_vec(), Object::name(n));
        }
        FieldAction::GoTo(p) => {
            let page = *pages.get(*p).ok_or_else(|| FormError::Invalid(format!("the document has no page {}", p.saturating_add(1))))?;
            d.set(b"S".to_vec(), Object::name("GoTo"));
            d.set(b"D".to_vec(), Object::Array(vec![Object::Ref(page), Object::name("Fit")]));
        }
        FieldAction::ShowHide { fields, hide } => {
            d.set(b"S".to_vec(), Object::name("Hide"));
            d.set(b"T".to_vec(), list(fields));
            d.set(b"H".to_vec(), Object::Bool(*hide));
        }
        FieldAction::Submit(u) => {
            d.set(b"S".to_vec(), Object::name("SubmitForm"));
            let mut fs = Dict::new();
            fs.set(b"FS".to_vec(), Object::name("URL"));
            fs.set(b"F".to_vec(), PdfString::literal(u.as_bytes().to_vec()));
            d.set(b"F".to_vec(), Object::Dict(fs));
            // HTML form format, as the viewer's default for web servers (ExportFormat, bit 3).
            d.set(b"Flags".to_vec(), Object::Int(4));
        }
        FieldAction::Other(kind) => {
            return Err(FormError::Invalid(format!("a {kind} action can't be created here; only an existing one can be kept")));
        }
    }
    Ok(Object::Dict(d))
}

/// Replace field `name`'s actions (on every widget) with `actions`: for each trigger, the
/// actions listed for it, run in the order given; triggers not listed lose theirs. An action
/// equal to one the field has (its first widget) keeps that action's dictionary, which is how
/// [`FieldAction::Other`] and edits PdfCraft doesn't model survive. Nothing changes when an
/// action is refused.
pub fn set_field_actions(doc: &mut Document, name: &str, actions: &[(Trigger, FieldAction)]) -> Result<(), FormError> {
    let all = fields(doc);
    let f = all.iter().find(|f| f.name == name).ok_or_else(|| FormError::NoSuchField(name.into()))?.clone();
    let pages = crate::page_refs(doc);
    let old: Vec<(Trigger, Vec<Read>)> = f.widgets.first().map(|w| widget_actions(doc, w.obj, &pages)).unwrap_or_default();

    // Every action's dictionary, decided before anything changes.
    let mut used = vec![Vec::new(); old.len()];
    for (slot, (_, list)) in used.iter_mut().zip(&old) {
        *slot = vec![false; list.len()];
    }
    let mut plan: Vec<(Trigger, Vec<FieldAction>, Vec<Object>)> = Vec::new();
    for t in Trigger::ALL {
        let wanted: Vec<FieldAction> = actions.iter().filter(|(x, _)| *x == t).map(|(_, a)| a.clone()).collect();
        if wanted.len() > MAX_ACTIONS_PER_TRIGGER {
            return Err(FormError::Invalid(format!("{}: at most {MAX_ACTIONS_PER_TRIGGER} actions per trigger", t.label())));
        }
        let mut dicts = Vec::with_capacity(wanted.len());
        for a in &wanted {
            // The same action of this trigger first, then of any other.
            let same = Trigger::ALL.iter().position(|x| *x == t).unwrap_or(0);
            let mut order = std::iter::once(same).chain((0..old.len()).filter(move |i| *i != same));
            let found = order.find_map(|ti| {
                let list = &old.get(ti)?.1;
                let i = list.iter().enumerate().position(|(i, r)| r.action == *a && !used.get(ti).and_then(|u| u.get(i)).copied().unwrap_or(true))?;
                Some((ti, i))
            });
            match found {
                Some((ti, i)) => {
                    if let Some(u) = used.get_mut(ti).and_then(|u| u.get_mut(i)) {
                        *u = true;
                    }
                    let original = old.get(ti).and_then(|(_, l)| l.get(i)).map(|r| r.original.clone()).unwrap_or(Object::Null);
                    dicts.push(original);
                }
                None => dicts.push(fresh(a, &pages)?),
            }
        }
        plan.push((t, wanted, dicts));
    }

    for w in &f.widgets {
        let current = widget_actions(doc, w.obj, &pages);
        let wd = doc.get(w.obj).as_dict().cloned().unwrap_or_default();
        let mut aa = wd.get(b"AA").map(|a| doc.resolve(a)).and_then(|a| a.as_dict().cloned()).unwrap_or_default();
        let mut a_entry = wd.get(b"A").cloned();
        let mut changed = false;
        for (t, wanted, dicts) in &plan {
            let unchanged = current.iter().find(|(x, _)| x == t).is_some_and(|(_, list)| list.iter().map(|r| &r.action).eq(wanted.iter()));
            if unchanged {
                continue;
            }
            changed = true;
            // Link the chain from its end: each action's /Next is the one after it.
            let mut next: Option<ObjRef> = None;
            for d in dicts.iter().rev() {
                let mut d = d.clone();
                if let Some(dd) = d.as_dict_mut() {
                    match next {
                        Some(n) => dd.set(b"Next".to_vec(), Object::Ref(n)),
                        None => {
                            dd.remove(b"Next");
                        }
                    }
                }
                next = Some(doc.add(d));
            }
            let obj = next.map(Object::Ref);
            match t.key() {
                None => {
                    a_entry = obj;
                    aa.remove(b"U");
                }
                Some(k) => match obj {
                    Some(o) => aa.set(k.to_vec(), o),
                    None => {
                        aa.remove(k);
                    }
                },
            }
        }
        if !changed {
            continue;
        }
        doc.update_dict(w.obj, |d| {
            match &a_entry {
                Some(a) => d.set(b"A".to_vec(), a.clone()),
                None => {
                    d.remove(b"A");
                }
            }
            if aa.is_empty() {
                d.remove(b"AA");
            } else {
                d.set(b"AA".to_vec(), Object::Dict(aa.clone()));
            }
        })?;
    }
    Ok(())
}

/// Set Document Actions: the document's own triggers (§12.6.3, Table 199: the catalog's `/AA`
/// WC, WS, DS, WP and DP). Like the viewer's dialog, they run JavaScript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DocTrigger {
    WillClose,
    WillSave,
    DidSave,
    WillPrint,
    DidPrint,
}

impl DocTrigger {
    pub const ALL: [DocTrigger; 5] = [DocTrigger::WillClose, DocTrigger::WillSave, DocTrigger::DidSave, DocTrigger::WillPrint, DocTrigger::DidPrint];

    pub fn label(self) -> &'static str {
        match self {
            DocTrigger::WillClose => "Document Will Close",
            DocTrigger::WillSave => "Document Will Save",
            DocTrigger::DidSave => "Document Did Save",
            DocTrigger::WillPrint => "Document Will Print",
            DocTrigger::DidPrint => "Document Did Print",
        }
    }

    /// Snake-case id (agents).
    pub fn id(self) -> &'static str {
        match self {
            DocTrigger::WillClose => "will_close",
            DocTrigger::WillSave => "will_save",
            DocTrigger::DidSave => "did_save",
            DocTrigger::WillPrint => "will_print",
            DocTrigger::DidPrint => "did_print",
        }
    }

    pub fn from_id(id: &str) -> Option<DocTrigger> {
        DocTrigger::ALL.into_iter().find(|t| t.id() == id)
    }

    /// `event.name` for its script (`event.type` is "Doc").
    pub fn event_name(self) -> &'static str {
        match self {
            DocTrigger::WillClose => "WillClose",
            DocTrigger::WillSave => "WillSave",
            DocTrigger::DidSave => "DidSave",
            DocTrigger::WillPrint => "WillPrint",
            DocTrigger::DidPrint => "DidPrint",
        }
    }

    fn key(self) -> &'static [u8] {
        match self {
            DocTrigger::WillClose => b"WC",
            DocTrigger::WillSave => b"WS",
            DocTrigger::DidSave => b"DS",
            DocTrigger::WillPrint => b"WP",
            DocTrigger::DidPrint => b"DP",
        }
    }
}

/// The catalog's `/AA`, resolved.
fn catalog_aa(doc: &Document) -> Option<(ObjRef, Dict)> {
    let root = doc.root()?;
    let cat = doc.get(root);
    let aa = cat.as_dict()?.get(b"AA").map(|a| doc.resolve(a)).and_then(|a| a.as_dict().cloned()).unwrap_or_default();
    Some((root, aa))
}

/// The document's actions: for each trigger that has any, its JavaScripts joined in the order
/// they run (other action types on these triggers are left out: they don't run here).
pub fn document_actions(doc: &Document) -> Vec<(DocTrigger, String)> {
    let Some((_, aa)) = catalog_aa(doc) else { return Vec::new() };
    let pages = crate::page_refs(doc);
    DocTrigger::ALL
        .into_iter()
        .filter_map(|t| {
            let entry = aa.get(t.key())?;
            let scripts: Vec<String> = chain(doc, entry, &pages)
                .into_iter()
                .filter_map(|r| match r.action {
                    FieldAction::JavaScript(js) => Some(js),
                    _ => None,
                })
                .collect();
            (!scripts.is_empty()).then(|| (t, scripts.join("\n")))
        })
        .collect()
}

/// Set (or with `None` or blank, remove) the JavaScript `trigger` runs, replacing what it ran.
pub fn set_document_action(doc: &mut Document, trigger: DocTrigger, script: Option<&str>) -> Result<(), FormError> {
    let (root, mut aa) = catalog_aa(doc).ok_or_else(|| FormError::Invalid("the document has no catalog".into()))?;
    match script.filter(|s| !s.trim().is_empty()) {
        Some(js) => {
            let action = fresh(&FieldAction::JavaScript(js.to_string()), &[])?;
            let r = doc.add(action);
            aa.set(trigger.key().to_vec(), Object::Ref(r));
        }
        None => {
            aa.remove(trigger.key());
        }
    }
    doc.update_dict(root, |d| {
        if aa.is_empty() {
            d.remove(b"AA");
        } else {
            d.set(b"AA".to_vec(), Object::Dict(aa.clone()));
        }
    })?;
    Ok(())
}
