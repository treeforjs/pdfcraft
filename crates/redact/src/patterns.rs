//! Search & Redact patterns (Acrobat's Find Text ▸ Patterns): phone numbers, email addresses,
//! credit card numbers, US Social Security numbers and dates, plus locale-specific sets (see
//! [`LOCALES`]): US ZIP codes, UK postcodes and National Insurance numbers, Canadian postal codes
//! and Social Insurance numbers, French social security numbers (NIR), Japanese postal codes and
//! Individual Numbers, and IBANs. Each matcher works on a page's text as characters and returns
//! character ranges; callers map them to glyphs and areas.
//!
//! Identifiers with a check digit (credit cards, SIN, NIR, My Number, IBAN) only match when it is
//! valid. Formats come from the issuing bodies' public descriptions; test values are synthetic.

use std::ops::Range;

#[path = "patterns_locale.rs"]
mod locale;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pattern {
    Phone,
    Email,
    CreditCard,
    Ssn,
    Date,
    UsZip,
    UkPostcode,
    UkNino,
    CaPostal,
    CaSin,
    FrNir,
    JpPostal,
    JpMyNumber,
    Iban,
}

pub const PATTERNS: [Pattern; 14] = [
    Pattern::Phone,
    Pattern::Email,
    Pattern::CreditCard,
    Pattern::Ssn,
    Pattern::Date,
    Pattern::UsZip,
    Pattern::UkPostcode,
    Pattern::UkNino,
    Pattern::CaPostal,
    Pattern::CaSin,
    Pattern::FrNir,
    Pattern::JpPostal,
    Pattern::JpMyNumber,
    Pattern::Iban,
];

/// Pattern sets by locale: (id, name). `intl` patterns apply everywhere and belong to every set.
pub const LOCALES: [(&str, &str); 6] = [
    ("intl", "International"),
    ("en-US", "United States"),
    ("en-GB", "United Kingdom"),
    ("en-CA", "Canada"),
    ("fr-FR", "France"),
    ("ja-JP", "Japan"),
];

/// The patterns of a locale set (the international ones first); `None` for an unknown locale.
pub fn patterns_for(locale: &str) -> Option<Vec<Pattern>> {
    let (id, _) = LOCALES.iter().find(|(id, _)| id.eq_ignore_ascii_case(locale))?;
    Some(PATTERNS.into_iter().filter(|p| p.locale() == "intl" || p.locale() == *id).collect())
}

impl Pattern {
    pub fn label(self) -> &'static str {
        match self {
            Pattern::Phone => "Phone Numbers",
            Pattern::Email => "Email Addresses",
            Pattern::CreditCard => "Credit Cards",
            Pattern::Ssn => "Social Security Numbers",
            Pattern::Date => "Dates",
            Pattern::UsZip => "US ZIP Codes",
            Pattern::UkPostcode => "UK Postcodes",
            Pattern::UkNino => "UK National Insurance Numbers",
            Pattern::CaPostal => "Canadian Postal Codes",
            Pattern::CaSin => "Canadian Social Insurance Numbers",
            Pattern::FrNir => "French Social Security Numbers (NIR)",
            Pattern::JpPostal => "Japanese Postal Codes",
            Pattern::JpMyNumber => "Japanese Individual Numbers (My Number)",
            Pattern::Iban => "Bank Account Numbers (IBAN)",
        }
    }

    /// The locale set the pattern belongs to (an id of [`LOCALES`]).
    pub fn locale(self) -> &'static str {
        match self {
            Pattern::Phone | Pattern::Email | Pattern::CreditCard | Pattern::Date | Pattern::Iban => "intl",
            Pattern::Ssn | Pattern::UsZip => "en-US",
            Pattern::UkPostcode | Pattern::UkNino => "en-GB",
            Pattern::CaPostal | Pattern::CaSin => "en-CA",
            Pattern::FrNir => "fr-FR",
            Pattern::JpPostal | Pattern::JpMyNumber => "ja-JP",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Pattern::Phone => "phone",
            Pattern::Email => "email",
            Pattern::CreditCard => "credit-card",
            Pattern::Ssn => "ssn",
            Pattern::Date => "date",
            Pattern::UsZip => "us-zip",
            Pattern::UkPostcode => "uk-postcode",
            Pattern::UkNino => "uk-nino",
            Pattern::CaPostal => "ca-postal",
            Pattern::CaSin => "ca-sin",
            Pattern::FrNir => "fr-nir",
            Pattern::JpPostal => "jp-postal",
            Pattern::JpMyNumber => "jp-my-number",
            Pattern::Iban => "iban",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        PATTERNS.into_iter().find(|p| p.id() == id)
    }
}

fn digit(c: Option<&char>) -> bool {
    c.is_some_and(char::is_ascii_digit)
}

fn word(c: Option<&char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric())
}

/// Digits and the separators allowed between them, from `start`: (end, digit count).
fn digit_run(s: &[char], start: usize, seps: &[char], max_len: usize) -> (usize, usize) {
    let (mut i, mut n) = (start, 0);
    while i < s.len() && i - start < max_len {
        let c = s[i];
        if c.is_ascii_digit() {
            n += 1;
        } else if !(seps.contains(&c) && (digit(s.get(i + 1)) || s.get(i + 1) == Some(&'(')) || (c == '(' || c == ')') && seps.contains(&c)) {
            break;
        }
        i += 1;
    }
    // Never end on a separator.
    while i > start && !s[i - 1].is_ascii_digit() {
        i -= 1;
    }
    (i, n)
}

fn phone(s: &[char], i: usize) -> Option<usize> {
    let start_ok = s[i] == '+' && digit(s.get(i + 1)) || s[i] == '(' && digit(s.get(i + 1)) || s[i].is_ascii_digit();
    if !start_ok || word(i.checked_sub(1).and_then(|p| s.get(p))) || s.get(i.wrapping_sub(1)) == Some(&'+') {
        return None;
    }
    let plus = s[i] == '+';
    let first = if plus { i + 1 } else { i };
    let (end, n) = digit_run(s, first, &[' ', '-', '.', '(', ')'], 28);
    let run = s.get(first..end)?;
    let seps = run.iter().filter(|c| !c.is_ascii_digit()).count();
    // Always grouped by at least one separator, so plain numbers never match.
    if seps == 0 || digit(s.get(end)) {
        return None;
    }
    let ok = if plus {
        // E.164: a `+` country code and up to 15 digits in all ("+49 2151 123456").
        (8..=15).contains(&n)
    } else {
        // North American: 10 digits, 11 with a country code ("(555) 123-4567").
        let nanp = (10..=11).contains(&n);
        // National with a trunk 0 and 6–14 more digits ("0211 123456", "01 23 45 67 89").
        // The first group needs two digits so decimals ("0.1234567") don't match, and
        // day-first dates ("01.10.2026") are left to the date pattern.
        let mut digits = run.iter().skip_while(|c| !c.is_ascii_digit());
        let trunk_zero = digits.clone().next() == Some(&'0');
        let lead = digits.by_ref().take_while(|c| c.is_ascii_digit()).count();
        let trunk = trunk_zero && (7..=15).contains(&n) && lead >= 2 && date(s, i).is_none();
        nanp || trunk
    };
    ok.then_some(end - i)
}

fn email(s: &[char], i: usize) -> Option<usize> {
    let local = |c: &char| c.is_ascii_alphanumeric() || "._%+-".contains(*c);
    if !s[i].is_ascii_alphanumeric() || i.checked_sub(1).and_then(|p| s.get(p)).is_some_and(local) {
        return None;
    }
    let mut j = i;
    while j < s.len() && local(&s[j]) {
        j += 1;
    }
    if s.get(j) != Some(&'@') {
        return None;
    }
    let d0 = j + 1;
    let mut k = d0;
    while k < s.len() && (s[k].is_ascii_alphanumeric() || s[k] == '-' || s[k] == '.' && s.get(k + 1).is_some_and(char::is_ascii_alphanumeric)) {
        k += 1;
    }
    let domain: String = s[d0..k].iter().collect();
    let tld = domain.rsplit('.').next().unwrap_or("");
    (domain.contains('.') && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())).then_some(k - i)
}

pub(crate) fn luhn(digits: &[u32]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(k, d)| {
            if k % 2 == 1 {
                let x = d * 2;
                if x > 9 { x - 9 } else { x }
            } else {
                *d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

fn credit_card(s: &[char], i: usize) -> Option<usize> {
    if !s[i].is_ascii_digit() || digit(i.checked_sub(1).and_then(|p| s.get(p))) {
        return None;
    }
    let (end, n) = digit_run(s, i, &[' ', '-'], 23);
    if !(13..=19).contains(&n) || digit(s.get(end)) {
        return None;
    }
    let digits: Vec<u32> = s[i..end].iter().filter_map(|c| c.to_digit(10)).collect();
    luhn(&digits).then_some(end - i)
}

fn ssn(s: &[char], i: usize) -> Option<usize> {
    if digit(i.checked_sub(1).and_then(|p| s.get(p))) {
        return None;
    }
    let g = |from: usize, n: usize| (from..from + n).all(|k| digit(s.get(k)));
    let sep = |k: usize| matches!(s.get(k), Some('-' | ' '));
    if !(g(i, 3) && sep(i + 3) && g(i + 4, 2) && sep(i + 6) && g(i + 7, 4)) || digit(s.get(i + 11)) {
        return None;
    }
    let area: String = s[i..i + 3].iter().collect();
    (area != "000" && area != "666" && !area.starts_with('9')).then_some(11)
}

const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];

/// A month name or its three-letter abbreviation (optionally with a full stop) at `i`.
fn month(s: &[char], i: usize) -> Option<usize> {
    if word(i.checked_sub(1).and_then(|p| s.get(p))) {
        return None;
    }
    let rest: String = s[i..s.len().min(i + 10)].iter().collect::<String>().to_lowercase();
    for m in MONTHS {
        for cand in [m, &m[..3]] {
            if rest.starts_with(cand) && !rest[cand.len()..].starts_with(|c: char| c.is_alphabetic()) {
                let mut n = cand.chars().count();
                if cand.len() == 3 && s.get(i + n) == Some(&'.') {
                    n += 1;
                }
                return Some(n);
            }
        }
    }
    None
}

fn number(s: &[char], i: usize, min: usize, max: usize) -> Option<usize> {
    let n = s[i.min(s.len())..].iter().take(max + 1).take_while(|c| c.is_ascii_digit()).count();
    (n >= min && n <= max).then_some(n)
}

fn date(s: &[char], i: usize) -> Option<usize> {
    if word(i.checked_sub(1).and_then(|p| s.get(p))) {
        return None;
    }
    let spaces = |k: usize| s[k.min(s.len())..].iter().take_while(|c| **c == ' ').count();
    // Numeric: m/d/yy(yy), m-d-yyyy, d.m.yyyy, yyyy-mm-dd.
    if let Some(a) = number(s, i, 1, 4) {
        let sep = s.get(i + a).copied();
        if matches!(sep, Some('/' | '-' | '.'))
            && let Some(b) = number(s, i + a + 1, 1, 2)
            && s.get(i + a + 1 + b).copied() == sep
            && let Some(c) = number(s, i + a + b + 2, if a == 4 { 1 } else { 2 }, if a == 4 { 2 } else { 4 })
            && (a <= 2 || a == 4)
            && !digit(s.get(i + a + b + 2 + c))
        {
            return Some(a + b + c + 2);
        }
        // "5 January 2024".
        if a <= 2
            && let k = i + a + spaces(i + a)
            && k > i + a
            && let Some(m) = month(s, k)
        {
            let k2 = k + m + spaces(k + m);
            if let Some(y) = number(s, k2, 4, 4) {
                return Some(k2 + y - i);
            }
        }
        return None;
    }
    // "January 5, 2024" / "Jan. 5 2024".
    let m = month(s, i)?;
    let k = i + m + spaces(i + m);
    let d = number(s, k, 1, 2)?;
    let mut k2 = k + d;
    if s.get(k2) == Some(&',') {
        k2 += 1;
    }
    k2 += spaces(k2);
    let y = number(s, k2, 4, 4)?;
    Some(k2 + y - i)
}

/// Character ranges of `text` matching `pattern`.
pub fn find(pattern: Pattern, text: &[char]) -> Vec<Range<usize>> {
    let f: fn(&[char], usize) -> Option<usize> = match pattern {
        Pattern::Phone => phone,
        Pattern::Email => email,
        Pattern::CreditCard => credit_card,
        Pattern::Ssn => ssn,
        Pattern::Date => date,
        Pattern::UsZip => locale::us_zip,
        Pattern::UkPostcode => locale::uk_postcode,
        Pattern::UkNino => locale::uk_nino,
        Pattern::CaPostal => locale::ca_postal,
        Pattern::CaSin => locale::ca_sin,
        Pattern::FrNir => locale::fr_nir,
        Pattern::JpPostal => locale::jp_postal,
        Pattern::JpMyNumber => locale::jp_my_number,
        Pattern::Iban => locale::iban,
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        match f(text, i) {
            Some(n) if n > 0 => {
                out.push(i..i + n);
                i += n;
            }
            _ => i += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(p: Pattern, s: &str) -> Vec<String> {
        let c: Vec<char> = s.chars().collect();
        find(p, &c).into_iter().map(|r| c[r].iter().collect()).collect()
    }

    #[test]
    fn phone_numbers() {
        assert_eq!(
            found(Pattern::Phone, "Call (555) 123-4567 or 555.987.6543, +1 555 222 3333; not 12345 or 5551234567890"),
            ["(555) 123-4567", "555.987.6543", "+1 555 222 3333"]
        );
        // #525: international numbers match whole; "+49" used to stay visible.
        assert_eq!(found(Pattern::Phone, "Tel. +49 2151 123456."), ["+49 2151 123456"]);
        assert_eq!(
            found(Pattern::Phone, "+44 20 7946 0018, +33 1 23 45 67 89, +49 (0) 2151 123456, (0211) 123456, 030 12345678, 01 23 45 67 89"),
            ["+44 20 7946 0018", "+33 1 23 45 67 89", "+49 (0) 2151 123456", "(0211) 123456", "030 12345678", "01 23 45 67 89"]
        );
        // Not phone numbers: day-first dates, decimals, unseparated, too short or too long.
        assert_eq!(
            found(
                Pattern::Phone,
                "on 01.10.2026 or 09-10-2026, ratio 0.1234567, id 0211123456, +4921511234567, +1 234 567, 0211 12, +49 1234 5678 9012 3456"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn emails() {
        assert_eq!(
            found(Pattern::Email, "Write to ada.lovelace+pdf@example.co.uk, or bob@host (no TLD) or x@y.z."),
            ["ada.lovelace+pdf@example.co.uk"]
        );
    }

    #[test]
    fn credit_cards_need_a_valid_checksum() {
        assert_eq!(
            found(Pattern::CreditCard, "Visa 4111 1111 1111 1111, bad 4111 1111 1111 1112, amex 3782-822463-10005"),
            ["4111 1111 1111 1111", "3782-822463-10005"]
        );
    }

    #[test]
    fn social_security_numbers() {
        assert_eq!(
            found(Pattern::Ssn, "SSN 123-45-6789 and 123 45 6789; invalid 000-12-3456, 666-12-3456, 912-12-3456, 1234-56-7890"),
            ["123-45-6789", "123 45 6789"]
        );
    }

    #[test]
    fn dates() {
        assert_eq!(
            found(Pattern::Date, "Due 10/01/2026, 2026-10-01, 1.10.26, March 5, 2024, Jan. 7 2025 and 5 June 2023; not 10/2026 or Mayday 12"),
            ["10/01/2026", "2026-10-01", "1.10.26", "March 5, 2024", "Jan. 7 2025", "5 June 2023"]
        );
    }

    // Locale patterns. Every value below is synthetic (made up for these tests, with valid check
    // digits where the format has them) or a format's published specimen; none identifies anyone.

    #[test]
    fn locale_sets_group_the_patterns() {
        for p in PATTERNS {
            assert_eq!(Pattern::from_id(p.id()), Some(p));
            assert!(LOCALES.iter().any(|(id, _)| *id == p.locale()), "{p:?}");
        }
        let uk = patterns_for("en-gb").unwrap();
        assert!(uk.contains(&Pattern::UkPostcode) && uk.contains(&Pattern::Email) && !uk.contains(&Pattern::Ssn));
        assert_eq!(patterns_for("intl").unwrap().len(), 5);
        assert_eq!(patterns_for("xx-YY"), None);
    }

    #[test]
    fn us_zip_codes() {
        assert_eq!(found(Pattern::UsZip, "Springfield, IL 62704 or 62704-1234; order 12345, ZZ 54321, 123456, 62704-12"), ["62704", "62704-1234"]);
    }

    #[test]
    fn uk_postcodes_and_national_insurance_numbers() {
        assert_eq!(
            found(Pattern::UkPostcode, "SW1A 1AA, M1 1AE, CR2 6XH, DN55 1PT, W1A 0AX; not SW1A1AA, Q1 1AA, M1 1AI, ABCD 1AA"),
            ["SW1A 1AA", "M1 1AE", "CR2 6XH", "DN55 1PT", "W1A 0AX"]
        );
        assert_eq!(
            found(Pattern::UkNino, "NI AB 12 34 56 C, JK123456D; not QQ 12 34 56 C, GB 12 34 56 A, AB 12 34 56 E, AB 12 3456 C"),
            ["AB 12 34 56 C", "JK123456D"]
        );
    }

    #[test]
    fn canadian_postal_codes_and_sins() {
        assert_eq!(found(Pattern::CaPostal, "K1A 0B1, H0H0H0; not D1A 0B1, W1A 0B1, K1A 0B12"), ["K1A 0B1", "H0H0H0"]);
        assert_eq!(
            found(Pattern::CaSin, "SIN 130 692 544 or 130-692-544; bad checksum 130 692 545, mixed 130-692 544, starts with 0: 030 692 544"),
            ["130 692 544", "130-692-544"]
        );
    }

    #[test]
    fn french_social_security_numbers() {
        assert_eq!(
            found(
                Pattern::FrNir,
                "NIR 1 99 03 75 123 456 34, 2 90 05 2A 004 012 66, 199037512345634; bad key 1 99 03 75 123 456 35, month 13: 1 99 13 75 123 456 34"
            ),
            ["1 99 03 75 123 456 34", "2 90 05 2A 004 012 66", "199037512345634"]
        );
    }

    #[test]
    fn japanese_postal_codes_and_individual_numbers() {
        assert_eq!(found(Pattern::JpPostal, "〒100-0001 and 530-0001; not 1000-0001, 100-00011 or 555-123-4567"), ["100-0001", "530-0001"]);
        assert_eq!(
            found(Pattern::JpMyNumber, "1234 5678 9018, 123456789018; bad check 1234 5678 9017, 13 digits 1234567890185"),
            ["1234 5678 9018", "123456789018"]
        );
    }

    #[test]
    fn ibans_need_a_valid_checksum() {
        assert_eq!(
            found(Pattern::Iban, "Pay DE89 3704 0044 0532 0130 00 or GB82WEST12345698765432 NOW; bad GB82 WEST 1234 5698 7654 33, short DE89 3704"),
            ["DE89 3704 0044 0532 0130 00", "GB82WEST12345698765432"]
        );
    }
}
