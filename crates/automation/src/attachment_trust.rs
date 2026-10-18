//! File attachments and the attachment trust policy (Trust Manager ▸ attachments).
//!
//! `attachment_list`, `attachment_open` and `attachment_extract` work on the document's
//! attachments: the `/Names /EmbeddedFiles` entries and FileAttachment annotations, in the order
//! the Attachments panel shows them, chosen by `name` or 1-based `index`. Opening consults the
//! session's policy ([`pdfcraft_engine::attachment_trust`]) first: a blocked type is never opened,
//! an unknown one needs `confirm: true`. Plain extraction to a chosen path is always allowed and
//! reports the risk. `trust_attachments_get` / `trust_attachments_set` read and change the
//! policy's allow and deny lists.

use std::sync::Arc;

use pdfcraft_engine::attachment_trust::{AttachmentTrust, BUILTIN_ALLOW, BUILTIN_DENY, Check, Decision, MAX_USER_EXTENSIONS, normalize_extension};
use pdfcraft_render::{Attachment, AttachmentSource};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed, summary};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

pub(crate) fn check_json(c: &Check) -> Value {
    serde_json::to_value(c).unwrap_or_else(|_| json!({ "decision": c.decision.as_str(), "reason": c.reason }))
}

fn source_json(s: &AttachmentSource) -> Value {
    match s {
        AttachmentSource::Document { .. } => json!("document"),
        AttachmentSource::Annotation { page, index } => json!({ "page": page.saturating_add(1), "annotation": index.saturating_add(1) }),
    }
}

fn policy_json(t: &AttachmentTrust) -> Value {
    json!({
        "allow": t.allow,
        "deny": t.deny,
        "builtin_deny": BUILTIN_DENY,
        "builtin_allow": BUILTIN_ALLOW,
        "unknown": "prompt",
        "order": "user deny, then user allow, then built-in deny, then built-in allow; anything else (or no extension) prompts",
    })
}

/// An optional array of extension strings, canonicalized.
fn extension_list(a: &Args, key: &str) -> Result<Option<Vec<String>>> {
    if a.get(key).is_none() {
        return Ok(None);
    }
    let list = a.strs(key)?;
    if list.len() > MAX_USER_EXTENSIONS {
        return Err(bad(format!("{key} can hold at most {MAX_USER_EXTENSIONS} extensions")));
    }
    list.iter().map(|e| normalize_extension(e).map_err(|m| bad(format!("{key}: {m}")))).collect::<Result<Vec<_>>>().map(Some)
}

impl Automation {
    /// The attachment chosen by `name` or 1-based `index`, with its 0-based position.
    fn pick_attachment(&self, a: &Args) -> Result<(usize, Attachment)> {
        let doc = self.doc(a)?;
        let list = &doc.info.attachments;
        let none = || failed("the document has no attachments");
        match (a.opt_str("name")?, a.get("index")) {
            (Some(_), Some(_)) => Err(bad("pass name or index, not both")),
            (None, None) => Err(bad("pass the attachment's name or its 1-based index (see attachment_list)")),
            (None, Some(i)) => {
                let i = i.as_u64().filter(|n| *n >= 1).ok_or_else(|| bad("index must be a positive integer (1-based)"))?;
                if list.is_empty() {
                    return Err(none());
                }
                let at = usize::try_from(i - 1).ok().and_then(|i| list.get(i).map(|x| (i, x.clone())));
                at.ok_or_else(|| failed(format!("there is no attachment {i}; the document has {}", list.len())))
            }
            (Some(n), None) => {
                let mut found = list.iter().enumerate().filter(|(_, x)| x.name == n);
                let first = found.next().map(|(i, x)| (i, x.clone()));
                if found.next().is_some() {
                    return Err(failed(format!("several attachments are called {n:?}: pass index instead (see attachment_list)")));
                }
                first.ok_or_else(|| if list.is_empty() { none() } else { failed(format!("no attachment is called {n:?} (see attachment_list)")) })
            }
        }
    }

    fn attachment_bytes(&self, a: &Args, att: &Attachment) -> Result<Vec<u8>> {
        let doc = self.doc(a)?;
        pdfcraft_render::attachment_data(&doc.bytes, doc.password.as_deref(), att).map_err(|e| failed(format!("couldn't read {}: {e}", att.name)))
    }

    pub(crate) fn attachment_list(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let items: Vec<Value> = doc
            .info
            .attachments
            .iter()
            .enumerate()
            .map(|(i, x)| {
                json!({
                    "index": i.saturating_add(1),
                    "name": x.name,
                    "description": x.description,
                    "size": x.size,
                    "source": source_json(&x.source),
                    "trust": check_json(&self.session.check_attachment(&x.name)),
                })
            })
            .collect();
        Ok(json!({ "count": items.len(), "attachments": items }))
    }

    pub(crate) fn attachment_open(&mut self, a: &Args) -> Result<Value> {
        let (index, att) = self.pick_attachment(a)?;
        let check = self.session.check_attachment(&att.name);
        let trust = check_json(&check);
        let base = json!({ "index": index.saturating_add(1), "name": att.name, "source": source_json(&att.source) });
        match check.decision {
            Decision::Block => {
                return Ok(json!({
                    "status": "blocked",
                    "opened": false,
                    "attachment": base,
                    "reason": check.reason,
                    "trust": trust,
                    "hint": "Blocked types are never opened or launched, even with confirm. attachment_extract can still save it to a path you choose; trust_attachments_set changes the lists.",
                }));
            }
            Decision::Prompt if !a.opt_bool("confirm")?.unwrap_or(false) => {
                return Ok(json!({
                    "status": "prompt",
                    "opened": false,
                    "attachment": base,
                    "question": format!("{} may harm this computer if it contains a program or a script: {}. Open it only if you trust its source; call attachment_open again with confirm: true to open it.", att.name, check.reason),
                    "reason": check.reason,
                    "trust": trust,
                }));
            }
            Decision::Prompt | Decision::Allow => {}
        }
        let data = self.attachment_bytes(a, &att)?;
        if check.extension.as_deref() == Some("pdf") {
            if a.get("path").is_some() {
                return Err(bad("a PDF attachment opens as a new document; path is for other types (use attachment_extract to save it)"));
            }
            let id = self
                .session
                .open(att.name.clone(), None, Arc::new(data), a.opt_str("password")?)
                .map_err(|e| failed(format!("couldn't open {}: {e}", att.name)))?;
            let doc = self.session.get(id).ok_or_else(|| failed("the document vanished"))?;
            return Ok(json!({ "status": "opened", "opened": true, "attachment": base, "document": summary(doc), "trust": trust }));
        }
        if a.get("password").is_some() {
            return Err(bad("password is only for PDF attachments"));
        }
        let Some(out) = a.opt_str("path")? else {
            return Err(bad(format!(
                "{} is not a PDF, so it opens in another application: pass path, where to write it for launching (nothing is launched headlessly)",
                att.name
            )));
        };
        let written = self.write_output(out, &data)?;
        Ok(json!({
            "status": "ready",
            "opened": false,
            "launched": false,
            "attachment": base,
            "path": written.display().to_string(),
            "bytes": data.len(),
            "trust": trust,
        }))
    }

    pub(crate) fn attachment_extract(&self, a: &Args) -> Result<Value> {
        let (index, att) = self.pick_attachment(a)?;
        let out = a.str("path")?;
        let check = self.session.check_attachment(&att.name);
        let data = self.attachment_bytes(a, &att)?;
        let written = self.write_output(out, &data)?;
        let mut v = json!({
            "path": written.display().to_string(),
            "bytes": data.len(),
            "attachment": { "index": index.saturating_add(1), "name": att.name, "source": source_json(&att.source) },
            "risk": check.decision.as_str(),
            "trust": check_json(&check),
        });
        if check.decision != Decision::Allow {
            v["warning"] = json!(format!("Saved, but don't open it unless you trust its source: {}", check.reason));
        }
        Ok(v)
    }

    pub(crate) fn trust_attachments_get(&self, a: &Args) -> Result<Value> {
        let mut v = policy_json(self.session.attachment_trust());
        if a.get("names").is_some() {
            let names = a.strs("names")?;
            if names.len() > 1024 {
                return Err(bad("names can hold at most 1024 entries"));
            }
            v["checks"] = json!(names.iter().map(|n| check_json(&self.session.check_attachment(n))).collect::<Vec<_>>());
        }
        Ok(v)
    }

    pub(crate) fn trust_attachments_set(&mut self, a: &Args) -> Result<Value> {
        let current = if a.opt_bool("reset")?.unwrap_or(false) { AttachmentTrust::default() } else { self.session.attachment_trust().clone() };
        let mut allow = extension_list(a, "allow")?.unwrap_or(current.allow);
        let mut deny = extension_list(a, "deny")?.unwrap_or(current.deny);
        let add_allow = extension_list(a, "add_allow")?.unwrap_or_default();
        let add_deny = extension_list(a, "add_deny")?.unwrap_or_default();
        if let Some(e) = add_allow.iter().find(|e| add_deny.contains(e)) {
            return Err(bad(format!(".{e} is in both add_allow and add_deny")));
        }
        for e in extension_list(a, "remove")?.unwrap_or_default() {
            allow.retain(|x| *x != e);
            deny.retain(|x| *x != e);
        }
        for e in add_allow {
            deny.retain(|x| *x != e);
            allow.push(e);
        }
        for e in add_deny {
            allow.retain(|x| *x != e);
            deny.push(e);
        }
        let trust = AttachmentTrust::new(&allow, &deny).map_err(bad)?;
        self.session.set_attachment_trust(trust);
        Ok(policy_json(self.session.attachment_trust()))
    }
}
