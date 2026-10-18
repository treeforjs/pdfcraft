//! RFC 3161 acceptance uses original synthetic TSA certificates and local tokens.
use pdfcraft_sign::{DigestAlg, Time, TimestampQuery, timestamp};

mod support;

fn at() -> Time {
    Time { year: 2026, month: 10, day: 12, hour: 12, minute: 0, second: 0 }
}

#[test]
fn response_must_match_nonce_and_requested_policy() {
    let tsa = support::timestamp_id();
    let mut query = TimestampQuery::new(DigestAlg::Sha256, DigestAlg::Sha256.digest(&[b"original synthetic document"])).unwrap();
    query.nonce = Some(u64::MAX);
    query.policy = Some("1.2.3.4".into());
    let reply = |q: &TimestampQuery, policy| timestamp::respond(&tsa.key, &tsa.certificate, &[], DigestAlg::Sha256, q, policy, at(), 1).unwrap();
    let valid = reply(&query, "1.2.3.4");
    assert_eq!(timestamp::parse_response(&valid, &query).unwrap().gen_time, at());
    let mut changed = query.clone();
    changed.nonce = Some(7);
    assert!(timestamp::parse_response(&reply(&changed, "1.2.3.4"), &query).unwrap_err().to_string().contains("nonce"));
    changed.nonce = None;
    assert!(timestamp::parse_response(&reply(&changed, "1.2.3.4"), &query).unwrap_err().to_string().contains("nonce"));
    assert!(timestamp::parse_response(&reply(&query, "1.2.3.5"), &query).unwrap_err().to_string().contains("policy"));
}

#[test]
fn tsa_certificate_requires_critical_exclusive_timestamp_usage() {
    let tsa = support::timestamp_id();
    let query = TimestampQuery::new(DigestAlg::Sha256, DigestAlg::Sha256.digest(&[b"synthetic"])).unwrap();
    for (critical, usage) in [
        (false, vec!["1.3.6.1.5.5.7.3.8"]),
        (true, vec!["1.3.6.1.5.5.7.3.8", "1.3.6.1.5.5.7.3.4"]),
        (true, vec!["1.3.6.1.5.5.7.3.4"]),
        (true, vec![]),
    ] {
        let cert = support::certificate(&tsa.key, critical, &usage);
        let response = timestamp::respond(&tsa.key, &cert, &[], DigestAlg::Sha256, &query, "1.2.3.4", at(), 1).unwrap();
        assert!(timestamp::parse_response(&response, &query).unwrap_err().to_string().contains("timeStamping"));
    }
}

#[test]
fn openssl_independently_verifies_the_original_rfc3161_reply() {
    use base64::Engine;
    let tsa = support::timestamp_id();
    let query = TimestampQuery::online(DigestAlg::Sha256, DigestAlg::Sha256.digest(&[b"Original PdfCraft OpenSSL oracle fixture"])).unwrap();
    let response = timestamp::respond(&tsa.key, &tsa.certificate, &[], DigestAlg::Sha256, &query, "1.2.3.4", at(), 13).unwrap();
    let dir = std::env::temp_dir().join(format!("pdfcraft-rfc3161-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("request.tsq"), query.encode().unwrap()).unwrap();
    std::fs::write(dir.join("response.tsr"), response).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&tsa.certificate.raw);
    let pem = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        encoded.as_bytes().chunks(64).map(|b| std::str::from_utf8(b).unwrap()).collect::<Vec<_>>().join("\n")
    );
    std::fs::write(dir.join("tsa.pem"), pem).unwrap();
    let result = std::process::Command::new("openssl")
        .args(["ts", "-verify", "-queryfile", "request.tsq", "-in", "response.tsr", "-CAfile", "tsa.pem", "-attime", &at().unix().to_string()])
        .current_dir(&dir)
        .output()
        .expect("OpenSSL is required for this interoperability gate");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(result.status.success(), "{} {}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
}

struct LocalTsa(pdfcraft_sign::DigitalId);

impl pdfcraft_sign::TimestampAuthority for LocalTsa {
    fn timestamp(&self, request: &[u8]) -> Result<Vec<u8>, pdfcraft_sign::SignError> {
        let query = timestamp::parse_request(request)?;
        timestamp::respond(&self.0.key, &self.0.certificate, &[], DigestAlg::Sha256, &query, "1.2.3.4", at(), 17)
    }
}

/// ISO 32000-2 §12.8.5: other validators find a document timestamp only through a signature
/// field. Before, the /DocTimeStamp dictionary was written unreferenced.
#[test]
fn a_document_timestamp_is_the_value_of_an_invisible_signature_field() {
    use std::sync::Arc;
    let tsa = LocalTsa(support::timestamp_id());
    let open = |bytes: &[u8]| pdfcraft_cos::Document::open(Arc::new(bytes.to_vec())).unwrap();
    let once = pdfcraft_sign::timestamp_document(&open(&support::pdf()), &tsa, "D:20261012120000Z").unwrap();
    let twice = pdfcraft_sign::timestamp_document(&open(&once), &tsa, "D:20261012120000Z").unwrap();
    assert!(twice.starts_with(&once));
    let doc = open(&twice);
    let trust = pdfcraft_sign::TrustStore::new(vec![tsa.0.certificate.clone()]);
    let listed = pdfcraft_sign::signatures(&doc, &twice, &trust);
    let names: Vec<_> = listed.iter().map(|s| (s.field.as_str(), s.doc_timestamp, s.visible, s.status)).collect();
    assert_eq!(
        names,
        [("DocumentTimestamp1", true, false, pdfcraft_sign::Status::Valid), ("DocumentTimestamp2", true, false, pdfcraft_sign::Status::Valid)],
        "{:?}",
        listed.iter().map(|s| &s.details).collect::<Vec<_>>()
    );
    // The field is reachable from the catalog's AcroForm, with signature flags set.
    let root = doc.root().unwrap();
    let catalog = doc.get(root).as_dict().cloned().unwrap();
    let form = doc.resolve(catalog.get(b"AcroForm").unwrap()).as_dict().cloned().unwrap();
    assert_eq!(form.get(b"SigFlags"), Some(&pdfcraft_cos::Object::Int(3)));
    let fields = doc.resolve(form.get(b"Fields").unwrap()).as_array().cloned().unwrap();
    assert_eq!(fields.len(), 2);
    for field in fields {
        let field = doc.resolve(&field).as_dict().cloned().unwrap();
        let value = doc.resolve(field.get(b"V").unwrap()).as_dict().cloned().unwrap();
        assert_eq!(value.name(b"Type"), Some(&b"DocTimeStamp"[..]));
    }
}
