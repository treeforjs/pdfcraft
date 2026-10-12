use super::*;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn rc4_reference_vectors() {
    assert_eq!(rc4(b"Key", b"Plaintext"), hex("bbf316e8d940af0ad3"));
    assert_eq!(rc4(b"Wiki", b"pedia"), hex("1021bf0420"));
    assert_eq!(rc4(b"Secret", b"Attack at dawn"), hex("45a01f645fc35b383552544b9bf5"));
}

#[test]
fn aes_cbc_nist_sp800_38a_vectors() {
    let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
    let plain = hex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
    let k128 = hex("2b7e151628aed2a6abf7158809cf4f3c");
    let c128 = aes_cbc_encrypt(&k128, &iv, &plain, false);
    assert_eq!(c128, hex("7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2"));
    assert_eq!(aes_cbc_decrypt(&k128, &iv, &c128, false), plain);
    let k256 = hex("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
    let c256 = aes_cbc_encrypt(&k256, &iv, &plain, false);
    assert_eq!(c256, hex("f58c4c04d6e5f1ba779eabfb5f7bfbd69cfc4e967edb808d679f777bc6702c7d"));
    assert_eq!(aes_cbc_decrypt(&k256, &iv, &c256, false), plain);
}

#[test]
fn aes_padding_is_added_and_tolerantly_removed() {
    let key = [7u8; 16];
    let iv = [1u8; 16];
    for n in [0, 1, 15, 16, 17, 100] {
        let data: Vec<u8> = (0..n as u8).collect();
        let c = aes_cbc_encrypt(&key, &iv, &data, true);
        assert_eq!(c.len() % 16, 0);
        assert_eq!(aes_cbc_decrypt(&key, &iv, &c, true), data);
    }
    // Garbage (bad padding, partial block) decrypts without panicking.
    let _ = aes_cbc_decrypt(&key, &iv, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17], true);
}

const ID: &[u8] = b"0123456789abcdef";

fn new(alg: Algorithm, user: &str, owner: &str, perms: i32) -> SecurityHandler {
    create(
        &NewEncryption { algorithm: alg, user_password: user, owner_password: owner, permissions: perms, encrypt_metadata: true, seed: [42; 32] },
        ID,
    )
    .unwrap()
}

const ALL: [Algorithm; 4] = [Algorithm::Rc4_40, Algorithm::Rc4_128, Algorithm::Aes128, Algorithm::Aes256];

#[test]
fn created_encryption_authenticates_owner_and_user() {
    for alg in ALL {
        let made = new(alg, "user pw", "owner pw", 0b0100); // print only
        let d = made.dict().clone();
        let owner = SecurityHandler::open(d.clone(), ID, Some("owner pw")).unwrap();
        assert_eq!(owner.auth(), Auth::Owner, "{alg:?}");
        assert_eq!(owner.file_key(), made.file_key(), "{alg:?}");
        assert!(owner.permissions().modify(), "owners can do everything");
        let user = SecurityHandler::open(d.clone(), ID, Some("user pw")).unwrap();
        assert_eq!(user.auth(), Auth::User, "{alg:?}");
        assert_eq!(user.file_key(), made.file_key(), "{alg:?}");
        assert!(user.permissions().print() && !user.permissions().modify() && !user.permissions().copy(), "{alg:?}");
        assert_eq!(SecurityHandler::open(d.clone(), ID, Some("nope")).unwrap_err(), CryptError::WrongPassword, "{alg:?}");
        assert_eq!(SecurityHandler::open(d, ID, None).unwrap_err(), CryptError::WrongPassword, "{alg:?}");
    }
}

#[test]
fn empty_user_password_opens_without_asking() {
    for alg in ALL {
        let d = new(alg, "", "secret", -1).dict().clone();
        let h = SecurityHandler::open(d, ID, None).unwrap();
        assert_eq!(h.auth(), Auth::User, "{alg:?}");
    }
}

#[test]
fn empty_owner_password_uses_the_user_password() {
    for alg in ALL {
        let made = new(alg, "user secret", "", -1);
        let d = made.dict().clone();
        assert_eq!(SecurityHandler::open(d.clone(), ID, None).unwrap_err(), CryptError::WrongPassword, "{alg:?}");
        assert_eq!(SecurityHandler::open(d.clone(), ID, Some("")).unwrap_err(), CryptError::WrongPassword, "{alg:?}");
        let reopened = SecurityHandler::open(d, ID, Some("user secret")).unwrap();
        assert_eq!(reopened.auth(), Auth::Owner, "{alg:?}");
        assert_eq!(reopened.file_key(), made.file_key(), "{alg:?}");
    }
}

#[test]
fn r6_rejects_tampered_permissions() {
    let made = new(Algorithm::Aes256, "user", "owner", 0b0100);
    for password in ["user", "owner"] {
        let mut d = made.dict().clone();
        d.p |= 1 << 4; // Grant copying without knowing the owner password or file key.
        assert!(matches!(SecurityHandler::open(d, ID, Some(password)), Err(CryptError::Malformed(_))));
        let mut d = made.dict().clone();
        d.encrypt_metadata = !d.encrypt_metadata;
        assert!(matches!(SecurityHandler::open(d, ID, Some(password)), Err(CryptError::Malformed(_))));
        let mut d = made.dict().clone();
        d.perms[0] ^= 1;
        assert!(matches!(SecurityHandler::open(d, ID, Some(password)), Err(CryptError::Malformed(_))));
        for len in [0, 15, 17] {
            let mut d = made.dict().clone();
            d.perms.resize(len, 0);
            assert!(matches!(SecurityHandler::open(d, ID, Some(password)), Err(CryptError::Malformed(_))));
        }
    }
}

#[test]
fn r5_validates_permissions_with_both_passwords() {
    let made = new(Algorithm::Aes256, "user", "owner", 0b0100);
    let mut d = made.dict().clone();
    d.r = 5;
    // R5 uses SHA-256 directly for the credential hashes; the file key and /Perms are
    // encrypted the same way as R6. Keep the generated salts and wrap that same key.
    let mut u = hash_r6(5, b"user", &d.u[32..40], &[]);
    u.extend_from_slice(&d.u[32..48]);
    d.ue = aes_cbc_encrypt(&hash_r6(5, b"user", &u[40..48], &[]), &[0; 16], made.file_key(), false);
    d.u = u;
    let mut o = hash_r6(5, b"owner", &d.o[32..40], &d.u);
    o.extend_from_slice(&d.o[32..48]);
    d.oe = aes_cbc_encrypt(&hash_r6(5, b"owner", &o[40..48], &d.u), &[0; 16], made.file_key(), false);
    d.o = o;
    for (password, auth) in [("user", Auth::User), ("owner", Auth::Owner)] {
        let reopened = SecurityHandler::open(d.clone(), ID, Some(password)).unwrap();
        assert_eq!(reopened.auth(), auth);
        assert_eq!(reopened.file_key(), made.file_key());
        let mut tampered = d.clone();
        tampered.p |= 1 << 4;
        assert!(matches!(SecurityHandler::open(tampered, ID, Some(password)), Err(CryptError::Malformed(_))));
    }
}

#[test]
fn strings_and_streams_round_trip_and_are_scrambled() {
    for alg in ALL {
        let h = new(alg, "", "o", -1);
        let text = b"Confidential quarterly numbers".to_vec();
        let enc = h.encrypt_string(12, 0, &text);
        assert_ne!(enc, text, "{alg:?}");
        assert_eq!(h.decrypt_string(12, 0, &enc), text, "{alg:?}");
        if alg != Algorithm::Aes256 {
            // R2–R4 derive a key per object; R6 uses the file key everywhere (§7.6.3.3).
            assert_ne!(h.decrypt_string(13, 0, &enc), text, "object keys differ per object ({alg:?})");
        }
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
        let enc = h.encrypt_stream(5, 0, &data, StreamKind::Normal);
        assert_eq!(h.decrypt_stream(5, 0, &enc, StreamKind::Normal), data, "{alg:?}");
        // Deterministic: the same object encrypts to the same bytes (reproducible saves).
        assert_eq!(h.encrypt_stream(5, 0, &data, StreamKind::Normal), enc);
    }
}

#[test]
fn identity_crypt_filters_leave_data_alone() {
    let mut d = new(Algorithm::Aes256, "", "o", -1).dict().clone();
    // Embedded-file-only encryption: strings and normal streams are not encrypted.
    d.str_f = b"Identity".to_vec();
    d.stm_f = b"Identity".to_vec();
    d.ef_f = b"StdCF".to_vec();
    let h = SecurityHandler::open(d, ID, None).unwrap();
    assert!(!h.encrypts_strings());
    assert_eq!(h.decrypt_stream(3, 0, b"plain", StreamKind::Normal), b"plain");
    let enc = h.encrypt_stream(3, 0, b"attachment", StreamKind::EmbeddedFile);
    assert_ne!(enc, b"attachment");
    assert_eq!(h.decrypt_stream(3, 0, &enc, StreamKind::EmbeddedFile), b"attachment");
    assert_eq!(h.decrypt_stream(3, 0, b"x", StreamKind::Named(b"Identity")), b"x");
}

#[test]
fn r6_passwords_are_saslprepped() {
    // U+00AD (soft hyphen) is mapped to nothing by SASLprep; "ª" normalises to "a" (NFKC).
    let d = new(Algorithm::Aes256, "SªSL\u{AD}prep", "o", -1).dict().clone();
    assert!(SecurityHandler::open(d.clone(), ID, Some("SaSLprep")).is_ok());
    assert!(SecurityHandler::open(d, ID, Some("SaSL-prep")).is_err());
}

#[test]
fn unsupported_handlers_are_reported() {
    let mut d = new(Algorithm::Aes128, "", "o", -1).dict().clone();
    d.filter = b"Vendor.Sec".to_vec();
    assert!(matches!(SecurityHandler::open(d.clone(), ID, None), Err(CryptError::Unsupported(_))));
    // The public-key handler is supported, but a password can't open it.
    d.filter = b"Adobe.PubSec".to_vec();
    assert_eq!(SecurityHandler::open(d.clone(), ID, None).unwrap_err(), CryptError::NeedsCertificate);
    d.filter = b"Standard".to_vec();
    d.r = 7;
    assert!(matches!(SecurityHandler::open(d, ID, None), Err(CryptError::Unsupported(_))));
}

#[test]
fn permission_bits_follow_table_22() {
    let p = Permissions { bits: -3904, owner: false }; // 0xFFFFF0C0: only reserved bits set
    assert!(!p.print() && !p.modify() && !p.copy() && !p.annotate() && !p.assemble());
    let p = Permissions { bits: -1, owner: false };
    assert!(p.print() && p.modify() && p.copy() && p.annotate() && p.assemble() && p.fill_forms());
    let p = Permissions { bits: 0, owner: true };
    assert!(p.modify(), "owners ignore /P");
}

proptest::proptest! {
    #[test]
    fn malformed_dictionaries_never_panic(o in proptest::collection::vec(proptest::num::u8::ANY, 0..60),
                                          u in proptest::collection::vec(proptest::num::u8::ANY, 0..60),
                                          r in 0i64..9, len in -10i64..300) {
        let d = EncryptDict { filter: b"Standard".to_vec(), v: 4, r, length_bits: len, o, u, p: -1, encrypt_metadata: true, ..Default::default() };
        let _ = SecurityHandler::open(d, ID, Some("x"));
    }
}

#[test]
fn owner_authentication_recovers_the_user_password() {
    for alg in [Algorithm::Rc4_40, Algorithm::Rc4_128, Algorithm::Aes128] {
        let d = new(alg, "user pw", "owner pw", -1).dict().clone();
        let h = SecurityHandler::open(d.clone(), ID, Some("owner pw")).unwrap();
        assert_eq!(h.recovered_user_password().as_deref(), Some(&b"user pw"[..]), "{alg:?}");
        assert_eq!(SecurityHandler::open(d, ID, Some("user pw")).unwrap().recovered_user_password(), None);
    }
    let d = new(Algorithm::Rc4_128, "", "owner", -1).dict().clone();
    assert_eq!(SecurityHandler::open(d, ID, Some("owner")).unwrap().recovered_user_password().as_deref(), Some(&b""[..]));
}

// ── Public-key handler (§7.6.5) ───────────────────────────────────────────────────────────────

/// A stand-in for a recipient's private key: "envelopes" are `id || content`, opened when the
/// id matches. The CMS side is tested in pdfcraft-sign; here only the key derivation matters.
struct FakeKey(u8);

impl RecipientKey for FakeKey {
    fn open_envelope(&self, envelope: &[u8]) -> Option<Vec<u8>> {
        let (&id, content) = envelope.split_first()?;
        (id == self.0).then(|| content.to_vec())
    }
}

fn fake_envelope(id: u8, seed: &[u8; 20], permissions: i32) -> Vec<u8> {
    let mut e = vec![id];
    e.extend(envelope_content(seed, permissions));
    e
}

#[test]
fn public_key_file_keys_follow_the_spec_and_recipients_get_their_permissions() {
    use sha1::Sha1;
    let seed = [9u8; 20];
    let full = fake_envelope(1, &seed, 0b10 | 0xF3C);
    let print_only = fake_envelope(2, &seed, 0b100);
    for (algorithm, encrypt_metadata) in
        [(Algorithm::Aes256, true), (Algorithm::Aes256, false), (Algorithm::Aes128, true), (Algorithm::Aes128, false)]
    {
        let params = NewPublicKeyEncryption { algorithm, encrypt_metadata, seed, recipients: vec![full.clone(), print_only.clone()] };
        let created = create_public_key(&params).unwrap();
        assert!(created.is_public_key());
        assert_eq!(created.auth(), Auth::Owner, "whoever encrypts knows the key");
        // §7.6.5.3: digest of seed, every recipient entry, then FF FF FF FF without metadata encryption.
        let mut parts = seed.to_vec();
        parts.extend(&full);
        parts.extend(&print_only);
        if !encrypt_metadata {
            parts.extend([0xFF; 4]);
        }
        let expected = if algorithm == Algorithm::Aes256 { Sha256::digest(&parts).to_vec() } else { Sha1::digest(&parts)[..16].to_vec() };
        assert_eq!(created.file_key(), expected.as_slice(), "{algorithm:?} {encrypt_metadata}");
        // The full-access recipient is owner-equivalent; the other gets only its bits.
        let owner = SecurityHandler::open_with(created.dict().clone(), b"", Credential::Recipient(&FakeKey(1))).unwrap();
        assert_eq!((owner.auth(), owner.file_key()), (Auth::Owner, expected.as_slice()));
        assert!(owner.permissions().modify() && owner.permissions().copy());
        let user = SecurityHandler::open_with(created.dict().clone(), b"", Credential::Recipient(&FakeKey(2))).unwrap();
        assert_eq!((user.auth(), user.file_key()), (Auth::User, expected.as_slice()));
        let p = user.permissions();
        assert!(p.print() && !p.modify() && !p.copy() && !p.annotate() && !p.assemble() && !p.print_high_quality());
        // Data round-trips between the creator and a recipient.
        let sealed = created.encrypt_stream(7, 0, b"secret page", StreamKind::Normal);
        assert_ne!(sealed.as_slice(), b"secret page");
        assert_eq!(user.decrypt_stream(7, 0, &sealed, StreamKind::Normal), b"secret page");
    }
}

#[test]
fn public_key_documents_refuse_passwords_strangers_and_hostile_recipient_data() {
    let seed = [3u8; 20];
    let params =
        NewPublicKeyEncryption { algorithm: Algorithm::Aes256, encrypt_metadata: true, seed, recipients: vec![fake_envelope(1, &seed, 0b100)] };
    let dict = create_public_key(&params).unwrap().dict().clone();
    assert_eq!(SecurityHandler::open(dict.clone(), b"", Some("guess")).unwrap_err(), CryptError::NeedsCertificate);
    assert_eq!(SecurityHandler::open(dict.clone(), b"", None).unwrap_err(), CryptError::NeedsCertificate);
    assert_eq!(SecurityHandler::open_with(dict.clone(), b"", Credential::Recipient(&FakeKey(5))).unwrap_err(), CryptError::NotRecipient);
    // Too short to hold a seed and permissions.
    let short = EncryptDict { recipients: vec![vec![1, 2, 3]], ..dict.clone() };
    assert!(matches!(SecurityHandler::open_with(short, b"", Credential::Recipient(&FakeKey(1))), Err(CryptError::Malformed(_))));
    // No recipients, too many, an unknown /V: errors, never panics.
    let none = EncryptDict { recipients: vec![], ..dict.clone() };
    assert!(matches!(SecurityHandler::open_with(none, b"", Credential::Recipient(&FakeKey(1))), Err(CryptError::Malformed(_))));
    let many = EncryptDict { recipients: vec![vec![0]; MAX_RECIPIENTS + 1], ..dict.clone() };
    assert!(matches!(SecurityHandler::open_with(many, b"", Credential::Recipient(&FakeKey(1))), Err(CryptError::Malformed(_))));
    let odd = EncryptDict { v: 9, ..dict.clone() };
    assert!(matches!(SecurityHandler::open_with(odd, b"", Credential::Recipient(&FakeKey(1))), Err(CryptError::Unsupported(_))));
    // RC4 is never written.
    let rc4 = NewPublicKeyEncryption { algorithm: Algorithm::Rc4_128, ..params.clone() };
    assert!(create_public_key(&rc4).is_err());
    assert!(create_public_key(&NewPublicKeyEncryption { recipients: vec![], ..params }).is_err());
}

#[test]
fn a_known_handler_reopens_only_the_same_encryption() {
    let seed = [4u8; 20];
    let params =
        NewPublicKeyEncryption { algorithm: Algorithm::Aes128, encrypt_metadata: true, seed, recipients: vec![fake_envelope(1, &seed, 0b100)] };
    let created = create_public_key(&params).unwrap();
    let again = SecurityHandler::open_with(created.dict().clone(), b"", Credential::Handler(&created)).unwrap();
    assert_eq!((again.file_key(), again.auth()), (created.file_key(), Auth::Owner));
    let other = EncryptDict { recipients: vec![fake_envelope(2, &seed, 0b100)], ..created.dict().clone() };
    assert_eq!(SecurityHandler::open_with(other, b"", Credential::Handler(&created)).unwrap_err(), CryptError::NotRecipient);
    // A recipient key offered for a password document tries the empty password only.
    let pw = NewEncryption {
        algorithm: Algorithm::Aes256,
        user_password: "",
        owner_password: "boss",
        permissions: -1,
        encrypt_metadata: true,
        seed: [1; 32],
    };
    let standard = create(&pw, b"id").unwrap();
    let opened = SecurityHandler::open_with(standard.dict().clone(), b"id", Credential::Recipient(&FakeKey(1))).unwrap();
    assert_eq!(opened.auth(), Auth::User);
    let pw = NewEncryption { user_password: "open", ..pw };
    let locked = create(&pw, b"id").unwrap();
    assert_eq!(SecurityHandler::open_with(locked.dict().clone(), b"id", Credential::Recipient(&FakeKey(1))).unwrap_err(), CryptError::WrongPassword);
}

#[test]
fn permission_bytes_are_big_endian_with_reserved_bits() {
    assert_eq!(permission_bytes(0b100), [0xFF, 0xFF, 0xF0, 0xC4]);
    assert_eq!(permission_bytes(-1), [0xFF, 0xFF, 0xFF, 0xFE], "bit 1 is always clear");
}
