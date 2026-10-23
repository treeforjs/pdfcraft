//! Compare files: choose the older version, the pages and how closely to look, then the Compare
//! panel shows the page map (old page → new page: same, modified, moved, inserted, deleted) and
//! lists the differences (replaced blue, inserted green, deleted red, looks different orange),
//! shades them on the newer document's pages, and offers a report, the differences as comments,
//! the JSON report and a marked-up copy. The comparison runs on a worker thread with progress
//! and can be stopped.

use std::sync::{Arc, Mutex};

use egui::{Align, Color32, Layout};
use pdfcraft_engine::DocId;
use pdfcraft_engine::compare::{CompareOptions, FileComparison, Kind, PageStatus};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, RightPanel, widgets};

/// The last comparison.
pub struct CompareState {
    pub old: DocId,
    pub new: DocId,
    /// The page map, the text changes and the pages that look different, from one run (each
    /// page's text is read once).
    pub result: FileComparison,
    pub opts: CompareOptions,
    pub selected: Option<usize>,
}

/// The Compare Files dialog's choices (kept between runs). Page numbers are 1-based.
#[derive(Clone, Debug, PartialEq)]
pub struct CompareDraft {
    pub old_all: bool,
    pub old_from: usize,
    pub old_to: usize,
    pub new_all: bool,
    pub new_from: usize,
    pub new_to: usize,
    /// Also compare how the paired pages look.
    pub visual: bool,
    pub dpi: u32,
    /// A pixel differs when a colour channel differs by more than this.
    pub tolerance: u8,
    /// A page looks different when more than this percentage of its pixels differ.
    pub threshold: f64,
}

impl Default for CompareDraft {
    fn default() -> Self {
        let d = CompareOptions::default();
        CompareDraft {
            old_all: true,
            old_from: 1,
            old_to: 1,
            new_all: true,
            new_from: 1,
            new_to: 1,
            visual: true,
            dpi: d.dpi as u32,
            tolerance: d.tolerance,
            threshold: d.threshold * 100.0,
        }
    }
}

impl CompareDraft {
    /// The engine options for files of `old_pages` and `new_pages` pages (ranges clamped).
    pub fn options(&self, old_pages: usize, new_pages: usize) -> CompareOptions {
        let range = |all: bool, from: usize, to: usize, n: usize| {
            if all || n == 0 {
                return None;
            }
            let to = to.clamp(1, n);
            let from = from.clamp(1, to);
            Some((from - 1, to - 1))
        };
        let threshold = if self.threshold.is_finite() { (self.threshold / 100.0).clamp(0.0, 1.0) } else { 0.0 };
        CompareOptions {
            old_pages: range(self.old_all, self.old_from, self.old_to, old_pages),
            new_pages: range(self.new_all, self.new_from, self.new_to, new_pages),
            visual: self.visual,
            dpi: self.dpi.clamp(18, 150) as f32,
            tolerance: self.tolerance,
            threshold,
        }
    }
}

/// Progress of a running comparison, shared with its worker.
#[derive(Default)]
pub struct CompareProgress {
    pub done: usize,
    pub total: usize,
    pub cancel: bool,
    /// `Some(None)` when it stopped on request.
    pub result: Option<Option<FileComparison>>,
}

/// A comparison in progress.
pub struct CompareRun {
    pub old: DocId,
    pub new: DocId,
    pub opts: CompareOptions,
    pub progress: Arc<Mutex<CompareProgress>>,
}

impl CompareRun {
    /// (done, total, stopping) for the panel.
    pub fn snapshot(&self) -> Option<(usize, usize, bool)> {
        self.progress.lock().ok().map(|s| (s.done, s.total, s.cancel))
    }
}

/// Visual differences are shaded orange.
pub const VISUAL: Color32 = Color32::from_rgb(0xF0, 0x8C, 0x1A);
/// Moved pages in the page map.
const MOVED: Color32 = Color32::from_rgb(0x8E, 0x5C, 0xD9);

/// What the panel asks for.
pub enum PanelAction {
    Select(usize),
    GoToPage(usize),
    Mark,
    Report,
    SaveJson,
    SaveOverlay,
    Clear,
    Stop,
    SideBySide,
}

pub fn colour(kind: Kind) -> Color32 {
    let [r, g, b] = pdfcraft_engine::compare::colour(kind);
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// A page's status in the page map (its own context: "Modified" is also a date column).
pub(crate) fn status_label(s: PageStatus) -> &'static str {
    match s {
        PageStatus::Same => tl_ctx!("compare-page", "Same"),
        PageStatus::Modified => tl_ctx!("compare-page", "Modified"),
        PageStatus::Inserted => tl_ctx!("compare-page", "Inserted"),
        PageStatus::Deleted => tl_ctx!("compare-page", "Deleted"),
        PageStatus::Moved => tl_ctx!("compare-page", "Moved"),
    }
}

pub(crate) fn status_colour(s: PageStatus, t: &Tokens) -> Color32 {
    match s {
        PageStatus::Same => t.text_faint,
        PageStatus::Modified => colour(Kind::Replaced),
        PageStatus::Inserted => colour(Kind::Inserted),
        PageStatus::Deleted => colour(Kind::Deleted),
        PageStatus::Moved => MOVED,
    }
}

/// "All pages" or a From–to range of `n` pages.
fn range_row(ui: &mut egui::Ui, all: &mut bool, from: &mut usize, to: &mut usize, n: usize) {
    ui.horizontal(|ui| {
        ui.checkbox(all, tl!("All pages"));
        ui.add_enabled_ui(!*all, |ui| {
            ui.label(tl!("From"));
            ui.add(egui::DragValue::new(from).range(1..=n));
            ui.label(tl!("to"));
            ui.add(egui::DragValue::new(to).range(1..=n));
        });
    });
}

/// The Compare Files dialog: pick the older document, the pages and the visual settings.
/// Returns (compare, cancel).
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> (bool, bool) {
    ui.label(egui::RichText::new(tl!("Compare Files")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    let Some((_, new)) = app.active_ids() else { return (false, true) };
    let others: Vec<(DocId, String)> =
        app.views.iter().filter(|v| v.id != new).filter_map(|v| app.session.get(v.id).map(|d| (d.id, d.name.clone()))).collect();
    if app.compare_old.is_none_or(|o| !others.iter().any(|(id, _)| *id == o)) {
        app.compare_old = others.first().map(|(id, _)| *id);
    }
    let new_name = app.session.get(new).map(|d| d.name.clone()).unwrap_or_default();
    let pages = |id: Option<DocId>| id.and_then(|id| app.session.get(id)).map_or(1, |d| d.info.pages.len().max(1));
    let (old_n, new_n) = (pages(app.compare_old), pages(Some(new)));
    let d = &mut app.compare_draft;
    d.old_to = d.old_to.clamp(1, old_n);
    d.old_from = d.old_from.clamp(1, d.old_to);
    d.new_to = d.new_to.clamp(1, new_n);
    d.new_from = d.new_from.clamp(1, d.new_to);
    egui::Grid::new("compare-files").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label(tl!("Old file:"));
        let shown =
            others.iter().find(|(id, _)| Some(*id) == app.compare_old).map_or(tl!("Open the older version first").to_string(), |(_, n)| n.clone());
        egui::ComboBox::from_id_salt("compare-old").selected_text(shown).width(320.0).show_ui(ui, |ui| {
            for (id, n) in &others {
                ui.selectable_value(&mut app.compare_old, Some(*id), n);
            }
        });
        ui.end_row();
        ui.label(tl!("Old pages:"));
        ui.push_id("old-pages", |ui| range_row(ui, &mut d.old_all, &mut d.old_from, &mut d.old_to, old_n));
        ui.end_row();
        ui.label(tl!("New file:"));
        ui.label(egui::RichText::new(new_name).strong());
        ui.end_row();
        ui.label(tl!("New pages:"));
        ui.push_id("new-pages", |ui| range_row(ui, &mut d.new_all, &mut d.new_from, &mut d.new_to, new_n));
        ui.end_row();
    });
    ui.add_space(6.0);
    ui.checkbox(&mut d.visual, tl!("Also compare how the pages look"));
    ui.add_enabled_ui(d.visual, |ui| {
        egui::Grid::new("compare-visual").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(tl!("Resolution"));
            ui.add(egui::DragValue::new(&mut d.dpi).range(18..=150).suffix(" dpi"));
            ui.end_row();
            ui.label(tl!("Colour tolerance"));
            ui.add(egui::DragValue::new(&mut d.tolerance).range(0..=255))
                .on_hover_text(tl!("A pixel counts as changed when a colour channel differs by more than this (0–255)"));
            ui.end_row();
            ui.label(tl!("Change threshold"));
            ui.add(egui::DragValue::new(&mut d.threshold).range(0.0..=100.0).speed(0.1).max_decimals(2).suffix(" %"))
                .on_hover_text(tl!("A page looks different only when more than this share of its pixels changed"));
            ui.end_row();
        });
    });
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Text is compared word by word; the differences are shown on the new file.")).small().color(t.text_muted));
    ui.add_space(10.0);
    let (mut go, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add_enabled_ui(app.compare_old.is_some(), |ui| widgets::pill_button(ui, tl!("Compare"), true)).inner.clicked() {
                go = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                cancel = true;
            }
        })
    });
    (go, cancel)
}

/// The Compare panel. `running` is (done, total, stopping) while a comparison of this document
/// is in progress.
pub(crate) fn panel(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &Option<CompareState>,
    running: Option<(usize, usize, bool)>,
    id: DocId,
) -> Option<PanelAction> {
    let mut action = None;
    if let Some((done, total, stopping)) = running {
        let total = total.max(1);
        ui.label(crate::i18n::fmt(tl!("Comparing… {d} of {t}"), &[("d", &done.min(total).to_string()), ("t", &total.to_string())]));
        ui.add(egui::ProgressBar::new(done.min(total) as f32 / total as f32));
        ui.add_space(4.0);
        if stopping {
            ui.label(egui::RichText::new(tl!("Stopping…")).color(t.text_muted));
        } else if ui.button(tl!("Stop")).clicked() {
            action = Some(PanelAction::Stop);
        }
        return action;
    }
    let Some(s) = state.as_ref().filter(|s| s.new == id) else {
        ui.label(egui::RichText::new(tl!("Compare this file with an older version: Compare files… in All tools.")).color(t.text_muted));
        return None;
    };
    let r = &s.result;
    let c = &r.text;
    if c.identical() {
        ui.label(tl!("No differences in the text."));
    }
    let visual_regions: usize = r.visual.iter().map(|v| v.regions.len()).sum();
    ui.horizontal_wrapped(|ui| {
        for k in [Kind::Replaced, Kind::Inserted, Kind::Deleted] {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().rect_filled(dot, egui::CornerRadius::same(2), colour(k));
            ui.label(crate::i18n::fmt(tl!("{n} {kind}"), &[("n", &c.count(k).to_string()), ("kind", tl!(k.label()))]));
        }
        let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().rect_filled(dot, egui::CornerRadius::same(2), VISUAL);
        ui.label(crate::i18n::fmt(tl!("{n} {kind}"), &[("n", &visual_regions.to_string()), ("kind", tl!("Visual"))]));
    });
    if r.visual_skipped > 0 {
        ui.label(
            egui::RichText::new(crate::i18n::fmt(tl!("{n} page pairs were not compared for looks"), &[("n", &r.visual_skipped.to_string())]))
                .small()
                .color(t.text_muted),
        );
    }
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        if ui.button(tl!("Compare side by side")).clicked() {
            action = Some(PanelAction::SideBySide);
        }
        if ui.button(tl!("Mark as comments")).clicked() {
            action = Some(PanelAction::Mark);
        }
        if ui.button(tl!("Report…")).clicked() {
            action = Some(PanelAction::Report);
        }
        if ui.button(tl!("Save JSON report…")).clicked() {
            action = Some(PanelAction::SaveJson);
        }
        if ui.button(tl!("Save marked-up copy…")).clicked() {
            action = Some(PanelAction::SaveOverlay);
        }
        if ui.button(tl!("Clear")).clicked() {
            action = Some(PanelAction::Clear);
        }
    });
    ui.separator();
    egui::CollapsingHeader::new(tl!("Page map")).id_salt("compare-page-map").default_open(true).show(ui, |ui| {
        ui.label(
            egui::RichText::new(crate::i18n::fmt(
                tl!("{i} inserted, {d} deleted, {m} moved, {c} modified pages"),
                &[
                    ("i", &r.count(PageStatus::Inserted).to_string()),
                    ("d", &r.count(PageStatus::Deleted).to_string()),
                    ("m", &r.count(PageStatus::Moved).to_string()),
                    ("c", &r.count(PageStatus::Modified).to_string()),
                ],
            ))
            .small()
            .color(t.text_muted),
        );
        let num = |p: Option<usize>| p.map_or_else(|| "—".to_string(), |p| (p + 1).to_string());
        for (k, p) in r.pages.iter().enumerate().take(2000) {
            let text = crate::i18n::fmt(
                tl!("Old page {o} → new page {n} · {status}"),
                &[("o", &num(p.old)), ("n", &num(p.new)), ("status", status_label(p.status))],
            );
            let row = ui.push_id(("page", k), |ui| {
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().rect_filled(dot, egui::CornerRadius::same(2), status_colour(p.status, t));
                    ui.selectable_label(false, text)
                })
                .inner
            });
            if row.inner.clicked()
                && let Some(n) = p.new
            {
                action = Some(PanelAction::GoToPage(n));
            }
        }
    });
    ui.separator();
    for (i, ch) in c.changes.iter().enumerate().take(2000) {
        let text = match ch.kind {
            Kind::Replaced => format!("\"{}\" → \"{}\"", ch.old.text, ch.new.text),
            Kind::Inserted => format!("+ \"{}\"", ch.new.text),
            Kind::Deleted => format!("− \"{}\"", ch.old.text),
        };
        let selected = s.selected == Some(i);
        let r = ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                let (bar, _) = ui.allocate_exact_size(egui::vec2(4.0, 34.0), egui::Sense::hover());
                ui.painter().rect_filled(bar, egui::CornerRadius::same(2), colour(ch.kind));
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(crate::i18n::fmt(
                            tl!("{kind} · page {p}"),
                            &[("kind", tl!(ch.kind.label())), ("p", &(ch.new.page + 1).to_string())],
                        ))
                        .small()
                        .color(t.text_muted),
                    );
                    ui.selectable_label(selected, text)
                })
                .inner
            })
            .inner
        });
        if r.inner.clicked() {
            action = Some(PanelAction::Select(i));
        }
    }
    action
}

impl PdfCraftApp {
    /// Compare the chosen older document with the active one using the dialog's choices, on a
    /// worker thread (inline in tests with [`PdfCraftApp::run_inline`]); the Compare panel
    /// shows the progress and then the differences.
    pub fn run_compare(&mut self) {
        let Some((i, new)) = self.active_ids() else { return };
        let Some(old) = self.compare_old else { return };
        if self.compare_run.is_some() {
            self.notify(tl!("A comparison is already running").to_string());
            return;
        }
        let pages = |id| self.session.get(id).map_or(0, |d| d.info.pages.len());
        let opts = self.compare_draft.options(pages(old), pages(new));
        let job = match self.session.compare_job(old, new, &opts) {
            Ok(job) => job,
            Err(e) => {
                self.notify_error(e);
                return;
            }
        };
        // The earlier comparison's marks and side-by-side view go now.
        self.side_by_side = None;
        if let Some(prev) = self.compare.take()
            && let Some(v) = self.views.iter_mut().find(|v| v.id == prev.new)
        {
            v.compare_marks.clear();
        }
        self.views[i].compare_marks.clear();
        let progress = Arc::new(Mutex::new(CompareProgress::default()));
        let p = progress.clone();
        let work = move || {
            let result = job.run(|done, total| {
                let Ok(mut s) = p.lock() else { return false };
                s.done = done;
                s.total = total;
                !s.cancel
            });
            if let Ok(mut s) = p.lock() {
                s.result = Some(result);
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if self.run_inline {
            work();
        } else if let Err(e) = std::thread::Builder::new().name("pdfcraft-compare".into()).spawn(work) {
            self.notify_error(e);
            return;
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.compare_run = Some(CompareRun { old, new, opts, progress });
        self.right = Some(RightPanel::Compare);
        self.poll_compare();
    }

    /// Show a finished comparison (called every frame while one runs).
    pub(crate) fn poll_compare(&mut self) {
        let Some(run) = self.compare_run.as_ref() else { return };
        let progress = run.progress.clone();
        let result = match progress.lock() {
            Ok(mut s) => s.result.take(),
            Err(_) => {
                // The worker failed; nothing will arrive.
                self.compare_run = None;
                self.notify(tl!("The comparison stopped unexpectedly").to_string());
                return;
            }
        };
        let Some(result) = result else {
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            return;
        };
        let Some(run) = self.compare_run.take() else { return };
        match result {
            Some(result) => self.show_comparison(run, result),
            None => self.notify(tl!("Comparison stopped").to_string()),
        }
    }

    fn show_comparison(&mut self, run: CompareRun, result: FileComparison) {
        // The new document may have been closed meanwhile.
        let Some(i) = self.views.iter().position(|v| v.id == run.new) else { return };
        let mut marks: Vec<(usize, [f32; 4], Color32)> =
            result.text.changes.iter().flat_map(|ch| ch.new.rects.iter().map(move |r| (ch.new.page, r.map(|v| v as f32), colour(ch.kind)))).collect();
        // Visual differences not already covered by a text change.
        for v in &result.visual {
            for r in v.regions.iter().filter(|r| !pdfcraft_engine::compare::explained_by_text(&result.text, v.new_page, r)) {
                marks.push((v.new_page, r.map(|x| x as f32), VISUAL));
            }
        }
        self.views[i].compare_marks = marks;
        let n = result.text.changes.len();
        self.compare = Some(CompareState { old: run.old, new: run.new, result, opts: run.opts, selected: None });
        if self.active_ids().is_some_and(|(_, id)| id == run.new) {
            self.right = Some(RightPanel::Compare);
        }
        // Compare signed version with current opens straight into the side-by-side view.
        if std::mem::take(&mut self.side_by_side_after_compare) && self.active_ids().is_some_and(|(_, id)| id == run.new) {
            self.open_side_by_side();
        }
        self.notify(if n == 0 {
            tl!("The text is the same").to_string()
        } else if n == 1 {
            tl!("1 difference found").to_string()
        } else {
            crate::i18n::fmt(tl!("{n} differences found"), &[("n", &n.to_string())])
        });
    }

    pub(crate) fn compare_action(&mut self, index: usize, a: PanelAction) {
        if let PanelAction::Stop = a {
            if let Some(run) = &self.compare_run
                && let Ok(mut s) = run.progress.lock()
            {
                s.cancel = true;
            }
            return;
        }
        let Some(s) = self.compare.take() else { return };
        let (old, new) = (s.old, s.new);
        let mut keep = true;
        match a {
            PanelAction::Select(i) => {
                if let Some(ch) = s.result.text.changes.get(i)
                    && let Some(v) = self.views.get_mut(index)
                {
                    let rect = ch.new.rects.first().copied().or(ch.new.near).unwrap_or_default();
                    let page = ch.new.page;
                    v.go_to_page(page);
                    v.flash = Some((page, rect.map(|x| x as f32), 0.0));
                }
                self.compare = Some(CompareState { selected: Some(i), ..s });
                self.side_by_side_follow_text(i);
                return;
            }
            PanelAction::GoToPage(p) => {
                if let Some(v) = self.views.get_mut(index) {
                    v.go_to_page(p);
                }
            }
            PanelAction::Mark => match self.session.mark_file_differences(new, &s.result) {
                Ok(n) => {
                    if let Some(info) = self.session.get(new).map(|d| d.info.clone())
                        && let Some(v) = self.views.get_mut(index)
                    {
                        v.document_changed(&info);
                    }
                    self.notify(if n == 1 {
                        tl!("Added 1 comment").to_string()
                    } else {
                        crate::i18n::fmt(tl!("Added {n} comments"), &[("n", &n.to_string())])
                    });
                }
                Err(e) => self.notify_error(e),
            },
            PanelAction::Report => match self.session.compare_files_report(old, new, &s.result) {
                Ok(bytes) => {
                    if let Err(e) = self.open_bytes("Compare Report.pdf", None, bytes.to_vec()) {
                        self.notify_error(e);
                    }
                }
                Err(e) => self.notify_error(e),
            },
            PanelAction::SaveJson => {
                let report = self.session.compare_json(old, new, &s.result, &s.opts, usize::MAX);
                match serde_json::to_vec_pretty(&report) {
                    Ok(bytes) => {
                        let name = format!("{} compare.json", self.compare_stem(new));
                        self.save_compare_file(name, "JSON", "json", bytes);
                    }
                    Err(e) => self.notify_error(e),
                }
            }
            PanelAction::SaveOverlay => match self.session.compare_overlay(new, &s.result) {
                Ok(bytes) => {
                    let name = format!("{} (compared).pdf", self.compare_stem(new));
                    self.save_compare_file(name, "PDF", "pdf", bytes.to_vec());
                }
                Err(e) => self.notify_error(e),
            },
            PanelAction::SideBySide => {
                self.compare = Some(s);
                self.toggle_side_by_side();
                return;
            }
            PanelAction::Clear => {
                keep = false;
                self.side_by_side = None;
                if let Some(v) = self.views.get_mut(index) {
                    v.compare_marks.clear();
                }
                self.right = None;
            }
            PanelAction::Stop => {}
        }
        if keep {
            self.compare = Some(s);
        }
    }

    fn compare_stem(&self, id: DocId) -> String {
        let name = self.session.get(id).map(|d| d.name.clone()).unwrap_or_default();
        name.trim_end_matches(".pdf").trim_end_matches(".PDF").to_string()
    }

    /// Save a comparison result: ask where (`save_override` in tests), then write.
    fn save_compare_file(&mut self, name: String, kind: &str, ext: &str, bytes: Vec<u8>) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let write = move |app: &mut Self, path: std::path::PathBuf| match crate::editing::write_atomically(&path.to_string_lossy(), &bytes) {
                Ok(()) => app.notify_fmt("Exported to {path}", &[("path", &path.display().to_string())]),
                Err(e) => app.notify_fmt("Couldn't write {name}: {e}", &[("name", &path.display().to_string()), ("e", &e.to_string())]),
            };
            match self.save_override.clone() {
                Some(p) => write(self, p.into()),
                None => {
                    let dialog = rfd::AsyncFileDialog::new().set_title(tl!("Export").to_string()).add_filter(kind, &[ext]).set_file_name(name);
                    self.ask_one(crate::pickers::Ask::Save(dialog), None, write);
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (kind, ext);
            if let Err(e) = crate::editing::download(&name, &bytes) {
                self.notify_error(e);
            }
        }
    }
}
