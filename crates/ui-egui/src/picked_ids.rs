//! Digital ID files held in memory instead of on disk (sign.web-signing): a .p12/.pfx the user
//! picks in the browser, or one the web app has just created. The browser gives a page no paths,
//! so the file's bytes (still password-protected) are kept for as long as the page is open and
//! never written to its settings; the password is asked for at each signing and never kept.
//! The desktop app takes paths, but the same store backs `add_digital_id_bytes` there too.

use std::sync::Arc;

use pdfcraft_engine::sign::{self, Certificate, DigitalId};

use crate::PdfCraftApp;

/// The [`crate::DigitalIdEntry::path`] of an ID held here: `picked:<n>:<file name>`.
pub const PREFIX: &str = "picked:";

/// How many picked ID files are held at once; adding another forgets the oldest.
pub const MAX_PICKED_IDS: usize = 16;

/// A picked file's bytes. Its `Debug` shows only the length, never the (encrypted) key.
#[derive(Clone)]
pub struct IdFileBytes(pub Arc<Vec<u8>>);

impl std::fmt::Debug for IdFileBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IdFileBytes({} bytes)", self.0.len())
    }
}

impl PartialEq for IdFileBytes {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

/// The ID files of this session, oldest first.
#[derive(Default)]
pub struct PickedIds {
    files: Vec<(String, Arc<Vec<u8>>)>,
    next: u64,
    #[cfg(target_arch = "wasm32")]
    pub(crate) inbox: web::Inbox,
}

impl PickedIds {
    /// The bytes of the ID file `key` (a `picked:` path), if it's still held.
    pub fn bytes(&self, key: &str) -> Option<Arc<Vec<u8>>> {
        self.files.iter().find(|(k, _)| k == key).map(|(_, b)| b.clone())
    }

    /// How many ID files are held.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Hold `bytes` as `name`; the same bytes again keep their key. Returns the key and, when the
    /// store was full, the key of the file it forgot.
    fn insert(&mut self, name: &str, bytes: Arc<Vec<u8>>) -> (String, Option<String>) {
        if let Some((k, _)) = self.files.iter().find(|(_, b)| **b == *bytes) {
            return (k.clone(), None);
        }
        self.next = self.next.wrapping_add(1);
        let key = format!("{PREFIX}{}:{}", self.next, sign::id_file::display_name(name));
        let forgot = (self.files.len() >= MAX_PICKED_IDS).then(|| self.files.remove(0).0);
        self.files.push((key.clone(), bytes));
        (key, forgot)
    }
}

/// Whether a digital ID entry is held in memory (and so isn't saved with the settings).
pub fn is_picked(path: &str) -> bool {
    path.starts_with(PREFIX)
}

fn open_error(e: sign::SignError) -> String {
    match e {
        sign::SignError::WrongPassword => "The password is incorrect.".to_string(),
        e => e.to_string(),
    }
}

impl PdfCraftApp {
    /// Add a digital ID file from its bytes (picked in the browser, or created in the web app):
    /// opened with `password` to check it and read its certificate, then held for the session.
    /// The password is not kept. Returns the ID's index in [`PdfCraftApp::digital_ids`].
    pub fn add_digital_id_bytes(&mut self, name: &str, bytes: Arc<Vec<u8>>, password: &str) -> Result<usize, String> {
        let id = pdfcraft_engine::guard(|| sign::id_file::open(&bytes, password))
            .map_err(|m| format!("The digital ID couldn't be read ({m})."))?
            .map_err(open_error)?;
        Ok(self.hold_digital_id(name, bytes, &id.certificate))
    }

    /// Hold an ID file whose certificate is known, listing it among the digital IDs.
    pub(crate) fn hold_digital_id(&mut self, name: &str, bytes: Arc<Vec<u8>>, cert: &Certificate) -> usize {
        let (key, forgot) = self.picked_ids.insert(name, bytes);
        if let Some(old) = forgot {
            self.digital_ids.retain(|e| e.path != old);
        }
        self.add_digital_id(&key, cert)
    }

    /// Open the held ID file `key` with `password`.
    pub(crate) fn open_picked_id(&self, key: &str, password: &str) -> Result<DigitalId, String> {
        let bytes = self
            .picked_ids
            .bytes(key)
            .ok_or("Choose the digital ID file again: files picked in the browser are kept only while the page is open.")?;
        pdfcraft_engine::guard(|| sign::id_file::open(&bytes, password))
            .map_err(|m| format!("The digital ID couldn't be read ({m})."))?
            .map_err(open_error)
    }
}

/// The browser's file picker for digital ID files.
#[cfg(target_arch = "wasm32")]
pub(crate) mod web {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use pdfcraft_engine::sign::id_file::MAX_ID_FILE_BYTES;

    use super::IdFileBytes;
    use crate::PdfCraftApp;

    #[derive(Clone, Default)]
    pub(crate) struct Inbox {
        busy: Arc<AtomicBool>,
        picked: Arc<Mutex<Option<Picked>>>,
    }

    struct Picked {
        epoch: u64,
        name: String,
        bytes: Result<Vec<u8>, String>,
    }

    fn too_large() -> String {
        format!("The digital ID file is larger than {} KiB.", MAX_ID_FILE_BYTES / 1024)
    }

    impl PdfCraftApp {
        /// Configure a Digital ID ▸ Browse…: read the picked file in the page, answering only into
        /// the dialog that asked for it.
        pub(crate) fn pick_digital_id_file(&mut self) {
            if self.picked_ids.inbox.busy.swap(true, Ordering::SeqCst) {
                self.notify_tr("Another file dialog is still open. Finish with it first.");
                return;
            }
            let epoch = self.dialog_epoch();
            let inbox = self.picked_ids.inbox.clone();
            let ctx = self.ctx.clone();
            let dialog = rfd::AsyncFileDialog::new().add_filter(tl!("Digital ID"), &["p12", "pfx"]);
            wasm_bindgen_futures::spawn_local(async move {
                let picked = match dialog.pick_file().await {
                    None => None,
                    Some(file) => {
                        let name = file.file_name();
                        // The size is checked before reading, and again on what was read.
                        let bytes = if file.inner().size() > MAX_ID_FILE_BYTES as f64 {
                            Err(too_large())
                        } else {
                            match wasm_bindgen_futures::JsFuture::from(file.inner().array_buffer()).await {
                                Ok(buffer) => {
                                    let bytes = js_sys::Uint8Array::new(&buffer);
                                    if bytes.length() as usize > MAX_ID_FILE_BYTES { Err(too_large()) } else { Ok(bytes.to_vec()) }
                                }
                                Err(error) => Err(format!("The browser couldn't read the file ({error:?}).")),
                            }
                        };
                        Some(Picked { epoch, name, bytes })
                    }
                };
                match picked {
                    Some(p) => *inbox.picked.lock().unwrap_or_else(PoisonError::into_inner) = Some(p),
                    None => inbox.busy.store(false, Ordering::SeqCst),
                }
                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            });
        }

        /// A picked file arrives in the Configure step it was picked from (once per frame).
        pub(crate) fn process_picked_ids(&mut self) {
            let picked = self.picked_ids.inbox.picked.lock().unwrap_or_else(PoisonError::into_inner).take();
            let Some(picked) = picked else { return };
            self.picked_ids.inbox.busy.store(false, Ordering::SeqCst);
            if self.dialog_epoch() != picked.epoch || self.dialog != Some(crate::Dialog::Sign) {
                return;
            }
            let Some(d) = self.sign_draft.as_mut() else { return };
            match picked.bytes {
                Ok(bytes) => {
                    d.new_id.file = pdfcraft_engine::sign::id_file::display_name(&picked.name);
                    d.new_id.file_bytes = Some(IdFileBytes(Arc::new(bytes)));
                    d.error = None;
                }
                Err(e) => d.error = Some(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfcraft_engine::sign::{Name, PrivateKey, Time};

    fn p12(name: &str, password: &str) -> Vec<u8> {
        let key = PrivateKey::generate_p256().unwrap();
        let dn = Name::build(name, "", "", "", "");
        let cert = Certificate::self_signed(&dn, &key, Time::from_unix(1_700_000_000), 5, &[7]).unwrap();
        sign::pkcs12::write(&DigitalId { key, certificate: cert, chain: Vec::new(), friendly_name: None }, password).unwrap()
    }

    #[test]
    fn picked_ids_are_held_opened_and_never_saved() {
        let mut app = PdfCraftApp::new();
        let bytes = Arc::new(p12("Ada Lovelace", "secret1"));
        let i = app.add_digital_id_bytes("C:\\keys\\ada.p12", bytes.clone(), "secret1").unwrap();
        let entry = app.digital_ids[i].clone();
        assert!(is_picked(&entry.path) && entry.path.ends_with(":ada.p12"), "{}", entry.path);
        assert_eq!(entry.name, "Ada Lovelace");
        // The same file again is the same entry.
        assert_eq!(app.add_digital_id_bytes("again.p12", bytes, "secret1").unwrap(), i);
        assert_eq!((app.digital_ids.len(), app.picked_ids.len()), (1, 1));
        assert_eq!(app.open_picked_id(&entry.path, "secret1").unwrap().certificate.display_name(), "Ada Lovelace");
        assert_eq!(app.open_picked_id(&entry.path, "wrong").unwrap_err(), "The password is incorrect.");
        // Held IDs are not written to the settings: their bytes would be gone on the next load.
        let saved: serde_json::Value = serde_json::from_str(&app.persist()).unwrap();
        assert!(saved["digital_ids"].as_array().unwrap().is_empty(), "{saved}");
        // A settings file naming one (hostile, or from an older session) lists nothing.
        let mut other = PdfCraftApp::new();
        let mut v = saved.clone();
        v["digital_ids"] = serde_json::json!([entry]);
        other.restore(&v.to_string());
        assert!(other.digital_ids.is_empty());
        assert!(other.open_picked_id(&entry.path, "secret1").unwrap_err().starts_with("Choose the digital ID file again"));
    }

    #[test]
    fn hostile_files_are_refused_without_listing() {
        let mut app = PdfCraftApp::new();
        let good = p12("Grace Hopper", "secret1");
        for bad in [Vec::new(), b"not a p12".to_vec(), good[..good.len() / 2].to_vec(), vec![0x30; sign::id_file::MAX_ID_FILE_BYTES + 1]] {
            assert!(app.add_digital_id_bytes("bad.p12", Arc::new(bad), "secret1").is_err());
        }
        assert_eq!(app.add_digital_id_bytes("good.p12", Arc::new(good.clone()), "nope").unwrap_err(), "The password is incorrect.");
        assert!(app.digital_ids.is_empty() && app.picked_ids.is_empty());
        // A name that tries to look like something else is cleaned.
        let i = app.add_digital_id_bytes("../../\u{202E}x\n.p12", Arc::new(good), "secret1").unwrap();
        assert!(app.digital_ids[i].path.ends_with(":x.p12"), "{}", app.digital_ids[i].path);
    }

    const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] >> endobj
trailer << /Root 1 0 R >>
%%EOF";

    /// The web app's signing path (no file to save to): an RSA ID from picked bytes signs the
    /// document in memory, which then shows the signed file with a valid-but-untrusted signature.
    #[test]
    fn a_picked_rsa_id_signs_in_memory() {
        let mut app = PdfCraftApp::new();
        app.open_bytes("contract.pdf", None, FIXTURE.to_vec()).unwrap();
        let doc = app.views[0].id;
        let key = PrivateKey::generate_rsa(2048).unwrap();
        let cert =
            Certificate::self_signed(&Name::build("Rosalind Franklin", "", "", "", ""), &key, Time::from_unix(1_700_000_000), 5, &[9]).unwrap();
        let p12 = sign::pkcs12::write(&DigitalId { key, certificate: cert, chain: Vec::new(), friendly_name: None }, "secret1").unwrap();
        let i = app.add_digital_id_bytes("rosalind.pfx", Arc::new(p12), "secret1").unwrap();
        let path = app.digital_ids[i].path.clone();
        let id = app.open_picked_id(&path, "secret1").unwrap();
        let opts =
            pdfcraft_engine::SignOptions { page: 0, rect: Some([20.0, 20.0, 200.0, 70.0]), reason: Some("Approved".into()), ..Default::default() };
        let signed = app.sign_in_memory(doc, &id, opts).unwrap();
        let d = app.session.get(doc).unwrap();
        assert_eq!(*d.bytes, *signed, "the document is the signed file");
        let sig = d.signatures.iter().find(|s| s.signed).expect("signed");
        assert_eq!((sig.status, sig.reason.as_deref()), (pdfcraft_engine::SignatureStatus::Unknown, Some("Approved")));
        assert_eq!(app.right, Some(crate::RightPanel::Signatures));
    }

    #[test]
    fn the_store_is_capped_and_forgets_the_oldest() {
        let mut app = PdfCraftApp::new();
        let first = Arc::new(p12("First", "secret1"));
        let i = app.add_digital_id_bytes("first.p12", first, "secret1").unwrap();
        let first_key = app.digital_ids[i].path.clone();
        for n in 0..MAX_PICKED_IDS {
            app.add_digital_id_bytes(&format!("{n}.p12"), Arc::new(p12(&format!("Signer {n}"), "secret1")), "secret1").unwrap();
        }
        assert_eq!(app.picked_ids.len(), MAX_PICKED_IDS);
        assert_eq!(app.digital_ids.len(), MAX_PICKED_IDS);
        assert!(!app.digital_ids.iter().any(|e| e.path == first_key), "the oldest is forgotten, with its entry");
        assert!(app.open_picked_id(&first_key, "secret1").is_err());
    }
}
