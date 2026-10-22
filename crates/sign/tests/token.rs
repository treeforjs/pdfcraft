//! Signing with smart cards and tokens (sign.pkcs11) through the software mock token. The real
//! PKCS #11 path is tested against SoftHSM in `pdfcraft-platform`.

use std::sync::Arc;

use pdfcraft_cos::Document;
use pdfcraft_sign::token::mock::{Hostile, MockModule, MockProvider, MockToken};
use pdfcraft_sign::token::{self, Login, MAX_LABEL_CHARS, MAX_SLOTS, Pin, TokenError, TokenProvider};
use pdfcraft_sign::{DigitalId, Modification, SignOptions, Status, TrustStore, pkcs12, signatures};

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn id(file: &str) -> DigitalId {
    pkcs12::open(&data(file), "test").unwrap()
}

/// One page, no fields.
fn fixture() -> Vec<u8> {
    let objs: Vec<&[u8]> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R >>",
        b"<< /Length 0 >>\nstream\n\nendstream",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn opts() -> SignOptions {
    SignOptions { page: 0, rect: Some([20.0, 100.0, 220.0, 150.0]), date: "D:20261002120000Z".into(), ..SignOptions::default() }
}

/// A card with the RSA and P-256 test IDs, and a certificate without a key.
fn card() -> MockToken {
    let rsa = id("rsa-aes.p12");
    let ec = id("ec-p256.p12");
    let lonely = id("ec-p384.p12");
    MockToken::new("My Card", "1234")
        .with_certificate(&[1], "Signing RSA", &rsa.certificate.raw)
        .with_key(&[1], rsa.key)
        .with_certificate(&[2], "Signing EC", &ec.certificate.raw)
        .with_key(&[2], ec.key)
        .with_certificate(&[3], "Encryption only", &lonely.certificate.raw)
}

fn module(hostile: Hostile) -> MockModule {
    MockModule::new().with_slot(0, None).with_slot(1, Some(card())).hostile(hostile)
}

fn unlock(m: &MockModule) -> Login {
    token::login(m, 1, Some(Pin::take(&mut "1234".to_string()))).unwrap()
}

fn sign_and_validate(login: &Login, which: &str) -> Vec<u8> {
    let digital_id = login.find(which).unwrap();
    assert!(digital_id.key.is_external());
    let signed = pdfcraft_sign::sign(&Document::open(Arc::new(fixture())).unwrap(), &digital_id, &opts()).unwrap();
    let doc = Document::open(Arc::new(signed.clone())).unwrap();
    let s = signatures(&doc, &signed, &TrustStore::new(vec![digital_id.certificate.clone()])).into_iter().find(|s| s.signed).unwrap();
    assert_eq!(s.status, Status::Valid, "{which}: {:?}", s.details);
    assert_eq!(s.modification, Modification::None);
    signed
}

#[test]
fn lists_slots_unlocks_and_signs_on_the_token() {
    let m = module(Hostile::default());
    let slots = token::slots(&m).unwrap();
    assert_eq!(slots.len(), 2);
    assert!(slots[0].token.is_none(), "an empty reader");
    let t = slots[1].token.as_ref().unwrap();
    assert_eq!(t.label, "My Card");
    assert!(t.login_required && !t.pin_locked);

    let login = unlock(&m);
    let found = login.identities().unwrap();
    let labels: Vec<&str> = found.ids.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["Signing RSA", "Signing EC"]);
    // The certificate without a private key is left out, and says why.
    assert_eq!(found.skipped.len(), 1);
    assert_eq!(found.skipped[0].label, "Encryption only");
    assert_eq!(found.skipped[0].reason, "no private key on the token");
    // RSA: the mock offers only on-token hashing (CKM_SHA256_RSA_PKCS); EC: CKM_ECDSA over a digest.
    for ident in &found.ids {
        assert!(ident.reference.starts_with("pkcs11:") && ident.reference.len() == 7 + 64);
        sign_and_validate(&login, &ident.reference);
    }
    // By common name or bare fingerprint, too.
    let cn = found.ids[0].certificate.subject.common_name().unwrap().to_string();
    sign_and_validate(&login, &cn);
    sign_and_validate(&login, found.ids[1].reference.trim_start_matches("pkcs11:"));
    assert_eq!(m.counts().1, 4, "every signature was made on the token");
    assert!(matches!(login.find("pkcs11:00"), Err(TokenError::NoIdentity(_))));
}

#[test]
fn wrong_pins_count_down_and_a_locked_token_is_not_tried_again() {
    let m = module(Hostile::default());
    let wrong = || token::login(&m, 1, Some(Pin::from_bytes(b"0000".to_vec())));
    assert_eq!(wrong().unwrap_err(), TokenError::WrongPin { final_try: false });
    assert_eq!(wrong().unwrap_err(), TokenError::WrongPin { final_try: true });
    assert!(wrong().unwrap_err().to_string().contains("locked"));
    assert_eq!(m.tries_left(1), Some(0));
    let attempts = m.counts().0;
    // Locked: refused before the PIN reaches the token, even the right one.
    assert_eq!(token::login(&m, 1, Some(Pin::from_bytes(b"1234".to_vec()))).unwrap_err(), TokenError::PinLocked);
    assert_eq!(m.counts().0, attempts);
}

#[test]
fn a_right_pin_resets_the_count_and_missing_pins_or_slots_are_reported() {
    let m = module(Hostile::default());
    assert!(token::login(&m, 1, Some(Pin::from_bytes(b"9".to_vec()))).is_err());
    assert_eq!(m.tries_left(1), Some(2));
    drop(unlock(&m));
    assert_eq!(m.tries_left(1), Some(3));
    assert_eq!(token::login(&m, 1, None).unwrap_err(), TokenError::PinRequired);
    assert_eq!(token::login(&m, 1, Some(Pin::from_bytes(Vec::new()))).unwrap_err(), TokenError::PinRequired);
    assert_eq!(token::login(&m, 0, Some(Pin::from_bytes(b"1".to_vec()))).unwrap_err(), TokenError::TokenNotPresent);
    assert_eq!(token::login(&m, 7, None).unwrap_err(), TokenError::NoSuchSlot(7));
    assert_eq!(token::login(&m, 1, Some(Pin::from_bytes(vec![b'1'; 10_000]))).unwrap_err(), TokenError::PinTooLong);
    // A PIN-pad reader: no PIN is typed in PdfCraft.
    let mut padded = card();
    padded.protected_path = true;
    let pad = MockModule::new().with_slot(4, Some(padded));
    assert!(token::login(&pad, 4, None).unwrap().identities().unwrap().ids.len() == 2);
}

#[test]
fn the_pin_is_cleared_from_the_field_and_never_printed() {
    let mut typed = "123456".to_string();
    let pin = Pin::take(&mut typed);
    assert!(typed.is_empty());
    assert_eq!(pin.expose(), b"123456");
    assert!(!format!("{pin:?}").contains("123456"));
    let e = token::login(&module(Hostile::default()), 1, Some(pin)).unwrap_err();
    assert!(!e.to_string().contains("123456"));
}

#[test]
fn missing_and_unavailable_modules() {
    let p = MockProvider::new();
    assert_eq!(token::load_module(&p, None).err(), Some(TokenError::NoModule));
    assert_eq!(token::load_module(&p, Some("/nope/libtoken.so")).err(), Some(TokenError::ModuleNotFound("/nope/libtoken.so".into())));
    assert_eq!(token::Unavailable.load("/x").err(), Some(TokenError::NotAvailable));
    let p = MockProvider::new().with_module("/opt/a.so", module(Hostile::default()));
    let (path, m) = token::load_module(&p, Some("  ")).unwrap();
    assert_eq!(path, "/opt/a.so");
    assert_eq!(m.info().unwrap().manufacturer, "PdfCraft");
}

#[test]
fn hostile_module_responses_are_contained() {
    // A flood of slots with control characters in their names.
    let m = module(Hostile { slot_flood: 100_000, ..Hostile::default() });
    let slots = token::slots(&m).unwrap();
    assert_eq!(slots.len(), MAX_SLOTS);
    assert!(slots.iter().all(|s| s.description.chars().count() <= MAX_LABEL_CHARS && !s.description.chars().any(char::is_control)));

    // Junk certificates and huge labels.
    let m = module(Hostile { junk_certificate: true, huge_label: true, ..Hostile::default() });
    let found = unlock(&m).identities().unwrap();
    assert_eq!(found.ids.len(), 2);
    assert!(found.ids.iter().all(|i| i.label.chars().count() <= MAX_LABEL_CHARS && !i.label.chars().any(char::is_control)));
    assert!(found.skipped.iter().any(|s| s.reason == "unreadable certificate"));

    // A module that can't list certificates.
    let m = module(Hostile { fail_certificates: true, ..Hostile::default() });
    assert!(matches!(unlock(&m).identities(), Err(TokenError::Device(_))));

    // Signatures that don't verify, are far too long, or a module that panics: no signed file.
    for hostile in [
        Hostile { garbage_signature: true, ..Hostile::default() },
        Hostile { oversized_signature: true, ..Hostile::default() },
        Hostile { panic_on_sign: true, ..Hostile::default() },
    ] {
        let m = module(hostile.clone());
        let login = unlock(&m);
        for ident in login.identities().unwrap().ids {
            let digital_id = login.digital_id(&ident);
            let err = pdfcraft_sign::sign(&Document::open(Arc::new(fixture())).unwrap(), &digital_id, &opts()).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("unusable") || msg.contains("failed"), "{hostile:?}: {msg}");
        }
    }
}

#[test]
fn chains_on_the_token_go_into_the_signature() {
    let chained = id("chain.p12");
    let ca = chained.chain.first().unwrap().clone();
    let cert = chained.certificate.clone();
    let token = MockToken::new("Chain", "1").with_certificate(b"k", "", &cert.raw).with_key(b"k", chained.key).with_certificate(b"ca", "CA", &ca.raw);
    let m = MockModule::new().with_slot(2, Some(token));
    let login = token::login(&m, 2, Some(Pin::from_bytes(b"1".to_vec()))).unwrap();
    let found = login.identities().unwrap();
    assert_eq!(found.ids.len(), 1, "the CA certificate is not offered as an ID");
    assert!(found.skipped.is_empty(), "nor reported as skipped");
    assert_eq!(found.ids[0].chain, vec![ca.clone()]);
    // An empty label falls back to the certificate's name.
    assert_eq!(found.ids[0].label, cert.display_name());
    let signed = pdfcraft_sign::sign(&Document::open(Arc::new(fixture())).unwrap(), &login.digital_id(&found.ids[0]), &opts()).unwrap();
    let doc = Document::open(Arc::new(signed.clone())).unwrap();
    let s = signatures(&doc, &signed, &TrustStore::new(vec![ca])).into_iter().find(|s| s.signed).unwrap();
    assert_eq!(s.status, Status::Valid, "{:?}", s.details);
}
