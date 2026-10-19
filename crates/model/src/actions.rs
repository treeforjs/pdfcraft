//! JavaScript actions (ISO 32000-2 §12.6.4.17): read the script of an action dictionary and
//! make a new one. Links, bookmarks, page triggers and form fields all hold the same kind of
//! dictionary, so the reading and writing rules live here once.

use pdfcraft_cos::{Dict, Document, Object, PdfString};

/// The longest script an action may be given here (the JavaScript engine refuses longer ones
/// too, so a bigger one could never run).
pub const MAX_SCRIPT_BYTES: usize = 256 * 1024;

/// Whether `action` (resolved) is a JavaScript action dictionary.
pub fn is_javascript(doc: &Document, action: &Object) -> bool {
    doc.resolve(action).as_dict().and_then(|d| d.name(b"S")) == Some(b"JavaScript")
}

/// The script of a JavaScript action dictionary: its `/JS` text string or stream (decoded
/// within the reader's decompression bound). `None` when `action` isn't a JavaScript action or
/// has no readable `/JS`.
pub fn script(doc: &Document, action: &Object) -> Option<String> {
    let a = doc.resolve(action);
    let d = a.as_dict()?;
    if d.name(b"S") != Some(b"JavaScript") {
        return None;
    }
    let js = d.get(b"JS")?.clone();
    match &*doc.resolve(&js) {
        Object::String(s) => Some(s.to_text()),
        Object::Stream(s) => s.decoded().ok().map(|b| String::from_utf8_lossy(&b).into_owned()),
        _ => None,
    }
}

/// Why a script can't be written as an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptError {
    Empty,
    TooLong { bytes: usize },
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptError::Empty => write!(f, "type the JavaScript first"),
            ScriptError::TooLong { bytes } => {
                write!(f, "the script is too long ({} KiB; the limit is {} KiB)", bytes / 1024, MAX_SCRIPT_BYTES / 1024)
            }
        }
    }
}

impl std::error::Error for ScriptError {}

/// A new JavaScript action dictionary running `script` (`/S /JavaScript /JS (…)`, the text
/// as a text string). Blank and over-long scripts are refused.
pub fn new_javascript(script: &str) -> Result<Object, ScriptError> {
    if script.trim().is_empty() {
        return Err(ScriptError::Empty);
    }
    if script.len() > MAX_SCRIPT_BYTES {
        return Err(ScriptError::TooLong { bytes: script.len() });
    }
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("Action"));
    d.set(b"S".to_vec(), Object::name("JavaScript"));
    d.set(b"JS".to_vec(), Object::String(PdfString::text(script)));
    Ok(Object::Dict(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_read_from_strings_and_streams_and_written_back() {
        let mut doc = Document::new_empty();
        let a = new_javascript("app.alert(\"héllo\")").unwrap();
        assert!(is_javascript(&doc, &a));
        assert_eq!(script(&doc, &a).as_deref(), Some("app.alert(\"héllo\")"));
        let r = doc.add(a);
        assert_eq!(script(&doc, &Object::Ref(r)).as_deref(), Some("app.alert(\"héllo\")"));
        // A /JS stream.
        let s = doc.add(Object::Stream(pdfcraft_cos::Stream::from_raw(Dict::new(), b"console.println(1)".to_vec())));
        let mut d = Dict::new();
        d.set(b"S".to_vec(), Object::name("JavaScript"));
        d.set(b"JS".to_vec(), Object::Ref(s));
        assert_eq!(script(&doc, &Object::Dict(d)).as_deref(), Some("console.println(1)"));
        // Other actions and broken ones have no script.
        let mut uri = Dict::new();
        uri.set(b"S".to_vec(), Object::name("URI"));
        assert_eq!(script(&doc, &Object::Dict(uri)), None);
        let mut broken = Dict::new();
        broken.set(b"S".to_vec(), Object::name("JavaScript"));
        broken.set(b"JS".to_vec(), Object::Int(7));
        assert_eq!(script(&doc, &Object::Dict(broken)), None);
        assert_eq!(script(&doc, &Object::Null), None);
        // A missing reference resolves to nothing rather than failing.
        assert_eq!(script(&doc, &Object::Ref(pdfcraft_cos::ObjRef { num: 999, generation: 0 })), None);
    }

    #[test]
    fn blank_and_over_long_scripts_are_refused() {
        assert_eq!(new_javascript("  \n"), Err(ScriptError::Empty));
        let long = "x".repeat(MAX_SCRIPT_BYTES + 1);
        assert!(matches!(new_javascript(&long), Err(ScriptError::TooLong { .. })));
        assert!(new_javascript(&"x".repeat(MAX_SCRIPT_BYTES)).is_ok());
    }
}
