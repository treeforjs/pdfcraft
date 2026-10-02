//! printcraft-engine — the façade every frontend talks to (architecture §4).
//!
//! Holds open documents, their edit history and the tool catalogue. Frontends never touch the
//! parsing, rendering or editing crates directly.
//!
//! **Editing model.** Each document keeps a `printcraft_cos::Document` (the object graph with a
//! copy-on-write overlay of edits). An edit runs on a clone, and on success the previous state is
//! pushed onto the undo stack (clones share all unchanged data, so this is cheap). After every
//! edit the *working file* is produced by an incremental write — original bytes plus one
//! appended revision — and the view is refreshed from it, so what you see is exactly what Save
//! will write. Saving rebases onto the written bytes, so the next save appends only new edits.

pub mod catalog;
pub mod commands;
pub mod links;

pub use printcraft_organize::{SplitBy, split_ranges};

/// One file produced by a split: (1-based first page, last page, PDF bytes).
pub use printcraft_organize::LabelStyle;

/// Comment geometry helpers (text-box line breaking) for frontends.
pub use printcraft_annot::appearance as annot_text;
pub use printcraft_annot::{Markup, NewAnnotation, NoteIcon, ReviewState, Rgb, Shape, Style};

pub type SplitPart = (usize, usize, Arc<Vec<u8>>);

use std::sync::Arc;

use printcraft_cos::{SaveOptions, write_full, write_incremental};
use printcraft_render::{DocInfo, OpenError, RenderConfig, RenderPool, inspect};

/// Stable identifier of an open document within a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocId(pub u64);

/// Undo/redo depth. Snapshots share unchanged data, so this is memory-cheap.
const MAX_UNDO: usize = 100;

/// Editing state of a document (absent when the document cannot be edited yet, e.g. encrypted).
#[derive(Clone)]
struct Editor {
    cos: printcraft_cos::Document,
    undo: Vec<(String, printcraft_cos::Document)>,
    redo: Vec<(String, printcraft_cos::Document)>,
}

pub struct Document {
    pub id: DocId,
    pub name: String,
    pub path: Option<String>,
    /// The working file: what Save writes and what is displayed.
    pub bytes: Arc<Vec<u8>>,
    pub info: DocInfo,
    pub renderer: RenderPool,
    /// The password the document was opened with (needed to read attachments, etc.).
    pub password: Option<String>,
    /// `true` when there are edits that have not been saved.
    pub dirty: bool,
    /// Bumped on every change to the working file (edit, undo, redo, save); autosave compares it.
    generation: u64,
    /// The generation last handed out by `autosave_snapshots`.
    snapshot_generation: u64,
    /// Why the document cannot be edited (e.g. encryption), if so.
    pub read_only_reason: Option<String>,
    editor: Option<Editor>,
    config: RenderConfig,
}

impl Document {
    pub fn can_undo(&self) -> Option<&str> {
        self.editor.as_ref().and_then(|e| e.undo.last()).map(|(l, _)| l.as_str())
    }

    pub fn can_redo(&self) -> Option<&str> {
        self.editor.as_ref().and_then(|e| e.redo.last()).map(|(l, _)| l.as_str())
    }

    pub fn editable(&self) -> bool {
        self.editor.is_some()
    }

    /// What the opening password allows; `None` when the document is not encrypted.
    pub fn permissions(&self) -> Option<printcraft_cos::Permissions> {
        self.editor.as_ref().and_then(|e| e.cos.permissions())
    }

    /// Page changes (insert, delete, rotate, move, extract) are allowed.
    pub fn allows_assembly(&self) -> bool {
        self.editable() && self.permissions().is_none_or(|p| p.assemble())
    }

    /// Changes to content and document information are allowed.
    pub fn allows_modification(&self) -> bool {
        self.editable() && self.permissions().is_none_or(|p| p.modify())
    }

    /// Adding and changing comments is allowed (Table 22, bit 6).
    pub fn allows_annotation(&self) -> bool {
        self.editable() && self.permissions().is_none_or(|p| p.annotate())
    }

    /// A summary of the document's security for Document Properties ▸ Security.
    pub fn security_summary(&self) -> Option<SecuritySummary> {
        let editor = self.editor.as_ref()?;
        let h = editor.cos.security()?;
        let d = h.dict();
        let stream = d.crypt_filters.iter().find(|(name, _)| *name == d.stm_f).map(|(_, m)| *m);
        let method = match (d.v, stream) {
            (1..=3, _) if d.length_bits <= 40 => "RC4, 40-bit",
            (1..=3, _) | (_, Some(printcraft_cos::CryptMethod::Rc4)) => "RC4, 128-bit",
            (_, Some(printcraft_cos::CryptMethod::Aes128)) => "AES, 128-bit",
            (_, Some(printcraft_cos::CryptMethod::Aes256)) => "AES, 256-bit",
            _ => "Attachments only",
        };
        Some(SecuritySummary { method: method.into(), owner: h.auth() == printcraft_cos::Auth::Owner, permissions: h.permissions() })
    }

    /// Current value of a document-information entry (Title, Author, …).
    pub fn info_value(&self, key: &str) -> Option<String> {
        self.editor.as_ref().and_then(|e| printcraft_organize::info(&e.cos, key))
    }

    /// Notes about damage repaired while opening (Document Properties ▸ Advanced, notices).
    pub fn repair_log(&self) -> Vec<String> {
        self.editor.as_ref().map(|e| e.cos.repair_log().to_vec()).unwrap_or_default()
    }
}

fn render_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8) - 1
}

/// A document's working file captured for crash recovery.
#[derive(Clone, Debug)]
pub struct RecoverySnapshot {
    pub doc: DocId,
    pub name: String,
    pub path: Option<String>,
    pub bytes: Arc<Vec<u8>>,
    /// The snapshot is encrypted (recovering it asks for the password again).
    pub encrypted: bool,
}

/// Document Properties ▸ Security.
#[derive(Clone, Debug, PartialEq)]
pub struct SecuritySummary {
    /// "AES, 256-bit" etc.
    pub method: String,
    /// Opened with the owner password (no restrictions apply).
    pub owner: bool,
    pub permissions: printcraft_cos::Permissions,
}

/// Edits that can be applied to a document. Page indices are 0-based.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    RotatePages {
        pages: Vec<usize>,
        degrees: i64,
    },
    DeletePages {
        pages: Vec<usize>,
    },
    MovePages {
        pages: Vec<usize>,
        to: usize,
    },
    InsertBlankPage {
        at: usize,
        width: f64,
        height: f64,
    },
    SetInfo {
        key: String,
        value: String,
    },
    /// Insert pages from another PDF (all pages when `pages` is `None`) at position `at`.
    InsertPagesFrom {
        name: String,
        bytes: Arc<Vec<u8>>,
        pages: Option<Vec<usize>>,
        at: usize,
    },
    /// Add a bookmark to `page` as child `index` of the bookmark at `parent` (`[]` = top level).
    AddBookmark {
        parent: Vec<usize>,
        index: usize,
        title: String,
        page: usize,
    },
    RenameBookmark {
        path: Vec<usize>,
        title: String,
    },
    /// Delete a bookmark and the bookmarks under it.
    DeleteBookmark {
        path: Vec<usize>,
    },
    /// Move a bookmark to child `index` of `to_parent` (indices after removing it).
    MoveBookmark {
        from: Vec<usize>,
        to_parent: Vec<usize>,
        index: usize,
    },
    /// Point a bookmark at another page.
    SetBookmarkPage {
        path: Vec<usize>,
        page: usize,
    },
    /// Label pages `from..=to` (0-based) as Acrobat's "Number pages" does; later pages keep their labels.
    NumberPages {
        from: usize,
        to: usize,
        style: printcraft_organize::LabelStyle,
        prefix: String,
        first: u32,
    },
    /// Add a comment (sticky note, highlight, shape, drawing, text box…).
    AddAnnotation(NewAnnotation),
    /// Delete the comment at `index` in the page's `/Annots`, with its pop-up and replies.
    DeleteAnnotation {
        page: usize,
        index: usize,
    },
    SetAnnotationContents {
        page: usize,
        index: usize,
        text: String,
    },
    ReplyToAnnotation {
        page: usize,
        index: usize,
        text: String,
        author: String,
    },
    /// Acrobat's "Set status" (a state reply by `author`).
    SetAnnotationStatus {
        page: usize,
        index: usize,
        state: ReviewState,
        author: String,
    },
    MoveAnnotation {
        page: usize,
        index: usize,
        dx: f64,
        dy: f64,
    },
    /// Resize a rectangle, oval or text box.
    ResizeAnnotation {
        page: usize,
        index: usize,
        rect: [f64; 4],
    },
    StyleAnnotation {
        page: usize,
        index: usize,
        color: Option<Rgb>,
        opacity: Option<f64>,
        width: Option<f64>,
    },
    /// Several edits applied as one undoable step (all or nothing).
    Batch {
        label: String,
        edits: Vec<Edit>,
    },
}

impl Edit {
    /// Label for the Edit menu and history ("Undo Rotate pages").
    pub fn label(&self) -> String {
        match self {
            Edit::RotatePages { pages, .. } => plural("Rotate page", pages.len()),
            Edit::DeletePages { pages } => plural("Delete page", pages.len()),
            Edit::MovePages { pages, .. } => plural("Move page", pages.len()),
            Edit::InsertBlankPage { .. } => "Insert blank page".into(),
            Edit::SetInfo { key, .. } => format!("Change {key}"),
            Edit::InsertPagesFrom { name, .. } => format!("Insert pages from {name}"),
            Edit::AddBookmark { .. } => "Add bookmark".into(),
            Edit::RenameBookmark { .. } => "Rename bookmark".into(),
            Edit::DeleteBookmark { .. } => "Delete bookmark".into(),
            Edit::MoveBookmark { .. } => "Move bookmark".into(),
            Edit::SetBookmarkPage { .. } => "Set bookmark destination".into(),
            Edit::NumberPages { .. } => "Number pages".into(),
            Edit::AddAnnotation(a) => format!("Add {}", annotation_noun(&a.shape)),
            Edit::DeleteAnnotation { .. } => "Delete comment".into(),
            Edit::SetAnnotationContents { .. } => "Edit comment".into(),
            Edit::ReplyToAnnotation { .. } => "Reply".into(),
            Edit::SetAnnotationStatus { state, .. } => format!("Set status {}", state.name()),
            Edit::MoveAnnotation { .. } => "Move comment".into(),
            Edit::ResizeAnnotation { .. } => "Resize comment".into(),
            Edit::StyleAnnotation { .. } => "Change comment properties".into(),
            Edit::Batch { label, .. } => label.clone(),
        }
    }
}

/// What the Edit menu calls a new comment ("Undo Add highlight").
fn annotation_noun(s: &Shape) -> &'static str {
    match s {
        Shape::Note { .. } => "sticky note",
        Shape::TextMarkup { kind: Markup::Highlight, .. } => "highlight",
        Shape::TextMarkup { kind: Markup::Underline, .. } => "underline",
        Shape::TextMarkup { kind: Markup::StrikeOut, .. } => "strikethrough",
        Shape::TextMarkup { kind: Markup::Squiggly, .. } => "squiggly underline",
        Shape::Rectangle { .. } => "rectangle",
        Shape::Oval { .. } => "oval",
        Shape::Line { arrow: true, .. } => "arrow",
        Shape::Line { .. } => "line",
        Shape::Ink { .. } => "drawing",
        Shape::TextBox { .. } => "text box",
    }
}

/// Whether the opening password allows an edit (§7.6.4.2, Table 22).
fn check_permission(edit: &Edit, p: &printcraft_cos::Permissions) -> Result<(), EditError> {
    match edit {
        Edit::RotatePages { .. }
        | Edit::DeletePages { .. }
        | Edit::MovePages { .. }
        | Edit::InsertBlankPage { .. }
        | Edit::InsertPagesFrom { .. }
        // "Assemble the document: insert, rotate or delete pages and create bookmarks" (Table 22).
        | Edit::AddBookmark { .. }
        | Edit::RenameBookmark { .. }
        | Edit::DeleteBookmark { .. }
        | Edit::MoveBookmark { .. }
        | Edit::SetBookmarkPage { .. }
        | Edit::NumberPages { .. } => {
            if p.assemble() {
                Ok(())
            } else {
                Err(EditError::NotPermitted("page changes"))
            }
        }
        Edit::AddAnnotation(_)
        | Edit::DeleteAnnotation { .. }
        | Edit::SetAnnotationContents { .. }
        | Edit::ReplyToAnnotation { .. }
        | Edit::SetAnnotationStatus { .. }
        | Edit::MoveAnnotation { .. }
        | Edit::ResizeAnnotation { .. }
        | Edit::StyleAnnotation { .. } => {
            if p.annotate() {
                Ok(())
            } else {
                Err(EditError::NotPermitted("comments"))
            }
        }
        Edit::SetInfo { .. } => {
            if p.modify() {
                Ok(())
            } else {
                Err(EditError::NotPermitted("changes to the document"))
            }
        }
        Edit::Batch { edits, .. } => edits.iter().try_for_each(|e| check_permission(e, p)),
    }
}

/// Dates and unique ids stamped onto what an edit creates.
struct EditCtx {
    date: Option<String>,
    seed: u64,
    count: u64,
}

impl EditCtx {
    fn new(now: Option<i64>, salt: u64) -> Self {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64 ^ salt;
        if let Some(t) = now {
            seed ^= (t as u64).rotate_left(17);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if now.is_some() {
            // Real clock: mix in sub-second time so ids from two sessions don't collide.
            seed ^= std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as u64).unwrap_or(0) << 32;
        }
        Self { date: now.map(printcraft_cos::pdf_date), seed, count: 0 }
    }

    /// A fresh `/NM`: a random-looking UUID (version 4 layout) from a splitmix64 stream.
    fn meta(&mut self) -> printcraft_annot::Meta {
        let mut next = || {
            self.count += 1;
            let mut z = self.seed.wrapping_add(self.count.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let (a, b) = (next(), next());
        let id = format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            a >> 32,
            (a >> 16) & 0xffff,
            a & 0xfff,
            0x8000 | (b >> 48) & 0x3fff,
            b & 0xffff_ffff_ffff
        );
        printcraft_annot::Meta { date: self.date.clone(), id }
    }
}

/// Perform an edit on a working copy (the caller discards it on error).
fn run_edit(doc: &mut printcraft_cos::Document, edit: &Edit, cx: &mut EditCtx) -> Result<(), EditError> {
    match edit {
        Edit::RotatePages { pages, degrees } => printcraft_organize::rotate_pages(doc, pages, *degrees)?,
        Edit::DeletePages { pages } => printcraft_organize::delete_pages(doc, pages)?,
        Edit::MovePages { pages, to } => printcraft_organize::move_pages(doc, pages, *to)?,
        Edit::InsertBlankPage { at, width, height } => {
            printcraft_organize::insert_blank_page(doc, *at, *width, *height)?;
        }
        Edit::SetInfo { key, value } => printcraft_organize::set_info(doc, key, value)?,
        Edit::InsertPagesFrom { name, bytes, pages, at } => {
            let src = open_source(name, bytes)?;
            let pages = match pages {
                Some(p) => p.clone(),
                None => (0..printcraft_organize::page_count(&src)?).collect(),
            };
            printcraft_organize::import_pages(doc, &src, &pages, *at)?;
        }
        Edit::AddBookmark { parent, index, title, page } => {
            printcraft_organize::add_bookmark(doc, parent, *index, title, *page)?;
        }
        Edit::RenameBookmark { path, title } => printcraft_organize::rename_bookmark(doc, path, title)?,
        Edit::DeleteBookmark { path } => printcraft_organize::delete_bookmark(doc, path)?,
        Edit::MoveBookmark { from, to_parent, index } => {
            printcraft_organize::move_bookmark(doc, from, to_parent, *index)?;
        }
        Edit::SetBookmarkPage { path, page } => printcraft_organize::set_bookmark_page(doc, path, *page)?,
        Edit::NumberPages { from, to, style, prefix, first } => printcraft_organize::number_pages(doc, *from, *to, *style, prefix, *first)?,
        Edit::AddAnnotation(a) => {
            printcraft_annot::add_annotation(doc, a, &cx.meta())?;
        }
        Edit::DeleteAnnotation { page, index } => printcraft_annot::delete_annotation(doc, *page, *index)?,
        Edit::SetAnnotationContents { page, index, text } => printcraft_annot::set_contents(doc, *page, *index, text, &cx.meta())?,
        Edit::ReplyToAnnotation { page, index, text, author } => {
            printcraft_annot::add_reply(doc, *page, *index, text, author, &cx.meta())?;
        }
        Edit::SetAnnotationStatus { page, index, state, author } => {
            printcraft_annot::set_review_state(doc, *page, *index, *state, author, &cx.meta())?;
        }
        Edit::MoveAnnotation { page, index, dx, dy } => printcraft_annot::move_annotation(doc, *page, *index, *dx, *dy, &cx.meta())?,
        Edit::ResizeAnnotation { page, index, rect } => printcraft_annot::set_rect(doc, *page, *index, *rect, &cx.meta())?,
        Edit::StyleAnnotation { page, index, color, opacity, width } => {
            printcraft_annot::set_style(doc, *page, *index, *color, *opacity, *width, &cx.meta())?;
        }
        Edit::Batch { edits, .. } => {
            for e in edits {
                run_edit(doc, e, cx)?;
            }
        }
    }
    Ok(())
}

/// Parse another PDF to copy pages from.
fn open_source(name: &str, bytes: &Arc<Vec<u8>>) -> Result<printcraft_cos::Document, EditError> {
    match std::panic::catch_unwind(|| printcraft_cos::Document::open(bytes.clone())) {
        Ok(Ok(d)) if d.permissions().is_some_and(|p| !p.assemble()) => {
            Err(EditError::Source(format!("{name}: its security settings don't allow copying pages")))
        }
        Ok(Ok(d)) => Ok(d),
        Ok(Err(printcraft_cos::CosError::NeedsPassword)) => Err(EditError::Source(format!("{name}: it is password-protected"))),
        Ok(Err(e)) => Err(EditError::Source(format!("{name}: {e}"))),
        Err(_) => Err(EditError::Source(format!("{name}: the file could not be read"))),
    }
}

fn plural(s: &str, n: usize) -> String {
    if n == 1 { s.to_string() } else { format!("{s}s") }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum EditError {
    #[error("no such document")]
    NoDocument,
    #[error("this document can't be edited: {0}")]
    ReadOnly(String),
    #[error("the document's security settings don't allow {0}; open it with the owner password to make this change")]
    NotPermitted(&'static str),
    #[error("{0}")]
    Organize(#[from] printcraft_organize::OrganizeError),
    #[error("{0}")]
    Bookmark(#[from] printcraft_organize::OutlineError),
    #[error("{0}")]
    Comment(#[from] printcraft_annot::AnnotError),
    #[error("the edited document could not be written: {0}")]
    Write(String),
    #[error("the edited document could not be reopened: {0}")]
    Reopen(String),
    #[error("couldn't use {0}")]
    Source(String),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
}

#[derive(Default)]
pub struct Session {
    docs: Vec<Document>,
    next_id: u64,
    /// Seconds since the Unix epoch, injected so saves are deterministic in tests.
    clock: Option<fn() -> i64>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a fixed clock (tests) instead of the system time.
    pub fn with_clock(mut self, clock: fn() -> i64) -> Self {
        self.clock = Some(clock);
        self
    }

    fn now(&self) -> Option<i64> {
        if let Some(c) = self.clock {
            return Some(c());
        }
        #[cfg(not(target_arch = "wasm32"))]
        return std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64);
        #[cfg(target_arch = "wasm32")]
        return None;
    }

    /// Open a document from bytes. Rendering starts lazily when pages are requested.
    ///
    /// `password` is tried as either the user or owner password when the file is encrypted.
    pub fn open(&mut self, name: impl Into<String>, path: Option<String>, bytes: Arc<Vec<u8>>, password: Option<&str>) -> Result<DocId, OpenError> {
        let cos = std::panic::catch_unwind(|| printcraft_cos::Document::open_with_password(bytes.clone(), password));
        // The renderer authenticates on its own. It cannot use the owner password of R2–R4
        // files, so give it the user password that owner authentication recovers.
        let (info, render_password) = match inspect(bytes.clone(), password) {
            Ok(info) => (info, password.map(str::to_owned)),
            Err(OpenError::WrongPassword) => {
                let user = match &cos {
                    Ok(Ok(d)) => d.security().and_then(|s| s.recovered_user_password()),
                    _ => None,
                };
                let user: String = user.ok_or(OpenError::WrongPassword)?.iter().map(|b| char::from(*b)).collect();
                (inspect(bytes.clone(), Some(&user))?, Some(user))
            }
            Err(e) => return Err(e),
        };
        let config = RenderConfig { password: render_password.as_deref().map(Arc::from), ..Default::default() };
        let renderer = RenderPool::new(bytes.clone(), render_threads(), config.clone());
        let (editor, read_only_reason) = match cos {
            Ok(Ok(cos)) => (Some(Editor { cos, undo: Vec::new(), redo: Vec::new() }), None),
            Ok(Err(e)) => (None, Some(e.to_string())),
            Err(_) => (None, Some("the document structure could not be read for editing".into())),
        };
        self.next_id += 1;
        let id = DocId(self.next_id);
        self.docs.push(Document {
            id,
            name: name.into(),
            path,
            bytes,
            info,
            renderer,
            password: render_password,
            dirty: false,
            generation: 0,
            snapshot_generation: 0,
            read_only_reason,
            editor,
            config,
        });
        Ok(id)
    }

    fn doc_mut(&mut self, id: DocId) -> Result<&mut Document, EditError> {
        self.docs.iter_mut().find(|d| d.id == id).ok_or(EditError::NoDocument)
    }

    /// Apply an edit. On success the previous state is undoable and the view data is refreshed.
    pub fn apply(&mut self, id: DocId, edit: Edit) -> Result<(), EditError> {
        let now = self.now();
        let doc = self.doc_mut(id)?;
        let mut cx = EditCtx::new(now, doc.generation ^ (id.0 << 48));
        let reason = doc.read_only_reason.clone().unwrap_or_default();
        let editor = doc.editor.as_mut().ok_or(EditError::ReadOnly(reason))?;
        if let Some(p) = editor.cos.permissions() {
            check_permission(&edit, &p)?;
        }
        let mut next = editor.cos.clone();
        run_edit(&mut next, &edit, &mut cx)?;
        let previous = std::mem::replace(&mut editor.cos, next);
        editor.undo.push((edit.label(), previous));
        if editor.undo.len() > MAX_UNDO {
            editor.undo.remove(0);
        }
        editor.redo.clear();
        if let Err(e) = Self::refresh(doc) {
            // Roll back: the edit produced something we cannot display.
            if let Some(ed) = doc.editor.as_mut()
                && let Some((_, prev)) = ed.undo.pop()
            {
                ed.cos = prev;
            }
            let _ = Self::refresh(doc);
            return Err(e);
        }
        doc.dirty = true;
        doc.generation += 1;
        Ok(())
    }

    pub fn undo(&mut self, id: DocId) -> Result<String, EditError> {
        let doc = self.doc_mut(id)?;
        let editor = doc.editor.as_mut().ok_or(EditError::NothingToUndo)?;
        let (label, prev) = editor.undo.pop().ok_or(EditError::NothingToUndo)?;
        let current = std::mem::replace(&mut editor.cos, prev);
        editor.redo.push((label.clone(), current));
        Self::refresh(doc)?;
        doc.dirty = true;
        doc.generation += 1;
        Ok(label)
    }

    pub fn redo(&mut self, id: DocId) -> Result<String, EditError> {
        let doc = self.doc_mut(id)?;
        let editor = doc.editor.as_mut().ok_or(EditError::NothingToRedo)?;
        let (label, next) = editor.redo.pop().ok_or(EditError::NothingToRedo)?;
        let current = std::mem::replace(&mut editor.cos, next);
        editor.undo.push((label.clone(), current));
        Self::refresh(doc)?;
        doc.dirty = true;
        doc.generation += 1;
        Ok(label)
    }

    /// Rebuild working bytes, inspection and renderer from the current edit state.
    fn refresh(doc: &mut Document) -> Result<(), EditError> {
        let Some(editor) = doc.editor.as_ref() else { return Ok(()) };
        let bytes = if editor.cos.is_modified() {
            Arc::new(write_incremental(&editor.cos, &SaveOptions::default()).map_err(|e| EditError::Write(e.to_string()))?)
        } else {
            editor.cos.bytes().clone()
        };
        let info = inspect(bytes.clone(), doc.password.as_deref()).map_err(|e| EditError::Reopen(e.to_string()))?;
        // Keep the user's layer choices where the layers still exist.
        let mut info = info;
        for l in &mut info.layers {
            if let Some(old) = doc.info.layers.iter().find(|o| o.id == l.id) {
                l.visible = old.visible;
            }
        }
        doc.info = info;
        doc.bytes = bytes.clone();
        doc.renderer = RenderPool::new(bytes, render_threads(), doc.config.clone());
        Ok(())
    }

    /// The bytes to write for Save: an incremental update of the file as opened/last saved,
    /// with `/ModDate` stamped. Call `mark_saved` after writing them successfully.
    pub fn save_bytes(&self, id: DocId) -> Result<Arc<Vec<u8>>, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let Some(editor) = doc.editor.as_ref() else { return Ok(doc.bytes.clone()) };
        if !editor.cos.is_modified() {
            return Ok(editor.cos.bytes().clone());
        }
        let opts = SaveOptions { mod_date: self.now().map(printcraft_cos::pdf_date), ..SaveOptions::default() };
        write_incremental(&editor.cos, &opts).map(Arc::new).map_err(|e| EditError::Write(e.to_string()))
    }

    /// A compact, garbage-collected rewrite (Save As ▸ "Optimized" / Reduce File Size groundwork).
    pub fn save_full_bytes(&self, id: DocId) -> Result<Arc<Vec<u8>>, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let editor = doc.editor.as_ref().ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))?;
        let opts = SaveOptions { mod_date: self.now().map(printcraft_cos::pdf_date), ..SaveOptions::default() };
        write_full(&editor.cos, &opts).map(Arc::new).map_err(|e| EditError::Write(e.to_string()))
    }

    /// Record a successful save of `bytes` (to `path`, if any): rebase editing on the saved file
    /// so the next save appends only newer edits. Undo history is kept.
    pub fn mark_saved(&mut self, id: DocId, bytes: Arc<Vec<u8>>, path: Option<String>) -> Result<(), EditError> {
        let doc = self.doc_mut(id)?;
        if let Some(editor) = doc.editor.as_mut() {
            editor.cos = printcraft_cos::Document::open(bytes.clone()).map_err(|e| EditError::Reopen(e.to_string()))?;
        }
        if let Some(p) = path {
            doc.name = std::path::Path::new(&p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
            doc.path = Some(p);
        }
        doc.dirty = false;
        doc.generation += 1;
        Self::refresh(doc)
    }

    /// Combine whole files, in order, into new PDF bytes (one bookmark per file).
    pub fn combine(&self, sources: &[(String, Arc<Vec<u8>>)]) -> Result<Arc<Vec<u8>>, EditError> {
        let docs = sources.iter().map(|(n, b)| open_source(n, b)).collect::<Result<Vec<_>, _>>()?;
        let named: Vec<(&str, &printcraft_cos::Document)> = sources.iter().map(|(n, _)| n.as_str()).zip(docs.iter()).collect();
        let out = printcraft_organize::combine(&named)?;
        self.write_new(&out)
    }

    /// New PDF bytes containing copies of `pages` of the document (Extract Pages).
    pub fn extract(&self, id: DocId, pages: &[usize]) -> Result<Arc<Vec<u8>>, EditError> {
        let src = self.cos(id)?;
        if src.permissions().is_some_and(|p| !p.assemble()) {
            return Err(EditError::NotPermitted("extracting pages"));
        }
        let out = printcraft_organize::extract_pages(src, pages)?;
        self.write_new(&out)
    }

    /// Split the document into several new PDFs.
    pub fn split(&self, id: DocId, by: &printcraft_organize::SplitBy) -> Result<Vec<SplitPart>, EditError> {
        let src = self.cos(id)?;
        if src.permissions().is_some_and(|p| !p.assemble()) {
            return Err(EditError::NotPermitted("splitting the document"));
        }
        let n = printcraft_organize::page_count(src)?;
        printcraft_organize::split_ranges(n, by)
            .into_iter()
            .map(|r| {
                let doc = printcraft_organize::extract_pages(src, &r.clone().collect::<Vec<_>>())?;
                Ok((r.start + 1, r.end, self.write_new(&doc)?))
            })
            .collect()
    }

    /// Open freshly created bytes (combine / extract) as a new, unsaved document.
    pub fn open_new(&mut self, name: impl Into<String>, bytes: Arc<Vec<u8>>) -> Result<DocId, OpenError> {
        let id = self.open(name, None, bytes, None)?;
        if let Some(d) = self.docs.iter_mut().find(|d| d.id == id) {
            d.dirty = true;
        }
        Ok(id)
    }

    fn cos(&self, id: DocId) -> Result<&printcraft_cos::Document, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        doc.editor.as_ref().map(|e| &e.cos).ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))
    }

    fn write_new(&self, doc: &printcraft_cos::Document) -> Result<Arc<Vec<u8>>, EditError> {
        let opts = SaveOptions { mod_date: self.now().map(printcraft_cos::pdf_date), ..SaveOptions::default() };
        write_full(doc, &opts).map(Arc::new).map_err(|e| EditError::Write(e.to_string()))
    }

    /// Documents with unsaved changes made since the last call: their current working file,
    /// for crash recovery. Encrypted documents stay encrypted in the snapshot.
    pub fn autosave_snapshots(&mut self) -> Vec<RecoverySnapshot> {
        let mut out = Vec::new();
        for d in &mut self.docs {
            if d.dirty && d.generation != d.snapshot_generation {
                d.snapshot_generation = d.generation;
                out.push(RecoverySnapshot {
                    doc: d.id,
                    name: d.name.clone(),
                    path: d.path.clone(),
                    bytes: d.bytes.clone(),
                    encrypted: d.info.encrypted || d.editor.as_ref().is_some_and(|e| e.cos.security().is_some()),
                });
            }
        }
        out
    }

    /// Mark a document opened from a recovery file: it has unsaved changes and belongs at
    /// `path` (where Save writes), as when the session ended.
    pub fn mark_recovered(&mut self, id: DocId, path: Option<String>) {
        if let Some(d) = self.docs.iter_mut().find(|d| d.id == id) {
            d.path = path;
            d.dirty = true;
            d.generation += 1;
            d.snapshot_generation = d.generation; // its bytes are already in the recovery store
        }
    }

    /// Show or hide a layer (optional content group) for viewing. Returns `true` if it changed;
    /// callers must drop cached rasters and text for the document.
    pub fn set_layer_visible(&mut self, id: DocId, layer: usize, visible: bool) -> bool {
        let Some(doc) = self.docs.iter_mut().find(|d| d.id == id) else { return false };
        let Some(l) = doc.info.layers.get_mut(layer) else { return false };
        if l.visible == visible {
            return false;
        }
        l.visible = visible;
        let overrides: Vec<(i32, i32, bool)> = doc.info.layers.iter().map(|l| (l.id.0 as i32, l.id.1 as i32, l.visible)).collect();
        doc.config.layers = Arc::new(overrides);
        doc.renderer = RenderPool::new(doc.bytes.clone(), render_threads(), doc.config.clone());
        true
    }

    pub fn close(&mut self, id: DocId) {
        self.docs.retain(|d| d.id != id);
    }

    pub fn get(&self, id: DocId) -> Option<&Document> {
        self.docs.iter().find(|d| d.id == id)
    }

    pub fn docs(&self) -> &[Document] {
        &self.docs
    }
}

#[cfg(test)]
mod tests;
