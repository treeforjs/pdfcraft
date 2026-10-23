//! Tools for frontends that don't link the automation crate.
//!
//! The automation tool table lives above the engine, beside the app's interface, and a frontend
//! may not depend on it (architecture §3). Features built from tools, such as custom commands,
//! reach it through this trait: the app's start-up code, which links both, installs an
//! implementation that runs a tool against the app's own session, so the tool sees exactly the
//! documents the window shows and its edits are the window's (undoable as usual).

use serde_json::Value;

use crate::Session;

/// A tool's name, title and input schema (as `pdfcraft-cli tools` lists them).
#[derive(Clone, Debug, PartialEq)]
pub struct ToolInfo {
    pub name: String,
    pub title: String,
    pub description: String,
    pub schema: Value,
}

/// Runs automation tools for a frontend.
pub trait ToolHost: Send + Sync {
    /// Run tool `name` with `args` (an object) on `session`'s documents. Returns the tool's JSON
    /// result, or its error message. A tool that panics is an error, and the session is kept.
    fn call(&self, session: &mut Session, name: &str, args: &Value) -> Result<Value, String>;

    /// The tools a custom command may wrap: document tools (they take `doc`).
    fn document_tools(&self) -> Vec<ToolInfo>;
}
