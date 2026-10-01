//! Multi-document page operations: Combine Files, Insert Pages from File, Extract Pages, Split.
//!
//! Desktop builds pick files synchronously. Browsers pick them asynchronously: the chosen
//! files land in `requests` with their purpose and are handled on the next frame.

use std::sync::Arc;

use printcraft_engine::{Edit, SplitBy};

use crate::PrintCraftApp;

/// Why files were picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePurpose {
    Combine,
    InsertPages,
}

/// Files picked asynchronously (web), waiting to be used: (purpose, [(name, bytes)]).
pub type Requests = Arc<std::sync::Mutex<Vec<(FilePurpose, Vec<(String, Vec<u8>)>)>>>;

/// Settings for the Split dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct SplitDraft {
    /// Pages per file.
    pub every: usize,
    /// Split before each selected page instead of by count.
    pub at_selection: bool,
}

impl PrintCraftApp {
    /// Ask for files to combine (File ▸ Combine files…).
    pub fn combine_dialog(&mut self) {
        self.pick_files(FilePurpose::Combine, true);
    }

    /// Ask for a PDF whose pages to insert after the selection (Organize ▸ Insert from file).
    pub fn insert_from_file_dialog(&mut self) {
        if self.active.is_none() {
            self.notify("Open a document first");
            return;
        }
        self.pick_files(FilePurpose::InsertPages, false);
    }

    fn pick_files(&mut self, purpose: FilePurpose, multiple: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dialog = rfd::FileDialog::new().add_filter("PDF", &["pdf"]);
            let paths = if multiple { dialog.pick_files().unwrap_or_default() } else { dialog.pick_file().into_iter().collect() };
            let mut files = Vec::new();
            for p in paths {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file.pdf".into());
                match std::fs::read(&p) {
                    Ok(b) => files.push((name, b)),
                    Err(e) => {
                        self.notify(format!("Couldn't read {name}: {e}"));
                        return;
                    }
                }
            }
            if !files.is_empty() {
                self.use_files(purpose, files);
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let requests = self.requests.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let dialog = rfd::AsyncFileDialog::new().add_filter("PDF", &["pdf"]);
                let handles = if multiple { dialog.pick_files().await.unwrap_or_default() } else { dialog.pick_file().await.into_iter().collect() };
                let mut files = Vec::new();
                for h in handles {
                    files.push((h.file_name(), h.read().await));
                }
                if !files.is_empty()
                    && let Ok(mut q) = requests.lock()
                {
                    q.push((purpose, files));
                }
            });
        }
    }

    /// Handle files picked asynchronously.
    pub(crate) fn process_file_requests(&mut self) {
        let pending: Vec<_> = self.requests.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for (purpose, files) in pending {
            self.use_files(purpose, files);
        }
    }

    /// Use picked files (also the entry point for tests and automation).
    pub fn use_files(&mut self, purpose: FilePurpose, files: Vec<(String, Vec<u8>)>) {
        match purpose {
            FilePurpose::Combine => self.combine_files(files),
            FilePurpose::InsertPages => {
                for (name, bytes) in files {
                    self.insert_pages_from(&name, bytes);
                }
            }
        }
    }

    /// Combine files, in order, into a new unsaved document tab.
    pub fn combine_files(&mut self, files: Vec<(String, Vec<u8>)>) {
        if files.is_empty() {
            return;
        }
        let sources: Vec<(String, Arc<Vec<u8>>)> = files.into_iter().map(|(n, b)| (strip_pdf(&n).to_string(), Arc::new(b))).collect();
        let count = sources.len();
        match self.session.combine(&sources) {
            Ok(bytes) => self.open_created("Combined.pdf", bytes, &format!("Combined {count} files")),
            Err(e) => self.notify(format!("Couldn't combine files: {e}")),
        }
    }

    /// Insert all pages of a PDF after the organize selection (or the current page).
    pub fn insert_pages_from(&mut self, name: &str, bytes: Vec<u8>) {
        let Some(i) = self.active else { return };
        let at = self.views[i].target_pages().last().map(|p| p + 1).unwrap_or(0);
        self.apply_edit(Edit::InsertPagesFrom { name: name.to_string(), bytes: Arc::new(bytes), pages: None, at });
    }

    /// Copy the selected pages (or the current page) into a new unsaved document tab.
    pub fn extract_selection(&mut self) {
        let Some((i, id)) = self.active_ids() else { return };
        let pages = self.views[i].target_pages();
        let stem = self.session.get(id).map(|d| strip_pdf(&d.name).to_string()).unwrap_or_default();
        match self.session.extract(id, &pages) {
            Ok(bytes) => self.open_created(&format!("{stem} (extract).pdf"), bytes, &format!("Extracted {} page(s)", pages.len())),
            Err(e) => self.notify(format!("Couldn't extract pages: {e}")),
        }
    }

    /// Split the active document and write the parts: into a chosen folder (desktop) or as
    /// downloads (web). Returns the number of files written.
    pub fn split_active(&mut self, by: &SplitBy) -> usize {
        let Some((_, id)) = self.active_ids() else { return 0 };
        let stem = self.session.get(id).map(|d| strip_pdf(&d.name).to_string()).unwrap_or_else(|| "document".into());
        let parts = match self.session.split(id, by) {
            Ok(p) => p,
            Err(e) => {
                self.notify(format!("Couldn't split: {e}"));
                return 0;
            }
        };
        let named: Vec<(String, Arc<Vec<u8>>)> = parts
            .into_iter()
            .map(|(a, b, bytes)| (if a == b { format!("{stem} (page {a}).pdf") } else { format!("{stem} (pages {a}-{b}).pdf") }, bytes))
            .collect();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dir = match &self.export_dir_override {
                Some(d) => Some(std::path::PathBuf::from(d)),
                None => rfd::FileDialog::new().set_title("Choose a folder for the split files").pick_folder(),
            };
            let Some(dir) = dir else { return 0 };
            for (name, bytes) in &named {
                if let Err(e) = crate::editing::write_atomically(&dir.join(name).to_string_lossy(), bytes) {
                    self.notify(format!("Couldn't write {name}: {e}"));
                    return 0;
                }
            }
            self.notify(format!("Split into {} files in {}", named.len(), dir.display()));
        }
        #[cfg(target_arch = "wasm32")]
        for (name, bytes) in &named {
            if let Err(e) = crate::editing::download(name, bytes) {
                self.notify(format!("Couldn't download {name}: {e}"));
                return 0;
            }
        }
        named.len()
    }

    fn open_created(&mut self, name: &str, bytes: Arc<Vec<u8>>, message: &str) {
        match self.session.open_new(name, bytes) {
            Ok(id) => {
                let info = &self.session.get(id).expect("just opened").info;
                self.views.push(crate::DocView::new(id, info));
                self.active = Some(self.views.len() - 1);
                self.notify(message);
            }
            Err(e) => self.notify(format!("Couldn't open the result: {e}")),
        }
    }
}

fn strip_pdf(name: &str) -> &str {
    name.strip_suffix(".pdf").or_else(|| name.strip_suffix(".PDF")).unwrap_or(name)
}
