//! Revision-bound structural preflight and explicit, undoable fixes.

use crate::{DocId, Edit, EditError, Session};
pub use pdfcraft_preflight::analysis::{Finding, Fixup, Location, Profile, Report, Rule, Settings, Severity};
pub use pdfcraft_preflight::coverage::{Options as CoverageOptions, PageCoverage, Region as CoverageRegion};

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

    /// Total area coverage (Output Preview): the most ink each of `pages` (0-based) puts down
    /// and where it exceeds `options.limit`. Read-only; see [`pdfcraft_preflight::coverage`]
    /// for what is exact, what is converted from RGB and what is not measured.
    pub fn ink_coverage(&self, pages: &[usize], options: &CoverageOptions) -> Result<Vec<PageCoverage>, EditError> {
        options.validate().map_err(EditError::Invalid)?;
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        pages
            .iter()
            .map(|&page| {
                crate::guard(|| pdfcraft_preflight::coverage::page_coverage(&editor.cos, page, options))
                    .map_err(|e| EditError::Invalid(format!("Ink coverage failed on page {}: {e}", page.saturating_add(1))))?
                    .map_err(EditError::Invalid)
            })
            .collect()
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
