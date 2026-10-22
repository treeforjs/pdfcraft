//! Updates (issue #28): Help ▸ Check for updates, the quiet daily check and installing from
//! inside PdfCraft, against a stand-in GitHub (no network).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use craft_update::{AppInfo, InstallKind, Source};
use ed25519_dalek::{Signer, SigningKey};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::PdfCraftApp;
use pdfcraft_ui_egui::updates::{Error, State, Updater, is_newer};
use sha2::{Digest, Sha256};

const REPO: &str = "treeforjs/pdfcraft";

/// A stand-in GitHub serving fixed answers, counting the release-list requests.
#[derive(Default)]
struct Fake {
    files: Mutex<HashMap<String, Vec<u8>>>,
    asked: std::sync::atomic::AtomicUsize,
}

impl Source for Fake {
    fn get(&self, url: &str, _limit: u64) -> Result<Vec<u8>, Error> {
        if url.contains("api.github.com") {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        self.files.lock().unwrap().get(url).cloned().ok_or_else(|| Error::Network("couldn't reach GitHub".into()))
    }

    fn download(&self, url: &str, limit: u64, sink: &mut dyn FnMut(&[u8]) -> Result<(), Error>) -> Result<(), Error> {
        sink(&self.get(url, limit)?)
    }
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[3u8; 32])
}

fn info() -> AppInfo {
    AppInfo {
        id: "pdfcraft".into(),
        display_name: "PdfCraft".into(),
        repo: REPO.into(),
        current: env!("CARGO_PKG_VERSION").into(),
        public_key: Some(STANDARD.encode(key().verifying_key().to_bytes())),
        prereleases: false,
    }
}

/// GitHub with release `tag` (signed) holding `payload` as the Linux AppImage; `None`: no release.
fn github(tag: Option<&str>, payload: &[u8]) -> Arc<Fake> {
    let fake = Fake::default();
    let mut files = fake.files.lock().unwrap();
    let list = match tag {
        None => "[]".to_string(),
        Some(tag) => {
            let version = craft_update::Version::parse(tag).unwrap();
            // The AppImage for this machine's processor, as the updater asks for it.
            let asset = InstallKind::AppImage("/x".into()).asset_name(&info(), &version, craft_update::Arch::current()).unwrap_or_default();
            let hex: String = Sha256::digest(payload).iter().map(|b| format!("{b:02x}")).collect();
            let sums = format!("{hex}  {asset}\n");
            let sig = STANDARD.encode(key().sign(sums.as_bytes()).to_bytes());
            let url = |n: &str| format!("https://github.com/{REPO}/releases/download/{tag}/{n}");
            let entry = |n: &str, size: usize| format!(r#"{{"name":"{n}","size":{size},"browser_download_url":"{}"}}"#, url(n));
            files.insert(url(&asset), payload.to_vec());
            files.insert(url("SHA256SUMS.txt"), sums.clone().into_bytes());
            files.insert(url("SHA256SUMS.txt.sig"), sig.clone().into_bytes());
            format!(
                r#"[{{"tag_name":"{tag}","html_url":"https://github.com/{REPO}/releases/tag/{tag}","body":"- Faster rendering","assets":[{},{},{}]}}]"#,
                entry(&asset, payload.len()),
                entry("SHA256SUMS.txt", sums.len()),
                entry("SHA256SUMS.txt.sig", sig.len())
            )
        }
    };
    files.insert(format!("https://api.github.com/repos/{REPO}/releases?per_page=20"), list.into_bytes());
    drop(files);
    Arc::new(fake)
}

fn harness(source: Arc<Fake>, kind: InstallKind, settings: &str) -> Harness<'static, PdfCraftApp> {
    let settings = settings.to_string();
    Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.restore(&settings);
        app.set_updater(Updater::new(info(), source.clone()).with_install_kind(kind.clone()));
        app
    })
}

/// Run frames until the updater is idle (or give up).
fn settle(h: &mut Harness<'static, PdfCraftApp>) {
    for _ in 0..400 {
        h.run_steps(2);
        if !h.state().updater().is_some_and(Updater::busy) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h.run_steps(3);
}

const NO_AUTO: &str = r#"{"updates":{"enabled":false}}"#;

#[test]
fn versions_compare_by_number() {
    assert!(is_newer("v0.2.0", "0.1.1"));
    assert!(is_newer("v0.1.10", "0.1.9"));
    assert!(is_newer("1", "0.9.9"));
    assert!(!is_newer("v0.1.1", "0.1.1"));
    assert!(!is_newer("v0.1.0", "0.1.1"));
    assert!(!is_newer("v0.1.1-beta.2", "0.1.1"), "a pre-release comes before its release");
    assert!(!is_newer("nightly", "0.1.1"), "a tag that isn't a version is never newer");
    assert!(!is_newer("v1.2.3.4", "0.1.1"));
    assert!(!is_newer("v99999999999999999999.0.0", "0.1.1"), "out of range");
}

#[test]
fn a_newer_release_downloads_verifies_and_waits_for_quit() {
    if craft_update::Arch::current() == craft_update::Arch::X86 {
        return; // No 32-bit Linux AppImage exists to update to.
    }
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("PdfCraft.AppImage");
    std::fs::write(&target, b"old").unwrap();
    let mut h = harness(github(Some("v99.0.0"), b"new"), InstallKind::AppImage(target.clone()), NO_AUTO);
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    h.get_by_label_contains("PdfCraft 99.0.0 is available");
    h.get_by_label_contains("Faster rendering");
    h.get_by_label("Download and install").click();
    settle(&mut h);
    h.get_by_label_contains("PdfCraft 99.0.0 is ready to install");
    h.get_by_label("Restart now");
    h.get_by_label("Install when I quit").click();
    h.run_steps(3);
    assert!(h.query_by_label_contains("ready to install").is_none(), "the dialog closes");
    assert_eq!(std::fs::read(&target).unwrap(), b"old", "nothing is installed before quitting");
}

#[test]
fn without_an_installer_the_release_page_is_offered() {
    let mut h = harness(github(Some("v99.0.0"), b"new"), InstallKind::WindowsPortable, NO_AUTO);
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    h.get_by_label_contains("PdfCraft 99.0.0 is available");
    h.get_by_label_contains("portable copy");
    assert!(h.query_by_label("Download and install").is_none());
    h.get_by_label("Open release page").click();
    h.run_steps(3);
    assert_eq!(h.state().last_opened_url.as_deref(), Some("https://github.com/treeforjs/pdfcraft/releases/tag/v99.0.0"));
}

#[test]
fn an_up_to_date_or_failed_check_says_so() {
    let mut h = harness(github(None, b""), InstallKind::WindowsMsi, NO_AUTO);
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    h.get_by_label_contains("is up to date");
    assert!(h.query_by_label("Download and install").is_none());

    let mut h = harness(Arc::new(Fake::default()), InstallKind::WindowsMsi, NO_AUTO);
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    h.get_by_label_contains("Couldn't update:");
    h.get_by_label_contains("couldn't reach GitHub");
}

#[test]
fn skipping_a_version_closes_and_remembers_it() {
    let mut h = harness(github(Some("v99.0.0"), b"new"), InstallKind::WindowsMsi, NO_AUTO);
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    h.get_by_label("Skip this version").click();
    h.run_steps(3);
    assert!(h.query_by_label_contains("is available").is_none());
    let saved: serde_json::Value = serde_json::from_str(&h.state().persist()).unwrap();
    assert_eq!(saved["updates"]["skipped"], "99.0.0");
    assert_eq!(saved["updates"]["enabled"], false);
}

#[test]
fn the_daily_check_waits_for_startup_and_only_notifies() {
    let source = github(Some("v99.0.0"), b"new");
    let mut h = harness(source.clone(), InstallKind::WindowsMsi, "{}");
    h.run_steps(5);
    assert_eq!(source.asked.load(std::sync::atomic::Ordering::SeqCst), 0, "nothing is asked while the app starts");
    // Let the start-up delay pass on egui's clock.
    for _ in 0..40 {
        h.input_mut().time = Some(h.input().time.unwrap_or(0.0) + 1.0);
        h.run_steps(1);
        if source.asked.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            break;
        }
    }
    settle(&mut h);
    assert_eq!(source.asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    // A notice, never a dialog over the user's work.
    h.get_by_label_contains("PdfCraft 99.0.0 is available. Choose Help ▸ Check for updates");
    assert!(h.query_by_label("Download and install").is_none());
    let saved: serde_json::Value = serde_json::from_str(&h.state().persist()).unwrap();
    assert!(saved["updates"]["last_checked"].as_u64().is_some());
    assert!(matches!(h.state().updater().map(Updater::state), Some(State::Available(_))));
}

#[test]
fn a_recent_or_disabled_check_doesnt_run_again() {
    let now = craft_update::AutoCheck::now();
    for settings in [NO_AUTO.to_string(), format!(r#"{{"updates":{{"last_checked":{now}}}}}"#)] {
        let source = github(Some("v99.0.0"), b"new");
        let mut h = harness(source.clone(), InstallKind::WindowsMsi, &settings);
        for _ in 0..20 {
            h.input_mut().time = Some(h.input().time.unwrap_or(0.0) + 1.0);
            h.run_steps(1);
        }
        assert_eq!(source.asked.load(std::sync::atomic::Ordering::SeqCst), 0, "{settings}");
    }
}
