//! Page text extraction for whole-document jobs (searches, exports): pages in parallel, one
//! renderer per worker, delivered as soon as each is ready.

use std::sync::Arc;

use pdfcraft_render::{PageRenderer, PageText, RenderConfig, RenderRequest, RequestKind};

/// Extract the text of `pages` (0-based, distinct) with one renderer per worker thread, handing
/// each page to `sink` on this thread as soon as it is ready (in no particular order), so
/// pages are not all held at once. Stops at the first error from extraction or `sink`; a page a
/// worker could not deliver (a worker panicked) is an error, never silently missing.
pub fn extract_each<E>(
    bytes: &Arc<Vec<u8>>,
    password: Option<Arc<str>>,
    pages: &[usize],
    mut sink: impl FnMut(usize, PageText) -> Result<(), E>,
    err: impl Fn(String) -> E,
) -> Result<(), E> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let extract = |r: &mut PageRenderer, p: usize| {
        let out = r.render(RenderRequest { page: p, kind: RequestKind::Text, tile: None, scale: 1.0, tag: 0 });
        match out.error {
            Some(e) => Err(e),
            None => Ok(out.text.map(Arc::unwrap_or_clone).unwrap_or_default()),
        }
    };
    let config = RenderConfig { password, ..Default::default() };
    let workers = if cfg!(target_arch = "wasm32") { 1 } else { std::thread::available_parallelism().map_or(1, |n| n.get()).min(8) };
    // Small jobs aren't worth a second parse of the document.
    if workers == 1 || pages.len() < 8 {
        if pages.is_empty() {
            return Ok(());
        }
        let mut r = PageRenderer::new(bytes.clone(), config);
        for &p in pages {
            let text = extract(&mut r, p).map_err(|e| err(format!("page {}: {e}", p + 1)))?;
            sink(p, text)?;
        }
        return Ok(());
    }
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        // Bounded: workers wait while `sink` catches up instead of piling pages up.
        let (tx, rx) = std::sync::mpsc::sync_channel::<(usize, std::result::Result<PageText, String>)>(workers * 2);
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let (tx, next, stop, config) = (tx.clone(), &next, &stop, config.clone());
                s.spawn(move || {
                    let mut r = PageRenderer::new(bytes.clone(), config);
                    while !stop.load(Ordering::Relaxed) {
                        let Some(&p) = pages.get(next.fetch_add(1, Ordering::Relaxed)) else { break };
                        if tx.send((p, extract(&mut r, p))).is_err() {
                            break;
                        }
                    }
                })
            })
            .collect();
        drop(tx);
        let mut result = Ok(());
        let mut delivered = 0usize;
        for (p, text) in rx.iter() {
            delivered += 1;
            if let Err(e) = text.map_err(|e| err(format!("page {}: {e}", p + 1))).and_then(|t| sink(p, t)) {
                result = Err(e);
                break;
            }
        }
        stop.store(true, Ordering::Relaxed);
        // Unblock workers waiting to send, then wait for them (a panic is joined, not rethrown).
        drop(rx);
        for h in handles {
            let _ = h.join();
        }
        match result {
            Ok(()) if delivered < pages.len() => Err(err("page text could not be extracted (an extraction worker failed)".into())),
            other => other,
        }
    })
}
