//! Export a PDF ▸ Image (PNG) and Text (Acrobat's Export a PDF tool, first formats).
//!
//! On the desktop the export runs on a worker thread and reports progress in the notice bar;
//! on the web it runs in place and downloads the files.

use std::sync::{Arc, Mutex};

use egui::{Align, Layout};
use printcraft_engine::export::{ExportSource, Exporter};

use crate::marks_ui::PageRange;
use crate::theme::{self, Tokens};
use crate::{PrintCraftApp, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    Image,
    Text,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportDraft {
    pub dpi: f64,
    pub range: PageRange,
}

impl Default for ExportDraft {
    fn default() -> Self {
        Self { dpi: 150.0, range: PageRange::default() }
    }
}

/// Progress of a background export: (done, total, final message once finished).
pub type ExportStatus = Arc<Mutex<Option<(usize, usize, Option<String>)>>>;

pub(crate) fn body(ui: &mut egui::Ui, app: &mut PrintCraftApp, t: &Tokens, kind: ExportKind) -> (bool, bool) {
    let count = app.active_ids().and_then(|(_, id)| app.session.get(id)).map(|d| d.info.pages.len()).unwrap_or(0);
    let d = &mut app.export_draft;
    ui.label(
        egui::RichText::new(match kind {
            ExportKind::Image => "Export to Image (PNG)",
            ExportKind::Text => "Export to Text",
        })
        .font(theme::semibold(18.0)),
    );
    ui.add_space(8.0);
    if kind == ExportKind::Image {
        ui.horizontal(|ui| {
            ui.label("Resolution");
            egui::ComboBox::from_id_salt("export-dpi").selected_text(format!("{} pixels/inch", d.dpi)).show_ui(ui, |ui| {
                for dpi in [72.0, 96.0, 150.0, 300.0, 600.0] {
                    ui.selectable_value(&mut d.dpi, dpi, format!("{dpi} pixels/inch"));
                }
            });
        });
        ui.label(egui::RichText::new("One PNG file per page, named after the document.").small().color(t.text_faint));
    } else {
        ui.label(egui::RichText::new("Plain text in reading order; pages are separated by form feeds.").small().color(t.text_faint));
    }
    ui.add_space(6.0);
    ui.label(egui::RichText::new("Pages").font(theme::semibold(12.5)));
    d.range.ui(ui, count);
    ui.add_space(12.0);
    let (mut apply, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let ok = !d.range.pages(count).is_empty();
        if ui.add_enabled_ui(ok, |ui| widgets::pill_button(ui, "Export", true)).inner.clicked() {
            apply = true;
        }
        if widgets::pill_button(ui, "Cancel", false).clicked() {
            cancel = true;
        }
    });
    (apply, cancel)
}

/// Write the files (`name`, bytes) produced for `pages`, reporting progress.
fn run(
    src: ExportSource,
    kind: ExportKind,
    dpi: f64,
    pages: Vec<usize>,
    stem: String,
    mut sink: impl FnMut(&str, Vec<u8>) -> Result<(), String>,
    status: &ExportStatus,
) -> String {
    let mut ex = Exporter::from_source(src);
    let total = pages.len();
    let set = |done: usize, msg: Option<String>| {
        if let Ok(mut s) = status.lock() {
            *s = Some((done, total, msg));
        }
    };
    match kind {
        ExportKind::Image => {
            for (k, p) in pages.iter().enumerate() {
                set(k, None);
                let result = ex.png(*p, dpi).and_then(|png| sink(&format!("{stem}_page_{}.png", p + 1), png));
                if let Err(e) = result {
                    return format!("Export stopped: {e}");
                }
            }
            format!("Exported {total} image{}", if total == 1 { "" } else { "s" })
        }
        ExportKind::Text => {
            set(0, None);
            match ex.text_of(&pages).and_then(|text| sink(&format!("{stem}.txt"), text.into_bytes())) {
                Ok(()) => format!("Exported the text of {total} page{}", if total == 1 { "" } else { "s" }),
                Err(e) => format!("Export stopped: {e}"),
            }
        }
    }
}

impl PrintCraftApp {
    /// Start exporting the active document with the dialog's settings.
    pub(crate) fn start_export(&mut self, kind: ExportKind) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let src = doc.export_source();
        let stem = doc.name.trim_end_matches(".pdf").trim_end_matches(".PDF").to_string();
        let pages = self.export_draft.range.pages(src.pages);
        let dpi = self.export_draft.dpi;
        let status: ExportStatus = Arc::new(Mutex::new(Some((0, pages.len(), None))));
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dir = match &self.export_dir_override {
                Some(d) => Some(std::path::PathBuf::from(d)),
                None => rfd::FileDialog::new().set_title("Choose a folder for the exported files").pick_folder(),
            };
            let Some(dir) = dir else { return };
            let st = status.clone();
            let shown = dir.display().to_string();
            let work = move || {
                let sink = |name: &str, bytes: Vec<u8>| {
                    crate::editing::write_atomically(&dir.join(name).to_string_lossy(), &bytes).map_err(|e| format!("{name}: {e}"))
                };
                let msg = run(src, kind, dpi, pages, stem, sink, &st);
                if let Ok(mut s) = st.lock() {
                    let (done, total) = s.as_ref().map_or((0, 0), |(d, t, _)| (*d, *t));
                    *s = Some((done.max(total), total, Some(format!("{msg} to {shown}"))));
                }
            };
            if self.export_dir_override.is_some() {
                work(); // tests and automation: synchronous
            } else {
                std::thread::Builder::new().name("printcraft-export".into()).spawn(work).ok();
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let msg = run(src, kind, dpi, pages, stem, |name, bytes| crate::editing::download(name, &bytes), &status);
            if let Ok(mut s) = status.lock() {
                *s = Some((0, 0, Some(msg)));
            }
        }
        self.export_status = Some(status);
    }

    /// Show export progress, and the result once it is done.
    pub(crate) fn poll_export(&mut self) {
        let Some(st) = self.export_status.clone() else { return };
        let snapshot = st.lock().ok().and_then(|s| s.clone());
        match snapshot {
            Some((_, _, Some(msg))) => {
                self.export_status = None;
                self.notify(msg);
            }
            Some((done, total, None)) if total > 1 => self.notify(format!("Exporting… {done} of {total}")),
            _ => {}
        }
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}
