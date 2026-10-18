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

/// The bits of a submit-form action's `/Flags` (ISO 32000-2 Table 239), 1-based as the
/// specification numbers them.
pub mod flag {
    /// 1: `/Fields` lists the fields *not* to send.
    pub const EXCLUDE: u32 = 1 << 0;
    /// 2: send fields with no value too.
    pub const INCLUDE_NO_VALUE_FIELDS: u32 = 1 << 1;
    /// 3: send as an HTML form.
    pub const EXPORT_FORMAT: u32 = 1 << 2;
    /// 4: HTML: a GET query instead of a POST body.
    pub const GET_METHOD: u32 = 1 << 3;
    /// 5: HTML: send where the button was clicked (`name.x`, `name.y`).
    pub const SUBMIT_COORDINATES: u32 = 1 << 4;
    /// 6: send XFDF.
    pub const XFDF: u32 = 1 << 5;
    /// 7: FDF: send the incremental updates since the file was opened (`/Differences`).
    pub const INCLUDE_APPEND_SAVES: u32 = 1 << 6;
    /// 8: FDF and XFDF: send the markup annotations too.
    pub const INCLUDE_ANNOTATIONS: u32 = 1 << 7;
    /// 9: send the whole document as PDF.
    pub const SUBMIT_PDF: u32 = 1 << 8;
    /// 10: send dates in the canonical `D:YYYYMMDDHHmmSS` form.
    pub const CANONICAL_FORMAT: u32 = 1 << 9;
    /// 11: with IncludeAnnotations, only the current user's annotations.
    pub const EXCL_NON_USER_ANNOTS: u32 = 1 << 10;
    /// 12: FDF: leave out `/F` (the source file's name).
    pub const EXCL_F_KEY: u32 = 1 << 11;
    /// 14: FDF: embed the whole document in the FDF.
    pub const EMBED_FORM: u32 = 1 << 13;
    /// Every flag, with the name the specification gives it.
    pub const NAMES: [(u32, &str); 13] = [
        (EXCLUDE, "Exclude"),
        (INCLUDE_NO_VALUE_FIELDS, "IncludeNoValueFields"),
        (EXPORT_FORMAT, "ExportFormat"),
        (GET_METHOD, "GetMethod"),
        (SUBMIT_COORDINATES, "SubmitCoordinates"),
        (XFDF, "XFDF"),
        (INCLUDE_APPEND_SAVES, "IncludeAppendSaves"),
        (INCLUDE_ANNOTATIONS, "IncludeAnnotations"),
        (SUBMIT_PDF, "SubmitPDF"),
        (CANONICAL_FORMAT, "CanonicalFormat"),
        (EXCL_NON_USER_ANNOTS, "ExclNonUserAnnots"),
        (EXCL_F_KEY, "ExclFKey"),
        (EMBED_FORM, "EmbedForm"),
    ];
    /// Every defined bit.
    pub const ALL: u32 = EXCLUDE
        | INCLUDE_NO_VALUE_FIELDS
        | EXPORT_FORMAT
        | GET_METHOD
        | SUBMIT_COORDINATES
        | XFDF
        | INCLUDE_APPEND_SAVES
        | INCLUDE_ANNOTATIONS
        | SUBMIT_PDF
        | CANONICAL_FORMAT
        | EXCL_NON_USER_ANNOTS
        | EXCL_F_KEY
        | EMBED_FORM;

    /// The flag named `name` (as in [`NAMES`], any case).
    pub fn by_name(name: &str) -> Option<u32> {
        NAMES.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(b, _)| *b)
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
    /// HTML only: send the click position as `name.x` / `name.y` (`SubmitCoordinates`, bit 5).
    pub coordinates: bool,
    /// FDF only: send the incremental updates as `/Differences` (`IncludeAppendSaves`, bit 7).
    pub append_saves: bool,
    /// FDF and XFDF: send the markup annotations (`IncludeAnnotations`, bit 8).
    pub annotations: bool,
    /// Dates in canonical form (`CanonicalFormat`, bit 10).
    pub canonical: bool,
    /// With `annotations`: only the current user's (`ExclNonUserAnnots`, bit 11).
    pub user_annotations_only: bool,
    /// FDF only: no `/F` entry (`ExclFKey`, bit 12).
    pub no_file_key: bool,
    /// FDF only: the document embedded in the FDF (`EmbedForm`, bit 14).
    pub embed_form: bool,
}

/// At most this many `/Fields` entries are read.
const MAX_LISTED: usize = 10_000;

impl Submission {
    /// `this.submitForm(url)` and actions without flags: every field, as FDF.
    pub fn to_url(url: &str) -> Self {
        Self::with_flags(url, 0, Vec::new())
    }

    /// The action `/F url /Flags flags /Fields fields` reads as. The format follows the
    /// specification's precedence: SubmitPDF, then XFDF, then ExportFormat, else FDF; flags
    /// that the chosen format ignores are cleared, so [`Submission::flags`] says what is used.
    pub fn with_flags(url: &str, flags: u32, fields: Vec<String>) -> Self {
        let bit = |b: u32| flags & b != 0;
        let format = if bit(flag::SUBMIT_PDF) {
            SubmitFormat::Pdf
        } else if bit(flag::XFDF) {
            SubmitFormat::Xfdf
        } else if bit(flag::EXPORT_FORMAT) {
            SubmitFormat::Html
        } else {
            SubmitFormat::Fdf
        };
        let html = format == SubmitFormat::Html;
        let fdf = format == SubmitFormat::Fdf;
        let annotations = bit(flag::INCLUDE_ANNOTATIONS) && matches!(format, SubmitFormat::Fdf | SubmitFormat::Xfdf);
        // SubmitPDF: "all other flags shall be ignored".
        let pdf = format == SubmitFormat::Pdf;
        Self {
            url: url.to_string(),
            format,
            get: html && bit(flag::GET_METHOD),
            fields: if pdf { Vec::new() } else { fields },
            exclude: !pdf && bit(flag::EXCLUDE),
            include_empty: !pdf && bit(flag::INCLUDE_NO_VALUE_FIELDS),
            coordinates: html && bit(flag::SUBMIT_COORDINATES),
            append_saves: fdf && bit(flag::INCLUDE_APPEND_SAVES),
            annotations,
            canonical: !pdf && bit(flag::CANONICAL_FORMAT),
            user_annotations_only: annotations && bit(flag::EXCL_NON_USER_ANNOTS),
            no_file_key: fdf && bit(flag::EXCL_F_KEY),
            embed_form: fdf && bit(flag::EMBED_FORM),
        }
    }

    /// The `/Flags` value for this submission (only the bits its format uses).
    pub fn flags(&self) -> u32 {
        let mut f = 0;
        let mut put = |on: bool, b: u32| {
            if on {
                f |= b;
            }
        };
        put(self.format == SubmitFormat::Pdf, flag::SUBMIT_PDF);
        put(self.format == SubmitFormat::Xfdf, flag::XFDF);
        put(self.format == SubmitFormat::Html, flag::EXPORT_FORMAT);
        put(self.get, flag::GET_METHOD);
        put(self.exclude, flag::EXCLUDE);
        put(self.include_empty, flag::INCLUDE_NO_VALUE_FIELDS);
        put(self.coordinates, flag::SUBMIT_COORDINATES);
        put(self.append_saves, flag::INCLUDE_APPEND_SAVES);
        put(self.annotations, flag::INCLUDE_ANNOTATIONS);
        put(self.canonical, flag::CANONICAL_FORMAT);
        put(self.user_annotations_only, flag::EXCL_NON_USER_ANNOTS);
        put(self.no_file_key, flag::EXCL_F_KEY);
        put(self.embed_form, flag::EMBED_FORM);
        f
    }

    /// The same submission in another format, keeping the flags that format still uses.
    pub fn in_format(&self, format: SubmitFormat) -> Self {
        let mut flags = self.flags() & !(flag::SUBMIT_PDF | flag::XFDF | flag::EXPORT_FORMAT);
        flags |= match format {
            SubmitFormat::Pdf => flag::SUBMIT_PDF,
            SubmitFormat::Xfdf => flag::XFDF,
            SubmitFormat::Html => flag::EXPORT_FORMAT,
            SubmitFormat::Fdf => 0,
        };
        Self::with_flags(&self.url, flags, self.fields.clone())
    }

    /// The values sent for `field`: its own, except that with `canonical` a date field
    /// (an `AFDate_FormatEx` format) sends each value as `D:YYYYMMDDHHmmSS` (§7.9.4). A value
    /// that doesn't read as a date under the field's format is sent as it is.
    pub fn values(&self, field: &Field) -> Vec<String> {
        let crate::af::Format::Date(fmt) = &field.actions.format else { return field.value.clone() };
        if !self.canonical {
            return field.value.clone();
        }
        field
            .value
            .iter()
            .map(|v| match crate::af::parse_date(v, fmt) {
                Some(dt) => format!("D:{:04}{:02}{:02}{:02}{:02}{:02}", dt.y, dt.m, dt.d, dt.hh, dt.mm, dt.ss),
                None => v.clone(),
            })
            .collect()
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
    // Only the low 32 bits carry flags; anything else (negative, huge) is read as its bits.
    let flags = a.get(b"Flags").and_then(|f| doc.resolve(f).as_int()).unwrap_or(0);
    let flags = (flags & i64::from(u32::MAX)) as u32;
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
    Submission::with_flags(&url, flags, fields)
}
