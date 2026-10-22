//! Crash recovery headlessly (`recovery_snapshot`, `recovery_list`, `recovery_restore`,
//! `recovery_discard`): the same entries, checks and layout as the desktop app's recovery
//! folder and the web app's browser store ([`pdfcraft_engine::recovery`]), in a folder the
//! caller names (confined to the root).

use std::path::PathBuf;
use std::sync::Arc;

use pdfcraft_engine::recovery::{self as rec, RecoveryFiles, RecoveryMeta};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed, summary};

/// A recovery folder: files are written through the automation's atomic write.
struct Folder(PathBuf);

impl RecoveryFiles for Folder {
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

    fn write(&self, name: &str, bytes: &Arc<Vec<u8>>) -> std::result::Result<(), String> {
        crate::write_atomic(&self.0.join(name), bytes).map_err(|e| e.to_string())
    }

    fn remove(&self, name: &str) {
        let _ = std::fs::remove_file(self.0.join(name));
    }
}

fn entry_json(files: &Folder, m: &RecoveryMeta) -> Value {
    json!({
        "key": m.key,
        "name": m.name,
        "path": m.path,
        "saved_at": m.saved_at,
        "encrypted": m.encrypted,
        "bytes": files.size(&rec::pdf_file(&m.key)),
    })
}

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

fn check_key(key: &str) -> Result<()> {
    if rec::valid_key(key) { Ok(()) } else { Err(bad(format!("key {key:?} is not a recovery key (1–64 of A–Z, a–z, 0–9, _ and -)"))) }
}

fn now_secs(session: &pdfcraft_engine::Session) -> u64 {
    u64::try_from(session.now_secs()).unwrap_or(0)
}

impl Automation {
    fn recovery_folder(&self, a: &Args, for_write: bool) -> Result<Folder> {
        let dir = self.resolve(a.str("dir")?, for_write)?;
        if dir.exists() && !dir.is_dir() {
            return Err(failed(format!("{}: is a file, not a folder", dir.display())));
        }
        Ok(Folder(dir))
    }

    /// Write recovery entries for documents changed since their last snapshot (like the
    /// app's autosave).
    pub(crate) fn recovery_snapshot(&mut self, a: &Args) -> Result<Value> {
        let folder = self.recovery_folder(a, true)?;
        let mut written = Vec::new();
        for snap in self.session.autosave_snapshots() {
            let now = now_secs(&self.session);
            let key = self.recovery_keys.entry(snap.doc).or_insert_with(|| format!("{now}-{}-{}", std::process::id(), snap.doc.0)).clone();
            let meta = RecoveryMeta::for_snapshot(key, &snap, now);
            if let Err(e) = rec::write_entry(&folder, &meta, &snap.bytes, None) {
                return Err(failed(e));
            }
            written.push(json!({ "key": meta.key, "doc": snap.doc.0, "name": meta.name, "bytes": snap.bytes.len(), "encrypted": meta.encrypted }));
        }
        Ok(json!({ "written": written }))
    }

    /// Complete entries, newest first, and the files of damaged ones (`recovery_discard` with
    /// all removes those).
    pub(crate) fn recovery_list(&self, a: &Args) -> Result<Value> {
        let folder = self.recovery_folder(a, false)?;
        let scan = rec::scan(&folder);
        let entries: Vec<Value> = scan.entries.iter().map(|m| entry_json(&folder, m)).collect();
        Ok(json!({ "entries": entries, "damaged": scan.damaged }))
    }

    /// Open an entry as an unsaved document at its original path. A damaged entry opens
    /// nothing.
    pub(crate) fn recovery_restore(&mut self, a: &Args) -> Result<Value> {
        let key = a.str("key")?;
        check_key(key)?;
        let folder = self.recovery_folder(a, false)?;
        let meta = rec::scan(&folder)
            .entries
            .into_iter()
            .find(|m| m.key == key)
            .ok_or_else(|| failed(format!("no complete recovery entry {key:?} (recovery_list shows them)")))?;
        if meta.encrypted && a.opt_str("password")?.is_none() {
            return Err(bad(format!("{} is password-protected: pass its password", meta.name)));
        }
        let bytes = rec::read_entry(&folder, &meta).map_err(failed)?;
        let id = self.session.open(meta.name.clone(), None, Arc::new(bytes), a.opt_str("password")?).map_err(failed)?;
        self.session.mark_recovered(id, meta.path.clone());
        self.recovery_keys.insert(id, meta.key.clone());
        let doc = self.session.get(id).ok_or_else(|| failed("the document vanished"))?;
        let mut out = summary(doc);
        out["key"] = json!(meta.key);
        Ok(out)
    }

    /// Delete entries (`key`, or `all`).
    pub(crate) fn recovery_discard(&mut self, a: &Args) -> Result<Value> {
        let folder = self.recovery_folder(a, false)?;
        let mut damaged = Vec::new();
        let keys: Vec<String> = match (a.opt_str("key")?, a.opt_bool("all")?.unwrap_or(false)) {
            (Some(_), true) => return Err(bad("pass key or all, not both")),
            (Some(k), false) => {
                check_key(k)?;
                if folder.size(&rec::meta_file(k)).is_none() && folder.size(&rec::pdf_file(k)).is_none() {
                    return Err(failed(format!("no recovery entry {k:?}")));
                }
                vec![k.to_string()]
            }
            (None, true) => {
                let scan = rec::scan(&folder);
                rec::remove_damaged(&folder, &scan);
                damaged = scan.damaged;
                scan.entries.into_iter().map(|m| m.key).collect()
            }
            (None, false) => return Err(bad("pass key (one entry) or all: true")),
        };
        for k in &keys {
            rec::remove_entry(&folder, k);
        }
        self.recovery_keys.retain(|_, k| !keys.contains(k));
        Ok(json!({ "discarded": keys, "damaged_removed": damaged }))
    }
}
