//! Help → Keyboard shortcuts and Help → User guide.
//!
//! The shortcuts window lists [`pdfcraft_engine::help::key_reference`], the registry's bindings
//! and the document view's own keys, grouped by section, with a search box that matches the
//! action (in English and the interface language), the section, the keys and the command id.
//!
//! The user guide shows the `docs/user` pages bundled in [`pdfcraft_engine::help::GUIDE`]: a page
//! list on the left and the page on the right, rendered from its Markdown by a small renderer
//! (headings, paragraphs, lists, tables, code, bold, italic, code spans and links). Pages are
//! parsed once, on first open; nothing is fetched.

use std::sync::OnceLock;

use egui::{Align, Color32, CornerRadius, Layout, Sense, vec2};
use pdfcraft_engine::help::{self, GUIDE, KeyRow};

use crate::theme::{self, Tokens};
use crate::{Dialog, PdfCraftApp, widgets};

/// What the two help windows remember while the app runs.
#[derive(Clone, Debug, Default)]
pub struct HelpUi {
    /// The shortcuts search text.
    pub query: String,
    /// The user guide page shown (an index into [`GUIDE`]).
    pub page: usize,
    /// Put the keyboard focus in the search box the next time the shortcuts window draws.
    focus_search: bool,
}

impl PdfCraftApp {
    pub(crate) fn open_shortcuts(&mut self) {
        self.help_ui.focus_search = true;
        self.dialog = Some(Dialog::Shortcuts);
    }

    pub(crate) fn open_user_guide(&mut self) {
        self.dialog = Some(Dialog::UserGuide);
    }

    /// The guide page shown, by slug (`ui.set user-guide-page`).
    pub(crate) fn set_guide_page(&mut self, slug: &str) -> Result<(), String> {
        let page = help::guide_page(slug).ok_or_else(|| {
            let all: Vec<&str> = GUIDE.iter().map(|p| p.slug).collect();
            format!("user-guide-page must be one of {}", all.join(", "))
        })?;
        self.help_ui.page = GUIDE.iter().position(|p| p.slug == page.slug).unwrap_or(0);
        Ok(())
    }

    /// The guide page shown (its slug, as `ui.state` reports it).
    pub fn guide_page_slug(&self) -> &'static str {
        GUIDE.get(self.help_ui.page).map_or("index", |p| p.slug)
    }
}

/// A section's name in the interface language.
fn section_label(section: &str) -> &str {
    match section {
        help::TOOLS_SECTION => tl!("Tools"),
        help::VIEW_SECTION => tl!("Document view"),
        other => tl!(other),
    }
}

/// The rows the search finds, by section in reference order.
pub fn search(query: &str) -> Vec<(&'static str, Vec<KeyRow>)> {
    let terms = help::query_terms(query);
    let mut out: Vec<(&'static str, Vec<KeyRow>)> = Vec::new();
    for row in help::key_reference() {
        let action = tl!(row.action);
        let section = section_label(row.section);
        if !help::row_matches(&row, &terms, &[action, section]) {
            continue;
        }
        match out.last_mut() {
            Some((s, rows)) if *s == row.section => rows.push(row),
            _ => out.push((row.section, vec![row])),
        }
    }
    out
}

/// One key combination as a chip.
fn key_chip(ui: &mut egui::Ui, t: &Tokens, keys: &str) {
    egui::Frame::NONE
        .fill(t.hover)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(keys).font(egui::FontId::monospace(12.5)).color(t.text));
        });
}

/// The Keyboard shortcuts window's body (the dialog adds the Close button).
pub(crate) fn shortcuts_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) {
    ui.set_width(620.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Keyboard shortcuts")).font(theme::semibold(18.0)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut app.help_ui.query)
                    .id(egui::Id::new("shortcuts-search"))
                    .hint_text(tl!("Search shortcuts"))
                    .desired_width(240.0)
                    .char_limit(help::MAX_QUERY_CHARS),
            );
            if std::mem::take(&mut app.help_ui.focus_search) {
                resp.request_focus();
            }
        });
    });
    ui.add_space(4.0);
    let mac = cfg!(target_os = "macos") || cfg!(target_arch = "wasm32");
    let found = search(&app.help_ui.query);
    egui::ScrollArea::vertical().id_salt("shortcuts").max_height(460.0).auto_shrink([false, true]).show(ui, |ui| {
        ui.set_width(ui.available_width() - 8.0);
        if found.is_empty() {
            ui.add_space(12.0);
            let q: String = app.help_ui.query.trim().chars().take(80).collect();
            ui.label(egui::RichText::new(crate::i18n::fmt(tl!("No shortcuts match “{query}”."), &[("query", &q)])).color(t.text_muted));
        }
        for (section, rows) in &found {
            widgets::section_title(ui, section_label(section));
            for row in rows {
                ui.horizontal(|ui| {
                    ui.set_min_height(28.0);
                    ui.label(tl!(row.action));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // "⌘G / ⇧⌘G" is two combinations: one chip each, the last on the right.
                        let combos: Vec<&str> = tl!(row.keys(mac)).split(" / ").collect();
                        for (i, k) in combos.iter().enumerate().rev() {
                            key_chip(ui, t, k);
                            if i > 0 {
                                ui.label(egui::RichText::new("/").color(t.text_faint));
                            }
                        }
                    });
                });
            }
        }
    });
}

/// A run of text inside a paragraph, list item or table cell.
#[derive(Clone, Debug, PartialEq)]
enum Span {
    Text { text: String, bold: bool, italic: bool },
    Code(String),
    Link { text: String, target: String },
}

#[derive(Clone, Debug, PartialEq)]
enum Block {
    Heading(u8, Vec<Span>),
    Para(Vec<Span>),
    /// A list item: indent level, marker ("•" or "1."), text.
    Item(usize, String, Vec<Span>),
    Code(String),
    Table(Vec<Vec<Vec<Span>>>),
}

/// The longest page that is rendered (the bundled pages are far smaller).
const MAX_PAGE_BYTES: usize = 512 * 1024;

/// Inline Markdown: `**bold**`, `*italic*`, `` `code` `` and `[text](target)`.
fn spans(s: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut text = String::new();
    let (mut bold, mut italic) = (false, false);
    let flush = |text: &mut String, out: &mut Vec<Span>, bold: bool, italic: bool| {
        if !text.is_empty() {
            out.push(Span::Text { text: std::mem::take(text), bold, italic });
        }
    };
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        // `after` is everything past `c`; the markers are ASCII, so their slices are on char
        // boundaries, and `get` keeps that from ever panicking anyway.
        let after = rest.get(c.len_utf8()..).unwrap_or("");
        if let Some(r) = rest.strip_prefix("**") {
            flush(&mut text, &mut out, bold, italic);
            bold = !bold;
            rest = r;
        } else if c == '`'
            && let Some((code, r)) = after.split_once('`')
        {
            flush(&mut text, &mut out, bold, italic);
            out.push(Span::Code(code.to_string()));
            rest = r;
        } else if c == '['
            && let Some((label, r)) = after.split_once("](")
            && let Some((target, r)) = r.split_once(')')
        {
            flush(&mut text, &mut out, bold, italic);
            out.push(Span::Link { text: label.to_string(), target: target.to_string() });
            rest = r;
        } else if c == '*' && !rest.starts_with("* ") {
            flush(&mut text, &mut out, bold, italic);
            italic = !italic;
            rest = after;
        } else {
            text.push(c);
            rest = after;
        }
    }
    flush(&mut text, &mut out, bold, italic);
    out
}

fn table_row(line: &str) -> Vec<Vec<Span>> {
    let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
    // `\|` is a literal bar inside a cell.
    inner.replace("\\|", "\u{0}").split('|').map(|c| spans(c.replace('\u{0}', "|").trim())).collect()
}

/// A page's Markdown as blocks. HTML comments are left out.
fn parse(md: &str) -> Vec<Block> {
    let mut cut = md.len().min(MAX_PAGE_BYTES);
    while !md.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    let md = md.get(..cut).unwrap_or("");
    let mut blocks = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut table: Vec<Vec<Vec<Span>>> = Vec::new();
    let mut code: Option<String> = None;
    let mut comment = false;
    let end_para = |para: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            blocks.push(Block::Para(spans(&para.join(" "))));
            para.clear();
        }
    };
    for line in md.lines() {
        if let Some(c) = code.as_mut() {
            if line.trim_start().starts_with("```") {
                blocks.push(Block::Code(std::mem::take(c)));
                code = None;
            } else {
                if !c.is_empty() {
                    c.push('\n');
                }
                c.push_str(line);
            }
            continue;
        }
        if comment || line.trim_start().starts_with("<!--") {
            comment = !line.contains("-->");
            continue;
        }
        let trimmed = line.trim();
        if !trimmed.starts_with('|') && !table.is_empty() {
            blocks.push(Block::Table(std::mem::take(&mut table)));
        }
        if trimmed.is_empty() {
            end_para(&mut para, &mut blocks);
        } else if trimmed.starts_with("```") {
            end_para(&mut para, &mut blocks);
            code = Some(String::new());
        } else if let Some(level) = (1..=4).find(|n| trimmed.strip_prefix(&"#".repeat(*n)).is_some_and(|r| r.starts_with(' '))) {
            end_para(&mut para, &mut blocks);
            blocks.push(Block::Heading(level as u8, spans(trimmed.get(level..).unwrap_or("").trim())));
        } else if trimmed.starts_with('|') {
            end_para(&mut para, &mut blocks);
            if !trimmed.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                table.push(table_row(trimmed));
            }
        } else if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            end_para(&mut para, &mut blocks);
            let indent = (line.len() - line.trim_start().len()) / 2;
            blocks.push(Block::Item(indent.min(6), "•".to_string(), spans(item)));
        } else if let Some((num, item)) =
            trimmed.split_once(". ").filter(|(n, _)| !n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit()))
        {
            end_para(&mut para, &mut blocks);
            let indent = (line.len() - line.trim_start().len()) / 3;
            blocks.push(Block::Item(indent.min(6), format!("{num}."), spans(item)));
        } else if let Some(Block::Item(..)) = blocks.last().filter(|_| para.is_empty() && line.starts_with("  ")) {
            // A continuation line of a list item.
            if let Some(Block::Item(_, _, s)) = blocks.last_mut() {
                s.push(Span::Text { text: " ".into(), bold: false, italic: false });
                s.extend(spans(trimmed));
            }
        } else {
            para.push(trimmed);
        }
    }
    end_para(&mut para, &mut blocks);
    if !table.is_empty() {
        blocks.push(Block::Table(table));
    }
    if let Some(c) = code {
        blocks.push(Block::Code(c));
    }
    blocks
}

/// Every guide page, parsed once.
fn pages() -> &'static [Vec<Block>] {
    static PAGES: OnceLock<Vec<Vec<Block>>> = OnceLock::new();
    PAGES.get_or_init(|| GUIDE.iter().map(|p| parse(p.markdown)).collect())
}

/// Spans laid out in a wrapping row; returns the target of a clicked link.
fn show_spans(ui: &mut egui::Ui, t: &Tokens, spans: &[Span], font: egui::FontId, color: Color32) -> Option<String> {
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for s in spans {
            match s {
                Span::Text { text, bold, italic } => {
                    let mut r = egui::RichText::new(text).font(font.clone()).color(color);
                    if *bold {
                        r = r.strong();
                    }
                    if *italic {
                        r = r.italics();
                    }
                    ui.label(r);
                }
                Span::Code(c) => {
                    ui.label(egui::RichText::new(c).font(egui::FontId::monospace(font.size - 1.0)).background_color(t.hover).color(t.text));
                }
                Span::Link { text, target } => {
                    if ui.link(egui::RichText::new(text).font(font.clone())).on_hover_text(target).clicked() {
                        clicked = Some(target.clone());
                    }
                }
            }
        }
    });
    clicked
}

/// Draw a page; returns the target of a clicked link.
fn show_page(ui: &mut egui::Ui, t: &Tokens, blocks: &[Block]) -> Option<String> {
    let mut clicked = None;
    let body = theme::regular(13.5);
    for (i, b) in blocks.iter().enumerate() {
        let c = match b {
            Block::Heading(level, s) => {
                let (size, space) = match level {
                    1 => (21.0, 2.0),
                    2 => (16.5, 12.0),
                    _ => (14.5, 8.0),
                };
                ui.add_space(space);
                let c = show_spans(ui, t, s, theme::semibold(size), t.text);
                ui.add_space(4.0);
                c
            }
            Block::Para(s) => {
                let c = show_spans(ui, t, s, body.clone(), t.text);
                ui.add_space(6.0);
                c
            }
            Block::Item(indent, marker, s) => {
                ui.horizontal_top(|ui| {
                    ui.add_space(8.0 + 18.0 * *indent as f32);
                    let (rect, _) = ui.allocate_exact_size(vec2(20.0, 18.0), Sense::hover());
                    ui.painter().text(rect.left_top() + vec2(0.0, 1.0), egui::Align2::LEFT_TOP, marker, body.clone(), t.text_muted);
                    let c = show_spans(ui, t, s, body.clone(), t.text);
                    ui.add_space(2.0);
                    c
                })
                .inner
            }
            Block::Code(code) => {
                egui::Frame::NONE.fill(t.hover).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new(code).font(egui::FontId::monospace(12.0)).color(t.text));
                });
                ui.add_space(6.0);
                None
            }
            Block::Table(rows) => {
                let mut c = None;
                egui::Grid::new(("guide-table", i)).striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
                    for (r, row) in rows.iter().enumerate() {
                        for cell in row {
                            let font = if r == 0 { theme::semibold(13.0) } else { theme::regular(13.0) };
                            if let Some(x) = show_spans(ui, t, cell, font, t.text) {
                                c = Some(x);
                            }
                        }
                        ui.end_row();
                    }
                });
                ui.add_space(8.0);
                c
            }
        };
        if c.is_some() {
            clicked = c;
        }
    }
    clicked
}

/// The User guide window. Returns `true` to close.
pub(crate) fn guide_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    ui.set_width(880.0);
    ui.label(egui::RichText::new(tl!("User guide")).font(theme::semibold(18.0)));
    ui.add_space(6.0);
    let page = app.help_ui.page.min(GUIDE.len().saturating_sub(1));
    let mut link = None;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(210.0);
            egui::ScrollArea::vertical().id_salt("guide-pages").max_height(520.0).show(ui, |ui| {
                for (i, p) in GUIDE.iter().enumerate() {
                    let title = p.title();
                    let selected = i == page;
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, title));
                    let fill = if selected {
                        t.selected
                    } else if resp.hovered() {
                        t.hover
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
                    let font = if selected { theme::medium(13.0) } else { theme::regular(13.0) };
                    widgets::row_text(ui, rect.left_center() + vec2(10.0, 0.0), title, font, t.text, rect.width() - 16.0);
                    if resp.clicked() {
                        app.help_ui.page = i;
                    }
                }
            });
        });
        ui.add_space(4.0);
        let (rect, _) = ui.allocate_exact_size(vec2(1.0, 520.0), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::ZERO, t.divider);
        ui.add_space(12.0);
        ui.vertical(|ui| {
            egui::ScrollArea::vertical().id_salt(("guide-page", page)).max_height(520.0).auto_shrink([false, false]).show(ui, |ui| {
                ui.set_width(ui.available_width() - 12.0);
                if let Some(blocks) = pages().get(page) {
                    link = show_page(ui, t, blocks);
                }
            });
        });
    });
    if let Some(target) = link {
        if target.starts_with("https://") || target.starts_with("http://") {
            app.open_url(&target);
        } else {
            // A link to something that isn't a page (a repository file) has nothing to open offline.
            let _ = app.set_guide_page(&target);
        }
    }
    ui.add_space(8.0);
    let mut close = false;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if widgets::pill_button(ui, tl!("Close"), true).clicked() {
            close = true;
        }
        if widgets::pill_button(ui, tl!("Keyboard shortcuts"), false).clicked() {
            app.open_shortcuts();
        }
    });
    close
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_markdown_is_split_into_spans() {
        let s = spans("Press **⌘S** to `save`, see [Forms](forms.md) or *this*.");
        assert!(s.contains(&Span::Text { text: "⌘S".into(), bold: true, italic: false }));
        assert!(s.contains(&Span::Code("save".into())));
        assert!(s.contains(&Span::Link { text: "Forms".into(), target: "forms.md".into() }));
        assert!(s.contains(&Span::Text { text: "this".into(), bold: false, italic: true }));
    }

    #[test]
    fn every_bundled_page_parses_into_blocks() {
        for (p, blocks) in GUIDE.iter().zip(pages()) {
            assert!(matches!(blocks.first(), Some(Block::Heading(1, _))), "{} starts with its title", p.slug);
        }
        let keyboard = &pages()[GUIDE.iter().position(|p| p.slug == "keyboard").unwrap()];
        assert!(keyboard.iter().any(|b| matches!(b, Block::Table(rows) if rows.len() > 3)), "tables render as tables");
        assert!(!keyboard.iter().any(|b| matches!(b, Block::Para(s) if format!("{s:?}").contains("Generated"))), "comments are hidden");
    }

    #[test]
    fn hostile_markdown_never_panics() {
        let huge = "|".repeat(MAX_PAGE_BYTES * 2);
        for md in [
            "",
            "**",
            "`",
            "[",
            "[x](",
            "](",
            "*",
            "```",
            "<!--",
            "| a | b \\|",
            "#",
            "####### seven",
            "1. ",
            "999999. x",
            "- \u{202e}x\u{0}",
            "é\u{301}**\u{1F600}`日本`[語](x)",
            &huge,
        ] {
            let _ = parse(md);
            let _ = spans(md);
        }
    }

    #[test]
    fn search_matches_translations_and_hostile_text() {
        assert!(search("").iter().map(|(_, r)| r.len()).sum::<usize>() == help::key_reference().len());
        assert!(search("save").iter().any(|(_, rows)| rows.iter().any(|r| r.command == Some("file.save_as"))));
        assert!(search("zzzz-nothing").is_empty());
        for q in ["\0\u{1}", &"🙂".repeat(100_000), "(.*)[", "\u{202e}"] {
            let _ = search(q);
        }
    }
}
