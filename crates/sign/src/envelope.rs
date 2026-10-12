//! CMS `EnvelopedData` (RFC 5652 §6) for certificate security (ISO 32000-2 §7.6.5): the
//! PKCS #7 envelopes in a public-key encryption dictionary's `/Recipients`.
//!
//! - [`open`] finds the `KeyTransRecipientInfo` addressed to a certificate (by issuer and
//!   serial number, or subject key identifier), decrypts the content-encryption key with the
//!   recipient's RSA private key (PKCS #1 v1.5, or OAEP with SHA-1) and the content with
//!   AES-128/192/256-CBC, 3DES-CBC or RC2-CBC (Acrobat's older 40- and 128-bit envelopes).
//! - [`seal`] writes an envelope for RSA recipients with AES-256-CBC content encryption and
//!   PKCS #1 v1.5 key transport, as Acrobat does.
//!
//! Envelopes are untrusted input: they are read as BER with size bounds and never panic.
//! RSA private-key operations use aws-lc-rs (ADR-0009), so they are unavailable on wasm.

use std::borrow::Cow;

use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeDecrypt, BlockModeEncrypt, InnerIvInit, KeyIvInit};

use crate::SignError;
use crate::der::{self, Tlv, tag};
use crate::keys::{PrivateKey, PublicKey};
use crate::x509::Certificate;

/// `id-envelopedData`.
pub const ENVELOPED_DATA: &str = "1.2.840.113549.1.7.3";
const DATA: &str = "1.2.840.113549.1.7.1";
const RSA_ENCRYPTION: &str = "1.2.840.113549.1.1.1";
const RSAES_OAEP: &str = "1.2.840.113549.1.1.7";
const AES128_CBC: &str = "2.16.840.1.101.3.4.1.2";
const AES192_CBC: &str = "2.16.840.1.101.3.4.1.22";
const AES256_CBC: &str = "2.16.840.1.101.3.4.1.42";
const DES_EDE3_CBC: &str = "1.2.840.113549.3.7";
const RC2_CBC: &str = "1.2.840.113549.3.2";

/// The largest envelope read.
pub const MAX_ENVELOPE: usize = 4 << 20;
/// The most recipient infos looked at in one envelope.
pub const MAX_RECIPIENT_INFOS: usize = 4096;
/// The most encrypted content decrypted (certificate security carries 24 bytes).
const MAX_CONTENT: usize = 1 << 20;
/// The most recipients one [`seal`] writes.
pub const MAX_SEAL_RECIPIENTS: usize = 1024;

fn bad(what: &str) -> SignError {
    SignError::Malformed(format!("CMS EnvelopedData: {what}"))
}

/// The children of `t`, or an error naming `what`.
fn parts<'a>(t: &Tlv<'a>, what: &str) -> Result<Vec<Tlv<'a>>, SignError> {
    t.children().map_err(|_| bad(what))
}

/// Whether the recipient identifier `rid` names `cert`.
fn names(rid: &Tlv<'_>, cert: &Certificate) -> bool {
    match rid.tag {
        // IssuerAndSerialNumber
        tag::SEQUENCE => match rid.children().ok().as_deref() {
            Some([issuer, serial]) => {
                issuer.raw == cert.issuer.raw.as_slice() && serial.tag == tag::INTEGER && serial.uint_bytes() == strip(&cert.serial)
            }
            _ => false,
        },
        // [0] IMPLICIT SubjectKeyIdentifier
        t if t == tag::ctx_prim(0) => cert.subject_key_id.as_deref() == Some(rid.value),
        _ => false,
    }
}

/// An INTEGER's magnitude without its sign octet (certificate serials are stored as written).
fn strip(v: &[u8]) -> &[u8] {
    match v {
        [0, rest @ ..] if !rest.is_empty() => rest,
        v => v,
    }
}

/// The content of `envelope` if it has a recipient info for `cert`, decrypted with `key`.
/// `Ok(None)` when the envelope is not addressed to `cert`; an error when it is malformed or
/// does not decrypt.
pub fn open(envelope: &[u8], cert: &Certificate, key: &PrivateKey) -> Result<Option<Vec<u8>>, SignError> {
    if envelope.len() > MAX_ENVELOPE {
        return Err(bad("too large"));
    }
    // PDF strings are sometimes padded after the DER: read one element and ignore the rest.
    let (info, _) = Tlv::parse_ber(envelope).map_err(|_| bad("not BER"))?;
    let info = parts(&info.expect(tag::SEQUENCE, "ContentInfo")?, "ContentInfo")?;
    let [content_type, content, ..] = info.as_slice() else { return Err(bad("ContentInfo")) };
    if content_type.oid()? != ENVELOPED_DATA {
        return Err(bad("not an EnvelopedData"));
    }
    if content.tag != tag::ctx(0) {
        return Err(bad("content"));
    }
    let enveloped = content.inner().map_err(|_| bad("content"))?;
    let fields = parts(&enveloped.expect(tag::SEQUENCE, "EnvelopedData")?, "EnvelopedData")?;
    // version, [0] originatorInfo (optional), recipientInfos, encryptedContentInfo, [1] attributes
    let mut rest = fields.iter().skip(1).filter(|f| f.tag != tag::ctx(0) && f.tag != tag::ctx(1));
    let (Some(recipients), Some(encrypted)) = (rest.next(), rest.next()) else { return Err(bad("EnvelopedData fields")) };
    let recipients = parts(&recipients.expect(tag::SET, "recipientInfos")?, "recipientInfos")?;
    if recipients.len() > MAX_RECIPIENT_INFOS {
        return Err(bad("too many recipient infos"));
    }
    // KeyTransRecipientInfo ::= SEQUENCE { version, rid, keyEncryptionAlgorithm, encryptedKey }.
    // Other kinds of recipient info are tagged [1]..[4] and skipped.
    let ours = recipients.iter().filter(|r| r.tag == tag::SEQUENCE).find_map(|r| {
        let f = r.children().ok()?;
        match f.as_slice() {
            [_, rid, alg, ek] if names(rid, cert) => Some((*alg, *ek)),
            _ => None,
        }
    });
    let Some((alg, encrypted_key)) = ours else { return Ok(None) };
    let encrypted_key = encrypted_key.octets("encryptedKey").map_err(|_| bad("encryptedKey"))?;
    let cek = unwrap_key(&alg, &encrypted_key, key)?;
    let content = decrypt_content(encrypted, &cek)?;
    Ok(Some(content))
}

/// Decrypt the content-encryption key (key transport with RSA).
fn unwrap_key(alg: &Tlv<'_>, encrypted_key: &[u8], key: &PrivateKey) -> Result<Vec<u8>, SignError> {
    let alg = parts(alg, "keyEncryptionAlgorithm")?;
    let oid = alg.first().ok_or_else(|| bad("keyEncryptionAlgorithm"))?.oid()?;
    let oaep = match oid.as_str() {
        RSA_ENCRYPTION => false,
        // Only the default parameters (SHA-1, MGF1 with SHA-1).
        RSAES_OAEP if alg.get(1).is_none_or(|p| p.tag == tag::NULL || p.value.is_empty()) => true,
        other => return Err(SignError::Unsupported(format!("key transport algorithm {other}"))),
    };
    if !matches!(key.public_key(), PublicKey::Rsa { .. }) {
        return Err(SignError::Unsupported("certificate security with a non-RSA key".into()));
    }
    rsa_decrypt(key, encrypted_key, oaep)
}

#[cfg(not(target_arch = "wasm32"))]
fn rsa_decrypt(key: &PrivateKey, ciphertext: &[u8], oaep: bool) -> Result<Vec<u8>, SignError> {
    use aws_lc_rs::rsa::{OAEP_SHA1_MGF1SHA1, OaepPrivateDecryptingKey, Pkcs1PrivateDecryptingKey, PrivateDecryptingKey};
    if key.is_external() || key.pkcs8().is_empty() {
        return Err(SignError::Unsupported("decrypting with a key held outside PdfCraft (use a .p12/.pfx digital ID)".into()));
    }
    let private = PrivateDecryptingKey::from_pkcs8(key.pkcs8()).map_err(|_| SignError::Crypto("the RSA private key can't decrypt".into()))?;
    let failed = || SignError::Crypto("the envelope's key does not decrypt with this digital ID".into());
    if ciphertext.len() != private.key_size_bytes() {
        return Err(failed());
    }
    if oaep {
        let k = OaepPrivateDecryptingKey::new(private).map_err(|_| failed())?;
        let mut out = vec![0u8; k.min_output_size()];
        let n = k.decrypt(&OAEP_SHA1_MGF1SHA1, ciphertext, &mut out, None).map_err(|_| failed())?.len();
        out.truncate(n);
        Ok(out)
    } else {
        let k = Pkcs1PrivateDecryptingKey::new(private).map_err(|_| failed())?;
        let mut out = vec![0u8; k.min_output_size()];
        let n = k.decrypt(ciphertext, &mut out).map_err(|_| failed())?.len();
        out.truncate(n);
        Ok(out)
    }
}

#[cfg(target_arch = "wasm32")]
fn rsa_decrypt(_key: &PrivateKey, _ciphertext: &[u8], _oaep: bool) -> Result<Vec<u8>, SignError> {
    Err(SignError::Unsupported("opening certificate-encrypted documents in the browser".into()))
}

/// `EncryptedContentInfo ::= SEQUENCE { contentType, contentEncryptionAlgorithm,
/// encryptedContent [0] IMPLICIT OCTET STRING }`, decrypted.
fn decrypt_content(info: &Tlv<'_>, cek: &[u8]) -> Result<Vec<u8>, SignError> {
    let fields = parts(&info.expect(tag::SEQUENCE, "EncryptedContentInfo")?, "EncryptedContentInfo")?;
    let [_, alg, ct, ..] = fields.as_slice() else { return Err(bad("no encrypted content")) };
    let ct: Cow<'_, [u8]> = match ct.tag {
        t if t == tag::ctx_prim(0) => Cow::Borrowed(ct.value),
        // BER: the constructed form, OCTET STRING segments.
        t if t == tag::ctx(0) => {
            let mut out = Vec::new();
            for s in parts(ct, "encryptedContent")? {
                out.extend_from_slice(&s.octets("encryptedContent").map_err(|_| bad("encryptedContent"))?);
                if out.len() > MAX_CONTENT {
                    return Err(bad("content too large"));
                }
            }
            Cow::Owned(out)
        }
        _ => return Err(bad("encryptedContent")),
    };
    if ct.len() > MAX_CONTENT {
        return Err(bad("content too large"));
    }
    let alg = parts(alg, "contentEncryptionAlgorithm")?;
    let oid = alg.first().ok_or_else(|| bad("contentEncryptionAlgorithm"))?.oid()?;
    let params = alg.get(1);
    let iv =
        |what: &str| -> Result<Vec<u8>, SignError> { params.ok_or_else(|| bad(what))?.octets(what).map(|c| c.into_owned()).map_err(|_| bad(what)) };
    let wrong = || SignError::Crypto("the envelope content does not decrypt (wrong key or damaged data)".into());
    let plain = match oid.as_str() {
        AES128_CBC => cbc::Decryptor::<aes::Aes128>::new_from_slices(cek, &iv("AES IV")?).map_err(|_| wrong())?.decrypt_padded_vec::<Pkcs7>(&ct),
        AES192_CBC => cbc::Decryptor::<aes::Aes192>::new_from_slices(cek, &iv("AES IV")?).map_err(|_| wrong())?.decrypt_padded_vec::<Pkcs7>(&ct),
        AES256_CBC => cbc::Decryptor::<aes::Aes256>::new_from_slices(cek, &iv("AES IV")?).map_err(|_| wrong())?.decrypt_padded_vec::<Pkcs7>(&ct),
        DES_EDE3_CBC => cbc::Decryptor::<des::TdesEde3>::new_from_slices(cek, &iv("3DES IV")?).map_err(|_| wrong())?.decrypt_padded_vec::<Pkcs7>(&ct),
        RC2_CBC => {
            // RC2CBCParameter ::= SEQUENCE { rc2ParameterVersion INTEGER OPTIONAL, iv } or a bare IV.
            let p = params.ok_or_else(|| bad("RC2 parameters"))?;
            let (version, iv) = if p.tag == tag::SEQUENCE {
                match parts(p, "RC2 parameters")?.as_slice() {
                    [v, iv] => (Some(v.u64().map_err(|_| bad("RC2 version"))?), iv.octets("RC2 IV").map_err(|_| bad("RC2 IV"))?.into_owned()),
                    [iv] => (None, iv.octets("RC2 IV").map_err(|_| bad("RC2 IV"))?.into_owned()),
                    _ => return Err(bad("RC2 parameters")),
                }
            } else {
                (None, p.octets("RC2 IV").map_err(|_| bad("RC2 IV"))?.into_owned())
            };
            // RFC 8018 B.2.3: the version encodes the effective key bits.
            let bits = match version {
                Some(160) => 40,
                Some(120) => 64,
                Some(58) => 128,
                Some(v) if (256..=1024).contains(&v) => v as usize,
                Some(_) => return Err(bad("RC2 version")),
                None => 32,
            };
            if cek.is_empty() || cek.len() > 128 {
                return Err(wrong());
            }
            let c = rc2::Rc2::new_with_eff_key_len(cek, bits);
            cbc::Decryptor::<rc2::Rc2>::inner_iv_slice_init(c, &iv).map_err(|_| wrong())?.decrypt_padded_vec::<Pkcs7>(&ct)
        }
        other => return Err(SignError::Unsupported(format!("content encryption algorithm {other}"))),
    };
    plain.map_err(|_| wrong())
}

/// `n` random bytes from the operating system.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], SignError> {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).map_err(|e| SignError::Crypto(format!("no randomness: {e}")))?;
    Ok(out)
}

/// An envelope of `content` for `recipients` (RSA certificates): AES-256-CBC content
/// encryption, PKCS #1 v1.5 key transport, recipients named by issuer and serial number.
pub fn seal(content: &[u8], recipients: &[&Certificate]) -> Result<Vec<u8>, SignError> {
    if recipients.is_empty() {
        return Err(SignError::Unsupported("an envelope needs at least one recipient".into()));
    }
    if recipients.len() > MAX_SEAL_RECIPIENTS || content.len() > MAX_CONTENT {
        return Err(SignError::Unsupported("too many recipients or too much content for one envelope".into()));
    }
    let cek: [u8; 32] = random_bytes()?;
    let iv: [u8; 16] = random_bytes()?;
    let mut infos = Vec::with_capacity(recipients.len());
    for cert in recipients {
        let encrypted_key = rsa_encrypt(cert, &cek)?;
        let rid = der::seq(&[&cert.issuer.raw, &der::tlv(tag::INTEGER, &cert.serial)]);
        infos.push(der::seq(&[&der::int(0), &rid, &der::algorithm(RSA_ENCRYPTION, Some(&der::null())), &der::octets(&encrypted_key)]));
    }
    let ct = cbc::Encryptor::<aes::Aes256>::new_from_slices(&cek, &iv)
        .map_err(|_| SignError::Crypto("AES key setup failed".into()))?
        .encrypt_padded_vec::<Pkcs7>(content);
    let infos: Vec<&[u8]> = infos.iter().map(Vec::as_slice).collect();
    let encrypted = der::seq(&[&der::oid(DATA), &der::algorithm(AES256_CBC, Some(&der::octets(&iv))), &der::tlv(tag::ctx_prim(0), &ct)]);
    let enveloped = der::seq(&[&der::int(0), &der::set_of(&infos), &encrypted]);
    Ok(der::seq(&[&der::oid(ENVELOPED_DATA), &der::explicit(0, &enveloped)]))
}

#[cfg(not(target_arch = "wasm32"))]
fn rsa_encrypt(cert: &Certificate, cek: &[u8]) -> Result<Vec<u8>, SignError> {
    use aws_lc_rs::rsa::{Pkcs1PublicEncryptingKey, PublicEncryptingKey};
    let who = cert.display_name();
    if !matches!(cert.public_key, PublicKey::Rsa { .. }) {
        return Err(SignError::Unsupported(format!("{who}: only RSA certificates can be recipients ({})", cert.public_key.describe())));
    }
    let public =
        PublicEncryptingKey::from_der(&cert.public_key.spki()).map_err(|_| SignError::Crypto(format!("{who}: the RSA public key is not usable")))?;
    let k = Pkcs1PublicEncryptingKey::new(public).map_err(|_| SignError::Crypto(format!("{who}: the RSA public key is not usable")))?;
    let mut out = vec![0u8; k.ciphertext_size()];
    let n = k.encrypt(cek, &mut out).map_err(|_| SignError::Crypto(format!("{who}: RSA encryption failed")))?.len();
    out.truncate(n);
    Ok(out)
}

#[cfg(target_arch = "wasm32")]
fn rsa_encrypt(_cert: &Certificate, _cek: &[u8]) -> Result<Vec<u8>, SignError> {
    Err(SignError::Unsupported("certificate encryption in the browser".into()))
}
