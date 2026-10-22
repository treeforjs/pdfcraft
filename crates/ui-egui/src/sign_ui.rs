//! Use a certificate (execution plan M9.6): Digitally sign and Certify (drag a rectangle, or
//! click an empty signature field), the Sign with a Digital ID / Configure New Digital ID /
//! Sign as dialogs, the Signatures panel, and the signature message bar.

use std::path::PathBuf;

use egui::{Align, Color32, CornerRadius, Layout, Pos2, Rect, Stroke, vec2};
use pdfcraft_engine::sign::{self, Appearance, Certificate, DigitalId, Modification, Name, PrivateKey};
use pdfcraft_engine::{SignOptions, SignatureInfo, SignatureStatus};

use crate::canvas::{DocView, PageXform};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons, widgets};

/// A digital ID the app knows about (Acrobat: Digital ID files). The file stays where it is;
/// its password is asked for at each signing.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DigitalIdEntry {
    pub path: String,
    pub name: String,
    pub issuer: String,
    pub email: String,
    pub expires: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignStep {
    /// Sign with a Digital ID: pick one.
    Choose,
    /// Configure a Digital ID for signing: from a file or a new self-signed one.
    Configure,
    /// Sign as "Name": appearance, password, Sign.
    SignAs,
}

/// The key algorithms of a new digital ID (Acrobat offers RSA; P-256 also works in browsers).
pub const KEY_ALGORITHMS: [(&str, &str); 4] =
    [("2048-bit RSA", "rsa2048"), ("3072-bit RSA", "rsa3072"), ("4096-bit RSA", "rsa4096"), ("256-bit ECDSA (P-256)", "p256")];

#[derive(Clone, Debug, PartialEq)]
pub struct NewIdDraft {
    /// `true`: create a self-signed ID; `false`: use an ID from a file.
    pub create: bool,
    pub name: String,
    pub unit: String,
    pub organization: String,
    pub email: String,
    pub country: String,
    pub key: usize,
    pub password: String,
    pub confirm: String,
    /// Use a Digital ID from a file: its path (in the browser, the picked file's name) and
    /// password.
    pub file: String,
    pub file_password: String,
    /// The picked file's bytes, in the browser (which gives no paths).
    pub file_bytes: Option<crate::picked_ids::IdFileBytes>,
}

impl Default for NewIdDraft {
    fn default() -> Self {
        Self {
            create: true,
            name: String::new(),
            unit: String::new(),
            organization: String::new(),
            email: String::new(),
            country: String::new(),
            key: 0,
            password: String::new(),
            confirm: String::new(),
            file: String::new(),
            file_password: String::new(),
            file_bytes: None,
        }
    }
}

/// The signing dialogs' state.
#[derive(Clone, Debug, PartialEq)]
pub struct SignDraft {
    pub page: usize,
    /// The signature rectangle (user space); `None`: invisible.
    pub rect: Option<[f64; 4]>,
    /// Sign this empty signature field instead.
    pub field: Option<String>,
    /// Certify (DocMDP) with these permissions: 1 none, 2 form fill and signing, 3 also comments.
    pub certify: Option<u8>,
    pub step: SignStep,
    pub selected: Option<usize>,
    pub password: String,
    pub reason: String,
    pub location: String,
    pub appearance: Appearance,
    pub new_id: NewIdDraft,
    pub error: Option<String>,
    pub timestamp_server: Option<usize>,
    pub timestamp_network: bool,
}

impl SignDraft {
    fn new(page: usize, rect: Option<[f64; 4]>, field: Option<String>, certify: Option<u8>, ids: usize) -> Self {
        Self {
            page,
            rect,
            field,
            certify,
            step: if ids == 0 { SignStep::Configure } else { SignStep::Choose },
            selected: (ids > 0).then_some(0),
            password: String::new(),
            reason: String::new(),
            location: String::new(),
            appearance: Appearance::default(),
            new_id: NewIdDraft::default(),
            error: None,
            timestamp_server: None,
            timestamp_network: false,
        }
    }
}

/// What a page asks of the signing flow.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SignView {
    /// A signature rectangle being dragged: (page, start on screen).
    pub drag: Option<(usize, Pos2)>,
    /// A rectangle was drawn: (page, user-space rect).
    pub drawn: Option<(usize, [f64; 4])>,
    /// An empty signature field was clicked (its name).
    pub field: Option<String>,
}

/// Drag a signature rectangle on one page (Digitally sign, Certify (visible signature)).
pub(crate) fn page_input(ui: &egui::Ui, resp: &egui::Response, xf: &PageXform, page: usize, info: &pdfcraft_render::DocInfo, view: &mut DocView) {
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let origin = ui.input(|i| i.pointer.press_origin());
    if pointer.is_some_and(|p| xf.rect.contains(p)) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if resp.drag_started()
        && let Some(o) = origin.filter(|o| xf.rect.contains(*o))
    {
        view.sign.drag = Some((page, o));
    }
    if let Some((p, start)) = view.sign.drag
        && p == page
        && let Some(end) = pointer
    {
        let end = Pos2::new(end.x.clamp(xf.rect.left(), xf.rect.right()), end.y.clamp(xf.rect.top(), xf.rect.bottom()));
        let r = Rect::from_two_pos(start, end);
        ui.painter().rect_filled(r, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(0x14, 0x73, 0xE6, 28));
        ui.painter().rect_stroke(r, CornerRadius::ZERO, Stroke::new(1.0, Color32::from_rgb(0x14, 0x73, 0xE6)), egui::StrokeKind::Middle);
        if resp.drag_stopped() {
            view.sign.drag = None;
            if r.width() >= 8.0 && r.height() >= 8.0 {
                let user = |q: Pos2| {
                    let (vx, vy) = xf.screen_to_view(q);
                    info.pages[page].view_to_user(vx, vy)
                };
                let (a, b) = (user(r.left_top()), user(r.right_bottom()));
                let rect = [a[0].min(b[0]) as f64, a[1].min(b[1]) as f64, a[0].max(b[0]) as f64, a[1].max(b[1]) as f64];
                view.sign.drawn = Some((page, rect));
            }
        }
    }
}

/// Where new digital IDs are saved: next to the recovery folder (`…/PdfCraft/Digital IDs`, or
/// `PdfCraftData/Digital IDs` in portable mode).
#[cfg(not(target_arch = "wasm32"))]
fn id_dir() -> Option<PathBuf> {
    crate::recovery::RecoveryStore::default_dir().and_then(|d| d.parent().map(|p| p.join("Digital IDs")))
}

/// Save a new digital ID as `<stem>.p12`, or `<stem> 2.p12`, … when the name is taken. The file
/// is always created fresh, never opened through a file or link already at the name (checking
/// first and then writing would let one be planted in between), and on Unix only its owner can
/// read it: it holds the private key.
#[cfg(not(target_arch = "wasm32"))]
fn save_new_id_file(dir: &std::path::Path, stem: &str, p12: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::{ErrorKind, Write};
    for i in 1..=10_000u32 {
        let path = dir.join(if i == 1 { format!("{stem}.p12") } else { format!("{stem} {i}.p12") });
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let file = match opts.open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            // Windows refuses `create_new` on a folder with "access denied"; the name is taken all
            // the same. A folder we can't write to, with nothing at the name, still fails here.
            Err(e) if e.kind() == ErrorKind::PermissionDenied && path.symlink_metadata().is_ok() => continue,
            Err(e) => return Err(e),
        };
        // The block closes the file before a failed one is removed (Windows can't remove an open file).
        let written = {
            let mut file = file;
            file.write_all(p12).and_then(|()| file.sync_all())
        };
        if let Err(e) = written {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        return Ok(path);
    }
    Err(std::io::Error::new(ErrorKind::AlreadyExists, "no free file name for the digital ID"))
}

/// A Keychain identity by its `keychain:` reference.
fn keychain_id(reference: &str) -> Result<DigitalId, String> {
    #[cfg(target_os = "macos")]
    return sign::keychain::find(reference).map_err(|e| e.to_string());
    #[cfg(not(target_os = "macos"))]
    Err(format!("{reference}: Keychain identities are only available on macOS"))
}

/// A Windows store identity by its `windows:` reference.
fn windows_id(reference: &str) -> Result<DigitalId, String> {
    #[cfg(target_os = "windows")]
    return sign::windows::find(reference).map_err(|e| e.to_string());
    #[cfg(not(target_os = "windows"))]
    Err(format!("{reference}: Windows certificate store identities are only available on Windows"))
}

pub fn entry_for(path: &str, c: &Certificate) -> DigitalIdEntry {
    DigitalIdEntry {
        path: path.to_string(),
        name: c.display_name(),
        issuer: c.issuer.common_name().map(str::to_string).unwrap_or_else(|| c.issuer.display()),
        email: c.subject.email().unwrap_or("").to_string(),
        expires: format!("{:04}.{:02}.{:02}", c.not_after.year, c.not_after.month, c.not_after.day),
    }
}

impl PdfCraftApp {
    /// A new digital ID's fields, suggested from Preferences ▸ Identity.
    pub(crate) fn new_id_draft(&self) -> NewIdDraft {
        let id = &self.session.prefs().identity;
        NewIdDraft {
            name: id.name.clone(),
            organization: id.organization.clone(),
            unit: id.unit.clone(),
            email: id.email.clone(),
            // The browser makes P-256 keys only (RSA key generation needs aws-lc-rs, ADR-0009).
            key: if cfg!(target_arch = "wasm32") { 3 } else { 0 },
            ..NewIdDraft::default()
        }
    }

    /// Preferences ▸ Signatures and Identity: the reason and location a signature starts with,
    /// and the suggested fields of a new digital ID.
    fn prefill_sign_draft(&self, draft: &mut SignDraft) {
        let p = self.session.prefs();
        draft.reason = p.signing_reason.clone();
        draft.location = p.signing_location.clone();
        draft.new_id = self.new_id_draft();
    }

    /// Start signing: the rectangle (or field) is known; show Sign with a Digital ID.
    pub fn start_signing(&mut self, page: usize, rect: Option<[f64; 4]>, field: Option<String>, certify: Option<u8>) {
        self.refresh_os_key_store_ids();
        let mut draft = SignDraft::new(page, rect, field, certify, self.digital_ids.len());
        self.prefill_sign_draft(&mut draft);
        self.sign_draft = Some(draft);
        self.dialog = Some(crate::Dialog::Sign);
    }

    /// List OS key store signing identities (after the file-based IDs).
    fn refresh_os_key_store_ids(&mut self) {
        self.digital_ids.retain(|e| !e.path.starts_with("keychain:") && !e.path.starts_with("windows:"));
        #[cfg(target_os = "macos")]
        if self.os_key_store_ids {
            match sign::keychain::identities(None) {
                Ok(ids) => {
                    for id in ids {
                        self.digital_ids.push(entry_for(&sign::keychain::reference(&id.certificate), &id.certificate));
                    }
                }
                Err(e) => self.notify_fmt("The Keychain's digital IDs couldn't be listed: {e}", &[("e", &e.to_string())]),
            }
        }
        #[cfg(target_os = "windows")]
        if self.os_key_store_ids {
            match sign::windows::identities() {
                Ok(ids) => {
                    for id in ids {
                        self.digital_ids.push(entry_for(&sign::windows::reference(&id.certificate), &id.certificate));
                    }
                }
                Err(e) => self.notify_fmt("The Windows store's digital IDs couldn't be listed: {e}", &[("e", &e.to_string())]),
            }
        }
    }

    /// Add a digital ID file to the list (or select it if it's there).
    pub fn add_digital_id(&mut self, path: &str, cert: &Certificate) -> usize {
        if let Some(i) = self.digital_ids.iter().position(|e| e.path == path) {
            self.digital_ids[i] = entry_for(path, cert);
            return i;
        }
        self.digital_ids.push(entry_for(path, cert));
        self.digital_ids.len() - 1
    }

    /// Configure New Digital ID ▸ Create: a self-signed ID saved as a .p12 next to the app data
    /// (or in `export_dir_override`). Returns its list index.
    fn create_digital_id(&mut self) -> Result<usize, String> {
        let Some(d) = self.sign_draft.as_ref().map(|d| d.new_id.clone()) else { return Err("nothing to create".into()) };
        if d.name.trim().is_empty() {
            return Err("Enter a name.".into());
        }
        if d.password.chars().count() < 6 {
            return Err("The password must have at least 6 characters.".into());
        }
        if d.password != d.confirm {
            return Err("The passwords do not match.".into());
        }
        let key = match KEY_ALGORITHMS[d.key.min(KEY_ALGORITHMS.len() - 1)].1 {
            "rsa3072" => PrivateKey::generate_rsa(3072),
            "rsa4096" => PrivateKey::generate_rsa(4096),
            "p256" => PrivateKey::generate_p256(),
            _ => PrivateKey::generate_rsa(2048),
        }
        .map_err(|e| e.to_string())?;
        let now = self.session.now_secs();
        let dn = Name::build(&d.name, &d.unit, &d.organization, &d.email, &d.country);
        let serial = sign::keys::DigestAlg::Sha256.digest(&[d.name.as_bytes(), &now.to_be_bytes()])[..8].to_vec();
        let cert = Certificate::self_signed(&dn, &key, sign::Time::from_unix(now), 5, &serial).map_err(|e| e.to_string())?;
        let id = DigitalId { key, certificate: cert.clone(), chain: Vec::new(), friendly_name: Some(d.name.trim().to_string()) };
        let p12 = sign::pkcs12::write(&id, &d.password).map_err(|e| e.to_string())?;
        let stem: String = d.name.trim().chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
        self.store_new_id(&stem, p12, &cert)
    }

    /// Save a new digital ID's file next to the app data (or in `export_dir_override`).
    #[cfg(not(target_arch = "wasm32"))]
    fn store_new_id(&mut self, stem: &str, p12: Vec<u8>, cert: &Certificate) -> Result<usize, String> {
        let dir = match &self.export_dir_override {
            Some(p) => Some(PathBuf::from(p)),
            None => id_dir(),
        }
        .ok_or("There is no folder to save the digital ID in.")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = save_new_id_file(&dir, stem, &p12).map_err(|e| e.to_string())?;
        Ok(self.add_digital_id(&path.to_string_lossy(), cert))
    }

    /// In the browser a new digital ID is held for the page and offered as a download, which is
    /// how the user keeps it (a page can't save files of its own).
    #[cfg(target_arch = "wasm32")]
    fn store_new_id(&mut self, stem: &str, p12: Vec<u8>, cert: &Certificate) -> Result<usize, String> {
        let name = format!("{stem}.p12");
        let p12 = std::sync::Arc::new(p12);
        let i = self.hold_digital_id(&name, p12.clone(), cert);
        match crate::editing::download_typed(&name, &p12, "application/x-pkcs12") {
            Ok(()) => self.notify_fmt("Downloaded the new digital ID as {name}. Keep it: this page forgets it when it closes.", &[("name", &name)]),
            Err(e) => self.notify_fmt("Couldn't download {name}: {e}", &[("name", &name), ("e", &e)]),
        }
        Ok(i)
    }

    /// Configure New Digital ID ▸ Use a Digital ID from a file.
    fn import_digital_id(&mut self) -> Result<usize, String> {
        let Some(d) = self.sign_draft.as_ref().map(|d| d.new_id.clone()) else { return Err("nothing to import".into()) };
        // A file picked in the browser: its bytes, held for the page.
        if let Some(picked) = d.file_bytes {
            return self.add_digital_id_bytes(&d.file, picked.0, &d.file_password);
        }
        let bytes = std::fs::read(&d.file).map_err(|e| format!("{}: {e}", d.file))?;
        let id = sign::pkcs12::open(&bytes, &d.file_password).map_err(|e| match e {
            sign::SignError::WrongPassword => "The password is incorrect.".to_string(),
            e => e.to_string(),
        })?;
        Ok(self.add_digital_id(&d.file, &id.certificate))
    }

    /// Sign as …: open the ID, sign, save the signed file (Save As), and show it.
    fn finish_signing(&mut self) -> Result<(), String> {
        // What's typed in a form field is signed with the document (#166). A rejected value keeps
        // the dialog open; the notice says why.
        if !self.commit_form_typing() {
            return Err(String::new());
        }
        let Some((_, doc_id)) = self.active_ids() else { return Err("no document".into()) };
        let d = self.sign_draft.clone().ok_or("nothing to sign")?;
        let entry = d.selected.and_then(|i| self.digital_ids.get(i)).cloned().ok_or("Choose a digital ID.")?;
        let id = if entry.path.starts_with("keychain:") {
            keychain_id(&entry.path)?
        } else if entry.path.starts_with("windows:") {
            windows_id(&entry.path)?
        } else if crate::picked_ids::is_picked(&entry.path) {
            self.open_picked_id(&entry.path, &d.password)?
        } else {
            let bytes = std::fs::read(&entry.path).map_err(|e| format!("{}: {e}", entry.path))?;
            sign::pkcs12::open(&bytes, &d.password).map_err(|e| match e {
                sign::SignError::WrongPassword => "The password is incorrect.".to_string(),
                e => e.to_string(),
            })?
        };
        let some = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
        let opts = SignOptions {
            field: d.field.clone(),
            page: d.page,
            rect: d.rect,
            reason: some(&d.reason),
            location: some(&d.location),
            certify: d.certify,
            appearance: d.appearance.clone(),
            ..SignOptions::default()
        };
        let cancellation = pdfcraft_engine::signature_workflow::SignatureCancellation::default();
        let authority = self.signature_authority(d.timestamp_server, d.timestamp_network, cancellation.clone())?;
        // The password has opened the ID; it isn't kept (a retry asks for it again).
        if let Some(d) = self.sign_draft.as_mut() {
            d.password.clear();
        }
        let name = self.session.get(doc_id).map(|d| d.name.clone()).unwrap_or_default();
        let stem = name.trim_end_matches(".pdf").trim_end_matches(".PDF").to_string();
        // In the browser the signed file is a download.
        #[cfg(target_arch = "wasm32")]
        if self.save_override.is_none() {
            if authority.is_some() {
                return Err(
                    "Timestamping a signature with a server isn't available in the browser yet. Sign without a timestamp, or use the desktop app."
                        .into(),
                );
            }
            let signed = self.sign_in_memory(doc_id, &id, opts)?;
            let file = format!("{stem}_signed.pdf");
            match crate::editing::download(&file, &signed) {
                Ok(()) => self.notify_fmt("Signed. Downloaded {name}", &[("name", &file)]),
                Err(e) => self.notify_fmt("Couldn't download {name}: {e}", &[("name", &file), ("e", &e)]),
            }
            return Ok(());
        }
        // Signing saves, as in Acrobat: choose where (a cancelled save cancels signing). The
        // document is signed once the user has chosen.
        let sign_and_save = move |app: &mut Self, path: PathBuf| -> Result<(), String> {
            // Sign what was typed while the save panel was open, too (#166); a refused value
            // has said why already.
            if !app.commit_typing_in(doc_id) {
                return Err(String::new());
            }
            if let Some(authority) = authority {
                let job = app.session.signature_job(doc_id).map_err(|e| e.to_string())?;
                return app.spawn_signature_job(doc_id, path, cancellation, move || {
                    job.sign(&id, opts, Some(&authority)).map(|r| (r, Vec::new())).map_err(|e| e.to_string())
                });
            }
            let signed = app.session.sign(doc_id, &id, opts).map_err(|e| e.to_string())?;
            crate::editing::write_atomically(&path.to_string_lossy(), signed.as_slice()).map_err(|e| format!("Could not save: {e}"))?;
            app.session.mark_signed(doc_id, signed, Some(path.to_string_lossy().into_owned())).map_err(|e| e.to_string())?;
            if let Some(view) = app.views.iter_mut().find(|v| v.id == doc_id) {
                view.invalidate_content();
            }
            app.right = Some(crate::RightPanel::Signatures);
            app.notify_fmt("Signed and saved to {path}", &[("path", &path.display().to_string())]);
            Ok(())
        };
        match self.save_override.clone() {
            Some(p) => sign_and_save(self, PathBuf::from(p)),
            #[cfg(not(target_arch = "wasm32"))]
            None => {
                let dialog = rfd::AsyncFileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(format!("{stem}_signed.pdf"));
                // The signature's page, rectangle and field refer to the document as it is now:
                // the pick is dropped (with a notice) if it is edited or switched away from
                // meanwhile. Once the picker shows, the dialog closes; an error from here on
                // arrives as a notice. If no picker could show, the dialog stays open.
                let asked = self.ask_one(crate::pickers::Ask::Save(dialog), Some(doc_id), move |app, path| {
                    if let Err(e) = sign_and_save(app, path)
                        && !e.is_empty()
                    {
                        app.notify(e);
                    }
                });
                if asked { Ok(()) } else { Err(String::new()) }
            }
            #[cfg(target_arch = "wasm32")]
            None => {
                let _ = (stem, sign_and_save);
                Err(String::new())
            }
        }
    }

    /// Sign the document's current state with `id` and make the signed file the document (as a
    /// save does), without writing it anywhere: the web app then offers it as a download.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn sign_in_memory(
        &mut self,
        doc_id: pdfcraft_engine::DocId,
        id: &DigitalId,
        opts: SignOptions,
    ) -> Result<std::sync::Arc<Vec<u8>>, String> {
        let signed = self.session.sign(doc_id, id, opts).map_err(|e| e.to_string())?;
        self.session.mark_signed(doc_id, signed.clone(), None).map_err(|e| e.to_string())?;
        if let Some(view) = self.views.iter_mut().find(|v| v.id == doc_id) {
            view.invalidate_content();
        }
        self.right = Some(crate::RightPanel::Signatures);
        Ok(signed)
    }

    /// Trust a certificate (Signatures panel ▸ Add to trusted certificates), revalidating.
    pub fn trust_certificate(&mut self, cert: Certificate) {
        let mut certs = self.session.trusted_certificates().to_vec();
        if !certs.iter().any(|c| c.raw == cert.raw) {
            certs.push(cert);
        }
        self.session.set_trusted_certificates(certs);
    }

    /// Open saved revision `n` (1 = the oldest) of the active document as a new document.
    pub fn open_revision(&mut self, n: usize) {
        let Some((_, id)) = self.active_ids() else { return };
        match self.session.open_revision(id, n) {
            Ok(new) => {
                let Some(doc) = self.session.get(new) else { return };
                self.views.push(DocView::new(new, &doc.info, self.view_defaults));
                self.active = Some(self.views.len() - 1);
            }
            Err(e) => self.notify_fmt("Couldn't open revision {n}: {e}", &[("n", &n.to_string()), ("e", &e.to_string())]),
        }
    }

    /// Signature Properties for a signature field of the active document.
    pub fn open_signature_properties(&mut self, field: &str) -> Result<(), String> {
        let Some((_, id)) = self.active_ids() else { return Err("No document is open.".into()) };
        let known = self.session.get(id).is_some_and(|d| d.signatures.iter().any(|s| s.field == field));
        if !known {
            return Err(format!("No signature field named {field:?}."));
        }
        self.sig_props = Some(SigProps { field: field.to_string(), tab: SigPropsTab::Summary });
        self.dialog = Some(crate::Dialog::SignatureProperties);
        Ok(())
    }

    /// Open the signed version of a signature as a new document (View signed version).
    pub fn view_signed_version(&mut self, len: usize) {
        self.view_saved_version(len, "signed version".into());
    }

    /// Open saved revision `revision` (the file's first `end` bytes) as a new document.
    pub fn view_revision(&mut self, revision: usize, end: usize) {
        self.view_saved_version(end, format!("revision {revision}"));
    }

    /// Open the active document's first `len` bytes (a saved revision) as "<name> (<what>)".
    fn view_saved_version(&mut self, len: usize, what: String) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let bytes = doc.bytes[..len.min(doc.bytes.len())].to_vec();
        let name = format!("{} ({what})", doc.name.trim_end_matches(".pdf"));
        match self.session.open(format!("{name}.pdf"), None, std::sync::Arc::new(bytes), doc.password.clone().as_deref()) {
            Ok(new) => {
                let Some(doc) = self.session.get(new) else { return };
                self.views.push(DocView::new(new, &doc.info, self.view_defaults));
                self.active = Some(self.views.len() - 1);
            }
            Err(e) => self.notify_fmt("Couldn't open the signed version: {e}", &[("e", &e.to_string())]),
        }
    }
}

/// Draw the signing dialogs; returns `true` when the dialog should close.
pub(crate) fn dialog(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let Some(step) = app.sign_draft.as_ref().map(|d| d.step) else { return true };
    match step {
        SignStep::Choose => choose(ui, app, t),
        SignStep::Configure => configure(ui, app, t),
        SignStep::SignAs => sign_as(ui, app, t),
    }
}

fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(tl!(text)).font(theme::semibold(18.0)));
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(8.0);
}

fn error(ui: &mut egui::Ui, err: &Option<String>) {
    if let Some(e) = err.as_ref().filter(|e| !e.is_empty()) {
        ui.colored_label(Color32::from_rgb(0xD7, 0x37, 0x3F), e);
        ui.add_space(4.0);
    }
}

fn choose(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    title(ui, "Sign with a Digital ID");
    ui.label(tl!("Choose the digital ID that you want to use for signing:"));
    ui.add_space(6.0);
    let ids = app.digital_ids.clone();
    let Some(d) = app.sign_draft.as_mut() else { return true };
    let mut close = false;
    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
        for (i, e) in ids.iter().enumerate() {
            let selected = d.selected == Some(i);
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), egui::Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, &e.name));
            let fill = if selected {
                t.accent_soft
            } else if resp.hovered() {
                t.hover
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
            icons::paint(ui, Rect::from_min_size(rect.min + vec2(10.0, 15.0), vec2(20.0, 20.0)), "badge-check", 18.0, t.accent);
            ui.painter().text(rect.min + vec2(40.0, 9.0), egui::Align2::LEFT_TOP, &e.name, theme::semibold(13.0), t.text);
            let sub = format!(
                "{keychain}{email}{issued}{issuer}{expires}{date}",
                keychain = if e.path.starts_with("keychain:") {
                    tl!("Keychain  ·  ").to_string()
                } else if e.path.starts_with("windows:") {
                    tl!("Windows store  ·  ").to_string()
                } else if crate::picked_ids::is_picked(&e.path) {
                    tl!("This page only  ·  ").to_string()
                } else {
                    String::new()
                },
                email = if e.email.is_empty() { String::new() } else { format!("{}  ·  ", e.email) },
                issued = tl!("Issued by: "),
                issuer = e.issuer,
                expires = tl!(", Expires: "),
                date = e.expires,
            );
            ui.painter().text(rect.min + vec2(40.0, 28.0), egui::Align2::LEFT_TOP, sub, theme::regular(11.5), t.text_muted);
            if resp.clicked() {
                d.selected = Some(i);
            }
            if resp.double_clicked() {
                d.step = SignStep::SignAs;
            }
        }
        if ids.is_empty() {
            ui.label(egui::RichText::new(tl!("No digital IDs yet.")).color(t.text_muted));
        }
    });
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if widgets::pill_button(ui, tl!("Configure New Digital ID"), false).clicked() {
            d.step = SignStep::Configure;
            d.error = None;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add_enabled_ui(d.selected.is_some(), |ui| widgets::pill_button(ui, tl!("Continue"), true)).inner.clicked() {
                d.step = SignStep::SignAs;
                d.error = None;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                close = true;
            }
        });
    });
    close
}

fn configure(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    title(ui, "Configure a Digital ID for Signing");
    let Some(d) = app.sign_draft.as_mut() else { return true };
    let mut close = false;
    let mut go = false;
    let mut browse = false;
    ui.radio_value(&mut d.new_id.create, false, tl!("Use a Digital ID from a file"));
    ui.radio_value(&mut d.new_id.create, true, tl!("Create a new Digital ID (self-signed, saved to a password-protected file)"));
    ui.add_space(8.0);
    let n = &mut d.new_id;
    if n.create {
        egui::Grid::new("new-id").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            for (label, value) in [
                (tl!("Name"), &mut n.name),
                (tl!("Organizational Unit"), &mut n.unit),
                (tl!("Organization Name"), &mut n.organization),
                (tl!("Email Address"), &mut n.email),
            ] {
                let l = ui.label(label);
                ui.add(egui::TextEdit::singleline(value).desired_width(260.0)).labelled_by(l.id);
                ui.end_row();
            }
            let l = ui.label(tl!("Country/Region"));
            ui.add(egui::TextEdit::singleline(&mut n.country).hint_text("US").char_limit(2).desired_width(60.0)).labelled_by(l.id);
            ui.end_row();
            ui.label(tl!("Key Algorithm"));
            egui::ComboBox::from_id_salt("key-alg").selected_text(KEY_ALGORITHMS[n.key].0).show_ui(ui, |ui| {
                for (i, (label, _)) in KEY_ALGORITHMS.iter().enumerate() {
                    ui.selectable_value(&mut n.key, i, *label);
                }
            });
            ui.end_row();
            let l = ui.label(tl!("Password"));
            ui.add(egui::TextEdit::singleline(&mut n.password).password(true).desired_width(200.0)).labelled_by(l.id);
            ui.end_row();
            let l = ui.label(tl!("Confirm Password"));
            ui.add(egui::TextEdit::singleline(&mut n.confirm).password(true).desired_width(200.0)).labelled_by(l.id);
            ui.end_row();
        });
        ui.label(
            egui::RichText::new(tl!("Valid for 5 years, for digital signatures. Self-signed IDs are usually not trusted by others."))
                .small()
                .color(t.text_muted),
        );
    } else {
        egui::Grid::new("id-file").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            let l = ui.label(tl!("File (.p12, .pfx)"));
            ui.horizontal(|ui| {
                // The browser gives no paths: the picked file's name shows, read-only.
                let typed = !cfg!(target_arch = "wasm32");
                ui.add(egui::TextEdit::singleline(&mut n.file).interactive(typed).desired_width(220.0)).labelled_by(l.id);
                if ui.button(tl!("Browse…")).clicked() {
                    browse = true;
                }
            });
            ui.end_row();
            let l = ui.label(tl!("Password"));
            ui.add(egui::TextEdit::singleline(&mut n.file_password).password(true).desired_width(200.0)).labelled_by(l.id);
            ui.end_row();
        });
        if cfg!(target_arch = "wasm32") {
            ui.label(
                egui::RichText::new(tl!(
                    "The file stays on this device: PdfCraft reads it in this page and forgets it when the page closes. The password is never stored."
                ))
                .small()
                .color(t.text_muted),
            );
        }
    }
    ui.add_space(6.0);
    error(ui, &d.error);
    ui.horizontal(|ui| {
        if !app.digital_ids.is_empty() && widgets::pill_button(ui, tl!("Back"), false).clicked() {
            d.step = SignStep::Choose;
            d.error = None;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, if d.new_id.create { tl!("Save") } else { tl!("Continue") }, true).clicked() {
                go = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                close = true;
            }
        });
    });
    if go {
        let create = app.sign_draft.as_ref().is_some_and(|d| d.new_id.create);
        let result = if create { app.create_digital_id() } else { app.import_digital_id() };
        let fresh = app.new_id_draft();
        let Some(d) = app.sign_draft.as_mut() else { return true };
        match result {
            Ok(i) => {
                d.selected = Some(i);
                d.step = SignStep::Choose;
                d.error = None;
                d.new_id = fresh;
            }
            Err(e) => d.error = Some(e),
        }
    }
    // The picker answers on a later frame, into the draft if the same dialog is still open.
    #[cfg(not(target_arch = "wasm32"))]
    if browse {
        let dialog = rfd::AsyncFileDialog::new().add_filter(tl!("Digital ID"), &["p12", "pfx"]);
        let epoch = app.dialog_epoch();
        app.ask_one(crate::pickers::Ask::File(dialog), None, move |app, p| {
            // Not one closed meanwhile, or opened again since.
            if app.dialog_epoch() == epoch
                && let Some(d) = app.sign_draft.as_mut()
            {
                d.new_id.file = p.to_string_lossy().into_owned();
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    if browse {
        app.pick_digital_id_file();
    }
    close
}

/// The appearance preview: Acrobat's standard layout (name left, details right).
fn preview(ui: &mut egui::Ui, t: &Tokens, name: &str, d: &SignDraft) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 96.0), egui::Sense::hover());
    let bg = if t.dark() { Color32::from_gray(0xF4) } else { Color32::WHITE };
    ui.painter().rect(rect, CornerRadius::same(4), bg, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let inner = rect.shrink(10.0);
    let a = &d.appearance;
    if a.name {
        let half = Rect::from_min_size(inner.min, vec2(inner.width() * 0.5 - 6.0, inner.height()));
        let galley = ui.painter().layout(name.to_string(), theme::regular(22.0), Color32::BLACK, half.width());
        ui.painter().galley(half.left_center() - vec2(0.0, galley.size().y / 2.0), galley, Color32::BLACK);
    }
    let mut lines = Vec::new();
    if a.name {
        lines.push(if a.labels { format!("Digitally signed by {name}") } else { name.to_string() });
    }
    if a.reason && !d.reason.trim().is_empty() {
        lines.push(format!("{}{}", if a.labels { "Reason: " } else { "" }, d.reason.trim()));
    }
    if a.location && !d.location.trim().is_empty() {
        lines.push(format!("{}{}", if a.labels { "Location: " } else { "" }, d.location.trim()));
    }
    if a.date {
        lines.push(format!("{}{}", if a.labels { "Date: " } else { "" }, "(the signing time)"));
    }
    let x = if a.name { inner.center().x + 6.0 } else { inner.left() };
    let galley = ui.painter().layout(lines.join("\n"), theme::regular(11.0), Color32::from_gray(0x20), inner.right() - x);
    ui.painter().galley(egui::pos2(x, inner.center().y - galley.size().y / 2.0), galley, Color32::BLACK);
}

fn sign_as(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let ids = app.digital_ids.clone();
    let Some(d) = app.sign_draft.as_mut() else { return true };
    let Some(entry) = d.selected.and_then(|i| ids.get(i)).cloned() else {
        d.step = SignStep::Choose;
        return false;
    };
    title(
        ui,
        &crate::i18n::fmt(
            tl!("{verb} as \"{name}\""),
            &[("verb", if d.certify.is_some() { tl!("Certify") } else { tl!("Sign") }), ("name", &entry.name)],
        ),
    );
    let in_os_key_store = entry.path.starts_with("keychain:") || entry.path.starts_with("windows:");
    let mut close = false;
    let mut go = false;
    if d.rect.is_some() || d.field.is_some() {
        ui.label(egui::RichText::new(tl!("Appearance")).font(theme::semibold(12.5)));
        preview(ui, t, &entry.name, d);
        ui.horizontal_wrapped(|ui| {
            let a = &mut d.appearance;
            ui.checkbox(&mut a.name, tl!("Name"));
            ui.checkbox(&mut a.date, tl!("Date"));
            ui.checkbox(&mut a.reason, tl!("Reason"));
            ui.checkbox(&mut a.location, tl!("Location"));
            ui.checkbox(&mut a.distinguished_name, tl!("Distinguished name"));
            ui.checkbox(&mut a.labels, tl!("Labels"));
        });
        ui.add_space(6.0);
    } else {
        ui.label(egui::RichText::new(tl!("The signature will be invisible.")).color(t.text_muted));
    }
    egui::Grid::new("sign-as").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        if d.certify.is_some() {
            ui.label(tl!("Permitted actions after certifying"));
            let mut p = d.certify.unwrap_or(2);
            let label = |p: u8| match p {
                1 => tl!("No changes allowed"),
                3 => tl!("Annotations, form fill-in, and digital signatures"),
                _ => tl!("Form fill-in and digital signatures"),
            };
            egui::ComboBox::from_id_salt("certify-p").selected_text(label(p)).width(300.0).show_ui(ui, |ui| {
                for v in [1u8, 2, 3] {
                    ui.selectable_value(&mut p, v, label(v));
                }
            });
            d.certify = Some(p);
            ui.end_row();
        }
        let l = ui.label(tl!("Reason"));
        ui.add(egui::TextEdit::singleline(&mut d.reason).hint_text(tl!("Optional")).desired_width(260.0)).labelled_by(l.id);
        ui.end_row();
        let l = ui.label(tl!("Location"));
        ui.add(egui::TextEdit::singleline(&mut d.location).hint_text(tl!("Optional")).desired_width(260.0)).labelled_by(l.id);
        ui.end_row();
        if in_os_key_store {
            ui.label("");
            ui.label(
                egui::RichText::new(if entry.path.starts_with("windows:") {
                    tl!("The key is in the Windows certificate store, which may ask to allow PdfCraft to use it.")
                } else {
                    tl!("The key is in the macOS Keychain, which may ask to allow PdfCraft to use it.")
                })
                .small()
                .color(t.text_muted),
            );
        } else {
            let l = ui.label(tl!("Digital ID password"));
            let r = ui.add(egui::TextEdit::singleline(&mut d.password).password(true).desired_width(200.0)).labelled_by(l.id);
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go = true;
            }
        }
        ui.end_row();
    });
    ui.add_space(6.0);
    ui.label("Timestamp (optional)");
    crate::timestamp_ui::server_choice(ui, &app.timestamp_servers, &mut d.timestamp_server, true);
    if d.timestamp_server.is_some() {
        ui.checkbox(&mut d.timestamp_network, "Allow a timestamp request to this server");
    }
    ui.label("Configure servers in Use a certificate → Timestamp servers.");
    error(ui, &d.error);
    ui.horizontal(|ui| {
        if widgets::pill_button(ui, tl!("Back"), false).clicked() {
            d.step = SignStep::Choose;
            d.error = None;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("Sign"), true).clicked() {
                go = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                close = true;
            }
        });
    });
    if go {
        match app.finish_signing() {
            Ok(()) => {
                app.sign_draft = None;
                return true;
            }
            Err(e) => {
                if let Some(d) = app.sign_draft.as_mut() {
                    d.error = Some(e);
                }
            }
        }
    }
    close
}

/// The message bar for signed documents: (icon, colour, template, argument).
/// The template renders through [`crate::i18n::tr_fmt`] at the call site, so the bar follows
/// the UI language; names inside `argument` stay as they are.
pub(crate) fn banner(sigs: &[SignatureInfo]) -> Option<(&'static str, Color32, &'static str, String)> {
    let signed: Vec<&SignatureInfo> = sigs.iter().filter(|s| s.signed).collect();
    if signed.is_empty() {
        return None;
    }
    if let Some(c) = signed.iter().find(|s| s.certify.is_some())
        && c.status == SignatureStatus::Valid
    {
        let by = c.certificate.as_ref().map(|x| {
            let org = x.subject.organization().map(|o| format!(", {o}")).unwrap_or_default();
            format!("{}{org}, certificate issued by {}.", x.display_name(), x.issuer.common_name().unwrap_or("an unknown issuer"))
        });
        return Some(("badge-check", Color32::from_rgb(0x14, 0x73, 0xE6), "Certified by {by}", by.unwrap_or_default()));
    }
    Some(if signed.iter().any(|s| s.status == SignatureStatus::Invalid) {
        ("circle-x", Color32::from_rgb(0xD7, 0x37, 0x3F), "At least one signature is invalid.", String::new())
    } else if signed.iter().all(|s| s.status == SignatureStatus::Valid) {
        ("circle-check", Color32::from_rgb(0x2D, 0x9D, 0x78), "Signed and all signatures are valid.", String::new())
    } else {
        ("triangle-alert", Color32::from_rgb(0xE6, 0x86, 0x19), "At least one signature has problems.", String::new())
    })
}

fn status_icon(s: &SignatureInfo) -> (&'static str, Color32) {
    if !s.signed {
        return ("pen-line", Color32::from_gray(0x80));
    }
    match (s.status, &s.modification) {
        (SignatureStatus::Valid, Modification::None) => ("circle-check", Color32::from_rgb(0x2D, 0x9D, 0x78)),
        (SignatureStatus::Valid, _) => ("circle-check", Color32::from_rgb(0x2D, 0x9D, 0x78)),
        (SignatureStatus::Unknown, _) => ("triangle-alert", Color32::from_rgb(0xE6, 0x86, 0x19)),
        (SignatureStatus::Invalid, _) => ("circle-x", Color32::from_rgb(0xD7, 0x37, 0x3F)),
    }
}

/// What the Signatures panel asks the app to do.
#[derive(Clone, Debug)]
pub enum PanelAction {
    Validate,
    GoTo(usize),
    Trust(Box<Certificate>),
    ViewSigned(usize),
    Sign(String),
    ExportCertificate(Box<Certificate>),
    /// Certificate Viewer for the signer's chain (signer first).
    ViewCertificate(Vec<Certificate>),
    /// Signature Properties for the field.
    Properties(String),
    /// Open saved revision `revision` (the file's first `end` bytes) as a new document.
    ViewRevision {
        revision: usize,
        end: usize,
    },
}

/// How the Signatures panel names a kind of change (the kinds [`Modification`] lists).
fn change_label(kind: &str) -> &'static str {
    match kind {
        "comments" => tl!("Comments added or changed"),
        "form fill" => tl!("Form fields filled in"),
        "page content" => tl!("Page content changed"),
        "links" => tl!("Links changed"),
        "signature" => tl!("Signature added"),
        "document security store" => tl!("Validation data added"),
        "metadata" => tl!("Document properties changed"),
        "document structure" => tl!("Document structure changed"),
        "pages added or removed" => tl!("Pages added or removed"),
        _ => tl!("Other changes"),
    }
}

/// The panel's key for a revision node's expanded state (never a field name: those have no NUL).
fn revision_key(revision: usize) -> String {
    format!("\0rev{revision}")
}

/// A saved revision that no signature covers: what it changed, page by page.
fn revision_node(ui: &mut egui::Ui, t: &Tokens, r: &sign::RevisionChanges, expanded: &mut Vec<String>, action: &mut Option<PanelAction>) {
    let key = revision_key(r.revision);
    let open = expanded.contains(&key);
    let head = crate::i18n::fmt(tl!("Rev. {n}: Changes made after signing"), &[("n", &r.revision.to_string())]);
    let toggled = ui
        .horizontal(|ui| {
            let chevron = if open { "chevron-down" } else { "chevron-right" };
            let toggle = icons::button(ui, chevron, 20.0, false, if open { "Collapse" } else { "Expand" }).clicked();
            ui.add(icons::image("file-pen-line", 16.0, t.text_muted));
            let l = ui.add(egui::Label::new(egui::RichText::new(&head).font(theme::semibold(12.5)).color(t.text)).sense(egui::Sense::click()));
            toggle || l.clicked()
        })
        .inner;
    if toggled {
        if open {
            expanded.retain(|f| f != &key);
        } else {
            expanded.push(key);
        }
    }
    if !open {
        return;
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 44, right: 4, top: 2, bottom: 8 }).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        if r.changes.is_empty() {
            ui.label(egui::RichText::new(tl!("No changes were found in this revision.")).font(theme::regular(11.5)).color(t.text_muted));
        }
        for c in &r.changes {
            let what = change_label(&c.kind);
            match c.page {
                Some(p) => {
                    let text = crate::i18n::fmt(tl!("Page {p}: {change}"), &[("p", &(p + 1).to_string()), ("change", what)]);
                    let link = ui.add(
                        egui::Label::new(egui::RichText::new(text).font(theme::regular(11.5)).color(t.accent_text).underline())
                            .sense(egui::Sense::click()),
                    );
                    if link.clicked() {
                        *action = Some(PanelAction::GoTo(p));
                    }
                }
                None => {
                    ui.label(egui::RichText::new(crate::i18n::fmt(tl!("Document: {change}"), &[("change", what)])).font(theme::regular(11.5)));
                }
            }
        }
        ui.add_space(4.0);
        if ui.button(tl!("View this version")).clicked() {
            *action = Some(PanelAction::ViewRevision { revision: r.revision, end: r.end });
        }
    });
}

/// The Signatures panel body.
pub(crate) fn panel(
    ui: &mut egui::Ui,
    t: &Tokens,
    sigs: &[SignatureInfo],
    revisions: &[sign::RevisionChanges],
    expanded: &mut Vec<String>,
) -> Option<PanelAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        if widgets::pill_button(ui, tl!("Validate all"), false).clicked() {
            action = Some(PanelAction::Validate);
        }
    });
    ui.add_space(6.0);
    if sigs.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.add(icons::image("signature", 32.0, t.text_faint));
            ui.add_space(6.0);
            ui.label(egui::RichText::new(tl!("This document has no signatures.")).color(t.text_muted));
        });
        return action;
    }
    let mut ordered: Vec<&SignatureInfo> = sigs.iter().collect();
    ordered.sort_by_key(|s| (!s.signed, s.revision));
    // Revisions saved after the first signature that no signature saved, in revision order
    // among the signatures (Acrobat lists them as changes made after signing).
    let first_signed = sigs.iter().filter(|s| s.signed).map(|s| s.revision).min();
    let mut changes: Vec<&sign::RevisionChanges> = revisions
        .iter()
        .filter(|r| first_signed.is_some_and(|f| r.revision > f) && !sigs.iter().any(|s| s.signed && s.revision == r.revision))
        .collect();
    changes.sort_by_key(|r| r.revision);
    let mut changes = changes.into_iter().peekable();
    for s in ordered {
        while let Some(r) = changes.next_if(|r| !s.signed || r.revision < s.revision) {
            revision_node(ui, t, r, expanded, &mut action);
        }
        let open = expanded.contains(&s.field);
        let (icon, color) = status_icon(s);
        let head = if s.signed {
            format!(
                "Rev. {}: {} by {}",
                s.revision,
                if s.certify.is_some() { "Certified" } else { "Signed" },
                s.signer.as_deref().unwrap_or("an unknown signer")
            )
        } else {
            format!("Unsigned signature field: {}", s.field)
        };
        let resp = ui
            .horizontal(|ui| {
                let chevron = if open { "chevron-down" } else { "chevron-right" };
                let toggle = icons::button(ui, chevron, 20.0, false, if open { "Collapse" } else { "Expand" }).clicked();
                ui.add(icons::image(icon, 16.0, color));
                let l = ui.add(egui::Label::new(egui::RichText::new(&head).font(theme::semibold(12.5)).color(t.text)).sense(egui::Sense::click()));
                toggle || l.clicked()
            })
            .inner;
        if resp {
            if open {
                expanded.retain(|f| f != &s.field);
            } else {
                expanded.push(s.field.clone());
            }
        }
        if !open {
            continue;
        }
        egui::Frame::NONE.inner_margin(egui::Margin { left: 44, right: 4, top: 2, bottom: 8 }).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            if !s.signed {
                if widgets::pill_button(ui, tl!("Sign this field…"), true).clicked() {
                    action = Some(PanelAction::Sign(s.field.clone()));
                }
                return;
            }
            ui.label(
                egui::RichText::new(match s.status {
                    SignatureStatus::Valid => tl!("Signature is valid:"),
                    SignatureStatus::Unknown => tl!("Signature validity is UNKNOWN:"),
                    SignatureStatus::Invalid => tl!("Signature is INVALID:"),
                })
                .font(theme::semibold(12.0)),
            );
            for line in &s.details {
                ui.add(egui::Label::new(egui::RichText::new(format!("• {line}")).font(theme::regular(11.5)).color(t.text_muted)).wrap());
            }
            ui.add_space(4.0);
            ui.label(egui::RichText::new(tl!("Signature Details")).font(theme::semibold(12.0)));
            let row = |ui: &mut egui::Ui, k: &str, v: &str| {
                ui.add(egui::Label::new(egui::RichText::new(format!("{}: {v}", tl!(k))).font(theme::regular(11.5))).wrap());
            };
            if let Some(r) = &s.reason {
                row(ui, "Reason", r);
            }
            if let Some(l) = &s.location {
                row(ui, "Location", l);
            }
            if let Some(d) = &s.date {
                row(ui, "Signing time", &sign::pdf::display_date(d));
            }
            if let Some(a) = &s.algorithm {
                row(ui, "Algorithm", a);
            }
            if let Some(c) = &s.certificate {
                row(ui, "Signer", &c.subject.display());
                row(ui, "Issued by", &c.issuer.display());
                row(ui, "Valid", &format!("{} to {}", c.not_before, c.not_after));
            }
            match s.page {
                Some(p) => {
                    let link = ui.add(
                        egui::Label::new(
                            egui::RichText::new(crate::i18n::fmt(
                                tl!("Field: {field} on page {p}"),
                                &[("field", &s.field), ("p", &(p + 1).to_string())],
                            ))
                            .font(theme::regular(11.5))
                            .color(t.accent_text)
                            .underline(),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if link.clicked() {
                        action = Some(PanelAction::GoTo(p));
                    }
                }
                None => row(ui, "Field", &s.field),
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if let Some(c) = s.chain.last().or(s.certificate.as_ref())
                    && s.status == SignatureStatus::Unknown
                    && ui.button(tl!("Add to trusted certificates")).clicked()
                {
                    action = Some(PanelAction::Trust(Box::new(c.clone())));
                }
                if ui.button(tl!("Signature Properties…")).clicked() {
                    action = Some(PanelAction::Properties(s.field.clone()));
                }
                if ui.button(tl!("View signed version")).clicked() {
                    action = Some(PanelAction::ViewSigned(s.signed_len));
                }
                if let Some(chain) = viewer_chain(s)
                    && ui.button(tl!("Show certificate…")).clicked()
                {
                    action = Some(PanelAction::ViewCertificate(chain));
                }
                if let Some(c) = &s.certificate
                    && ui.button(tl!("Export certificate…")).clicked()
                {
                    action = Some(PanelAction::ExportCertificate(Box::new(c.clone())));
                }
            });
        });
    }
    for r in changes {
        revision_node(ui, t, r, expanded, &mut action);
    }
    action
}

/// Certificate Viewer: a chain (the end certificate first) and the tab shown.
#[derive(Clone, Debug)]
pub struct CertViewer {
    pub chain: Vec<Certificate>,
    pub selected: usize,
    pub tab: CertTab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertTab {
    Summary,
    Details,
    Trust,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

fn key_usage(bits: u16) -> String {
    const NAMES: [&str; 9] = [
        "Digital Signature",
        "Non-Repudiation",
        "Key Encipherment",
        "Data Encipherment",
        "Key Agreement",
        "Certificate Signing",
        "CRL Signing",
        "Encipher Only",
        "Decipher Only",
    ];
    let used: Vec<&str> = NAMES.iter().enumerate().filter(|(i, _)| bits & (1 << i) != 0).map(|(_, n)| *n).collect();
    if used.is_empty() { "None".into() } else { used.join(", ") }
}

/// What the viewer asks for.
pub(crate) enum CertAction {
    Trust(Box<Certificate>),
    Export(Box<Certificate>),
}

/// The Certificate Viewer dialog: the chain on the left, the selected certificate's tabs.
pub(crate) fn cert_viewer(ui: &mut egui::Ui, v: &mut CertViewer, trusted: &[Certificate], t: &Tokens) -> (bool, Option<CertAction>) {
    title(ui, "Certificate Viewer");
    ui.label(egui::RichText::new(tl!("This dialog shows the details of a certificate and its chain.")).color(t.text_muted));
    ui.add_space(8.0);
    let mut action = None;
    v.selected = v.selected.min(v.chain.len().saturating_sub(1));
    ui.horizontal_top(|ui| {
        // The chain, root at the top (as Acrobat shows it).
        ui.vertical(|ui| {
            ui.set_width(200.0);
            for (depth, i) in (0..v.chain.len()).rev().enumerate() {
                let c = &v.chain[i];
                ui.horizontal(|ui| {
                    ui.add_space(depth as f32 * 12.0);
                    if ui.selectable_label(v.selected == i, c.display_name()).clicked() {
                        v.selected = i;
                    }
                });
            }
        });
        ui.add_space(16.0);
        ui.vertical(|ui| {
            ui.set_width(440.0);
            let Some(c) = v.chain.get(v.selected).cloned() else { return };
            ui.horizontal(|ui| {
                for (tab, label) in [(CertTab::Summary, tl!("Summary")), (CertTab::Details, tl!("Details")), (CertTab::Trust, tl!("Trust"))] {
                    if widgets::pill_button(ui, label, v.tab == tab).clicked() {
                        v.tab = tab;
                    }
                }
            });
            ui.add_space(8.0);
            let grid = |ui: &mut egui::Ui, rows: Vec<(&str, String)>| {
                egui::Grid::new(("cert-rows", v.tab as u8)).num_columns(2).spacing([12.0, 5.0]).show(ui, |ui| {
                    for (k, val) in rows {
                        ui.label(egui::RichText::new(tl_ctx!("certificate", k)).color(t.text_muted));
                        ui.add(egui::Label::new(val).wrap());
                        ui.end_row();
                    }
                });
            };
            match v.tab {
                CertTab::Summary => grid(
                    ui,
                    vec![
                        ("Issued to", c.subject.display()),
                        ("Issued by", c.issuer.display()),
                        ("Valid from", c.not_before.to_string()),
                        ("Valid to", c.not_after.to_string()),
                        ("Intended usage", c.key_usage.map(key_usage).unwrap_or_else(|| tl!("Any").to_string())),
                    ],
                ),
                CertTab::Details => grid(
                    ui,
                    vec![
                        ("Version", "3".into()),
                        ("Serial number", c.serial_hex()),
                        ("Issuer", c.issuer.display()),
                        ("Subject", c.subject.display()),
                        ("Validity starts", c.not_before.to_string()),
                        ("Validity ends", c.not_after.to_string()),
                        ("Public key", c.public_key.describe()),
                        ("Basic constraints", tl!(if c.is_ca { "Certificate authority" } else { "End entity" }).to_string()),
                        ("Key usage", c.key_usage.map(key_usage).unwrap_or_else(|| tl!("Not present").to_string())),
                        ("Self-signed", if c.is_self_signed() { tl!("Yes").to_string() } else { tl!("No").to_string() }),
                        ("SHA-1 digest", hex(&sign::keys::DigestAlg::Sha1.digest(&[&c.raw]))),
                        ("SHA-256 digest", hex(&sign::keys::DigestAlg::Sha256.digest(&[&c.raw]))),
                    ],
                ),
                CertTab::Trust => {
                    let is_trusted = trusted.iter().any(|x| x.raw == c.raw);
                    let anchored = v.chain.iter().any(|x| trusted.iter().any(|y| y.raw == x.raw));
                    ui.label(if is_trusted {
                        tl!("This certificate is in your list of trusted certificates.")
                    } else if anchored {
                        tl!("This certificate is trusted through a certificate above it in the chain.")
                    } else {
                        tl!("This certificate is not trusted. Signatures made with it show an unknown identity.")
                    });
                    ui.add_space(8.0);
                    if !is_trusted && widgets::pill_button(ui, tl!("Add to Trusted Certificates"), false).clicked() {
                        action = Some(CertAction::Trust(Box::new(c.clone())));
                    }
                }
            }
            ui.add_space(8.0);
            if widgets::pill_button(ui, tl!("Export…"), false).clicked() {
                action = Some(CertAction::Export(Box::new(c.clone())));
            }
        });
    });
    ui.add_space(10.0);
    let mut close = false;
    ui.horizontal(|ui| ui.with_layout(Layout::right_to_left(Align::Center), |ui| close = widgets::pill_button(ui, tl!("OK"), true).clicked()));
    (close, action)
}

/// Signature Properties for one signature field, and the tab shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigProps {
    pub field: String,
    pub tab: SigPropsTab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigPropsTab {
    Summary,
    Document,
    Signer,
    DateTime,
    Legal,
}

impl SigPropsTab {
    pub const ALL: [SigPropsTab; 5] = [Self::Summary, Self::Document, Self::Signer, Self::DateTime, Self::Legal];

    pub fn label(self) -> &'static str {
        match self {
            Self::Summary => tl!("Summary"),
            Self::Document => tl!("Document"),
            Self::Signer => tl!("Signer"),
            Self::DateTime => tl!("Date/Time"),
            Self::Legal => tl!("Legal"),
        }
    }

    /// `summary`, `document`, `signer`, `date-time` or `legal` (the control channel's names).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "summary" => Self::Summary,
            "document" => Self::Document,
            "signer" => Self::Signer,
            "date-time" => Self::DateTime,
            "legal" => Self::Legal,
            _ => return None,
        })
    }
}

/// What Signature Properties asks the app to do.
#[derive(Clone, Debug)]
pub(crate) enum SigPropsAction {
    Validate,
    Trust(Box<Certificate>),
    ViewSigned(usize),
    /// Certificate Viewer for the signer's chain (signer first).
    ViewCertificate(Vec<Certificate>),
}

/// The signer's chain, signer first, for the Certificate Viewer.
fn viewer_chain(s: &SignatureInfo) -> Option<Vec<Certificate>> {
    let c = s.certificate.as_ref()?;
    let mut chain = vec![c.clone()];
    chain.extend(s.chain.iter().filter(|x| x.raw != c.raw).cloned());
    Some(chain)
}

fn certify_label(p: u8) -> &'static str {
    match p {
        1 => tl!("No changes allowed"),
        3 => tl!("Annotations, form fill-in, and digital signatures"),
        _ => tl!("Form fill-in and digital signatures"),
    }
}

/// The rows of one Signature Properties tab, as (label, value) in the UI language. Rows whose
/// value the signature doesn't carry are left out.
pub fn property_rows(s: &SignatureInfo, tab: SigPropsTab) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut push = |k: &str, v: Option<String>| {
        if let Some(v) = v.filter(|v| !v.trim().is_empty()) {
            rows.push((k.to_string(), v));
        }
    };
    let signing_time = s.date.as_deref().map(sign::pdf::display_date).or_else(|| s.signing_time.map(|t| t.to_string()));
    match tab {
        SigPropsTab::Summary => {
            push(tl!("Signer"), s.signer.clone());
            push(tl!("Reason"), s.reason.clone());
            push(tl!("Signing time"), signing_time);
            push(tl!("Location"), s.location.clone());
        }
        SigPropsTab::Document => {
            push(tl!("Revision"), Some(s.revision.to_string()));
            push(tl!("Field"), Some(s.field.clone()));
            push(tl!("Page"), s.page.map(|p| (p + 1).to_string()));
            let changes = match &s.modification {
                Modification::None => tl!("The document has not been modified since this signature was applied.").to_string(),
                Modification::Allowed(list) => {
                    crate::i18n::fmt(tl!("Changes the signer permitted were made: {changes}"), &[("changes", &list.join("; "))])
                }
                Modification::Disallowed(list) => {
                    crate::i18n::fmt(tl!("Changes the signer did not permit were made: {changes}"), &[("changes", &list.join("; "))])
                }
            };
            push(tl!("Changes after signing"), Some(changes));
        }
        SigPropsTab::Signer => {
            if let Some(c) = &s.certificate {
                push(tl!("Signer"), Some(c.subject.display()));
                push(tl!("Issued by"), Some(c.issuer.display()));
                push(tl!("Valid from"), Some(c.not_before.to_string()));
                push(tl!("Valid to"), Some(c.not_after.to_string()));
            } else {
                push(tl!("Signer"), s.signer.clone());
            }
            push(tl!("Contact"), s.contact.clone());
        }
        SigPropsTab::DateTime => {
            push(tl!("Signing time"), signing_time);
            let source = if s.timestamp_time.is_some() {
                tl!("Trusted timestamp")
            } else if s.timestamp {
                tl!("Untrusted timestamp")
            } else {
                tl!("Signer's computer clock")
            };
            push(tl!("Time source"), Some(source.to_string()));
            push(tl!("Timestamp"), s.timestamp_time.map(|t| t.to_string()));
            push(tl!("Timestamp authority"), s.timestamp_chain.first().map(|c| c.display_name()));
            let when = s.verification_time.map(|t| t.to_string());
            let how = match s.verification_time_source {
                sign::pdf::VerificationTimeSource::TrustedTimestamp => tl!("Trusted timestamp"),
                sign::pdf::VerificationTimeSource::ClaimedSigningTime => tl!("Claimed signing time"),
                sign::pdf::VerificationTimeSource::ChosenTime => tl!("Chosen time"),
                sign::pdf::VerificationTimeSource::Unavailable => tl!("Not available"),
            };
            push(tl!("Verification time"), Some(when.map_or_else(|| how.to_string(), |w| format!("{w} ({how})"))));
        }
        SigPropsTab::Legal => {
            let kind = if s.doc_timestamp {
                tl!("Document timestamp")
            } else if s.certify.is_some() {
                tl!("Certification signature")
            } else {
                tl!("Approval signature")
            };
            push(tl!("Signature type"), Some(kind.to_string()));
            push(tl!("Permitted actions after certifying"), s.certify.map(|p| certify_label(p).to_string()));
            push(tl!("Format"), s.sub_filter.clone());
            push(tl!("Algorithm"), s.algorithm.clone());
        }
    }
    rows
}

/// The Signature Properties dialog (Acrobat: right-click a signature ▸ Show Signature
/// Properties). Returns whether to close, and what to do.
pub(crate) fn properties_dialog(ui: &mut egui::Ui, p: &mut SigProps, s: &SignatureInfo, t: &Tokens) -> (bool, Option<SigPropsAction>) {
    title(ui, "Signature Properties");
    let mut action = None;
    let (icon, color) = status_icon(s);
    ui.horizontal(|ui| {
        ui.add(icons::image(icon, 20.0, color));
        ui.label(
            egui::RichText::new(if !s.signed {
                tl!("This signature field is not signed.")
            } else {
                match s.status {
                    SignatureStatus::Valid => tl!("Signature is valid:"),
                    SignatureStatus::Unknown => tl!("Signature validity is UNKNOWN:"),
                    SignatureStatus::Invalid => tl!("Signature is INVALID:"),
                }
            })
            .font(theme::semibold(13.0)),
        );
    });
    for line in &s.details {
        ui.add(egui::Label::new(egui::RichText::new(format!("• {line}")).font(theme::regular(11.5)).color(t.text_muted)).wrap());
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        for tab in SigPropsTab::ALL {
            if widgets::pill_button(ui, tab.label(), p.tab == tab).clicked() {
                p.tab = tab;
            }
        }
    });
    ui.add_space(8.0);
    egui::Grid::new(("sig-props", p.tab as u8)).num_columns(2).spacing([12.0, 5.0]).show(ui, |ui| {
        for (k, v) in property_rows(s, p.tab) {
            ui.label(egui::RichText::new(k).color(t.text_muted));
            ui.add(egui::Label::new(v).wrap());
            ui.end_row();
        }
    });
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| match p.tab {
        SigPropsTab::Summary | SigPropsTab::Signer => {
            if let Some(chain) = viewer_chain(s)
                && ui.button(tl!("Show certificate…")).clicked()
            {
                action = Some(SigPropsAction::ViewCertificate(chain));
            }
            if let Some(c) = s.chain.last().or(s.certificate.as_ref())
                && s.status == SignatureStatus::Unknown
                && ui.button(tl!("Add to trusted certificates")).clicked()
            {
                action = Some(SigPropsAction::Trust(Box::new(c.clone())));
            }
        }
        SigPropsTab::Document if s.signed && ui.button(tl!("View signed version")).clicked() => {
            action = Some(SigPropsAction::ViewSigned(s.signed_len));
        }
        _ => {}
    });
    ui.add_space(10.0);
    let mut close = false;
    ui.horizontal(|ui| {
        if s.signed && widgets::pill_button(ui, tl!("Validate Signature"), false).clicked() {
            action = Some(SigPropsAction::Validate);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| close = widgets::pill_button(ui, tl!("Close"), true).clicked());
    });
    (close, action)
}

/// The text of an exported certificate (`.cer`, PEM).
pub fn certificate_pem(c: &Certificate) -> String {
    sign::x509::to_pem(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder per call: tests run in parallel.
    fn scratch() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("pdfcraft-sign-ui-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Create a digital ID named "Grace Hopper" in `dir`, as Configure New Digital ID ▸ Create
    /// does. Returns where it was saved.
    fn create_in(dir: &std::path::Path) -> Result<PathBuf, String> {
        let mut app = PdfCraftApp::new();
        app.export_dir_override = Some(dir.to_string_lossy().into_owned());
        let mut draft = SignDraft::new(0, None, None, None, 0);
        // P-256 keeps the test fast.
        let key = KEY_ALGORITHMS.iter().position(|k| k.1 == "p256").unwrap();
        draft.new_id =
            NewIdDraft { name: "Grace Hopper".into(), key, password: "secret1".into(), confirm: "secret1".into(), ..NewIdDraft::default() };
        app.sign_draft = Some(draft);
        let i = app.create_digital_id()?;
        Ok(PathBuf::from(&app.digital_ids[i].path))
    }

    #[test]
    fn a_new_digital_id_skips_names_already_taken_without_writing_through_them() {
        let dir = scratch();
        // Another file's hard link at the first name, and a folder at the second (Windows refuses
        // `create_new` on a folder with "access denied" rather than "already exists").
        std::fs::write(dir.join("victim.txt"), "keep me").unwrap();
        std::fs::hard_link(dir.join("victim.txt"), dir.join("Grace Hopper.p12")).unwrap();
        std::fs::create_dir(dir.join("Grace Hopper 2.p12")).unwrap();
        let saved = create_in(&dir).unwrap();
        assert_eq!(saved, dir.join("Grace Hopper 3.p12"));
        assert_eq!(std::fs::read_to_string(dir.join("victim.txt")).unwrap(), "keep me");
        assert!(sign::pkcs12::open(&std::fs::read(&saved).unwrap(), "secret1").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_digital_id_is_not_written_through_a_dangling_link() {
        // `exists()` follows links, so a link to a file that doesn't exist yet looked free, and
        // `fs::write` then created the private key wherever the link pointed.
        let dir = scratch();
        let (link, target) = (dir.join("Grace Hopper.p12"), dir.join("elsewhere.p12"));
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&target, &link);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&target, &link);
        #[cfg(not(any(unix, windows)))]
        let made: std::io::Result<()> = {
            let _ = (&target, &link);
            Err(std::io::ErrorKind::Unsupported.into())
        };
        if let Err(e) = made {
            // Windows needs Developer Mode (or admin) for symlinks.
            eprintln!("skipped: can't create a symlink here: {e}");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let saved = create_in(&dir).unwrap();
        assert!(!target.exists(), "nothing written through the link");
        assert_eq!(saved, dir.join("Grace Hopper 2.p12"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_digital_id_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch();
        let saved = create_in(&dir).unwrap();
        let mode = std::fs::metadata(&saved).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the private key is not readable by others: {mode:o}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
