//! Acrobat JavaScript in the shell: what scripts ask for (alerts, printing, navigation, links),
//! the JavaScript console (⌘J), Document JavaScripts, and Preferences ▸ JavaScript.

use egui::{Align, Layout};
use pdfcraft_engine::js::{ActionSource, Answer, Dialogs, JsOutput, Question, Request};
use pdfcraft_engine::{DocId, Edit, PageTrigger};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// The JavaScript console: the input and the output so far.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsConsole {
    pub input: String,
    pub log: Vec<String>,
}

/// A script stopped on a dialog: what to run again once the user answers.
#[derive(Clone, Debug, PartialEq)]
pub struct JsQuestion {
    pub doc: DocId,
    pub script: String,
    /// The button the script belongs to (`None`: the console, or `source`).
    pub target: Option<String>,
    /// The link, bookmark or page action the script is (run again from the document).
    pub source: Option<ActionSource>,
    pub answers: Vec<Answer>,
    pub question: Question,
    /// What is typed for `app.response`.
    pub text: String,
    /// The dialog to go back to afterwards (the console).
    pub back: Option<crate::Dialog>,
}

/// The answer when a script's dialog is dismissed: Cancel, or No when there is no Cancel.
pub fn cancel_answer(q: &Question) -> Answer {
    match q {
        Question::Response { .. } => Answer::Text(None),
        Question::Alert { .. } => Answer::Button(if q.buttons().contains(&2) { 2 } else { 3 }),
    }
}

/// Document JavaScripts: the script being edited.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocJsDraft {
    pub name: String,
    pub script: String,
}

impl PdfCraftApp {
    /// Act on what scripts produced in document `id`: alerts are shown, console output goes to
    /// the console, print and page requests are carried out, links and form submissions wait
    /// for the user's permission.
    pub fn handle_js(&mut self, id: DocId, out: JsOutput) {
        if out.is_empty() {
            return;
        }
        self.js_console.log.extend(out.console.iter().cloned());
        for e in &out.errors {
            self.js_console.log.push(format!("Error: {e}"));
        }
        let view = self.views.iter().position(|v| v.id == id);
        for r in out.requests {
            match r {
                // The engine has shown or hidden the layer already.
                Request::SetLayer { .. } => {
                    if let Some(i) = view {
                        self.views[i].invalidate_content();
                    }
                }
                Request::Print => self.open_print(),
                Request::GoToPage(p) => {
                    if let Some(i) = view {
                        self.views[i].go_to_page(p);
                    }
                }
                Request::LaunchUrl(u) => self.request_document_url(&u, crate::LinkOrigin::Script),
                Request::Submit(u) => self.start_submit(id, "", pdfcraft_engine::submit::Submission::to_url(&u)),
                Request::SaveAs => self.run_command("file.save_as"),
                Request::Focus(_) | Request::Beep | Request::Reset(_) => {}
            }
        }
        if let Some(a) = out.alerts.last() {
            self.notify(a.clone());
        }
    }

    /// Run a push button's JavaScript (its Mouse Up action).
    pub fn run_button_script(&mut self, id: DocId, field: &str, script: &str) {
        // A script reads the fields: include what's still being typed in one (#166).
        if !self.commit_form_typing() {
            return;
        }
        self.run_script(id, script, Some(field), Vec::new());
    }

    /// Run `script` (a button's, or the console's when `target` is `None`) with the answers its
    /// dialogs have had so far; a new question opens its dialog and waits.
    fn run_script(&mut self, id: DocId, script: &str, target: Option<&str>, answers: Vec<Answer>) {
        self.run_script_from(id, script, target, None, answers);
    }

    /// Run a link's, bookmark's or page trigger's Run-a-JavaScript action (a click on it); its
    /// dialogs wait for the user like a button's.
    pub fn run_action_script(&mut self, id: DocId, source: ActionSource) {
        if !self.session.javascript() {
            self.notify_tr("JavaScript is turned off (Preferences ▸ JavaScript).");
            return;
        }
        // A script reads the fields: include what's still being typed in one (#166).
        if !self.commit_form_typing() {
            return;
        }
        self.run_script_from(id, "", None, Some(source), Vec::new());
    }

    /// [`Self::run_script`], or with `source` that action's script as the document holds it.
    fn run_script_from(&mut self, id: DocId, script: &str, target: Option<&str>, source: Option<ActionSource>, answers: Vec<Answer>) {
        let dialogs = Dialogs { interactive: true, answers };
        let result = match &source {
            Some(src) => self.session.run_action_javascript(id, src, &dialogs),
            None => self.session.run_javascript_with(id, script, target, &dialogs),
        };
        match result {
            Ok(o) => {
                if let Some(question) = o.question {
                    let text = match &question {
                        Question::Response { default, .. } => default.clone(),
                        Question::Alert { .. } => String::new(),
                    };
                    let back = self.dialog.filter(|d| *d != crate::Dialog::JsQuestion);
                    self.js_question = Some(JsQuestion {
                        doc: id,
                        script: script.to_string(),
                        target: target.map(str::to_string),
                        source: source.clone(),
                        answers: dialogs.answers,
                        question,
                        text,
                        back,
                    });
                    self.dialog = Some(crate::Dialog::JsQuestion);
                    return;
                }
                if let Some(i) = self.views.iter().position(|v| v.id == id)
                    && let Some(info) = self.session.get(id).map(|d| d.info.clone())
                {
                    self.views[i].document_changed(&info);
                }
                let console = target.is_none() && source.is_none();
                let (error, result) = (o.error.clone(), o.result.clone());
                let errors = if console { Vec::new() } else { o.error.into_iter().collect() };
                self.handle_js(id, JsOutput { alerts: o.alerts, console: o.console, requests: o.requests, errors });
                if console {
                    match error {
                        Some(e) => self.js_console.log.push(format!("Error: {e}")),
                        None => self.js_console.log.extend(result),
                    }
                }
            }
            Err(e) => match target {
                Some(field) => self.notify_fmt("{field}: {e}", &[("field", field), ("e", &e.to_string())]),
                None if source.is_some() => self.notify_error(e),
                None => self.js_console.log.push(format!("Error: {e}")),
            },
        }
    }

    /// The user answered the dialog a script is waiting on: go back to where it was asked
    /// from and run the script again with the answer.
    pub fn answer_js_question(&mut self, answer: Answer) {
        let Some(mut q) = self.js_question.take() else { return };
        self.dialog = q.back;
        q.answers.push(answer);
        if self.session.get(q.doc).is_some() {
            self.run_script_from(q.doc, &q.script, q.target.as_deref(), q.source, q.answers);
        }
    }

    /// Prepare a form ▸ detect fields from the page's blanks, lines and boxes.
    pub fn detect_fields(&mut self) {
        let Some((i, id)) = self.active_ids() else { return };
        match self.session.auto_detect_fields(id, &[]) {
            Ok(names) if names.is_empty() => self.notify_tr("No form fields were detected"),
            Ok(names) => {
                if let Some(info) = self.session.get(id).map(|d| d.info.clone()) {
                    self.views[i].document_changed(&info);
                }
                if names.len() == 1 {
                    self.notify_tr("Detected 1 form field");
                } else {
                    self.notify_fmt("Detected {n} form fields", &[("n", &names.len().to_string())]);
                }
            }
            Err(e) => self.notify_error(e),
        }
    }

    /// The console's Run: evaluate the input in the active document.
    pub fn run_console(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let script = self.js_console.input.clone();
        if script.trim().is_empty() {
            return;
        }
        self.js_console.log.push(format!("> {}", script.trim()));
        self.run_script(id, &script, None, Vec::new());
    }
}

fn buttons(ui: &mut egui::Ui, primary: &str, others: &[&str]) -> Option<String> {
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Stable ids: the console's output above changes how many widgets come first.
            // Labels are translated for display; the returned id stays English.
            if ui.push_id(primary, |ui| widgets::pill_button(ui, tl!(primary), true)).inner.clicked() {
                clicked = Some(primary.to_string());
            }
            for o in others {
                if ui.push_id(o, |ui| widgets::pill_button(ui, tl!(o), false)).inner.clicked() {
                    clicked = Some(o.to_string());
                }
            }
        })
    });
    clicked
}

/// A script's dialog: its message and buttons, or a line to type for `app.response`. Returns
/// the answer once one is given.
pub(crate) fn question_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> Option<Answer> {
    let Some(q) = app.js_question.as_mut() else { return Some(Answer::Text(None)) };
    let (title, message) = match &q.question {
        Question::Alert { title, message, .. } | Question::Response { title, question: message, .. } => (title.clone(), message.clone()),
    };
    // Scripts name the dialog; one that doesn't is labelled for what asked.
    let title = if title.trim().is_empty() { tl!("Message from the document").to_string() } else { title };
    ui.label(egui::RichText::new(title).font(theme::semibold(18.0)));
    ui.label(egui::RichText::new(tl!("Asked by a script in this PDF")).small().color(t.text_muted));
    ui.add_space(8.0);
    ui.label(message);
    ui.add_space(10.0);
    let mut answer = None;
    match q.question.clone() {
        Question::Response { label, password, .. } => {
            let mut submit = false;
            ui.horizontal(|ui| {
                if !label.is_empty() {
                    ui.label(&label);
                }
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut q.text)
                        .password(password)
                        .desired_width(ui.available_width())
                        .id_salt("js-response")
                        .char_limit(4096),
                );
                // Ready to type into when it opens, without holding focus against the buttons.
                if ui.memory(|m| m.focused().is_none()) {
                    edit.request_focus();
                }
                submit = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
            ui.add_space(10.0);
            match buttons(ui, "OK", &["Cancel"]).as_deref() {
                Some("OK") => answer = Some(Answer::Text(Some(q.text.clone()))),
                Some("Cancel") => answer = Some(Answer::Text(None)),
                _ if submit => answer = Some(Answer::Text(Some(q.text.clone()))),
                _ => {}
            }
        }
        Question::Alert { .. } => {
            let names: Vec<(&str, i32)> = q
                .question
                .buttons()
                .iter()
                .map(|b| match b {
                    1 => ("OK", 1),
                    2 => ("Cancel", 2),
                    3 => ("No", 3),
                    _ => ("Yes", 4),
                })
                .collect();
            // The first button (OK or Yes) is the primary one; the rest follow it.
            if let Some(((primary, _), rest)) = names.split_first() {
                let others: Vec<&str> = rest.iter().rev().map(|(n, _)| *n).collect();
                if let Some(clicked) = buttons(ui, primary, &others) {
                    answer = names.iter().find(|(n, _)| *n == clicked).map(|(_, b)| Answer::Button(*b));
                }
            }
        }
    }
    answer
}

/// The JavaScript console. Returns `true` to close.
pub(crate) fn console_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    ui.label(egui::RichText::new(tl!("JavaScript Console")).font(theme::semibold(18.0)));
    ui.add_space(6.0);
    if !app.session.javascript() {
        ui.label(egui::RichText::new(tl!("JavaScript is turned off (Preferences ▸ JavaScript).")).small().color(t.text_muted));
    }
    egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().max_height(220.0).stick_to_bottom(true).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(160.0);
            if app.js_console.log.is_empty() {
                ui.label(egui::RichText::new(tl!("Output appears here.")).color(t.text_muted));
            }
            for line in &app.js_console.log {
                ui.label(egui::RichText::new(line).monospace());
            }
        });
    });
    ui.add_space(6.0);
    let input = ui.add(
        egui::TextEdit::multiline(&mut app.js_console.input)
            .code_editor()
            .desired_rows(4)
            .desired_width(f32::INFINITY)
            .hint_text(tl!("JavaScript, e.g. getField(\"total\").value"))
            .id_salt("js-console-input"),
    );
    let run_key = input.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
    ui.add_space(8.0);
    match buttons(ui, "Run", &["Close", "Clear"]).as_deref() {
        Some("Run") => app.run_console(),
        Some("Clear") => app.js_console.log.clear(),
        Some("Close") => return true,
        _ if run_key => app.run_console(),
        _ => {}
    }
    false
}

/// Document JavaScripts: list, edit, add and delete. Returns `true` to close.
pub(crate) fn document_js_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    ui.label(egui::RichText::new(tl!("Document JavaScripts")).font(theme::semibold(18.0)));
    ui.add_space(6.0);
    let scripts = app.active_ids().and_then(|(_, id)| app.session.get(id)).map(|d| d.document_scripts()).unwrap_or_default();
    let mut edit: Option<Edit> = None;
    ui.horizontal(|ui| {
        ui.label(tl!("Script Name:"));
        ui.add(egui::TextEdit::singleline(&mut app.doc_js.name).desired_width(240.0).id_salt("doc-js-name"));
    });
    ui.add_space(4.0);
    egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().max_height(120.0).id_salt("doc-js-list").show(ui, |ui| {
            ui.set_width(ui.available_width());
            if scripts.is_empty() {
                ui.label(egui::RichText::new(tl!("This document has no document-level scripts.")).color(t.text_muted));
            }
            for (name, js) in &scripts {
                if ui.selectable_label(app.doc_js.name == *name, name).clicked() {
                    app.doc_js = DocJsDraft { name: name.clone(), script: js.clone() };
                }
            }
        });
    });
    ui.add_space(6.0);
    ui.add(egui::TextEdit::multiline(&mut app.doc_js.script).code_editor().desired_rows(8).desired_width(f32::INFINITY).id_salt("doc-js-script"));
    ui.add_space(8.0);
    let name = app.doc_js.name.trim().to_string();
    let close = match buttons(ui, "Save", &["Close", "Delete"]).as_deref() {
        Some("Save") if !name.is_empty() => {
            edit = Some(Edit::SetDocumentScript { name, script: Some(app.doc_js.script.clone()) });
            false
        }
        Some("Delete") if scripts.iter().any(|(n, _)| *n == name) => {
            edit = Some(Edit::SetDocumentScript { name, script: None });
            app.doc_js = DocJsDraft::default();
            false
        }
        Some("Close") => true,
        _ => false,
    };
    if let Some(e) = edit {
        app.apply_edit(e);
    }
    close
}

/// Preferences ▸ Date format: a preset, or any pattern typed in, for Fill & Sign dates, and the
/// language of its month and weekday names. A valid pattern applies as it's typed; an invalid
/// one stays in the box with the reason, and the last valid one stays in use.
pub(crate) fn date_format(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) {
    use pdfcraft_engine::dates::{DATE_FORMATS, DATE_LANGUAGES, MAX_DATE_FORMAT_CHARS};
    ui.label(egui::RichText::new(tl!("Date format")).font(theme::semibold(13.0)));
    let sample = |app: &PdfCraftApp, f: &str| format!("{}   {f}", app.date_text(Some(f)).unwrap_or_default());
    ui.horizontal(|ui| {
        let label = ui.label(tl!("Fill & Sign dates"));
        let current = app.session.date_format().to_string();
        let selected = if DATE_FORMATS.contains(&current.as_str()) { sample(app, &current) } else { tl!("Custom").to_string() };
        egui::ComboBox::from_id_salt("date-format")
            .selected_text(selected)
            .width(240.0)
            .show_ui(ui, |ui| {
                for f in DATE_FORMATS {
                    if ui.selectable_label(current == f, sample(app, f)).clicked() && app.session.set_date_format(f).is_ok() {
                        app.date_format_draft = None;
                        ui.close();
                    }
                }
            })
            .response
            .labelled_by(label.id);
    });
    ui.horizontal(|ui| {
        let label = ui.label(tl!("Pattern"));
        let mut text = app.date_format_draft.clone().unwrap_or_else(|| app.session.date_format().to_string());
        let edit = ui.add(egui::TextEdit::singleline(&mut text).desired_width(160.0).char_limit(MAX_DATE_FORMAT_CHARS)).labelled_by(label.id);
        // The box keeps the text as typed (a trailing space included) while it has focus; the
        // setting takes the trimmed pattern whenever it's valid.
        if edit.changed() {
            let _ = app.session.set_date_format(&text);
            app.date_format_draft = Some(text.clone());
        }
        if !edit.has_focus() && pdfcraft_engine::dates::check_date_format(&text).is_ok() {
            app.date_format_draft = None;
        }
        match app.date_text(Some(&text)) {
            Ok(today) => ui.label(egui::RichText::new(crate::i18n::fmt(tl!("Today: {date}"), &[("date", &today)])).color(t.text_muted)),
            // The engine's reason, in English.
            Err(e) => ui.label(egui::RichText::new(e).color(egui::Color32::from_rgb(0xD1, 0x3B, 0x3B))),
        };
    });
    // Fill & Sign refuses a date it would save as "?"; say so before anyone tries.
    let bad = app.date_text(None).map(|today| pdfcraft_engine::dates::unwritable(&today)).unwrap_or_default();
    if !bad.is_empty() {
        ui.label(
            egui::RichText::new(crate::i18n::fmt(
                tl!(
                    "{chars} can't be written into the PDF yet: Fill & Sign text is Western European only. Pick a numeric format such as dd.mm.yyyy."
                ),
                &[("chars", &bad)],
            ))
            .color(egui::Color32::from_rgb(0xD1, 0x3B, 0x3B)),
        );
    }
    ui.horizontal(|ui| {
        let label = ui.label(tl!("Language"));
        let interface = crate::i18n::current().name();
        let follow = crate::i18n::fmt(tl!("Same as interface ({language})"), &[("language", interface)]);
        let current = app.session.date_language().map(str::to_string);
        let selected = DATE_LANGUAGES.iter().find(|l| current.as_deref() == Some(l.code)).map_or(follow.clone(), |l| l.name.to_string());
        egui::ComboBox::from_id_salt("date-language")
            .selected_text(selected)
            .width(240.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(current.is_none(), follow).clicked() && app.session.set_date_language(None).is_ok() {
                    ui.close();
                }
                for l in &DATE_LANGUAGES {
                    if ui.selectable_label(current.as_deref() == Some(l.code), l.name).clicked()
                        && app.session.set_date_language(Some(l.code)).is_ok()
                    {
                        ui.close();
                    }
                }
            })
            .response
            .labelled_by(label.id);
    });
    ui.label(
        egui::RichText::new(crate::i18n::fmt(
            tl!("yyyy or yy year · m or mm month · mmm or mmmm month name · d or dd day · ddd or dddd weekday. H, h, M, s and t are time letters and can't be used on their own: put {escape} before a letter to show it as is."),
            &[("escape", "\\")],
        ))
        .small()
        .color(t.text_muted),
    );
}

/// What a Run-a-JavaScript action editor (a bookmark's action, or Page Properties ▸ Actions)
/// is editing.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptTarget {
    Bookmark(Vec<usize>),
    /// A page (0-based): its Page Open and Page Close scripts.
    Page(usize),
}

/// The action editor's working copy. A bookmark has one script; a page has one per trigger
/// ([`PageTrigger::ALL`] order), and `trigger` is the one shown.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionScriptDraft {
    pub target: ScriptTarget,
    pub trigger: PageTrigger,
    pub scripts: Vec<String>,
    /// The scripts as the document has them, to save only what changed.
    pub original: Vec<String>,
}

impl ActionScriptDraft {
    fn slot(&self) -> usize {
        match self.target {
            ScriptTarget::Bookmark(_) => 0,
            ScriptTarget::Page(_) => PageTrigger::ALL.iter().position(|t| *t == self.trigger).unwrap_or(0),
        }
    }

    /// The edit that saves the draft (`None`: nothing changed, or nothing to save).
    pub fn edit(&self) -> Option<Edit> {
        match &self.target {
            ScriptTarget::Bookmark(path) => {
                let js = self.scripts.first()?;
                (js.trim() != self.original.first().map_or("", |s| s.trim()) && !js.trim().is_empty())
                    .then(|| Edit::SetBookmarkAction { path: path.clone(), action: pdfcraft_engine::BookmarkAction::JavaScript(js.clone()) })
            }
            ScriptTarget::Page(page) => {
                let edits: Vec<Edit> = PageTrigger::ALL
                    .iter()
                    .zip(self.scripts.iter().zip(&self.original))
                    .filter(|(_, (now, was))| now.trim() != was.trim())
                    .map(|(t, (now, _))| Edit::SetPageAction { page: *page, trigger: *t, script: (!now.trim().is_empty()).then(|| now.clone()) })
                    .collect();
                match edits.len() {
                    0 => None,
                    1 => edits.into_iter().next(),
                    _ => Some(Edit::Batch { label: "Set page actions".into(), edits }),
                }
            }
        }
    }
}

impl PdfCraftApp {
    /// Bookmark ▸ Run a JavaScript: edit the script the bookmark runs when clicked.
    pub fn open_bookmark_script(&mut self, path: Vec<usize>) {
        let Some((_, id)) = self.active_ids() else { return };
        let current = match self.session.bookmark_action(id, &path) {
            Ok(pdfcraft_engine::BookmarkAction::JavaScript(js)) => js,
            Ok(_) => String::new(),
            Err(e) => return self.notify_error(e),
        };
        self.action_script = Some(ActionScriptDraft {
            target: ScriptTarget::Bookmark(path),
            trigger: PageTrigger::Open,
            scripts: vec![current.clone()],
            original: vec![current],
        });
        self.dialog = Some(crate::Dialog::ActionScript);
    }

    /// Page Properties ▸ Actions for page `page` (0-based): its Page Open and Page Close scripts.
    pub fn open_page_actions(&mut self, page: usize) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let scripts: Vec<String> = PageTrigger::ALL.iter().map(|t| doc.page_action(page, *t).unwrap_or_default()).collect();
        self.action_script =
            Some(ActionScriptDraft { target: ScriptTarget::Page(page), trigger: PageTrigger::Open, original: scripts.clone(), scripts });
        self.dialog = Some(crate::Dialog::ActionScript);
    }

    /// Run the Page Close script of the page the active view leaves and the Page Open script
    /// of the one it arrives at. Cheap per frame: nothing happens until the current page
    /// changes, and only pages the open pass found with additional actions are looked at.
    pub(crate) fn run_page_actions(&mut self) {
        let Some((i, id)) = self.active_ids() else { return };
        let Some(view) = self.views.get_mut(i) else { return };
        let now = view.current;
        let before = view.script_page.replace(now);
        if before == Some(now) || !self.session.javascript() || self.dialog == Some(crate::Dialog::JsQuestion) {
            return;
        }
        let has = |p: usize, s: &Self| s.session.get(id).is_some_and(|d| d.info.page_actions.binary_search(&p).is_ok());
        let mut ran = false;
        for (page, trigger) in [(before, PageTrigger::Close), (Some(now), PageTrigger::Open)] {
            let Some(page) = page.filter(|p| has(*p, self)) else { continue };
            match self.session.run_page_action(id, page, trigger) {
                Ok(r) => ran |= r,
                Err(e) => self.notify_error(e),
            }
        }
        if ran
            && let Some(doc) = self.session.get(id)
            && let Some(view) = self.views.iter_mut().find(|v| v.id == id)
        {
            view.document_changed(&doc.info);
        }
    }
}

/// The Run-a-JavaScript action editor. Returns `true` to close.
pub(crate) fn action_script_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let Some(d) = app.action_script.as_mut() else { return true };
    let (title, hint) = match &d.target {
        ScriptTarget::Bookmark(_) => (tl!("Bookmark Action").to_string(), tl!("Runs when the bookmark is clicked.").to_string()),
        ScriptTarget::Page(p) => (
            crate::i18n::fmt(tl!("Page Actions: page {n}"), &[("n", &p.saturating_add(1).to_string())]),
            tl!("Page Open runs when the page becomes the current page; Page Close when another page does.").to_string(),
        ),
    };
    ui.label(egui::RichText::new(title).font(theme::semibold(18.0)));
    ui.label(egui::RichText::new(hint).small().color(t.text_muted));
    if !app.session.javascript() {
        ui.label(egui::RichText::new(tl!("JavaScript is turned off (Preferences ▸ JavaScript).")).small().color(t.text_muted));
    }
    ui.add_space(8.0);
    if matches!(d.target, ScriptTarget::Page(_)) {
        ui.horizontal(|ui| {
            ui.label(tl!("Trigger:"));
            for tr in PageTrigger::ALL {
                let set = !d.scripts.get(PageTrigger::ALL.iter().position(|x| *x == tr).unwrap_or(0)).is_none_or(|s| s.trim().is_empty());
                let label = if set { format!("{} ●", tl!(tr.label())) } else { tl!(tr.label()).to_string() };
                ui.selectable_value(&mut d.trigger, tr, label);
            }
        });
        ui.add_space(4.0);
    }
    widgets::section_title(ui, tl!("Run a JavaScript"));
    let slot = d.slot();
    if let Some(script) = d.scripts.get_mut(slot) {
        ui.add(
            egui::TextEdit::multiline(script)
                .code_editor()
                .desired_rows(10)
                .desired_width(f32::INFINITY)
                .char_limit(pdfcraft_engine::MAX_ACTION_SCRIPT_BYTES)
                .hint_text(tl!("JavaScript, e.g. app.alert(\"Hello\")"))
                .id_salt(("action-js", slot)),
        );
    }
    ui.add_space(8.0);
    let changed = d.edit().is_some();
    let bookmark = matches!(d.target, ScriptTarget::Bookmark(_));
    let mut close = false;
    let mut save = false;
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.push_id("Save", |ui| ui.add_enabled_ui(changed, |ui| widgets::pill_button(ui, tl!("Save"), true)).inner).inner.clicked() {
                save = true;
            }
            if ui.push_id("Close", |ui| widgets::pill_button(ui, tl!("Close"), false)).inner.clicked() {
                close = true;
            }
            if !bookmark
                && ui.push_id("Remove", |ui| widgets::pill_button(ui, tl!("Remove"), false)).inner.clicked()
                && let Some(s) = d.scripts.get_mut(slot)
            {
                s.clear();
            }
        });
    });
    if save
        && let Some(e) = app.action_script.as_ref().and_then(ActionScriptDraft::edit)
        && app.apply_edit(e)
    {
        app.action_script = None;
        return true;
    }
    if close {
        app.action_script = None;
    }
    close
}
