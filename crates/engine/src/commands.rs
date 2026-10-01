//! The command registry: every user-facing action, by id (architecture §4, "everything is a
//! command").
//!
//! Menus, keyboard shortcuts, the ⌘K palette and automation (CLI `run`, the control channel and
//! MCP later) all go through this table. A frontend implements *what* each id does. The registry
//! says what it is called, where it appears, which key runs it, and when it is enabled, so that
//! every surface agrees.
//!
//! View-local keys (zoom, page navigation, find-next) stay with the document view. They act on
//! view state, not on the document.

use crate::{DocId, Session};

/// When a command can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Needs {
    /// Always.
    Nothing,
    /// A document is open.
    Document,
    /// The open document's security allows page changes (insert, delete, rotate, extract…).
    Assembly,
    /// The open document's security allows content and metadata changes.
    Modification,
    /// There is something to undo.
    Undo,
    /// There is something to redo.
    Redo,
}

/// A keyboard shortcut. `command` is ⌘ on macOS and Ctrl elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub command: bool,
    pub shift: bool,
    /// ⌃ on macOS (in addition to ⌘); unused elsewhere.
    pub mac_ctrl: bool,
    /// Key name: a letter, a digit, or `Delete`.
    pub key: &'static str,
}

impl Shortcut {
    const fn cmd(key: &'static str) -> Self {
        Self { command: true, shift: false, mac_ctrl: false, key }
    }

    const fn cmd_shift(key: &'static str) -> Self {
        Self { command: true, shift: true, mac_ctrl: false, key }
    }

    /// How the shortcut is written in menus: `⇧⌘S` on macOS, `Ctrl+Shift+S` elsewhere.
    pub fn label(&self, mac: bool) -> String {
        if mac {
            let mut s = String::new();
            if self.mac_ctrl {
                s.push('⌃');
            }
            if self.shift {
                s.push('⇧');
            }
            if self.command {
                s.push('⌘');
            }
            s.push_str(self.key);
            s
        } else {
            let mut parts = Vec::new();
            if self.command || self.mac_ctrl {
                parts.push("Ctrl");
            }
            if self.shift {
                parts.push("Shift");
            }
            parts.push(self.key);
            parts.join("+")
        }
    }

    /// Number of modifiers (more specific shortcuts are matched first).
    pub fn modifier_count(&self) -> usize {
        usize::from(self.command) + usize::from(self.shift) + usize::from(self.mac_ctrl)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CommandSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// Top-level menu it appears in (`File`, `Edit`, `View`, `Pages`, `Help`), if any.
    pub menu: Option<&'static str>,
    pub shortcut: Option<Shortcut>,
    pub needs: Needs,
    /// The shortcut also works while a text field has focus.
    pub in_text: bool,
    /// Lucide icon name for palettes and toolbars.
    pub icon: &'static str,
}

const fn c(
    id: &'static str,
    label: &'static str,
    menu: Option<&'static str>,
    shortcut: Option<Shortcut>,
    needs: Needs,
    icon: &'static str,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut, needs, in_text: true, icon }
}

/// Like `c`, but the shortcut is left to text fields while one has focus (⌘Z, ⌘A…).
const fn ct(
    id: &'static str,
    label: &'static str,
    menu: Option<&'static str>,
    shortcut: Option<Shortcut>,
    needs: Needs,
    icon: &'static str,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut, needs, in_text: false, icon }
}

use Needs::*;

const FILE: Option<&str> = Some("File");
const EDIT: Option<&str> = Some("Edit");
const VIEW: Option<&str> = Some("View");
const PAGES: Option<&str> = Some("Pages");
const HELP: Option<&str> = Some("Help");

/// Every command, in menu order.
pub const COMMANDS: &[CommandSpec] = &[
    c("file.open", "Open…", FILE, Some(Shortcut::cmd("O")), Nothing, "folder-open"),
    c("page.combine", "Combine files…", FILE, None, Nothing, "files"),
    c("file.save", "Save", FILE, Some(Shortcut::cmd("S")), Document, "save"),
    c("file.save_as", "Save as…", FILE, Some(Shortcut::cmd_shift("S")), Document, "save"),
    c("file.close", "Close file", FILE, Some(Shortcut::cmd("W")), Document, "x"),
    c("file.properties", "Document properties…", FILE, Some(Shortcut::cmd("D")), Document, "info"),
    ct("edit.undo", "Undo", EDIT, Some(Shortcut::cmd("Z")), Undo, "undo-2"),
    ct("edit.redo", "Redo", EDIT, Some(Shortcut::cmd_shift("Z")), Redo, "redo-2"),
    c("edit.find", "Find…", EDIT, Some(Shortcut::cmd("F")), Document, "search"),
    c("view.palette", "Find tools and commands…", VIEW, Some(Shortcut::cmd("K")), Nothing, "search"),
    c("view.full_screen", "Full screen mode", VIEW, Some(Shortcut::cmd("L")), Document, "maximize"),
    c("view.read_mode", "Read mode", VIEW, Some(Shortcut { command: true, shift: false, mac_ctrl: true, key: "H" }), Document, "book-open"),
    c("view.theme", "Switch light / dark theme", VIEW, None, Nothing, "moon"),
    c("comment.list", "Comments panel", VIEW, None, Document, "message-square-text"),
    c("form.fields", "Form fields panel", VIEW, None, Document, "list"),
    c("protect.properties", "Security properties…", FILE, None, Document, "shield-check"),
    c("page.organize", "Organize pages", PAGES, None, Document, "layout-grid"),
    c("bookmark.add", "New bookmark", PAGES, Some(Shortcut::cmd("B")), Assembly, "bookmark-plus"),
    c("page.rotate", "Rotate pages clockwise", PAGES, None, Assembly, "rotate-cw"),
    c("page.rotate_ccw", "Rotate pages counterclockwise", PAGES, None, Assembly, "rotate-ccw"),
    c("page.delete", "Delete pages", PAGES, None, Assembly, "trash-2"),
    c("page.insert_blank", "Insert blank page", PAGES, None, Assembly, "file-plus"),
    c("page.insert", "Insert pages from file…", PAGES, None, Assembly, "file-input"),
    c("page.extract", "Extract pages…", PAGES, None, Assembly, "file-output"),
    c("page.split", "Split document…", PAGES, None, Assembly, "scissors"),
    c("page.number", "Number pages…", PAGES, None, Assembly, "hash"),
    c("help.shortcuts", "Keyboard shortcuts", HELP, None, Nothing, "circle-help"),
    c("help.about", "About PrintCraft", HELP, None, Nothing, "info"),
];

pub fn command(id: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// Commands that appear in `menu`, in order.
pub fn menu(menu: &str) -> impl Iterator<Item = &'static CommandSpec> + '_ {
    COMMANDS.iter().filter(move |c| c.menu == Some(menu))
}

/// Whether `spec` can run now, for the document `active` (the focused tab).
pub fn is_enabled(spec: &CommandSpec, session: &Session, active: Option<DocId>) -> bool {
    let doc = active.and_then(|id| session.get(id));
    match spec.needs {
        Nothing => true,
        Document => doc.is_some(),
        Assembly => doc.is_some_and(|d| d.allows_assembly()),
        Modification => doc.is_some_and(|d| d.allows_modification()),
        Undo => doc.is_some_and(|d| d.can_undo().is_some()),
        Redo => doc.is_some_and(|d| d.can_redo().is_some()),
    }
}

/// The label to show for `spec` now ("Undo Rotate page" rather than "Undo").
pub fn current_label(spec: &CommandSpec, session: &Session, active: Option<DocId>) -> String {
    let doc = active.and_then(|id| session.get(id));
    match (spec.id, doc) {
        ("edit.undo", Some(d)) => d.can_undo().map(|l| format!("Undo {l}")).unwrap_or_else(|| spec.label.into()),
        ("edit.redo", Some(d)) => d.can_redo().map(|l| format!("Redo {l}")).unwrap_or_else(|| spec.label.into()),
        _ => spec.label.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_shortcuts_are_unique() {
        let mut ids = std::collections::HashSet::new();
        let mut keys = std::collections::HashSet::new();
        for c in COMMANDS {
            assert!(ids.insert(c.id), "duplicate id {}", c.id);
            if let Some(s) = c.shortcut {
                assert!(keys.insert(s), "duplicate shortcut {} ({})", s.label(true), c.id);
            }
        }
    }

    #[test]
    fn every_ready_catalogue_item_is_a_registered_command() {
        for g in crate::catalog::TOOL_GROUPS {
            for s in g.sections {
                for i in s.items {
                    if i.availability == crate::catalog::Availability::Ready {
                        assert!(command(i.command).is_some(), "catalogue item {} ({}) is Ready but not registered", i.label, i.command);
                    }
                }
            }
        }
    }

    #[test]
    fn shortcut_labels_follow_platform_conventions() {
        let s = command("file.save_as").unwrap().shortcut.unwrap();
        assert_eq!(s.label(true), "⇧⌘S");
        assert_eq!(s.label(false), "Ctrl+Shift+S");
        assert_eq!(command("view.read_mode").unwrap().shortcut.unwrap().label(true), "⌃⌘H");
    }

    #[test]
    fn enablement_follows_the_document_state() {
        let mut s = Session::new();
        let undo = command("edit.undo").unwrap();
        let save = command("file.save").unwrap();
        assert!(!is_enabled(save, &s, None));
        assert!(is_enabled(command("file.open").unwrap(), &s, None));
        let id = s.open("a.pdf", None, std::sync::Arc::new(crate::tests::fixture(2)), None).unwrap();
        assert!(is_enabled(save, &s, Some(id)));
        assert!(!is_enabled(undo, &s, Some(id)));
        s.apply(id, crate::Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
        assert!(is_enabled(undo, &s, Some(id)));
        assert_eq!(current_label(undo, &s, Some(id)), "Undo Rotate page");
    }
}
