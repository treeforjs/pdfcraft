//! PDF 2.0 associated files (`/AF`, ISO 32000-2 §14.13): list, add, remove and extract.
//!
//! A target is `"document"` (the default), `{"page": n}`, `{"page": n, "annotation": i}` (the
//! 1-based position in the page's annotations, as comment_list reports it), or `{"object": n}`
//! (a structure element, an XObject or an annotation by object number). Files are chosen by
//! `name` or by 1-based `index` in the owner's `/AF`.

use base64::Engine as _;
use pdfcraft_engine::associated_files::{MAX_FILE_BYTES, guess_mime};
use pdfcraft_engine::{AfRelationship, AfSelector, AfTarget, AssociatedFile, AssociatedFiles, Edit, NewAssociatedFile};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::{Args, Automation, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

const TARGET_HELP: &str = "target must be \"document\", {\"page\": n}, {\"page\": n, \"annotation\": i} or {\"object\": n}";

/// A 1-based positive integer from JSON.
fn one_based(v: &Value, what: &str) -> Result<usize> {
    v.as_u64()
        .filter(|n| *n >= 1)
        .and_then(|n| usize::try_from(n - 1).ok())
        .ok_or_else(|| bad(format!("{what} must be a positive integer (1-based)")))
}

/// The `target` argument (default: the document).
fn target(a: &Args) -> Result<AfTarget> {
    let Some(v) = a.get("target") else { return Ok(AfTarget::Document) };
    if let Some(s) = v.as_str() {
        return if s == "document" { Ok(AfTarget::Document) } else { Err(bad(TARGET_HELP)) };
    }
    let o = v.as_object().ok_or_else(|| bad(TARGET_HELP))?;
    if let Some(k) = o.keys().find(|k| !["page", "annotation", "object"].contains(&k.as_str())) {
        return Err(bad(format!("unknown target key {k:?}; {TARGET_HELP}")));
    }
    match (o.get("page"), o.get("annotation"), o.get("object")) {
        (Some(p), None, None) => Ok(AfTarget::Page(one_based(p, "target.page")?)),
        (Some(p), Some(i), None) => Ok(AfTarget::Annotation { page: one_based(p, "target.page")?, index: one_based(i, "target.annotation")? }),
        (None, None, Some(n)) => {
            let n =
                n.as_u64().filter(|n| *n >= 1).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| bad("target.object must be an object number"))?;
            Ok(AfTarget::Object(n))
        }
        _ => Err(bad(TARGET_HELP)),
    }
}

fn target_json(t: &AfTarget) -> Value {
    match t {
        AfTarget::Document => json!("document"),
        AfTarget::Page(p) => json!({ "page": p.saturating_add(1) }),
        AfTarget::Annotation { page, index } => json!({ "page": page.saturating_add(1), "annotation": index.saturating_add(1) }),
        AfTarget::Object(n) => json!({ "object": n }),
    }
}

fn file_json(f: &AssociatedFile) -> Value {
    json!({
        "index": f.index.saturating_add(1),
        "name": f.name,
        "description": f.description,
        "relationship": f.relationship,
        "mime": f.mime,
        "size": f.size,
        "embedded": f.embedded,
        "creation_date": f.creation_date,
        "mod_date": f.mod_date,
        "checksum": f.checksum,
        "object": f.object,
        "attachment": f.in_embedded_files,
        "problem": f.problem,
    })
}

fn owner_json(t: &AfTarget, l: &AssociatedFiles) -> Value {
    json!({ "target": target_json(t), "files": l.files.iter().map(file_json).collect::<Vec<_>>(), "issues": l.issues })
}

/// `name` or 1-based `index`, exactly one.
fn selector(a: &Args) -> Result<AfSelector> {
    match (a.opt_str("name")?, a.get("index")) {
        (Some(n), None) => Ok(AfSelector::Name(n.to_string())),
        (None, Some(i)) => Ok(AfSelector::Index(one_based(i, "index")?)),
        (Some(_), Some(_)) => Err(bad("pass name or index, not both")),
        (None, None) => Err(bad("pass the file's name or its 1-based index (see associated_files_list)")),
    }
}

impl Automation {
    pub(crate) fn associated_files_list(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let owners: Vec<Value> = if a.get("target").is_some() {
            let t = target(a)?;
            vec![owner_json(&t, &doc.associated_files(&t).map_err(failed)?)]
        } else {
            doc.all_associated_files().map_err(failed)?.iter().map(|(t, l)| owner_json(t, l)).collect()
        };
        let count: usize = owners.iter().map(|o| o["files"].as_array().map_or(0, Vec::len)).sum();
        Ok(json!({ "count": count, "owners": owners }))
    }

    pub(crate) fn associated_file_add(&mut self, a: &Args) -> Result<Value> {
        let t = target(a)?;
        let (data, default_name) = match (a.opt_str("path")?, a.opt_str("data_base64")?) {
            (Some(p), None) => {
                let path = self.resolve(p, false)?;
                let len = std::fs::metadata(&path).map_err(|e| failed(format!("{p}: {e}")))?.len();
                if len > MAX_FILE_BYTES as u64 {
                    return Err(failed(format!("{p} is {len} bytes; associated files can be at most {} MiB", MAX_FILE_BYTES >> 20)));
                }
                let data = std::fs::read(&path).map_err(|e| failed(format!("{p}: {e}")))?;
                (data, path.file_name().map(|n| n.to_string_lossy().into_owned()))
            }
            (None, Some(b)) => {
                if b.len() > MAX_FILE_BYTES.saturating_div(3).saturating_mul(4).saturating_add(8) {
                    return Err(bad(format!("data_base64 is too long; associated files can be at most {} MiB", MAX_FILE_BYTES >> 20)));
                }
                let clean: String = b.chars().filter(|c| !c.is_ascii_whitespace()).collect();
                let data = base64::engine::general_purpose::STANDARD
                    .decode(clean.as_bytes())
                    .map_err(|e| bad(format!("data_base64 is not valid base64: {e}")))?;
                (data, None)
            }
            (Some(_), Some(_)) => return Err(bad("pass path or data_base64, not both")),
            (None, None) => return Err(bad("pass the file's path or its data_base64")),
        };
        let name = match (a.opt_str("name")?, default_name) {
            (Some(n), _) => n.to_string(),
            (None, Some(n)) => n,
            (None, None) => return Err(bad("name is required with data_base64")),
        };
        let relationship = match a.opt_str("relationship")? {
            Some(r) => AfRelationship::from_name(r).ok_or_else(|| bad(format!("unknown relationship {r:?} ({})", AfRelationship::names())))?,
            None => AfRelationship::Unspecified,
        };
        let mime = a.opt_str("mime")?.map(str::to_string).or_else(|| guess_mime(&name).map(str::to_string));
        let file = NewAssociatedFile { name, data: Arc::new(data), mime, relationship, description: a.opt_str("description")?.map(str::to_string) };
        file.validate().map_err(|e| bad(e.to_string()))?;
        let name = file.name.trim().to_string();
        let mut out = self.apply(a, Edit::AddAssociatedFile { target: t.clone(), file })?;
        let list = self.doc(a)?.associated_files(&t).map_err(failed)?;
        if let Some(f) = list.files.last() {
            out["added"] = json!({ "target": target_json(&t), "file": file_json(f) });
        } else {
            out["added"] = json!({ "target": target_json(&t), "name": name });
        }
        Ok(out)
    }

    pub(crate) fn associated_file_remove(&mut self, a: &Args) -> Result<Value> {
        let t = target(a)?;
        let sel = selector(a)?;
        // Resolve the selection first, so the result can say what went.
        let doc = self.doc(a)?;
        let list = doc.associated_files(&t).map_err(failed)?;
        let removed = match &sel {
            AfSelector::Index(i) => list.files.get(*i).cloned(),
            AfSelector::Name(n) => list.files.iter().find(|f| f.name == *n).cloned(),
        };
        let mut out = self.apply(a, Edit::RemoveAssociatedFile { target: t.clone(), select: sel })?;
        out["removed"] = json!({ "target": target_json(&t), "file": removed.as_ref().map(file_json) });
        Ok(out)
    }

    pub(crate) fn associated_file_extract(&self, a: &Args) -> Result<Value> {
        let t = target(a)?;
        let sel = selector(a)?;
        let out = a.str("path")?;
        let doc = self.doc(a)?;
        let (info, data) = doc.associated_file_data(&t, &sel, MAX_FILE_BYTES.saturating_mul(4)).map_err(failed)?;
        let written = self.write_output(out, &data)?;
        // Saving is allowed whatever the type; the result says whether opening it would be.
        let check = self.session.check_attachment(&info.name);
        let mut v = json!({
            "path": written.display().to_string(),
            "bytes": data.len(),
            "file": file_json(&info),
            "risk": check.decision.as_str(),
            "trust": crate::attachment_trust::check_json(&check),
        });
        if check.decision != pdfcraft_engine::attachment_trust::Decision::Allow {
            v["warning"] = json!(format!("Saved, but don't open it unless you trust its source: {}", check.reason));
        }
        Ok(v)
    }
}
