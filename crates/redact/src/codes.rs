//! Redaction codes: named sets of short codes that cite why content was withheld (for example the
//! exemptions of the U.S. Freedom of Information Act). A chosen code becomes the overlay text of a
//! Redact annotation (`/OverlayText`, ISO 32000-2 §12.5.6.23), drawn into the page when the mark is
//! applied.
//!
//! The built-in sets cite public law (5 U.S.C. § 552(b) and § 552a); the descriptions are our own
//! short summaries, not the statutory text. Custom sets are plain lists of codes made by the user.

/// One code of a set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Code {
    /// The text drawn on the box, e.g. `(b)(6)`.
    pub code: &'static str,
    /// What the code stands for (a short summary).
    pub description: &'static str,
}

/// A named set of redaction codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeSet {
    /// Stable identifier (automation tools, settings).
    pub id: &'static str,
    pub name: &'static str,
    pub codes: &'static [Code],
}

const fn c(code: &'static str, description: &'static str) -> Code {
    Code { code, description }
}

/// U.S. Freedom of Information Act exemptions, 5 U.S.C. § 552(b).
pub const US_FOIA: CodeSet = CodeSet {
    id: "us-foia",
    name: "U.S. FOIA",
    codes: &[
        c("(b)(1)", "Classified national defence or foreign policy information"),
        c("(b)(2)", "Internal personnel rules and practices of an agency"),
        c("(b)(3)", "Information exempted from disclosure by another statute"),
        c("(b)(4)", "Trade secrets and confidential commercial or financial information"),
        c("(b)(5)", "Privileged inter-agency or intra-agency communications"),
        c("(b)(6)", "Personnel, medical and similar files: personal privacy"),
        c("(b)(7)(A)", "Law enforcement records: interference with proceedings"),
        c("(b)(7)(B)", "Law enforcement records: right to a fair trial"),
        c("(b)(7)(C)", "Law enforcement records: personal privacy"),
        c("(b)(7)(D)", "Law enforcement records: confidential sources"),
        c("(b)(7)(E)", "Law enforcement records: techniques, procedures and guidelines"),
        c("(b)(7)(F)", "Law enforcement records: life or physical safety"),
        c("(b)(8)", "Reports about the supervision of financial institutions"),
        c("(b)(9)", "Geological and geophysical information about wells"),
    ],
};

/// U.S. Privacy Act exemptions, 5 U.S.C. § 552a.
pub const US_PRIVACY_ACT: CodeSet = CodeSet {
    id: "us-privacy-act",
    name: "U.S. Privacy Act",
    codes: &[
        c("(d)(5)", "Information compiled in reasonable anticipation of a civil action"),
        c("(j)(1)", "Records maintained by the Central Intelligence Agency"),
        c("(j)(2)", "Records of agencies whose principal function is criminal law enforcement"),
        c("(k)(1)", "Classified information"),
        c("(k)(2)", "Investigatory material compiled for law enforcement"),
        c("(k)(3)", "Records about protective services for the President and others"),
        c("(k)(4)", "Records used only for statistical purposes"),
        c("(k)(5)", "Suitability investigations for federal employment or contracts: confidential sources"),
        c("(k)(6)", "Testing or examination material for appointment or promotion"),
        c("(k)(7)", "Armed services promotion evaluations: confidential sources"),
    ],
};

/// The built-in code sets.
pub const CODE_SETS: [CodeSet; 2] = [US_FOIA, US_PRIVACY_ACT];

/// The longest overlay text made from codes (characters).
pub const MAX_OVERLAY_CHARS: usize = 512;

impl CodeSet {
    pub fn from_id(id: &str) -> Option<&'static CodeSet> {
        CODE_SETS.iter().find(|s| s.id == id)
    }

    pub fn find(&self, code: &str) -> Option<&Code> {
        let code = code.trim();
        self.codes.iter().find(|c| c.code.eq_ignore_ascii_case(code))
    }
}

/// Why codes could not be turned into overlay text.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    #[error("no codes were given")]
    Empty,
    #[error("{code:?} is not a code of {set} (codes: {known})")]
    Unknown { code: String, set: &'static str, known: String },
    #[error("a code is empty or contains a line break")]
    Malformed,
    #[error("the codes make an overlay text longer than {MAX_OVERLAY_CHARS} characters")]
    TooLong,
}

/// The overlay text for `codes` of `set`, in the given order, separated by commas (as several
/// codes are cited on one box). Codes are matched ignoring case and written in the set's form.
pub fn overlay_text(set: &CodeSet, codes: &[&str]) -> Result<String, CodeError> {
    let mut out: Vec<&str> = Vec::new();
    for code in codes {
        let found = set.find(code).ok_or_else(|| CodeError::Unknown {
            code: code.chars().take(40).collect(),
            set: set.name,
            known: set.codes.iter().map(|c| c.code).collect::<Vec<_>>().join(", "),
        })?;
        if !out.contains(&found.code) {
            out.push(found.code);
        }
    }
    join(&out)
}

/// The overlay text for codes of a custom set (any non-empty single-line text per code).
pub fn custom_overlay_text(codes: &[&str]) -> Result<String, CodeError> {
    let mut out: Vec<&str> = Vec::new();
    for code in codes {
        let code = code.trim();
        if code.is_empty() || code.contains(['\n', '\r']) {
            return Err(CodeError::Malformed);
        }
        if !out.contains(&code) {
            out.push(code);
        }
    }
    join(&out)
}

fn join(codes: &[&str]) -> Result<String, CodeError> {
    if codes.is_empty() {
        return Err(CodeError::Empty);
    }
    let text = codes.join(", ");
    if text.chars().count() > MAX_OVERLAY_CHARS {
        return Err(CodeError::TooLong);
    }
    Ok(text)
}

/// The most custom code sets kept.
pub const MAX_CUSTOM_SETS: usize = 64;
/// The most codes in one custom set.
pub const MAX_CUSTOM_CODES: usize = 500;
/// The longest custom set name and code (characters).
pub const MAX_CUSTOM_NAME_CHARS: usize = 64;
/// The longest code description (characters).
pub const MAX_CUSTOM_DESCRIPTION_CHARS: usize = 300;

/// One code of a set the user made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CustomCode {
    pub code: String,
    pub description: String,
}

/// A code set the user made (Redaction Properties ▸ Edit codes), saved with the settings and
/// imported or exported as a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CustomCodeSet {
    pub name: String,
    pub codes: Vec<CustomCode>,
}

fn one_line(text: &str, what: &str, max: usize) -> Result<String, String> {
    let t = text.trim();
    if t.contains(['\n', '\r']) || t.chars().any(char::is_control) {
        return Err(format!("{what} {:?} contains a line break or a control character", t.chars().take(40).collect::<String>()));
    }
    if t.chars().count() > max {
        return Err(format!("{what} {:?}… is longer than {max} characters", t.chars().take(40).collect::<String>()));
    }
    Ok(t.to_string())
}

impl CustomCodeSet {
    /// A set checked and cleaned: a name, at most [`MAX_CUSTOM_CODES`] one-line codes (each at
    /// most [`MAX_CUSTOM_NAME_CHARS`]), no code twice (ignoring case), descriptions on one line.
    /// Codes left empty are dropped.
    pub fn new(name: &str, codes: &[(String, String)]) -> Result<Self, String> {
        let name = one_line(name, "the set name", MAX_CUSTOM_NAME_CHARS)?;
        if name.is_empty() {
            return Err("give the code set a name".into());
        }
        if CODE_SETS.iter().any(|s| s.name.eq_ignore_ascii_case(&name) || s.id.eq_ignore_ascii_case(&name)) {
            return Err(format!("{name:?} is the name of a built-in set; choose another"));
        }
        if codes.len() > MAX_CUSTOM_CODES {
            return Err(format!("a code set holds at most {MAX_CUSTOM_CODES} codes"));
        }
        let mut out: Vec<CustomCode> = Vec::new();
        for (code, description) in codes {
            let code = one_line(code, "the code", MAX_CUSTOM_NAME_CHARS)?;
            if code.is_empty() {
                continue;
            }
            if out.iter().any(|c| c.code.eq_ignore_ascii_case(&code)) {
                return Err(format!("{code:?} is in the set twice"));
            }
            let description = one_line(description, "the description", MAX_CUSTOM_DESCRIPTION_CHARS)?;
            out.push(CustomCode { code, description });
        }
        if out.is_empty() {
            return Err(format!("{name:?} has no codes"));
        }
        Ok(Self { name, codes: out })
    }

    pub fn find(&self, code: &str) -> Option<&CustomCode> {
        let code = code.trim();
        self.codes.iter().find(|c| c.code.eq_ignore_ascii_case(code))
    }

    /// The overlay text for `codes` of this set (as [`overlay_text`] for a built-in set).
    pub fn overlay_text(&self, codes: &[&str]) -> Result<String, String> {
        let mut out: Vec<&str> = Vec::new();
        for code in codes {
            let found = self.find(code).ok_or_else(|| {
                let known: Vec<&str> = self.codes.iter().map(|c| c.code.as_str()).take(30).collect();
                format!("{:?} is not a code of {} (codes: {})", code.chars().take(40).collect::<String>(), self.name, known.join(", "))
            })?;
            if !out.contains(&found.code.as_str()) {
                out.push(found.code.as_str());
            }
        }
        join(&out).map_err(|e| e.to_string())
    }
}

/// Add `set` to `sets`, replacing a set of the same name (ignoring case). Refused once
/// [`MAX_CUSTOM_SETS`] are kept.
pub fn save_custom_set(sets: &mut Vec<CustomCodeSet>, set: CustomCodeSet) -> Result<(), String> {
    if let Some(old) = sets.iter_mut().find(|s| s.name.eq_ignore_ascii_case(&set.name)) {
        *old = set;
        return Ok(());
    }
    if sets.len() >= MAX_CUSTOM_SETS {
        return Err(format!("at most {MAX_CUSTOM_SETS} code sets are kept; delete one first"));
    }
    sets.push(set);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_sets_are_checked_saved_and_cited() {
        let codes = |list: &[(&str, &str)]| list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        let set = CustomCodeSet::new(" Contract ", &codes(&[("C-1", "Pricing"), (" C-2 ", ""), ("", "dropped")])).unwrap();
        assert_eq!(set.name, "Contract");
        assert_eq!(set.codes.iter().map(|c| c.code.as_str()).collect::<Vec<_>>(), ["C-1", "C-2"]);
        assert_eq!(set.overlay_text(&["c-2", "C-1", "C-2"]).unwrap(), "C-2, C-1");
        assert!(set.overlay_text(&["C-3"]).is_err());
        assert!(CustomCodeSet::new("", &codes(&[("a", "")])).is_err());
        assert!(CustomCodeSet::new("U.S. FOIA", &codes(&[("a", "")])).is_err(), "built-in names are taken");
        assert!(CustomCodeSet::new("x", &codes(&[("a", ""), ("A", "")])).is_err(), "duplicates");
        assert!(CustomCodeSet::new("x", &codes(&[("a\nb", "")])).is_err());
        assert!(CustomCodeSet::new("x", &codes(&[("a", "\u{7}")])).is_err());
        assert!(CustomCodeSet::new("x", &codes(&[])).is_err());
        assert!(CustomCodeSet::new(&"n".repeat(65), &codes(&[("a", "")])).is_err());
        let many: Vec<(String, String)> = (0..=MAX_CUSTOM_CODES).map(|i| (format!("c{i}"), String::new())).collect();
        assert!(CustomCodeSet::new("x", &many).is_err());
        let mut sets = Vec::new();
        save_custom_set(&mut sets, set.clone()).unwrap();
        let mut renamed = set.clone();
        renamed.name = "CONTRACT".into();
        save_custom_set(&mut sets, renamed).unwrap();
        assert_eq!(sets.len(), 1, "same name replaces");
        for i in 1..MAX_CUSTOM_SETS {
            save_custom_set(&mut sets, CustomCodeSet { name: format!("s{i}"), ..set.clone() }).unwrap();
        }
        assert!(save_custom_set(&mut sets, CustomCodeSet { name: "one more".into(), ..set }).is_err());
    }

    #[test]
    fn code_sets_are_well_formed() {
        let mut ids = std::collections::BTreeSet::new();
        for set in CODE_SETS {
            assert!(ids.insert(set.id), "duplicate set {}", set.id);
            assert_eq!(CodeSet::from_id(set.id), Some(&set));
            let mut codes = std::collections::BTreeSet::new();
            for c in set.codes {
                assert!(codes.insert(c.code.to_ascii_lowercase()), "duplicate code {} in {}", c.code, set.id);
                assert!(c.code.is_ascii() && !c.description.is_empty());
            }
        }
        assert_eq!(US_FOIA.codes.len(), 14);
        assert_eq!(CodeSet::from_id("nope"), None);
    }

    #[test]
    fn codes_become_overlay_text() {
        assert_eq!(overlay_text(&US_FOIA, &["(b)(6)"]).unwrap(), "(b)(6)");
        assert_eq!(overlay_text(&US_FOIA, &["(B)(7)(c)", " (b)(6) ", "(b)(7)(C)"]).unwrap(), "(b)(7)(C), (b)(6)");
        assert_eq!(overlay_text(&US_PRIVACY_ACT, &["(k)(2)"]).unwrap(), "(k)(2)");
        assert!(matches!(overlay_text(&US_FOIA, &["(k)(2)"]), Err(CodeError::Unknown { .. })));
        assert_eq!(overlay_text(&US_FOIA, &[]), Err(CodeError::Empty));
        assert_eq!(custom_overlay_text(&["Exempt 1", "Exempt 2"]).unwrap(), "Exempt 1, Exempt 2");
        assert_eq!(custom_overlay_text(&["a\nb"]), Err(CodeError::Malformed));
        assert_eq!(custom_overlay_text(&[""]), Err(CodeError::Malformed));
        let long = "x".repeat(MAX_OVERLAY_CHARS);
        assert_eq!(custom_overlay_text(&[long.as_str(), "y"]), Err(CodeError::TooLong));
    }
}
