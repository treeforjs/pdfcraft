//! Export a PDF in the background: Word, Excel, PowerPoint, HTML, RTF, SVG, accessible text or
//! plain text, with Run OCR, progress and cancel.
//!
//! [`Session::export_job`] checks the request against the open document (pages, permissions,
//! the recognition language) and detaches what the export needs: the working file, its
//! password and the session's selected OCR engine. [`ExportJob::run`] then works anywhere, on a
//! worker thread for the app: it opens its own copy, recognizes scanned pages when asked
//! ([`Session::with_export_ocr_progress`]), writes the files and returns them. The open
//! document, its history and its dirty state never change.
//!
//! Progress is reported per page read (and per page recognized); returning false from the
//! progress callback stops the job with [`crate::compare::CANCELLED`], writing nothing. Large
//! documents are capped: office formats read at most [`crate::compare::MAX_OFFICE_PAGES`] pages
//! and [`crate::compare::MAX_OFFICE_IMAGE_BYTES`] of images, PowerPoint at most
//! [`pdfcraft_export::MAX_SLIDES`] slides, SVG at most [`MAX_SVG_PAGES`] pages; what is left out
//! is reported as a warning.

use std::cell::RefCell;
use std::sync::Arc;

use crate::compare::{CANCELLED, OfficeFormat};
use crate::export::{Exporter, SvgOptions};
use crate::export_ocr::{ExportOcr, ExportOcrMode, ExportOcrReport};
use crate::ocr::{OcrEngine, OcrSettings};
use crate::pptx_export::{PptxMode, PptxOptions};
use crate::{DocId, Session};

/// SVG export writes at most this many pages (one file each).
pub const MAX_SVG_PAGES: usize = 2000;

/// What to export to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExportTarget {
    /// Word, HTML, RTF or Excel: one file.
    Office(OfficeFormat),
    /// PowerPoint: one slide per page.
    Pptx { mode: PptxMode, dpi: f64 },
    /// One SVG file per page.
    Svg { text_layer: bool },
    /// The tagged text in logical order (needs a structure tree).
    AccessibleText,
    /// Plain text in reading order.
    Text,
}

impl ExportTarget {
    /// The file extension the target writes.
    pub fn extension(self) -> &'static str {
        match self {
            ExportTarget::Office(f) => f.extension(),
            ExportTarget::Pptx { .. } => "pptx",
            ExportTarget::Svg { .. } => "svg",
            ExportTarget::AccessibleText | ExportTarget::Text => "txt",
        }
    }

    /// Whether the target writes several files (one per page), into a folder.
    pub fn per_page(self) -> bool {
        matches!(self, ExportTarget::Svg { .. })
    }

    /// Whether text recognition can add anything to the export: the text formats, and
    /// PowerPoint in editable mode. Pictures (image-mode PowerPoint, SVG, which draws the scan
    /// as it is) and accessible text (read from the tags) don't take recognized text.
    pub fn takes_ocr(self) -> bool {
        match self {
            ExportTarget::Office(_) | ExportTarget::Text => true,
            ExportTarget::Pptx { mode, .. } => mode == PptxMode::Editable,
            ExportTarget::Svg { .. } | ExportTarget::AccessibleText => false,
        }
    }
}

/// Where a running export is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportStage {
    /// Recognizing text on scanned pages: pages done, of total.
    Recognizing(usize, usize),
    /// Reading the pages: pages done, of total.
    Reading(usize, usize),
    /// Writing the file.
    Writing,
}

impl ExportStage {
    /// Rough progress from 0 to 1 (recognition is the first half when it runs).
    pub fn fraction(self) -> f32 {
        let part = |d: usize, t: usize| if t == 0 { 0.0 } else { (d.min(t) as f32) / (t as f32) };
        match self {
            ExportStage::Recognizing(d, t) => 0.5 * part(d, t),
            ExportStage::Reading(d, t) => 0.5 + 0.45 * part(d, t),
            ExportStage::Writing => 0.95,
        }
    }
}

/// One file an export wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportFile {
    /// The file name: the document's name with the target's extension, or for per-page targets
    /// `<stem>_page_<n>.<ext>`.
    pub name: String,
    pub bytes: Vec<u8>,
}

/// What an export produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportOutput {
    pub files: Vec<ExportFile>,
    pub ocr: ExportOcrReport,
    /// Pages, images or slides the caps left out, and other notes.
    pub warnings: Vec<String>,
}

/// An export detached from the session, ready to run on any thread ([`ExportJob::run`]).
pub struct ExportJob {
    name: String,
    stem: String,
    bytes: Arc<Vec<u8>>,
    password: Option<String>,
    engine: Option<Arc<dyn OcrEngine>>,
    pub target: ExportTarget,
    /// 0-based; empty = all.
    pub pages: Vec<usize>,
    pub ocr: ExportOcr,
}

impl Session {
    /// Check an export of document `id` and detach it as a job (see the module notes). Refused
    /// up front when the document doesn't allow copying (except image-mode PowerPoint, which only
    /// renders), when a page doesn't exist, when accessible text is asked of an untagged
    /// document, or when the recognition language is unknown.
    pub fn export_job(&self, id: DocId, target: ExportTarget, pages: &[usize], ocr: &ExportOcr) -> Result<ExportJob, String> {
        let doc = self.get(id).ok_or("the document is not open")?;
        let n = doc.info.pages.len();
        if let Some(p) = pages.iter().find(|p| **p >= n) {
            return Err(format!("page {} does not exist (the document has {n} pages)", p.saturating_add(1)));
        }
        let renders_only = matches!(target, ExportTarget::Pptx { mode: PptxMode::Image, .. });
        if !renders_only {
            doc.require_copying()?;
        }
        if target == ExportTarget::AccessibleText {
            let tags = doc.tags();
            if !tags.exists || tags.nodes.is_empty() {
                return Err("the document is not tagged: it has no structure tree to read accessible text from (add tags first)".into());
            }
        }
        if let ExportTarget::Pptx { dpi, .. } = target
            && !(36.0..=600.0).contains(&dpi)
        {
            return Err(format!("dpi must be between 36 and 600, not {dpi}"));
        }
        let mut ocr = ocr.clone();
        if !target.takes_ocr() {
            // No recognized text goes into this format: no recognition, whatever language.
            ocr = ExportOcr { mode: ExportOcrMode::Off, ..Default::default() };
        }
        ocr.validate()?;
        let engine = if ocr.mode == ExportOcrMode::Off {
            None
        } else {
            self.ocr_engine_for(&OcrSettings { language: ocr.language.clone(), ..Default::default() }).ok()
        };
        let stem = doc.name.trim_end_matches(".pdf").trim_end_matches(".PDF").to_string();
        let mut pages = pages.to_vec();
        pages.dedup();
        Ok(ExportJob { name: doc.name.clone(), stem, bytes: doc.bytes.clone(), password: doc.password.clone(), engine, target, pages, ocr })
    }
}

impl ExportJob {
    /// The file name a single-file export is offered as (`<document stem>.<ext>`).
    pub fn file_name(&self) -> String {
        format!("{}.{}", self.stem, self.target.extension())
    }

    /// Run the export (see the module notes). `progress` returning false cancels it.
    pub fn run(self, progress: impl FnMut(ExportStage) -> bool) -> Result<ExportOutput, String> {
        let progress = RefCell::new(progress);
        let go_on = |stage: ExportStage| progress.try_borrow_mut().map(|mut p| p(stage)).unwrap_or(true);
        if !go_on(ExportStage::Reading(0, 1)) {
            return Err(CANCELLED.into());
        }
        let mut session = Session::new();
        // The engine the document's session reads with; when it can't be used here the export
        // still runs, and recognition reports why it didn't (auto) or fails (on).
        if let Some(engine) = self.engine.clone() {
            let id = engine.id().to_owned();
            if session.ocr_engine_id() != id && session.register_ocr_engine(engine).is_ok() {
                let _ = session.set_ocr_engine(&id);
            }
        }
        let id = session.open(self.name.clone(), None, self.bytes.clone(), self.password.as_deref()).map_err(|e| e.to_string())?;
        let ExportJob { stem, target, pages, ocr, .. } = self;
        let mut recognize = |d: usize, t: usize| go_on(ExportStage::Recognizing(d, t));
        let export = |d: &crate::Document| -> Result<(Vec<ExportFile>, Vec<String>), String> {
            let file = |ext: &str, bytes: Vec<u8>| ExportFile { name: format!("{stem}.{ext}"), bytes };
            match target {
                ExportTarget::Office(format) => {
                    let mut read = |d: usize, t: usize| go_on(ExportStage::Reading(d, t));
                    let (bytes, report) = d.export_office_pages(format, &pages, &mut read)?;
                    if !go_on(ExportStage::Writing) {
                        return Err(CANCELLED.into());
                    }
                    Ok((vec![file(format.extension(), bytes)], report.warnings()))
                }
                ExportTarget::Pptx { mode, dpi } => {
                    if !go_on(ExportStage::Writing) {
                        return Err(CANCELLED.into());
                    }
                    let out = d.export_pptx(&PptxOptions { pages: pages.clone(), mode, dpi })?;
                    Ok((vec![file("pptx", out.bytes)], out.warnings))
                }
                ExportTarget::Svg { text_layer } => {
                    let all: Vec<usize> = if pages.is_empty() { (0..d.info.pages.len()).collect() } else { pages.clone() };
                    let mut warnings = Vec::new();
                    if all.len() > MAX_SVG_PAGES {
                        warnings.push(format!("only the first {MAX_SVG_PAGES} pages were exported ({} left out)", all.len() - MAX_SVG_PAGES));
                    }
                    let mut ex = Exporter::new(d);
                    let options = SvgOptions { text_layer, transparent: false };
                    let total = all.len().min(MAX_SVG_PAGES);
                    let mut files = Vec::with_capacity(total);
                    for (k, p) in all.into_iter().take(MAX_SVG_PAGES).enumerate() {
                        if !go_on(ExportStage::Reading(k, total)) {
                            return Err(CANCELLED.into());
                        }
                        let page = ex.svg(p, &options)?;
                        if page.stats.truncated {
                            warnings.push(format!("page {} was too large and was cut short", p + 1));
                        }
                        files.push(ExportFile { name: format!("{stem}_page_{}.svg", p + 1), bytes: page.svg.into_bytes() });
                    }
                    Ok((files, warnings))
                }
                ExportTarget::AccessibleText => {
                    if !go_on(ExportStage::Writing) {
                        return Err(CANCELLED.into());
                    }
                    let out = d.accessible_text()?;
                    let warnings = if out.truncated { vec!["the text was too long and was cut short".to_string()] } else { Vec::new() };
                    Ok((vec![file("txt", out.text.into_bytes())], warnings))
                }
                ExportTarget::Text => {
                    let all: Vec<usize> = if pages.is_empty() { (0..d.info.pages.len()).collect() } else { pages.clone() };
                    let mut ex = Exporter::new(d);
                    let mut text = String::new();
                    for (k, p) in all.iter().enumerate() {
                        if !go_on(ExportStage::Reading(k, all.len())) {
                            return Err(CANCELLED.into());
                        }
                        if k > 0 {
                            text.push('\u{c}');
                        }
                        text.push_str(&ex.text(*p)?);
                    }
                    Ok((vec![file("txt", text.into_bytes())], Vec::new()))
                }
            }
        };
        let ((files, warnings), report) = session.with_export_ocr_progress(id, &pages, &ocr, &mut recognize, export)?;
        Ok(ExportOutput { files, ocr: report, warnings })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with(pdf: Vec<u8>) -> (Session, DocId) {
        let mut s = Session::new();
        let id = s.open("Report.pdf", None, Arc::new(pdf), None).unwrap();
        (s, id)
    }

    fn two_page_pdf() -> Vec<u8> {
        crate::tests::fixture(2)
    }

    #[test]
    fn office_exports_run_detached_with_progress_and_leave_the_document_alone() {
        let (s, id) = session_with(two_page_pdf());
        let before = s.get(id).unwrap().bytes.clone();
        let ocr = ExportOcr { mode: ExportOcrMode::Off, ..Default::default() };
        let job = s.export_job(id, ExportTarget::Office(OfficeFormat::Docx), &[1], &ocr).unwrap();
        assert_eq!(job.file_name(), "Report.docx");
        let mut stages = Vec::new();
        let out = std::thread::spawn(move || {
            let r = job.run(|st| {
                stages.push(st);
                true
            });
            (r, stages)
        })
        .join()
        .unwrap();
        let (out, stages) = (out.0.unwrap(), out.1);
        assert_eq!(out.files.len(), 1);
        assert!(out.files[0].bytes.starts_with(b"PK"));
        assert!(stages.contains(&ExportStage::Writing) && stages.iter().any(|s| matches!(s, ExportStage::Reading(_, 1))), "{stages:?}");
        assert!(stages.windows(2).all(|w| w[0].fraction() <= w[1].fraction()), "{stages:?}");
        assert_eq!(s.get(id).unwrap().bytes, before);
        assert!(!s.get(id).unwrap().dirty);
    }

    #[test]
    fn cancelling_stops_without_files_and_every_target_runs() {
        let (s, id) = session_with(two_page_pdf());
        let ocr = ExportOcr::default();
        let job = s.export_job(id, ExportTarget::Office(OfficeFormat::Html), &[], &ocr).unwrap();
        let mut calls = 0;
        let r = job.run(|_| {
            calls += 1;
            calls < 3
        });
        assert_eq!(r, Err(CANCELLED.to_string()));
        for target in [
            ExportTarget::Office(OfficeFormat::Xlsx),
            ExportTarget::Office(OfficeFormat::Rtf),
            ExportTarget::Pptx { mode: PptxMode::Editable, dpi: 72.0 },
            ExportTarget::Pptx { mode: PptxMode::Image, dpi: 72.0 },
            ExportTarget::Svg { text_layer: true },
            ExportTarget::Text,
        ] {
            let job = s.export_job(id, target, &[], &ExportOcr { mode: ExportOcrMode::Off, ..Default::default() }).unwrap();
            let out = job.run(|_| true).unwrap_or_else(|e| panic!("{target:?}: {e}"));
            let expected = if target.per_page() { 2 } else { 1 };
            assert_eq!(out.files.len(), expected, "{target:?}");
            assert!(out.files.iter().all(|f| f.name.ends_with(target.extension()) && !f.bytes.is_empty()), "{target:?}");
        }
        let text =
            s.export_job(id, ExportTarget::Text, &[], &ExportOcr { mode: ExportOcrMode::Off, ..Default::default() }).unwrap().run(|_| true).unwrap();
        let text = String::from_utf8(text.files[0].bytes.clone()).unwrap();
        assert!(text.contains("Page 1") && text.contains('\u{c}') && text.contains("Page 2"), "{text}");
    }

    #[test]
    fn bad_export_requests_are_refused_up_front() {
        let (s, id) = session_with(two_page_pdf());
        let ocr = ExportOcr::default();
        assert!(s.export_job(id, ExportTarget::Text, &[2], &ocr).err().unwrap().contains("page 3 does not exist"));
        assert!(s.export_job(id, ExportTarget::Text, &[usize::MAX], &ocr).is_err());
        assert!(s.export_job(id, ExportTarget::AccessibleText, &[], &ocr).err().unwrap().contains("not tagged"));
        assert!(s.export_job(id, ExportTarget::Pptx { mode: PptxMode::Image, dpi: f64::NAN }, &[], &ocr).is_err());
        assert!(s.export_job(id, ExportTarget::Pptx { mode: PptxMode::Image, dpi: 1e9 }, &[], &ocr).is_err());
        let klingon = ExportOcr { language: "tlh".into(), ..Default::default() };
        assert!(s.export_job(id, ExportTarget::Office(OfficeFormat::Docx), &[], &klingon).is_err());
        // A target that takes no recognized text ignores the language.
        assert!(s.export_job(id, ExportTarget::Svg { text_layer: false }, &[], &klingon).is_ok());
        assert!(s.export_job(DocId(9999), ExportTarget::Text, &[], &ocr).is_err());
    }
}
