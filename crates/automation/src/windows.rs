//! Window tools: `window_list`, `window_new`, `window_move_tab`, `window_close`.
//!
//! Headless, these keep the same model the desktop app uses
//! ([`pdfcraft_engine::windows::WindowSet`]): every open document has a tab (in the main
//! window, 0, unless moved), `window_new` adds a second view of a document in a window of its
//! own, tabs move between windows, and closing a window closes its tabs, a document with them
//! when no other tab shows it. Edits and undo act on the document, so every tab of it sees
//! them. The running app's windows are driven through the control channel instead
//! (`ui.set window-new`, `window-move-tab`, `window-close`, `window-focus`).

use pdfcraft_engine::DocId;
use pdfcraft_engine::windows::{Tab, WindowId, WindowSet};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

/// The headless window state: every tab, and the windows they are in.
#[derive(Default)]
pub(crate) struct Windows {
    set: WindowSet,
    /// Every tab, in the order opened (the main window's tab order).
    tabs: Vec<Tab>,
    last_key: u64,
}

impl Windows {
    fn new_key(&mut self) -> Result<u64> {
        self.last_key = self.last_key.checked_add(1).ok_or_else(|| failed("too many tabs were opened"))?;
        Ok(self.last_key)
    }
}

fn tab_arg(a: &Args) -> Result<u64> {
    u64::try_from(a.int("tab")?).map_err(|_| ToolError::InvalidArgs("tab must be a tab number from window_list".into()))
}

fn window_arg(a: &Args, key: &str) -> Result<Option<WindowId>> {
    a.opt_int(key)?
        .map(|w| {
            u32::try_from(w)
                .map(WindowId)
                .map_err(|_| ToolError::InvalidArgs(format!("{key} must be a window number from window_list (0 is the main window)")))
        })
        .transpose()
}

impl Automation {
    /// Give every open document a tab (newly opened ones) and forget the tabs of closed ones.
    fn sync_windows(&mut self) -> Result<()> {
        let open: Vec<DocId> = self.session.docs().iter().map(|d| d.id).collect();
        let w = &mut self.windows;
        w.tabs.retain(|t| open.contains(&t.doc));
        for id in open {
            if !w.tabs.iter().any(|t| t.doc == id) {
                let key = w.new_key()?;
                w.tabs.push(Tab { key, doc: id });
            }
        }
        let tabs = &w.tabs;
        w.set.retain(|t| tabs.contains(t));
        Ok(())
    }

    fn windows_json(&self) -> Value {
        let name = |t: &Tab| self.session.get(t.doc).map(|d| d.name.clone());
        let tab = |t: &Tab| json!({ "tab": t.key, "doc": t.doc.0, "name": name(t) });
        let main: Vec<Value> = self.windows.tabs.iter().filter(|t| !self.windows.set.is_torn_off(t.key)).map(tab).collect();
        let mut out = vec![json!({ "window": 0, "main": true, "tabs": main })];
        for w in self.windows.set.windows() {
            out.push(json!({ "window": w.id.0, "main": false, "tabs": w.tabs.iter().map(tab).collect::<Vec<_>>(), "active_tab": w.active }));
        }
        json!({ "windows": out })
    }

    fn find_tab(&self, key: u64) -> Result<Tab> {
        self.windows
            .tabs
            .iter()
            .copied()
            .find(|t| t.key == key)
            .ok_or_else(|| failed(pdfcraft_engine::windows::WindowError::NoSuchTab(key).to_string()))
    }

    /// `window_list`: the windows and their tabs.
    pub(crate) fn window_list(&mut self) -> Result<Value> {
        self.sync_windows()?;
        Ok(self.windows_json())
    }

    /// `window_new`: a second view of `doc` in a new window.
    pub(crate) fn window_new(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?.id;
        self.sync_windows()?;
        let key = self.windows.new_key()?;
        let tab = Tab { key, doc };
        let w = self.windows.set.new_window(tab).map_err(|e| failed(e.to_string()))?;
        self.windows.tabs.push(tab);
        let mut out = self.windows_json();
        out["window"] = json!(w.0);
        out["tab"] = json!(key);
        Ok(out)
    }

    /// `window_move_tab`: move a tab into another window (0 is the main window) or a new one.
    pub(crate) fn window_move_tab(&mut self, a: &Args) -> Result<Value> {
        let key = tab_arg(a)?;
        let to = window_arg(a, "window")?;
        let new = a.opt_bool("new_window")?.unwrap_or(false);
        if to.is_some() == new {
            return Err(ToolError::InvalidArgs("pass either window (a window number; 0 is the main window) or new_window: true".into()));
        }
        self.sync_windows()?;
        let tab = self.find_tab(key)?;
        let w = match to {
            Some(w) => {
                self.windows.set.move_tab(tab, w).map_err(|e| failed(e.to_string()))?;
                w
            }
            None => {
                let from = self.windows.set.window_of(key);
                if self.windows.set.get(from).is_some_and(|w| w.tabs.len() <= 1) {
                    return Err(failed(format!("tab {key} is already alone in window {}", from.0)));
                }
                self.windows.set.new_window(tab).map_err(|e| failed(e.to_string()))?
            }
        };
        let mut out = self.windows_json();
        out["window"] = json!(w.0);
        Ok(out)
    }

    /// `window_close`: close a window and its tabs; a document no other tab shows closes with
    /// them, refused while it has unsaved changes unless `discard_changes`.
    pub(crate) fn window_close(&mut self, a: &Args) -> Result<Value> {
        let w = window_arg(a, "window")?.ok_or_else(|| ToolError::InvalidArgs("missing argument window".into()))?;
        if w.is_main() {
            return Err(failed("window 0 is the main window: close its documents with doc_close"));
        }
        let discard = a.opt_bool("discard_changes")?.unwrap_or(false);
        self.sync_windows()?;
        let tabs = self
            .windows
            .set
            .get(w)
            .map(|win| win.tabs.clone())
            .ok_or_else(|| failed(pdfcraft_engine::windows::WindowError::NoSuchWindow(w).to_string()))?;
        let closing: Vec<DocId> = tabs
            .iter()
            .map(|t| t.doc)
            .filter(|&doc| !self.windows.tabs.iter().any(|o| o.doc == doc && !tabs.iter().any(|c| c.key == o.key)))
            .collect();
        if !discard && let Some(d) = closing.iter().filter_map(|&id| self.session.get(id)).find(|d| d.dirty) {
            return Err(failed(format!(
                "{} has unsaved changes and no other tab shows it: save it with doc_save, or pass discard_changes: true",
                d.name
            )));
        }
        self.windows.set.close_window(w).map_err(|e| failed(e.to_string()))?;
        self.windows.tabs.retain(|t| !tabs.contains(t));
        let mut closed = Vec::new();
        for id in closing {
            if closed.contains(&id.0) {
                continue;
            }
            let _ = self.session.run_document_action(id, pdfcraft_engine::DocTrigger::WillClose);
            self.forget_doc(id);
            closed.push(id.0);
        }
        let mut out = self.windows_json();
        out["closed_documents"] = json!(closed);
        Ok(out)
    }
}
