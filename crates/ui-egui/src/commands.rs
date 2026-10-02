//! What each registered command does in this frontend (`printcraft_engine::commands` says
//! what it is called, where it appears, which key runs it and when it is enabled).
//!
//! Menus, keyboard shortcuts, the palette, the tool panels and automation (`set_option`, and
//! the control channel later) all call `PrintCraftApp::execute`.

use printcraft_engine::Edit;
use printcraft_engine::commands::{self, COMMANDS, CommandSpec};

use crate::{Dialog, Mode, PrintCraftApp, PropsTab, RightPanel, SaveTarget, theme::ThemeKind, widgets};

impl PrintCraftApp {
    pub(crate) fn command_enabled(&self, spec: &CommandSpec) -> bool {
        commands::is_enabled(spec, &self.session, self.active_ids().map(|(_, id)| id))
    }

    /// Run a registered command by id. Returns `false` when the id is unknown or the command
    /// is disabled right now (the user is told why).
    pub fn execute(&mut self, id: &str) -> bool {
        let Some(spec) = commands::command(id) else { return false };
        if !self.command_enabled(spec) {
            let why = match spec.needs {
                commands::Needs::Undo => "Nothing to undo".to_string(),
                commands::Needs::Redo => "Nothing to redo".to_string(),
                commands::Needs::FillForms if self.active.is_some() => "This document has no form fields you can fill in".to_string(),
                commands::Needs::Marks(k) if self.active.is_some() => format!(
                    "This document has no {} to change",
                    match k {
                        printcraft_engine::MarkKind::HeaderFooter => "header or footer",
                        printcraft_engine::MarkKind::Watermark => "watermark",
                        printcraft_engine::MarkKind::Background => "background",
                    }
                ),
                commands::Needs::Security | commands::Needs::ProtectedSecurity if self.active.is_some() => {
                    if self.active_ids().and_then(|(_, id)| self.session.get(id)).is_some_and(|d| d.allows_security_change()) {
                        "This document isn't password-protected".to_string()
                    } else {
                        "Only the document's owner can change its security (open it with the permissions password)".to_string()
                    }
                }
                commands::Needs::Assembly | commands::Needs::Modification | commands::Needs::Annotate if self.active.is_some() => {
                    "The document's security settings don't allow this change".to_string()
                }
                _ => "Open a document first".to_string(),
            };
            self.notify(why);
            return false;
        }
        let active = self.active;
        let targets = active.map(|i| self.views[i].target_pages()).unwrap_or_default();
        match id {
            "file.open" => self.open_dialog(),
            "page.combine" => self.combine_dialog(),
            "file.save" => {
                self.save_active(SaveTarget::InPlace);
            }
            "file.save_as" => {
                self.save_active(SaveTarget::As);
            }
            "file.close" => {
                if let Some(i) = active {
                    self.request_close_tab(i);
                }
            }
            "file.properties" => self.dialog = Some(Dialog::Properties(PropsTab::Description)),
            "protect.properties" => self.dialog = Some(Dialog::Properties(PropsTab::Security)),
            "protect.password" => {
                self.protect_draft = Default::default();
                self.dialog = Some(Dialog::Protect);
            }
            "protect.remove" => {
                if self.apply_edit(Edit::RemoveProtection) {
                    self.notify("Security will be removed when you save");
                }
            }
            "page.number" => {
                if let Some(i) = active {
                    let v = &self.views[i];
                    let pages: Vec<usize> = if v.selected.is_empty() { vec![v.current] } else { v.selected.iter().copied().collect() };
                    let (lo, hi) = (pages.iter().min().copied().unwrap_or(0), pages.iter().max().copied().unwrap_or(0));
                    self.number_draft = crate::NumberDraft { from: lo + 1, to: hi + 1, ..self.number_draft.clone() };
                }
                self.dialog = Some(Dialog::NumberPages);
            }
            link if printcraft_engine::links::for_command(link).is_some() => {
                let url = printcraft_engine::links::for_command(link).expect("checked").url;
                self.open_url(url);
            }
            "bookmark.add" => self.bookmark_action(crate::panels::BmAction::New),
            "edit.undo" => self.undo(),
            "edit.redo" => self.redo(),
            "edit.find" => {
                if let Some(i) = active {
                    self.views[i].open_find();
                }
            }
            "view.palette" => self.palette_open = !self.palette_open,
            "view.full_screen" => {
                let on = !self.full_screen;
                match self.ctx.clone() {
                    Some(ctx) => self.set_full_screen(&ctx, on),
                    None => self.full_screen = on,
                }
            }
            "view.read_mode" => self.mode = if self.mode == Mode::Read { Mode::AllTools } else { Mode::Read },
            "view.theme" => {
                let next = if self.theme == ThemeKind::Light { ThemeKind::Dark } else { ThemeKind::Light };
                match self.ctx.clone() {
                    Some(ctx) => self.set_theme(&ctx, next),
                    None => self.theme = next,
                }
            }
            "comment.list" => self.right = Some(RightPanel::Comments),
            tool if crate::comments::CommentTool::from_command(tool).is_some() => {
                let tool = crate::comments::CommentTool::from_command(tool).expect("checked");
                self.comment_prefs.group_tool[tool.group()] = tool;
                self.quick_tool = crate::QuickTool::Comment(tool);
                // Acrobat opens the Comments panel with the commenting tools.
                if self.right.is_none() {
                    self.right = Some(RightPanel::Comments);
                }
                // A text selection made before picking a markup tool is marked right away.
                if let (Some(kind), Some(i)) = (tool.markup(), active) {
                    let info = &self.session.get(self.views[i].id).expect("active").info;
                    if let Some((page, quads)) = self.views[i].selection_quads(info) {
                        self.views[i].clear_selection();
                        let style = self.comment_prefs.style(tool);
                        let author = self.comment_prefs.author.clone();
                        let shape = printcraft_engine::Shape::TextMarkup { kind, quads };
                        self.apply_edit(Edit::AddAnnotation(printcraft_engine::NewAnnotation {
                            page,
                            shape,
                            style,
                            contents: String::new(),
                            author,
                        }));
                    }
                }
            }
            "form.fields" => self.right = Some(RightPanel::Fields),
            "form.clear" => {
                if let Some(i) = active {
                    self.views[i].forms.focus = None;
                }
                self.apply_edit(Edit::ResetForm { names: None });
            }
            "page.organize" => {
                if let Some(i) = active {
                    self.views[i].organize = !self.views[i].organize;
                }
            }
            "page.rotate" => {
                self.apply_edit(Edit::RotatePages { pages: targets, degrees: 90 });
            }
            "page.rotate_ccw" => {
                self.apply_edit(Edit::RotatePages { pages: targets, degrees: -90 });
            }
            "page.delete" => {
                self.apply_edit(Edit::DeletePages { pages: targets });
            }
            "page.insert_blank" => {
                if let (Some(i), Some(&last)) = (active, targets.last())
                    && let Some(p) = self.session.get(self.views[i].id).and_then(|d| d.info.pages.get(last))
                {
                    let (w, h) = ((p.crop[2] - p.crop[0]).abs().max(1.0) as f64, (p.crop[3] - p.crop[1]).abs().max(1.0) as f64);
                    self.apply_edit(Edit::InsertBlankPage { at: last + 1, width: w, height: h });
                }
            }
            "page.insert" => self.insert_from_file_dialog(),
            "edit.header_footer"
            | "edit.watermark"
            | "edit.background"
            | "edit.header_footer.update"
            | "edit.watermark.update"
            | "edit.background.update" => {
                use printcraft_engine::MarkKind as K;
                let kind = if id.starts_with("edit.header_footer") {
                    K::HeaderFooter
                } else if id.starts_with("edit.watermark") {
                    K::Watermark
                } else {
                    K::Background
                };
                self.marks_draft.replace = id.ends_with(".update");
                self.dialog = Some(Dialog::Marks(kind));
            }
            "edit.header_footer.remove" | "edit.watermark.remove" | "edit.background.remove" => {
                use printcraft_engine::MarkKind as K;
                let kind = match id {
                    "edit.header_footer.remove" => K::HeaderFooter,
                    "edit.watermark.remove" => K::Watermark,
                    _ => K::Background,
                };
                self.apply_edit(Edit::RemoveMarks { kind });
            }
            "export.image" => self.dialog = Some(Dialog::Export(crate::export_ui::ExportKind::Image)),
            "export.text" => self.dialog = Some(Dialog::Export(crate::export_ui::ExportKind::Text)),
            "page.duplicate" => {
                self.apply_edit(Edit::DuplicatePages { pages: targets });
            }
            "page.crop" => {
                self.quick_tool = crate::QuickTool::Crop;
                self.notify("Drag a rectangle on a page to crop it; double-click a page for Set Page Boxes");
            }
            "page.boxes" => {
                self.boxes_draft.seeded = None;
                self.dialog = Some(Dialog::PageBoxes);
            }
            "page.extract" => self.extract_selection(),
            "page.split" => self.dialog = Some(Dialog::Split),
            "help.shortcuts" => self.dialog = Some(Dialog::Shortcuts),
            "help.about" => self.dialog = Some(Dialog::About),
            _ => return false,
        }
        true
    }

    /// Run the registered keyboard shortcuts (more specific combinations first).
    pub(crate) fn registry_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let typing = ctx.egui_wants_keyboard_input();
        let mut specs: Vec<&CommandSpec> = COMMANDS.iter().filter(|c| c.shortcut.is_some()).collect();
        specs.sort_by_key(|c| std::cmp::Reverse(c.shortcut.map(|s| s.modifier_count()).unwrap_or(0)));
        for spec in specs {
            let s = spec.shortcut.expect("filtered");
            if typing && !spec.in_text {
                continue;
            }
            let Some(key) = Key::from_name(s.key) else { continue };
            let mut m = Modifiers::NONE;
            if s.command {
                m |= Modifiers::COMMAND;
            }
            if s.shift {
                m |= Modifiers::SHIFT;
            }
            if s.mac_ctrl {
                m |= Modifiers::CTRL;
            }
            if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, key))) {
                self.execute(spec.id);
            }
        }
    }
}

/// Render a top-level menu's registered commands (with live labels, shortcuts and enablement).
pub(crate) fn registry_menu(app: &mut PrintCraftApp, ui: &mut egui::Ui, menu: &str) {
    let mac = cfg!(target_os = "macos") || cfg!(target_arch = "wasm32");
    for spec in commands::menu(menu) {
        let label = commands::current_label(spec, &app.session, app.active_ids().map(|(_, id)| id));
        let shortcut = spec.shortcut.map(|s| s.label(mac)).unwrap_or_default();
        let enabled = app.command_enabled(spec);
        let resp = ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(shortcut));
        if resp.clicked() {
            app.execute(spec.id);
            ui.close();
        }
    }
    let _ = widgets::menu_item;
}
