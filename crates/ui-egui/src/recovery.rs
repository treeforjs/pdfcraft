//! Autosave and crash recovery, on the desktop and in the browser.
//!
//! While documents have unsaved changes, their working file is written every
//! `AUTOSAVE_SECS` (Preferences ▸ Documents changes it, or turns autosave off) to a recovery
//! store: a folder in the user's data directory on the desktop, the origin-private file system
//! of the browser in the web app. Entries, their checks against damage and the cap on browser
//! storage are shared ([`pdfcraft_engine::recovery`]). Saving, discarding or closing a document
//! removes its entry, and a clean quit leaves the store empty. Entries found at startup mean the
//! previous session ended unexpectedly (a crash, or on the web a closed tab), and the app offers
//! to recover them.
//!
//! Encrypted documents are stored encrypted (the working file is), so recovery never writes
//! plaintext of a protected document to disk.
//!
//! The browser's storage is asynchronous: writes are queued and done in order off the frame,
//! and entries are loaded once at startup (the Recovery dialog opens when they arrive). PDF bytes
//! written by this session are not kept in memory, only their sizes (for the cap).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

pub use pdfcraft_engine::recovery::RecoveryMeta;
use pdfcraft_engine::recovery::{self as rec, MemoryFiles, RecoveryError, RecoveryFiles};

use crate::PdfCraftApp;

/// How often unsaved changes are written to the recovery folder.
pub const AUTOSAVE_SECS: f64 = 60.0;

/// The most files read from browser storage at startup.
const MAX_LOADED_FILES: usize = 1000;

/// The desktop recovery folder.
#[derive(Clone, Debug)]
struct DirFiles(PathBuf);

impl RecoveryFiles for DirFiles {
    fn names(&self) -> Vec<String> {
        let Ok(dir) = std::fs::read_dir(&self.0) else { return Vec::new() };
        dir.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).filter_map(|e| e.file_name().into_string().ok()).collect()
    }

    fn size(&self, name: &str) -> Option<u64> {
        // Not through links: an entry is a plain file in the folder.
        std::fs::symlink_metadata(self.0.join(name)).ok().filter(std::fs::Metadata::is_file).map(|m| m.len())
    }

    fn read(&self, name: &str, limit: u64) -> Option<Arc<Vec<u8>>> {
        if self.size(name)? > limit {
            return None;
        }
        let bytes = std::fs::read(self.0.join(name)).ok()?;
        (bytes.len() as u64 <= limit).then(|| Arc::new(bytes))
    }

    fn write(&self, name: &str, bytes: &Arc<Vec<u8>>) -> Result<(), String> {
        std::fs::create_dir_all(&self.0).map_err(|e| e.to_string())?;
        crate::editing::write_atomically(&self.0.join(name).to_string_lossy(), bytes).map_err(|e| e.to_string())
    }

    fn remove(&self, name: &str) {
        let _ = std::fs::remove_file(self.0.join(name));
    }
}

/// Where browser-storage writes go: the origin-private file system in the web app, a
/// [`MemoryFiles`] standing in for it elsewhere (tests).
#[derive(Clone, Debug)]
enum Backing {
    Memory(MemoryFiles),
    #[cfg(target_arch = "wasm32")]
    Opfs(crate::recovery_web::Writer),
}

/// The web app's store: metadata and the PDF bytes loaded at startup in memory, sizes of the
/// PDFs this session wrote, and every change passed on to browser storage.
#[derive(Clone, Debug)]
pub(crate) struct BrowserFiles {
    files: MemoryFiles,
    written: Arc<Mutex<BTreeMap<String, u64>>>,
    loaded: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    backing: Backing,
}

impl BrowserFiles {
    fn new(backing: Backing) -> Self {
        Self { files: MemoryFiles::new(), written: Default::default(), loaded: Default::default(), failure: Default::default(), backing }
    }

    fn written(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, u64>> {
        self.written.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take in files read from browser storage: only recovery entries, at most
    /// [`MAX_LOADED_FILES`] and [`rec::WEB_STORE_CAP`] bytes together. Returns the names that
    /// were not taken (to remove: they could never be recovered).
    pub(crate) fn load(&self, found: Vec<(String, Vec<u8>)>) -> Vec<String> {
        let mut total = 0u64;
        let mut refused = Vec::new();
        for (i, (name, bytes)) in found.into_iter().enumerate() {
            if rec::entry_name(&name).is_none() {
                continue;
            }
            let next = total.saturating_add(bytes.len() as u64);
            if i >= MAX_LOADED_FILES || next > rec::WEB_STORE_CAP {
                refused.push(name);
                continue;
            }
            total = next;
            self.files.insert_if_absent(&name, bytes);
        }
        refused
    }

    pub(crate) fn finish_loading(&self, failure: Option<String>) {
        if let Some(f) = failure {
            *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(f);
        }
        self.loaded.store(true, Ordering::Release);
    }

    fn pass_on_write(&self, name: &str, bytes: &Arc<Vec<u8>>) {
        match &self.backing {
            Backing::Memory(m) => m.put(name, bytes.to_vec()),
            #[cfg(target_arch = "wasm32")]
            Backing::Opfs(w) => w.push(crate::recovery_web::Op::Write(name.to_string(), bytes.clone())),
        }
    }

    pub(crate) fn pass_on_remove(&self, name: &str) {
        match &self.backing {
            Backing::Memory(m) => m.remove(name),
            #[cfg(target_arch = "wasm32")]
            Backing::Opfs(w) => w.push(crate::recovery_web::Op::Remove(name.to_string())),
        }
    }

    /// Stop holding loaded PDF bytes (the user has decided about them); their sizes still count.
    fn release_loaded(&self) {
        for name in self.files.names() {
            if name.ends_with(".pdf")
                && let Some(size) = self.files.size(&name)
            {
                self.written().insert(name.clone(), size);
                self.files.forget(&name);
            }
        }
    }
}

impl RecoveryFiles for BrowserFiles {
    fn names(&self) -> Vec<String> {
        let mut names = self.files.names();
        names.extend(self.written().keys().cloned());
        names.sort();
        names.dedup();
        names
    }

    fn size(&self, name: &str) -> Option<u64> {
        self.written().get(name).copied().or_else(|| self.files.size(name))
    }

    fn read(&self, name: &str, limit: u64) -> Option<Arc<Vec<u8>>> {
        self.files.read(name, limit)
    }

    fn write(&self, name: &str, bytes: &Arc<Vec<u8>>) -> Result<(), String> {
        if name.ends_with(".pdf") {
            self.written().insert(name.to_string(), bytes.len() as u64);
            self.files.forget(name);
        } else {
            self.files.write(name, bytes)?;
        }
        self.pass_on_write(name, bytes);
        Ok(())
    }

    fn remove(&self, name: &str) {
        self.files.remove(name);
        self.written().remove(name);
        self.pass_on_remove(name);
    }
}

#[derive(Clone, Debug)]
enum Backend {
    Dir(DirFiles),
    Browser(BrowserFiles),
}

/// A recovery store: the desktop folder, or browser storage in the web app.
#[derive(Clone, Debug)]
pub struct RecoveryStore {
    backend: Backend,
}

impl RecoveryStore {
    /// The recovery folder `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { backend: Backend::Dir(DirFiles(dir.into())) }
    }

    /// A store that behaves like the web app's, over `storage` standing in for the browser's:
    /// what it holds is loaded (and checked) now, and changes are written through to it, with
    /// the browser's cap ([`rec::WEB_STORE_CAP`]). For tests of the web behaviour.
    pub fn in_browser_storage(storage: MemoryFiles) -> Self {
        let files = BrowserFiles::new(Backing::Memory(storage.clone()));
        let found: Vec<(String, Vec<u8>)> =
            storage.names().into_iter().filter_map(|n| storage.read(&n, rec::WEB_STORE_CAP).map(|b| (n, b.to_vec()))).collect();
        for name in files.load(found) {
            storage.remove(&name);
        }
        files.finish_loading(None);
        Self { backend: Backend::Browser(files) }
    }

    /// The web app's store in the browser's origin-private file system. Entries arrive
    /// asynchronously; the app shows the Recovery dialog once they are loaded.
    #[cfg(target_arch = "wasm32")]
    pub fn browser(ctx: &egui::Context) -> Self {
        let writer = crate::recovery_web::Writer::default();
        let files = BrowserFiles::new(Backing::Opfs(writer));
        crate::recovery_web::start_loading(files.clone(), ctx.clone());
        Self { backend: Backend::Browser(files) }
    }

    /// `Recovery` in the portable data folder in portable mode ([`crate::portable`]), otherwise
    /// the platform's per-user data folder: `~/Library/Application Support/PdfCraft/Recovery`
    /// (macOS), `%LOCALAPPDATA%\PdfCraft\Recovery` (Windows), or
    /// `$XDG_DATA_HOME/pdfcraft/recovery` / `~/.local/share/pdfcraft/recovery` (others).
    pub fn default_dir() -> Option<PathBuf> {
        if let Some(dir) = crate::portable::data_dir() {
            return Some(dir.join("Recovery"));
        }
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        if cfg!(target_os = "macos") {
            env("HOME").map(|h| h.join("Library/Application Support/PdfCraft/Recovery"))
        } else if cfg!(windows) {
            env("LOCALAPPDATA").map(|d| d.join("PdfCraft").join("Recovery"))
        } else {
            env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share"))).map(|d| d.join("pdfcraft/recovery"))
        }
    }

    /// The recovery folder (empty for browser storage).
    pub fn dir(&self) -> &Path {
        match &self.backend {
            Backend::Dir(d) => &d.0,
            Backend::Browser(_) => Path::new(""),
        }
    }

    /// The store is the browser's (the web app, or a stand-in for it).
    pub fn is_browser(&self) -> bool {
        matches!(self.backend, Backend::Browser(_))
    }

    fn files(&self) -> &dyn RecoveryFiles {
        match &self.backend {
            Backend::Dir(d) => d,
            Backend::Browser(b) => b,
        }
    }

    /// Browser storage is capped; the desktop folder is not.
    fn cap(&self) -> Option<u64> {
        self.is_browser().then_some(rec::WEB_STORE_CAP)
    }

    /// Write (or replace) an entry: the bytes before their metadata, so a listed entry always
    /// has complete bytes. In the browser, an entry that would go over the cap is refused.
    pub fn write(&self, meta: &RecoveryMeta, bytes: &Arc<Vec<u8>>) -> Result<(), RecoveryError> {
        rec::write_entry(self.files(), meta, bytes, self.cap())
    }

    pub fn remove(&self, key: &str) {
        rec::remove_entry(self.files(), key);
    }

    /// Complete entries, newest first. Incomplete or damaged leftovers are removed.
    pub fn list(&self) -> Vec<RecoveryMeta> {
        let scan = rec::scan(self.files());
        rec::remove_damaged(self.files(), &scan);
        scan.entries
    }

    /// The bytes of entry `key`, only when they are complete and undamaged.
    pub fn read(&self, key: &str) -> Result<Vec<u8>, RecoveryError> {
        let meta = rec::scan(self.files()).entries.into_iter().find(|m| m.key == key).ok_or_else(|| RecoveryError::Missing(key.to_string()))?;
        rec::read_entry(self.files(), &meta)
    }

    /// Remove every entry (clean quit, or "Discard all").
    pub fn clear(&self) {
        for m in self.list() {
            self.remove(&m.key);
        }
    }

    /// Entries are available: always for a folder, once loaded for browser storage.
    pub fn loaded(&self) -> bool {
        match &self.backend {
            Backend::Dir(_) => true,
            Backend::Browser(b) => b.loaded.load(Ordering::Acquire),
        }
    }

    /// Why browser storage can't be used, once (then `None`).
    fn take_failure(&self) -> Option<String> {
        match &self.backend {
            Backend::Dir(_) => None,
            Backend::Browser(b) => b.failure.lock().unwrap_or_else(PoisonError::into_inner).take(),
        }
    }

    fn release_loaded(&self) {
        if let Backend::Browser(b) = &self.backend {
            b.release_loaded();
        }
    }
}

/// Seconds since the Unix epoch (the browser's clock in the web app, where `SystemTime` has none).
pub(crate) fn wall_secs() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let ms = js_sys::Date::now();
        if ms.is_finite() && ms > 0.0 { (ms / 1000.0) as u64 } else { 0 }
    }
}

/// Tells this run's recovery keys apart from another run's: the process id on the desktop, a
/// random number in the browser (which has no process ids).
fn run_tag() -> u64 {
    static TAG: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *TAG.get_or_init(|| {
        #[cfg(not(target_arch = "wasm32"))]
        {
            u64::from(std::process::id())
        }
        #[cfg(target_arch = "wasm32")]
        {
            (js_sys::Math::random() * 4_294_967_295.0) as u64
        }
    })
}

impl PdfCraftApp {
    /// Turn on autosave into `store`, and look for documents a previous session left behind
    /// (browser storage: once they are loaded, see [`Self::poll_recovery`]).
    pub fn enable_recovery(&mut self, store: RecoveryStore) {
        let ready = store.loaded();
        self.recovery = Some(store);
        if ready {
            self.recovery_loaded = true;
            self.offer_recoverable(false);
        }
    }

    /// List what the store holds from earlier sessions; with entries, open the Recovery dialog
    /// (unless another dialog is open). Returns whether anything is recoverable.
    pub(crate) fn offer_recoverable(&mut self, always: bool) -> bool {
        let Some(store) = &self.recovery else { return false };
        let mine: Vec<&String> = self.recovery_keys.values().collect();
        self.recoverable = store.list().into_iter().filter(|m| !mine.contains(&&m.key)).collect();
        if self.recoverable.is_empty() {
            store.release_loaded();
            return false;
        }
        if always || self.dialog.is_none() {
            self.dialog = Some(crate::Dialog::Recovery);
        }
        true
    }

    /// Browser storage: show what was recovered once it has loaded (called every frame; a flag
    /// check until then).
    pub(crate) fn poll_recovery(&mut self) {
        let Some(store) = &self.recovery else { return };
        if self.recovery_loaded || !store.loaded() {
            return;
        }
        self.recovery_loaded = true;
        if let Some(why) = store.take_failure() {
            log::warn!("browser recovery storage unavailable: {why}");
            self.notify_fmt("This browser can't keep recovery copies of unsaved changes: {e}", &[("e", &why)]);
        }
        self.offer_recoverable(false);
    }

    /// Write snapshots of documents changed since the last autosave (called periodically).
    pub fn autosave_now(&mut self) {
        let Some(store) = self.recovery.clone() else { return };
        for snap in self.session.autosave_snapshots() {
            let key = self.recovery_keys.entry(snap.doc).or_insert_with(|| format!("{}-{}-{}", wall_secs(), run_tag(), snap.doc.0)).clone();
            let meta = RecoveryMeta::for_snapshot(key, &snap, wall_secs());
            match store.write(&meta, &snap.bytes) {
                Ok(()) => {
                    self.recovery_too_large.remove(&snap.doc);
                }
                // Said once per document; the entry it would have replaced stays.
                Err(RecoveryError::TooLarge { .. }) => {
                    if self.recovery_too_large.insert(snap.doc) {
                        self.notify_fmt("{name} is too large to keep a recovery copy in the browser", &[("name", &snap.name)]);
                    }
                }
                Err(e) => log::warn!("autosave failed: {e}"),
            }
        }
    }

    /// Drop a document's recovery entry (it was saved, discarded or closed).
    pub(crate) fn forget_recovery(&mut self, doc: pdfcraft_engine::DocId) {
        self.recovery_too_large.remove(&doc);
        if let (Some(store), Some(key)) = (&self.recovery, self.recovery_keys.remove(&doc)) {
            store.remove(&key);
        }
    }

    /// Seconds between autosaves (Preferences ▸ Documents; [`AUTOSAVE_SECS`] by default), or
    /// `None` when autosave is turned off.
    pub fn autosave_interval(&self) -> Option<f64> {
        let minutes = self.session.prefs().autosave_minutes;
        (minutes > 0).then(|| f64::from(minutes) * 60.0)
    }

    /// Autosave when the interval has passed (called every frame: a few comparisons unless a
    /// snapshot is due). While changes wait for their snapshot, a repaint is asked for when it
    /// is due, so an idle window still autosaves.
    pub(crate) fn autosave_tick(&mut self, now: f64, ctx: Option<&egui::Context>) {
        if self.recovery.is_none() {
            return;
        }
        self.poll_recovery();
        let Some(interval) = self.autosave_interval() else { return };
        if now - self.last_autosave >= interval {
            self.last_autosave = now;
            self.autosave_now();
        } else if let Some(ctx) = ctx
            && self.session.has_unsnapshotted_changes()
        {
            let wait = (self.last_autosave + interval - now).clamp(0.0, interval);
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
        }
    }

    /// Reopen recovered documents (`keys`), as unsaved changes at their original paths. A
    /// damaged entry opens nothing and says so.
    pub fn recover(&mut self, keys: &[String]) {
        let Some(store) = self.recovery.clone() else { return };
        for key in keys {
            let Some(meta) = self.recoverable.iter().find(|m| &m.key == key).cloned() else { continue };
            let bytes = match rec::read_entry(store.files(), &meta) {
                Ok(b) => b,
                Err(e) => {
                    self.notify_fmt("Couldn't recover {name}: {e}", &[("name", &meta.name), ("e", &e.to_string())]);
                    if matches!(e, RecoveryError::Damaged(_)) {
                        store.remove(key);
                    }
                    continue;
                }
            };
            if meta.encrypted {
                // The password prompt opens it; mark it recovered once it is open.
                self.pending_recovered = Some(meta.clone());
            }
            match self.open_bytes(&meta.name, None, bytes) {
                Ok(()) if self.password_prompt.is_none() => self.finish_recovery(&meta),
                Ok(()) => {}
                Err(e) => self.notify_fmt("Couldn't recover {name}: {e}", &[("name", &meta.name), ("e", &e.to_string())]),
            }
        }
        self.recoverable.retain(|m| !keys.contains(&m.key));
        if self.recoverable.is_empty() {
            store.release_loaded();
        }
    }

    /// The most recently opened tab came from `meta`: keep its recovery entry and path.
    pub(crate) fn finish_recovery(&mut self, meta: &RecoveryMeta) {
        let Some((_, id)) = self.active_ids() else { return };
        self.session.mark_recovered(id, meta.path.clone());
        self.recovery_keys.insert(id, meta.key.clone());
        self.pending_recovered = None;
    }

    /// Delete recovery entries the user chose not to recover.
    pub fn discard_recovered(&mut self, keys: &[String]) {
        if let Some(store) = &self.recovery {
            for k in keys {
                store.remove(k);
            }
        }
        self.recoverable.retain(|m| !keys.contains(&m.key));
        if self.recoverable.is_empty()
            && let Some(store) = &self.recovery
        {
            store.release_loaded();
        }
    }

    /// File ▸ Recover unsaved documents: show the Recovery dialog again (closing it keeps the
    /// entries), or say there is nothing to recover.
    pub(crate) fn recover_command(&mut self) {
        match &self.recovery {
            None => self.notify_tr("Autosave is off, so there is nothing to recover"),
            Some(s) if !s.loaded() => self.notify_tr("Still looking for recovery copies…"),
            Some(_) => {
                if !self.offer_recoverable(true) {
                    self.notify_tr("Nothing to recover");
                }
            }
        }
    }

    /// File ▸ Save recovery copies now.
    pub(crate) fn autosave_command(&mut self) {
        if self.recovery.is_none() {
            self.notify_tr("Autosave is off, so there is nothing to recover");
            return;
        }
        let pending = self.session.has_unsnapshotted_changes();
        self.autosave_now();
        self.notify_tr(if pending { "Saved recovery copies of unsaved changes" } else { "Recovery copies are up to date" });
    }

    /// Clean shutdown: documents were saved or discarded, so nothing needs recovering.
    pub fn shutdown_recovery(&mut self) {
        if self.first_dirty().is_none()
            && let Some(store) = &self.recovery
        {
            for key in self.recovery_keys.values() {
                store.remove(key);
            }
            self.recovery_keys.clear();
        }
    }
}
