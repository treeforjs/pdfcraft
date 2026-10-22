//! pdfcraft-cos — the PDF object layer (L1, standalone with `pdfcraft-filters`).
//!
//! Parses the COS object graph lazily and tolerantly, keeps edits in a copy-on-write overlay
//! (cheap snapshots for undo), and writes documents back either incrementally (original bytes
//! untouched) or as a full, garbage-collected rewrite. The PDF object graph *is* PdfCraft's
//! document model (plan/architecture.md §5, ADR-0010).
//!
//! Done: every cross-reference form (tables, streams, hybrid `/XRefStm`), object streams, repair
//! by scanning, encryption on load and save (R2–R6, crypt filters, `/EFF`), incremental writing,
//! full saves that pack objects into compressed object streams with a cross-reference stream, and
//! linearized output with its checker (`core.linearization`, Annex F).
//! Parse caches are bounded whatever the size of the file (`core.lazy-loading`,
//! [`Document::cache_footprint`]). Not yet: reading the file through a `ByteSource` instead of
//! one in-memory buffer, and linearized first-page-first loading. See the README.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod bytes;
mod cache;
mod document;
mod linearize;
mod object;
pub mod page_labels;
mod parser;
mod security;
mod writer;

pub use bytes::Bytes;
pub use document::{CacheFootprint, Document, Revision, SourceIdentity, XrefEntry};
pub use linearize::{LinearizationParams, LinearizationReport, check_linearization, fast_web_view, write_linearized};
pub use object::{Dict, MAX_DECODED, Name, ObjRef, Object, PdfString, Stream};
pub use parser::{Lexer, parse_indirect};
pub use pdfcraft_crypt::{
    Algorithm, Auth, Credential, Method as CryptMethod, NewEncryption, NewPublicKeyEncryption, Permissions, RecipientKey, SecurityHandler,
    envelope_content,
};
pub use writer::{SaveOptions, pdf_date, serialize, write_decrypted_view, write_full, write_incremental};

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum CosError {
    #[error("not a PDF file (no %PDF- header)")]
    NotPdf,
    #[error("syntax error at byte {offset}: {detail}")]
    Syntax { offset: usize, detail: String },
    #[error("object {0} is missing")]
    MissingObject(u32),
    #[error("object {0} is not a dictionary")]
    NotADictionary(ObjRef),
    #[error("stream data could not be decoded: {0}")]
    Filter(String),
    #[error("the document is protected by a password")]
    NeedsPassword,
    #[error("the password is incorrect")]
    WrongPassword,
    #[error("the document is encrypted for specific recipients (certificate security); open it with a digital ID")]
    NeedsCertificate,
    #[error("the digital ID is not one of the document's recipients")]
    NotARecipient,
    #[error("unsupported security: {0}")]
    Security(String),
    #[error("internal lock poisoned")]
    Poisoned,
    #[error("this document was opened read-only with a decode limit and cannot be saved")]
    ReadOnlyLimit,
    #[error("the document can't be linearized: {0}")]
    Linearize(String),
}
