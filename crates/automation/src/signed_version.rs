//! View Signed Version and Compare Signed Version to Current Version: `sign_signed_version`
//! extracts (and opens) the revision a signature covers, `sign_compare_signed_version` lists
//! what changed since it was signed and whether the signature permits each change.

use pdfcraft_engine::compare::{CompareOptions, report_json};
use pdfcraft_engine::sign::signed_version::permissions_label;
use serde_json::{Value, json};

use crate::{Args, Automation, Result, failed, summary, write_atomic};

/// Changes listed in a result by default.
const DEFAULT_LIMIT: i64 = 500;

impl Automation {
    pub(crate) fn sign_signed_version(&mut self, a: &Args) -> Result<Value> {
        let field = a.str("field")?.to_string();
        let id = self.doc(a)?.id;
        let out = a.opt_str("out")?.map(|p| self.resolve(p, true)).transpose()?;
        let open = a.opt_bool("open")?.unwrap_or(out.is_none());
        let (sig, bytes) = self.session.signed_version_bytes(id, &field).map_err(failed)?;
        let current = self.doc(a)?.bytes.len();
        let mut v = json!({
            "field": sig.field,
            "revision": sig.revision,
            "length": bytes.len(),
            "current_length": current,
            "whole_file": bytes.len() == current,
        });
        if let Some(p) = &out {
            write_atomic(p, &bytes)?;
            v["out"] = json!(p.to_string_lossy());
        }
        if open {
            let new = self.session.open_signed_version(id, &field).map_err(failed)?;
            let d = self.session.get(new).ok_or_else(|| failed("the document vanished"))?;
            v["doc"] = json!(d.id.0);
            v["opened"] = summary(d);
        }
        Ok(v)
    }

    pub(crate) fn sign_compare_signed_version(&mut self, a: &Args) -> Result<Value> {
        let field = a.str("field")?.to_string();
        let id = self.doc(a)?.id;
        let limit = usize::try_from(a.opt_int("limit")?.unwrap_or(DEFAULT_LIMIT).max(1)).unwrap_or(usize::MAX);
        let text = a.opt_bool("text")?.unwrap_or(true);
        let visual = a.opt_bool("visual")?.unwrap_or(false);
        let c = self.session.compare_signed_version(id, &field).map_err(failed)?;
        let changes: Vec<Value> = c
            .changes
            .iter()
            .take(limit)
            .map(|ch| {
                json!({
                    "kind": ch.kind,
                    "action": ch.action.label(),
                    "page": ch.page.map(|p| p + 1),
                    "object": ch.object.map(|r| format!("{} {} R", r.num, r.generation)),
                    "subject": ch.subject,
                    "field": ch.field,
                    "before": ch.before,
                    "after": ch.after,
                    "permitted": ch.permitted,
                })
            })
            .collect();
        let kinds: Vec<Value> = c.kinds().into_iter().map(|(kind, permitted)| json!({ "kind": kind, "permitted": permitted })).collect();
        let count = |pred: &dyn Fn(&pdfcraft_engine::sign::VersionChange) -> bool| c.changes.iter().filter(|ch| pred(ch)).count();
        let pages_added: Vec<usize> = c
            .changes
            .iter()
            .filter(|ch| ch.subject == "page" && ch.action == pdfcraft_engine::sign::ChangeAction::Added && ch.kind == "pages added or removed")
            .filter_map(|ch| ch.page.map(|p| p + 1))
            .collect();
        let pages_removed: Vec<usize> = c
            .changes
            .iter()
            .filter(|ch| ch.subject == "page" && ch.action == pdfcraft_engine::sign::ChangeAction::Removed && ch.kind == "pages added or removed")
            .filter_map(|ch| ch.page.map(|p| p + 1))
            .collect();
        let mut v = json!({
            "field": c.field,
            "revision": c.revision,
            "signed_length": c.signed_len,
            "current_length": c.current_len,
            "certify": c.certify,
            "permissions": c.permissions,
            "permissions_text": permissions_label(c.permissions),
            "signed_pages": c.signed_pages,
            "current_pages": c.current_pages,
            "pages_added": pages_added,
            "pages_removed": pages_removed,
            "modified": c.modified(),
            "permitted": c.permitted(),
            "unreadable": c.unreadable,
            "kinds": kinds,
            "summary": {
                "page_content": count(&|ch| ch.kind == "page content"),
                "comments": count(&|ch| ch.kind == "comments"),
                "form_fields": count(&|ch| ch.field.is_some()),
                "pages_added": pages_added.len(),
                "pages_removed": pages_removed.len(),
                "not_permitted": count(&|ch| !ch.permitted),
            },
            "total_changes": c.changes.len(),
            "truncated": c.truncated || c.changes.len() > limit,
            "changes": changes,
        });
        if text && c.modified() && !c.unreadable {
            v["text"] = self.signed_text_compare(id, &field, visual, limit)?;
        }
        Ok(v)
    }

    /// Compare Files between the signed version (opened for the comparison only) and the
    /// current document; a refusal (e.g. a document that forbids copying text) is reported in
    /// the result instead of failing the whole comparison.
    fn signed_text_compare(&mut self, id: pdfcraft_engine::DocId, field: &str, visual: bool, limit: usize) -> Result<Value> {
        let old = self.session.open_signed_version(id, field).map_err(failed)?;
        let opts = CompareOptions { visual, ..Default::default() };
        let result = self.session.compare_files(old, id, &opts);
        let v = match result {
            Ok(c) => {
                let mut v = report_json(&c, &opts, limit);
                v["identical"] = json!(c.text.identical() && c.visual.is_empty());
                v
            }
            Err(e) => json!({ "error": e.to_string() }),
        };
        self.session.close(old);
        Ok(v)
    }
}
