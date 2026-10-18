//! The tool table: names, descriptions and JSON Schemas for every automation tool.
//!
//! Tool names use `[a-z_]` only (MCP clients reject dots). `command` links a tool to the
//! registry id it automates, so `command_list` can tell an agent which tool runs a menu command.

use serde_json::{Value, json};

use crate::ToolError;

#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// JSON Schema (draft 2020-12 subset) for the arguments object.
    pub input_schema: Value,
    /// Does not change any document or file.
    pub read_only: bool,
    /// May remove content or overwrite files.
    pub destructive: bool,
    /// The registry command this tool automates, if any.
    pub command: Option<&'static str>,
}

/// A list of internet access rules (`trust_urls_set`).
fn url_rules() -> Value {
    json!({ "type": "array", "maxItems": 1000, "items": { "type": "object", "properties": {
        "host": { "type": "string" },
        "action": { "type": "string", "enum": ["allow", "block", "ask"] },
        "subdomains": { "type": "boolean", "description": "Also match subdomains (default true; never for an IP literal)." },
    }, "required": ["host", "action"] } })
}

/// The `overprint_preview` option of the page render tools.
fn overprint_preview() -> Value {
    json!({ "type": "string", "enum": ["never", "always", "pdfx", "automatic"], "description": "Simulate overprint: never, always, pdfx (only PDF/X files) or automatic (pages with overprinting objects). Default: the print_overprint_preview preference (pdfx)." })
}

fn doc() -> Value {
    json!({ "type": "integer", "minimum": 1, "description": "Document id from doc_open or doc_list." })
}

fn timestamp_server() -> Value {
    schema(
        json!({
            "name":{"type":"string","minLength":1,"maxLength":200},
            "url":{"type":"string","maxLength":4096,"description":"HTTPS endpoint; HTTP only for literal loopback IPs. No redirects or URL credentials."},
            "policy":{"type":["string","null"],"description":"Optional requested policy OID; reply must match."},
            "timeout_seconds":{"type":"integer","minimum":1,"maximum":30},
            "require_trusted_authority":{"type":"boolean","description":"Require the TSA chain to reach an explicitly trusted certificate. False permits an untrusted token, never confers trust."}
        }),
        &["name", "url", "timeout_seconds", "require_trusted_authority"],
    )
}

fn verification_time() -> Value {
    json!({"type":"string","description":"best (default: trusted timestamp, else claimed signing time), claimed, now, or complete UTC YYYYMMDDHHmmSSZ. Chosen and claimed times are never proof of time."})
}

fn pages(what: &str) -> Value {
    json!({ "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "description": format!("1-based page numbers {what}.") })
}

fn path(what: &str) -> Value {
    json!({ "type": "array", "items": { "type": "integer", "minimum": 1 }, "description": format!("{what}: 1-based positions from the top level, e.g. [2, 1] = the first child of the second bookmark.") })
}

/// The Compare Files options shared by `doc_compare` and `compare_files`, added to `props`.
fn compare_props(mut props: Value) -> Value {
    let range = |which: &str| json!({ "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 2, "maxItems": 2, "description": format!("[first, last] 1-based pages of the {which} file to compare (default: all).") });
    let extra = json!({
        "limit": { "type": "integer", "minimum": 1, "description": "List at most this many text changes (default 500)." },
        "old_pages": range("old"),
        "new_pages": range("new"),
        "dpi": { "type": "number", "minimum": 18, "maximum": 150, "description": "Resolution of the visual compare (default 72)." },
        "tolerance": { "type": "integer", "minimum": 0, "maximum": 255, "description": "A pixel differs when a colour channel differs by more than this (default 24)." },
        "threshold": { "type": "number", "minimum": 0, "maximum": 1, "description": "A page looks different when more than this share of its pixels differ (default 0)." },
    });
    if let (Some(p), Some(e)) = (props.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            p.insert(k.clone(), v.clone());
        }
    }
    props
}

fn path_arg() -> Value {
    json!({ "type": "string", "description": "A file path (relative to --root when set)." })
}

fn point() -> Value {
    json!({ "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "[x, y] in points from the top-left of the displayed page." })
}

fn measure_points() -> Value {
    json!({"type":"array","items":point(),"minItems":1,"maxItems":2048})
}
fn measure_schema() -> Value {
    schema(
        json!({"doc":doc(),"page":{"type":"integer","minimum":1},"points":measure_points(),"label":{"type":"string","maxLength":512},"author":{"type":"string","maxLength":512}}),
        &["doc", "page", "points"],
    )
}

fn color() -> Value {
    json!({ "type": "string", "description": "#RRGGBB or a name: yellow, red, orange, green, blue, purple, pink, black, gray, white." })
}

/// Properties that pick one comment: its `id` (from comment_list), or `page` + `index`.
fn comment_ref(mut extra: Value) -> Value {
    if let Some(props) = extra.as_object_mut() {
        props.insert("doc".into(), doc());
        props.insert("id".into(), json!({ "type": "string", "description": "The comment's id (from comment_list)." }));
        props.insert("page".into(), json!({ "type": "integer", "minimum": 1, "description": "With `index`, instead of `id`." }));
        props.insert(
            "index".into(),
            json!({ "type": "integer", "minimum": 1, "description": "1-based position on the page, as comment_list reports." }),
        );
    }
    extra
}

/// The owner of PDF 2.0 associated files.
fn af_target() -> Value {
    json!({
        "description": "\"document\" (default), {page: n}, {page: n, annotation: i} (1-based position in the page's annotations, as comment_list's index), or {object: n} (a structure element, XObject or annotation by object number).",
        "oneOf": [
            { "type": "string", "enum": ["document"] },
            { "type": "object", "properties": { "page": { "type": "integer", "minimum": 1 }, "annotation": { "type": "integer", "minimum": 1 } }, "required": ["page"], "additionalProperties": false },
            { "type": "object", "properties": { "object": { "type": "integer", "minimum": 1 } }, "required": ["object"], "additionalProperties": false },
        ],
    })
}

fn schema(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

struct T {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    read_only: bool,
    destructive: bool,
    command: Option<&'static str>,
}

const fn t(name: &'static str, title: &'static str, description: &'static str) -> T {
    T { name, title, description, read_only: false, destructive: false, command: None }
}

impl T {
    const fn ro(mut self) -> Self {
        self.read_only = true;
        self
    }
    const fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }
    const fn cmd(mut self, id: &'static str) -> Self {
        self.command = Some(id);
        self
    }
    fn with(self, input_schema: Value) -> ToolDef {
        ToolDef {
            name: self.name,
            title: self.title,
            description: self.description,
            input_schema,
            read_only: self.read_only,
            destructive: self.destructive,
            command: self.command,
        }
    }
}

/// Every tool, in a stable order.
pub fn tools() -> Vec<ToolDef> {
    let save_out = json!({ "type": "string", "description": "File to write. Omit to open the result as a new unsaved document instead." });
    let open = json!({ "type": "boolean", "description": "Also open the result as a new document (default: only when out is omitted)." });
    vec![
        t("doc_open", "Open a PDF", "Open a PDF file and return its document id, page count and whether it can be edited.")
            .cmd("file.open")
            .with(schema(
                json!({
                    "path": { "type": "string" },
                    "password": { "type": "string", "description": "User or owner password for encrypted files." },
                    "certificate": { "type": "string", "description": "A digital ID file (.p12/.pfx) for a certificate-encrypted document: its certificate must be one of the recipients, whose permissions then apply. The private key is not kept." },
                    "certificate_password": { "type": "string", "description": "The digital ID file password (default empty). Never echoed." },
                }),
                &["path"],
            )),
        t("doc_list", "List open documents", "List the open documents with their ids, page counts and unsaved state.").ro().with(schema(json!({}), &[])),
        t("doc_info", "Inspect a document", "Metadata, page sizes and labels, bookmarks, annotations, form fields, links, layers, attachments, fonts, security and repair notes.")
            .ro()
            .cmd("file.properties")
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("doc_close", "Close a document", "Close a document. Fails if it has unsaved changes unless discard_changes is true.")
            .cmd("file.close")
            .with(schema(json!({ "doc": doc(), "discard_changes": { "type": "boolean" } }), &["doc"])),
        t(
            "doc_save",
            "Save a document",
            "Save to its own file (an incremental update, which keeps signatures valid) or to a new path (a full rewrite). The write is atomic. linearize writes a linearized file (Fast Web View, ISO 32000 Annex F): a full rewrite that puts page 1 first with hint tables, so viewers show it before the rest downloads; it uses classic cross-reference tables without object streams, keeps encryption, and is refused for signed documents. A later incremental save ends Fast Web View. flatten_fill_sign bakes Fill & Sign text, marks and signatures into the page first and removes those annotations; other comments and form fields stay. Omit it, or pass false, to leave them editable.",
        )
        .destructive()
        .cmd("file.save")
        .with(schema(
            json!({
                "doc": doc(),
                "path": { "type": "string", "description": "Save as this file. Omit to save in place." },
                "full": { "type": "boolean", "description": "Force a full rewrite (or, with false, an incremental update)." },
                "linearize": { "type": "boolean", "description": "Fast Web View: write a linearized full rewrite. Can't be combined with full: false. Default false." },
                "flatten_fill_sign": { "type": "boolean", "description": "Bake Fill & Sign marks into the page before writing. Default false." }
            }),
            &["doc"],
        )),
        t("doc_set_info", "Set document metadata", "Set a document information entry such as Title, Author, Subject or Keywords. Undoable.")
            .with(schema(json!({ "doc": doc(), "key": { "type": "string" }, "value": { "type": "string" } }), &["doc", "key", "value"])),
        t("page_render", "Render a page", "Render one page to a PNG image (default 96 dpi, at most 600). overprint_preview (never, always, pdfx, automatic; default the print_overprint_preview preference, Only For PDF/X Files) simulates how objects painted with overprint on combine with the inks below them on press instead of knocking them out (see print_overprint_preview).")
            .ro()
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "dpi": { "type": "number", "minimum": 1, "maximum": 600 }, "overprint_preview": overprint_preview() }), &["doc", "page"])),
        t("text_extract", "Extract text", "Extract the text of some or all pages, in reading order. Refused when the document does not allow copying text (open it with the permissions password).")
            .ro()
            .with(schema(json!({ "doc": doc(), "pages": pages("to extract (default: all)"), "include_glyphs": {"type":"boolean","description":"Include one-based glyph indices, Unicode mappings and view-space rectangles for text_selection endpoints. At most 200000 glyphs; default false. Copy permission applies."} }), &["doc"])),
        t("text_selection", "Copy selected text", "Copy a reading-order text range across pages, a detected paragraph, or the whole document. Page and glyph endpoints are one-based and inclusive; glyphs may contain a whole ligature. Reverse ranges are normalized. Line breaks are preserved and pages separated by two newlines, including blank pages. Copy permission is enforced and output is limited to 32 MiB. Read-only; no history changes.")
            .ro()
            .with(schema(json!({
                "doc":doc(), "mode":{"type":"string","enum":["range","paragraph","all"],"default":"range"},
                "start_page":{"type":"integer","minimum":1}, "start_glyph":{"type":"integer","minimum":1},
                "end_page":{"type":"integer","minimum":1}, "end_glyph":{"type":"integer","minimum":1}
            }), &["doc"])),
        t("text_find", "Find text", "Find a phrase (case-insensitive, whitespace-normalised) and return each match with its page and line rectangles in points (origin top-left). Searching is allowed on documents that forbid copying, but then matches are locations only: text is null and text_withheld is set (only the query you supplied is echoed).")
            .ro()
            .cmd("edit.find")
            .with(schema(json!({ "doc": doc(), "query": { "type": "string", "minLength": 1 }, "limit": { "type": "integer", "minimum": 1, "description": "Maximum matches (default 500)." } }), &["doc", "query"])),
        t("search_folder", "Search all PDFs in a folder", "Advanced Search ▸ All PDF documents in a folder: search every .pdf in folder (with recursive, its subfolders too) for a word or phrase and return the matches grouped by file, each with its page (1-based), the matched text with up to 36 characters of context before and after, and where it was found (found_in): text (page text, with rects as text_find gives them), bookmark (title; bookmark is its 1-based path), comment (its text; subtype, author), attachment (its name or description) or attachment_text (page text of an attached PDF). Page text is matched exactly as text_find matches it: case-insensitive and whitespace-normalised unless case_sensitive / whole_words. Files that can't be searched (not a PDF, damaged, password-protected without the right password, over 512 MB) are listed in skipped with the reason and the search goes on; a file is searched for at most 60 s (incomplete says so). Links to folders are never followed, links to files only inside the allowed directory; folders are followed at most 32 levels deep. truncated and truncated_by (max_files, max_results, max_entries) say when the walk or the results were cut short. Read-only: no document is opened in the session.")
            .ro()
            .with(schema(
                json!({
                    "folder": { "type": "string", "description": "The folder to search (inside the root when one is set)." },
                    "query": { "type": "string", "minLength": 1, "maxLength": 1000, "description": "The word or phrase." },
                    "recursive": { "type": "boolean", "description": "Also search subfolders (default false)." },
                    "whole_words": { "type": "boolean", "description": "Whole words only (default false)." },
                    "case_sensitive": { "type": "boolean", "description": "Match case (default false)." },
                    "include_bookmarks": { "type": "boolean", "description": "Also search bookmark titles (default false)." },
                    "include_comments": { "type": "boolean", "description": "Also search comment text (default false)." },
                    "include_attachments": { "type": "boolean", "description": "Also search attachment names and descriptions, and the pages of attached PDFs (default false)." },
                    "password": { "type": "string", "description": "Tried on every password-protected file; files it doesn't open are skipped. Never echoed back." },
                    "max_files": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "Search at most this many PDFs (default 1000)." },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": 100000, "description": "Return at most this many matches in all (default 1000)." },
                }),
                &["folder", "query"],
            )),
        t("page_rotate", "Rotate pages", "Rotate pages by a multiple of 90 degrees (positive is clockwise): the listed pages (default all), filtered like Acrobat's Rotate Pages by subset (all, even, odd page numbers) and orientation (all, landscape, portrait). Undoable.")
            .cmd("page.rotate")
            .with(schema(
                json!({
                    "doc": doc(), "pages": pages("to rotate (default: all)"), "degrees": { "type": "integer" },
                    "subset": { "type": "string", "enum": ["all", "even", "odd"] },
                    "orientation": { "type": "string", "enum": ["all", "landscape", "portrait"] },
                }),
                &["doc", "degrees"],
            )),
        t("page_delete", "Delete pages", "Delete pages. Undoable until saved.")
            .destructive()
            .cmd("page.delete")
            .with(schema(json!({ "doc": doc(), "pages": pages("to delete") }), &["doc", "pages"])),
        t("page_move", "Move pages", "Move pages so the first of them lands at position `to` (1-based, counted before the move). Undoable.").with(schema(
            json!({ "doc": doc(), "pages": pages("to move"), "to": { "type": "integer", "minimum": 1 } }),
            &["doc", "pages", "to"],
        )),
        t("page_insert_blank", "Insert a blank page", "Insert a blank page so it becomes page `at`. Size defaults to the neighbouring page. Undoable.")
            .cmd("page.insert_blank")
            .with(schema(
                json!({ "doc": doc(), "at": { "type": "integer", "minimum": 1 }, "width": { "type": "number", "description": "Points." }, "height": { "type": "number", "description": "Points." } }),
                &["doc", "at"],
            )),
        t("page_insert_file", "Insert pages from a file", "Insert pages of another PDF so the first becomes page `at`. Undoable. The file's tags (structure tree) are merged into the document's (merge_tags, default true): the inserted elements go under the one Document element by page, with role maps, class maps, IDs and PDF 2.0 namespaces merged and the parent tree rebuilt. The result's tags entry reports it: tagged and untagged parts, role-map conflicts (the document's mapping is kept), whether the result stays marked as tagged (only when both sides were) or is partially tagged, and the element count.")
            .cmd("page.insert")
            .with(schema(
                json!({ "doc": doc(), "path": { "type": "string" }, "at": { "type": "integer", "minimum": 1 }, "pages": pages("of the source file (default: all)"), "merge_tags": { "type": "boolean", "description": "Merge the file's structure tree (default true); false inserts the pages untagged." } }),
                &["doc", "path", "at"],
            )),
        t("page_extract", "Extract pages", "Copy pages into a new PDF (links, bookmarks, fields, layers and the tags (structure elements) that belong to them come along). separate: true writes each page as its own file into out_dir; delete: true removes the pages from this document afterwards (undoable).")
            .cmd("page.extract")
            .with(schema(
                json!({ "doc": doc(), "pages": pages("to extract"), "out": save_out.clone(), "open": open.clone(), "separate": { "type": "boolean" }, "out_dir": { "type": "string" }, "delete": { "type": "boolean" } }),
                &["doc", "pages"],
            )),
        t("doc_combine", "Combine files", "Combine PDFs, in order, into one (bookmarks are kept under one entry per file). pages optionally chooses each file's pages, in step with paths: a range such as \"1-3, 6\" or null for all pages. passwords, also in step with paths, opens encrypted files (the open password, or the permissions password where a file's security doesn't allow copying pages; null for none). The result is not encrypted. Passwords are never echoed back. Form fields whose names collide across files are renamed (name#2) so each copy keeps its own value; field_conflicts: \"merge\" instead joins compatible fields (same type, no sub-fields, not signatures) into one shared-value field. The result's fields entry reports what was renamed or merged. The files' tags (structure trees) are merged (merge_tags, default true): one Document element holding each file's elements in page order, role maps (a conflicting mapping keeps the first file's and is reported), class maps, IDs (made unique) and PDF 2.0 namespaces merged, the parent tree rebuilt; the result is marked tagged only when every file was, and the tags entry reports elements, conflicts, renamed IDs and classes, and partial tagging.")
            .cmd("page.combine")
            .with(schema(
                json!({
                    "paths": { "type": "array", "items": { "type": "string" }, "minItems": 2 },
                    "pages": { "type": "array", "items": { "type": ["string", "null"] } },
                    "passwords": { "type": "array", "items": { "type": ["string", "null"] } },
                    "field_conflicts": { "type": "string", "enum": ["rename", "merge"] },
                    "merge_tags": { "type": "boolean", "description": "Merge the files' structure trees (default true); false drops the tags." },
                    "out": save_out,
                    "open": open,
                }),
                &["paths"],
            )),
        t("doc_bates_files", "Bates numbering across files", "Add Bates numbers to a list of PDFs, numbered on from one file to the next (the first page of each file follows the last page of the one before). Put a <<Bates Number#digits#start#prefix#suffix>> token in one of the header and footer boxes (as in doc_header_footer, which takes the same text, font, colour and margin options); start overrides the token's start number. Each numbered copy is written into out_dir as <name><suffix>.pdf (suffix default \"_bates\"; existing files are never overwritten) and the originals are left as they are. replace: true swaps out Bates numbers a file already has. passwords, in step with paths, opens encrypted files. A file that fails stops the run so the numbering stays unbroken; the result lists each file's output and first and last number, and next, the number the following batch would start at.")
            .cmd("edit.bates")
            .with(schema(
                json!({
                    "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": pdfcraft_engine::MAX_CREATE_FILES },
                    "out_dir": { "type": "string" },
                    "suffix": { "type": "string" },
                    "start": { "type": "integer", "minimum": 0 },
                    "header_left": { "type": "string" }, "header_center": { "type": "string" }, "header_right": { "type": "string" },
                    "footer_left": { "type": "string" }, "footer_center": { "type": "string" }, "footer_right": { "type": "string" },
                    "font_size": { "type": "number", "exclusiveMinimum": 0 },
                    "color": { "type": "string" },
                    "margins": { "type": "array", "items": { "type": "number", "minimum": 0 }, "minItems": 4, "maxItems": 4 },
                    "start_number": { "type": "integer", "minimum": 1 },
                    "replace": { "type": "boolean" },
                    "passwords": { "type": "array", "items": { "type": ["string", "null"] } },
                }),
                &["paths", "out_dir"],
            )),
        t("optimize_flatten_transparency", "Flatten transparency", "Give pages that use transparency (soft masks, constant alpha below 1, blend modes other than Normal, transparency groups, image soft masks: what PDF/X-1a forbids) an opaque equivalent. Transparent resources are made opaque and the areas where that changes the look are replaced by opaque RGB images rendered from the original at the preset's resolution (low 150, medium 300, high 600 ppi; resolution overrides it); everything outside them stays vector. raster_vector_balance (0-100, default 75): 0 turns each such page into one image; otherwise a page becomes one image when the areas cover more than that percentage of it. Pages without transparency are left alone (flattened: 0 changes nothing); results lists each flattened page with its action (group_removed, cleaned, patched, rasterized), regions, images, raster_fraction and resolution. Annotations are not flattened. One undo step.")
            .destructive()
            .with(schema(
                json!({
                    "doc": doc(),
                    "pages": pages("to flatten (default: all)"),
                    "preset": { "type": "string", "enum": ["low", "medium", "high"], "description": "Default medium (300 ppi)." },
                    "resolution": { "type": "number", "minimum": 36, "maximum": 1200, "description": "Raster resolution in pixels per inch; overrides the preset's." },
                    "raster_vector_balance": { "type": "integer", "minimum": 0, "maximum": 100 },
                }),
                &["doc"],
            )),
        t("doc_fix_hairlines", "Fix hairlines", "Thicken strokes that would print thinner than thinner_than points (default 0.25) to replace_with points (default 0.25), measured as printed: the line width times the page's transform, in page content and the forms it draws. Other lines are left as they are. Reports how many strokes were fixed (fixed: 0 changes nothing). Undoable.")
            .cmd("prepress.hairlines")
            .with(schema(
                json!({
                    "doc": doc(),
                    "pages": pages("to fix (default: all)"),
                    "thinner_than": { "type": "number", "exclusiveMinimum": 0, "maximum": 72 },
                    "replace_with": { "type": "number", "exclusiveMinimum": 0, "maximum": 72 },
                }),
                &["doc"],
            )),
        t("doc_create_multiple", "Create PDF from multiple files", "Convert several files (PDFs, PNG/JPEG/JPEG 2000/TIFF/GIF/BMP images, .txt files) to PDF in one run. mode \"combine\" (default) joins them, in order, into one PDF with a bookmark per file; pages optionally chooses each file's pages, in step with paths (a range such as \"1-3, 6\" or null). mode \"separate\" writes one PDF per file into out_dir (files that are already PDFs are skipped; existing files are never overwritten) and reports each file's result.")
            .cmd("create.multiple")
            .with(schema(
                json!({
                    "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": pdfcraft_engine::MAX_CREATE_FILES },
                    "mode": { "type": "string", "enum": ["combine", "separate"] },
                    "pages": { "type": "array", "items": { "type": ["string", "null"] } },
                    "field_conflicts": { "type": "string", "enum": ["rename", "merge"], "description": "Mode \"combine\": form fields whose names collide across files are renamed (default) or merged when compatible, as in doc_combine." },
                    "out": save_out.clone(),
                    "open": open.clone(),
                    "out_dir": { "type": "string", "description": "Folder for mode \"separate\"." },
                }),
                &["paths"],
            )),
        t("doc_split", "Split a document", "Split into several files written to out_dir: every N pages, before given pages, at top-level bookmarks (bookmarks: true; files named after them), or by file size (max_mb). Files are <name>-partK.pdf; numeric suffixes keep existing files and colliding bookmark names distinct.")
            .cmd("page.split")
            .with(schema(
                json!({ "doc": doc(), "every": { "type": "integer", "minimum": 1 }, "before": pages("that start a new part"), "bookmarks": { "type": "boolean" }, "max_mb": { "type": "number", "exclusiveMinimum": 0 }, "out_dir": { "type": "string" } }),
                &["doc", "out_dir"],
            )),
        t("bookmark_list", "List bookmarks", "The bookmark tree with each bookmark's path, title, target page, open state and action: { type: goto, to_page, view, dest? } | { type: uri, url } | { type: other, action } (JavaScript, Launch… kept as is) | { type: none }.")
            .ro()
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("bookmark_add", "Add a bookmark", "Add a bookmark that goes to a page, under a parent bookmark (or at the top level), at a position. Undoable.").with(schema(
            json!({ "doc": doc(), "title": { "type": "string", "minLength": 1 }, "page": { "type": "integer", "minimum": 1 }, "parent": path("Parent bookmark (omit for the top level)"), "position": { "type": "integer", "minimum": 1, "description": "1-based position among the parent's children (default: last)." } }),
            &["doc", "title", "page"],
        )),
        t("bookmark_rename", "Rename a bookmark", "Change a bookmark's title. Undoable.")
            .with(schema(json!({ "doc": doc(), "path": path("The bookmark"), "title": { "type": "string", "minLength": 1 } }), &["doc", "path", "title"])),
        t("bookmark_delete", "Delete a bookmark", "Delete a bookmark and the bookmarks under it. Undoable.")
            .destructive()
            .with(schema(json!({ "doc": doc(), "path": path("The bookmark") }), &["doc", "path"])),
        t("bookmark_move", "Move a bookmark", "Move a bookmark (with its children) under another parent, or within its level. Undoable.").with(schema(
            json!({ "doc": doc(), "path": path("The bookmark to move"), "parent": path("New parent (omit for the top level)"), "position": { "type": "integer", "minimum": 1, "description": "1-based position among the new parent's children, counted after the bookmark is removed (default: last)." } }),
            &["doc", "path"],
        )),
        t("bookmark_set_page", "Set a bookmark's page", "Point a bookmark at another page. Undoable.")
            .with(schema(json!({ "doc": doc(), "path": path("The bookmark"), "page": { "type": "integer", "minimum": 1 } }), &["doc", "path", "page"])),
        t("bookmark_set_action", "Set a bookmark's action", "Bookmark Properties ▸ Actions: make a bookmark go to to_page (optionally shown as view), go to the named destination dest, or open url. Replaces its go-to or action; other actions (JavaScript…) can't be written here but are kept on other bookmarks. Undoable.").with(schema(
            json!({ "doc": doc(), "path": path("The bookmark"), "to_page": { "type": "integer", "minimum": 1 }, "view": { "type": "object", "description": "With to_page: how to show the page. { fit: XYZ|Fit|FitH|FitV|FitR|FitB|FitBH|FitBV, left, top, zoom (percent), rect [left, bottom, right, top] }; coordinates are PDF user space as stored in the destination (origin bottom-left of the unrotated page). Default fit: XYZ when left/top/zoom are given, FitR with rect, else Fit." }, "dest": { "type": "string", "description": "A named destination (see dest_list)." }, "url": { "type": "string" } }),
            &["doc", "path"],
        )),
        t("bookmark_from_structure", "New bookmarks from structure", "Make bookmarks from the headings of a tagged PDF's structure tree (H1–H6, Hn, and H, whose level is the number of Sect/Part/Art elements around it): one per heading in document order, nested by level (a deeper heading goes under the nearest earlier shallower one), titled with the heading's actual text, else its alternate text, else the text of the marked content it covers, and going to where the heading starts on its page (/XYZ with the heading's top; the page alone on a rotated page). replace: true removes every existing bookmark first; otherwise the new ones follow them at the top level. max_level leaves out deeper headings. Headings without any text or page are skipped and counted (untitled, unplaced). Untagged documents and documents without headings are refused. Undoable.").with(schema(
            json!({ "doc": doc(), "replace": { "type": "boolean", "description": "Replace all existing bookmarks (default false: append after them)." }, "max_level": { "type": "integer", "minimum": 1, "maximum": 64, "description": "Deepest heading level to include (default: all)." } }),
            &["doc"],
        )),
        t("dest_list", "List named destinations", "Every named destination: name, stored_in (names = the /Names /Dests tree, dests = the legacy /Dests dictionary), to_page and view (fit, left, top, zoom percent, rect in PDF user space), or unresolved: true, and how many links and bookmarks reference it.")
            .ro()
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("dest_set", "Set a named destination", "Create a named destination, or re-target an existing one (where it is stored), to show page with fit XYZ|Fit|FitH|FitV|FitR|FitB|FitBH|FitBV. left, top and rect [left, bottom, right, top] are PDF user-space coordinates as stored in the destination; zoom is a percentage (XYZ only; omitted keeps the reader's zoom). Default fit: XYZ when left/top/zoom are given, FitR with rect, else Fit. Returns created. Undoable.").with(schema(
            json!({
                "doc": doc(), "name": { "type": "string", "minLength": 1 }, "page": { "type": "integer", "minimum": 1 },
                "fit": { "type": "string", "enum": ["XYZ", "Fit", "FitH", "FitV", "FitR", "FitB", "FitBH", "FitBV"] },
                "left": { "type": "number" }, "top": { "type": "number" }, "zoom": { "type": "number", "exclusiveMinimum": 0, "maximum": 6400 },
                "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
            }),
            &["doc", "name", "page"],
        )),
        t("dest_rename", "Rename a named destination", "Rename a named destination; links and bookmarks that go to it follow (references_updated). Undoable.")
            .with(schema(json!({ "doc": doc(), "name": { "type": "string", "minLength": 1 }, "new_name": { "type": "string", "minLength": 1 } }), &["doc", "name", "new_name"])),
        t("dest_delete", "Delete a named destination", "Delete a named destination. Links and bookmarks that went to it keep the name and stop working, as in Acrobat; still_referenced says how many. Undoable.")
            .destructive()
            .with(schema(json!({ "doc": doc(), "name": { "type": "string", "minLength": 1 } }), &["doc", "name"])),
        t("page_number", "Number pages", "Label a range of pages (e.g. i, ii, iii for front matter, or A-1, A-2 for an appendix). Later pages keep their labels. Undoable.").with(schema(
            json!({
                "doc": doc(),
                "from": { "type": "integer", "minimum": 1 },
                "to": { "type": "integer", "minimum": 1 },
                "style": { "type": "string", "enum": ["decimal", "upper-roman", "lower-roman", "upper-alpha", "lower-alpha", "none"], "description": "Numbering style (default decimal; none = prefix only)." },
                "prefix": { "type": "string" },
                "start": { "type": "integer", "minimum": 1, "description": "Number of the first page in the range (default 1)." },
            }),
            &["doc", "from", "to"],
        )),
        t("form_fields", "List form fields", "Every interactive form field: name, type (text, checkbox, radio, combo, list, button, signature), value, options, page, rect (top-left-origin points) and rotation (0, 90, 180 or 270 degrees counterclockwise), read-only and required flags.")
            .ro()
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t(
            "form_fill",
            "Fill in form fields",
            "Set several fields at once (one undo step). values maps field names (from form_fields) to: a string for text fields, combo boxes and radio groups (an option), true/false for check boxes, an array of strings for multi-select lists. Appearances are regenerated so every viewer shows the values.",
        )
        .with(schema(json!({ "doc": doc(), "values": { "type": "object", "description": "Field name → value." } }), &["doc", "values"])),
        t("form_reset", "Clear form", "Reset fields to their default values: all of them, or only those listed. Undoable.")
            .destructive()
            .with(schema(json!({ "doc": doc(), "fields": { "type": "array", "items": { "type": "string" } } }), &["doc"])),
        t(
            "form_add_field",
            "Prepare form: add a field",
            "Add a form field on a page. type: text, date, checkbox, radio, combo, list, button, image (a picture placeholder; fill it with form_set_image), signature. rect in points from the top-left of the displayed page [x0, y0, x1, y1]. name defaults to Acrobat's next free name (Text1, Check Box1, Group1, Dropdown1, List Box1, Button1, Image1, Signature1, Date1). Radio buttons join the radio group named by group (a new group otherwise) with the export value export. Returns the field's name. Undoable.",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "page": { "type": "integer", "minimum": 1 },
                "type": { "type": "string", "enum": ["text", "date", "checkbox", "radio", "combo", "list", "button", "image", "signature"] },
                "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                "name": { "type": "string" },
                "multiline": { "type": "boolean" },
                "group": { "type": "string", "description": "Radio group to join." },
                "export": { "type": "string", "description": "Radio button export value (default Choice1)." },
                "options": { "type": "array", "items": { "type": "string" }, "description": "Combo box / list box items." },
                "editable": { "type": "boolean", "description": "Combo box accepts typed text." },
                "multi_select": { "type": "boolean" },
                "caption": { "type": "string", "description": "Button label." },
            }),
            &["doc", "page", "type", "rect"],
        )),
        t("stamp_custom", "Add a custom stamp", "Stamp a picture: a PDF page (file_page, default 1) or an image file, centred at at [x, y] (points from the top-left of the displayed page) at its natural size, at most 200 pt. name labels it (default: the file name). Undoable.")
            .cmd("comment.stamp")
            .with(schema(
                json!({
                    "doc": doc(),
                    "page": { "type": "integer", "minimum": 1 },
                    "path": { "type": "string" },
                    "at": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 },
                    "file_page": { "type": "integer", "minimum": 1 },
                    "name": { "type": "string" },
                    "author": { "type": "string" },
                }),
                &["doc", "page", "path", "at"],
            )),
        t("form_set_image", "Set an image field's picture", "Show an image file (PNG, JPEG, TIFF, GIF or BMP) in an image field or button, scaled to fit and centred (what clicking an image field does). Undoable.")
            .with(schema(json!({ "doc": doc(), "field": { "type": "string" }, "path": { "type": "string" } }), &["doc", "field", "path"])),
        t(
            "form_set_props",
            "Field properties",
            "Change field properties: give field for one field, or fields for a list of unique field names (1–1000). A bulk change is atomic and one Undo step; an invalid or locked field leaves every field unchanged. Only the given properties change, and partial appearance changes preserve each field's other colours and font. name (rename), rect (position) and rotation require a single field. General and Options: tooltip, read_only, required, multiline, max_length (0 = no limit), options (combo/list items), font_size (0 = auto), format, validate, calculate (Acrobat's native AF rules). Options: align (left, center, right), default (Reset form's value; check boxes/radio buttons use their on state, \"\" for off), flags { scroll, rich_text, password, file_select, spell_check, comb (needs max_length), sort, editable, multi_select, commit_immediately }, check_style (check boxes/radio buttons: check, circle, cross, diamond, square, star). rotation is 0, 90, 180 or 270 degrees counterclockwise and swaps width/height when the axis changes. Undoable.",
        )
        .with({
            let mut input = schema(
            json!({
                "doc": doc(),
                "field": { "type": "string", "description": "The field's current name." },
                "fields": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 1000, "uniqueItems": true, "description": "Apply the same changes to these fields in one atomic edit. Use either field or fields." },
                "name": { "type": "string" },
                "tooltip": { "type": "string" },
                "read_only": { "type": "boolean" },
                "required": { "type": "boolean" },
                "multiline": { "type": "boolean" },
                "max_length": { "type": "integer", "minimum": 0 },
                "options": { "type": "array", "items": { "type": "string" } },
                "align": { "type": "string", "enum": ["left", "center", "right"] },
                "locked": { "type": "boolean", "description": "Lock the field's properties (only unlocking is accepted while locked)." },
                "default": { "type": "string" },
                "check_style": { "type": "string", "enum": ["check", "circle", "cross", "diamond", "square", "star"], "description": "Check boxes and radio buttons: the mark shown when on." },
                "flags": { "type": "object", "additionalProperties": { "type": "boolean" } },
                "font_size": { "type": "number", "minimum": 0 },
                "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4, "description": "Move/resize: points from the top-left of the displayed page." },
                "rotation": { "type": "integer", "enum": [0, 90, 180, 270], "description": "Widget rotation in degrees, counterclockwise. Changing between horizontal and vertical swaps the rectangle around its center." },
                "appearance": { "type": "object", "description": "Appearance tab: border, fill, text_color (#RRGGBB, a name or \"none\"), width (0–12 points; 1 thin, 2 medium, 3 thick), style (solid, dashed, beveled, inset, underline), font (helvetica, times, courier)." },
                "format": { "description": "Format tab: {\"type\": \"number\", \"decimals\": 2, \"currency\": \"$\", \"separator\": 0-4, \"negative\": 0-3}, {\"type\": \"percent\"}, {\"type\": \"date\"|\"time\", \"pattern\": \"mm/dd/yyyy\"}, {\"type\": \"zip\"|\"zip4\"|\"phone\"|\"ssn\"}, {\"type\": \"mask\", \"mask\": \"AA-9999\"}, or \"none\"." },
                "validate": { "description": "Validate tab: {\"min\": 0, \"max\": 100} (either may be left out) or \"none\"." },
                "calculate": { "description": "Calculate tab: {\"op\": \"sum\"|\"product\"|\"average\"|\"min\"|\"max\", \"fields\": [\"a\", \"b\"]}, {\"notation\": \"Price * Qty\"} (simplified field notation), or \"none\"." },
            }),
            &["doc"],
        );
            input["oneOf"] = json!([{ "required": ["field"] }, { "required": ["fields"] }]);
            input
        }),
        t("form_tab_order", "Set the tab order", "Read or set the tab order of pages (default all). Without order or field, returns each page's order (pages: [{page, order}]). Set: row (top to bottom, left to right), column, structure, or annotations (unspecified). Or order tabs manually: `field` with `move` earlier|later moves that field one place on its page. Returns the resulting order of fields. Undoable.")
            .with(schema(
                json!({ "doc": doc(), "order": { "type": "string", "enum": ["row", "column", "structure", "annotations"] }, "pages": pages("to set (default: all)"), "field": { "type": "string" }, "move": { "type": "string", "enum": ["earlier", "later"] } }),
                &["doc"],
            )),
        t("doc_export_data", "Export comments or form data", "Write comments and/or form data to path; the extension picks the format: .xfdf or .fdf (comments and/or fields), .xml, .csv or .txt (form data). what: all (default), comments, fields.")
            .destructive()
            .with(schema(json!({ "doc": doc(), "path": { "type": "string" }, "what": { "type": "string", "enum": ["all", "comments", "fields"] } }), &["doc", "path"])),
        t("form_submit", "Submit a form", "Run a Submit Form action: field names a push button whose action submits the form (its address, format and field selection are used), or url submits every field as FDF. format overrides it: fdf, xfdf, html (name=value pairs) or pdf (the whole document as Save writes it). Fields marked NoExport, buttons and signatures are never sent. Give path to save the data there, or send: true to send it to the address (http or https only; no redirects; refused when the internet access settings block the host, see trust_urls_get, and the decision is returned as url_access). Returns url, format, content_type, method, bytes, the fields sent, and for a send the server's status.")
            .with(schema(
                json!({ "doc": doc(), "field": { "type": "string" }, "url": { "type": "string" }, "format": { "type": "string", "enum": ["fdf", "xfdf", "html", "pdf"] }, "path": { "type": "string" }, "send": { "type": "boolean" } }),
                &["doc"],
            )),
        t("doc_import_data", "Import comments or form data", "Import comments and/or field values from an XFDF, FDF, XML, CSV or tab-delimited text file (detected from its content). Comments with the same name are replaced; values go through the form's formats and validation. Undoable.")
            .with(schema(json!({ "doc": doc(), "path": { "type": "string" } }), &["doc", "path"])),
        t("form_delete_field", "Delete a field", "Delete a form field and all its widgets. Undoable.")
            .destructive()
            .with(schema(json!({ "doc": doc(), "field": { "type": "string" } }), &["doc", "field"])),
        t(
            "redact_mark",
            "Mark for redaction",
            "Mark content for redaction (nothing is removed until redact_apply). One of: rect [x0, y0, x1, y1] (points from the top-left of the displayed page) with page; find (text, every match); pattern (one Search & Redact pattern, every match; see redact_patterns); locale (every pattern of a locale set, e.g. en-GB); words (a list of words or phrases, every match of any) or words_file (a UTF-8 text file, one word or phrase per line), with match_case and whole_words; whole_pages: true. find, pattern, locale, words and whole_pages work on pages (default all). Overlay text shown on the box once applied: overlay (custom text), or codes from code_set (redaction codes such as us-foia (b)(6); see redact_codes; codes without code_set are a custom set). overlay_font (helvetica, times, courier), overlay_size (points; 0 = auto-size to fit, the default), overlay_color (default red), overlay_align (left, center, right), overlay_repeat (fill the box). Stored as /OverlayText, /DA, /Q and /Repeat. fill: box colour (default black). Undoable.",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "page": { "type": "integer", "minimum": 1 },
                "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                "find": { "type": "string", "minLength": 1 },
                "pattern": { "type": "string", "enum": ["phone", "email", "credit-card", "ssn", "date", "us-zip", "uk-postcode", "uk-nino", "ca-postal", "ca-sin", "fr-nir", "jp-postal", "jp-my-number", "iban"] },
                "locale": { "type": "string", "enum": ["intl", "en-US", "en-GB", "en-CA", "fr-FR", "ja-JP"] },
                "words": { "type": "array", "items": { "type": "string", "minLength": 1, "maxLength": 256 }, "minItems": 1, "maxItems": 1000 },
                "words_file": { "type": "string", "description": "A UTF-8 text file with one word or phrase per line (at most 1 MiB)." },
                "match_case": { "type": "boolean", "description": "words: match upper and lower case exactly (default false)." },
                "whole_words": { "type": "boolean", "description": "words: only whole words (default false)." },
                "whole_pages": { "type": "boolean" },
                "pages": pages("to search or mark (default: all)"),
                "overlay": { "type": "string", "maxLength": 512 },
                "code_set": { "type": "string", "enum": ["us-foia", "us-privacy-act"] },
                "codes": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                "overlay_font": { "type": "string", "enum": ["helvetica", "times", "courier"] },
                "overlay_size": { "type": "number", "minimum": 0, "maximum": 144 },
                "overlay_color": { "type": "string", "description": "#RRGGBB or a colour name." },
                "overlay_align": { "type": "string", "enum": ["left", "center", "right"] },
                "overlay_repeat": { "type": "boolean" },
                "fill": { "type": "string", "description": "#RRGGBB or a colour name." },
                "author": { "type": "string" },
            }),
            &["doc"],
        )),
        t(
            "redact_apply",
            "Apply redactions",
            "Apply the redaction marks (all, or those on pages): text, images, vectors, comments and form fields under them are removed for good and boxes are drawn in their place. A verification pass fails the operation if anything readable remains. Undoable until saved; the saved file no longer contains the content.",
        )
        .destructive()
        .with(schema(json!({ "doc": doc(), "pages": pages("whose marks to apply (default: all)") }), &["doc"])),
        t(
            "redact_codes",
            "List redaction codes",
            "The built-in redaction code sets (us-foia: U.S. FOIA exemptions, 5 U.S.C. 552(b); us-privacy-act: U.S. Privacy Act exemptions, 5 U.S.C. 552a) with each code and a short description. Pass code_set and codes to redact_mark to show codes on the boxes.",
        )
        .ro()
        .with(schema(json!({ "code_set": { "type": "string", "enum": ["us-foia", "us-privacy-act"] } }), &[])),
        t(
            "redact_patterns",
            "List Search & Redact patterns",
            "The Search & Redact patterns by locale set (intl, en-US, en-GB, en-CA, fr-FR, ja-JP) with their ids and labels. A locale set passed to redact_mark also includes the intl patterns.",
        )
        .ro()
        .with(schema(json!({ "locale": { "type": "string", "enum": ["intl", "en-US", "en-GB", "en-CA", "fr-FR", "ja-JP"] } }), &[])),
        t("redact_clear", "Remove redaction marks", "Remove every redaction mark without applying it. Undoable.")
            .destructive()
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t(
            "doc_hidden_info",
            "Find hidden information",
            "What Remove Hidden Information would remove, by category (metadata, attachments, comments, form-fields, hidden-text, hidden-layers, bookmarks, links-actions-scripts, private-data) with counts.",
        )
        .ro()
        .with(schema(json!({ "doc": doc() }), &["doc"])),
        t(
            "doc_remove_hidden",
            "Remove hidden information / Sanitize",
            "Remove the listed categories (see doc_hidden_info), or every category when none are listed (Sanitize Document). Form fields are flattened so their values stay visible. The next save rewrites the whole file. Undoable until saved.",
        )
        .destructive()
        .with(schema(
            json!({
                "doc": doc(),
                "categories": { "type": "array", "items": { "type": "string", "enum": ["metadata", "attachments", "comments", "form-fields", "hidden-text", "hidden-layers", "bookmarks", "links-actions-scripts", "private-data"] } },
            }),
            &["doc"],
        )),
        t(
            "print_total_area_coverage",
            "Total area coverage",
            "Output Preview's Total Area Coverage check: per page, the highest total ink (C+M+Y+K plus spot inks, 0-400%+) and where it is (max_at, a point from the top-left of the displayed page; pdf_max_at in user space), whether it is within limit, the area above limit (points² and % of the page) and the worst regions (rect from the top-left of the displayed page, plus pdf_rect in user space). Measured per plate on a grid at dpi (lowered on very large pages or with many inks), simulating overprint unless overprint is false. CMYK, Gray, Separation, DeviceN and Indexed colours are exact; RGB/Lab convert through the document's output intent CMYK profile, else a built-in approximation without a press profile (conversion says which; converted_paints counts them); patterns, shadings, JPEG/JPX images and annotations are listed under unmeasured, transparency painted opaque under approximated. Read-only.",
        )
        .ro()
        .with(schema(
            json!({
                "doc": doc(),
                "pages": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "maxItems": 200, "description": "1-based pages to check (default: all, at most the first 200)." },
                "limit": { "type": "number", "minimum": 1, "maximum": 2000, "description": "Total area coverage limit in percent (default 300)." },
                "dpi": { "type": "number", "minimum": 9, "maximum": 300, "description": "Grid resolution in cells per inch (default 72)." },
                "max_regions": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Worst regions to list per page (default 5)." },
                "overprint": { "type": "boolean", "description": "Simulate overprint (OP/op/OPM), as a press does (default true); false knocks every object out." },
            }),
            &["doc"],
        )),
        t(
            "print_separations_list",
            "Separations",
            "Output Preview's separations list: the inks of the document (or pages) - the four process inks (Cyan, Magenta, Yellow, Black) and every spot colorant of Separation and DeviceN colour spaces (forms, images, patterns and shadings included), each with kind, defined_by, its alternate colour space, tint transform function type, alternate_value (the alternate's components at 100% tint, for Type 2 and one-input Type 0 transforms into gray, RGB or CMYK), a preview colour, the 1-based pages where it is painted (non-zero ink) and the pages that declare it. Registration (All) and None are not inks. RGB/Lab content puts ink on the process plates through conversion (the output intent's CMYK profile, else the built-in approximation; converted_pages lists where). Read-only.",
        )
        .ro()
        .with(schema(
            json!({
                "doc": doc(),
                "pages": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "maxItems": 500, "description": "1-based pages (default: all, at most the first 500)." },
            }),
            &["doc"],
        )),
        t(
            "print_separations_render",
            "Render separations",
            "Render a page's separations as PNG: one 8-bit grayscale plate per ink (0 = 100% ink, 255 = none, like film), or with composite true one RGB preview of only the inks listed (the others hidden, as Output Preview's checkboxes do; each ink multiplies the paper by its preview colour). Images are in the displayed orientation (after /Rotate). Without path the images are returned inline (at most 8 plates); with path they are written as <path>-<ink>.png (composite: path itself). The result lists each plate's maximum and inked area. Ink is measured per plate on a grid at dpi (lowered on large pages or with many inks; see print_total_area_coverage for what is exact and what is not measured). Read-only.",
        )
        .ro()
        .with(schema(
            json!({
                "doc": doc(),
                "page": { "type": "integer", "minimum": 1 },
                "inks": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Ink names from print_separations_list (default: every ink of the page)." },
                "dpi": { "type": "number", "minimum": 9, "maximum": 300, "description": "Resolution in pixels per inch (default 72)." },
                "overprint": { "type": "boolean", "description": "Simulate overprint (default true)." },
                "composite": { "type": "boolean", "description": "One colour preview of the listed inks instead of a plate per ink (default false)." },
                "path": { "type": "string", "description": "Write the PNGs here instead of returning them: a file name prefix for plates, the file for a composite." },
            }),
            &["doc", "page"],
        )),
        t(
            "print_separations_sample",
            "Ink at a point",
            "Output Preview's ink readout: the percentage of every ink of the page at point (x, y) (points from the top-left of the displayed page), their total, and the sampled grid cell (cell, pdf_cell). Measured at dpi (default 72) with overprint simulated unless overprint is false; RGB/Lab convert as conversion says. Read-only.",
        )
        .ro()
        .with(schema(
            json!({
                "doc": doc(),
                "page": { "type": "integer", "minimum": 1 },
                "x": { "type": "number", "minimum": 0 },
                "y": { "type": "number", "minimum": 0 },
                "dpi": { "type": "number", "minimum": 9, "maximum": 300, "description": "Grid resolution in cells per inch (default 72)." },
                "overprint": { "type": "boolean", "description": "Simulate overprint (default true)." },
            }),
            &["doc", "page", "x", "y"],
        )),
        t(
            "print_object_inspector",
            "Object Inspector",
            "Output Preview's Object Inspector: every object painted at point (x, y) of a page, topmost first (annotations above page content): type (text, path, image, shading, form, annotation), order (painting order), rect (top-left of the displayed page; pdf_rect in user space), painted (fill/stroke covering the point), fill and stroke colour (space such as DeviceCMYK, ICCBased (4), Separation (Gold), DeviceN (...), Indexed (...); components; inks with percentages after the Ink Manager's routing, converted when from RGB/Lab or a spot converted to process; pattern), overprint (op, OP, OPM), transparency (blend_mode, fill_alpha ca, stroke_alpha CA, soft_mask, alpha_is_shape), rendering_intent, line_width (points on the page), fill_rule, forms (enclosing form XObjects); text: font, resource, font_type, embedded, subset, size, effective_size, render_mode, invisible; image: name (null for inline), width, height, bits_per_component, colour_space, filters, ppi [horizontal, vertical] and effective_ppi, mask, interpolate, intent; shading: shading_type and colour_space; form: name and transparency group; annotation: subtype, name, hidden, printed. separations holds every plate's ink at the point as print_separations_sample does (at dpi, default 72, overprint simulated unless overprint is false). units: view (default, points from the top-left of the displayed page) or pdf (user space). Geometry: fills by winding rule, strokes within half the line width (at least 0.5 pt), clipping paths as bounding boxes, text as string boxes; optional content is not hidden. Read-only.",
        )
        .ro()
        .with(schema(
            json!({
                "doc": doc(),
                "page": { "type": "integer", "minimum": 1 },
                "x": { "type": "number" },
                "y": { "type": "number" },
                "units": { "type": "string", "enum": ["view", "pdf"], "description": "view (default): points from the top-left of the displayed page; pdf: PDF user space." },
                "dpi": { "type": "number", "minimum": 9, "maximum": 300, "description": "Separation grid resolution for the ink values, in cells per inch (default 72)." },
                "overprint": { "type": "boolean", "description": "Simulate overprint in the ink values (default true)." },
            }),
            &["doc", "page", "x", "y"],
        )),
        t(
            "print_ink_manager_list",
            "Ink Manager",
            "Ink Manager's list: every ink the document (or pages) uses - the four process inks and each spot colorant of Separation and DeviceN colour spaces (forms, images, patterns and shadings included), as print_separations_list describes them, ignoring the Ink Manager settings - with its usage (pages where it is painted, pages that declare it, objects: the kinds of object that put it down: text, vector, image; shadings and patterns are not measured) and where it prints under the session's Ink Manager settings: route own (its own plate), alias (prints_on another ink's plate, aliased_to the ink it was aliased to) or process (converted to CMYK). settings holds the current settings. Read-only.",
        )
        .ro()
        .with(schema(
            json!({
                "doc": doc(),
                "pages": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "maxItems": 500, "description": "1-based pages (default: all, at most the first 500)." },
            }),
            &["doc"],
        )),
        t(
            "print_ink_manager_set",
            "Set Ink Manager",
            "Change the Ink Manager settings of an open document for this session (not saved with the document, not an undo step): aliases maps a spot ink to the ink (process or spot) it prints on instead (null or \"\" removes an alias; aliases chain, cycles are refused); to_process adds spot inks converted to process (CMYK through their tint transform and alternate colour: gray to black, CMYK as is, RGB and Lab through the output intent's CMYK profile or the built-in conversion); own_plate puts inks back on their own plate (removing their alias and to-process setting); all_to_process converts every spot ink not aliased to a process ink; use_lab previews spot inks with a Lab alternate from their Lab values (otherwise mid gray); reset starts from the defaults. An aliased ink follows the ink it is aliased to (converted when that one is). Names must be inks of the document (checked for documents of at most 500 pages). print_separations_list, print_separations_render, print_separations_sample and print_total_area_coverage honour the settings (routed lists the inks moved); print_ink_manager_apply makes them permanent. Returns the settings and each spot ink's route.",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "aliases": { "type": "object", "additionalProperties": { "type": ["string", "null"] }, "description": "Spot ink name to the ink it prints on (null or empty removes the alias)." },
                "to_process": { "type": "array", "items": { "type": "string" }, "description": "Spot inks to convert to process." },
                "own_plate": { "type": "array", "items": { "type": "string" }, "description": "Inks to print on their own plate again (alias and to-process removed)." },
                "all_to_process": { "type": "boolean", "description": "Convert every spot ink to process (Acrobat's All Spots to Process)." },
                "use_lab": { "type": "boolean", "description": "Preview Lab-defined spot inks from their Lab values." },
                "reset": { "type": "boolean", "description": "Start from the default settings (every ink on its own plate) before applying the rest." },
            }),
            &["doc"],
        )),
        t(
            "print_ink_manager_apply",
            "Apply Ink Manager",
            "Make the session's Ink Manager settings permanent, as one undoable edit: every Separation and DeviceN colour space naming an aliased spot ink names the ink it finally prints on instead, in every object of the document (pages, forms, images, shadings, patterns, annotation appearances; DeviceN Colorants entries too; each keeps its own alternate and tint transform, so composite viewing is unchanged), then the spot inks set to process are converted to DeviceCMYK on every page through their tint transforms (Convert Colors' spot conversion: through the output intent's CMYK profile when it has a usable one, else the built-in conversion; destination says which). A DeviceN colour space in which an alias would repeat an ink, or which mixes inks converted to process with inks kept, is left as it was and listed in skipped. dry_run reports what would change without changing anything. changed false means nothing would change (no undo step). The settings stay set. Save the document to keep it.",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "dry_run": { "type": "boolean", "description": "Only report what would change (default false)." },
            }),
            &["doc"],
        )),
        t(
            "print_overprint_preview",
            "Overprint preview preference",
            "Preferences ▸ Page Display ▸ Use Overprint Preview: when page_render and render_preview simulate overprint. Modes: never; always; pdfx (Only For PDF/X Files, the default: documents declaring GTS_PDFXVersion or with a GTS_PDFX output intent); automatic (pages with objects painted with overprint on; pages without them render as usual, so it shows the same as always). Set it with mode (for this session), or read it; with doc also says whether that document is PDF/X and whether the preference applies to it. Simulation: the page is separated into ink plates by PdfCraft's ink rasterizer (as print_separations_render) with and without overprint (OP/op, OPM 1 nonzero rule) and without the overprinting objects; where overprint changes a plate, the render's pixels move from the knocked-out colour to the overprinted one by the object's coverage (process inks through the renderer's own CMYK conversion, spot inks multiplied by their preview colour, RGB/Lab separated through the output intent's CMYK profile when usable). Patterns, shadings, DCT/JPX/JBIG2/CCITT images and annotations are not simulated; transparency is painted opaque.",
        )
        .with(schema(
            json!({
                "mode": { "type": "string", "enum": ["never", "always", "pdfx", "automatic"], "description": "Set the preference (default pdfx)." },
                "doc": { "type": "integer", "minimum": 1, "description": "Also report whether this document is PDF/X and the preference applies to it." },
            }),
            &[],
        )),
        t(
            "print_overprint_objects",
            "Overprinting objects",
            "The objects of a page painted with overprint on: fills, strokes, text and images (paint), with their colour (space, components, CMYK when known), overprint mode, rect (top-left of the displayed page; pdf_rect in user space), object (the form or page whose content paints them) and occurrences (identical objects merge, as in print_color_warnings: kind overprint-paint, or overprint-white for a white or zero-tint object that may disappear on press). changes_appearance says whether overprinting changes the page inside the object's rect (changed_cells: separation grid cells, at dpi, default 72, where a plate differs from knockout); visible_objects counts those. states lists the ExtGStates that turn overprint on or set OPM 1. changed_percent is the share of the page that overprint preview changes; preference and preview_applies say whether page_render simulates it. Read-only.",
        )
        .ro()
        .with(schema(
            json!({
                "doc": doc(),
                "page": { "type": "integer", "minimum": 1 },
                "dpi": { "type": "number", "minimum": 9, "maximum": 300, "description": "Separation grid resolution in cells per inch (default 72)." },
                "max_objects": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "Objects listed at most (default 100)." },
            }),
            &["doc", "page"],
        )),
        t(
            "print_color_warnings",
            "Overprint and rich-black warnings",
            "Output Preview's colour warnings, per page: overprint-state (an ExtGState sets OP/op true or OPM 1; object is the ExtGState), overprint-paint (fills, strokes, text or images painted with overprint on), overprint-white (warning: a white or zero-tint object set to overprint, which disappears on press for DeviceCMYK with OPM 1 and for spot colours), rich-black (K at or above rich_black_k with C+M+Y at or above rich_black_cmy_total; colour.total is the summed ink), rich-black-small-text (warning: rich-black text below small_text_pt, using font size × text and current matrices) and registration-colour (warning: Separation /All outside printer marks, recognised by Artifact PrinterMarks marked content, a \"Printer marks\" layer, or lying outside the TrimBox). DeviceCMYK, four-component ICC, Indexed, DeviceN process colorants and spot colours with Type 0/Type 2 tint transforms are resolved to C, M, Y, K (via_alternate marks spot alternates); RGB/Lab, image pixels, shadings and other tint transforms are counted under unresolved. Follows forms and coloured tiling patterns (loops and depth capped). Identical findings merge, with occurrences and a union rect (top-left of the displayed page; pdf_rect in user space). counts has every kind; findings keep at most max_findings per kind. Read-only.",
        )
        .ro()
        .with(schema(
            json!({
                "doc": doc(),
                "pages": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "description": "1-based pages to check (default: all)." },
                "rich_black_k": { "type": "number", "minimum": 1, "maximum": 100, "description": "Percent black at or above which a colour counts as black (default 95)." },
                "rich_black_cmy_total": { "type": "number", "minimum": 1, "maximum": 300, "description": "Percent C+M+Y at or above which a black is rich (default 10)." },
                "small_text_pt": { "type": "number", "minimum": 1, "maximum": 144, "description": "Rich-black text below this effective size in points is a warning (default 12)." },
                "max_findings": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "Findings kept per kind (default 100); counts include the rest." },
            }),
            &["doc"],
        )),
        t(
            "print_convert_colors",
            "Convert colors",
            "Print Production ▸ Convert Colors: colours of the chosen pages in the chosen source spaces become the destination space: DeviceCMYK (default), DeviceGray, DeviceRGB, or ICCBased with an ICC profile file (gray, RGB or CMYK). Converts fill and stroke colours of page content, the forms it draws (each once, even when pages share them), coloured tiling patterns, shadings (functions resampled), image XObjects and inline images (pixels re-encoded; Indexed images get a new palette), and annotation appearances; objects limits it to text, vector (paths, shadings, patterns) and/or images. With a profile its BToA table for the intent (lut8/lut16) or its matrix/TRC is used from CIE XYZ (DeviceRGB and 3-component ICC read as sRGB, CalRGB/CalGray/Lab colorimetrically); otherwise (and for v4-only profiles: approximate true) a built-in approximation: RGB to CMYK with full black generation and 70% under-colour removal. Gray always goes to the black plate and CMYK numbers carry over between CMYK spaces. preserve_black (default true) makes pure black (RGB 0 0 0, gray 0, 100% K, Lab L* 0) the destination's pure black (100% K). Spot colours (Separation, DeviceN) are kept unless spots is convert (through their tint transforms); /All registration colour never changes. output_intent makes the profile the PDF/X output intent. Images in JPX, JBIG2, CCITT or CMYK JPEG, with colour-key masks or over 40 million pixels are left and listed in skipped. Returns counts of what changed (colors = colour operators written) and changed false when nothing would change (no undo step). Undoable as one step.",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "pages": pages("to convert (default: all)"),
                "destination": { "type": "string", "enum": ["DeviceCMYK", "DeviceGray", "DeviceRGB", "ICCBased"], "description": "Destination space (default DeviceCMYK, or ICCBased when profile is given)." },
                "profile": { "type": "string", "description": "Path to the destination ICC profile (.icc/.icm, at most 32 MB), for ICCBased." },
                "sources": { "type": "array", "items": { "type": "string", "enum": ["all", "DeviceGray", "DeviceRGB", "DeviceCMYK", "ICCBased", "CalGray", "CalRGB", "Lab"] }, "minItems": 1, "description": "Source spaces to convert (default: all; the destination's own device space is never converted)." },
                "objects": { "type": "array", "items": { "type": "string", "enum": ["all", "text", "vector", "images"] }, "minItems": 1, "description": "Object types to convert (default: all)." },
                "intent": { "type": "string", "enum": ["perceptual", "relative_colorimetric", "saturation", "absolute_colorimetric"], "description": "Rendering intent: which profile table is used (default relative_colorimetric; absolute is converted as relative)." },
                "preserve_black": { "type": "boolean", "description": "Pure black becomes pure black (100% K) (default true)." },
                "spots": { "type": "string", "enum": ["preserve", "convert"], "description": "Keep spot colours (default) or convert them to the destination." },
                "output_intent": { "type": "boolean", "description": "Make the destination profile the PDF/X output intent (needs profile; default false)." },
                "output_condition": { "type": "string", "maxLength": 256, "description": "The output intent's printing condition (default: the profile's description)." },
            }),
            &["doc"],
        )),
        t(
            "print_add_printer_marks",
            "Add printer marks",
            "Print Production ▸ Add Printer Marks: enlarge each page's media box around its trim box (the crop box when there is none) and draw printer marks outside it: trim (crop) marks at the corners, bleed marks on the bleed box edges, registration targets in the /Separation /All colour, CMYK colour bars and a black tint bar along the top, and a page information line (file name, page number, date) along the bottom, as the page is displayed. Marks start offset points outside the trim (further out when the bleed is wider) and run 18 pt. The crop box becomes the new media box; the trim, bleed and art boxes keep their old rectangles. Marks are drawn as page artifacts in an optional content group named \"Printer marks\" (hide it to hide them), and the page information is outlines, so text extraction ignores it. Adding marks again replaces them. Returns each page's trim, bleed and new media box (user space) and the margin added (points). Undoable.",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "pages": pages("to mark (default: all)"),
                "marks": { "type": "array", "items": { "type": "string", "enum": ["all", "trim", "bleed", "registration", "color_bars", "page_info"] }, "minItems": 1, "description": "Marks to draw (default: all)." },
                "line_weight": { "type": "number", "exclusiveMinimum": 0, "maximum": 4, "description": "Mark line weight in points (default 0.25)." },
                "offset": { "type": "number", "minimum": 0, "maximum": 72, "description": "Distance of the marks from the trim, in points (default 6)." },
                "style": { "type": "string", "enum": ["western", "eastern"], "description": "Western (default) or Eastern (Japanese tombo: double corner lines, centre crosses)." },
                "file_name": { "type": "string", "maxLength": 256, "description": "Name the page information shows (default: the document's file name)." },
            }),
            &["doc"],
        )),
        t(
            "associated_files_list",
            "List associated files",
            "PDF 2.0 associated files (/AF, ISO 32000-2 §14.13): file specifications with a relationship (Source, Data, Alternative, Supplement, EncryptedPayload, FormData, Schema, Unspecified) that belong to the document, a page, an annotation, a structure element or an XObject. Without target: the document's files, then every page's and annotation's that has any (structure elements and XObjects only by target object). Returns {count, owners: [{target, files: [{index (1-based), name, description, relationship, mime, size, embedded, creation_date, mod_date, checksum (MD5 hex), object, attachment (also in the EmbeddedFiles name tree), problem}], issues}]}.",
        )
        .ro()
        .with(schema(json!({ "doc": doc(), "target": af_target() }), &["doc"])),
        t(
            "associated_file_add",
            "Add an associated file",
            "Embed a file (path, or data_base64 with name) and associate it with target (default the document): an EmbeddedFile stream with its MIME type (mime, else guessed from the name's extension) and Size, CheckSum, CreationDate and ModDate, in a file specification with /F, /UF, /Desc (description) and /AFRelationship (relationship, default Unspecified), appended to the target's /AF. A document-level file is also added to the EmbeddedFiles name tree, so it is an attachment too (as PDF/A-3 expects); a name already taken there gets \" (2)\". At most 64 MiB. Returns the document summary and added {target, file}. Undoable.",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "target": af_target(),
                "path": { "type": "string", "description": "The file to embed." },
                "data_base64": { "type": "string", "description": "The file's bytes in base64, instead of path (needs name)." },
                "name": { "type": "string", "maxLength": 255, "description": "File name (default: path's file name)." },
                "mime": { "type": "string", "description": "MIME type such as text/csv (default: guessed from the name; none when unknown)." },
                "relationship": { "type": "string", "enum": ["Source", "Data", "Alternative", "Supplement", "EncryptedPayload", "FormData", "Schema", "Unspecified"], "description": "How the file relates to the target (case-insensitive; default Unspecified)." },
                "description": { "type": "string", "maxLength": 4096, "description": "/Desc." },
            }),
            &["doc"],
        )),
        t(
            "associated_file_remove",
            "Remove an associated file",
            "Take one associated file (by name, or 1-based index as associated_files_list reports) out of target's /AF (default the document); a document-level file also leaves the EmbeddedFiles name tree. A name that several files share is refused (pass index). Entries that aren't file specifications can be removed by index. Returns the document summary and removed {target, file}. Undoable.",
        )
        .destructive()
        .with(schema(json!({ "doc": doc(), "target": af_target(), "name": { "type": "string" }, "index": { "type": "integer", "minimum": 1 } }), &["doc"])),
        t(
            "associated_file_extract",
            "Save an associated file",
            "Write one associated file's embedded data (by name or 1-based index, in target, default the document) to path. Saving is allowed for any type; risk (allow, prompt or block) and trust (as trust_attachments_get checks) say whether opening it would be, with a warning when it isn't plainly safe. Returns {path, bytes, file, risk, trust, warning?}.",
        )
        .destructive()
        .with(schema(
            json!({ "doc": doc(), "target": af_target(), "name": { "type": "string" }, "index": { "type": "integer", "minimum": 1 }, "path": { "type": "string", "description": "Where to write the file." } }),
            &["doc", "path"],
        )),
        t(
            "attachment_list",
            "List attachments",
            "The document's file attachments as the Attachments panel lists them: the EmbeddedFiles name tree, then FileAttachment comments by page. Returns {count, attachments: [{index (1-based), name, description, size, source (\"document\" or {page, annotation}), trust}]}, where trust is the attachment trust policy's verdict on opening it (decision allow, prompt or block, with extension, basis and reason; see trust_attachments_get).",
        )
        .ro()
        .with(schema(json!({ "doc": doc() }), &["doc"])),
        t(
            "attachment_open",
            "Open an attachment",
            "Open a file attachment (by name or 1-based index from attachment_list) the way double-clicking it in the Attachments panel does, after checking the attachment trust policy (Trust Manager ▸ attachments). A blocked type (programs and scripts such as .exe, .bat, .js, .vbs, or one on your deny list) is never opened: returns {status: \"blocked\", opened: false, reason, trust}, even with confirm. An unknown type (or no extension) returns {status: \"prompt\", question, trust} until called again with confirm: true. An allowed (or confirmed) PDF opens as a new document: {status: \"opened\", document}. Any other type opens in another application, which nothing headless launches: it is written to path for launching, {status: \"ready\", path, bytes, launched: false}. To save a file without opening it, use attachment_extract.",
        )
        .destructive()
        .with(schema(
            json!({
                "doc": doc(),
                "name": { "type": "string" },
                "index": { "type": "integer", "minimum": 1 },
                "confirm": { "type": "boolean", "description": "Answer Open to the prompt for an unknown type (default false). Has no effect on blocked types." },
                "path": { "type": "string", "description": "A non-PDF attachment: where to write it for launching." },
                "password": { "type": "string", "description": "An encrypted PDF attachment's password." },
            }),
            &["doc"],
        )),
        t(
            "attachment_extract",
            "Save an attachment",
            "Save a file attachment (by name or 1-based index from attachment_list) to path, without opening it. Allowed for any type, as saving to a chosen place is; risk (allow, prompt or block) and trust say whether opening it would be, with a warning when it isn't plainly safe. Returns {path, bytes, attachment, risk, trust, warning?}.",
        )
        .destructive()
        .with(schema(
            json!({ "doc": doc(), "name": { "type": "string" }, "index": { "type": "integer", "minimum": 1 }, "path": { "type": "string", "description": "Where to write the file." } }),
            &["doc", "path"],
        )),
        t(
            "trust_attachments_get",
            "Attachment trust policy",
            "Trust Manager ▸ attachments: which attachment types may be opened. Returns {allow, deny (your lists of extensions, lower case without the dot), builtin_deny (programs, scripts, installers, shortcuts, disk images and macro documents: never opened), builtin_allow (plain documents and images), unknown: \"prompt\", order}. Your deny list wins, then your allow list, then the built-in lists; anything else (or a name with no extension) prompts. With names, also checks each name as it would be launched (last path component, case folded, trailing dots and spaces and invisible or direction-changing characters removed, an NTFS stream suffix read both ways; the last extension decides, so report.pdf.exe is an .exe and is flagged disguised): checks [{name, normalized, extension, inner_extensions, decision (allow, prompt, block), basis (user_deny, user_allow, builtin_deny, builtin_allow, unknown, no_extension), disguised, reason}].",
        )
        .ro()
        .with(schema(json!({ "names": { "type": "array", "items": { "type": "string" }, "maxItems": 1024, "description": "File names to check." } }), &[])),
        t(
            "trust_attachments_set",
            "Change the attachment trust policy",
            "Change your attachment allow and deny lists (Trust Manager ▸ attachments). allow / deny replace a list; add_allow / add_deny add extensions (taking them off the other list); remove takes extensions off both; reset starts from empty lists first. Extensions may be written .EXE, exe or *.exe; letters, digits and - _ + ~ $ only, at most 32 characters and 256 per list; one extension can't be in both. Allowing a built-in blocked type such as exe makes it open. The desktop app keeps the lists in its settings; a headless session keeps them until it ends. Returns the policy as trust_attachments_get does.",
        )
        .with(schema(
            json!({
                "allow": { "type": "array", "items": { "type": "string" }, "maxItems": 256 },
                "deny": { "type": "array", "items": { "type": "string" }, "maxItems": 256 },
                "add_allow": { "type": "array", "items": { "type": "string" }, "maxItems": 256 },
                "add_deny": { "type": "array", "items": { "type": "string" }, "maxItems": 256 },
                "remove": { "type": "array", "items": { "type": "string" }, "maxItems": 256 },
                "reset": { "type": "boolean", "description": "Clear both lists before applying the rest (default false)." },
            }),
            &[],
        )),
        t("printers", "List printers", "The printers the system's print spooler knows (CUPS on macOS and Linux), with the default marked.").ro().with(schema(json!({}), &[])),
        t("printer_options", "List printer options", "A printer driver's own job options (CUPS: from its PPD), such as the paper tray, paper type or finishing: key, label, group, default and choices. Pass chosen values to doc_print as options. Empty on Windows, where the driver's preferences window holds them.")
            .ro()
            .with(schema(json!({ "printer": { "type": "string" } }), &["printer"])),
        t(
            "doc_print",
            "Print",
            "Print with Acrobat's Print dialog options, or save the print-ready PDF. pages: a range such as \"1-3, 6, 9-\" (page labels allowed; default all); subset odd/even; reverse. layout: fit (default), actual, shrink, custom (scale %), multiple (per_sheet 2/4/6/9/16, order, border, auto_rotate; cut-stack order arranges single-sided sheets for cutting into piles and stacking left to right, top to bottom, keeping sheet order within each pile; duplex must be off), booklet (booklet_subset both/front/back, binding left/right), poster (scale %, overlap pt, cut_marks). orientation auto/portrait/landscape; comments_forms document / document-and-markups (default) / document-and-stamps / form-fields-only; paper Letter/Legal/Tabloid/A3/A4/A5. as_image (Advanced ▸ Print As Image) sends each sheet as one picture at dpi (36-1200, default 300; lowered for a sheet above 64 million pixels, refused above 1 billion for the job) so the printer gets no fonts, vectors or transparency; the result's as_image lists each sheet's pixel size and dpi. Then path (save) or printer (a name or \"default\") with copies, collate, duplex off/long-edge/short-edge, grayscale, and options (driver options from printer_options, CUPS).",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "pages": { "type": "string" },
                "subset": { "type": "string", "enum": ["all", "odd", "even"] },
                "reverse": { "type": "boolean" },
                "layout": { "type": "string", "enum": ["fit", "actual", "shrink", "custom", "multiple", "booklet", "poster"] },
                "scale": { "type": "number", "exclusiveMinimum": 0 },
                "per_sheet": { "type": "integer", "minimum": 1, "maximum": 256 },
                "order": { "type": "string", "enum": ["horizontal", "horizontal-reversed", "vertical", "vertical-reversed", "cut-stack"] },
                "border": { "type": "boolean" },
                "auto_rotate": { "type": "boolean" },
                "booklet_subset": { "type": "string", "enum": ["both", "front", "back"] },
                "binding": { "type": "string", "enum": ["left", "right"] },
                "overlap": { "type": "number", "minimum": 0 },
                "cut_marks": { "type": "boolean" },
                "orientation": { "type": "string", "enum": ["auto", "portrait", "landscape"] },
                "comments_forms": { "type": "string", "enum": ["document", "document-and-markups", "document-and-stamps", "form-fields-only"] },
                "paper": { "type": "string" },
                "as_image": { "type": "boolean", "description": "Print as image: rasterize each sheet (works around font and transparency problems)." },
                "dpi": { "type": "number", "minimum": 36, "maximum": 1200, "description": "Print-as-image resolution, pixels per inch (default 300)." },
                "path": { "type": "string" },
                "printer": { "type": "string" },
                "copies": { "type": "integer", "minimum": 1, "maximum": 999 },
                "collate": { "type": "boolean" },
                "duplex": { "type": "string", "enum": ["off", "long-edge", "short-edge"] },
                "grayscale": { "type": "boolean" },
                "options": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Printer driver options from printer_options: key → choice value." },
            }),
            &["doc"],
        )),
        t("content_list", "List added content", "Text and images added with page_add_text/page_add_image (or Edit a PDF ▸ Add content), per page with a 1-based index, rect (points from the top-left of the page), and text style. When the document does not allow copying text, text fields are null and text_withheld explains why.")
            .ro()
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t(
            "page_add_text",
            "Add text to a page",
            "Add text as page content (not a comment). Place it with at [x, y] (top-left, points from the top-left of the displayed page) and width (wrap width, default 200), or rect. Newlines start new lines; long lines wrap. Style: font helvetica/times/courier, size, bold, italic, color (#RRGGBB or a name), align left/center/right. It stays editable with content_update. Undoable.",
        )
        .cmd("edit.text")
        .with(schema(
            json!({
                "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "text": { "type": "string", "minLength": 1 },
                "at": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 },
                "width": { "type": "number", "exclusiveMinimum": 0 },
                "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                "font": { "type": "string", "enum": ["helvetica", "times", "courier"] }, "size": { "type": "number", "minimum": 1, "maximum": 500 },
                "bold": { "type": "boolean" }, "italic": { "type": "boolean" }, "color": { "type": "string" },
                "align": { "type": "string", "enum": ["left", "center", "right", "justify"] },
            }),
            &["doc", "page", "text"],
        )),
        t(
            "page_add_image",
            "Add an image to a page",
            "Add an image file (PNG, JPEG, TIFF, GIF, BMP) as page content: in rect (points from the top-left of the page), or centred at its natural size (shrunk to fit). Undoable; movable with content_update.",
        )
        .cmd("edit.image")
        .with(schema(
            json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "path": { "type": "string" }, "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 } }),
            &["doc", "page", "path"],
        )),
        t("content_update", "Edit added content", "Move/resize (rect), retype (text) or restyle (font, size, bold, italic, color, align) an added item (page, index from content_list). Images: rotate (degrees, multiple of 90, counter-clockwise), flip_h / flip_v (toggle), crop [left, bottom, right, top] as fractions trimmed, image (a file that replaces the picture). Undoable.")
            .with(schema(
                json!({
                    "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "index": { "type": "integer", "minimum": 1 },
                    "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 }, "text": { "type": "string" },
                    "font": { "type": "string", "enum": ["helvetica", "times", "courier"] }, "size": { "type": "number", "minimum": 1, "maximum": 500 },
                    "bold": { "type": "boolean" }, "italic": { "type": "boolean" }, "color": { "type": "string" },
                    "align": { "type": "string", "enum": ["left", "center", "right", "justify"] },
                    "rotate": { "type": "integer" }, "flip_h": { "type": "boolean" }, "flip_v": { "type": "boolean" },
                    "crop": { "type": "array", "items": { "type": "number", "minimum": 0, "maximum": 0.49 }, "minItems": 4, "maxItems": 4 },
                    "image": { "type": "string", "description": "Replace the picture with this file." },
                }),
                &["doc", "page", "index"],
            )),
        t("content_delete", "Delete added content", "Delete an added item (page, index from content_list). Undoable.")
            .destructive()
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "index": { "type": "integer", "minimum": 1 } }), &["doc", "page", "index"])),
        t("link_list", "List links", "Every link: page, 1-based index (for link_edit/link_delete), rect (points from the top-left of the page), and where it goes: url, to_page (with view when it is not plain Fit), or dest (a named destination, with its resolved to_page and view, or unresolved: true).")
            .ro()
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("link_add", "Add a link", "Add a link over rect on page that opens url, goes to to_page (optionally shown as view: position and zoom, fit width, a rectangle…) or goes to the named destination dest. Appearance: visible (rectangle), color, width 1-3, highlight none/invert/outline/inset. Undoable.")
            .cmd("edit.link")
            .with(schema(
                json!({
                    "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                    "url": { "type": "string" }, "to_page": { "type": "integer", "minimum": 1 }, "view": { "type": "object", "description": "With to_page: how to show the page. { fit: XYZ|Fit|FitH|FitV|FitR|FitB|FitBH|FitBV, left, top, zoom (percent), rect [left, bottom, right, top] }; coordinates are PDF user space as stored in the destination (origin bottom-left of the unrotated page). Default fit: XYZ when left/top/zoom are given, FitR with rect, else Fit." }, "dest": { "type": "string", "description": "A named destination (see dest_list)." },
                    "visible": { "type": "boolean" }, "color": { "type": "string" }, "width": { "type": "number" }, "highlight": { "type": "string", "enum": ["none", "invert", "outline", "inset"] },
                }),
                &["doc", "page", "rect"],
            )),
        t("link_edit", "Edit a link", "Change a link's rect, destination (url, to_page with optional view, or dest) or appearance. Undoable.")
            .with(schema(
                json!({
                    "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "index": { "type": "integer", "minimum": 1 },
                    "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                    "url": { "type": "string" }, "to_page": { "type": "integer", "minimum": 1 }, "view": { "type": "object", "description": "With to_page: how to show the page. { fit: XYZ|Fit|FitH|FitV|FitR|FitB|FitBH|FitBV, left, top, zoom (percent), rect [left, bottom, right, top] }; coordinates are PDF user space as stored in the destination (origin bottom-left of the unrotated page). Default fit: XYZ when left/top/zoom are given, FitR with rect, else Fit." }, "dest": { "type": "string", "description": "A named destination (see dest_list)." },
                    "visible": { "type": "boolean" }, "color": { "type": "string" }, "width": { "type": "number" }, "highlight": { "type": "string", "enum": ["none", "invert", "outline", "inset"] },
                }),
                &["doc", "page", "index"],
            )),
        t("link_delete", "Delete a link", "Delete one link (page, index from link_list). Undoable.")
            .destructive()
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "index": { "type": "integer", "minimum": 1 } }), &["doc", "page", "index"])),
        t("links_from_urls", "Create links from URLs", "Find web addresses (http://, https://, www.) in the text of every page and make them clickable links. Returns the URLs. Undoable.")
            .cmd("edit.links_from_urls")
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("links_remove", "Remove all links", "Remove every link in the document. Undoable.")
            .destructive()
            .cmd("edit.remove_links")
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("measure_distance", "Measure distance", "Add an undoable two-point distance annotation using the scale of the first point's viewport.")
            .cmd("measure.distance").with(measure_schema()),
        t("measure_perimeter", "Measure perimeter", "Add an undoable connected-line length annotation. To include a closing edge, repeat the first point at the end.")
            .cmd("measure.perimeter").with(measure_schema()),
        t("measure_area", "Measure area", "Add an undoable area annotation from a simple polygon. The last edge closes automatically.")
            .cmd("measure.area").with(measure_schema()),
        t("measure_info", "Read a measurement", "Calculate a live distance, perimeter or area, deltas, angle and scale without adding an annotation. Incomplete paths are allowed.")
            .ro().cmd("measure.info").with(schema(json!({"doc":doc(),"page":{"type":"integer","minimum":1},"points":measure_points(),"type":{"type":"string","enum":["distance","perimeter","area"]}}), &["doc","page","points"])),
        t("measure_list", "List measurements", "Saved measurement annotations with calculated values, scale and vertices in display coordinates. Measurements with unsupported imported formats (compound or fractional units, non-rectilinear scales) or invalid geometry are listed under unsupported with a reason.")
            .ro().with(schema(json!({"doc":doc(),"page":{"type":"integer","minimum":1}}), &["doc"])),
        t("measure_scale", "Set or read a measurement scale", "Read the scale at a point, or add a rectangular viewport using units_per_point or two calibration points and their real-world distance. Existing measurements retain their original scales. Undoable.")
            .cmd("measure.scale").with(schema(json!({"doc":doc(),"page":{"type":"integer","minimum":1},"at":point(),"rect":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4},"name":{"type":"string"},"unit":{"type":"string"},"precision":{"type":"integer","minimum":0,"maximum":6},"units_per_point":{"type":"number","exclusiveMinimum":0},"points":measure_points(),"distance":{"type":"number","exclusiveMinimum":0}}), &["doc","page"])),
        t("measure_snap", "Snap a measurement vertex", "Snap a point to vector paths, endpoints, midpoints or intersections. Coordinates and tolerance are in display points. Bounded extraction reports truncated geometry.")
            .ro().cmd("measure.snap").with(schema(json!({"doc":doc(),"page":{"type":"integer","minimum":1},"at":point(),"tolerance":{"type":"number","minimum":0,"maximum":10000},"endpoints":{"type":"boolean"},"midpoints":{"type":"boolean"},"intersections":{"type":"boolean"},"paths":{"type":"boolean"}}), &["doc","page","at"])),
        t("measure_export", "Export measurements as CSV", "Atomically write saved measurement values, labels, authors and scale ratios as spreadsheet-safe CSV. Returns how many unsupported measurements were left out.")
            .cmd("measure.export").with(schema(json!({"doc":doc(),"out":path_arg()}), &["doc","out"])),
        t("comment_list", "List comments", "Every comment (annotation other than links, form widgets and pop-ups) with its page, index, id, type, author, text, date, rectangle, colour, review status and replies.")
            .ro()
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1, "description": "Only this page." } }), &["doc"])),
        t(
            "comment_add",
            "Add a comment",
            "Add a comment as Acrobat's commenting tools do. Geometry is in points with the origin at the top-left of the displayed page, y down (as in page_render images at 72 dpi and text_find rects). \
             note: `at` [x, y] (icon top-left). stamp: `at` [x, y] (its centre) and `stamp`: approved, completed, confidential, draft, final, for comment, for public release, information only, not approved, not for public release, preliminary results, void, accepted, initial here, rejected, sign here, witness; dynamic: true for the dynamic approved/confidential/received/reviewed/revised stamps with a By … at … line. highlight/underline/strikeout/squiggly: `find` (text on the page to mark; every match with all: true) or `quads`. \
             rectangle/oval/textbox: `rect` [x0, y0, x1, y1]. line/arrow: `from`, `to`. Optional `endings` (two names: start, end) for a line, arrow or polyline, or one name for a callout: None, Square, Circle, Diamond, OpenArrow, ClosedArrow, Butt, ROpenArrow, RClosedArrow, Slash. An arrow without endings is None then OpenArrow. ink: `strokes` [[[x, y], …], …]. \
             polygon/cloud/polyline (connected lines): `points` [[x, y], …]. callout: `rect` (its text box), `to` (the point the arrow touches), optional `knee`. caret (inserted text): `at`, the insertion point on the baseline. replace (Replace Text): `find` or `quads` like highlight, `contents` the replacement. attachment: `path` (the file), `at`, optional `icon` (PushPin, Paperclip, Graph, Tag). Undoable.",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "page": { "type": "integer", "minimum": 1 },
                "type": { "type": "string", "enum": ["note", "highlight", "underline", "strikeout", "squiggly", "replace", "rectangle", "oval", "line", "arrow", "ink", "textbox", "stamp", "polygon", "cloud", "polyline", "callout", "caret", "attachment"] },
                "path": { "type": "string", "description": "attachment: the file to attach." },
                "points": { "type": "array", "items": point(), "minItems": 2 },
                "knee": point(),
                "contents": { "type": "string", "description": "The comment text (what a text box shows)." },
                "author": { "type": "string" },
                "at": point(),
                "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                "from": point(),
                "to": point(),
                "find": { "type": "string", "description": "Text on the page to mark up (case-insensitive)." },
                "all": { "type": "boolean", "description": "Mark every match of `find` on the page, not just the first." },
                "quads": { "type": "array", "items": { "type": "array", "items": { "type": "number" }, "minItems": 8, "maxItems": 8 } },
                "stamp": { "type": "string" },
                "dynamic": { "type": "boolean" },
                "strokes": { "type": "array", "items": { "type": "array", "items": point() } },
                "icon": { "type": "string", "description": "note: Comment, Note, Help, Insert, Key, NewParagraph, Paragraph; attachment: PushPin, Paperclip, Graph, Tag." },
                "color": color(),
                "fill": color(),
                "opacity": { "type": "number", "minimum": 0, "maximum": 1 },
                "width": { "type": "number", "minimum": 0, "description": "Line width in points." },
                "endings": { "type": "array", "items": { "type": "string", "enum": ["None", "Square", "Circle", "Diamond", "OpenArrow", "ClosedArrow", "Butt", "ROpenArrow", "RClosedArrow", "Slash"] }, "minItems": 1, "maxItems": 2, "description": "Line endings. Two names (start, end) for a line, arrow or polyline; one name for a callout." },
                "font_size": { "type": "number", "exclusiveMinimum": 0 },
            }),
            &["doc", "page", "type"],
        )),
        t("comment_reply", "Reply to a comment", "Add a reply to a comment's thread. Undoable.").with(schema(
            comment_ref(json!({ "text": { "type": "string", "minLength": 1 }, "author": { "type": "string" } })),
            &["doc", "text"],
        )),
        t("comment_set_status", "Set a comment's status", "Set a comment's review status (Acrobat: Set status), recorded as a status reply. Undoable.").with(schema(
            comment_ref(json!({ "status": { "type": "string", "enum": ["none", "accepted", "rejected", "cancelled", "completed"] }, "author": { "type": "string" } })),
            &["doc", "status"],
        )),
        t("sign_list", "List and validate signatures", "Every signature field with its validation: status (valid, unknown = intact but the signer isn't trusted, invalid, unsigned), signer and certificate, date, reason, location, certification level, page and rect, the revision it covers and changes made after signing (none, allowed, disallowed), with Acrobat-style explanations. `revisions` outlines every saved revision after the first: its number, length in bytes, the signature field that saved it (or null) and what it changed, as {page (1-based, null for the whole document), kind}.")
            .ro()
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("sign_verify", "Verify signatures at a selected time", "Read-only certificate and revocation validation at an explicit time. Reports trusted, claimed and chosen time separately. Never fetches or changes trust.")
            .ro().cmd("sign.verification_options").with(schema(json!({"doc":doc(),"verification_time":verification_time()}), &["doc"])),
        t("sign_signed_version", "View signed version", "View Signed Version: the file exactly as the signature in `field` signed it (the bytes up to the end of its /ByteRange, i.e. the revision it covers). Writes it to `out` (atomically) and/or opens it as a new, unsaved document (open: default true unless out is given). Returns the revision, its length, the current length, whole_file (nothing was added after signing), out and the opened doc id and summary. Refused for unsigned fields, an invalid byte range and certificate-encrypted documents.")
            .with(schema(json!({ "doc": doc(), "field": { "type": "string", "description": "The signature field's fully qualified name (from sign_list)." }, "out": path_arg(), "open": { "type": "boolean" } }), &["doc", "field"])),
        t("sign_compare_signed_version", "Compare signed version to current version", "Compare Signed Version to Current Version: every change made after the signature in `field` was applied, read from the objects the later revisions changed or added (the same diff signature validation uses). Each change has kind (page content, comments, form fill, changes to locked fields, signature, pages added or removed, document structure, links, metadata, document security store, other changes), action (added, modified, removed), page (1-based; for something removed, in the signed version), object, subject (page, page content, an annotation subtype, form field…), the form field's name and its value before and after, and whether the signature's DocMDP/FieldMDP permissions allow it. Also: permissions (certification level tightened by a field lock; null = approval signature) in words, page counts and pages added/removed, the categories with permitted flags, modified and permitted (the verdict signature validation reports). With text (default true) also a word-level Compare Files report between the signed and current versions (visual: also compare how pages look, default false). limit caps the listed changes (default 500).")
            .ro()
            .with(schema(json!({ "doc": doc(), "field": { "type": "string", "description": "The signature field's fully qualified name (from sign_list)." }, "text": { "type": "boolean" }, "visual": { "type": "boolean" }, "limit": { "type": "integer", "minimum": 1 } }), &["doc", "field"])),
        t("sign_timestamp", "Timestamp and save document", "Request an RFC 3161 document timestamp from an explicitly configured server and save an incremental revision. Sends only the digest request. Requires allow_network: true; no redirects; response and time limits. No automatic certificate trust.")
            .cmd("sign.timestamp").with(schema(json!({"doc":doc(),"out":path_arg(),"timestamp_server":timestamp_server(),"allow_network":{"type":"boolean"}}), &["doc","out","timestamp_server","allow_network"])),
        t("sign_embed_evidence", "Embed local validation evidence", "Validate supplied DER certificates, CRLs and OCSP responses, then append a DSS and optionally a document timestamp. Rejects unrelated/unverifiable revocation data. Preserves original signed bytes. Reports incomplete coverage; does not declare PAdES B-LT/B-LTA compliance or trust imported certificates.")
            .cmd("sign.embed_evidence").with(schema(json!({"doc":doc(),"out":path_arg(),"certificates":{"type":"array","items":path_arg(),"maxItems":64},
                "crls":{"type":"array","items":path_arg(),"maxItems":64},"ocsps":{"type":"array","items":path_arg(),"maxItems":64},
                "verification_time":verification_time(),"timestamp_server":timestamp_server(),"allow_network":{"type":"boolean"}}), &["doc","out"])),
        t("sign_id_create", "Create a digital ID", "Create a self-signed digital ID and save it as a password-protected .p12 file (Acrobat: Configure a new digital ID ▸ Create a new digital ID ▸ Save to file). Default key: 2048-bit RSA, valid 5 years.").with(schema(
            json!({
                "name": { "type": "string", "minLength": 1 },
                "organization": { "type": "string" },
                "unit": { "type": "string" },
                "email": { "type": "string" },
                "country": { "type": "string", "description": "Two-letter country code." },
                "key": { "type": "string", "enum": ["rsa2048", "rsa3072", "rsa4096", "p256"] },
                "years": { "type": "integer", "minimum": 1, "maximum": 50 },
                "password": { "type": "string", "minLength": 6 },
                "path": { "type": "string", "description": "The .p12 file to write." },
            }),
            &["name", "password", "path"],
        )),
        t("sign_windows_ids", "List Windows store digital IDs", "Windows: signing identities in the Current User Personal certificate store (certificate details and the windows: reference sign_document takes). Private keys remain in CNG; Windows may ask permission to use them.")
            .ro()
            .cmd("sign.digital")
            .with(schema(json!({}), &[])),
        t("sign_keychain_ids", "List Keychain digital IDs", "macOS: the signing identities in the user's keychains (certificate details and the keychain: reference sign_document takes). The private keys stay in the Keychain, which may ask the user to allow their use.")
            .ro()
            .cmd("sign.digital")
            .with(schema(json!({}), &[])),
        t("sign_document", "Sign a document", "Sign with a digital ID (a .p12/.pfx path, or on macOS a Keychain identity: \"keychain:<common name or fingerprint>\" from sign_keychain_ids, or on Windows a store identity: \"windows:<common name or fingerprint>\" from sign_windows_ids) and save the signed file to `out` (signing always saves, as in Acrobat; the document then shows the signed file). Sign an existing empty signature field (`field`), or a new one on `page` at `rect` (omit rect for an invisible signature). certify: no_changes, form_fill or comments makes a certification signature. lock (all, include or exclude, with lock_fields) makes the fields read-only once signed and records a FieldMDP lock, so later changes to them invalidate the signature; without it, a field's own lock (sign_field_lock) applies. lock_document: true permits no changes after signing. PAdES B-B, SHA-256 (SHA-384 for P-384 keys).")
            .cmd("sign.digital")
            .with(schema(
                json!({
                    "doc": doc(),
                    "id": { "type": "string", "description": "Digital ID file (.p12 / .pfx), keychain: reference on macOS, or windows: reference on Windows." },
                    "password": { "type": "string" },
                    "field": { "type": "string" },
                    "page": { "type": "integer", "minimum": 1 },
                    "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                    "reason": { "type": "string" },
                    "location": { "type": "string" },
                    "contact": { "type": "string" },
                    "certify": { "type": "string", "enum": ["no_changes", "form_fill", "comments"] },
                    "lock": lock_schema(),
                    "lock_fields": { "type": "array", "items": { "type": "string" }, "description": "The fields lock include or exclude names." },
                    "lock_document": { "type": "boolean", "description": "Lock the document after signing: no further changes are permitted." },
                    "out": { "type": "string", "description": "Where to save the signed document." },
                    "timestamp_server": timestamp_server(),
                    "allow_network": {"type":"boolean","description":"Required true when timestamp_server is supplied."},
                }),
                &["doc", "id", "out"],
            )),
        t("sign_field_lock", "Set what a signature field locks", "Signature field properties ▸ Signed: choose the fields an unsigned signature field `field` makes read-only when it is signed: lock all, include or exclude (with lock_fields), or none to clear it. lock_document: true also permits no changes to the document after signing. Signature fields are never locked, so they can still be signed. Undoable.")
            .with(schema(
                json!({
                    "doc": doc(),
                    "field": { "type": "string" },
                    "lock": lock_schema(),
                    "lock_fields": { "type": "array", "items": { "type": "string" } },
                    "lock_document": { "type": "boolean" },
                }),
                &["doc", "field"],
            )),
        t("sign_trust", "Trust certificates", "Add certificates (.cer/.crt/.pem/.der, or the certificates in a .p12 with `password`) to the trusted certificates used to validate signatures, or clear the list (clear: true). Every open document is revalidated. Returns the trusted list.").with(schema(
            json!({ "paths": { "type": "array", "items": { "type": "string" } }, "password": { "type": "string" }, "clear": { "type": "boolean" } }),
            &[],
        )),
        t("sign_os_trust_get", "Operating system trust settings", "Read whether the operating system's root certificates are trusted (Acrobat: Preferences ▸ Signatures ▸ Identities & Trusted Certificates ▸ Windows Integration / macOS Keychain): signatures (for validating signatures and timestamps), certified_documents (for validating certification signatures; effective only while signatures is on, reported as certified_documents_effective) and source (system, bundle:<path> or directory:<path>). Both are off by default. Here they last for the session.")
            .ro()
            .with(schema(json!({}), &[])),
        t("sign_os_trust_set", "Trust operating system root certificates", "Turn trusting the operating system's root certificates on or off for validating signatures (signatures) and certified documents (certified_documents; needs signatures on, as in Acrobat), and optionally choose where they are read from: source system (the platform store: macOS keychains, Windows certificate stores, the Linux/BSD system CA bundle and directory, honouring SSL_CERT_FILE/SSL_CERT_DIR), bundle (a PEM/DER file at path) or directory (PEM/DER files at path). Turning one on reads the roots once (malformed entries are skipped and counted); every open document is revalidated, and sign_list reports trust_source operating_system for a chain anchored by one of them. Nothing changes if the source can't be read. Returns the settings, and roots and skipped counts when on.").with(schema(
            json!({
                "signatures": { "type": "boolean" },
                "certified_documents": { "type": "boolean" },
                "source": { "type": "string", "enum": ["system", "bundle", "directory"] },
                "path": { "type": "string", "description": "The bundle file or certificate directory, for source bundle or directory." },
            }),
            &[],
        )),
        t("sign_os_roots", "List operating system root certificates", "The operating system's root certificates as read from the configured source (sign_os_trust_set), whether or not they are trusted: count, skipped (unreadable entries), duplicates, truncated (capped), supported (false in the browser), up to 16 errors, and roots (certificate details, filtered by query on subject, name or SHA-256 fingerprint; offset and limit, default 100, at most 1000). reload: true reads the source again and revalidates open documents.")
            .ro()
            .with(schema(
                json!({
                    "reload": { "type": "boolean" },
                    "query": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000 },
                }),
                &[],
            )),
        t("trust_urls_get", "Internet access settings", "Read the policy for internet access from PDFs (Acrobat: Trust Manager ▸ Internet Access from PDF Files): mode (allow-all, block-all or custom), default_action for hosts on no rule in custom mode (allow, block or ask), and rules [{host, action, subdomains}]. It governs links' URI actions, Submit Form and scripts' app.launchURL/submitForm. The desktop app keeps it with its settings; here it lasts for the session.")
            .ro()
            .with(schema(json!({}), &[])),
        t("trust_urls_set", "Change internet access settings", "Change the internet access policy: mode (allow-all, block-all, custom), default_action (allow, block, ask), rules (replace the whole list), remove (hosts to drop), then add (rules to add or replace). A rule's host is a name (matches its subdomains unless subdomains: false), *.name, an IP literal (matches only itself) or a whole address; it is stored lowercase, without a trailing dot, port or user name, internationalised names in their xn-- form. Nothing changes unless every argument is valid. Returns the policy and the hosts removed.").with(schema(
            json!({
                "mode": { "type": "string", "enum": ["allow-all", "block-all", "custom"] },
                "default_action": { "type": "string", "enum": ["allow", "block", "ask"] },
                "rules": url_rules(),
                "add": url_rules(),
                "remove": { "type": "array", "maxItems": 1000, "items": { "type": "string" } },
            }),
            &[],
        )),
        t("trust_url_check", "Check an address against internet access settings", "Evaluate url (or up to 100 urls) the way a link, Submit Form or script request is: decision allow, block or prompt (ask first), kind web (governed by the policy), mail (mailto: opens the email app; always prompt) or refused (any other scheme, hidden characters, too long: always block), the scheme, the normalised host, the rule that decided and the reason. Reaches nothing.")
            .ro()
            .with(schema(json!({ "url": { "type": "string" }, "urls": { "type": "array", "minItems": 1, "maxItems": 100, "items": { "type": "string" } } }), &[])),
        t("comment_mark", "Mark a comment with a checkmark", "Mark or unmark a comment with a checkmark (Acrobat: Mark with checkmark), recorded as a private reply. Undoable.").with(schema(
            comment_ref(json!({ "marked": { "type": "boolean", "description": "Default true." }, "author": { "type": "string" } })),
            &["doc"],
        )),
        t("comment_lock", "Lock a comment", "Lock or unlock a comment (Properties ▸ Locked). Locked comments can't be moved, resized, restyled or deleted; their text stays editable. Undoable.").with(schema(
            comment_ref(json!({ "locked": { "type": "boolean", "description": "Default true." } })),
            &["doc"],
        )),
        t("comments_hide", "Hide all comments", "Hide or show every comment on the page (fields and links still draw). A view setting: the file doesn't change.")
            .cmd("comment.hide_all")
            .with(schema(json!({ "doc": doc(), "hidden": { "type": "boolean", "description": "Default true." } }), &["doc"])),
        t("comments_summarize", "Summarize comments", "Make a PDF summarising every comment (number, author, type, date, text, replies), sorted by page, author, date or type. layout: comments-only (default); connector-lines (each commented page beside its comments, joined by lines from numbered markers); sequence-numbers (each commented page with numbered markers, then its comments).")
            .cmd("comment.summarize")
            .with(schema(
                json!({
                    "doc": doc(),
                    "sort": { "type": "string", "enum": ["page", "author", "date", "type"] },
                    "layout": { "type": "string", "enum": ["comments-only", "connector-lines", "sequence-numbers"] },
                    "out": save_out,
                    "open": open
                }),
                &["doc"],
            )),
        t("comment_edit", "Edit a comment", "Change a comment's text, colour, opacity, line width, rectangle (rectangle/oval/text box/stamp) or position (`move` [dx, dy] in points). Stamps keep their original appearance when resized. One undo step.").with(schema(
            comment_ref(json!({
                "contents": { "type": "string" },
                "color": color(),
                "opacity": { "type": "number", "minimum": 0, "maximum": 1 },
                "width": { "type": "number", "minimum": 0 },
                "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                "move": point(),
            })),
            &["doc"],
        )),
        t("comment_format_text", "Format a text box's text", "Format characters start..end (0-based, end exclusive; the whole text when both are left out) of a text box or callout: font (Helvetica, Times or Courier), size, color, bold, italic, underline, strikethrough; align sets the box's alignment. The box keeps its width and fits the text. Returns the text's runs (comment_list lists them too). One undo step.").with(schema(
            comment_ref(json!({
                "start": { "type": "integer", "minimum": 0 },
                "end": { "type": "integer", "minimum": 0 },
                "font": { "type": "string", "enum": ["Helvetica", "Times", "Courier"] },
                "size": { "type": "number", "minimum": 1, "maximum": 400 },
                "color": color(),
                "bold": { "type": "boolean" },
                "italic": { "type": "boolean" },
                "underline": { "type": "boolean" },
                "strikethrough": { "type": "boolean" },
                "align": { "type": "string", "enum": ["left", "center", "right"] },
            })),
            &["doc"],
        )),
        t("comment_image_preview", "Preview image signature layers", "Return a PNG of the page without the selected image signature/initials (layer=background, default), or its embedded image with alpha (layer=image). Includes the displayed rectangle, document rotation, image_rotation (how far clockwise the image is shown turned) and annotation opacity. Cache these layers for live placement/resizing; commit once with comment_edit. Does not change the document or undo history.")
            .ro().with(schema(comment_ref(json!({ "layer": { "type": "string", "enum": ["background", "image"], "default": "background" }, "dpi": { "type": "number", "minimum": 1, "maximum": 600, "default": 96 } })), &["doc"])),
        t("comment_delete", "Delete a comment", "Delete a comment with its pop-up and replies. Undoable.").destructive().with(schema(comment_ref(json!({})), &["doc"])),
        t(
            "doc_protect",
            "Protect with passwords",
            "Encrypt the document (applied by the next doc_save, a full rewrite). open_password is needed to open it. permissions_password is needed to change security and lifts the restrictions given by printing/changes/copy/accessibility; those restrictions (and their defaults) apply only when permissions_password is given. With open_password alone the document is encrypted and everything stays allowed, so passing a restriction without permissions_password is an error. Passwords are never echoed back. Undoable.",
        )
        .cmd("protect.password")
        .with(schema(
            json!({
                "doc": doc(),
                "open_password": { "type": "string", "minLength": 1, "description": "Required to open the document. On its own it restricts nothing." },
                "permissions_password": { "type": "string", "minLength": 1, "description": "Required to change security; enables printing/changes/copy/accessibility and their defaults." },
                "printing": { "type": "string", "enum": ["none", "low", "high"], "description": "Needs permissions_password. Default high." },
                "changes": { "type": "string", "enum": ["none", "pages", "fill-sign", "comment-fill-sign", "any-except-extract"], "description": "Needs permissions_password. Default none." },
                "copy": { "type": "boolean", "description": "Allow copying text and images (needs permissions_password; default false)." },
                "accessibility": { "type": "boolean", "description": "Allow screen readers to read the text (needs permissions_password; default true)." },
                "compatibility": { "type": "string", "enum": ["aes-256", "aes-128", "rc4-128", "rc4-40"], "description": "Default aes-256 (Acrobat X and later)." },
                "encrypt_metadata": { "type": "boolean", "description": "Default true." },
            }),
            &["doc"],
        )),
        t(
            "security_protect_certificates",
            "Protect with certificates",
            "Encrypt the document for recipient certificates (public-key security, ISO 32000-2 §7.6.5, adbe.pkcs7.s5): it then opens only with a recipient's digital ID (doc_open with certificate). Each recipient is a certificate file (.cer/.crt/.pem/.der) or a digital ID (.p12/.pfx, with password; only its certificate is used) with its own permissions; change_security makes a recipient owner-equivalent (may change or remove security, everything allowed). Include yourself with change_security, or nobody can change the security later. RSA certificates only. Applied by the next doc_save (a full rewrite). Undoable.",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "recipients": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 256,
                    "items": {
                        "type": "object",
                        "properties": {
                            "certificate": { "type": "string", "description": "Certificate or digital ID file (relative to --root when set)." },
                            "password": { "type": "string", "description": "Password of a .p12/.pfx file." },
                            "printing": { "type": "string", "enum": ["none", "low", "high"], "description": "Default high." },
                            "changes": { "type": "string", "enum": ["none", "pages", "fill-sign", "comment-fill-sign", "any-except-extract"], "description": "Default none." },
                            "copy": { "type": "boolean", "description": "Allow copying text and images (default false)." },
                            "accessibility": { "type": "boolean", "description": "Allow screen readers (default true)." },
                            "change_security": { "type": "boolean", "description": "Owner-equivalent: change or remove security, every permission (default false)." },
                        },
                        "required": ["certificate"],
                        "additionalProperties": false,
                    },
                },
                "compatibility": { "type": "string", "enum": ["aes-256", "aes-128"], "description": "Default aes-256 (Acrobat X and later)." },
                "encrypt_metadata": { "type": "boolean", "description": "Default true." },
            }),
            &["doc", "recipients"],
        )),
        t("doc_unprotect", "Remove security", "Remove password or certificate security (the document must have been opened with its permissions password, or have none; certificate security needs a recipient allowed to change security). Applied by the next doc_save. Undoable.")
            .cmd("protect.remove")
            .destructive()
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("page_replace", "Replace pages", "Replace the content of pages with pages of another PDF (same count). Links, comments, form fields and bookmarks on the original pages stay, as in Acrobat. Undoable.")
            .cmd("page.replace")
            .with(schema(json!({ "doc": doc(), "pages": pages("to replace"), "path": path_arg(), "from_pages": pages("of the other file, in order (default: its first pages)") }), &["doc", "pages", "path"])),
        t("page_duplicate", "Duplicate pages", "Insert copies of pages after the last of them (fonts and images are shared, not copied). Undoable.")
            .cmd("page.duplicate")
            .with(schema(json!({ "doc": doc(), "pages": pages("to duplicate") }), &["doc", "pages"])),
        t(
            "page_set_box",
            "Set page boxes / crop",
            "Set a page box (crop by default; also trim, bleed, art, media) on pages: either margins in points from the media box [left, bottom, right, top], or an absolute rect in points from the top-left of the displayed page. Omit both to reset the box to its default. Undoable.",
        )
        .cmd("page.boxes")
        .with(schema(
            json!({
                "doc": doc(),
                "pages": pages("to change (default: all)"),
                "box": { "type": "string", "enum": ["crop", "trim", "bleed", "art", "media"] },
                "margins": { "type": "array", "items": { "type": "number", "minimum": 0 }, "minItems": 4, "maxItems": 4 },
                "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
            }),
            &["doc"],
        )),
        t("page_transitions", "List page transitions", "Each page's transition (ISO 32000-2 §12.4.4: style Split, Blinds, Box, Wipe, Dissolve, Glitter, R (replace), Fly, Push, Cover, Uncover, Fade, or none; duration in seconds; dimension, motion, direction, scale, opaque) and advance: seconds before a presentation moves on automatically (null = manually). Viewers use them in full-screen mode.")
            .ro()
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t(
            "page_set_transition",
            "Set page transitions",
            "Set the transition shown when pages appear in full-screen mode (Page Transitions), or style \"none\" to remove it. Options apply by style (ISO 32000-2 Table 164): dimension for Split and Blinds; motion for Split, Box and Fly; direction (degrees counterclockwise from left-to-right) for Wipe (0, 90, 180, 270), Glitter (0, 270, 315), Fly, Cover, Uncover and Push (0, 270; Fly also \"none\" when scale is not 1); scale and opaque for Fly. advance: seconds each page stays before moving on automatically; omit it for manual advance (it is replaced along with the transition). Undoable.",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "pages": pages("to change (default: all)"),
                "style": { "type": "string", "enum": ["none", "Split", "Blinds", "Box", "Wipe", "Dissolve", "Glitter", "R", "Fly", "Push", "Cover", "Uncover", "Fade"] },
                "duration": { "type": "number", "exclusiveMinimum": 0, "maximum": 3600, "description": "Seconds the effect takes (default 1)." },
                "dimension": { "type": "string", "enum": ["horizontal", "vertical"] },
                "motion": { "type": "string", "enum": ["inward", "outward"] },
                "direction": { "description": "0, 90, 180, 270, 315 or \"none\" (see above)." },
                "scale": { "type": "number", "exclusiveMinimum": 0, "maximum": 100 },
                "opaque": { "type": "boolean" },
                "advance": { "type": "number", "minimum": 0, "maximum": 3600, "description": "Seconds before advancing automatically." },
            }),
            &["doc", "style"],
        )),
        t(
            "doc_header_footer",
            "Add header and footer",
            "Add a header and/or footer to pages. Text boxes: header_left/center/right, footer_left/center/right. Tokens: <<1>> page number, <<n>> page count, <<1 of n>>, <<Page 1 of n>>, <<1/n>>, dates <<m/d/yyyy>> <<yyyy-mm-dd>> <<mmmm d, yyyy>> (and more), Bates <<Bates Number#6#1#PREFIX#SUFFIX>>. replace: true swaps out existing ones (Update). Undoable.",
        )
        .cmd("edit.header_footer")
        .with(schema(
            json!({
                "doc": doc(),
                "pages": pages("to mark (default: all)"),
                "header_left": { "type": "string" }, "header_center": { "type": "string" }, "header_right": { "type": "string" },
                "footer_left": { "type": "string" }, "footer_center": { "type": "string" }, "footer_right": { "type": "string" },
                "font_size": { "type": "number", "exclusiveMinimum": 0 },
                "color": { "type": "string", "description": "#RRGGBB or a colour name." },
                "margins": { "type": "array", "items": { "type": "number", "minimum": 0 }, "minItems": 4, "maxItems": 4, "description": "Top, bottom, left, right in points (default 36, 36, 72, 72)." },
                "start_number": { "type": "integer", "minimum": 1 },
                "replace": { "type": "boolean" },
            }),
            &["doc"],
        )),
        t("doc_watermark", "Add watermark", "Add a watermark to pages: text, or a picture from `file` (an image, or page `file_page` of a PDF) at `scale` of the page (default 0.5); rotated (degrees counter-clockwise, default 45), semi-transparent (opacity 0–1, default 0.5), text fitted to the page unless font_size is given, on top unless behind: true. replace: true swaps out existing ones. Undoable.")
            .cmd("edit.watermark")
            .with(schema(
                json!({
                    "doc": doc(),
                    "pages": pages("to mark (default: all)"),
                    "text": { "type": "string", "minLength": 1 },
                    "file": { "type": "string" },
                    "file_page": { "type": "integer", "minimum": 1 },
                    "scale": { "type": "number", "exclusiveMinimum": 0, "maximum": 1 },
                    "font_size": { "type": "number", "exclusiveMinimum": 0 },
                    "color": { "type": "string" },
                    "opacity": { "type": "number", "minimum": 0, "maximum": 1 },
                    "rotation": { "type": "number" },
                    "behind": { "type": "boolean" },
                    "replace": { "type": "boolean" },
                }),
                &["doc"],
            )),
        t("doc_background", "Add background", "Fill page backgrounds with a colour, or a picture from `file` (an image, or page `file_page` of a PDF) fitted at `scale` (default 1), behind the content. Undoable.")
            .cmd("edit.background")
            .with(schema(
                json!({ "doc": doc(), "pages": pages("to fill (default: all)"), "color": { "type": "string" }, "file": { "type": "string" }, "file_page": { "type": "integer", "minimum": 1 }, "scale": { "type": "number", "exclusiveMinimum": 0, "maximum": 1 }, "opacity": { "type": "number", "minimum": 0, "maximum": 1 }, "replace": { "type": "boolean" } }),
                &["doc"],
            )),
        t("doc_remove_marks", "Remove header & footer, watermark, background or Bates numbering", "Remove every header and footer, watermark, background or Bates number PdfCraft (or a compatible tool) added. Headers and footers with a Bates number are kind bates: header_footer leaves them, bates removes only them. Undoable.")
            .destructive()
            .with(schema(json!({ "doc": doc(), "kind": { "type": "string", "enum": ["header_footer", "watermark", "background", "bates"] } }), &["doc", "kind"])),
        t("doc_export_images", "Export pages as images", "Write pages as PNG, JPEG or TIFF files (`<name>_page_<n>.png|jpg|tif`) into a folder, at a resolution (default 150 dpi). JPEG and TIFF are flattened onto white paper. A page too large for the renderer at that resolution is drawn at the most it allows, listed in `lower_dpi` ({page, dpi}). Includes unsaved edits.")
            .cmd("export.image")
            .with(schema(
                json!({
                    "doc": doc(),
                    "folder": { "type": "string" },
                    "format": { "type": "string", "enum": ["png", "jpeg", "tiff"] },
                    "quality": { "type": "integer", "minimum": 1, "maximum": 100, "description": "JPEG quality (default 85)." },
                    "dpi": { "type": "number", "minimum": 18, "maximum": 1200 },
                    "pages": pages("to export (default: all)"),
                }),
                &["doc", "folder"],
            )),
        t("doc_export_svg", "Export pages as SVG", "Save As ▸ SVG: write pages as standalone SVG files (`<name>_page_<n>.svg`) into a folder, one per page, sized to the page as displayed (crop box after rotation, in points). Vector where possible: paths with fill rule, line width, caps, joins and dashes; clipping; colours converted to sRGB with their opacity; text as glyph outlines (fonts are not embedded) plus a transparent text layer for searching and selecting (text_layer false leaves it out); images as embedded PNG (downsampled above 4 megapixels); nested forms flattened; tiling patterns, soft masks and blend modes kept as SVG patterns, masks and mix-blend-mode. Shadings are rasterised over the region they paint. Each file reports what it holds and `omitted` (content it could not express) and `truncated` (the 128 MiB page limit was reached). transparent leaves out the white paper. Includes unsaved edits. Refused when the document does not allow copying (extracting text and graphics).")
            .with(schema(
                json!({
                    "doc": doc(),
                    "folder": { "type": "string" },
                    "pages": pages("to export (default: all)"),
                    "text_layer": { "type": "boolean", "description": "Add a transparent, selectable text layer (default true)." },
                    "transparent": { "type": "boolean", "description": "Leave the page background transparent instead of white (default false)." },
                }),
                &["doc", "folder"],
            )),
        t("accessibility_check", "Check for accessibility", "Run the Accessibility Checker's full check (32 rules in 7 categories: document, page_content, forms, alternate_text, tables, lists, headings). Each rule is passed, failed (with findings and pages), manual (needs a person) or skipped. Colour contrast is off unless all is true; rules (ids such as tagged-pdf, figures-alt-text) or categories narrow the check; pages limit the page rules.")
            .ro()
            .cmd("a11y.check")
            .with(schema(
                json!({
                    "doc": doc(),
                    "rules": { "type": "array", "items": { "type": "string" } },
                    "categories": { "type": "array", "items": { "type": "string", "enum": ["document", "page_content", "forms", "alternate_text", "tables", "lists", "headings"] } },
                    "all": { "type": "boolean", "description": "Include rules that are off by default (colour contrast)." },
                    "pages": pages("for the page rules (default: all)"),
                }),
                &["doc"],
            )),
        t("accessibility_report", "Accessibility report", "Run the full check and write the accessibility report (HTML) to path; returns the results too. Takes the same options as accessibility_check.")
            .destructive()
            .cmd("a11y.report")
            .with(schema(
                json!({
                    "doc": doc(),
                    "path": { "type": "string" },
                    "rules": { "type": "array", "items": { "type": "string" } },
                    "categories": { "type": "array", "items": { "type": "string" } },
                    "all": { "type": "boolean" },
                    "pages": pages("for the page rules (default: all)"),
                }),
                &["doc", "path"],
            )),
        t("accessibility_fix", "Fix an accessibility problem", "Apply the checker's automatic fix for a rule: primary-language (value: the language, e.g. en-US), title (value: the title; default the current title or file name; also shows it in the title bar) or tab-order (every page tabs in structure order). Returns the rule's new status. Undoable.")
            .with(schema(json!({ "doc": doc(), "rule": { "type": "string", "enum": ["primary-language", "title", "tab-order"] }, "value": { "type": "string" } }), &["doc", "rule"])),
        t("accessibility_figures", "List figures", "The tagged figures (Figure elements, through the role map) in document order: figure number, page, alternate text and where the figure is drawn (top-left-origin points).")
            .ro()
            .cmd("a11y.alt_text")
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("accessibility_set_alt", "Set alternate text", "Set a figure's alternate text (alt; empty or omitted clears it), or mark it decorative (decorative: true: its content becomes an artifact and the figure leaves the tags). figure is a number from accessibility_figures. Returns the figures. Undoable.")
            .cmd("a11y.alt_text")
            .with(schema(json!({ "doc": doc(), "figure": { "type": "integer", "minimum": 1 }, "alt": { "type": "string" }, "decorative": { "type": "boolean" } }), &["doc", "figure"])),
        t("tags_tree", "Show the tags tree", "The logical structure tree (Tags panel), read with cycle and size limits: each element in document order with its 1-based element number, object number (obj, null for an element that can't be edited), parent, depth, type as written and its standard type through the role map and PDF 2.0 namespaces (RoleMapNS), its namespace and the standard type's namespace (URIs; null: the default PDF 1.7 namespace; namespace_missing when /NS leads to no namespace dictionary), title, alt, actual_text, lang, page, and kids (elements, marked content as mcid + page, object references as object + subtype). Also the catalog language, the role map, the namespaces (obj, uri, kind pdf-1.7/pdf-2.0/mathml/other, schema, listed in /Namespaces, role_map), and the types tags_set accepts.")
            .ro()
            .with(schema(json!({ "doc": doc(), "limit": { "type": "integer", "minimum": 1, "maximum": 100000, "description": "At most this many elements (default 5000)." } }), &["doc"])),
        t("tags_mark_artifact", "Change to artifact", "Reading Order / Content panel ▸ Change to Artifact: mark page content as an artifact (running headers and footers, page numbers, rules, backgrounds, watermarks) so assistive technology and accessible-text export skip it. Select it with page plus rect (points from the top-left of the displayed page) and/or mcids (marked-content ids on that page, from tags_tree), or with elements (structure element obj numbers from tags_tree; everything they cover, on any page). Inside rect, tagged marked content changes as a whole when everything it paints is inside (text by where each run starts, paths by their points, images by their boxes), and untagged text runs, paths and images are wrapped one by one, so untagged documents can be marked too. The content is wrapped in /Artifact marked content with the optional type (Pagination, Layout, Page, Background) and subtype (Header, Footer, Watermark; pagination only, implies Pagination); its marked-content references leave the structure tree and the parent tree, and elements left empty leave the tree. q/Q, BT/ET and marked-content nesting are kept. All or nothing, one undo step. Returns marked (page, mcid), untagged (operations wrapped), elements_removed, pages changed and the elements left. mcids and elements need a tagged document; content inside form XObjects and elements holding annotations are refused.")
            .with(schema(
                json!({
                    "doc": doc(),
                    "page": { "type": "integer", "minimum": 1, "description": "The page of rect and mcids (1-based)." },
                    "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4, "description": "[x0, y0, x1, y1] in points from the top-left of the displayed page." },
                    "mcids": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "maxItems": 100000, "description": "Marked-content ids on page." },
                    "elements": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "maxItems": 100000, "description": "Structure element object numbers (obj) from tags_tree." },
                    "type": { "type": "string", "enum": ["Pagination", "Layout", "Page", "Background"] },
                    "subtype": { "type": "string", "enum": ["Header", "Footer", "Watermark"] }
                }),
                &["doc"],
            )),
        t("tags_autotag", "Autotag document", "Accessibility ▸ Autotag Document: give an untagged document a logical structure tree from its layout, as one undo step. Text is grouped into lines and blocks by spacing, alignment, size and weight; text larger than the body size becomes headings (H1 the largest, levels never skip) and short lines wholly in a bold font headings below those; blocks starting with a bullet or number become lists (L > LI > Lbl + LBody) when detect_lists (default true); ruled tables (a grid of at least two rows and two columns of lines) become Table > TR > TH (first row, Scope Column) / TD when detect_tables (default true); images become Figure elements, which all need alternate text (listed in figures_needing_alt: set it with accessibility_set_alt); link annotations become Link elements holding the text under them and an OBJR (and get a description from that text or their address when they have none); other annotations except widgets and popups become Annot elements. Form fields (detect_fields, default true): every widget, radio and checkbox kids included, becomes a Form element with an OBJR to it, a StructParent and a parent-tree entry, placed in reading order next to its label (inside the paragraph, list body or cell holding a label on its left or above it, before a checkbox's or radio button's label on its right, after a heading that labels it; a widget with no label nearby is ordered by its own position); its Alt is the field's tooltip (TU), else its name (T), else the label (a radio button adds its option, as in Payment: Card). Fields without a tooltip are listed in fields_needing_tooltip with one suggested from their label (else their name); field_tooltips fill writes the suggestions as TU (and uses them as Alt), report (default) only lists them. Running headers and footers (the same text at the top or bottom of two or more pages, digits ignored) and page numbers become pagination artifacts, paths and shadings layout and background artifacts. Blocks are ordered by rows and columns (two-column pages read down the left column, then the right). Every page's content is rewritten with marked content (MCIDs) and the parent tree, StructParents, StructParent on annotations, Tabs /S on every tagged page and MarkInfo Marked true are written; lang sets the document language when it has none. A tagged document is refused unless replace_existing is true (then the old tags are discarded and the whole document is tagged; pages must be left out). pages (1-based) limits which pages are tagged. Pages whose content can't be read safely are left untagged and listed in skipped. Returns pages tagged, skipped, counts per structure type, elements, artifacts (marked-content sequences), pagination_artifacts (header, footer and page-number blocks), figures_needing_alt (page, figure obj, rect in points from the top-left), links, link_descriptions, annotations, fields (page, name, kind, widget, field and element obj, rect, alt, alt_source: tooltip/suggested/name/label, label), fields_needing_tooltip (page, name, field obj, suggested, filled), tooltips_filled, lang, replaced, warnings, and before/after summaries (pdfua: conforms and failed checkpoints; checker: failed rules and the status of the tagging rules).")
            .with(schema(
                json!({
                    "doc": doc(),
                    "pages": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "description": "1-based pages to tag (default all)." },
                    "replace_existing": { "type": "boolean", "description": "Discard the existing tags and tag again (default false: a tagged document is refused)." },
                    "lang": { "type": "string", "description": "The document language (for example en-US), set when the document has none." },
                    "detect_tables": { "type": "boolean", "description": "Find ruled tables (default true)." },
                    "detect_lists": { "type": "boolean", "description": "Find bulleted and numbered lists (default true)." },
                    "detect_fields": { "type": "boolean", "description": "Tag form fields: each widget becomes a Form element next to its label (default true)." },
                    "field_tooltips": { "type": "string", "enum": ["report", "fill"], "description": "Fields without a tooltip (TU): report lists them with a suggestion (default); fill also writes the suggestion as their TU." }
                }),
                &["doc"],
            )),
        t("accessibility_make_accessible", "Make accessible", "Accessibility ▸ Make Accessible (the guided action), headless: runs its steps in order and reports each as done, skipped or failed with a reason; a step that fails is reported and the later steps still run. Steps: language (sets /Lang to lang; a document with no language and no lang is refused, the language is never guessed), ocr (auto, the default: recognizes only image-only pages, those that draw an image and have no text, and only when the OCR models are installed, else the step is skipped saying why; always: every page without text, failing without models; never), field_tooltips (an object mapping field names to tooltips, /TU, written before autotagging so the fields' Form elements take them as alternate text; an unknown field is refused), autotag (auto, the default: only an untagged document, a tagged one is left as it is; replace: tag again, discarding the old tags; never; tables, lists and form fields are tagged, fields still without a tooltip are listed with a suggestion), title (title, else the existing title, else the first H1, else the file name; DisplayDocTitle set so it shows in the title bar), alt_text (an object mapping figure object numbers, as listed in needs_review.figures_without_alt or tags_autotag's figures_needing_alt, to alternate text, applied after autotagging; a figure tagged in the same call has the number an earlier run with the same arguments reported, after undoing it), tables (the tagged tables, reported for review), tab_order (every page to the structure order), full_check (the Full Check afterwards). Everything the steps changed is one undo step (Make accessible). Bad arguments change nothing. Returns steps, changed, undo, needs_review (figures_without_alt, untagged_images, fields_without_tooltips, form_fields_tagged, tables_to_review, reading_order to verify, manual_checks) and before/after summaries (checker: failed rules, counts, the tagging rules' status, manual rules; pdfua: conforms and failed checkpoints).")
            .with(schema(
                json!({
                    "doc": doc(),
                    "title": { "type": "string", "description": "The document title (default: the existing title, else the first H1, else the file name)." },
                    "lang": { "type": "string", "description": "The document language, such as en-US; required when the document has none." },
                    "ocr": { "type": "string", "enum": ["auto", "always", "never"], "description": "auto (default): OCR image-only pages when models are installed; always; never." },
                    "autotag": { "type": "string", "enum": ["auto", "replace", "never"], "description": "auto (default): tag only an untagged document; replace: tag again; never." },
                    "alt_text": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Figure object number (as a string key) → alternate text, applied after autotagging." },
                    "field_tooltips": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Field name → tooltip (/TU)." }
                }),
                &["doc"],
            )),
        t("tags_set", "Change tag properties", "Change one structure element (obj from tags_tree): type (a standard structure type, or a custom type the role map maps to one), namespace (a PDF 2.0 structure namespace URI, or pdf-2.0, pdf-1.7 or mathml; added to the structure tree's /Namespaces if missing; empty puts the element back in the default PDF 1.7 namespace), alt, actual_text, lang (a language tag such as en-US) or title; an empty string removes that entry. The type must be standard in the element's namespace or role-mapped from it to a standard type. All or nothing, one undo step. Returns the element.")
            .with(schema(
                json!({
                    "doc": doc(),
                    "obj": { "type": "integer", "minimum": 1, "description": "The element's object number (obj) from tags_tree." },
                    "type": { "type": "string" },
                    "namespace": { "type": "string", "description": "Namespace URI or pdf-2.0 / pdf-1.7 / mathml; empty: the default namespace." },
                    "alt": { "type": "string" },
                    "actual_text": { "type": "string" },
                    "lang": { "type": "string" },
                    "title": { "type": "string" }
                }),
                &["doc", "obj"],
            )),
        t("tags_add_namespace", "Add structure namespace", "PDF 2.0 structure namespaces (ISO 32000-2 §14.7.4): add a namespace (uri, or pdf-2.0, pdf-1.7, mathml) to the structure tree's /Namespaces, or change the one there: its schema (a file name or URL) and role mappings (role_map: [{type, maps_to, namespace}], /RoleMapNS; namespace is the target's URI or short name, omitted for the default namespace; an empty maps_to removes that mapping). Every mapped type must then reach a standard type: circular or dead-end chains are refused, and standard PDF 2.0 types can't be remapped in the PDF 2.0 namespace. Then put elements in it with tags_set namespace. All or nothing, one undo step. Returns the namespace.")
            .with(schema(
                json!({
                    "doc": doc(),
                    "uri": { "type": "string", "description": "Namespace URI or pdf-2.0 / pdf-1.7 / mathml." },
                    "schema": { "type": "string" },
                    "role_map": {
                        "type": "array",
                        "maxItems": 1000,
                        "items": {
                            "type": "object",
                            "properties": { "type": { "type": "string" }, "maps_to": { "type": "string" }, "namespace": { "type": "string" } },
                            "required": ["type", "maps_to"]
                        }
                    }
                }),
                &["doc", "uri"],
            )),
        t("alt_text_list", "List figures needing alternate text", "Set Alternate Text's walk: Figure elements (through the role map) that have no alternate or actual text on themselves or an ancestor (all: true lists every figure), and images painted outside tagged and artifact content (untagged_images: page and 1-based image number). Rects are top-left-origin points. Also the Accessibility Checker's figures-alt-text status.")
            .ro()
            .cmd("a11y.alt_text")
            .with(schema(json!({ "doc": doc(), "all": { "type": "boolean" } }), &["doc"])),
        t("alt_text_set", "Set alternate text", "Give a figure (figure from alt_text_list) alternate text (alt; empty clears it) or mark it decorative (decorative: true); or tag an untagged image (page and image from alt_text_list) as a Figure with alt, adding a structure tree if the document has none. One undo step. Returns alt_text_list.")
            .cmd("a11y.alt_text")
            .with(schema(
                json!({
                    "doc": doc(),
                    "figure": { "type": "integer", "minimum": 1 },
                    "page": { "type": "integer", "minimum": 1 },
                    "image": { "type": "integer", "minimum": 1 },
                    "alt": { "type": "string" },
                    "decorative": { "type": "boolean" }
                }),
                &["doc"],
            )),
        t("form_merge_data", "Merge data files into spreadsheet", "Collect the field values of form data files (FDF, XFDF) or filled-in PDF forms into one CSV file at path: a column per field name, a row per file. Returns the row and column counts.")
            .cmd("form.merge_data")
            .with(schema(json!({ "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1 }, "path": { "type": "string" } }), &["paths", "path"])),
        t("js_run", "Run JavaScript", "Run Acrobat JavaScript in the document, as the JavaScript console does (or as push button `field`'s Mouse Up script when field is given; on a laid-out XFA form, `field` runs that button's XFA click script instead, JavaScript or FormCalc, which can add and remove rows and show or hide subforms). The form object model is available: this/getField, event, app, util, console, display, color, comments (getAnnots, getAnnot, addAnnot), layers (getOCGs), and the document-level scripts. Field, comment and resetForm changes are applied as one undoable step and layers shown or hidden; returns the script's alerts, console output, requests (print, page, url, submit, layer) and error. A dialog that needs an answer (app.alert with buttons, app.response) stops the script with nothing applied and returns `question` (kind, message, title, buttons or default): run it again with the answers so far in `answers`.")
            .cmd("tools.js_console")
            .with(schema(
                json!({
                    "doc": doc(),
                    "script": { "type": "string" },
                    "field": { "type": "string", "description": "Run as this button's Mouse Up event." },
                    "answers": {
                        "type": "array",
                        "description": "Answers to the script's dialogs in the order it asks: \"yes\", \"no\", \"ok\" or \"cancel\" for app.alert; {\"text\": \"…\"} or null (Cancel) for app.response.",
                        "items": { "type": ["string", "object", "null"] }
                    }
                }),
                &["doc", "script"],
            )),
        t("js_document_scripts", "Document JavaScripts", "List the document-level JavaScripts (name and source), which define functions field scripts use.")
            .ro()
            .cmd("tools.document_js")
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("js_set_document_script", "Edit a document JavaScript", "Add or replace the document-level JavaScript `name` with `script`, or delete it (script omitted). Undoable.")
            .cmd("tools.document_js")
            .with(schema(json!({ "doc": doc(), "name": { "type": "string", "minLength": 1 }, "script": { "type": "string" } }), &["doc", "name"])),
        t("js_document_actions", "Document actions", "Set Document Actions: the JavaScript each document trigger runs (will_close, will_save, did_save, will_print, did_print; script null when none). doc_save, doc_print and doc_close run them.")
            .ro()
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("js_set_document_action", "Set a document action", "Set the JavaScript a document trigger runs (will_close, will_save, did_save, will_print, did_print), or remove it (script omitted). Undoable.")
            .with(schema(json!({ "doc": doc(), "trigger": { "type": "string", "enum": ["will_close", "will_save", "did_save", "will_print", "did_print"] }, "script": { "type": "string" } }), &["doc", "trigger"])),
        t("form_set_script", "Set a field's JavaScript", "Field Properties ▸ Run custom script: set field's JavaScript for event keystroke, format, validate or calculate (the field's actions; calculated fields join the calculation order) or mouse_up (a button's action). Omit script to remove it. The scripts use Acrobat's object model (event.value, event.rc, getField, util.printf, …) and run when the field changes. Undoable.")
            .cmd("form.prepare")
            .with(schema(
                json!({ "doc": doc(), "field": { "type": "string" }, "event": { "type": "string", "enum": ["keystroke", "format", "validate", "calculate", "mouse_up"] }, "script": { "type": "string" } }),
                &["doc", "field", "event"],
            )),
        t("doc_export_office", "Export to Word, HTML, RTF or Excel", "Export a PDF ▸ Word (.docx), HTML (.html, one file with images inline), RTF (.rtf) or Excel (.xlsx), chosen by path's extension: paragraphs in reading order, headings from larger text, bold and italic, images where they fall, a page break between pages. Excel gets one worksheet per page: paragraphs in column A, tables detected from the text layout as cell grids, unambiguous numbers (1,234.50, (12.00), 12.5%) stored as numbers. Refused when the document does not allow copying text (open it with the permissions password).")
            .cmd("export.docx")
            .with(schema(json!({ "doc": doc(), "path": { "type": "string" } }), &["doc", "path"])),
        t("doc_export_pptx", "Export to PowerPoint", "Export a PDF ▸ Microsoft PowerPoint (.pptx): one slide per page, the slide size from the first exported page as displayed (crop box after rotation, within PowerPoint's 1–56 in; other page sizes are scaled to fit and centred). mode editable (default): each paragraph becomes an editable text box placed and turned as on the page (font family, size, bold, italic, colour), each extractable image a picture, and the rest of the page (vector art, shadings, inline images, form fields, comments) one background picture rendered at dpi without the text and those images (left out when it is blank). Vector shapes are not rebuilt as PowerPoint shapes. mode image: each slide is one picture of its page. Returns counts (slides, text_boxes, pictures, backgrounds, omitted) and warnings. Includes unsaved edits. Editable mode is refused when the document does not allow copying (open it with the permissions password); image mode only renders.")
            .with(schema(
                json!({
                    "doc": doc(),
                    "path": { "type": "string", "description": "The .pptx file to write." },
                    "pages": pages("to export, in slide order (default: all)"),
                    "mode": { "type": "string", "enum": ["editable", "image"], "description": "editable (default): text boxes, pictures and a background picture; image: one picture per slide." },
                    "dpi": { "type": "number", "minimum": 36, "maximum": 600, "description": "Resolution of the rendered pictures (default 150)." }
                }),
                &["doc", "path"],
            )),
        t("preflight_profiles", "Preflight profiles and checks", "List built-in structural preflight profiles and supported single checks. These checks do not establish full standards conformance.")
            .ro().cmd("preflight.run").with(schema(json!({}), &[])),
        t("preflight_analyze", "Analyze with preflight", "Analyze the working document without edits. Returns supported checks, object/path/page locations, explicit available fixups and a revision binding. No detected issues is not proof of PDF/A, PDF/X or PDF/UA conformance.")
            .ro().cmd("preflight.run").with(schema(json!({"doc":doc(),"profile":{"type":"string","enum":["general","print","pdfa-2b","pdfa-3b"]},"checks":{"type":"array","minItems":1,"maxItems":9,"items":{"type":"string"}}}), &["doc"])),
        t("preflight_fix", "Apply selected preflight fixes", "Explicitly apply findings selected from preflight_analyze using its revision and the same profile/checks. Fixes may change interpolation, annotation printing or interactive behavior, or losslessly recompress LZW. Stale/invalid targets fail atomically. One undo step; no automatic save.")
            .destructive().cmd("preflight.run").with(schema(json!({"doc":doc(),"revision":{"type":"integer","minimum":0},"profile":{"type":"string","enum":["general","print","pdfa-2b","pdfa-3b"]},"checks":{"type":"array","minItems":1,"maxItems":9,"items":{"type":"string"}},"findings":{"type":"array","minItems":1,"maxItems":1000,"uniqueItems":true,"items":{"type":"integer","minimum":1}}}), &["doc","revision","findings"])),
        t("preflight_report", "Export preflight report", "Write a structural preflight report as text or JSON, atomically replacing path inside --root. Document stays unchanged; reports explain coverage limits.")
            .destructive().cmd("preflight.run").with(schema(json!({"doc":doc(),"profile":{"type":"string","enum":["general","print","pdfa-2b","pdfa-3b"]},"checks":{"type":"array","minItems":1,"maxItems":9,"items":{"type":"string"}},"path":{"type":"string"},"format":{"type":"string","enum":["text","json"]}}), &["doc","path"])),
        t("pdfa_verify", "Check PDF/A structure", "Run the implemented subset of the PDF/A checks (ISO 19005-1/-2/-3) for a level: 1a, 1b, 2a, 2b, 2u, 3a, 3b or 3u (default: the level the document declares in pdfaid:part/conformance, else 2b; level_source says which). Level b: identification, encryption, output intent, fonts embedded, annotations, actions, images, embedded files (PDF/A-1 also transparency and optional content). Levels u and a add Unicode for every glyph shown (ToUnicode, or a predefined encoding or known glyph names; no U+0000/U+FEFF/U+FFFE/U+FFFD; Private Use Area values warn). Level a adds MarkInfo Marked, a structure tree, all content tagged or artifact, standard types or a role map, Lang, figure alternate text, and warnings for formulas without Alt and word breaks made by positioning. Each issue has its ISO 19005 clause, severity (error or warning), message, 1-based page and whether pdfa_convert can fix it; no_detected_issues means no errors. The legacy compliant boolean is conservatively false: validation_complete is false and conformance is not established even with no detected issues.")
            .ro()
            .cmd("standards.pdfa")
            .with(schema(
                json!({ "doc": doc(), "level": { "type": "string", "enum": ["1a", "1b", "2a", "2b", "2u", "3a", "3b", "3u"], "description": "Default: the declared level, else 2b." } }),
                &["doc"],
            )),
        t("pdfua_verify", "Verify PDF/UA", "Verify the machine-checkable requirements of PDF/UA-1 (ISO 14289-1, part 1, the default) or PDF/UA-2 (ISO 14289-2, part 2), reported by Matterhorn Protocol checkpoint (06-002, 28-011, …) or a short code where none applies (struct-tree-root, fonts-embedded, ua2-namespace): identification and dc:title in XMP, DisplayDocTitle, MarkInfo, the structure tree (loops, role mapping, namespaces, tables, lists, headings, figure and formula alternate text, notes), content tagged or artifact with every MCID in the structure and parent tree, fonts embedded and mapped to Unicode, language, annotations and fields (tags, Contents, TU, Tabs), optional content, embedded files, XFA, encryption, reference and PostScript XObjects. conforms is true when no checked requirement fails; checkpoints that need a person are listed in not_checked and are not part of it. counts has every checked checkpoint; findings keep at most max_findings per checkpoint (default 100), with 1-based page and object number.")
            .ro()
            .with(schema(
                json!({
                    "doc": doc(),
                    "part": { "type": "integer", "enum": [1, 2], "description": "1 for PDF/UA-1 (default), 2 for PDF/UA-2." },
                    "max_findings": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "Findings kept per checkpoint (default 100); counts include the rest." },
                }),
                &["doc"],
            )),
        t("doc_validate_arlington", "Validate against the Arlington model", "Structural validation against the Arlington PDF Model (PDF Association; the vendored model's commit is in the result): walks the object graph from the trailer, works out each object's Arlington type from the links of the key that reaches it (choosing among alternatives such as page or page tree node, or the action kinds, by the object's /Type, /Subtype, /S and keys), and checks it. Codes: missing-required (error; inheritable keys are also looked up through /Parent), wrong-type (error), value-not-allowed (error: outside the possible values or failing their conditions such as /Rotate a multiple of 90), introduced-later (warning: key, type or value newer than the PDF version), deprecated (warning), must-be-indirect / must-be-direct (warning), unknown-key and extra-element (info), limit (info: the walk stopped at a bound). Each finding has the object number holding the value (null for the trailer), path (e.g. Trailer/Root/Pages/Kids[0]/Resources/Font/F1/BaseFont), key, object_type and message. version (1.0 to 1.7 or 2.0) checks against that PDF version instead of the document's (the header, or the catalog /Version when later). Findings kept: errors first, then warnings, then information, at most max_findings (default 1000); errors/warnings/infos count all. Predicates evaluated are listed in predicates_evaluated; rules that need any other predicate (and the SpecialCase column) are not reported as problems but counted in not_evaluated. valid is true when no error is found. Includes unsaved edits. Read-only.")
            .ro()
            .with(schema(
                json!({
                    "doc": doc(),
                    "version": { "type": "string", "description": "PDF version to check against (1.0 to 1.7 or 2.0). Default: the document's." },
                    "max_findings": { "type": "integer", "minimum": 1, "maximum": 100000, "description": "Findings kept (default 1000); the counts include the rest." },
                }),
                &["doc"],
            )),
        t("pdfx_verify", "Verify PDF/X", "Verify the machine-checkable requirements of PDF/X-1a:2001, PDF/X-1a:2003, PDF/X-3:2002, PDF/X-3:2003 or PDF/X-4 (ISO 15930-1/-4/-3/-6/-7), like Acrobat Preflight's PDF/X profiles. standard defaults to the one the document declares (GTS_PDFXVersion), else X-4. Rules by id: GTS_PDFXVersion (document information; XMP pdfxid for X-4), PDF version limit, information and XMP keys, Trapped True/False, file ID, encryption, exactly one GTS_PDFX output intent with OutputConditionIdentifier and an ICC output (prtr) profile or RegistryName, TrimBox or ArtBox on every page and box nesting, printing annotations in the bleed area, fonts embedded, X-1a colour (CMYK, gray, spot only), DeviceRGB/DeviceCMYK against the output intent for X-3/X-4, no transparency for X-1a/X-3, X-4 optional content names, transfer functions, halftone types, PostScript, OPI, external content, embedded files, JavaScript. Severity error breaks conformance; warning marks a rule whose wording we are not certain of or a limit reached. conforms is true when no error is found; not_checked lists what isn't decided. counts has every checked rule; findings keep at most max_findings per rule (default 100), with 1-based page and object number.")
            .ro()
            .with(schema(
                json!({
                    "doc": doc(),
                    "standard": { "type": "string", "enum": ["X-1a:2001", "X-1a:2003", "X-3:2002", "X-3:2003", "X-4"], "description": "Default: the declared standard, else X-4." },
                    "max_findings": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "Findings kept per rule (default 100); counts include the rest." },
                }),
                &["doc"],
            )),
        t("pdfx_convert", "Convert to PDF/X", "Preflight ▸ Convert to PDF/X: fix the document for level x1a (PDF/X-1a:2003), x3 (PDF/X-3:2003) or x4 (PDF/X-4); X-1a:2001 and X-3:2002 are accepted too. As one undoable edit: encryption, JavaScript and additional actions removed; annotations that don't print removed (links kept, not printing) and printing annotations and form fields flattened into the page, then the form removed; transfer functions, bad halftones and OPI removed; unembedded standard-14 fonts embedded (the renderer's BSD-licensed substitute programs, with standard widths; other fonts can't be embedded); X-1a and X-3: transparency flattened (flatten_preset low/medium/high, default medium); colour converted to the output intent (X-1a: RGB, ICC, calibrated and Lab colour to CMYK, or gray with a gray profile; X-3/X-4: DeviceRGB unless the intent is RGB), through the profile's tables when it has them, else a built-in approximation (notes says which); the one GTS_PDFX output intent (output_condition: a registered condition such as FOGRA39, and/or profile: an ICC output (prtr) profile file; X-4 needs the profile; without either the document's own intent is kept); TrimBox (trim, default the page's TrimBox, ArtBox, CropBox or MediaBox) and BleedBox (bleed, default BleedBox or CropBox, always containing the trim) in points of default user space; the PDF version (1.3/1.4/1.6; a lower version is saved in full); GTS_PDFXVersion, Title (title, default existing or Untitled), CreationDate, ModDate, Trapped False; XMP for X-4; file ID. Then pdfx_verify runs again: before and after hold both results and unfixable the errors left. When the result wouldn't conform nothing changes (converted false, refused says why) unless allow_partial. Converting a conforming document again changes nothing (changed false). Bad arguments are errors. Save the document to keep it.")
            .with(schema(
                json!({
                    "doc": doc(),
                    "level": { "type": "string", "description": "x1a, x3 or x4 (or X-1a:2001, X-1a:2003, X-3:2002, X-3:2003, X-4)." },
                    "output_condition": { "type": "string", "description": "OutputConditionIdentifier: a registered printing condition (FOGRA39, CGATS TR 001, …) or a name for the profile's condition." },
                    "output_info": { "type": "string", "description": "The output intent's Info (default: output_condition)." },
                    "profile": { "type": "string", "description": "Path of an ICC output (prtr) profile for DestOutputProfile, CMYK or gray (RGB for X-3/X-4)." },
                    "trim": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4, "description": "TrimBox [left, bottom, right, top] in points for every page." },
                    "bleed": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4, "description": "BleedBox [left, bottom, right, top] in points for every page; must contain trim." },
                    "title": { "type": "string", "description": "Title written when the document has none." },
                    "flatten_preset": { "type": "string", "enum": ["low", "medium", "high"], "description": "Transparency flattening resolution for X-1a and X-3 (150, 300, 600 ppi)." },
                    "allow_partial": { "type": "boolean", "description": "Convert even when the result won't conform (default false)." },
                }),
                &["doc", "level"],
            )),
        t("pdfa_convert", "Save as PDF/A", "Standards ▸ Save as PDF/A: fix what can be fixed for a PDF/A level (1a, 1b, 2a, 2b, 2u, 3a, 3b, 3u; default the declared level, else 2b): XMP identification (pdfaid:part and pdfaid:conformance) and metadata, an sRGB output intent, forbidden actions and JavaScript, annotation print flags, image interpolation, encryption; for levels u and a, ToUnicode maps written from fonts' encodings where a used glyph has no or a bad mapping. Levels u and a refuse when some glyph's meaning can't be derived, and level a refuses unless the document is already tagged well enough (it never tags automatically): then converted is false, nothing changes, and blocking lists the findings with a hint. Otherwise converted is true and the result is pdfa_verify's for the level (what remains, e.g. fonts that aren't embedded). Undoable; save the document to keep it.")
            .cmd("standards.pdfa")
            .with(schema(json!({ "doc": doc(), "level": { "type": "string", "enum": ["1a", "1b", "2a", "2b", "2u", "3a", "3b", "3u"] } }), &["doc"])),
        t("action_list", "List actions", "Action Wizard: the built-in actions (name, description, steps) and every step an action can use (id, label, whether it takes a text argument).")
            .ro()
            .cmd("actions.wizard")
            .with(schema(json!({}), &[])),
        t("action_run", "Run an action", "Action Wizard: run a built-in action (action: its name) or a list of steps ([{step, arg}], ids from action_list) on each PDF in paths, writing the results into folder under the same names. Returns each file's step log or error.")
            .cmd("actions.wizard")
            .with(schema(
                json!({
                    "action": { "type": "string" },
                    "steps": { "type": "array", "items": { "type": "object", "properties": { "step": { "type": "string" }, "arg": { "type": "string" } }, "required": ["step"] } },
                    "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                    "folder": { "type": "string" },
                }),
                &["paths", "folder"],
            )),
        t("doc_compare", "Compare files", "Compare two open documents: other is the older version, doc the newer. Returns the page map (each page same, modified, moved, inserted or deleted, 1-based old and new numbers), summary counts and each text change (replaced, inserted, deleted) with the old and new text, pages (1-based) and rectangles (points, origin bottom-left); with visual, the paired pages that look different (changed_percent, regions) and visual_regions as flat {page, rect}. Moved pages are compared with their counterpart.")
            .ro()
            .cmd("doc.compare")
            .with(schema(compare_props(json!({ "doc": doc(), "other": { "type": "integer", "description": "The older document (from doc_open)." }, "visual": { "type": "boolean", "description": "Also compare how the paired pages look (default false)." } })), &["doc", "other"])),
        t("compare_files", "Compare two files", "Compare Files on two PDF files (old = the older version, new = the newer): the page map (each page same, modified, moved, inserted or deleted; 1-based old and new numbers), word-level text changes with pages and rectangles (points, origin bottom-left), and pages that look different (rendered at dpi; a page counts when more than threshold of its pixels differ by more than tolerance). Optionally writes the full JSON report to json_path, a summary PDF to report_path and a copy of the new file with every difference marked as comments to overlay_path. The files are opened only for the comparison.")
            .cmd("doc.compare")
            .with(schema(
                compare_props(json!({
                    "old": path_arg(),
                    "new": path_arg(),
                    "old_password": { "type": "string" },
                    "new_password": { "type": "string" },
                    "visual": { "type": "boolean", "description": "Also compare how the paired pages look (default true)." },
                    "json_path": { "type": "string", "description": "A file path (relative to --root when set) for the full JSON report (every change)." },
                    "report_path": { "type": "string", "description": "A file path (relative to --root when set) for the summary report PDF." },
                    "overlay_path": { "type": "string", "description": "A file path (relative to --root when set) for a copy of the new file with the differences marked as comments." },
                })),
                &["old", "new"],
            )),
        t("doc_compare_report", "Compare report", "Write the compare summary report (a PDF listing every change) to path. Refused unless both documents allow copying text.")
            .cmd("doc.compare")
            .with(schema(json!({ "doc": doc(), "other": { "type": "integer" }, "path": { "type": "string" } }), &["doc", "other", "path"])),
        t("doc_compare_mark", "Mark differences as comments", "Add the differences from other (older) to doc (newer) as comments in doc: highlights over replaced (blue) and inserted (green) text, notes where text was deleted (red), authored Compare. Undoable. Refused unless both documents allow copying text.")
            .cmd("doc.compare")
            .with(schema(json!({ "doc": doc(), "other": { "type": "integer" } }), &["doc", "other"])),
        t("form_detect_fields", "Detect form fields", "Prepare a form ▸ automatic field detection: find the blanks a printed form asks to be filled (underscore runs, lines, empty boxes, small squares for check boxes) and name each field from its label. With add (default true) the fields are created as one undoable step; otherwise they are only proposed. Returns page (1-based), kind, name and rect (points, origin bottom-left).")
            .cmd("form.detect")
            .with(schema(json!({ "doc": doc(), "pages": pages("to look at (default: all)"), "add": { "type": "boolean" } }), &["doc"])),
        t("form_actions", "Field actions", "Field Properties ▸ Actions: the field's actions, each with its trigger (mouse_up, mouse_down, mouse_enter, mouse_exit, on_focus, on_blur), in the order they run (a trigger's later actions follow its /Next chain). Actions PdfCraft doesn't edit are listed as other (their /S type).")
            .ro()
            .cmd("form.prepare")
            .with(schema(json!({ "doc": doc(), "field": { "type": "string" } }), &["doc", "field"])),
        t("form_set_actions", "Set field actions", "Field Properties ▸ Actions: replace the field's actions (add, edit, reorder or remove by passing the whole list, e.g. form_actions' list changed). Each item: trigger (mouse_up, mouse_down, mouse_enter, mouse_exit, on_focus, on_blur) and one of javascript (source), url, reset (field names; [] for all), menu (Print, NextPage, PrevPage, FirstPage, LastPage), page (1-based), show / hide (field names), submit (URL), or other (an /S type form_actions listed: keeps that existing action). Several items with one trigger run in the order given (linked through /Next). An action equal to an existing one keeps its original data; unchanged triggers are left as they are; triggers not listed lose their actions. Refused without changes if any item is invalid. Undoable.")
            .cmd("form.prepare")
            .with(schema(
                json!({ "doc": doc(), "field": { "type": "string" }, "actions": { "type": "array", "items": { "type": "object", "properties": {
                    "trigger": { "type": "string", "enum": ["mouse_up", "mouse_down", "mouse_enter", "mouse_exit", "on_focus", "on_blur"] },
                    "javascript": { "type": "string" }, "url": { "type": "string" }, "reset": { "type": "array", "items": { "type": "string" } },
                    "menu": { "type": "string" }, "page": { "type": "integer", "minimum": 1 }, "show": { "type": "array", "items": { "type": "string" } },
                    "hide": { "type": "array", "items": { "type": "string" } }, "submit": { "type": "string" }, "other": { "type": "string" } }, "required": ["trigger"] } } }),
                &["doc", "field", "actions"],
            )),
        t("form_run_trigger", "Run a field trigger", "Fire a field's trigger (mouse_up, mouse_down, mouse_enter, mouse_exit, on_focus, on_blur) as the viewer would: its actions run in order. JavaScript runs with that event's name (needs JavaScript on); reset and show/hide change the form (undoable); pages, links, print, submits and menu items are only returned as requests, never performed. Returns ran, skipped (actions not run), alerts, console, errors, requests and menu.")
            .cmd("form.prepare")
            .with(schema(
                json!({ "doc": doc(), "field": { "type": "string" }, "trigger": { "type": "string", "enum": ["mouse_up", "mouse_down", "mouse_enter", "mouse_exit", "on_focus", "on_blur"] } }),
                &["doc", "field", "trigger"],
            )),
        t("edit_keep_tags", "Keep tags on edit", "Keep a tagged document's structure tree in step with content edits (on by default), as Acrobat does: text edited in place keeps its marked content and element (an ActualText that spelled the old words is removed); a rewritten paragraph, or an added item that is moved, retyped or replaced, takes over the tag of the content it replaced; new text and images get new P and Figure elements next to the nearest tagged content (figures need alternate text: see accessibility_set_alt); new underlines become artifacts; deleted content, images and pages leave the tree, elements left empty go, and the parent tree is rebuilt, all in the edit's one undo step. The results of text_edit, image_edit, page_add_text, page_add_image, content_update, content_delete and page_delete then have a tags entry: pages, kept, adopted, added (page, element, role, mcid), artifacts, removed, pruned, actual_text_cleared, review_alt (figures whose image changed: check their alternate text), parent_tree and warnings. Set it with enabled, or read it; off, edits leave the tree as it was.")
            .with(schema(json!({ "enabled": { "type": "boolean" } }), &[])),
        t("js_enabled", "JavaScript on or off", "Preferences ▸ JavaScript ▸ Enable Acrobat JavaScript: set it with `enabled`, or read it. With JavaScript off, field scripts other than Acrobat's AF calls don't run.")
            .with(schema(json!({ "enabled": { "type": "boolean" } }), &[])),
        t("ocr_recognize", "Recognize text (OCR)", "Scan & OCR ▸ Recognize text: render pages, read the words in them and add them as invisible text over the page image, so scanned pages become searchable and selectable (Searchable Image (Exact) by default; output straighten also turns skewed or turned pages straight; output editable replaces the recognized text with real, visible, editable text and cleans it out of the image). Pages that already have text are skipped unless skip_text_pages is false. Returns each page's recognised text, word count and the orientation corrected for reading (auto_rotate/deskew), or why it was skipped. Needs the OCR models (ocr_status) and permission both to copy text and to change the document (checked before any page is read). Undoable as one step.")
            .cmd("ocr.recognize")
            .with(schema(
                json!({
                    "doc": doc(),
                    "pages": pages("to recognise (default: all)"),
                    "dpi": { "type": "number", "minimum": 72, "maximum": 600, "description": "Resolution pages are read at (default 300; a scanned page is read at most at its own resolution)." },
                    "language": { "type": "string", "enum": ["en"], "description": "Document language (default en)." },
                    "skip_text_pages": { "type": "boolean", "description": "Leave pages that already contain text alone (default true)." },
                    "auto_rotate": { "type": "boolean", "description": "Detect pages turned by 90, 180 or 270 degrees and read them upright (default false). Detection compares recognition of reduced copies; the page image is not changed and the text is placed over the original words." },
                    "deskew": { "type": "boolean", "description": "Detect skew of up to 15 degrees and read the page straightened (default false). The page image is not changed unless output is straighten." },
                    "output": { "type": "string", "enum": ["exact", "straighten", "editable"], "description": "exact (default): Searchable Image (Exact), the page is not changed. straighten: Searchable Image, pages found turned or skewed by auto_rotate/deskew are also turned straight (content and /Rotate), and the recognized pages' images finer than dpi are downsampled to it. editable: Editable Text and Images, recognized lines become visible text in a matching standard font (serif, sans or mono, regular or bold), size and colour, fitted to the line's width with horizontal scaling; the text areas are filled with the local paper colour in the page image, which stays as the background with everything else (pictures, rules) intact; words below min_confidence stay as image with invisible text over them. Each page reports its lines, fonts and low-confidence words (editable)." },
                    "min_confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "With output editable: words the recognizer is less sure of than this stay as image with invisible text (default 0.5). The bundled recognizer reports no confidence, so then words whose text doesn't look like a word are kept as image instead; 0 converts every word." },
                    "despeckle": { "type": "string", "enum": ["off", "low", "medium", "high"], "description": "Remove isolated dark specks before reading (default off): low up to 0.13 mm, medium 0.21 mm, high 0.34 mm across, sized by the page's resolution; specks within about 1 mm of text (periods, i-dots) are kept. The page image changes only with output straighten." },
                    "despeckle_light": { "type": "boolean", "description": "With despeckle: also fill small light holes inside solid ink (default false)." },
                    "despeckle_median": { "type": "boolean", "description": "Also replace single-pixel impulses in gray/colour scans by their 3x3 median (default false; ignores the punctuation rule)." },
                    "background": { "type": "string", "enum": ["off", "low", "medium", "high"], "description": "Whiten the off-white, grey or yellowed paper before reading (default off): the paper level is estimated from the page's luma histogram and pixels within a margin of it (low, medium, high reach further) become white; ink and mid-greys keep their values. Runs before despeckle. The page image changes only with output straighten." },
                    "background_white_point": { "type": "integer", "minimum": 128, "maximum": 255, "description": "With background (or alone): an explicit white point instead of the estimate; pixels at least this light become white." },
                    "camera": { "type": "boolean", "description": "First correct camera photos (as scan_camera_image with its defaults, but only on pages where a photographed page's outline is found): the page becomes the straightened, enhanced page. Default false. The correction and the recognized text undo as one step; the result lists what was corrected under camera." },
                }),
                &["doc"],
            )),
        t("ocr_recognize_files", "Recognize text in multiple files", "Scan & OCR ▸ Recognize text ▸ In multiple files: read every page of each PDF in paths and write the searchable result into folder under the same name (pages that already have text are left alone). Returns, per file, the output path, word count and skipped pages, or the error.")
            .cmd("ocr.recognize_batch")
            .with(schema(
                json!({
                    "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                    "folder": { "type": "string" },
                    "dpi": { "type": "number", "minimum": 72, "maximum": 600 },
                    "language": { "type": "string", "enum": ["en"] },
                    "auto_rotate": { "type": "boolean", "description": "Detect pages turned by 90, 180 or 270 degrees and read them upright (default false). Detection compares recognition of reduced copies; the page image is not changed and the text is placed over the original words." },
                    "deskew": { "type": "boolean", "description": "Detect skew of up to 15 degrees and read the page straightened (default false). The page image is not changed unless output is straighten." },
                    "output": { "type": "string", "enum": ["exact", "straighten", "editable"], "description": "exact (default): Searchable Image (Exact), the page is not changed. straighten: Searchable Image, pages found turned or skewed by auto_rotate/deskew are also turned straight (content and /Rotate), and the recognized pages' images finer than dpi are downsampled to it. editable: Editable Text and Images, recognized lines become visible text in a matching standard font (serif, sans or mono, regular or bold), size and colour, fitted to the line's width with horizontal scaling; the text areas are filled with the local paper colour in the page image, which stays as the background with everything else (pictures, rules) intact; words below min_confidence stay as image with invisible text over them. Each page reports its lines, fonts and low-confidence words (editable)." },
                    "min_confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "With output editable: words the recognizer is less sure of than this stay as image with invisible text (default 0.5). The bundled recognizer reports no confidence, so then words whose text doesn't look like a word are kept as image instead; 0 converts every word." },
                    "despeckle": { "type": "string", "enum": ["off", "low", "medium", "high"], "description": "Remove isolated dark specks before reading (default off): low up to 0.13 mm, medium 0.21 mm, high 0.34 mm across, sized by the page's resolution; specks within about 1 mm of text (periods, i-dots) are kept. The page image changes only with output straighten." },
                    "despeckle_light": { "type": "boolean", "description": "With despeckle: also fill small light holes inside solid ink (default false)." },
                    "despeckle_median": { "type": "boolean", "description": "Also replace single-pixel impulses in gray/colour scans by their 3x3 median (default false; ignores the punctuation rule)." },
                    "background": { "type": "string", "enum": ["off", "low", "medium", "high"], "description": "Whiten the off-white, grey or yellowed paper before reading (default off): the paper level is estimated from the page's luma histogram and pixels within a margin of it (low, medium, high reach further) become white; ink and mid-greys keep their values. Runs before despeckle. The page image changes only with output straighten." },
                    "background_white_point": { "type": "integer", "minimum": 128, "maximum": 255, "description": "With background (or alone): an explicit white point instead of the estimate; pixels at least this light become white." },
                }),
                &["paths", "folder"],
            )),
        t("scan_remove_background", "Remove scan background", "Scan & OCR ▸ Enhance scan ▸ Remove background: whiten the off-white, grey or yellowed paper of the images the pages draw, as one undoable step. Each image's paper level is the main light peak of its luma histogram (at least 30 % of the pixels within 16 levels of it; photographs, dark and inverted scans are left alone); pixels within a margin below it (low, medium (default) or high, widened by the paper's measured noise; never below 128) become white, and a levels curve keeps ink and mid-greys at their values. Colour scans are neutralised per channel from the paper's own colour. 8-bit gray and RGB images are rewritten (JPEG at quality 90 when they were JPEG); one-bit images already have white paper and are counted in images_unchanged; JPEG 2000, CMYK and other images it cannot decode faithfully are left alone and counted in images_skipped. Images shared with other pages change there too. Returns images, images_changed, images_skipped, images_unchanged, pixels_whitened and pixels_changed; when nothing changed no step is added.")
            .with(schema(
                json!({
                    "doc": doc(),
                    "pages": pages("to clean (default: all)"),
                    "strength": { "type": "string", "enum": ["off", "low", "medium", "high"], "description": "How far below the paper level pixels become white (default medium, or off when only white_point is given)." },
                    "white_point": { "type": "integer", "minimum": 128, "maximum": 255, "description": "An explicit white point instead of the estimate: pixels at least this light (luma) become white." },
                }),
                &["doc"],
            )),
        t("scan_camera_image", "Enhance camera images", "Scan & OCR ▸ Enhance ▸ Camera Image: for a page that is a photo of a document (a skewed page against a background), find the page's outline in the largest image the page draws (edge detection, line fitting and the best-supported convex quadrilateral, refined to sub-pixel at full resolution), correct the perspective to an upright rectangle at the page's estimated aspect ratio (single-view rectangle geometry, bilinear resampling), and even out the lighting (deshadow, white balance, contrast stretch, paper whitening). The page is replaced by the corrected image on a page of that aspect ratio whose longer side is the old page's (other page content, page boxes and rotation are dropped; annotations are kept), as one undoable step for all pages. Returns per page found, confidence (0-1), corners (points from the top-left of the displayed page, top-left/top-right/bottom-right/bottom-left), image_corners (photo pixels), image_size, aspect (width/height), output_size (pixels) and page_size (points), or skipped with the reason (no image, an image it cannot decode: only 8-bit gray/RGB JPEG or Flate). Where no outline is found (confidence below 0.45) the whole photo is only enhanced, or skipped with enhance false; give corners to set the outline yourself. dry_run reports without changing anything.")
            .with(schema(
                json!({
                    "doc": doc(),
                    "pages": pages("to correct (default: all)"),
                    "corners": { "type": "array", "items": point(), "minItems": 4, "maxItems": 4, "description": "The page's four corners in the photo, in points from the top-left of the displayed page, in any order, instead of detecting them (applies to every page given). They must outline a convex quadrilateral covering at least 1 % of the photo." },
                    "enhance": { "type": "boolean", "description": "Even out lighting and shadows, neutralise the paper colour, stretch contrast and whiten the paper (default true)." },
                    "dry_run": { "type": "boolean", "description": "Only report what would be found and done (default false)." },
                }),
                &["doc"],
            )),
        t("scan_despeckle", "Despeckle scanned pages", "Scan & OCR ▸ Enhance scan ▸ Despeckle: remove isolated dark specks (scanner dust, noise) from the images the pages draw, as one undoable step. Each image is sized by its effective resolution where it is drawn: level low removes specks up to 0.13 mm across, medium (default) 0.21 mm, high 0.34 mm; a speck within about 1 mm of text-sized ink is kept, so periods, commas and i-dots survive. One-bit scans (CCITT fax, Flate, LZW) stay one-bit; 8-bit gray and RGB images are rewritten (JPEG at quality 90 when they were JPEG). JBIG2, JPEG 2000, CMYK and other images it cannot decode faithfully are left alone and counted in images_skipped. Images shared with other pages change there too. Returns images, images_changed, images_skipped, dark_specks, light_specks, impulses and pixels_changed; when nothing changed no step is added.")
            .with(schema(
                json!({
                    "doc": doc(),
                    "pages": pages("to despeckle (default: all)"),
                    "level": { "type": "string", "enum": ["off", "low", "medium", "high"], "description": "Largest speck removed (default medium); off with median true only median-filters." },
                    "light": { "type": "boolean", "description": "Also fill small light holes inside solid ink (default false)." },
                    "median": { "type": "boolean", "description": "Also replace single-pixel impulses in gray/colour images by their 3x3 median (default false; ignores the punctuation rule)." },
                }),
                &["doc"],
            )),
        t("ocr_status", "OCR status", "Whether text recognition is available (its models are installed: release packages include them; a source build fetches them with `cargo xtask models`; PDFCRAFT_MODELS overrides), where it looks for them, and the languages it reads.")
            .ro()
            .cmd("ocr.recognize")
            .with(schema(json!({}), &[])),
        t("ocr_prepare", "Recognize text for review", "Recognize pages into a draft without editing the PDF. Use ocr_review/ocr_correct, then ocr_apply or ocr_cancel. Needs permission both to copy text and to change the document (a restricted encrypted file must be opened with its permissions password); this is checked before any page is read. Drafts are bound to the document version; intervening edits require recognition again. At most eight drafts, two million words and 128 MiB of text are held at once. English recognition only; the original image is preserved.")
            .ro().cmd("ocr.recognize").with(schema(json!({
                "doc": doc(), "pages": pages("to recognize (default all)"),
                "dpi": {"type":"number", "minimum":72, "maximum":600},
                "language": {"type":"string", "enum":["en"]},
                "skip_text_pages": {"type":"boolean"},
                "auto_rotate": {"type":"boolean", "description":"Detect 90/180/270-degree page turns and read pages upright (default false); the image is not changed."},
                "deskew": {"type":"boolean", "description":"Detect skew up to 15 degrees and read pages straightened (default false); the image is not changed unless output is straighten."},
                "output": { "type": "string", "enum": ["exact", "straighten", "editable"], "description": "exact (default): Searchable Image (Exact), the page is not changed. straighten: Searchable Image, pages found turned or skewed by auto_rotate/deskew are also turned straight (content and /Rotate), and the recognized pages' images finer than dpi are downsampled to it. editable: Editable Text and Images, recognized lines become visible text in a matching standard font (serif, sans or mono, regular or bold), size and colour, fitted to the line's width with horizontal scaling; the text areas are filled with the local paper colour in the page image, which stays as the background with everything else (pictures, rules) intact; words below min_confidence stay as image with invisible text over them. Each page reports its lines, fonts and low-confidence words (editable)." },
                    "min_confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "With output editable: words the recognizer is less sure of than this stay as image with invisible text (default 0.5). The bundled recognizer reports no confidence, so then words whose text doesn't look like a word are kept as image instead; 0 converts every word." },
                "despeckle": {"type":"string", "enum":["off", "low", "medium", "high"], "description":"Remove isolated dark specks before reading (default off); with output straighten the page images are despeckled too."},
                "despeckle_light": {"type":"boolean", "description":"With despeckle: also fill small light holes inside solid ink (default false)."},
                "despeckle_median": {"type":"boolean", "description":"Also median-filter single-pixel impulses in gray/colour scans (default false)."},
                "background": {"type":"string", "enum":["off", "low", "medium", "high"], "description":"Whiten the paper background before reading (default off); with output straighten the page images get a white background too."},
                "background_white_point": {"type":"integer", "minimum":128, "maximum":255, "description":"An explicit white point for background removal instead of the estimate."}
            }), &["doc"])),
        t("ocr_review", "Inspect recognized text draft", "List recognized words, page coordinates and skipped-page reasons. Does not change the PDF. If the document changed, was closed or no longer allows copying and changes, returns an error and discards the draft instead of its words.")
            .ro().cmd("ocr.recognize").with(schema(json!({"doc":doc()}), &["doc"])),
        t("ocr_correct", "Correct a recognized word", "Change one word in a recognition draft, preserving its box. Page and word are 1-based indexes from ocr_review. Empty text omits a word. Current searchable text supports WinAnsi characters; other characters are rejected without changing the draft.")
            .cmd("ocr.recognize").with(schema(json!({"doc":doc(), "page":{"type":"integer", "minimum":1}, "word":{"type":"integer", "minimum":1}, "text":{"type":"string", "maxLength":16384}}), &["doc","page","word","text"])),
        t("ocr_apply", "Apply recognized text draft", "Add reviewed words as invisible searchable text in one undoable edit. Refuses (and discards the draft for) a changed, closed or restricted document. Preserves original page images and removes the draft after success.")
            .cmd("ocr.recognize").with(schema(json!({"doc":doc()}), &["doc"])),
        t("ocr_cancel", "Discard recognized text draft", "Discard the pending recognition draft without changing document bytes or history.")
            .cmd("ocr.recognize").with(schema(json!({"doc":doc()}), &["doc"])),
        t("doc_export_all_images", "Export all images", "Write the images that pages use into a folder (`<name>_Page_<n>_Image_<k>.jpg|png`), each once: JPEG images unchanged, others as PNG with their soft mask as alpha. min_size skips images with fewer pixels on their shorter side. Images that can't be decoded yet (JPEG 2000, JBIG2, CCITT, separations) are listed under skipped. Includes unsaved edits. Refused when the document does not allow copying (extracting graphics); doc_export_images renders pages and stays available.")
            .cmd("export.all_images")
            .with(schema(
                json!({
                    "doc": doc(),
                    "folder": { "type": "string" },
                    "pages": pages("whose images to export (default: all)"),
                    "min_size": { "type": "integer", "minimum": 0, "description": "Skip images smaller than this many pixels on their shorter side (default 0)." },
                }),
                &["doc", "folder"],
            )),
        t("doc_export_text", "Export text", "Write the reading-order text of pages to a .txt file (pages separated by form feeds). Refused when the document does not allow copying text (open it with the permissions password).")
            .cmd("export.text")
            .with(schema(json!({ "doc": doc(), "path": { "type": "string" }, "pages": pages("to export (default: all)") }), &["doc", "path"])),
        t("doc_export_accessible_text", "Export accessible text", "Save As ▸ Accessible Text: write a tagged PDF's text in logical structure order, as a screen reader reads it, to a .txt file (atomically replacing path). Each structure element gives its /ActualText, else its /Alt (a figure's alternate text), else the text of the marked content it covers; paragraphs, headings, figures and captions are separated by a blank line, consecutive list items and table rows by a line break, table cells by tabs. Artifacts (running headers, footers, page numbers marked /Artifact) are left out, and so is untagged content. Without path, returns the text instead (up to 1 MiB, then text_truncated). Returns counts of blocks, headings, list items, table rows and figures, and truncated when the structure tree or the text was too large to read in full. Untagged documents, empty structure trees and documents that do not allow copying text are refused.")
            .cmd("export.text")
            .with(schema(json!({ "doc": doc(), "path": { "type": "string", "description": "The .txt file to write (default: return the text)." } }), &["doc"])),
        t(
            "fill_sign_add",
            "Fill & Sign: type text or place a mark",
            "Fill in a form that has no fields, as Acrobat's Fill & Sign does: type text (`text`, 10 pt), place a check, cross, dot or line, today's date (in Preferences ▸ Date format, see fill_sign_date_format, or `format`), or a signature or initials (either `text` drawn in a script font, or `path` to a local PNG/JPEG image up to 4 MiB and 4 megapixels, preserving transparency). `at` [x, y] is in points from the top-left of the page: text's top-left, mark's centre, signature's left edge centred vertically. Image signatures fit within 150 pt wide and 32 pt tall (24 pt for initials). Creates movable, undoable annotations.",
        )
        .cmd("sign.fill.text")
        .with(schema(
            json!({
                "doc": doc(),
                "page": { "type": "integer", "minimum": 1 },
                "type": { "type": "string", "enum": ["text", "check", "cross", "dot", "line", "date", "signature", "initials"] },
                "at": point(),
                "text": { "type": "string", "minLength": 1 },
                "path": { "type": "string", "description": "PNG or JPEG for signature/initials; pass either path or text." },
                "format": { "type": "string", "description": "For type date: this pattern instead of the date-format preference, e.g. \"dd.mm.yyyy\"." },
                "language": { "type": "string", "description": "For type date: month and weekday names in this language code (see fill_sign_date_format) instead of the preference." },
                "author": { "type": "string" },
            }),
            &["doc", "page", "type", "at"],
        )),
        t("fill_sign_date_format", "Date format for Fill & Sign", "Preferences ▸ Date format: the pattern Fill & Sign dates use (default m/d/yyyy). Set it with `format` (Acrobat date codes yyyy yy mmmm mmm mm m dddd ddd dd d, other runs of y, m or d are refused; H h M s t are time letters and need \\ before them; \\ shows the next character as is), or read it. `language` sets the language of month and weekday names (a code from `languages`, or `auto` to follow the app's interface language; English headless). Returns the format, the language, today in them, `unwritable` (characters of today's date that Fill & Sign can't write into a PDF yet, Western European only; fill_sign_add refuses such a date), the ready-made presets and the languages.")
            .with(schema(json!({ "format": { "type": "string", "minLength": 1, "maxLength": 64 }, "language": { "type": "string" } }), &[])),
        t(
            "doc_create",
            "Create a PDF",
            "Create a new, unsaved document and return it like doc_open: `blank` (pages, width, height in points; default 1 US Letter page), `images` (paths of PNG, JPEG, TIFF (every page), GIF or BMP files, one page each at the image's resolution) or `text` (a .txt path, or `text` directly). Save it with doc_save and a path.",
        )
        .with(schema(
            json!({
                "from": { "type": "string", "enum": ["blank", "images", "text"] },
                "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                "dpi": { "type": "number", "minimum": 1, "maximum": 1200, "description": "For images: override the embedded resolution without resampling. 72 gives one point per pixel; omit to use each image's resolution (72 when absent)." },
                "text": { "type": "string" },
                "path": { "type": "string" },
                "pages": { "type": "integer", "minimum": 1, "maximum": 10000 },
                "width": { "type": "number", "minimum": 3 },
                "height": { "type": "number", "minimum": 3 },
                "name": { "type": "string", "description": "Name for the new document (default derived from the source)." },
            }),
            &["from"],
        )),
        t("doc_reduce", "Reduce file size", "Write a smaller copy of the document to `path` with Acrobat's Reduce File Size choices: images above 225 ppi downsampled to 150 ppi and JPEG-compressed (medium quality), thumbnails dropped, identical fonts and images merged, unused objects dropped, compressed object streams. The open document is unchanged.")
            .cmd("optimize.reduce")
            .with(schema(json!({ "doc": doc(), "path": { "type": "string" } }), &["doc", "path"])),
        t("doc_initial_view", "Initial view", "Read or change how the document opens (Document Properties ▸ Initial View) and its reading options: navigation (page, bookmarks, pages, attachments, layers), layout (default, single, continuous, two_up, two_up_continuous, two_up_cover, two_up_continuous_cover), magnification (default, actual, fit_page, fit_width, fit_height, fit_visible, or a percentage), page, window options (fit_window, center_window, full_screen, display_title), interface options (hide_menubar, hide_toolbar, hide_window_ui), language and binding (left, right). Only the given ones change; returns the result. Undoable.").with(schema(
            json!({
                "doc": doc(),
                "navigation": { "type": "string", "enum": ["page", "bookmarks", "pages", "attachments", "layers"] },
                "layout": { "type": "string", "enum": ["default", "single", "continuous", "two_up", "two_up_continuous", "two_up_cover", "two_up_continuous_cover"] },
                "magnification": {},
                "page": { "type": "integer", "minimum": 1 },
                "fit_window": { "type": "boolean" },
                "center_window": { "type": "boolean" },
                "full_screen": { "type": "boolean" },
                "display_title": { "type": "boolean" },
                "hide_menubar": { "type": "boolean" },
                "hide_toolbar": { "type": "boolean" },
                "hide_window_ui": { "type": "boolean" },
                "language": { "type": "string" },
                "binding": { "type": "string", "enum": ["left", "right"] },
            }),
            &["doc"],
        )),
        t("doc_audit_space", "Audit space usage", "How many bytes each kind of content takes and its share of the file (PDF Optimizer ▸ Audit space usage): images, content streams, fonts, forms, comments, structure, bookmarks, … and document overhead, with the object count per category. Sizes are measured in the file (each object from `n g obj` to `endobj`; objects in compressed object streams share that stream's bytes by decoded size: apportioned_bytes); an object reached from several places counts once. The categories add up to total_bytes (the file size; more only when unsaved edits, counted in estimated_objects, are sized as they would be written). overhead splits Document Overhead into cross_reference (tables/streams and trailers of every revision), unclaimed_objects (catalog, page tree, info, metadata, unreachable objects) and other (header, superseded revisions, whitespace).")
            .ro()
            .cmd("optimize.advanced")
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("doc_embedded_fonts", "List embedded fonts", "PDF Optimizer ▸ Fonts: the fonts whose program is embedded (name, subtype, program FontFile/FontFile2/FontFile3, program_bytes, subset) and whether each can_unembed, with the reason when not. Only complete, non-symbolic simple fonts with an explicit standard-based encoding and full widths can be unembedded (codes and layout stay; glyph shapes then come from fonts installed where the file is viewed); composite, subset and symbolic fonts and PDF/A or PDF/X documents are refused. Pass names to doc_optimize's unembed_fonts.")
            .ro()
            .cmd("optimize.advanced")
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("text_lines", "List text lines", "The lines of existing text on a page (Edit a PDF ▸ Edit text): number, text, box (top-left-origin points), font and size. Use the number with text_edit. When the document does not allow copying text, text fields are null and text_withheld explains why.")
            .ro()
            .cmd("edit.edit_text")
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 } }), &["doc", "page"])),
        t("page_images", "List page images", "The raster images and grouped Form artwork a page draws (Edit a PDF): number, box (top-left-origin points), kind (image/form), pixel size ([0,0] for forms) and resource name. Forms include nested graphics and are edited as a whole. Numbers count forms and images together in drawing order, so a page with forms numbers its images differently than before forms were listed: always take numbers from a fresh page_images call.")
            .ro()
            .cmd("edit.edit_text")
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 } }), &["doc", "page"])),
        t("image_edit", "Edit an image", "Change one of a page's images (number from page_images): action move (rect: new box in top-left-origin points), crop (rect: the part to keep, in top-left-origin points; the rest is hidden and can be cropped again), rotate (quarters clockwise, default 1), flip_horizontal, flip_vertical, replace (path: an image file, drawn in the same place) or delete. Undoable. Form artwork supports move/resize/rotate/flip/crop/delete as a group; replace and image_save only support raster images.")
            .cmd("edit.edit_text")
            .with(schema(
                json!({
                    "doc": doc(),
                    "page": { "type": "integer", "minimum": 1 },
                    "image": { "type": "integer", "minimum": 1 },
                    "action": { "type": "string", "enum": ["move", "crop", "rotate", "flip_horizontal", "flip_vertical", "replace", "delete"] },
                    "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                    "quarters": { "type": "integer" },
                    "path": { "type": "string" },
                }),
                &["doc", "page", "image", "action"],
            )),
        t("image_save", "Save image as", "Write one of a page's raster images (number from page_images; forms are refused) to path: JPEG images unchanged, others as PNG (the extension is added when missing).")
            .destructive()
            .cmd("edit.edit_text")
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "image": { "type": "integer", "minimum": 1 }, "path": { "type": "string" } }), &["doc", "page", "image", "path"])),
        t("page_objects", "List page objects", "The objects a page's own content draws, in paint order from back to front (Edit a PDF ▸ select objects): number, kind (text, path, image, form, inline_image, shading, or group for tagged content of several kinds), box (top-left-origin points; null when it can't be measured, e.g. a shading), the MCID that makes tagged content one object, and the text a text object shows. Everything inside one marked-content sequence with an MCID is one object. Added text and images and page marks (headers, watermarks) are not listed: edit them with content_update / doc_header_footer and friends. Numbers change when the order changes: take them from a fresh call. When the document does not allow copying text, text fields are null and text_withheld explains why.")
            .ro()
            .cmd("edit.edit_text")
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 } }), &["doc", "page"])),
        t("objects_arrange", "Arrange objects", "Change the paint order of page objects (numbers from page_objects): order front (Bring to Front), forward (Bring Forward: above the next object up that overlaps them, or the next one up when none does), backward (Send Backward, the same downwards) or back (Send to Back). Several objects keep their relative order. Each object keeps its look (graphics state, clip, colours, tags and optional content). One undo step; a no-op when nothing would change. Pages that use text as a clipping path, or whose text objects hold stray graphics-state operators, are refused.")
            .cmd("edit.edit_text")
            .with(schema(
                json!({
                    "doc": doc(),
                    "page": { "type": "integer", "minimum": 1 },
                    "objects": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "description": "Object numbers from page_objects." },
                    "order": { "type": "string", "enum": ["front", "forward", "backward", "back"] },
                }),
                &["doc", "page", "objects", "order"],
            )),
        t("objects_align", "Align objects", "Align page objects (numbers from page_objects) as displayed: left, right, top or bottom edges, horizontal_center (centres on one vertical line) or vertical_center (centres on one horizontal line). Several objects align to their combined box; a single object aligns to the page (crop box). Objects move by a translation of their own drawing, so text stays editable text. One undo step; a no-op when already aligned. Objects without a box (null rect) can't be aligned.")
            .cmd("edit.edit_text")
            .with(schema(
                json!({
                    "doc": doc(),
                    "page": { "type": "integer", "minimum": 1 },
                    "objects": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "description": "Object numbers from page_objects." },
                    "align": { "type": "string", "enum": ["left", "horizontal_center", "right", "top", "vertical_center", "bottom"] },
                }),
                &["doc", "page", "objects", "align"],
            )),
        t("objects_distribute", "Distribute objects", "Space three or more page objects (numbers from page_objects) evenly as displayed: axis horizontal or vertical. The two outermost objects (by centre) stay where they are and the others move so the gaps between neighbours are equal. One undo step.")
            .cmd("edit.edit_text")
            .with(schema(
                json!({
                    "doc": doc(),
                    "page": { "type": "integer", "minimum": 1 },
                    "objects": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 3, "description": "Object numbers from page_objects." },
                    "axis": { "type": "string", "enum": ["horizontal", "vertical"] },
                }),
                &["doc", "page", "objects", "axis"],
            )),
        t("objects_transform", "Rotate, flip or move objects", "Change page objects (numbers from page_objects) in place: action rotate (quarters clockwise, default 1, each about its own centre), flip_horizontal / flip_vertical (as displayed, each about its own centre) or move (dx right, dy down, in points). Text stays editable text; everything else on the page is untouched. One undo step.")
            .cmd("edit.edit_text")
            .with(schema(
                json!({
                    "doc": doc(),
                    "page": { "type": "integer", "minimum": 1 },
                    "objects": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "description": "Object numbers from page_objects." },
                    "action": { "type": "string", "enum": ["rotate", "flip_horizontal", "flip_vertical", "move"] },
                    "quarters": { "type": "integer" },
                    "dx": { "type": "number" },
                    "dy": { "type": "number" },
                }),
                &["doc", "page", "objects", "action"],
            )),
        t("objects_resize", "Resize objects", "Scale page objects (numbers from page_objects) in place, each about an anchor of its own displayed box (default center; top_left, top, top_right, left, right, bottom_left, bottom, bottom_right): give width and/or height (displayed points; with only one, the other scales in proportion), or scale (both directions), or scale_x / scale_y (factors, 0.001 to 1000). Line widths, text and images scale with the object; text stays editable text. One undo step; objects without a box (null rect) can't be resized.")
            .cmd("edit.edit_text")
            .with(schema(
                json!({
                    "doc": doc(),
                    "page": { "type": "integer", "minimum": 1 },
                    "objects": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "description": "Object numbers from page_objects." },
                    "width": { "type": "number", "exclusiveMinimum": 0, "description": "New displayed width in points." },
                    "height": { "type": "number", "exclusiveMinimum": 0, "description": "New displayed height in points." },
                    "scale": { "type": "number", "exclusiveMinimum": 0, "description": "Scale factor in both directions." },
                    "scale_x": { "type": "number", "exclusiveMinimum": 0 },
                    "scale_y": { "type": "number", "exclusiveMinimum": 0 },
                    "anchor": { "type": "string", "enum": ["center", "top_left", "top", "top_right", "left", "right", "bottom_left", "bottom", "bottom_right"] },
                }),
                &["doc", "page", "objects"],
            )),
        t("objects_style", "Change path appearance", "Change how vector paths (numbers from page_objects; kind path, or the paths inside a tagged group) are painted, as Edit PDF's object properties do: fill and stroke colour (\"none\" turns the fill or stroke off, a colour turns it on; \"#RRGGBB\", [gray], [r, g, b] or [c, m, y, k] with components 0-1 write DeviceGray/RGB/CMYK, { space: current, components } keeps the colour space the path already paints with, e.g. a spot colour or ICC space, components checked against it), line_width (displayed points, 0 = thinnest), dash (lengths in displayed points, [] = solid; dash_phase), cap (butt, round, square), join (miter, round, bevel) and opacity (0-1, fill and stroke: a new ExtGState). Fields left out stay as they are. Each path is wrapped in q … Q with the new state, so nothing else on the page changes; a path left with neither fill nor stroke is refused (use objects_delete). Text, images and other objects are refused. One undo step; tagged content keeps its MCIDs.")
            .cmd("edit.edit_text")
            .with(schema(
                json!({
                    "doc": doc(),
                    "page": { "type": "integer", "minimum": 1 },
                    "objects": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "description": "Object numbers from page_objects." },
                    "fill": { "description": "\"none\", \"#RRGGBB\", [gray], [r, g, b], [c, m, y, k] or { space: gray|rgb|cmyk|current, components }." },
                    "stroke": { "description": "As fill." },
                    "line_width": { "type": "number", "minimum": 0, "maximum": 10000 },
                    "dash": { "type": "array", "items": { "type": "number", "minimum": 0 }, "maxItems": 16, "description": "Dash and gap lengths in points; [] for a solid line." },
                    "dash_phase": { "type": "number", "minimum": 0 },
                    "cap": { "type": "string", "enum": ["butt", "round", "square"] },
                    "join": { "type": "string", "enum": ["miter", "round", "bevel"] },
                    "opacity": { "type": "number", "minimum": 0, "maximum": 1 },
                }),
                &["doc", "page", "objects"],
            )),
        t("objects_delete", "Delete objects", "Delete page objects (numbers from page_objects) from the page's own content: their painting operators go, and the state they leave for later content (a clip, text state set inside a text object) stays, so everything else looks the same. In a tagged PDF the emptied structure elements leave the tag tree. Added items and page marks are deleted with content_delete and the header/footer/watermark tools. One undo step.")
            .destructive()
            .cmd("edit.edit_text")
            .with(schema(
                json!({
                    "doc": doc(),
                    "page": { "type": "integer", "minimum": 1 },
                    "objects": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "description": "Object numbers from page_objects." },
                }),
                &["doc", "page", "objects"],
            )),
        t("text_fonts", "List page fonts", "List the fonts available in a page's resources, with exact resource names, face, weight/slant, subset status and whether each can encode the optional proposed text. Pass resource as text_edit's font_resource to select that exact face. Subsets are never expanded; unavailable characters are refused.")
            .ro()
            .cmd("edit.edit_text")
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "text": { "type": "string" } }), &["doc", "page"])),
        t("text_paragraphs", "List paragraphs", "The paragraphs on a page: number, text, source line numbers, visual line_count, box (top-left-origin points), first style and runs. Each run reports zero-based Unicode scalar start/end (end exclusive), font_resource, font, size, bold, italic and RGB color. Use paragraph and selected range offsets with text_edit. When the document does not allow copying text, text fields are null and text_withheld explains why.")
            .ro()
            .cmd("edit.edit_text")
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 } }), &["doc", "page"])),
        t("text_edit", "Edit text", "Replace the text of one paragraph (paragraph, from text_paragraphs: rewrapped to the paragraph's width with its line spacing) or one line (line, from text_lines) in place, keeping position, size and colour. For a paragraph, also change its formatting: font (helvetica, times, courier) with bold/italic, size (points), color (#rrggbb), align (left, center, right, justify), underline, line_spacing (× size), char_spacing (points) and scale (horizontal, percent), or move it (dx, dy in points; up is +dy) and rewrap it to a new width (points); text may then be omitted. Its own font is reused when it can show every character and no font/weight change is requested; otherwise it uses a standard font matching the source style or, for Japanese and other text no standard font can show (Cyrillic, Greek), real Mincho/Gothic fallback outlines matching the requested/source family and available weight, from a face that has every character (the result shows the font used). Missing Japanese weights use Regular, not synthetic bold; the small web build has only Gothic Regular. Paragraph bold and italic may be set independently without font to keep the source family and the other style. Changing font keeps the current weight/slant unless explicitly overridden. Alternatively font_resource (from text_fonts) selects an exact existing page font and refuses unsupported characters; it cannot be combined with font, bold or italic. Standard Latin faces use their exact glyph advances for reflow. Text that no available font can show is refused. range_start/range_end select Unicode scalar offsets (end exclusive): character formatting affects that range; alignment/width/position/line spacing remain paragraph-wide. Unchanged surrounding runs retain source glyph codes, supported spacing and styles. Underline on/off applies to selected characters and persists for PdfCraft-authored associations through reflow and reopen; unmarked imported paths remain artwork. Malformed underline associations refuse unchanged. Rotated/skewed/reflected mixed text and splitting indivisible PDF glyphs are refused unchanged. preview=true returns a bounded actual-PDF PNG (max_side, default1024) without changing bytes/history. Already-satisfied edits preserve history. Undoable.")
            .cmd("edit.edit_text")
            .with(schema(
                json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "line": { "type": "integer", "minimum": 1 },
                    "paragraph": { "type": "integer", "minimum": 1 },
                    "range_start": { "type": "integer", "minimum": 0, "description": "Start of selected paragraph characters, zero-based Unicode scalar offset in text (or the current paragraph). Pair with range_end. Character formatting affects only this range; geometry affects the paragraph." },
                    "range_end": { "type": "integer", "minimum": 1, "description": "Exclusive end of selected paragraph characters. Existing multi-scalar glyphs cannot be split unless a replacement font can safely represent the resulting text." },
                    "preview": { "type": "boolean", "description": "Return a rendered PNG of the proposed paragraph edit without changing document bytes, dirty state or history." },
                    "max_side": { "type": "integer", "minimum": 128, "maximum": 4096, "description": "Preview PNG maximum side in pixels, default 1024." },
                    "text": { "type": "string" },
                    "font": { "type": "string", "enum": ["helvetica", "times", "courier"] },
                    "font_resource": { "type": "string", "description": "Exact page font resource from text_fonts; cannot be combined with font/bold/italic." },
                    "bold": { "type": "boolean" },
                    "italic": { "type": "boolean" },
                    "size": { "type": "number", "exclusiveMinimum": 0 },
                    "color": { "type": "string", "description": "#rrggbb" },
                    "align": { "type": "string", "enum": ["left", "center", "right", "justify"] },
                    "underline": { "type": "boolean" },
                    "line_spacing": { "type": "number", "description": "Multiple of the font size (1.2 is ordinary)." },
                    "dx": { "type": "number", "description": "Paragraph only: move right by this many points (negative: left)." },
                    "dy": { "type": "number", "description": "Paragraph only: move up by this many points (negative: down)." },
                    "width": { "type": "number", "description": "Paragraph only: rewrap to this width in points (dragging the box's edge)." },
                    "char_spacing": { "type": "number", "description": "Points." },
                    "scale": { "type": "number", "description": "Horizontal scale in percent." } }),
                &["doc", "page"],
            )),
        t(
            "doc_linearization_check",
            "Check Fast Web View",
            "Report whether a PDF is linearized (Fast Web View) and whether its linearization is consistent: the parameter dictionary comes first, /L is the file length, /O the first page, /N the page count, /E the end of the first-page section, /T the main cross-reference section, /H the hint stream, and the hint tables match where the pages and shared objects are. Give doc (the file as last saved or opened; unsaved edits are ignored) or path (with password for an encrypted file, so its hint stream can be read). Returns linearized, valid, fast_web_view, problems, notes (checks that could not be made), params and file_size. Read-only.",
        )
        .ro()
        .with(schema(
            json!({
                "doc": doc(),
                "path": { "type": "string", "description": "A PDF file to check instead of an open document." },
                "password": { "type": "string", "description": "With path: the password of an encrypted file." }
            }),
            &[],
        )),
        t("doc_revisions", "List revisions", "List the document's saved revisions (oldest first): each incremental update is one. Returns revision number, where it ends in the file and its size, and which signatures sign exactly that revision.")
            .with(schema(json!({ "doc": doc() }), &["doc"])),
        t("doc_open_revision", "Open a revision", "Open saved revision `revision` (1 = the oldest) of a document as a new, unsaved document, to see the file as it was then.")
            .with(schema(json!({ "doc": doc(), "revision": { "type": "integer", "minimum": 1 } }), &["doc", "revision"])),
        t("doc_optimize", "Optimize PDF", "Write an optimized copy to `path` (Acrobat's PDF Optimizer). color / gray: { downsample, ppi, above_ppi, compression: jpeg|zip|retain, quality 1–100 } (defaults: downsample to 150 ppi above 225, JPEG 60). mono (one-bit images and image masks): { downsample, ppi, above_ppi, compression: ccitt-g4|zip|retain } or just the compression name (defaults: downsample to 300 ppi above 450, CCITT Group 4; without downsampling the samples are unchanged). Images are measured where pages draw them and replaced only when smaller. discard_*: thumbnails (default true), alternate_images (true), tags, print_settings; flate_unencoded (true); lzw_to_flate (true: LZW streams that decode completely are re-encoded as Flate, others are left and counted in lzw_skipped); compress_structure (true: pack objects into object streams); remove_invalid_links and remove_unreferenced_dests (true). compatibility: retain (default) or a PDF version (1.3, 1.4, 1.5, 1.6, 1.7, 2.0) to write; below 1.5 there are no object streams, and features the version doesn't define are kept and listed in newer_features (feature, needs, count). discard: Remove Hidden Information categories (metadata, attachments, comments, form-fields, hidden-text, hidden-layers, bookmarks, links-actions-scripts, private-data). unembed_fonts: embedded font names (from doc_embedded_fonts) whose program is removed; a name that matches no embedded font or a font that can't be unembedded refuses the whole call. Signed documents are refused. The open document is unchanged.")
            .cmd("optimize.advanced")
            .with(schema(
                json!({
                    "doc": doc(),
                    "path": { "type": "string" },
                    "color": { "type": "object" },
                    "gray": { "type": "object" },
                    "discard_thumbnails": { "type": "boolean" },
                    "discard_alternate_images": { "type": "boolean" },
                    "discard_tags": { "type": "boolean" },
                    "discard_print_settings": { "type": "boolean" },
                    "mono": { "type": ["object", "string"] },
                    "flate_unencoded": { "type": "boolean" },
                    "lzw_to_flate": { "type": "boolean" },
                    "compress_structure": { "type": "boolean" },
                    "remove_invalid_links": { "type": "boolean" },
                    "remove_unreferenced_dests": { "type": "boolean" },
                    "compatibility": { "type": "string", "enum": ["retain", "1.3", "1.4", "1.5", "1.6", "1.7", "2.0"] },
                    "discard": { "type": "array", "items": { "type": "string" } },
                    "unembed_fonts": { "type": "array", "items": { "type": "string" } },
                }),
                &["doc", "path"],
            )),
        t("doc_flatten", "Flatten", "Merge comment and/or form field appearances into the page content, so they print and display everywhere but can no longer be edited. Undoable until saved.")
            .destructive()
            .with(schema(json!({ "doc": doc(), "comments": { "type": "boolean", "description": "Default true." }, "fields": { "type": "boolean", "description": "Default true." } }), &["doc"])),
        t("edit_undo", "Undo", "Undo the last edit of a document.").cmd("edit.undo").with(schema(json!({ "doc": doc() }), &["doc"])),
        t("edit_redo", "Redo", "Redo the last undone edit of a document.").cmd("edit.redo").with(schema(json!({ "doc": doc() }), &["doc"])),
        t("command_list", "List commands", "Every registered PdfCraft command with its menu, shortcut, whether it is enabled now, and the tool that automates it.")
            .ro()
            .with(schema(json!({ "doc": doc(), "filter":{"type":"string"}, "enabled_only":{"type":"boolean"} }), &[])),
        t("command_run", "Run a command", "Run a registry command through its headless tool. Pass that tool's arguments in params (see command_list). Unknown params are ignored with warnings; path confinement still applies.")
            .destructive().with(schema(json!({"id":{"type":"string"},"params":{"type":"object"}}), &["id"])),
        t("command_batch", "Run several commands", "Run steps in order (at most 1000). Returns completed/failed counts and per-step ok/result/error; stop_on_error defaults to true.")
            .destructive().with(schema(json!({"steps":{"type":"array","items":schema(json!({"id":{"type":"string"},"params":{"type":"object"}}), &["id"])},"stop_on_error":{"type":"boolean"}}), &["steps"])),
        t("doc_inspect", "Inspect open documents", "With doc, return doc_info; otherwise return information for every open document (empty documents array when none are open).")
            .ro().with(schema(json!({"doc":doc()}), &[])),
        t("render_preview", "Render a preview", "Render a PNG bounded by max_side (default 1024), without changing or saving the document. Page defaults to 1; doc may be omitted when exactly one document is open. overprint_preview as for page_render.")
            .ro().with(schema(json!({"doc":doc(),"page":{"type":"integer","minimum":1},"max_side":{"type":"integer","minimum":1,"maximum":4096},"overprint_preview":overprint_preview()}), &[])),
    ]
}

static TOOLS: std::sync::LazyLock<Vec<ToolDef>> = std::sync::LazyLock::new(tools);

pub(crate) fn find(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

pub(crate) fn tool_for_command(id: &str) -> Option<&'static str> {
    TOOLS.iter().find(|t| t.command == Some(id)).map(|t| t.name)
}

/// Reject unknown and missing arguments (types are checked when each value is read).
pub(crate) fn check_args(def: &ToolDef, args: &Value) -> Result<(), ToolError> {
    let obj = args.as_object().ok_or_else(|| ToolError::InvalidArgs("arguments must be a JSON object".into()))?;
    let props = def.input_schema["properties"].as_object().cloned().unwrap_or_default();
    if let Some(k) = obj.keys().find(|k| !props.contains_key(*k)) {
        let known: Vec<&str> = props.keys().map(String::as_str).collect();
        return Err(ToolError::InvalidArgs(format!("{}: unknown argument {k:?} (expected: {})", def.name, known.join(", "))));
    }
    for r in def.input_schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if obj.get(r).is_none_or(Value::is_null) {
            return Err(ToolError::InvalidArgs(format!("{}: missing argument {r}", def.name)));
        }
    }
    Ok(())
}

fn lock_schema() -> Value {
    json!({ "type": "string", "enum": ["none", "all", "include", "exclude"], "description": "Which fields signing makes read-only: all, only lock_fields (include), all but lock_fields (exclude), or none." })
}
