//! Scan & OCR ▸ Recognize text: the Recognize Text dialog (pages, language, output, resolution)
//! and the background run with its progress. Recognition happens on a worker thread; the result
//! is reviewed by default, then applied to the captured document version as one undoable edit.

use std::sync::{Arc, Mutex};

use egui::{Align, Layout};
use pdfcraft_engine::DocId;
use pdfcraft_engine::ocr::{LANGUAGES, OcrReview, OcrSettings};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Which pages to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OcrPages {
    All,
    Current,
    Range,
}

/// The dialog's choices (kept between runs, as in PDF).
#[derive(Clone, Debug, PartialEq)]
pub struct OcrDraft {
    pub pages: OcrPages,
    pub from: usize,
    pub to: usize,
    pub language: String,
    /// "Downsample to" resolution.
    pub dpi: u32,
    pub review: bool,
    /// Detect turned pages and read them upright (the page image is not changed).
    pub auto_rotate: bool,
    /// Detect skewed pages and read them straightened (the page image is not changed).
    pub deskew: bool,
    /// Output Searchable Image: also straighten the pages themselves (`OcrSettings::straighten`).
    pub straighten: bool,
}

impl Default for OcrDraft {
    fn default() -> Self {
        OcrDraft {
            pages: OcrPages::All,
            from: 1,
            to: 1,
            language: "en".into(),
            dpi: 300,
            review: true,
            auto_rotate: false,
            deskew: false,
            straighten: false,
        }
    }
}

/// Progress of a run: pages done, total, the result once finished, and a cancel request.
#[derive(Default)]
pub struct OcrProgress {
    pub done: usize,
    pub total: usize,
    pub result: Option<Result<OcrReview, String>>,
    pub cancel: bool,
}

/// Recognize text in multiple files: files done, total, and the summary once finished.
#[derive(Default)]
pub struct BatchProgress {
    pub done: usize,
    pub total: usize,
    pub message: Option<String>,
}

pub struct OcrRun {
    pub doc: DocId,
    pub progress: Arc<Mutex<OcrProgress>>,
    review_first: bool,
    review: Option<ReviewDraft>,
}

struct ReviewDraft {
    result: OcrReview,
    locations: Vec<(usize, usize)>,
    selected: usize,
    text: String,
    error: Option<String>,
}

impl ReviewDraft {
    fn new(result: OcrReview) -> Self {
        let locations = result.pages().iter().enumerate().flat_map(|(p, page)| (0..page.words.len()).map(move |w| (p, w))).collect();
        let mut draft = Self { result, locations, selected: 0, text: String::new(), error: None };
        draft.load();
        draft
    }

    fn load(&mut self) {
        self.text = self
            .locations
            .get(self.selected)
            .and_then(|(p, w)| self.result.pages().get(*p)?.words.get(*w))
            .map(|w| w.text.clone())
            .unwrap_or_default();
        self.error = None;
    }

    fn save(&mut self) -> bool {
        let Some(&(slot, word)) = self.locations.get(self.selected) else { return true };
        let Some(page) = self.result.pages().get(slot).map(|p| p.page) else { return false };
        match self.result.correct(page, word, &self.text) {
            Ok(()) => {
                self.error = None;
                true
            }
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }
}

pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> (bool, bool) {
    let pages = app.active_ids().and_then(|(_, id)| app.session.get(id)).map_or(1, |d| d.info.pages.len().max(1));
    let available = pdfcraft_engine::ocr::available();
    let d = &mut app.ocr_draft;
    d.to = d.to.clamp(1, pages);
    d.from = d.from.clamp(1, d.to);
    ui.label(egui::RichText::new(tl!("Recognize Text")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    let group = |ui: &mut egui::Ui, title: &str, body: &mut dyn FnMut(&mut egui::Ui)| {
        ui.label(egui::RichText::new(tl!(title)).font(theme::semibold(13.0)));
        egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
        ui.add_space(8.0);
    };
    group(ui, "Pages", &mut |ui| {
        ui.radio_value(&mut d.pages, OcrPages::All, tl!("All pages"));
        ui.radio_value(&mut d.pages, OcrPages::Current, tl!("Current page"));
        ui.horizontal(|ui| {
            ui.radio_value(&mut d.pages, OcrPages::Range, tl!("From"));
            let on = d.pages == OcrPages::Range;
            ui.add_enabled(on, egui::DragValue::new(&mut d.from).range(1..=pages));
            ui.label(tl!("to"));
            ui.add_enabled(on, egui::DragValue::new(&mut d.to).range(1..=pages));
        });
    });
    group(ui, "Settings", &mut |ui| {
        egui::Grid::new("ocr-settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(tl!("Document language"));
            let name = LANGUAGES.iter().find(|l| l.0 == d.language).map_or("English", |l| l.1);
            egui::ComboBox::from_id_salt("ocr-language").selected_text(name).width(220.0).show_ui(ui, |ui| {
                for (code, name) in LANGUAGES {
                    ui.selectable_value(&mut d.language, (*code).to_string(), *name);
                }
            });
            ui.end_row();
            ui.label(tl!("Output"));
            let output = if d.straighten { tl!("Searchable Image") } else { tl!("Searchable Image (Exact)") };
            egui::ComboBox::from_id_salt("ocr-output").selected_text(output).width(220.0).show_ui(ui, |ui| {
                if ui
                    .selectable_label(d.straighten, tl!("Searchable Image"))
                    .on_hover_text(tl!(
                        "Adds invisible text, straightens turned or skewed pages and downsamples finer images to the chosen resolution"
                    ))
                    .clicked()
                    && !d.straighten
                {
                    d.straighten = true;
                    d.deskew = true;
                }
                if ui
                    .selectable_label(!d.straighten, tl!("Searchable Image (Exact)"))
                    .on_hover_text(tl!("Adds invisible text over each word; the page image is not changed"))
                    .clicked()
                {
                    d.straighten = false;
                }
            });
            ui.end_row();
            ui.label(tl!("Downsample to"));
            egui::ComboBox::from_id_salt("ocr-dpi").selected_text(format!("{} dpi", d.dpi)).width(220.0).show_ui(ui, |ui| {
                for v in [600, 300, 150, 72] {
                    ui.selectable_value(&mut d.dpi, v, format!("{v} dpi"));
                }
            });
            ui.end_row();
        });
        let hint = if d.straighten {
            tl!("Detects turned or skewed pages and straightens them")
        } else {
            tl!("Detects turned or skewed pages and reads them upright; the page image is not changed")
        };
        ui.checkbox(&mut d.auto_rotate, tl!("Detect page rotation")).on_hover_text(hint);
        ui.checkbox(&mut d.deskew, tl!("Deskew pages")).on_hover_text(hint);
        ui.checkbox(&mut d.review, tl!("Review recognized text before applying"));
    });
    if !available {
        ui.label(
            egui::RichText::new(tl!("Text recognition isn't installed: its model files are missing. Reinstall PdfCraft, or set PDFCRAFT_MODELS to the folder that holds them."))
                .small()
                .color(t.text_muted),
        );
        ui.add_space(6.0);
    }
    ui.add_space(6.0);
    let (mut go, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add_enabled_ui(available, |ui| widgets::pill_button(ui, tl!("Recognize text"), true)).inner.clicked() {
                go = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                cancel = true;
            }
        })
    });
    (go, cancel)
}

impl PdfCraftApp {
    /// Recognize text on the pages chosen in the dialog, in the background.
    pub fn start_ocr(&mut self) {
        let Some((vi, id)) = self.active_ids() else { return };
        if self.ocr_run.is_some() {
            self.notify_tr("Text recognition is already running");
            return;
        }
        // Copy and modification authority, before any page is rendered or the models load.
        if let Err(why) = self.session.ocr_authority(id) {
            self.notify_error(why);
            return;
        }
        let d = &self.ocr_draft;
        let pages: Vec<usize> = match d.pages {
            OcrPages::All => Vec::new(),
            OcrPages::Current => vec![self.views[vi].current],
            OcrPages::Range => (d.from.saturating_sub(1)..d.to).collect(),
        };
        let settings = OcrSettings {
            dpi: d.dpi as f32,
            language: d.language.clone(),
            auto_rotate: d.auto_rotate,
            deskew: d.deskew,
            straighten: d.straighten,
            ..Default::default()
        };
        let job = match self.session.ocr_job_checked(id, &pages, settings) {
            Ok(job) => job,
            Err(error) => {
                self.notify_error(error);
                return;
            }
        };
        let review_first = d.review;
        let progress = Arc::new(Mutex::new(OcrProgress { total: job.pages.len(), ..Default::default() }));
        let p = progress.clone();
        let work = move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                job.run_selected(|done, total| {
                    let Ok(mut s) = p.lock() else { return false };
                    s.done = done;
                    s.total = total;
                    !s.cancel
                })
            }))
            .unwrap_or_else(|_| Err("text recognition failed unexpectedly; no text was applied".into()));
            if let Ok(mut s) = p.lock() {
                s.result = Some(result);
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if self.run_inline {
            work();
        } else {
            if let Err(error) = std::thread::Builder::new().name("pdfcraft-ocr".into()).spawn(work) {
                self.notify_error(format!("could not start text recognition: {error}"));
                return;
            }
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.ocr_run = Some(OcrRun { doc: id, progress, review_first, review: None });
        self.poll_ocr();
    }

    /// Recognize text in each file, writing the searchable copies into a folder the user picks
    /// (the export folder override in tests), under the same names.
    pub fn ocr_files(&mut self, files: Vec<(String, Vec<u8>)>) {
        if self.ocr_batch.is_some() {
            self.notify_tr("Text recognition is already running");
            return;
        }
        // The settings showing now, not when the folder arrives.
        let d = &self.ocr_draft;
        let settings = OcrSettings {
            dpi: d.dpi as f32,
            language: d.language.clone(),
            auto_rotate: d.auto_rotate,
            deskew: d.deskew,
            straighten: d.straighten,
            ..Default::default()
        };
        #[cfg(not(target_arch = "wasm32"))]
        match self.export_dir_override.clone() {
            Some(d) => self.ocr_files_into(files, settings, d.into()),
            None => {
                let dialog = rfd::AsyncFileDialog::new().set_title(tl!("Choose a folder for the searchable files").to_string());
                self.ask_one(crate::pickers::Ask::Folder(dialog), None, move |app, dir| app.ocr_files_into(files, settings, dir));
            }
        }
        #[cfg(target_arch = "wasm32")]
        self.ocr_files_into(files, settings);
    }

    /// [`Self::ocr_files`] once the folder for the searchable files is known.
    fn ocr_files_into(&mut self, files: Vec<(String, Vec<u8>)>, settings: OcrSettings, #[cfg(not(target_arch = "wasm32"))] dir: std::path::PathBuf) {
        // Another batch may have started while the folder picker was open.
        if self.ocr_batch.is_some() {
            self.notify_tr("Text recognition is already running");
            return;
        }
        let progress = Arc::new(Mutex::new(BatchProgress { total: files.len(), ..Default::default() }));
        let p = progress.clone();
        // The session's selected engine, readied on the worker thread.
        let engine = self.session.ocr_engine_for(&settings);
        // The summary is written on the worker thread: draw it in the UI's language.
        let lang = crate::i18n::current();
        let work = move || {
            crate::i18n::set_current(lang);
            let (mut ok, mut words, mut failed) = (0, 0, Vec::new());
            let language = settings.language.clone();
            match engine.and_then(|e| e.prepare(&language).map(|_| e).map_err(|e| e.to_string())) {
                Err(e) => failed.push(e),
                Ok(ocr) => {
                    for (i, (name, bytes)) in files.into_iter().enumerate() {
                        if let Ok(mut s) = p.lock() {
                            s.done = i;
                        }
                        let r = pdfcraft_engine::ocr::recognize_file(&name, Arc::new(bytes), None, settings.clone(), &ocr, |_, _| true);
                        let saved = r.and_then(|r| {
                            #[cfg(not(target_arch = "wasm32"))]
                            crate::editing::write_atomically(&dir.join(&name).to_string_lossy(), &r.bytes).map_err(|e| e.to_string())?;
                            #[cfg(target_arch = "wasm32")]
                            crate::editing::download(&name, &r.bytes)?;
                            Ok(r.words())
                        });
                        match saved {
                            Ok(n) => {
                                ok += 1;
                                words += n;
                            }
                            Err(e) => failed.push(format!("{name}: {}", e)),
                        }
                    }
                }
            }
            let done = if ok == 1 {
                crate::i18n::fmt(tl!("Recognized {w} words in 1 file"), &[("w", &words.to_string())])
            } else {
                crate::i18n::fmt(tl!("Recognized {w} words in {n} files"), &[("w", &words.to_string()), ("n", &ok.to_string())])
            };
            let mut msg = done;
            if !failed.is_empty() {
                msg.push_str(&crate::i18n::fmt(tl!("; failed: {list}"), &[("list", &failed.join("; "))]));
            }
            if let Ok(mut s) = p.lock() {
                s.done = s.total;
                s.message = Some(msg);
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if self.run_inline {
            work();
        } else {
            std::thread::Builder::new().name("pdfcraft-ocr-files".into()).spawn(work).ok();
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.ocr_batch = Some(progress);
        self.poll_ocr();
    }

    /// Stop recognition or discard its draft. Cancellation never applies partial pages.
    pub fn cancel_ocr(&mut self) {
        if self.ocr_run.as_ref().is_some_and(|r| r.review.is_some()) {
            self.ocr_run = None;
            self.notify_tr("Text recognition canceled; no text was applied");
            return;
        }
        if let Some(r) = &self.ocr_run
            && let Ok(mut s) = r.progress.lock()
        {
            s.cancel = true;
        }
    }

    /// Show progress; apply the result once the worker is done.
    pub(crate) fn poll_ocr(&mut self) {
        if let Some(b) = self.ocr_batch.clone() {
            let msg = b.lock().ok().map(|mut s| s.message.take().ok_or((s.done, s.total)));
            match msg {
                Some(Ok(m)) => {
                    self.ocr_batch = None;
                    self.notify(m);
                }
                Some(Err((done, total))) => {
                    let m = crate::i18n::fmt(
                        tl!("Recognizing text… file {d} of {t}"),
                        &[("d", &(done + 1).min(total.max(1)).to_string()), ("t", &total.max(1).to_string())],
                    );
                    if self.toast.as_ref().is_none_or(|t| t.0 != m) {
                        self.notify(m);
                    }
                    if let Some(ctx) = &self.ctx {
                        ctx.request_repaint_after(std::time::Duration::from_millis(200));
                    }
                }
                None => self.ocr_batch = None,
            }
        }
        if self.ocr_run.as_ref().is_some_and(|r| r.review.is_some()) {
            self.show_ocr_review();
            return;
        }
        let Some(run) = self.ocr_run.as_ref() else { return };
        let (doc, progress) = (run.doc, run.progress.clone());
        let Ok(mut s) = progress.lock() else { return };
        let Some(result) = s.result.take() else {
            let msg = crate::i18n::fmt(
                tl!("Recognizing text… page {d} of {t}"),
                &[("d", &(s.done + 1).min(s.total.max(1)).to_string()), ("t", &s.total.max(1).to_string())],
            );
            drop(s);
            if self.toast.as_ref().is_none_or(|t| t.0 != msg) {
                self.notify(msg);
            }
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
            return;
        };
        let canceled = s.cancel;
        drop(s);
        if canceled {
            self.ocr_run = None;
            self.notify_tr("Text recognition canceled; no text was applied");
            return;
        }
        let review = match result {
            Ok(f) => f,
            Err(e) => {
                self.ocr_run = None;
                self.notify_error(e);
                return;
            }
        };
        if self.ocr_run.as_ref().is_some_and(|r| r.review_first) {
            if let Some(run) = self.ocr_run.as_mut() {
                run.review = Some(ReviewDraft::new(review));
            }
            self.show_ocr_review();
            return;
        }
        self.ocr_run = None;
        self.finish_ocr_review(doc, &review);
    }

    fn finish_ocr_review(&mut self, doc: DocId, review: &OcrReview) {
        let found = review.pages();
        let read = found.iter().filter(|p| p.skipped.is_none()).count();
        let skipped = found.len() - read;
        match self.session.apply_ocr_review(review) {
            Ok(words) => {
                if let Some(info) = self.session.get(doc).map(|d| d.info.clone())
                    && let Some(view) = self.views.iter_mut().find(|v| v.id == doc)
                {
                    view.document_changed(&info);
                }
                let words_part = if words == 1 {
                    crate::i18n::fmt(tl!("Recognized 1 word"), &[])
                } else {
                    crate::i18n::fmt(tl!("Recognized {n} words"), &[("n", &words.to_string())])
                };
                let pages_part = if read == 1 { tl!("1 page").to_string() } else { crate::i18n::fmt(tl!("{n} pages"), &[("n", &read.to_string())]) };
                let mut msg = crate::i18n::fmt(tl!("{words} on {pages}"), &[("words", &words_part), ("pages", &pages_part)]);
                if skipped > 0 {
                    msg.push_str(&if skipped == 1 {
                        crate::i18n::fmt(tl!("; 1 page skipped"), &[])
                    } else {
                        crate::i18n::fmt(tl!("; {n} pages skipped"), &[("n", &skipped.to_string())])
                    });
                }
                self.notify(msg);
            }
            Err(e) => self.notify_error(e),
        }
    }

    fn show_ocr_review(&mut self) {
        let Some(ctx) = self.ctx.clone() else { return };
        let Some(doc) = self.ocr_run.as_ref().map(|r| r.doc) else { return };
        // The words are shown only while the document is open and still allows copying and
        // changes; otherwise the draft is discarded without showing them.
        if let Err(error) = self.session.ocr_authority(doc) {
            self.ocr_run = None;
            self.notify_error(error);
            return;
        }
        let Some(draft) = self.ocr_run.as_mut().and_then(|r| r.review.as_mut()) else { return };
        let stale = self.session.validate_ocr_review(&draft.result).err();
        let (mut apply, mut cancel) = (false, ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)));
        let mut open = true;
        egui::Window::new(tl!("Review recognized text"))
            .id(egui::Id::new("ocr-review"))
            .open(&mut open)
            .collapsible(false)
            .default_width(540.0)
            .show(&ctx, |ui| {
                ui.label(tl!("Correct the words before adding searchable text. The page image will not change."));
                if let Some(error) = &stale {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if let Some(&(slot, word)) = draft.locations.get(draft.selected) {
                    if let Some(page) = draft.result.pages().get(slot) {
                        ui.label(crate::i18n::fmt(
                            tl!("Page {p}, word {w} of {n}"),
                            &[("p", &(page.page + 1).to_string()), ("w", &(word + 1).to_string()), ("n", &page.words.len().to_string())],
                        ));
                    }
                    let label = ui.label(tl!("Recognized word"));
                    ui.add_enabled(
                        stale.is_none(),
                        egui::TextEdit::singleline(&mut draft.text).id(egui::Id::new("ocr-review-text")).desired_width(f32::INFINITY),
                    )
                    .labelled_by(label.id);
                    ui.label(tl!("Leave a word empty to omit it from the searchable text."));
                    ui.horizontal(|ui| {
                        if ui.add_enabled(draft.selected > 0, egui::Button::new(tl!("Previous word"))).clicked() && draft.save() {
                            draft.selected -= 1;
                            draft.load();
                        }
                        if ui.add_enabled(draft.selected + 1 < draft.locations.len(), egui::Button::new(tl!("Next word"))).clicked() && draft.save() {
                            draft.selected += 1;
                            draft.load();
                        }
                    });
                } else {
                    ui.label(tl!("No words were recognized."));
                }
                let skipped = draft.result.pages().iter().filter(|p| p.skipped.is_some()).count();
                if skipped > 0 {
                    egui::CollapsingHeader::new(tl!("Skipped pages")).show(ui, |ui| {
                        for page in draft.result.pages().iter().filter(|p| p.skipped.is_some()) {
                            if let Some(reason) = &page.skipped {
                                ui.label(format!("{}: {reason}", page.page + 1));
                            }
                        }
                    });
                }
                if let Some(error) = &draft.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.horizontal(|ui| {
                    apply = ui.add_enabled(stale.is_none(), egui::Button::new(tl!("Apply recognized text"))).clicked() && draft.save();
                    cancel |= ui.button(tl!("Cancel")).clicked();
                });
            });
        if cancel || !open {
            self.cancel_ocr();
        } else if apply
            && let Some(run) = self.ocr_run.take()
            && let Some(draft) = run.review
        {
            self.finish_ocr_review(run.doc, &draft.result);
        }
    }
}
