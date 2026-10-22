//! The JavaScript debugger: run a script with breakpoints, stepping and watch expressions,
//! under a step budget.
//!
//! boa has no debugger interface (its `debugger` statement compiles to nothing), so the
//! debugger works with what it does offer:
//!
//! - **A step budget.** Scripts run through boa's budgeted evaluation, which hands control back
//!   every few thousand instruction "cycles"; a run that uses up [`Debug::max_steps`] is
//!   stopped where it is, even inside nested loops the loop limit would let through. (Native
//!   callbacks such as `Array.prototype.forEach` run without handing control back, so inside
//!   them only the loop and recursion limits apply.)
//! - **Probes.** Each line where a statement starts (and inserting one changes nothing) gets a
//!   call to a native probe on the same line, so line numbers stay as they are. A probe knows
//!   the call stack, and the closure it is given evaluates watch expressions in the paused
//!   scope (local variables included).
//! - **Replay.** A run never applies anything until it ends, and the sandbox is deterministic
//!   (apart from `Date` and `Math.random`), so pausing is "stop at the n-th probe" and resuming
//!   runs the script again to the next stop after that one. Each step costs a run from the
//!   start, bounded by the same budget.
//!
//! Lines a probe can't be put on (inside a multi-line expression, a template, an object
//! literal…) can't hold a breakpoint and stepping passes over them; [`DebugOutcome::breakable`]
//! lists the lines that can.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::rc::Rc;
use std::task::{Context as TaskContext, Poll, Waker};

use boa_engine::object::builtins::JsFunction;
use boa_engine::{Context, JsResult, JsValue, NativeFunction, Script, Source, js_string};

use crate::{
    DocInfo, ErrorAt, Event, FieldState, Limits, Outcome, SCRIPT_PATH, context, doc_script_path, error_location, error_message, finish, globals,
    read_event, refuse, script_of_path, text,
};

/// The step budget a debug run gets unless it asks for another.
pub const DEFAULT_STEPS: u64 = 20_000_000;
/// The largest step budget a debug run may ask for.
pub const MAX_STEPS: u64 = 500_000_000;
/// Most watch expressions evaluated at a pause.
pub const MAX_WATCHES: usize = 32;
/// Longest watch expression, in bytes.
pub const MAX_WATCH_BYTES: usize = 4096;
/// Longest watch value shown, in characters.
const MAX_VALUE_CHARS: usize = 2000;
/// Most call stack frames reported at a pause.
const MAX_STACK: usize = 64;
/// Scripts longer than this run without probes (no breakpoints or stepping).
const MAX_INSTRUMENTED_BYTES: usize = 128 * 1024;
/// Most parses spent finding the lines probes can go on.
const MAX_VERIFY_PARSES: usize = 48;
/// The instruction "cycles" between budget checks.
const BUDGET: u32 = 4096;
/// The probe's global name.
const PROBE: &str = "__pdfcraft_debug_probe";

/// How a debug run goes on from where it starts ([`Debug::resume`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Step {
    /// To the next breakpoint, or the end.
    #[default]
    Run,
    /// To the next line, entering calls.
    Into,
    /// To the next line in the same function or a caller.
    Over,
    /// To the next line in a caller.
    Out,
}

impl Step {
    pub fn id(self) -> &'static str {
        match self {
            Step::Run => "run",
            Step::Into => "into",
            Step::Over => "over",
            Step::Out => "out",
        }
    }

    pub fn from_id(id: &str) -> Option<Step> {
        [Step::Run, Step::Into, Step::Over, Step::Out].into_iter().find(|s| s.id() == id)
    }
}

/// Where a paused run was ([`Pause::resume`]): which probe it stopped at, and how deep the
/// call stack was.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Resume {
    pub hit: u64,
    pub depth: usize,
}

/// What a debug run should do.
#[derive(Clone, Debug, PartialEq)]
pub struct Debug {
    /// 1-based lines of the script to stop at.
    pub breakpoints: Vec<u32>,
    /// Expressions to evaluate where the run stops.
    pub watches: Vec<String>,
    pub step: Step,
    /// Go on from this pause (`None`: from the start; `step` is then [`Step::Run`] to stop at
    /// the first breakpoint, or [`Step::Into`] to stop on the first line).
    pub resume: Option<Resume>,
    /// The step budget (instruction "cycles"), at most [`MAX_STEPS`].
    pub max_steps: u64,
    /// How long to wait for the run, in milliseconds (at most [`MAX_TIMEOUT_MS`]): a run still
    /// going then (stuck inside a built-in, where steps aren't counted) is abandoned on its
    /// thread and reported as stopped.
    pub timeout_ms: u64,
}

impl Default for Debug {
    fn default() -> Self {
        Debug {
            breakpoints: Vec::new(),
            watches: Vec::new(),
            step: Step::Run,
            resume: None,
            max_steps: DEFAULT_STEPS,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

/// How long a debug run is waited for unless it asks otherwise.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
/// The longest a debug run may ask to be waited for.
pub const MAX_TIMEOUT_MS: u64 = 60_000;

/// A call stack frame at a pause, innermost first.
#[derive(Clone, Debug, PartialEq)]
pub struct StackFrame {
    /// The function's name (empty for the script's top level).
    pub function: String,
    /// The script it is in: `None` the debugged script, `Some(i)` document-level script `i`.
    pub script: Option<usize>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// A watch expression's value at a pause.
#[derive(Clone, Debug, PartialEq)]
pub struct Watch {
    pub expression: String,
    pub value: Option<String>,
    pub error: Option<String>,
}

/// Where a debug run stopped.
#[derive(Clone, Debug, PartialEq)]
pub struct Pause {
    /// The 1-based line about to run.
    pub line: u32,
    /// Which probe stopped it (counting from the start of the run).
    pub hit: u64,
    /// The call stack's depth.
    pub depth: usize,
    pub stack: Vec<StackFrame>,
    pub watches: Vec<Watch>,
    /// Whether a breakpoint stopped it (otherwise a step did).
    pub breakpoint: bool,
    /// How much console output and how many alerts there were when it stopped (the script
    /// runs on a little before the debugger gets control back; that part doesn't count).
    printed: (usize, usize),
}

impl Pause {
    /// What to pass as [`Debug::resume`] to go on from here.
    pub fn resume(&self) -> Resume {
        Resume { hit: self.hit, depth: self.depth }
    }
}

/// What a debug run did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DebugOutcome {
    /// As [`crate::run`] gives it when the run finished. When it paused or used up its budget,
    /// only the console output and alerts so far are kept (nothing else may be applied).
    pub outcome: Outcome,
    pub paused: Option<Pause>,
    /// The lines a breakpoint can be set on.
    pub breakable: Vec<u32>,
    /// The steps used.
    pub steps: u64,
    /// The run used up its step budget and was stopped.
    pub out_of_steps: bool,
}

// ── finding the lines probes can go on ──────────────────────────────────────────────────────

/// A line a probe can go on: its number and the byte offset of its first character.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Spot {
    line: u32,
    at: usize,
}

/// One line as the scanner saw it.
#[derive(Default)]
struct LineScan {
    /// It starts inside a comment, string or template.
    inside: bool,
    /// Byte offset of its first character of code.
    first: Option<usize>,
    /// Its last character of code.
    last: Option<u8>,
    /// The word it starts with.
    word: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Scan {
    Code,
    LineComment,
    BlockComment,
    Str(u8),
    Template,
}

fn scan_lines(src: &str) -> Vec<LineScan> {
    let b = src.as_bytes();
    let mut lines = vec![LineScan::default()];
    let mut state = Scan::Code;
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'\n' {
            if state == Scan::LineComment || matches!(state, Scan::Str(_)) {
                state = Scan::Code;
            }
            lines.push(LineScan { inside: state != Scan::Code, ..Default::default() });
            i += 1;
            continue;
        }
        let Some(cur) = lines.last_mut() else { break };
        match state {
            Scan::LineComment => {}
            Scan::BlockComment => {
                if c == b'*' && b.get(i + 1) == Some(&b'/') {
                    state = Scan::Code;
                    i += 1;
                }
            }
            Scan::Str(_) | Scan::Template if c == b'\\' => {
                // An escaped line break continues the string on the next line.
                if b.get(i + 1) == Some(&b'\n') {
                    lines.push(LineScan { inside: true, ..Default::default() });
                }
                i += 1;
            }
            Scan::Str(q) => {
                if c == q {
                    state = Scan::Code;
                }
            }
            Scan::Template => {
                if c == b'`' {
                    state = Scan::Code;
                    cur.last = Some(c);
                }
            }
            Scan::Code => {
                if c == b'/' && b.get(i + 1) == Some(&b'/') {
                    state = Scan::LineComment;
                    i += 2;
                    continue;
                }
                if c == b'/' && b.get(i + 1) == Some(&b'*') {
                    state = Scan::BlockComment;
                    i += 2;
                    continue;
                }
                if !c.is_ascii_whitespace() {
                    if cur.first.is_none() {
                        cur.first = Some(i);
                        cur.word =
                            src.get(i..).unwrap_or("").chars().take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '$').collect();
                    }
                    cur.last = Some(c);
                }
                match c {
                    b'\'' | b'"' => state = Scan::Str(c),
                    b'`' => state = Scan::Template,
                    _ => {}
                }
            }
        }
        i += 1;
    }
    lines
}

/// Lines where a statement starts for sure: the previous line of code ends one (`;`, `{`, `}`,
/// or a `case`/`default` label) and this one doesn't continue an expression or a statement
/// (`else`, `catch`, …).
fn candidates(src: &str) -> Vec<Spot> {
    let lines = scan_lines(src);
    let mut out = Vec::new();
    // The previous line of code: its last character and first word.
    let mut prev: Option<(u8, String)> = None;
    for (n, l) in lines.iter().enumerate() {
        if let Some(at) = l.first
            && !l.inside
        {
            let first = src.as_bytes().get(at).copied().unwrap_or(b' ');
            let starts_ok = !b".)]},?:=\"'`".contains(&first) && !["else", "catch", "finally", "case", "default"].contains(&l.word.as_str());
            let after_ok = match &prev {
                None => true,
                Some((b';' | b'{', _)) => true,
                // After a block, a line opening with one of these would join the expression
                // before it (an immediately called function, an index, a template…).
                Some((b'}', _)) => !b"([+-/".contains(&first),
                Some((b':', w)) => w == "case" || w == "default",
                Some(_) => false,
            };
            if starts_ok && after_ok {
                out.push(Spot { line: u32::try_from(n).unwrap_or(u32::MAX).saturating_add(1), at });
            }
        }
        if let Some(last) = l.last {
            let word = if l.first.is_some() && !l.inside { l.word.clone() } else { String::new() };
            prev = Some((last, word));
        }
    }
    out
}

/// The probe call put in front of line `line`.
fn probe_text(line: u32) -> String {
    format!("{PROBE}({line}, function (__pdfcraft_x) {{ return eval(__pdfcraft_x); }});")
}

/// `src` with probes in front of `spots` (sorted).
fn instrument(src: &str, spots: &[Spot]) -> String {
    let mut out = String::with_capacity(src.len().saturating_add(spots.len().saturating_mul(80)));
    let mut from = 0;
    for s in spots {
        let Some(piece) = src.get(from..s.at) else { continue };
        out.push_str(piece);
        out.push_str(&probe_text(s.line));
        from = s.at;
    }
    out.push_str(src.get(from..).unwrap_or(""));
    out
}

fn parses(ctx: &mut Context, src: &str) -> bool {
    Script::parse(Source::from_bytes(src.as_bytes()), None, ctx).is_ok()
}

/// The candidates whose probes still let the script parse, found in groups (a group that
/// fails is split) within [`MAX_VERIFY_PARSES`] parses.
fn verify(ctx: &mut Context, src: &str, spots: Vec<Spot>) -> Vec<Spot> {
    if spots.is_empty() || src.len() > MAX_INSTRUMENTED_BYTES || !parses(ctx, src) {
        return Vec::new();
    }
    let mut accepted: Vec<Spot> = Vec::new();
    let mut work = vec![spots];
    let mut used = 1;
    while let Some(group) = work.pop() {
        if used >= MAX_VERIFY_PARSES {
            break;
        }
        let mut trial = accepted.clone();
        trial.extend(group.iter().copied());
        trial.sort();
        used += 1;
        if parses(ctx, &instrument(src, &trial)) {
            accepted = trial;
        } else if group.len() > 1 {
            let mut group = group;
            let back = group.split_off(group.len() / 2);
            // Earlier lines first.
            work.push(back);
            work.push(group);
        }
    }
    accepted
}

/// The 1-based lines of `script` a breakpoint can be set on.
pub fn breakable_lines(script: &str) -> Vec<u32> {
    if refuse(script).is_some() {
        return Vec::new();
    }
    let mut ctx = Context::default();
    verify(&mut ctx, script, candidates(script)).iter().map(|s| s.line).collect()
}

// ── running ─────────────────────────────────────────────────────────────────────────────────

struct DebugState {
    hits: u64,
    resume: Resume,
    step: Step,
    breakpoints: BTreeSet<u32>,
    watches: Vec<String>,
    pause: Option<Pause>,
    /// The line of the last probe passed (where an error without a position happened).
    last_line: Option<u32>,
}

type SharedState = Rc<RefCell<DebugState>>;

/// A watch value as the debugger shows it: strings quoted, other values as `String()` makes
/// them.
fn describe(v: &JsValue, ctx: &mut Context) -> JsResult<String> {
    if v.is_undefined() {
        return Ok("undefined".into());
    }
    if v.is_null() {
        return Ok("null".into());
    }
    let t = text(v, ctx)?;
    let t = if v.is_string() { format!("{t:?}") } else { t };
    Ok(if t.chars().count() > MAX_VALUE_CHARS { t.chars().take(MAX_VALUE_CHARS).chain("…".chars()).collect() } else { t })
}

/// The probe: count it, and stop here if a breakpoint or the step asks to.
fn probe(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let Some(state) = ctx.get_data::<SharedState>().cloned() else { return Ok(JsValue::undefined()) };
    let line = args.first().and_then(JsValue::as_number).filter(|n| n.is_finite() && *n >= 0.0).map_or(0, |n| n.min(f64::from(u32::MAX)) as u32);
    let depth = ctx.stack_trace().count();
    let (hit, breakpoint, watches) = {
        let mut st = state.borrow_mut();
        if st.pause.is_some() {
            return Ok(JsValue::undefined());
        }
        st.hits = st.hits.saturating_add(1);
        st.last_line = Some(line);
        if st.hits <= st.resume.hit {
            return Ok(JsValue::undefined());
        }
        let breakpoint = st.breakpoints.contains(&line);
        let stepped = match st.step {
            Step::Run => false,
            Step::Into => true,
            Step::Over => depth <= st.resume.depth,
            Step::Out => depth < st.resume.depth,
        };
        if !breakpoint && !stepped {
            return Ok(JsValue::undefined());
        }
        (st.hits, breakpoint, st.watches.clone())
    };
    let stack = ctx
        .stack_trace()
        .take(MAX_STACK)
        .filter_map(|f| {
            let p = f.position();
            let script = script_of_path(&p.path.to_string())?;
            Some(StackFrame {
                function: p.function_name.to_std_string_escaped(),
                script,
                line: p.position.map(|x| x.line_number()),
                column: p.position.map(|x| x.column_number()),
            })
        })
        .collect();
    let printed = crate::host(ctx).map(|h| (h.borrow().console.len(), h.borrow().alerts.len())).unwrap_or_default();
    // Paused before the watches run, so probes they pass (a watch can call a function) don't count.
    state.borrow_mut().pause = Some(Pause { line, hit, depth, stack, watches: Vec::new(), breakpoint, printed });
    let eval = args.get(1).and_then(JsValue::as_object).and_then(JsFunction::from_object);
    let mut values = Vec::new();
    for w in watches.iter().take(MAX_WATCHES) {
        let r = match &eval {
            Some(f) => f.call(&JsValue::undefined(), &[JsValue::from(boa_engine::JsString::from(w.as_str()))], ctx).and_then(|v| describe(&v, ctx)),
            None => Err(crate::error("the watch can't be evaluated here")),
        };
        values.push(match r {
            Ok(v) => Watch { expression: w.clone(), value: Some(v), error: None },
            Err(e) => Watch { expression: w.clone(), value: None, error: Some(error_message(&e.to_string())) },
        });
    }
    if let Some(p) = state.borrow_mut().pause.as_mut() {
        p.watches = values;
    }
    Ok(JsValue::undefined())
}

/// How a budgeted evaluation ended.
enum Ran {
    Done(JsResult<JsValue>),
    Paused,
    OutOfSteps,
}

/// Evaluate `src` (named `path`) in steps of [`BUDGET`], stopping when a probe pauses the run or
/// `steps` reaches `max`.
fn run_budgeted(ctx: &mut Context, src: &str, path: &str, state: &SharedState, steps: &mut u64, max: u64) -> Ran {
    let script = match Script::parse(Source::from_bytes(src.as_bytes()).with_path(Path::new(path)), None, ctx) {
        Ok(s) => s,
        Err(e) => return Ran::Done(Err(e)),
    };
    let mut task = TaskContext::from_waker(Waker::noop());
    let mut run = std::pin::pin!(script.evaluate_async_with_budget(ctx, BUDGET));
    loop {
        match run.as_mut().poll(&mut task) {
            Poll::Ready(r) => {
                return if state.borrow().pause.is_some() { Ran::Paused } else { Ran::Done(r) };
            }
            Poll::Pending => {
                *steps = steps.saturating_add(u64::from(BUDGET));
                if state.borrow().pause.is_some() {
                    return Ran::Paused;
                }
                if *steps >= max {
                    return Ran::OutOfSteps;
                }
            }
        }
    }
}

/// Run `script` like [`crate::run`] (as the console does: dialogs get Cancel or No), with
/// breakpoints, stepping and watches. Nothing the script did counts unless the run finished:
/// a paused run keeps only what it printed and alerted so far.
pub fn run_debug(
    script: &str,
    event: &Event,
    doc: &DocInfo,
    fields: &[FieldState],
    doc_scripts: &[String],
    limits: Limits,
    debug: &Debug,
) -> DebugOutcome {
    let failed = |why: String| DebugOutcome {
        outcome: Outcome { rc: true, value: event.value.clone(), change: event.change.clone(), error: Some(why), ..Default::default() },
        ..Default::default()
    };
    if let Some(why) = std::iter::once(script).chain(doc_scripts.iter().map(String::as_str)).find_map(refuse) {
        return failed(why);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let timeout = std::time::Duration::from_millis(debug.timeout_ms.clamp(100, MAX_TIMEOUT_MS));
        let (tx, rx) = std::sync::mpsc::channel();
        let (script, event, doc, fields, doc_scripts, debug) =
            (script.to_string(), event.clone(), doc.clone(), fields.to_vec(), doc_scripts.to_vec(), debug.clone());
        let spawned = std::thread::Builder::new().name("pdfcraft-js-debug".into()).stack_size(crate::SCRIPT_STACK).spawn(move || {
            // The receiver is gone when the caller stopped waiting: nothing to report then.
            let _ = tx.send(debug_here(&script, &event, &doc, &fields, &doc_scripts, limits, &debug));
        });
        if let Err(e) = spawned {
            return failed(format!("the script engine could not start: {e}"));
        }
        match rx.recv_timeout(timeout) {
            Ok(o) => o,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let mut o = failed(format!("the script ran longer than {:.1} s and was abandoned", timeout.as_secs_f64()));
                o.out_of_steps = true;
                o
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => failed("the script stopped with an internal error".into()),
        }
    }
    #[cfg(target_arch = "wasm32")]
    debug_here(script, event, doc, fields, doc_scripts, limits, debug)
}

fn debug_here(
    script: &str,
    event: &Event,
    doc: &DocInfo,
    fields: &[FieldState],
    doc_scripts: &[String],
    limits: Limits,
    debug: &Debug,
) -> DebugOutcome {
    let mut ctx = context(doc, fields, limits, &crate::Dialogs::default());
    let max = debug.max_steps.clamp(u64::from(BUDGET), MAX_STEPS);
    let spots = verify(&mut ctx, script, candidates(script));
    let source = instrument(script, &spots);
    let state: SharedState = Rc::new(RefCell::new(DebugState {
        hits: 0,
        resume: debug.resume.unwrap_or_default(),
        step: debug.step,
        breakpoints: debug.breakpoints.iter().copied().collect(),
        watches: debug.watches.iter().take(MAX_WATCHES).map(|w| w.chars().take(MAX_WATCH_BYTES).collect()).collect(),
        pause: None,
        last_line: None,
    }));
    ctx.insert_data(state.clone());
    let mut out = Outcome { rc: true, value: event.value.clone(), change: event.change.clone(), ..Default::default() };
    let mut steps = 0u64;
    let mut running: Option<usize> = None;
    let mut ended: Option<Ran> = None;
    let result = (|| -> JsResult<()> {
        let ev = globals(&mut ctx, event)?;
        ctx.register_global_builtin_callable(js_string!(PROBE), 2, NativeFunction::from_fn_ptr(probe))?;
        for (i, d) in doc_scripts.iter().enumerate() {
            running = Some(i);
            match run_budgeted(&mut ctx, d, &doc_script_path(i), &state, &mut steps, max) {
                Ran::Done(r) => {
                    r?;
                }
                other => {
                    ended = Some(other);
                    return Ok(());
                }
            }
        }
        running = None;
        match run_budgeted(&mut ctx, &source, SCRIPT_PATH, &state, &mut steps, max) {
            Ran::Done(r) => {
                let v = r?;
                read_event(&mut ctx, &ev, &v, &mut out)
            }
            other => {
                ended = Some(other);
                Ok(())
            }
        }
    })();
    let last_line = state.borrow().last_line;
    if let Err(e) = result {
        let full = e.to_string();
        out.error = Some(error_message(&full));
        out.error_at = error_location(&full, running)
            .map(|at| unshift(at, &spots))
            .or_else(|| running.is_none().then_some(last_line).flatten().map(|line| ErrorAt { script: None, line, column: 0 }));
    }
    let out_of_steps = matches!(ended, Some(Ran::OutOfSteps));
    if out_of_steps {
        // Where it was when the budget ran out.
        let at = ctx.stack_trace().next().map(|f| f.position()).and_then(|p| {
            let script = script_of_path(&p.path.to_string())?;
            let pos = p.position?;
            Some(unshift(ErrorAt { script, line: pos.line_number(), column: pos.column_number() }, &spots))
        });
        out.error = Some(format!("the script ran too long and was stopped after {steps} steps"));
        out.error_at = at.or_else(|| running.is_none().then_some(last_line).flatten().map(|line| ErrorAt { script: None, line, column: 0 }));
    }
    let paused = state.borrow_mut().pause.take();
    let mut outcome = finish(&mut ctx, event, doc, out);
    if let Some(p) = &paused {
        outcome.console.truncate(p.printed.0);
        outcome.alerts.truncate(p.printed.1);
    }
    if paused.is_some() || out_of_steps {
        // Only finished runs change anything.
        outcome.changed.clear();
        outcome.annots.clear();
        outcome.requests.clear();
        outcome.result = None;
        outcome.rc = true;
        outcome.value = event.value.clone();
        outcome.change = event.change.clone();
    }
    DebugOutcome { outcome, paused, breakable: spots.iter().map(|s| s.line).collect(), steps, out_of_steps }
}

/// An error position in the instrumented script, as a position in the script itself: a probe
/// put in front of the line moves the rest of it to the right.
fn unshift(mut at: ErrorAt, spots: &[Spot]) -> ErrorAt {
    if at.script.is_none() && spots.iter().any(|s| s.line == at.line) {
        let shift = u32::try_from(probe_text(at.line).chars().count()).unwrap_or(0);
        if at.column > shift {
            at.column -= shift;
        }
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    fn go(script: &str, debug: &Debug) -> DebugOutcome {
        run_debug(
            script,
            &Event::doc("Console"),
            &DocInfo::default(),
            &[FieldState::new("total", crate::FieldType::Text, vec!["1".into()])],
            &[],
            Limits::default(),
            debug,
        )
    }

    #[test]
    fn statement_lines_take_breakpoints() {
        let src = "var a = 1;\nvar b = a +\n  2;\nif (a) {\n  b = 3;\n}\nelse b = 4;\nvar o = {\n  x: 1\n};\nvar s = `x\ny`;\n// note\nb;";
        assert_eq!(breakable_lines(src), [1, 2, 4, 5, 8, 11, 14]);
        assert!(breakable_lines("var x = ;").is_empty(), "a script that doesn't parse has none");
    }

    #[test]
    fn breakpoints_pause_and_steps_replay() {
        let src = "function sq(n) {\n  var r = n * n;\n  return r;\n}\nvar a = 2;\nvar b = sq(a);\nconsole.println(b);\nb + 1";
        let d = Debug { breakpoints: vec![6], watches: vec!["a".into(), "b".into(), "nosuch".into()], ..Default::default() };
        let o = go(src, &d);
        let p = o.paused.clone().unwrap();
        assert_eq!((p.line, p.breakpoint), (6, true));
        assert_eq!(p.watches[0].value.as_deref(), Some("2"));
        assert_eq!(p.watches[1].value.as_deref(), Some("undefined"));
        assert!(p.watches[2].error.is_some());
        assert!(o.outcome.console.is_empty() && o.outcome.result.is_none());
        // Step into the call: line 2, inside sq, with its local in scope.
        let d2 = Debug { step: Step::Into, resume: Some(p.resume()), watches: vec!["n".into()], ..d.clone() };
        let p2 = go(src, &d2).paused.unwrap();
        assert_eq!(p2.line, 2);
        assert_eq!(p2.stack[0].function, "sq");
        assert_eq!(p2.stack.len(), 2);
        assert_eq!(p2.watches[0].value.as_deref(), Some("2"));
        // Step out: back in the caller, on the next line.
        let p3 = go(src, &Debug { step: Step::Out, resume: Some(p2.resume()), ..d.clone() }).paused.unwrap();
        assert_eq!(p3.line, 7);
        // Step over from line 6 doesn't enter sq.
        let p4 = go(src, &Debug { step: Step::Over, resume: Some(p.resume()), ..d.clone() }).paused.unwrap();
        assert_eq!(p4.line, 7);
        // Continue to the end.
        let end = go(src, &Debug { step: Step::Run, resume: Some(p4.resume()), ..d });
        assert!(end.paused.is_none());
        assert_eq!(end.outcome.console, ["4"]);
        assert_eq!(end.outcome.result.as_deref(), Some("5"));
    }

    #[test]
    fn errors_have_lines() {
        let o = go("var a = 1;\n\nfunction f() {\n  throw new Error('boom');\n}\nf();", &Debug::default());
        assert_eq!(o.outcome.error.as_deref(), Some("Error: boom"));
        assert_eq!(o.outcome.error_at.map(|e| (e.script, e.line)), Some((None, 4)));
        let o = go("var a = 1;\nvar b = ;", &Debug::default());
        assert_eq!(o.outcome.error_at.map(|e| e.line), Some(2));
        let o = go("var a = 1;\nnosuch();", &Debug::default());
        assert_eq!(o.outcome.error_at.map(|e| e.line), Some(2), "{:?}", o.outcome);
        // Plain runs say where, too.
        let o = crate::run("var a = 1;\nnull.x;", &Event::doc("Console"), &DocInfo::default(), &[], &[], Limits::default());
        assert_eq!(o.error_at.map(|e| e.line), Some(2), "{o:?}");
        assert!(!o.error.unwrap_or_default().contains("script:"));
        let o =
            crate::run("1", &Event::doc("Console"), &DocInfo::default(), &[], &["var x = 1;\nthrow new Error('init');".into()], Limits::default());
        assert_eq!(o.error_at, Some(ErrorAt { script: Some(0), line: 2, column: o.error_at.map_or(0, |e| e.column) }));
    }

    /// Hostile scripts: nested loops the per-frame loop limit lets through, deep recursion,
    /// huge allocations, catastrophic nesting, and garbage all end with an error, never a hang
    /// or a crash.
    #[test]
    fn hostile_scripts_are_bounded() {
        let d = Debug { max_steps: 2_000_000, ..Default::default() };
        let start = std::time::Instant::now();
        let o = go("function inner() { for (var i = 0; i < 900000; i++) {} }\nfor (var j = 0; j < 900000; j++) inner();", &d);
        assert!(o.out_of_steps, "{o:?}");
        assert!(o.outcome.error.as_deref().unwrap_or("").contains("too long"));
        assert!(start.elapsed() < std::time::Duration::from_secs(20));
        for s in [
            "while (true) {}",
            "function f() { return f(); } f();",
            "var s = 'x'; while (true) s = s + s;",
            "new Array(4294967295).fill(0)",
            "for (;;) { try { for (;;) {} } catch (e) {} }",
            "\u{0}\u{ffff}garbage((",
            "debugger; debugger;",
        ] {
            let o = go(s, &d);
            assert!(o.outcome.error.is_some() || o.out_of_steps || o.outcome.result.is_some() || s.starts_with("debugger"), "{s}: {o:?}");
        }
        let deep = format!("var a = {}1{};", "(".repeat(100_000), ")".repeat(100_000));
        assert!(go(&deep, &d).outcome.error.is_some());
        // Stuck inside a built-in's callback, where steps aren't counted: abandoned on time.
        let start = std::time::Instant::now();
        let o = go(
            "[1, 2, 3].forEach(function () { for (var j = 0; j < 900000; j++) { (function () { for (var i = 0; i < 900000; i++) {} })(); } });",
            &Debug { timeout_ms: 300, ..d.clone() },
        );
        assert!(o.out_of_steps && o.outcome.error.is_some(), "{o:?}");
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        // A script that tries to fake or reach the probe gets nothing from it.
        let o = go(
            "__pdfcraft_debug_probe(1, 5); __pdfcraft_debug_probe('x'); __pdfcraft_debug_probe(); 7",
            &Debug { step: Step::Into, ..Default::default() },
        );
        assert!(o.paused.is_some() || o.outcome.result.as_deref() == Some("7"), "{o:?}");
        // Hostile watches.
        let d = Debug {
            breakpoints: vec![2],
            watches: vec!["(function f(){ return f(); })()".into(), "x".repeat(100_000), "}{".into()],
            ..Default::default()
        };
        let p = go("var x = 1;\nx = 2;", &d).paused.unwrap();
        assert!(p.watches.iter().all(|w| w.error.is_some() || w.value.is_some()));
    }

    #[test]
    fn paused_runs_change_nothing() {
        let src = "getField('total').value = 9;\napp.alert('hi');\nconsole.println('x');\nthis.pageNum = 2;";
        let o = go(src, &Debug { breakpoints: vec![4], ..Default::default() });
        assert!(o.paused.is_some());
        assert!(o.outcome.changed.is_empty() && o.outcome.requests.is_empty());
        assert_eq!(o.outcome.console, ["x"]);
        let o = go(src, &Debug::default());
        assert_eq!(o.outcome.changed.len(), 1);
    }
}
