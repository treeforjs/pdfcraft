//! pdfcraft-js — PDF JavaScript for forms (L3).
//!
//! Field scripts (format, keystroke, validate, calculate), button actions and document-level
//! scripts run in [boa](https://boajs.dev), a JavaScript engine written in Rust, with the subset
//! of the viewer's object model that forms use, as the public *JavaScript for PDF API Reference*
//! describes it:
//!
//! - `event`: `value`, `change`, `willCommit`, `rc`, `name`, `type`, `target`, `targetName`.
//! - the document (`this`, also the global object): `getField`, `numFields`,
//!   `getNthFieldName`, `numPages`, `pageNum`, `documentFileName`, `info`, `resetForm`,
//!   `calculateNow`, `print`, `submitForm`, `mailDoc`.
//! - Field objects: `name`, `type`, `value`, `valueAsString`, `defaultValue`, `readonly`,
//!   `required`, `display`, `hidden`, `numItems`, `getItemAt`, `currentValueIndices`,
//!   `isBoxChecked`, `checkThisBox`, `exportValues`, `charLimit`, `textColor`, `fillColor`,
//!   `setFocus`.
//! - `app` (`alert`, `beep`, `response`, `launchURL`, `viewerType`, `viewerVersion`,
//!   `platform`, `setTimeOut`), `util` (`printf`, `printd`, `printx`), `console` (`println`,
//!   `show`, `clear`), `display` and `color`.
//! - comments: `this.getAnnots`, `getAnnot` and `addAnnot`, and Annotation objects (`type`,
//!   `page`, `name`, `author`, `contents`, `rect`, `readOnly`, `destroy`).
//! - layers: `this.getOCGs`, and OCG objects (`name`, `state`).
//!
//! Every run uses a fresh, fuel-metered WebAssembly store with a capped linear memory.
//! The kernel has no file or network capabilities; parsing, built-ins and script instructions
//! all share the work budget. No timed-out script remains running. Side effects (alerts, field and comment changes, resets, printing, navigation,
//! layer visibility) are returned in an [`Outcome`] for the caller to apply.
//!
//! Dialogs that need an answer (`app.alert` with Yes/No or OK/Cancel buttons, `app.response`)
//! can't stop the script while the viewer asks. With [`Dialogs::interactive`], the first
//! unanswered one ends the run with [`Outcome::question`] and nothing else: the caller asks the
//! user and runs the script again with the answers so far. A script's only effects are its
//! outcome, so the next run takes the same path up to the next question (unless the script
//! reads the clock).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod encode_budget;
mod protocol;
mod wire;
mod xfa_protocol;
pub use protocol::*;

#[cfg(target_os = "wasi")]
include!("kernel.rs");
#[cfg(target_os = "wasi")]
#[path = "formcalc_kernel.rs"]
pub mod formcalc;
#[cfg(target_os = "wasi")]
mod guest;
#[cfg(target_os = "wasi")]
mod objects;
#[cfg(target_os = "wasi")]
#[path = "xfa_kernel.rs"]
pub mod xfa;
#[cfg(target_os = "wasi")]
pub use guest::main as guest_main;

#[cfg(not(target_os = "wasi"))]
mod runner;
#[cfg(not(target_os = "wasi"))]
mod sandbox;
#[cfg(not(target_os = "wasi"))]
pub use sandbox::{run, run_with, run_within};
#[cfg(not(target_os = "wasi"))]
pub mod formcalc;
#[cfg(not(target_os = "wasi"))]
pub mod xfa;

#[cfg(all(test, not(target_os = "wasi")))]
mod tests;

#[cfg(all(test, not(target_os = "wasi")))]
mod containment;
