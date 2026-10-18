//! Preferences ▸ Signatures ▸ trust the operating system's root certificates (sign.os-trust):
//! Acrobat's "Trust ALL root certificates in the Windows Certificate Store for validating
//! signatures / validating certified documents" (and the macOS Keychain equivalent). Both are
//! off by default. When one is on, the roots are read from [`OsRootSource`] (the platform's
//! store unless a bundle or directory was chosen) and added to the session's trust store for
//! that purpose; every open document is revalidated.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use pdfcraft_sign::os_trust::{OsRoots, RootSource as OsRootSource};

/// The two settings. `certified_documents` applies only while `signatures` is on, as in
/// Acrobat, where its box is unavailable otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsTrustSettings {
    /// Trust the operating system's roots for validating signatures (and timestamps).
    pub signatures: bool,
    /// Trust them for validating certification signatures (certified documents).
    pub certified_documents: bool,
}

/// The session's state: settings, where the roots come from and the roots once read.
#[derive(Clone, Debug, Default)]
pub(crate) struct OsTrust {
    pub(crate) settings: OsTrustSettings,
    pub(crate) source: OsRootSource,
    pub(crate) roots: Option<Arc<OsRoots>>,
}

impl OsTrust {
    /// The roots, read from the source the first time they are needed.
    pub(crate) fn roots(&mut self) -> Result<Arc<OsRoots>, pdfcraft_sign::SignError> {
        if let Some(r) = &self.roots {
            return Ok(r.clone());
        }
        let loaded = Arc::new(pdfcraft_sign::os_trust::load(&self.source)?);
        self.roots = Some(loaded.clone());
        Ok(loaded)
    }
}
