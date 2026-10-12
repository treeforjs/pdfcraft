//! Original RFC 5280/6960 fixtures, created from test-only keys at runtime.
use pdfcraft_sign::{self as sign, Time, TrustStore, VerificationTime, revocation::RevocationStatus};
use std::sync::Arc;

mod support;

#[test]
fn verified_local_evidence_preserves_signature_bytes_and_never_grants_trust() {
    let (issuer, leaf) = support::chain();
    let doc = pdfcraft_cos::Document::open(Arc::new(support::pdf())).unwrap();
    let signed = sign::sign(&doc, &leaf, &sign::SignOptions { date: "D:20261012120000Z".into(), ..Default::default() }).unwrap();
    let doc = pdfcraft_cos::Document::open(Arc::new(signed.clone())).unwrap();
    let evidence = sign::dss::Evidence {
        certs: vec![issuer.certificate.raw.clone()],
        crls: vec![support::crl(&issuer, None)],
        ocsps: vec![support::ocsp(&issuer, &leaf.certificate)],
    };
    let trust = TrustStore { certs: vec![issuer.certificate.clone()] };
    let report = sign::evidence::check(&doc, &signed, &evidence, &trust, VerificationTime::BestAvailable).unwrap();
    assert!(report.has_complete_coverage());
    assert_eq!(report.coverage.len(), 1);
    assert_eq!(report.coverage[0].status, RevocationStatus::Good);
    let saved = sign::dss::embed(&doc, &evidence).unwrap();
    assert!(saved.starts_with(&signed));
    let reopened = pdfcraft_cos::Document::open(Arc::new(saved.clone())).unwrap();
    let valid = sign::signatures(&reopened, &saved, &trust);
    assert_eq!(valid[0].status, sign::Status::Valid, "{:?}", valid[0].details);
    assert!(valid[0].details.iter().any(|s| s.contains("revocation")));
    let untrusted = sign::signatures(&reopened, &saved, &TrustStore::default());
    assert_eq!(untrusted[0].status, sign::Status::Unknown);
    let expired = VerificationTime::At(Time::from_pdf("20261201000000Z").unwrap());
    assert!(sign::evidence::check(&doc, &signed, &evidence, &trust, expired).is_err());
    let mut corrupted = evidence.clone();
    let last = corrupted.crls[0].len() - 1;
    corrupted.crls[0][last] ^= 1;
    assert!(sign::evidence::check(&doc, &signed, &corrupted, &trust, VerificationTime::BestAvailable).is_err());
    let revoked = sign::dss::Evidence { crls: vec![support::crl(&issuer, Some(&leaf.certificate)), support::crl(&issuer, None)], ..evidence };
    let report = sign::evidence::check(&doc, &signed, &revoked, &trust, VerificationTime::BestAvailable).unwrap();
    assert!(!report.has_complete_coverage());
    assert!(matches!(report.coverage[0].status, RevocationStatus::Revoked { .. }), "later good CRL/OCSP must not erase a revocation");
}

#[test]
fn evidence_limits_and_unsigned_documents_fail_before_embedding() {
    let doc = pdfcraft_cos::Document::open(Arc::new(support::pdf())).unwrap();
    for evidence in [
        sign::dss::Evidence::default(),
        sign::dss::Evidence { certs: vec![vec![0; 2 * 1024 * 1024 + 1]], ..Default::default() },
        sign::dss::Evidence { certs: vec![support::timestamp_id().certificate.raw], ..Default::default() },
    ] {
        assert!(sign::evidence::check(&doc, &support::pdf(), &evidence, &TrustStore::default(), VerificationTime::BestAvailable).is_err());
    }
}

#[test]
fn openssl_verifies_original_crl_and_ocsp_evidence() {
    use base64::Engine;
    let (issuer, leaf) = support::chain();
    let dir = std::env::temp_dir().join(format!("pdfcraft-evidence-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, cert) in [("root.pem", &issuer.certificate), ("leaf.pem", &leaf.certificate)] {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&cert.raw);
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            encoded.as_bytes().chunks(64).map(|b| std::str::from_utf8(b).unwrap()).collect::<Vec<_>>().join("\n")
        );
        std::fs::write(dir.join(name), pem).unwrap();
    }
    std::fs::write(dir.join("good.crl"), support::crl(&issuer, None)).unwrap();
    std::fs::write(dir.join("good.ocsp"), support::ocsp(&issuer, &leaf.certificate)).unwrap();
    let crl = std::process::Command::new("openssl")
        .args(["crl", "-in", "good.crl", "-inform", "DER", "-noout", "-verify", "-CAfile", "root.pem"])
        .current_dir(&dir)
        .output()
        .expect("OpenSSL is required for this interoperability gate");
    assert!(crl.status.success(), "{}", String::from_utf8_lossy(&crl.stderr));
    let ocsp = std::process::Command::new("openssl")
        .args([
            "ocsp",
            "-respin",
            "good.ocsp",
            "-CAfile",
            "root.pem",
            "-verify_other",
            "root.pem",
            "-issuer",
            "root.pem",
            "-sha256",
            "-cert",
            "leaf.pem",
            "-no_nonce",
            "-attime",
            &Time::from_pdf("20261012120000Z").unwrap().unix().to_string(),
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ocsp.status.success(), "{} {}", String::from_utf8_lossy(&ocsp.stdout), String::from_utf8_lossy(&ocsp.stderr));
    assert!(String::from_utf8_lossy(&ocsp.stdout).contains("leaf.pem: good"), "{}", String::from_utf8_lossy(&ocsp.stdout));
    assert!(String::from_utf8_lossy(&ocsp.stderr).contains("Response verify OK"));
}

const OCSP_SIGNING: &str = "1.3.6.1.5.5.7.3.9";

fn october() -> Time {
    Time::from_pdf("20261012120000Z").unwrap()
}

fn responder_name() -> pdfcraft_sign::Name {
    pdfcraft_sign::Name::build("Synthetic Delegated OCSP Responder", "", "PdfCraft Tests", "", "")
}

/// A responder certificate issued by the evidence root, valid through the fixture window.
fn delegated(usages: &[&str]) -> pdfcraft_sign::Certificate {
    let (issuer, _) = support::chain();
    support::responder_certificate(
        &support::responder_key(),
        &responder_name(),
        &issuer.key,
        &issuer.certificate.subject,
        usages,
        ("20260101000000Z", "20360101000000Z"),
    )
}

#[test]
fn a_delegated_ocsp_responder_with_the_signing_eku_verifies_by_name_and_by_key() {
    use pdfcraft_sign::revocation::OcspResponse;
    use support::ResponderIdForm;
    let (issuer, leaf) = support::chain();
    let responder = delegated(&[OCSP_SIGNING]);
    for form in [ResponderIdForm::Name, ResponderIdForm::Key] {
        let raw = support::ocsp_signed_by(&issuer, &leaf.certificate, &support::responder_key(), &responder, form, true);
        let parsed = OcspResponse::parse(&raw).unwrap();
        assert_eq!(parsed.check(&leaf.certificate, &issuer.certificate, october()), RevocationStatus::Good);
        // The same response without its embedded certificate names nobody that can be checked.
        let bare = support::ocsp_signed_by(&issuer, &leaf.certificate, &support::responder_key(), &responder, form, false);
        assert_eq!(OcspResponse::parse(&bare).unwrap().check(&leaf.certificate, &issuer.certificate, october()), RevocationStatus::Unknown);
    }
    // And through the user workflow: complete coverage from a delegated response alone.
    let doc = pdfcraft_cos::Document::open(Arc::new(support::pdf())).unwrap();
    let signed = sign::sign(&doc, &leaf, &sign::SignOptions { date: "D:20261012120000Z".into(), ..Default::default() }).unwrap();
    let doc = pdfcraft_cos::Document::open(Arc::new(signed.clone())).unwrap();
    let raw = support::ocsp_signed_by(&issuer, &leaf.certificate, &support::responder_key(), &responder, ResponderIdForm::Name, true);
    let evidence = sign::dss::Evidence { certs: Vec::new(), crls: Vec::new(), ocsps: vec![raw] };
    let trust = TrustStore { certs: vec![issuer.certificate.clone()] };
    let report = sign::evidence::check(&doc, &signed, &evidence, &trust, VerificationTime::BestAvailable).unwrap();
    assert!(report.has_complete_coverage(), "{report:?}");
}

#[test]
fn delegated_ocsp_responders_without_eku_from_another_issuer_or_copying_the_issuer_name_are_refused() {
    use pdfcraft_sign::revocation::OcspResponse;
    use support::ResponderIdForm;
    let (issuer, leaf) = support::chain();
    let key = support::responder_key();
    let check = |responder: &pdfcraft_sign::Certificate, form| {
        let raw = support::ocsp_signed_by(&issuer, &leaf.certificate, &key, responder, form, true);
        OcspResponse::parse(&raw).unwrap().check(&leaf.certificate, &issuer.certificate, october())
    };
    // No extended key usage, or a different one.
    assert_eq!(check(&delegated(&[]), ResponderIdForm::Name), RevocationStatus::Unknown);
    assert_eq!(check(&delegated(&["1.3.6.1.5.5.7.3.4"]), ResponderIdForm::Name), RevocationStatus::Unknown);
    // Correct EKU, but issued by an unrelated CA that merely uses the issuer's name.
    let foreign = support::responder_certificate(
        &key,
        &responder_name(),
        &support::other_ca_key(),
        &issuer.certificate.subject,
        &[OCSP_SIGNING],
        ("20260101000000Z", "20360101000000Z"),
    );
    assert_eq!(check(&foreign, ResponderIdForm::Name), RevocationStatus::Unknown);
    // Self-issued with the EKU.
    let own =
        support::responder_certificate(&key, &responder_name(), &key, &responder_name(), &[OCSP_SIGNING], ("20260101000000Z", "20360101000000Z"));
    assert_eq!(check(&own, ResponderIdForm::Name), RevocationStatus::Unknown);
    // A certificate copying the issuer's subject name, identified by its own key: still a
    // delegate, and this one was never issued by the CA.
    let impostor = support::responder_certificate(
        &key,
        &issuer.certificate.subject,
        &key,
        &issuer.certificate.subject,
        &[OCSP_SIGNING],
        ("20260101000000Z", "20360101000000Z"),
    );
    assert_eq!(check(&impostor, ResponderIdForm::Key), RevocationStatus::Unknown);
    // Properly issued, but expired before the response was produced.
    let (issuer_id, _) = support::chain();
    let expired = support::responder_certificate(
        &key,
        &responder_name(),
        &issuer_id.key,
        &issuer_id.certificate.subject,
        &[OCSP_SIGNING],
        ("20260101000000Z", "20260601000000Z"),
    );
    assert_eq!(check(&expired, ResponderIdForm::Name), RevocationStatus::Unknown);
}

#[test]
fn openssl_verifies_the_original_delegated_ocsp_response() {
    use base64::Engine;
    let (issuer, leaf) = support::chain();
    let responder = delegated(&[OCSP_SIGNING]);
    let raw = support::ocsp_signed_by(&issuer, &leaf.certificate, &support::responder_key(), &responder, support::ResponderIdForm::Name, true);
    let dir = std::env::temp_dir().join(format!("pdfcraft-delegated-ocsp-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, cert) in [("root.pem", &issuer.certificate), ("leaf.pem", &leaf.certificate)] {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&cert.raw);
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            encoded.as_bytes().chunks(64).map(|b| std::str::from_utf8(b).unwrap()).collect::<Vec<_>>().join("\n")
        );
        std::fs::write(dir.join(name), pem).unwrap();
    }
    std::fs::write(dir.join("delegated.ocsp"), raw).unwrap();
    let at = october().unix().to_string();
    let run = |extra: &[&str]| {
        let mut args = vec![
            "ocsp",
            "-respin",
            "delegated.ocsp",
            "-CAfile",
            "root.pem",
            "-issuer",
            "root.pem",
            "-sha256",
            "-cert",
            "leaf.pem",
            "-no_nonce",
            "-attime",
            &at,
        ];
        args.extend_from_slice(extra);
        std::process::Command::new("openssl").args(&args).current_dir(&dir).output().expect("OpenSSL is required for this interoperability gate")
    };
    let ocsp = run(&[]);
    let text = format!("{}{}", String::from_utf8_lossy(&ocsp.stdout), String::from_utf8_lossy(&ocsp.stderr));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ocsp.status.success(), "{text}");
    assert!(text.contains("Response verify OK"), "{text}");
    assert!(text.contains("leaf.pem: good"), "{text}");
}

/// A local in-process authority with a certificate issued by the evidence root.
struct IssuedTsa {
    key: pdfcraft_sign::PrivateKey,
    certificate: pdfcraft_sign::Certificate,
}

impl pdfcraft_sign::TimestampAuthority for IssuedTsa {
    fn timestamp(&self, request: &[u8]) -> Result<Vec<u8>, pdfcraft_sign::SignError> {
        let query = pdfcraft_sign::timestamp::parse_request(request)?;
        pdfcraft_sign::timestamp::respond(&self.key, &self.certificate, &[], pdfcraft_sign::DigestAlg::Sha256, &query, "1.2.3.4", october(), 21)
    }
}

#[test]
fn timestamp_authority_chains_are_part_of_revocation_coverage() {
    use pdfcraft_sign::evidence::CoverageRole;
    let (issuer, leaf) = support::chain();
    let key = support::responder_key();
    let tsa = IssuedTsa { certificate: support::issued_timestamp_authority(&key, &issuer), key };
    let options = sign::SignOptions { date: "D:20261012120000Z".into(), ..Default::default() };
    let signed = sign::sign_with_timestamp(&pdfcraft_cos::Document::open(Arc::new(support::pdf())).unwrap(), &leaf, &options, &tsa).unwrap();
    let doc = pdfcraft_cos::Document::open(Arc::new(signed.clone())).unwrap();
    let trust = TrustStore { certs: vec![issuer.certificate.clone()] };
    let listed = sign::signatures(&doc, &signed, &trust);
    assert!(listed[0].timestamp_time.is_some(), "{:?}", listed[0].details);
    assert_eq!(listed[0].timestamp_chain.first(), Some(&tsa.certificate));
    let roles = |report: &sign::evidence::EvidenceReport, role| {
        report.coverage.iter().filter(|c| c.role == role).map(|c| c.status.clone()).collect::<Vec<_>>()
    };

    // The root's CRL covers both the signer and the timestamp authority.
    let crl = sign::dss::Evidence { crls: vec![support::crl(&issuer, None)], ..Default::default() };
    let report = sign::evidence::check(&doc, &signed, &crl, &trust, VerificationTime::BestAvailable).unwrap();
    assert_eq!(roles(&report, CoverageRole::Signer), vec![RevocationStatus::Good]);
    assert_eq!(roles(&report, CoverageRole::SignatureTimestamp), vec![RevocationStatus::Good]);
    assert!(report.has_complete_coverage(), "{report:?}");

    // OCSP for the signer alone leaves the authority uncovered, and the report says so.
    let signer_only = sign::dss::Evidence { ocsps: vec![support::ocsp(&issuer, &leaf.certificate)], ..Default::default() };
    let report = sign::evidence::check(&doc, &signed, &signer_only, &trust, VerificationTime::BestAvailable).unwrap();
    assert_eq!(roles(&report, CoverageRole::Signer), vec![RevocationStatus::Good]);
    assert_eq!(roles(&report, CoverageRole::SignatureTimestamp), vec![RevocationStatus::Unknown]);
    assert!(!report.has_complete_coverage());
    assert!(report.notes.iter().any(|n| n.contains("incomplete")), "{:?}", report.notes);

    // Evidence about the authority alone is related to the document and is accepted.
    let authority_only = sign::dss::Evidence { ocsps: vec![support::ocsp(&issuer, &tsa.certificate)], ..Default::default() };
    let report = sign::evidence::check(&doc, &signed, &authority_only, &trust, VerificationTime::BestAvailable).unwrap();
    assert_eq!(roles(&report, CoverageRole::SignatureTimestamp), vec![RevocationStatus::Good]);
    assert_eq!(roles(&report, CoverageRole::Signer), vec![RevocationStatus::Unknown]);

    // A revoked authority is reported, never hidden behind a good signer status.
    let revoked = sign::dss::Evidence { crls: vec![support::crl(&issuer, Some(&tsa.certificate))], ..Default::default() };
    let report = sign::evidence::check(&doc, &signed, &revoked, &trust, VerificationTime::BestAvailable).unwrap();
    assert!(matches!(roles(&report, CoverageRole::SignatureTimestamp).as_slice(), [RevocationStatus::Revoked { .. }]));
    assert!(!report.has_complete_coverage());

    // A standalone document timestamp's authority chain is covered under its own role.
    let stamped = sign::timestamp_document(&doc, &tsa, "D:20261012120000Z").unwrap();
    let stamped_doc = pdfcraft_cos::Document::open(Arc::new(stamped.clone())).unwrap();
    let report = sign::evidence::check(&stamped_doc, &stamped, &crl, &trust, VerificationTime::BestAvailable).unwrap();
    assert_eq!(roles(&report, CoverageRole::DocumentTimestamp), vec![RevocationStatus::Good]);
    assert!(report.has_complete_coverage(), "{report:?}");
}
