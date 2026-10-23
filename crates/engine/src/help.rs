//! Help: the keyboard reference and the user guide.
//!
//! The keyboard reference is generated from the command registry ([`crate::commands::COMMANDS`])
//! plus [`VIEW_KEYS`], the keys the document view handles itself, so Help → Keyboard shortcuts,
//! the `help_shortcuts` tool and `docs/user/keyboard.md` can't disagree with the real bindings. A
//! unit test fails when `docs/user/keyboard.md` is out of date (`UPDATE_USER_DOCS=1` rewrites it).
//!
//! The user guide is `docs/user/*.md`, bundled into the binary so Help → User guide works offline
//! and nothing is fetched. Pages are plain Markdown; [`GuidePage::title`] is the first `# ` line.

use crate::commands::{COMMANDS, CommandSpec};

/// A key the document view handles itself (not a registry command).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewKey {
    /// How it is written on macOS (and in the web app).
    pub mac: &'static str,
    /// How it is written on Windows, Linux and FreeBSD.
    pub other: &'static str,
    /// What it does, in English (the UI translates it).
    pub action: &'static str,
    pub section: &'static str,
}

/// The section of registry commands that are in no menu.
pub const TOOLS_SECTION: &str = "Tools";
/// The section of the keys the document view handles.
pub const VIEW_SECTION: &str = "Document view";
/// The section of the keys the page grid handles.
pub const ORGANIZE_SECTION: &str = "Organize pages";

/// Sections in the order the reference lists them.
pub const SECTIONS: [&str; 8] = ["File", "Edit", "View", "Pages", "Help", TOOLS_SECTION, VIEW_SECTION, ORGANIZE_SECTION];

const fn vk(mac: &'static str, other: &'static str, action: &'static str, section: &'static str) -> ViewKey {
    ViewKey { mac, other, action, section }
}

/// Keys handled by the document view and the page grid (`ui-egui` `canvas::shortcuts`).
pub const VIEW_KEYS: &[ViewKey] = &[
    vk("⌘G / ⇧⌘G", "Ctrl+G / Ctrl+Shift+G", "Next / previous match", VIEW_SECTION),
    vk("⌘C", "Ctrl+C", "Copy selected text", VIEW_SECTION),
    vk("Double-click", "Double-click", "Select a word", VIEW_SECTION),
    vk("Esc", "Esc", "Clear selection / close find", VIEW_SECTION),
    vk("⌘1", "Ctrl+1", "Actual size", VIEW_SECTION),
    vk("⌘0", "Ctrl+0", "Zoom to page level", VIEW_SECTION),
    vk("⌘2", "Ctrl+2", "Fit to width", VIEW_SECTION),
    vk("⌘+ / ⌘−", "Ctrl++ / Ctrl+−", "Zoom in / out (also pinch or ⌘-scroll)", VIEW_SECTION),
    vk("⇧⌘+ / ⇧⌘−", "Ctrl+Shift++ / Ctrl+Shift+−", "Rotate view", VIEW_SECTION),
    vk("⌘[", "Ctrl+[", "Previous view", VIEW_SECTION),
    vk("⌘]", "Ctrl+]", "Next view", VIEW_SECTION),
    vk("Home / End", "Home / End", "First / last page", VIEW_SECTION),
    vk("← / →, ⌘Page Up / ⌘Page Down", "← / →, Ctrl+Page Up / Ctrl+Page Down", "Previous / next page", VIEW_SECTION),
    vk("V", "V", "Select (V)", VIEW_SECTION),
    vk("H / Space (hold)", "H / Space (hold)", "Hand (H)", VIEW_SECTION),
    vk("Delete", "Delete", "Delete selected pages (Organize)", ORGANIZE_SECTION),
    vk("⌘A", "Ctrl+A", "Select all pages (Organize)", ORGANIZE_SECTION),
];

/// One line of the keyboard reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyRow {
    pub section: &'static str,
    /// What it does, in English (the UI translates it); menu labels lose their trailing "…".
    pub action: &'static str,
    pub mac: String,
    pub other: String,
    /// The registry command it runs, when it is one.
    pub command: Option<&'static str>,
}

impl KeyRow {
    /// The keys on one platform.
    pub fn keys(&self, mac: bool) -> &str {
        if mac { &self.mac } else { &self.other }
    }
}

fn section_of(c: &CommandSpec) -> &'static str {
    c.menu.unwrap_or(TOOLS_SECTION)
}

fn section_rank(section: &str) -> usize {
    SECTIONS.iter().position(|s| *s == section).unwrap_or(SECTIONS.len())
}

/// Every shortcut: registry commands (by section, in registry order), then the view's own keys.
pub fn key_reference() -> Vec<KeyRow> {
    let mut rows: Vec<KeyRow> = COMMANDS
        .iter()
        .filter_map(|c| {
            let k = c.shortcut?;
            Some(KeyRow {
                section: section_of(c),
                action: c.label.trim_end_matches('…'),
                mac: k.label(true),
                other: k.label(false),
                command: Some(c.id),
            })
        })
        .collect();
    rows.extend(VIEW_KEYS.iter().map(|v| KeyRow {
        section: v.section,
        action: v.action,
        mac: v.mac.to_string(),
        other: v.other.to_string(),
        command: None,
    }));
    // Stable: registry order is kept within a section.
    rows.sort_by_key(|r| section_rank(r.section));
    rows
}

/// The longest search text that is looked at (in characters); the rest is ignored.
pub const MAX_QUERY_CHARS: usize = 200;
/// At most this many search terms are used.
const MAX_TERMS: usize = 16;

/// Search text as terms: lower case, control characters dropped, capped in length and count.
pub fn query_terms(query: &str) -> Vec<String> {
    let clean: String = query.chars().take(MAX_QUERY_CHARS).filter(|c| !c.is_control()).collect::<String>().to_lowercase();
    clean.split_whitespace().take(MAX_TERMS).map(str::to_string).collect()
}

/// Whether `row` matches every term. `extra` is more text to match (the UI passes the action
/// and section in the interface language, so a translated name finds its row too).
pub fn row_matches(row: &KeyRow, terms: &[String], extra: &[&str]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let mut hay = format!("{} {} {} {} {}", row.action, row.section, row.mac, row.other, row.command.unwrap_or(""));
    for e in extra {
        hay.push(' ');
        hay.push_str(e);
    }
    let hay = hay.to_lowercase();
    terms.iter().all(|t| hay.contains(t.as_str()))
}

/// The rows matching `query` (all of them for an empty one).
pub fn search_keys(query: &str) -> Vec<KeyRow> {
    let terms = query_terms(query);
    key_reference().into_iter().filter(|r| row_matches(r, &terms, &[])).collect()
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|")
}

/// `docs/user/keyboard.md`, generated from [`key_reference`].
pub fn keyboard_markdown() -> String {
    let mut out = String::from(
        "# Keyboard shortcuts\n\n\
         <!-- Generated from the command registry (crates/engine/src/help.rs); do not edit by hand.\n     \
         Rewrite it with: UPDATE_USER_DOCS=1 cargo test -p pdfcraft-engine --lib help:: -->\n\n\
         On macOS PdfCraft uses ⌘ (Command); on Windows, Linux and FreeBSD the same shortcuts use Ctrl. \
         ⇧ is Shift and ⌃ is Control. The web app shows the macOS keys; press Ctrl instead of ⌘ on \
         Windows and Linux.\n\n\
         The same list is in the app: **Menu → Help → Keyboard shortcuts** (⌘/), where you can search it. \
         Press ⌘K to find any command by name, with or without a shortcut.\n",
    );
    let rows = key_reference();
    for section in SECTIONS {
        let in_section: Vec<&KeyRow> = rows.iter().filter(|r| r.section == section).collect();
        if in_section.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {section}\n\n| Action | macOS | Windows, Linux and FreeBSD |\n|---|---|---|\n"));
        for r in in_section {
            out.push_str(&format!("| {} | {} | {} |\n", cell(r.action), cell(&r.mac), cell(&r.other)));
        }
    }
    out.push_str(
        "\n## Editing objects\n\n\
         With **Edit objects**, a selected object takes these keys:\n\n\
         | Action | macOS | Windows, Linux and FreeBSD |\n|---|---|---|\n\
         | Move by 1 point (10 with Shift) | ← → ↑ ↓ | ← → ↑ ↓ |\n\
         | Bring forward / to front | ⌘] / ⇧⌘] | Ctrl+] / Ctrl+Shift+] |\n\
         | Send backward / to back | ⌘[ / ⇧⌘[ | Ctrl+[ / Ctrl+Shift+[ |\n\
         | Delete | Delete | Delete |\n\
         | Deselect | Esc | Esc |\n\
         \n## Forms\n\n\
         | Action | macOS | Windows, Linux and FreeBSD |\n|---|---|---|\n\
         | Next / previous field | Tab / ⇧Tab | Tab / Shift+Tab |\n\
         | Finish typing in a field | Return | Enter |\n\
         | Cancel typing in a field | Esc | Esc |\n",
    );
    out
}

/// One page of the user guide.
#[derive(Clone, Copy, Debug)]
pub struct GuidePage {
    /// File name without `.md` (`viewing`), also the page's id in tools and links.
    pub slug: &'static str,
    pub markdown: &'static str,
}

impl GuidePage {
    /// The first `# ` heading, else the slug.
    pub fn title(&self) -> &'static str {
        self.markdown.lines().find_map(|l| l.strip_prefix("# ")).map(str::trim).filter(|t| !t.is_empty()).unwrap_or(self.slug)
    }
}

macro_rules! page {
    ($slug:literal, $file:literal) => {
        GuidePage { slug: $slug, markdown: include_str!(concat!("../../../docs/user/", $file)) }
    };
}

/// The user guide, in reading order (`docs/user/README.md` is the contents page).
pub const GUIDE: &[GuidePage] = &[
    page!("index", "README.md"),
    page!("getting-started", "getting-started.md"),
    page!("viewing", "viewing.md"),
    page!("editing", "editing.md"),
    page!("comments", "comments.md"),
    page!("forms", "forms.md"),
    page!("signing", "signing.md"),
    page!("organizing", "organizing.md"),
    page!("create-and-export", "create-and-export.md"),
    page!("security", "security.md"),
    page!("ocr", "ocr.md"),
    page!("accessibility", "accessibility.md"),
    page!("preferences", "preferences.md"),
    page!("web-app", "web-app.md"),
    page!("automation", "automation.md"),
    page!("keyboard", "keyboard.md"),
];

/// A page by slug, also accepting its file name (`viewing.md`, `README.md`) as links use.
pub fn guide_page(slug: &str) -> Option<&'static GuidePage> {
    let s = slug.trim();
    let s = s.split('#').next().unwrap_or(s);
    let s = s.strip_prefix("./").unwrap_or(s);
    let s = s.strip_suffix(".md").unwrap_or(s);
    let s = if s.eq_ignore_ascii_case("readme") { "index" } else { s };
    GUIDE.iter().find(|p| p.slug == s)
}

/// A line of the guide that matches a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuideHit {
    pub slug: &'static str,
    /// 1-based line number in the page.
    pub line: usize,
    pub text: &'static str,
}

/// Lines of the guide that contain every term of `query`, at most `limit` (capped at 500).
pub fn search_guide(query: &str, limit: usize) -> Vec<GuideHit> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let limit = limit.min(500);
    let mut hits = Vec::new();
    for page in GUIDE {
        for (i, line) in page.markdown.lines().enumerate() {
            if hits.len() >= limit {
                return hits;
            }
            let lower = line.to_lowercase();
            if !line.trim().is_empty() && terms.iter().all(|t| lower.contains(t.as_str())) {
                hits.push(GuideHit { slug: page.slug, line: i.saturating_add(1), text: line.trim() });
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/user")
    }

    /// `docs/user/keyboard.md` is generated: it fails here when a shortcut changed and the file
    /// wasn't regenerated (`UPDATE_USER_DOCS=1 cargo test -p pdfcraft-engine --lib help::`).
    #[test]
    fn keyboard_reference_doc_is_up_to_date() {
        let path = docs_dir().join("keyboard.md");
        let want = keyboard_markdown();
        if std::env::var_os("UPDATE_USER_DOCS").is_some() {
            std::fs::write(&path, &want).unwrap();
            return;
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            have == want,
            "docs/user/keyboard.md is out of date with the command registry; run UPDATE_USER_DOCS=1 cargo test -p pdfcraft-engine --lib help::"
        );
        // The bundled copy is the same file.
        assert_eq!(guide_page("keyboard").unwrap().markdown.replace("\r\n", "\n"), have.replace("\r\n", "\n"));
    }

    #[test]
    fn every_registry_shortcut_is_in_the_reference() {
        let rows = key_reference();
        let doc = keyboard_markdown();
        for c in COMMANDS.iter().filter(|c| c.shortcut.is_some()) {
            let k = c.shortcut.unwrap();
            let row = rows.iter().find(|r| r.command == Some(c.id)).unwrap_or_else(|| panic!("{} missing", c.id));
            assert_eq!(row.mac, k.label(true));
            assert_eq!(row.other, k.label(false));
            assert!(doc.contains(&format!("| {} | {} |", k.label(true), k.label(false))), "{} not in keyboard.md", c.id);
        }
        assert!(rows.iter().any(|r| r.command == Some("help.shortcuts")), "⌘/ opens the reference");
        // Sections come in the documented order.
        let ranks: Vec<usize> = rows.iter().map(|r| section_rank(r.section)).collect();
        assert!(ranks.windows(2).all(|w| w[0] <= w[1]));
        assert!(rows.iter().all(|r| SECTIONS.contains(&r.section)));
    }

    #[test]
    fn search_finds_by_action_key_and_id() {
        assert_eq!(search_keys("").len(), key_reference().len());
        assert!(search_keys("save as").iter().any(|r| r.command == Some("file.save_as")));
        assert!(search_keys("CTRL+SHIFT+S").iter().any(|r| r.command == Some("file.save_as")));
        assert!(search_keys("⇧⌘S").iter().any(|r| r.command == Some("file.save_as")));
        assert!(search_keys("file.open").iter().any(|r| r.command == Some("file.open")));
        assert!(search_keys("zoom page").iter().any(|r| r.action == "Zoom to page level"));
        assert!(search_keys("zzzz-nothing").is_empty());
        let row = &key_reference()[0];
        assert!(row_matches(row, &query_terms("ファイル"), &["ファイル"]), "translated text matches too");
    }

    #[test]
    fn hostile_search_text_is_bounded_and_never_panics() {
        let huge = "a".repeat(1_000_000);
        assert!(query_terms(&huge)[0].chars().count() <= MAX_QUERY_CHARS);
        let many = "x ".repeat(10_000);
        assert!(query_terms(&many).len() <= MAX_TERMS);
        for q in [
            "\0\u{1}\u{7f}\n\t",
            "(.*)[]{}\\^$|?+",
            "\u{202e}evas\u{202c}",
            "e\u{301}\u{301}\u{301}",
            "🙂👍🏽🇯🇵",
            "İSTANBUL ß ﬁ",
            "\u{feff}\u{200b}",
            "%s%n{}{0}",
            &huge,
            &many,
        ] {
            let _ = search_keys(q);
            let _ = search_guide(q, usize::MAX);
        }
        assert_eq!(search_keys("\0\u{1}\n").len(), key_reference().len(), "control characters alone are no search");
        assert!(search_guide("", 10).is_empty());
        assert!(search_guide("the", usize::MAX).len() <= 500);
        assert!(guide_page("../../etc/passwd").is_none());
        assert!(guide_page("").is_none());
        assert!(guide_page(&huge).is_none());
    }

    #[test]
    fn the_guide_bundles_every_page_and_its_links_resolve() {
        let mut files: Vec<String> =
            std::fs::read_dir(docs_dir()).unwrap().filter_map(|e| e.ok()?.file_name().into_string().ok()).filter(|n| n.ends_with(".md")).collect();
        files.sort();
        for f in &files {
            assert!(guide_page(f).is_some(), "docs/user/{f} is not bundled in help::GUIDE");
        }
        assert_eq!(files.len(), GUIDE.len(), "every bundled page is a file");
        for p in GUIDE {
            assert!(p.title() != p.slug, "{} has a # title", p.slug);
            // Relative links point at bundled pages.
            for (i, _) in p.markdown.match_indices("](") {
                let rest = &p.markdown[i + 2..];
                let target = &rest[..rest.find(')').unwrap()];
                if target.starts_with("http") || target.starts_with('#') {
                    continue;
                }
                assert!(guide_page(target).is_some(), "{}: broken link {target}", p.slug);
            }
        }
        assert_eq!(guide_page("README.md").unwrap().slug, "index");
        assert_eq!(guide_page("signing.md#checking").unwrap().slug, "signing");
        assert!(search_guide("redaction", 50).iter().any(|h| h.slug == "security"));
    }
}
