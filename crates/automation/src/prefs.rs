//! Preferences by key (`prefs_get`, `prefs_set`): the schema of [`pdfcraft_engine::prefs`],
//! the values now and changes to them. Keys that apply to the session (identity name, signing
//! reason and location, JavaScript, internet access, colour management, the date format) change
//! what the other tools do; `app` keys are kept for the desktop app, which reads the same keys.

use pdfcraft_engine::Session;
use pdfcraft_engine::prefs::{CATEGORIES, SCHEMA, schema_json, spec_of};
use serde_json::{Map, Value, json};

use crate::{Args, Automation, Result, ToolError};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

fn check_category(category: Option<&str>) -> Result<()> {
    match category {
        Some(c) if !CATEGORIES.contains(&c) => Err(bad(format!("unknown category {c:?}; use one of {}", CATEGORIES.join(", ")))),
        _ => Ok(()),
    }
}

fn check_key(key: &str) -> Result<()> {
    if spec_of(key).is_none() {
        let keys: Vec<&str> = SCHEMA.iter().map(|s| s.key).collect();
        return Err(bad(format!("unknown preference {key:?}; the keys are {}", keys.join(", "))));
    }
    Ok(())
}

impl Automation {
    /// Values (all, one category or one key), and with `schema` their types, ranges and defaults.
    pub(crate) fn prefs_get(&self, a: &Args) -> Result<Value> {
        let category = a.opt_str("category")?;
        check_category(category)?;
        let mut out = match a.opt_str("key")? {
            Some(key) => {
                check_key(key)?;
                json!({ "key": key, "value": self.session.pref(key) })
            }
            None => json!({ "values": self.session.prefs_json(category) }),
        };
        if a.opt_bool("schema")?.unwrap_or(false) {
            let schema: Vec<Value> = schema_json(&Session::new())
                .into_iter()
                .filter(|s| category.is_none_or(|c| s["category"] == c))
                .filter(|s| a.opt_str("key").ok().flatten().is_none_or(|k| s["key"] == k))
                .collect();
            out["schema"] = json!(schema);
            out["categories"] = json!(CATEGORIES);
        }
        Ok(out)
    }

    /// Set one preference (`key` + `value`), several (`values`), or put keys back to their
    /// defaults (`reset`), all or none.
    pub(crate) fn prefs_set(&mut self, a: &Args) -> Result<Value> {
        let mut values: Map<String, Value> = match a.get("values") {
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err(bad("values must be an object of preference keys and values")),
            None => Map::new(),
        };
        match (a.opt_str("key")?, a.get("value")) {
            (Some(key), Some(value)) => {
                values.insert(key.to_string(), value.clone());
            }
            (Some(key), None) => return Err(bad(format!("give value for {key:?} (or use reset)"))),
            (None, Some(_)) => return Err(bad("value needs key")),
            (None, None) => {}
        }
        let reset: Vec<String> = match a.get("reset") {
            None => Vec::new(),
            Some(Value::Bool(true)) => SCHEMA.iter().map(|s| s.key.to_string()).collect(),
            Some(Value::Bool(false)) => Vec::new(),
            Some(Value::Array(keys)) => keys
                .iter()
                .map(|k| k.as_str().map(str::to_string).ok_or_else(|| bad("reset must be true or an array of keys")))
                .collect::<Result<_>>()?,
            Some(_) => return Err(bad("reset must be true or an array of keys")),
        };
        if values.is_empty() && reset.is_empty() {
            return Err(bad("nothing to set: give key and value, values, or reset"));
        }
        for key in values.keys().chain(reset.iter()) {
            check_key(key)?;
        }
        // Defaults first, then the values given; one all-or-none batch.
        let defaults = Session::new();
        let mut batch = Map::new();
        for key in &reset {
            if !values.contains_key(key)
                && let Some(d) = defaults.pref(key)
            {
                batch.insert(key.clone(), d);
            }
        }
        batch.extend(values);
        let stored = self.session.set_prefs(&batch).map_err(bad)?;
        let changed: Map<String, Value> = stored.iter().map(|(k, p)| (k.clone(), p.value.clone())).collect();
        let clamped: Vec<&str> = stored.iter().filter(|(_, p)| p.clamped).map(|(k, _)| k.as_str()).collect();
        Ok(json!({ "values": changed, "clamped": clamped }))
    }
}
