//! The web app's recovery store in the browser's origin-private file system (OPFS), through
//! web-sys: a `pdfcraft-recovery` folder holding the same `<key>.pdf` / `<key>.json` entries as
//! the desktop recovery folder.
//!
//! Every call is asynchronous. Writes and removals are queued and carried out in order by one
//! task at a time (so a removal never overtakes the write it follows), outside the frame. Each
//! file is written through a writable stream, which the browser swaps in only when it is closed,
//! so a closed tab mid-write leaves the previous file. Entries are read once at startup, capped
//! by size and count, and checked like any stored entry before they are offered.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pdfcraft_engine::recovery as rec;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions, FileSystemGetFileOptions, FileSystemWritableFileStream,
};

use crate::recovery::BrowserFiles;

const FOLDER: &str = "pdfcraft-recovery";

#[derive(Debug)]
pub(crate) enum Op {
    Write(String, Arc<Vec<u8>>),
    Remove(String),
}

/// The queue of changes waiting to reach browser storage.
#[derive(Clone, Debug, Default)]
pub(crate) struct Writer {
    queue: Arc<Mutex<VecDeque<Op>>>,
    running: Arc<AtomicBool>,
}

impl Writer {
    pub(crate) fn push(&self, op: Op) {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner).push_back(op);
        if !self.running.swap(true, Ordering::AcqRel) {
            let me = self.clone();
            wasm_bindgen_futures::spawn_local(async move { me.drain().await });
        }
    }

    async fn drain(self) {
        let folder = folder().await;
        loop {
            // The page is single-threaded: nothing is pushed between this pop and the flag
            // being cleared below, so no change is left waiting.
            let next = self.queue.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
            let Some(op) = next else { break };
            let Ok(dir) = &folder else { continue };
            let done = match op {
                Op::Write(name, bytes) => write(dir, &name, &bytes).await,
                Op::Remove(name) => JsFuture::from(dir.remove_entry(&name)).await.map(|_| ()).or(Ok(())),
            };
            if let Err(e) = done {
                log::warn!("recovery copy not written to browser storage: {e}");
            }
        }
        if let Err(e) = folder {
            log::warn!("browser recovery storage unavailable: {e}");
        }
        self.running.store(false, Ordering::Release);
    }
}

fn js(e: JsValue) -> String {
    e.as_string().or_else(|| e.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message()))).unwrap_or_else(|| format!("{e:?}"))
}

async fn folder() -> Result<FileSystemDirectoryHandle, String> {
    let window = web_sys::window().ok_or("no window")?;
    let root = JsFuture::from(window.navigator().storage().get_directory()).await.map_err(js)?;
    let root: FileSystemDirectoryHandle = root.dyn_into().map_err(|_| "no origin-private file system")?;
    let opts = FileSystemGetDirectoryOptions::new();
    opts.set_create(true);
    JsFuture::from(root.get_directory_handle_with_options(FOLDER, &opts)).await.map_err(js)?.dyn_into().map_err(|_| "not a folder".to_string())
}

async fn write(dir: &FileSystemDirectoryHandle, name: &str, bytes: &[u8]) -> Result<(), String> {
    let opts = FileSystemGetFileOptions::new();
    opts.set_create(true);
    let file: FileSystemFileHandle =
        JsFuture::from(dir.get_file_handle_with_options(name, &opts)).await.map_err(js)?.dyn_into().map_err(|_| "not a file")?;
    let stream: FileSystemWritableFileStream = JsFuture::from(file.create_writable()).await.map_err(js)?.dyn_into().map_err(|_| "not writable")?;
    // A copy in JavaScript memory: the write finishes after this frame, when wasm memory may
    // have moved.
    let data = js_sys::Uint8Array::from(bytes);
    let written = match stream.write_with_js_u8_array(&data) {
        Ok(p) => JsFuture::from(p).await.map(|_| ()).map_err(js),
        Err(e) => Err(js(e)),
    };
    match written {
        Ok(()) => JsFuture::from(stream.close()).await.map(|_| ()).map_err(js),
        Err(e) => {
            // Keeps the previous file.
            let _ = JsFuture::from(stream.abort()).await;
            Err(e)
        }
    }
}

/// Read the stored entries into `files` (asynchronously), then mark it loaded and repaint.
pub(crate) fn start_loading(files: BrowserFiles, ctx: egui::Context) {
    wasm_bindgen_futures::spawn_local(async move {
        let failure = load(&files).await.err();
        files.finish_loading(failure);
        ctx.request_repaint();
    });
}

async fn load(files: &BrowserFiles) -> Result<(), String> {
    let dir = folder().await?;
    let mut names = Vec::new();
    let keys = dir.keys();
    loop {
        let next = JsFuture::from(keys.next().map_err(js)?).await.map_err(js)?;
        if js_sys::Reflect::get(&next, &"done".into()).map_err(js)?.is_truthy() {
            break;
        }
        if let Some(name) = js_sys::Reflect::get(&next, &"value".into()).map_err(js)?.as_string() {
            names.push(name);
        }
        // Far more than a session leaves; the rest is not looked at.
        if names.len() > 4 * 1000 {
            break;
        }
    }
    let mut found = Vec::new();
    let mut total = 0f64;
    for name in names {
        if rec::entry_name(&name).is_none() {
            continue;
        }
        let Ok(handle) = JsFuture::from(dir.get_file_handle(&name)).await else { continue };
        let Ok(handle) = handle.dyn_into::<FileSystemFileHandle>() else { continue };
        let Ok(file) = JsFuture::from(handle.get_file()).await else { continue };
        let Ok(file) = file.dyn_into::<web_sys::File>() else { continue };
        let size = file.size();
        // Too large to keep (or a nonsense size): never read, offered nothing.
        if !size.is_finite() || size < 0.0 || total + size > rec::WEB_STORE_CAP as f64 {
            found.push((name, Vec::new()));
            continue;
        }
        total += size;
        let Ok(buffer) = JsFuture::from(file.array_buffer()).await else { continue };
        found.push((name, js_sys::Uint8Array::new(&buffer).to_vec()));
    }
    // Entries that can't be taken in are removed, so they don't fill the store.
    for name in files.load(found) {
        files.pass_on_remove(&name);
    }
    Ok(())
}
