//! Redaction code sets the user made (Redaction Properties ▸ Edit code sets): kept on the
//! [`Session`] beside the built-in sets ([`crate::REDACTION_CODE_SETS`]), saved with the app's
//! settings, and imported from or exported to a JSON file:
//!
//! ```json
//! { "code_sets": [ { "name": "Contract", "codes": [ { "code": "C-1", "description": "Pricing" } ] } ] }
//! ```
//!
//! A single set object (`{"name": …, "codes": […]}`) is read as well, and a code may be a plain
//! string. Files and settings are untrusted: [`parse_code_sets`] caps their size and checks every
//! set with [`CustomCodeSet::new`].

use pdfcraft_redact::codes::{CustomCodeSet, MAX_CUSTOM_SETS, save_custom_set};
use serde_json::{Value, json};

use crate::Session;

/// The largest code set file read.
pub const MAX_CODE_SET_FILE: usize = 1 << 20;

fn read_set(v: &Value) -> Result<CustomCodeSet, String> {
    let name = v.get("name").and_then(Value::as_str).ok_or("each code set needs a name")?;
    let list = v.get("codes").and_then(Value::as_array).ok_or_else(|| format!("{name:?} needs a list of codes"))?;
    let codes: Vec<(String, String)> = list
        .iter()
        .take(pdfcraft_redact::codes::MAX_CUSTOM_CODES.saturating_add(1))
        .map(|c| match c {
            Value::String(s) => Ok((s.clone(), String::new())),
            Value::Object(_) => Ok((
                c.get("code").and_then(Value::as_str).ok_or("each code needs its text (\"code\")")?.to_string(),
                c.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
            )),
            _ => Err("a code is a string or an object with \"code\" and \"description\"".to_string()),
        })
        .collect::<Result<_, _>>()?;
    CustomCodeSet::new(name, &codes)
}

/// The code sets in a file (see the module docs). Refuses files over [`MAX_CODE_SET_FILE`]
/// bytes, sets that fail [`CustomCodeSet::new`], and more than [`MAX_CUSTOM_SETS`] sets.
pub fn parse_code_sets(bytes: &[u8]) -> Result<Vec<CustomCodeSet>, String> {
    if bytes.len() > MAX_CODE_SET_FILE {
        return Err(format!("the file is larger than {} KiB; a code set file is small", MAX_CODE_SET_FILE / 1024));
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("not a code set file (JSON): {e}"))?;
    let sets: Vec<&Value> = match v.get("code_sets").and_then(Value::as_array) {
        Some(list) => list.iter().collect(),
        None if v.get("name").is_some() => vec![&v],
        None => return Err("not a code set file: expected \"code_sets\" (a list) or one set with \"name\" and \"codes\"".into()),
    };
    if sets.len() > MAX_CUSTOM_SETS {
        return Err(format!("the file holds {} code sets; at most {MAX_CUSTOM_SETS} are kept", sets.len()));
    }
    let mut out: Vec<CustomCodeSet> = Vec::new();
    for s in sets {
        save_custom_set(&mut out, read_set(s)?)?;
    }
    if out.is_empty() {
        return Err("the file holds no code sets".into());
    }
    Ok(out)
}

/// `sets` as a code set file.
pub fn code_sets_json(sets: &[CustomCodeSet]) -> Value {
    json!({
        "code_sets": sets.iter().map(|s| json!({
            "name": s.name,
            "codes": s.codes.iter().map(|c| json!({ "code": c.code, "description": c.description })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

impl Session {
    /// The redaction code sets the user made.
    pub fn redaction_code_sets(&self) -> &[CustomCodeSet] {
        &self.redaction_code_sets
    }

    /// The user's set named `name` (ignoring case).
    pub fn redaction_code_set(&self, name: &str) -> Option<&CustomCodeSet> {
        self.redaction_code_sets.iter().find(|s| s.name.eq_ignore_ascii_case(name.trim()))
    }

    /// Add `set`, or replace the user's set of that name.
    pub fn save_redaction_code_set(&mut self, set: CustomCodeSet) -> Result<(), String> {
        save_custom_set(&mut self.redaction_code_sets, set)
    }

    /// Delete the user's set named `name`. Returns whether there was one.
    pub fn delete_redaction_code_set(&mut self, name: &str) -> bool {
        let before = self.redaction_code_sets.len();
        self.redaction_code_sets.retain(|s| !s.name.eq_ignore_ascii_case(name.trim()));
        before != self.redaction_code_sets.len()
    }

    /// Import a code set file: its sets join the user's (a set of an existing name replaces
    /// it). Nothing changes when the file can't be read. Returns the names imported.
    pub fn import_redaction_code_sets(&mut self, bytes: &[u8]) -> Result<Vec<String>, String> {
        let sets = parse_code_sets(bytes)?;
        let mut all = self.redaction_code_sets.clone();
        let names = sets.iter().map(|s| s.name.clone()).collect();
        for s in sets {
            save_custom_set(&mut all, s)?;
        }
        self.redaction_code_sets = all;
        Ok(names)
    }

    /// Restore saved settings leniently: sets that can't be read are skipped. Returns how many
    /// were skipped.
    pub fn restore_redaction_code_sets(&mut self, v: &Value) -> usize {
        let mut skipped = 0usize;
        let mut out = Vec::new();
        for s in v.get("code_sets").and_then(Value::as_array).into_iter().flatten().take(MAX_CUSTOM_SETS.saturating_mul(2)) {
            match read_set(s).and_then(|set| save_custom_set(&mut out, set)) {
                Ok(()) => {}
                Err(_) => skipped = skipped.saturating_add(1),
            }
        }
        self.redaction_code_sets = out;
        skipped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_set_files_round_trip_and_hostile_files_are_refused() {
        let mut s = Session::default();
        let file = br#"{"code_sets":[{"name":"Contract","codes":[{"code":"C-1","description":"Pricing"},"C-2"]}]}"#;
        assert_eq!(s.import_redaction_code_sets(file).unwrap(), ["Contract"]);
        let set = s.redaction_code_set("contract").unwrap().clone();
        assert_eq!(set.codes.len(), 2);
        let out = serde_json::to_vec(&code_sets_json(s.redaction_code_sets())).unwrap();
        assert_eq!(parse_code_sets(&out).unwrap(), vec![set.clone()]);
        // One set on its own is read too; the same name replaces.
        assert_eq!(s.import_redaction_code_sets(br#"{"name":"CONTRACT","codes":["Z"]}"#).unwrap(), ["CONTRACT"]);
        assert_eq!(s.redaction_code_sets().len(), 1);
        for bad in [&b"not json"[..], br#"{"code_sets":[]}"#, br#"{"code_sets":[{"name":"x","codes":[1]}]}"#, br#"[1,2]"#, br#"{"name":"x"}"#] {
            assert!(s.import_redaction_code_sets(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        assert_eq!(s.redaction_code_sets().len(), 1, "a refused file changes nothing");
        let huge = vec![b' '; MAX_CODE_SET_FILE + 1];
        assert!(parse_code_sets(&huge).is_err());
        let nested = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(parse_code_sets(nested.as_bytes()).is_err());
        // Settings: bad sets are skipped.
        let saved = json!({"code_sets": [{"name": "ok", "codes": ["a"]}, {"name": "", "codes": ["b"]}, 7]});
        let mut t = Session::default();
        assert_eq!(t.restore_redaction_code_sets(&saved), 2);
        assert_eq!(t.redaction_code_sets().len(), 1);
        assert!(t.delete_redaction_code_set("OK") && !t.delete_redaction_code_set("ok"));
    }
}
