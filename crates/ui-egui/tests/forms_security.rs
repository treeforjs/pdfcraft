//! Field triggers fired by the pointer and the focus, keystroke checks while typing, Set Document
//! Actions, the Signed tab, Submit Form's mock sender, reply import and email hand-off, certificate
//! security, the Trust Manager, and redaction code sets and locales, in the real shell
//! (egui_kittest). All values are synthetic.

use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::submit::{Payload, Reply};
use pdfcraft_ui_egui::{Dialog, PdfCraftApp, ScriptTarget};

fn pdf(objs: &[&str]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn harness(bytes: Vec<u8>) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("form.pdf", None, bytes).unwrap();
        app.set_option("left", "closed").unwrap();
        app.set_option("zoom", "150").unwrap();
        app
    });
    for _ in 0..60 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h
}

fn field_center(h: &Harness<'static, PdfCraftApp>, name: &str) -> egui::Pos2 {
    let s = h.state();
    let doc = s.session.get(s.views[0].id).unwrap();
    let f = doc.form.iter().find(|f| f.name == name).unwrap();
    pdfcraft_ui_egui::forms_ui::field_screen_rect(&s.views[0], &doc.info, f, 0).expect("on screen").center()
}

fn click_at(h: &mut Harness<'static, PdfCraftApp>, p: egui::Pos2) {
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(3);
}

/// Whether widget object `obj` has the Hidden annotation flag in what Save would write.
fn hidden(h: &Harness<'static, PdfCraftApp>, obj: u32) -> bool {
    let s = h.state();
    let bytes = s.session.save_bytes(s.views[0].id).unwrap();
    let doc = pdfcraft_cos::Document::open(bytes).unwrap();
    doc.get(pdfcraft_cos::ObjRef::new(obj, 0)).as_dict().and_then(|d| d.int(b"F")).unwrap_or(0) & 2 != 0
}

fn value(h: &Harness<'static, PdfCraftApp>, name: &str) -> Vec<String> {
    let s = h.state();
    s.session.get(s.views[0].id).unwrap().form.iter().find(|f| f.name == name).unwrap().value.clone()
}

/// Text field `a` hides `t1` on Mouse Enter and shows it on Mouse Exit, hides `t3` on Mouse Down,
/// hides `t2` on Focus and shows it on Blur; push button `b` hides `t4` then (its chain's second
/// action) `t5`. The targets are objects 6 to 10.
fn trigger_fixture() -> Vec<u8> {
    let hide = |t: &str, h: bool| format!("<< /S /Hide /T ({t}) /H {h} >>");
    let a = format!(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (a) /Rect [20 340 280 360] /P 3 0 R /AA << /E {} /X {} /D {} /Fo {} /Bl {} >> >>",
        hide("t1", true),
        hide("t1", false),
        hide("t3", true),
        hide("t2", true),
        hide("t2", false)
    );
    let b = "<< /Type /Annot /Subtype /Widget /FT /Btn /Ff 65536 /T (b) /Rect [20 300 100 320] /P 3 0 R /A << /S /Hide /T (t4) /Next << /S /Hide /T (t5) >> >> >>".to_string();
    let target = |n: usize| {
        format!("<< /Type /Annot /Subtype /Widget /FT /Tx /T (t{n}) /F 4 /Rect [{x} 40 {x2} 60] /P 3 0 R >>", x = 20 + n * 50, x2 = 60 + n * 50)
    };
    let (t1, t2, t3, t4, t5) = (target(1), target(2), target(3), target(4), target(5));
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R 6 0 R 7 0 R 8 0 R 9 0 R 10 0 R] /DA (/Helv 0 Tf 0 g) >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 400] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R 6 0 R 7 0 R 8 0 R 9 0 R 10 0 R] >>",
        &a,
        &b,
        &t1,
        &t2,
        &t3,
        &t4,
        &t5,
    ])
}

#[test]
fn pointer_and_focus_fire_every_field_trigger_in_order() {
    let mut h = harness(trigger_fixture());
    let (a, b) = (field_center(&h, "a"), field_center(&h, "b"));
    let empty = egui::pos2(a.x, a.y + 160.0);
    assert!(!hidden(&h, 6));
    h.hover_at(a);
    h.run_steps(3);
    assert!(hidden(&h, 6), "Mouse Enter hid t1");
    h.hover_at(empty);
    h.run_steps(3);
    assert!(!hidden(&h, 6), "Mouse Exit showed t1 again");
    click_at(&mut h, a);
    assert!(hidden(&h, 8), "Mouse Down hid t3");
    assert!(hidden(&h, 7), "On Focus hid t2");
    assert!(h.state().views[0].forms.focus.is_some());
    click_at(&mut h, b);
    assert!(!hidden(&h, 7), "On Blur showed t2 again when the button took the press");
    assert!(hidden(&h, 9) && hidden(&h, 10), "the button's Mouse Up chain ran both actions");
    // Nothing more fires while the pointer rests: the queue stays empty.
    h.run_steps(5);
    assert!(h.state().views[0].forms.fires.is_empty());
}

/// Text field `n` with a number format and `c` with a custom Keystroke script.
fn keystroke_fixture() -> Vec<u8> {
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R] /DA (/Helv 0 Tf 0 g) >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 400] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (n) /Rect [20 340 280 360] /P 3 0 R /AA << /F << /S /JavaScript /JS (AFNumber_Format\\(2, 0, 0, 0, \"\", true\\);) >> /K << /S /JavaScript /JS (AFNumber_Keystroke\\(2, 0, 0, 0, \"\", true\\);) >> >> >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (c) /Rect [20 300 280 320] /P 3 0 R /AA << /K << /S /JavaScript /JS (if \\(!event.willCommit\\) event.change = event.change.toUpperCase\\(\\);) >> >> >>",
    ])
}

#[test]
fn keystrokes_are_checked_while_typing() {
    let mut h = harness(keystroke_fixture());
    h.state_mut().session.set_javascript(false);
    let n = field_center(&h, "n");
    click_at(&mut h, n);
    for c in ["1", "2", "x", "3"] {
        h.event(egui::Event::Text(c.into()));
        h.run_steps(2);
    }
    h.key_press(egui::Key::Enter);
    h.run_steps(4);
    assert_eq!(value(&h, "n"), ["123"], "the letter was refused as it was typed");
    h.state_mut().session.set_javascript(true);
    let c = field_center(&h, "c");
    click_at(&mut h, c);
    for ch in ["a", "b"] {
        h.event(egui::Event::Text(ch.into()));
        h.run_steps(2);
    }
    h.key_press(egui::Key::Enter);
    h.run_steps(4);
    assert_eq!(value(&h, "c"), ["AB"], "the script rewrote each keystroke");
}

fn blank() -> Vec<u8> {
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 400] >>",
        "<< /Type /Page /Parent 2 0 R >>",
    ])
}

#[test]
fn set_document_actions_saves_each_trigger_as_one_undo_step() {
    let mut h = harness(blank());
    assert!(h.state_mut().execute("tools.document_actions"));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::ActionScript));
    assert_eq!(h.state().action_script.as_ref().unwrap().target, ScriptTarget::Document);
    h.get_by_label("Document Actions");
    h.get_by_label("Document Will Save").click();
    h.run_steps(2);
    h.state_mut().action_script.as_mut().unwrap().scripts[1] = "console.println('saving')".into();
    h.get_by_label("Document Did Print").click();
    h.run_steps(2);
    h.state_mut().action_script.as_mut().unwrap().scripts[4] = "console.println('printed')".into();
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    let id = h.state().views[0].id;
    let doc = h.state().session.get(id).unwrap();
    let actions = doc.document_actions();
    assert_eq!(actions.len(), 2, "{actions:?}");
    assert_eq!(doc.can_undo(), Some("Set document actions"));
    // Reopened, the dialog shows them; clearing one removes only it.
    h.state_mut().open_document_actions();
    h.run_steps(2);
    assert_eq!(h.state().action_script.as_ref().unwrap().scripts[1], "console.println('saving')");
    h.state_mut().action_script.as_mut().unwrap().scripts[1] = "  ".into();
    h.run_steps(1);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert_eq!(h.state().session.get(id).unwrap().document_actions().len(), 1);
}

fn signature_fixture() -> Vec<u8> {
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R 6 0 R] /DA (/Helv 0 Tf 0 g) >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 400] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R 6 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /FT /Sig /T (Approval) /Rect [20 300 200 340] /P 3 0 R >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (Amount) /Rect [20 260 200 280] /P 3 0 R >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (Notes) /Rect [20 220 200 240] /P 3 0 R >>",
    ])
}

#[test]
fn the_signed_tab_sets_what_signing_locks() {
    use pdfcraft_engine::sign::{FieldLock, LockAction};
    let mut h = harness(signature_fixture());
    let id = h.state().views[0].id;
    h.state_mut().open_field_props("Approval", 0);
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::FieldProps));
    h.get_by_label("Signed").click();
    h.run_steps(2);
    h.get_by_label("Mark as read-only just these fields").click();
    h.run_steps(2);
    h.get_by_label("Amount").click();
    h.run_steps(1);
    h.get_by_label("Lock the document after signing").click();
    h.run_steps(1);
    h.get_by_label("OK").click();
    h.run_steps(3);
    let lock = h.state().session.get(id).unwrap().signature_field_lock("Approval");
    assert_eq!(lock, Some(FieldLock { action: LockAction::Include(vec!["Amount".into()]), permissions: Some(1) }));
    // Back to nothing: the lock is removed.
    h.state_mut().open_field_props("Approval", 0);
    h.run_steps(2);
    h.get_by_label("Signed").click();
    h.run_steps(2);
    h.get_by_label("Lock the document after signing").click();
    h.run_steps(1);
    h.get_by_label("Nothing happens").click();
    h.run_steps(1);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert_eq!(h.state().session.get(id).unwrap().signature_field_lock("Approval"), None);
}

fn submit_fixture(url: &str) -> Vec<u8> {
    let button = format!(
        "<< /Type /Annot /Subtype /Widget /FT /Btn /Ff 65536 /T (send) /Rect [20 300 100 320] /P 3 0 R /A << /S /SubmitForm /F << /FS /URL /F ({url}) >> /Flags 4 >> >>"
    );
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R] /DA (/Helv 0 Tf 0 g) >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 400] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (Ada) /Rect [20 340 280 360] /P 3 0 R >>",
        &button,
    ])
}

#[test]
fn submitting_uses_the_sender_imports_the_reply_and_hands_email_off() {
    let mut h = harness(submit_fixture("https://forms.example.test/in"));
    let sent: Arc<Mutex<Vec<Payload>>> = Arc::default();
    let log = sent.clone();
    h.state_mut().run_inline = true;
    h.state_mut().submit_sender = Some(Arc::new(move |p: &Payload| {
        log.lock().unwrap().push(p.clone());
        let fdf = b"%FDF-1.2\n1 0 obj << /FDF << /Fields [<< /T (name) /V (Grace) >>] >> >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n";
        Ok(Reply { status: 200, content_type: "application/vnd.fdf".into(), body: fdf.to_vec(), truncated: false, location: None })
    }));
    let b = field_center(&h, "send");
    click_at(&mut h, b);
    assert_eq!(h.state().dialog, Some(Dialog::SubmitForm));
    h.get_by_label("Send").click();
    h.run_steps(4);
    assert_eq!(sent.lock().unwrap().len(), 1, "the mock sender got the payload");
    assert!(String::from_utf8_lossy(&sent.lock().unwrap()[0].body).contains("Ada"));
    assert_eq!(h.state().dialog, None);
    assert_eq!(value(&h, "name"), ["Grace"], "the FDF reply filled the field back in");

    // A blocked host is never handed to the sender.
    h.state_mut().trust_add_host("forms.example.test", pdfcraft_engine::url_access::HostAction::Block, true).unwrap();
    h.state_mut().set_option("internet_access", "custom").ok();
    let mut policy = h.state().session.url_policy().clone();
    policy.mode = pdfcraft_engine::url_access::UrlMode::Custom;
    h.state_mut().session.set_url_policy(&policy);
    click_at(&mut h, b);
    h.get_by_label_contains("Not sent");
    assert!(h.query_by(|n| n.label().as_deref() == Some("Send") && n.is_disabled()).is_some());
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert_eq!(sent.lock().unwrap().len(), 1);

    // mailto: the addresses are shown and the email app is opened, never the sender.
    let mut h = harness(submit_fixture("mailto:forms@example.test?subject=Order"));
    let b = field_center(&h, "send");
    click_at(&mut h, b);
    h.get_by_label("forms@example.test");
    h.get_by_label("Order");
    h.get_by_label("Open email app").click();
    h.run_steps(3);
    assert_eq!(h.state().last_opened_url.as_deref(), Some("mailto:forms@example.test?subject=Order"));
}

#[test]
fn certificate_security_gives_each_recipient_their_permissions() {
    use pdfcraft_sign::{Certificate, Name, PrivateKey, Time};
    let mut h = harness(blank());
    assert!(h.state_mut().execute("protect.certificate"));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::ProtectCertificates));
    h.get_by_label("Encrypt with certificate");
    // Hostile files are refused with a reason; nothing is added.
    h.state_mut().add_certificate_recipient(b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n");
    h.run_steps(2);
    assert!(h.state().cert_draft.recipients.is_empty() && h.state().cert_draft.error.is_some());
    let key = PrivateKey::generate_rsa(2048).unwrap();
    let from = Time { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
    let cert = Certificate::self_signed(&Name::build("Reader Test", "", "", "reader@example.test", ""), &key, from, 5, &[7]).unwrap();
    let pem = pdfcraft_sign::x509::to_pem(&cert);
    h.state_mut().add_certificate_recipient(pem.as_bytes());
    h.state_mut().add_certificate_recipient(pem.as_bytes());
    h.run_steps(2);
    assert_eq!(h.state().cert_draft.recipients.len(), 1, "the same recipient isn't added twice");
    h.get_by_label("Allow changing the security settings (and everything else)").click();
    h.run_steps(2);
    h.get_by_label("Apply").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    let id = h.state().views[0].id;
    let sec = h.state().session.get(id).unwrap().security_summary().unwrap();
    assert!(sec.pending);
}

#[test]
fn the_trust_manager_edits_sites_and_attachment_types() {
    let mut h = harness(blank());
    assert!(h.state_mut().execute("app.trust_manager"));
    h.run_steps(3);
    assert_eq!(h.state().dialog, Some(Dialog::Preferences));
    h.get_by_label("Include subdomains");
    h.state_mut().trust_draft.host = "Evil.Example.TEST.".into();
    h.run_steps(1);
    h.get_by_label("Add site").click();
    h.run_steps(2);
    let rules = h.state().session.url_policy().rules.clone();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].host, "evil.example.test", "hosts are normalised");
    h.get_by_label("evil.example.test");
    // A bad host is refused with a reason.
    h.state_mut().trust_draft.host = "http://".into();
    h.run_steps(1);
    h.get_by_label("Add site").click();
    h.run_steps(2);
    assert!(h.state().trust_draft.error.is_some());
    assert_eq!(h.state().session.url_policy().rules.len(), 1);
    // Attachments: always allow .zip, then never open it (it moves lists).
    h.state_mut().trust_add_extension(".ZIP", true).unwrap();
    assert_eq!(h.state().session.attachment_trust().allow, ["zip"]);
    h.state_mut().trust_add_extension("zip", false).unwrap();
    assert!(h.state().session.attachment_trust().allow.is_empty());
    assert_eq!(h.state().session.attachment_trust().deny, ["zip"]);
    assert!(h.state_mut().trust_add_extension("a.b", true).is_err());
    h.run_steps(2);
    h.get_by_label("Remove").click();
    h.run_steps(2);
    assert!(h.state().session.url_policy().rules.is_empty());
    // Both are saved with the settings.
    let saved: serde_json::Value = serde_json::from_str(&h.state().persist()).unwrap();
    assert_eq!(saved["attachment_trust"]["deny"], serde_json::json!(["zip"]));
}

#[test]
fn redaction_code_sets_are_made_offered_and_saved() {
    let mut h = harness(blank());
    assert!(h.state_mut().execute("redact.code_sets"));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::RedactCodeSets));
    h.get_by_label("New set").click();
    h.run_steps(2);
    {
        let d = &mut h.state_mut().code_sets_draft;
        d.name = "Contract".into();
        d.codes = vec![("C-1".into(), "Pricing".into()), ("C-2".into(), String::new()), (String::new(), String::new())];
    }
    h.run_steps(1);
    h.run_steps(2);
    h.get_by_label("Save set").click();
    h.run_steps(3);
    assert_eq!(h.state().session.redaction_code_sets().len(), 1, "{:?}", h.state().code_sets_draft);
    assert_eq!(h.state().session.redaction_code_sets()[0].codes.len(), 2, "empty rows are dropped");
    // A broken file is refused and changes nothing; a good one is imported.
    h.state_mut().import_code_sets(b"{\"code_sets\": [{\"name\": \"\", \"codes\": [\"x\"]}]}");
    assert_eq!(h.state().session.redaction_code_sets().len(), 1);
    h.state_mut().import_code_sets(br#"{"name": "Medical", "codes": ["M-1"]}"#);
    assert_eq!(h.state().session.redaction_code_sets().len(), 2);
    let file: serde_json::Value = serde_json::from_slice(&h.state().code_sets_file()).unwrap();
    assert_eq!(file["code_sets"].as_array().unwrap().len(), 2);
    // Saved with the settings and restored leniently.
    let saved = h.state().persist();
    let mut app = PdfCraftApp::new();
    app.restore(&saved);
    assert_eq!(app.session.redaction_code_sets().len(), 2);
    h.get_by_label("Done").click();
    h.run_steps(2);
    // Redaction Properties offers the user's sets, and codes add up.
    {
        let s = h.state_mut();
        s.redact_prefs.use_overlay = true;
        s.redact_prefs.code_set = pdfcraft_engine::REDACTION_CODE_SETS.len();
    }
    assert!(h.state_mut().execute("redact.properties"));
    h.run_steps(2);
    assert!(h.query_all_by(|n| n.value().is_some_and(|v| v == "Contract")).next().is_some(), "the user's set is offered");
    assert_eq!(pdfcraft_ui_egui::toggle_redaction_code("C-1", "C-2"), "C-1, C-2");
    assert_eq!(pdfcraft_ui_egui::toggle_redaction_code("C-1, C-2", "C-1"), "C-2");
}

#[test]
fn find_and_redact_offers_patterns_by_locale() {
    let mut h = harness(blank());
    assert!(h.state_mut().execute("redact.search"));
    h.run_steps(2);
    h.get_by_label("Patterns").click();
    h.run_steps(2);
    h.state_mut().redact_search.locale = "de-DE";
    h.run_steps(2);
    h.get_by_label("Mark every pattern of this locale").click();
    h.run_steps(2);
    assert!(h.state().redact_search.every_pattern);
}
