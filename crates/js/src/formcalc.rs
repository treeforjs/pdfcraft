//! FormCalc shares the same memory and instruction boundary as document JavaScript.
use crate::Limits;
use crate::xfa::{XfaDoc, XfaEvent, XfaNode, XfaOutcome};

pub fn run_formcalc(script: &str, event: &XfaEvent, doc: &XfaDoc, root: &XfaNode, limits: Limits) -> XfaOutcome {
    crate::sandbox::xfa(script, event, doc, root, limits, None, true)
}

pub fn run_formcalc_within(script: &str, event: &XfaEvent, doc: &XfaDoc, root: XfaNode, limits: Limits, timeout: std::time::Duration) -> XfaOutcome {
    run_formcalc_at(script, event, doc, root, limits, Some(timeout))
}

pub(crate) fn run_formcalc_at(
    script: &str,
    event: &XfaEvent,
    doc: &XfaDoc,
    root: XfaNode,
    limits: Limits,
    timeout: Option<std::time::Duration>,
) -> XfaOutcome {
    crate::sandbox::xfa(script, event, doc, &root, limits, timeout, true)
}
