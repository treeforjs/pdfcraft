//! Custom commands (Acrobat's Custom Commands): one tool bundled with preset options, kept as a
//! JSON file per command in a library folder and run in one step on an open document or on files.
//!
//! A command names a document tool from the tool table (anything taking `doc`, except the session
//! and meta tools in [`DENIED`]) and a preset `params` object, validated against that tool's input
//! schema when it is stored and again when it runs. `prompt_user` is Acrobat's "Prompt user"
//! option: the caller may then supply (and must supply any missing required) options at run time;
//! without it the presets are fixed. Running goes through [`Automation::call`], so the tool keeps
//! its own validation, atomic writes and `--root` confinement; the library folder, imports,
//! exports and the files a run reads and writes are confined the same way.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::{Args, Automation, Content, Result, ToolError, failed, tools, write_atomic};

/// The library folder when `library` is not given (relative: inside the root).
pub(crate) const DEFAULT_LIBRARY: &str = "custom-commands";
/// The most commands a library, an import or an export holds.
const MAX_COMMANDS: usize = 500;
/// The largest command file read from a library (bytes).
const MAX_COMMAND_FILE: u64 = 256 * 1024;
/// The largest import file (bytes).
const MAX_IMPORT_FILE: u64 = 4 * 1024 * 1024;
/// The largest preset `params` object, serialized (bytes), and its most keys.
const MAX_PARAMS_BYTES: usize = 64 * 1024;
const MAX_PARAMS_KEYS: usize = 64;
/// The most files one run processes.
const MAX_RUN_FILES: usize = 1000;
/// How deep schema validation and token substitution go.
const MAX_DEPTH: usize = 32;
/// The most entries in one array checked against a schema.
const MAX_ARRAY: usize = 10_000;
const MAX_LABEL: usize = 200;
const MAX_DESCRIPTION: usize = 4000;
const FORMAT: &str = "pdfcraft-custom-command";
const BUNDLE_FORMAT: &str = "pdfcraft-custom-commands";
const VERSION: u64 = 1;

/// Document tools a custom command may not wrap: the run itself opens, saves and closes files,
/// and meta tools would let commands call commands.
const DENIED: &[&str] = &[
    "doc_close",
    "doc_save",
    "doc_open",
    "doc_list",
    "doc_inspect",
    "command_run",
    "command_batch",
    "command_list",
    "action_run",
    "action_list",
    "custom_command_create",
    "custom_command_edit",
    "custom_command_delete",
    "custom_command_list",
    "custom_command_get",
    "custom_command_import",
    "custom_command_export",
    "custom_command_run",
];

/// One stored custom command.
#[derive(Clone, Debug, PartialEq)]
struct Command {
    id: String,
    label: String,
    description: String,
    tool: String,
    params: Map<String, Value>,
    prompt_user: bool,
}

impl Command {
    fn to_json(&self) -> Value {
        json!({
            "format": FORMAT,
            "version": VERSION,
            "id": self.id,
            "label": self.label,
            "description": self.description,
            "tool": self.tool,
            "params": self.params,
            "prompt_user": self.prompt_user,
        })
    }

    /// Parse and fully validate a stored or imported command.
    fn from_json(v: &Value) -> std::result::Result<Command, String> {
        let obj = v.as_object().ok_or("a custom command must be a JSON object")?;
        const KEYS: &[&str] = &["format", "version", "id", "label", "description", "tool", "params", "prompt_user"];
        if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("unknown key {k:?} (expected: {})", KEYS.join(", ")));
        }
        if let Some(f) = obj.get("format")
            && f.as_str() != Some(FORMAT)
        {
            return Err(format!("format must be {FORMAT:?}"));
        }
        if let Some(ver) = obj.get("version") {
            match ver.as_u64() {
                Some(VERSION) => {}
                _ => return Err(format!("unsupported version {ver} (this version reads {VERSION})")),
            }
        }
        let text = |k: &str| -> std::result::Result<Option<String>, String> {
            match obj.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(format!("{k} must be a string")),
            }
        };
        let id = text("id")?.ok_or("missing id")?;
        check_id(&id)?;
        let label = text("label")?.ok_or("missing label")?;
        check_label(&label)?;
        let description = text("description")?.unwrap_or_default();
        check_description(&description)?;
        let tool = text("tool")?.ok_or("missing tool")?;
        let params = match obj.get("params") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err("params must be an object".into()),
        };
        let prompt_user = match obj.get("prompt_user") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err("prompt_user must be true or false".into()),
        };
        let c = Command { id, label, description, tool, params, prompt_user };
        validate(&c)?;
        Ok(c)
    }
}

fn check_id(id: &str) -> std::result::Result<(), String> {
    let mut chars = id.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !first_ok || id.len() > 64 || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Err(format!(
            "id {:?} is not valid: 1–64 characters of a–z, 0–9, - and _, starting with a letter or digit",
            id.chars().take(80).collect::<String>()
        ));
    }
    Ok(())
}

fn check_label(label: &str) -> std::result::Result<(), String> {
    let n = label.chars().count();
    if label.trim().is_empty() || n > MAX_LABEL || label.chars().any(char::is_control) {
        return Err(format!("label must be 1–{MAX_LABEL} characters with no control characters"));
    }
    Ok(())
}

fn check_description(d: &str) -> std::result::Result<(), String> {
    if d.chars().count() > MAX_DESCRIPTION || d.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(format!("description must be at most {MAX_DESCRIPTION} characters with no control characters other than line breaks and tabs"));
    }
    Ok(())
}

/// The tool definition a custom command may wrap, or why not.
fn target_tool(name: &str) -> std::result::Result<&'static tools::ToolDef, String> {
    let def =
        tools::find(name).ok_or_else(|| format!("unknown tool {:?} (see custom_command_list's tools)", name.chars().take(80).collect::<String>()))?;
    let takes_doc = def.input_schema["properties"].as_object().is_some_and(|p| p.contains_key("doc"));
    if !takes_doc || DENIED.contains(&def.name) {
        return Err(format!("{} can't be a custom command: it must be a document tool (see custom_command_list's tools)", def.name));
    }
    Ok(def)
}

/// The tools a custom command may wrap.
fn allowed_tools() -> Vec<&'static str> {
    wrappable_tools().into_iter().map(|d| d.name).collect()
}

/// The definitions of the tools a custom command may wrap (the app's command builder lists them).
pub(crate) fn wrappable_tools() -> Vec<&'static tools::ToolDef> {
    tools::all().iter().filter(|d| target_tool(d.name).is_ok()).collect()
}

/// Check `params` against `def`'s schema: known keys (never `doc`), each value's type, range,
/// length and items. Required options are checked by the caller.
fn check_params(def: &tools::ToolDef, params: &Map<String, Value>) -> std::result::Result<(), String> {
    if params.len() > MAX_PARAMS_KEYS {
        return Err(format!("params has {} keys; at most {MAX_PARAMS_KEYS}", params.len()));
    }
    let size = serde_json::to_vec(params).map_or(usize::MAX, |b| b.len());
    if size > MAX_PARAMS_BYTES {
        return Err(format!("params is {size} bytes; at most {MAX_PARAMS_BYTES}"));
    }
    let props = def.input_schema["properties"].as_object().ok_or_else(|| format!("{} has no parameter schema", def.name))?;
    for (k, v) in params {
        if k == "doc" {
            return Err("params must not contain doc: the document is chosen when the command runs".into());
        }
        let spec = props.get(k).ok_or_else(|| {
            let known: Vec<&str> = props.keys().map(String::as_str).filter(|k| *k != "doc").collect();
            format!("{}: unknown option {:?} (expected: {})", def.name, k.chars().take(80).collect::<String>(), known.join(", "))
        })?;
        check_value(spec, v, k, 0).map_err(|e| format!("{}: {e}", def.name))?;
    }
    Ok(())
}

/// Required options of `def` (other than `doc`) that `params` doesn't set.
fn missing_required(def: &tools::ToolDef, params: &Map<String, Value>) -> Vec<String> {
    def.input_schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|r| *r != "doc" && params.get(*r).is_none_or(Value::is_null))
        .map(str::to_string)
        .collect()
}

fn validate(c: &Command) -> std::result::Result<(), String> {
    let def = target_tool(&c.tool)?;
    check_params(def, &c.params)?;
    let missing = missing_required(def, &c.params);
    if !missing.is_empty() && !c.prompt_user {
        return Err(format!(
            "{} needs {}: preset it in params, or set prompt_user so it is asked for when the command runs",
            def.name,
            missing.join(", ")
        ));
    }
    Ok(())
}

fn type_matches(t: &str, v: &Value) -> bool {
    match t {
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.as_f64().is_some_and(f64::is_finite),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true,
    }
}

/// Validate `v` against the JSON Schema subset the tool table uses.
fn check_value(schema: &Value, v: &Value, at: &str, depth: usize) -> std::result::Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("{at} is nested too deeply"));
    }
    let Some(s) = schema.as_object() else { return Ok(()) };
    match s.get("type") {
        Some(Value::String(t)) if !type_matches(t, v) => return Err(format!("{at} must be of type {t}")),
        Some(Value::Array(ts)) if !ts.iter().filter_map(Value::as_str).any(|t| type_matches(t, v)) => {
            let names: Vec<&str> = ts.iter().filter_map(Value::as_str).collect();
            return Err(format!("{at} must be of type {}", names.join(" or ")));
        }
        _ => {}
    }
    if let Some(Value::Array(options)) = s.get("enum")
        && !options.contains(v)
    {
        let names: Vec<String> = options.iter().map(Value::to_string).collect();
        return Err(format!("{at} must be one of {}", names.join(", ")));
    }
    if let Some(c) = s.get("const")
        && c != v
    {
        return Err(format!("{at} must be {c}"));
    }
    if let Some(x) = v.as_f64() {
        let bound = |k: &str| s.get(k).and_then(Value::as_f64);
        if let Some(m) = bound("minimum")
            && x < m
        {
            return Err(format!("{at} must be at least {m}"));
        }
        if let Some(m) = bound("maximum")
            && x > m
        {
            return Err(format!("{at} must be at most {m}"));
        }
        if let Some(m) = bound("exclusiveMinimum")
            && x <= m
        {
            return Err(format!("{at} must be more than {m}"));
        }
        if let Some(m) = bound("exclusiveMaximum")
            && x >= m
        {
            return Err(format!("{at} must be less than {m}"));
        }
    }
    if let Some(text) = v.as_str() {
        let n = text.chars().count() as u64;
        if let Some(m) = s.get("minLength").and_then(Value::as_u64)
            && n < m
        {
            return Err(format!("{at} must have at least {m} characters"));
        }
        if let Some(m) = s.get("maxLength").and_then(Value::as_u64)
            && n > m
        {
            return Err(format!("{at} must have at most {m} characters"));
        }
    }
    if let Some(items) = v.as_array() {
        if items.len() > MAX_ARRAY {
            return Err(format!("{at} has {} items; at most {MAX_ARRAY}", items.len()));
        }
        let n = items.len() as u64;
        if let Some(m) = s.get("minItems").and_then(Value::as_u64)
            && n < m
        {
            return Err(format!("{at} must have at least {m} items"));
        }
        if let Some(m) = s.get("maxItems").and_then(Value::as_u64)
            && n > m
        {
            return Err(format!("{at} must have at most {m} items"));
        }
        if s.get("uniqueItems").and_then(Value::as_bool) == Some(true) {
            let mut seen = HashSet::new();
            if !items.iter().all(|i| seen.insert(i.to_string())) {
                return Err(format!("{at} must not repeat items"));
            }
        }
        if let Some(item_schema) = s.get("items") {
            for (i, item) in items.iter().enumerate() {
                check_value(item_schema, item, &format!("{at}[{i}]"), depth.saturating_add(1))?;
            }
        }
    }
    if let Some(obj) = v.as_object() {
        let props = s.get("properties").and_then(Value::as_object);
        if let Some(props) = props {
            for (k, sub) in props {
                if let Some(x) = obj.get(k) {
                    check_value(sub, x, &format!("{at}.{k}"), depth.saturating_add(1))?;
                }
            }
        }
        if s.get("additionalProperties") == Some(&Value::Bool(false))
            && let Some(k) = obj.keys().find(|k| !props.is_some_and(|p| p.contains_key(*k)))
        {
            return Err(format!("{at} has an unknown key {:?}", k.chars().take(80).collect::<String>()));
        } else if let Some(extra @ Value::Object(_)) = s.get("additionalProperties") {
            for (k, x) in obj.iter().filter(|(k, _)| !props.is_some_and(|p| p.contains_key(*k))) {
                check_value(extra, x, &format!("{at}.{k}"), depth.saturating_add(1))?;
            }
        }
        for r in s.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            if obj.get(r).is_none_or(Value::is_null) {
                return Err(format!("{at}.{r} is required"));
            }
        }
    }
    for (key, exactly_one) in [("oneOf", true), ("anyOf", false)] {
        if let Some(Value::Array(alts)) = s.get(key) {
            let valid = alts.iter().filter(|alt| check_value(alt, v, at, depth.saturating_add(1)).is_ok()).count();
            if (exactly_one && valid != 1) || valid == 0 {
                return Err(format!(
                    "{at} doesn't match {}",
                    if exactly_one { "exactly one of its allowed forms" } else { "any of its allowed forms" }
                ));
            }
        }
    }
    Ok(())
}

/// Replace `{file}` (the file name) and `{stem}` (without its extension) in every string of `v`.
fn substitute(v: &mut Value, file: &str, stem: &str, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    match v {
        Value::String(s) if s.contains('{') => *s = s.replace("{file}", file).replace("{stem}", stem),
        Value::Array(items) => items.iter_mut().for_each(|i| substitute(i, file, stem, depth.saturating_add(1))),
        Value::Object(m) => m.values_mut().for_each(|i| substitute(i, file, stem, depth.saturating_add(1))),
        _ => {}
    }
}

/// A file name safe to put into a path: separators, colons and control characters become `_`.
fn safe_name(name: &str) -> String {
    let mut s: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':') || c.is_control() { '_' } else { c }).collect();
    if s.chars().all(|c| c == '.') {
        s.insert(0, '_');
    }
    s
}

fn stem_of(name: &str) -> String {
    Path::new(name).file_stem().map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned())
}

/// Read at most `cap` bytes of a regular file (never through a link), or say why not.
fn read_capped(path: &Path, cap: u64) -> std::result::Result<Vec<u8>, String> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > cap {
        return Err(format!("{} bytes; at most {cap}", meta.len()));
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > cap {
        return Err(format!("more than {cap} bytes"));
    }
    Ok(bytes)
}

fn invalid(e: impl std::fmt::Display) -> ToolError {
    ToolError::InvalidArgs(e.to_string())
}

/// The first JSON value of a tool result.
fn first_json(content: &[Content]) -> Value {
    content
        .iter()
        .find_map(|c| match c {
            Content::Json(v) => Some(v.clone()),
            Content::Png { .. } => None,
        })
        .unwrap_or(Value::Null)
}

impl Automation {
    fn custom_library(&self, a: &Args) -> Result<PathBuf> {
        let lib = a.opt_str("library")?.unwrap_or(DEFAULT_LIBRARY);
        let dir = self.resolve(lib, true)?;
        if dir.exists() && !dir.is_dir() {
            return Err(failed(format!("{lib} is not a folder")));
        }
        Ok(dir)
    }

    /// Every command in the library, sorted by id, and the files that couldn't be read.
    fn custom_load_all(dir: &Path) -> Result<(Vec<Command>, Vec<Value>)> {
        let mut commands = Vec::new();
        let mut skipped = Vec::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((commands, skipped)),
            Err(e) => return Err(failed(format!("{}: {e}", dir.display()))),
        };
        let mut seen = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            seen = seen.saturating_add(1);
            let file = entry.file_name().to_string_lossy().into_owned();
            if seen > MAX_COMMANDS {
                skipped.push(json!({ "file": file, "reason": format!("the library holds more than {MAX_COMMANDS} commands") }));
                continue;
            }
            let parsed = read_capped(&path, MAX_COMMAND_FILE)
                .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| format!("not JSON: {e}")))
                .and_then(|v| Command::from_json(&v))
                .and_then(|c| if format!("{}.json", c.id) == file { Ok(c) } else { Err(format!("its id {:?} doesn't match the file name", c.id)) });
            match parsed {
                Ok(c) => commands.push(c),
                Err(reason) => skipped.push(json!({ "file": file, "reason": reason })),
            }
        }
        commands.sort_by(|a, b| a.id.cmp(&b.id));
        Ok((commands, skipped))
    }

    fn custom_load(dir: &Path, id: &str) -> Result<Command> {
        check_id(id).map_err(invalid)?;
        let path = dir.join(format!("{id}.json"));
        if std::fs::symlink_metadata(&path).is_err() {
            return Err(failed(format!("no custom command {id:?} (see custom_command_list)")));
        }
        read_capped(&path, MAX_COMMAND_FILE)
            .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| format!("not JSON: {e}")))
            .and_then(|v| Command::from_json(&v))
            .and_then(|c| if c.id == id { Ok(c) } else { Err(format!("its id {:?} doesn't match the file name", c.id)) })
            .map_err(|e| failed(format!("custom command {id:?} can't be read: {e}")))
    }

    fn custom_store(dir: &Path, c: &Command) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&c.to_json()).map_err(failed)?;
        write_atomic(&dir.join(format!("{}.json", c.id)), &bytes)
    }

    fn custom_count(dir: &Path) -> usize {
        std::fs::read_dir(dir).map(|e| e.flatten().filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json")).count()).unwrap_or(0)
    }

    pub(crate) fn custom_command_list(&self, a: &Args) -> Result<Value> {
        let dir = self.custom_library(a)?;
        let (commands, skipped) = Self::custom_load_all(&dir)?;
        Ok(json!({
            "library": dir.to_string_lossy(),
            "commands": commands.iter().map(Command::to_json).collect::<Vec<_>>(),
            "skipped": skipped,
            "tools": allowed_tools(),
        }))
    }

    pub(crate) fn custom_command_get(&self, a: &Args) -> Result<Value> {
        let dir = self.custom_library(a)?;
        let c = Self::custom_load(&dir, a.str("id")?)?;
        let def = target_tool(&c.tool).map_err(failed)?;
        let mut options = def.input_schema.clone();
        if let Some(p) = options.get_mut("properties").and_then(Value::as_object_mut) {
            p.remove("doc");
        }
        if let Some(r) = options.get_mut("required").and_then(Value::as_array_mut) {
            r.retain(|x| x != "doc");
        }
        let mut out = c.to_json();
        out["tool_title"] = json!(def.title);
        out["tool_description"] = json!(def.description);
        // What a prompt (an agent or a dialog) may set at run time, and what it must.
        out["options"] = if c.prompt_user { options } else { Value::Null };
        out["needs"] = json!(missing_required(def, &c.params));
        Ok(out)
    }

    pub(crate) fn custom_command_create(&self, a: &Args) -> Result<Value> {
        let dir = self.custom_library(a)?;
        let c = Command {
            id: a.str("id")?.to_string(),
            label: a.str("label")?.to_string(),
            description: a.opt_str("description")?.unwrap_or_default().to_string(),
            tool: a.str("tool")?.to_string(),
            params: match a.get("params") {
                None => Map::new(),
                Some(Value::Object(m)) => m.clone(),
                Some(_) => return Err(invalid("params must be an object")),
            },
            prompt_user: a.opt_bool("prompt_user")?.unwrap_or(false),
        };
        check_id(&c.id).map_err(invalid)?;
        check_label(&c.label).map_err(invalid)?;
        check_description(&c.description).map_err(invalid)?;
        validate(&c).map_err(invalid)?;
        if std::fs::symlink_metadata(dir.join(format!("{}.json", c.id))).is_ok() {
            return Err(failed(format!("a custom command {:?} already exists: edit it with custom_command_edit, or delete it first", c.id)));
        }
        if Self::custom_count(&dir) >= MAX_COMMANDS {
            return Err(failed(format!("the library already holds {MAX_COMMANDS} commands")));
        }
        Self::custom_store(&dir, &c)?;
        Ok(json!({ "created": c.to_json() }))
    }

    pub(crate) fn custom_command_edit(&self, a: &Args) -> Result<Value> {
        let dir = self.custom_library(a)?;
        let mut c = Self::custom_load(&dir, a.str("id")?)?;
        let before = c.clone();
        if let Some(l) = a.opt_str("label")? {
            check_label(l).map_err(invalid)?;
            c.label = l.to_string();
        }
        if let Some(d) = a.opt_str("description")? {
            check_description(d).map_err(invalid)?;
            c.description = d.to_string();
        }
        if let Some(t) = a.opt_str("tool")? {
            c.tool = t.to_string();
        }
        match a.get("params") {
            None => {}
            Some(Value::Object(m)) => c.params = m.clone(),
            Some(_) => return Err(invalid("params must be an object")),
        }
        if let Some(p) = a.opt_bool("prompt_user")? {
            c.prompt_user = p;
        }
        validate(&c).map_err(invalid)?;
        let changed = c != before;
        if changed {
            Self::custom_store(&dir, &c)?;
        }
        Ok(json!({ "edited": c.to_json(), "changed": changed }))
    }

    pub(crate) fn custom_command_delete(&self, a: &Args) -> Result<Value> {
        let dir = self.custom_library(a)?;
        let id = a.str("id")?;
        check_id(id).map_err(invalid)?;
        let path = dir.join(format!("{id}.json"));
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() || m.file_type().is_symlink() => {}
            Ok(_) => return Err(failed(format!("{id}.json is not a file"))),
            Err(_) => return Err(failed(format!("no custom command {id:?} (see custom_command_list)"))),
        }
        // Removing a link removes the link, never its target.
        std::fs::remove_file(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
        Ok(json!({ "deleted": id }))
    }

    pub(crate) fn custom_command_export(&self, a: &Args) -> Result<Value> {
        let dir = self.custom_library(a)?;
        let commands = match a.get("ids") {
            None => Self::custom_load_all(&dir)?.0,
            Some(_) => {
                let ids = a.strs("ids")?;
                if ids.len() > MAX_COMMANDS {
                    return Err(invalid(format!("at most {MAX_COMMANDS} ids")));
                }
                let mut seen = HashSet::new();
                let mut out = Vec::new();
                for id in ids {
                    if seen.insert(id) {
                        out.push(Self::custom_load(&dir, id)?);
                    }
                }
                out
            }
        };
        let target = self.resolve(a.str("path")?, true)?;
        let bundle = json!({ "format": BUNDLE_FORMAT, "version": VERSION, "commands": commands.iter().map(Command::to_json).collect::<Vec<_>>() });
        let bytes = serde_json::to_vec_pretty(&bundle).map_err(failed)?;
        write_atomic(&target, &bytes)?;
        Ok(json!({ "path": target.to_string_lossy(), "exported": commands.iter().map(|c| c.id.clone()).collect::<Vec<_>>() }))
    }

    pub(crate) fn custom_command_import(&self, a: &Args) -> Result<Value> {
        let dir = self.custom_library(a)?;
        let src = self.resolve(a.str("path")?, false)?;
        let replace = a.opt_bool("replace")?.unwrap_or(false);
        let bytes = read_capped(&src, MAX_IMPORT_FILE).map_err(|e| failed(format!("{}: {e}", src.display())))?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| failed(format!("{}: not JSON: {e}", src.display())))?;
        let items: Vec<Value> = match v.get("commands") {
            Some(list) => {
                if v.get("format").is_some_and(|f| f.as_str() != Some(BUNDLE_FORMAT)) {
                    return Err(failed(format!("a bundle's format must be {BUNDLE_FORMAT:?}")));
                }
                if let Some(ver) = v.get("version")
                    && ver.as_u64() != Some(VERSION)
                {
                    return Err(failed(format!("unsupported bundle version {ver} (this version reads {VERSION})")));
                }
                list.as_array().cloned().ok_or_else(|| failed("commands must be an array"))?
            }
            None => vec![v],
        };
        if items.len() > MAX_COMMANDS {
            return Err(failed(format!("the file holds {} commands; at most {MAX_COMMANDS}", items.len())));
        }
        // Validate everything before writing anything.
        let mut commands = Vec::new();
        let mut ids = HashSet::new();
        for (i, item) in items.iter().enumerate() {
            let c = Command::from_json(item).map_err(|e| failed(format!("command {}: {e}", i.saturating_add(1))))?;
            if !ids.insert(c.id.clone()) {
                return Err(failed(format!("the file holds {:?} twice", c.id)));
            }
            commands.push(c);
        }
        let existing: Vec<String> =
            commands.iter().filter(|c| std::fs::symlink_metadata(dir.join(format!("{}.json", c.id))).is_ok()).map(|c| c.id.clone()).collect();
        if !existing.is_empty() && !replace {
            return Err(failed(format!("the library already has {}: pass replace: true to overwrite them", existing.join(", "))));
        }
        let new = commands.len().saturating_sub(existing.len());
        if Self::custom_count(&dir).saturating_add(new) > MAX_COMMANDS {
            return Err(failed(format!("the library would hold more than {MAX_COMMANDS} commands")));
        }
        for c in &commands {
            Self::custom_store(&dir, c)?;
        }
        Ok(json!({ "imported": commands.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), "replaced": existing }))
    }

    /// The command's options for one run: presets, then (with prompt_user) the caller's.
    fn custom_effective(c: &Command, a: &Args) -> Result<(&'static tools::ToolDef, Map<String, Value>)> {
        let def = target_tool(&c.tool).map_err(failed)?;
        let mut params = c.params.clone();
        match a.get("params") {
            None => {}
            Some(Value::Object(over)) => {
                if !c.prompt_user {
                    return Err(invalid(format!(
                        "{} doesn't prompt for options (prompt_user is off), so params can't be given: edit the command to change its presets",
                        c.id
                    )));
                }
                check_params(def, over).map_err(invalid)?;
                for (k, v) in over {
                    params.insert(k.clone(), v.clone());
                }
            }
            Some(_) => return Err(invalid("params must be an object")),
        }
        let missing = missing_required(def, &params);
        if !missing.is_empty() {
            return Err(invalid(format!("{} asks for {}: pass them in params", c.id, missing.join(", "))));
        }
        check_params(def, &params).map_err(invalid)?;
        Ok((def, params))
    }

    pub(crate) fn custom_command_run(&mut self, a: &Args) -> Result<Vec<Content>> {
        let dir = self.custom_library(a)?;
        let c = Self::custom_load(&dir, a.str("id")?)?;
        let (def, params) = Self::custom_effective(&c, a)?;
        match (a.get("doc").is_some(), a.get("paths").is_some()) {
            (true, false) => {
                if a.get("folder").is_some() || a.get("password").is_some() || a.get("stop_on_error").is_some() {
                    return Err(invalid("folder, password and stop_on_error go with paths, not doc"));
                }
                let doc = self.doc(a)?;
                let (id, name) = (doc.id, doc.name.clone());
                let mut args = Value::Object(params);
                substitute(&mut args, &safe_name(&name), &safe_name(&stem_of(&name)), 0);
                let shown = args.clone();
                if let Some(m) = args.as_object_mut() {
                    m.insert("doc".into(), json!(id.0));
                }
                let mut content = self.call(def.name, &args)?;
                let result = first_json(&content);
                content.retain(|c| !matches!(c, Content::Json(_)));
                let mut out = vec![Content::Json(json!({ "command": c.id, "label": c.label, "tool": def.name, "params": shown, "result": result }))];
                out.extend(content);
                Ok(out)
            }
            (false, true) => self.custom_run_files(&c, def, &params, a).map(|v| vec![Content::Json(v)]),
            _ => Err(invalid("give doc (an open document) or paths (files, with folder for the results), not both")),
        }
    }

    fn custom_run_files(&mut self, c: &Command, def: &tools::ToolDef, params: &Map<String, Value>, a: &Args) -> Result<Value> {
        let paths = a.strs("paths")?;
        if paths.is_empty() || paths.len() > MAX_RUN_FILES {
            return Err(invalid(format!("paths must hold 1–{MAX_RUN_FILES} files")));
        }
        let folder_arg = a.str("folder")?;
        let folder = self.resolve(folder_arg, true)?;
        if folder.exists() && !folder.is_dir() {
            return Err(failed(format!("{folder_arg} is not a folder")));
        }
        let password = a.opt_str("password")?.map(str::to_string);
        let stop = a.opt_bool("stop_on_error")?.unwrap_or(false);
        let mut used: HashSet<PathBuf> = HashSet::new();
        let (mut completed, mut failures, mut files) = (0usize, 0usize, Vec::new());
        for p in paths {
            let r = self.custom_run_file(def, params, p, &folder, password.as_deref(), &mut used);
            match r {
                Ok(v) => {
                    completed = completed.saturating_add(1);
                    files.push(v);
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    files.push(json!({ "path": p, "error": e.to_string() }));
                    if stop {
                        break;
                    }
                }
            }
        }
        Ok(
            json!({ "command": c.id, "label": c.label, "tool": def.name, "folder": folder.to_string_lossy(), "completed": completed, "failed": failures, "files": files }),
        )
    }

    fn custom_run_file(
        &mut self,
        def: &tools::ToolDef,
        params: &Map<String, Value>,
        path: &str,
        folder: &Path,
        password: Option<&str>,
        used: &mut HashSet<PathBuf>,
    ) -> Result<Value> {
        let src = self.resolve(path, false)?;
        let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| failed(format!("{path} is not a file")))?;
        let target = folder.join(safe_name(&name));
        if !used.insert(target.clone()) {
            return Err(failed(format!("another file in this run already writes {}", target.display())));
        }
        if target.canonicalize().ok().zip(src.canonicalize().ok()).is_some_and(|(t, s)| t == s) {
            return Err(failed("the result would replace the input file: choose another folder"));
        }
        let mut open = json!({ "path": src.to_string_lossy() });
        if let Some(pw) = password {
            open["password"] = json!(pw);
        }
        let opened = first_json(&self.call("doc_open", &open)?);
        let id = opened["doc"].as_u64().ok_or_else(|| failed("doc_open returned no document id"))?;
        let outcome = self.custom_apply_and_save(def, params, id, &name, &target);
        // Close it whatever happened: the result is saved, or the file is unchanged on disk.
        let _ = self.call("doc_close", &json!({ "doc": id, "discard_changes": true }));
        let (result, saved) = outcome?;
        let mut v = json!({ "path": path, "result": result });
        v["output"] = if saved { json!(target.to_string_lossy()) } else { Value::Null };
        if !saved {
            v["unchanged"] = json!(true);
        }
        Ok(v)
    }

    fn custom_apply_and_save(
        &mut self,
        def: &tools::ToolDef,
        params: &Map<String, Value>,
        id: u64,
        name: &str,
        target: &Path,
    ) -> Result<(Value, bool)> {
        let mut args = Value::Object(params.clone());
        substitute(&mut args, &safe_name(name), &safe_name(&stem_of(name)), 0);
        if let Some(m) = args.as_object_mut() {
            m.insert("doc".into(), json!(id));
        }
        let result = first_json(&self.call(def.name, &args)?);
        let dirty = self.session.get(pdfcraft_engine::DocId(id)).is_some_and(|d| d.dirty);
        if dirty {
            self.call("doc_save", &json!({ "doc": id, "path": target.to_string_lossy() }))?;
        }
        Ok((result, dirty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_checks_types_ranges_and_items() {
        let s = json!({ "type": "array", "items": { "type": "integer", "minimum": 1 }, "maxItems": 2 });
        assert!(check_value(&s, &json!([1, 2]), "pages", 0).is_ok());
        assert!(check_value(&s, &json!([0]), "pages", 0).is_err());
        assert!(check_value(&s, &json!([1, 2, 3]), "pages", 0).is_err());
        assert!(check_value(&s, &json!("1"), "pages", 0).is_err());
        let e = json!({ "type": "string", "enum": ["a", "b"] });
        assert!(check_value(&e, &json!("c"), "x", 0).is_err());
        let n = json!({ "type": ["string", "null"] });
        assert!(check_value(&n, &Value::Null, "x", 0).is_ok());
        assert!(check_value(&n, &json!(3), "x", 0).is_err());
        let o = json!({ "type": "object", "properties": { "a": { "type": "boolean" } }, "required": ["a"], "additionalProperties": false });
        assert!(check_value(&o, &json!({ "a": true }), "x", 0).is_ok());
        assert!(check_value(&o, &json!({ "a": true, "b": 1 }), "x", 0).is_err());
        assert!(check_value(&o, &json!({}), "x", 0).is_err());
    }

    #[test]
    fn deep_schemas_and_values_stop() {
        let mut s = json!({ "type": "array" });
        for _ in 0..100 {
            s = json!({ "type": "array", "items": s });
        }
        let mut v = json!([]);
        for _ in 0..100 {
            v = json!([v]);
        }
        assert!(check_value(&s, &v, "x", 0).is_err());
        substitute(&mut v, "a", "b", 0);
    }

    #[test]
    fn ids_are_file_name_safe() {
        for bad in ["", "../x", "A", "a/b", "a.json", "-a", &"a".repeat(65), "a b", "é"] {
            assert!(check_id(bad).is_err(), "{bad}");
        }
        assert!(check_id("ocr-150dpi_2").is_ok());
    }
}
