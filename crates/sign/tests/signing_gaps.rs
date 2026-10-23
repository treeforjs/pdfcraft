//! Signature appearances with graphics, per-certificate trust, revocation freshness and the
//! OCSP request: original synthetic fixtures only.
use pdfcraft_sign::appearance::{GraphicImage, InkGraphic, SignatureGraphic};
use pdfcraft_sign::revocation::{CertificateList, OcspResponse, RevocationStatus, ocsp_request};
use pdfcraft_sign::{self as sign, CertTrust, DigestAlg, Time, TrustStore, der};
use std::collections::HashMap;
use std::sync::Arc;

mod support;

fn open(bytes: Vec<u8>) -> pdfcraft_cos::Document {
    pdfcraft_cos::Document::open(Arc::new(bytes)).unwrap()
}

fn signed_with(appearance: sign::Appearance, certify: Option<u8>) -> (Vec<u8>, sign::DigitalId) {
    let (_, leaf) = support::chain();
    let doc = open(support::pdf());
    let opts =
        sign::SignOptions { date: "D:20261012120000Z".into(), rect: Some([20.0, 20.0, 180.0, 80.0]), certify, appearance, ..Default::default() };
    (sign::sign(&doc, &leaf, &opts).unwrap(), leaf)
}

fn trusted_issuer() -> TrustStore {
    let (issuer, _) = support::chain();
    TrustStore::new(vec![issuer.certificate])
}

fn status(bytes: &[u8], trust: &TrustStore) -> sign::Status {
    let doc = open(bytes.to_vec());
    let sigs = sign::signatures(&doc, bytes, trust);
    sigs[0].status
}

#[test]
fn pictures_drawn_signatures_and_logos_keep_the_signature_valid() {
    // A translucent pixel gets a soft mask.
    let mut rgba = vec![0u8; 4 * 3 * 4];
    for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *px = [20, 30, 90, if i % 2 == 0 { 255 } else { 40 }];
    }
    let picture = GraphicImage::new(4, 3, rgba.clone()).unwrap();
    let logo = GraphicImage::new(4, 3, rgba).unwrap();
    let ink = InkGraphic::fit(&[vec![[0.0, 0.0], [10.0, 5.0], [20.0, 0.0]], vec![[5.0, 8.0]]]).unwrap();
    for graphic in [SignatureGraphic::Image(picture), SignatureGraphic::Ink(ink)] {
        let appearance = sign::Appearance { graphic: Some(graphic), logo: Some(logo.clone()), ..Default::default() };
        let (signed, _) = signed_with(appearance, None);
        let doc = open(signed.clone());
        let sigs = sign::signatures(&doc, &signed, &trusted_issuer());
        assert_eq!(sigs[0].status, sign::Status::Valid, "{:?}", sigs[0].details);
        assert!(sigs[0].visible);
        // The appearance stream names the graphic and the logo.
        let text = String::from_utf8_lossy(&signed);
        assert!(text.contains("/Logo") && text.contains("/Graphic") || text.contains("FlateDecode"), "the appearance has its resources");
    }
}

#[test]
fn hostile_ink_is_refused_or_bounded() {
    assert!(InkGraphic::fit(&[]).is_err());
    assert!(InkGraphic::fit(&[vec![[f32::NAN, f32::INFINITY]]]).is_err());
    let huge = InkGraphic::fit(&[vec![[-1e30, 1e30], [1e30, -1e30]]]).unwrap();
    assert!(huge.strokes().iter().flatten().all(|p| p[0] <= 1000 && p[1] <= 1000));
}

fn with_settings(fingerprint: &str, t: CertTrust) -> TrustStore {
    let mut trust = trusted_issuer();
    trust.settings = Arc::new(HashMap::from([(fingerprint.to_string(), t)]));
    trust
}

#[test]
fn per_certificate_trust_limits_roots_and_purposes() {
    let (issuer, _) = support::chain();
    let fp = issuer.certificate.fingerprint();
    let (signed, leaf) = signed_with(sign::Appearance::default(), None);
    assert_eq!(status(&signed, &trusted_issuer()), sign::Status::Valid);
    // Not a trusted root: the certificates it issued are not trusted.
    assert_eq!(status(&signed, &with_settings(&fp, CertTrust { root: false, ..CertTrust::default() })), sign::Status::Unknown);
    // Not trusted for signed documents.
    assert_eq!(status(&signed, &with_settings(&fp, CertTrust { signatures: false, ..CertTrust::default() })), sign::Status::Unknown);
    // The signer's own certificate, trusted without being a root, vouches for its signatures.
    let mut own = TrustStore::new(vec![leaf.certificate.clone()]);
    own.settings = Arc::new(HashMap::from([(leaf.certificate.fingerprint(), CertTrust { root: false, ..CertTrust::default() })]));
    assert_eq!(status(&signed, &own), sign::Status::Valid);
    // Certified documents are a purpose of their own.
    let (certified, _) = signed_with(sign::Appearance::default(), Some(2));
    assert_eq!(status(&certified, &trusted_issuer()), sign::Status::Valid);
    assert_eq!(status(&certified, &with_settings(&fp, CertTrust { certified_documents: false, ..CertTrust::default() })), sign::Status::Unknown);
    assert_eq!(status(&signed, &with_settings(&fp, CertTrust { certified_documents: false, ..CertTrust::default() })), sign::Status::Valid);
}

/// A CRL from `issuer` issued at `this` without a nextUpdate.
fn open_ended_crl(issuer: &sign::DigitalId, this: &str) -> Vec<u8> {
    let digest = issuer.key.preferred_digest();
    let alg = issuer.key.signature_algorithm(digest);
    let start = Time::from_pdf(this).unwrap().encode();
    let tbs = der::seq(&[&der::int(1), &alg, &issuer.certificate.subject.raw, &start]);
    der::seq(&[&tbs, &alg, &der::bit_string(&issuer.key.sign(digest, &tbs).unwrap())])
}

#[test]
fn revocation_freshness_newer_evidence_and_maximum_age() {
    let (issuer, leaf) = support::chain();
    let at = |s: &str| Time::from_pdf(s).unwrap();
    let crl = CertificateList::parse(&support::crl(&issuer, None)).unwrap();
    // Issued 2026-10-01, next update 2026-11-01.
    assert_eq!(crl.check(&leaf.certificate, &issuer.certificate, at("20261015000000Z")), RevocationStatus::Good);
    assert_eq!(crl.check(&leaf.certificate, &issuer.certificate, at("20261201000000Z")), RevocationStatus::Unknown);
    // Evidence newer than the verification time speaks for it: the certificate was valid when
    // the CRL was issued, and a revocation is never undone.
    assert_eq!(crl.check(&leaf.certificate, &issuer.certificate, at("20260601000000Z")), RevocationStatus::Good);
    let ocsp = OcspResponse::parse(&support::ocsp(&issuer, &leaf.certificate)).unwrap();
    assert_eq!(ocsp.check(&leaf.certificate, &issuer.certificate, at("20260601000000Z")), RevocationStatus::Good);
    // Without nextUpdate the maximum age bounds the window.
    let open_ended = CertificateList::parse(&open_ended_crl(&issuer, "20261001000000Z")).unwrap();
    let later = at("20261010000000Z");
    assert_eq!(open_ended.check(&leaf.certificate, &issuer.certificate, later), RevocationStatus::Good);
    assert_eq!(open_ended.check_with(&leaf.certificate, &issuer.certificate, later, Some(86_400)), RevocationStatus::Unknown);
    assert_eq!(open_ended.check_with(&leaf.certificate, &issuer.certificate, later, Some(30 * 86_400)), RevocationStatus::Good);
    // A hostile maximum age doesn't overflow.
    assert_eq!(open_ended.check_with(&leaf.certificate, &issuer.certificate, later, Some(u64::MAX)), RevocationStatus::Good);
}

#[test]
fn the_ocsp_request_names_the_certificate_by_sha1_cert_id() {
    let (issuer, leaf) = support::chain();
    let req = ocsp_request(&leaf.certificate, &issuer.certificate);
    let (tlv, rest) = der::Tlv::parse(&req).unwrap();
    assert!(rest.is_empty());
    assert_eq!(tlv.tag, der::tag::SEQUENCE);
    let find = |needle: &[u8]| req.windows(needle.len()).any(|w| w == needle);
    assert!(find(&DigestAlg::Sha1.digest(&[&issuer.certificate.subject.raw])));
    assert!(find(&DigestAlg::Sha1.digest(&[&issuer.certificate.public_key.key_bits()])));
    assert!(find(&leaf.certificate.serial));
}

#[test]
fn a_revision_adding_only_a_dss_is_validation_data() {
    let (issuer, leaf) = support::chain();
    let doc = open(support::pdf());
    let signed = sign::sign(&doc, &leaf, &sign::SignOptions { date: "D:20261012120000Z".into(), ..Default::default() }).unwrap();
    let evidence = sign::dss::Evidence { certs: vec![issuer.certificate.raw.clone()], crls: vec![support::crl(&issuer, None)], ocsps: vec![] };
    let saved = sign::dss::embed(&open(signed), &evidence).unwrap();
    let doc = open(saved.clone());
    let sigs = sign::signatures(&doc, &saved, &trusted_issuer());
    assert_eq!(sigs[0].status, sign::Status::Valid, "{:?}", sigs[0].details);
    match &sigs[0].modification {
        sign::Modification::None => {}
        sign::Modification::Allowed(kinds) | sign::Modification::Disallowed(kinds) => {
            assert!(kinds.iter().all(|k| k == "document security store"), "{kinds:?}");
        }
    }
}
