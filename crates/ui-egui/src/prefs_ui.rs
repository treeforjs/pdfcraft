//! Preferences (⌘,): categories on the left, the chosen page on the right, and a search box that
//! filters every setting across pages. Each row writes a persisted setting that the app already
//! reads: the app's own fields (theme, language, workspace, page display, comment tools) or the
//! session's preferences ([`pdfcraft_engine::prefs`]: autosave, recent files, identity, field
//! highlights, signing defaults, security, accessibility and colour management).
//!
//! The same keys are reachable without the window: [`PdfCraftApp::pref`] and
//! [`PdfCraftApp::set_pref`] back `ui.set pref.<key>` on the control channel, and `ui.state`
//! lists them under `prefs`. Values are validated and clamped there, never trusted.
//!
//! A new page is a [`Page`] variant (with its id, label, icon and rows); a new row is a key in
//! its page's [`Page::rows`], a label in [`label`] and a widget in [`row`].

use std::collections::HashMap;

use egui::{Align, Color32, CornerRadius, Layout, Sense, vec2};
use pdfcraft_engine::prefs::{self as engine_prefs, MAX_AUTOSAVE_MINUTES, MAX_RECENT_FILES, MAX_TEXT_CHARS, MAX_TEXT_SCALE, MIN_TEXT_SCALE};
use serde_json::{Map, Value, json};

use crate::theme::{self, ThemePreference, Tokens};
use crate::{Mode, PdfCraftApp, icons, widgets};

/// A Preferences page. Its id is the engine's category name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    General,
    Documents,
    Commenting,
    Forms,
    Identity,
    Signatures,
    Security,
    Accessibility,
    Color,
}

impl Page {
    pub const ALL: [Page; 9] = [
        Page::General,
        Page::Documents,
        Page::Commenting,
        Page::Forms,
        Page::Identity,
        Page::Signatures,
        Page::Security,
        Page::Accessibility,
        Page::Color,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Page::General => "general",
            Page::Documents => "documents",
            Page::Commenting => "commenting",
            Page::Forms => "forms",
            Page::Identity => "identity",
            Page::Signatures => "signatures",
            Page::Security => "security",
            Page::Accessibility => "accessibility",
            Page::Color => "color",
        }
    }

    pub fn from_id(id: &str) -> Option<Page> {
        Page::ALL.into_iter().find(|p| p.id().eq_ignore_ascii_case(id.trim()))
    }

    fn label(self) -> &'static str {
        match self {
            Page::General => tl!("General"),
            Page::Documents => tl!("Documents"),
            Page::Commenting => tl!("Commenting"),
            Page::Forms => tl!("Forms"),
            Page::Identity => tl!("Identity"),
            Page::Signatures => tl!("Signatures"),
            Page::Security => tl!("Security"),
            Page::Accessibility => tl!("Accessibility"),
            Page::Color => tl!("Color management"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::General => "settings",
            Page::Documents => "file-text",
            Page::Commenting => "message-square",
            Page::Forms => "text-cursor-input",
            Page::Identity => "user-round",
            Page::Signatures => "signature",
            Page::Security => "shield-check",
            Page::Accessibility => "accessibility",
            Page::Color => "palette",
        }
    }

    /// The keys shown on this page, in order.
    pub fn rows(self) -> &'static [&'static str] {
        match self {
            Page::General => &["general.language", "general.theme", "general.default_mode", "general.reopen_last_session", "general.check_updates"],
            Page::Documents => &["documents.autosave_minutes", "documents.recent_files", "documents.default_layout", "documents.default_zoom"],
            Page::Commenting => &["commenting.keep_tool_selected", "commenting.open_panel"],
            Page::Forms => {
                &["forms.highlight_fields", "forms.highlight_color", "forms.required_color", "forms.flatten_fill_sign", "forms.date_format"]
            }
            Page::Identity => &["identity.name", "identity.organization", "identity.unit", "identity.email"],
            Page::Signatures => &["signatures.reason", "signatures.location"],
            Page::Security => &["security.javascript", "security.internet_access", "security.unlisted_sites"],
            Page::Accessibility => &["accessibility.text_scale", "accessibility.reduce_motion", "accessibility.high_contrast"],
            Page::Color => &[
                "color.rgb",
                "color.cmyk",
                "color.gray",
                "color.intent",
                "color.black_point_compensation",
                "color.untagged",
                "color.overprint_preview",
            ],
        }
    }
}

/// The window's state: the page shown, the search text, and text boxes being typed in (they
/// keep what was typed, spaces included, while the setting takes the cleaned value).
#[derive(Clone, Debug, Default)]
pub struct PrefsUi {
    pub page: Page,
    pub query: String,
    drafts: HashMap<&'static str, String>,
}

/// The keys the app holds itself (the rest are the session's, [`engine_prefs::SCHEMA`]).
pub const APP_KEYS: [&str; 12] = [
    "general.language",
    "general.theme",
    "general.default_mode",
    "general.reopen_last_session",
    "general.check_updates",
    "documents.default_layout",
    "documents.default_zoom",
    "commenting.keep_tool_selected",
    "commenting.open_panel",
    "forms.highlight_fields",
    "forms.flatten_fill_sign",
    // Shown with the session's identity keys; the app keeps it as the comment author.
    "identity.name",
];

/// A row's label in the interface language (also what the search box matches).
fn label(key: &str) -> &'static str {
    match key {
        "general.language" => tl!("Interface language"),
        "general.theme" => tl!("Theme"),
        "general.default_mode" => tl!("Default workspace mode"),
        "general.reopen_last_session" => tl!("Reopen the files that were open when PdfCraft last closed"),
        "general.check_updates" => tl!("Check for updates automatically"),
        "documents.autosave_minutes" => tl!("Save recovery copies of unsaved changes every"),
        "documents.recent_files" => tl!("Recent files to remember"),
        "documents.default_layout" => tl!("Default page display"),
        "documents.default_zoom" => tl!("Default zoom"),
        "commenting.keep_tool_selected" => tl!("Keep the comment tool selected after adding a comment"),
        "commenting.open_panel" => tl!("Open the Comments panel when a comment tool is chosen"),
        "forms.highlight_fields" => tl!("Highlight form fields in documents that open"),
        "forms.highlight_color" => tl!("Field highlight color"),
        "forms.required_color" => tl!("Required field border color"),
        "forms.flatten_fill_sign" => tl!("Flatten Fill & Sign when saving"),
        "forms.date_format" => tl!("Date format"),
        "identity.name" => tl!("Name on new comments"),
        "identity.organization" => tl!("Organization Name"),
        "identity.unit" => tl!("Organizational Unit"),
        "identity.email" => tl!("Email Address"),
        "signatures.reason" => tl!("Default reason"),
        "signatures.location" => tl!("Default location"),
        "security.javascript" => tl!("Enable Acrobat JavaScript"),
        "security.internet_access" => tl!("Internet access from PDFs"),
        "security.unlisted_sites" => tl!("Sites without a rule"),
        "accessibility.text_scale" => tl!("Text size"),
        "accessibility.reduce_motion" => tl!("Reduce motion"),
        "accessibility.high_contrast" => tl!("High contrast"),
        "color.rgb" => tl!("RGB working space"),
        "color.cmyk" => tl!("CMYK working space"),
        "color.gray" => tl!("Gray working space"),
        "color.intent" => tl!("Rendering intent"),
        "color.black_point_compensation" => tl!("Black point compensation"),
        "color.untagged" => tl!("Untagged color"),
        "color.overprint_preview" => tl!("Use overprint preview"),
        _ => "",
    }
}

/// English words the search also matches, so a setting is found by its usual names in any
/// interface language.
fn keywords(key: &str) -> &'static str {
    match key {
        "general.language" => "language locale translation",
        "general.theme" => "theme appearance dark light mode colors",
        "general.default_mode" => "workspace mode start",
        "general.reopen_last_session" => "reopen restore session startup",
        "general.check_updates" => "updates update version release automatic check",
        "documents.autosave_minutes" => "autosave auto-save recovery backup interval minutes crash",
        "documents.recent_files" => "recent files history list count",
        "documents.default_layout" => "page display layout continuous single two-up",
        "documents.default_zoom" => "zoom magnification fit width page",
        "commenting.keep_tool_selected" => "comment tool pin sticky",
        "commenting.open_panel" => "comments panel list sidebar",
        "forms.highlight_fields" => "form fields highlight",
        "forms.highlight_color" => "form field highlight color colour",
        "forms.required_color" => "form required field color colour border",
        "forms.flatten_fill_sign" => "flatten fill sign save",
        "forms.date_format" => "date format fill sign pattern",
        "identity.name" => "identity name author login",
        "identity.organization" => "identity organization company corporation",
        "identity.unit" => "identity department unit",
        "identity.email" => "identity email address",
        "signatures.reason" => "signature signing reason",
        "signatures.location" => "signature signing location place",
        "security.javascript" => "javascript scripts security",
        "security.internet_access" => "internet web urls links trust security network",
        "security.unlisted_sites" => "internet web sites hosts trust ask",
        "accessibility.text_scale" => "accessibility text size zoom scale larger font",
        "accessibility.reduce_motion" => "accessibility motion animation smooth scrolling",
        "accessibility.high_contrast" => "accessibility contrast borders visibility",
        "color.rgb" => "color colour rgb profile working space icc",
        "color.cmyk" => "color colour cmyk profile working space icc print",
        "color.gray" => "color colour gray grey profile working space icc",
        "color.intent" => "color colour rendering intent perceptual colorimetric",
        "color.black_point_compensation" => "color colour black point compensation bpc",
        "color.untagged" => "color colour untagged device output intent",
        "color.overprint_preview" => "overprint preview print pdfx separations",
        _ => "",
    }
}

/// Whether row `key` on `page` matches the (lower-case) search text.
fn matches(page: Page, key: &str, q: &str) -> bool {
    q.is_empty()
        || label(key).to_lowercase().contains(q)
        || keywords(key).contains(q)
        || key.contains(q)
        || page.label().to_lowercase().contains(q)
        || page.id().contains(q)
}

/// The rows the search finds, by page (only pages with a match).
pub fn search(query: &str) -> Vec<(Page, Vec<&'static str>)> {
    let q = query.trim().to_lowercase();
    Page::ALL
        .into_iter()
        .filter_map(|p| {
            let rows: Vec<&'static str> = p.rows().iter().copied().filter(|k| matches(p, k, &q)).collect();
            (!rows.is_empty()).then_some((p, rows))
        })
        .collect()
}

/// `true`/`false`, or `"on"`/`"off"`/`"true"`/`"false"`.
fn as_bool(key: &str, v: &Value) -> Result<bool, String> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Ok(true),
            "false" | "off" | "no" | "0" => Ok(false),
            _ => Err(format!("{key} must be true or false")),
        },
        _ => Err(format!("{key} must be true or false")),
    }
}

fn as_text<'a>(key: &str, v: &'a Value) -> Result<&'a str, String> {
    v.as_str().ok_or_else(|| format!("{key} must be text"))
}

impl PdfCraftApp {
    /// The value of preference `key`: the app's own keys ([`APP_KEYS`]) or the session's.
    pub fn pref(&self, key: &str) -> Option<Value> {
        Some(match key {
            "general.language" => json!(self.language),
            "general.theme" => json!(self.theme_preference.id()),
            "general.default_mode" => serde_json::to_value(self.default_mode).ok()?,
            "general.reopen_last_session" => json!(self.reopen_last_session),
            "general.check_updates" => json!(self.updates.auto.enabled),
            "documents.default_layout" => json!(self.view_defaults.layout.as_str()),
            "documents.default_zoom" => json!(self.view_defaults.zoom_name()),
            "commenting.keep_tool_selected" => json!(self.comment_prefs.pinned),
            "commenting.open_panel" => json!(!self.comments_panel_closed),
            "forms.highlight_fields" => json!(self.view_defaults.highlight_fields),
            "forms.flatten_fill_sign" => json!(self.flatten_fill_sign_on_save),
            "identity.name" => json!(self.comment_prefs.author),
            other => return self.session.pref(other),
        })
    }

    /// Every preference by key (`ui.state` ▸ `prefs`).
    pub fn prefs_values(&self) -> Map<String, Value> {
        let mut out = self.session.prefs_json(None);
        for key in APP_KEYS {
            if let Some(v) = self.pref(key) {
                out.insert(key.to_string(), v);
            }
        }
        out
    }

    /// Set preference `key` (validated and clamped), applying it where the app reads it.
    /// Returns the value stored.
    pub fn set_pref(&mut self, key: &str, value: &Value) -> Result<Value, String> {
        match key {
            "general.language" => {
                let l = crate::i18n::normalize_pref(as_text(key, value)?).ok_or_else(|| format!("{key} must be auto or a language code"))?;
                self.language = l.to_string();
            }
            "general.theme" => {
                let p = ThemePreference::parse(as_text(key, value)?).ok_or_else(|| format!("{key} must be system, light, warm, dim or dark"))?;
                self.set_theme_preference(p);
            }
            "general.default_mode" => {
                self.default_mode = Mode::parse(as_text(key, value)?).ok_or_else(|| format!("{key} must be all, read, edit, convert or sign"))?;
            }
            "general.reopen_last_session" => self.reopen_last_session = as_bool(key, value)?,
            "general.check_updates" => self.updates.auto.enabled = as_bool(key, value)?,
            "documents.default_layout" => {
                self.view_defaults.layout = crate::canvas::PageLayout::try_parse(as_text(key, value)?)
                    .ok_or_else(|| format!("{key} must be continuous, single or two-up"))?;
            }
            "documents.default_zoom" => {
                let zoom = match value {
                    Value::Number(n) => format!("{n}%"),
                    other => as_text(key, other)?.to_string(),
                };
                self.view_defaults =
                    self.view_defaults.with_zoom(&zoom).ok_or_else(|| format!("{key} must be fit-width, fit-page or a percentage from 8 to 6400"))?;
            }
            "commenting.keep_tool_selected" => self.comment_prefs.pinned = as_bool(key, value)?,
            "commenting.open_panel" => self.comments_panel_closed = !as_bool(key, value)?,
            "forms.highlight_fields" => self.view_defaults.highlight_fields = as_bool(key, value)?,
            "forms.flatten_fill_sign" => self.flatten_fill_sign_on_save = as_bool(key, value)?,
            "identity.name" => {
                let stored = self.session.set_pref(key, value)?;
                let name = stored.value.as_str().unwrap_or_default();
                // An empty name goes back to the login name, as in a fresh install.
                self.comment_prefs.author = if name.is_empty() { crate::comments::CommentPrefs::default().author } else { name.to_string() };
            }
            _ => {
                self.session.set_pref(key, value)?;
                self.prefs_changed();
            }
        }
        self.pref(key).ok_or_else(|| format!("unknown preference {key:?}"))
    }

    /// A session preference changed: apply what the app derives from it now.
    pub(crate) fn prefs_changed(&mut self) {
        let keep = self.session.prefs().recent_files as usize;
        self.recent.truncate(keep);
    }

    /// The comment author mirrored into the session's identity, so automation and the app agree.
    pub(crate) fn sync_identity_name(&mut self) {
        let name = self.comment_prefs.author.clone();
        let _ = self.session.set_pref("identity.name", &json!(name));
    }
}

/// The Preferences window. Returns `true` to close.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    ui.set_width(780.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Preferences")).font(theme::semibold(18.0)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut app.prefs_ui.query)
                    .id(egui::Id::new("prefs-search"))
                    .hint_text(tl!("Search preferences"))
                    .desired_width(240.0)
                    .char_limit(100),
            );
        });
    });
    ui.add_space(6.0);
    let query = app.prefs_ui.query.trim().to_string();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(190.0);
            for page in Page::ALL {
                let selected = query.is_empty() && app.prefs_ui.page == page;
                if category(ui, t, page, selected).clicked() {
                    app.prefs_ui.page = page;
                    app.prefs_ui.query.clear();
                }
            }
        });
        ui.add_space(4.0);
        let (rect, _) = ui.allocate_exact_size(vec2(1.0, 460.0), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::ZERO, t.divider);
        ui.add_space(8.0);
        ui.vertical(|ui| {
            egui::ScrollArea::vertical().id_salt("prefs-page").max_height(460.0).auto_shrink([false, false]).show(ui, |ui| {
                ui.set_width(ui.available_width() - 8.0);
                if query.is_empty() {
                    let page = app.prefs_ui.page;
                    ui.label(egui::RichText::new(page.label()).font(theme::semibold(15.0)));
                    ui.add_space(4.0);
                    for key in page.rows() {
                        row(ui, app, t, key);
                    }
                } else {
                    let found = search(&query);
                    if found.is_empty() {
                        ui.label(
                            egui::RichText::new(crate::i18n::fmt(tl!("No preferences match “{query}”."), &[("query", &query)])).color(t.text_muted),
                        );
                    }
                    for (page, keys) in found {
                        widgets::section_title(ui, page.label());
                        for key in keys {
                            row(ui, app, t, key);
                        }
                    }
                }
            });
        });
    });
    ui.add_space(8.0);
    let mut close = false;
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("OK"), true).clicked() {
                close = true;
            }
        });
    });
    close
}

/// One category in the list on the left.
fn category(ui: &mut egui::Ui, t: &Tokens, page: Page, selected: bool) -> egui::Response {
    let label = page.label();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label));
    let fill = if selected {
        t.selected
    } else if resp.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(t.radius), fill);
    let fg = if selected { t.accent_text } else { t.text };
    icons::paint(
        ui,
        egui::Rect::from_min_size(rect.min + vec2(10.0, 8.0), vec2(16.0, 16.0)),
        page.icon(),
        15.0,
        if selected { t.accent_text } else { t.icon },
    );
    widgets::row_text(ui, egui::pos2(rect.left() + 34.0, rect.center().y), label, theme::medium(13.0), fg, rect.width() - 40.0);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A setting: its label, then its control, then a hint in small muted text.
fn setting(ui: &mut egui::Ui, t: &Tokens, key: &str, hint: Option<&str>, add: impl FnOnce(&mut egui::Ui, egui::Id)) {
    ui.push_id(key, |ui| {
        ui.add_space(6.0);
        let l = ui.label(egui::RichText::new(label(key)).font(theme::medium(13.0)));
        ui.horizontal_wrapped(|ui| add(ui, l.id));
        if let Some(h) = hint {
            ui.label(egui::RichText::new(h).small().color(t.text_muted));
        }
        ui.add_space(4.0);
    });
}

/// A checkbox setting labelled with its row label.
fn check(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens, key: &str, hint: Option<&str>) {
    let mut on = app.pref(key).and_then(|v| v.as_bool()).unwrap_or(false);
    ui.push_id(key, |ui| {
        ui.add_space(4.0);
        if ui.checkbox(&mut on, label(key)).changed() {
            set(app, key, json!(on));
        }
        if let Some(h) = hint {
            ui.label(egui::RichText::new(h).small().color(t.text_muted));
        }
        ui.add_space(2.0);
    });
}

/// Set `key`, telling the user if the value was refused (the controls only offer valid values,
/// so this is for the rare engine refusal, such as a colour profile that can't be used).
fn set(app: &mut PdfCraftApp, key: &str, v: Value) {
    if let Err(e) = app.set_pref(key, &v) {
        app.notify(e);
    }
}

/// A text box for session text `key`.
fn text(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens, key: &'static str, hint: Option<&str>) {
    setting(ui, t, key, hint, |ui, label| {
        let current = app.pref(key).and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        let mut s = app.prefs_ui.drafts.get(key).cloned().unwrap_or(current);
        let edit = ui.add(egui::TextEdit::singleline(&mut s).desired_width(320.0).char_limit(MAX_TEXT_CHARS)).labelled_by(label);
        if edit.changed() {
            set(app, key, json!(s));
            app.prefs_ui.drafts.insert(key, s);
        } else if !edit.has_focus() {
            app.prefs_ui.drafts.remove(key);
        }
    });
}

/// A colour button for session colour `key`.
fn color(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens, key: &'static str, hint: Option<&str>) {
    setting(ui, t, key, hint, |ui, label| {
        let mut c = app.pref(key).and_then(|v| engine_prefs::parse_color(&v)).map_or([0, 0, 0], |(c, _)| c);
        let before = c;
        ui.color_edit_button_srgb(&mut c).labelled_by(label);
        ui.label(egui::RichText::new(engine_prefs::color_hex(c)).color(t.text_muted).monospace());
        if c != before {
            set(app, key, json!(engine_prefs::color_hex(c)));
        }
    });
}

/// A drop-down for session choice `key`: (id, label) pairs.
fn choice(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens, key: &'static str, options: &[(&str, &str)], hint: Option<&str>) {
    setting(ui, t, key, hint, |ui, label| {
        let current = app.pref(key).and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        let shown = options.iter().find(|(id, _)| *id == current).map_or(current.clone(), |(_, l)| l.to_string());
        let mut picked = None;
        egui::ComboBox::from_id_salt(key)
            .selected_text(shown)
            .width(320.0)
            .show_ui(ui, |ui| {
                for (id, l) in options {
                    if ui.selectable_label(*id == current, *l).clicked() {
                        picked = Some(id.to_string());
                        ui.close();
                    }
                }
            })
            .response
            .labelled_by(label);
        if let Some(id) = picked.filter(|id| *id != current) {
            set(app, key, json!(id));
        }
    });
}

/// The control for row `key`.
fn row(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens, key: &'static str) {
    match key {
        "general.language" => setting(ui, t, key, None, |ui, label| {
            let selected = crate::i18n::Lang::from_code(&app.language).map_or(tl!("Auto"), crate::i18n::Lang::name);
            let before = app.language.clone();
            egui::ComboBox::from_id_salt("interface-language")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut app.language, crate::i18n::AUTO.to_string(), tl!("Auto")).clicked() {
                        ui.close();
                    }
                    for language in crate::i18n::Lang::all() {
                        if ui.selectable_value(&mut app.language, language.code().to_string(), language.name()).clicked() {
                            ui.close();
                        }
                    }
                })
                .response
                .labelled_by(label);
            // Relabel the rest of this window in the new language right away, not next frame.
            if app.language != before {
                crate::i18n::set_current(crate::i18n::Lang::from_pref(&app.language));
            }
        }),
        "general.theme" => {
            let options: Vec<(&str, &str)> = ThemePreference::ALL
                .iter()
                .map(|p| {
                    let l = match p {
                        ThemePreference::System => tl!("Use system setting"),
                        ThemePreference::Light => tl!("Light gray"),
                        ThemePreference::Warm => tl!("Warm"),
                        ThemePreference::Dim => tl!("Dim"),
                        ThemePreference::Dark => tl!("Dark gray"),
                    };
                    (p.id(), l)
                })
                .collect();
            choice(ui, app, t, key, &options, None);
        }
        "general.default_mode" => {
            setting(ui, t, key, Some(tl!("Used when opening PDFs. An explicit launch or control mode takes precedence for the session.")), |ui, _| {
                for (mode, l) in [
                    (Mode::AllTools, tl!("All tools")),
                    (Mode::Read, tl!("Read")),
                    (Mode::Edit, tl!("Edit")),
                    (Mode::Convert, tl!("Convert")),
                    (Mode::Sign, tl!("E-Sign")),
                ] {
                    ui.radio_value(&mut app.default_mode, mode, l);
                }
            })
        }
        "general.reopen_last_session" => {
            // The web build has no file paths to reopen.
            if cfg!(not(target_arch = "wasm32")) {
                check(ui, app, t, key, None);
            }
        }
        "general.check_updates" => {
            // The web app is always the deployed version.
            if cfg!(not(target_arch = "wasm32")) {
                check(ui, app, t, key, None);
            }
        }
        "documents.autosave_minutes" => setting(
            ui,
            t,
            key,
            Some(tl!("Unsaved changes are kept in a recovery folder so they survive a crash. 0 turns autosave off.")),
            |ui, label| {
                let mut m = app.session.prefs().autosave_minutes;
                let r = ui.add(egui::DragValue::new(&mut m).range(0..=MAX_AUTOSAVE_MINUTES).suffix(format!(" {}", tl!("min")))).labelled_by(label);
                if r.changed() {
                    set(app, key, json!(m));
                }
                if m == 0 {
                    ui.label(egui::RichText::new(tl!("Off")).color(t.text_muted));
                }
            },
        ),
        "documents.recent_files" => setting(ui, t, key, Some(tl!("Shown on Home and in File ▸ Open Recent. 0 keeps no list.")), |ui, label| {
            let mut n = app.session.prefs().recent_files;
            if ui.add(egui::DragValue::new(&mut n).range(0..=MAX_RECENT_FILES)).labelled_by(label).changed() {
                set(app, key, json!(n));
            }
        }),
        "documents.default_layout" => setting(ui, t, key, None, |ui, _| {
            for l in crate::canvas::PageLayout::ORDER {
                ui.radio_value(&mut app.view_defaults.layout, l, tl!(l.label()));
            }
        }),
        "documents.default_zoom" => setting(
            ui,
            t,
            key,
            Some(tl!("Used when a PDF doesn't ask for a layout or zoom. Continuous scrolling never snaps between pages.")),
            |ui, _| {
                use crate::canvas::Fit;
                let defaults = &mut app.view_defaults;
                ui.radio_value(&mut defaults.fit, Fit::Width, tl!("Fit to width"));
                ui.radio_value(&mut defaults.fit, Fit::Page, tl!("Zoom to page level"));
                ui.radio_value(&mut defaults.fit, Fit::None, tl!("Custom"));
                // Editing the percentage selects Custom.
                let mut percent = defaults.zoom * 100.0;
                if ui.add(egui::DragValue::new(&mut percent).range(8.0..=6400.0).max_decimals(0).suffix("%")).changed() {
                    (defaults.fit, defaults.zoom) = (Fit::None, percent / 100.0);
                }
            },
        ),
        "commenting.keep_tool_selected" => check(ui, app, t, key, Some(tl!("Off: after one comment, the Select tool comes back."))),
        "commenting.open_panel" => check(ui, app, t, key, None),
        "forms.highlight_fields" => check(ui, app, t, key, Some(tl!("Each document's own Highlight fields button still turns them on and off."))),
        "forms.highlight_color" | "forms.required_color" => color(ui, app, t, key, None),
        "forms.flatten_fill_sign" => {
            check(ui, app, t, key, Some(tl!("Text, marks and signatures become part of the page. Other comments stay editable.")))
        }
        "forms.date_format" => {
            ui.add_space(6.0);
            crate::js_ui::date_format(ui, app, t);
        }
        "identity.name" => setting(ui, t, key, Some(tl!("Signs new comments. Empty uses your login name.")), |ui, label| {
            let r = ui
                .add(egui::TextEdit::singleline(&mut app.comment_prefs.author).desired_width(320.0).char_limit(crate::MAX_AUTHOR_CHARS))
                .labelled_by(label);
            if r.changed() {
                app.sync_identity_name();
            }
        }),
        "identity.organization" | "identity.unit" => text(ui, app, t, key, None),
        "identity.email" => text(ui, app, t, key, Some(tl!("Your identity is suggested when you create a digital ID."))),
        "signatures.reason" => text(ui, app, t, key, None),
        "signatures.location" => text(ui, app, t, key, Some(tl!("New signatures start with these; you can change them when signing."))),
        "security.javascript" => {
            check(
                ui,
                app,
                t,
                key,
                Some(tl!(
                    "Scripts run in a sandbox without file or network access. A script that asks to open a web page needs your permission first. With JavaScript off, Acrobat's standard format, validate and calculate functions still work."
                )),
            );
        }
        "security.internet_access" => choice(
            ui,
            app,
            t,
            key,
            &[("custom", tl!("Use the site rules")), ("allow-all", tl!("Allow all sites")), ("block-all", tl!("Block all sites"))],
            Some(tl!("Whether links and scripts in PDFs may reach web sites.")),
        ),
        "security.unlisted_sites" => {
            choice(ui, app, t, key, &[("ask", tl!("Ask each time")), ("allow", tl!("Allow")), ("block", tl!("Block"))], None);
        }
        "accessibility.text_scale" => setting(ui, t, key, Some(tl!("Makes all interface text and controls larger or smaller.")), |ui, label| {
            let mut percent = (app.session.prefs().text_scale * 100.0).round();
            let r = ui
                .add(egui::Slider::new(&mut percent, MIN_TEXT_SCALE * 100.0..=MAX_TEXT_SCALE * 100.0).step_by(5.0).suffix("%").max_decimals(0))
                .labelled_by(label);
            // Applied when the slider is let go, so the window doesn't grow under the pointer.
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                set(app, key, json!(percent / 100.0));
            }
        }),
        "accessibility.reduce_motion" => check(ui, app, t, key, Some(tl!("Turns off animations and smooth scrolling."))),
        "accessibility.high_contrast" => check(ui, app, t, key, Some(tl!("Stronger text, borders and outlines in every theme."))),
        "color.rgb" | "color.cmyk" | "color.gray" => {
            let family = match key {
                "color.rgb" => pdfcraft_engine::color_settings::ProfileFamily::Rgb,
                "color.cmyk" => pdfcraft_engine::color_settings::ProfileFamily::Cmyk,
                _ => pdfcraft_engine::color_settings::ProfileFamily::Gray,
            };
            let profiles: Vec<(String, String)> = app
                .session
                .color_management()
                .profiles()
                .into_iter()
                .filter(|p| p.family == family)
                .map(|p| {
                    let shown =
                        if p.description.is_empty() || p.description == p.id { p.id.clone() } else { format!("{} ({})", p.description, p.id) };
                    (p.id, shown)
                })
                .collect();
            let options: Vec<(&str, &str)> = profiles.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            let hint = (key == "color.gray").then_some(tl!("Untagged gray, RGB and CMYK color is shown through these profiles."));
            choice(ui, app, t, key, &options, hint);
        }
        "color.intent" => choice(
            ui,
            app,
            t,
            key,
            &[
                ("perceptual", tl!("Perceptual")),
                ("relative_colorimetric", tl!("Relative colorimetric")),
                ("saturation", tl!("Saturation")),
                ("absolute_colorimetric", tl!("Absolute colorimetric")),
            ],
            None,
        ),
        "color.black_point_compensation" => check(ui, app, t, key, None),
        "color.untagged" => choice(
            ui,
            app,
            t,
            key,
            &[
                ("output_intent", tl!("Output intent overrides working spaces")),
                ("working_space", tl!("Always use the working spaces")),
                ("unmanaged", tl!("Don't color-manage untagged color")),
            ],
            None,
        ),
        "color.overprint_preview" => choice(
            ui,
            app,
            t,
            key,
            &[("pdfx", tl!("Only for PDF/X files")), ("always", tl!("Always")), ("never", tl!("Never")), ("automatic", tl!("Automatic"))],
            Some(tl!("Used by Output Preview and page images made for print checks.")),
        ),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_has_a_label_keywords_and_a_key() {
        let mut seen = std::collections::HashSet::new();
        for page in Page::ALL {
            assert_eq!(Page::from_id(page.id()), Some(page));
            for key in page.rows() {
                assert!(seen.insert(*key), "{key} is on two pages");
                assert!(!label(key).is_empty() && !keywords(key).is_empty(), "{key}");
                assert!(APP_KEYS.contains(key) || engine_prefs::spec_of(key).is_some(), "{key} is neither an app nor a session key");
            }
        }
        // Every session preference has a row somewhere.
        for spec in engine_prefs::SCHEMA {
            assert!(seen.contains(spec.key), "{} has no row", spec.key);
        }
    }

    #[test]
    fn search_finds_settings_across_pages() {
        let found = search("autosave");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], (Page::Documents, vec!["documents.autosave_minutes"]));
        let color: Vec<Page> = search("COLOUR").into_iter().map(|(p, _)| p).collect();
        assert!(color.contains(&Page::Forms) && color.contains(&Page::Color), "{color:?}");
        assert!(search("zzzz-nothing").is_empty());
        assert_eq!(search("").len(), Page::ALL.len());
        // A page name finds the whole page.
        assert_eq!(search("security")[0].1.len(), Page::Security.rows().len());
    }
}
