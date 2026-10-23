//! Crash-recovery entries, shared by every store: the desktop recovery folder, the web app's
//! browser storage (origin-private file system) and the `recovery_*` automation tools.
//!
//! A store is a flat set of files ([`RecoveryFiles`]). Each entry is `<key>.pdf` (the working
//! file) plus `<key>.json` ([`RecoveryMeta`]: name, original path, time, length and checksum of
//! the bytes). The bytes are written before the metadata, so a listed entry is complete.
//!
//! Stored entries are hostile input: a crash, a full disk, another program or a damaged browser
//! profile can leave them truncated, corrupt or planted. [`scan`] lists only well-formed entries
//! (keys are checked, metadata is size-capped and validated, names are cleaned) and reports the
//! rest as damaged; [`read_entry`] loads bytes only when their length and checksum match, so a
//! damaged entry opens nothing rather than a half-document.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::RecoverySnapshot;

/// The largest metadata file read (real ones are a few hundred bytes).
pub const MAX_META_BYTES: u64 = 64 * 1024;
/// Longest key kept: `[A-Za-z0-9_-]`, also the file stem.
pub const MAX_KEY_CHARS: usize = 64;
/// Longest document name kept from stored metadata.
pub const MAX_NAME_CHARS: usize = 255;
/// Longest original path kept from stored metadata (a longer one is dropped).
pub const MAX_PATH_CHARS: usize = 4096;
/// The web app keeps at most this much in browser storage (all entries together).
pub const WEB_STORE_CAP: u64 = 256 * 1024 * 1024;

/// What a recovery entry records next to the PDF bytes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecoveryMeta {
    pub key: String,
    pub name: String,
    /// Where the document was saved before (Save writes back there after recovery).
    pub path: Option<String>,
    /// Seconds since the Unix epoch when the snapshot was taken.
    pub saved_at: u64,
    /// The bytes are encrypted; recovering asks for the password.
    pub encrypted: bool,
    /// Length of the PDF bytes (entries written before checksums were added have none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub len: Option<u64>,
    /// [`checksum`] of the PDF bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

impl RecoveryMeta {
    /// Metadata for `snap` stored under `key`, taken at `saved_at`.
    pub fn for_snapshot(key: String, snap: &RecoverySnapshot, saved_at: u64) -> Self {
        Self {
            key,
            name: snap.name.clone(),
            path: snap.path.clone(),
            saved_at,
            encrypted: snap.encrypted,
            len: Some(snap.bytes.len() as u64),
            checksum: Some(checksum(&snap.bytes)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryError {
    #[error("no recovery entry {0:?}")]
    Missing(String),
    #[error("the recovery copy of {0} is damaged, so nothing was opened")]
    Damaged(String),
    #[error("{name} is too large to keep a recovery copy ({len} bytes; the store keeps {cap} bytes)")]
    TooLarge { name: String, len: u64, cap: u64 },
    #[error("couldn't write the recovery copy: {0}")]
    Write(String),
}

/// A flat set of named files: a folder, browser storage or memory.
pub trait RecoveryFiles {
    /// Names of the files in the store.
    fn names(&self) -> Vec<String>;
    /// A file's size, `None` when it is missing.
    fn size(&self, name: &str) -> Option<u64>;
    /// A file's bytes, `None` when missing, unreadable or larger than `limit`.
    fn read(&self, name: &str, limit: u64) -> Option<Arc<Vec<u8>>>;
    /// Write (or replace) a file atomically.
    fn write(&self, name: &str, bytes: &Arc<Vec<u8>>) -> Result<(), String>;
    /// Remove a file (missing is fine).
    fn remove(&self, name: &str);
}

/// A key is 1–64 of `A–Z a–z 0–9 _ -`, so it can never name another folder or file.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY_CHARS && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn meta_file(key: &str) -> String {
    format!("{key}.json")
}

pub fn pdf_file(key: &str) -> String {
    format!("{key}.pdf")
}

/// The key and kind (`true`: metadata) of a store file name; `None` for files that aren't ours.
pub fn entry_name(name: &str) -> Option<(&str, bool)> {
    let (key, meta) = match (name.strip_suffix(".json"), name.strip_suffix(".pdf")) {
        (Some(k), _) => (k, true),
        (_, Some(k)) => (k, false),
        _ => return None,
    };
    valid_key(key).then_some((key, meta))
}

/// A fast checksum (64-bit, word at a time) that catches truncated or damaged bytes; not a
/// defence against deliberate tampering (the PDF reader treats the bytes as hostile anyway).
pub fn checksum(bytes: &[u8]) -> String {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let (words, rest) = bytes.as_chunks::<8>();
    let mut h = 0xCBF2_9CE4_8422_2325u64 ^ (bytes.len() as u64).wrapping_mul(K);
    for w in words {
        h = (h ^ u64::from_le_bytes(*w)).wrapping_mul(K);
        h ^= h >> 29;
    }
    for &b in rest {
        h = (h ^ u64::from(b)).wrapping_mul(K);
        h ^= h >> 29;
    }
    format!("{h:016x}")
}

/// Text from stored metadata without control characters, at most `max` characters.
fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Parse and validate stored metadata. `None` for anything malformed: too large, not JSON, a
/// bad key or checksum. Names are cleaned and cut; an unusable original path is dropped.
pub fn parse_meta(bytes: &[u8]) -> Option<RecoveryMeta> {
    if bytes.len() as u64 > MAX_META_BYTES {
        return None;
    }
    let mut m: RecoveryMeta = serde_json::from_slice(bytes).ok()?;
    if !valid_key(&m.key) {
        return None;
    }
    if m.checksum.as_deref().is_some_and(|c| c.len() != 16 || !c.bytes().all(|b| b.is_ascii_hexdigit())) {
        return None;
    }
    m.name = clean(&m.name, MAX_NAME_CHARS);
    if m.name.trim().is_empty() {
        m.name = "Recovered.pdf".into();
    }
    m.path = m.path.filter(|p| !p.is_empty() && p.chars().count() <= MAX_PATH_CHARS && !p.chars().any(char::is_control));
    Some(m)
}

/// What [`scan`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scan {
    /// Complete entries, newest first.
    pub entries: Vec<RecoveryMeta>,
    /// Files of incomplete or damaged entries (bytes without metadata, unreadable or
    /// malformed metadata, bytes of the wrong length).
    pub damaged: Vec<String>,
}

/// List the entries of a store. Files that aren't recovery entries are left alone.
pub fn scan(files: &dyn RecoveryFiles) -> Scan {
    let names = files.names();
    let mut metas: BTreeMap<&str, ()> = BTreeMap::new();
    let mut pdfs: BTreeMap<&str, ()> = BTreeMap::new();
    for n in &names {
        match entry_name(n) {
            Some((k, true)) => metas.insert(k, ()),
            Some((k, false)) => pdfs.insert(k, ()),
            None => None,
        };
    }
    let mut out = Scan::default();
    for key in metas.keys() {
        let meta = files.read(&meta_file(key), MAX_META_BYTES).and_then(|b| parse_meta(&b)).filter(|m| m.key == *key);
        let size = files.size(&pdf_file(key));
        match (meta, size) {
            (Some(m), Some(size)) if m.len.is_none_or(|l| l == size) => out.entries.push(m),
            (_, size) => {
                out.damaged.push(meta_file(key));
                if size.is_some() {
                    out.damaged.push(pdf_file(key));
                }
            }
        }
    }
    out.damaged.extend(pdfs.keys().filter(|k| !metas.contains_key(*k)).map(|k| pdf_file(k)));
    out.entries.sort_by_key(|m| std::cmp::Reverse(m.saved_at));
    out
}

/// Remove the damaged files a [`scan`] found.
pub fn remove_damaged(files: &dyn RecoveryFiles, scan: &Scan) {
    for n in &scan.damaged {
        files.remove(n);
    }
}

/// The PDF bytes of an entry, only when their length and checksum match its metadata.
pub fn read_entry(files: &dyn RecoveryFiles, meta: &RecoveryMeta) -> Result<Vec<u8>, RecoveryError> {
    if !valid_key(&meta.key) {
        return Err(RecoveryError::Missing(meta.key.clone()));
    }
    let name = pdf_file(&meta.key);
    let size = files.size(&name).ok_or_else(|| RecoveryError::Missing(meta.key.clone()))?;
    let damaged = || RecoveryError::Damaged(meta.name.clone());
    if meta.len.is_some_and(|l| l != size) {
        return Err(damaged());
    }
    let bytes = files.read(&name, meta.len.unwrap_or(size)).ok_or_else(damaged)?;
    if meta.len.is_some_and(|l| l != bytes.len() as u64) || meta.checksum.as_deref().is_some_and(|c| c != checksum(&bytes)) {
        return Err(damaged());
    }
    Ok(Arc::unwrap_or_clone(bytes))
}

/// Bytes the store's recovery entries use, leaving out entry `except`.
pub fn used_bytes(files: &dyn RecoveryFiles, except: &str) -> u64 {
    files.names().iter().filter(|n| entry_name(n).is_some_and(|(k, _)| k != except)).filter_map(|n| files.size(n)).fold(0u64, u64::saturating_add)
}

/// Write (or replace) an entry: bytes first, then metadata. With a `cap`, an entry that would
/// take the store over it is refused (the entry it would replace is kept).
pub fn write_entry(files: &dyn RecoveryFiles, meta: &RecoveryMeta, bytes: &Arc<Vec<u8>>, cap: Option<u64>) -> Result<(), RecoveryError> {
    if !valid_key(&meta.key) {
        return Err(RecoveryError::Write(format!("invalid key {:?}", meta.key)));
    }
    let json = serde_json::to_vec_pretty(meta).map_err(|e| RecoveryError::Write(e.to_string()))?;
    if let Some(cap) = cap {
        let len = (bytes.len() as u64).saturating_add(json.len() as u64);
        if used_bytes(files, &meta.key).saturating_add(len) > cap {
            return Err(RecoveryError::TooLarge { name: meta.name.clone(), len, cap });
        }
    }
    files.write(&pdf_file(&meta.key), bytes).map_err(RecoveryError::Write)?;
    files.write(&meta_file(&meta.key), &Arc::new(json)).map_err(RecoveryError::Write)
}

/// Remove an entry (metadata first, so a half-removed entry is never listed).
pub fn remove_entry(files: &dyn RecoveryFiles, key: &str) {
    if valid_key(key) {
        files.remove(&meta_file(key));
        files.remove(&pdf_file(key));
    }
}

/// A store in memory: tests, and the web app's mirror of its browser storage.
#[derive(Clone, Debug, Default)]
pub struct MemoryFiles(Arc<Mutex<BTreeMap<String, Arc<Vec<u8>>>>>);

impl MemoryFiles {
    pub fn new() -> Self {
        Self::default()
    }

    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Arc<Vec<u8>>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Put raw bytes in (loading from browser storage, or planting damage in tests); a file
    /// already there is kept.
    pub fn insert_if_absent(&self, name: &str, bytes: Vec<u8>) {
        self.map().entry(name.to_string()).or_insert_with(|| Arc::new(bytes));
    }

    /// Replace a file's raw bytes (tests).
    pub fn put(&self, name: &str, bytes: Vec<u8>) {
        self.map().insert(name.to_string(), Arc::new(bytes));
    }

    /// Drop a file from memory only (its bytes are no longer needed here).
    pub fn forget(&self, name: &str) {
        self.map().remove(name);
    }
}

impl RecoveryFiles for MemoryFiles {
    fn names(&self) -> Vec<String> {
        self.map().keys().cloned().collect()
    }

    fn size(&self, name: &str) -> Option<u64> {
        self.map().get(name).map(|b| b.len() as u64)
    }

    fn read(&self, name: &str, limit: u64) -> Option<Arc<Vec<u8>>> {
        self.map().get(name).filter(|b| b.len() as u64 <= limit).cloned()
    }

    fn write(&self, name: &str, bytes: &Arc<Vec<u8>>) -> Result<(), String> {
        self.map().insert(name.to_string(), bytes.clone());
        Ok(())
    }

    fn remove(&self, name: &str) {
        self.map().remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocId;

    fn snap(bytes: &[u8]) -> RecoverySnapshot {
        RecoverySnapshot {
            doc: DocId(1),
            name: "work.pdf".into(),
            path: Some("/docs/work.pdf".into()),
            bytes: Arc::new(bytes.to_vec()),
            encrypted: false,
        }
    }

    fn stored(files: &MemoryFiles, key: &str, bytes: &[u8]) -> RecoveryMeta {
        let s = snap(bytes);
        let m = RecoveryMeta::for_snapshot(key.into(), &s, 100);
        write_entry(files, &m, &s.bytes, None).unwrap();
        m
    }

    #[test]
    fn entries_round_trip_and_list_newest_first() {
        let f = MemoryFiles::new();
        let a = stored(&f, "a-1", b"%PDF-1.7 one");
        let b = RecoveryMeta { saved_at: 200, ..stored(&f, "b-2", b"%PDF-1.7 two") };
        write_entry(&f, &b, &Arc::new(b"%PDF-1.7 two".to_vec()), None).unwrap();
        let s = scan(&f);
        assert!(s.damaged.is_empty());
        assert_eq!(s.entries.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(), ["b-2", "a-1"]);
        assert_eq!(read_entry(&f, &a).unwrap(), b"%PDF-1.7 one");
        remove_entry(&f, "b-2");
        assert_eq!(read_entry(&f, &b), Err(RecoveryError::Missing("b-2".into())));
        assert_eq!(scan(&f).entries.len(), 1);
    }

    #[test]
    fn truncated_or_corrupt_bytes_load_nothing() {
        let f = MemoryFiles::new();
        let m = stored(&f, "k", b"%PDF-1.7 the whole document");
        // Truncated: listed as damaged, and reading refuses.
        f.put("k.pdf", b"%PDF-1.7 the whole".to_vec());
        assert_eq!(scan(&f).damaged, ["k.json", "k.pdf"]);
        assert_eq!(read_entry(&f, &m), Err(RecoveryError::Damaged("work.pdf".into())));
        // Same length, different bytes: the checksum refuses.
        f.put("k.pdf", b"%PDF-1.7 the whole documenT".to_vec());
        assert_eq!(scan(&f).entries.len(), 1);
        assert_eq!(read_entry(&f, &m), Err(RecoveryError::Damaged("work.pdf".into())));
    }

    #[test]
    fn hostile_metadata_is_rejected_or_cleaned() {
        let f = MemoryFiles::new();
        let pdf = b"%PDF-1.7 x".to_vec();
        for (key, meta) in [
            ("a", b"".to_vec()),
            ("b", b"{not json".to_vec()),
            ("c", br#"{"key":"c"}"#.to_vec()),
            ("d", br#"{"key":"../../etc/passwd","name":"x","path":null,"saved_at":1,"encrypted":false}"#.to_vec()),
            ("e", br#"{"key":"other","name":"x","path":null,"saved_at":1,"encrypted":false}"#.to_vec()),
            ("f", br#"{"key":"f","name":"x","path":null,"saved_at":1,"encrypted":false,"checksum":"zz"}"#.to_vec()),
            ("g", br#"{"key":"g","name":"x","path":null,"saved_at":-1,"encrypted":false}"#.to_vec()),
            ("h", br#"{"key":"h","name":"x","path":null,"saved_at":1,"encrypted":false,"len":99}"#.to_vec()),
            ("i", vec![b' '; MAX_META_BYTES as usize + 1]),
            ("j", vec![0xff, 0xfe, 0x00]),
        ] {
            f.put(&meta_file(key), meta);
            f.put(&pdf_file(key), pdf.clone());
        }
        // Bytes with no metadata, and files that aren't entries at all.
        f.put("orphan.pdf", pdf.clone());
        f.put("../evil.json", b"{}".to_vec());
        f.put("notes.txt", b"keep me".to_vec());
        let s = scan(&f);
        assert!(s.entries.is_empty(), "{:?}", s.entries);
        assert_eq!(s.damaged.len(), 21);
        remove_damaged(&f, &s);
        let mut left = f.names();
        left.sort();
        assert_eq!(left, ["../evil.json", "notes.txt"]);

        // Control characters and over-long names are cleaned, unusable paths dropped.
        let long = "n".repeat(1000);
        let json = format!(r#"{{"key":"ok","name":"a\u0000b\u001b{long}","path":"/x\ny","saved_at":1,"encrypted":false}}"#);
        let m = parse_meta(json.as_bytes()).unwrap();
        assert_eq!(m.name.chars().count(), MAX_NAME_CHARS);
        assert!(m.name.starts_with("ab") && m.path.is_none());
        let m = parse_meta(br#"{"key":"ok","name":"\u0007","path":"","saved_at":1,"encrypted":false}"#).unwrap();
        assert_eq!((m.name.as_str(), m.path), ("Recovered.pdf", None));
    }

    #[test]
    fn entries_written_before_checksums_still_list_and_read() {
        let f = MemoryFiles::new();
        f.put("old.json", br#"{"key":"old","name":"w.pdf","path":"/w.pdf","saved_at":5,"encrypted":false}"#.to_vec());
        f.put("old.pdf", b"%PDF-1.4".to_vec());
        let s = scan(&f);
        assert_eq!(s.entries.len(), 1);
        assert_eq!(read_entry(&f, &s.entries[0]).unwrap(), b"%PDF-1.4");
    }

    #[test]
    fn the_cap_refuses_entries_that_do_not_fit_and_counts_replacements_once() {
        let f = MemoryFiles::new();
        let big = Arc::new(vec![b'x'; 600]);
        let s = RecoverySnapshot { bytes: big.clone(), ..snap(b"") };
        let m = RecoveryMeta::for_snapshot("one".into(), &s, 1);
        write_entry(&f, &m, &big, Some(1000)).unwrap();
        // Replacing the same entry only counts its new size.
        write_entry(&f, &m, &big, Some(1000)).unwrap();
        let other = RecoveryMeta { key: "two".into(), ..m.clone() };
        assert!(matches!(write_entry(&f, &other, &big, Some(1000)), Err(RecoveryError::TooLarge { .. })));
        assert_eq!(scan(&f).entries.len(), 1);
        assert!(used_bytes(&f, "") <= 1000);
    }

    #[test]
    fn keys_cannot_name_other_files() {
        for k in ["", "a/b", "..", "a.b", "a\\b", "é", &"k".repeat(65)] {
            assert!(!valid_key(k), "{k:?}");
        }
        assert!(valid_key("1700000000-123-4") && valid_key("web_x-9"));
        let f = MemoryFiles::new();
        let m = RecoveryMeta { key: "../x".into(), ..RecoveryMeta::for_snapshot("k".into(), &snap(b"1"), 1) };
        assert!(write_entry(&f, &m, &Arc::new(vec![1]), None).is_err());
        assert!(f.names().is_empty());
    }

    #[test]
    fn checksums_differ_for_small_changes() {
        let a = vec![7u8; 4099];
        let mut b = a.clone();
        b[4098] = 8;
        assert_ne!(checksum(&a), checksum(&b));
        assert_ne!(checksum(&a[..4098]), checksum(&a));
        assert_eq!(checksum(&a).len(), 16);
        assert_eq!(checksum(&[]), checksum(&[]));
    }
}
