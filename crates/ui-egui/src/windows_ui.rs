//! Multiple windows: View ▸ New window (a second view of the document), Move tab to new window,
//! Move tab to main window, Close window, and dragging a tab out of the tab strip (into a new
//! window, or onto another window's tabs).
//!
//! The windows are egui viewports, shown as immediate viewports so they draw with the app's
//! state directly ([`PdfCraftApp::show_windows`]). [`pdfcraft_engine::windows::WindowSet`] says
//! which tabs are in which window; the main window keeps every other tab, as before. While a
//! window draws, the app's active tab is that window's ([`PdfCraftApp::active`] is swapped for
//! the frame), so menus, shortcuts, panels and dialogs act on the document in front of the
//! user, whichever window it is in.
//!
//! Costs: with no extra windows nothing here runs beyond a few comparisons. A minimised window
//! draws nothing and its view's textures are released like a background tab's. Views on screen
//! split the application's texture allowance ([`crate::canvas::DocView::raster_share`]); two
//! views of one document share one render queue and each rendered page becomes one texture,
//! handed to both ([`crate::canvas::DocView::finish_shared_render_frame`]). Edits and undo
//! refresh every view of the document ([`PdfCraftApp::sync_sibling_views`]).
//!
//! The web build has one window: there the commands are disabled and tabs work as before.

use std::collections::HashMap;

use egui::{Align, Pos2, Rect, Stroke, ViewportCommand, ViewportId, vec2};
use pdfcraft_engine::DocId;
use pdfcraft_engine::windows::{Tab, WindowError, WindowId};
use pdfcraft_render::RenderRequest;
use serde_json::{Value, json};

use crate::theme::{self, Tokens};
use crate::{CloseRequest, PdfCraftApp, chrome, icons};

/// How far below its tab strip a tab must be let go to open in a new window (points).
const TEAR_OFF: f32 = 36.0;

/// What the app knows about each window, read while it draws.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct WinInfo {
    pub minimized: bool,
    pub focused: bool,
    /// Where the window's content starts on the screen (points), to compare pointer positions
    /// across windows. Zero when the platform doesn't say (then drops go to a new window).
    pub origin: Pos2,
    /// The tab strip, in the window's own coordinates.
    pub strip: Option<Rect>,
}

impl WinInfo {
    fn strip_on_screen(&self) -> Option<Rect> {
        self.strip.map(|r| r.translate(self.origin.to_vec2()))
    }
}

/// A tab being dragged by its strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TabDrag {
    pub key: u64,
    pub from: WindowId,
}

/// The app's multiple-window state besides [`PdfCraftApp::windows`].
#[derive(Default)]
pub struct WindowsState {
    /// The window being drawn ([`WindowId::MAIN`] outside another window's frame).
    pub(crate) current: WindowId,
    /// The window the open dialog, palette or prompt belongs to (it was opened there).
    pub(crate) overlay: WindowId,
    pub(crate) info: HashMap<WindowId, WinInfo>,
    /// The main window's tab at the last check, by key (indexes shift as tabs close).
    pub(crate) main_active: Option<u64>,
    /// The render queue of each document that several views on screen share.
    pub(crate) shared_queues: HashMap<DocId, Vec<RenderRequest>>,
    /// Views drawn in other windows this frame.
    pub(crate) drawn: Vec<usize>,
    /// Shortcuts pressed in a window while a text field had the keyboard, run next frame there.
    pub(crate) deferred: HashMap<WindowId, Vec<(&'static str, Option<DocId>)>>,
    pub(crate) drag: Option<TabDrag>,
    /// Bring this window to the front once it is shown.
    pub(crate) focus: Option<WindowId>,
    /// Where a window torn off by dragging first appears (screen points).
    pub(crate) spawn_at: HashMap<WindowId, Pos2>,
}

fn viewport_id(w: WindowId) -> ViewportId {
    ViewportId::from_hash_of(("pdfcraft-window", w.0))
}

/// Windows besides the main one exist only where the platform has them.
pub(crate) const SUPPORTED: bool = cfg!(not(target_arch = "wasm32"));

fn unsupported() -> String {
    tl!("Separate windows are only available in the desktop app").to_string()
}

/// A window operation's refusal, in the interface language.
fn window_error(e: WindowError) -> String {
    match e {
        WindowError::NoSuchWindow(w) => crate::i18n::fmt(tl!("{window} has closed"), &[("window", &window_name(w))]),
        WindowError::NoSuchTab(_) => tl!("That tab has closed").to_string(),
        WindowError::TooManyWindows => crate::i18n::fmt(
            tl!("At most {n} windows can be open besides the main window: close one first"),
            &[("n", &pdfcraft_engine::windows::MAX_WINDOWS.to_string())],
        ),
        WindowError::TooManyTabs(w) => crate::i18n::fmt(tl!("{window} has no room for more tabs"), &[("window", &window_name(w))]),
    }
}

/// "Window 2" for window 1: the main window is the first.
fn window_name(w: WindowId) -> String {
    crate::i18n::fmt(tl!("Window {n}"), &[("n", &w.0.saturating_add(1).to_string())])
}

impl PdfCraftApp {
    /// The index of the view (tab) with this key.
    pub fn view_index(&self, key: u64) -> Option<usize> {
        self.views.iter().position(|v| v.key == key)
    }

    /// The window showing the view at `index`.
    pub fn window_of_view(&self, index: usize) -> WindowId {
        self.views.get(index).map_or(WindowId::MAIN, |v| self.windows.window_of(v.key))
    }

    /// The page number box of the window being drawn (each window has its own).
    pub(crate) fn page_input_id(&self) -> egui::Id {
        let id = egui::Id::new("page-input");
        if self.win.current.is_main() { id } else { id.with(self.win.current.0) }
    }

    /// The window being drawn, or the one a command from a shortcut or menu acts in.
    pub fn current_window(&self) -> WindowId {
        self.win.current
    }

    fn tab_at(&self, index: usize) -> Option<Tab> {
        self.views.get(index).map(|v| Tab { key: v.key, doc: v.id })
    }

    pub(crate) fn in_main(&self, index: usize) -> bool {
        self.window_of_view(index).is_main()
    }

    /// A dialog, the palette or a prompt is open (it belongs to one window).
    pub(crate) fn overlay_open(&self) -> bool {
        self.dialog.is_some() || self.palette_open || self.close_request.is_some() || self.password_prompt.is_some() || self.pending_link.is_some()
    }

    /// The main-window tab nearest `index`, other than it (when `index` leaves the main window).
    fn main_neighbour(&self, index: usize) -> Option<usize> {
        let main: Vec<usize> = (0..self.views.len()).filter(|&i| i != index && self.in_main(i)).collect();
        main.iter().copied().find(|&i| i > index).or_else(|| main.last().copied())
    }

    /// View ▸ New window: the active tab's document again, in a new window with its own view.
    pub fn new_window(&mut self) -> Result<WindowId, String> {
        let key = self.active.and_then(|i| self.views.get(i)).map(|v| v.key).ok_or_else(|| tl!("Open a document first").to_string())?;
        self.new_window_for(key)
    }

    /// A new window with a second view of the document that tab `key` shows.
    pub fn new_window_for(&mut self, key: u64) -> Result<WindowId, String> {
        if !SUPPORTED {
            return Err(unsupported());
        }
        let i = self.view_index(key).ok_or_else(|| window_error(WindowError::NoSuchTab(key)))?;
        let view = self.views.get(i).ok_or_else(|| window_error(WindowError::NoSuchTab(key)))?;
        let doc = self.session.get(view.id).ok_or_else(|| window_error(WindowError::NoSuchTab(key)))?;
        let second = view.second_view(&doc.info);
        let w = self.windows.new_window(Tab { key: second.key, doc: second.id }).map_err(window_error)?;
        self.views.push(second);
        self.win.focus = Some(w);
        Ok(w)
    }

    /// Move tab `key` into window `to` (`None`: a new window; [`WindowId::MAIN`]: back to the
    /// main window). Returns the window it is in now.
    pub fn move_tab_to_window(&mut self, key: u64, to: Option<WindowId>) -> Result<WindowId, String> {
        if !SUPPORTED {
            return Err(unsupported());
        }
        let i = self.view_index(key).ok_or_else(|| window_error(WindowError::NoSuchTab(key)))?;
        let tab = self.tab_at(i).ok_or_else(|| window_error(WindowError::NoSuchTab(key)))?;
        let from = self.windows.window_of(key);
        let main_showed_it = from.is_main() && self.active == Some(i) && self.win.current.is_main();
        let target = match to {
            None => {
                if self.windows.get(from).is_some_and(|w| w.tabs.len() <= 1) {
                    return Err(tl!("This tab is already in a window of its own").to_string());
                }
                self.windows.new_window(tab).map_err(window_error)?
            }
            Some(w) => {
                self.windows.move_tab(tab, w).map_err(window_error)?;
                w
            }
        };
        if target.is_main() {
            self.active = Some(i);
        } else if main_showed_it {
            self.active = self.main_neighbour(i);
        }
        self.win.focus = Some(target);
        self.settle_windows();
        Ok(target)
    }

    /// Bring window `w` to the front ([`WindowId::MAIN`] included).
    pub fn focus_window(&mut self, w: WindowId) -> Result<(), String> {
        if !self.windows.exists(w) {
            return Err(window_error(WindowError::NoSuchWindow(w)));
        }
        self.win.focus = Some(w);
        Ok(())
    }

    /// Close window `w`: its tabs close, and each document with unsaved changes that no other
    /// window shows asks first, as closing its tab does. The main window closes the app.
    pub fn request_close_window(&mut self, w: WindowId) {
        if w.is_main() {
            if let Some(ctx) = &self.ctx {
                ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Close);
            }
            return;
        }
        self.close_clean_tabs_of(w);
        if self.windows.get(w).is_some() {
            self.close_request = Some(CloseRequest::Window(w));
            self.win.overlay = w;
            self.win.focus = Some(w);
        }
    }

    /// Close the tabs of window `w` that need no question: those whose document stays open in
    /// another tab, and those without unsaved changes.
    pub(crate) fn close_clean_tabs_of(&mut self, w: WindowId) {
        let keys: Vec<u64> = self.windows.get(w).map(|win| win.tabs.iter().map(|t| t.key).collect()).unwrap_or_default();
        for key in keys.into_iter().rev() {
            let Some(i) = self.view_index(key) else { continue };
            let Some(id) = self.views.get(i).map(|v| v.id) else { continue };
            let elsewhere = self.views.iter().any(|v| v.id == id && v.key != key);
            if elsewhere || !self.has_unsaved_work(i) {
                self.close_tab(i);
            }
        }
    }

    /// The first tab of window `w` with unsaved work.
    pub(crate) fn first_dirty_in(&self, w: WindowId) -> Option<usize> {
        (0..self.views.len()).find(|&i| self.window_of_view(i) == w && self.has_unsaved_work(i))
    }

    /// Keep the window model and the tabs in step: forget closed tabs and windows, and when
    /// something pointed the main window at a tab in another window (open a recent file that
    /// is already open there, a prompt bringing its document forward), bring that window
    /// forward instead.
    pub(crate) fn settle_windows(&mut self) {
        if self.windows.is_empty() && self.win.info.len() <= 1 {
            self.win.main_active = self.active.and_then(|i| self.views.get(i)).map(|v| v.key);
            self.win.overlay = WindowId::MAIN;
            return;
        }
        let views = &self.views;
        self.windows.retain(|t| views.iter().any(|v| v.key == t.key));
        let windows = &self.windows;
        self.win.info.retain(|w, _| windows.exists(*w));
        self.win.deferred.retain(|w, _| windows.exists(*w));
        self.win.spawn_at.retain(|w, _| windows.exists(*w));
        if self.active.is_some_and(|i| i >= self.views.len()) {
            self.active = None;
        }
        if let Some(i) = self.active
            && let Some(key) = self.views.get(i).map(|v| v.key)
            && self.windows.is_torn_off(key)
        {
            if let Ok(w) = self.windows.set_active(key) {
                self.win.focus = Some(w);
            }
            self.active = self.win.main_active.and_then(|k| self.view_index(k)).filter(|&j| self.in_main(j));
        }
        self.win.main_active = self.active.and_then(|i| self.views.get(i)).map(|v| v.key);
        let overlay = self.win.overlay;
        if !self.overlay_open() || !self.windows.exists(overlay) || self.win.info.get(&overlay).is_some_and(|i| i.minimized) {
            self.win.overlay = WindowId::MAIN;
        }
    }

    /// Views on screen in other windows this frame (their windows' tabs, unless minimised).
    pub(crate) fn window_views(&self) -> Vec<(WindowId, usize)> {
        self.windows
            .windows()
            .iter()
            .filter(|w| !self.win.info.get(&w.id).is_some_and(|i| i.minimized))
            .filter_map(|w| self.view_index(w.active).map(|i| (w.id, i)))
            .collect()
    }

    /// Whether window `w` has the keyboard (the main window too).
    pub(crate) fn window_focused(&self, w: WindowId) -> bool {
        self.win.info.get(&w).is_some_and(|i| i.focused)
    }

    /// An edit or undo refreshed one view of a document: refresh the others showing it (in
    /// other windows) the same way, so every window shows the document as it is.
    pub(crate) fn sync_sibling_views(&mut self) {
        use crate::canvas::Refresh;
        if self.views.len() > 1 {
            let mut groups: HashMap<DocId, Vec<usize>> = HashMap::new();
            for (i, v) in self.views.iter().enumerate() {
                groups.entry(v.id).or_default().push(i);
            }
            for (id, idxs) in groups {
                if idxs.len() < 2 {
                    continue;
                }
                let mut merged = Refresh::None;
                for &i in &idxs {
                    let Some(v) = self.views.get(i) else { continue };
                    merged = match (merged, &v.refreshed) {
                        (Refresh::All, _) | (_, Refresh::All) => Refresh::All,
                        (Refresh::None, r) => r.clone(),
                        (Refresh::Pages(mut a), Refresh::Pages(b)) => {
                            a.extend(b.iter().copied());
                            Refresh::Pages(a)
                        }
                        (m, Refresh::None) => m,
                    };
                }
                if merged == Refresh::None {
                    continue;
                }
                let Some(doc) = self.session.get(id) else { continue };
                for &i in &idxs {
                    let Some(v) = self.views.get_mut(i) else { continue };
                    if v.refreshed == merged {
                        continue;
                    }
                    match &merged {
                        Refresh::All => v.document_changed(&doc.info),
                        Refresh::Pages(pages) => pages.iter().for_each(|&p| v.page_changed(p)),
                        Refresh::None => {}
                    }
                }
            }
        }
        for v in &mut self.views {
            v.refreshed = Refresh::None;
        }
    }

    /// Draw every window besides the main one (call from the main window's frame).
    pub(crate) fn show_windows(&mut self, ctx: &egui::Context) {
        self.win.drawn.clear();
        if !SUPPORTED || self.windows.is_empty() {
            if self.win.focus.take().is_some_and(WindowId::is_main) && SUPPORTED {
                ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
            }
            return;
        }
        let ids: Vec<WindowId> = self.windows.windows().iter().map(|w| w.id).collect();
        for w in ids {
            self.show_window(ctx, w);
        }
        if self.win.focus.take().is_some_and(WindowId::is_main) {
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
        }
    }

    fn show_window(&mut self, ctx: &egui::Context, w: WindowId) {
        let Some(tab) = self.windows.active(w) else { return };
        let name = self.view_index(tab.key).and_then(|i| self.views.get(i)).and_then(|v| self.session.get(v.id)).map(|d| d.display_name());
        let title = name.map_or_else(|| "PdfCraft".to_owned(), |n| format!("{n} — PdfCraft"));
        let vid = viewport_id(w);
        if ctx.embed_viewports() {
            // No native windows (tests, platforms without them): a window inside the main one.
            let mut open = true;
            egui::Window::new(title)
                .id(egui::Id::new(vid))
                .fixed_size([900.0, 620.0])
                .collapsible(false)
                .open(&mut open)
                .show(ctx, |ui| self.window_frame(w, ui, true));
            if !open {
                self.request_close_window(w);
            }
        } else {
            let mut builder = egui::ViewportBuilder::default().with_title(title).with_inner_size([1100.0, 800.0]).with_min_inner_size([520.0, 380.0]);
            if let Some(at) = self.win.spawn_at.get(&w) {
                builder = builder.with_position(*at);
            }
            ctx.show_viewport_immediate(vid, builder, |ui, _class| self.window_frame(w, ui, false));
        }
        if self.win.focus == Some(w) {
            self.win.focus = None;
            ctx.send_viewport_cmd_to(vid, ViewportCommand::Focus);
        }
    }

    /// One window's frame: its tabs, the menus and panels, its document, and any dialog opened
    /// from it. The app's active tab is this window's while it draws.
    fn window_frame(&mut self, w: WindowId, ui: &mut egui::Ui, embedded: bool) {
        let ctx = ui.ctx().clone();
        let (close, minimized, focused, inner) =
            ctx.input(|i| (i.viewport().close_requested(), i.viewport().minimized.unwrap_or(false), i.focused, i.viewport().inner_rect));
        if close && !embedded {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.request_close_window(w);
        }
        // Embedded in the main window, the main window's input is this window's too: keys and
        // focus stay the main window's, so nothing runs twice.
        let focused = focused && !embedded;
        let origin = if embedded { self.win.info.get(&WindowId::MAIN).map_or(Pos2::ZERO, |i| i.origin) } else { inner.map_or(Pos2::ZERO, |r| r.min) };
        let info = self.win.info.entry(w).or_default();
        info.minimized = minimized;
        info.focused = focused;
        info.origin = origin;
        // A minimised window costs nothing: no layout, no rendering (its textures are released
        // like a background tab's).
        if minimized {
            return;
        }
        self.win.spawn_at.remove(&w);
        let Some(index) = self.windows.active(w).and_then(|t| self.view_index(t.key)) else { return };
        let main_active = self.win.main_active;
        let newest = self.views.iter().map(|v| v.key).max().unwrap_or(0);
        let had_overlay = self.overlay_open();
        self.active = Some(index);
        self.win.current = w;
        let pending = self.win.deferred.remove(&w).unwrap_or_default();
        if !embedded && (!had_overlay || self.win.overlay == w) {
            self.shortcuts(&ctx);
        }
        self.window_tab_strip(ui, w);
        if self.active.is_some_and(|i| self.window_of_view(i) == w) {
            chrome::mode_bar(self, ui);
            chrome::right_rail(self, ui);
            if self.right.is_some() && self.mode != crate::Mode::Read {
                crate::panels::right_panel(self, ui);
            }
            if self.left_open && self.mode != crate::Mode::Read {
                crate::panels::left_panel(self, ui);
            }
            let t = Tokens::get(&ctx);
            let shown = self.active;
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.pasteboard)).show(ui, |ui| {
                if let Some(i) = shown.filter(|&i| i < self.views.len()) {
                    crate::canvas::document_area(self, i, ui);
                }
            });
        }
        if self.process_pending_edits() && self.close_request.is_none() {
            for (id, doc) in pending {
                if self.active_ids().map(|(_, active)| active) == doc {
                    self.execute(id);
                }
            }
        }
        if self.win.overlay == w || (!had_overlay && self.overlay_open()) {
            crate::palette::show(self, &ctx);
            crate::dialogs::show(self, &ctx);
            crate::edit_text_ui::preview_dialog(self, &ctx);
        }
        if focused {
            self.show_progress(&ctx);
            crate::widgets::toast(self, &ctx);
        }
        let active_doc = self.active_ids().map(|(_, id)| id);
        if let Some(i) = self.active.filter(|&i| self.window_of_view(i) == w) {
            let blocked = self.overlay_open() || !focused || ctx.egui_wants_keyboard_input();
            let copy = self.views.get(i).and_then(|v| self.session.get(v.id)).is_some_and(|d| d.allows_copying());
            if let Some(view) = self.views.get_mut(i) {
                if blocked || view.organize {
                    view.cancel_reader_input();
                } else {
                    view.finish_reader_copy(&ctx, copy);
                }
                view.draw_ppp = Some(ctx.pixels_per_point());
            }
            self.win.drawn.push(i);
        }
        // Shortcuts deferred in this window run in this window next frame.
        let (mine, rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.deferred_commands).into_iter().partition(|(_, doc)| *doc == active_doc && doc.is_some());
        self.deferred_commands = rest;
        if !mine.is_empty() {
            self.win.deferred.insert(w, mine);
        }
        if !had_overlay && self.overlay_open() {
            self.win.overlay = w;
        }
        self.finish_window_frame(w, main_active, newest);
    }

    /// After a window's frame: give the main window its own tab back, and adopt what the frame
    /// did to the active tab (a tab clicked, a document opened from this window, a tab of the
    /// main window brought forward).
    fn finish_window_frame(&mut self, w: WindowId, main_active: Option<u64>, newest: u64) {
        let shown = self.active.and_then(|i| self.views.get(i)).map(|v| (v.key, v.id));
        let mut main = main_active.and_then(|k| self.view_index(k)).filter(|&i| self.in_main(i));
        if let Some((key, doc)) = shown {
            match self.windows.window_of(key) {
                // Opened from this window: it stays here.
                m if m.is_main() && key > newest => {
                    if self.windows.add_tab(Tab { key, doc }, w).is_err() {
                        main = self.view_index(key);
                    }
                }
                m if m.is_main() => {
                    main = self.view_index(key);
                    self.win.focus = Some(WindowId::MAIN);
                }
                other => {
                    let _ = self.windows.set_active(key);
                    if other != w {
                        self.win.focus = Some(other);
                    }
                }
            }
        }
        self.active = main;
        self.win.main_active = main.and_then(|i| self.views.get(i)).map(|v| v.key);
        self.win.current = WindowId::MAIN;
    }

    /// A window's tab strip: its tabs (click to show, ⌘W or × to close, right-click for the
    /// window commands, drag out to tear off or onto another window).
    fn window_tab_strip(&mut self, ui: &mut egui::Ui, w: WindowId) {
        let t = Tokens::get(ui.ctx());
        let Some(win) = self.windows.get(w).cloned() else { return };
        egui::Panel::top(egui::Id::new(("window_tabs", w.0)))
            .exact_size(38.0)
            .frame(egui::Frame::NONE.fill(t.titlebar).inner_margin(egui::Margin { left: 8, right: 10, top: 0, bottom: 0 }))
            .show(ui, |ui| {
                if let Some(info) = self.win.info.get_mut(&w) {
                    info.strip = Some(ui.max_rect());
                }
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let back_width = 32.0;
                    let budget = (ui.available_width() - back_width - 8.0).max(0.0);
                    let named: Vec<(usize, String, Option<String>, bool)> = win
                        .tabs
                        .iter()
                        .filter_map(|tab| {
                            let i = self.view_index(tab.key)?;
                            let d = self.session.get(tab.doc)?;
                            let name = d.display_name();
                            let file = (name != d.name).then(|| d.name.clone());
                            Some((i, name, file, d.dirty))
                        })
                        .collect();
                    let natural: Vec<f32> = named.iter().map(|(_, n, _, _)| chrome::tab_natural_width(ui, &t, n)).collect();
                    let gap = ui.spacing().item_spacing.x;
                    let cap = chrome::tab_cap(&natural, budget, gap, chrome::TAB_MIN_WIDTH);
                    let mut close = None;
                    egui::ScrollArea::horizontal().id_salt(("window_tab_scroll", w.0)).max_width(budget).auto_shrink([true, true]).show(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            for (i, name, file, dirty) in &named {
                                let Some(key) = self.views.get(*i).map(|v| v.key) else { continue };
                                let active = key == win.active;
                                let resp = chrome::tab(ui, &t, "file-text", name, file.as_deref(), *dirty, active, &mut close, *i, cap);
                                if resp.clicked() {
                                    let _ = self.windows.set_active(key);
                                    self.active = Some(*i);
                                }
                                tab_context_menu(self, &resp, key);
                                tab_drag(self, ui, &resp, key, w, name);
                            }
                        });
                    });
                    if let Some(i) = close {
                        self.request_close_tab(i);
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if icons::button(ui, "panels-top-left", 28.0, false, tl!("Move tab to main window")).clicked() {
                            self.execute("window.move_to_main");
                        }
                    });
                });
            });
    }

    /// `ui.state`'s `windows`: every window, its tabs (by view key, the documents they show) and
    /// which one it shows.
    pub(crate) fn windows_state(&self) -> Value {
        let tab = |i: usize| {
            let v = self.views.get(i)?;
            let d = self.session.get(v.id)?;
            Some(json!({ "view": v.key, "doc": v.id.0, "name": d.name, "dirty": d.dirty }))
        };
        let info = |w: WindowId| self.win.info.get(&w).copied().unwrap_or_default();
        let main: Vec<Value> = (0..self.views.len()).filter(|&i| self.in_main(i)).filter_map(tab).collect();
        let mut out = vec![json!({
            "id": 0,
            "main": true,
            "tabs": main,
            "active_view": self.active.and_then(|i| self.views.get(i)).map(|v| v.key),
            "focused": info(WindowId::MAIN).focused,
        })];
        for w in self.windows.windows() {
            let tabs: Vec<Value> = w.tabs.iter().filter_map(|t| self.view_index(t.key)).filter_map(tab).collect();
            out.push(json!({
                "id": w.id.0,
                "main": false,
                "tabs": tabs,
                "active_view": w.active,
                "minimized": info(w.id).minimized,
                "focused": info(w.id).focused,
            }));
        }
        json!({ "supported": SUPPORTED, "windows": out })
    }

    /// The control channel's window options: `window-new` (a view key, or `active`),
    /// `window-move-tab` (`<view>:<window>`, the window a number, `new` or `main`),
    /// `window-close` and `window-focus` (a window number; 0 is the main window).
    pub(crate) fn set_window_option(&mut self, key: &str, value: &str) -> Option<Result<(), String>> {
        let window = |s: &str| -> Result<WindowId, String> {
            match s.trim() {
                "main" => Ok(WindowId::MAIN),
                n => n.parse::<u32>().map(WindowId).map_err(|_| format!("{n:?} is not a window: use a number from ui.state windows, or main")),
            }
        };
        let view = |s: &str| -> Result<u64, String> {
            s.trim().parse::<u64>().map_err(|_| format!("{s:?} is not a view key: use a view from ui.state windows"))
        };
        let r = match key {
            "window-new" => (|| {
                let key = if value.trim() == "active" {
                    self.active.and_then(|i| self.views.get(i)).map(|v| v.key).ok_or("no document is active")?
                } else {
                    view(value)?
                };
                self.new_window_for(key).map(|_| ())
            })(),
            "window-move-tab" => (|| {
                let (v, to) = value.split_once(':').ok_or("window-move-tab takes <view>:<window> (a window number, new or main)")?;
                let to = if to.trim() == "new" { None } else { Some(window(to)?) };
                self.move_tab_to_window(view(v)?, to).map(|_| ())
            })(),
            "window-close" => (|| {
                let w = window(value)?;
                if w.is_main() {
                    return Err("window-close closes another window; quit to close the main window".to_string());
                }
                if !self.windows.exists(w) {
                    return Err(window_error(WindowError::NoSuchWindow(w)));
                }
                self.request_close_window(w);
                Ok(())
            })(),
            "window-focus" => (|| self.focus_window(window(value)?))(),
            _ => return None,
        };
        Some(r)
    }

    /// The window commands (View menu, palette, tab menus).
    pub(crate) fn run_window_command(&mut self, id: &str) -> bool {
        let key = self.active.and_then(|i| self.views.get(i)).map(|v| v.key);
        let w = self.win.current;
        let result = match (id, key) {
            ("window.new", Some(key)) => self.new_window_for(key).map(|_| ()),
            ("window.move_tab", Some(key)) => self.move_tab_to_window(key, None).map(|_| ()),
            ("window.move_to_main", Some(key)) => self.move_tab_to_window(key, Some(WindowId::MAIN)).map(|_| ()),
            ("window.close", _) => {
                self.request_close_window(w);
                Ok(())
            }
            ("window.new" | "window.move_tab" | "window.move_to_main", None) => Err(tl!("Open a document first").to_string()),
            _ => return false,
        };
        if let Err(e) = result {
            self.notify(e);
        }
        true
    }

    /// Why a disabled window command can't run (`None` for other commands, or when it is only
    /// waiting for a document).
    pub(crate) fn window_refusal(&self, id: &str) -> Option<String> {
        if !id.starts_with("window.") {
            return None;
        }
        if !SUPPORTED {
            return Some(unsupported());
        }
        let tab_window = self.active.map(|i| self.window_of_view(i));
        let full = self.windows.len() >= pdfcraft_engine::windows::MAX_WINDOWS;
        match id {
            "window.close" if self.win.current.is_main() => Some(tl!("This is the main window: quit to close it").to_string()),
            "window.move_to_main" if tab_window.is_some_and(WindowId::is_main) => Some(tl!("This tab is in the main window").to_string()),
            "window.move_tab" if tab_window.is_some_and(|w| !w.is_main()) && !full => {
                Some(tl!("This tab is already in a window of its own").to_string())
            }
            "window.new" | "window.move_tab" if full && tab_window.is_some() => Some(window_error(WindowError::TooManyWindows)),
            _ => None,
        }
    }

    /// Whether a window command can run now (beyond its registry entry).
    pub(crate) fn window_command_enabled(&self, id: &str) -> Option<bool> {
        let tab_window = self.active.map(|i| self.window_of_view(i));
        Some(match id {
            "window.new" => SUPPORTED && self.windows.len() < pdfcraft_engine::windows::MAX_WINDOWS,
            "window.move_tab" => {
                SUPPORTED
                    && match tab_window {
                        Some(w) if w.is_main() => self.windows.len() < pdfcraft_engine::windows::MAX_WINDOWS,
                        Some(w) => self.windows.get(w).is_some_and(|win| win.tabs.len() > 1),
                        None => false,
                    }
            }
            "window.move_to_main" => SUPPORTED && tab_window.is_some_and(|w| !w.is_main()),
            "window.close" => SUPPORTED && !self.win.current.is_main(),
            _ => return None,
        })
    }
}

/// Right-click a tab: the window commands for it.
pub(crate) fn tab_context_menu(app: &mut PdfCraftApp, resp: &egui::Response, key: u64) {
    if !SUPPORTED {
        return;
    }
    resp.context_menu(|ui| {
        ui.set_min_width(220.0);
        let from = app.windows.window_of(key);
        let mut result = None;
        if ui.add_enabled(app.windows.len() < pdfcraft_engine::windows::MAX_WINDOWS, egui::Button::new(tl!("New window"))).clicked() {
            result = Some(app.new_window_for(key).map(|_| ()));
        }
        let alone = app.windows.get(from).is_some_and(|w| w.tabs.len() <= 1);
        if ui.add_enabled(!alone, egui::Button::new(tl!("Move tab to new window"))).clicked() {
            result = Some(app.move_tab_to_window(key, None).map(|_| ()));
        }
        if !from.is_main() && ui.button(tl!("Move tab to main window")).clicked() {
            result = Some(app.move_tab_to_window(key, Some(WindowId::MAIN)).map(|_| ()));
        }
        let others: Vec<WindowId> = app.windows.windows().iter().map(|w| w.id).filter(|&id| id != from).collect();
        if !others.is_empty() {
            ui.separator();
            for other in others {
                let label = crate::i18n::fmt(tl!("Move tab to {window}"), &[("window", &window_name(other))]);
                if ui.button(label).clicked() {
                    result = Some(app.move_tab_to_window(key, Some(other)).map(|_| ()));
                }
            }
        }
        if let Some(r) = result {
            ui.close();
            if let Err(e) = r {
                app.notify(e);
            }
        }
    });
}

/// Dragging a tab: let go over another window's tabs to move it there, or away from the tab
/// strip to open it in a window of its own.
pub(crate) fn tab_drag(app: &mut PdfCraftApp, ui: &egui::Ui, resp: &egui::Response, key: u64, from: WindowId, name: &str) {
    if !SUPPORTED {
        return;
    }
    if resp.drag_started() {
        app.win.drag = Some(TabDrag { key, from });
    }
    if app.win.drag.is_none_or(|d| d.key != key) {
        return;
    }
    let ctx = ui.ctx();
    let pointer = resp.interact_pointer_pos().or_else(|| ctx.pointer_latest_pos());
    let strip = app.win.info.get(&from).and_then(|i| i.strip).unwrap_or(resp.rect);
    let away = pointer.is_some_and(|p| p.y > strip.bottom() + TEAR_OFF || p.y < strip.top() - TEAR_OFF || !ctx.content_rect().contains(p));
    if resp.dragged()
        && let Some(p) = pointer
    {
        let t = Tokens::get(ctx);
        egui::Area::new(egui::Id::new("tab-drag-ghost")).order(egui::Order::Tooltip).fixed_pos(p + vec2(12.0, 10.0)).interactable(false).show(
            ctx,
            |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(icons::image("file-text", 15.0, t.accent));
                        ui.label(egui::RichText::new(chrome::tab_name(name)).font(theme::regular(13.0)));
                    });
                    if away {
                        ui.label(egui::RichText::new(tl!("Release to open in a new window")).small().color(t.text_muted));
                    }
                });
            },
        );
        // A line under the strip shows where tearing off starts.
        if away {
            ui.painter()
                .clone()
                .with_layer_id(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("tab-tear")))
                .line_segment([Pos2::new(strip.left(), strip.bottom()), Pos2::new(strip.right(), strip.bottom())], Stroke::new(2.0, t.accent));
        }
    }
    if !resp.drag_stopped() {
        return;
    }
    app.win.drag = None;
    let Some(p) = pointer else { return };
    let origin = app.win.info.get(&from).map_or(Pos2::ZERO, |i| i.origin);
    let on_screen = p + origin.to_vec2();
    let target = drop_target(app, on_screen, from);
    let result = match target {
        Some(w) if w == from => return,
        Some(w) => app.move_tab_to_window(key, Some(w)),
        None if away => {
            let r = app.move_tab_to_window(key, None);
            if let Ok(w) = r
                && origin != Pos2::ZERO
            {
                app.win.spawn_at.insert(w, on_screen - vec2(80.0, 16.0));
            }
            r
        }
        None => return,
    };
    if let Err(e) = result {
        app.notify(e);
    }
}

/// The window whose tab strip is under `on_screen` (other windows first: they are in front).
fn drop_target(app: &PdfCraftApp, on_screen: Pos2, from: WindowId) -> Option<WindowId> {
    let hit = |w: WindowId| {
        app.win.info.get(&w).filter(|i| !i.minimized).and_then(WinInfo::strip_on_screen).is_some_and(|r| r.expand(6.0).contains(on_screen))
    };
    // Without screen positions every window starts at zero: only the window itself can be told.
    let positioned =
        |w: WindowId| app.win.info.get(&w).is_some_and(|i| i.origin != Pos2::ZERO) || app.ctx.as_ref().is_some_and(egui::Context::embed_viewports);
    app.windows
        .windows()
        .iter()
        .rev()
        .map(|w| w.id)
        .chain(std::iter::once(WindowId::MAIN))
        .find(|&w| (w == from || (positioned(w) && positioned(from))) && hit(w))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Vec2;

    #[test]
    fn window_names_count_from_the_main_window() {
        crate::i18n::set_current(crate::i18n::Lang::from_pref("en"));
        assert_eq!(window_name(WindowId(1)), "Window 2");
        assert_eq!(window_name(WindowId(u32::MAX)), format!("Window {}", u32::MAX));
    }

    #[test]
    fn strips_are_compared_on_the_screen() {
        let info =
            WinInfo { origin: Pos2::new(100.0, 50.0), strip: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 38.0))), ..Default::default() };
        assert!(info.strip_on_screen().is_some_and(|r| r.contains(Pos2::new(150.0, 60.0))));
        assert!(!info.strip_on_screen().is_some_and(|r| r.contains(Pos2::new(50.0, 60.0))));
    }
}
