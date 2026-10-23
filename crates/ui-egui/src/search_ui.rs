//! Edit ▸ Advanced Search (⇧⌘F): the search in a side panel that lists every match with its
//! context, page by page; click a result to go to it. It searches the current document (sharing
//! the find bar's search, comments included on request), every open document, or every PDF in a
//! folder (and its subfolders). Other documents and folders are searched on a worker thread
//! with progress and Cancel, within the folder search's caps; folder links are never followed
//! and file links only inside the chosen folder.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use egui::text::LayoutJob;
use egui::{Color32, FontId, TextFormat};
use pdfcraft_engine::DocId;
use pdfcraft_engine::folder_search::{self, Control, Options};
use serde_json::{Value, json};

use crate::canvas::DocView;
use crate::theme::{self, Tokens};

/// Where Advanced Search looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    Document,
    OpenDocuments,
    Folder,
}

/// Most files a folder search looks at, and most matches any search keeps.
const MAX_FILES: usize = folder_search::MAX_FILES;
const MAX_RESULTS: usize = 5000;
/// Results drawn at once (the rest are counted).
const MAX_ROWS: usize = 2000;

/// A search running on a worker: its progress (files done, files in all, the file now) and,
/// once finished, its result in the folder search's shape.
pub struct SearchRun {
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<Mutex<(usize, usize, String)>>,
    pub result: Arc<Mutex<Option<Result<Value, String>>>>,
}

/// Advanced Search beyond the current document.
#[derive(Default)]
pub struct AdvancedSearch {
    pub scope: Scope,
    /// The folder to search, and whether its subfolders are searched too.
    pub folder: Option<PathBuf>,
    pub recursive: bool,
    pub query: String,
    pub whole_words: bool,
    pub case_sensitive: bool,
    /// Bookmarks, comments and attachments too (folders and open documents).
    pub more: bool,
    pub run: Option<SearchRun>,
    /// The last search's result (`results`: per file, `matches`), or why it failed.
    pub result: Option<Result<Value, String>>,
    /// Tests: the folder Choose folder picks.
    pub folder_override: Option<PathBuf>,
}

/// What the panel asked for (handled once it is drawn).
pub enum SearchAction {
    ChooseFolder,
    Start,
    Cancel,
    /// Open a folder result: the file, its page (0-based) and the query to show.
    OpenFile(String, Option<usize>),
    /// Go to a result in an open document.
    GoToDoc(DocId, Option<usize>),
}

impl AdvancedSearch {
    /// The running search's progress, collecting its result once it has finished.
    pub fn poll(&mut self) -> Option<(usize, usize, String)> {
        let run = self.run.as_ref()?;
        let done = run.result.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(r) = done {
            self.run = None;
            self.result = Some(r);
            return None;
        }
        Some(run.progress.lock().unwrap_or_else(PoisonError::into_inner).clone())
    }

    /// A machine-readable summary for the control channel (`ui.state.advanced_search`).
    pub fn state(&self) -> Value {
        json!({
            "scope": match self.scope { Scope::Document => "document", Scope::OpenDocuments => "open_documents", Scope::Folder => "folder" },
            "folder": self.folder.as_ref().map(|f| f.display().to_string()),
            "recursive": self.recursive,
            "query": self.query,
            "running": self.run.as_ref().map(|r| { let p = r.progress.lock().unwrap_or_else(PoisonError::into_inner); json!({ "done": p.0, "total": p.1, "cancelled": r.cancel.load(Ordering::Relaxed) }) }),
            "result": match &self.result {
                Some(Ok(v)) => json!({
                    "files_searched": v["files_searched"], "files_matched": v["files_matched"], "total_matches": v["total_matches"],
                    "cancelled": v["cancelled"], "truncated_by": v["truncated_by"], "skipped": v["skipped"].as_array().map_or(0, Vec::len),
                    "files": v["results"].as_array().map(|r| r.iter().map(|f| json!({ "path": f["path"], "count": f["count"] })).collect::<Vec<_>>()),
                }),
                Some(Err(e)) => json!({ "error": e }),
                None => Value::Null,
            },
        })
    }
}

/// One open document to search: id, name, bytes, password, whether text may be copied, pages.
pub type OpenDoc = (DocId, String, Arc<Vec<u8>>, Option<String>, bool, usize);

/// Search open documents' page text (and, with `more`, their comments and bookmarks) the way
/// the folder search does, with the same result shape (`doc` names each document).
pub fn search_open_documents(
    docs: &[OpenDoc],
    extra: &[(DocId, Vec<(usize, String)>)],
    opts: &Options,
    cancel: &AtomicBool,
    progress: pdfcraft_engine::folder_search::Progress<'_>,
) -> Value {
    let mut results = Vec::new();
    let (mut total, mut searched, mut cancelled, mut truncated) = (0usize, 0usize, false, false);
    for (k, (id, name, bytes, password, copy, count)) in docs.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        progress(k, docs.len(), name);
        let mut matches: Vec<(usize, Value)> = Vec::new();
        let pages: Vec<usize> = (0..*count).collect();
        let r = pdfcraft_engine::text_extract::extract_each(
            bytes,
            password.as_deref().map(Arc::from),
            &pages,
            |p, text| {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".to_string());
                }
                for r in text.find_opts(opts.query, opts.case_sensitive, opts.whole_words) {
                    if total.saturating_add(matches.len()) >= MAX_RESULTS {
                        truncated = true;
                        break;
                    }
                    let n = text.glyphs.len();
                    let (a, b) = (r.start.min(n), r.end.min(n));
                    let flat = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
                    let m = if *copy {
                        json!({ "page": p + 1, "found_in": "text", "text": flat(text.text_of(a..b)), "before": flat(text.text_of(a.saturating_sub(CONTEXT)..a)), "after": flat(text.text_of(b..b.saturating_add(CONTEXT).min(n))) })
                    } else {
                        json!({ "page": p + 1, "found_in": "text", "text": Value::Null })
                    };
                    matches.push((p, m));
                }
                Ok(())
            },
            |e| e,
        );
        if r.is_err() && cancel.load(Ordering::Relaxed) {
            cancelled = true;
        }
        if opts.comments || opts.bookmarks {
            for (page, t) in extra.iter().filter(|(d, _)| d == id).flat_map(|(_, list)| list) {
                if crate::canvas::comment_text_matches(t, opts.query, opts.case_sensitive, opts.whole_words)
                    && total.saturating_add(matches.len()) < MAX_RESULTS
                {
                    matches.push((*page, json!({ "page": page + 1, "found_in": "comment", "text": t.chars().take(200).collect::<String>() })));
                }
            }
        }
        matches.sort_by_key(|(p, _)| *p);
        searched += 1;
        if !matches.is_empty() {
            total = total.saturating_add(matches.len());
            results.push(
                json!({ "path": name, "doc": id.0, "count": matches.len(), "matches": matches.into_iter().map(|(_, m)| m).collect::<Vec<_>>() }),
            );
        }
        if cancelled {
            break;
        }
    }
    let matched = results.len();
    let mut out = json!({
        "query": opts.query, "files_found": docs.len(), "files_searched": searched, "files_matched": matched, "total_matches": total,
        "results": results, "skipped": [], "truncated": truncated, "truncated_by": if truncated { vec!["max_results"] } else { Vec::new() },
    });
    if cancelled {
        out["cancelled"] = json!(true);
    }
    out
}

impl crate::PdfCraftApp {
    /// Start the Advanced Search for other documents or a folder, on a worker thread (inline in
    /// tests and the browser).
    pub fn start_advanced_search(&mut self) {
        let a = &mut self.adv_search;
        if a.run.is_some() {
            return;
        }
        let query = a.query.trim().to_string();
        if query.is_empty() || query.chars().count() > folder_search::MAX_QUERY_CHARS {
            a.result = Some(Err(tl!("Type a word or phrase to search for.").to_string()));
            return;
        }
        let (whole, case, more, recursive) = (a.whole_words, a.case_sensitive, a.more, a.recursive);
        let run = SearchRun { cancel: Arc::default(), progress: Arc::default(), result: Arc::default() };
        let (cancel, progress, result) = (run.cancel.clone(), run.progress.clone(), run.result.clone());
        let work: Box<dyn FnOnce() + Send> = match a.scope {
            Scope::Document => return,
            Scope::Folder => {
                let Some(folder) = a.folder.clone() else {
                    a.result = Some(Err(tl!("Choose a folder to search.").to_string()));
                    return;
                };
                Box::new(move || {
                    let opts = Options {
                        query: &query,
                        case_sensitive: case,
                        whole_words: whole,
                        bookmarks: more,
                        comments: more,
                        attachments: more,
                        password: None,
                    };
                    let report = |d: usize, t: usize, name: &str| *progress.lock().unwrap_or_else(PoisonError::into_inner) = (d, t, name.to_string());
                    let control = Control { cancel: Some(&cancel), progress: Some(&report) };
                    // Links to files are followed only inside the chosen folder.
                    let root = folder.canonicalize().ok();
                    let shown = folder.display().to_string();
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        folder_search::search(
                            &folder,
                            &shown,
                            recursive,
                            root.as_deref(),
                            &opts,
                            MAX_FILES,
                            MAX_RESULTS,
                            &folder_search::LIMITS,
                            &control,
                        )
                    }))
                    .unwrap_or_else(|_| Err("the search failed unexpectedly".into()));
                    *result.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
                })
            }
            Scope::OpenDocuments => {
                let docs: Vec<OpenDoc> = self
                    .session
                    .docs()
                    .iter()
                    .map(|d| (d.id, d.name.clone(), d.bytes.clone(), d.password.clone(), d.allows_copying(), d.info.pages.len()))
                    .collect();
                // Comment texts and bookmark titles (cheap: already read with the documents).
                let extra: Vec<(DocId, Vec<(usize, String)>)> = if more {
                    self.session
                        .docs()
                        .iter()
                        .map(|d| {
                            let mut v: Vec<(usize, String)> = d
                                .info
                                .annotations
                                .iter()
                                .filter(|a| !matches!(a.subtype.as_str(), "Popup" | "Link" | "Widget"))
                                .filter_map(|a| a.contents.clone().map(|c| (a.page, c)))
                                .take(MAX_RESULTS)
                                .collect();
                            v.extend(d.info.outline.iter().take(MAX_RESULTS).map(|b| (b.page.unwrap_or(0), b.title.clone())));
                            (d.id, v)
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                Box::new(move || {
                    let opts = Options {
                        query: &query,
                        case_sensitive: case,
                        whole_words: whole,
                        bookmarks: more,
                        comments: more,
                        attachments: false,
                        password: None,
                    };
                    let report = |d: usize, t: usize, name: &str| *progress.lock().unwrap_or_else(PoisonError::into_inner) = (d, t, name.to_string());
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| search_open_documents(&docs, &extra, &opts, &cancel, &report)))
                        .map_err(|_| "the search failed unexpectedly".to_string());
                    *result.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
                })
            }
        };
        a.result = None;
        a.run = Some(run);
        #[cfg(not(target_arch = "wasm32"))]
        if self.run_inline {
            work();
        } else if std::thread::Builder::new().name("pdfcraft-search".into()).spawn(work).is_err() {
            self.adv_search.run = None;
            self.adv_search.result = Some(Err("the search could not be started".into()));
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.adv_search.poll();
    }

    /// Handle what the Advanced Search panel asked for.
    pub(crate) fn advanced_search_action(&mut self, action: SearchAction) {
        match action {
            SearchAction::Start => self.start_advanced_search(),
            SearchAction::Cancel => {
                if let Some(r) = &self.adv_search.run {
                    r.cancel.store(true, Ordering::Relaxed);
                }
            }
            SearchAction::ChooseFolder =>
            {
                #[cfg(not(target_arch = "wasm32"))]
                match self.adv_search.folder_override.clone() {
                    Some(f) => self.adv_search.folder = Some(f),
                    None => {
                        let dialog = rfd::AsyncFileDialog::new().set_title(tl!("Choose a folder to search").to_string());
                        self.ask_one(crate::pickers::Ask::Folder(dialog), None, |app, dir| app.adv_search.folder = Some(dir));
                    }
                }
            }
            SearchAction::OpenFile(path, page) => {
                let query = self.adv_search.query.clone();
                let open = self.session.docs().iter().find(|d| d.path.as_deref() == Some(path.as_str())).map(|d| d.id);
                match open {
                    Some(id) => self.show_search_result(id, page, &query),
                    #[cfg(not(target_arch = "wasm32"))]
                    None => {
                        self.open_path(&path);
                        if let Some(id) = self.session.docs().iter().find(|d| d.path.as_deref() == Some(path.as_str())).map(|d| d.id) {
                            self.show_search_result(id, page, &query);
                        }
                    }
                    // The browser can't open files by path; folder results don't come up there.
                    #[cfg(target_arch = "wasm32")]
                    None => {}
                }
            }
            SearchAction::GoToDoc(id, page) => {
                let query = self.adv_search.query.clone();
                self.show_search_result(id, page, &query);
            }
        }
    }

    /// Bring document `id` forward at `page` with the query in its find bar, so the match is
    /// highlighted once its page's text is read.
    fn show_search_result(&mut self, id: DocId, page: Option<usize>, query: &str) {
        let Some(i) = self.views.iter().position(|v| v.id == id) else { return };
        self.active = Some(i);
        let v = &mut self.views[i];
        let f = v.find.get_or_insert_with(Default::default);
        f.query = query.to_string();
        f.case_sensitive = self.adv_search.case_sensitive;
        f.whole_words = self.adv_search.whole_words;
        if let Some(p) = page {
            v.go_to_page(p);
        }
    }
}

/// Glyphs of context shown on each side of a match.
const CONTEXT: usize = 36;

/// The panel: the scope, then the current document's search or the other scopes' results.
pub(crate) fn panel(
    ui: &mut egui::Ui,
    t: &Tokens,
    view: &mut DocView,
    info: &pdfcraft_render::DocInfo,
    adv: &mut AdvancedSearch,
    action: &mut Option<SearchAction>,
) {
    ui.horizontal(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.bg_stroke = egui::Stroke::new(1.0, t.border);
        ui.label(egui::RichText::new(tl!("Look in")).color(t.text_muted));
        let label = |s: Scope| match s {
            Scope::Document => tl!("The current document"),
            Scope::OpenDocuments => tl!("All open documents"),
            Scope::Folder => tl!("A folder"),
        };
        egui::ComboBox::from_id_salt("search-scope").selected_text(label(adv.scope)).width(ui.available_width()).show_ui(ui, |ui| {
            let mut scopes = vec![Scope::Document, Scope::OpenDocuments];
            if cfg!(not(target_arch = "wasm32")) {
                scopes.push(Scope::Folder);
            }
            for s in scopes {
                if ui.selectable_value(&mut adv.scope, s, label(s)).changed() {
                    adv.result = None;
                }
            }
        });
    });
    ui.add_space(6.0);
    if adv.scope != Scope::Document {
        other_scopes(ui, t, adv, action);
        return;
    }
    document_panel(ui, t, view, info);
}

/// All open documents or a folder.
fn other_scopes(ui: &mut egui::Ui, t: &Tokens, adv: &mut AdvancedSearch, action: &mut Option<SearchAction>) {
    let progress = adv.poll();
    if progress.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    let l = ui.label(egui::RichText::new(tl!("What word or phrase would you like to search for?")).color(t.text_muted));
    let r = ui
        .add(egui::TextEdit::singleline(&mut adv.query).id_salt("adv-search-query").hint_text(tl!("Search")).desired_width(f32::INFINITY))
        .labelled_by(l.id);
    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if adv.scope == Scope::Folder {
        ui.horizontal(|ui| {
            let name = adv.folder.as_ref().map_or_else(|| tl!("No folder chosen").to_string(), |f| f.display().to_string());
            ui.add(egui::Label::new(egui::RichText::new(name).small().color(t.text_muted)).truncate()).on_hover_text(tl!("The folder to search"));
        });
        ui.horizontal(|ui| {
            if crate::widgets::pill_button(ui, tl!("Choose folder…"), false).clicked() {
                *action = Some(SearchAction::ChooseFolder);
            }
            ui.checkbox(&mut adv.recursive, tl!("Include subfolders"));
        });
    }
    ui.horizontal_wrapped(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.bg_stroke = egui::Stroke::new(1.0, t.border);
        w.inactive.bg_fill = t.hover;
        ui.checkbox(&mut adv.whole_words, tl!("Whole words only"));
        ui.checkbox(&mut adv.case_sensitive, tl!("Case-sensitive"));
        ui.checkbox(&mut adv.more, tl!("Include bookmarks and comments"));
    });
    ui.add_space(4.0);
    match &progress {
        Some((done, total, name)) => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                let msg = crate::i18n::fmt(
                    tl!("Searching {d} of {t}…"),
                    &[("d", &(done + 1).min((*total).max(1)).to_string()), ("t", &(*total).max(1).to_string())],
                );
                ui.label(egui::RichText::new(msg).small());
                if crate::widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                    *action = Some(SearchAction::Cancel);
                }
            });
            ui.add(egui::Label::new(egui::RichText::new(name).small().color(t.text_faint)).truncate());
        }
        None => {
            let ready = !adv.query.trim().is_empty() && (adv.scope != Scope::Folder || adv.folder.is_some());
            if (ui.add_enabled_ui(ready, |ui| crate::widgets::pill_button(ui, tl!("Search"), true)).inner.clicked() || enter && ready)
                && action.is_none()
            {
                *action = Some(SearchAction::Start);
            }
        }
    }
    ui.add_space(6.0);
    let Some(result) = &adv.result else { return };
    let v = match result {
        Ok(v) => v,
        Err(e) => {
            ui.label(egui::RichText::new(e).color(theme::DANGER));
            return;
        }
    };
    let total = v["total_matches"].as_u64().unwrap_or(0);
    let files = v["files_matched"].as_u64().unwrap_or(0);
    let mut summary = if total == 1 { tl!("1 instance").to_string() } else { crate::i18n::fmt(tl!("{n} instances"), &[("n", &total.to_string())]) };
    summary.push_str(&crate::i18n::fmt(
        tl!(" in {f} of {s} documents"),
        &[("f", &files.to_string()), ("s", &v["files_searched"].as_u64().unwrap_or(0).to_string())],
    ));
    ui.label(egui::RichText::new(summary).font(theme::semibold(12.5)));
    if v["cancelled"].as_bool() == Some(true) {
        ui.label(egui::RichText::new(tl!("Cancelled: not every document was searched.")).small().color(t.text_muted));
    }
    if v["truncated"].as_bool() == Some(true) {
        ui.label(egui::RichText::new(tl!("Stopped at the result limit: narrow the search to see the rest.")).small().color(t.text_muted));
    }
    let skipped = v["skipped"].as_array().map_or(0, Vec::len);
    if skipped > 0 {
        let msg = crate::i18n::fmt(tl!("{n} files skipped"), &[("n", &skipped.to_string())]);
        let reasons: Vec<String> = v["skipped"]
            .as_array()
            .into_iter()
            .flatten()
            .take(20)
            .map(|s| format!("{}: {}", s["path"].as_str().unwrap_or(""), s["reason"].as_str().unwrap_or("")))
            .collect();
        ui.label(egui::RichText::new(msg).small().color(t.text_muted)).on_hover_text(reasons.join("\n"));
    }
    ui.add_space(4.0);
    let mut rows = 0usize;
    for (fi, f) in v["results"].as_array().into_iter().flatten().enumerate() {
        if rows >= MAX_ROWS {
            break;
        }
        let path = f["path"].as_str().unwrap_or("").to_string();
        let name = std::path::Path::new(&path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
        let doc = f["doc"].as_u64().map(DocId);
        egui::CollapsingHeader::new(egui::RichText::new(format!("{name}  ·  {}", f["count"].as_u64().unwrap_or(0))).font(theme::medium(12.5)))
            .id_salt(("adv-result", fi))
            .default_open(true)
            .show(ui, |ui| {
                if let Some(why) = f["incomplete"].as_str() {
                    ui.label(egui::RichText::new(why).small().color(t.text_muted));
                }
                for m in f["matches"].as_array().into_iter().flatten() {
                    if rows >= MAX_ROWS {
                        break;
                    }
                    rows += 1;
                    let page = m["page"].as_u64().and_then(|p| usize::try_from(p).ok()).and_then(|p| p.checked_sub(1));
                    let mut job = LayoutJob::default();
                    let font = FontId::new(12.5, egui::FontFamily::Proportional);
                    let plain = TextFormat { font_id: font.clone(), color: t.text_muted, ..Default::default() };
                    let where_ = match (m["found_in"].as_str(), page) {
                        (Some("text"), Some(p)) => crate::i18n::fmt(tl!("Page {label}"), &[("label", &(p + 1).to_string())]),
                        (Some("text"), None) => String::new(),
                        (Some("comment"), _) => tl!("Comment").to_string(),
                        (Some("bookmark"), _) => tl!("Bookmark").to_string(),
                        _ => tl!("Attachment").to_string(),
                    };
                    job.append(&format!("{where_}  "), 0.0, TextFormat { font_id: theme::medium(11.5), color: t.text_faint, ..Default::default() });
                    match m["text"].as_str() {
                        Some(hit) => {
                            job.append(&format!("…{} ", m["before"].as_str().unwrap_or("")), 0.0, plain.clone());
                            job.append(
                                hit,
                                0.0,
                                TextFormat {
                                    font_id: theme::semibold(12.5),
                                    color: t.text,
                                    background: Color32::from_rgba_unmultiplied(255, 214, 0, 90),
                                    ..Default::default()
                                },
                            );
                            job.append(&format!(" {}…", m["after"].as_str().unwrap_or("")), 0.0, plain);
                        }
                        None => job.append(tl!("(this document doesn't allow copying its text)"), 0.0, plain),
                    }
                    job.wrap.max_width = ui.available_width();
                    if ui.add(egui::Button::selectable(false, job).wrap()).on_hover_text(tl!("Go to this result")).clicked() {
                        *action = Some(match doc {
                            Some(d) => SearchAction::GoToDoc(d, page),
                            None => SearchAction::OpenFile(path.clone(), page),
                        });
                    }
                }
            });
    }
}

/// The current document: the find bar's search, with every match listed.
fn document_panel(ui: &mut egui::Ui, t: &Tokens, view: &mut DocView, info: &pdfcraft_render::DocInfo) {
    let pages = info.pages.len();
    let searched = view.texts.len() + view.text_failed.len();
    let Some(find) = view.find.as_mut() else { return };
    find.in_panel = true;
    let l = ui.label(egui::RichText::new(tl!("What word or phrase would you like to search for?")).color(t.text_muted));
    let r = ui
        .add(
            egui::TextEdit::singleline(&mut find.query).id(egui::Id::new("search-panel-input")).hint_text(tl!("Search")).desired_width(f32::INFINITY),
        )
        .labelled_by(l.id);
    if find.focus {
        r.request_focus();
        find.focus = false;
    }
    ui.horizontal(|ui| {
        // Outlined boxes (the panel and the check-box fill are alike), as in dialogs.
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.bg_stroke = egui::Stroke::new(1.0, t.border);
        w.inactive.bg_fill = t.hover;
        let a = ui.checkbox(&mut find.whole_words, tl!("Whole words only")).changed();
        let b = ui.checkbox(&mut find.case_sensitive, tl!("Case-sensitive")).changed();
        let c = ui.checkbox(&mut find.comments, tl!("Include comments")).changed();
        if a || b || c {
            find.case_query.clear();
        }
    });
    if find.query != find.case_query {
        view.rerun_find();
    }
    let Some(find) = view.find.as_ref() else { return };
    ui.add_space(6.0);
    let status = if find.query.trim().is_empty() {
        String::new()
    } else if find.matches.is_empty() && searched < pages {
        crate::i18n::fmt(tl!("Searching… {s} of {p} pages"), &[("s", &searched.to_string()), ("p", &pages.to_string())])
    } else {
        let n = find.matches.len();
        let more = if searched < pages {
            crate::i18n::fmt(tl!(" (searching… {s} of {p} pages)"), &[("s", &searched.to_string()), ("p", &pages.to_string())])
        } else {
            String::new()
        };
        // English keeps its singular; Chinese has no plural.
        let count = if n == 1 { crate::i18n::fmt(tl!("1 instance"), &[]) } else { crate::i18n::fmt(tl!("{n} instances"), &[("n", &n.to_string())]) };
        format!("{count}{more}")
    };
    ui.label(egui::RichText::new(status).font(theme::semibold(12.5)));
    ui.add_space(4.0);
    let mut go = None;
    let mut last_page = None;
    for (i, (p, range)) in find.matches.iter().enumerate().take(2000) {
        if last_page != Some(*p) {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(crate::i18n::fmt(tl!("Page {label}"), &[("label", &(p + 1).to_string())])).small().color(t.text_faint));
            last_page = Some(*p);
        }
        let Some(text) = view.texts.get(p) else { continue };
        let n = text.glyphs.len();
        let before = text.text_of(range.start.saturating_sub(CONTEXT)..range.start.min(n));
        let hit = text.text_of(range.start.min(n)..range.end.min(n));
        let after = text.text_of(range.end.min(n)..(range.end + CONTEXT).min(n));
        let flat = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut job = LayoutJob::default();
        let font = FontId::new(12.5, egui::FontFamily::Proportional);
        let plain = TextFormat { font_id: font.clone(), color: t.text_muted, ..Default::default() };
        job.append(&format!("…{} ", flat(before)), 0.0, plain.clone());
        job.append(
            &flat(hit),
            0.0,
            TextFormat {
                font_id: theme::semibold(12.5),
                color: t.text,
                background: Color32::from_rgba_unmultiplied(255, 214, 0, 90),
                ..Default::default()
            },
        );
        job.append(&format!(" {}…", flat(after)), 0.0, plain);
        job.wrap.max_width = ui.available_width();
        let selected = find.current == Some(i);
        let resp = ui.add(egui::Button::selectable(selected, job).wrap()).on_hover_text(tl!("Go to this result"));
        if resp.clicked() {
            go = Some(i);
        }
    }
    // Comment matches (Include comments), after the page text.
    let mut go_comment = None;
    if !find.comment_matches.is_empty() {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("Comments")).small().color(t.text_faint));
        for (k, (p, index)) in find.comment_matches.iter().enumerate().take(MAX_ROWS) {
            let label = crate::i18n::fmt(tl!("Page {label}"), &[("label", &(p + 1).to_string())]);
            let a = info.annotations.iter().find(|a| a.page == *p && a.index == *index);
            let text: String =
                a.and_then(|a| a.contents.as_deref()).unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect();
            let who = a.and_then(|a| a.author.as_deref()).unwrap_or("");
            let selected = find.comment_current == Some(k);
            let row = format!("{label} · {who}: {text}");
            if ui.add(egui::Button::selectable(selected, row).wrap()).on_hover_text(tl!("Go to this result")).clicked() {
                go_comment = Some((k, *p, *index));
            }
        }
    }
    if let Some(i) = go {
        view.go_to_match(i);
        if let Some(f) = view.find.as_mut() {
            f.comment_current = None;
        }
    }
    if let Some((k, p, index)) = go_comment {
        if let Some(f) = view.find.as_mut() {
            f.comment_current = Some(k);
            f.current = None;
        }
        view.comments.selected = Some((p, index));
        view.comments.reveal = true;
        view.go_to_page(p);
    }
}
