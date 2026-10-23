//! The JavaScript debugger (Tools → JavaScript debugger): every script in the document and
//! where it lives, and debug runs with breakpoints, stepping, watches and a step budget
//! ([`pdfcraft_js::debug`]).
//!
//! The list is read on demand (when the debugger opens or an agent asks), never while the
//! document is opened, rendered or saved, and it is capped ([`MAX_LISTED_SCRIPTS`],
//! [`MAX_LISTED_BYTES`]).

use pdfcraft_cos::Object;
use pdfcraft_forms::{DocTrigger, FormScriptEvent, PageTrigger, Trigger};
pub use pdfcraft_js::debug::{
    DEFAULT_STEPS, DEFAULT_TIMEOUT_MS, Debug, DebugOutcome, MAX_STEPS, MAX_TIMEOUT_MS, MAX_WATCHES, Pause, Resume, StackFrame, Step, Watch,
    breakable_lines,
};
pub use pdfcraft_js::{ErrorAt, Outcome};

use crate::js::{ActionSource, ScriptEnv};
use crate::{DocId, EditError, Session};

/// Most scripts listed.
pub const MAX_LISTED_SCRIPTS: usize = 5000;
/// Most script text listed, in bytes.
pub const MAX_LISTED_BYTES: usize = 32 << 20;
/// Most annotations looked at on one page.
const MAX_ANNOTS_PER_PAGE: usize = 10_000;

/// Annotation triggers (§12.6.3, Table 194), with the `/AA` key; Mouse Up is also `/A`.
const ANNOT_TRIGGERS: [(&str, &[u8]); 10] = [
    ("mouse_up", b"U"),
    ("mouse_down", b"D"),
    ("mouse_enter", b"E"),
    ("mouse_exit", b"X"),
    ("on_focus", b"Fo"),
    ("on_blur", b"Bl"),
    ("page_open", b"PO"),
    ("page_close", b"PC"),
    ("page_visible", b"PV"),
    ("page_invisible", b"PI"),
];

/// Where a script lives in the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptLocation {
    /// A document-level script (the catalog's `/Names /JavaScript`).
    Document { name: String },
    /// The document's open action (the catalog's `/OpenAction`).
    OpenAction,
    /// Set Document Actions: will close, will save, …
    DocumentAction(DocTrigger),
    /// Page Properties → Actions (0-based page).
    Page { page: usize, trigger: PageTrigger },
    /// A field's keystroke, format, validate or calculate script.
    Field { field: String, event: &'static str },
    /// Field Properties → Actions.
    FieldTrigger { field: String, trigger: Trigger },
    /// A link (0-based page and index in its `/Annots`).
    Link { page: usize, index: usize },
    /// A bookmark, by its 0-based path, with its title.
    Bookmark { path: Vec<usize>, title: String },
    /// Another annotation's action (0-based page and index; trigger as in [`ANNOT_TRIGGERS`]).
    Annotation { page: usize, index: usize, subtype: String, trigger: String },
}

fn one_based(s: &str) -> Option<usize> {
    s.parse::<usize>().ok()?.checked_sub(1)
}

impl ScriptLocation {
    /// The kind, for grouping: document, open, document_action, page, field, field_trigger,
    /// link, bookmark or annotation.
    pub fn kind(&self) -> &'static str {
        match self {
            ScriptLocation::Document { .. } => "document",
            ScriptLocation::OpenAction => "open",
            ScriptLocation::DocumentAction(_) => "document_action",
            ScriptLocation::Page { .. } => "page",
            ScriptLocation::Field { .. } => "field",
            ScriptLocation::FieldTrigger { .. } => "field_trigger",
            ScriptLocation::Link { .. } => "link",
            ScriptLocation::Bookmark { .. } => "bookmark",
            ScriptLocation::Annotation { .. } => "annotation",
        }
    }

    /// A stable id agents pass back (pages and indexes 1-based; names last, so they may hold
    /// colons).
    pub fn id(&self) -> String {
        match self {
            ScriptLocation::Document { name } => format!("document:{name}"),
            ScriptLocation::OpenAction => "open".into(),
            ScriptLocation::DocumentAction(t) => format!("document_action:{}", t.id()),
            ScriptLocation::Page { page, trigger } => format!("page:{}:{}", page.saturating_add(1), trigger.id()),
            ScriptLocation::Field { field, event } => format!("field:{event}:{field}"),
            ScriptLocation::FieldTrigger { field, trigger } => format!("field_trigger:{}:{field}", trigger.id()),
            ScriptLocation::Link { page, index } => format!("link:{}:{}", page.saturating_add(1), index.saturating_add(1)),
            ScriptLocation::Bookmark { path, .. } => {
                format!("bookmark:{}", path.iter().map(|p| p.saturating_add(1).to_string()).collect::<Vec<_>>().join("."))
            }
            ScriptLocation::Annotation { page, index, trigger, .. } => {
                format!("annotation:{}:{}:{trigger}", page.saturating_add(1), index.saturating_add(1))
            }
        }
    }

    /// Where it is, in English (the debugger's list and tools).
    pub fn describe(&self) -> String {
        match self {
            ScriptLocation::Document { name } => format!("Document script {name}"),
            ScriptLocation::OpenAction => "Document open action".into(),
            ScriptLocation::DocumentAction(t) => t.label().into(),
            ScriptLocation::Page { page, trigger } => format!("Page {}: {}", page.saturating_add(1), trigger.label()),
            ScriptLocation::Field { field, event } => format!("Field {field}: {event}"),
            ScriptLocation::FieldTrigger { field, trigger } => format!("Field {field}: {}", trigger.label()),
            ScriptLocation::Link { page, index } => format!("Link {} on page {}", index.saturating_add(1), page.saturating_add(1)),
            ScriptLocation::Bookmark { title, .. } => format!("Bookmark {title}"),
            ScriptLocation::Annotation { page, index, subtype, trigger } => {
                format!("{subtype} annotation {} on page {}: {trigger}", index.saturating_add(1), page.saturating_add(1))
            }
        }
    }

    /// The location an [`ScriptLocation::id`] names (a bookmark's title is left empty).
    pub fn parse(id: &str) -> Option<ScriptLocation> {
        let (kind, rest) = id.split_once(':').unwrap_or((id, ""));
        Some(match kind {
            "document" if !rest.is_empty() => ScriptLocation::Document { name: rest.into() },
            "open" => ScriptLocation::OpenAction,
            "document_action" => ScriptLocation::DocumentAction(DocTrigger::from_id(rest)?),
            "page" => {
                let (p, t) = rest.split_once(':')?;
                ScriptLocation::Page { page: one_based(p)?, trigger: PageTrigger::from_id(t)? }
            }
            "field" => {
                let (e, f) = rest.split_once(':')?;
                let event = pdfcraft_forms::FIELD_SCRIPT_EVENTS.iter().find(|(x, _)| *x == e)?.0;
                ScriptLocation::Field { field: f.into(), event }
            }
            "field_trigger" => {
                let (t, f) = rest.split_once(':')?;
                ScriptLocation::FieldTrigger { field: f.into(), trigger: Trigger::from_id(t)? }
            }
            "link" => {
                let (p, i) = rest.split_once(':')?;
                ScriptLocation::Link { page: one_based(p)?, index: one_based(i)? }
            }
            "bookmark" => ScriptLocation::Bookmark { path: rest.split('.').map(one_based).collect::<Option<Vec<_>>>()?, title: String::new() },
            "annotation" => {
                let mut it = rest.splitn(3, ':');
                let page = one_based(it.next()?)?;
                let index = one_based(it.next()?)?;
                let trigger = it.next()?;
                ANNOT_TRIGGERS.iter().find(|(t, _)| *t == trigger)?;
                ScriptLocation::Annotation { page, index, subtype: String::new(), trigger: trigger.into() }
            }
            _ => return None,
        })
    }

    /// Whether `other` is the same place (titles and subtypes, which ids leave out, aside).
    pub fn same_place(&self, other: &ScriptLocation) -> bool {
        self.id() == other.id()
    }

    /// The link, bookmark or page trigger, as [`Session::run_action_javascript`] runs it.
    pub fn action_source(&self) -> Option<ActionSource> {
        match self {
            ScriptLocation::Link { page, index } => Some(ActionSource::Link { page: *page, index: *index }),
            ScriptLocation::Bookmark { path, .. } => Some(ActionSource::Bookmark { path: path.clone() }),
            ScriptLocation::Page { page, trigger } => Some(ActionSource::Page { page: *page, trigger: *trigger }),
            _ => None,
        }
    }
}

/// A script and where it lives.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptEntry {
    pub location: ScriptLocation,
    pub script: String,
}

impl ScriptEntry {
    pub fn lines(&self) -> usize {
        self.script.lines().count().max(1)
    }
}

/// Every script in a document ([`Session::javascript_index`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptIndex {
    pub scripts: Vec<ScriptEntry>,
    /// More scripts exist than [`MAX_LISTED_SCRIPTS`] or [`MAX_LISTED_BYTES`] allow.
    pub truncated: bool,
}

impl ScriptIndex {
    fn push(&mut self, location: ScriptLocation, script: String, bytes: &mut usize) -> bool {
        if self.scripts.len() >= MAX_LISTED_SCRIPTS || bytes.saturating_add(script.len()) > MAX_LISTED_BYTES {
            self.truncated = true;
            return false;
        }
        *bytes = bytes.saturating_add(script.len());
        self.scripts.push(ScriptEntry { location, script });
        true
    }

    pub fn find(&self, location: &ScriptLocation) -> Option<&ScriptEntry> {
        self.scripts.iter().find(|e| e.location.same_place(location))
    }
}

/// The title of the bookmark at `path`.
fn bookmark_title(outline: &[pdfcraft_render::OutlineItem], path: &[usize]) -> String {
    let mut items = outline;
    let mut title = String::new();
    for i in path {
        let Some(item) = items.get(*i) else { return title };
        title = item.title.clone();
        items = &item.children;
    }
    title
}

/// The event a script at `location` runs with (`event.type` and `event.name`, the target
/// field and its value), and the page `this.pageNum` starts on.
fn event_for(env: &mut ScriptEnv, location: Option<&ScriptLocation>) -> pdfcraft_js::Event {
    match location {
        None | Some(ScriptLocation::Document { .. } | ScriptLocation::Annotation { .. }) => pdfcraft_js::Event::doc("Console"),
        Some(ScriptLocation::OpenAction) => pdfcraft_js::Event::doc("Open"),
        Some(ScriptLocation::DocumentAction(t)) => pdfcraft_js::Event::doc(t.event_name()),
        Some(ScriptLocation::Field { field, event }) => {
            let value = env.states.iter().find(|s| s.name == *field).map(|s| s.value.join(",")).unwrap_or_default();
            let mut name: String = event.chars().take(1).flat_map(char::to_uppercase).collect();
            name.push_str(event.get(1..).unwrap_or(""));
            pdfcraft_js::Event { will_commit: true, ..pdfcraft_js::Event::field(&name, field, &value) }
        }
        Some(ScriptLocation::FieldTrigger { field, trigger }) => env.event(Some(field), trigger.event_name(), None),
        Some(other) => match other.action_source() {
            Some(src) => {
                let (kind, name, page) = src.event();
                env.event(None, name, Some((kind, page)))
            }
            None => pdfcraft_js::Event::doc("Console"),
        },
    }
}

impl Session {
    /// Every JavaScript in document `id` and where it lives: document-level scripts, the open
    /// action, document and page actions, field scripts and field actions, links, bookmarks and
    /// other annotations' actions, in that order. Read on demand and capped.
    pub fn javascript_index(&self, id: DocId) -> Result<ScriptIndex, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let cos = self.cos(id)?;
        let mut out = ScriptIndex::default();
        let mut bytes = 0usize;
        for (name, js) in pdfcraft_forms::document_scripts_named(cos) {
            out.push(ScriptLocation::Document { name }, js, &mut bytes);
        }
        if let Some(open) = cos.root().and_then(|r| cos.get(r).as_dict().and_then(|c| c.get(b"OpenAction").cloned())) {
            let js = pdfcraft_forms::action_scripts(cos, &open);
            if !js.is_empty() {
                out.push(ScriptLocation::OpenAction, js.join("\n"), &mut bytes);
            }
        }
        for (t, js) in pdfcraft_forms::document_actions(cos) {
            out.push(ScriptLocation::DocumentAction(t), js, &mut bytes);
        }
        for (page, trigger, js) in pdfcraft_forms::page_actions(cos) {
            out.push(ScriptLocation::Page { page, trigger }, js, &mut bytes);
        }
        for f in pdfcraft_forms::form_scripts(cos, MAX_LISTED_SCRIPTS.saturating_sub(out.scripts.len()).saturating_add(1)) {
            let location = match f.event {
                FormScriptEvent::Field(event) => ScriptLocation::Field { field: f.field, event },
                FormScriptEvent::Trigger(trigger) => ScriptLocation::FieldTrigger { field: f.field, trigger },
            };
            if !out.push(location, f.script, &mut bytes) {
                break;
            }
        }
        for l in &doc.links {
            if let pdfcraft_annot::links::LinkAction::JavaScript(js) = &l.action {
                out.push(ScriptLocation::Link { page: l.page, index: l.index }, js.clone(), &mut bytes);
            }
        }
        for (path, action) in pdfcraft_organize::bookmark_actions(cos) {
            if let crate::BookmarkAction::JavaScript(js) = action {
                let title = bookmark_title(&doc.info.outline, &path);
                out.push(ScriptLocation::Bookmark { path, title }, js, &mut bytes);
            }
        }
        'pages: for (p, page) in pdfcraft_model::pages(cos).iter().enumerate() {
            let Some(annots) = page.dict.get(b"Annots").map(|a| cos.resolve(a)).and_then(|a| a.as_array().cloned()) else { continue };
            for (i, a) in annots.iter().take(MAX_ANNOTS_PER_PAGE).enumerate() {
                let resolved = cos.resolve(a);
                let Some(d) = resolved.as_dict() else { continue };
                let subtype = d.name(b"Subtype").map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
                // Links and form fields are listed above.
                if subtype == "Link" || subtype == "Widget" {
                    continue;
                }
                let aa = d.get(b"AA").map(|x| cos.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
                for (trigger, key) in ANNOT_TRIGGERS {
                    let entry: Option<Object> =
                        if trigger == "mouse_up" { d.get(b"A").cloned().or_else(|| aa.get(key).cloned()) } else { aa.get(key).cloned() };
                    let Some(entry) = entry else { continue };
                    let js = pdfcraft_forms::action_scripts(cos, &entry);
                    if js.is_empty() {
                        continue;
                    }
                    let location = ScriptLocation::Annotation { page: p, index: i, subtype: subtype.clone(), trigger: trigger.into() };
                    if !out.push(location, js.join("\n"), &mut bytes) {
                        break 'pages;
                    }
                }
            }
        }
        Ok(out)
    }

    /// The script at `location` (see [`ScriptLocation::id`]), or an error an agent can act on.
    pub fn javascript_at(&self, id: DocId, location: &ScriptLocation) -> Result<ScriptEntry, EditError> {
        self.javascript_index(id)?
            .find(location)
            .cloned()
            .ok_or_else(|| EditError::Invalid(format!("there is no script at {} (list them with js_scripts)", location.id())))
    }

    /// Debug `script` in document `id` (as the script at `location` runs, when given: its
    /// event, target field and page; otherwise as the console runs it). Same sandbox and
    /// permission checks as [`Session::run_javascript`]; dialogs get Cancel or No. A run that
    /// finishes applies its field, comment and form-reset changes as one undoable step, like
    /// the console; a run that pauses or is stopped applies nothing.
    pub fn debug_javascript(&mut self, id: DocId, script: &str, location: Option<&ScriptLocation>, debug: &Debug) -> Result<DebugOutcome, EditError> {
        if self.js_off {
            return Err(EditError::Invalid("JavaScript is turned off (Preferences → JavaScript)".into()));
        }
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let cos = doc.editor.as_ref().map(|e| e.cos.clone()).ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))?;
        let mut env = ScriptEnv::new(doc, &cos);
        let event = event_for(&mut env, location);
        let mut d = pdfcraft_js::debug::run_debug(
            script,
            &event,
            &env.runner.doc,
            &env.states,
            &env.runner.doc_scripts,
            pdfcraft_js::Limits::default(),
            debug,
        );
        if d.paused.is_none() && !d.out_of_steps {
            self.apply_script_outcome(id, &mut d.outcome, &env.fields, &env.states)?;
        }
        Ok(d)
    }

    /// The names of document `id`'s document-level scripts, in the order they run (what
    /// [`ErrorAt::script`] and [`StackFrame::script`] index).
    pub fn document_script_names(&self, id: DocId) -> Vec<String> {
        self.cos(id).map(|c| pdfcraft_forms::document_scripts_named(c).into_iter().map(|(n, _)| n).collect()).unwrap_or_default()
    }
}
