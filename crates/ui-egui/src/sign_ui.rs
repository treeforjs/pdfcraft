//! Use a certificate (execution plan M9.6): Digitally sign and Certify (drag a rectangle, or
//! click an empty signature field), the Sign with a Digital ID / Configure New Digital ID /
//! Sign as dialogs, the Signatures panel, and the signature message bar.

use std::path::PathBuf;

use egui::{Align, Color32, CornerRadius, Layout, Pos2, Rect, Stroke, vec2};
use pdfcraft_engine::sign::token::{self as tok, Identities, Login, Pin, SlotInfo, TokenError};
use pdfcraft_engine::sign::{self, Appearance, Certificate, DigitalId, Modification, Name, PrivateKey};
use pdfcraft_engine::signature_settings::{AppearanceGraphic, AppearanceImage, SavedAppearance};
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
    /// Sign with a smart card or token: module, slot, PIN, certificate.
    Token,
}

/// Identities kept in the OS key stores or on a token, listed afresh each time (never saved).
pub(crate) fn is_key_store_entry(path: &str) -> bool {
    path.starts_with("keychain:") || path.starts_with("windows:") || path.starts_with("pkcs11:")
}

/// A PIN as typed: never printed, zeroed when dropped or used.
#[derive(Clone, Default, PartialEq)]
pub struct PinField(pub String);

impl std::fmt::Debug for PinField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PinField(…)")
    }
}

impl Drop for PinField {
    fn drop(&mut self) {
        // Moves the bytes out and zeroes them (Pin's own drop).
        drop(Pin::take(&mut self.0));
    }
}

/// Sign with a smart card or token: what the step shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TokenDraft {
    /// Module files found in the usual places.
    pub modules: Vec<String>,
    /// The module to use (a found one, or a path the user chose).
    pub module: String,
    /// Its slots, once listed.
    pub slots: Vec<SlotInfo>,
    pub slot: Option<u64>,
    pub pin: PinField,
    /// The unlocked token's identities.
    pub found: Option<Identities>,
    pub selected: Option<usize>,
}

/// A token being unlocked in the background (a PIN-pad reader waits for its keypad).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) struct TokenUnlock {
    rx: std::sync::mpsc::Receiver<Result<(Login, Identities), TokenError>>,
    epoch: u64,
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
    pub token: TokenDraft,
    /// Lock the document after signing: every field locked, no later changes permitted.
    pub lock_document: bool,
    /// What the left side of the visible signature shows.
    pub graphic: GraphicChoice,
    /// The drawn signature, in the signer's unit box (`0..=1000`, y down).
    pub ink: Vec<Vec<[u16; 2]>>,
    /// The chosen picture of a signature.
    pub picture: Option<AppearanceImage>,
    /// A faint logo behind the signature.
    pub logo: Option<AppearanceImage>,
    /// The saved appearance applied last, and the name the next one is saved under.
    pub saved: Option<String>,
    pub save_as: String,
}

/// The left side of a visible signature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub enum GraphicChoice {
    /// The signer's name in large type (or nothing, with Name off).
    #[default]
    Name,
    /// A signature drawn with the pointer.
    Drawn,
    /// A picture of a signature.
    Picture,
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
            token: TokenDraft::default(),
            lock_document: false,
            graphic: GraphicChoice::Name,
            ink: Vec::new(),
            picture: None,
            logo: None,
            saved: None,
            save_as: String::new(),
        }
    }

    /// The appearance being configured, as a saved appearance named `name`.
    pub fn look(&self, name: &str) -> SavedAppearance {
        let a = &self.appearance;
        let graphic = match self.graphic {
            GraphicChoice::Name => None,
            GraphicChoice::Drawn => (!self.ink.is_empty()).then(|| AppearanceGraphic::Drawn(self.ink.clone())),
            GraphicChoice::Picture => self.picture.clone().map(AppearanceGraphic::Image),
        };
        SavedAppearance {
            name: name.to_string(),
            show_name: a.name,
            date: a.date,
            reason: a.reason,
            location: a.location,
            distinguished_name: a.distinguished_name,
            labels: a.labels,
            graphic,
            logo: self.logo.clone(),
        }
    }

    /// Use a saved appearance: its text choices, graphic and logo.
    pub fn apply(&mut self, saved: &SavedAppearance) {
        let a = &mut self.appearance;
        a.name = saved.show_name;
        a.date = saved.date;
        a.reason = saved.reason;
        a.location = saved.location;
        a.distinguished_name = saved.distinguished_name;
        a.labels = saved.labels;
        match &saved.graphic {
            None => self.graphic = GraphicChoice::Name,
            Some(AppearanceGraphic::Drawn(s)) => {
                self.graphic = GraphicChoice::Drawn;
                self.ink = s.clone();
            }
            Some(AppearanceGraphic::Image(i)) => {
                self.graphic = GraphicChoice::Picture;
                self.picture = Some(i.clone());
            }
        }
        self.logo = saved.logo.clone();
        self.saved = Some(saved.name.clone());
        self.save_as = saved.name.clone();
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

/// A token identity by its `pkcs11:` reference, on the token unlocked in this dialog.
fn token_id(login: Option<&Login>, reference: &str) -> Result<DigitalId, String> {
    let login = login.ok_or_else(|| tl!("Unlock the token again.").to_string())?;
    login.find(reference).map_err(|e| e.to_string())
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
        let token = std::mem::take(&mut self.sign_with_token);
        self.sign_draft = Some(draft);
        self.dialog = Some(crate::Dialog::Sign);
        if token {
            self.open_token_step();
        }
    }

    /// Sign with a Smart Card or Token: find modules and, with one, its slots.
    pub fn open_token_step(&mut self) {
        let modules = self.token_provider.discover();
        let Some(d) = self.sign_draft.as_mut() else { return };
        d.step = SignStep::Token;
        d.error = None;
        if d.token.module.trim().is_empty() {
            d.token.module = modules.first().cloned().unwrap_or_default();
        }
        d.token.modules = modules;
        if !d.token.module.trim().is_empty() && d.token.slots.is_empty() {
            self.find_tokens();
        }
    }

    /// List the chosen module's slots (loading the module on first use).
    pub fn find_tokens(&mut self) {
        let module = self.sign_draft.as_ref().map(|d| d.token.module.clone()).unwrap_or_default();
        let result = tok::load_module(self.token_provider.as_ref(), Some(&module)).and_then(|(_, m)| tok::slots(m.as_ref()));
        // A token from another module or slot is no longer the one in use.
        self.token_login = None;
        self.token_unlock = None;
        let Some(d) = self.sign_draft.as_mut() else { return };
        d.token.found = None;
        d.token.selected = None;
        match result {
            Ok(slots) => {
                // The first slot with a card in it.
                d.token.slot = slots.iter().find(|s| s.token.is_some()).map(|s| s.id);
                d.token.slots = slots;
                d.error = None;
            }
            Err(e) => {
                d.token.slots.clear();
                d.token.slot = None;
                d.error = Some(e.to_string());
            }
        }
    }

    /// Unlock the chosen token with the typed PIN (cleared from the field at once), then list
    /// its certificates. Runs in the background so a PIN-pad reader can wait for its keypad.
    pub fn unlock_token(&mut self) {
        let epoch = self.dialog_epoch();
        let Some(d) = self.sign_draft.as_mut() else { return };
        let Some(slot) = d.token.slot else {
            d.error = Some(tl!("Choose a slot with a card or token.").to_string());
            return;
        };
        let pin = Pin::take(&mut d.token.pin.0);
        let module = d.token.module.clone();
        d.token.found = None;
        d.token.selected = None;
        d.error = None;
        self.token_login = None;
        let provider = self.token_provider.clone();
        let work = move || -> Result<(Login, Identities), TokenError> {
            let (_, module) = tok::load_module(provider.as_ref(), Some(&module))?;
            let login = tok::login(module.as_ref(), slot, Some(pin))?;
            let found = login.identities()?;
            Ok((login, found))
        };
        #[cfg(not(target_arch = "wasm32"))]
        if !self.run_inline {
            let (tx, rx) = std::sync::mpsc::channel();
            let spawned = std::thread::Builder::new().name("token-unlock".into()).spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                    .unwrap_or_else(|_| Err(TokenError::Device("the token module failed".into())));
                let _ = tx.send(result);
            });
            match spawned {
                Ok(_) => self.token_unlock = Some(TokenUnlock { rx, epoch }),
                Err(e) => {
                    if let Some(d) = self.sign_draft.as_mut() {
                        d.error = Some(e.to_string());
                    }
                }
            }
            return;
        }
        #[cfg(target_arch = "wasm32")]
        let _ = epoch;
        self.token_unlocked(work());
    }

    /// An unlock finished: keep the token open and show its certificates, or say what failed.
    fn token_unlocked(&mut self, result: Result<(Login, Identities), TokenError>) {
        self.token_unlock = None;
        let Some(d) = self.sign_draft.as_mut() else { return };
        match result {
            Ok((login, found)) => {
                d.token.selected = (!found.ids.is_empty()).then_some(0);
                d.token.found = Some(found);
                self.token_login = Some(login);
            }
            Err(e) => d.error = Some(e.to_string()),
        }
    }

    /// Poll a background unlock (each frame while the token step shows).
    fn poll_token_unlock(&mut self, ctx: &egui::Context) {
        let epoch = self.dialog_epoch();
        let Some(u) = self.token_unlock.as_ref() else { return };
        if u.epoch != epoch {
            // The dialog was closed or reopened meanwhile: the result is dropped (logging out).
            self.token_unlock = None;
            return;
        }
        match u.rx.try_recv() {
            Ok(result) => self.token_unlocked(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.token_unlocked(Err(TokenError::Device("the token module failed".into()))),
        }
    }

    /// Use the token identity chosen in the token step: list it with the digital IDs and go on
    /// to Sign as.
    pub fn use_token_identity(&mut self) -> Result<(), String> {
        let ident = self
            .sign_draft
            .as_ref()
            .and_then(|d| d.token.found.as_ref().zip(d.token.selected))
            .and_then(|(f, i)| f.ids.get(i).cloned())
            .ok_or_else(|| tl!("Choose the certificate to sign with:").trim_end_matches(':').to_string())?;
        if self.token_login.is_none() {
            return Err(tl!("Unlock the token again.").to_string());
        }
        self.digital_ids.retain(|e| !e.path.starts_with("pkcs11:"));
        let mut entry = entry_for(&ident.reference, &ident.certificate);
        entry.name = ident.label.clone();
        self.digital_ids.push(entry);
        let i = self.digital_ids.len() - 1;
        if let Some(d) = self.sign_draft.as_mut() {
            d.selected = Some(i);
            d.step = SignStep::SignAs;
            d.error = None;
        }
        Ok(())
    }

    /// The signing dialog closed: log out of the token (a signature still being made keeps its
    /// own hold until it is done) and forget its identities.
    pub(crate) fn close_token(&mut self) {
        self.token_login = None;
        self.token_unlock = None;
        self.digital_ids.retain(|e| !e.path.starts_with("pkcs11:"));
    }

    /// List OS key store signing identities (after the file-based IDs).
    fn refresh_os_key_store_ids(&mut self) {
        self.digital_ids.retain(|e| !is_key_store_entry(&e.path));
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
        } else if entry.path.starts_with("pkcs11:") {
            token_id(self.token_login.as_ref(), &entry.path)?
        } else {
            let bytes = std::fs::read(&entry.path).map_err(|e| format!("{}: {e}", entry.path))?;
            sign::pkcs12::open(&bytes, &d.password).map_err(|e| match e {
                sign::SignError::WrongPassword => "The password is incorrect.".to_string(),
                e => e.to_string(),
            })?
        };
        let some = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
        // The picture, drawn signature and logo, decoded and bounded by the engine.
        let appearance = d.look("").appearance().map_err(|e| e.to_string())?;
        let opts = SignOptions {
            field: d.field.clone(),
            page: d.page,
            rect: d.rect,
            reason: some(&d.reason),
            location: some(&d.location),
            certify: d.certify,
            lock: d.lock_document.then(sign::FieldLock::document),
            appearance,
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

    /// Compare Signed Version to Current Version: open the version the signature in `field`
    /// covers beside the current document, compare them side by side, and list what changed
    /// since signing with whether the signature permits it. The current document stays active.
    pub fn compare_signed_with_current(&mut self, field: &str) {
        let Some((_, id)) = self.active_ids() else {
            self.notify_tr("Open a signed document first");
            return;
        };
        let result = match self.session.compare_signed_version(id, field) {
            Ok(r) => r,
            Err(e) => {
                self.notify_fmt("Couldn't compare the signed version: {e}", &[("e", &e)]);
                return;
            }
        };
        // The signed version from an earlier comparison of this field is reused when still open.
        let reuse = self
            .signed_comparison
            .as_ref()
            .filter(|c| c.doc == id && c.field == field)
            .map(|c| c.signed)
            .filter(|s| self.views.iter().any(|v| v.id == *s));
        let signed = match reuse {
            Some(s) => s,
            None => match self.session.open_signed_version(id, field) {
                Ok(s) => {
                    let Some(doc) = self.session.get(s) else { return };
                    self.views.push(DocView::new(s, &doc.info, self.view_defaults));
                    s
                }
                Err(e) => {
                    self.notify_fmt("Couldn't open the signed version: {e}", &[("e", &e)]);
                    return;
                }
            },
        };
        if let Some(i) = self.views.iter().position(|v| v.id == id) {
            self.active = Some(i);
        }
        self.signed_comparison = Some(SignedComparison { doc: id, signed, field: field.to_string(), result });
        self.compare_old = Some(signed);
        self.side_by_side_after_compare = true;
        self.run_compare();
    }

    /// Save the Sign As dialog's appearance under `name` (replacing one of that name).
    pub fn save_sign_appearance(&mut self, name: &str) -> Result<(), String> {
        let d = self.sign_draft.as_ref().ok_or("nothing to sign")?;
        let look = d.look(name);
        // The engine checks the name, sizes and limits.
        look.appearance().map_err(|e| e.to_string())?;
        self.session.save_signature_appearance(look).map_err(|e| e.to_string())?;
        let saved = self.session.signature_appearance(name.trim()).map(|a| a.name.clone());
        if let Some(d) = self.sign_draft.as_mut() {
            d.saved = saved;
        }
        self.notify_fmt("Saved the appearance {name}", &[("name", name.trim())]);
        Ok(())
    }

    /// Use the picture (or, with `logo`, the logo) in the file at `path` in the Sign As dialog.
    /// A signature picture loses its white background, as in Fill & Sign.
    pub fn set_sign_picture(&mut self, logo: bool, path: &std::path::Path) -> Result<(), String> {
        let image = std::fs::File::open(path)
            .map_err(pdfcraft_engine::signature_image::SignatureImageError::from)
            .and_then(pdfcraft_engine::SignatureImage::read)
            .map_err(|e| e.to_string())?;
        let image = if logo { image } else { image.remove_white_background(245, 35).map_err(|e| e.to_string())? };
        let image = AppearanceImage(image);
        // Refuse here what signing would refuse.
        image.graphic().map_err(|e| e.to_string())?;
        let d = self.sign_draft.as_mut().ok_or("nothing to sign")?;
        if logo {
            d.logo = Some(image);
        } else {
            d.picture = Some(image);
            d.graphic = GraphicChoice::Picture;
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn pick_sign_picture(&mut self, logo: bool) {
        let dialog = rfd::AsyncFileDialog::new().add_filter(tl!("Image"), &["png", "jpg", "jpeg"]);
        let _ = self.ask_one(crate::pickers::Ask::File(dialog), None, move |app, path| {
            if app.sign_draft.is_none() {
                return;
            }
            if let Err(e) = app.set_sign_picture(logo, &path) {
                app.notify_fmt("Couldn't use the picture: {e}", &[("e", &e)]);
            }
        });
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
        SignStep::Token => token_step(ui, app, t),
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
                } else if e.path.starts_with("pkcs11:") {
                    tl!("Token  ·  ").to_string()
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
    let mut token = false;
    ui.horizontal(|ui| {
        if widgets::pill_button(ui, tl!("Configure New Digital ID"), false).clicked() {
            d.step = SignStep::Configure;
            d.error = None;
        }
        if widgets::icon_pill(ui, "shield-check", tl!("Use a Smart Card or Token"), false).clicked() {
            token = true;
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
    if token {
        app.open_token_step();
    }
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

/// One selectable row in the token step: an icon, a title and a muted line, with optional
/// status chips (text, colour) on the right.
fn token_row(
    ui: &mut egui::Ui,
    t: &Tokens,
    (selected, enabled): (bool, bool),
    icon: &str,
    (title_text, sub): (&str, &str),
    chips: &[(&str, Color32)],
) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), sense);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, enabled, selected, title_text));
    let fill = if selected {
        t.accent_soft
    } else if resp.hovered() && enabled {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
    let tint = if enabled { t.accent } else { t.text_faint };
    icons::paint(ui, Rect::from_min_size(rect.min + vec2(10.0, 13.0), vec2(20.0, 20.0)), icon, 18.0, tint);
    let mut right = rect.right() - 8.0;
    for (label, colour) in chips.iter().rev() {
        let galley = ui.painter().layout_no_wrap(label.to_string(), theme::semibold(10.5), *colour);
        let chip = Rect::from_min_size(egui::pos2(right - galley.size().x - 14.0, rect.center().y - 10.0), vec2(galley.size().x + 14.0, 20.0));
        ui.painter().rect_filled(chip, CornerRadius::same(10), colour.gamma_multiply(0.14));
        ui.painter().galley(chip.min + vec2(7.0, (20.0 - galley.size().y) / 2.0), galley, *colour);
        right = chip.left() - 6.0;
    }
    let width = (right - rect.left() - 46.0).max(20.0);
    widgets::row_text(ui, rect.min + vec2(40.0, 16.0), title_text, theme::semibold(13.0), if enabled { t.text } else { t.text_muted }, width);
    widgets::row_text(ui, rect.min + vec2(40.0, 32.0), sub, theme::regular(11.5), t.text_muted, width);
    resp
}

/// Sign with a Smart Card or Token: the module, a slot, the PIN, then a certificate.
fn token_step(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    app.poll_token_unlock(ui.ctx());
    title(ui, "Sign with a Smart Card or Token");
    ui.label(egui::RichText::new(tl!("The signature is made on the card or token. Its private key never leaves it.")).color(t.text_muted));
    ui.add_space(8.0);
    let busy = app.token_unlock.is_some();
    let unlocked = app.token_login.is_some();
    let Some(d) = app.sign_draft.as_mut() else { return true };
    let (mut close, mut find, mut unlock, mut go) = (false, false, false, false);
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut browse = false;
    let tk = &mut d.token;

    // The module.
    let l = ui.label(egui::RichText::new(tl!("PKCS #11 module")).font(theme::semibold(12.5)));
    ui.horizontal(|ui| {
        let r = ui.add_enabled(!busy, egui::TextEdit::singleline(&mut tk.module).desired_width(300.0).hint_text("opensc-pkcs11")).labelled_by(l.id);
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            find = true;
        }
        if !tk.modules.is_empty() {
            egui::ComboBox::from_id_salt("token-modules").selected_text("").width(24.0).show_ui(ui, |ui| {
                for m in &tk.modules {
                    if ui.selectable_label(tk.module == *m, m.as_str()).clicked() {
                        tk.module = m.clone();
                        find = true;
                    }
                }
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        if ui.add_enabled(!busy, egui::Button::new(tl!("Browse…"))).clicked() {
            browse = true;
        }
        if ui.add_enabled(!busy && !tk.module.trim().is_empty(), egui::Button::new(tl!("Find tokens"))).clicked() {
            find = true;
        }
    });
    if tk.modules.is_empty() && tk.module.trim().is_empty() {
        ui.label(
            egui::RichText::new(tl!(
                "No PKCS #11 module was found here. Choose the module file that came with your card or token (for example opensc-pkcs11)."
            ))
            .small()
            .color(t.text_muted),
        );
    }
    ui.add_space(6.0);

    // Slots, while no token is unlocked.
    if tk.found.is_none() && !tk.slots.is_empty() {
        egui::ScrollArea::vertical().id_salt("token-slots").max_height(150.0).show(ui, |ui| {
            for s in &tk.slots {
                let red = Color32::from_rgb(0xD7, 0x37, 0x3F);
                let amber = Color32::from_rgb(0xE6, 0x86, 0x19);
                let (title_text, sub, chips): (String, String, Vec<(&str, Color32)>) = match &s.token {
                    Some(info) => {
                        let mut parts: Vec<String> =
                            [info.model.as_str(), info.manufacturer.as_str()].iter().filter(|p| !p.is_empty()).map(|p| p.to_string()).collect();
                        if !info.serial.is_empty() {
                            parts.push(format!("{} {}", tl!("Serial"), info.serial));
                        }
                        let mut chips = Vec::new();
                        if info.pin_locked {
                            chips.push((tl!("PIN locked"), red));
                        } else if info.pin_final_try {
                            chips.push((tl!("One try left"), amber));
                        }
                        if info.protected_path {
                            chips.push((tl!("PIN pad"), t.accent));
                        }
                        let name = if info.label.is_empty() { s.description.clone() } else { info.label.clone() };
                        (name, parts.join("  ·  "), chips)
                    }
                    None => (s.description.clone(), tl!("No card").to_string(), Vec::new()),
                };
                let enabled = s.token.is_some() && !busy;
                if token_row(ui, t, (tk.slot == Some(s.id), enabled), "shield-check", (&title_text, &sub), &chips).clicked() {
                    tk.slot = Some(s.id);
                }
            }
        });
        ui.add_space(6.0);
        let slot = tk.slot.and_then(|id| tk.slots.iter().find(|s| s.id == id)).and_then(|s| s.token.as_ref());
        let locked = slot.is_some_and(|s| s.pin_locked);
        let pad = slot.is_some_and(|s| s.protected_path);
        let needs_pin = slot.is_some_and(|s| s.login_required && !s.protected_path);
        ui.horizontal(|ui| {
            if needs_pin {
                let l = ui.label(tl!("PIN"));
                let r =
                    ui.add_enabled(!busy && !locked, egui::TextEdit::singleline(&mut tk.pin.0).password(true).desired_width(160.0)).labelled_by(l.id);
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    unlock = true;
                }
            } else if pad {
                ui.label(egui::RichText::new(tl!("Enter the PIN on the reader's keypad, then choose Unlock.")).color(t.text_muted));
            }
            if ui.add_enabled(!busy && !locked && slot.is_some(), egui::Button::new(tl!("Unlock"))).clicked() {
                unlock = true;
            }
            if busy {
                ui.spinner();
                ui.label(egui::RichText::new(tl!("Waiting for the token…")).color(t.text_muted));
            }
        });
    }

    // The unlocked token's certificates.
    if let Some(found) = tk.found.as_ref().filter(|_| unlocked) {
        ui.label(tl!("Choose the certificate to sign with:"));
        ui.add_space(4.0);
        egui::ScrollArea::vertical().id_salt("token-ids").max_height(180.0).show(ui, |ui| {
            for (i, ident) in found.ids.iter().enumerate() {
                let c = &ident.certificate;
                let issuer = c.issuer.common_name().map(str::to_string).unwrap_or_else(|| c.issuer.display());
                let sub = format!(
                    "{}{issuer}{}{:04}.{:02}.{:02}",
                    tl!("Issued by: "),
                    tl!(", Expires: "),
                    c.not_after.year,
                    c.not_after.month,
                    c.not_after.day
                );
                let r = token_row(ui, t, (tk.selected == Some(i), true), "badge-check", (&ident.label, &sub), &[]);
                if r.clicked() {
                    tk.selected = Some(i);
                }
                if r.double_clicked() {
                    go = true;
                }
            }
            if found.ids.is_empty() {
                ui.label(egui::RichText::new(tl!("No certificate on this token can sign.")).color(t.text_muted));
            }
        });
        if !found.skipped.is_empty() {
            let why: Vec<String> = found.skipped.iter().map(|s| format!("{}: {}", s.label, s.reason)).collect();
            ui.label(
                egui::RichText::new(crate::i18n::fmt(tl!("Certificates left out: {n}"), &[("n", &found.skipped.len().to_string())]))
                    .small()
                    .color(t.text_muted),
            )
            .on_hover_text(why.join("\n"));
        }
    }
    ui.add_space(6.0);
    error(ui, &d.error);
    let can_go = d.token.found.as_ref().is_some_and(|f| !f.ids.is_empty()) && d.token.selected.is_some() && unlocked;
    ui.horizontal(|ui| {
        if widgets::pill_button(ui, tl!("Back"), false).clicked() {
            d.step = if app.digital_ids.is_empty() { SignStep::Configure } else { SignStep::Choose };
            d.error = None;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add_enabled_ui(can_go, |ui| widgets::pill_button(ui, tl!("Continue"), true)).inner.clicked() {
                go = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                close = true;
            }
        });
    });
    if find {
        app.find_tokens();
    } else if unlock {
        app.unlock_token();
    } else if go
        && let Err(e) = app.use_token_identity()
        && let Some(d) = app.sign_draft.as_mut()
    {
        d.error = Some(e);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if browse {
        let dialog = rfd::AsyncFileDialog::new().add_filter(tl!("PKCS #11 module"), &["so", "dylib", "dll"]);
        let epoch = app.dialog_epoch();
        app.ask_one(crate::pickers::Ask::File(dialog), None, move |app, p| {
            if app.dialog_epoch() == epoch
                && let Some(d) = app.sign_draft.as_mut()
            {
                d.token.module = p.to_string_lossy().into_owned();
                app.find_tokens();
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    let _ = browse;
    close
}

/// A picture's texture, made once per picture (kept in egui's memory under `key`).
fn picture_texture(ctx: &egui::Context, key: &str, img: &AppearanceImage) -> Option<egui::TextureHandle> {
    let id = egui::Id::new(("sign-appearance-texture", key));
    let tag = (img.0.bytes().as_ptr() as usize, img.0.bytes().len());
    if let Some((t, tex)) = ctx.data(|d| d.get_temp::<((usize, usize), egui::TextureHandle)>(id))
        && t == tag
    {
        return Some(tex);
    }
    // Bounded by the signer's size limit (the same scaling signing applies).
    let g = img.graphic().ok()?;
    let color = egui::ColorImage::from_rgba_unmultiplied([g.width() as usize, g.height() as usize], g.rgba());
    let tex = ctx.load_texture(format!("sign-appearance-{key}"), color, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, (tag, tex.clone())));
    Some(tex)
}

/// `w`×`h` fitted into `area`, centred, keeping its proportions.
fn fit_rect(area: Rect, w: f32, h: f32) -> Option<Rect> {
    if w <= 0.0 || h <= 0.0 || area.width() <= 1.0 || area.height() <= 1.0 {
        return None;
    }
    let k = (area.width() / w).min(area.height() / h);
    Some(Rect::from_center_size(area.center(), vec2(w * k, h * k)))
}

/// Ink strokes (signer units) fitted into `area`, as the signer draws them.
fn paint_ink(painter: &egui::Painter, area: Rect, ink: &[Vec<[u16; 2]>], color: Color32) {
    let all = || ink.iter().flatten();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in all() {
        let (x, y) = (f32::from(p[0]), f32::from(p[1]));
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    let Some(fit) = fit_rect(area, (x1 - x0).max(1.0), (y1 - y0).max(1.0)) else { return };
    let k = fit.width() / (x1 - x0).max(1.0);
    let width = (fit.height() * 0.04).clamp(1.0, 3.0);
    for s in ink {
        let pts: Vec<Pos2> = s.iter().map(|p| fit.min + vec2((f32::from(p[0]) - x0) * k, (f32::from(p[1]) - y0) * k)).collect();
        match pts.as_slice() {
            [] => {}
            [one] => {
                painter.circle_filled(*one, width / 2.0, color);
            }
            _ => {
                painter.add(egui::Shape::line(pts, Stroke::new(width, color)));
            }
        }
    }
}

/// The ink colour of a drawn signature (the signer's: a dark blue).
const INK: Color32 = Color32::from_rgb(0x14, 0x1A, 0x52);

/// The appearance preview: Acrobat's standard layout (name or graphic left, details right),
/// with the logo faint behind.
fn preview(ui: &mut egui::Ui, t: &Tokens, name: &str, d: &SignDraft) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 96.0), egui::Sense::hover());
    let bg = if t.dark() { Color32::from_gray(0xF4) } else { Color32::WHITE };
    ui.painter().rect(rect, CornerRadius::same(4), bg, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let inner = rect.shrink(10.0);
    let a = &d.appearance;
    if let Some(logo) = &d.logo
        && let Some(tex) = picture_texture(ui.ctx(), "logo", logo)
        && let Some(r) = fit_rect(inner, tex.size_vec2().x, tex.size_vec2().y)
    {
        let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(tex.id(), r, uv, Color32::from_white_alpha(64));
    }
    let look = d.look("");
    let half = Rect::from_min_size(inner.min, vec2(inner.width() * 0.5 - 6.0, inner.height()));
    let has_graphic = match &look.graphic {
        Some(AppearanceGraphic::Drawn(ink)) => {
            paint_ink(ui.painter(), half, ink, INK);
            true
        }
        Some(AppearanceGraphic::Image(img)) => {
            if let Some(tex) = picture_texture(ui.ctx(), "graphic", img)
                && let Some(r) = fit_rect(half, tex.size_vec2().x, tex.size_vec2().y)
            {
                let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(tex.id(), r, uv, Color32::WHITE);
            }
            true
        }
        None => false,
    };
    if a.name && !has_graphic {
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
    let x = if a.name || has_graphic { inner.center().x + 6.0 } else { inner.left() };
    let galley = ui.painter().layout(lines.join("\n"), theme::regular(11.0), Color32::from_gray(0x20), inner.right() - x);
    ui.painter().galley(egui::pos2(x, inner.center().y - galley.size().y / 2.0), galley, Color32::BLACK);
}

/// Strokes and points a drawing pad keeps (the signer's limits).
const PAD_MAX_STROKES: usize = sign::appearance::MAX_INK_STROKES;
const PAD_MAX_POINTS: usize = sign::appearance::MAX_INK_POINTS;

/// Draw a signature with the pointer: strokes in signer units (the pad's width is 1000).
fn drawing_pad(ui: &mut egui::Ui, t: &Tokens, ink: &mut Vec<Vec<[u16; 2]>>) {
    let w = ui.available_width().min(420.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, w / 3.0), egui::Sense::drag());
    let resp = resp.on_hover_cursor(egui::CursorIcon::Crosshair);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, tl!("Sign here with your mouse, pen or finger")));
    let bg = if t.dark() { Color32::from_gray(0xF4) } else { Color32::WHITE };
    ui.painter().rect(rect, CornerRadius::same(4), bg, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    // The baseline the signature sits on.
    let base = rect.bottom() - rect.height() * 0.25;
    ui.painter()
        .line_segment([egui::pos2(rect.left() + 12.0, base), egui::pos2(rect.right() - 12.0, base)], Stroke::new(1.0, Color32::from_gray(0xC8)));
    let points: usize = ink.iter().map(Vec::len).sum();
    if let Some(p) = resp.interact_pointer_pos()
        && rect.contains(p)
        && points < PAD_MAX_POINTS
    {
        let k = 1000.0 / rect.width().max(1.0);
        let unit = |v: f32| (v * k).round().clamp(0.0, 1000.0) as u16;
        let pt = [unit(p.x - rect.left()), unit(p.y - rect.top())];
        // Whether this drag owns the last stroke (none starts past the stroke limit).
        let owner = resp.id.with("drawing");
        if resp.drag_started() {
            let started = ink.len() < PAD_MAX_STROKES;
            if started {
                ink.push(vec![pt]);
            }
            ui.data_mut(|d| d.insert_temp(owner, started));
        } else if ui.data(|d| d.get_temp::<bool>(owner)).unwrap_or(false)
            && let Some(last) = ink.last_mut()
            && last.last().is_none_or(|q| q[0].abs_diff(pt[0]) + q[1].abs_diff(pt[1]) >= 3)
        {
            last.push(pt);
        }
    }
    let k = rect.width() / 1000.0;
    let width = 2.0;
    for s in ink.iter() {
        let pts: Vec<Pos2> = s.iter().map(|p| rect.min + vec2(f32::from(p[0]) * k, f32::from(p[1]) * k)).collect();
        match pts.as_slice() {
            [] => {}
            [one] => {
                ui.painter().circle_filled(*one, width / 2.0, INK);
            }
            _ => {
                ui.painter().add(egui::Shape::line(pts, Stroke::new(width, INK)));
            }
        }
    }
    if ink.is_empty() {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            tl!("Sign here with your mouse, pen or finger"),
            theme::regular(12.0),
            t.text_muted,
        );
    }
}

/// What the appearance part of the Sign As dialog asks of the app.
enum LookAction {
    Apply(String),
    Save(String),
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Pick {
        logo: bool,
    },
}

fn sign_as(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let ids = app.digital_ids.clone();
    let servers = app.session.timestamp_servers().to_vec();
    let saved: Vec<String> = app.session.signature_appearances().iter().map(|a| a.name.clone()).collect();
    let mut look_action = None;
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
    let in_os_key_store = is_key_store_entry(&entry.path);
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
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(tl!("Show on the left")).color(t.text_muted));
            for (choice, label) in
                [(GraphicChoice::Name, tl!("Name")), (GraphicChoice::Drawn, tl!("Drawn signature")), (GraphicChoice::Picture, tl!("Picture"))]
            {
                if widgets::pill_button(ui, label, d.graphic == choice).clicked() {
                    d.graphic = choice;
                }
            }
        });
        match d.graphic {
            GraphicChoice::Name => {}
            GraphicChoice::Drawn => {
                drawing_pad(ui, t, &mut d.ink);
                if !d.ink.is_empty() && ui.button(tl!("Clear drawing")).clicked() {
                    d.ink.clear();
                }
            }
            GraphicChoice::Picture => {
                ui.horizontal_wrapped(|ui| {
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.button(if d.picture.is_some() { tl!("Choose another picture…") } else { tl!("Choose picture…") }).clicked() {
                        look_action = Some(LookAction::Pick { logo: false });
                    }
                    #[cfg(target_arch = "wasm32")]
                    ui.label(
                        egui::RichText::new(tl!("Pictures can be chosen in the desktop app; draw your signature here instead."))
                            .small()
                            .color(t.text_muted),
                    );
                    if d.picture.is_some() && ui.button(tl!("Remove picture")).clicked() {
                        d.picture = None;
                    }
                });
            }
        }
        ui.horizontal_wrapped(|ui| {
            #[cfg(not(target_arch = "wasm32"))]
            if ui.button(if d.logo.is_some() { tl!("Change logo…") } else { tl!("Add logo…") }).clicked() {
                look_action = Some(LookAction::Pick { logo: true });
            }
            if d.logo.is_some() && ui.button(tl!("Remove logo")).clicked() {
                d.logo = None;
            }
        });
        ui.horizontal_wrapped(|ui| {
            if !saved.is_empty() {
                let shown = d.saved.clone().unwrap_or_else(|| tl!("Saved appearances").to_string());
                egui::ComboBox::from_id_salt("sign-saved-appearance").selected_text(shown).width(180.0).show_ui(ui, |ui| {
                    for n in &saved {
                        if ui.selectable_label(d.saved.as_deref() == Some(n.as_str()), n).clicked() {
                            look_action = Some(LookAction::Apply(n.clone()));
                        }
                    }
                });
            }
            ui.add(egui::TextEdit::singleline(&mut d.save_as).hint_text(tl!("Appearance name")).desired_width(150.0));
            if ui.add_enabled(!d.save_as.trim().is_empty(), egui::Button::new(tl!("Save appearance"))).clicked() {
                look_action = Some(LookAction::Save(d.save_as.trim().to_string()));
            }
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
                egui::RichText::new(if entry.path.starts_with("pkcs11:") {
                    tl!("The key is on the smart card or token, and the signature is made there.")
                } else if entry.path.starts_with("windows:") {
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
    ui.checkbox(&mut d.lock_document, tl!("Lock document after signing"));
    if d.lock_document {
        ui.label(egui::RichText::new(tl!("Every form field is locked and no later change keeps the signature valid.")).small().color(t.text_muted));
    }
    ui.add_space(6.0);
    ui.label(tl!("Timestamp (optional)"));
    crate::timestamp_ui::server_choice(ui, &servers, &mut d.timestamp_server, true);
    if d.timestamp_server.is_some() {
        ui.checkbox(&mut d.timestamp_network, tl!("Allow a timestamp request to this server"));
    }
    ui.label(egui::RichText::new(tl!("Manage timestamp servers in Preferences → Signatures.")).small().color(t.text_muted));
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
    match look_action {
        Some(LookAction::Apply(name)) => {
            if let Some(a) = app.session.signature_appearance(&name).cloned()
                && let Some(d) = app.sign_draft.as_mut()
            {
                d.apply(&a);
            }
        }
        Some(LookAction::Save(name)) => {
            if let Err(e) = app.save_sign_appearance(&name)
                && let Some(d) = app.sign_draft.as_mut()
            {
                d.error = Some(e);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        Some(LookAction::Pick { logo }) => app.pick_sign_picture(logo),
        #[cfg(target_arch = "wasm32")]
        Some(LookAction::Pick { .. }) => {}
        None => {}
    }
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
    /// Compare the version the signature in the field covers with the current document.
    CompareSigned(String),
    /// Add verification information (LTV) for every signature.
    AddVerification,
}

/// Compare Signed Version to Current Version: what changed after the signature in `field`,
/// shown above the side-by-side comparison of `signed` (the signed version) and `doc`.
#[derive(Clone, Debug)]
pub struct SignedComparison {
    pub doc: pdfcraft_engine::DocId,
    pub signed: pdfcraft_engine::DocId,
    pub field: String,
    pub result: sign::SignedVersionComparison,
}

/// The card above the Compare panel for a signed version: the verdict and each kind of change.
pub(crate) fn signed_comparison_card(ui: &mut egui::Ui, t: &Tokens, c: &SignedComparison) {
    let r = &c.result;
    let (icon, color, verdict) = if r.unreadable {
        ("circle-x", Color32::from_rgb(0xD7, 0x37, 0x3F), tl!("The signed version could not be read."))
    } else if !r.modified() {
        ("circle-check", Color32::from_rgb(0x2D, 0x9D, 0x78), tl!("Nothing has changed since it was signed."))
    } else if r.permitted() {
        ("circle-check", Color32::from_rgb(0x2D, 0x9D, 0x78), tl!("Every change since signing is permitted by the signature."))
    } else {
        ("triangle-alert", Color32::from_rgb(0xE6, 0x86, 0x19), tl!("Some changes since signing are not permitted by the signature."))
    };
    egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(icons::image(icon, 16.0, color));
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        tl!("Signed version of {field} (revision {n})"),
                        &[("field", &c.field), ("n", &r.revision.to_string())],
                    ))
                    .font(theme::semibold(12.5)),
                );
            });
            ui.add(egui::Label::new(egui::RichText::new(verdict).font(theme::regular(11.5))).wrap());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(tl!(sign::signed_version::permissions_label(r.permissions))).font(theme::regular(11.5)).color(t.text_muted),
                )
                .wrap(),
            );
            if r.signed_pages != r.current_pages {
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        tl!("Pages: {a} when signed, {b} now"),
                        &[("a", &r.signed_pages.to_string()), ("b", &r.current_pages.to_string())],
                    ))
                    .font(theme::regular(11.5)),
                );
            }
            for (kind, permitted) in r.kinds() {
                ui.horizontal(|ui| {
                    let (mark, mc) = if permitted { ("check", t.text_muted) } else { ("x", Color32::from_rgb(0xD7, 0x37, 0x3F)) };
                    ui.add(icons::image(mark, 12.0, mc));
                    ui.label(egui::RichText::new(change_label(&kind)).font(theme::regular(11.5)));
                });
            }
            if r.truncated {
                ui.label(egui::RichText::new(tl!("More changes were found than are listed.")).small().color(t.text_muted));
            }
        });
    ui.add_space(6.0);
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
        if sigs.iter().any(|s| s.signed) && widgets::pill_button(ui, tl!("Add verification information…"), false).clicked() {
            action = Some(PanelAction::AddVerification);
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
                if ui.button(tl!("Compare with current")).clicked() {
                    action = Some(PanelAction::CompareSigned(s.field.clone()));
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
