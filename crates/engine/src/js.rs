//! Acrobat JavaScript in documents: field scripts run during form edits ([`JsRunner`], wired
//! into `forms` through its [`pdfcraft_forms::Scripts`] hook), and button actions and console
//! input run on request ([`Session::run_javascript`]). What scripts ask of the viewer (alerts,
//! printing, navigation, links) is kept for the front end in [`JsOutput`].
//!
//! Button actions and console input can change comments (`getAnnots`, `addAnnot`) and show or
//! hide layers (`getOCGs`), and can stop on a dialog that needs an answer
//! ([`Session::run_javascript_with`]).
//!
//! Preferences ▸ JavaScript ▸ Enable Acrobat JavaScript is [`Session::set_javascript`]; with it
//! off, scripts don't run and AF calls keep working natively.

use pdfcraft_forms::{Field, FieldChange, FieldEvent, FieldKind, ScriptResult, Scripts};
pub use pdfcraft_js::{Answer, Dialogs, Limits, Outcome, Question, Request};

use crate::{DocId, Edit, EditError, Session};

/// What scripts produced for the viewer since it last looked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsOutput {
    pub alerts: Vec<String>,
    pub console: Vec<String>,
    pub requests: Vec<Request>,
    /// Script errors (as Acrobat's console shows them).
    pub errors: Vec<String>,
}

impl JsOutput {
    pub fn is_empty(&self) -> bool {
        self.alerts.is_empty() && self.console.is_empty() && self.requests.is_empty() && self.errors.is_empty()
    }

    fn absorb(&mut self, o: &Outcome) {
        self.alerts.extend(o.alerts.iter().cloned());
        self.console.extend(o.console.iter().cloned());
        self.requests.extend(o.requests.iter().cloned());
        self.errors.extend(o.error.iter().cloned());
    }

    pub(crate) fn append(&mut self, other: JsOutput) {
        self.alerts.extend(other.alerts);
        self.console.extend(other.console);
        self.requests.extend(other.requests);
        self.errors.extend(other.errors);
    }
}

fn kind(k: FieldKind) -> pdfcraft_js::FieldType {
    use pdfcraft_js::FieldType as T;
    match k {
        FieldKind::Text => T::Text,
        FieldKind::CheckBox => T::CheckBox,
        FieldKind::Radio => T::RadioButton,
        FieldKind::PushButton => T::Button,
        FieldKind::Combo => T::ComboBox,
        FieldKind::List => T::ListBox,
        FieldKind::Signature => T::Signature,
    }
}

/// `field.display` from the first widget's annotation flags.
fn display(doc: &pdfcraft_cos::Document, f: &Field) -> i32 {
    let flags = f.widgets.first().and_then(|w| doc.get(w.obj).as_dict().and_then(|d| d.int(b"F"))).unwrap_or(4);
    if flags & 2 != 0 {
        pdfcraft_js::DISPLAY_HIDDEN
    } else if flags & 32 != 0 {
        pdfcraft_js::DISPLAY_NO_VIEW
    } else if flags & 4 == 0 {
        pdfcraft_js::DISPLAY_NO_PRINT
    } else {
        pdfcraft_js::DISPLAY_VISIBLE
    }
}

/// A form field as scripts see it.
pub fn field_state(doc: &pdfcraft_cos::Document, f: &Field) -> pdfcraft_js::FieldState {
    let mut s = pdfcraft_js::FieldState::new(f.name.clone(), kind(f.kind), f.value.clone());
    s.default = f.default.clone();
    s.readonly = f.read_only();
    s.required = f.has(pdfcraft_forms::flags::REQUIRED);
    s.display = display(doc, f);
    s.char_limit = f.max_len;
    s.options = match f.kind {
        FieldKind::CheckBox | FieldKind::Radio => f.widgets.iter().filter_map(|w| w.on_state.clone()).map(|o| (o.clone(), o)).collect(),
        _ => f.options.clone(),
    };
    s
}

/// The changes a script made, against the fields as they were.
fn changes(before: &[pdfcraft_js::FieldState], after: &[pdfcraft_js::FieldState]) -> Vec<FieldChange> {
    after
        .iter()
        .filter_map(|a| {
            let b = before.iter().find(|b| b.name == a.name)?;
            let c = FieldChange {
                name: a.name.clone(),
                value: (a.value != b.value).then(|| a.value.clone()),
                read_only: (a.readonly != b.readonly).then_some(a.readonly),
                required: (a.required != b.required).then_some(a.required),
                display: (a.display != b.display).then_some(a.display),
            };
            (c.value.is_some() || c.read_only.is_some() || c.required.is_some() || c.display.is_some()).then_some(c)
        })
        .collect()
}

/// Runs field scripts with the document's object model and keeps what they print or ask for.
pub struct JsRunner {
    pub doc: pdfcraft_js::DocInfo,
    pub doc_scripts: Vec<String>,
    pub output: JsOutput,
    /// The fields in the document as the forms code last read them (for `display`).
    cos: pdfcraft_cos::Document,
}

impl JsRunner {
    pub fn new(cos: &pdfcraft_cos::Document, file_name: &str) -> JsRunner {
        let doc = pdfcraft_js::DocInfo {
            file_name: file_name.to_string(),
            num_pages: pdfcraft_model::pages(cos).len(),
            page: 0,
            info: info(cos),
            ..Default::default()
        };
        JsRunner { doc, doc_scripts: pdfcraft_forms::document_scripts(cos), output: JsOutput::default(), cos: cos.clone() }
    }
}

/// The Info dictionary as `this.info` keys (lower-case).
fn info(cos: &pdfcraft_cos::Document) -> Vec<(String, String)> {
    let Some(d) = cos.trailer().get(b"Info").map(|i| cos.resolve(i)).and_then(|i| i.as_dict().cloned()) else { return Vec::new() };
    d.iter()
        .filter_map(|(k, v)| match &*cos.resolve(v) {
            pdfcraft_cos::Object::String(s) => Some((String::from_utf8_lossy(k).to_lowercase(), s.to_text())),
            _ => None,
        })
        .collect()
}

impl Scripts for JsRunner {
    fn run(&mut self, event: FieldEvent, script: &str, target: &Field, value: &str, fields: &[Field]) -> ScriptResult {
        let states: Vec<_> = fields.iter().map(|f| field_state(&self.cos, f)).collect();
        let ev = pdfcraft_js::Event::field(event.name(), &target.name, value);
        let o = pdfcraft_js::run(script, &ev, &self.doc, &states, &self.doc_scripts, Limits::default());
        self.output.absorb(&o);
        if o.error.is_some() {
            // As in Acrobat, a failing script leaves the value alone (the error goes to the console).
            return ScriptResult { rc: true, value: value.to_string(), changes: Vec::new(), message: None };
        }
        let after: Vec<_> = states.iter().map(|s| o.changed.iter().find(|c| c.name == s.name).unwrap_or(s).clone()).collect();
        ScriptResult {
            rc: o.rc,
            value: o.value.clone(),
            changes: changes(&states, &after),
            message: (!o.rc).then(|| o.alerts.last().cloned()).flatten(),
        }
    }
}

/// The comments as `this.getAnnots()` lists them.
fn annot_states(cos: &pdfcraft_cos::Document) -> Vec<pdfcraft_js::AnnotState> {
    pdfcraft_annot::summaries(cos)
        .into_iter()
        .map(|a| pdfcraft_js::AnnotState {
            page: a.page,
            index: a.index,
            name: a.name.unwrap_or_default(),
            kind: a.subtype,
            author: a.author.unwrap_or_default(),
            contents: a.contents.unwrap_or_default(),
            rect: a.rect.map(f64::from),
            locked: a.locked,
        })
        .collect()
}

/// The edits for a comment change a script made.
fn annot_edits(c: &pdfcraft_js::AnnotChange) -> Vec<Edit> {
    use pdfcraft_annot::{LineEnding, Markup, NewAnnotation, NoteIcon, Shape, Style};
    use pdfcraft_js::AnnotChange as C;
    match c {
        C::Set { page, index, contents, author } => {
            let mut edits = Vec::new();
            if let Some(text) = contents {
                edits.push(Edit::SetAnnotationContents { page: *page, index: *index, text: text.clone() });
            }
            if author.is_some() {
                edits.push(Edit::SetAnnotationInfo { page: *page, index: *index, author: author.clone(), subject: None, icon: None });
            }
            edits
        }
        C::Delete { page, index } => vec![Edit::DeleteAnnotation { page: *page, index: *index }],
        C::Add(a) => {
            let r = a.rect;
            let shape = match a.kind.as_str() {
                "Text" => Shape::Note { at: [r[0], r[3]], icon: NoteIcon::default() },
                "FreeText" => Shape::TextBox { rect: r, font_size: a.text_size.unwrap_or(12.0) },
                "Circle" => Shape::Oval { rect: r },
                "Line" => {
                    let (from, to) = match a.points[..] {
                        [from, to] => (from, to),
                        _ => ([r[0], r[1]], [r[2], r[3]]),
                    };
                    Shape::Line { from, to, start: LineEnding::None, end: LineEnding::None }
                }
                "Highlight" => Shape::TextMarkup { kind: Markup::Highlight, quads: a.quads.clone() },
                "Underline" => Shape::TextMarkup { kind: Markup::Underline, quads: a.quads.clone() },
                "StrikeOut" => Shape::TextMarkup { kind: Markup::StrikeOut, quads: a.quads.clone() },
                "Squiggly" => Shape::TextMarkup { kind: Markup::Squiggly, quads: a.quads.clone() },
                _ => Shape::Rectangle { rect: r },
            };
            let mut style = Style::default_for(&shape);
            if let Some(c) = a.color {
                style.color = c;
            }
            vec![Edit::AddAnnotation(NewAnnotation { page: a.page, shape, style, contents: a.contents.clone(), author: a.author.clone() })]
        }
    }
}

/// A document-level JavaScript: its name and source.
pub fn document_scripts(cos: &pdfcraft_cos::Document) -> Vec<(String, String)> {
    pdfcraft_forms::document_scripts_named(cos)
}

impl crate::Document {
    /// Field Properties ▸ Actions: field `name`'s action per trigger.
    pub fn field_actions(&self, name: &str) -> Vec<(pdfcraft_forms::Trigger, pdfcraft_forms::FieldAction)> {
        self.editor.as_ref().and_then(|e| pdfcraft_forms::field_actions(&e.cos, name).ok()).unwrap_or_default()
    }

    /// Set Document Actions: each document trigger's JavaScript.
    pub fn document_actions(&self) -> Vec<(pdfcraft_forms::DocTrigger, String)> {
        self.editor.as_ref().map(|e| pdfcraft_forms::document_actions(&e.cos)).unwrap_or_default()
    }

    /// Page Properties ▸ Actions: each page trigger's JavaScript, as (0-based page, trigger,
    /// script). Reads every page; for one page use [`crate::Document::page_action`].
    pub fn page_actions(&self) -> Vec<(usize, pdfcraft_forms::PageTrigger, String)> {
        self.editor.as_ref().map(|e| pdfcraft_forms::page_actions(&e.cos)).unwrap_or_default()
    }

    /// The JavaScript page `page` runs on `trigger`, if any. Pages the open pass found without
    /// additional actions (`info.page_actions`) are answered without reading the document.
    pub fn page_action(&self, page: usize, trigger: pdfcraft_forms::PageTrigger) -> Option<String> {
        if self.info.page_actions.binary_search(&page).is_err() {
            return None;
        }
        let cos = &self.editor.as_ref()?.cos;
        pdfcraft_forms::page_action(cos, page, trigger)
    }

    /// What the unsigned signature field `field` locks when signed (its `/Lock`).
    pub fn signature_field_lock(&self, field: &str) -> Option<pdfcraft_sign::FieldLock> {
        self.editor.as_ref().and_then(|e| pdfcraft_sign::lock::field_lock(&e.cos, field))
    }

    /// Document JavaScripts (name, source), in name order.
    pub fn document_scripts(&self) -> Vec<(String, String)> {
        self.editor.as_ref().map(|e| document_scripts(&e.cos)).unwrap_or_default()
    }
}

impl Session {
    /// Preferences ▸ JavaScript ▸ Enable Acrobat JavaScript (on by default).
    pub fn set_javascript(&mut self, on: bool) {
        self.js_off = !on;
    }

    pub fn javascript(&self) -> bool {
        !self.js_off
    }

    /// What scripts printed or asked for since the last call (alerts, console, print, …).
    pub fn take_js_output(&mut self, id: DocId) -> JsOutput {
        self.docs.iter_mut().find(|d| d.id == id).map(|d| std::mem::take(&mut d.js_output)).unwrap_or_default()
    }

    /// Run `script` in document `id`, for a button (`target`: the field, event "Mouse Up") or
    /// the console (no target). Field changes and `resetForm` are applied as one undoable step;
    /// everything else the script asked for is returned.
    /// Dialogs that need an answer get Cancel or No.
    pub fn run_javascript(&mut self, id: DocId, script: &str, target: Option<&str>) -> Result<Outcome, EditError> {
        self.run_javascript_with(id, script, target, &Dialogs::default())
    }

    /// [`Session::run_javascript`] with `dialogs` answering `app.alert` buttons and
    /// `app.response`. When the script stops on a question ([`Outcome::question`]) nothing is
    /// applied: ask the user and run it again with the answer added.
    pub fn run_javascript_with(&mut self, id: DocId, script: &str, target: Option<&str>, dialogs: &Dialogs) -> Result<Outcome, EditError> {
        let event = if target.is_some() { pdfcraft_forms::Trigger::MouseUp.event_name() } else { "Console" };
        self.run_script_event(id, script, target, event, dialogs)
    }

    /// Run the JavaScript of document trigger `trigger` (Set Document Actions), if it has one
    /// and JavaScript is on: its field changes are one undoable step, and what it printed or
    /// asked for joins the document's script output ([`Session::take_js_output`]). Requests to
    /// print or save are dropped, so a Will Print or Will Save script can't start another one.
    /// Dialogs get Cancel or No. Returns whether a script ran.
    pub fn run_document_action(&mut self, id: DocId, trigger: pdfcraft_forms::DocTrigger) -> Result<bool, EditError> {
        if self.js_off {
            return Ok(false);
        }
        let Some(script) = self.get(id).and_then(|d| d.document_actions().into_iter().find(|(t, _)| *t == trigger)).map(|(_, s)| s) else {
            return Ok(false);
        };
        let mut o = self.run_script_event(id, &script, None, trigger.event_name(), &Dialogs::default())?;
        o.requests.retain(|r| !matches!(r, Request::Print | Request::SaveAs | Request::SetLayer { .. }));
        if let Some(d) = self.docs.iter_mut().find(|d| d.id == id) {
            d.js_output.absorb(&o);
        }
        Ok(true)
    }

    /// [`Session::run_javascript`] for a field event named `event` (`event.name`); without a
    /// target, a document event of that name.
    fn run_script_event(&mut self, id: DocId, script: &str, target: Option<&str>, event: &str, dialogs: &Dialogs) -> Result<Outcome, EditError> {
        self.run_script_in(id, script, target, event, None, dialogs)
    }

    /// [`Session::run_script_event`] with, for a script without a target, `scope` = (`event.type`,
    /// the page `this.pageNum` starts on) instead of a document event on the current page.
    fn run_script_in(
        &mut self,
        id: DocId,
        script: &str,
        target: Option<&str>,
        event: &str,
        scope: Option<(&str, usize)>,
        dialogs: &Dialogs,
    ) -> Result<Outcome, EditError> {
        if self.js_off {
            return Err(EditError::Invalid("JavaScript is turned off (Preferences ▸ JavaScript)".into()));
        }
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let cos = doc.editor.as_ref().map(|e| e.cos.clone()).ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))?;
        // A button PdfCraft generated from an XFA template runs its XFA click script.
        if let Some(t) = target
            && event == pdfcraft_forms::Trigger::MouseUp.event_name()
            && let Some(tpl) = doc.xfa_template.clone()
            && let Some(som) = crate::xfa::clickable_som(&cos, &tpl, t)
        {
            self.apply(id, Edit::XfaEvent { som, activity: "click".into() })?;
            let out = self.take_js_output(id);
            return Ok(Outcome {
                rc: true,
                alerts: out.alerts,
                console: out.console,
                requests: out.requests,
                error: out.errors.first().cloned(),
                ..Default::default()
            });
        }
        let mut env = ScriptEnv::new(doc, &cos);
        let event = env.event(target, event, scope);
        let mut o = pdfcraft_js::run_with(script, &event, &env.runner.doc, &env.states, &env.runner.doc_scripts, Limits::default(), dialogs);
        if o.question.is_some() {
            return Ok(o);
        }
        self.apply_script_outcome(id, &mut o, &env.fields, &env.states)?;
        Ok(o)
    }

    /// Apply what a finished script did: field changes, form resets and comment changes as one
    /// undoable step (Reset requests are taken out of `o`), and layers shown or hidden.
    pub(crate) fn apply_script_outcome(
        &mut self,
        id: DocId,
        o: &mut Outcome,
        fields: &[pdfcraft_forms::Field],
        states: &[pdfcraft_js::FieldState],
    ) -> Result<(), EditError> {
        let after: Vec<_> = states.iter().map(|s| o.changed.iter().find(|c| c.name == s.name).unwrap_or(s).clone()).collect();
        let changes = changes(states, &after);
        let resets: Vec<Vec<String>> = o
            .requests
            .iter()
            .filter_map(|r| match r {
                Request::Reset(n) => Some(n.clone()),
                _ => None,
            })
            .collect();
        o.requests.retain(|r| !matches!(r, Request::Reset(_)));
        let mut edits = Vec::new();
        if !changes.is_empty() {
            edits.push(Edit::ApplyScriptChanges { changes });
        }
        for names in resets {
            let all: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
            let names = (!names.is_empty())
                .then(|| all.iter().filter(|n| names.iter().any(|x| *n == x || n.starts_with(&format!("{x}.")))).cloned().collect());
            edits.push(Edit::ResetForm { names });
        }
        edits.extend(o.annots.iter().flat_map(annot_edits));
        if !edits.is_empty() {
            self.apply(id, Edit::Batch { label: "Run JavaScript".into(), edits })?;
        }
        // Layer visibility is a view setting, as in the Layers panel: not part of the undo step.
        for r in &o.requests {
            if let Request::SetLayer { index, on } = r {
                self.set_layer_visible(id, *index, *on);
            }
        }
        Ok(())
    }

    /// The JavaScript an action runs (a link's, a bookmark's or a page trigger's), or an error
    /// an agent can act on when it runs something else or nothing.
    pub fn action_script(&self, id: DocId, source: &ActionSource) -> Result<String, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        match source {
            ActionSource::Link { page, index } => {
                let l = doc
                    .links
                    .iter()
                    .find(|l| l.page == *page && l.index == *index)
                    .ok_or_else(|| EditError::Invalid(format!("page {} has no link {}", page.saturating_add(1), index.saturating_add(1))))?;
                match &l.action {
                    pdfcraft_annot::links::LinkAction::JavaScript(js) => Ok(js.clone()),
                    _ => Err(EditError::Invalid("that link doesn't run a JavaScript".into())),
                }
            }
            ActionSource::Bookmark { path } => match self.bookmark_action(id, path)? {
                crate::BookmarkAction::JavaScript(js) => Ok(js),
                _ => Err(EditError::Invalid("that bookmark doesn't run a JavaScript".into())),
            },
            ActionSource::Page { page, trigger } => {
                if *page >= doc.info.pages.len() {
                    return Err(EditError::Invalid(format!("the document has no page {}", page.saturating_add(1))));
                }
                doc.page_action(*page, *trigger)
                    .ok_or_else(|| EditError::Invalid(format!("page {} has no {} script", page.saturating_add(1), trigger.label())))
            }
        }
    }

    /// Run a link's, bookmark's or page trigger's JavaScript action, as the viewer does when it
    /// is clicked or the page opens or closes: `event.type` is Link, Bookmark or Page and
    /// `event.name` Mouse Up, Open or Close; `this.pageNum` starts on the link's or trigger's
    /// page. Same sandbox, limits and permission checks as [`Session::run_javascript_with`]:
    /// field and comment changes are one undoable step (refused when the document's
    /// permissions don't allow them), and a dialog that needs an answer stops it with nothing
    /// applied.
    pub fn run_action_javascript(&mut self, id: DocId, source: &ActionSource, dialogs: &Dialogs) -> Result<Outcome, EditError> {
        if self.js_off {
            return Err(EditError::Invalid("JavaScript is turned off (Preferences ▸ JavaScript)".into()));
        }
        let script = self.action_script(id, source)?;
        let (kind, name, page) = source.event();
        self.run_script_in(id, &script, None, name, Some((kind, page)), dialogs)
    }

    /// Run page `page`'s `trigger` script, if it has one and JavaScript is on, as the viewer
    /// does when the page becomes (or stops being) the current one. Like a document action,
    /// what it printed or asked for joins the document's script output
    /// ([`Session::take_js_output`]) and dialogs get Cancel or No; requests to print, save or
    /// go to another page are dropped, so page scripts can't bounce the view between pages or
    /// start a print. Returns whether a script ran.
    pub fn run_page_action(&mut self, id: DocId, page: usize, trigger: pdfcraft_forms::PageTrigger) -> Result<bool, EditError> {
        if self.js_off || self.get(id).and_then(|d| d.page_action(page, trigger)).is_none() {
            return Ok(false);
        }
        let mut o = self.run_action_javascript(id, &ActionSource::Page { page, trigger }, &Dialogs::default())?;
        o.requests.retain(|r| !matches!(r, Request::Print | Request::SaveAs | Request::GoToPage(_)));
        if let Some(d) = self.docs.iter_mut().find(|d| d.id == id) {
            d.js_output.absorb(&o);
        }
        Ok(true)
    }

    /// Fire `trigger` on field `field`: run its actions (Field Properties ▸ Actions) in order, as
    /// the viewer does when the pointer or focus reaches the field. Scripts run with that event's
    /// name; Reset a form and Show/hide a field change the document (each an undoable step, like
    /// a script's changes); navigation, links, menu items and submits are returned for the front
    /// end, never acted on here. Actions PdfCraft can't run are listed in `skipped`.
    pub fn run_field_trigger(&mut self, id: DocId, field: &str, trigger: pdfcraft_forms::Trigger) -> Result<TriggerOutcome, EditError> {
        self.run_field_trigger_from(id, field, trigger, 0)
    }

    /// [`Session::run_field_trigger`] without the chain's first `skip` actions (a viewer that
    /// ran a push button's first Mouse Up action itself runs the rest here).
    pub fn run_field_trigger_from(
        &mut self,
        id: DocId,
        field: &str,
        trigger: pdfcraft_forms::Trigger,
        skip: usize,
    ) -> Result<TriggerOutcome, EditError> {
        use pdfcraft_forms::FieldAction as A;
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let cos = doc.editor.as_ref().map(|e| &e.cos).ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))?;
        let actions: Vec<A> = pdfcraft_forms::field_actions(cos, field)
            .map_err(|e| EditError::Invalid(e.to_string()))?
            .into_iter()
            .filter(|(t, _)| *t == trigger)
            .map(|(_, a)| a)
            .skip(skip)
            .collect();
        let mut out = TriggerOutcome::default();
        for a in actions {
            let names = |list: &[String], this: &Session| -> Vec<String> {
                let all: Vec<String> = this.get(id).map(|d| d.form.iter().map(|f| f.name.clone()).collect()).unwrap_or_default();
                all.into_iter().filter(|n| list.iter().any(|x| n == x || n.starts_with(&format!("{x}.")))).collect()
            };
            match &a {
                A::JavaScript(script) => {
                    if self.js_off {
                        out.skipped.push(format!("{} (JavaScript is turned off)", a.describe()));
                        continue;
                    }
                    let o = self.run_script_event(id, script, Some(field), trigger.event_name(), &Dialogs::default())?;
                    out.alerts.extend(o.alerts);
                    out.console.extend(o.console);
                    out.errors.extend(o.error);
                    out.requests.extend(o.requests);
                }
                A::Reset(list) => {
                    let targets = (!list.is_empty()).then(|| names(list, self));
                    self.apply(id, Edit::ResetForm { names: targets })?;
                }
                A::ShowHide { fields, hide } => {
                    let changes: Vec<FieldChange> = names(fields, self)
                        .into_iter()
                        .map(|name| FieldChange {
                            name,
                            value: None,
                            read_only: None,
                            required: None,
                            display: Some(if *hide { pdfcraft_js::DISPLAY_HIDDEN } else { pdfcraft_js::DISPLAY_VISIBLE }),
                        })
                        .collect();
                    if !changes.is_empty() {
                        let label = if *hide { "Hide a field" } else { "Show a field" };
                        self.apply(id, Edit::Batch { label: label.into(), edits: vec![Edit::ApplyScriptChanges { changes }] })?;
                    }
                }
                A::GoTo(p) => out.requests.push(Request::GoToPage(*p)),
                A::Uri(u) => out.requests.push(Request::LaunchUrl(u.clone())),
                A::Submit(u) => out.requests.push(Request::Submit(u.clone())),
                A::Named(n) if n == "Print" => out.requests.push(Request::Print),
                A::Named(n) if pdfcraft_forms::MENU_ITEMS.contains(&n.as_str()) => out.menu.push(n.clone()),
                A::Named(_) | A::Other(_) => {
                    out.skipped.push(a.describe());
                    continue;
                }
            }
            out.ran.push(a.describe());
        }
        Ok(out)
    }
}

/// What a keystroke in a text field did ([`Session::field_keystroke`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeystrokeOutcome {
    /// `event.rc`: `false` refuses the keystroke (the field keeps its text).
    pub rc: bool,
    /// The text to insert instead of the one typed (`event.change` after the script).
    pub change: String,
    /// What ran: "script" (the field's Keystroke JavaScript), "format" (its AF format,
    /// natively) or "none".
    pub checked_by: &'static str,
    pub alerts: Vec<String>,
    pub console: Vec<String>,
    pub errors: Vec<String>,
}

/// The longest text a keystroke check looks at (a paste longer than this is refused).
pub const MAX_KEYSTROKE_TEXT: usize = 64 * 1024;

impl Session {
    /// The Keystroke event while typing in text field `field` (`event.willCommit` false): the
    /// field holds `value`, and `change` replaces its characters `sel` (a char range, clamped).
    /// The field's Keystroke JavaScript runs when it has one and JavaScript is on (bounded like
    /// every script, and only its `rc` and `change` count: other fields are not changed while
    /// typing); otherwise its AF format decides natively ([`pdfcraft_forms::af::keystroke_partial`]).
    /// A field with neither accepts everything. Nothing in the document changes.
    pub fn field_keystroke(
        &self,
        id: DocId,
        field: &str,
        value: &str,
        change: &str,
        sel: std::ops::Range<usize>,
    ) -> Result<KeystrokeOutcome, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let f = doc.form.iter().find(|f| f.name == field).ok_or_else(|| EditError::Invalid(format!("no field named {field:?}")))?;
        if value.len().saturating_add(change.len()) > MAX_KEYSTROKE_TEXT {
            return Ok(KeystrokeOutcome { rc: false, change: String::new(), checked_by: "none", ..Default::default() });
        }
        let len = value.chars().count();
        let (start, end) = (sel.start.min(len), sel.end.clamp(sel.start.min(len), len));
        let proposed: String = value.chars().take(start).chain(change.chars()).chain(value.chars().skip(end)).collect();
        if let Some(script) = f.actions.scripts.keystroke.as_ref().filter(|_| !self.js_off)
            && let Some(cos) = doc.editor.as_ref().map(|e| &e.cos)
        {
            let env = ScriptEnv::new(doc, cos);
            let mut ev = pdfcraft_js::Event::field("Keystroke", field, value);
            ev.change = change.to_string();
            ev.will_commit = false;
            // selStart/selEnd count UTF-16 units, as JavaScript does.
            let units = |n: usize| value.chars().take(n).map(char::len_utf16).sum::<usize>();
            (ev.sel_start, ev.sel_end) = (units(start), units(end));
            let o = pdfcraft_js::run(script, &ev, &env.runner.doc, &env.states, &env.runner.doc_scripts, Limits::default());
            let failed = o.error.is_some();
            return Ok(KeystrokeOutcome {
                // A failing script leaves the keystroke alone, as on commit.
                rc: o.rc || failed,
                change: if failed { change.to_string() } else { o.change.clone() },
                checked_by: "script",
                alerts: o.alerts,
                console: o.console,
                errors: o.error.into_iter().collect(),
            });
        }
        let format = &f.actions.format;
        let checked_by = if *format == pdfcraft_forms::af::Format::None { "none" } else { "format" };
        Ok(KeystrokeOutcome {
            rc: pdfcraft_forms::af::keystroke_partial(format, &proposed),
            change: change.to_string(),
            checked_by,
            ..Default::default()
        })
    }
}

/// What a script run needs from the document: the object model state and the form.
pub(crate) struct ScriptEnv {
    pub(crate) runner: JsRunner,
    pub(crate) fields: Vec<pdfcraft_forms::Field>,
    pub(crate) states: Vec<pdfcraft_js::FieldState>,
}

impl ScriptEnv {
    pub(crate) fn new(doc: &crate::Document, cos: &pdfcraft_cos::Document) -> ScriptEnv {
        let mut runner = JsRunner::new(cos, &doc.name);
        runner.doc.annots = annot_states(cos);
        runner.doc.layers = doc.info.layers.iter().map(|l| pdfcraft_js::LayerState { name: l.name.clone(), on: l.visible }).collect();
        let fields = pdfcraft_forms::fields(cos);
        let states: Vec<_> = fields.iter().map(|f| field_state(cos, f)).collect();
        ScriptEnv { runner, fields, states }
    }

    /// The event for a button's script (`target`), or without one a document event named
    /// `event`, or with `scope` = (`event.type`, page) an action's event on that page.
    pub(crate) fn event(&mut self, target: Option<&str>, event: &str, scope: Option<(&str, usize)>) -> pdfcraft_js::Event {
        match (target, scope) {
            (Some(t), _) => pdfcraft_js::Event { will_commit: false, ..pdfcraft_js::Event::field(event, t, "") },
            (None, Some((kind, page))) => {
                self.runner.doc.page = page.min(self.runner.doc.num_pages.saturating_sub(1));
                pdfcraft_js::Event { kind: kind.to_string(), ..pdfcraft_js::Event::doc(event) }
            }
            (None, None) => pdfcraft_js::Event::doc(event),
        }
    }
}

/// Where a JavaScript action lives ([`Session::run_action_javascript`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionSource {
    /// A link: its 0-based page and index in the page's `/Annots`.
    Link { page: usize, index: usize },
    /// A bookmark, by its 0-based path.
    Bookmark { path: Vec<usize> },
    /// A page's Page Open or Page Close trigger.
    Page { page: usize, trigger: pdfcraft_forms::PageTrigger },
}

impl ActionSource {
    /// (`event.type`, `event.name`, the page the script starts on).
    pub(crate) fn event(&self) -> (&'static str, &'static str, usize) {
        match self {
            ActionSource::Link { page, .. } => ("Link", "Mouse Up", *page),
            ActionSource::Bookmark { .. } => ("Bookmark", "Mouse Up", 0),
            ActionSource::Page { page, trigger } => ("Page", trigger.event_name(), *page),
        }
    }
}

/// What firing a field trigger did ([`Session::run_field_trigger`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TriggerOutcome {
    /// The actions that ran, in order (as the Actions tab describes them).
    pub ran: Vec<String>,
    /// Actions not run: types PdfCraft doesn't run, menu items off the safe list, and scripts
    /// while JavaScript is turned off.
    pub skipped: Vec<String>,
    pub alerts: Vec<String>,
    pub console: Vec<String>,
    /// Script errors.
    pub errors: Vec<String>,
    /// What the actions and their scripts ask the viewer for (pages, links, print, submit…).
    pub requests: Vec<Request>,
    /// Menu items for the viewer to run (NextPage, PrevPage, FirstPage, LastPage).
    pub menu: Vec<String>,
}

/// A page's words for form-field detection, in user space (underscore runs split from the
/// text around them).
fn detection_words(text: &pdfcraft_render::PageText, info: &pdfcraft_render::PageInfo) -> Vec<pdfcraft_forms::detect::Word> {
    page_words(text, info).into_iter().map(|(text, rect)| pdfcraft_forms::detect::Word { text, rect }).collect()
}

/// A page's words in reading order, in user space ([x0, y0, x1, y1]); runs of underscores are
/// words of their own.
pub(crate) fn page_words(text: &pdfcraft_render::PageText, info: &pdfcraft_render::PageInfo) -> Vec<(String, [f64; 4])> {
    let mut words: Vec<(String, [f32; 4])> = Vec::new();
    let mut last_line = u32::MAX;
    // A space glyph ends the word before it.
    let mut gap = false;
    for (i, g) in text.glyphs.iter().enumerate() {
        let line = text.line_of.get(i).copied().unwrap_or(0);
        if g.text.trim().is_empty() {
            gap = true;
            continue;
        }
        let under = g.text.chars().all(|c| c == '_');
        let breaks = match words.last() {
            None => true,
            Some((w, _)) => gap || line != last_line || text.space_before.get(i).copied().unwrap_or(false) || w.chars().all(|c| c == '_') != under,
        };
        gap = false;
        last_line = line;
        match words.last_mut() {
            Some((w, r)) if !breaks => {
                w.push_str(&g.text);
                r[0] = r[0].min(g.rect[0]);
                r[1] = r[1].min(g.rect[1]);
                r[2] = r[2].max(g.rect[2]);
                r[3] = r[3].max(g.rect[3]);
            }
            _ => words.push((g.text.clone(), g.rect)),
        }
    }
    words
        .into_iter()
        .map(|(text, r)| {
            let a = info.view_to_user(r[0], r[1]);
            let b = info.view_to_user(r[2], r[3]);
            let rect = [a[0].min(b[0]) as f64, a[1].min(b[1]) as f64, a[0].max(b[0]) as f64, a[1].max(b[1]) as f64];
            (text, rect)
        })
        .collect()
}

impl Session {
    /// Prepare a form ▸ automatic field detection: the fields `pages` (0-based; empty = all)
    /// seem to ask for, named from their labels, as (page, candidate). A document that does not
    /// allow copying text (ISO 32000-2 Table 22 bit 5, see `Document::allows_copying`) gets
    /// the same fields with generic names: label words are page text, and a field name is
    /// returned to tools and stored in the file.
    pub fn detect_fields(&self, id: DocId, pages: &[usize]) -> Vec<(usize, pdfcraft_forms::detect::Candidate)> {
        let Some(doc) = self.get(id) else { return Vec::new() };
        let copy = doc.allows_copying();
        let Some(cos) = doc.editor.as_ref().map(|e| &e.cos) else { return Vec::new() };
        let config = pdfcraft_render::RenderConfig { password: doc.password.as_deref().map(std::sync::Arc::from), ..Default::default() };
        let mut r = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), config);
        let mut taken: Vec<String> = doc.form.iter().map(|f| f.name.clone()).collect();
        let all = doc.info.pages.len();
        let list: Vec<usize> = if pages.is_empty() { (0..all).collect() } else { pages.iter().copied().filter(|p| *p < all).collect() };
        let mut out = Vec::new();
        for page in list {
            let res = r.render(pdfcraft_render::RenderRequest { page, kind: pdfcraft_render::RequestKind::Text, scale: 1.0, ..Default::default() });
            let mut words = res.text.map(|t| detection_words(&t, &doc.info.pages[page])).unwrap_or_default();
            if !copy {
                // Labels still place and veto fields (geometry), but their words never become
                // field names: names are generic (Text1, Check Box1, …).
                for w in words.iter_mut().filter(|w| !w.text.chars().all(|c| c == '_')) {
                    w.text.clear();
                }
            }
            let shapes = pdfcraft_forms::detect::page_shapes(cos, page);
            let existing: Vec<[f64; 4]> = doc.form.iter().flat_map(|f| f.widgets.iter()).filter(|w| w.page == Some(page)).map(|w| w.rect).collect();
            for c in pdfcraft_forms::detect::detect(&words, &shapes, &existing, &taken) {
                taken.push(c.name.clone());
                out.push((page, c));
            }
        }
        out
    }

    /// Detect fields and add them, as one undoable step. Returns the new fields' names.
    pub fn auto_detect_fields(&mut self, id: DocId, pages: &[usize]) -> Result<Vec<String>, EditError> {
        let found = self.detect_fields(id, pages);
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let mut names: Vec<String> = Vec::new();
        let mut edits = Vec::new();
        for (page, c) in found {
            use pdfcraft_forms::detect::Kind;
            if !names.contains(&c.name) {
                names.push(c.name.clone());
            }
            match c.kind {
                Kind::Text => edits.push(Edit::AddField {
                    page,
                    rect: c.rect,
                    kind: pdfcraft_forms::NewField::Text { multiline: c.rect[3] - c.rect[1] > 30.0 },
                    name: Some(c.name),
                }),
                Kind::CheckBox => edits.push(Edit::AddField { page, rect: c.rect, kind: pdfcraft_forms::NewField::CheckBox, name: Some(c.name) }),
                // One button of a group: the first creates the group, the others join it.
                Kind::Radio { export, .. } => edits.push(Edit::AddField {
                    page,
                    rect: c.rect,
                    kind: pdfcraft_forms::NewField::Radio { group: Some(c.name), export },
                    name: None,
                }),
                // A comb: a text field with one character per cell.
                Kind::Comb { cells } => {
                    edits.push(Edit::AddField {
                        page,
                        rect: c.rect,
                        kind: pdfcraft_forms::NewField::Text { multiline: false },
                        name: Some(c.name.clone()),
                    });
                    let props = pdfcraft_forms::FieldProps {
                        max_len: Some(Some(cells)),
                        flags: vec![(pdfcraft_forms::flags::COMB, true)],
                        ..Default::default()
                    };
                    edits.push(Edit::SetFieldProps { name: c.name, props: Box::new(props) });
                }
            }
        }
        self.apply(id, Edit::Batch { label: "Detect form fields".into(), edits })?;
        Ok(names)
    }
}
