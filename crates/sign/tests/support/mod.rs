//! Original synthetic certificates for signature workflow tests; no network or shipped assets.
#![allow(dead_code)]

use pdfcraft_sign::{Certificate, DigitalId, Name, PrivateKey, Time, der};

pub fn timestamp_id() -> DigitalId {
    let bytes = include_bytes!("../data/ec-p256.p12");
    let mut id = pdfcraft_sign::pkcs12::open(bytes, "test").unwrap();
    id.certificate = certificate(&id.key, true, &["1.3.6.1.5.5.7.3.8"]);
    id.chain.clear();
    id
}

pub fn certificate(key: &PrivateKey, critical: bool, usages: &[&str]) -> Certificate {
    let name = Name::build("Synthetic Timestamp Authority", "", "PdfCraft Tests", "", "");
    let alg = key.preferred_digest();
    let sig_alg = key.signature_algorithm(alg);
    let oid_bytes: Vec<_> = usages.iter().map(|oid| der::oid(oid)).collect();
    let oid_refs: Vec<_> = oid_bytes.iter().map(Vec::as_slice).collect();
    let usage = der::seq(&oid_refs);
    let extension = if critical {
        der::seq(&[&der::oid("2.5.29.37"), &der::boolean(true), &der::octets(&usage)])
    } else {
        der::seq(&[&der::oid("2.5.29.37"), &der::octets(&usage)])
    };
    let validity = der::seq(&[
        &Time { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 }.encode(),
        &Time { year: 2036, month: 1, day: 1, hour: 0, minute: 0, second: 0 }.encode(),
    ]);
    let tbs = der::seq(&[
        &der::explicit(0, &der::int(2)),
        &der::int(3161),
        &sig_alg,
        &name.raw,
        &validity,
        &name.raw,
        &key.public_key().spki(),
        &der::explicit(3, &der::seq(&[&extension])),
    ]);
    let signature = key.sign(alg, &tbs).unwrap();
    Certificate::parse(&der::seq(&[&tbs, &sig_alg, &der::bit_string(&signature)])).unwrap()
}

pub fn chain() -> (DigitalId, DigitalId) {
    let mut issuer = pdfcraft_sign::pkcs12::open(include_bytes!("../data/ec-p256.p12"), "test").unwrap();
    let mut leaf = pdfcraft_sign::pkcs12::open(include_bytes!("../data/ec-p384.p12"), "test").unwrap();
    let root_name = Name::build("Synthetic Evidence Root", "", "PdfCraft Tests", "", "");
    let leaf_name = Name::build("Synthetic Evidence Signer", "", "PdfCraft Tests", "", "");
    issuer.certificate = issued(&issuer.key, &root_name, &issuer.key, &root_name, 100, true);
    issuer.chain.clear();
    leaf.certificate = issued(&leaf.key, &leaf_name, &issuer.key, &root_name, 101, false);
    leaf.chain = vec![issuer.certificate.clone()];
    (issuer, leaf)
}

fn issued(key: &PrivateKey, name: &Name, issuer: &PrivateKey, issuer_name: &Name, serial: u64, ca: bool) -> Certificate {
    let digest = issuer.preferred_digest();
    let alg = issuer.signature_algorithm(digest);
    let constraints = if ca { der::seq(&[&der::boolean(true)]) } else { der::seq(&[]) };
    let constraints = der::seq(&[&der::oid("2.5.29.19"), &der::boolean(true), &der::octets(&constraints)]);
    let usage = der::seq(&[&der::oid("2.5.29.15"), &der::boolean(true), &der::octets(&der::bit_string(&[if ca { 0x06 } else { 0x80 }]))]);
    let validity = der::seq(&[&Time::from_pdf("20260101000000Z").unwrap().encode(), &Time::from_pdf("20360101000000Z").unwrap().encode()]);
    let tbs = der::seq(&[
        &der::explicit(0, &der::int(2)),
        &der::int(serial),
        &alg,
        &issuer_name.raw,
        &validity,
        &name.raw,
        &key.public_key().spki(),
        &der::explicit(3, &der::seq(&[&constraints, &usage])),
    ]);
    let signature = issuer.sign(digest, &tbs).unwrap();
    Certificate::parse(&der::seq(&[&tbs, &alg, &der::bit_string(&signature)])).unwrap()
}

pub fn crl(issuer: &DigitalId, revoked: Option<&Certificate>) -> Vec<u8> {
    let digest = issuer.key.preferred_digest();
    let alg = issuer.key.signature_algorithm(digest);
    let start = Time::from_pdf("20261001000000Z").unwrap().encode();
    let end = Time::from_pdf("20261101000000Z").unwrap().encode();
    let revocations = revoked.map(|c| der::seq(&[&der::seq(&[&der::uint(&c.serial), &start])])).unwrap_or_default();
    let tbs = der::seq(&[&der::int(1), &alg, &issuer.certificate.subject.raw, &start, &end, &revocations]);
    der::seq(&[&tbs, &alg, &der::bit_string(&issuer.key.sign(digest, &tbs).unwrap())])
}

pub fn ocsp(issuer: &DigitalId, leaf: &Certificate) -> Vec<u8> {
    ocsp_signed_by(issuer, leaf, &issuer.key, &issuer.certificate, ResponderIdForm::Name, false)
}

/// How a response names its signer (RFC 6960 §4.2.1 ResponderID).
#[derive(Clone, Copy)]
pub enum ResponderIdForm {
    Name,
    /// SHA-1 of the responder's subjectPublicKey BIT STRING contents.
    Key,
}

/// A good response for `leaf` (issued by `issuer`) signed by `key`, whose certificate
/// `responder` is named in the ResponderID and, when `embed`, carried in `certs [0]`.
pub fn ocsp_signed_by(
    issuer: &DigitalId,
    leaf: &Certificate,
    key: &PrivateKey,
    responder: &Certificate,
    form: ResponderIdForm,
    embed: bool,
) -> Vec<u8> {
    let digest = pdfcraft_sign::DigestAlg::Sha256;
    let id = der::seq(&[
        &digest.algorithm(),
        &der::octets(&digest.digest(&[&issuer.certificate.subject.raw])),
        &der::octets(&digest.digest(&[&issuer.certificate.public_key.key_bits()])),
        &der::uint(&leaf.serial),
    ]);
    let start = der::tlv(der::tag::GENERALIZED_TIME, b"20261001000000Z");
    let end = der::tlv(der::tag::GENERALIZED_TIME, b"20261101000000Z");
    let single = der::seq(&[&id, &der::tlv(der::tag::ctx_prim(0), &[]), &start, &der::explicit(0, &end)]);
    let responder_id = match form {
        ResponderIdForm::Name => der::explicit(1, &responder.subject.raw),
        ResponderIdForm::Key => der::explicit(2, &der::octets(&pdfcraft_sign::DigestAlg::Sha1.digest(&[&responder.public_key.key_bits()]))),
    };
    let data = der::seq(&[&responder_id, &start, &der::seq(&[&single])]);
    let alg = key.signature_algorithm(digest);
    let signature = der::bit_string(&key.sign(digest, &data).unwrap());
    let basic = if embed {
        // certs [0] EXPLICIT SEQUENCE OF Certificate
        der::seq(&[&data, &alg, &signature, &der::explicit(0, &der::seq(&[&responder.raw]))])
    } else {
        der::seq(&[&data, &alg, &signature])
    };
    let bytes = der::seq(&[&der::oid("1.3.6.1.5.5.7.48.1.1"), &der::octets(&basic)]);
    der::seq(&[&der::tlv(der::tag::ENUMERATED, &[0]), &der::explicit(0, &bytes)])
}

/// An original delegated-responder certificate: `key`'s certificate issued by `issuer_key`
/// under `issuer_name`, with the given extended key usages (none: no EKU extension).
pub fn responder_certificate(
    key: &PrivateKey,
    name: &Name,
    issuer_key: &PrivateKey,
    issuer_name: &Name,
    usages: &[&str],
    validity: (&str, &str),
) -> Certificate {
    end_entity(key, name, issuer_key, issuer_name, usages, false, 6960, validity)
}

/// An original timestamp-authority certificate issued by `issuer` (critical, exclusive
/// timeStamping EKU, as RFC 3161 §2.3 requires).
pub fn issued_timestamp_authority(key: &PrivateKey, issuer: &DigitalId) -> Certificate {
    let name = Name::build("Synthetic Issued Timestamp Authority", "", "PdfCraft Tests", "", "");
    end_entity(key, &name, &issuer.key, &issuer.certificate.subject, &["1.3.6.1.5.5.7.3.8"], true, 3162, ("20260101000000Z", "20360101000000Z"))
}

#[allow(clippy::too_many_arguments)]
fn end_entity(
    key: &PrivateKey,
    name: &Name,
    issuer_key: &PrivateKey,
    issuer_name: &Name,
    usages: &[&str],
    eku_critical: bool,
    serial: u64,
    validity: (&str, &str),
) -> Certificate {
    let digest = issuer_key.preferred_digest();
    let alg = issuer_key.signature_algorithm(digest);
    let usage = der::seq(&[&der::oid("2.5.29.15"), &der::boolean(true), &der::octets(&der::bit_string(&[0x80]))]);
    let mut extensions = vec![usage];
    if !usages.is_empty() {
        let oids: Vec<_> = usages.iter().map(|oid| der::oid(oid)).collect();
        let refs: Vec<_> = oids.iter().map(Vec::as_slice).collect();
        let value = der::octets(&der::seq(&refs));
        extensions.push(if eku_critical {
            der::seq(&[&der::oid("2.5.29.37"), &der::boolean(true), &value])
        } else {
            der::seq(&[&der::oid("2.5.29.37"), &value])
        });
    }
    let refs: Vec<_> = extensions.iter().map(Vec::as_slice).collect();
    let validity = der::seq(&[&Time::from_pdf(validity.0).unwrap().encode(), &Time::from_pdf(validity.1).unwrap().encode()]);
    let tbs = der::seq(&[
        &der::explicit(0, &der::int(2)),
        &der::int(serial),
        &alg,
        &issuer_name.raw,
        &validity,
        &name.raw,
        &key.public_key().spki(),
        &der::explicit(3, &der::seq(&refs)),
    ]);
    let signature = issuer_key.sign(digest, &tbs).unwrap();
    Certificate::parse(&der::seq(&[&tbs, &alg, &der::bit_string(&signature)])).unwrap()
}

/// A third, unrelated test key for delegated OCSP responders.
pub fn responder_key() -> PrivateKey {
    pdfcraft_sign::pkcs12::open(include_bytes!("../data/rsa-aes.p12"), "test").unwrap().key
}

/// A CA key unrelated to the evidence chain, for wrong-issuer cases.
pub fn other_ca_key() -> PrivateKey {
    pdfcraft_sign::pkcs12::open(include_bytes!("../data/chain.p12"), "test").unwrap().key
}

pub fn pdf() -> Vec<u8> {
    b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF".to_vec()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn local_timestamp_server() -> (String, Certificate, std::thread::JoinHandle<Vec<u8>>) {
    use std::io::{Read, Write};
    let tsa = timestamp_id();
    let certificate = tsa.certificate.clone();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/timestamp", socket.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = socket.accept().unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut b = [0];
            stream.read_exact(&mut b).unwrap();
            headers.push(b[0]);
            assert!(headers.len() < 16384);
        }
        let text = String::from_utf8(headers).unwrap();
        let size: usize = text
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
            })
            .unwrap();
        assert!(size < 4096);
        let mut request = vec![0; size];
        stream.read_exact(&mut request).unwrap();
        let query = pdfcraft_sign::timestamp::parse_request(&request).unwrap();
        assert!(query.nonce.is_some());
        let at = Time::from_pdf("20261012120000Z").unwrap();
        let response =
            pdfcraft_sign::timestamp::respond(&tsa.key, &tsa.certificate, &[], pdfcraft_sign::DigestAlg::Sha256, &query, "1.2.3.4", at, 8).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/timestamp-reply\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.len()
        )
        .unwrap();
        stream.write_all(&response).unwrap();
        request
    });
    (url, certificate, worker)
}
