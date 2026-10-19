//! Preferences: one schema for the settings that aren't stored with documents, read and written
//! by key (`documents.autosave_minutes`, `identity.name`, `color.intent`, …). The desktop app's
//! Preferences window, its control channel and the `prefs_get` / `prefs_set` automation tools
//! all go through [`Session::pref`] and [`Session::set_pref`].
//!
//! Some keys are held here ([`Preferences`]); others are views of session settings that already
//! existed (JavaScript, internet access, colour management, the date format), so a key always
//! changes the same thing the rest of PdfCraft reads. Every value is untrusted: numbers are
//! clamped into their range, text is cut to its length and stripped of control characters, and
//! anything of the wrong type is refused with a message that says what is allowed.
//! [`Session::load_prefs`] reads saved settings leniently: an unknown key or an unusable value
//! keeps the default, so an old or corrupt settings file never stops PdfCraft from starting.

use serde_json::{Map, Value, json};

use crate::Session;
use crate::color_settings::{ColorSettings, ProfileFamily, UntaggedPolicy};
use crate::preflight::OverprintPreview;
use crate::url_access::{HostAction, UrlMode};

/// Recovery copies of unsaved changes: every minute by default; 0 turns autosave off.
pub const DEFAULT_AUTOSAVE_MINUTES: u32 = 1;
pub const MAX_AUTOSAVE_MINUTES: u32 = 120;
/// Files listed under Recent (Home and File ▸ Open Recent).
pub const DEFAULT_RECENT_FILES: u32 = 12;
pub const MAX_RECENT_FILES: u32 = 50;
/// The longest identity, reason or location text kept.
pub const MAX_TEXT_CHARS: usize = 200;
/// Interface text size, as a factor of the normal size.
pub const MIN_TEXT_SCALE: f32 = 0.75;
pub const MAX_TEXT_SCALE: f32 = 2.0;
/// Form field highlights (Highlight existing fields): the tint and border of fields, and the
/// border of required fields.
pub const DEFAULT_FIELD_HIGHLIGHT: [u8; 3] = [0x6E, 0x8E, 0xF5];
pub const DEFAULT_REQUIRED_HIGHLIGHT: [u8; 3] = [0xE3, 0x22, 0x22];

/// Who you are (Acrobat: Preferences ▸ Identity). An empty name means the default: the login
/// name in the app, "PdfCraft" for automation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub organization: String,
    pub unit: String,
    pub email: String,
}

/// The preferences held by the session itself (the rest of the schema reads settings that live
/// elsewhere in [`Session`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Preferences {
    /// Minutes between recovery copies of unsaved changes; 0 is off.
    pub autosave_minutes: u32,
    /// How many recent files are remembered.
    pub recent_files: u32,
    pub identity: Identity,
    /// Field highlight colour, and the border of required fields.
    pub field_highlight: [u8; 3],
    pub required_highlight: [u8; 3],
    /// Signatures: the reason and location a new signature starts with.
    pub signing_reason: String,
    pub signing_location: String,
    /// Accessibility: interface text size factor.
    pub text_scale: f32,
    /// Accessibility: no animations or smooth scrolling.
    pub reduce_motion: bool,
    /// Accessibility: stronger text and borders in every theme.
    pub high_contrast: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            autosave_minutes: DEFAULT_AUTOSAVE_MINUTES,
            recent_files: DEFAULT_RECENT_FILES,
            identity: Identity::default(),
            field_highlight: DEFAULT_FIELD_HIGHLIGHT,
            required_highlight: DEFAULT_REQUIRED_HIGHLIGHT,
            signing_reason: String::new(),
            signing_location: String::new(),
            text_scale: 1.0,
            reduce_motion: false,
            high_contrast: false,
        }
    }
}

/// The type of a preference's value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// `true` or `false` (also "on"/"off").
    Bool,
    /// A whole number, clamped into `min..=max`.
    Integer { min: i64, max: i64 },
    /// A number, clamped into `min..=max`.
    Number { min: f64, max: f64 },
    /// Text, cut to `max_chars` characters.
    Text { max_chars: usize },
    /// `"#RRGGBB"` or `[r, g, b]` (0–255 each).
    Color,
    /// One of these ids.
    Choice(&'static [&'static str]),
    /// A colour profile id of this family (`color_profiles_list`).
    Profile(ProfileFamily),
}

/// One preference.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub key: &'static str,
    /// The Preferences page it is on: general, documents, commenting, forms, identity,
    /// signatures, security, accessibility or color.
    pub category: &'static str,
    pub kind: Kind,
    pub description: &'static str,
    /// Where it takes effect: `session` (the engine and automation tools) or `app` (the desktop
    /// app only; automation keeps the value for the app to read).
    pub applies: &'static str,
    /// Written by [`Session::saved_prefs`]. Keys that the app already saves under their own
    /// names (JavaScript, internet access, the date format) are not.
    pub saved: bool,
}

const fn spec(key: &'static str, category: &'static str, kind: Kind, applies: &'static str, saved: bool, description: &'static str) -> Spec {
    Spec { key, category, kind, description, applies, saved }
}

const TEXT: Kind = Kind::Text { max_chars: MAX_TEXT_CHARS };

/// Every preference the session knows, in Preferences-window order.
pub const SCHEMA: &[Spec] = &[
    spec(
        "documents.autosave_minutes",
        "documents",
        Kind::Integer { min: 0, max: MAX_AUTOSAVE_MINUTES as i64 },
        "app",
        true,
        "Minutes between recovery copies of documents with unsaved changes (0 turns autosave off; default 1).",
    ),
    spec(
        "documents.recent_files",
        "documents",
        Kind::Integer { min: 0, max: MAX_RECENT_FILES as i64 },
        "app",
        true,
        "How many recent files Home and File ▸ Open Recent list (default 12).",
    ),
    spec("forms.highlight_color", "forms", Kind::Color, "app", true, "Fill and border colour of highlighted form fields (default #6E8EF5)."),
    spec("forms.required_color", "forms", Kind::Color, "app", true, "Border colour of highlighted required fields (default #E32222)."),
    spec(
        "forms.date_format",
        "forms",
        Kind::Text { max_chars: crate::dates::MAX_DATE_FORMAT_CHARS },
        "session",
        false,
        "The pattern Fill & Sign dates use, e.g. m/d/yyyy (the default) or d mmmm yyyy.",
    ),
    spec(
        "identity.name",
        "identity",
        TEXT,
        "session",
        true,
        "Your name: the author of new comments (empty: the login name in the app, PdfCraft for automation).",
    ),
    spec("identity.organization", "identity", TEXT, "app", true, "Organization: suggested for new digital IDs."),
    spec("identity.unit", "identity", TEXT, "app", true, "Organizational unit: suggested for new digital IDs."),
    spec("identity.email", "identity", TEXT, "app", true, "Email address: suggested for new digital IDs."),
    spec("signatures.reason", "signatures", TEXT, "session", true, "The reason a new signature starts with (empty: none)."),
    spec("signatures.location", "signatures", TEXT, "session", true, "The location a new signature starts with (empty: none)."),
    spec(
        "security.javascript",
        "security",
        Kind::Bool,
        "session",
        false,
        "Enable Acrobat JavaScript (default true). Scripts run in a sandbox without file or network access.",
    ),
    spec(
        "security.internet_access",
        "security",
        Kind::Choice(&["custom", "allow-all", "block-all"]),
        "session",
        false,
        "Internet access from PDFs: custom (the site rules, then unlisted_sites; the default), allow-all or block-all.",
    ),
    spec(
        "security.unlisted_sites",
        "security",
        Kind::Choice(&["ask", "allow", "block"]),
        "session",
        false,
        "With custom internet access, what happens with a site that has no rule: ask (the default), allow or block.",
    ),
    spec(
        "accessibility.text_scale",
        "accessibility",
        Kind::Number { min: MIN_TEXT_SCALE as f64, max: MAX_TEXT_SCALE as f64 },
        "app",
        true,
        "Interface text and control size, as a factor (0.75 to 2; default 1).",
    ),
    spec("accessibility.reduce_motion", "accessibility", Kind::Bool, "app", true, "Turn off animations and smooth scrolling (default false)."),
    spec(
        "accessibility.high_contrast",
        "accessibility",
        Kind::Bool,
        "app",
        true,
        "Stronger text, borders and focus outlines in every theme (default false).",
    ),
    spec("color.rgb", "color", Kind::Profile(ProfileFamily::Rgb), "session", true, "RGB working space profile id (default srgb)."),
    spec("color.cmyk", "color", Kind::Profile(ProfileFamily::Cmyk), "session", true, "CMYK working space profile id."),
    spec("color.gray", "color", Kind::Profile(ProfileFamily::Gray), "session", true, "Gray working space profile id (default sgray)."),
    spec(
        "color.intent",
        "color",
        Kind::Choice(&["perceptual", "relative_colorimetric", "saturation", "absolute_colorimetric"]),
        "session",
        true,
        "Rendering intent of colour conversions (default relative_colorimetric).",
    ),
    spec("color.black_point_compensation", "color", Kind::Bool, "session", true, "Black point compensation (default false)."),
    spec(
        "color.untagged",
        "color",
        Kind::Choice(&["output_intent", "working_space", "unmanaged"]),
        "session",
        true,
        "Untagged colour: output_intent (a document's output intent replaces the working space; the default), working_space or unmanaged.",
    ),
    spec(
        "color.overprint_preview",
        "color",
        Kind::Choice(&["never", "always", "pdfx", "automatic"]),
        "session",
        true,
        "Use overprint preview: never, always, pdfx (only for PDF/X files; the default) or automatic.",
    ),
];

/// The spec of `key`.
pub fn spec_of(key: &str) -> Option<&'static Spec> {
    SCHEMA.iter().find(|s| s.key == key)
}

/// The categories, in Preferences-window order.
pub const CATEGORIES: [&str; 9] = ["general", "documents", "commenting", "forms", "identity", "signatures", "security", "accessibility", "color"];

/// A value parsed for `kind`: the stored value, and whether it had to be clamped or cut.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub value: Value,
    pub clamped: bool,
}

fn wrong(key: &str, what: &str) -> String {
    format!("{key} must be {what}")
}

/// `"on"`, `"off"`, `"true"`, `"false"` or a JSON boolean.
fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Some(true),
            "false" | "off" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A finite number, or a string holding one.
fn as_number(v: &Value) -> Option<f64> {
    let n = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }?;
    n.is_finite().then_some(n)
}

/// Text without control characters, trimmed and cut to `max` characters.
pub fn clean_text(s: &str, max: usize) -> (String, bool) {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    let cut: String = trimmed.chars().take(max).collect();
    let changed = cut.len() != s.len();
    (cut, changed)
}

/// `#RRGGBB`, `RRGGBB` or `[r, g, b]`.
pub fn parse_color(v: &Value) -> Option<([u8; 3], bool)> {
    match v {
        Value::String(s) => {
            let hex = s.trim().trim_start_matches('#');
            if hex.len() != 6 || !hex.is_ascii() {
                return None;
            }
            let byte = |i: usize| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok());
            Some(([byte(0)?, byte(2)?, byte(4)?], false))
        }
        Value::Array(a) if a.len() == 3 => {
            let mut out = [0u8; 3];
            let mut clamped = false;
            for (slot, x) in out.iter_mut().zip(a) {
                let n = as_number(x)?;
                let c = n.round().clamp(0.0, 255.0);
                clamped |= c != n;
                *slot = c as u8;
            }
            Some((out, clamped))
        }
        _ => None,
    }
}

/// `[r, g, b]` as `#RRGGBB`.
pub fn color_hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// Check `v` against `spec`, clamping numbers and cutting text.
pub fn parse(spec: &Spec, v: &Value) -> Result<Parsed, String> {
    let key = spec.key;
    match spec.kind {
        Kind::Bool => as_bool(v).map(|b| Parsed { value: json!(b), clamped: false }).ok_or_else(|| wrong(key, "true or false")),
        Kind::Integer { min, max } => {
            let n = as_number(v).ok_or_else(|| wrong(key, &format!("a whole number from {min} to {max}")))?;
            let r = n.round().clamp(min as f64, max as f64);
            Ok(Parsed { value: json!(r as i64), clamped: r != n })
        }
        Kind::Number { min, max } => {
            let n = as_number(v).ok_or_else(|| wrong(key, &format!("a number from {min} to {max}")))?;
            let r = n.clamp(min, max);
            // Two decimals are plenty for a size factor, and keep the saved value tidy.
            let r = (r * 100.0).round() / 100.0;
            Ok(Parsed { value: json!(r), clamped: (r - n).abs() > 1e-9 })
        }
        Kind::Text { max_chars } => {
            let s = v.as_str().ok_or_else(|| wrong(key, "text"))?;
            let (text, clamped) = clean_text(s, max_chars);
            Ok(Parsed { value: json!(text), clamped })
        }
        Kind::Color => parse_color(v)
            .map(|(c, clamped)| Parsed { value: json!(color_hex(c)), clamped })
            .ok_or_else(|| wrong(key, "a colour: \"#RRGGBB\" or [r, g, b]")),
        Kind::Choice(ids) => {
            let s = v.as_str().map(str::trim).unwrap_or_default();
            ids.iter()
                .find(|id| id.eq_ignore_ascii_case(s))
                .map(|id| Parsed { value: json!(id), clamped: false })
                .ok_or_else(|| wrong(key, &format!("one of {}", ids.join(", "))))
        }
        Kind::Profile(_) => {
            let s = v.as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| wrong(key, "a profile id (color_profiles_list)"))?;
            Ok(Parsed { value: json!(s.chars().take(crate::color_settings::MAX_PROFILE_NAME + 8).collect::<String>()), clamped: false })
        }
    }
}

/// The schema as JSON, with each default (`prefs_get`).
pub fn schema_json(defaults: &Session) -> Vec<Value> {
    SCHEMA
        .iter()
        .map(|s| {
            let mut v = json!({
                "key": s.key,
                "category": s.category,
                "description": s.description,
                "applies": s.applies,
                "default": defaults.pref(s.key),
            });
            let (ty, extra) = match s.kind {
                Kind::Bool => ("boolean", json!({})),
                Kind::Integer { min, max } => ("integer", json!({ "minimum": min, "maximum": max })),
                Kind::Number { min, max } => ("number", json!({ "minimum": min, "maximum": max })),
                Kind::Text { max_chars } => ("string", json!({ "maxLength": max_chars })),
                Kind::Color => ("color", json!({ "pattern": "#RRGGBB or [r, g, b]" })),
                Kind::Choice(ids) => ("string", json!({ "enum": ids })),
                Kind::Profile(f) => ("string", json!({ "profile_family": crate::color_settings::family_id(f) })),
            };
            v["type"] = json!(ty);
            if let (Value::Object(o), Value::Object(e)) = (&mut v, extra) {
                o.extend(e);
            }
            v
        })
        .collect()
}

impl Session {
    /// The preferences the session holds.
    pub fn prefs(&self) -> &Preferences {
        &self.prefs
    }

    /// The current value of preference `key` (`None` for an unknown key).
    pub fn pref(&self, key: &str) -> Option<Value> {
        let p = &self.prefs;
        let c = self.color_management().settings();
        Some(match key {
            "documents.autosave_minutes" => json!(p.autosave_minutes),
            "documents.recent_files" => json!(p.recent_files),
            "forms.highlight_color" => json!(color_hex(p.field_highlight)),
            "forms.required_color" => json!(color_hex(p.required_highlight)),
            "forms.date_format" => json!(self.date_format()),
            "identity.name" => json!(p.identity.name),
            "identity.organization" => json!(p.identity.organization),
            "identity.unit" => json!(p.identity.unit),
            "identity.email" => json!(p.identity.email),
            "signatures.reason" => json!(p.signing_reason),
            "signatures.location" => json!(p.signing_location),
            "security.javascript" => json!(self.javascript()),
            "security.internet_access" => json!(self.url_policy().mode.id()),
            "security.unlisted_sites" => json!(self.url_policy().default_action.id()),
            "accessibility.text_scale" => json!((p.text_scale as f64 * 100.0).round() / 100.0),
            "accessibility.reduce_motion" => json!(p.reduce_motion),
            "accessibility.high_contrast" => json!(p.high_contrast),
            "color.rgb" => json!(c.rgb),
            "color.cmyk" => json!(c.cmyk),
            "color.gray" => json!(c.gray),
            "color.intent" => json!(c.intent.name()),
            "color.black_point_compensation" => json!(c.black_point_compensation),
            "color.untagged" => json!(c.untagged.id()),
            "color.overprint_preview" => json!(self.overprint_preview().id()),
            _ => return None,
        })
    }

    /// Every preference (or those of `category`) by key.
    pub fn prefs_json(&self, category: Option<&str>) -> Map<String, Value> {
        SCHEMA.iter().filter(|s| category.is_none_or(|c| c == s.category)).filter_map(|s| Some((s.key.to_string(), self.pref(s.key)?))).collect()
    }

    /// The preferences to save with the app's settings ([`Spec::saved`]).
    pub fn saved_prefs(&self) -> Value {
        Value::Object(SCHEMA.iter().filter(|s| s.saved).filter_map(|s| Some((s.key.to_string(), self.pref(s.key)?))).collect())
    }

    /// Read saved preferences leniently: unknown keys and unusable values keep what is set now
    /// (the defaults, at startup). Returns the keys that couldn't be used.
    pub fn load_prefs(&mut self, saved: &Value) -> Vec<String> {
        let Some(map) = saved.as_object() else { return Vec::new() };
        let mut skipped = Vec::new();
        // Only keys of the schema, so a huge or hostile object costs one lookup per key at most.
        for s in SCHEMA.iter().filter(|s| s.saved) {
            if let Some(v) = map.get(s.key)
                && self.set_pref(s.key, v).is_err()
            {
                skipped.push(s.key.to_string());
            }
        }
        skipped
    }

    /// Set preference `key` to `value` (validated, clamped and applied where PdfCraft reads it).
    /// Returns what was stored.
    pub fn set_pref(&mut self, key: &str, value: &Value) -> Result<Parsed, String> {
        let spec = spec_of(key).ok_or_else(|| format!("unknown preference {key:?} (prefs_get lists them)"))?;
        let parsed = parse(spec, value)?;
        let v = &parsed.value;
        let text = || v.as_str().unwrap_or_default().to_string();
        let int = || v.as_i64().unwrap_or(0).clamp(0, u32::MAX as i64) as u32;
        let flag = || v.as_bool().unwrap_or(false);
        let color = || parse_color(v).map(|(c, _)| c).unwrap_or([0, 0, 0]);
        let p = &mut self.prefs;
        match key {
            "documents.autosave_minutes" => p.autosave_minutes = int(),
            "documents.recent_files" => p.recent_files = int(),
            "forms.highlight_color" => p.field_highlight = color(),
            "forms.required_color" => p.required_highlight = color(),
            "forms.date_format" => self.set_date_format(&text())?,
            "identity.name" => p.identity.name = text(),
            "identity.organization" => p.identity.organization = text(),
            "identity.unit" => p.identity.unit = text(),
            "identity.email" => p.identity.email = text(),
            "signatures.reason" => p.signing_reason = text(),
            "signatures.location" => p.signing_location = text(),
            "accessibility.text_scale" => p.text_scale = (v.as_f64().unwrap_or(1.0) as f32).clamp(MIN_TEXT_SCALE, MAX_TEXT_SCALE),
            "accessibility.reduce_motion" => p.reduce_motion = flag(),
            "accessibility.high_contrast" => p.high_contrast = flag(),
            "security.javascript" => self.set_javascript(flag()),
            "security.internet_access" => {
                let mut policy = self.url_policy().clone();
                policy.mode = UrlMode::from_id(&text()).ok_or_else(|| wrong(key, "custom, allow-all or block-all"))?;
                self.set_url_policy(&policy);
            }
            "security.unlisted_sites" => {
                let mut policy = self.url_policy().clone();
                policy.default_action = HostAction::from_id(&text()).ok_or_else(|| wrong(key, "ask, allow or block"))?;
                self.set_url_policy(&policy);
            }
            "color.overprint_preview" => {
                self.set_overprint_preview(OverprintPreview::from_id(&text()).ok_or_else(|| wrong(key, "never, always, pdfx or automatic"))?)
            }
            _ if key.starts_with("color.") => {
                let mut s: ColorSettings = self.color_management().settings().clone();
                match key {
                    "color.rgb" => s.rgb = text(),
                    "color.cmyk" => s.cmyk = text(),
                    "color.gray" => s.gray = text(),
                    "color.intent" => s.intent = crate::RenderingIntent::from_name(&text()).ok_or_else(|| wrong(key, "a rendering intent"))?,
                    "color.black_point_compensation" => s.black_point_compensation = flag(),
                    "color.untagged" => s.untagged = UntaggedPolicy::from_id(&text()).ok_or_else(|| wrong(key, "an untagged colour policy"))?,
                    _ => return Err(format!("unknown preference {key:?}")),
                }
                // Unchanged settings don't restart the renderers of open documents.
                if s != *self.color_management().settings() {
                    self.set_color_settings(s).map_err(|e| e.to_string())?;
                }
            }
            _ => return Err(format!("unknown preference {key:?}")),
        }
        Ok(parsed)
    }

    /// Set several preferences at once, all or none: when one is refused, the ones already set
    /// go back to what they were. Returns what each stored.
    pub fn set_prefs(&mut self, values: &Map<String, Value>) -> Result<Vec<(String, Parsed)>, String> {
        if values.len() > SCHEMA.len() {
            return Err(format!("at most {} preferences can be set at once", SCHEMA.len()));
        }
        let mut done: Vec<(String, Value)> = Vec::new();
        let mut out = Vec::new();
        for (key, value) in values {
            let before = self.pref(key);
            match self.set_pref(key, value) {
                Ok(parsed) => {
                    if let Some(b) = before {
                        done.push((key.clone(), b));
                    }
                    out.push((key.clone(), parsed));
                }
                Err(e) => {
                    for (k, b) in done.iter().rev() {
                        let _ = self.set_pref(k, b);
                    }
                    return Err(e);
                }
            }
        }
        Ok(out)
    }

    /// Put preference `key` back to its default.
    pub fn reset_pref(&mut self, key: &str) -> Result<Parsed, String> {
        let default = Session::new().pref(key).ok_or_else(|| format!("unknown preference {key:?} (prefs_get lists them)"))?;
        self.set_pref(key, &default)
    }

    /// The author of new comments when none is given: the identity name, or `fallback`.
    pub fn author_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        let name = self.prefs.identity.name.as_str();
        if name.is_empty() { fallback } else { name }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_reads_and_round_trips_its_default() {
        let mut s = Session::new();
        for spec in SCHEMA {
            assert!(CATEGORIES.contains(&spec.category), "{}", spec.key);
            let v = s.pref(spec.key).unwrap_or_else(|| panic!("{} has no value", spec.key));
            let stored = s.set_pref(spec.key, &v).unwrap_or_else(|e| panic!("{}: {e}", spec.key));
            assert_eq!(stored.value, v, "{}", spec.key);
            assert!(!stored.clamped, "{}", spec.key);
        }
        assert_eq!(s.prefs(), &Preferences::default());
    }

    #[test]
    fn hostile_values_are_clamped_or_refused() {
        let mut s = Session::new();
        let clamped = s.set_pref("documents.autosave_minutes", &json!(1e300)).unwrap();
        assert_eq!((clamped.value, clamped.clamped), (json!(MAX_AUTOSAVE_MINUTES), true));
        assert_eq!(s.set_pref("documents.recent_files", &json!(-5)).unwrap().value, json!(0));
        assert_eq!(s.prefs().recent_files, 0);
        assert!(s.set_pref("documents.recent_files", &json!("NaN")).is_err());
        assert!(s.set_pref("documents.recent_files", &json!({"x": 1})).is_err());
        assert!(s.set_pref("accessibility.text_scale", &json!(f64::MAX)).is_ok());
        assert_eq!(s.prefs().text_scale, MAX_TEXT_SCALE);
        assert_eq!(s.set_pref("accessibility.text_scale", &json!("0")).unwrap().value, json!(0.75));
        assert!(s.set_pref("accessibility.reduce_motion", &json!("maybe")).is_err());
        assert!(s.set_pref("accessibility.reduce_motion", &json!("on")).is_ok());
        assert!(s.prefs().reduce_motion);
        let long = "x\u{0}y\u{7}".repeat(10_000);
        let stored = s.set_pref("identity.name", &json!(long)).unwrap();
        assert!(stored.clamped);
        assert_eq!(s.prefs().identity.name.chars().count(), MAX_TEXT_CHARS);
        assert!(!s.prefs().identity.name.chars().any(char::is_control));
        for bad in [json!("#12345"), json!("#GGHHII"), json!("#ÄÄÄÄÄ"), json!([1, 2]), json!(null), json!("é1234")] {
            assert!(s.set_pref("forms.highlight_color", &bad).is_err(), "{bad}");
        }
        assert_eq!(s.set_pref("forms.highlight_color", &json!([300, -4, 12.4])).unwrap().value, json!("#FF000C"));
        assert_eq!(s.prefs().field_highlight, [255, 0, 12]);
        assert!(s.set_pref("color.intent", &json!("loud")).is_err());
        assert!(s.set_pref("color.rgb", &json!("user:nope")).is_err());
        assert!(s.set_pref("color.rgb", &json!("sgray")).is_err(), "a gray profile is not an RGB working space");
        assert_eq!(s.pref("color.rgb"), Some(json!("srgb")));
        assert!(s.set_pref("no.such.key", &json!(1)).is_err());
        assert!(s.set_pref("forms.date_format", &json!("HH")).is_err());
    }

    #[test]
    fn session_settings_follow_their_keys() {
        let mut s = Session::new();
        s.set_pref("security.javascript", &json!(false)).unwrap();
        assert!(!s.javascript());
        s.set_pref("security.internet_access", &json!("BLOCK-ALL")).unwrap();
        assert_eq!(s.url_policy().mode, UrlMode::BlockAll);
        s.set_pref("security.unlisted_sites", &json!("block")).unwrap();
        assert_eq!(s.url_policy().default_action, HostAction::Block);
        s.set_pref("color.intent", &json!("perceptual")).unwrap();
        assert_eq!(s.color_management().settings().intent.name(), "perceptual");
        s.set_pref("color.overprint_preview", &json!("always")).unwrap();
        assert_eq!(s.overprint_preview(), OverprintPreview::Always);
        s.set_pref("forms.date_format", &json!("d mmmm yyyy")).unwrap();
        assert_eq!(s.date_format(), "d mmmm yyyy");
        assert_eq!(s.author_or("PdfCraft"), "PdfCraft");
        s.set_pref("identity.name", &json!("  Ada  ")).unwrap();
        assert_eq!(s.author_or("PdfCraft"), "Ada");
        s.reset_pref("color.intent").unwrap();
        assert_eq!(s.color_management().settings().intent.name(), "relative_colorimetric");
    }

    #[test]
    fn several_at_once_is_all_or_nothing() {
        let mut s = Session::new();
        let mut m = Map::new();
        m.insert("documents.recent_files".into(), json!(3));
        m.insert("identity.name".into(), json!("Ada"));
        m.insert("zzz.unknown".into(), json!(1));
        assert!(s.set_prefs(&m).is_err());
        assert_eq!(s.prefs(), &Preferences::default());
        m.remove("zzz.unknown");
        assert_eq!(s.set_prefs(&m).unwrap().len(), 2);
        assert_eq!((s.prefs().recent_files, s.prefs().identity.name.as_str()), (3, "Ada"));
    }

    #[test]
    fn saved_settings_load_leniently() {
        let mut s = Session::new();
        s.set_pref("documents.autosave_minutes", &json!(5)).unwrap();
        s.set_pref("color.untagged", &json!("unmanaged")).unwrap();
        s.set_pref("forms.required_color", &json!("#00ff00")).unwrap();
        let saved = s.saved_prefs();
        assert!(saved.get("security.javascript").is_none(), "saved under its own key by the app");
        let mut fresh = Session::new();
        assert!(fresh.load_prefs(&saved).is_empty());
        assert_eq!(fresh.saved_prefs(), saved);
        // Old or corrupt files: wrong types, unknown keys and non-objects keep the defaults.
        for corrupt in [json!(null), json!([1, 2, 3]), json!("prefs"), json!(42)] {
            let mut f = Session::new();
            assert!(f.load_prefs(&corrupt).is_empty());
            assert_eq!(f.prefs(), &Preferences::default());
        }
        let mut f = Session::new();
        let skipped = f.load_prefs(&json!({
            "documents.autosave_minutes": "soon",
            "documents.recent_files": 1e9,
            "color.rgb": "user:gone",
            "forms.highlight_color": [1, 2, 3, 4],
            "unknown.key": true,
        }));
        assert_eq!(skipped, ["documents.autosave_minutes", "forms.highlight_color", "color.rgb"]);
        assert_eq!(f.prefs().autosave_minutes, DEFAULT_AUTOSAVE_MINUTES);
        assert_eq!(f.prefs().recent_files, MAX_RECENT_FILES);
        assert_eq!(f.pref("color.rgb"), Some(json!("srgb")));
    }

    #[test]
    fn the_schema_lists_types_and_defaults() {
        let schema = schema_json(&Session::new());
        assert_eq!(schema.len(), SCHEMA.len());
        let autosave = schema.iter().find(|s| s["key"] == "documents.autosave_minutes").unwrap();
        assert_eq!((autosave["type"].as_str(), autosave["maximum"].as_i64(), autosave["default"].as_i64()), (Some("integer"), Some(120), Some(1)));
        let untagged = schema.iter().find(|s| s["key"] == "color.untagged").unwrap();
        assert_eq!(untagged["enum"][0], "output_intent");
    }
}
