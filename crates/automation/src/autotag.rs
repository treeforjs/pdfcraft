//! `tags_autotag`: Accessibility ▸ Autotag Document, with the PDF/UA and checker results
//! before and after.

use pdfcraft_engine::a11y::{AutotagOptions, FieldTooltips, Options, Status, UaOptions};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

/// A language tag (BCP 47 shape): letters, digits and hyphens, starting with a letter.
fn valid_lang(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 35
        && l.split('-').all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_alphanumeric()))
        && l.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
}

/// The checker rules autotagging is about.
const RULES: [&str; 8] = [
    "tagged-pdf",
    "tagged-content",
    "tagged-annotations",
    "tagged-form-fields",
    "field-descriptions",
    "tab-order",
    "figures-alt-text",
    "heading-nesting",
];

fn summary(doc: &pdfcraft_engine::Document) -> Value {
    let ua = doc.pdfua_verify(&UaOptions::default());
    let check = doc.accessibility_check(&Options::default());
    let rules: serde_json::Map<String, Value> = RULES
        .iter()
        .map(|id| {
            let status = check.as_ref().and_then(|r| r.results.iter().find(|x| x.rule.id() == *id)).map(|x| match x.status {
                Status::Passed => "passed",
                Status::Failed => "failed",
                Status::Manual => "manual",
                Status::Skipped => "skipped",
            });
            ((*id).to_owned(), json!(status))
        })
        .collect();
    json!({
        "pdfua": ua.map(|u| json!({ "conforms": u.conforms(), "failed": u.failed() })),
        "checker": {
            "failed": check.as_ref().map(|r| r.results.iter().filter(|x| x.status == Status::Failed).map(|x| x.rule.id()).collect::<Vec<_>>()),
            "rules": rules,
        },
    })
}

impl Automation {
    pub(crate) fn tags_autotag(&mut self, a: &Args) -> Result<Value> {
        let pages = if a.get("pages").is_some() { Some(self.pages(a, "pages")?) } else { None };
        let lang = match a.opt_str("lang")?.map(str::trim) {
            Some(l) if !valid_lang(l) => {
                return Err(ToolError::InvalidArgs(format!("lang must be a language tag such as en-US, not {l:?}")));
            }
            other => other.map(str::to_owned),
        };
        let field_tooltips = match a.opt_str("field_tooltips")? {
            None | Some("report") => FieldTooltips::Report,
            Some("fill") => FieldTooltips::Fill,
            Some(other) => {
                return Err(ToolError::InvalidArgs(format!("field_tooltips must be \"report\" or \"fill\", not {other:?}")));
            }
        };
        let options = AutotagOptions {
            pages,
            replace_existing: a.opt_bool("replace_existing")?.unwrap_or(false),
            lang,
            detect_tables: a.opt_bool("detect_tables")?.unwrap_or(true),
            detect_lists: a.opt_bool("detect_lists")?.unwrap_or(true),
            detect_fields: a.opt_bool("detect_fields")?.unwrap_or(true),
            field_tooltips,
        };
        let doc = self.doc(a)?;
        let before = summary(doc);
        let r = doc.autotag_preview(&options).map_err(failed)?;
        let id = doc.id;
        self.session.apply(id, pdfcraft_engine::Edit::Autotag { options }).map_err(failed)?;
        let after = summary(self.doc(a)?);
        let round = |x: f64| (x * 100.0).round() / 100.0;
        Ok(json!({
            "pages": r.pages.iter().map(|p| p + 1).collect::<Vec<_>>(),
            "skipped": r.skipped.iter().map(|(p, why)| json!({ "page": p + 1, "reason": why })).collect::<Vec<_>>(),
            "counts": r.counts,
            "elements": r.counts.values().sum::<usize>(),
            "artifacts": r.artifacts,
            "pagination_artifacts": r.pagination_artifacts,
            "figures_needing_alt": r
                .figures_needing_alt
                .iter()
                .map(|f| json!({ "page": f.page + 1, "figure": f.element.num, "rect": f.rect.map(round) }))
                .collect::<Vec<_>>(),
            "links": r.links,
            "link_descriptions": r.link_descriptions,
            "annotations": r.annotations,
            "fields": r
                .fields
                .iter()
                .map(|f| {
                    json!({
                        "page": f.page + 1,
                        "name": f.name,
                        "kind": f.kind,
                        "widget": f.widget.num,
                        "field": f.field.num,
                        "element": f.element.num,
                        "rect": f.rect.map(round),
                        "alt": f.alt,
                        "alt_source": f.alt_source,
                        "label": f.label,
                    })
                })
                .collect::<Vec<_>>(),
            "fields_needing_tooltip": r
                .fields_needing_tooltip
                .iter()
                .map(|f| json!({ "page": f.page + 1, "name": f.name, "field": f.field.num, "suggested": f.suggested, "filled": f.filled }))
                .collect::<Vec<_>>(),
            "tooltips_filled": r.tooltips_filled,
            "lang": r.lang,
            "replaced": r.replaced,
            "warnings": r.warnings,
            "before": before,
            "after": after,
        }))
    }
}
