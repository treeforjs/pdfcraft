//! Locale data for the interface languages: plural rules, number and date formatting, text
//! direction and how a system locale tag (`de_AT.UTF-8`, `zh-Hant-TW`) picks a language. The
//! translation catalogs themselves live with the UI (`pdfcraft-ui-egui`'s `i18n`), which takes
//! its plural rules and formats from here, so the desktop app, the control channel and the
//! automation tools (`ui_language_get`, `ui_language_set`, `locale_format`) agree.
//!
//! Everything is computed on demand from small constant tables: nothing here runs per frame
//! unless a caller formats a number or a date.

use crate::Session;

/// The `language` preference value that follows the system locale.
pub const AUTO: &str = "auto";

/// How the digits left of the decimal separator are grouped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grouping {
    /// Groups of three: 1,234,567.
    Thousands,
    /// The last three digits, then groups of two (Indian numbering): 12,34,567.
    Indian,
}

/// One interface language's locale conventions.
#[derive(Clone, Copy, Debug)]
pub struct Locale {
    /// The interface-language code (`en`, `pt-br`, `zh-hant`), lowercase.
    pub code: &'static str,
    /// The language's name in itself.
    pub name: &'static str,
    /// Written right to left (the interface would be mirrored). No language offered today is.
    pub rtl: bool,
    /// Plural form index for a count; the catalog's `@plural` entries list one form per index.
    pub plural: fn(u64) -> usize,
    /// The CLDR category of each plural form index (`one`, `few`, `many`, `other`).
    pub plural_categories: &'static [&'static str],
    pub decimal: char,
    pub group: char,
    pub grouping: Grouping,
    /// Numbers with fewer integer digits than this are not grouped (Spanish and Bulgarian
    /// write 1234 but 12 345).
    pub min_grouping: usize,
    /// The short numeric date, in [`crate::dates`] pattern letters.
    pub date: &'static str,
}

/// One form: Japanese and Chinese don't inflect for number.
pub fn plural_none(_: u64) -> usize {
    0
}

/// English and most others: 1 → one, everything else → other.
pub fn plural_one_other(n: u64) -> usize {
    usize::from(n != 1)
}

/// Czech: 1 → one, 2–4 → few, everything else (0, 5+) → other.
pub fn plural_cs(n: u64) -> usize {
    match n {
        1 => 0,
        2..=4 => 1,
        _ => 2,
    }
}

/// Portuguese and French (CLDR `pt`, `fr`): 0 and 1 take the singular, everything else the plural.
pub fn plural_zero_one_other(n: u64) -> usize {
    usize::from(n > 1)
}

/// Russian and Ukrainian (integer counts): one (1, 21), few (2–4, 22–24), many (0, 5–20, 25…).
/// Counts ending in 11–14 always take many, regardless of the last digit.
pub fn plural_east_slavic(n: u64) -> usize {
    match n % 100 {
        11..=14 => 2,
        _ => match n % 10 {
            1 => 0,
            2..=4 => 1,
            _ => 2,
        },
    }
}

const ONE_OTHER: &[&str] = &["one", "other"];
const NBSP: char = '\u{a0}';

const fn l(
    code: &'static str,
    name: &'static str,
    plural: fn(u64) -> usize,
    plural_categories: &'static [&'static str],
    (decimal, group, min_grouping): (char, char, usize),
    date: &'static str,
) -> Locale {
    Locale { code, name, rtl: false, plural, plural_categories, decimal, group, grouping: Grouping::Thousands, min_grouping, date }
}

/// The interface languages, in the order the UI registry and the date languages use. English
/// keeps ISO dates (2026-10-08), which read the same in every English-speaking region.
pub static LOCALES: [Locale; 15] = [
    l("en", "English", plural_one_other, ONE_OTHER, ('.', ',', 1), "yyyy-mm-dd"),
    l("ja", "日本語", plural_none, &["other"], ('.', ',', 1), "yyyy/mm/dd"),
    l("zh-hans", "简体中文", plural_none, &["other"], ('.', ',', 1), "yyyy/m/d"),
    l("zh-hant", "繁體中文", plural_none, &["other"], ('.', ',', 1), "yyyy/m/d"),
    l("cs", "Čeština", plural_cs, &["one", "few", "other"], (',', NBSP, 1), "d. m. yyyy"),
    l("pt-br", "Português (Brasil)", plural_zero_one_other, ONE_OTHER, (',', '.', 1), "dd/mm/yyyy"),
    l("de", "Deutsch", plural_one_other, ONE_OTHER, (',', '.', 1), "dd.mm.yyyy"),
    l("es", "Español", plural_one_other, ONE_OTHER, (',', '.', 2), "dd/mm/yyyy"),
    l("fr", "Français", plural_zero_one_other, ONE_OTHER, (',', NBSP, 1), "dd/mm/yyyy"),
    l("ru", "Русский", plural_east_slavic, &["one", "few", "many"], (',', NBSP, 1), "dd.mm.yyyy"),
    l("bg", "Български", plural_one_other, ONE_OTHER, (',', NBSP, 2), "d.mm.yyyy \\г."),
    Locale { grouping: Grouping::Indian, ..l("te", "తెలుగు", plural_one_other, ONE_OTHER, ('.', ',', 1), "dd-mm-yyyy") },
    l("hu", "Magyar", plural_one_other, ONE_OTHER, (',', NBSP, 1), "yyyy. mm. dd."),
    l("uk", "Українська", plural_east_slavic, &["one", "few", "many"], (',', NBSP, 1), "dd.mm.yyyy"),
    l("it", "Italiano", plural_one_other, ONE_OTHER, (',', '.', 1), "dd/mm/yyyy"),
];

/// Every value the `general.language` preference takes: [`AUTO`], then the codes of [`LOCALES`].
pub const LANGUAGE_PREFS: [&str; 16] = [AUTO, "en", "ja", "zh-hans", "zh-hant", "cs", "pt-br", "de", "es", "fr", "ru", "bg", "te", "hu", "uk", "it"];

/// The most fraction digits [`Locale::decimal`] writes (callers' counts are untrusted).
pub const MAX_DECIMALS: usize = 10;

/// The locale of an interface-language code (any case).
pub fn locale(code: &str) -> Option<&'static Locale> {
    LOCALES.iter().find(|l| l.code.eq_ignore_ascii_case(code.trim()))
}

/// English: the fallback for codes without a locale of their own.
pub fn english() -> &'static Locale {
    &LOCALES[0]
}

/// The canonical `language` preference for user input: [`AUTO`] or a registered code (any
/// case). `None` for anything else, so callers can keep the current setting.
pub fn normalize_pref(pref: &str) -> Option<&'static str> {
    if pref.trim().eq_ignore_ascii_case(AUTO) {
        return Some(AUTO);
    }
    locale(pref).map(|l| l.code)
}

/// Candidate language codes for a locale tag, most specific first: `zh_TW.UTF-8` →
/// `zh-tw`, `zh-hant`, `zh`. Chinese without a script goes by region, and every Portuguese
/// region falls back to the Brazilian catalog rather than English.
pub fn candidates(tag: &str) -> Vec<String> {
    let base = tag.split(['.', '@']).next().unwrap_or("").replace('_', "-").to_ascii_lowercase();
    // A tag is a few short parts; a hostile one costs at most this many.
    let parts: Vec<&str> = base.split('-').filter(|p| !p.is_empty()).take(8).collect();
    let Some(&primary) = parts.first() else { return Vec::new() };
    let mut out = Vec::new();
    for n in (1..=parts.len()).rev() {
        out.push(parts.get(..n).unwrap_or_default().join("-"));
    }
    if primary == "zh" && !parts.iter().any(|p| matches!(*p, "hans" | "hant")) {
        let script = if parts.iter().any(|p| matches!(*p, "tw" | "hk" | "mo")) { "zh-hant" } else { "zh-hans" };
        out.insert(out.len().saturating_sub(1), script.to_string());
    }
    if primary == "pt" && !out.iter().any(|c| c == "pt-br") {
        out.insert(out.len().saturating_sub(1), "pt-br".to_string());
    }
    out
}

/// The interface language for a locale tag such as `ja_JP.UTF-8` or `de-AT`; `None` if the
/// language isn't offered. `C` and `POSIX` mean English.
pub fn code_from_tag(tag: &str) -> Option<&'static str> {
    let cands = candidates(tag.trim());
    if matches!(cands.first().map(String::as_str), Some("c" | "posix")) {
        return Some("en");
    }
    cands.iter().find_map(|c| locale(c)).map(|l| l.code)
}

/// The language the `LC_ALL`, `LC_MESSAGES` and `LANG` environment variables ask for, in that
/// order (the headless view of "follow the system"; the desktop app also asks macOS and
/// Windows for their display languages).
pub fn env_language() -> Option<&'static str> {
    ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|var| std::env::var(var).ok().and_then(|v| code_from_tag(&v)))
}

/// `digits` (ASCII) with `sep` between groups as `grouping` says.
fn group_digits(digits: &str, sep: char, grouping: Grouping, min_grouping: usize) -> String {
    let len = digits.chars().count();
    if len < min_grouping.saturating_add(3) || len <= 3 {
        return digits.to_string();
    }
    let mut out = String::with_capacity(len.saturating_mul(2));
    for (i, c) in digits.chars().enumerate() {
        let left = len - i;
        let boundary = match grouping {
            Grouping::Thousands => left.is_multiple_of(3),
            Grouping::Indian => left == 3 || (left > 3 && (left - 3).is_multiple_of(2)),
        };
        if i > 0 && boundary {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

impl Locale {
    /// How many plural forms the language has (one per [`Locale::plural`] index).
    pub fn plural_forms(&self) -> usize {
        self.plural_categories.len()
    }

    /// The plural form index for `n`, never past the last form.
    pub fn plural_index(&self, n: u64) -> usize {
        (self.plural)(n).min(self.plural_forms().saturating_sub(1))
    }

    /// The CLDR category for `n` (`one`, `few`, `many` or `other`).
    pub fn plural_category(&self, n: u64) -> &'static str {
        self.plural_categories.get(self.plural_index(n)).copied().unwrap_or("other")
    }

    /// A whole number with the language's digit grouping: 1,234,567 / 1.234.567 / 1 234 567.
    pub fn integer(&self, n: i64) -> String {
        let grouped = group_digits(&n.unsigned_abs().to_string(), self.group, self.grouping, self.min_grouping);
        if n < 0 { format!("-{grouped}") } else { grouped }
    }

    /// A count (`{n}` in plural messages) with the language's digit grouping.
    pub fn count(&self, n: u64) -> String {
        group_digits(&n.to_string(), self.group, self.grouping, self.min_grouping)
    }

    /// `x` rounded to `decimals` fraction digits (at most [`MAX_DECIMALS`]) with the language's
    /// decimal separator and grouping: 1,234.5 / 1.234,5. Infinite and NaN values come back as
    /// `∞`, `-∞` and `NaN`, never as a panic or a page of digits.
    pub fn decimal(&self, x: f64, decimals: usize) -> String {
        if x.is_nan() {
            return "NaN".into();
        }
        if x.is_infinite() {
            return if x < 0.0 { "-∞".into() } else { "∞".into() };
        }
        let decimals = decimals.min(MAX_DECIMALS);
        let text = format!("{:.*}", decimals, x.abs());
        let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
        let mut out = String::new();
        // `-0.0` and values that round to zero show no sign.
        if x < 0.0 && text.bytes().any(|b| matches!(b, b'1'..=b'9')) {
            out.push('-');
        }
        out.push_str(&group_digits(int, self.group, self.grouping, self.min_grouping));
        if !frac.is_empty() {
            out.push(self.decimal);
            out.push_str(frac);
        }
        out
    }

    /// The short numeric date for `(year, month, day)`: 2026-10-08, 08.10.2026, 2026/10/08.
    pub fn date(&self, ymd: (i64, u32, u32)) -> String {
        let names = crate::dates::date_language(self.code).or_else(|| crate::dates::DATE_LANGUAGES.first());
        match names {
            Some(names) => crate::dates::format_day(self.date, ymd, names),
            None => format!("{:04}-{:02}-{:02}", ymd.0, ymd.1, ymd.2),
        }
    }

    /// Date and 24-hour time: 2026-10-08 14:05 / 08.10.2026 14:05.
    pub fn date_time(&self, ymd: (i64, u32, u32), (hour, minute): (u32, u32)) -> String {
        format!("{} {:02}:{:02}", self.date(ymd), hour.min(23), minute.min(59))
    }

    /// The JSON summary `ui_language_get` lists for each language.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "name": self.name,
            "direction": if self.rtl { "rtl" } else { "ltr" },
            "plural_categories": self.plural_categories,
            "decimal_separator": self.decimal.to_string(),
            "group_separator": self.group.to_string(),
            "sample_number": self.decimal(1_234_567.5, 1),
            "sample_date": self.date((2026, 10, 8)),
        })
    }
}

/// A PDF date string (`D:20261008140500+02'00'`, as written) as the language's short date and
/// time, or `None` when it doesn't start with twelve digits.
pub fn pdf_date(locale: &Locale, s: &str) -> Option<String> {
    let d = s.trim().trim_start_matches("D:");
    let digits = d.get(..12).filter(|d| d.bytes().all(|b| b.is_ascii_digit()))?;
    let num = |a: usize, b: usize| digits.get(a..b).and_then(|p| p.parse::<u32>().ok());
    let (y, m, day, h, min) = (num(0, 4)?, num(4, 6)?, num(6, 8)?, num(8, 10)?, num(10, 12)?);
    Some(locale.date_time((i64::from(y), m, day), (h, min)))
}

impl Session {
    /// The `general.language` preference: [`AUTO`] or an interface-language code.
    pub fn interface_language_pref(&self) -> &str {
        &self.prefs().language
    }

    /// The interface language in effect: the preference, or for [`AUTO`] the language the
    /// environment asks for (English when it asks for none that is offered).
    pub fn interface_locale(&self) -> &'static Locale {
        locale(self.interface_language_pref()).or_else(|| env_language().and_then(locale)).unwrap_or_else(english)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(code: &str) -> &'static Locale {
        locale(code).unwrap()
    }

    #[test]
    fn numbers_follow_each_language() {
        assert_eq!(loc("en").decimal(1_234_567.25, 2), "1,234,567.25");
        assert_eq!(loc("de").decimal(1_234_567.25, 2), "1.234.567,25");
        assert_eq!(loc("fr").decimal(1_234_567.25, 2), "1\u{a0}234\u{a0}567,25");
        assert_eq!(loc("te").integer(123_456_789), "12,34,56,789");
        assert_eq!(loc("es").integer(1234), "1234", "Spanish groups from five digits");
        assert_eq!(loc("es").integer(12_345), "12.345");
        assert_eq!(loc("ja").integer(-1234), "-1,234");
        assert_eq!(loc("en").integer(999), "999");
        assert_eq!(loc("ru").count(1000), "1\u{a0}000");
        assert_eq!(loc("en").decimal(-0.004, 2), "0.00", "no negative zero");
        assert_eq!(loc("en").decimal(2.5, 0), "2");
    }

    #[test]
    fn hostile_numbers_never_panic() {
        let en = loc("en");
        assert_eq!(en.decimal(f64::NAN, 2), "NaN");
        assert_eq!(en.decimal(f64::INFINITY, 2), "∞");
        assert_eq!(en.decimal(f64::NEG_INFINITY, 2), "-∞");
        assert_eq!(en.decimal(1.0, usize::MAX), "1.0000000000", "decimals are capped");
        assert!(en.decimal(f64::MAX, 2).len() < 500);
        assert_eq!(en.integer(i64::MIN), "-9,223,372,036,854,775,808");
        assert_eq!(en.count(u64::MAX), "18,446,744,073,709,551,615");
        for l in &LOCALES {
            for n in [0, 1, 2, 5, 11, 21, 101, u64::MAX] {
                assert!(l.plural_index(n) < l.plural_forms(), "{} {n}", l.code);
            }
            let _ = l.date((i64::MIN, 99, 0));
            let _ = l.date_time((2026, 13, 40), (99, 99));
        }
    }

    #[test]
    fn dates_follow_each_language() {
        let day = (2026, 10, 8);
        assert_eq!(loc("en").date(day), "2026-10-08");
        assert_eq!(loc("de").date(day), "08.10.2026");
        assert_eq!(loc("cs").date(day), "8. 10. 2026");
        assert_eq!(loc("bg").date(day), "8.10.2026 г.");
        assert_eq!(loc("hu").date(day), "2026. 10. 08.");
        assert_eq!(loc("ja").date(day), "2026/10/08");
        assert_eq!(pdf_date(loc("fr"), "D:20261008140500+02'00'").as_deref(), Some("08/10/2026 14:05"));
        assert_eq!(pdf_date(loc("en"), "D:2026"), None);
        assert_eq!(pdf_date(loc("en"), "D:2026١٠٠٨١٤٠٥"), None, "non-ASCII digits are refused, not sliced");
    }

    #[test]
    fn plural_rules() {
        let forms = |code: &str, ns: &[u64]| ns.iter().map(|&n| loc(code).plural_category(n)).collect::<Vec<_>>();
        assert_eq!(forms("en", &[0, 1, 2]), ["other", "one", "other"]);
        assert_eq!(forms("fr", &[0, 1, 2]), ["one", "one", "other"]);
        assert_eq!(forms("cs", &[1, 3, 5]), ["one", "few", "other"]);
        assert_eq!(forms("ru", &[1, 2, 5, 11, 21, 22, 112]), ["one", "few", "many", "many", "one", "few", "many"]);
        assert_eq!(forms("ja", &[1, 2]), ["other", "other"]);
    }

    #[test]
    fn tags_resolve_to_languages() {
        assert_eq!(code_from_tag("de_AT.UTF-8"), Some("de"));
        assert_eq!(code_from_tag("zh-TW"), Some("zh-hant"));
        assert_eq!(code_from_tag("zh"), Some("zh-hans"));
        assert_eq!(code_from_tag("pt-PT"), Some("pt-br"));
        assert_eq!(code_from_tag("C"), Some("en"));
        assert_eq!(code_from_tag("ar-EG"), None);
        assert_eq!(code_from_tag(""), None);
        assert_eq!(code_from_tag(&"-x".repeat(100_000)), None);
        assert_eq!(normalize_pref(" AUTO "), Some(AUTO));
        assert_eq!(normalize_pref("PT-BR"), Some("pt-br"));
        assert_eq!(normalize_pref("klingon"), None);
    }

    #[test]
    fn tables_agree() {
        let codes: Vec<&str> = LOCALES.iter().map(|l| l.code).collect();
        assert_eq!(LANGUAGE_PREFS.get(1..), Some(codes.as_slice()));
        let dates: Vec<(&str, &str)> = crate::dates::DATE_LANGUAGES.iter().map(|l| (l.code, l.name)).collect();
        let locales: Vec<(&str, &str)> = LOCALES.iter().map(|l| (l.code, l.name)).collect();
        assert_eq!(dates, locales);
        for l in &LOCALES {
            assert_eq!((0..=1000).map(l.plural).max().map(|m| m + 1), Some(l.plural_forms()), "{}", l.code);
            assert!(crate::dates::check_date_format(l.date).is_ok(), "{}", l.code);
            // No right-to-left language is offered: one needs a mirrored interface first.
            assert!(!l.rtl, "{}", l.code);
        }
    }
}
