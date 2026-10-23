//! Native signature workflow state, local evidence persistence and save-result cancellation.
use egui_kittest::{Harness, kittest::Queryable};
use pdfcraft_engine::{Session, SignOptions};
use pdfcraft_ui_egui::{PdfCraftApp, timestamp_ui::SignatureWorkflowMode};

#[path = "../../sign/tests/support/mod.rs"]
mod support;

fn harness() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.session = Session::new().with_clock(|| pdfcraft_sign::Time::from_pdf("20261012120000Z").unwrap().unix());
        app.open_bytes("original.pdf", None, support::pdf()).unwrap();
        app.run_inline = true;
        app
    });
    h.run_steps(6);
    h
}

#[test]
fn configured_servers_persist_and_network_opt_in_is_required_each_time() {
    let mut h = harness();
    let server = r#"[{"name":"Local synthetic","url":"http://127.0.0.1:1/unused","timeout_seconds":1,"require_trusted_authority":true}]"#;
    h.state_mut().set_option("timestamp-servers", server).unwrap();
    let mut restored = PdfCraftApp::new();
    restored.restore(&h.state().persist());
    assert_eq!(restored.session.timestamp_servers(), h.state().session.timestamp_servers());
    assert!(h.state_mut().execute("sign.timestamp"));
    h.run_steps(2);
    h.get_by_label("Timestamp document");
    h.state_mut().set_option("timestamp-server", "0").unwrap();
    assert!(h.state_mut().save_signature_workflow().unwrap_err().contains("Allow"));
    h.state_mut().set_option("timestamp-network", "true").unwrap();
    h.state_mut().open_signature_workflow(SignatureWorkflowMode::Timestamp);
    assert!(!h.state().signature_workflow.allow_network);
    assert!(
        h.state_mut()
            .set_option(
                "timestamp-servers",
                r#"[{"name":"bad","url":"http://example.invalid/","timeout_seconds":1,"require_trusted_authority":true}]"#
            )
            .is_err()
    );
    assert_eq!(h.state().session.timestamp_servers().len(), 1);
}

#[test]
fn native_evidence_save_cancel_and_stale_result_keep_original_document() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-signature-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (issuer, leaf) = support::chain();
    let crl = dir.join("good.crl");
    std::fs::write(&crl, support::crl(&issuer, None)).unwrap();
    let mut h = harness();
    let doc = h.state().views[0].id;
    h.state_mut().session.set_trusted_certificates(vec![issuer.certificate.clone()]);
    let signed = h.state().session.sign(doc, &leaf, SignOptions::default()).unwrap();
    h.state_mut().session.mark_signed(doc, signed.clone(), None).unwrap();
    h.state_mut().open_signature_workflow(SignatureWorkflowMode::Evidence);
    h.state_mut().signature_workflow.crls = crl.to_string_lossy().into_owned();
    let out = dir.join("saved.pdf");
    h.state_mut().save_override = Some(out.to_string_lossy().into_owned());
    h.state_mut().save_signature_workflow().unwrap();
    h.state_mut().cancel_signature_workflow();
    h.run_steps(2);
    assert!(!out.exists());
    assert!(std::sync::Arc::ptr_eq(&h.state().session.get(doc).unwrap().bytes, &signed));
    h.state_mut().save_signature_workflow().unwrap();
    h.state_mut().session.apply(doc, pdfcraft_engine::Edit::SetInfo { key: "Title".into(), value: "Edit during job".into() }).unwrap();
    h.run_steps(2);
    assert!(!out.exists());
    assert!(h.state().signature_workflow.error.as_deref().unwrap().contains("changed"));
    h.state_mut().save_signature_workflow().unwrap();
    h.run_steps(3);
    assert!(out.exists());
    let saved = std::fs::read(&out).unwrap();
    assert!(saved.starts_with(&signed));
    let reopened = h.state_mut().session.open("reopened.pdf", None, std::sync::Arc::new(saved), None).unwrap();
    assert_eq!(h.state().session.get(reopened).unwrap().signatures[0].status, pdfcraft_sign::Status::Valid);
    h.state_mut().open_signature_workflow(SignatureWorkflowMode::Verify);
    h.state_mut().set_option("signature-verification-time", "20500101000000Z").unwrap();
    h.state_mut().verify_signature_workflow().unwrap();
    assert_eq!(h.state().signature_workflow.results[0].verification_time_source, pdfcraft_sign::VerificationTimeSource::ChosenTime);
    assert_eq!(h.state().signature_workflow.results[0].status, pdfcraft_sign::Status::Unknown);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn native_timestamp_dialog_saves_and_reopens_with_trusted_time() {
    let mut h = harness();
    let (url, cert, worker) = support::local_timestamp_server();
    let server = pdfcraft_engine::signature_workflow::TimestampServer {
        name: "Synthetic local authority".into(),
        url,
        policy: Some("1.2.3.4".into()),
        timeout_seconds: 3,
        require_trusted_authority: true,
        username: None,
    };
    h.state_mut().session.set_timestamp_servers(vec![server]).unwrap();
    h.state_mut().session.set_trusted_certificates(vec![cert]);
    let out = std::env::temp_dir().join(format!("pdfcraft-native-timestamp-{}.pdf", std::process::id()));
    h.state_mut().save_override = Some(out.to_string_lossy().into_owned());
    h.state_mut().execute("sign.timestamp");
    h.run_steps(2);
    h.state_mut().set_option("timestamp-server", "0").unwrap();
    h.state_mut().set_option("timestamp-network", "true").unwrap();
    // Choosing a server shows its consent checkbox: let the dialog settle before clicking.
    h.run_steps(3);
    h.get_by_label("Save result…").click();
    h.run_steps(4);
    worker.join().unwrap();
    let saved = std::fs::read(&out).unwrap();
    let doc = h.state_mut().session.open("reopened.pdf", None, std::sync::Arc::new(saved), None).unwrap();
    let signature = &h.state().session.get(doc).unwrap().signatures[0];
    assert!(signature.doc_timestamp);
    assert_eq!(signature.status, pdfcraft_sign::Status::Valid);
    assert_eq!(signature.verification_time_source, pdfcraft_sign::VerificationTimeSource::TrustedTimestamp);
    std::fs::remove_file(out).unwrap();
}

fn signing_harness(dir: &std::path::Path) -> Harness<'static, PdfCraftApp> {
    let (issuer, leaf) = support::chain();
    let p12 = dir.join("id.p12");
    std::fs::write(&p12, pdfcraft_sign::pkcs12::write(&leaf, "test").unwrap()).unwrap();
    let mut h = harness();
    h.state_mut().session.set_trusted_certificates(vec![issuer.certificate.clone()]);
    h.state_mut().digital_ids.push(pdfcraft_ui_egui::DigitalIdEntry {
        path: p12.to_string_lossy().into_owned(),
        name: "Synthetic Evidence Signer".into(),
        issuer: String::new(),
        email: String::new(),
        expires: String::new(),
    });
    h.state_mut().save_override = Some(dir.join("signed.pdf").to_string_lossy().into_owned());
    h
}

#[test]
fn sign_as_draws_a_signature_saves_the_appearance_and_locks_the_document() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-sign-look-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = signing_harness(&dir);
    h.state_mut().start_signing(0, Some([20.0, 20.0, 180.0, 80.0]), None, None);
    {
        let d = h.state_mut().sign_draft.as_mut().unwrap();
        d.step = pdfcraft_ui_egui::SignStep::SignAs;
        d.selected = Some(0);
        d.password = "test".into();
    }
    h.run_steps(2);
    h.get_by_label("Drawn signature").click_accesskit();
    h.run_steps(2);
    h.get_by_label("Sign here with your mouse, pen or finger");
    {
        let d = h.state_mut().sign_draft.as_mut().unwrap();
        assert_eq!(d.graphic, pdfcraft_ui_egui::GraphicChoice::Drawn);
        d.ink = vec![vec![[0, 0], [300, 120], [600, 10]], vec![[200, 200]]];
        d.save_as = "Initials".into();
    }
    h.run_steps(1);
    h.get_by_label("Save appearance").click_accesskit();
    h.run_steps(2);
    assert_eq!(h.state().session.signature_appearances().len(), 1);
    assert_eq!(h.state().sign_draft.as_ref().unwrap().saved.as_deref(), Some("Initials"));
    h.get_by_label("Lock document after signing").click_accesskit();
    h.run_steps(1);
    assert!(h.state().sign_draft.as_ref().unwrap().lock_document);
    h.get_by_label("Sign").click_accesskit();
    h.run_steps(6);
    let s = h.state();
    assert_eq!(s.sign_draft.as_ref().and_then(|d| d.error.clone()), None);
    let doc = s.session.get(s.views[0].id).unwrap();
    let sig = doc.signatures.iter().find(|x| x.signed).unwrap();
    assert_eq!(sig.status, pdfcraft_sign::Status::Valid, "{:?}", sig.details);
    assert!(sig.visible);
    // Locked: every later change breaks the signature's permissions.
    assert!(doc.signatures.iter().any(|x| x.lock.as_ref().is_some_and(|l| l.permissions == Some(1))), "{:?}", doc.signatures);
    // The saved appearance survives a restart and applies to a new signature.
    let mut restored = PdfCraftApp::new();
    restored.restore(&h.state().persist());
    assert_eq!(restored.session.signature_appearances(), h.state().session.signature_appearances());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compare_signed_version_opens_side_by_side_with_the_permissions_verdict() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-sign-compare-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = harness();
    let (issuer, leaf) = support::chain();
    let doc = h.state().views[0].id;
    h.state_mut().session.set_trusted_certificates(vec![issuer.certificate.clone()]);
    let signed = h.state().session.sign(doc, &leaf, SignOptions::default()).unwrap();
    h.state_mut().session.mark_signed(doc, signed, None).unwrap();
    h.state_mut().session.apply(doc, pdfcraft_engine::Edit::SetInfo { key: "Title".into(), value: "Changed after signing".into() }).unwrap();
    h.state_mut().right = Some(pdfcraft_ui_egui::RightPanel::Signatures);
    h.run_steps(3);
    assert!(h.state_mut().execute("sign.compare_signed_version"));
    h.run_steps(6);
    let s = h.state();
    let c = s.signed_comparison.as_ref().expect("the signed comparison is kept");
    assert_eq!(c.doc, doc);
    assert!(c.result.modified());
    assert_eq!(s.views.len(), 2, "the signed version opened beside the current document");
    assert_eq!(s.views[s.active.unwrap()].id, doc, "the current document stays active");
    assert!(s.compare.as_ref().is_some_and(|x| x.old == c.signed && x.new == doc));
    assert!(s.side_by_side_showing(), "the comparison opens side by side");
    h.get_by_label_contains("Signed version of");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn preferences_signatures_lists_trusted_certificates_with_their_trust() {
    let mut h = harness();
    let (issuer, _) = support::chain();
    h.state_mut().session.set_trusted_certificates(vec![issuer.certificate.clone()]);
    assert!(h.state_mut().execute("sign.trusted_certificates"));
    h.run_steps(3);
    assert_eq!(h.state().dialog, Some(pdfcraft_ui_egui::Dialog::Preferences));
    h.get_by_label("Trusted root");
    h.get_by_label("Certified documents").click_accesskit();
    h.run_steps(2);
    let fp = issuer.certificate.fingerprint();
    assert!(!h.state().session.certificate_trust(&fp).unwrap().1.certified_documents);
    let mut restored = PdfCraftApp::new();
    restored.restore(&h.state().persist());
    assert!(!restored.session.certificate_trust(&fp).unwrap().1.certified_documents, "trust settings are saved");
    // Hostile import files are refused without changing the list.
    let bad = std::env::temp_dir().join(format!("pdfcraft-bad-cert-{}.cer", std::process::id()));
    std::fs::write(&bad, b"-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n").unwrap();
    assert!(h.state_mut().import_trusted_certificates(&bad).is_err());
    assert_eq!(h.state().session.trusted_certificates().len(), 1);
    std::fs::remove_file(bad).unwrap();
}
