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
    assert_eq!(restored.timestamp_servers, h.state().timestamp_servers);
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
    assert_eq!(h.state().timestamp_servers.len(), 1);
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
    h.state_mut().timestamp_servers.push(pdfcraft_engine::signature_workflow::TimestampServer {
        name: "Synthetic local authority".into(),
        url,
        policy: Some("1.2.3.4".into()),
        timeout_seconds: 3,
        require_trusted_authority: true,
    });
    h.state_mut().session.set_trusted_certificates(vec![cert]);
    let out = std::env::temp_dir().join(format!("pdfcraft-native-timestamp-{}.pdf", std::process::id()));
    h.state_mut().save_override = Some(out.to_string_lossy().into_owned());
    h.state_mut().execute("sign.timestamp");
    h.run_steps(2);
    h.state_mut().set_option("timestamp-server", "0").unwrap();
    h.state_mut().set_option("timestamp-network", "true").unwrap();
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
