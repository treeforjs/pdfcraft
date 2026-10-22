//! The JavaScript debugger (Tools → JavaScript debugger…, ⌘⇧J): every script in the document
//! and where it lives, its source with line numbers and breakpoints, Run / Continue / Step,
//! watches and the call stack at a pause, and a console that evaluates in the paused scope.
//!
//! Work happens on demand only: the script list is read when the debugger opens (or on
//! Refresh), the breakable lines of a script when it is selected or its edit is done, and
//! runs when a button is pressed. Drawing the source shows only the rows on screen.

use std::collections::BTreeSet;

use egui::{Align, Color32, CornerRadius, Layout, RichText, Sense, vec2};
use pdfcraft_engine::DocId;
use pdfcraft_engine::js::JsOutput;
use pdfcraft_engine::js_debug::{Debug, DebugOutcome, Pause, Resume, ScriptEntry, ScriptLocation, Step, breakable_lines};
use serde_json::{Value, json};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Most console lines kept.
const MAX_LOG: usize = 2000;
/// Height of a source row.
const ROW: f32 = 18.0;

/// What a console line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogKind {
    /// What the script printed (`console.println`).
    Output,
    /// A completion value or a watch's value.
    Result,
    Error,
    /// What was typed.
    Input,
    /// A note from the debugger.
    Info,
}

/// How the last run went.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum RunStatus {
    #[default]
    Idle,
    Paused(u32),
    Finished,
    /// The error and its line (in the script shown), when known.
    Error(Option<u32>),
    /// Stopped for running too long.
    Stopped,
}

/// The debugger's state.
#[derive(Clone, Debug, Default)]
pub struct JsDebugger {
    pub doc: Option<DocId>,
    pub scripts: Vec<ScriptEntry>,
    pub truncated: bool,
    pub filter: String,
    /// The script shown: an index into `scripts`, or `None` for the scratch script.
    pub selected: Option<usize>,
    /// The source shown and run (a document script's, possibly edited here, or the scratch).
    pub source: String,
    pub editing: bool,
    pub breakpoints: BTreeSet<u32>,
    /// The lines that take a breakpoint (for `source`, unless `breakable_stale`).
    pub breakable: Vec<u32>,
    breakable_stale: bool,
    pub watches: Vec<String>,
    pub new_watch: String,
    pub paused: Option<Pause>,
    pub status: RunStatus,
    pub log: Vec<(LogKind, String)>,
    pub input: String,
    /// Console lines of the current run already shown (each step replays the run).
    shown: usize,
    /// A line to bring into view once.
    scroll_to: Option<u32>,
    scratch: String,
}

impl JsDebugger {
    fn push(&mut self, kind: LogKind, text: impl Into<String>) {
        self.log.push((kind, text.into()));
        if self.log.len() > MAX_LOG {
            let extra = self.log.len() - MAX_LOG;
            self.log.drain(..extra);
        }
    }

    fn entry(&self) -> Option<&ScriptEntry> {
        self.selected.and_then(|i| self.scripts.get(i))
    }

    /// Show script `i` (`None`: the scratch script), with no breakpoints or pause.
    pub fn select(&mut self, i: Option<usize>) {
        if self.selected.is_none() {
            self.scratch = self.source.clone();
        }
        self.selected = i.filter(|i| *i < self.scripts.len());
        self.source = match self.entry() {
            Some(e) => e.script.clone(),
            None => self.scratch.clone(),
        };
        self.editing = self.selected.is_none() && self.source.trim().is_empty();
        self.breakpoints.clear();
        self.paused = None;
        self.status = RunStatus::Idle;
        self.shown = 0;
        self.breakable_stale = true;
    }

    /// Recompute the breakable lines if the source changed.
    fn refresh_breakable(&mut self) {
        if self.breakable_stale && !self.editing {
            self.breakable = breakable_lines(&self.source);
            let ok: BTreeSet<u32> = self.breakable.iter().copied().collect();
            self.breakpoints.retain(|b| ok.contains(b));
            self.breakable_stale = false;
        }
    }

    /// Set or clear a breakpoint on `line` (only lines that take one).
    pub fn toggle_breakpoint(&mut self, line: u32) {
        self.refresh_breakable();
        if !self.breakable.contains(&line) {
            return;
        }
        if !self.breakpoints.remove(&line) {
            self.breakpoints.insert(line);
        }
    }

    /// The control channel's view (`ui.state`).
    pub fn state(&self) -> Value {
        json!({
            "scripts": self.scripts.iter().map(|e| e.location.id()).collect::<Vec<_>>(),
            "truncated": self.truncated,
            "selected": self.entry().map(|e| e.location.id()),
            "breakpoints": self.breakpoints,
            "breakable": self.breakable,
            "watches": self.watches,
            "status": match &self.status {
                RunStatus::Idle => json!("idle"),
                RunStatus::Paused(l) => json!({ "paused": l }),
                RunStatus::Finished => json!("finished"),
                RunStatus::Error(l) => json!({ "error": l }),
                RunStatus::Stopped => json!("stopped"),
            },
            "paused": self.paused.as_ref().map(|p| json!({
                "line": p.line,
                "stack": p.stack.iter().map(|f| json!({ "function": f.function, "line": f.line })).collect::<Vec<_>>(),
                "watches": p.watches.iter().map(|w| json!({ "expression": w.expression, "value": w.value, "error": w.error })).collect::<Vec<_>>(),
            })),
            "log": self.log.iter().rev().take(50).rev().map(|(k, l)| json!({ "kind": format!("{k:?}"), "text": l })).collect::<Vec<_>>(),
        })
    }
}

/// The list's two lines for a script: what it belongs to, and the event.
fn row_label(loc: &ScriptLocation) -> (String, String) {
    let page = |p: &usize| crate::i18n::fmt(tl!("Page {n}"), &[("n", &p.saturating_add(1).to_string())]);
    match loc {
        ScriptLocation::Document { name } => (name.clone(), tl!("Document script").to_string()),
        ScriptLocation::OpenAction => (tl!("Document").to_string(), tl!("Open action").to_string()),
        ScriptLocation::DocumentAction(t) => (tl!("Document").to_string(), tl!(t.label()).to_string()),
        ScriptLocation::Page { page: p, trigger } => (page(p), tl!(trigger.label()).to_string()),
        ScriptLocation::Field { field, event } => {
            let e = match *event {
                "keystroke" => tl!("Keystroke"),
                "format" => tl!("Format"),
                "validate" => tl!("Validate"),
                _ => tl!("Calculate"),
            };
            (field.clone(), e.to_string())
        }
        ScriptLocation::FieldTrigger { field, trigger } => (field.clone(), tl!(trigger.label()).to_string()),
        ScriptLocation::Link { page: p, index } => (crate::i18n::fmt(tl!("Link {n}"), &[("n", &index.saturating_add(1).to_string())]), page(p)),
        ScriptLocation::Bookmark { title, .. } => {
            (if title.trim().is_empty() { tl!("Bookmark").to_string() } else { title.clone() }, tl!("Bookmark").to_string())
        }
        ScriptLocation::Annotation { page: p, subtype, trigger, .. } => (format!("{subtype} · {trigger}"), page(p)),
    }
}

/// The list's group for a script.
fn group(loc: &ScriptLocation) -> &'static str {
    match loc.kind() {
        "document" | "open" | "document_action" => "Document",
        "page" => "Pages",
        "field" | "field_trigger" => "Form fields",
        "link" => "Links",
        "bookmark" => "Bookmarks",
        _ => "Annotations",
    }
}

impl PdfCraftApp {
    /// Tools → JavaScript debugger: read the active document's scripts and open the debugger.
    pub fn open_js_debugger(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let same = self.js_debugger.doc == Some(id);
        let keep = same.then(|| self.js_debugger.entry().map(|e| e.location.id())).flatten();
        self.debugger_refresh();
        if !same {
            self.js_debugger.log.clear();
            self.js_debugger.select(if self.js_debugger.scripts.is_empty() { None } else { Some(0) });
        } else if let Some(k) = keep {
            let i = self.js_debugger.scripts.iter().position(|e| e.location.id() == k);
            if i != self.js_debugger.selected {
                self.js_debugger.select(i);
            }
        }
        self.dialog = Some(crate::Dialog::JsDebugger);
    }

    /// Read the script list again (after the document changed).
    pub fn debugger_refresh(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let d = &mut self.js_debugger;
        d.doc = Some(id);
        match self.session.javascript_index(id) {
            Ok(index) => {
                d.scripts = index.scripts;
                d.truncated = index.truncated;
            }
            Err(e) => {
                d.scripts.clear();
                d.push(LogKind::Error, e.to_string());
            }
        }
        if d.selected.is_some_and(|i| i >= d.scripts.len()) {
            d.select(None);
        }
    }

    /// Select a script by its id (`scratch` for the scratch script).
    pub fn debugger_select_id(&mut self, id: &str) -> Result<(), String> {
        let i = if id == "scratch" {
            None
        } else {
            Some(
                self.js_debugger
                    .scripts
                    .iter()
                    .position(|e| e.location.id() == id)
                    .ok_or_else(|| format!("no script {id:?} in the debugger's list"))?,
            )
        };
        self.js_debugger.select(i);
        Ok(())
    }

    /// Run, Continue or a Step. From a pause, `step` goes on from it; otherwise a run starts
    /// (Step into, over and out then stop on the first line).
    pub fn debugger_run(&mut self, step: Step) {
        let Some((_, id)) = self.active_ids() else { return };
        if !self.session.javascript() {
            self.js_debugger.push(LogKind::Info, tl!("JavaScript is turned off (Preferences → JavaScript)."));
            return;
        }
        let d = &mut self.js_debugger;
        d.editing = false;
        d.refresh_breakable();
        let resume = d.paused.as_ref().map(Pause::resume);
        let step = if resume.is_none() && step != Step::Run { Step::Into } else { step };
        if resume.is_none() {
            d.shown = 0;
        }
        let debug = Debug { breakpoints: d.breakpoints.iter().copied().collect(), watches: d.watches.clone(), step, resume, ..Default::default() };
        let location = d.entry().map(|e| e.location.clone());
        let script = d.source.clone();
        match self.session.debug_javascript(id, &script, location.as_ref(), &debug) {
            Ok(o) => self.debugger_outcome(id, o),
            Err(e) => self.js_debugger.push(LogKind::Error, e.to_string()),
        }
    }

    /// Stop debugging: forget the pause (nothing the paused run did was applied).
    pub fn debugger_stop(&mut self) {
        let d = &mut self.js_debugger;
        if d.paused.take().is_some() {
            d.push(LogKind::Info, tl!("Stopped. Nothing the script did was applied."));
        }
        d.status = RunStatus::Idle;
        d.shown = 0;
    }

    fn debugger_outcome(&mut self, id: DocId, o: DebugOutcome) {
        let d = &mut self.js_debugger;
        d.breakable = o.breakable.clone();
        d.breakable_stale = false;
        for line in o.outcome.console.iter().skip(d.shown) {
            d.push(LogKind::Output, line.clone());
        }
        if let Some(p) = o.paused {
            d.shown = o.outcome.console.len();
            d.status = RunStatus::Paused(p.line);
            d.scroll_to = Some(p.line);
            d.paused = Some(p);
            return;
        }
        d.paused = None;
        d.shown = 0;
        let at = o.outcome.error_at.filter(|e| e.script.is_none()).map(|e| e.line);
        if o.out_of_steps {
            d.status = RunStatus::Stopped;
            d.scroll_to = at;
            d.push(LogKind::Error, o.outcome.error.clone().unwrap_or_default());
            return;
        }
        match &o.outcome.error {
            Some(e) => {
                d.status = RunStatus::Error(at);
                d.scroll_to = at;
                let text = match o.outcome.error_at {
                    Some(a) if a.script.is_none() => crate::i18n::fmt(tl!("{error} (line {n})"), &[("error", e), ("n", &a.line.to_string())]),
                    Some(a) => {
                        let names = self.session.document_script_names(id);
                        let name = a.script.and_then(|i| names.get(i).cloned()).unwrap_or_default();
                        crate::i18n::fmt(
                            tl!("{error} (document script {name}, line {n})"),
                            &[("error", e), ("name", &name), ("n", &a.line.to_string())],
                        )
                    }
                    None => e.clone(),
                };
                d.push(LogKind::Error, text);
            }
            None => {
                d.status = RunStatus::Finished;
                if let Some(r) = &o.outcome.result {
                    d.push(LogKind::Result, r.clone());
                }
            }
        }
        // A finished run applied its changes: show them, and do what it asked for.
        if let Some(i) = self.views.iter().position(|v| v.id == id)
            && let Some(info) = self.session.get(id).map(|doc| doc.info.clone())
        {
            self.views[i].document_changed(&info);
        }
        let out = o.outcome;
        self.handle_js(id, JsOutput { alerts: out.alerts, console: Vec::new(), requests: out.requests, errors: Vec::new() });
    }

    /// The console's Evaluate: at a pause, the expression's value in the paused scope; otherwise
    /// run it against the document, as the JavaScript console does.
    pub fn debugger_eval(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let expr = self.js_debugger.input.trim().to_string();
        if expr.is_empty() {
            return;
        }
        self.js_debugger.push(LogKind::Input, format!("> {expr}"));
        self.js_debugger.input.clear();
        if !self.session.javascript() {
            self.js_debugger.push(LogKind::Info, tl!("JavaScript is turned off (Preferences → JavaScript)."));
            return;
        }
        if let Some(p) = self.js_debugger.paused.clone() {
            // Run again to the same stop (the probe just before it, then one step), watching it.
            let d = &self.js_debugger;
            let debug = Debug {
                watches: vec![expr],
                step: Step::Into,
                resume: Some(Resume { hit: p.hit.saturating_sub(1), depth: p.depth }),
                ..Default::default()
            };
            let location = d.entry().map(|e| e.location.clone());
            let script = d.source.clone();
            let r = self.session.debug_javascript(id, &script, location.as_ref(), &debug);
            let d = &mut self.js_debugger;
            match r.map(|o| o.paused.and_then(|q| q.watches.into_iter().next())) {
                Ok(Some(w)) => match (w.value, w.error) {
                    (Some(v), _) => d.push(LogKind::Result, v),
                    (None, e) => d.push(LogKind::Error, e.unwrap_or_default()),
                },
                Ok(None) => d.push(LogKind::Error, tl!("The paused point could not be reached again.")),
                Err(e) => d.push(LogKind::Error, e.to_string()),
            }
            return;
        }
        match self.session.debug_javascript(id, &expr, None, &Debug::default()) {
            Ok(o) => {
                let d = &mut self.js_debugger;
                for line in &o.outcome.console {
                    d.push(LogKind::Output, line.clone());
                }
                match (&o.outcome.error, o.outcome.error_at) {
                    (Some(e), Some(a)) if a.script.is_none() => {
                        d.push(LogKind::Error, crate::i18n::fmt(tl!("{error} (line {n})"), &[("error", e), ("n", &a.line.to_string())]))
                    }
                    (Some(e), _) => d.push(LogKind::Error, e.clone()),
                    (None, _) => {
                        if let Some(r) = &o.outcome.result {
                            d.push(LogKind::Result, r.clone());
                        }
                    }
                }
                if !o.out_of_steps && o.paused.is_none() {
                    if let Some(i) = self.views.iter().position(|v| v.id == id)
                        && let Some(info) = self.session.get(id).map(|doc| doc.info.clone())
                    {
                        self.views[i].document_changed(&info);
                    }
                    let out = o.outcome;
                    self.handle_js(id, JsOutput { alerts: out.alerts, console: Vec::new(), requests: out.requests, errors: Vec::new() });
                }
            }
            Err(e) => self.js_debugger.push(LogKind::Error, e.to_string()),
        }
    }
}

fn section(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.label(RichText::new(text).font(theme::semibold(12.0)).color(t.text_muted));
}

/// A small text button with an id that doesn't move.
fn button(ui: &mut egui::Ui, label: &str, primary: bool, enabled: bool) -> bool {
    ui.push_id(label, |ui| ui.add_enabled_ui(enabled, |ui| widgets::pill_button(ui, tl!(label), primary)).inner).inner.clicked()
}

fn panel<R>(ui: &mut egui::Ui, t: &Tokens, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(t.field)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| add(ui))
        .inner
}

/// The script list. Returns the script clicked, if any (`Some(None)`: the scratch script).
fn script_list(ui: &mut egui::Ui, d: &JsDebugger, t: &Tokens, height: f32) -> Option<Option<usize>> {
    let mut clicked = None;
    egui::ScrollArea::vertical().id_salt("jsdbg-scripts").max_height(height).auto_shrink([false, false]).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if ui.selectable_label(d.selected.is_none(), RichText::new(tl!("Scratch script")).strong()).clicked() {
            clicked = Some(None);
        }
        ui.label(RichText::new(tl!("Run JavaScript against the document")).small().color(t.text_faint));
        let filter = d.filter.trim().to_lowercase();
        let mut last_group = "";
        let mut shown = 0usize;
        for (i, e) in d.scripts.iter().enumerate() {
            let (main, sub) = row_label(&e.location);
            if !filter.is_empty() && !main.to_lowercase().contains(&filter) && !sub.to_lowercase().contains(&filter) {
                continue;
            }
            shown += 1;
            let g = group(&e.location);
            if g != last_group {
                ui.add_space(6.0);
                section(ui, t, tl!(g));
                last_group = g;
            }
            let text = RichText::new(format!("{main}\n{sub} · {}", crate::i18n::fmt(tl!("Lines: {n}"), &[("n", &e.lines().to_string())])));
            let r = ui.push_id(("jsdbg-script", i), |ui| ui.selectable_label(d.selected == Some(i), text)).inner;
            if r.clicked() {
                clicked = Some(Some(i));
            }
        }
        if d.scripts.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new(tl!("This document has no scripts.")).color(t.text_muted));
        } else if shown == 0 {
            ui.label(RichText::new(tl!("No scripts match.")).color(t.text_muted));
        }
        if d.truncated {
            ui.label(RichText::new(tl!("More scripts than can be listed; the list is cut.")).small().color(t.text_muted));
        }
    });
    clicked
}

/// The source with line numbers; a click on a line number toggles its breakpoint.
fn source_view(ui: &mut egui::Ui, d: &mut JsDebugger, t: &Tokens, height: f32) {
    let error_red = Color32::from_rgb(0xD1, 0x3B, 0x3B);
    let paused = d.paused.as_ref().map(|p| p.line);
    let error = match d.status {
        RunStatus::Error(l) => l,
        _ => None,
    };
    let lines: Vec<&str> = d.source.split('\n').collect();
    let breakable: BTreeSet<u32> = d.breakable.iter().copied().collect();
    let mut toggle = None;
    let mut area = egui::ScrollArea::both().id_salt("jsdbg-source").max_height(height).auto_shrink([false, false]);
    if let Some(l) = d.scroll_to.take() {
        area = area.vertical_scroll_offset((l.saturating_sub(4) as f32) * ROW);
    }
    let font = egui::FontId::monospace(12.5);
    area.show_rows(ui, ROW, lines.len().max(1), |ui, rows| {
        for i in rows {
            let n = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
            let text = lines.get(i).copied().unwrap_or("");
            let width = ui.available_width().max(200.0);
            let (rect, _) = ui.allocate_exact_size(vec2(width, ROW), Sense::hover());
            let fill = if paused == Some(n) {
                Some(t.accent_soft)
            } else if error == Some(n) {
                Some(error_red.gamma_multiply(0.18))
            } else {
                None
            };
            if let Some(f) = fill {
                ui.painter().rect_filled(rect, CornerRadius::ZERO, f);
            }
            let gutter = egui::Rect::from_min_size(rect.min, vec2(48.0, ROW));
            let can = breakable.contains(&n);
            let resp = ui.interact(gutter, ui.id().with(("jsdbg-gutter", n)), if can { Sense::click() } else { Sense::hover() });
            let label = crate::i18n::fmt(tl!("Breakpoint on line {n}"), &[("n", &n.to_string())]);
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, can, d.breakpoints.contains(&n), &label));
            if resp.clicked() {
                toggle = Some(n);
            }
            if can && resp.hovered() {
                ui.painter().rect_filled(gutter, CornerRadius::same(3), t.hover);
            }
            if d.breakpoints.contains(&n) {
                ui.painter().circle_filled(gutter.left_center() + vec2(8.0, 0.0), 4.5, error_red);
            } else if can && resp.hovered() {
                ui.painter().circle_stroke(gutter.left_center() + vec2(8.0, 0.0), 4.0, egui::Stroke::new(1.0, error_red));
            }
            let num_color = if can { t.text_muted } else { t.text_faint };
            ui.painter().text(gutter.right_center() - vec2(6.0, 0.0), egui::Align2::RIGHT_CENTER, n.to_string(), font.clone(), num_color);
            if paused == Some(n) {
                ui.painter().text(gutter.left_center() + vec2(1.0, 0.0), egui::Align2::LEFT_CENTER, "→", font.clone(), t.accent);
            }
            let shown: String = text.chars().take(2000).collect::<String>().replace('\t', "    ");
            ui.painter().text(rect.left_center() + vec2(56.0, 0.0), egui::Align2::LEFT_CENTER, shown, font.clone(), t.text);
        }
    });
    if let Some(n) = toggle {
        d.toggle_breakpoint(n);
    }
}

/// The JavaScript debugger. Returns `true` to close.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let screen = ui.ctx().content_rect();
    let width = (screen.width() - 80.0).clamp(640.0, 1120.0);
    let tall = (screen.height() - 300.0).clamp(220.0, 620.0);
    ui.set_width(width);
    let mut close = false;
    let mut action: Option<&str> = None;
    let mut select = None;
    // Header.
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("JavaScript Debugger")).font(theme::semibold(18.0)));
        let n = app.js_debugger.scripts.len();
        ui.label(RichText::new(crate::i18n::fmt(tl!("Scripts: {n}"), &[("n", &n.to_string())])).color(t.text_muted));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if button(ui, "Close", false, true) {
                close = true;
            }
            if button(ui, "Refresh", false, true) {
                action = Some("refresh");
            }
        });
    });
    if !app.session.javascript() {
        ui.label(RichText::new(tl!("JavaScript is turned off (Preferences → JavaScript).")).small().color(t.text_muted));
    }
    ui.add_space(6.0);
    let side = 230.0;
    let center = (width - 2.0 * side - 40.0).max(260.0);
    ui.horizontal_top(|ui| {
        // Scripts.
        ui.vertical(|ui| {
            ui.set_width(side);
            ui.add(
                egui::TextEdit::singleline(&mut app.js_debugger.filter).hint_text(tl!("Filter scripts")).desired_width(side).id_salt("jsdbg-filter"),
            );
            ui.add_space(4.0);
            select = panel(ui, t, |ui| script_list(ui, &app.js_debugger, t, tall - 30.0));
        });
        // Source and controls.
        ui.vertical(|ui| {
            ui.set_width(center);
            let d = &app.js_debugger;
            let paused = d.paused.is_some();
            ui.horizontal_wrapped(|ui| {
                if button(ui, "Run", true, !paused) {
                    action = Some("run");
                }
                if button(ui, "Continue", false, paused) {
                    action = Some("continue");
                }
                if button(ui, "Step into", false, true) {
                    action = Some("into");
                }
                if button(ui, "Step over", false, true) {
                    action = Some("over");
                }
                if button(ui, "Step out", false, paused) {
                    action = Some("out");
                }
                if button(ui, "Stop", false, paused) {
                    action = Some("stop");
                }
                if button(ui, if d.editing { "Done" } else { "Edit" }, false, !paused) {
                    action = Some("edit");
                }
            });
            ui.add_space(4.0);
            let (status, color) = match &d.status {
                RunStatus::Idle => (tl!("Click a line number to set a breakpoint.").to_string(), t.text_muted),
                RunStatus::Paused(l) => (crate::i18n::fmt(tl!("Paused on line {n}"), &[("n", &l.to_string())]), t.accent),
                RunStatus::Finished => (tl!("Finished").to_string(), t.text_muted),
                RunStatus::Error(Some(l)) => {
                    (crate::i18n::fmt(tl!("Error on line {n}"), &[("n", &l.to_string())]), Color32::from_rgb(0xD1, 0x3B, 0x3B))
                }
                RunStatus::Error(None) => (tl!("Error").to_string(), Color32::from_rgb(0xD1, 0x3B, 0x3B)),
                RunStatus::Stopped => (tl!("Stopped: the script ran too long").to_string(), Color32::from_rgb(0xD1, 0x3B, 0x3B)),
            };
            ui.label(RichText::new(status).small().color(color));
            let d = &mut app.js_debugger;
            panel(ui, t, |ui| {
                if d.editing {
                    egui::ScrollArea::vertical().id_salt("jsdbg-edit").max_height(tall - 60.0).show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut d.source)
                                .code_editor()
                                .desired_rows(14)
                                .desired_width(f32::INFINITY)
                                .char_limit(pdfcraft_engine::MAX_ACTION_SCRIPT_BYTES)
                                .hint_text(tl!("JavaScript, e.g. getField(\"total\").value"))
                                .id_salt("jsdbg-source-edit"),
                        );
                    });
                } else {
                    source_view(ui, d, t, tall - 60.0);
                }
            });
            if d.selected.is_some() && d.entry().is_some_and(|e| e.script != d.source) {
                ui.label(RichText::new(tl!("Edited here only: runs use this text; the document keeps its script.")).small().color(t.text_muted));
            }
        });
        // Watches and call stack.
        ui.vertical(|ui| {
            ui.set_width(side);
            section(ui, t, tl!("Watches"));
            let d = &mut app.js_debugger;
            let mut remove = None;
            panel(ui, t, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().id_salt("jsdbg-watches").max_height(tall * 0.45).show(ui, |ui| {
                    if d.watches.is_empty() {
                        ui.label(RichText::new(tl!("Watch values show here when the script pauses.")).small().color(t.text_faint));
                    }
                    for (i, w) in d.watches.iter().enumerate() {
                        let value = d.paused.as_ref().and_then(|p| p.watches.iter().find(|x| x.expression == *w));
                        ui.horizontal(|ui| {
                            if ui.push_id(("jsdbg-unwatch", i), |ui| ui.small_button("×")).inner.on_hover_text(tl!("Remove")).clicked() {
                                remove = Some(i);
                            }
                            ui.label(RichText::new(w).monospace());
                        });
                        let (text, color) = match value {
                            Some(x) if x.value.is_some() => (x.value.clone().unwrap_or_default(), t.text),
                            Some(x) => (x.error.clone().unwrap_or_default(), Color32::from_rgb(0xD1, 0x3B, 0x3B)),
                            None => ("—".to_string(), t.text_faint),
                        };
                        ui.label(RichText::new(text).monospace().color(color));
                    }
                });
                let add = ui.add(
                    egui::TextEdit::singleline(&mut d.new_watch)
                        .hint_text(tl!("Add a watch expression"))
                        .desired_width(f32::INFINITY)
                        .id_salt("jsdbg-new-watch"),
                );
                if add.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    action = Some("watch");
                }
            });
            if let Some(i) = remove {
                d.watches.remove(i);
            }
            ui.add_space(8.0);
            section(ui, t, tl!("Call stack"));
            panel(ui, t, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().id_salt("jsdbg-stack").max_height(tall * 0.35).show(ui, |ui| match &d.paused {
                    None => {
                        ui.label(RichText::new(tl!("Not paused")).small().color(t.text_faint));
                    }
                    Some(p) => {
                        for f in &p.stack {
                            let name = if f.function.is_empty() { tl!("(top level)").to_string() } else { f.function.clone() };
                            let place = match (f.script, f.line) {
                                (None, Some(l)) => crate::i18n::fmt(tl!("line {n}"), &[("n", &l.to_string())]),
                                (Some(_), Some(l)) => crate::i18n::fmt(tl!("document script, line {n}"), &[("n", &l.to_string())]),
                                _ => String::new(),
                            };
                            ui.label(RichText::new(format!("{name}  {place}")).monospace());
                        }
                    }
                });
            });
        });
    });
    // Console.
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        section(ui, t, tl!("Console"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.push_id("jsdbg-clear", |ui| ui.small_button(tl!("Clear"))).inner.clicked() {
                action = Some("clear");
            }
        });
    });
    let d = &mut app.js_debugger;
    panel(ui, t, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt("jsdbg-log").max_height(130.0).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if d.log.is_empty() {
                ui.label(RichText::new(tl!("Output appears here.")).color(t.text_faint));
            }
            for (kind, line) in &d.log {
                let color = match kind {
                    LogKind::Error => Color32::from_rgb(0xD1, 0x3B, 0x3B),
                    LogKind::Input | LogKind::Info => t.text_muted,
                    LogKind::Result => t.accent,
                    LogKind::Output => t.text,
                };
                ui.label(RichText::new(line).monospace().color(color));
            }
        });
    });
    ui.horizontal(|ui| {
        let hint = if d.paused.is_some() { tl!("Evaluate in the paused scope") } else { tl!("Run JavaScript against the document") };
        let input = ui.add(
            egui::TextEdit::singleline(&mut d.input)
                .hint_text(hint)
                .desired_width(width - 110.0)
                .font(egui::TextStyle::Monospace)
                .id_salt("jsdbg-input"),
        );
        let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if button(ui, "Evaluate", false, true) || enter {
            action = Some("eval");
        }
    });
    if let Some(sel) = select {
        app.js_debugger.select(sel);
    }
    match action {
        Some("refresh") => app.debugger_refresh(),
        Some("run") => app.debugger_run(Step::Run),
        Some("continue") => app.debugger_run(Step::Run),
        Some("into") => app.debugger_run(Step::Into),
        Some("over") => app.debugger_run(Step::Over),
        Some("out") => app.debugger_run(Step::Out),
        Some("stop") => app.debugger_stop(),
        Some("edit") => {
            let d = &mut app.js_debugger;
            d.editing = !d.editing;
            d.breakable_stale = true;
            d.refresh_breakable();
        }
        Some("watch") => {
            let d = &mut app.js_debugger;
            let w = d.new_watch.trim().to_string();
            if !w.is_empty() && d.watches.len() < pdfcraft_engine::js_debug::MAX_WATCHES && !d.watches.contains(&w) {
                d.watches.push(w);
            }
            d.new_watch.clear();
        }
        Some("clear") => app.js_debugger.log.clear(),
        Some("eval") => app.debugger_eval(),
        _ => {}
    }
    app.js_debugger.refresh_breakable();
    close
}
