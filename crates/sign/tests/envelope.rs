//! CMS EnvelopedData for certificate security (ISO 32000-2 §7.6.5): sealing and opening with
//! throwaway identities generated at run time, cross-checked against `openssl cms` when it is
//! installed (an external oracle process only).

use pdfcraft_sign::der::Time;
use pdfcraft_sign::envelope;
use pdfcraft_sign::{Certificate, Name, PrivateKey};

fn identity(cn: &str, serial: &[u8]) -> (PrivateKey, Certificate) {
    let key = PrivateKey::generate_rsa(2048).unwrap();
    let name = Name::build(cn, "Testing", "PdfCraft test identities", "", "");
    let now = Time { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
    let cert = Certificate::self_signed(&name, &key, now, 2, serial).unwrap();
    (key, cert)
}

#[test]
fn sealed_envelopes_open_for_each_recipient_only() {
    let (ka, ca) = identity("Recipient A", &[0x01]);
    let (kb, cb) = identity("Recipient B", &[0x80, 0x02]); // a serial with its sign octet
    let (ks, cs) = identity("Stranger", &[0x03]);
    let content: Vec<u8> = (0..24u8).collect();
    let sealed = envelope::seal(&content, &[&ca, &cb]).unwrap();
    assert_eq!(envelope::open(&sealed, &ca, &ka).unwrap().as_deref(), Some(content.as_slice()));
    assert_eq!(envelope::open(&sealed, &cb, &kb).unwrap().as_deref(), Some(content.as_slice()));
    assert_eq!(envelope::open(&sealed, &cs, &ks).unwrap(), None, "not addressed to the stranger");
    // The right certificate with the wrong key is an error, not a match.
    assert!(envelope::open(&sealed, &ca, &kb).is_err());
    // Trailing padding (PDF strings are sometimes zero-padded) is ignored.
    let mut padded = sealed.clone();
    padded.extend([0u8; 7]);
    assert_eq!(envelope::open(&padded, &ca, &ka).unwrap().as_deref(), Some(content.as_slice()));
    // Non-RSA recipients are refused with a clear error.
    let ec = PrivateKey::generate_p256().unwrap();
    let ec_cert = Certificate::self_signed(
        &Name::build("EC", "", "", "", ""),
        &ec,
        Time { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 },
        1,
        &[9],
    )
    .unwrap();
    assert!(envelope::seal(&content, &[&ec_cert]).is_err());
    assert!(envelope::seal(&content, &[]).is_err());
}

#[test]
fn hostile_envelopes_never_panic() {
    let (ka, ca) = identity("Recipient A", &[0x01]);
    let sealed = envelope::seal(b"0123456789abcdefghij\xff\xff\xf0\xc4", &[&ca]).unwrap();
    // Every truncation, and single-byte corruptions throughout: errors or "not for us", never a panic.
    for n in 0..sealed.len() {
        let _ = envelope::open(&sealed[..n], &ca, &ka);
    }
    for i in 0..sealed.len() {
        for x in [0x01u8, 0x80, 0xFF] {
            let mut bad = sealed.clone();
            bad[i] ^= x;
            let _ = envelope::open(&bad, &ca, &ka);
        }
    }
    // Oversized input is refused before parsing.
    assert!(envelope::open(&vec![0x30; envelope::MAX_ENVELOPE + 1], &ca, &ka).is_err());
    for junk in [&b""[..], b"\x30", b"\x30\x80", b"\x30\x84\xff\xff\xff\xff", b"\xa0\x00", b"hello"] {
        assert!(envelope::open(junk, &ca, &ka).is_err());
    }
}

/// `openssl cms` as an independent implementation: it opens what we seal, and we open what it
/// seals (AES-256, 3DES, and OAEP key transport). Skipped when openssl is not installed.
#[test]
fn openssl_cms_agrees() {
    let Ok(v) = std::process::Command::new("openssl").arg("version").output() else {
        eprintln!("openssl not installed; skipping");
        return;
    };
    eprintln!("oracle: {}", String::from_utf8_lossy(&v.stdout).trim());
    let dir = std::env::temp_dir().join(format!("pdfcraft-envelope-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (key, cert) = identity("Oracle Recipient", &[0x2A]);
    let cert_pem = dir.join("cert.pem");
    let key_der = dir.join("key.der");
    std::fs::write(&cert_pem, pdfcraft_sign::x509::to_pem(&cert)).unwrap();
    std::fs::write(&key_der, key.pkcs8()).unwrap();
    let content = b"seed-seed-seed-seed!\xff\xff\xf0\xc4".to_vec();
    // We seal, openssl opens.
    let ours = dir.join("ours.der");
    std::fs::write(&ours, envelope::seal(&content, &[&cert]).unwrap()).unwrap();
    let out = std::process::Command::new("openssl")
        .args(["cms", "-decrypt", "-binary", "-inform", "DER", "-keyform", "DER"])
        .arg("-in")
        .arg(&ours)
        .arg("-recip")
        .arg(&cert_pem)
        .arg("-inkey")
        .arg(&key_der)
        .output()
        .unwrap();
    assert!(out.status.success(), "openssl cms -decrypt: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, content);
    // openssl seals, we open.
    let plain = dir.join("plain.bin");
    std::fs::write(&plain, &content).unwrap();
    // (cipher, key transport options; -keyopt applies to the -recip before it)
    for (cipher, keyopt) in [("-aes256", &[][..]), ("-des3", &[]), ("-aes128", &["-keyopt", "rsa_padding_mode:oaep"])] {
        let extra = (cipher, keyopt);
        let theirs = dir.join("theirs.der");
        let made = std::process::Command::new("openssl")
            .args(["cms", "-encrypt", "-binary", "-outform", "DER", cipher])
            .arg("-in")
            .arg(&plain)
            .arg("-out")
            .arg(&theirs)
            .arg("-recip")
            .arg(&cert_pem)
            .args(keyopt)
            .output()
            .unwrap();
        assert!(made.status.success(), "openssl cms -encrypt {extra:?}: {}", String::from_utf8_lossy(&made.stderr));
        let bytes = std::fs::read(&theirs).unwrap();
        assert_eq!(envelope::open(&bytes, &cert, &key).unwrap().as_deref(), Some(content.as_slice()), "{extra:?}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
