//! Export a PDF (Acrobat's Export a PDF tool): Image (PNG, JPEG, TIFF), all images, text, Word,
//! Excel, PowerPoint, HTML, RTF, SVG and accessible text.
//!
//! One dialog per format: the pages, the format's own choices, and for the text formats the
//! Run text recognition setting. On the desktop the export runs on a worker thread
//! ([`pdfcraft_engine::export_job::ExportJob`] for the document formats) and shows its progress
//! in the progress notice, with Cancel; on the web it runs in place and downloads the files.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use egui::{Align, Layout};
#[cfg(not(target_arch = "wasm32"))]
use pdfcraft_engine::compare::CANCELLED;
use pdfcraft_engine::compare::OfficeFormat;
use pdfcraft_engine::export::{ExportSource, Exporter, ImageFormat};
#[cfg(not(target_arch = "wasm32"))]
use pdfcraft_engine::export_job::ExportJob;
use pdfcraft_engine::export_job::{ExportOutput, ExportStage, ExportTarget};
use pdfcraft_engine::export_ocr::{ExportOcr, ExportOcrMode};
use pdfcraft_engine::pptx_export::PptxMode;

use crate::marks_ui::PageRange;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    Image,
    Text,
    /// Export all images: the images pages use, as files.
    AllImages,
    Word,
    Excel,
    PowerPoint,
    Html,
    Rtf,
    /// One SVG file per page.
    Svg,
    /// The tagged text in logical order.
    AccessibleText,
}

impl ExportKind {
    /// The dialog's title.
    fn title(self) -> &'static str {
        match self {
            ExportKind::Image => tl!("Export to Image"),
            ExportKind::Text => tl!("Export to Text"),
            ExportKind::AllImages => tl!("Export All Images"),
            ExportKind::Word => tl!("Export to Word"),
            ExportKind::Excel => tl!("Export to Excel"),
            ExportKind::PowerPoint => tl!("Export to PowerPoint"),
            ExportKind::Html => tl!("Export to HTML"),
            ExportKind::Rtf => tl!("Export to RTF"),
            ExportKind::Svg => tl!("Export to SVG"),
            ExportKind::AccessibleText => tl!("Export Accessible Text"),
        }
    }

    /// What the export writes, for the dialog.
    fn summary(self) -> &'static str {
        match self {
            ExportKind::Word => {
                tl!("A flowing document: paragraphs in reading order with their fonts and styles, headings, lists, tables and images.")
            }
            ExportKind::Excel => tl!("One worksheet per page: tables as cell grids, numbers as numbers, images as pictures."),
            ExportKind::PowerPoint => tl!("One slide per page."),
            ExportKind::Html => tl!("One web page: headings, paragraphs with their fonts and styles, lists, tables and images inline."),
            ExportKind::Rtf => tl!("Rich text: paragraphs with their fonts and styles, and tables."),
            ExportKind::Svg => tl!("One SVG file per page, named after the document: vector paths, text as outlines, images."),
            ExportKind::AccessibleText => tl!("The tagged text in logical reading order, as a screen reader reads it."),
            _ => "",
        }
    }

    /// The document export the kind runs, with the dialog's choices (`None`: images, which
    /// render or extract pages instead).
    pub fn target(self, d: &ExportDraft) -> Option<ExportTarget> {
        Some(match self {
            ExportKind::Image | ExportKind::AllImages => return None,
            ExportKind::Text => ExportTarget::Text,
            ExportKind::Word => ExportTarget::Office(OfficeFormat::Docx),
            ExportKind::Excel => ExportTarget::Office(OfficeFormat::Xlsx),
            ExportKind::Html => ExportTarget::Office(OfficeFormat::Html),
            ExportKind::Rtf => ExportTarget::Office(OfficeFormat::Rtf),
            ExportKind::PowerPoint => {
                ExportTarget::Pptx { mode: if d.pptx_pictures { PptxMode::Image } else { PptxMode::Editable }, dpi: d.picture_dpi }
            }
            ExportKind::Svg => ExportTarget::Svg { text_layer: d.svg_text_layer },
            ExportKind::AccessibleText => ExportTarget::AccessibleText,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportDraft {
    pub dpi: f64,
    pub range: PageRange,
    pub format: ImageFormat,
    /// Export all images ▸ skip images with fewer pixels than this on their shorter side.
    pub min_side: u32,
    /// Run text recognition on scanned pages (the text formats).
    pub ocr: ExportOcrMode,
    /// Recognition language.
    pub ocr_language: String,
    /// PowerPoint: one picture per slide instead of editable text and pictures.
    pub pptx_pictures: bool,
    /// PowerPoint: resolution of the rendered pictures.
    pub picture_dpi: f64,
    /// SVG: add the transparent, selectable text layer.
    pub svg_text_layer: bool,
}

impl Default for ExportDraft {
    fn default() -> Self {
        Self {
            dpi: 150.0,
            range: PageRange::default(),
            format: ImageFormat::Png,
            min_side: 0,
            ocr: ExportOcrMode::Auto,
            ocr_language: "en".into(),
            pptx_pictures: false,
            picture_dpi: 150.0,
            svg_text_layer: true,
        }
    }
}

/// Progress of a background image export: (done, total, final message once finished).
pub type ExportStatus = Arc<Mutex<Option<(usize, usize, Option<String>)>>>;

/// A document export running on a worker thread.
pub struct ExportRun {
    pub kind: ExportKind,
    pub progress: Arc<Mutex<JobProgress>>,
}

/// Where a document export is, and its message once it is done.
#[derive(Default)]
pub struct JobProgress {
    pub stage: Option<ExportStage>,
    pub cancel: bool,
    pub message: Option<String>,
}

pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens, kind: ExportKind) -> (bool, bool) {
    let count = app.active_ids().and_then(|(_, id)| app.session.get(id)).map(|d| d.info.pages.len()).unwrap_or(0);
    let d = &mut app.export_draft;
    ui.label(egui::RichText::new(kind.title()).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    if kind.target(d).is_some() && kind != ExportKind::Text {
        return job_body(ui, d, t, kind, count);
    }
    if kind == ExportKind::Image {
        ui.horizontal(|ui| {
            ui.label(tl!("Resolution"));
            egui::ComboBox::from_id_salt("export-dpi")
                .selected_text(crate::i18n::fmt(tl!("{dpi} pixels/inch"), &[("dpi", &d.dpi.to_string())]))
                .show_ui(ui, |ui| {
                    for dpi in [72.0, 96.0, 150.0, 300.0, 600.0] {
                        ui.selectable_value(&mut d.dpi, dpi, crate::i18n::fmt(tl!("{dpi} pixels/inch"), &[("dpi", &dpi.to_string())]));
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label(tl!("Format"));
            let mut quality = match d.format {
                ImageFormat::Jpeg { quality } => quality,
                _ => 85,
            };
            egui::ComboBox::from_id_salt("export-format").selected_text(d.format.label()).show_ui(ui, |ui| {
                for f in [ImageFormat::Png, ImageFormat::Jpeg { quality }, ImageFormat::Tiff] {
                    let on = std::mem::discriminant(&d.format) == std::mem::discriminant(&f);
                    if ui.selectable_label(on, f.label()).clicked() {
                        d.format = f;
                    }
                }
            });
            if let ImageFormat::Jpeg { .. } = d.format {
                ui.label(tl!("Quality"));
                if ui.add(egui::Slider::new(&mut quality, 10..=100)).changed() {
                    d.format = ImageFormat::Jpeg { quality };
                }
            }
        });
        ui.label(
            egui::RichText::new(crate::i18n::fmt(tl!("One {f} file per page, named after the document."), &[("f", d.format.label())]))
                .small()
                .color(t.text_faint),
        );
    } else if kind == ExportKind::AllImages {
        ui.horizontal(|ui| {
            ui.label(tl!("Exclude images smaller than"));
            let label = move |n: u32| if n == 0 { tl!("No limit").to_string() } else { format!("{n} pixels") };
            egui::ComboBox::from_id_salt("export-min").selected_text(label(d.min_side)).show_ui(ui, |ui| {
                for n in [0, 16, 32, 64, 128, 256] {
                    ui.selectable_value(&mut d.min_side, n, label(n));
                }
            });
        });
        ui.label(
            egui::RichText::new(tl!("Each image once, named after the document and page. JPEG images are saved unchanged; others as PNG."))
                .small()
                .color(t.text_faint),
        );
    } else {
        ui.label(egui::RichText::new(tl!("Plain text in reading order; pages are separated by form feeds.")).small().color(t.text_faint));
        ui.add_space(6.0);
        ocr_choice(ui, d);
    }
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!("Pages")).font(theme::semibold(12.5)));
    d.range.ui(ui, count);
    ui.add_space(12.0);
    let (mut apply, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let ok = !d.range.pages(count).is_empty();
        if ui.add_enabled_ui(ok, |ui| widgets::pill_button(ui, tl!("Export"), true)).inner.clicked() {
            apply = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            cancel = true;
        }
    });
    (apply, cancel)
}

/// The Run text recognition choice: when to recognize scanned pages first, and in which language.
fn ocr_choice(ui: &mut egui::Ui, d: &mut ExportDraft) {
    let label = |m: ExportOcrMode| match m {
        ExportOcrMode::Auto => tl!("On scanned pages"),
        ExportOcrMode::On => tl!("On every page without text"),
        ExportOcrMode::Off => tl!("Off"),
    };
    egui::Grid::new("export-ocr").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label(tl!("Recognize text"));
        egui::ComboBox::from_id_salt("export-ocr-mode").selected_text(label(d.ocr)).width(220.0).show_ui(ui, |ui| {
            for m in ExportOcrMode::ALL {
                ui.selectable_value(&mut d.ocr, m, label(m));
            }
        });
        ui.end_row();
        if d.ocr != ExportOcrMode::Off {
            ui.label(tl!("Document language"));
            let name = pdfcraft_engine::ocr::LANGUAGES.iter().find(|l| l.0 == d.ocr_language).map_or("English", |l| l.1);
            egui::ComboBox::from_id_salt("export-ocr-language").selected_text(name).width(220.0).show_ui(ui, |ui| {
                for (code, name) in pdfcraft_engine::ocr::LANGUAGES {
                    ui.selectable_value(&mut d.ocr_language, (*code).to_string(), *name);
                }
            });
            ui.end_row();
        }
    });
}

/// The dialog of a document export (Word, Excel, PowerPoint, HTML, RTF, SVG, accessible text).
fn job_body(ui: &mut egui::Ui, d: &mut ExportDraft, t: &Tokens, kind: ExportKind, count: usize) -> (bool, bool) {
    ui.add(egui::Label::new(egui::RichText::new(kind.summary()).small().color(t.text_faint)).wrap());
    ui.add_space(8.0);
    if kind == ExportKind::PowerPoint {
        ui.radio_value(&mut d.pptx_pictures, false, tl!("Editable text boxes and pictures"));
        ui.radio_value(&mut d.pptx_pictures, true, tl!("One picture per page"));
        ui.horizontal(|ui| {
            ui.label(tl!("Picture resolution"));
            egui::ComboBox::from_id_salt("export-picture-dpi")
                .selected_text(crate::i18n::fmt(tl!("{dpi} pixels/inch"), &[("dpi", &d.picture_dpi.to_string())]))
                .show_ui(ui, |ui| {
                    for dpi in [72.0, 96.0, 150.0, 300.0] {
                        ui.selectable_value(&mut d.picture_dpi, dpi, crate::i18n::fmt(tl!("{dpi} pixels/inch"), &[("dpi", &dpi.to_string())]));
                    }
                });
        });
        ui.add_space(6.0);
    }
    if kind == ExportKind::Svg {
        ui.checkbox(&mut d.svg_text_layer, tl!("Add a selectable text layer"));
        ui.add_space(6.0);
    }
    if kind.target(d).is_some_and(ExportTarget::takes_ocr) {
        ocr_choice(ui, d);
        ui.add_space(6.0);
    }
    if kind != ExportKind::AccessibleText {
        ui.label(egui::RichText::new(tl!("Pages")).font(theme::semibold(12.5)));
        d.range.ui(ui, count);
    }
    ui.add_space(12.0);
    let (mut apply, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let ok = kind == ExportKind::AccessibleText || !d.range.pages(count).is_empty();
        if ui.add_enabled_ui(ok, |ui| widgets::pill_button(ui, tl!("Export"), true)).inner.clicked() {
            apply = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            cancel = true;
        }
    });
    (apply, cancel)
}

/// What a finished document export says: where it went, what recognition did, what the caps
/// left out.
fn job_message(out: &ExportOutput, place: &str) -> String {
    let mut msg = crate::i18n::fmt(tl!("Exported to {path}"), &[("path", place)]);
    let n = out.ocr.recognized.len();
    if n > 0 {
        msg.push_str(&crate::i18n::fmt(tl!("; text recognized on {n} scanned pages"), &[("n", &n.to_string())]));
    } else if let Some(why) = &out.ocr.not_run {
        // The engine's reason, in its own words.
        msg.push_str(&format!("; {why}"));
    }
    for w in &out.warnings {
        msg.push_str(&format!("; {w}"));
    }
    msg
}

/// The progress notice's label for a stage.
fn stage_label(kind: ExportKind, stage: Option<ExportStage>) -> String {
    match stage {
        Some(ExportStage::Recognizing(d, t)) => {
            crate::i18n::fmt(tl!("Recognizing text… page {d} of {t}"), &[("d", &(d + 1).min(t.max(1)).to_string()), ("t", &t.max(1).to_string())])
        }
        Some(ExportStage::Reading(d, t)) if t > 1 => {
            crate::i18n::fmt(tl!("Exporting… {d} of {t}"), &[("d", &(d + 1).min(t).to_string()), ("t", &t.to_string())])
        }
        _ => format!("{}…", kind.title()),
    }
}

/// Write the files (`name`, bytes) produced for `pages`, reporting progress.
#[allow(clippy::too_many_arguments)]
fn run(
    src: ExportSource,
    kind: ExportKind,
    dpi: f64,
    format: ImageFormat,
    min_side: u32,
    pages: Vec<usize>,
    stem: String,
    mut sink: impl FnMut(&str, Vec<u8>) -> Result<(), String>,
    status: &ExportStatus,
    cancel: &AtomicBool,
    lang: crate::i18n::Lang,
) -> String {
    // May run on a worker thread: messages are drawn in the UI's language.
    crate::i18n::set_current(lang);
    let total = pages.len();
    let set = |done: usize, msg: Option<String>| {
        if let Ok(mut s) = status.lock() {
            *s = Some((done, total, msg));
        }
    };
    if kind == ExportKind::AllImages {
        set(0, None);
        let out = match pdfcraft_engine::export::extract_images(&src, &pages, min_side) {
            Ok(o) => o,
            Err(e) => return crate::i18n::fmt(tl!("Export stopped: {e}"), &[("e", &e.to_string())]),
        };
        for (k, img) in out.images.iter().enumerate() {
            if let Err(e) = sink(&pdfcraft_engine::export::image_file_name(&stem, img, k + 1), img.data.clone()) {
                return crate::i18n::fmt(tl!("Export stopped: {e}"), &[("e", &e.to_string())]);
            }
        }
        let n = out.images.len();
        let mut msg = if n == 1 {
            crate::i18n::fmt(tl!("Exported 1 image"), &[])
        } else {
            crate::i18n::fmt(tl!("Exported {n} images"), &[("n", &n.to_string())])
        };
        if !out.skipped.is_empty() {
            msg.push_str(&crate::i18n::fmt(
                tl!(" ({s} not exported: {first})"),
                &[("s", &out.skipped.len().to_string()), ("first", &out.skipped[0].2)],
            ));
        }
        return msg;
    }
    let mut ex = Exporter::from_source(src);
    match kind {
        ExportKind::Image => {
            for (k, p) in pages.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    return crate::i18n::fmt(tl!("Export cancelled after {n} pages"), &[("n", &k.to_string())]);
                }
                set(k, None);
                let result = ex.image(*p, dpi, format).and_then(|img| sink(&format!("{stem}_page_{}.{}", p + 1, format.extension()), img));
                if let Err(e) = result {
                    return crate::i18n::fmt(tl!("Export stopped: {e}"), &[("e", &e.to_string())]);
                }
            }
            let mut msg = if total == 1 {
                crate::i18n::fmt(tl!("Exported 1 image"), &[])
            } else {
                crate::i18n::fmt(tl!("Exported {n} images"), &[("n", &total.to_string())])
            };
            // Pages too large for the renderer at the chosen resolution are drawn at the most it
            // allows: say so, rather than leave a smaller image unexplained.
            let lowered: Vec<f64> = pages.iter().map(|p| ex.dpi_used(*p, dpi)).filter(|used| *used < dpi.clamp(18.0, 1200.0) - 0.5).collect();
            if let Some(lowest) = lowered.iter().copied().reduce(f64::min) {
                msg.push_str(&crate::i18n::fmt(
                    tl!(" ({n} at {dpi} dpi, the largest size they can be drawn at)"),
                    &[("n", &lowered.len().to_string()), ("dpi", &format!("{}", lowest.floor()))],
                ));
            }
            msg
        }
        // Handled above, and by document exports.
        _ => {
            set(0, None);
            match ex.text_of(&pages).and_then(|text| sink(&format!("{stem}.txt"), text.into_bytes())) {
                Ok(()) => {
                    if total == 1 {
                        crate::i18n::fmt(tl!("Exported the text of 1 page"), &[])
                    } else {
                        crate::i18n::fmt(tl!("Exported the text of {n} pages"), &[("n", &total.to_string())])
                    }
                }
                Err(e) => crate::i18n::fmt(tl!("Export stopped: {e}"), &[("e", &e.to_string())]),
            }
        }
    }
}

impl PdfCraftApp {
    /// The open Export dialog's Export button, for tests and automation: close the dialog and
    /// start the export with its settings. False when no Export dialog is open.
    pub fn run_export_dialog(&mut self) -> bool {
        let Some(crate::Dialog::Export(kind)) = self.dialog else { return false };
        self.dialog = None;
        self.start_export(kind);
        true
    }

    /// Start exporting the active document with the dialog's settings.
    pub(crate) fn start_export(&mut self, kind: ExportKind) {
        // What's typed in a form field is part of the document (#166).
        if !self.commit_form_typing() {
            return;
        }
        let Some((_, id)) = self.active_ids() else { return };
        if let Some(target) = kind.target(&self.export_draft) {
            return self.start_job_export(kind, id, target);
        }
        let Some(doc) = self.session.get(id) else { return };
        let src = doc.export_source();
        let stem = doc.name.trim_end_matches(".pdf").trim_end_matches(".PDF").to_string();
        let pages = self.export_draft.range.pages(src.pages);
        let dpi = self.export_draft.dpi;
        let format = self.export_draft.format;
        let min_side = self.export_draft.min_side;
        let status: ExportStatus = Arc::new(Mutex::new(Some((0, pages.len(), None))));
        let cancel = Arc::new(AtomicBool::new(false));
        self.export_cancel = cancel.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let start = move |app: &mut Self, dir: std::path::PathBuf, inline: bool| {
                let st = status.clone();
                let cancel = cancel.clone();
                let shown = dir.display().to_string();
                let lang = crate::i18n::current();
                let work = move || {
                    let sink = |name: &str, bytes: Vec<u8>| {
                        crate::editing::write_atomically(&dir.join(name).to_string_lossy(), &bytes).map_err(|e| format!("{name}: {e}"))
                    };
                    let msg = run(src, kind, dpi, format, min_side, pages, stem, sink, &st, &cancel, lang);
                    if let Ok(mut s) = st.lock() {
                        let (done, total) = s.as_ref().map_or((0, 0), |(d, t, _)| (*d, *t));
                        let done_msg = crate::i18n::fmt(tl!("{msg} to {dir}"), &[("msg", &msg), ("dir", &shown)]);
                        *s = Some((done.max(total), total, Some(done_msg)));
                    }
                };
                if inline {
                    work(); // tests and automation: synchronous
                } else {
                    std::thread::Builder::new().name("pdfcraft-export".into()).spawn(work).ok();
                }
                app.export_status = Some(status);
            };
            match self.export_dir_override.clone() {
                Some(d) => start(self, d.into(), true),
                None => {
                    let dialog = rfd::AsyncFileDialog::new().set_title(tl!("Choose a folder for the exported files").to_string());
                    self.ask_one(crate::pickers::Ask::Folder(dialog), None, move |app, dir| start(app, dir, false));
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let msg = run(
                src,
                kind,
                dpi,
                format,
                min_side,
                pages,
                stem,
                |name, bytes| crate::editing::download(name, &bytes),
                &status,
                &cancel,
                crate::i18n::current(),
            );
            if let Ok(mut s) = status.lock() {
                *s = Some((0, 0, Some(msg)));
            }
            self.export_status = Some(status);
        }
    }

    /// Export a document format ([`ExportJob`]): check it now, ask where to save, then run it on
    /// a worker thread (in place in tests, with the save or folder override, and on the web).
    fn start_job_export(&mut self, kind: ExportKind, id: pdfcraft_engine::DocId, target: ExportTarget) {
        if self.export_run.is_some() {
            self.notify_tr("An export is already running");
            return;
        }
        let count = self.session.get(id).map_or(0, |d| d.info.pages.len());
        let d = &self.export_draft;
        let pages = if kind == ExportKind::AccessibleText || d.range.all { Vec::new() } else { d.range.pages(count) };
        let ocr = ExportOcr { mode: d.ocr, language: d.ocr_language.clone() };
        let job = match self.session.export_job(id, target, &pages, &ocr) {
            Ok(job) => job,
            Err(e) => return self.notify_fmt("Export stopped: {e}", &[("e", &e)]),
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let ext = target.extension();
            if target.per_page() {
                match self.export_dir_override.clone() {
                    Some(dir) => self.run_job(kind, job, std::path::PathBuf::from(dir), true),
                    None => {
                        let dialog = rfd::AsyncFileDialog::new().set_title(tl!("Choose a folder for the exported files").to_string());
                        self.ask_one(crate::pickers::Ask::Folder(dialog), None, move |app, dir| app.run_job(kind, job, dir, true));
                    }
                }
            } else {
                let fixed = self.save_override.clone().map(std::path::PathBuf::from).or_else(|| {
                    // Tests and automation that set only the export folder.
                    self.export_dir_override.clone().map(|d| std::path::PathBuf::from(d).join(job.file_name()))
                });
                match fixed {
                    Some(p) => self.run_job(kind, job, p, false),
                    None => {
                        let dialog = rfd::AsyncFileDialog::new()
                            .set_title(tl!("Export").to_string())
                            .add_filter(ext.to_uppercase(), &[ext])
                            .set_file_name(job.file_name());
                        self.ask_one(crate::pickers::Ask::Save(dialog), None, move |app, path| app.run_job(kind, job, path, false));
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let progress = Arc::new(Mutex::new(JobProgress::default()));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run(|_| true)))
                .unwrap_or_else(|_| Err("the export failed unexpectedly".into()));
            let msg = match result {
                Ok(out) => {
                    let mut failed = None;
                    for f in &out.files {
                        if let Err(e) = crate::editing::download(&f.name, &f.bytes) {
                            failed = Some(e);
                        }
                    }
                    match failed {
                        Some(e) => crate::i18n::fmt(tl!("Export stopped: {e}"), &[("e", &e)]),
                        None => job_message(&out, &out.files.first().map(|f| f.name.clone()).unwrap_or_default()),
                    }
                }
                Err(e) => crate::i18n::fmt(tl!("Export stopped: {e}"), &[("e", &e)]),
            };
            if let Ok(mut p) = progress.lock() {
                p.message = Some(msg);
            }
            self.export_run = Some(ExportRun { kind, progress });
            self.poll_export();
        }
    }

    /// Run `job` on a worker thread, writing its file to `dest` (or its files into the folder
    /// `dest` when `folder`).
    #[cfg(not(target_arch = "wasm32"))]
    fn run_job(&mut self, kind: ExportKind, job: ExportJob, dest: std::path::PathBuf, folder: bool) {
        if self.export_run.is_some() {
            self.notify_tr("An export is already running");
            return;
        }
        let progress = Arc::new(Mutex::new(JobProgress::default()));
        let p = progress.clone();
        let lang = crate::i18n::current();
        let work = move || {
            // Messages are drawn in the UI's language.
            crate::i18n::set_current(lang);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                job.run(|stage| {
                    let Ok(mut s) = p.lock() else { return false };
                    s.stage = Some(stage);
                    !s.cancel
                })
            }))
            .unwrap_or_else(|_| Err("the export failed unexpectedly".into()));
            let cancelled = p.lock().map(|s| s.cancel).unwrap_or(false);
            let msg = match result {
                Err(e) if e == CANCELLED || cancelled => tl!("Export cancelled").to_string(),
                Err(e) => crate::i18n::fmt(tl!("Export stopped: {e}"), &[("e", &e)]),
                Ok(out) => {
                    let mut written = Ok(());
                    if folder {
                        if let Err(e) = std::fs::create_dir_all(&dest) {
                            written = Err(format!("{}: {e}", dest.display()));
                        }
                        for f in &out.files {
                            if written.is_err() {
                                break;
                            }
                            written = crate::editing::write_atomically(&dest.join(&f.name).to_string_lossy(), &f.bytes)
                                .map_err(|e| format!("{}: {e}", f.name));
                        }
                    } else if let Some(f) = out.files.first() {
                        written = crate::editing::write_atomically(&dest.to_string_lossy(), &f.bytes).map_err(|e| format!("{}: {e}", dest.display()));
                    }
                    match written {
                        Ok(()) => job_message(&out, &dest.display().to_string()),
                        Err(e) => crate::i18n::fmt(tl!("Export stopped: {e}"), &[("e", &e)]),
                    }
                }
            };
            if let Ok(mut s) = p.lock() {
                s.message = Some(msg);
            }
        };
        if self.run_inline {
            work();
        } else if let Err(e) = std::thread::Builder::new().name("pdfcraft-export".into()).spawn(work) {
            return self.notify_fmt("Export stopped: {e}", &[("e", &e.to_string())]);
        }
        self.export_run = Some(ExportRun { kind, progress });
        self.poll_export();
    }

    /// Stop a running export: nothing more is written.
    pub fn cancel_export(&mut self) {
        self.export_cancel.store(true, Ordering::Relaxed);
        if let Some(r) = &self.export_run
            && let Ok(mut s) = r.progress.lock()
        {
            s.cancel = true;
        }
    }

    /// Pick up export progress and results now, as each frame does (tests without a frame loop).
    pub fn poll_export_for_tests(&mut self) {
        self.poll_export();
    }

    /// Whether a document export is running.
    pub fn exporting(&self) -> bool {
        self.export_run.is_some()
    }

    /// Show document export progress (with Cancel) and the result once it is done.
    fn poll_job_export(&mut self) {
        let Some(run) = self.export_run.as_ref() else { return };
        let (kind, progress) = (run.kind, run.progress.clone());
        let Ok(mut s) = progress.lock() else {
            self.export_run = None;
            self.progress_notice = None;
            self.notify_tr("Export stopped unexpectedly");
            return;
        };
        if let Some(msg) = s.message.take() {
            drop(s);
            self.export_run = None;
            self.progress_notice = None;
            self.notify(msg);
            return;
        }
        let (stage, cancelling) = (s.stage, s.cancel);
        drop(s);
        let label = if cancelling { tl!("Cancelling…").to_string() } else { stage_label(kind, stage) };
        let fraction = stage.map_or(0.0, ExportStage::fraction);
        self.progress_notice = Some(crate::widgets::ProgressNotice { label, fraction, cancellable: !cancelling });
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// Show export progress, and the result once it is done.
    pub(crate) fn poll_export(&mut self) {
        self.poll_job_export();
        let Some(st) = self.export_status.clone() else { return };
        let snapshot = st.lock().ok().and_then(|s| s.clone());
        match snapshot {
            Some((_, _, Some(msg))) => {
                self.export_status = None;
                if self.export_run.is_none() {
                    self.progress_notice = None;
                }
                self.notify(msg);
            }
            Some((done, total, None)) if total > 1 => {
                let cancelling = self.export_cancel.load(Ordering::Relaxed);
                let label = if cancelling {
                    tl!("Cancelling…").to_string()
                } else {
                    crate::i18n::fmt(tl!("Exporting… {d} of {t}"), &[("d", &(done + 1).min(total).to_string()), ("t", &total.to_string())])
                };
                let fraction = done as f32 / total as f32;
                self.progress_notice = Some(crate::widgets::ProgressNotice { label, fraction, cancellable: !cancelling });
            }
            _ => {}
        }
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}
