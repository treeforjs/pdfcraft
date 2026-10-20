//! The operating system's root certificates, for trusting them when validating signatures
//! (PDF: Preferences ▸ Signatures ▸ Identities & Trusted Certificates ▸ "Trust ALL root
//! certificates in the Windows Certificate Store for validating signatures / certified
//! documents", and the macOS Keychain equivalent).
//!
//! [`load`] reads them from a [`RootSource`]: the platform's store (macOS keychains and
//! Windows' Current User and Local Machine stores through `rustls-native-certs`'s safe wrappers;
//! on Linux and the BSDs the system CA bundle and directory it finds, honouring `SSL_CERT_FILE` /
//! `SSL_CERT_DIR`), or an explicit PEM/DER bundle file or certificate directory (a custom store,
//! and how tests inject a fake "operating system"). Loading never fails because of one bad
//! entry: a malformed file or certificate is skipped and counted, and the number and size of
//! what is read are capped. The roots are only used as anchors when [`crate::TrustStore`]'s
//! settings say so.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::SignError;
use crate::x509::Certificate;

/// At most this many root certificates are kept (real stores hold a few hundred).
pub const MAX_ROOTS: usize = 4096;
/// A bundle file larger than this is refused.
pub const MAX_BUNDLE_BYTES: u64 = 32 * 1024 * 1024;
/// A file in a certificate directory larger than this is skipped.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// At most this many directory entries are looked at.
pub const MAX_DIR_ENTRIES: usize = 16_384;
/// At most this many problem messages are kept (the rest are only counted).
pub const MAX_ERRORS: usize = 16;

/// Where the operating system's roots come from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RootSource {
    /// The platform's own certificate store.
    #[default]
    System,
    /// A PEM file with any number of `CERTIFICATE` blocks, or one DER certificate.
    Bundle(PathBuf),
    /// A directory of PEM or DER certificate files (like `/etc/ssl/certs`).
    Directory(PathBuf),
}

impl RootSource {
    /// A short description for reports: `system`, `bundle:<path>`, `directory:<path>`.
    pub fn describe(&self) -> String {
        match self {
            RootSource::System => "system".into(),
            RootSource::Bundle(p) => format!("bundle:{}", p.display()),
            RootSource::Directory(p) => format!("directory:{}", p.display()),
        }
    }
}

/// The roots read from a [`RootSource`], with what could not be read.
#[derive(Clone, Debug, Default)]
pub struct OsRoots {
    pub source: RootSource,
    /// Distinct certificates, in the order found.
    pub certificates: Vec<Certificate>,
    /// Entries (files, PEM blocks, store items) that could not be read or parsed.
    pub skipped: usize,
    /// Certificates seen more than once (directories hold hash-named links to the same file).
    pub duplicates: usize,
    /// More than [`MAX_ROOTS`] certificates (or [`MAX_DIR_ENTRIES`] entries) were found; the
    /// rest were not read.
    pub truncated: bool,
    /// False when this platform has no certificate store PdfCraft can read (the browser).
    pub supported: bool,
    /// Up to [`MAX_ERRORS`] messages about what was skipped.
    pub errors: Vec<String>,
}

impl OsRoots {
    fn new(source: RootSource) -> Self {
        Self { source, supported: true, ..Self::default() }
    }

    fn note(&mut self, message: String) {
        self.skipped = self.skipped.saturating_add(1);
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(message);
        }
    }

    /// Add one DER certificate; returns false once the cap is reached.
    fn push_der(&mut self, der: &[u8], seen: &mut HashSet<Vec<u8>>, origin: &str) -> bool {
        if self.certificates.len() >= MAX_ROOTS {
            self.truncated = true;
            return false;
        }
        match Certificate::parse(der) {
            Ok(c) => {
                if seen.insert(c.raw.clone()) {
                    self.certificates.push(c);
                } else {
                    self.duplicates = self.duplicates.saturating_add(1);
                }
            }
            Err(e) => self.note(format!("{origin}: {e}")),
        }
        true
    }

    /// Add every certificate in `bytes` (one DER certificate, or PEM `CERTIFICATE` blocks);
    /// returns false once the cap is reached.
    fn push_file(&mut self, bytes: &[u8], seen: &mut HashSet<Vec<u8>>, origin: &str) -> bool {
        if bytes.first() == Some(&0x30) {
            return self.push_der(bytes, seen, origin);
        }
        let text = String::from_utf8_lossy(bytes);
        let mut block: Option<String> = None;
        let mut found = false;
        for line in text.lines().map(str::trim) {
            if line == "-----BEGIN CERTIFICATE-----" {
                block = Some(String::new());
            } else if line == "-----END CERTIFICATE-----" {
                let Some(b) = block.take() else { continue };
                found = true;
                match crate::x509::base64(&b) {
                    Some(der) => {
                        if !self.push_der(&der, seen, origin) {
                            return false;
                        }
                    }
                    None => self.note(format!("{origin}: a CERTIFICATE block is not valid base64")),
                }
            } else if let Some(b) = block.as_mut() {
                // A block that never ends must not grow without bound.
                if b.len() > MAX_FILE_BYTES as usize {
                    block = None;
                    self.note(format!("{origin}: a CERTIFICATE block is too long"));
                } else {
                    b.push_str(line);
                }
            }
        }
        if !found {
            self.note(format!("{origin}: no certificate found (expected DER or PEM)"));
        }
        true
    }
}

/// Read up to `limit` bytes of `path`; `None` (with a note) when it is unreadable or larger.
fn read_capped(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!("larger than {limit} bytes"));
    }
    Ok(bytes)
}

/// Read the roots from `source`. Fails only when an explicit bundle or directory cannot be
/// opened at all; anything unreadable inside it is skipped and reported.
pub fn load(source: &RootSource) -> Result<OsRoots, SignError> {
    let mut roots = OsRoots::new(source.clone());
    let mut seen = HashSet::new();
    match source {
        RootSource::System => load_system(&mut roots, &mut seen),
        RootSource::Bundle(path) => {
            let bytes = read_capped(path, MAX_BUNDLE_BYTES).map_err(|e| SignError::Malformed(format!("{}: {e}", path.display())))?;
            roots.push_file(&bytes, &mut seen, &path.display().to_string());
        }
        RootSource::Directory(dir) => {
            let entries = std::fs::read_dir(dir).map_err(|e| SignError::Malformed(format!("{}: {e}", dir.display())))?;
            let mut paths = Vec::new();
            for entry in entries {
                if paths.len() >= MAX_DIR_ENTRIES {
                    roots.truncated = true;
                    break;
                }
                match entry {
                    Ok(e) => paths.push(e.path()),
                    Err(e) => roots.note(format!("{}: {e}", dir.display())),
                }
            }
            paths.sort();
            for path in paths {
                // Follows links (hash-named links point at the real files); skips directories.
                let Ok(meta) = std::fs::metadata(&path) else { continue };
                if !meta.is_file() {
                    continue;
                }
                let name = path.display().to_string();
                if meta.len() > MAX_FILE_BYTES {
                    roots.note(format!("{name}: larger than {MAX_FILE_BYTES} bytes"));
                    continue;
                }
                match read_capped(&path, MAX_FILE_BYTES) {
                    Ok(bytes) => {
                        if !roots.push_file(&bytes, &mut seen, &name) {
                            break;
                        }
                    }
                    Err(e) => roots.note(format!("{name}: {e}")),
                }
            }
        }
    }
    Ok(roots)
}

#[cfg(not(target_arch = "wasm32"))]
fn load_system(roots: &mut OsRoots, seen: &mut HashSet<Vec<u8>>) {
    let found = rustls_native_certs::load_native_certs();
    for e in found.errors {
        roots.note(format!("system store: {e}"));
    }
    for (i, der) in found.certs.iter().enumerate() {
        if !roots.push_der(der.as_ref(), seen, &format!("system store item {}", i.saturating_add(1))) {
            break;
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn load_system(roots: &mut OsRoots, _seen: &mut HashSet<Vec<u8>>) {
    roots.supported = false;
    roots.errors.push("the browser gives web pages no access to the operating system's certificate store".into());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pdfcraft-os-trust-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cert(cn: &str) -> Certificate {
        let key = crate::PrivateKey::generate_p256().unwrap();
        Certificate::self_signed(&crate::Name::build(cn, "", "", "", ""), &key, crate::Time::from_unix(1_700_000_000), 5, &[1, 2, 3]).unwrap()
    }

    #[test]
    fn bundle_skips_malformed_blocks_and_dedupes() {
        let d = dir("bundle");
        let (a, b) = (cert("Root A"), cert("Root B"));
        let pem = format!(
            "{}garbage\n-----BEGIN CERTIFICATE-----\n!!!notbase64!!!\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n{}{}-----BEGIN TRUSTED CERTIFICATE-----\nAAAA\n-----END TRUSTED CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nAAAA",
            crate::x509::to_pem(&a),
            crate::x509::to_pem(&b),
            crate::x509::to_pem(&a)
        );
        std::fs::write(d.join("ca.pem"), pem).unwrap();
        let roots = load(&RootSource::Bundle(d.join("ca.pem"))).unwrap();
        assert_eq!(roots.certificates.len(), 2);
        assert_eq!(roots.duplicates, 1);
        assert_eq!(roots.skipped, 2, "{:?}", roots.errors);
        assert!(roots.supported && !roots.truncated);
        assert!(load(&RootSource::Bundle(d.join("missing.pem"))).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn directory_reads_pem_and_der_skips_bad_and_huge_files() {
        let d = dir("directory");
        let (a, b) = (cert("Root A"), cert("Root B"));
        std::fs::write(d.join("a.pem"), crate::x509::to_pem(&a)).unwrap();
        std::fs::write(d.join("b.der"), &b.raw).unwrap();
        std::fs::write(d.join("a.0"), crate::x509::to_pem(&a)).unwrap();
        std::fs::write(d.join("broken.der"), [0x30, 0x82, 0xff, 0xff, 0x01]).unwrap();
        std::fs::write(d.join("README"), "not a certificate").unwrap();
        std::fs::write(d.join("huge.pem"), vec![b'A'; MAX_FILE_BYTES as usize + 1]).unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let roots = load(&RootSource::Directory(d.clone())).unwrap();
        assert_eq!(roots.certificates.len(), 2);
        assert_eq!(roots.duplicates, 1);
        assert_eq!(roots.skipped, 3, "{:?}", roots.errors);
        assert!(load(&RootSource::Directory(d.join("missing"))).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn an_unterminated_huge_block_is_dropped_not_grown() {
        let d = dir("unterminated");
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for _ in 0..(MAX_FILE_BYTES as usize / 64 + 2) {
            pem.push_str(&"A".repeat(64));
            pem.push('\n');
        }
        std::fs::write(d.join("ca.pem"), pem).unwrap();
        let roots = load(&RootSource::Bundle(d.join("ca.pem"))).unwrap();
        assert!(roots.certificates.is_empty());
        assert!(roots.skipped >= 1);
        std::fs::remove_dir_all(d).unwrap();
    }
}
