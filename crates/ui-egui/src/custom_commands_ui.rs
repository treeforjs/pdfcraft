//! Custom commands (Acrobat's Custom Commands): build, edit, delete, import, export and run a
//! command that bundles one document tool with preset options, optionally asking for them again
//! when it runs (Prompt user). The library is the folder of JSON files the `custom_command_*`
//! tools keep; everything goes through those tools, so the app and agents share one library and
//! one set of checks. The tools reach the app through [`pdfcraft_engine::tool_host::ToolHost`],
//! which the desktop app installs; without it (the web app) the dialog says it isn't available.

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui::{Align, Layout};
use pdfcraft_engine::tool_host::ToolInfo;
use serde_json::{Map, Value, json};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// One option of the tool: whether it is set, and its value as typed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OptionValue {
    pub set: bool,
    pub text: String,
    pub flag: bool,
}

/// A command being created or edited.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    /// The id of the command being edited (`None`: a new one).
    pub original: Option<String>,
    pub label: String,
    pub description: String,
    pub tool: String,
    pub options: BTreeMap<String, OptionValue>,
    pub prompt_user: bool,
}

/// Running a command that asks for its options.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prompt {
    pub id: String,
    pub label: String,
    pub tool: String,
    pub options: BTreeMap<String, OptionValue>,
}

/// The dialog's state.
#[derive(Default)]
pub struct CustomCommands {
    /// The library's commands (`custom_command_list`), read when the dialog opens or changes.
    pub commands: Vec<Value>,
    pub selected: Option<String>,
    pub editing: Option<Draft>,
    pub prompt: Option<Prompt>,
    pub tools: Vec<ToolInfo>,
    pub error: Option<String>,
    /// Tests: the library folder (instead of the per-user one) and the file Import/Export use.
    pub library_override: Option<PathBuf>,
    pub file_override: Option<PathBuf>,
}

/// Most options a tool's form shows.
const MAX_OPTIONS: usize = 64;

impl CustomCommands {
    fn tool(&self, name: &str) -> Option<&ToolInfo> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// The control channel's view of the dialog.
    pub fn state(&self) -> Value {
        json!({
            "commands": self.commands.iter().map(|c| json!({ "id": c["id"], "label": c["label"], "tool": c["tool"], "prompt_user": c["prompt_user"], "params": c["params"] })).collect::<Vec<_>>(),
            "selected": self.selected,
            "editing": self.editing.as_ref().map(|d| json!({ "label": d.label, "tool": d.tool, "prompt_user": d.prompt_user, "options": d.options.iter().filter(|(_, o)| o.set).map(|(k, _)| k.clone()).collect::<Vec<_>>() })),
            "prompt": self.prompt.as_ref().map(|p| json!({ "id": p.id, "tool": p.tool })),
            "error": self.error,
        })
    }
}

/// The options a tool takes (its schema's properties, without `doc`), in name order.
fn tool_options(tool: &ToolInfo) -> Vec<(String, Value)> {
    let mut v: Vec<(String, Value)> = tool.schema["properties"]
        .as_object()
        .map(|p| p.iter().filter(|(k, _)| k.as_str() != "doc").map(|(k, s)| (k.clone(), s.clone())).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v.truncate(MAX_OPTIONS);
    v
}

fn required(tool: &ToolInfo) -> Vec<String> {
    tool.schema["required"]
        .as_array()
        .map(|r| r.iter().filter_map(|x| x.as_str()).filter(|x| *x != "doc").map(str::to_string).collect())
        .unwrap_or_default()
}

/// The form values for preset `params`.
fn options_from(tool: Option<&ToolInfo>, params: &Map<String, Value>) -> BTreeMap<String, OptionValue> {
    let mut out = BTreeMap::new();
    for (k, schema) in tool.map(tool_options).unwrap_or_default() {
        let v = params.get(&k);
        let o = match (schema["type"].as_str(), v) {
            (_, None) => OptionValue::default(),
            (Some("boolean"), Some(v)) => OptionValue { set: true, flag: v.as_bool().unwrap_or(false), text: String::new() },
            (Some("string"), Some(Value::String(s))) => OptionValue { set: true, text: s.clone(), flag: false },
            (_, Some(v)) => OptionValue { set: true, text: v.to_string(), flag: false },
        };
        out.insert(k, o);
    }
    out
}

/// The params object the form describes, or which option is wrong.
pub fn params_of(tool: &ToolInfo, options: &BTreeMap<String, OptionValue>) -> Result<Map<String, Value>, String> {
    let mut m = Map::new();
    for (k, schema) in tool_options(tool) {
        let Some(o) = options.get(&k).filter(|o| o.set) else { continue };
        let bad = || crate::i18n::fmt(tl!("{name} isn't valid"), &[("name", &k)]);
        let v = match schema["type"].as_str() {
            Some("boolean") => Value::Bool(o.flag),
            Some("integer") => Value::from(o.text.trim().parse::<i64>().map_err(|_| bad())?),
            Some("number") => {
                let n = o.text.trim().parse::<f64>().ok().filter(|n| n.is_finite()).ok_or_else(bad)?;
                serde_json::Number::from_f64(n).map(Value::Number).ok_or_else(bad)?
            }
            Some("string") => Value::String(o.text.clone()),
            _ => serde_json::from_str::<Value>(o.text.trim()).map_err(|_| bad())?,
        };
        m.insert(k, v);
    }
    Ok(m)
}

/// The option rows: a check box to set each one and an editor for its value.
fn options_form(ui: &mut egui::Ui, t: &Tokens, tool: &ToolInfo, options: &mut BTreeMap<String, OptionValue>, salt: &str) {
    let req = required(tool);
    egui::ScrollArea::vertical().id_salt(("cc-options", salt)).max_height(260.0).show(ui, |ui| {
        egui::Grid::new(("cc-grid", salt)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            for (k, schema) in tool_options(tool) {
                let o = options.entry(k.clone()).or_default();
                let name = if req.contains(&k) { format!("{k} *") } else { k.clone() };
                ui.checkbox(&mut o.set, name).on_hover_text(schema["description"].as_str().unwrap_or(""));
                ui.add_enabled_ui(o.set, |ui| match (schema["type"].as_str(), schema["enum"].as_array()) {
                    (Some("boolean"), _) => {
                        ui.checkbox(&mut o.flag, "");
                    }
                    (Some("string"), Some(choices)) => {
                        let shown = if o.text.is_empty() { tl!("Choose…").to_string() } else { o.text.clone() };
                        egui::ComboBox::from_id_salt(("cc-enum", salt, k.as_str())).selected_text(shown).show_ui(ui, |ui| {
                            for c in choices.iter().filter_map(Value::as_str).take(200) {
                                ui.selectable_value(&mut o.text, c.to_string(), c);
                            }
                        });
                    }
                    (kind, _) => {
                        let hint = match kind {
                            Some("integer") | Some("number") => "0",
                            Some("string") => "",
                            _ => "JSON",
                        };
                        ui.add(egui::TextEdit::singleline(&mut o.text).hint_text(hint).desired_width(240.0).id_salt(("cc-value", salt, k.as_str())));
                    }
                });
                ui.end_row();
            }
        });
    });
    ui.label(egui::RichText::new(tl!("* the tool needs this option")).small().color(t.text_faint));
}

/// A command id from its label: lowercase letters, digits and `-`, unique in the library.
fn id_for(label: &str, taken: &[Value]) -> String {
    let mut base: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    base.truncate(48);
    if base.is_empty() {
        base = "command".into();
    }
    let used = |id: &str| taken.iter().any(|c| c["id"].as_str() == Some(id));
    if !used(&base) {
        return base;
    }
    (2..1000).map(|n| format!("{base}-{n}")).find(|id| !used(id)).unwrap_or(base)
}

impl PdfCraftApp {
    /// The custom command library folder: beside the recovery folder (per user or portable).
    pub fn custom_commands_library(&self) -> Option<PathBuf> {
        if let Some(d) = &self.custom_commands.library_override {
            return Some(d.clone());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::recovery::RecoveryStore::default_dir().and_then(|d| d.parent().map(|p| p.join("Custom Commands")))
        }
        #[cfg(target_arch = "wasm32")]
        None
    }

    /// Run a custom-command tool with the library filled in.
    fn custom_tool(&mut self, name: &str, mut args: Value) -> Result<Value, String> {
        let host = self.tool_host.clone().ok_or_else(|| tl!("Custom commands aren't available in this version of PdfCraft.").to_string())?;
        let library = self.custom_commands_library().ok_or_else(|| tl!("There is no folder to keep custom commands in.").to_string())?;
        if args.get("library").is_none()
            && let Some(m) = args.as_object_mut()
        {
            m.insert("library".into(), json!(library.to_string_lossy()));
        }
        if !library.exists() {
            std::fs::create_dir_all(&library).map_err(|e| format!("{}: {e}", library.display()))?;
        }
        host.call(&mut self.session, name, &args)
    }

    /// Open the Custom Commands dialog, reading the library.
    pub fn open_custom_commands(&mut self) {
        self.custom_commands.editing = None;
        self.custom_commands.prompt = None;
        self.custom_commands.error = None;
        if let Some(h) = self.tool_host.clone() {
            let mut tools = h.document_tools();
            tools.sort_by(|a, b| a.title.cmp(&b.title));
            self.custom_commands.tools = tools;
        }
        self.reload_custom_commands();
        self.dialog = Some(crate::Dialog::CustomCommands);
    }

    fn reload_custom_commands(&mut self) {
        match self.custom_tool("custom_command_list", json!({})) {
            Ok(v) => {
                self.custom_commands.commands = v["commands"].as_array().cloned().unwrap_or_default();
                let cc = &mut self.custom_commands;
                if cc.selected.as_ref().is_none_or(|s| !cc.commands.iter().any(|c| c["id"].as_str() == Some(s))) {
                    cc.selected = cc.commands.first().and_then(|c| c["id"].as_str()).map(str::to_string);
                }
            }
            Err(e) => {
                self.custom_commands.commands.clear();
                self.custom_commands.error = Some(e);
            }
        }
    }

    /// Save the command being edited (create or edit), checked by the tool.
    fn save_custom_command(&mut self) {
        let Some(d) = self.custom_commands.editing.clone() else { return };
        let Some(tool) = self.custom_commands.tool(&d.tool).cloned() else {
            self.custom_commands.error = Some(tl!("Choose the tool the command runs.").to_string());
            return;
        };
        let params = match params_of(&tool, &d.options) {
            Ok(p) => p,
            Err(e) => {
                self.custom_commands.error = Some(e);
                return;
            }
        };
        let (name, id) = match &d.original {
            Some(id) => ("custom_command_edit", id.clone()),
            None => ("custom_command_create", id_for(&d.label, &self.custom_commands.commands)),
        };
        let args = json!({ "id": id, "label": d.label.trim(), "description": d.description, "tool": d.tool, "params": params, "prompt_user": d.prompt_user });
        match self.custom_tool(name, args) {
            Ok(_) => {
                self.custom_commands.editing = None;
                self.custom_commands.error = None;
                self.custom_commands.selected = Some(id);
                self.reload_custom_commands();
            }
            Err(e) => self.custom_commands.error = Some(e),
        }
    }

    /// Run command `id` on the active document; one that prompts asks for its options first.
    pub fn run_custom_command(&mut self, id: &str, params: Option<Map<String, Value>>) {
        let Some((_, doc)) = self.active_ids() else {
            self.custom_commands.error = Some(tl!("Open a document to run the command on.").to_string());
            return;
        };
        let Some(c) = self.custom_commands.commands.iter().find(|c| c["id"].as_str() == Some(id)).cloned() else { return };
        if params.is_none() && c["prompt_user"].as_bool() == Some(true) {
            let tool = c["tool"].as_str().unwrap_or("").to_string();
            let presets = c["params"].as_object().cloned().unwrap_or_default();
            let options = options_from(self.custom_commands.tool(&tool), &presets);
            self.custom_commands.prompt = Some(Prompt { id: id.to_string(), label: c["label"].as_str().unwrap_or("").to_string(), tool, options });
            return;
        }
        let mut args = json!({ "id": id, "doc": doc.0 });
        if let Some(p) = params {
            args["params"] = Value::Object(p);
        }
        let label = c["label"].as_str().unwrap_or(id).to_string();
        match self.custom_tool("custom_command_run", args) {
            Ok(_) => {
                self.custom_commands.prompt = None;
                self.custom_commands.error = None;
                if let Some(v) = self.views.iter_mut().find(|v| v.id == doc) {
                    v.invalidate_content();
                }
                self.notify(crate::i18n::fmt(tl!("Ran {name}"), &[("name", &label)]));
            }
            Err(e) => self.custom_commands.error = Some(e),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn import_custom_commands(&mut self, path: PathBuf) {
        match self.custom_tool("custom_command_import", json!({ "path": path.to_string_lossy() })) {
            Ok(v) => {
                let n = v["imported"].as_array().map_or(0, Vec::len);
                self.notify(crate::i18n::fmt(tl!("Imported {n} custom commands"), &[("n", &n.to_string())]));
                self.reload_custom_commands();
            }
            Err(e) => self.custom_commands.error = Some(e),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn export_custom_commands(&mut self, path: PathBuf) {
        match self.custom_tool("custom_command_export", json!({ "path": path.to_string_lossy() })) {
            Ok(_) => self.notify(crate::i18n::fmt(tl!("Exported to {path}"), &[("path", &path.display().to_string())])),
            Err(e) => self.custom_commands.error = Some(e),
        }
    }
}

/// The dialog. Returns `true` to close.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    ui.set_width(640.0);
    if app.tool_host.is_none() {
        ui.label(egui::RichText::new(tl!("Custom commands")).font(theme::semibold(18.0)));
        ui.add_space(8.0);
        ui.label(tl!("Custom commands aren't available in this version of PdfCraft."));
        ui.add_space(10.0);
        return widgets::pill_button(ui, tl!("Close"), false).clicked();
    }
    if app.custom_commands.prompt.is_some() {
        return prompt_body(ui, app, t);
    }
    if app.custom_commands.editing.is_some() {
        return edit_body(ui, app, t);
    }
    ui.label(egui::RichText::new(tl!("Custom commands")).font(theme::semibold(18.0)));
    ui.label(egui::RichText::new(tl!("One tool with the options you choose, run in one step.")).color(t.text_muted));
    ui.add_space(8.0);
    let mut run = None;
    ui.horizontal_top(|ui| {
        egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            ui.set_width(220.0);
            ui.set_min_height(240.0);
            egui::ScrollArea::vertical().id_salt("cc-list").max_height(300.0).show(ui, |ui| {
                if app.custom_commands.commands.is_empty() {
                    ui.label(egui::RichText::new(tl!("No custom commands yet.")).color(t.text_muted));
                }
                for c in &app.custom_commands.commands {
                    let id = c["id"].as_str().unwrap_or("").to_string();
                    let on = app.custom_commands.selected.as_deref() == Some(id.as_str());
                    if ui.selectable_label(on, c["label"].as_str().unwrap_or(&id)).clicked() {
                        app.custom_commands.selected = Some(id);
                    }
                }
            });
        });
        ui.vertical(|ui| {
            let cc = &app.custom_commands;
            let Some(c) = cc.selected.as_ref().and_then(|s| cc.commands.iter().find(|c| c["id"].as_str() == Some(s))) else { return };
            ui.label(egui::RichText::new(c["label"].as_str().unwrap_or("")).font(theme::semibold(15.0)));
            if let Some(d) = c["description"].as_str().filter(|d| !d.is_empty()) {
                ui.label(egui::RichText::new(d).color(t.text_muted));
            }
            let tool = c["tool"].as_str().unwrap_or("");
            let title = cc.tool(tool).map_or(tool, |t| t.title.as_str());
            ui.label(crate::i18n::fmt(tl!("Runs: {tool}"), &[("tool", title)]));
            let presets = c["params"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| format!("{k} = {v}")).take(20).collect::<Vec<_>>().join("\n"))
                .unwrap_or_default();
            if !presets.is_empty() {
                ui.label(egui::RichText::new(presets).monospace().small());
            }
            if c["prompt_user"].as_bool() == Some(true) {
                ui.label(egui::RichText::new(tl!("Asks for its options when it runs.")).small().color(t.text_faint));
            }
            ui.add_space(8.0);
            let id = c["id"].as_str().unwrap_or("").to_string();
            if widgets::pill_button(ui, tl!("Run on this document"), true).clicked() {
                run = Some(id);
            }
        });
    });
    if let Some(id) = run {
        app.run_custom_command(&id, None);
    }
    if let Some(e) = &app.custom_commands.error {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(e).color(theme::DANGER));
    }
    ui.add_space(10.0);
    let mut close = false;
    ui.horizontal(|ui| {
        if ui.button(tl!("New Command…")).clicked() {
            app.custom_commands.error = None;
            app.custom_commands.editing = Some(Draft::default());
        }
        let selected = app
            .custom_commands
            .selected
            .clone()
            .and_then(|s| app.custom_commands.commands.iter().find(|c| c["id"].as_str() == Some(s.as_str())).cloned());
        if let Some(c) = selected {
            if ui.button(tl!("Edit…")).clicked() {
                let tool = c["tool"].as_str().unwrap_or("").to_string();
                let options = options_from(app.custom_commands.tool(&tool), &c["params"].as_object().cloned().unwrap_or_default());
                app.custom_commands.error = None;
                app.custom_commands.editing = Some(Draft {
                    original: c["id"].as_str().map(str::to_string),
                    label: c["label"].as_str().unwrap_or("").to_string(),
                    description: c["description"].as_str().unwrap_or("").to_string(),
                    tool,
                    options,
                    prompt_user: c["prompt_user"].as_bool().unwrap_or(false),
                });
            }
            if ui.button(tl!("Delete")).clicked() {
                let id = c["id"].as_str().unwrap_or("").to_string();
                match app.custom_tool("custom_command_delete", json!({ "id": id })) {
                    Ok(_) => {
                        app.custom_commands.selected = None;
                        app.reload_custom_commands();
                    }
                    Err(e) => app.custom_commands.error = Some(e),
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if ui.button(tl!("Import…")).clicked() {
                match app.custom_commands.file_override.clone() {
                    Some(p) => app.import_custom_commands(p),
                    None => {
                        let d = rfd::AsyncFileDialog::new().set_title(tl!("Import custom commands")).add_filter("JSON", &["json"]);
                        app.ask_one(crate::pickers::Ask::File(d), None, |app, p| app.import_custom_commands(p));
                    }
                }
            }
            if !app.custom_commands.commands.is_empty() && ui.button(tl!("Export…")).clicked() {
                match app.custom_commands.file_override.clone() {
                    Some(p) => app.export_custom_commands(p),
                    None => {
                        let d = rfd::AsyncFileDialog::new().set_title(tl!("Export custom commands")).set_file_name("custom-commands.json");
                        app.ask_one(crate::pickers::Ask::Save(d), None, |app, p| app.export_custom_commands(p));
                    }
                }
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("Close"), false).clicked() {
                close = true;
            }
        });
    });
    close
}

fn edit_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let tools = app.custom_commands.tools.clone();
    let Some(d) = app.custom_commands.editing.as_mut() else { return false };
    ui.label(egui::RichText::new(if d.original.is_some() { tl!("Edit Command") } else { tl!("New Command") }).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    egui::Grid::new("cc-edit").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        let l = ui.label(tl!("Name:"));
        ui.add(egui::TextEdit::singleline(&mut d.label).desired_width(360.0).id_salt("cc-label")).labelled_by(l.id);
        ui.end_row();
        let l = ui.label(tl!("Description:"));
        ui.add(egui::TextEdit::singleline(&mut d.description).desired_width(360.0).id_salt("cc-description")).labelled_by(l.id);
        ui.end_row();
        ui.label(tl!("Tool:"));
        let shown = tools.iter().find(|x| x.name == d.tool).map_or_else(|| tl!("Choose…").to_string(), |x| x.title.clone());
        let before = d.tool.clone();
        egui::ComboBox::from_id_salt("cc-tool").selected_text(shown).width(360.0).height(320.0).show_ui(ui, |ui| {
            for x in &tools {
                ui.selectable_value(&mut d.tool, x.name.clone(), &x.title).on_hover_text(&x.description);
            }
        });
        if d.tool != before {
            d.options.clear();
        }
        ui.end_row();
    });
    if let Some(tool) = tools.iter().find(|x| x.name == d.tool) {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("Options")).font(theme::semibold(13.0)));
        options_form(ui, t, tool, &mut d.options, "edit");
    }
    ui.checkbox(&mut d.prompt_user, tl!("Ask for the options each time it runs"));
    if let Some(e) = &app.custom_commands.error {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(e).color(theme::DANGER));
    }
    ui.add_space(10.0);
    let ok = app.custom_commands.editing.as_ref().is_some_and(|d| !d.label.trim().is_empty() && !d.tool.is_empty());
    let (mut save, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        save = ui.add_enabled_ui(ok, |ui| widgets::pill_button(ui, tl!("Save"), true)).inner.clicked();
        cancel = widgets::pill_button(ui, tl!("Cancel"), false).clicked();
    });
    if cancel {
        app.custom_commands.editing = None;
        app.custom_commands.error = None;
    }
    if save {
        app.save_custom_command();
    }
    false
}

fn prompt_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let tools = app.custom_commands.tools.clone();
    let Some(p) = app.custom_commands.prompt.as_mut() else { return false };
    ui.label(egui::RichText::new(&p.label).font(theme::semibold(18.0)));
    ui.label(egui::RichText::new(tl!("Check the options, then run the command.")).color(t.text_muted));
    ui.add_space(8.0);
    let tool = tools.iter().find(|x| x.name == p.tool);
    if let Some(tool) = tool {
        options_form(ui, t, tool, &mut p.options, "prompt");
    }
    if let Some(e) = &app.custom_commands.error {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(e).color(theme::DANGER));
    }
    ui.add_space(10.0);
    let (mut go, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        go = widgets::pill_button(ui, tl!("Run"), true).clicked();
        cancel = widgets::pill_button(ui, tl!("Cancel"), false).clicked();
    });
    if cancel {
        app.custom_commands.prompt = None;
        app.custom_commands.error = None;
        return false;
    }
    if go && let (Some(p), Some(tool)) = (app.custom_commands.prompt.clone(), tool) {
        match params_of(tool, &p.options) {
            Ok(params) => app.run_custom_command(&p.id, Some(params)),
            Err(e) => app.custom_commands.error = Some(e),
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ToolInfo {
        ToolInfo {
            name: "x".into(),
            title: "X".into(),
            description: String::new(),
            schema: json!({ "properties": { "doc": {}, "n": { "type": "integer" }, "f": { "type": "number" }, "b": { "type": "boolean" }, "s": { "type": "string" }, "a": { "type": "array" } }, "required": ["doc", "n"] }),
        }
    }

    #[test]
    fn options_round_trip_and_bad_values_are_refused() {
        let t = tool();
        let presets = json!({ "n": 3, "f": 1.5, "b": true, "s": "hi", "a": [1, 2] });
        let opts = options_from(Some(&t), presets.as_object().unwrap());
        assert!(!opts.contains_key("doc"));
        assert_eq!(params_of(&t, &opts).unwrap(), *presets.as_object().unwrap());
        assert_eq!(required(&t), vec!["n".to_string()]);
        for (k, bad) in [("n", "1.5"), ("n", "x"), ("f", "NaN"), ("f", "inf"), ("a", "[1,"), ("n", "99999999999999999999999")] {
            let mut o = opts.clone();
            o.insert(k.into(), OptionValue { set: true, text: bad.into(), flag: false });
            assert!(params_of(&t, &o).is_err(), "{k} = {bad}");
        }
        // Unset options aren't sent; unknown keys in the form are ignored.
        let mut o = BTreeMap::new();
        o.insert("zzz".into(), OptionValue { set: true, text: "1".into(), flag: false });
        o.insert("n".into(), OptionValue { set: false, text: "x".into(), flag: false });
        assert!(params_of(&t, &o).unwrap().is_empty());
    }

    #[test]
    fn ids_are_slugs_and_unique() {
        assert_eq!(id_for("Make it Searchable!", &[]), "make-it-searchable");
        assert_eq!(id_for("日本語", &[]), "command");
        assert_eq!(id_for("a", &[json!({ "id": "a" }), json!({ "id": "a-2" })]), "a-3");
        assert!(id_for(&"x".repeat(500), &[]).len() <= 48);
    }
}
