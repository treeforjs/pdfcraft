//! Recognize Text in the web app, through web-sys (no handwritten JavaScript).
//!
//! The browser has no model files to find, and the models (about 12 MB) are not part of the
//! initial download: the first time text is recognized they are fetched from the site's
//! `models/` folder (the release zip ships them there) and kept in the browser's origin-private
//! file system (`pdfcraft-ocr-models`), so later recognitions, also after a reload, read them
//! from there. Every copy, fetched or stored, must be exactly the pinned file: the announced
//! and actual length are checked before and while reading (nothing past the pinned size is
//! kept), then its SHA-256 sum ([`pdfcraft_engine::ocr::models::verify`]). A stored copy that
//! fails is removed and fetched again; a download that fails is an error, never loaded.
//!
//! The page has one thread, so recognition runs as a task that reads each page in short steps
//! ([`pdfcraft_engine::ocr::OcrJob::run_selected_paced`]) and gives the browser back its thread
//! once it has worked [`BUDGET_MS`]: the page keeps drawing (progress) and handling input
//! (Cancel) while it reads. One step (finding the text lines of a page, or reading a few of
//! them) still runs to its end.

use std::sync::{Arc, Mutex, PoisonError};

use pdfcraft_engine::ocr::{DEFAULT_ENGINE, Ocr, OcrJob, OcrSettings, OcrsEngine, Pace, models};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions, FileSystemGetFileOptions, FileSystemWritableFileStream,
};

use crate::ocr_ui::{BatchProgress, OcrProgress};

const FOLDER: &str = "pdfcraft-ocr-models";

/// How long recognition works before letting the page draw and take input.
pub(crate) const BUDGET_MS: f64 = 30.0;

fn js(e: JsValue) -> String {
    e.as_string().or_else(|| e.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message()))).unwrap_or_else(|| format!("{e:?}"))
}

/// Give the thread back to the browser until its next task (it draws a due frame first).
async fn next_task() {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let queued = web_sys::window().and_then(|w| w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0).ok());
        if queued.is_none() {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// Pauses once [`BUDGET_MS`] of work has passed since the last one.
pub(crate) struct FramePace {
    since: f64,
}

impl FramePace {
    pub(crate) fn new() -> Self {
        FramePace { since: js_sys::Date::now() }
    }
}

impl Pace for FramePace {
    fn stepwise(&self) -> bool {
        true
    }

    fn pause(&mut self) -> impl std::future::Future<Output = ()> {
        async move {
            if js_sys::Date::now() - self.since >= BUDGET_MS {
                next_task().await;
                self.since = js_sys::Date::now();
            }
        }
    }
}

/// What a model download reports: bytes so far (stored copies count at once), and whether to stop.
trait Download {
    fn received(&self, bytes: u64);
    fn canceled(&self) -> bool;
}

impl Download for Mutex<OcrProgress> {
    fn received(&self, bytes: u64) {
        self.lock().unwrap_or_else(PoisonError::into_inner).downloading = Some((bytes, models::total_bytes()));
    }
    fn canceled(&self) -> bool {
        self.lock().unwrap_or_else(PoisonError::into_inner).cancel
    }
}

impl Download for Mutex<BatchProgress> {
    fn received(&self, bytes: u64) {
        self.lock().unwrap_or_else(PoisonError::into_inner).downloading = Some((bytes, models::total_bytes()));
    }
    fn canceled(&self) -> bool {
        false
    }
}

const CANCELED: &str = "text recognition canceled; no text was applied";

/// Have the built-in engine's models loaded: from this browser's copy, else downloaded (and
/// then kept). Called before the first recognition with the built-in engine.
async fn ensure_models(progress: &impl Download, ctx: &Option<egui::Context>) -> Result<(), String> {
    if OcrsEngine::loaded() {
        return Ok(());
    }
    let folder = folder().await.map_err(|e| log::warn!("browser storage for the OCR models unavailable: {e}")).ok();
    let mut parts = Vec::with_capacity(models::PINNED.len());
    let mut before = 0u64;
    for model in &models::PINNED {
        progress.received(before);
        let stored = match &folder {
            Some(dir) => stored(dir, model).await,
            None => None,
        };
        let bytes = match stored {
            Some(bytes) => bytes,
            None => {
                let bytes = download(model, before, progress, ctx).await?;
                if let Some(dir) = &folder
                    && let Err(e) = store(dir, model.file, &bytes).await
                {
                    // Recognition still works; the next visit downloads again.
                    log::warn!("OCR model {} not kept in browser storage: {e}", model.file);
                }
                bytes
            }
        };
        before = before.saturating_add(model.bytes);
        parts.push(bytes);
        if progress.canceled() {
            return Err(CANCELED.into());
        }
    }
    progress.received(before);
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
    // Loading the models takes a moment: let the progress show first.
    next_task().await;
    let mut parts = parts.into_iter();
    let (Some(detection), Some(recognition)) = (parts.next(), parts.next()) else {
        return Err("the text recognition models are incomplete".into());
    };
    let ocr = Ocr::from_bytes(detection, recognition).map_err(|e| e.to_string())?;
    OcrsEngine::install(ocr);
    Ok(())
}

async fn folder() -> Result<FileSystemDirectoryHandle, String> {
    let window = web_sys::window().ok_or("no window")?;
    let root = JsFuture::from(window.navigator().storage().get_directory()).await.map_err(js)?;
    let root: FileSystemDirectoryHandle = root.dyn_into().map_err(|_| "no origin-private file system")?;
    let opts = FileSystemGetDirectoryOptions::new();
    opts.set_create(true);
    JsFuture::from(root.get_directory_handle_with_options(FOLDER, &opts)).await.map_err(js)?.dyn_into().map_err(|_| "not a folder".to_string())
}

/// This browser's copy of `model`, if it is there and exactly the pinned file. A copy that
/// isn't is removed (it is downloaded again).
async fn stored(dir: &FileSystemDirectoryHandle, model: &models::PinnedModel) -> Option<Vec<u8>> {
    let handle = JsFuture::from(dir.get_file_handle(model.file)).await.ok()?.dyn_into::<FileSystemFileHandle>().ok()?;
    let file = JsFuture::from(handle.get_file()).await.ok()?.dyn_into::<web_sys::File>().ok()?;
    let size = file.size();
    let fits = size.is_finite() && size >= 0.0 && models::check_length(model.file, size as u64).is_ok();
    let bytes = if fits { JsFuture::from(file.array_buffer()).await.ok().map(|b| js_sys::Uint8Array::new(&b).to_vec()) } else { None };
    match bytes {
        Some(b) if models::verify(model.file, &b).is_ok() => Some(b),
        _ => {
            log::warn!("the stored OCR model {} is damaged; downloading it again", model.file);
            let _ = JsFuture::from(dir.remove_entry(model.file)).await;
            None
        }
    }
}

async fn store(dir: &FileSystemDirectoryHandle, name: &str, bytes: &[u8]) -> Result<(), String> {
    let opts = FileSystemGetFileOptions::new();
    opts.set_create(true);
    let file: FileSystemFileHandle =
        JsFuture::from(dir.get_file_handle_with_options(name, &opts)).await.map_err(js)?.dyn_into().map_err(|_| "not a file")?;
    let stream: FileSystemWritableFileStream = JsFuture::from(file.create_writable()).await.map_err(js)?.dyn_into().map_err(|_| "not writable")?;
    let data = js_sys::Uint8Array::from(bytes);
    let written = match stream.write_with_js_u8_array(&data) {
        Ok(p) => JsFuture::from(p).await.map(|_| ()).map_err(js),
        Err(e) => Err(js(e)),
    };
    match written {
        // The browser swaps the file in only when the stream closes: a closed tab leaves none.
        Ok(()) => JsFuture::from(stream.close()).await.map(|_| ()).map_err(js),
        Err(e) => {
            let _ = JsFuture::from(stream.abort()).await;
            Err(e)
        }
    }
}

/// Fetch `model` from the site's `models/` folder, reporting progress; at most its pinned size
/// is read, and it must then verify.
async fn download(model: &models::PinnedModel, before: u64, progress: &impl Download, ctx: &Option<egui::Context>) -> Result<Vec<u8>, String> {
    let failed = |why: String| format!("could not download the text recognition model {}: {why}", model.file);
    let window = web_sys::window().ok_or_else(|| failed("no window".into()))?;
    let base = window.location().href().map_err(|e| failed(js(e)))?;
    // The checksum in the URL: a model pinned anew is never served from an old HTTP cache.
    let short: String = model.sha256.chars().take(12).collect();
    let url = web_sys::Url::new_with_base(&format!("models/{}?v={short}", model.file), &base).map_err(|e| failed(js(e)))?.href();
    let response: web_sys::Response =
        JsFuture::from(window.fetch_with_str(&url)).await.map_err(|e| failed(js(e)))?.dyn_into().map_err(|_| failed("no response".into()))?;
    if !response.ok() {
        return Err(failed(format!("the site answered {} {}", response.status(), response.status_text())));
    }
    if let Ok(Some(length)) = response.headers().get("content-length")
        && let Ok(length) = length.trim().parse::<u64>()
    {
        models::check_length(model.file, length).map_err(|e| failed(e.to_string()))?;
    }
    let body = response.body().ok_or_else(|| failed("the response is empty".into()))?;
    let reader = web_sys::ReadableStreamDefaultReader::new(&body).map_err(|e| failed(js(e)))?;
    let cap = usize::try_from(model.bytes).map_err(|_| failed("too large".into()))?;
    let mut bytes = Vec::with_capacity(cap);
    loop {
        if progress.canceled() {
            let _ = JsFuture::from(reader.cancel()).await;
            return Err(CANCELED.into());
        }
        let chunk = JsFuture::from(reader.read()).await.map_err(|e| failed(js(e)))?;
        if js_sys::Reflect::get(&chunk, &"done".into()).map_err(|e| failed(js(e)))?.is_truthy() {
            break;
        }
        let value = js_sys::Reflect::get(&chunk, &"value".into()).map_err(|e| failed(js(e)))?;
        let Ok(part) = value.dyn_into::<js_sys::Uint8Array>() else {
            let _ = JsFuture::from(reader.cancel()).await;
            return Err(failed("unexpected data".into()));
        };
        let len = part.length() as usize;
        if bytes.len().saturating_add(len) > cap {
            let _ = JsFuture::from(reader.cancel()).await;
            return Err(failed(format!("more than the pinned {} bytes", model.bytes)));
        }
        let start = bytes.len();
        bytes.resize(start + len, 0);
        if let Some(slot) = bytes.get_mut(start..) {
            part.copy_to(slot);
        }
        progress.received(before.saturating_add(bytes.len() as u64));
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    }
    models::verify(model.file, &bytes).map_err(|e| failed(e.to_string()))?;
    Ok(bytes)
}

/// Recognize Text: load the models if the job reads with the built-in engine and they aren't
/// yet, then read its pages paced. The result lands in `progress` like the desktop worker's.
pub(crate) fn run(job: OcrJob, progress: Arc<Mutex<OcrProgress>>, ctx: Option<egui::Context>) {
    wasm_bindgen_futures::spawn_local(async move {
        let mut result = Ok(());
        if job.ocr_engine().id() == DEFAULT_ENGINE {
            result = ensure_models(progress.as_ref(), &ctx).await;
        }
        progress.lock().unwrap_or_else(PoisonError::into_inner).downloading = None;
        let result = match result {
            Ok(()) => {
                let p = progress.clone();
                let ask = move |done, total| {
                    let mut s = p.lock().unwrap_or_else(PoisonError::into_inner);
                    s.done = done;
                    s.total = total;
                    !s.cancel
                };
                job.run_selected_paced(ask, &mut FramePace::new()).await
            }
            Err(e) => Err(e),
        };
        progress.lock().unwrap_or_else(PoisonError::into_inner).result = Some(result);
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
}

/// Recognize text in multiple files: each read paced and downloaded as a searchable copy.
pub(crate) fn run_files(
    files: Vec<(String, Vec<u8>)>,
    settings: OcrSettings,
    engine: Result<Arc<dyn pdfcraft_engine::ocr::OcrEngine>, String>,
    progress: Arc<Mutex<BatchProgress>>,
    ctx: Option<egui::Context>,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let (mut ok, mut words, mut failed) = (0, 0, Vec::new());
        let ready = match engine {
            Ok(e) if e.id() == DEFAULT_ENGINE => ensure_models(progress.as_ref(), &ctx).await.map(|_| e),
            other => other,
        };
        progress.lock().unwrap_or_else(PoisonError::into_inner).downloading = None;
        match ready.and_then(|e| e.prepare(&settings.language).map(|_| e).map_err(|e| e.to_string())) {
            Err(e) => failed.push(e),
            Ok(engine) => {
                for (i, (name, bytes)) in files.into_iter().enumerate() {
                    progress.lock().unwrap_or_else(PoisonError::into_inner).done = i;
                    if let Some(ctx) = &ctx {
                        ctx.request_repaint();
                    }
                    let read = pdfcraft_engine::ocr::recognize_file_paced(
                        &name,
                        Arc::new(bytes),
                        None,
                        settings.clone(),
                        &engine,
                        |_, _| true,
                        &mut FramePace::new(),
                    )
                    .await;
                    match read.and_then(|r| crate::editing::download(&name, &r.bytes).map(|_| r.words())) {
                        Ok(n) => {
                            ok += 1;
                            words += n;
                        }
                        Err(e) => failed.push(format!("{name}: {e}")),
                    }
                }
            }
        }
        let mut s = progress.lock().unwrap_or_else(PoisonError::into_inner);
        s.done = s.total;
        s.message = Some(crate::ocr_ui::batch_summary(ok, words, &failed));
        drop(s);
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
}
