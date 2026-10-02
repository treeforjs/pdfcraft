//! printcraft-ui-egui — the first PrintCraft shell (L7).
//!
//! Layout grammar follows plan/acrobat/02-ui-ux.md §1: tab strip, mode bar, left tool panel,
//! floating quick-action bar, document area, right panel + right rail with page navigation.
//! Everything here is presentation: documents, rendering and the tool catalogue live in
//! `printcraft-engine`.

mod canvas;
mod chrome;
mod commands;
pub mod comments;
mod comments_panel;
pub mod control;
mod crop;
mod export_ui;
mod marks_ui;
/// Header & footer / watermark / background dialog types (tests and automation).
pub mod marks {
    pub use crate::marks_ui::{MarksDraft, PageRange, Subset};
}
mod dialogs;
mod editing;
mod files;
pub mod fill_sign;
pub mod forms_ui;
mod home;
mod icon_data;
pub mod icons;
mod pageboxes;
mod palette;
mod panels;
mod protect;
mod recovery;
pub mod theme;
mod widgets;

use printcraft_engine::{DocId, Session};

pub use canvas::DocView;
pub use editing::{CloseRequest, SaveTarget};
pub use files::{FilePurpose, SplitDraft};
pub use recovery::{AUTOSAVE_SECS, RecoveryMeta, RecoveryStore};
use theme::ThemeKind;

/// Top-level workspace modes (Acrobat's mode bar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    AllTools,
    Read,
    Edit,
    Convert,
    Sign,
}

/// What the left panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeftPanel {
    AllTools,
    Tool(&'static str),
}

/// Right-hand panels, opened from the rail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightPanel {
    Comments,
    Bookmarks,
    Pages,
    Fields,
    Layers,
    Attachments,
}

/// Quick-action bar tools (the vertical floating strip).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickTool {
    Select,
    Hand,
    /// A commenting tool (Add comments).
    Comment(comments::CommentTool),
    /// Crop pages by dragging a rectangle.
    Crop,
    /// A Fill & Sign tool.
    Fill(fill_sign::FillTool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialog {
    Properties(PropsTab),
    About,
    Shortcuts,
    Split,
    /// Pages ▸ Number pages… (page labels).
    NumberPages,
    /// Protect Using Password.
    Protect,
    /// Set Page Boxes (crop, trim, bleed, art, media).
    PageBoxes,
    /// Add / Update Header and Footer, Watermark, Background.
    Marks(printcraft_engine::MarkKind),
    /// Export a PDF ▸ Image / Text.
    Export(export_ui::ExportKind),
    /// Fill & Sign ▸ Create signature (the drawing pad).
    Signature,
    /// Documents from a session that ended unexpectedly.
    Recovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropsTab {
    Description,
    Security,
    Fonts,
    Advanced,
}

/// Files delivered asynchronously: (name, bytes).
pub type Inbox = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<u8>)>>>;

pub struct PasswordPrompt {
    pub name: String,
    pub path: Option<String>,
    pub bytes: std::sync::Arc<Vec<u8>>,
    pub input: String,
    pub error: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct RecentFile {
    pub name: String,
    pub path: String,
    pub pages: usize,
    pub size: usize,
}

pub struct PrintCraftApp {
    pub session: Session,
    pub views: Vec<DocView>,
    /// `None` shows the Home tab.
    pub active: Option<usize>,
    pub mode: Mode,
    pub left: LeftPanel,
    pub left_open: bool,
    pub right: Option<RightPanel>,
    pub quick_tool: QuickTool,
    /// Comment author, per-tool colours and widths, pin.
    pub comment_prefs: comments::CommentPrefs,
    pub theme: ThemeKind,
    pub dialog: Option<Dialog>,
    pub palette_open: bool,
    pub palette_query: String,
    pub all_tools_expanded: bool,
    pub recent: Vec<RecentFile>,
    pub toast: Option<(String, f64)>,
    /// Whether the macOS title bar is drawn by us (traffic lights over our tab strip).
    pub integrated_titlebar: bool,
    pub password_prompt: Option<PasswordPrompt>,
    pub full_screen: bool,
    /// Files delivered asynchronously (web drag-and-drop, web file picker).
    pub inbox: Inbox,
    /// A pending "save changes?" question (closing a dirty tab or quitting).
    pub close_request: Option<CloseRequest>,
    /// Save to this path instead of asking (tests and automation).
    pub save_override: Option<String>,
    /// Document Properties ▸ Description fields being edited: (document, Title/Author/Subject/Keywords).
    pub props_draft: Option<(DocId, [String; 4])>,
    /// Files picked asynchronously for combine / insert (web).
    pub requests: files::Requests,
    /// Write exported files (split) here instead of asking (tests and automation).
    pub export_dir_override: Option<String>,
    /// Split dialog settings.
    pub split_draft: SplitDraft,
    /// Where autosaves go (`None`: autosave off, e.g. on the web and in tests).
    pub recovery: Option<RecoveryStore>,
    /// Entries left by a previous session, offered in the Recovery dialog.
    pub recoverable: Vec<RecoveryMeta>,
    recovery_keys: std::collections::HashMap<DocId, String>,
    last_autosave: f64,
    pending_recovered: Option<RecoveryMeta>,
    allow_quit: bool,
    /// The egui context, for commands that change window or theme state.
    ctx: Option<egui::Context>,
    pending_theme: Option<ThemeKind>,
    styled: bool,
    fonts_ready: bool,
    /// The UI control channel, when enabled (`--control`; off by default).
    control: Option<control::Control>,
    /// A bookmark being renamed in the Bookmarks panel: (path, text so far).
    pub bookmark_rename: Option<(Vec<usize>, String)>,
    /// Number pages dialog settings (1-based pages).
    pub number_draft: NumberDraft,
    /// Protect Using Password dialog state.
    pub protect_draft: protect::ProtectDraft,
    /// Set Page Boxes dialog state.
    pub boxes_draft: pageboxes::BoxesDraft,
    /// Header & footer / watermark / background dialog state.
    pub marks_draft: marks_ui::MarksDraft,
    /// Export dialog settings.
    pub export_draft: export_ui::ExportDraft,
    /// A running export's progress.
    export_status: Option<export_ui::ExportStatus>,
    /// The saved Fill & Sign signature (strokes normalised to the pad width, y up).
    pub signature: Option<Vec<Vec<[f32; 2]>>>,
    /// Strokes being drawn in the signature pad.
    pub signature_draft: Vec<Vec<[f32; 2]>>,
    /// The last web link the app asked the system to open (tests and automation).
    pub last_opened_url: Option<String>,
}

/// Settings for the Number pages dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct NumberDraft {
    pub from: usize,
    pub to: usize,
    pub style: printcraft_engine::LabelStyle,
    pub prefix: String,
    pub start: u32,
}

impl Default for PrintCraftApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PrintCraftApp {
    pub fn new() -> Self {
        Self {
            session: Session::new(),
            views: Vec::new(),
            active: None,
            mode: Mode::AllTools,
            left: LeftPanel::AllTools,
            left_open: true,
            right: None,
            quick_tool: QuickTool::Select,
            comment_prefs: Default::default(),
            theme: ThemeKind::Light,
            dialog: None,
            palette_open: false,
            palette_query: String::new(),
            all_tools_expanded: false,
            recent: Vec::new(),
            toast: None,
            integrated_titlebar: false,
            password_prompt: None,
            full_screen: false,
            inbox: Default::default(),
            close_request: None,
            save_override: None,
            props_draft: None,
            requests: Default::default(),
            export_dir_override: None,
            split_draft: SplitDraft { every: 1, at_selection: false },
            recovery: None,
            recoverable: Vec::new(),
            recovery_keys: Default::default(),
            last_autosave: 0.0,
            pending_recovered: None,
            allow_quit: false,
            ctx: None,
            pending_theme: None,
            styled: false,
            fonts_ready: false,
            control: None,
            bookmark_rename: None,
            last_opened_url: None,
            protect_draft: Default::default(),
            boxes_draft: Default::default(),
            marks_draft: Default::default(),
            export_draft: Default::default(),
            export_status: None,
            signature: None,
            signature_draft: Vec::new(),
            number_draft: NumberDraft { from: 1, to: 1, style: printcraft_engine::LabelStyle::Decimal, prefix: String::new(), start: 1 },
        }
    }

    /// Open a document and make it the active tab. Encrypted files raise the password prompt.
    pub fn open_bytes(&mut self, name: &str, path: Option<String>, bytes: Vec<u8>) -> Result<(), String> {
        self.try_open(name, path, std::sync::Arc::new(bytes), None)
    }

    fn try_open(&mut self, name: &str, path: Option<String>, bytes: std::sync::Arc<Vec<u8>>, password: Option<&str>) -> Result<(), String> {
        use printcraft_render::OpenError;
        let size = bytes.len();
        let id = match self.session.open(name, path.clone(), bytes.clone(), password) {
            Ok(id) => id,
            Err(e @ (OpenError::NeedsPassword | OpenError::WrongPassword)) => {
                let error = matches!(e, OpenError::WrongPassword).then(|| "Incorrect password. Try again.".to_string());
                self.password_prompt = Some(PasswordPrompt { name: name.to_string(), path, bytes, input: String::new(), error });
                return Ok(());
            }
            Err(e) => return Err(e.to_string()),
        };
        self.password_prompt = None;
        let doc = self.session.get(id).expect("just opened");
        let pages = doc.info.pages.len();
        // Acrobat opens straight to the Comments panel when a document has comments.
        if self.right.is_none() {
            self.right = if !doc.info.annotations.is_empty() {
                Some(RightPanel::Comments)
            } else if !doc.info.outline.is_empty() {
                Some(RightPanel::Bookmarks)
            } else {
                None
            };
        }
        self.views.push(DocView::new(id, &doc.info));
        self.active = Some(self.views.len() - 1);
        if let Some(p) = path {
            self.recent.retain(|r| r.path != p);
            self.recent.insert(0, RecentFile { name: name.to_string(), path: p, pages, size });
            self.recent.truncate(12);
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open_dropped(&mut self, f: egui::DroppedFileHandle, _ctx: &egui::Context) {
        let p = f.path().to_string_lossy().into_owned();
        if !p.is_empty() && f.path().is_absolute() {
            self.open_path(&p);
            return;
        }
        let name = f.path().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "dropped.pdf".into());
        match f.bytes() {
            Ok(bytes) => {
                if let Err(e) = self.open_bytes(&name, None, bytes) {
                    self.notify(format!("Couldn't open {name}: {e}"));
                }
            }
            Err(e) => self.notify(format!("Couldn't read {name}: {e}")),
        }
    }

    /// Browsers read dropped files asynchronously; the bytes land in `inbox` and open next frame.
    #[cfg(target_arch = "wasm32")]
    fn open_dropped(&mut self, f: egui::DroppedFileHandle, ctx: &egui::Context) {
        let inbox = self.inbox.clone();
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let name = f.path().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "dropped.pdf".into());
            if let Ok(bytes) = f.bytes_async().await
                && let Ok(mut q) = inbox.lock()
            {
                q.push((name, bytes));
                ctx.request_repaint();
            }
        });
    }

    /// Save an attachment to disk, or open a PDF attachment in a new tab.
    pub fn attachment_action(&mut self, doc: DocId, index: usize, open: bool) {
        let Some(d) = self.session.get(doc) else { return };
        let Some(att) = d.info.attachments.get(index).cloned() else { return };
        let data = printcraft_render::attachment_data(&d.bytes, d.password.as_deref(), &att);
        match (data, open) {
            (Err(e), _) => self.notify(format!("Couldn't read {}: {e}", att.name)),
            (Ok(bytes), true) => {
                if let Err(e) = self.open_bytes(&att.name, None, bytes) {
                    self.notify(format!("Couldn't open {}: {e}", att.name));
                }
            }
            (Ok(bytes), false) => {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(path) = rfd::FileDialog::new().set_file_name(&att.name).save_file() {
                    match std::fs::write(&path, &bytes) {
                        Ok(()) => self.notify(format!("Saved {}", path.display())),
                        Err(e) => self.notify(format!("Couldn't save: {e}")),
                    }
                }
                #[cfg(target_arch = "wasm32")]
                self.notify(format!("Downloading attachments on the web arrives with M3.10 ({} bytes ready)", bytes.len()));
            }
        }
    }

    /// Enter or leave full-screen reading (Acrobat: View ▸ Full Screen Mode, ⌘L).
    pub fn set_full_screen(&mut self, ctx: &egui::Context, on: bool) {
        self.full_screen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    /// Answer the password prompt (`None` cancels).
    pub fn submit_password(&mut self, password: Option<String>) {
        let Some(p) = self.password_prompt.take() else { return };
        let Some(pw) = password else { return };
        match self.try_open(&p.name, p.path, p.bytes, Some(&pw)) {
            Err(e) => self.notify(format!("Couldn't open {}: {e}", p.name)),
            // A recovered encrypted document is open once the prompt is gone.
            Ok(()) if self.password_prompt.is_none() => {
                if let Some(meta) = self.pending_recovered.clone() {
                    self.finish_recovery(&meta);
                }
            }
            Ok(()) => {}
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_path(&mut self, path: &str) {
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string());
        match std::fs::read(path) {
            Ok(bytes) => {
                if let Err(e) = self.open_bytes(&name, Some(path.to_string()), bytes) {
                    self.notify(format!("Couldn't open {name}: {e}"));
                }
            }
            Err(e) => self.notify(format!("Couldn't read {name}: {e}")),
        }
    }

    pub fn open_dialog(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(p) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_file() {
            self.open_path(&p.to_string_lossy());
        }
        // Browsers pick files asynchronously; the bytes arrive through `inbox`.
        #[cfg(target_arch = "wasm32")]
        {
            let inbox = self.inbox.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Some(h) = rfd::AsyncFileDialog::new().add_filter("PDF", &["pdf"]).pick_file().await {
                    let bytes = h.read().await;
                    if let Ok(mut q) = inbox.lock() {
                        q.push((h.file_name(), bytes));
                    }
                }
            });
        }
    }

    pub fn close_tab(&mut self, index: usize) {
        if index >= self.views.len() {
            return;
        }
        let id = self.views.remove(index).id;
        self.forget_recovery(id);
        self.session.close(id);
        self.active = match self.active {
            _ if self.views.is_empty() => None,
            Some(a) if a >= self.views.len() => Some(self.views.len() - 1),
            other => other,
        };
    }

    pub fn active_ids(&self) -> Option<(usize, DocId)> {
        self.active.and_then(|i| self.views.get(i).map(|v| (i, v.id)))
    }

    /// Enable the UI control channel on `ctx` (opt-in; see [`control`]). Returns a client that
    /// sends requests to this app; [`control::serve`] exposes it on loopback.
    pub fn attach_control(&mut self, ctx: &egui::Context) -> control::ControlClient {
        let (control, client) = control::attach(ctx);
        self.control = Some(control);
        client
    }

    /// Open a web link in the system browser (a new tab on the web).
    pub fn open_url(&mut self, url: &str) {
        if let Some(ctx) = &self.ctx {
            ctx.open_url(egui::OpenUrl::new_tab(url));
        }
        self.last_opened_url = Some(url.to_string());
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), 0.0));
    }

    pub fn set_theme(&mut self, ctx: &egui::Context, kind: ThemeKind) {
        self.theme = kind;
        theme::apply(ctx, kind);
    }

    /// Run a catalogue command. Commands that aren't implemented yet say which milestone ships them.
    pub fn run_command(&mut self, command: &str) {
        if printcraft_engine::commands::command(command).is_some() {
            self.execute(command);
            return;
        }
        // Not implemented yet: say which milestone ships it.
        let when = printcraft_engine::catalog::TOOL_GROUPS
            .iter()
            .flat_map(|g| g.sections.iter().flat_map(|s| s.items.iter()))
            .find(|i| i.command == command)
            .map(|i| match i.availability {
                printcraft_engine::catalog::Availability::Planned(m) => format!("ships in milestone {m}"),
                printcraft_engine::catalog::Availability::Provider => "needs an AI provider (off by default)".to_string(),
                printcraft_engine::catalog::Availability::Ready => "is available".to_string(),
            })
            .unwrap_or_else(|| "is not available yet".into());
        self.notify(format!("`{command}` {when}"));
    }

    /// Serialize the user's persistent state (recent files, theme). Local only.
    pub fn persist(&self) -> String {
        serde_json::json!({ "recent": self.recent, "theme": self.theme, "signature": self.signature }).to_string()
    }

    /// Restore state written by `persist`. Unknown or malformed data is ignored.
    pub fn restore(&mut self, json: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return };
        if let Ok(r) = serde_json::from_value::<Vec<RecentFile>>(v["recent"].clone()) {
            // Only keep entries whose files still exist.
            #[cfg(not(target_arch = "wasm32"))]
            let r: Vec<RecentFile> = r.into_iter().filter(|f| std::path::Path::new(&f.path).exists()).collect();
            self.recent = r;
        }
        if let Ok(t) = serde_json::from_value::<ThemeKind>(v["theme"].clone()) {
            self.theme = t;
        }
        if let Ok(s) = serde_json::from_value::<Vec<Vec<[f32; 2]>>>(v["signature"].clone())
            && s.iter().all(|st| st.iter().all(|p| p.iter().all(|x| x.is_finite())))
        {
            self.signature = Some(s);
        }
    }

    /// `true` while any open document still waits for page renders (used by headless capture).
    pub fn render_pending(&self) -> bool {
        self.views.iter().any(|v| v.render_pending())
    }

    /// Apply a named view option (`--page 3`, `--panel bookmarks`, `--theme dark`, …).
    ///
    /// This is the seed of the UI control channel (M3.9): the same verbs become `ui.set` calls.
    pub fn set_option(&mut self, key: &str, value: &str) -> Result<(), String> {
        let view = self.active.and_then(|i| self.views.get_mut(i));
        match (key, view) {
            ("theme", _) => self.pending_theme = Some(if value == "dark" { ThemeKind::Dark } else { ThemeKind::Light }),
            ("panel", _) => {
                self.right = match value {
                    "comments" => Some(RightPanel::Comments),
                    "bookmarks" => Some(RightPanel::Bookmarks),
                    "pages" => Some(RightPanel::Pages),
                    "fields" => Some(RightPanel::Fields),
                    "layers" => Some(RightPanel::Layers),
                    "attachments" => Some(RightPanel::Attachments),
                    "none" => None,
                    other => return Err(format!("unknown panel {other}")),
                }
            }
            ("mode", _) => {
                self.mode = match value {
                    "read" => Mode::Read,
                    "edit" => Mode::Edit,
                    "convert" => Mode::Convert,
                    "sign" => Mode::Sign,
                    _ => Mode::AllTools,
                }
            }
            ("tool", _) => {
                let g = printcraft_engine::catalog::group(value).ok_or_else(|| format!("unknown tool {value}"))?;
                self.left = LeftPanel::Tool(g.id);
                self.left_open = true;
            }
            ("left", _) => self.left_open = value != "closed",
            ("home", _) => self.active = None,
            ("dialog", _) => {
                self.dialog = match value {
                    "properties" => Some(Dialog::Properties(PropsTab::Description)),
                    "security" => Some(Dialog::Properties(PropsTab::Security)),
                    "fonts" => Some(Dialog::Properties(PropsTab::Fonts)),
                    "advanced" => Some(Dialog::Properties(PropsTab::Advanced)),
                    "shortcuts" => Some(Dialog::Shortcuts),
                    "split" => Some(Dialog::Split),
                    "protect" => Some(Dialog::Protect),
                    "page-boxes" => Some(Dialog::PageBoxes),
                    "header-footer" => Some(Dialog::Marks(printcraft_engine::MarkKind::HeaderFooter)),
                    "watermark" => Some(Dialog::Marks(printcraft_engine::MarkKind::Watermark)),
                    "background" => Some(Dialog::Marks(printcraft_engine::MarkKind::Background)),
                    "export-image" => Some(Dialog::Export(export_ui::ExportKind::Image)),
                    "export-text" => Some(Dialog::Export(export_ui::ExportKind::Text)),
                    "signature" => Some(Dialog::Signature),
                    "number-pages" => {
                        // Same path as the menu, so the page range is seeded.
                        self.execute("page.number");
                        Some(Dialog::NumberPages)
                    }
                    "none" => None,
                    _ => Some(Dialog::About),
                }
            }
            ("tools", _) => self.all_tools_expanded = value != "collapsed",
            ("palette", _) => {
                self.palette_open = true;
                self.palette_query = value.to_string();
            }
            ("page", Some(v)) => v.go_to_page(value.parse::<usize>().map_err(|e| e.to_string())?.saturating_sub(1)),
            ("zoom", Some(v)) => v.set_zoom(value.trim_end_matches('%').parse::<f32>().map_err(|e| e.to_string())? / 100.0),
            ("layout", Some(v)) => {
                v.layout = match value {
                    "two-up" => canvas::PageLayout::TwoUp,
                    "single" => canvas::PageLayout::Single,
                    _ => canvas::PageLayout::Continuous,
                }
            }
            ("organize", Some(v)) => v.organize = value != "off",
            ("rotate", Some(v)) => {
                let deg: u16 = value.parse().map_err(|_| "rotate: 0, 90, 180 or 270")?;
                if !deg.is_multiple_of(90) {
                    return Err("rotate: 0, 90, 180 or 270".into());
                }
                v.rotation = deg % 360;
            }
            ("layer", _) => {
                // `--layer "Name=off"` / `"Name=on"`
                let (name, state) = value.rsplit_once('=').ok_or("expected NAME=on|off")?;
                let (i, id) = self.active_ids().ok_or("`layer` needs an open document")?;
                let idx = self
                    .session
                    .get(id)
                    .and_then(|d| d.info.layers.iter().position(|l| l.name == name))
                    .ok_or_else(|| format!("no layer named {name}"))?;
                if self.session.set_layer_visible(id, idx, state != "off") {
                    self.views[i].invalidate_content();
                }
            }
            ("find", Some(v)) => {
                v.open_find();
                if let Some(f) = v.find.as_mut() {
                    f.query = value.to_string();
                }
                v.rerun_find();
            }
            ("fields", Some(v)) => v.highlight_fields = value != "off",
            ("select", Some(v)) => {
                // `--select 2,3,5` (1-based) selects pages in the organize grid.
                let pages: Result<Vec<usize>, _> = value.split(',').map(|p| p.trim().parse::<usize>().map(|n| n.saturating_sub(1))).collect();
                v.select_pages(&pages.map_err(|_| "select: comma-separated page numbers")?);
            }
            ("notice", Some(v)) => v.notice_dismissed = value == "off",
            ("quick", _) => {
                // `--quick select|hand|note|freetext|highlight|underline|strikeout|ink|line|arrow|square|circle`
                self.quick_tool = match value {
                    "select" => QuickTool::Select,
                    "hand" => QuickTool::Hand,
                    "crop" => QuickTool::Crop,
                    fill if fill.starts_with("fill-") => QuickTool::Fill(
                        fill_sign::FillTool::from_command(&format!("sign.fill.{}", &fill[5..])).ok_or_else(|| format!("unknown tool {fill}"))?,
                    ),
                    other => {
                        let t = comments::CommentTool::from_command(&format!("comment.{other}")).ok_or_else(|| format!("unknown tool {other}"))?;
                        self.comment_prefs.group_tool[t.group()] = t;
                        QuickTool::Comment(t)
                    }
                };
            }
            ("author", _) => self.comment_prefs.author = value.to_string(),
            ("comment", Some(v)) => {
                // `--comment 2:4` selects the 4th annotation of page 2 (1-based, as comment_list reports).
                let (p, i) = value.split_once(':').ok_or("comment: PAGE:INDEX")?;
                let (p, i): (usize, usize) =
                    (p.trim().parse().map_err(|_| "comment: PAGE:INDEX")?, i.trim().parse().map_err(|_| "comment: PAGE:INDEX")?);
                v.comments.selected = Some((p.saturating_sub(1), i.saturating_sub(1)));
                v.comments.reveal = true;
            }
            (k, None) if ["page", "zoom", "layout", "organize", "fields", "find", "rotate", "select", "notice", "comment"].contains(&k) => {
                return Err(format!("`{k}` needs an open document"));
            }
            (other, _) => return Err(format!("unknown option {other}")),
        }
        Ok(())
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Key;
        self.registry_shortcuts(ctx);
        if self.full_screen && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.set_full_screen(ctx, false);
        }
        if let Some(i) = self.active {
            canvas::shortcuts(&mut self.views[i], ctx);
        }
    }
}

impl eframe::App for PrintCraftApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("printcraft", self.persist());
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ctx = Some(ctx.clone());
        if !self.styled {
            egui_extras::install_image_loaders(ctx);
            theme::install_fonts(ctx);
            theme::apply(ctx, self.theme);
            self.styled = true;
        } else {
            self.fonts_ready = true;
        }
        if let Some(k) = self.pending_theme.take() {
            self.set_theme(ctx, k);
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            self.open_dropped(f, ctx);
        }
        let arrived: Vec<(String, Vec<u8>)> = self.inbox.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for (name, bytes) in arrived {
            if let Err(e) = self.open_bytes(&name, None, bytes) {
                self.notify(format!("Couldn't open {name}: {e}"));
            }
        }
        if let Some(mut control) = self.control.take() {
            control.tick(ctx, self);
            self.control = Some(control);
        }
        self.guard_quit(ctx);
        let now = ctx.input(|i| i.time);
        self.autosave_tick(now);
        self.shortcuts(ctx);
        self.process_pending_edits();
        self.poll_export();
        self.process_file_requests();
        // Pull finished renders into textures for every open document.
        for view in &mut self.views {
            if let Some(doc) = self.session.get(view.id) {
                view.receive(ctx, &doc.renderer);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Fonts registered via set_fonts only take effect next frame; named families would panic now.
        if !self.fonts_ready {
            ctx.request_repaint();
            return;
        }
        if self.full_screen && self.active.is_some() {
            // Full screen: the page, nothing else (Esc or ⌘L to leave).
            let t = theme::Tokens::get(&ctx);
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(if t.dark() { t.pasteboard } else { egui::Color32::from_gray(32) })).show(
                ui,
                |ui| {
                    if let Some(i) = self.active {
                        canvas::document_area(self, i, ui);
                    }
                },
            );
            dialogs::show(self, &ctx);
            return;
        }
        chrome::tab_strip(self, ui);
        chrome::mode_bar(self, ui);
        if self.active.is_some() {
            chrome::right_rail(self, ui);
            if self.right.is_some() && self.mode != Mode::Read {
                panels::right_panel(self, ui);
            }
        }
        if self.left_open && self.mode != Mode::Read {
            panels::left_panel(self, ui);
        }
        let t = theme::Tokens::get(&ctx);
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.pasteboard)).show(ui, |ui| match self.active {
            None => home::show(self, ui),
            Some(i) => canvas::document_area(self, i, ui),
        });
        self.process_pending_edits();
        palette::show(self, &ctx);
        dialogs::show(self, &ctx);
        widgets::toast(self, &ctx);
    }
}
