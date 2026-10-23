//! The interface language and locale formatting (`ui_language_get`, `ui_language_set`,
//! `locale_format`), from [`pdfcraft_engine::locale`]. The preference is the session's
//! `general.language`, the key the desktop app reads.

use pdfcraft_engine::locale::{self, AUTO, LOCALES, Locale, MAX_DECIMALS};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// Untrusted text quoted in an error, cut to a readable length.
fn shown(text: &str) -> String {
    format!("{:?}", text.chars().take(40).collect::<String>())
}

fn codes() -> String {
    LOCALES.iter().map(|l| l.code).collect::<Vec<_>>().join(", ")
}

/// `auto`, a language code (any case) or a locale tag that resolves to one.
fn resolve_pref(text: &str) -> Option<&'static str> {
    locale::normalize_pref(text).or_else(|| locale::code_from_tag(text))
}

/// `YYYY-MM-DD` with a real month and day.
fn parse_day(text: &str) -> Option<(i64, u32, u32)> {
    let mut parts = text.trim().splitn(3, '-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (i64, u32, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then_some((y, m, d))
}

impl Automation {
    pub(crate) fn ui_language_get(&self) -> Result<Value> {
        let current = self.session.interface_locale();
        Ok(json!({
            "preference": self.session.interface_language_pref(),
            "language": current.code,
            "name": current.name,
            "direction": if current.rtl { "rtl" } else { "ltr" },
            "system": locale::env_language(),
            "languages": LOCALES.iter().map(Locale::to_json).collect::<Vec<_>>(),
        }))
    }

    pub(crate) fn ui_language_set(&mut self, a: &Args) -> Result<Value> {
        let text = a.str("language")?;
        let code = resolve_pref(text).ok_or_else(|| bad(format!("unknown language {}; use {AUTO} or one of {}", shown(text), codes())))?;
        self.session.set_pref("general.language", &json!(code)).map_err(bad)?;
        self.ui_language_get()
    }

    pub(crate) fn locale_format(&self, a: &Args) -> Result<Value> {
        let loc = match a.opt_str("language")? {
            Some(text) => {
                let code = resolve_pref(text)
                    .filter(|c| *c != AUTO)
                    .ok_or_else(|| bad(format!("unknown language {}; use one of {}", shown(text), codes())))?;
                locale::locale(code).ok_or_else(|| bad(format!("unknown language {}", shown(text))))?
            }
            None => self.session.interface_locale(),
        };
        let mut out = json!({ "language": loc.code });
        let mut any = false;
        if let Some(x) = a.opt_num("number")? {
            let decimals = match a.opt_int("decimals")? {
                None => 2,
                Some(d) => {
                    usize::try_from(d).ok().filter(|d| *d <= MAX_DECIMALS).ok_or_else(|| bad(format!("decimals must be 0 to {MAX_DECIMALS}")))?
                }
            };
            out["number"] = json!(loc.decimal(x, decimals));
            any = true;
        }
        if let Some(n) = a.opt_int("integer")? {
            out["integer"] = json!(loc.integer(n));
            any = true;
        }
        if let Some(text) = a.opt_str("date")? {
            let day = parse_day(text).ok_or_else(|| bad(format!("date must be YYYY-MM-DD, not {}", shown(text))))?;
            out["date"] = json!(loc.date(day));
            any = true;
        }
        if let Some(text) = a.opt_str("pdf_date")? {
            let text_shown =
                locale::pdf_date(loc, text).ok_or_else(|| bad(format!("pdf_date must start with D:YYYYMMDDHHmm, not {}", shown(text))))?;
            out["pdf_date"] = json!(text_shown);
            any = true;
        }
        if let Some(n) = a.opt_int("count")? {
            let n = u64::try_from(n).map_err(|_| bad("count must be 0 or more"))?;
            out["count"] = json!({ "text": loc.count(n), "plural_category": loc.plural_category(n), "plural_form": loc.plural_index(n) });
            any = true;
        }
        if !any {
            return Err(bad("nothing to format: give number, integer, date, pdf_date or count"));
        }
        Ok(out)
    }
}
