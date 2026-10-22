//! The JavaScript debugger's tools: list every script in a document, read one, and debug a
//! script with breakpoints, stepping, watches and a step budget.

use pdfcraft_engine::js_debug::{Debug, DebugOutcome, ErrorAt, MAX_STEPS, MAX_TIMEOUT_MS, MAX_WATCHES, Resume, ScriptLocation, Step};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

/// Where a script error or stack frame is: `script` (the one debugged) or a document-level
/// script's name.
fn script_name(names: &[String], script: Option<usize>) -> Value {
    match script {
        None => json!("script"),
        Some(i) => names.get(i).map_or(json!(format!("document script {}", i.saturating_add(1))), |n| json!(n)),
    }
}

fn location(a: &Args) -> Result<ScriptLocation> {
    let id = a.str("id")?;
    ScriptLocation::parse(id).ok_or_else(|| {
        ToolError::InvalidArgs(format!("{id:?} is not a script id (js_scripts lists them, e.g. \"document:init\" or \"field:calculate:total\")"))
    })
}

fn first_line(script: &str) -> String {
    let line = script.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > 120 { line.chars().take(120).chain("…".chars()).collect() } else { line.to_string() }
}

pub(crate) fn outcome_json(session: &pdfcraft_engine::Session, names: &[String], d: &DebugOutcome) -> Value {
    let o = &d.outcome;
    let status = if d.paused.is_some() {
        "paused"
    } else if d.out_of_steps {
        "stopped"
    } else {
        "finished"
    };
    let at = |e: &ErrorAt| json!({ "script": script_name(names, e.script), "line": e.line, "column": (e.column > 0).then_some(e.column) });
    let paused = d.paused.as_ref().map(|p| {
        let stack: Vec<Value> = p
            .stack
            .iter()
            .map(|f| json!({ "function": f.function, "script": script_name(names, f.script), "line": f.line, "column": f.column }))
            .collect();
        let watches: Vec<Value> = p.watches.iter().map(|w| json!({ "expression": w.expression, "value": w.value, "error": w.error })).collect();
        json!({ "line": p.line, "breakpoint": p.breakpoint, "stack": stack, "watches": watches, "resume": { "hit": p.hit, "depth": p.depth } })
    });
    let requests: Vec<Value> = o.requests.iter().map(|r| crate::a11y::request_json(session, r)).collect();
    json!({
        "status": status,
        "paused": paused,
        "console": o.console,
        "alerts": o.alerts,
        "requests": requests,
        "result": o.result,
        "error": o.error,
        "error_at": o.error_at.as_ref().map(at),
        "value": o.value,
        "rc": o.rc,
        "breakable": d.breakable,
        "steps": d.steps,
    })
}

impl Automation {
    /// Every script in the document and where it lives.
    pub(crate) fn js_scripts(&self, a: &Args) -> Result<Value> {
        let id = self.doc(a)?.id;
        let kind = a.opt_str("kind")?;
        let index = self.session.javascript_index(id).map_err(failed)?;
        let list: Vec<Value> = index
            .scripts
            .iter()
            .filter(|e| kind.is_none_or(|k| e.location.kind() == k))
            .map(|e| {
                json!({
                    "id": e.location.id(),
                    "kind": e.location.kind(),
                    "location": e.location.describe(),
                    "lines": e.lines(),
                    "bytes": e.script.len(),
                    "first_line": first_line(&e.script),
                })
            })
            .collect();
        Ok(json!({ "scripts": list, "truncated": index.truncated }))
    }

    /// One script's source, and the lines that can take a breakpoint.
    pub(crate) fn js_script(&self, a: &Args) -> Result<Value> {
        let id = self.doc(a)?.id;
        let entry = self.session.javascript_at(id, &location(a)?).map_err(failed)?;
        let breakable = pdfcraft_engine::js_debug::breakable_lines(&entry.script);
        Ok(json!({
            "id": entry.location.id(),
            "kind": entry.location.kind(),
            "location": entry.location.describe(),
            "lines": entry.lines(),
            "script": entry.script,
            "breakable": breakable,
        }))
    }

    /// Debug a script: one typed in (`script`) or one of the document's (`id`).
    pub(crate) fn js_debug(&mut self, a: &Args) -> Result<Value> {
        let id = self.doc(a)?.id;
        let at = if a.get("id").is_some() { Some(location(a)?) } else { None };
        let script = match (a.opt_str("script")?, &at) {
            (Some(s), _) => s.to_string(),
            (None, Some(loc)) => self.session.javascript_at(id, loc).map_err(failed)?.script,
            (None, None) => return Err(ToolError::InvalidArgs("pass script (JavaScript to run) or id (a script from js_scripts)".into())),
        };
        let step = match a.opt_str("step")? {
            None => Step::Run,
            Some(s) => Step::from_id(s).ok_or_else(|| Args::wrong("step", "run, into, over or out"))?,
        };
        let breakpoints = a
            .opt_ints("breakpoints")?
            .unwrap_or_default()
            .into_iter()
            .map(|l| u32::try_from(l).ok().filter(|l| *l >= 1).ok_or_else(|| Args::wrong("breakpoints", "1-based line numbers")))
            .collect::<Result<Vec<_>>>()?;
        if breakpoints.len() > 10_000 {
            return Err(Args::wrong("breakpoints", "at most 10000 lines"));
        }
        let watches: Vec<String> = match a.get("watches") {
            None => Vec::new(),
            Some(_) => a.strs("watches")?.into_iter().map(str::to_string).collect(),
        };
        if watches.len() > MAX_WATCHES {
            return Err(Args::wrong("watches", &format!("at most {MAX_WATCHES} expressions")));
        }
        let resume = match a.get("resume") {
            None => None,
            Some(r) => {
                let n = |k: &str| r.get(k).and_then(Value::as_u64).ok_or_else(|| Args::wrong("resume", "{ hit, depth } from a paused run"));
                Some(Resume { hit: n("hit")?, depth: usize::try_from(n("depth")?).unwrap_or(usize::MAX) })
            }
        };
        let max_steps = match a.opt_int("max_steps")? {
            None => pdfcraft_engine::js_debug::DEFAULT_STEPS,
            Some(n) if n >= 1 => u64::try_from(n).unwrap_or(MAX_STEPS).min(MAX_STEPS),
            Some(_) => return Err(Args::wrong("max_steps", &format!("between 1 and {MAX_STEPS}"))),
        };
        let timeout_ms = match a.opt_int("timeout_ms")? {
            None => pdfcraft_engine::js_debug::DEFAULT_TIMEOUT_MS,
            Some(n) if n >= 100 => u64::try_from(n).unwrap_or(MAX_TIMEOUT_MS).min(MAX_TIMEOUT_MS),
            Some(_) => return Err(Args::wrong("timeout_ms", &format!("between 100 and {MAX_TIMEOUT_MS}"))),
        };
        let debug = Debug { breakpoints, watches, step, resume, max_steps, timeout_ms };
        let d = self.session.debug_javascript(id, &script, at.as_ref(), &debug).map_err(failed)?;
        let names = self.session.document_script_names(id);
        Ok(outcome_json(&self.session, &names, &d))
    }
}
