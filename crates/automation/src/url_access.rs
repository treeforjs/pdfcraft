//! Internet access from PDFs (PDF: Trust Manager ▸ Internet Access from PDF Files): read
//! and change the policy (`trust_urls_get`, `trust_urls_set`) and evaluate addresses against it
//! (`trust_url_check`). The policy lives in the session; the desktop app keeps it with its other
//! settings. `form_submit`, `js_run`, `form_run_trigger` and `link_list` report its decision.

use pdfcraft_engine::url_access::{HostAction, HostRule, MAX_RULES, UrlCheck, UrlDecision, UrlMode, UrlPolicy};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// The most addresses one `trust_url_check` evaluates.
pub(crate) const MAX_CHECK_URLS: usize = 100;

/// A policy as tool JSON.
pub(crate) fn policy_json(p: &UrlPolicy) -> Value {
    json!({
        "mode": p.mode.id(),
        "default_action": p.default_action.id(),
        "rules": p.rules.iter().map(rule_json).collect::<Vec<_>>(),
        "max_rules": MAX_RULES,
    })
}

fn rule_json(r: &HostRule) -> Value {
    json!({ "host": r.host, "action": r.action.id(), "subdomains": r.subdomains })
}

/// One decision as tool JSON.
pub(crate) fn check_json(url: &str, c: &UrlCheck) -> Value {
    let decision = match c.decision {
        UrlDecision::Allow => "allow",
        UrlDecision::Block => "block",
        UrlDecision::Prompt => "prompt",
    };
    let kind = match c.kind {
        pdfcraft_engine::url_access::UrlKind::Web => "web",
        pdfcraft_engine::url_access::UrlKind::Mail => "mail",
        pdfcraft_engine::url_access::UrlKind::Refused => "refused",
    };
    json!({
        "url": url, "decision": decision, "kind": kind, "scheme": c.scheme, "host": c.host,
        "rule": c.rule.as_ref().map(rule_json), "reason": c.reason,
    })
}

fn action_of(v: &str) -> Result<HostAction> {
    HostAction::from_id(v).ok_or_else(|| bad(format!("unknown action {v:?} (allow, block or ask)")))
}

/// A rule from `{host, action, subdomains?}`.
fn rule_of(v: &Value) -> Result<HostRule> {
    let host = v.get("host").and_then(Value::as_str).ok_or_else(|| bad("each rule needs host (a string)"))?;
    let action = action_of(v.get("action").and_then(Value::as_str).ok_or_else(|| bad("each rule needs action (allow, block or ask)"))?)?;
    let subdomains = match v.get("subdomains") {
        None | Some(Value::Null) => true,
        Some(s) => s.as_bool().ok_or_else(|| bad("subdomains must be true or false"))?,
    };
    HostRule::new(host, action, subdomains).map_err(|e| bad(format!("rule {host:?}: {e}")))
}

fn rules_of(a: &Args, key: &str) -> Result<Option<Vec<HostRule>>> {
    let Some(v) = a.get(key) else { return Ok(None) };
    let list = v.as_array().ok_or_else(|| bad(format!("{key} must be an array of {{host, action, subdomains}}")))?;
    if list.len() > MAX_RULES {
        return Err(bad(format!("{key} has {} rules; at most {MAX_RULES}", list.len())));
    }
    list.iter().map(rule_of).collect::<Result<Vec<_>>>().map(Some)
}

impl Automation {
    pub(crate) fn trust_urls_get(&self) -> Result<Value> {
        Ok(policy_json(self.session.url_policy()))
    }

    /// Change the policy: `mode`, `default_action`, `rules` (replace the list), `add` (add or
    /// replace rules), `remove` (hosts). Nothing changes unless every argument is valid.
    pub(crate) fn trust_urls_set(&mut self, a: &Args) -> Result<Value> {
        let mut p = self.session.url_policy().clone();
        if let Some(m) = a.opt_str("mode")? {
            p.mode = UrlMode::from_id(m).ok_or_else(|| bad(format!("unknown mode {m:?} (allow-all, block-all or custom)")))?;
        }
        if let Some(d) = a.opt_str("default_action")? {
            p.default_action = action_of(d)?;
        }
        if let Some(rules) = rules_of(a, "rules")? {
            p.rules.clear();
            for r in rules {
                p.set_rule(r).map_err(bad)?;
            }
        }
        let mut removed = Vec::new();
        if a.get("remove").is_some() {
            for host in a.strs("remove")? {
                if p.remove_rule(host).map_err(|e| bad(format!("remove {host:?}: {e}")))? {
                    removed.push(host.to_string());
                }
            }
        }
        if let Some(rules) = rules_of(a, "add")? {
            for r in rules {
                p.set_rule(r).map_err(bad)?;
            }
        }
        self.session.set_url_policy(&p);
        let mut out = policy_json(self.session.url_policy());
        out["removed"] = json!(removed);
        Ok(out)
    }

    pub(crate) fn trust_url_check(&self, a: &Args) -> Result<Value> {
        let urls: Vec<&str> = match (a.opt_str("url")?, a.get("urls")) {
            (Some(u), None) => vec![u],
            (None, Some(_)) => a.strs("urls")?,
            _ => return Err(bad("give url (one address) or urls (a list), not both")),
        };
        if urls.is_empty() || urls.len() > MAX_CHECK_URLS {
            return Err(bad(format!("urls must hold 1 to {MAX_CHECK_URLS} addresses")));
        }
        let results: Vec<Value> = urls.iter().map(|u| check_json(u, &self.session.check_url(u))).collect();
        Ok(json!({ "mode": self.session.url_policy().mode.id(), "results": results }))
    }
}
