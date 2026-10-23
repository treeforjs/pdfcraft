//! Bookmark Properties (a bookmark's title and its action: go to a page view, a named
//! destination, a web address or a JavaScript) and New Bookmarks from Structure (one bookmark
//! per heading of a tagged PDF). Both apply one undoable edit through the engine.

use egui::{Align, Layout};
use pdfcraft_engine::{BookmarkAction, DestFit, DestTarget, DestView, Edit};

use crate::theme::{self, Tokens};
use crate::{Dialog, PdfCraftApp, widgets};

/// What a bookmark does when clicked (the Actions part of Bookmark Properties).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActionKind {
    #[default]
    Page,
    Named,
    Web,
    Script,
    /// Another action (Launch, Named, …) read from the file: kept unless changed.
    Other,
}

/// How a "go to a page" action shows the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Zoom {
    /// Keep the reader's zoom (`/XYZ` with no zoom).
    #[default]
    Inherit,
    /// A fixed zoom (`/XYZ` with a zoom).
    Fixed,
    Fit,
    FitWidth,
    FitHeight,
    FitVisible,
}

/// The Bookmark Properties dialog's state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BookmarkProps {
    pub path: Vec<usize>,
    pub title: String,
    original_title: String,
    pub kind: ActionKind,
    /// 1-based page number as typed.
    pub page: String,
    pub zoom: Zoom,
    /// Percent, as typed.
    pub percent: String,
    pub dest: String,
    pub url: String,
    pub script: String,
    /// The kept action's type, when it is another action.
    pub other: Option<String>,
    /// Named destinations in the document (for the picker).
    pub dests: Vec<String>,
    pub error: Option<String>,
}

/// The New Bookmarks from Structure dialog's state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FromStructure {
    /// Deepest heading level kept (1–6); 0 keeps every level.
    pub max_level: usize,
    pub replace: bool,
    pub error: Option<String>,
}

/// The longest web address accepted (characters).
const MAX_URL: usize = 4096;
/// The longest script accepted (characters).
const MAX_SCRIPT: usize = 256 * 1024;
/// At most this many named destinations are offered in the picker.
const MAX_DESTS: usize = 2000;

impl BookmarkProps {
    /// The action the dialog describes, or why it can't be made. `pages`: the page count.
    pub fn action(&self, pages: usize) -> Result<Option<BookmarkAction>, String> {
        Ok(Some(match self.kind {
            ActionKind::Page => {
                let page = match self.page.trim().parse::<usize>() {
                    Ok(p) if p >= 1 && p <= pages => p - 1,
                    _ => return Err(crate::i18n::fmt(tl!("Enter a page number from 1 to {n}."), &[("n", &pages.to_string())])),
                };
                let fit = match self.zoom {
                    Zoom::Inherit => DestFit::Xyz { left: None, top: None, zoom: None },
                    Zoom::Fixed => {
                        let pct = self.percent.trim().trim_end_matches('%').trim().parse::<f64>().ok().filter(|z| z.is_finite());
                        match pct {
                            Some(z) if (1.0..=6400.0).contains(&z) => DestFit::Xyz { left: None, top: None, zoom: Some(z / 100.0) },
                            _ => return Err(tl!("Enter a zoom from 1% to 6400%.").to_string()),
                        }
                    }
                    Zoom::Fit => DestFit::Fit,
                    Zoom::FitWidth => DestFit::FitH { top: None },
                    Zoom::FitHeight => DestFit::FitV { left: None },
                    Zoom::FitVisible => DestFit::FitB,
                };
                BookmarkAction::GoTo(DestTarget::View(DestView { page, fit }))
            }
            ActionKind::Named => {
                let name = self.dest.trim();
                if !self.dests.iter().any(|d| d == name) {
                    return Err(tl!("Choose one of the document's named destinations.").to_string());
                }
                BookmarkAction::GoTo(DestTarget::Named(name.to_string()))
            }
            ActionKind::Web => {
                let url = self.url.trim();
                let lower = url.to_ascii_lowercase();
                if url.chars().count() > MAX_URL
                    || url.chars().any(char::is_control)
                    || !(lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:"))
                {
                    return Err(tl!("Enter a web address starting with https://, http:// or mailto:.").to_string());
                }
                BookmarkAction::Uri(url.to_string())
            }
            ActionKind::Script => {
                if self.script.chars().count() > MAX_SCRIPT {
                    return Err(tl!("The script is too long.").to_string());
                }
                BookmarkAction::JavaScript(self.script.clone())
            }
            ActionKind::Other => return Ok(None),
        }))
    }
}

impl PdfCraftApp {
    /// Bookmark Properties for the bookmark at `path` of the active document.
    pub fn open_bookmark_props(&mut self, path: Vec<usize>) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let Some(item) = bookmark_at(&doc.info.outline, &path) else { return self.notify_tr("That bookmark no longer exists") };
        let mut p = BookmarkProps {
            path: path.clone(),
            title: item.title.clone(),
            original_title: item.title.clone(),
            percent: "100".into(),
            ..Default::default()
        };
        p.page = (item.page.unwrap_or(0) + 1).to_string();
        p.dests = self.session.destinations(id).map(|d| d.into_iter().take(MAX_DESTS).map(|(d, _)| d.name).collect()).unwrap_or_default();
        match self.session.bookmark_action(id, &path) {
            Ok(BookmarkAction::GoTo(DestTarget::View(v))) => {
                p.kind = ActionKind::Page;
                p.page = (v.page + 1).to_string();
                p.zoom = match v.fit {
                    DestFit::Xyz { zoom: Some(z), .. } if z > 0.0 => {
                        p.percent = format!("{:.0}", z * 100.0);
                        Zoom::Fixed
                    }
                    DestFit::Xyz { .. } | DestFit::FitR { .. } => Zoom::Inherit,
                    DestFit::Fit => Zoom::Fit,
                    DestFit::FitH { .. } | DestFit::FitBH { .. } => Zoom::FitWidth,
                    DestFit::FitV { .. } | DestFit::FitBV { .. } => Zoom::FitHeight,
                    DestFit::FitB => Zoom::FitVisible,
                };
            }
            Ok(BookmarkAction::GoTo(DestTarget::Named(n))) => {
                p.kind = ActionKind::Named;
                p.dest = n;
            }
            Ok(BookmarkAction::Uri(u)) => {
                p.kind = ActionKind::Web;
                p.url = u;
            }
            Ok(BookmarkAction::JavaScript(js)) => {
                p.kind = ActionKind::Script;
                p.script = js;
            }
            Ok(BookmarkAction::Other(what)) => {
                p.kind = ActionKind::Other;
                p.other = Some(what);
            }
            Ok(BookmarkAction::None) => p.kind = ActionKind::Page,
            Err(e) => return self.notify_error(e),
        }
        self.bookmark_props = Some(p);
        self.dialog = Some(Dialog::BookmarkProperties);
    }

    /// Bookmark Properties ▸ OK: rename and set the action (each its own undo step).
    pub fn apply_bookmark_props(&mut self) -> bool {
        let Some(p) = self.bookmark_props.clone() else { return true };
        let Some((_, id)) = self.active_ids() else { return true };
        let pages = self.session.get(id).map_or(0, |d| d.info.pages.len());
        let action = match p.action(pages) {
            Ok(a) => a,
            Err(e) => {
                if let Some(s) = self.bookmark_props.as_mut() {
                    s.error = Some(e);
                }
                return false;
            }
        };
        let title = p.title.trim().to_string();
        if title.is_empty() {
            if let Some(s) = self.bookmark_props.as_mut() {
                s.error = Some(tl!("A bookmark needs a title.").to_string());
            }
            return false;
        }
        if title != p.original_title.trim() && !self.apply_edit(Edit::RenameBookmark { path: p.path.clone(), title }) {
            return false;
        }
        if let Some(action) = action
            && self.session.bookmark_action(id, &p.path).ok().as_ref() != Some(&action)
            && !self.apply_edit(Edit::SetBookmarkAction { path: p.path.clone(), action })
        {
            return false;
        }
        self.bookmark_props = None;
        true
    }

    /// The bookmark Bookmark Properties opens from the command: the deepest one for the
    /// current page, else the first.
    pub(crate) fn bookmark_for_current_page(&self) -> Option<Vec<usize>> {
        let (i, id) = self.active_ids()?;
        let doc = self.session.get(id)?;
        let current = self.views.get(i)?.current;
        let mut best: Option<Vec<usize>> = None;
        let mut stack: Vec<(Vec<usize>, &pdfcraft_render::OutlineItem)> = doc.info.outline.iter().enumerate().map(|(k, b)| (vec![k], b)).collect();
        let mut seen = 0usize;
        while let Some((path, b)) = stack.pop() {
            seen += 1;
            if seen > 100_000 {
                break;
            }
            if b.page == Some(current) && best.as_ref().is_none_or(|p| path.len() > p.len()) {
                best = Some(path.clone());
            }
            if path.len() < 64 {
                stack.extend(b.children.iter().enumerate().map(|(k, c)| {
                    let mut p = path.clone();
                    p.push(k);
                    (p, c)
                }));
            }
        }
        best.or_else(|| (!doc.info.outline.is_empty()).then(|| vec![0]))
    }

    /// New Bookmarks from Structure ▸ OK.
    pub fn apply_bookmarks_from_structure(&mut self) -> bool {
        let Some((_, id)) = self.active_ids() else { return true };
        let (max_level, replace) = (self.from_structure.max_level, self.from_structure.replace);
        let found = match self.session.get(id).map(|d| d.heading_bookmarks((max_level > 0).then_some(max_level.min(64)))) {
            Some(Ok(f)) => f,
            Some(Err(e)) => {
                self.from_structure.error = Some(e);
                return false;
            }
            None => return true,
        };
        let added = found.entries.len();
        if !self.apply_edit(Edit::AddBookmarkTree { entries: found.entries, replace }) {
            return false;
        }
        self.from_structure.error = None;
        self.right = Some(crate::RightPanel::Bookmarks);
        let msg = if added == 1 {
            crate::i18n::fmt(tl!("Added 1 bookmark"), &[])
        } else {
            crate::i18n::fmt(tl!("Added {n} bookmarks"), &[("n", &added.to_string())])
        };
        self.notify(msg);
        true
    }
}

fn bookmark_at<'a>(items: &'a [pdfcraft_render::OutlineItem], path: &[usize]) -> Option<&'a pdfcraft_render::OutlineItem> {
    let (first, rest) = path.split_first()?;
    let item = items.get(*first)?;
    if rest.is_empty() { Some(item) } else { bookmark_at(&item.children, rest) }
}

/// Bookmark Properties. Returns `true` to close.
pub(crate) fn props_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let Some(p) = app.bookmark_props.as_mut() else { return true };
    ui.label(egui::RichText::new(tl!("Bookmark properties")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    egui::Grid::new("bookmark-props").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        let l = ui.label(tl!("Title:"));
        ui.add(egui::TextEdit::singleline(&mut p.title).desired_width(320.0).id_salt("bookmark-title")).labelled_by(l.id);
        ui.end_row();
        ui.label(tl!("When clicked:"));
        let kinds = [
            (ActionKind::Page, tl!("Go to a page view")),
            (ActionKind::Named, tl!("Go to a named destination")),
            (ActionKind::Web, tl!("Open a web link")),
            (ActionKind::Script, tl!("Run a JavaScript")),
        ];
        let current =
            kinds.iter().find(|(k, _)| *k == p.kind).map_or_else(|| tl!("Another action (kept as it is)").to_string(), |(_, l)| l.to_string());
        egui::ComboBox::from_id_salt("bookmark-action").selected_text(current).width(320.0).show_ui(ui, |ui| {
            for (k, label) in kinds {
                ui.selectable_value(&mut p.kind, k, label);
            }
        });
        ui.end_row();
        match p.kind {
            ActionKind::Page => {
                let l = ui.label(tl!("Page:"));
                ui.add(egui::TextEdit::singleline(&mut p.page).desired_width(80.0).id_salt("bookmark-page")).labelled_by(l.id);
                ui.end_row();
                ui.label(tl!("Zoom:"));
                let zooms = [
                    (Zoom::Inherit, tl!("Inherit zoom")),
                    (Zoom::Fixed, tl!("Fixed zoom")),
                    (Zoom::Fit, tl!("Fit page")),
                    (Zoom::FitWidth, tl!("Fit width")),
                    (Zoom::FitHeight, tl!("Fit height")),
                    (Zoom::FitVisible, tl!("Fit visible")),
                ];
                ui.horizontal(|ui| {
                    let shown = zooms.iter().find(|(z, _)| *z == p.zoom).map(|(_, l)| *l).unwrap_or_default();
                    egui::ComboBox::from_id_salt("bookmark-zoom").selected_text(shown).width(180.0).show_ui(ui, |ui| {
                        for (z, label) in zooms {
                            ui.selectable_value(&mut p.zoom, z, label);
                        }
                    });
                    if p.zoom == Zoom::Fixed {
                        ui.add(egui::TextEdit::singleline(&mut p.percent).desired_width(60.0).id_salt("bookmark-percent"));
                        ui.label("%");
                    }
                });
                ui.end_row();
            }
            ActionKind::Named => {
                ui.label(tl!("Destination:"));
                if p.dests.is_empty() {
                    ui.label(egui::RichText::new(tl!("This document has no named destinations.")).color(t.text_muted));
                } else {
                    let shown = if p.dest.is_empty() { tl!("Choose…").to_string() } else { p.dest.clone() };
                    egui::ComboBox::from_id_salt("bookmark-dest").selected_text(shown).width(320.0).show_ui(ui, |ui| {
                        for d in &p.dests {
                            ui.selectable_value(&mut p.dest, d.clone(), d);
                        }
                    });
                }
                ui.end_row();
            }
            ActionKind::Web => {
                let l = ui.label(tl!("Web address:"));
                ui.add(egui::TextEdit::singleline(&mut p.url).desired_width(320.0).hint_text("https://").id_salt("bookmark-url")).labelled_by(l.id);
                ui.end_row();
            }
            ActionKind::Script => {
                let l = ui.label(tl!("Script:"));
                ui.add(egui::TextEdit::multiline(&mut p.script).code_editor().desired_rows(6).desired_width(320.0).id_salt("bookmark-script"))
                    .labelled_by(l.id);
                ui.end_row();
            }
            ActionKind::Other => {
                ui.label("");
                let what = p.other.clone().unwrap_or_default();
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        tl!("This bookmark runs a {kind} action, kept as it is unless you choose another."),
                        &[("kind", &what)],
                    ))
                    .color(t.text_muted),
                );
                ui.end_row();
            }
        }
    });
    if let Some(e) = &p.error {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(e).color(theme::DANGER));
    }
    ui.add_space(10.0);
    let (mut ok, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ok = widgets::pill_button(ui, tl!("OK"), true).clicked();
        cancel = widgets::pill_button(ui, tl!("Cancel"), false).clicked();
    });
    if cancel {
        app.bookmark_props = None;
        return true;
    }
    ok && app.apply_bookmark_props()
}

/// New Bookmarks from Structure. Returns `true` to close.
pub(crate) fn from_structure_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    ui.label(egui::RichText::new(tl!("New bookmarks from structure")).font(theme::semibold(18.0)));
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!("Make one bookmark for each heading of the document's tags, nested by heading level.")).color(t.text_muted));
    ui.add_space(8.0);
    let f = &mut app.from_structure;
    egui::Grid::new("bookmarks-from-structure").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(tl!("Headings:"));
        let label =
            |n: usize| if n == 0 { tl!("All levels").to_string() } else { crate::i18n::fmt(tl!("Levels 1 to {n}"), &[("n", &n.to_string())]) };
        egui::ComboBox::from_id_salt("structure-levels").selected_text(label(f.max_level)).width(200.0).show_ui(ui, |ui| {
            for n in [0, 1, 2, 3, 4, 5, 6] {
                ui.selectable_value(&mut f.max_level, n, label(n));
            }
        });
        ui.end_row();
        ui.label(tl!("Existing bookmarks:"));
        ui.vertical(|ui| {
            ui.radio_value(&mut f.replace, false, tl!("Keep them, add the new ones after"));
            ui.radio_value(&mut f.replace, true, tl!("Replace them"));
        });
        ui.end_row();
    });
    if let Some(e) = &f.error {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(e).color(theme::DANGER));
    }
    ui.add_space(10.0);
    let (mut ok, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ok = widgets::pill_button(ui, tl!("Make bookmarks"), true).clicked();
        cancel = widgets::pill_button(ui, tl!("Cancel"), false).clicked();
    });
    if cancel {
        app.from_structure.error = None;
        return true;
    }
    ok && app.apply_bookmarks_from_structure()
}
