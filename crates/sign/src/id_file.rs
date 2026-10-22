//! Digital ID files given as bytes rather than paths: a file picked in the browser, or one an
//! automation client sends inline. The bytes and the password are hostile; neither is kept.

use crate::{DigitalId, SignError};

/// The largest digital ID file read. Real PKCS #12 files hold a key and a short chain (a few
/// KiB); anything near this is not one.
pub const MAX_ID_FILE_BYTES: usize = 1024 * 1024;

/// Open a PKCS #12 (.p12/.pfx) digital ID from its bytes with `password`. Refuses files over
/// [`MAX_ID_FILE_BYTES`] before parsing anything.
pub fn open(bytes: &[u8], password: &str) -> Result<DigitalId, SignError> {
    if bytes.is_empty() {
        return Err(SignError::Malformed("the digital ID file is empty".into()));
    }
    if bytes.len() > MAX_ID_FILE_BYTES {
        return Err(SignError::Malformed(format!("the digital ID file is larger than {} KiB", MAX_ID_FILE_BYTES / 1024)));
    }
    crate::pkcs12::open(bytes, password)
}

/// A digital ID file's name made safe to show and to key on: its last path component, control
/// and bidirectional-override characters dropped, at most 120 characters; `"Digital ID"` when
/// nothing is left.
pub fn display_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        .take(120)
        .collect();
    let clean = clean.trim();
    if clean.is_empty() { "Digital ID".to_string() } else { clean.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Certificate, Name, PrivateKey, Time};

    fn p12(password: &str) -> Vec<u8> {
        let key = PrivateKey::generate_p256().unwrap();
        let dn = Name::build("Ada Lovelace", "", "", "", "");
        let cert = Certificate::self_signed(&dn, &key, Time::from_unix(1_700_000_000), 5, &[1, 2, 3]).unwrap();
        crate::pkcs12::write(&DigitalId { key, certificate: cert, chain: Vec::new(), friendly_name: None }, password).unwrap()
    }

    #[test]
    fn opens_with_the_password_only() {
        let bytes = p12("secret1");
        assert_eq!(open(&bytes, "secret1").unwrap().certificate.display_name(), "Ada Lovelace");
        assert!(matches!(open(&bytes, "nope"), Err(SignError::WrongPassword)));
    }

    #[test]
    fn hostile_files_are_errors() {
        let bytes = p12("secret1");
        assert!(open(&[], "secret1").is_err());
        assert!(open(&vec![0x30; MAX_ID_FILE_BYTES + 1], "secret1").is_err());
        for cut in [1, 4, bytes.len() / 2, bytes.len() - 1] {
            assert!(open(&bytes[..cut], "secret1").is_err());
        }
        let mut flipped = bytes.clone();
        for i in (0..flipped.len()).step_by(97) {
            flipped[i] ^= 0x5A;
            let _ = open(&flipped, "secret1");
        }
        assert!(open(b"\x30\x84\xff\xff\xff\xff", "").is_err());
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(display_name("C:\\Users\\ada\\id.p12"), "id.p12");
        assert_eq!(display_name("/home/ada/my id.pfx"), "my id.pfx");
        assert_eq!(display_name("evil\u{202E}21p.exe"), "evil21p.exe");
        assert_eq!(display_name("\n\t"), "Digital ID");
        assert_eq!(display_name(&"x".repeat(500)).chars().count(), 120);
    }
}
