//! Standards (PDF/A, PDF/X, PDF/UA and syntax validation) as background jobs.
//!
//! Verifying or converting a large document takes a while, so the Standards window works like
//! Optimize: [`Session::standards_job`] captures a copy of the document and what to do,
//! [`StandardsJob::run`] works anywhere (the UI runs it on a worker thread), and
//! [`Session::finish_standards`] takes the result back. A conversion is worked out on the copy;
//! finishing it makes the converted copy the document as one undo step ([`Edit::Converted`]),
//! unless the document changed in the meantime (then nothing changes and the caller converts
//! again). Every check is bounded by its own limits, so a run always ends; cancelling a run
//! means its result is not taken.

use std::sync::Arc;

use pdfcraft_a11y::{UaOptions, UaReport};
use pdfcraft_preflight::{ArlOptions, ArlReport, Issue, Level, PdfaIntent, XConvertOptions, XOptions, XReport};

use crate::pdfx::PdfxConversion;
use crate::{DocId, Edit, EditError, Session, guard};

/// A converted document carried by [`Edit::Converted`]. Two are equal when they are the same
/// copy.
#[derive(Clone)]
pub struct ConvertedDocument(Arc<pdfcraft_cos::Document>);

impl ConvertedDocument {
    pub fn document(&self) -> &pdfcraft_cos::Document {
        &self.0
    }
}

impl PartialEq for ConvertedDocument {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for ConvertedDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConvertedDocument")
    }
}

/// What a Standards job does.
#[derive(Clone, Debug, PartialEq)]
pub enum StandardsTask {
    /// Verify PDF/A at a level (levels a and u include Unicode and structure checks).
    VerifyPdfa(Level),
    /// Verify PDF/X (the declared part, else PDF/X-4, unless the options name one).
    VerifyPdfx(XOptions),
    /// Verify PDF/UA-1 or -2 (Matterhorn checkpoints that a machine can decide).
    VerifyPdfua(UaOptions),
    /// Validate the object graph against the Arlington PDF model.
    ValidateSyntax(ArlOptions),
    /// Save as PDF/A: fix what can be fixed for `level`, with an optional output intent.
    ConvertPdfa { level: Level, intent: Option<PdfaIntent> },
    /// Convert to PDF/X.
    ConvertPdfx(XConvertOptions),
}

impl StandardsTask {
    /// Whether the task changes the document.
    pub fn converts(&self) -> bool {
        matches!(self, StandardsTask::ConvertPdfa { .. } | StandardsTask::ConvertPdfx(_))
    }
}

/// What a Save as PDF/A did.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfaConversion {
    pub level: Level,
    /// What changed (empty when nothing needed changing or the conversion was refused).
    pub fixed: Vec<String>,
    /// Problems left afterwards (all rules of the level, including those that can't be fixed).
    pub remaining: Vec<Issue>,
    /// Why the conversion was refused (nothing changed): glyphs whose meaning can't be known
    /// for levels a and u, missing tags for level a, an unsuitable output intent.
    pub refused: Option<Vec<Issue>>,
}

/// What a Standards job found.
#[derive(Clone, Debug, PartialEq)]
pub enum StandardsOutcome {
    Pdfa(Level, Vec<Issue>),
    Pdfx(XReport),
    Pdfua(UaReport),
    Syntax(ArlReport),
    PdfaConverted(PdfaConversion),
    PdfxConverted(PdfxConversion),
}

/// A finished job, to hand to [`Session::finish_standards`].
pub struct StandardsResult {
    pub outcome: StandardsOutcome,
    /// The converted copy and its undo label, when the document is to change.
    converted: Option<(String, pdfcraft_cos::Document)>,
    generation: u64,
}

impl StandardsResult {
    /// Whether finishing will change the document.
    pub fn changes_document(&self) -> bool {
        self.converted.is_some()
    }
}

/// What a Standards job needs, independent of the session.
pub struct StandardsJob {
    cos: pdfcraft_cos::Document,
    task: StandardsTask,
    generation: u64,
    date: String,
}

impl Session {
    /// Capture what running `task` on document `id` needs. Conversions need an editable
    /// document; their options are checked here, before any work.
    pub fn standards_job(&self, id: DocId, task: StandardsTask) -> Result<StandardsJob, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let editor = doc.editor.as_ref().ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))?;
        match &task {
            StandardsTask::ConvertPdfx(o) => {
                if !doc.allows_modification() {
                    return Err(EditError::NotPermitted("changes to the document"));
                }
                o.validate(&editor.cos).map_err(EditError::Invalid)?;
            }
            StandardsTask::ConvertPdfa { level, intent } => {
                if !doc.allows_modification() {
                    return Err(EditError::NotPermitted("changes to the document"));
                }
                if let Some(i) = intent {
                    i.validate(*level).map_err(EditError::Invalid)?;
                }
            }
            _ => {}
        }
        let date = self.now().map(pdfcraft_cos::pdf_date).unwrap_or_else(|| "D:19700101000000Z".into());
        Ok(StandardsJob { cos: editor.cos.clone(), task, generation: doc.edit_generation(), date })
    }

    /// Take a finished job's result: a conversion becomes the document as one undo step,
    /// unless the document changed since the job started (an error; nothing changes). Returns
    /// what was found.
    pub fn finish_standards(&mut self, id: DocId, result: StandardsResult) -> Result<StandardsOutcome, EditError> {
        let StandardsResult { outcome, converted, generation } = result;
        let Some((label, cos)) = converted else { return Ok(outcome) };
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        if doc.edit_generation() != generation {
            return Err(EditError::Invalid("The document changed while it was being converted; nothing was changed. Convert again.".into()));
        }
        self.apply(id, Edit::Converted { label, result: ConvertedDocument(Arc::new(cos)) })?;
        Ok(outcome)
    }
}

impl StandardsJob {
    pub fn task(&self) -> &StandardsTask {
        &self.task
    }

    /// Do the work. A panic is returned as an error, so a worker thread always reports back.
    pub fn run(self) -> Result<StandardsResult, EditError> {
        guard(move || self.work()).map_err(|m| EditError::Invalid(format!("the check failed unexpectedly ({m})")))?
    }

    fn work(self) -> Result<StandardsResult, EditError> {
        let StandardsJob { cos, task, generation, date } = self;
        let done = |outcome| Ok(StandardsResult { outcome, converted: None, generation });
        match task {
            StandardsTask::VerifyPdfa(level) => done(StandardsOutcome::Pdfa(level, pdfcraft_a11y::verify_pdfa(&cos, level))),
            StandardsTask::VerifyPdfx(o) => done(StandardsOutcome::Pdfx(pdfcraft_preflight::verify_pdfx(&cos, &o))),
            StandardsTask::VerifyPdfua(o) => done(StandardsOutcome::Pdfua(pdfcraft_a11y::verify_pdfua(&cos, &o))),
            StandardsTask::ValidateSyntax(o) => done(StandardsOutcome::Syntax(pdfcraft_preflight::validate_arlington(&cos, &o))),
            StandardsTask::ConvertPdfx(o) => {
                let (conversion, converted) = crate::pdfx::trial(&cos, &o, &date)?;
                let converted = converted.map(|c| (format!("Convert to {}", o.part.label()), c));
                Ok(StandardsResult { outcome: StandardsOutcome::PdfxConverted(conversion), converted, generation })
            }
            StandardsTask::ConvertPdfa { level, intent } => {
                let blocking = pdfcraft_a11y::pdfa_blockers(&cos, level);
                if !blocking.is_empty() {
                    let remaining = pdfcraft_a11y::verify_pdfa(&cos, level);
                    return done(StandardsOutcome::PdfaConverted(PdfaConversion { level, fixed: Vec::new(), remaining, refused: Some(blocking) }));
                }
                let mut copy = cos.clone();
                match pdfcraft_a11y::convert_pdfa_with_intent(&mut copy, level, intent.as_ref()) {
                    Ok(report) => {
                        let remaining = pdfcraft_a11y::verify_pdfa(&copy, level);
                        let outcome = StandardsOutcome::PdfaConverted(PdfaConversion { level, fixed: report.fixed, remaining, refused: None });
                        Ok(StandardsResult { outcome, converted: Some((format!("Save as {}", level.label()), copy)), generation })
                    }
                    Err(pdfcraft_preflight::ConvertError::Refused { blocking, .. }) => {
                        let remaining = pdfcraft_a11y::verify_pdfa(&cos, level);
                        done(StandardsOutcome::PdfaConverted(PdfaConversion { level, fixed: Vec::new(), remaining, refused: Some(blocking) }))
                    }
                    Err(e) => Err(EditError::Invalid(e.to_string())),
                }
            }
        }
    }
}
