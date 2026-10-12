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

#[cfg(test)]
mod tests {
    use super::*;

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
