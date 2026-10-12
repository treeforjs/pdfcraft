//! The public-key security handler's keys (ISO 32000-2 §7.6.5): `/Filter /Adobe.PubSec`.
//!
//! Each `/Recipients` entry is a PKCS #7 (CMS) `EnvelopedData` whose content is a 20-byte seed
//! followed by the 4 permission bytes (big-endian) for the recipients it is addressed to. The
//! file key is the SHA-1 digest (SHA-256 for AES-256) of the seed, every `/Recipients` entry in
//! order and, when metadata stays unencrypted, `FF FF FF FF` (§7.6.5.3).
//!
//! Permissions (Table 24) are the standard bits plus bit 2, "change security settings, and
//! every other permission": a recipient with bit 2 is authenticated as [`Auth::Owner`]
//! (owner-equivalent: no restriction applies and security may be changed or removed); every
//! other recipient as [`Auth::User`] with exactly the bits of its envelope.
//!
//! This crate opens no envelopes itself. The caller supplies a [`RecipientKey`], which holds the
//! private key and the CMS code (in PdfCraft, `pdfcraft-sign`).

use sha1::Sha1;
use sha2::{Digest as _, Sha256};

use crate::{Algorithm, Auth, CryptError, EncryptDict, Method, SecurityHandler};

/// `/Filter` of the public-key security handler.
pub const PUBLIC_KEY_FILTER: &[u8] = b"Adobe.PubSec";
/// The SubFilter PdfCraft writes: envelopes in the crypt filter, crypt filters for streams and
/// strings (PDF 1.6 and later; the only one PDF 2.0 keeps besides s4).
pub const SUBFILTER_S5: &[u8] = b"adbe.pkcs7.s5";
/// The most `/Recipients` entries read. Acrobat writes one per distinct permission set.
pub const MAX_RECIPIENTS: usize = 1024;
/// The largest single `/Recipients` entry read (an envelope for thousands of recipients is still
/// far smaller).
pub const MAX_RECIPIENT_BYTES: usize = 4 << 20;
/// Seed length (§7.6.5.3: 20 bytes).
const SEED_LEN: usize = 20;

/// A recipient's private key: opens the envelopes addressed to it.
pub trait RecipientKey: Send + Sync {
    /// The content of the PKCS #7 `EnvelopedData` `envelope`, or `None` when it has no
    /// recipient matching this key or cannot be read. `envelope` is untrusted input.
    fn open_envelope(&self, envelope: &[u8]) -> Option<Vec<u8>>;
}

/// Hash and key length for the file key: SHA-256 and 32 bytes for AES-256, otherwise SHA-1
/// truncated to `/Length` (5–16 bytes).
fn file_key(d: &EncryptDict, seed: &[u8]) -> Vec<u8> {
    let aes256 = d.v >= 5 || d.crypt_filters.iter().any(|(n, m)| *n == d.stm_f && *m == Method::Aes256);
    let tail: &[u8] = if d.encrypt_metadata { &[] } else { &[0xFF; 4] };
    if aes256 {
        let mut h = Sha256::new();
        h.update(seed);
        for r in &d.recipients {
            h.update(r);
        }
        h.update(tail);
        h.finalize().to_vec()
    } else {
        let mut h = Sha1::new();
        h.update(seed);
        for r in &d.recipients {
            h.update(r);
        }
        h.update(tail);
        let n = (d.length_bits.clamp(40, 128) / 8) as usize;
        let mut k = h.finalize().to_vec();
        k.truncate(n);
        k
    }
}

/// Authenticate with a recipient key (§7.6.5.3).
pub(crate) fn open(mut dict: EncryptDict, key: &dyn RecipientKey) -> Result<SecurityHandler, CryptError> {
    if dict.v < 1 || dict.v > 5 || dict.v == 3 {
        return Err(CryptError::Unsupported(format!("public-key security with /V {}", dict.v)));
    }
    if dict.recipients.is_empty() {
        return Err(CryptError::Malformed("the public-key security dictionary has no /Recipients".into()));
    }
    if dict.recipients.len() > MAX_RECIPIENTS {
        return Err(CryptError::Malformed(format!("more than {MAX_RECIPIENTS} /Recipients entries")));
    }
    let content =
        dict.recipients.iter().filter(|r| r.len() <= MAX_RECIPIENT_BYTES).find_map(|r| key.open_envelope(r)).ok_or(CryptError::NotRecipient)?;
    let (Some(seed), Some(perms)) = (content.get(..SEED_LEN), content.get(SEED_LEN..SEED_LEN + 4)) else {
        return Err(CryptError::Malformed("the recipient data is shorter than a seed and permissions (24 bytes)".into()));
    };
    let p = i32::from_be_bytes([perms[0], perms[1], perms[2], perms[3]]);
    let file_key = file_key(&dict, seed);
    dict.p = p;
    let auth = if p & 0b10 != 0 { Auth::Owner } else { Auth::User };
    Ok(SecurityHandler { dict, key: file_key, auth, recovered_user: None })
}

/// Parameters for new public-key encryption (`adbe.pkcs7.s5`).
#[derive(Clone)]
pub struct NewPublicKeyEncryption {
    /// [`Algorithm::Aes256`] (`/V 5`, `AESV3`) or [`Algorithm::Aes128`] (`/V 4`, `AESV2`). RC4
    /// is refused.
    pub algorithm: Algorithm,
    pub encrypt_metadata: bool,
    /// The seed every envelope carries (20 random bytes from the caller).
    pub seed: [u8; 20],
    /// The PKCS #7 envelopes, in `/Recipients` order, each holding `seed` and the permission
    /// bytes of the recipients it is addressed to ([`permission_bytes`]).
    pub recipients: Vec<Vec<u8>>,
}

impl std::fmt::Debug for NewPublicKeyEncryption {
    // The seed is key material: never print it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewPublicKeyEncryption")
            .field("algorithm", &self.algorithm)
            .field("encrypt_metadata", &self.encrypt_metadata)
            .field("recipients", &self.recipients.len())
            .finish()
    }
}

/// The 24-byte envelope content for `permissions` (Table 24 bits): seed, then the bits
/// big-endian with the reserved bits set as the standard handler sets them.
pub fn envelope_content(seed: &[u8; 20], permissions: i32) -> Vec<u8> {
    let mut out = seed.to_vec();
    out.extend_from_slice(&permission_bits(permissions).to_be_bytes());
    out
}

/// Bits 7–8 and 13–32 set, bit 1 clear (§7.6.5.2, Table 24).
fn permission_bits(permissions: i32) -> i32 {
    (permissions | !0xFFF | 0xC0) & !0x1
}

/// Build the `/Encrypt` values and the handler (owner-equivalent: whoever encrypts knows the
/// file key) for new public-key encryption.
pub fn create_public_key(params: &NewPublicKeyEncryption) -> Result<SecurityHandler, CryptError> {
    let (v, length_bits, method) = match params.algorithm {
        Algorithm::Aes256 => (5, 256, Method::Aes256),
        Algorithm::Aes128 => (4, 128, Method::Aes128),
        other => return Err(CryptError::Unsupported(format!("{other:?} for certificate security (use AES)"))),
    };
    if params.recipients.is_empty() {
        return Err(CryptError::Malformed("certificate security needs at least one recipient".into()));
    }
    if params.recipients.len() > MAX_RECIPIENTS || params.recipients.iter().any(|r| r.is_empty() || r.len() > MAX_RECIPIENT_BYTES) {
        return Err(CryptError::Malformed("too many or oversized recipient envelopes".into()));
    }
    let name = b"DefaultCryptFilter".to_vec();
    let dict = EncryptDict {
        filter: PUBLIC_KEY_FILTER.to_vec(),
        sub_filter: SUBFILTER_S5.to_vec(),
        recipients: params.recipients.clone(),
        v,
        r: 0,
        length_bits,
        p: -1,
        encrypt_metadata: params.encrypt_metadata,
        crypt_filters: vec![(name.clone(), method)],
        stm_f: name.clone(),
        str_f: name,
        ..Default::default()
    };
    let key = file_key(&dict, &params.seed);
    Ok(SecurityHandler { dict, key, auth: Auth::Owner, recovered_user: None })
}

/// The 4 permission bytes an envelope carries for `permissions`.
pub fn permission_bytes(permissions: i32) -> [u8; 4] {
    permission_bits(permissions).to_be_bytes()
}
