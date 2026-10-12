//! Locale-specific Search & Redact patterns. Each matcher takes a page's characters and a start
//! index and returns the length of the match starting there. Every access goes through `get`, so
//! any start index is safe.

fn at(s: &[char], i: usize) -> Option<char> {
    s.get(i).copied()
}

fn before(s: &[char], i: usize) -> Option<char> {
    i.checked_sub(1).and_then(|p| at(s, p))
}

fn is_digit(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_digit())
}

fn is_alnum(c: Option<char>) -> bool {
    c.is_some_and(char::is_alphanumeric)
}

fn upper(c: Option<char>, excluded: &str) -> bool {
    c.is_some_and(|c| c.is_ascii_uppercase() && !excluded.contains(c))
}

/// `n` ASCII digits from `i`.
fn digits(s: &[char], i: usize, n: usize) -> bool {
    (i..i.saturating_add(n)).all(|k| is_digit(at(s, k)))
}

fn value(s: &[char], i: usize, n: usize) -> u64 {
    (i..i.saturating_add(n))
        .filter_map(|k| at(s, k).and_then(|c| c.to_digit(10)))
        .fold(0u64, |v, d| v.saturating_mul(10).saturating_add(u64::from(d)))
}

/// Digit groups of the given sizes from `i`, separated all by the same single separator from
/// `seps` or (when `bare`) by nothing. Returns (end, digits).
fn grouped(s: &[char], i: usize, sizes: &[usize], seps: &[char], bare: bool) -> Option<(usize, Vec<u32>)> {
    let mut options: Vec<Option<char>> = seps.iter().copied().map(Some).collect();
    if bare {
        options.push(None);
    }
    'sep: for sep in options {
        let (mut k, mut out) = (i, Vec::new());
        for (g, &n) in sizes.iter().enumerate() {
            if g > 0
                && let Some(c) = sep
            {
                if at(s, k) != Some(c) {
                    continue 'sep;
                }
                k += 1;
            }
            if !digits(s, k, n) {
                continue 'sep;
            }
            out.extend((k..k + n).filter_map(|x| at(s, x).and_then(|c| c.to_digit(10))));
            k += n;
        }
        if !is_alnum(at(s, k)) {
            return Some((k, out));
        }
    }
    None
}

const US_STATES: [&str; 56] = [
    "AL", "AK", "AZ", "AR", "CA", "CO", "CT", "DE", "DC", "FL", "GA", "HI", "ID", "IL", "IN", "IA", "KS", "KY", "LA", "ME", "MD", "MA", "MN", "MI",
    "MS", "MO", "MT", "NE", "NV", "NH", "NJ", "NM", "NY", "NC", "ND", "OH", "OK", "OR", "PA", "RI", "SC", "SD", "TN", "TX", "UT", "VT", "VA", "WA",
    "WV", "WI", "WY", "PR", "GU", "VI", "AS", "MP",
];

/// US ZIP codes: ZIP+4 anywhere ("12345-6789"), five-digit ZIPs only after a state abbreviation
/// ("Springfield, IL 62701"), so other five-digit numbers stay.
pub(super) fn us_zip(s: &[char], i: usize) -> Option<usize> {
    if is_alnum(before(s, i)) || before(s, i) == Some('-') || !digits(s, i, 5) || is_digit(at(s, i + 5)) {
        return None;
    }
    if at(s, i + 5) == Some('-') && digits(s, i + 6, 4) && !is_alnum(at(s, i + 10)) {
        return Some(10);
    }
    if is_alnum(at(s, i + 5)) || at(s, i + 5) == Some('-') {
        return None;
    }
    // "XX 12345": a state, one space, not part of a longer word.
    let state = i.checked_sub(3).and_then(|k| s.get(k..i))?;
    let name: String = state.iter().take(2).collect();
    let ok = state.get(2) == Some(&' ') && US_STATES.contains(&name.as_str()) && !is_alnum(i.checked_sub(4).and_then(|k| at(s, k)));
    ok.then_some(5)
}

/// UK postcodes, written with their single space: "SW1A 1AA", "M1 1AE", "CR2 6XH", "DN55 1PT".
pub(super) fn uk_postcode(s: &[char], i: usize) -> Option<usize> {
    if is_alnum(before(s, i)) || !upper(at(s, i), "QVX") {
        return None;
    }
    let mut k = i + 1;
    if upper(at(s, k), "IJZ") {
        k += 1;
    }
    if !is_digit(at(s, k)) {
        return None;
    }
    k += 1;
    if is_digit(at(s, k)) || upper(at(s, k), "") {
        k += 1;
    }
    let ok =
        at(s, k) == Some(' ') && is_digit(at(s, k + 1)) && upper(at(s, k + 2), "CIKMOV") && upper(at(s, k + 3), "CIKMOV") && !is_alnum(at(s, k + 4));
    ok.then_some(k + 4 - i)
}

/// UK National Insurance numbers: "AB 12 34 56 C" or "AB123456C" (prefix rules of HMRC's
/// published format; suffix A–D).
pub(super) fn uk_nino(s: &[char], i: usize) -> Option<usize> {
    if is_alnum(before(s, i)) || !upper(at(s, i), "DFIQUV") || !upper(at(s, i + 1), "DFIOQUV") {
        return None;
    }
    let prefix: String = s.get(i..i + 2)?.iter().collect();
    if ["BG", "GB", "KN", "NK", "NT", "TN", "ZZ"].contains(&prefix.as_str()) {
        return None;
    }
    for spaced in [true, false] {
        let mut k = i + 2;
        let mut ok = true;
        for _ in 0..3 {
            if spaced {
                ok &= at(s, k) == Some(' ');
                k += 1;
            }
            ok &= digits(s, k, 2);
            k += 2;
        }
        if spaced && ok {
            ok &= at(s, k) == Some(' ');
            k += 1;
        }
        if ok && upper(at(s, k), "EFGHIJKLMNOPQRSTUVWXYZ") && !is_alnum(at(s, k + 1)) {
            return Some(k + 1 - i);
        }
    }
    None
}

/// Canadian postal codes: "K1A 0B1" (letters D, F, I, O, Q and U are never used; W and Z never
/// start one).
pub(super) fn ca_postal(s: &[char], i: usize) -> Option<usize> {
    const NO: &str = "DFIOQU";
    if is_alnum(before(s, i)) || !upper(at(s, i), "DFIOQUWZ") || !is_digit(at(s, i + 1)) || !upper(at(s, i + 2), NO) {
        return None;
    }
    let k = if at(s, i + 3) == Some(' ') { i + 4 } else { i + 3 };
    let ok = is_digit(at(s, k)) && upper(at(s, k + 1), NO) && is_digit(at(s, k + 2)) && !is_alnum(at(s, k + 3));
    ok.then_some(k + 3 - i)
}

/// Canadian Social Insurance numbers: "123 456 782" or "123-456-782", Luhn-valid; numbers
/// starting with 0 or 8 are not issued.
pub(super) fn ca_sin(s: &[char], i: usize) -> Option<usize> {
    if is_alnum(before(s, i)) || matches!(at(s, i), Some('0' | '8')) {
        return None;
    }
    let (end, d) = grouped(s, i, &[3, 3, 3], &[' ', '-'], false)?;
    super::luhn(&d).then_some(end - i)
}

/// French social security numbers (NIR): sex, year, month, département (2A/2B for Corsica),
/// commune, order and a two-digit key (97 − the first 13 digits mod 97), with or without spaces:
/// "1 84 12 76 451 089 46".
pub(super) fn fr_nir(s: &[char], i: usize) -> Option<usize> {
    if is_alnum(before(s, i)) || !matches!(at(s, i), Some('1' | '2')) {
        return None;
    }
    for spaced in [true, false] {
        let mut k = i + 1;
        let mut number = u64::from(at(s, i).and_then(|c| c.to_digit(10)).unwrap_or(0));
        let mut ok = true;
        let mut month = 0;
        for (g, n) in [2usize, 2, 2, 3, 3].into_iter().enumerate() {
            if spaced {
                ok &= at(s, k) == Some(' ');
                k += 1;
            }
            // The département may be 2A or 2B (counted as 19 and 18 for the key).
            let corsica = g == 2 && at(s, k) == Some('2') && matches!(at(s, k + 1), Some('A' | 'B'));
            let v = if corsica {
                if at(s, k + 1) == Some('A') { 19 } else { 18 }
            } else {
                ok &= digits(s, k, n);
                value(s, k, n)
            };
            if g == 1 {
                month = v;
            }
            number = number.saturating_mul(10u64.pow(n as u32)).saturating_add(v);
            k += n;
        }
        if spaced {
            ok &= at(s, k) == Some(' ');
            k += 1;
        }
        ok &= digits(s, k, 2) && !is_alnum(at(s, k + 2));
        // Months 1–12, or the 20–42 and 50–99 ranges used when the birth month is unknown.
        ok &= matches!(month, 1..=12 | 20..=42 | 50..=99);
        if ok && 97 - number % 97 == value(s, k, 2) {
            return Some(k + 2 - i);
        }
    }
    None
}

/// Japanese postal codes: "123-4567" (also after 〒), hyphen required.
pub(super) fn jp_postal(s: &[char], i: usize) -> Option<usize> {
    if is_alnum(before(s, i)) || matches!(before(s, i), Some('-' | '+' | '(')) || !digits(s, i, 3) {
        return None;
    }
    let ok =
        matches!(at(s, i + 3), Some('-' | '－' | '−' | '‐')) && digits(s, i + 4, 4) && !is_alnum(at(s, i + 8)) && !matches!(at(s, i + 8), Some('-'));
    ok.then_some(8)
}

/// Japanese Individual Numbers (My Number): 12 digits, as one run or 4-4-4, whose last digit is
/// the check digit of the first eleven.
pub(super) fn jp_my_number(s: &[char], i: usize) -> Option<usize> {
    if is_alnum(before(s, i)) {
        return None;
    }
    let (end, d) = grouped(s, i, &[4, 4, 4], &[' ', '-'], true)?;
    // Weights from the right: 2..7 for the first six digits, then 2..6.
    let sum: u32 = d.iter().take(11).rev().enumerate().map(|(n, v)| v * if n < 6 { n as u32 + 2 } else { n as u32 - 4 }).sum();
    let r = sum % 11;
    let check = if r <= 1 { 0 } else { 11 - r };
    (d.get(11) == Some(&check)).then_some(end - i)
}

/// IBANs: country code, check digits and up to 30 letters and digits, bare or in groups of four
/// separated by single spaces; the ISO 13616 mod-97 check must hold.
pub(super) fn iban(s: &[char], i: usize) -> Option<usize> {
    if is_alnum(before(s, i)) || !upper(at(s, i), "") || !upper(at(s, i + 1), "") || !digits(s, i + 2, 2) {
        return None;
    }
    // Collect the characters and where each ends.
    let mut chars: Vec<char> = Vec::new();
    let mut ends: Vec<usize> = Vec::new();
    let mut k = i;
    while chars.len() < 34 {
        match at(s, k) {
            Some(c) if c.is_ascii_digit() || c.is_ascii_uppercase() => {
                chars.push(c);
                k += 1;
                ends.push(k);
            }
            Some(' ') if chars.len().is_multiple_of(4) && (is_digit(at(s, k + 1)) || upper(at(s, k + 1), "")) => k += 1,
            _ => break,
        }
    }
    // The longest valid prefix that ends a word (a following word in capitals may follow).
    for n in (15..=chars.len()).rev() {
        let end = *ends.get(n - 1)?;
        if is_alnum(at(s, end)) {
            continue;
        }
        let rearranged = chars.get(4..n)?.iter().chain(chars.get(..4)?.iter());
        let mut rem: u32 = 0;
        for c in rearranged {
            let v = c.to_digit(36)?;
            rem = if v >= 10 { (rem * 100 + v) % 97 } else { (rem * 10 + v) % 97 };
        }
        if rem == 1 {
            return Some(end - i);
        }
    }
    None
}
