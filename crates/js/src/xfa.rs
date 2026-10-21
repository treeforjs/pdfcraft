//! XFA scripting over a snapshot of the form tree, with effects returned to the caller.
use crate::Limits;
pub use crate::xfa_protocol::*;

/// Maximum returned effects, messages and console lines per run.
pub const MAX_EFFECTS: usize = 10_000;
pub const MAX_ALERTS: usize = 100;
pub const MAX_CONSOLE: usize = 1_000;

pub fn run_xfa(script: &str, event: &XfaEvent, doc: &XfaDoc, root: &XfaNode, limits: Limits) -> XfaOutcome {
    crate::sandbox::xfa(script, event, doc, root, limits, None, false)
}

/// Run with a monotonic deadline checked between bounded fuel slices and host calls.
/// The aggregate fuel cap still applies; no background thread is left running.
pub fn run_xfa_within(script: &str, event: &XfaEvent, doc: &XfaDoc, root: XfaNode, limits: Limits, timeout: std::time::Duration) -> XfaOutcome {
    crate::sandbox::xfa(script, event, doc, &root, limits, Some(timeout), false)
}
