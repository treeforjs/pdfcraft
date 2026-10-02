//! printcraft-automation — agent control for PrintCraft (architecture §13).
//!
//! - **Layer:** L7. Headless: depends on the engine, never on a UI toolkit.
//! - [`Automation`] is a tool set over an engine [`Session`]: open, inspect, render, extract and
//!   find text, edit pages and metadata, undo/redo, save, combine, extract and split. Every tool
//!   has a JSON Schema ([`tools`]) and takes and returns JSON, so the same table drives the MCP
//!   server ([`mcp`]), `printcraft-cli run` and (later) the UI control channel.
//! - Pages are **1-based** in every tool, as people number them. Rectangles are in PDF points
//!   with the origin at the top-left of the displayed page.
//! - An optional root directory confines every path a tool reads or writes.

mod comments;
mod content;
mod forms;
mod links;
#[cfg(feature = "mcp")]
pub mod mcp;
mod printing;
mod redact;
mod tools;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use printcraft_engine::{DocId, Document, Edit, Session, commands};
use printcraft_render::{PageRenderer, PageText, RenderConfig, RenderRequest, RequestKind};
use serde_json::{Value, json};

pub use tools::{ToolDef, tools};

/// One piece of a tool's result.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    /// Structured data (MCP: a text block with the JSON plus `structuredContent`).
    Json(Value),
    /// A PNG image.
    Png { data: Vec<u8>, width: u32, height: u32 },
}

/// Why a tool call failed.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolError {
    /// No tool has this name.
    UnknownTool(String),
    /// The arguments don't match the tool's schema.
    InvalidArgs(String),
    /// The tool ran and failed (the message is for the agent to read).
    Failed(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::UnknownTool(t) => write!(f, "unknown tool {t:?}"),
            ToolError::InvalidArgs(m) | ToolError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ToolError {}

type Result<T> = std::result::Result<T, ToolError>;

fn failed(e: impl std::fmt::Display) -> ToolError {
    ToolError::Failed(e.to_string())
}

/// Default and maximum resolution for `page_render`.
const DEFAULT_DPI: f64 = 96.0;
const MAX_DPI: f64 = 600.0;

/// Page texts of one document version: (the working bytes, one slot per page).
type TextCache = (Arc<Vec<u8>>, Vec<Option<Arc<PageText>>>);

/// A headless PrintCraft session driven by tool calls.
pub struct Automation {
    session: Session,
    root: Option<PathBuf>,
    /// Synchronous renderers, rebuilt when a document's working bytes change.
    renderers: HashMap<DocId, (Arc<Vec<u8>>, PageRenderer)>,
    /// Extracted page text per document version (the working bytes it was taken from).
    texts: HashMap<DocId, TextCache>,
}

impl Default for Automation {
    fn default() -> Self {
        Self::new()
    }
}

impl Automation {
    pub fn new() -> Self {
        Self { session: Session::new(), root: None, renderers: HashMap::new(), texts: HashMap::new() }
    }

    /// Confine every path the tools read or write to `root` (relative paths resolve inside it).
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> std::io::Result<Self> {
        self.root = Some(root.into().canonicalize()?);
        Ok(self)
    }

    /// Use a fixed clock for saves (deterministic output in tests).
    pub fn with_clock(mut self, clock: fn() -> i64) -> Self {
        self.session = std::mem::take(&mut self.session).with_clock(clock);
        self
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Run the tool `name` with JSON `args` (an object; `null` means no arguments).
    pub fn call(&mut self, name: &str, args: &Value) -> Result<Vec<Content>> {
        let empty = json!({});
        let args = if args.is_null() { &empty } else { args };
        if !args.is_object() {
            return Err(ToolError::InvalidArgs("arguments must be a JSON object".into()));
        }
        let def = tools::find(name).ok_or_else(|| ToolError::UnknownTool(name.into()))?;
        tools::check_args(def, args)?;
        let a = Args(args);
        let out = match name {
            "doc_open" => self.doc_open(&a)?,
            "doc_list" => json!({ "documents": self.session.docs().iter().map(summary).collect::<Vec<_>>() }),
            "doc_info" => info(self.doc(&a)?),
            "doc_close" => self.doc_close(&a)?,
            "doc_save" => self.doc_save(&a)?,
            "doc_set_info" => {
                let edit = Edit::SetInfo { key: a.str("key")?.into(), value: a.str("value")?.into() };
                self.apply(&a, edit)?
            }
            "page_render" => return self.page_render(&a).map(|c| vec![c]),
            "text_extract" => self.text_extract(&a)?,
            "text_find" => self.text_find(&a)?,
            "page_rotate" => {
                let degrees = a.int("degrees")?;
                if degrees % 90 != 0 {
                    return Err(ToolError::InvalidArgs("degrees must be a multiple of 90".into()));
                }
                let doc = self.doc(&a)?;
                let base = match a.opt_ints("pages")? {
                    Some(_) => self.pages(&a, "pages")?,
                    None => (0..doc.info.pages.len()).collect(),
                };
                let parity = match a.opt_str("subset")?.unwrap_or("all") {
                    "all" => printcraft_engine::PageParity::Both,
                    "even" => printcraft_engine::PageParity::Even,
                    "odd" => printcraft_engine::PageParity::Odd,
                    s => return Err(ToolError::InvalidArgs(format!("unknown subset {s:?} (all, even, odd)"))),
                };
                let orientation = match a.opt_str("orientation")?.unwrap_or("all") {
                    "all" => printcraft_engine::PageOrientation::Both,
                    "landscape" => printcraft_engine::PageOrientation::Landscape,
                    "portrait" => printcraft_engine::PageOrientation::Portrait,
                    o => return Err(ToolError::InvalidArgs(format!("unknown orientation {o:?} (all, landscape, portrait)"))),
                };
                let pages = printcraft_engine::filter_pages(&self.doc(&a)?.info, &base, parity, orientation);
                if pages.is_empty() {
                    return Err(failed("no pages match the filters"));
                }
                let n = pages.len();
                let mut out = self.apply(&a, Edit::RotatePages { pages, degrees })?;
                out["rotated"] = json!(n);
                out
            }
            "page_delete" => {
                let pages = self.pages(&a, "pages")?;
                self.apply(&a, Edit::DeletePages { pages })?
            }
            "page_move" => {
                let pages = self.pages(&a, "pages")?;
                let to = self.position(&a, "to")?;
                self.apply(&a, Edit::MovePages { pages, to })?
            }
            "page_insert_blank" => self.insert_blank(&a)?,
            "page_insert_file" => self.insert_file(&a)?,
            "page_extract" => self.page_extract(&a)?,
            "doc_combine" => self.doc_combine(&a)?,
            "doc_split" => self.doc_split(&a)?,
            "edit_undo" => {
                let id = self.doc(&a)?.id;
                let label = self.session.undo(id).map_err(failed)?;
                json!({ "undone": label, "document": summary(self.doc(&a)?) })
            }
            "edit_redo" => {
                let id = self.doc(&a)?.id;
                let label = self.session.redo(id).map_err(failed)?;
                json!({ "redone": label, "document": summary(self.doc(&a)?) })
            }
            "command_list" => self.command_list(&a)?,
            "page_number" => {
                use printcraft_organize::LabelStyle as L;
                let n = self.doc(&a)?.info.pages.len();
                let (from, to) = (a.int("from")?, a.int("to")?);
                if from < 1 || to < from || to as usize > n {
                    return Err(ToolError::InvalidArgs(format!("from and to must satisfy 1 ≤ from ≤ to ≤ {n}")));
                }
                let style = match a.opt_str("style")?.unwrap_or("decimal") {
                    "decimal" => L::Decimal,
                    "upper-roman" => L::UpperRoman,
                    "lower-roman" => L::LowerRoman,
                    "upper-alpha" => L::UpperAlpha,
                    "lower-alpha" => L::LowerAlpha,
                    "none" => L::None,
                    other => return Err(ToolError::InvalidArgs(format!("unknown style {other:?}"))),
                };
                let prefix = a.opt_str("prefix")?.unwrap_or_default().to_string();
                let first = a.opt_int("start")?.unwrap_or(1).clamp(1, u32::MAX as i64) as u32;
                let mut out = self.apply(&a, Edit::NumberPages { from: from as usize - 1, to: to as usize - 1, style, prefix, first })?;
                out["labels"] = json!(self.doc(&a)?.info.pages.iter().map(|p| p.label.clone()).collect::<Vec<_>>());
                out
            }
            "bookmark_list" => json!({ "bookmarks": bookmark_tree(&self.doc(&a)?.info.outline, &[]) }),
            "bookmark_add" => {
                let page = self.page(&a)?;
                let parent = a.opt_path("parent")?.unwrap_or_default();
                let index = a.opt_int("position")?.map_or(usize::MAX, |p| (p.max(1) - 1) as usize);
                let title = a.str("title")?.to_string();
                self.apply(&a, Edit::AddBookmark { parent, index, title, page })?
            }
            "bookmark_rename" => {
                let (path, title) = (a.path("path")?, a.str("title")?.to_string());
                self.apply(&a, Edit::RenameBookmark { path, title })?
            }
            "bookmark_delete" => {
                let path = a.path("path")?;
                self.apply(&a, Edit::DeleteBookmark { path })?
            }
            "bookmark_move" => {
                let from = a.path("path")?;
                let to_parent = a.opt_path("parent")?.unwrap_or_default();
                let index = a.opt_int("position")?.map_or(usize::MAX, |p| (p.max(1) - 1) as usize);
                self.apply(&a, Edit::MoveBookmark { from, to_parent, index })?
            }
            "bookmark_set_page" => {
                let (path, page) = (a.path("path")?, self.page(&a)?);
                self.apply(&a, Edit::SetBookmarkPage { path, page })?
            }
            "doc_protect" => self.doc_protect(&a)?,
            "page_replace" => {
                let pages = self.pages(&a, "pages")?;
                let path = self.resolve(a.str("path")?, false)?;
                let bytes = Arc::new(std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?);
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let src_pages = match a.opt_ints("from_pages")? {
                    Some(p) => one_based(&p)?,
                    None => (0..pages.len()).collect(),
                };
                self.apply(&a, Edit::ReplacePages { pages, name, bytes, src_pages })?
            }
            "page_duplicate" => {
                let pages = self.pages(&a, "pages")?;
                self.apply(&a, Edit::DuplicatePages { pages })?
            }
            "page_set_box" => self.page_set_box(&a)?,
            "doc_create" => self.doc_create(&a)?,
            "doc_flatten" => {
                let (comments, fields) = (a.opt_bool("comments")?.unwrap_or(true), a.opt_bool("fields")?.unwrap_or(true));
                if !comments && !fields {
                    return Err(ToolError::InvalidArgs("nothing to flatten".into()));
                }
                self.apply(&a, Edit::Flatten { comments, fields })?
            }
            "doc_reduce" => {
                let id = self.doc(&a)?.id;
                let before = self.doc(&a)?.bytes.len();
                let path = self.resolve(a.str("path")?, true)?;
                let (bytes, merged) = self.session.reduced_bytes(id).map_err(failed)?;
                write_atomic(&path, &bytes)?;
                json!({ "path": path.to_string_lossy(), "bytes_before": before, "bytes_after": bytes.len(), "merged_objects": merged })
            }
            "doc_export_images" | "doc_export_text" => self.export(name, &a)?,
            "doc_header_footer" | "doc_watermark" | "doc_background" | "doc_remove_marks" => self.marks(name, &a)?,
            "doc_unprotect" => {
                let mut out = self.apply(&a, Edit::RemoveProtection)?;
                out["security"] = security(self.doc(&a)?);
                out
            }
            "form_fields" => self.form_fields(&a)?,
            "form_fill" => self.form_fill(&a)?,
            "form_reset" => self.form_reset(&a)?,
            "form_add_field" => self.form_add_field(&a)?,
            "form_set_props" => self.form_set_props(&a)?,
            "form_delete_field" => self.form_delete_field(&a)?,
            "form_tab_order" => self.form_tab_order(&a)?,
            "doc_export_data" => self.doc_export_data(&a)?,
            "doc_import_data" => self.doc_import_data(&a)?,
            "redact_mark" => self.redact_mark(&a)?,
            "redact_apply" => self.redact_apply(&a)?,
            "redact_clear" => self.redact_clear(&a)?,
            "doc_hidden_info" => self.doc_hidden_info(&a)?,
            "printers" => self.printers()?,
            "link_list" => self.link_list(&a)?,
            "link_add" => self.link_add(&a)?,
            "link_edit" => self.link_edit(&a)?,
            "link_delete" => self.link_delete(&a)?,
            "links_from_urls" => self.links_from_urls(&a)?,
            "links_remove" => self.links_remove(&a)?,
            "content_list" => self.content_list(&a)?,
            "page_add_text" => self.page_add_text(&a)?,
            "page_add_image" => self.page_add_image(&a)?,
            "content_update" => self.content_update(&a)?,
            "content_delete" => self.content_delete(&a)?,
            "doc_print" => self.doc_print(&a)?,
            "doc_remove_hidden" => self.doc_remove_hidden(&a)?,
            "fill_sign_add" => self.fill_sign_add(&a)?,
            "comment_list" => self.comment_list(&a)?,
            "comment_add" => self.comment_add(&a)?,
            "comment_reply" => self.comment_reply(&a)?,
            "comment_set_status" => self.comment_set_status(&a)?,
            "comment_edit" => self.comment_edit(&a)?,
            "comment_delete" => self.comment_delete(&a)?,
            other => return Err(ToolError::UnknownTool(other.into())),
        };
        Ok(vec![Content::Json(out)])
    }

    // ---- documents ---------------------------------------------------------------------------

    fn doc(&self, a: &Args) -> Result<&Document> {
        let id = a.int("doc")?;
        let id = u64::try_from(id).map_err(|_| ToolError::InvalidArgs("doc must be positive".into()))?;
        self.session.get(DocId(id)).ok_or_else(|| ToolError::Failed(format!("no open document with id {id} (see doc_list)")))
    }

    fn doc_open(&mut self, a: &Args) -> Result<Value> {
        let path = self.resolve(a.str("path")?, false)?;
        let bytes = std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let id = self.session.open(name, Some(path.to_string_lossy().into_owned()), Arc::new(bytes), a.opt_str("password")?).map_err(failed)?;
        let doc = self.session.get(id).ok_or_else(|| failed("the document vanished"))?;
        Ok(summary(doc))
    }

    fn doc_close(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let id = doc.id;
        if doc.dirty && !a.opt_bool("discard_changes")?.unwrap_or(false) {
            return Err(failed("the document has unsaved changes: save it with doc_save, or pass discard_changes: true"));
        }
        self.session.close(id);
        self.renderers.remove(&id);
        self.texts.remove(&id);
        Ok(json!({ "closed": id.0 }))
    }

    fn doc_save(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let id = doc.id;
        let target = match a.opt_str("path")? {
            Some(p) => self.resolve(p, true)?,
            None => PathBuf::from(doc.path.clone().ok_or_else(|| failed("the document has never been saved: pass a path"))?),
        };
        let same_file = doc.path.as_deref().is_some_and(|p| Path::new(p) == target);
        // Saving to a new file is a full rewrite unless asked otherwise, like Save As.
        let full = a.opt_bool("full")?.unwrap_or(!same_file);
        let bytes = if full { self.session.save_full_bytes(id) } else { self.session.save_bytes(id) }.map_err(failed)?;
        write_atomic(&target, &bytes)?;
        let path = target.to_string_lossy().into_owned();
        self.session.mark_saved(id, bytes.clone(), Some(path.clone())).map_err(failed)?;
        Ok(json!({ "path": path, "bytes": bytes.len(), "incremental": !full, "document": summary(self.doc(a)?) }))
    }

    fn apply(&mut self, a: &Args, edit: Edit) -> Result<Value> {
        let id = self.doc(a)?.id;
        self.session.apply(id, edit).map_err(failed)?;
        Ok(summary(self.doc(a)?))
    }

    fn marks(&mut self, tool: &str, a: &Args) -> Result<Value> {
        use printcraft_engine::{Background, HeaderFooter, MarkKind, Watermark};
        let n = self.doc(a)?.info.pages.len();
        let pages = match a.opt_ints("pages")? {
            Some(_) => self.pages(a, "pages")?,
            None => (0..n).collect(),
        };
        let color =
            |key: &str, default: [f64; 3]| -> Result<[f64; 3]> { Ok(a.opt_str(key)?.map(comments::parse_color).transpose()?.unwrap_or(default)) };
        let replace = a.opt_bool("replace")?.unwrap_or(false);
        let edit = match tool {
            "doc_header_footer" => {
                let mut hf = HeaderFooter::default();
                for (k, key) in ["header_left", "header_center", "header_right", "footer_left", "footer_center", "footer_right"].iter().enumerate() {
                    hf.text[k] = a.opt_str(key)?.unwrap_or_default().to_string();
                }
                if let Some(s) = a.opt_num("font_size")? {
                    hf.font_size = s;
                }
                hf.color = color("color", hf.color)?;
                if let Some(m) = a.get("margins").and_then(Value::as_array) {
                    let m: Vec<f64> = m.iter().filter_map(Value::as_f64).collect();
                    hf.margins = <[f64; 4]>::try_from(m).map_err(|_| ToolError::InvalidArgs("margins must be 4 numbers".into()))?;
                }
                if let Some(s) = a.opt_int("start_number")? {
                    hf.start_number = s.clamp(1, u32::MAX as i64) as u32;
                }
                Edit::AddHeaderFooter { pages, settings: hf, replace }
            }
            "doc_watermark" => {
                let d = Watermark::default();
                let wm = Watermark {
                    text: a.str("text")?.to_string(),
                    font_size: a.opt_num("font_size")?.unwrap_or(0.0),
                    color: color("color", d.color)?,
                    opacity: a.opt_num("opacity")?.unwrap_or(d.opacity),
                    rotation: a.opt_num("rotation")?.unwrap_or(d.rotation),
                    behind: a.opt_bool("behind")?.unwrap_or(false),
                    offset: [0.0; 2],
                };
                Edit::AddWatermark { pages, settings: wm, replace }
            }
            "doc_background" => Edit::AddBackground {
                pages,
                settings: Background { color: color("color", [1.0; 3])?, opacity: a.opt_num("opacity")?.unwrap_or(1.0) },
                replace,
            },
            _ => {
                let kind = match a.str("kind")? {
                    "header_footer" => MarkKind::HeaderFooter,
                    "watermark" => MarkKind::Watermark,
                    "background" => MarkKind::Background,
                    other => return Err(ToolError::InvalidArgs(format!("unknown kind {other:?}"))),
                };
                Edit::RemoveMarks { kind }
            }
        };
        self.apply(a, edit)
    }

    fn export(&mut self, tool: &str, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let pages = match a.opt_ints("pages")? {
            Some(_) => self.pages(a, "pages")?,
            None => (0..doc.info.pages.len()).collect(),
        };
        let stem = doc.name.trim_end_matches(".pdf").trim_end_matches(".PDF").to_string();
        let mut ex = printcraft_engine::export::Exporter::new(doc);
        if tool == "doc_export_text" {
            let path = self.resolve(a.str("path")?, true)?;
            let text = ex.text_of(&pages).map_err(failed)?;
            write_atomic(&path, text.as_bytes())?;
            return Ok(json!({ "path": path.to_string_lossy(), "pages": pages.len(), "bytes": text.len() }));
        }
        let folder = self.resolve(a.str("folder")?, true)?;
        std::fs::create_dir_all(&folder).map_err(|e| failed(format!("{}: {e}", folder.display())))?;
        let dpi = a.opt_num("dpi")?.unwrap_or(150.0);
        let quality = a.opt_int("quality")?.unwrap_or(85).clamp(1, 100) as u8;
        let format = match a.opt_str("format")?.unwrap_or("png") {
            "png" => printcraft_engine::export::ImageFormat::Png,
            "jpeg" | "jpg" => printcraft_engine::export::ImageFormat::Jpeg { quality },
            "tiff" | "tif" => printcraft_engine::export::ImageFormat::Tiff,
            f => return Err(ToolError::InvalidArgs(format!("unknown format {f:?} (png, jpeg, tiff)"))),
        };
        let mut files = Vec::new();
        for p in pages {
            let img = ex.image(p, dpi, format).map_err(failed)?;
            let path = folder.join(format!("{stem}_page_{}.{}", p + 1, format.extension()));
            write_atomic(&path, &img)?;
            files.push(path.to_string_lossy().into_owned());
        }
        Ok(json!({ "count": files.len(), "files": files }))
    }

    fn doc_create(&mut self, a: &Args) -> Result<Value> {
        let (name, bytes) = match a.str("from")? {
            "blank" => {
                let n = a.opt_int("pages")?.unwrap_or(1).clamp(1, 10_000) as usize;
                let (w, h) = (a.opt_num("width")?.unwrap_or(612.0), a.opt_num("height")?.unwrap_or(792.0));
                ("Untitled.pdf".to_string(), self.session.create_blank(w, h, n).map_err(failed)?)
            }
            "images" => {
                let mut images = Vec::new();
                for p in a.strs("paths")? {
                    let path = self.resolve(p, false)?;
                    let bytes = std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
                    images.push((path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), bytes));
                }
                let name = if images.len() == 1 {
                    format!("{}.pdf", images[0].0.rsplit_once('.').map_or(images[0].0.as_str(), |(s, _)| s))
                } else {
                    "Images.pdf".into()
                };
                (name, self.session.create_from_images(&images).map_err(failed)?)
            }
            "text" => {
                let (title, text) = match (a.opt_str("text")?, a.opt_str("path")?) {
                    (Some(t), _) => ("Text".to_string(), t.to_string()),
                    (None, Some(p)) => {
                        let path = self.resolve(p, false)?;
                        let t = std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
                        (path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), String::from_utf8_lossy(&t).into_owned())
                    }
                    (None, None) => return Err(ToolError::InvalidArgs("text needs `text` or `path`".into())),
                };
                (format!("{title}.pdf"), self.session.create_from_text(&title, &text).map_err(failed)?)
            }
            other => return Err(ToolError::InvalidArgs(format!("unknown source {other:?}"))),
        };
        let name = a.opt_str("name")?.map(str::to_owned).unwrap_or(name);
        let id = self.session.open_new(name, bytes).map_err(failed)?;
        Ok(summary(self.session.get(id).ok_or_else(|| failed("the document vanished"))?))
    }

    fn page_set_box(&mut self, a: &Args) -> Result<Value> {
        use printcraft_engine::{BoxSpec, PageBox};
        let doc = self.doc(a)?;
        let n = doc.info.pages.len();
        let pages = match a.opt_ints("pages")? {
            Some(_) => self.pages(a, "pages")?,
            None => (0..n).collect(),
        };
        let which = match a.opt_str("box")? {
            None => PageBox::Crop,
            Some(b) => PageBox::from_name(b).ok_or_else(|| ToolError::InvalidArgs(format!("unknown box {b:?}")))?,
        };
        let nums = |key: &str| -> Result<Option<[f64; 4]>> {
            a.get(key)
                .map(|v| {
                    let v: Vec<f64> = v.as_array().map(|x| x.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
                    <[f64; 4]>::try_from(v).map_err(|_| ToolError::InvalidArgs(format!("{key} must be 4 numbers")))
                })
                .transpose()
        };
        let spec = match (nums("margins")?, nums("rect")?) {
            (Some(_), Some(_)) => return Err(ToolError::InvalidArgs("pass margins or rect, not both".into())),
            (Some(m), None) => BoxSpec::Margins(m),
            (None, Some(r)) => {
                // Top-left-origin points on the displayed page → user space (one page at a time).
                if pages.len() != 1 {
                    return Err(ToolError::InvalidArgs("rect applies to a single page; use margins for several".into()));
                }
                let p = &doc.info.pages[pages[0]];
                let (u0, u1) = (p.view_to_user(r[0] as f32, r[1] as f32), p.view_to_user(r[2] as f32, r[3] as f32));
                BoxSpec::Rect([u0[0].min(u1[0]) as f64, u0[1].min(u1[1]) as f64, u0[0].max(u1[0]) as f64, u0[1].max(u1[1]) as f64])
            }
            (None, None) => BoxSpec::Remove,
        };
        let mut out = self.apply(a, Edit::SetPageBox { pages, which, spec })?;
        out["page_sizes"] = json!(self.doc(a)?.info.pages.iter().map(|p| [p.width, p.height]).collect::<Vec<_>>());
        Ok(out)
    }

    fn doc_protect(&mut self, a: &Args) -> Result<Value> {
        use printcraft_engine::{Algorithm, Changes, Printing, Protection};
        let d = Protection::default();
        let p = Protection {
            open_password: a.opt_str("open_password")?.map(str::to_owned),
            permissions_password: a.opt_str("permissions_password")?.map(str::to_owned),
            printing: match a.opt_str("printing")? {
                None => d.printing,
                Some("none") => Printing::None,
                Some("low") => Printing::Low,
                Some("high") => Printing::High,
                Some(o) => return Err(ToolError::InvalidArgs(format!("unknown printing {o:?}"))),
            },
            changes: match a.opt_str("changes")? {
                None => d.changes,
                Some("none") => Changes::None,
                Some("pages") => Changes::Pages,
                Some("fill-sign") => Changes::FillSign,
                Some("comment-fill-sign") => Changes::CommentFillSign,
                Some("any-except-extract") => Changes::AnyExceptExtract,
                Some(o) => return Err(ToolError::InvalidArgs(format!("unknown changes {o:?}"))),
            },
            copy: a.opt_bool("copy")?.unwrap_or(d.copy),
            accessibility: a.opt_bool("accessibility")?.unwrap_or(d.accessibility),
            algorithm: match a.opt_str("compatibility")? {
                None | Some("aes-256") => Algorithm::Aes256,
                Some("aes-128") => Algorithm::Aes128,
                Some("rc4-128") => Algorithm::Rc4_128,
                Some("rc4-40") => Algorithm::Rc4_40,
                Some(o) => return Err(ToolError::InvalidArgs(format!("unknown compatibility {o:?}"))),
            },
            encrypt_metadata: a.opt_bool("encrypt_metadata")?.unwrap_or(d.encrypt_metadata),
        };
        let mut out = self.apply(a, Edit::Protect(p))?;
        out["security"] = security(self.doc(a)?);
        Ok(out)
    }

    fn insert_blank(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let at = self.position(a, "at")?;
        // Default to the size of the neighbouring page, as Acrobat does; Letter for empty files.
        let near = doc.info.pages.get(at.saturating_sub(1)).or(doc.info.pages.first());
        let (w, h) = near.map(|p| (f64::from(p.width), f64::from(p.height))).unwrap_or((612.0, 792.0));
        let width = a.opt_num("width")?.unwrap_or(w);
        let height = a.opt_num("height")?.unwrap_or(h);
        if !(1.0..=14400.0).contains(&width) || !(1.0..=14400.0).contains(&height) {
            return Err(ToolError::InvalidArgs("width and height must be between 1 and 14400 points".into()));
        }
        self.apply(a, Edit::InsertBlankPage { at, width, height })
    }

    fn insert_file(&mut self, a: &Args) -> Result<Value> {
        let at = self.position(a, "at")?;
        let path = self.resolve(a.str("path")?, false)?;
        let bytes = std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
        let pages = a.opt_ints("pages")?.map(|p| one_based(&p)).transpose()?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.apply(a, Edit::InsertPagesFrom { name, bytes: Arc::new(bytes), pages, at })
    }

    /// Write freshly created bytes to `out` and/or open them as a new document.
    fn deliver(&mut self, a: &Args, name: &str, bytes: Arc<Vec<u8>>) -> Result<Value> {
        let mut result = json!({ "bytes": bytes.len() });
        let out = a.opt_str("out")?;
        if let Some(out) = out {
            let path = self.resolve(out, true)?;
            write_atomic(&path, &bytes)?;
            result["path"] = json!(path.to_string_lossy());
        }
        if a.opt_bool("open")?.unwrap_or(out.is_none()) {
            let id = self.session.open_new(name, bytes).map_err(failed)?;
            result["document"] = summary(self.session.get(id).ok_or_else(|| failed("the document vanished"))?);
        }
        Ok(result)
    }

    fn page_extract(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let (id, name) = (doc.id, format!("{} (extract)", doc.name));
        let stem = Path::new(&doc.name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "page".into());
        let pages = self.pages(a, "pages")?;
        let mut out = if a.opt_bool("separate")?.unwrap_or(false) {
            // Each page as its own file.
            let dir = self.resolve(a.str("out_dir").map_err(|_| ToolError::InvalidArgs("separate files need out_dir".into()))?, true)?;
            std::fs::create_dir_all(&dir).map_err(|e| failed(format!("{}: {e}", dir.display())))?;
            let mut files = Vec::new();
            for &p in &pages {
                let bytes = self.session.extract(id, &[p]).map_err(failed)?;
                let path = dir.join(format!("{stem}-page{}.pdf", p + 1));
                write_atomic(&path, &bytes)?;
                files.push(path.to_string_lossy().into_owned());
            }
            json!({ "files": files })
        } else {
            let bytes = self.session.extract(id, &pages).map_err(failed)?;
            self.deliver(a, &name, bytes)?
        };
        if a.opt_bool("delete")?.unwrap_or(false) {
            let deleted = self.apply(a, Edit::DeletePages { pages })?;
            out["original"] = deleted;
        }
        Ok(out)
    }

    fn doc_combine(&mut self, a: &Args) -> Result<Value> {
        let paths = a.strs("paths")?;
        if paths.len() < 2 {
            return Err(ToolError::InvalidArgs("combine needs at least two files".into()));
        }
        let mut sources = Vec::new();
        for p in paths {
            let path = self.resolve(p, false)?;
            let bytes = std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
            let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            sources.push((name, Arc::new(bytes)));
        }
        let bytes = self.session.combine(&sources).map_err(failed)?;
        self.deliver(a, "Combined", bytes)
    }

    fn doc_split(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let id = doc.id;
        let stem = Path::new(&doc.name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "part".into());
        let bookmarks = a.opt_bool("bookmarks")?.unwrap_or(false);
        let max_mb = a.opt_num("max_mb")?;
        let chosen = [a.get("every").is_some(), a.get("before").is_some(), bookmarks, max_mb.is_some()].iter().filter(|x| **x).count();
        if chosen != 1 {
            return Err(ToolError::InvalidArgs(
                "pass exactly one of every (pages per file), before (page numbers), bookmarks: true, or max_mb".into(),
            ));
        }
        let mut titles: Vec<(usize, String)> = Vec::new();
        let parts = if let Some(mb) = max_mb {
            if !mb.is_finite() || mb <= 0.0 {
                return Err(ToolError::InvalidArgs("max_mb must be positive".into()));
            }
            self.session.split_by_size(id, (mb * 1_048_576.0) as usize).map_err(failed)?
        } else {
            let by = match (a.opt_int("every")?, a.opt_ints("before")?) {
                (Some(n), None) if n > 0 => printcraft_organize::SplitBy::PageCount(n as usize),
                (None, Some(b)) => printcraft_organize::SplitBy::Before(one_based(&b)?),
                _ if bookmarks => {
                    titles = self.session.bookmark_splits(id);
                    if titles.is_empty() {
                        return Err(failed("the document has no top-level bookmarks"));
                    }
                    printcraft_organize::SplitBy::Before(titles.iter().map(|t| t.0).collect())
                }
                _ => return Err(ToolError::InvalidArgs("every must be a positive page count".into())),
            };
            self.session.split(id, &by).map_err(failed)?
        };
        let dir = self.resolve(a.str("out_dir")?, true)?;
        std::fs::create_dir_all(&dir).map_err(|e| failed(format!("{}: {e}", dir.display())))?;
        let mut files = Vec::new();
        for (i, (first, last, bytes)) in parts.iter().enumerate() {
            let safe = |t: &str| t.chars().map(|c| if c.is_alphanumeric() || " -_.,()".contains(c) { c } else { '_' }).collect::<String>();
            let file = match titles.iter().find(|t| t.0 + 1 == *first) {
                Some((_, t)) => format!("{stem}-{}.pdf", safe(t)),
                None => format!("{stem}-part{}.pdf", i + 1),
            };
            let path = dir.join(file);
            write_atomic(&path, bytes)?;
            files.push(json!({ "path": path.to_string_lossy(), "first_page": first, "last_page": last }));
        }
        Ok(json!({ "files": files }))
    }

    // ---- pages and text ----------------------------------------------------------------------

    /// 1-based page list → validated 0-based indices.
    fn pages(&self, a: &Args, key: &str) -> Result<Vec<usize>> {
        let n = self.doc(a)?.info.pages.len();
        let pages = one_based(&a.ints(key)?)?;
        if pages.is_empty() {
            return Err(ToolError::InvalidArgs(format!("{key} must list at least one page")));
        }
        if let Some(p) = pages.iter().find(|p| **p >= n) {
            return Err(ToolError::InvalidArgs(format!("page {} is out of range: the document has {n} pages", p + 1)));
        }
        Ok(pages)
    }

    /// A 1-based insertion position (1 = before the first page, n+1 = after the last) → 0-based.
    fn position(&self, a: &Args, key: &str) -> Result<usize> {
        let n = self.doc(a)?.info.pages.len();
        let at = a.int(key)?;
        if at < 1 || at as usize > n + 1 {
            return Err(ToolError::InvalidArgs(format!("{key} must be between 1 and {} (the document has {n} pages)", n + 1)));
        }
        Ok(at as usize - 1)
    }

    fn renderer(&mut self, id: DocId) -> Result<&mut PageRenderer> {
        let doc = self.session.get(id).ok_or_else(|| failed("no such document"))?;
        let stale = self.renderers.get(&id).is_none_or(|(b, _)| !Arc::ptr_eq(b, &doc.bytes));
        if stale {
            let config = RenderConfig { password: doc.password.as_deref().map(Arc::from), ..Default::default() };
            self.renderers.insert(id, (doc.bytes.clone(), PageRenderer::new(doc.bytes.clone(), config)));
        }
        Ok(&mut self.renderers.get_mut(&id).expect("inserted above").1)
    }

    fn page_render(&mut self, a: &Args) -> Result<Content> {
        let id = self.doc(a)?.id;
        let page = self.page(a)?;
        let dpi = a.opt_num("dpi")?.unwrap_or(DEFAULT_DPI);
        if !(1.0..=MAX_DPI).contains(&dpi) {
            return Err(ToolError::InvalidArgs(format!("dpi must be between 1 and {MAX_DPI}")));
        }
        let out = self.renderer(id)?.render(RenderRequest { page, kind: RequestKind::Pixels, tile: None, scale: (dpi / 72.0) as f32, tag: 0 });
        if let Some(e) = out.error {
            return Err(failed(format!("page {} could not be rendered: {e}", page + 1)));
        }
        let data = encode_png(out.width, out.height, &out.rgba)?;
        Ok(Content::Png { data, width: out.width, height: out.height })
    }

    fn page(&self, a: &Args) -> Result<usize> {
        let n = self.doc(a)?.info.pages.len();
        let p = a.int("page")?;
        if p < 1 || p as usize > n {
            return Err(ToolError::InvalidArgs(format!("page {p} is out of range: the document has {n} pages")));
        }
        Ok(p as usize - 1)
    }

    /// The text of `pages` (0-based), from the cache or extracted in parallel.
    fn page_texts(&mut self, id: DocId, pages: &[usize]) -> Result<Vec<Arc<PageText>>> {
        let doc = self.session.get(id).ok_or_else(|| failed("no such document"))?;
        let (bytes, n) = (doc.bytes.clone(), doc.info.pages.len());
        let password: Option<Arc<str>> = doc.password.as_deref().map(Arc::from);
        let entry = self.texts.entry(id).or_insert_with(|| (bytes.clone(), Vec::new()));
        if !Arc::ptr_eq(&entry.0, &bytes) || entry.1.len() != n {
            *entry = (bytes.clone(), vec![None; n]);
        }
        let mut missing: Vec<usize> = pages.iter().copied().filter(|p| entry.1[*p].is_none()).collect();
        missing.sort_unstable();
        missing.dedup();
        for (p, text) in extract_parallel(&bytes, password, &missing) {
            entry.1[p] = Some(Arc::new(text.map_err(|e| failed(format!("page {}: {e}", p + 1)))?));
        }
        Ok(pages.iter().map(|p| entry.1[*p].clone().expect("extracted above")).collect())
    }

    fn text_extract(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let id = doc.id;
        let pages = match a.opt_ints("pages")? {
            Some(_) => self.pages(a, "pages")?,
            None => (0..doc.info.pages.len()).collect(),
        };
        let texts = self.page_texts(id, &pages)?;
        let out: Vec<Value> = pages.iter().zip(texts).map(|(p, t)| json!({ "page": p + 1, "text": t.plain_text() })).collect();
        Ok(json!({ "pages": out }))
    }

    fn text_find(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let (id, n) = (doc.id, doc.info.pages.len());
        let query = a.str("query")?;
        let limit = a.opt_int("limit")?.unwrap_or(500).max(1) as usize;
        let texts = self.page_texts(id, &(0..n).collect::<Vec<_>>())?;
        let mut matches = Vec::new();
        'pages: for (p, text) in texts.iter().enumerate() {
            for r in text.find(query) {
                if matches.len() == limit {
                    break 'pages;
                }
                matches.push(json!({ "page": p + 1, "text": text.text_of(r.clone()), "rects": text.line_rects(r) }));
            }
        }
        Ok(json!({ "query": query, "count": matches.len(), "matches": matches }))
    }

    fn command_list(&self, a: &Args) -> Result<Value> {
        let active = match a.opt_int("doc")? {
            Some(_) => Some(self.doc(a)?.id),
            None => None,
        };
        let list: Vec<Value> = commands::COMMANDS
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "label": commands::current_label(c, &self.session, active),
                    "menu": c.menu,
                    "shortcut": c.shortcut.map(|s| s.label(cfg!(target_os = "macos"))),
                    "enabled": commands::is_enabled(c, &self.session, active),
                    "tool": tools::tool_for_command(c.id),
                })
            })
            .collect();
        Ok(json!({ "commands": list }))
    }

    // ---- paths -------------------------------------------------------------------------------

    /// Resolve a user-supplied path, enforcing the root (if any). `for_write` allows a file that
    /// does not exist yet (its nearest existing ancestor must be inside the root).
    fn resolve(&self, path: &str, for_write: bool) -> Result<PathBuf> {
        let p = Path::new(path);
        let joined = match &self.root {
            Some(root) if p.is_relative() => root.join(p),
            _ => p.to_path_buf(),
        };
        let Some(root) = &self.root else { return Ok(joined) };
        let real = if for_write {
            // Canonicalize the deepest existing ancestor, then re-append the rest.
            let mut existing = joined.as_path();
            let mut rest = Vec::new();
            while !existing.exists() {
                rest.push(existing.file_name().ok_or_else(|| failed(format!("{path}: invalid path")))?);
                existing = existing.parent().ok_or_else(|| failed(format!("{path}: invalid path")))?;
            }
            if rest.iter().any(|c| *c == "..") {
                return Err(failed(format!("{path}: '..' is not allowed here")));
            }
            let mut real = existing.canonicalize().map_err(|e| failed(format!("{path}: {e}")))?;
            real.extend(rest.iter().rev());
            real
        } else {
            joined.canonicalize().map_err(|e| failed(format!("{path}: {e}")))?
        };
        if !real.starts_with(root) {
            return Err(failed(format!("{path} is outside the allowed directory {}", root.display())));
        }
        Ok(real)
    }
}

// ---- JSON helpers ----------------------------------------------------------------------------

/// Typed access to validated arguments.
struct Args<'a>(&'a Value);

impl Args<'_> {
    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }
    fn missing(key: &str) -> ToolError {
        ToolError::InvalidArgs(format!("missing argument {key}"))
    }
    fn wrong(key: &str, what: &str) -> ToolError {
        ToolError::InvalidArgs(format!("{key} must be {what}"))
    }
    fn str(&self, key: &str) -> Result<&str> {
        self.opt_str(key)?.ok_or_else(|| Self::missing(key))
    }
    fn opt_str(&self, key: &str) -> Result<Option<&str>> {
        self.get(key).map(|v| v.as_str().ok_or_else(|| Self::wrong(key, "a string"))).transpose()
    }
    fn int(&self, key: &str) -> Result<i64> {
        self.opt_int(key)?.ok_or_else(|| Self::missing(key))
    }
    fn opt_int(&self, key: &str) -> Result<Option<i64>> {
        self.get(key).map(|v| v.as_i64().ok_or_else(|| Self::wrong(key, "an integer"))).transpose()
    }
    fn opt_num(&self, key: &str) -> Result<Option<f64>> {
        self.get(key).map(|v| v.as_f64().filter(|f| f.is_finite()).ok_or_else(|| Self::wrong(key, "a number"))).transpose()
    }
    fn opt_bool(&self, key: &str) -> Result<Option<bool>> {
        self.get(key).map(|v| v.as_bool().ok_or_else(|| Self::wrong(key, "true or false"))).transpose()
    }
    fn ints(&self, key: &str) -> Result<Vec<i64>> {
        self.opt_ints(key)?.ok_or_else(|| Self::missing(key))
    }
    fn opt_ints(&self, key: &str) -> Result<Option<Vec<i64>>> {
        let Some(v) = self.get(key) else { return Ok(None) };
        let arr = v.as_array().ok_or_else(|| Self::wrong(key, "an array of integers"))?;
        arr.iter().map(|x| x.as_i64().ok_or_else(|| Self::wrong(key, "an array of integers"))).collect::<Result<Vec<_>>>().map(Some)
    }
    /// A 1-based bookmark path → 0-based indices.
    fn path(&self, key: &str) -> Result<Vec<usize>> {
        let p = self.opt_path(key)?.ok_or_else(|| Self::missing(key))?;
        if p.is_empty() {
            return Err(ToolError::InvalidArgs(format!("{key} must not be empty")));
        }
        Ok(p)
    }
    fn opt_path(&self, key: &str) -> Result<Option<Vec<usize>>> {
        let Some(v) = self.opt_ints(key)? else { return Ok(None) };
        v.iter().map(|i| if *i >= 1 { Ok(*i as usize - 1) } else { Err(Self::wrong(key, "1-based positions")) }).collect::<Result<Vec<_>>>().map(Some)
    }
    fn strs(&self, key: &str) -> Result<Vec<&str>> {
        let v = self.get(key).ok_or_else(|| Self::missing(key))?;
        let arr = v.as_array().ok_or_else(|| Self::wrong(key, "an array of strings"))?;
        arr.iter().map(|x| x.as_str().ok_or_else(|| Self::wrong(key, "an array of strings"))).collect()
    }
}

/// The bookmark tree as JSON, with 1-based paths and pages.
fn bookmark_tree(items: &[printcraft_render::OutlineItem], parent: &[usize]) -> Vec<Value> {
    items
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let mut path = parent.to_vec();
            path.push(i + 1);
            json!({ "path": path, "title": o.title, "page": o.page.map(|p| p + 1), "open": o.open, "children": bookmark_tree(&o.children, &path) })
        })
        .collect()
}

fn one_based(pages: &[i64]) -> Result<Vec<usize>> {
    pages
        .iter()
        .map(|p| if *p >= 1 { Ok(*p as usize - 1) } else { Err(ToolError::InvalidArgs(format!("page numbers start at 1 (got {p})"))) })
        .collect()
}

/// The document's security as the next save writes it (never includes passwords).
fn security(d: &Document) -> Value {
    match d.security_summary() {
        None => json!({ "protected": false }),
        Some(s) => {
            let p = s.permissions;
            json!({
                "protected": true,
                "method": s.method,
                "pending": s.pending,
                "printing": if !p.print() { "none" } else if p.print_high_quality() { "high" } else { "low" },
                "modify": p.modify(), "assemble": p.assemble(), "copy": p.copy(), "annotate": p.annotate(),
                "fill_forms": p.fill_forms(), "accessibility": p.extract_for_accessibility(),
            })
        }
    }
}

/// What every tool that changes a document returns.
fn summary(d: &Document) -> Value {
    json!({
        "doc": d.id.0,
        "name": d.name,
        "path": d.path,
        "pages": d.info.pages.len(),
        "dirty": d.dirty,
        "editable": d.read_only_reason.is_none(),
        "read_only_reason": d.read_only_reason,
        "encrypted": d.info.encrypted,
        "undo": d.can_undo(),
        "redo": d.can_redo(),
    })
}

fn info(d: &Document) -> Value {
    let i = &d.info;
    let page1 = |p: usize| p + 1;
    let mut security = security(d);
    if let Some(s) = d.security_summary() {
        security["opened_as_owner"] = json!(s.owner);
    }
    json!({
        "document": summary(d),
        "pdf_version": i.pdf_version,
        "file_size": i.file_size,
        "title": i.title, "author": i.author, "subject": i.subject, "keywords": i.keywords,
        "creator": i.creator, "producer": i.producer,
        "tagged": i.tagged,
        "has_javascript": i.has_javascript,
        "security": security,
        "pages": i.pages.iter().enumerate().map(|(n, p)| json!({
            "page": n + 1, "label": p.label, "width": p.width, "height": p.height, "rotation": p.rotation,
        })).collect::<Vec<_>>(),
        "outline": outline(&i.outline),
        "annotations": i.annotations.iter().map(|a| json!({
            "page": page1(a.page), "type": a.subtype, "author": a.author, "contents": a.contents,
            "modified": a.modified, "name": a.name, "in_reply_to": a.in_reply_to, "rect": a.rect,
        })).collect::<Vec<_>>(),
        "fields": i.fields.iter().map(|f| json!({
            "name": f.name, "kind": format!("{:?}", f.kind), "value": f.value, "page": f.page.map(page1),
            "tooltip": f.tooltip, "has_actions": f.has_actions,
        })).collect::<Vec<_>>(),
        "links": i.links.iter().map(|l| json!({
            "page": page1(l.page), "rect": l.rect,
            "target": match &l.target {
                printcraft_render::LinkTarget::Page(p) => json!({ "page": page1(*p) }),
                printcraft_render::LinkTarget::Uri(u) => json!({ "uri": u }),
                printcraft_render::LinkTarget::Other(o) => json!({ "other": o }),
            },
        })).collect::<Vec<_>>(),
        "layers": i.layers.iter().map(|l| json!({ "name": l.name, "visible": l.visible })).collect::<Vec<_>>(),
        "attachments": i.attachments.iter().map(|a| json!({ "name": a.name, "description": a.description, "size": a.size })).collect::<Vec<_>>(),
        "fonts": i.fonts.iter().map(|f| json!({
            "name": f.name, "kind": f.kind, "embedded": f.embedded, "subset": f.subset, "encoding": f.encoding,
        })).collect::<Vec<_>>(),
        "warnings": i.warnings,
        "repairs": d.repair_log(),
    })
}

fn outline(items: &[printcraft_render::OutlineItem]) -> Value {
    Value::Array(items.iter().map(|o| json!({ "title": o.title, "page": o.page.map(|p| p + 1), "children": outline(&o.children) })).collect())
}

fn encode_png(width: u32, height: u32, premultiplied: &[u8]) -> Result<Vec<u8>> {
    let mut rgba = premultiplied.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(failed)?;
    w.write_image_data(&rgba).map_err(failed)?;
    w.finish().map_err(failed)?;
    Ok(out)
}

/// Extract the text of `pages` (0-based) with one renderer per worker thread.
fn extract_parallel(bytes: &Arc<Vec<u8>>, password: Option<Arc<str>>, pages: &[usize]) -> Vec<(usize, std::result::Result<PageText, String>)> {
    let extract = |r: &mut PageRenderer, p: usize| {
        let out = r.render(RenderRequest { page: p, kind: RequestKind::Text, tile: None, scale: 1.0, tag: 0 });
        match out.error {
            Some(e) => Err(e),
            None => Ok(out.text.map(|t| (*t).clone()).unwrap_or_default()),
        }
    };
    let config = RenderConfig { password, ..Default::default() };
    let workers = if cfg!(target_arch = "wasm32") { 1 } else { std::thread::available_parallelism().map_or(1, |n| n.get()).min(8) };
    // Small jobs aren't worth a second parse of the document.
    if workers == 1 || pages.len() < 8 {
        let mut r = PageRenderer::new(bytes.clone(), config);
        return pages.iter().map(|p| (*p, extract(&mut r, *p))).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut out: Vec<(usize, std::result::Result<PageText, String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut r = PageRenderer::new(bytes.clone(), config.clone());
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(p) = pages.get(i) else { break };
                        done.push((*p, extract(&mut r, *p)));
                    }
                    done
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    out.sort_by_key(|(p, _)| *p);
    out
}

/// Write via a temporary file in the same directory, then rename over the target.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().ok_or_else(|| failed(format!("{}: not a file path", path.display())))?;
    std::fs::create_dir_all(dir).map_err(|e| failed(format!("{}: {e}", dir.display())))?;
    let tmp = dir.join(format!(".{}.printcraft-tmp", name.to_string_lossy()));
    std::fs::write(&tmp, bytes).map_err(|e| failed(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        failed(format!("{}: {e}", path.display()))
    })
}
