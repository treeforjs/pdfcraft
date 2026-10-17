//! Submit-form actions (ISO 32000-2 §12.7.6.2): where a form's data goes, in which format, and
//! which fields it carries. Building and sending the data is the caller's (the engine's) job;
//! nothing here touches the network.

use pdfcraft_cos::{Dict, Document, Object};

use crate::{Field, FieldKind, qualified_name, text_of};

/// What a submit-form action sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitFormat {
    /// Forms Data Format (the default).
    Fdf,
    /// XML Forms Data Format (`/Flags` bit 6).
    Xfdf,
    /// An HTML form submission, `name=value&…` (`ExportFormat`, bit 3).
    Html,
    /// The whole document (`SubmitPDF`, bit 9).
    Pdf,
}

impl SubmitFormat {
    /// The file extension for saving the data instead of sending it.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Fdf => "fdf",
            Self::Xfdf => "xfdf",
            Self::Html => "txt",
            Self::Pdf => "pdf",
        }
    }

    /// The MIME type it is sent as.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Fdf => "application/vnd.fdf",
            Self::Xfdf => "application/vnd.adobe.xfdf",
            Self::Html => "application/x-www-form-urlencoded",
            Self::Pdf => "application/pdf",
        }
    }

    /// How Acrobat names it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Fdf => "FDF",
            Self::Xfdf => "XFDF",
            Self::Html => "HTML",
            Self::Pdf => "PDF",
        }
    }
}

/// A submit-form action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    /// Where to send it, as written (`/F`).
    pub url: String,
    pub format: SubmitFormat,
    /// HTML only: send as a GET query instead of a POST body (`GetMethod`, bit 4).
    pub get: bool,
    /// `/Fields`: the fields (and their descendants) to send, or with `exclude` not to send.
    /// Empty: every field.
    pub fields: Vec<String>,
    pub exclude: bool,
    /// Send fields with no value too (`IncludeNoValueFields`, bit 2).
    pub include_empty: bool,
}

/// At most this many `/Fields` entries are read.
const MAX_LISTED: usize = 10_000;

impl Submission {
    /// `this.submitForm(url)` and actions without flags: every field, as FDF.
    pub fn to_url(url: &str) -> Self {
        Self { url: url.to_string(), format: SubmitFormat::Fdf, get: false, fields: Vec::new(), exclude: false, include_empty: false }
    }

    /// Whether `field` is sent: never push buttons, signature fields or fields marked
    /// NoExport (field flag 3); otherwise as `/Fields` and the Include/Exclude flag say, and
    /// fields without a value only with `include_empty`.
    pub fn sends(&self, field: &Field) -> bool {
        if matches!(field.kind, FieldKind::PushButton | FieldKind::Signature) || field.has(NO_EXPORT) {
            return false;
        }
        if !self.include_empty && field.value.iter().all(String::is_empty) {
            return false;
        }
        if self.fields.is_empty() {
            return true;
        }
        let listed = self.fields.iter().any(|l| field.name == *l || field.name.strip_prefix(l.as_str()).is_some_and(|rest| rest.starts_with('.')));
        listed != self.exclude
    }
}

/// Field flag NoExport (§12.7.4.1): never submitted.
pub const NO_EXPORT: u32 = 1 << 2;

/// Read a `/S /SubmitForm` action dictionary.
pub(crate) fn parse(doc: &Document, a: &Dict) -> Submission {
    let url = a
        .get(b"F")
        .map(|f| doc.resolve(f))
        .and_then(|f| {
            f.as_dict().and_then(|fd| fd.get(b"UF").or_else(|| fd.get(b"F")).and_then(|x| text_of(&doc.resolve(x)))).or_else(|| text_of(&f))
        })
        .unwrap_or_default();
    let flags = a.get(b"Flags").and_then(|f| doc.resolve(f).as_int()).unwrap_or(0);
    let bit = |n: u32| flags & (1i64 << (n - 1)) != 0;
    let format = if bit(9) {
        SubmitFormat::Pdf
    } else if bit(6) {
        SubmitFormat::Xfdf
    } else if bit(3) {
        SubmitFormat::Html
    } else {
        SubmitFormat::Fdf
    };
    let fields = a
        .get(b"Fields")
        .map(|f| doc.resolve(f))
        .and_then(|f| f.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .take(MAX_LISTED)
        .filter_map(|o| match o {
            Object::Ref(r) => qualified_name(doc, *r).or_else(|| text_of(&doc.resolve(o))),
            other => text_of(other),
        })
        .collect();
    Submission { url, format, get: format == SubmitFormat::Html && bit(4), fields, exclude: bit(1), include_empty: bit(2) }
}
