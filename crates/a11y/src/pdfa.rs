//! PDF/A level a (ISO 19005-1 §6.8, ISO 19005-2/-3 §6.7): the logical-structure requirements,
//! checked with the PDF/UA validator's structure tree and content scan ([`verify_pdfua`]) and
//! reported against ISO 19005 clauses, plus the complete PDF/A verification and conversion for
//! every level (the level-b and Unicode rules come from `pdfcraft-preflight`).
//!
//! Level a here means: MarkInfo `Marked` true, a structure tree, all content tagged or marked
//! as an artifact (properly nested, every MCID in the tree), standard structure types or a role
//! map to them, the natural language (`Lang`), and alternate text for figures. Formulas without
//! alternate text and word breaks made by positioning are warnings.

use pdfcraft_cos::Document;
use pdfcraft_preflight::{ConvertError, Issue, Level, Report};

use crate::{UaOptions, UaPart, verify_pdfua};

/// Findings kept per PDF/UA checkpoint when checking level a.
const PER_CHECKPOINT: usize = 20;

/// PDF/UA checkpoint → (ISO 19005-1 clause, ISO 19005-2/-3 clause, is an error). Where ISO
/// 19005 has no clause of its own for the rule, the logical-structure clause is given and the
/// message names the ISO 32000-1 section.
const RULES: &[(&str, &str, &str, bool)] = &[
    ("mark-info", "6.8.2.2", "6.7.2.2", true),
    ("struct-tree-root", "6.8.3.3", "6.7.3.3", true),
    ("struct-tree-shape", "6.8.3.3", "6.7.3.3", true),
    ("01-005", "6.8.3.3", "6.7.3.3", true),
    ("01-003", "6.8.3.3", "6.7.3.3", true),
    ("01-004", "6.8.3.3", "6.7.3.3", true),
    ("mcid-unreferenced", "6.8.3.3", "6.7.3.3", true),
    ("struct-missing-content", "6.8.3.3", "6.7.3.3", true),
    ("parent-tree", "6.8.3.3", "6.7.3.3", true),
    ("02-001", "6.8.3.4", "6.7.3.4", true),
    ("02-002", "6.8.3.4", "6.7.3.4", true),
    ("02-003", "6.8.3.4", "6.7.3.4", true),
    ("11-001", "6.8.4", "6.7.4", true),
    ("13-003", "6.8", "6.7", true),
    ("17-002", "6.8", "6.7", false),
    ("limits", "6.8", "6.7", false),
];

/// The level-a logical-structure findings for `doc` (nothing for other levels).
pub fn pdfa_structure(doc: &Document, level: Level) -> Vec<Issue> {
    if !level.needs_structure() {
        return Vec::new();
    }
    let report = verify_pdfua(doc, &UaOptions { part: UaPart::Ua1, max_findings: PER_CHECKPOINT });
    let p1 = level.part() == 1;
    let mut out = Vec::new();
    for f in &report.findings {
        let Some((_, c1, c2, error)) = RULES.iter().find(|r| r.0 == f.checkpoint) else { continue };
        let clause = if p1 { *c1 } else { *c2 };
        let note = match f.checkpoint {
            "13-003" | "17-002" => " (alternate descriptions, ISO 32000-1 §14.9.3)",
            "11-001" => " (natural language, ISO 32000-1 §14.9.2)",
            _ => "",
        };
        let message = format!("{}{note}", f.message);
        out.push(if *error { Issue::error(clause, message, f.page, false) } else { Issue::warning(clause, message, f.page) });
    }
    for (id, _, _, _) in RULES {
        let n = report.count(id);
        let kept = report.findings.iter().filter(|f| f.checkpoint == *id).count();
        if n > kept {
            let clause = RULES.iter().find(|r| r.0 == *id).map_or("6.7", |r| if p1 { r.1 } else { r.2 });
            out.push(Issue::warning(clause, format!("{} more problems of this kind ({id}) aren't listed", n - kept), None));
        }
    }
    out
}

/// Check `doc` against every implemented rule of `level`: the shared PDF/A rules, Unicode
/// mappings for levels a and u, and the logical structure for level a.
pub fn verify_pdfa(doc: &Document, level: Level) -> Vec<Issue> {
    let mut out = pdfcraft_preflight::verify(doc, level);
    out.extend(pdfa_structure(doc, level));
    out
}

/// What stops a conversion to `level` before anything changes (empty when it can go ahead).
pub fn pdfa_blockers(doc: &Document, level: Level) -> Vec<Issue> {
    pdfcraft_preflight::conversion_blockers(doc, level, |d| pdfa_structure(d, level))
}

/// Save as PDF/A: fix what can be fixed for `level` and re-check. Level u writes ToUnicode
/// maps where the encoding gives each glyph's meaning; levels a and u refuse, changing nothing,
/// when a glyph's meaning can't be known, and level a refuses unless the document is already
/// tagged well enough (it is never tagged automatically).
pub fn convert_pdfa(doc: &mut Document, level: Level) -> Result<Report, ConvertError> {
    pdfcraft_preflight::convert_with(doc, level, |d| pdfa_structure(d, level))
}

/// [`convert_pdfa`] with a chosen output intent (replacing the document's PDF/A intents).
pub fn convert_pdfa_with_intent(doc: &mut Document, level: Level, intent: Option<&pdfcraft_preflight::PdfaIntent>) -> Result<Report, ConvertError> {
    pdfcraft_preflight::convert_with_intent(doc, level, intent, |d| pdfa_structure(d, level))
}

#[cfg(test)]
#[path = "pdfa_tests.rs"]
mod tests;
