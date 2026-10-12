//! Revision-bound structural preflight and explicit, undoable fixes.

use crate::{DocId, Edit, EditError, Session};
pub use pdfcraft_preflight::analysis::{Finding, Fixup, Location, Profile, Report, Rule, Settings, Severity};

#[derive(Clone, Debug)]
pub struct ResultSet {
    pub document: DocId,
    pub revision: u64,
    pub report: Report,
}

impl crate::Document {
    pub fn preflight(&self, settings: &Settings) -> Result<ResultSet, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        let report = crate::guard(|| pdfcraft_preflight::analysis::analyze(&editor.cos, settings))
            .map_err(|e| EditError::Invalid(format!("Preflight failed: {e}")))?
            .map_err(EditError::Invalid)?;
        Ok(ResultSet { document: self.id, revision: self.generation, report })
    }
}

impl Session {
    /// Apply exactly the findings selected from this document version. No selection, an
    /// invalid selection, changed document, or denied permissions leaves history untouched.
    pub fn preflight_fix(&mut self, result: &ResultSet, findings: &[usize]) -> Result<ResultSet, EditError> {
        let doc = self.get(result.document).ok_or(EditError::NoDocument)?;
        if doc.generation != result.revision {
            return Err(EditError::Invalid("Preflight results are stale; analyze the document again".into()));
        }
        let fixes = result.report.fixes(findings).map_err(EditError::Invalid)?;
        self.apply(result.document, Edit::PreflightFix { fixes })?;
        self.get(result.document).ok_or(EditError::NoDocument)?.preflight(&result.report.settings)
    }
}

#[cfg(test)]
mod tests;
