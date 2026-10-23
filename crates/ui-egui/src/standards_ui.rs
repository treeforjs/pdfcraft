//! Standards: what the document declares (PDF/A, PDF/X, PDF/UA, output intents), and checking
//! it against PDF/A, PDF/X, PDF/UA or the Arlington syntax model, with Save as PDF/A and
//! Convert to PDF/X. One window with a tab per standard (`standards.pdfa`, `standards.pdfx`,
//! `standards.pdfua`, `standards.validate`).
//!
//! Checks and conversions run on a worker thread (inline in tests and on the web) on a copy of
//! the document, with a spinner and Cancel; a conversion's copy becomes the document as one undo
//! step when it is done, unless the document changed meanwhile.

use std::sync::{Arc, Mutex};

use egui::{Align, Layout};
use pdfcraft_engine::a11y::{UaOptions, UaPart, UaReport, UaSeverity};
use pdfcraft_engine::pdfa::{
    ArlOptions, ArlReport, ArlSeverity, Declared, IccHeader, Issue, Level, PdfaIntent, REGISTERED_CONDITIONS, XConvertOptions, XIntent, XOptions,
    XPart, XReport, XSeverity,
};
use pdfcraft_engine::standards::{StandardsOutcome, StandardsResult, StandardsTask};
use pdfcraft_engine::{DocId, EditError};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Findings listed at most (the checks cap their own output too).
const MAX_SHOWN: usize = 400;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StandardsTab {
    #[default]
    Pdfa,
    Pdfx,
    Pdfua,
    Syntax,
}

/// Where an output intent comes from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum IntentChoice {
    /// The document's own (for PDF/A: sRGB when it has none).
    #[default]
    Document,
    /// A registered printing condition, without a profile (PDF/X-1a and PDF/X-3).
    Registered(String),
    /// A colour profile the session holds (built in or loaded): its id.
    Profile(String),
}

/// A check or conversion running in the background.
pub struct StandardsRun {
    pub doc: DocId,
    pub converting: bool,
    pub result: Arc<Mutex<Option<Result<StandardsResult, EditError>>>>,
    pub cancelled: bool,
}

/// The window's state: the chosen tab and options, and the last results.
pub struct PdfaState {
    pub tab: StandardsTab,
    pub level: Level,
    pub pdfa_intent: IntentChoice,
    /// PDF/A results (Verify, or what Save as PDF/A left).
    pub issues: Option<Vec<Issue>>,
    /// What the last conversion changed, and notes about it.
    pub fixed: Vec<String>,
    pub notes: Vec<String>,
    pub pdfx_part: XPart,
    pub pdfx_intent: IntentChoice,
    pub pdfx: Option<XReport>,
    pub ua_part: UaPart,
    pub pdfua: Option<UaReport>,
    pub syntax: Option<ArlReport>,
    /// The document the results are about.
    pub doc: Option<DocId>,
    /// What the document declares, read once per document change: (document, generation).
    pub declared: Option<(DocId, u64, Declared)>,
    pub run: Option<StandardsRun>,
}

impl Default for PdfaState {
    fn default() -> Self {
        PdfaState {
            tab: StandardsTab::Pdfa,
            level: Level::A2b,
            pdfa_intent: IntentChoice::Document,
            issues: None,
            fixed: Vec::new(),
            notes: Vec::new(),
            pdfx_part: XPart::X4,
            pdfx_intent: IntentChoice::Document,
            pdfx: None,
            ua_part: UaPart::Ua1,
            pdfua: None,
            syntax: None,
            doc: None,
            declared: None,
            run: None,
        }
    }
}

impl PdfaState {
    /// Forget results (another document, or the options changed).
    pub fn clear_results(&mut self) {
        self.issues = None;
        self.pdfx = None;
        self.pdfua = None;
        self.syntax = None;
        self.fixed.clear();
        self.notes.clear();
    }
}

/// The ICC header of a session profile.
fn profile_header(app: &PdfCraftApp, id: &str) -> Option<(IccHeader, Arc<Vec<u8>>, String)> {
    let (info, bytes) = app.session.color_management().profile(id)?;
    let h = IccHeader::read(&bytes).ok()?;
    Some((h, bytes, info.description))
}

/// Session profiles that can be an output intent: output or monitor class for PDF/A, output
/// class for PDF/X (gray or CMYK for PDF/X-1a; RGB too otherwise).
fn intent_profiles(app: &PdfCraftApp, pdfx: Option<XPart>) -> Vec<(String, String)> {
    app.session
        .color_management()
        .profiles()
        .into_iter()
        .filter(|p| {
            let Some((h, _, _)) = profile_header(app, &p.id) else { return false };
            match pdfx {
                None => matches!(&h.class, b"prtr" | b"mntr") && h.components().is_some(),
                Some(part) => &h.class == b"prtr" && (matches!(&h.space, b"CMYK" | b"GRAY") || (&h.space == b"RGB " && !part.is_x1a())),
            }
        })
        .map(|p| (p.id, p.description))
        .collect()
}

impl PdfCraftApp {
    /// Open the Standards window on `tab`.
    pub(crate) fn open_standards(&mut self, tab: StandardsTab) {
        let doc = self.active_ids().map(|(_, id)| id);
        if self.pdfa.doc != doc {
            self.pdfa.clear_results();
            self.pdfa.doc = doc;
        }
        self.pdfa.tab = tab;
        if tab == StandardsTab::Pdfa {
            self.pdfa.issues = None;
        }
        if let Some(level) = doc.and_then(|id| self.session.get(id)).and_then(|d| d.pdfa_declared_level()) {
            self.pdfa.level = level;
        }
        self.dialog = Some(crate::Dialog::PdfA);
    }

    pub fn pdfa_verify(&mut self) {
        self.start_standards(StandardsTask::VerifyPdfa(self.pdfa.level));
    }

    pub fn pdfa_convert(&mut self) {
        let level = self.pdfa.level;
        let intent = match &self.pdfa.pdfa_intent {
            IntentChoice::Profile(id) => match profile_header(self, id) {
                Some((_, profile, description)) => Some(PdfaIntent { condition: description, info: None, profile }),
                None => return self.notify_tr("The chosen profile is no longer loaded."),
            },
            _ => None,
        };
        self.start_standards(StandardsTask::ConvertPdfa { level, intent });
    }

    pub fn pdfx_verify(&mut self) {
        self.start_standards(StandardsTask::VerifyPdfx(XOptions { part: Some(self.pdfa.pdfx_part), max_findings: 100 }));
    }

    pub fn pdfx_convert(&mut self) {
        let part = self.pdfa.pdfx_part;
        let intent = match &self.pdfa.pdfx_intent {
            IntentChoice::Document => None,
            IntentChoice::Registered(c) => Some(XIntent { condition: c.clone(), info: None, profile: None }),
            IntentChoice::Profile(id) => match profile_header(self, id) {
                Some((_, profile, description)) => Some(XIntent { condition: "Custom".into(), info: Some(description), profile: Some(profile) }),
                None => return self.notify_tr("The chosen profile is no longer loaded."),
            },
        };
        let mut options = XConvertOptions::new(part);
        options.intent = intent;
        self.start_standards(StandardsTask::ConvertPdfx(options));
    }

    pub fn pdfua_verify(&mut self) {
        self.start_standards(StandardsTask::VerifyPdfua(UaOptions { part: self.pdfa.ua_part, ..UaOptions::default() }));
    }

    pub fn syntax_validate(&mut self) {
        self.start_standards(StandardsTask::ValidateSyntax(ArlOptions::default()));
    }

    /// Run a Standards task on the active document in the background.
    pub(crate) fn start_standards(&mut self, task: StandardsTask) {
        if self.pdfa.run.is_some() {
            return;
        }
        if task.converts() && !self.commit_form_typing() {
            return;
        }
        let Some((_, id)) = self.active_ids() else { return };
        let job = match self.session.standards_job(id, task) {
            Ok(job) => job,
            Err(e) => return self.notify_error(e),
        };
        let converting = job.task().converts();
        let slot: Arc<Mutex<Option<Result<StandardsResult, EditError>>>> = Arc::default();
        let out = slot.clone();
        let work = move || {
            let r = job.run();
            if let Ok(mut s) = out.lock() {
                *s = Some(r);
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if self.run_inline {
            work();
        } else if let Err(e) = std::thread::Builder::new().name("pdfcraft-standards".into()).spawn(work)
            && let Ok(mut s) = slot.lock()
        {
            *s = Some(Err(EditError::Invalid(e.to_string())));
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.pdfa.doc = Some(id);
        self.pdfa.run = Some(StandardsRun { doc: id, converting, result: slot, cancelled: false });
        self.poll_standards();
    }

    /// Stop waiting for a running check; its result is not taken.
    pub fn cancel_standards(&mut self) {
        if let Some(r) = self.pdfa.run.as_mut() {
            r.cancelled = true;
        }
    }

    /// Take a finished check's result.
    pub(crate) fn poll_standards(&mut self) {
        let Some(run) = self.pdfa.run.as_ref() else { return };
        let (doc, cancelled, slot) = (run.doc, run.cancelled, run.result.clone());
        let taken = match slot.lock() {
            Ok(mut s) => s.take(),
            Err(_) => Some(Err(EditError::Invalid("the check stopped unexpectedly".into()))),
        };
        if cancelled {
            // The worker finishes on its own (every check is bounded); nothing is taken.
            self.pdfa.run = None;
            self.notify_tr("Cancelled");
            return;
        }
        let Some(result) = taken else {
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            return;
        };
        self.pdfa.run = None;
        let result = result.and_then(|r| {
            let changes = r.changes_document();
            let outcome = self.session.finish_standards(doc, r)?;
            Ok((outcome, changes))
        });
        let (outcome, changed) = match result {
            Ok(o) => o,
            Err(e) => return self.notify_error(e),
        };
        if changed && let Some(info) = self.session.get(doc).map(|d| d.info.clone()) {
            for v in self.views.iter_mut().filter(|v| v.id == doc) {
                v.document_changed(&info);
            }
        }
        self.pdfa.fixed.clear();
        self.pdfa.notes.clear();
        match outcome {
            StandardsOutcome::Pdfa(level, issues) => {
                if level == self.pdfa.level {
                    self.pdfa.issues = Some(issues);
                }
            }
            StandardsOutcome::Pdfx(r) => self.pdfa.pdfx = Some(r),
            StandardsOutcome::Pdfua(r) => self.pdfa.pdfua = Some(r),
            StandardsOutcome::Syntax(r) => self.pdfa.syntax = Some(r),
            StandardsOutcome::PdfaConverted(c) => {
                let left = c.remaining.iter().filter(|i| i.is_error()).count();
                self.pdfa.issues = Some(c.remaining);
                match c.refused {
                    Some(blocking) => {
                        self.pdfa.notes = blocking.iter().map(|i| i.message.clone()).collect();
                        self.notify(crate::i18n::fmt(
                            tl!("Can't reach {standard} automatically; nothing was changed."),
                            &[("standard", c.level.label())],
                        ));
                    }
                    None => {
                        self.pdfa.fixed = c.fixed;
                        self.notify(if left == 0 {
                            "No issues remain in the implemented checks. Full PDF/A conformance is not established; save to keep the changes.".into()
                        } else if left == 1 {
                            crate::i18n::fmt(tl!("Fixed what could be fixed; 1 problem remains"), &[])
                        } else {
                            crate::i18n::fmt(tl!("Fixed what could be fixed; {n} problems remain"), &[("n", &left.to_string())])
                        });
                    }
                }
            }
            StandardsOutcome::PdfxConverted(c) => {
                let label = c.part.label();
                self.pdfa.notes = c.notes.clone();
                if !c.converted {
                    self.notify(crate::i18n::fmt(tl!("Can't reach {standard} automatically; nothing was changed."), &[("standard", label)]));
                } else if c.fixed.is_empty() {
                    self.notify_tr("Nothing needed changing.");
                } else {
                    self.notify(crate::i18n::fmt(
                        tl!("Converted to {standard}: {n} changes. Save to keep them."),
                        &[("standard", label), ("n", &c.fixed.len().to_string())],
                    ));
                }
                self.pdfa.fixed = c.fixed;
                self.pdfa.pdfx = Some(c.after);
            }
        }
    }

    /// What the active document declares, read again only when it changed.
    fn declared_standards(&mut self) -> Declared {
        let Some((_, id)) = self.active_ids() else { return Declared::default() };
        let Some(doc) = self.session.get(id) else { return Declared::default() };
        let generation = doc.edit_generation();
        match &self.pdfa.declared {
            Some((d, g, declared)) if *d == id && *g == generation => declared.clone(),
            _ => {
                let declared = doc.standards();
                self.pdfa.declared = Some((id, generation, declared.clone()));
                declared
            }
        }
    }
}

/// One finding: its message, its rule in small muted text, and a link to its page.
fn finding(ui: &mut egui::Ui, t: &Tokens, error: bool, message: &str, rule: &str, page: Option<usize>, go_to: &mut Option<usize>) {
    ui.horizontal_wrapped(|ui| {
        let mark = if error { ui.visuals().error_fg_color } else { t.text_muted };
        ui.label(egui::RichText::new("●").color(mark).small());
        ui.label(message);
    });
    ui.horizontal_wrapped(|ui| {
        ui.add_space(14.0);
        ui.label(egui::RichText::new(rule).small().color(t.text_muted));
        if let Some(p) = page
            && ui.link(egui::RichText::new(crate::i18n::fmt(tl!("Go to page {p}"), &[("p", &(p + 1).to_string())])).small()).clicked()
        {
            *go_to = Some(p);
        }
    });
    ui.add_space(3.0);
}

fn counts(ui: &mut egui::Ui, t: &Tokens, errors: usize, warnings: usize) {
    ui.label(
        egui::RichText::new(crate::i18n::fmt(tl!("Errors: {e} · Warnings: {w}"), &[("e", &errors.to_string()), ("w", &warnings.to_string())]))
            .color(if errors > 0 { ui.visuals().error_fg_color } else { t.text_muted }),
    );
}

fn more(ui: &mut egui::Ui, t: &Tokens, total: usize) {
    if total > MAX_SHOWN {
        ui.label(
            egui::RichText::new(crate::i18n::fmt(tl!("Only the first {n} findings are shown."), &[("n", &MAX_SHOWN.to_string())]))
                .small()
                .color(t.text_muted),
        );
    }
}

/// The results box for the current tab.
fn results(ui: &mut egui::Ui, app: &PdfCraftApp, t: &Tokens, go_to: &mut Option<usize>) {
    let s = &app.pdfa;
    let none = |ui: &mut egui::Ui| {
        ui.label(egui::RichText::new(tl!("Verify to see what the document needs.")).color(t.text_muted));
    };
    match s.tab {
        StandardsTab::Pdfa => match &s.issues {
            None => none(ui),
            Some(v) if v.is_empty() => {
                ui.label("No issues found in the implemented checks. Full PDF/A conformance is not established.");
            }
            Some(v) => {
                let errors = v.iter().filter(|i| i.is_error()).count();
                counts(ui, t, errors, v.len() - errors);
                for i in v.iter().take(MAX_SHOWN) {
                    let fix = if i.fixable { String::new() } else { tl!("  · not fixable here").to_string() };
                    finding(ui, t, i.is_error(), &i.message, &format!("ISO 19005 {}{fix}", i.clause), i.page, go_to);
                }
                more(ui, t, v.len());
            }
        },
        StandardsTab::Pdfx => match &s.pdfx {
            None => none(ui),
            Some(r) => {
                let errors = r.findings.iter().filter(|f| f.severity == XSeverity::Error).count();
                counts(ui, t, errors, r.findings.len() - errors);
                if r.findings.is_empty() {
                    ui.label(tl!("No problems found in the implemented checks."));
                }
                for f in r.findings.iter().take(MAX_SHOWN) {
                    finding(ui, t, f.severity == XSeverity::Error, &f.message, &format!("{} · {}", r.part.label(), f.rule), f.page, go_to);
                }
                more(ui, t, r.findings.len());
            }
        },
        StandardsTab::Pdfua => match &s.pdfua {
            None => none(ui),
            Some(r) => {
                let errors = r.findings.iter().filter(|f| f.severity == UaSeverity::Error).count();
                counts(ui, t, errors, r.findings.len() - errors);
                if r.findings.is_empty() {
                    ui.label(tl!("No problems found in the implemented checks."));
                }
                for f in r.findings.iter().take(MAX_SHOWN) {
                    finding(ui, t, f.severity == UaSeverity::Error, &f.message, &format!("Matterhorn {}", f.checkpoint), f.page, go_to);
                }
                more(ui, t, r.findings.len());
            }
        },
        StandardsTab::Syntax => match &s.syntax {
            None => none(ui),
            Some(r) => {
                counts(ui, t, r.errors, r.warnings);
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        tl!("PDF {v} · objects checked: {n}"),
                        &[("v", &r.version.to_string()), ("n", &r.objects_checked.to_string())],
                    ))
                    .small()
                    .color(t.text_muted),
                );
                let shown: Vec<_> = r.findings.iter().filter(|f| f.severity != ArlSeverity::Info).collect();
                if shown.is_empty() {
                    ui.label(tl!("No problems found in the implemented checks."));
                }
                for f in shown.iter().take(MAX_SHOWN) {
                    let rule = match f.object {
                        Some(n) => format!("{} · {} {n} · {}", f.code, tl!("object"), f.path),
                        None => format!("{} · {}", f.code, f.path),
                    };
                    finding(ui, t, f.severity == ArlSeverity::Error, &f.message, &rule, None, go_to);
                }
                more(ui, t, shown.len());
            }
        },
    }
    if !s.fixed.is_empty() || !s.notes.is_empty() {
        ui.add_space(6.0);
        for f in s.fixed.iter().take(60) {
            ui.label(egui::RichText::new(format!("✓ {f}")).small());
        }
        for n in s.notes.iter().take(60) {
            ui.label(egui::RichText::new(n).small().color(t.text_muted));
        }
    }
}

/// What the document declares, and its output intents.
fn declared_box(ui: &mut egui::Ui, d: &Declared, t: &Tokens) {
    let pdfa = d.pdfa.as_ref().map_or(tl!("none").to_string(), |(p, c)| format!("PDF/A-{p}{}", c.to_lowercase()));
    let pdfx = d.pdfx.clone().unwrap_or_else(|| tl!("none").to_string());
    let pdfua = d.pdfua.map_or(tl!("none").to_string(), |p| format!("PDF/UA-{p}"));
    ui.label(crate::i18n::fmt(tl!("Declared conformance: {shown}"), &[("shown", &pdfa)]));
    ui.label(egui::RichText::new(format!("PDF/X: {pdfx} · PDF/UA: {pdfua}")).color(t.text_muted));
    if d.intents.is_empty() {
        ui.label(egui::RichText::new(tl!("No output intents")).small().color(t.text_muted));
    }
    for (i, oi) in d.intents.iter().enumerate() {
        ui.push_id(("intent", i), |ui| {
            let profile = match &oi.profile {
                None => tl!("no profile (registered condition)").to_string(),
                Some(Ok((h, n))) => crate::i18n::fmt(
                    tl!("{space} {class} profile, ICC version {v}, {n} bytes"),
                    &[
                        ("space", String::from_utf8_lossy(&h.space).trim()),
                        ("class", &String::from_utf8_lossy(&h.class)),
                        ("v", &h.major.to_string()),
                        ("n", &n.to_string()),
                    ],
                ),
                Some(Err(e)) => crate::i18n::fmt(tl!("the profile can't be read: {e}"), &[("e", e)]),
            };
            let condition = if oi.condition.is_empty() { oi.info.clone().unwrap_or_default() } else { oi.condition.clone() };
            ui.label(
                egui::RichText::new(crate::i18n::fmt(tl!("Output intent: {list}"), &[("list", &format!("{} · {condition}", oi.subtype))])).small(),
            );
            ui.label(egui::RichText::new(format!("    {profile}")).small().color(t.text_muted));
        });
    }
}

/// The output intent picker: the document's own, registered conditions (PDF/X without X-4),
/// and the session's suitable profiles, with Load ICC profile… on the desktop.
fn intent_picker(ui: &mut egui::Ui, app: &mut PdfCraftApp, pdfx: Option<XPart>) {
    let profiles = intent_profiles(app, pdfx);
    let choice = match pdfx {
        Some(_) => app.pdfa.pdfx_intent.clone(),
        None => app.pdfa.pdfa_intent.clone(),
    };
    let keep = match pdfx {
        Some(_) => tl!("Keep the document's output intent"),
        None => tl!("Keep the document's (sRGB if it has none)"),
    };
    let shown = match &choice {
        IntentChoice::Document => keep.to_string(),
        IntentChoice::Registered(c) => c.clone(),
        IntentChoice::Profile(id) => profiles.iter().find(|(p, _)| p == id).map_or_else(|| id.clone(), |(_, d)| d.clone()),
    };
    let mut next = choice.clone();
    ui.horizontal(|ui| {
        let l = ui.label(tl!("Output intent"));
        egui::ComboBox::from_id_salt(("standards-intent", pdfx.is_some()))
            .width(320.0)
            .selected_text(shown)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut next, IntentChoice::Document, keep);
                if pdfx.is_some_and(|p| p != XPart::X4) {
                    for (id, description) in REGISTERED_CONDITIONS {
                        ui.selectable_value(&mut next, IntentChoice::Registered((*id).to_string()), *id).on_hover_text(*description);
                    }
                }
                for (id, description) in &profiles {
                    ui.selectable_value(&mut next, IntentChoice::Profile(id.clone()), description);
                }
            })
            .response
            .labelled_by(l.id);
        #[cfg(not(target_arch = "wasm32"))]
        if widgets::pill_button(ui, tl!("Load ICC profile…"), false).clicked() {
            let dialog = rfd::AsyncFileDialog::new().add_filter(tl!("ICC profile"), &["icc", "icm"]).set_title(tl!("Load ICC profile…"));
            app.ask_one(crate::pickers::Ask::File(dialog), None, |app, path| app.load_color_profile_file(&path));
        }
    });
    if pdfx == Some(XPart::X4) && matches!(next, IntentChoice::Registered(_)) {
        next = IntentChoice::Document;
    }
    if next != choice {
        match pdfx {
            Some(_) => app.pdfa.pdfx_intent = next,
            None => app.pdfa.pdfa_intent = next,
        }
    }
}

/// Returns `true` to close.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    ui.set_width(620.0);
    app.poll_standards();
    ui.label(egui::RichText::new(tl!("Standards")).font(theme::semibold(18.0)));
    ui.add_space(4.0);
    let active = app.active_ids().map(|(_, id)| id);
    if app.pdfa.doc != active && app.pdfa.run.is_none() {
        app.pdfa.clear_results();
        app.pdfa.doc = active;
    }
    let declared = app.declared_standards();
    declared_box(ui, &declared, t);
    ui.add_space(6.0);
    let running = app.pdfa.run.is_some();
    ui.horizontal(|ui| {
        for (tab, label) in
            [(StandardsTab::Pdfa, "PDF/A"), (StandardsTab::Pdfx, "PDF/X"), (StandardsTab::Pdfua, "PDF/UA"), (StandardsTab::Syntax, tl!("Syntax"))]
        {
            if widgets::mode_tab(ui, label, app.pdfa.tab == tab).clicked() {
                app.pdfa.tab = tab;
            }
        }
    });
    ui.separator();
    ui.add_space(4.0);
    ui.add_enabled_ui(!running, |ui| match app.pdfa.tab {
        StandardsTab::Pdfa => {
            ui.label("Structural checks only. An independent full validator is needed to establish PDF/A conformance.");
            let before = app.pdfa.level;
            ui.horizontal(|ui| {
                ui.label(tl!("Conformance level:"));
                egui::ComboBox::from_id_salt("pdfa-level").selected_text(app.pdfa.level.label()).show_ui(ui, |ui| {
                    for l in Level::ALL {
                        ui.selectable_value(&mut app.pdfa.level, l, l.label());
                    }
                });
            });
            if app.pdfa.level != before {
                app.pdfa.issues = None;
                app.pdfa.fixed.clear();
            }
            intent_picker(ui, app, None);
        }
        StandardsTab::Pdfx => {
            let before = app.pdfa.pdfx_part;
            ui.horizontal(|ui| {
                ui.label(tl!("Standard"));
                egui::ComboBox::from_id_salt("pdfx-part").selected_text(app.pdfa.pdfx_part.label()).show_ui(ui, |ui| {
                    for p in XPart::ALL {
                        ui.selectable_value(&mut app.pdfa.pdfx_part, p, p.label());
                    }
                });
            });
            if app.pdfa.pdfx_part != before {
                app.pdfa.pdfx = None;
                app.pdfa.fixed.clear();
                app.pdfa.notes.clear();
            }
            intent_picker(ui, app, Some(app.pdfa.pdfx_part));
            if app.pdfa.pdfx_part == XPart::X4 {
                ui.label(egui::RichText::new(tl!("PDF/X-4 embeds the output profile: choose a profile.")).small().color(t.text_muted));
            }
        }
        StandardsTab::Pdfua => {
            let before = app.pdfa.ua_part;
            ui.horizontal(|ui| {
                ui.label(tl!("Standard"));
                egui::ComboBox::from_id_salt("pdfua-part").selected_text(app.pdfa.ua_part.label()).show_ui(ui, |ui| {
                    for p in [UaPart::Ua1, UaPart::Ua2] {
                        ui.selectable_value(&mut app.pdfa.ua_part, p, p.label());
                    }
                });
            });
            if app.pdfa.ua_part != before {
                app.pdfa.pdfua = None;
            }
            ui.add(
                egui::Label::new(
                    egui::RichText::new(tl!(
                        "Checks what a machine can decide. Reading order, the quality of alternate text and the use of colour need a person."
                    ))
                    .small()
                    .color(t.text_muted),
                )
                .wrap(),
            );
        }
        StandardsTab::Syntax => {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(tl!(
                        "Checks every object reachable from the trailer against the Arlington PDF model for the file's PDF version."
                    ))
                    .small()
                    .color(t.text_muted),
                )
                .wrap(),
            );
        }
    });
    ui.add_space(6.0);
    let mut go_to = None;
    egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().max_height(280.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.set_min_height(120.0);
            if let Some(run) = &app.pdfa.run {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(if run.converting { tl!("Converting…") } else { tl!("Checking…") });
                });
            } else {
                results(ui, app, t, &mut go_to);
            }
        });
    });
    ui.add_space(10.0);
    let mut close = false;
    let mut action: Option<fn(&mut PdfCraftApp)> = None;
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if running {
                if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                    app.cancel_standards();
                }
                return;
            }
            match app.pdfa.tab {
                StandardsTab::Pdfa => {
                    if widgets::pill_button(ui, tl!("Save as PDF/A"), true).clicked() {
                        action = Some(PdfCraftApp::pdfa_convert);
                    }
                    if widgets::pill_button(ui, tl!("Verify"), false).clicked() {
                        action = Some(PdfCraftApp::pdfa_verify);
                    }
                }
                StandardsTab::Pdfx => {
                    if widgets::pill_button(ui, tl!("Convert to PDF/X"), true).clicked() {
                        action = Some(PdfCraftApp::pdfx_convert);
                    }
                    if widgets::pill_button(ui, tl!("Verify"), false).clicked() {
                        action = Some(PdfCraftApp::pdfx_verify);
                    }
                }
                StandardsTab::Pdfua => {
                    if widgets::pill_button(ui, tl!("Verify"), true).clicked() {
                        action = Some(PdfCraftApp::pdfua_verify);
                    }
                }
                StandardsTab::Syntax => {
                    if widgets::pill_button(ui, tl!("Validate"), true).clicked() {
                        action = Some(PdfCraftApp::syntax_validate);
                    }
                }
            }
            if widgets::pill_button(ui, tl!("Close"), false).clicked() {
                close = true;
            }
        });
    });
    if let Some(f) = action {
        f(app);
    }
    if let Some(page) = go_to {
        if let Some((index, _)) = app.active_ids()
            && let Some(view) = app.views.get_mut(index)
        {
            view.go_to_page(page);
        }
        close = true;
    }
    if close {
        app.cancel_standards();
        app.poll_standards();
    }
    close
}
