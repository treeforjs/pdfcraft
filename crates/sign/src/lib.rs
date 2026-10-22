//! Digital signatures (execution plan M9; ISO 32000-2 §12.8; ETSI EN 319 142 PAdES).
//!
//! - [`der`], [`x509`], [`cms`], [`pkcs12`], [`keys`]: the cryptographic formats, on RustCrypto
//!   primitives (RSA private-key operations use aws-lc-rs on native targets, ADR-0009, and the
//!   pure-Rust `rsa` crate in the browser). [`id_file`] opens digital ID files given as bytes.
//! - [`pdf`]: signature fields in a document — listing and validating them, and signing
//!   (PAdES B-B, `ETSI.CAdES.detached`) with an incremental save.
//! - [`signed_version`]: a signature's signed version, and what changed since it was signed.
//! - [`os_trust`]: the operating system's root certificates, trusted only when the user opts in.
//! - [`token`]: smart cards, USB tokens and HSMs (PKCS #11) behind a trait, with a software mock.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod cms;
pub mod der;
pub mod dss;
mod ec512;
pub mod envelope;
pub mod evidence;
pub mod id_file;
#[cfg(target_os = "macos")]
pub mod keychain;
pub mod keys;
pub mod lock;
pub mod os_trust;
pub mod pdf;
pub mod pkcs12;
pub mod revocation;
mod rsa_pad;
mod rsa_soft;
pub mod signed_version;
pub mod timestamp;
pub mod token;
#[cfg(target_os = "windows")]
pub mod windows;
pub mod x509;

pub use der::Time;
pub use keys::{DigestAlg, PrivateKey, PublicKey};
pub use lock::{FieldLock, LockAction};
pub use pdf::{
    Appearance, DigestCache, MAX_OUTLINE_REVISIONS, Modification, PageChange, RevisionChanges, SignOptions, SignatureInfo, Status, TrustPurpose,
    TrustSource, TrustStore, VerificationTime, VerificationTimeSource, list as signatures, revision_changes, sign, sign_with_timestamp,
    timestamp_document, validate,
};
pub use pkcs12::DigitalId;
pub use signed_version::{ChangeAction, SignedVersionComparison, VersionChange, compare_signed_version, signed_version};
pub use timestamp::{TimestampAuthority, TimestampQuery, TimestampToken};
pub use x509::{Certificate, Name};

#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error("malformed data: {0}")]
    Malformed(String),
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("the password is incorrect")]
    WrongPassword,
    #[error("{0}")]
    Crypto(String),
    #[error("{0}")]
    Pdf(String),
    #[error(transparent)]
    Cos(#[from] pdfcraft_cos::CosError),
}
