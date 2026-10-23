//! Autotag Document / Autotag form fields (options and a results summary), and the guided Make
//! Accessible action: language and title, tooltips for form fields, tags, tab order, then
//! alternate text figure by figure, all as one undo step, ending with the Full Check.

use egui::{Align, Layout};
use pdfcraft_engine::a11y::{AutotagOptions, FieldTooltips, Options, Rule, Status};
use pdfcraft_engine::{DocId, Edit, FieldProps, TabOrder};

use crate::theme::{self, Tokens};
use crate::{Dialog, PdfCraftApp, RightPanel, icons, widgets};

/// Fields listed for tooltips at most.
const MAX_FIELDS: usize = 10_000;
/// Figures walked through at most (the rest stay listed by the checker).
const MAX_FIGURES: usize = 10_000;
/// The longest tooltip or alternate text written (characters).
const MAX_TEXT: usize = 10_000;

/// The Autotag dialog's choices.
#[derive(Clone, Debug, PartialEq)]
pub struct AutotagDraft {
    pub tables: bool,
    pub lists: bool,
    pub fields: bool,
    pub fill_tooltips: bool,
    pub replace: bool,
    /// Opened as Autotag form fields.
    pub forms: bool,
}

impl Default for AutotagDraft {
    fn default() -> Self {
        Self { tables: true, lists: true, fields: true, fill_tooltips: false, replace: false, forms: false }
    }
}

/// What autotagging made, read from the document afterwards.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutotagSummary {
    pub doc: Option<DocId>,
    pub pages: usize,
    pub elements: usize,
    /// (label, count) of the kinds that were found.
    pub counts: Vec<(&'static str, usize)>,
    pub figures_without_alt: usize,
    pub fields_without_tooltip: usize,
}

/// Where the Make Accessible action is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Setup,
    Tooltips,
    AltText,
    Done,
}

/// One step's outcome, for the final summary.
#[derive(Clone, Debug, PartialEq)]
pub struct StepLog {
    pub label: &'static str,
    pub done: bool,
    pub detail: String,
}

/// The Make Accessible action in progress.
#[derive(Clone)]
pub struct MakeAccessible {
    pub doc: DocId,
    pub step: Step,
    pub title: String,
    pub lang: String,
    pub tagged: bool,
    /// Tag the document (an untagged one), or tag it again (a tagged one).
    pub autotag: bool,
    /// Fields without a tooltip, with the tooltip to give them.
    pub fields: Vec<(String, String)>,
    pub figures: Vec<pdfcraft_engine::a11y::Figure>,
    pub alts: Vec<String>,
    pub decorative: Vec<bool>,
    pub index: usize,
    preview: Option<(usize, egui::TextureHandle)>,
    since: u64,
    pub log: Vec<StepLog>,
    pub failed_rules: Option<usize>,
    pub error: Option<String>,
}

impl std::fmt::Debug for MakeAccessible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MakeAccessible").field("step", &self.step).field("fields", &self.fields.len()).field("figures", &self.figures.len()).finish()
    }
}

/// A field name as words: `first_name` and `firstName` → `First name`.
pub fn humanize(name: &str) -> String {
    let last = name.rsplit('.').next().unwrap_or(name);
    let mut out = String::new();
    let mut prev_lower = false;
    for c in last.chars().take(200) {
        if c == '_' || c == '-' || c == ' ' {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        out.extend(c.to_lowercase());
    }
    let out = out.trim().to_owned();
    let mut chars = out.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().chain(chars).collect(),
        None => name.chars().take(200).collect(),
    }
}

/// A language tag such as en-US: letters, digits and hyphens, starting with a letter.
pub fn valid_lang(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 35
        && l.split('-').all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_alphanumeric()))
        && l.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
}

fn cap(s: &str) -> String {
    s.trim().chars().take(MAX_TEXT).collect()
}

fn buttons(ui: &mut egui::Ui, primary: &str, secondary: &str) -> (bool, bool) {
    let (mut a, mut b) = (false, false);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            a = widgets::pill_button(ui, primary, true).clicked();
            b = widgets::pill_button(ui, secondary, false).clicked();
        });
    });
    (a, b)
}

/// Autotag Document / Autotag form fields: the options.
pub(crate) fn autotag_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let tagged = app.active_ids().and_then(|(_, id)| app.session.get(id)).is_some_and(|d| d.tags().exists);
    let d = &mut app.a11y_tools.autotag;
    ui.label(egui::RichText::new(if d.forms { tl!("Autotag form fields") } else { tl!("Autotag document") }).font(theme::semibold(18.0)));
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(tl!(
            "PdfCraft reads the layout of every page and tags headings, paragraphs, lists, tables, figures, links and form fields in reading order."
        ))
        .color(t.text_muted),
    );
    ui.add_space(8.0);
    ui.checkbox(&mut d.tables, tl!("Find tables"));
    ui.checkbox(&mut d.lists, tl!("Find lists"));
    ui.checkbox(&mut d.fields, tl!("Tag form fields"));
    ui.add_enabled_ui(d.fields, |ui| ui.checkbox(&mut d.fill_tooltips, tl!("Give fields without a tooltip one from their label")));
    if tagged {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("This document is already tagged. Autotagging replaces all of its tags.")).color(t.text_muted));
        ui.checkbox(&mut d.replace, tl!("Replace the existing tags"));
    }
    ui.add_space(10.0);
    let can = !tagged || d.replace;
    let (mut go, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            go = ui.add_enabled_ui(can, |ui| widgets::pill_button(ui, tl!("Autotag"), true)).inner.clicked();
            cancel = widgets::pill_button(ui, tl!("Cancel"), false).clicked();
        });
    });
    if go {
        app.run_autotag();
        return false;
    }
    cancel
}

/// The results of autotagging.
pub(crate) fn summary_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let Some(s) = app.a11y_tools.summary.clone() else { return true };
    ui.label(egui::RichText::new(tl!("Autotag finished")).font(theme::semibold(18.0)));
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(crate::i18n::fmt(tl!("{n} tags on {p} pages."), &[("n", &s.elements.to_string()), ("p", &s.pages.to_string())]))
            .color(t.text_muted),
    );
    ui.add_space(8.0);
    egui::Grid::new("autotag-counts").num_columns(2).spacing(egui::vec2(24.0, 4.0)).show(ui, |ui| {
        for (label, n) in &s.counts {
            ui.label(tl!(label));
            ui.label(egui::RichText::new(n.to_string()).font(theme::semibold(13.0)));
            ui.end_row();
        }
    });
    ui.add_space(8.0);
    let mut action: Option<&'static str> = None;
    let warn = |ui: &mut egui::Ui, text: String| {
        ui.horizontal(|ui| {
            ui.add(icons::image("circle-help", 15.0, egui::Color32::from_rgb(0xD9, 0x8E, 0x04)));
            ui.label(text);
        });
    };
    if s.figures_without_alt > 0 {
        warn(ui, crate::i18n::fmt(tl!("{n} figures need alternate text."), &[("n", &s.figures_without_alt.to_string())]));
        if ui.button(tl!("Add alternate text…")).clicked() {
            action = Some("a11y.alt_text");
        }
    }
    if s.fields_without_tooltip > 0 {
        warn(ui, crate::i18n::fmt(tl!("{n} form fields have no tooltip."), &[("n", &s.fields_without_tooltip.to_string())]));
    }
    ui.label(egui::RichText::new(tl!("Check the reading order of each page with the Order panel.")).small().color(t.text_faint));
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("Close"), true).clicked() {
                action = Some("close");
            }
            if widgets::pill_button(ui, tl!("Check for accessibility"), false).clicked() {
                action = Some("a11y.check");
            }
            if widgets::pill_button(ui, tl!("Show tags"), false).clicked() {
                action = Some("a11y.tags");
            }
        });
    });
    match action {
        Some("close") => true,
        Some(id) => {
            app.dialog = None;
            app.execute(id);
            false
        }
        None => false,
    }
}

/// Make Accessible: one page per step.
pub(crate) fn make_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let ctx = ui.ctx().clone();
    let Some(step) = app.a11y_tools.make.as_ref().map(|m| m.step) else { return true };
    let titles = [tl!("Document"), tl!("Form fields"), tl!("Alternate text"), tl!("Results")];
    ui.label(egui::RichText::new(tl!("Make accessible")).font(theme::semibold(18.0)));
    ui.horizontal(|ui| {
        for (k, title) in titles.iter().enumerate() {
            let on = k == step as usize;
            ui.label(egui::RichText::new(format!("{} {title}", k + 1)).font(theme::medium(12.0)).color(if on {
                t.accent_text
            } else {
                t.text_faint
            }));
            if k + 1 < titles.len() {
                ui.label(egui::RichText::new("·").color(t.text_faint));
            }
        }
    });
    ui.add_space(8.0);
    match step {
        Step::Setup => {
            let Some(m) = app.a11y_tools.make.as_mut() else { return true };
            ui.label(egui::RichText::new(tl!("Screen readers announce the title and read the text in the document's language.")).color(t.text_muted));
            ui.add_space(6.0);
            egui::Grid::new("make-setup").num_columns(2).spacing(egui::vec2(12.0, 8.0)).show(ui, |ui| {
                let l = ui.label(tl!("Title"));
                ui.add(egui::TextEdit::singleline(&mut m.title).desired_width(320.0)).labelled_by(l.id);
                ui.end_row();
                let l = ui.label(tl!("Language"));
                ui.add(egui::TextEdit::singleline(&mut m.lang).desired_width(120.0).hint_text("en-US")).labelled_by(l.id);
                ui.end_row();
            });
            ui.add_space(4.0);
            let label = if m.tagged { tl!("Tag the document again (replaces its tags)") } else { tl!("Tag the document") };
            ui.checkbox(&mut m.autotag, label);
            if let Some(e) = &m.error {
                ui.label(egui::RichText::new(e).color(egui::Color32::from_rgb(0xD7, 0x37, 0x3F)));
            }
            ui.add_space(10.0);
            let (next, cancel) = buttons(ui, tl!("Next"), tl!("Cancel"));
            if next {
                let lang = m.lang.trim().to_owned();
                if !valid_lang(&lang) {
                    m.error = Some(tl!("Give the document's language as a language tag, such as en-US.").to_string());
                    return false;
                }
                m.error = None;
                if m.fields.is_empty() {
                    app.make_apply();
                } else {
                    m.step = Step::Tooltips;
                }
            }
            if cancel {
                app.a11y_tools.make = None;
            }
            cancel
        }
        Step::Tooltips => {
            let Some(m) = app.a11y_tools.make.as_mut() else { return true };
            ui.label(egui::RichText::new(tl!("Screen readers read a field's tooltip to say what to fill in. Check each one.")).color(t.text_muted));
            ui.add_space(6.0);
            let n = m.fields.len();
            egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show_rows(ui, 30.0, n, |ui, range| {
                for k in range {
                    let Some((name, tip)) = m.fields.get_mut(k) else { continue };
                    ui.horizontal(|ui| {
                        let l = ui.add_sized([150.0, 24.0], egui::Label::new(egui::RichText::new(name.as_str()).color(t.text_muted)).truncate());
                        ui.add(egui::TextEdit::singleline(tip).desired_width(300.0)).labelled_by(l.id);
                    });
                }
            });
            ui.add_space(10.0);
            let (next, back) = buttons(ui, tl!("Next"), tl!("Back"));
            if back {
                m.step = Step::Setup;
            }
            if next {
                app.make_apply();
            }
            false
        }
        Step::AltText => {
            let doc = app.a11y_tools.make.as_ref().and_then(|m| app.session.get(m.doc));
            let Some(m) = app.a11y_tools.make.as_mut() else { return true };
            let n = m.figures.len();
            m.index = m.index.min(n.saturating_sub(1));
            let i = m.index;
            if m.preview.as_ref().is_none_or(|(k, _)| *k != i)
                && let (Some(doc), Some(f)) = (doc, m.figures.get(i))
            {
                m.preview = crate::a11y_ui::figure_picture(&ctx, doc, f).map(|tex| (i, tex));
            }
            ui.label(
                egui::RichText::new(crate::i18n::fmt(tl!("Figure {i} of {n}"), &[("i", &(i + 1).to_string()), ("n", &n.to_string())]))
                    .color(t.text_muted),
            );
            ui.add_space(4.0);
            egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                let (area, _) = ui.allocate_exact_size(egui::vec2(380.0, 200.0), egui::Sense::hover());
                match &m.preview {
                    Some((_, tex)) => {
                        let s = tex.size_vec2();
                        let k = (area.width() / s.x.max(1.0)).min(area.height() / s.y.max(1.0)).min(2.0);
                        let fit = egui::Rect::from_center_size(area.center(), s * k);
                        ui.painter().image(tex.id(), fit, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    }
                    None => {
                        ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, tl!("No preview"), theme::regular(13.0), t.text_faint);
                    }
                }
            });
            ui.add_space(6.0);
            let decorative = m.decorative.get(i).copied().unwrap_or(false);
            if let Some(text) = m.alts.get_mut(i) {
                ui.add_enabled_ui(!decorative, |ui| {
                    let l = ui.label(tl!("Alternate text"));
                    ui.add(egui::TextEdit::multiline(text).desired_rows(3).desired_width(380.0).hint_text(tl!("Describe the figure")))
                        .labelled_by(l.id);
                });
            }
            if let Some(dec) = m.decorative.get_mut(i) {
                ui.checkbox(dec, tl!("Decorative figure"));
            }
            ui.add_space(10.0);
            let mut finish = false;
            ui.horizontal(|ui| {
                if ui.add_enabled_ui(i > 0, |ui| icons::button(ui, "chevron-left", 28.0, false, tl!("Previous figure"))).inner.clicked() {
                    m.index = i.saturating_sub(1);
                }
                if ui.add_enabled_ui(i + 1 < n, |ui| icons::button(ui, "chevron-right", 28.0, false, tl!("Next figure"))).inner.clicked() {
                    m.index = i + 1;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    finish = widgets::pill_button(ui, tl!("Finish"), true).clicked();
                });
            });
            if finish {
                app.make_finish(true);
            }
            false
        }
        Step::Done => {
            let Some(m) = app.a11y_tools.make.as_ref() else { return true };
            for s in &m.log {
                ui.horizontal(|ui| {
                    let (icon, color) =
                        if s.done { ("circle-check", egui::Color32::from_rgb(0x2D, 0x9D, 0x4F)) } else { ("circle-dashed", t.text_faint) };
                    ui.add(icons::image(icon, 15.0, color));
                    ui.label(egui::RichText::new(tl!(s.label)).font(theme::medium(13.0)));
                    ui.label(egui::RichText::new(&s.detail).color(t.text_muted));
                });
            }
            ui.add_space(8.0);
            let verdict = match m.failed_rules {
                Some(0) => tl!("The checker found no problems in this document.").to_string(),
                Some(n) => crate::i18n::fmt(
                    tl!("The checker found {n} problems which may prevent the document from being fully accessible."),
                    &[("n", &n.to_string())],
                ),
                None => String::new(),
            };
            ui.label(verdict);
            ui.label(egui::RichText::new(tl!("Undo takes back the whole action in one step.")).small().color(t.text_faint));
            ui.add_space(10.0);
            let (close, results) = buttons(ui, tl!("Close"), tl!("Show checker results"));
            if results {
                app.choose_right_panel(Some(RightPanel::Accessibility));
            }
            if close || results {
                app.a11y_tools.make = None;
                return true;
            }
            false
        }
    }
}

impl PdfCraftApp {
    /// Accessibility ▸ Autotag document (or form fields): the options.
    pub(crate) fn open_autotag(&mut self, forms: bool) {
        let d = &mut self.a11y_tools.autotag;
        d.forms = forms;
        d.replace = false;
        if forms {
            d.fields = true;
            d.fill_tooltips = true;
        }
        self.dialog = Some(Dialog::Autotag);
    }

    /// Autotag with the dialog's choices, then show what was made.
    pub fn run_autotag(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let d = self.a11y_tools.autotag.clone();
        let options = AutotagOptions {
            replace_existing: d.replace,
            detect_tables: d.tables,
            detect_lists: d.lists,
            detect_fields: d.fields,
            field_tooltips: if d.fields && d.fill_tooltips { FieldTooltips::Fill } else { FieldTooltips::Report },
            ..Default::default()
        };
        if !self.apply_edit(Edit::Autotag { options }) {
            self.dialog = None;
            return;
        }
        // The summary comes from the tagged document, so autotagging runs once.
        let Some(doc) = self.session.get(id) else { return };
        let tags = doc.tags();
        let count = |f: &dyn Fn(&str) -> bool| tags.nodes.iter().filter(|n| f(n.role())).count();
        let mut counts = vec![
            ("Headings", count(&|r| pdfcraft_engine::a11y::heading_level(r).is_some())),
            ("Paragraphs", count(&|r| r == "P")),
            ("Lists", count(&|r| r == "L")),
            ("Tables", count(&|r| r == "Table")),
            ("Figures", count(&|r| r == "Figure")),
            ("Links", count(&|r| r == "Link")),
            ("Form fields", count(&|r| r == "Form")),
        ];
        counts.retain(|(_, n)| *n > 0);
        let no_tip = doc.info.fields.iter().filter(|f| f.tooltip.as_deref().is_none_or(|t| t.trim().is_empty())).count();
        self.a11y_tools.summary = Some(AutotagSummary {
            doc: Some(id),
            pages: doc.info.pages.len(),
            elements: tags.nodes.len(),
            counts,
            figures_without_alt: doc.alt_targets().figures.iter().filter(|f| f.needs_alt).count(),
            fields_without_tooltip: if d.fields { no_tip } else { 0 },
        });
        self.dialog = Some(Dialog::AutotagSummary);
    }

    /// Use guided actions ▸ Make accessible.
    pub(crate) fn start_make_accessible(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        if !doc.editable() {
            self.notify_tr("This document can't be edited: it couldn't be read for editing");
            return;
        }
        let tags = doc.tags();
        let title = doc.info_value("Title").map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()).unwrap_or_else(|| {
            let name = doc.name.trim();
            name.strip_suffix(".pdf").or_else(|| name.strip_suffix(".PDF")).unwrap_or(name).to_owned()
        });
        let fields: Vec<(String, String)> = doc
            .info
            .fields
            .iter()
            .filter(|f| f.tooltip.as_deref().is_none_or(|t| t.trim().is_empty()))
            .take(MAX_FIELDS)
            .map(|f| (f.name.clone(), humanize(&f.name)))
            .collect();
        self.a11y_tools.make = Some(MakeAccessible {
            doc: id,
            step: Step::Setup,
            title,
            lang: tags.lang.clone().unwrap_or_default(),
            tagged: tags.exists,
            autotag: !tags.exists,
            fields,
            figures: Vec::new(),
            alts: Vec::new(),
            decorative: Vec::new(),
            index: 0,
            preview: None,
            since: doc.edit_generation(),
            log: Vec::new(),
            failed_rules: None,
            error: None,
        });
        self.dialog = Some(Dialog::MakeAccessible);
    }

    fn make_step(&mut self, log: &mut Vec<StepLog>, label: &'static str, edit: Option<Result<Edit, String>>, detail: String) {
        let done = match edit {
            None => false,
            Some(Ok(e)) => self.apply_edit(e),
            Some(Err(e)) => {
                self.notify_error(e);
                false
            }
        };
        log.push(StepLog { label, done, detail: if done { detail } else { tl!("skipped").to_string() } });
    }

    /// Run the document steps: language, tooltips, tags, title, tab order.
    pub(crate) fn make_apply(&mut self) {
        let Some(mut m) = self.a11y_tools.make.take() else { return };
        let Some(doc) = self.session.get(m.doc) else { return };
        if self.active_ids().map(|(_, id)| id) != Some(m.doc) {
            return;
        }
        m.since = doc.edit_generation();
        let mut log = Vec::new();
        let lang = m.lang.trim().to_owned();
        let same_lang = doc.tags().lang.as_deref() == Some(lang.as_str());
        let lang_edit = (!same_lang).then(|| doc.accessibility_fix(Rule::PrimaryLanguage, Some(&lang)));
        self.make_step(&mut log, "Language", lang_edit, lang.clone());
        // Tooltips first, so autotagging gives each field's tag its tooltip.
        let mut set = 0usize;
        for (name, tip) in &m.fields {
            let tip = cap(tip);
            if !tip.is_empty()
                && self
                    .apply_edit(Edit::SetFieldProps { name: name.clone(), props: Box::new(FieldProps { tooltip: Some(tip), ..Default::default() }) })
            {
                set += 1;
            }
        }
        log.push(StepLog { label: "Form field tooltips", done: set > 0, detail: crate::i18n::fmt(tl!("{n} set"), &[("n", &set.to_string())]) });
        let tag_edit = m.autotag.then(|| Ok(Edit::Autotag { options: AutotagOptions { replace_existing: m.tagged, ..Default::default() } }));
        self.make_step(&mut log, "Tags", tag_edit, String::new());
        let title = cap(&m.title);
        let title_edit = self.session.get(m.doc).filter(|_| !title.is_empty()).map(|d| d.accessibility_fix(Rule::Title, Some(&title)));
        self.make_step(&mut log, "Title", title_edit, title.clone());
        let pages = self.session.get(m.doc).map_or(0, |d| d.info.pages.len());
        let tab = (pages > 0).then(|| Ok(Edit::SetTabOrder { pages: (0..pages).collect(), order: TabOrder::Structure }));
        self.make_step(&mut log, "Tab order", tab, tl!("structure order").to_string());
        m.log = log;
        let figures: Vec<_> = self
            .session
            .get(m.doc)
            .map(|d| d.figures().into_iter().filter(|f| f.alt.as_deref().is_none_or(|a| a.trim().is_empty())).take(MAX_FIGURES).collect())
            .unwrap_or_default();
        m.alts = vec![String::new(); figures.len()];
        m.decorative = vec![false; figures.len()];
        m.figures = figures;
        m.index = 0;
        m.step = Step::AltText;
        let empty = m.figures.is_empty();
        self.a11y_tools.make = Some(m);
        if empty {
            self.make_finish(false);
        }
    }

    /// Write the alternate text, merge everything into one undo step and check the document.
    pub(crate) fn make_finish(&mut self, with_alt: bool) {
        let Some(mut m) = self.a11y_tools.make.take() else { return };
        if with_alt {
            let mut edits = Vec::new();
            for (k, f) in m.figures.iter().enumerate() {
                let alt = m.alts.get(k).map(|s| cap(s)).unwrap_or_default();
                if m.decorative.get(k).copied().unwrap_or(false) {
                    edits.push(Edit::MarkDecorative { figure: f.obj.num });
                } else if !alt.is_empty() {
                    edits.push(Edit::SetAltText { figure: f.obj.num, alt: Some(alt) });
                }
            }
            let n = edits.len();
            let done = n > 0 && self.apply_edit(Edit::Batch { label: "Set alternate text".into(), edits });
            m.log.push(StepLog { label: "Alternate text", done, detail: crate::i18n::fmt(tl!("{n} set"), &[("n", &n.to_string())]) });
        } else {
            m.log.push(StepLog { label: "Alternate text", done: false, detail: tl!("no figures").to_string() });
        }
        let _ = self.session.group_undo(m.doc, m.since, "Make accessible");
        if let Some(doc) = self.session.get(m.doc) {
            m.failed_rules = doc.accessibility_check(&Options::default()).map(|r| {
                let n = r.count(Status::Failed);
                self.a11y.report = Some((m.doc, r));
                n
            });
        }
        m.step = Step::Done;
        m.preview = None;
        self.a11y_tools.make = Some(m);
    }
}
