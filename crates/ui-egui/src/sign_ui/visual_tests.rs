//! Diagnostic captures of the production shell and real asynchronous identity operations.
//! No screenshots or generated identities belong in the repository or release packages.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use std::path::Path;
use std::time::{Duration, Instant};

struct CaptureApp {
    app: PdfCraftApp,
    hold_completion: bool,
}

impl eframe::App for CaptureApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Keep a genuine pending result unconsumed for one diagnostic capture. The worker still
        // runs and Cancel still uses its real token. Production code never pauses its polling.
        if !self.hold_completion {
            eframe::App::logic(&mut self.app, ctx, frame);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut self.app, ui, frame);
    }
}

fn settle(h: &mut Harness<'static, CaptureApp>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        h.run_steps(2);
        if h.state().app.id_import.is_none() && h.state().app.signature_run.is_none() {
            break;
        }
        assert!(Instant::now() < deadline, "identity operation did not settle within 30 seconds");
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(2);
}

fn capture(h: &mut Harness<'static, CaptureApp>, folder: &Path, name: &str) {
    // render() renders the most recent genuine UI pass; it does not replace any widgets.
    let image = h.render().unwrap();
    assert!(image.width() >= 1200 && image.height() >= 800);
    image.save(folder.join(name)).unwrap();
}

#[test]
#[ignore = "generates diagnostic UI images; set PDFCRAFT_IDENTITY_UI_SHOTS and run explicitly"]
fn capture_identity_workflow() {
    assert_eq!(std::env::var("PDFCRAFT_SYSTEM_FONTS").as_deref(), Ok("0"), "diagnostic captures require bundled fonts only");
    let folder = std::env::var_os("PDFCRAFT_IDENTITY_UI_SHOTS").map(PathBuf::from).expect("PDFCRAFT_IDENTITY_UI_SHOTS is required");
    std::fs::create_dir_all(&folder).unwrap();
    let work = tempfile::tempdir().unwrap();
    // Every document, identity and name shown here is synthetic. Only the bundled interface
    // fonts are used; the blank document needs no installed or third-party document font.
    const PDF: &[u8] = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF";
    let key = PrivateKey::generate_p256().unwrap();
    let name = Name::build("Synthetic Signer", "", "", "", "US");
    let cert = Certificate::self_signed(&name, &key, sign::Time::from_unix(1_780_000_000), 5, &[1]).unwrap();
    let id = DigitalId { key, certificate: cert, chain: Vec::new(), friendly_name: Some("Synthetic Signer".into()) };
    let identity_path = work.path().join("synthetic-identity.p12");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write as _;
    options.open(&identity_path).unwrap().write_all(&sign::pkcs12::write(&id, "visual-test").unwrap()).unwrap();

    for mode in ["light", "dark"] {
        let output = folder.join(mode);
        std::fs::create_dir_all(&output).unwrap();
        let identity_path = identity_path.clone();
        let signed_path = work.path().join(format!("signed-{mode}.pdf"));
        let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.set_option("theme", mode).unwrap();
            app.open_bytes("Synthetic signature check.pdf", None, PDF.to_vec()).unwrap();
            app.save_override = Some(signed_path.to_string_lossy().into_owned());
            app.start_signing(0, None, None, None);
            let draft = &mut app.sign_draft.as_mut().unwrap().new_id;
            draft.create = false;
            draft.file = identity_path.to_string_lossy().into_owned();
            draft.file_password = "visual-test".into();
            CaptureApp { app, hold_completion: false }
        });
        h.run_steps(6);
        h.get_by_label("Configure a Digital ID for Signing");
        capture(&mut h, &output, "01-import-file.png");

        h.state_mut().hold_completion = true;
        h.get_by_label("Continue").click();
        h.step();
        assert!(h.state().app.id_import.is_some(), "the real import worker must be queued");
        h.run_steps(2);
        h.get_by_label("Opening digital ID...");
        h.get_by_label("Cancel operation");
        capture(&mut h, &output, "02-import-pending.png");
        h.get_by_label("Cancel operation").click();
        h.step();
        assert!(h.state().app.id_import.as_ref().unwrap().control.is_cancelled());
        h.state_mut().hold_completion = false;
        settle(&mut h);
        assert!(h.state().app.digital_ids.is_empty(), "a cancelled completion cannot register the identity");
        h.get_by_label("Configure a Digital ID for Signing");
        capture(&mut h, &output, "03-import-cancelled.png");

        h.get_by_label("Continue").click();
        h.step();
        settle(&mut h);
        assert_eq!(h.state().app.digital_ids.len(), 1);
        h.get_by_label("Continue").click();
        h.run_steps(2);
        h.state_mut().app.sign_draft.as_mut().unwrap().password = "wrong-password".into();
        h.get_by_label("Sign").click();
        h.step();
        settle(&mut h);
        assert_eq!(h.state().app.sign_draft.as_ref().unwrap().step, SignStep::SignAs);
        assert_eq!(h.get_all_by_label("The password is incorrect.").count(), 1);
        h.get_by_label("The password is incorrect.");
        capture(&mut h, &output, "04-password-retry.png");
    }
    for mode in ["light", "dark"] {
        for image in ["01-import-file.png", "02-import-pending.png", "03-import-cancelled.png", "04-password-retry.png"] {
            assert!(std::fs::metadata(folder.join(mode).join(image)).unwrap().len() > 0);
        }
    }
    // The workflow uploads only these diagnostic images, never the generated private identity.
    eprintln!("Captured real identity import/cancel/retry controls in light and dark themes.");
}
