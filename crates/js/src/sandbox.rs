use std::borrow::Cow;
use std::sync::OnceLock;
use std::time::Duration;

use crate::wire::{Envelope, Job, MAX_EFFECTS, MAX_INPUT, MAX_OUTPUT, MAX_SCRIPT, Reply, VERSION, encode};
use crate::xfa::{XfaDoc, XfaEvent, XfaNode, XfaOutcome};
use crate::{Dialogs, DocInfo, Event, FieldState, Limits, Outcome};

const FUEL: u64 = 1_500_000_000;
static KERNEL: OnceLock<Result<crate::runner::Kernel, String>> = OnceLock::new();

fn kernel() -> Result<&'static crate::runner::Kernel, String> {
    KERNEL.get_or_init(|| crate::runner::Kernel::new(include_bytes!(concat!(env!("OUT_DIR"), "/script-kernel.wasm")))).as_ref().map_err(Clone::clone)
}

fn execute(job: Job<'_>, timeout: Option<Duration>) -> Result<Reply, String> {
    let timeout = timeout.unwrap_or(Duration::from_secs(2));
    if timeout.is_zero() {
        return Err("the script ran too long (zero work budget)".into());
    }
    let platform = if cfg!(target_os = "windows") {
        "WIN"
    } else if cfg!(target_os = "macos") {
        "MAC"
    } else {
        "UNIX"
    };
    let input = encode(&Envelope { version: VERSION, platform: platform.into(), job }, MAX_INPUT)?;
    let output = kernel()?.run(input, FUEL, MAX_OUTPUT, timeout)?;
    serde_json::from_slice(&output).map_err(|e| format!("invalid script kernel result: {e}"))
}

pub fn run(script: &str, event: &Event, doc: &DocInfo, fields: &[FieldState], doc_scripts: &[String], limits: Limits) -> Outcome {
    run_with(script, event, doc, fields, doc_scripts, limits, &Dialogs::default())
}

pub fn run_with(
    script: &str,
    event: &Event,
    doc: &DocInfo,
    fields: &[FieldState],
    doc_scripts: &[String],
    limits: Limits,
    dialogs: &Dialogs,
) -> Outcome {
    run_within(script, event, doc, fields, doc_scripts, limits, dialogs, Duration::from_secs(2))
}

/// The shared form protocol with an explicit per-call monotonic deadline.
#[allow(clippy::too_many_arguments)] // Keep the existing form API plus a deadline for event batches.
pub fn run_within(
    script: &str,
    event: &Event,
    doc: &DocInfo,
    fields: &[FieldState],
    doc_scripts: &[String],
    limits: Limits,
    dialogs: &Dialogs,
    timeout: Duration,
) -> Outcome {
    let failed = |why: String| {
        // Oversized input is never echoed into another unbounded host allocation. Callers
        // must retain their original event on error (and receive no changed fields/effects).
        let bounded = event.value.len().saturating_add(event.change.len()) <= MAX_INPUT;
        Outcome {
            limited: true,
            rc: true,
            value: if bounded { event.value.clone() } else { String::new() },
            change: if bounded { event.change.clone() } else { String::new() },
            error: Some(why),
            ..Default::default()
        }
    };
    if fields.len() > 200_000 || doc_scripts.len() > 4_096 || doc.annots.len() > 200_000 {
        return failed("the script snapshot has too many fields, scripts or annotations".into());
    }
    if script.len() > MAX_SCRIPT || doc_scripts.iter().any(|s| s.len() > MAX_SCRIPT) {
        return failed("the script is too long (the limit is 256 KiB)".into());
    }
    if doc.annots.iter().any(|a| a.rect.iter().any(|v| !v.is_finite())) {
        return failed("script annotation coordinates must be finite".into());
    }
    let job = Job::Field {
        script: Cow::Borrowed(script),
        event: Cow::Borrowed(event),
        doc: Cow::Borrowed(doc),
        fields: Cow::Borrowed(fields),
        doc_scripts: Cow::Borrowed(doc_scripts),
        limits,
        dialogs: Cow::Borrowed(dialogs),
    };
    match execute(job, Some(timeout)) {
        Ok(Reply::Field(out)) if out.limited => failed(out.error.unwrap_or_else(|| "script resource budget exhausted".into())),
        Ok(Reply::Field(out)) => {
            let count = out.changed.len().saturating_add(out.annots.len()).saturating_add(out.requests.len());
            if count > MAX_EFFECTS || out.alerts.len() > 100 || out.console.len() > 1_000 {
                failed("the script produced too many effects or messages".into())
            } else {
                out
            }
        }
        Ok(_) => failed("script kernel returned the wrong result kind".into()),
        Err(e) => failed(e),
    }
}

pub(crate) fn xfa(
    script: &str,
    event: &XfaEvent,
    doc: &XfaDoc,
    root: &XfaNode,
    limits: Limits,
    timeout: Option<Duration>,
    formcalc: bool,
) -> XfaOutcome {
    let failed = |why: String| XfaOutcome { limited: true, error: Some(why), ..Default::default() };
    if script.len() > MAX_SCRIPT {
        return failed("the script is too long (the limit is 256 KiB)".into());
    }
    // Check before recursive serde traversal; hold only one iterator for each depth.
    let mut stack = vec![std::slice::from_ref(root).iter()];
    let mut nodes = 0usize;
    while let Some(level) = stack.last_mut() {
        let Some(node) = level.next() else {
            stack.pop();
            continue;
        };
        nodes += 1;
        if nodes > 200_000 || stack.len() > 64 {
            return failed("the form snapshot is too large or nested too deeply".into());
        }
        if !node.children.is_empty() {
            stack.push(node.children.iter());
        }
    }
    let job =
        Job::Xfa { script: Cow::Borrowed(script), event: Cow::Borrowed(event), doc: Cow::Borrowed(doc), root: Cow::Borrowed(root), limits, formcalc };
    match execute(job, timeout) {
        Ok(Reply::Xfa(out)) if out.limited => failed(out.error.unwrap_or_else(|| "form script resource budget exhausted".into())),
        Ok(Reply::Xfa(out)) if out.effects.len() <= MAX_EFFECTS && out.console.len() <= 1_000 && out.notes.len() <= MAX_EFFECTS && !out.abandoned => {
            out
        }
        Ok(_) => failed("script kernel returned an invalid or oversized form result".into()),
        Err(e) => failed(e),
    }
}
