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

fn doc() -> Value {
    json!({ "type": "integer", "minimum": 1, "description": "Document id from doc_open or doc_list." })
}

fn pages(what: &str) -> Value {
    json!({ "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "description": format!("1-based page numbers {what}.") })
}

fn path(what: &str) -> Value {
    json!({ "type": "array", "items": { "type": "integer", "minimum": 1 }, "description": format!("{what}: 1-based positions from the top level, e.g. [2, 1] = the first child of the second bookmark.") })
}

fn point() -> Value {
    json!({ "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "[x, y] in points from the top-left of the displayed page." })
}

fn color() -> Value {
    json!({ "type": "string", "description": "#RRGGBB or a name: yellow, red, orange, green, blue, purple, pink, black, gray, white." })
}

/// Properties that pick one comment: its `id` (from comment_list), or `page` + `index`.
fn comment_ref(mut extra: Value) -> Value {
    let props = extra.as_object_mut().expect("an object");
    props.insert("doc".into(), doc());
    props.insert("id".into(), json!({ "type": "string", "description": "The comment's id (from comment_list)." }));
    props.insert("page".into(), json!({ "type": "integer", "minimum": 1, "description": "With `index`, instead of `id`." }));
    props.insert("index".into(), json!({ "type": "integer", "minimum": 1, "description": "1-based position on the page, as comment_list reports." }));
    extra
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
            .with(schema(json!({ "path": { "type": "string" }, "password": { "type": "string", "description": "User or owner password for encrypted files." } }), &["path"])),
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
            "Save to its own file (an incremental update, which keeps signatures valid) or to a new path (a full rewrite). The write is atomic.",
        )
        .destructive()
        .cmd("file.save")
        .with(schema(
            json!({ "doc": doc(), "path": { "type": "string", "description": "Save as this file. Omit to save in place." }, "full": { "type": "boolean", "description": "Force a full rewrite (or, with false, an incremental update)." } }),
            &["doc"],
        )),
        t("doc_set_info", "Set document metadata", "Set a document information entry such as Title, Author, Subject or Keywords. Undoable.")
            .with(schema(json!({ "doc": doc(), "key": { "type": "string" }, "value": { "type": "string" } }), &["doc", "key", "value"])),
        t("page_render", "Render a page", "Render one page to a PNG image (default 96 dpi, at most 600).")
            .ro()
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1 }, "dpi": { "type": "number", "minimum": 1, "maximum": 600 } }), &["doc", "page"])),
        t("text_extract", "Extract text", "Extract the text of some or all pages, in reading order.")
            .ro()
            .with(schema(json!({ "doc": doc(), "pages": pages("to extract (default: all)") }), &["doc"])),
        t("text_find", "Find text", "Find a phrase (case-insensitive, whitespace-normalised) and return each match with its page and line rectangles in points (origin top-left).")
            .ro()
            .cmd("edit.find")
            .with(schema(json!({ "doc": doc(), "query": { "type": "string", "minLength": 1 }, "limit": { "type": "integer", "minimum": 1, "description": "Maximum matches (default 500)." } }), &["doc", "query"])),
        t("page_rotate", "Rotate pages", "Rotate pages by a multiple of 90 degrees (positive is clockwise). Undoable.")
            .cmd("page.rotate")
            .with(schema(json!({ "doc": doc(), "pages": pages("to rotate"), "degrees": { "type": "integer" } }), &["doc", "pages", "degrees"])),
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
        t("page_insert_file", "Insert pages from a file", "Insert pages of another PDF so the first becomes page `at`. Undoable.")
            .cmd("page.insert")
            .with(schema(json!({ "doc": doc(), "path": { "type": "string" }, "at": { "type": "integer", "minimum": 1 }, "pages": pages("of the source file (default: all)") }), &["doc", "path", "at"])),
        t("page_extract", "Extract pages", "Copy pages into a new PDF (links, bookmarks, fields and layers that belong to them come along).")
            .cmd("page.extract")
            .with(schema(json!({ "doc": doc(), "pages": pages("to extract"), "out": save_out.clone(), "open": open.clone() }), &["doc", "pages"])),
        t("doc_combine", "Combine files", "Combine PDFs, in order, into one (bookmarks are kept under one entry per file).")
            .cmd("page.combine")
            .with(schema(json!({ "paths": { "type": "array", "items": { "type": "string" }, "minItems": 2 }, "out": save_out, "open": open }), &["paths"])),
        t("doc_split", "Split a document", "Split into several files, every N pages or before given pages, written to out_dir as <name>-partK.pdf.")
            .cmd("page.split")
            .with(schema(
                json!({ "doc": doc(), "every": { "type": "integer", "minimum": 1 }, "before": pages("that start a new part"), "out_dir": { "type": "string" } }),
                &["doc", "out_dir"],
            )),
        t("bookmark_list", "List bookmarks", "The bookmark tree with each bookmark's path, title, target page and open state.")
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
        t("comment_list", "List comments", "Every comment (annotation other than links, form widgets and pop-ups) with its page, index, id, type, author, text, date, rectangle, colour, review status and replies.")
            .ro()
            .with(schema(json!({ "doc": doc(), "page": { "type": "integer", "minimum": 1, "description": "Only this page." } }), &["doc"])),
        t(
            "comment_add",
            "Add a comment",
            "Add a comment as Acrobat's commenting tools do. Geometry is in points with the origin at the top-left of the displayed page, y down (as in page_render images at 72 dpi and text_find rects). \
             note: `at` [x, y] (icon top-left). highlight/underline/strikeout/squiggly: `find` (text on the page to mark; every match with all: true) or `quads`. \
             rectangle/oval/textbox: `rect` [x0, y0, x1, y1]. line/arrow: `from`, `to`. ink: `strokes` [[[x, y], …], …]. Undoable.",
        )
        .with(schema(
            json!({
                "doc": doc(),
                "page": { "type": "integer", "minimum": 1 },
                "type": { "type": "string", "enum": ["note", "highlight", "underline", "strikeout", "squiggly", "rectangle", "oval", "line", "arrow", "ink", "textbox"] },
                "contents": { "type": "string", "description": "The comment text (what a text box shows)." },
                "author": { "type": "string" },
                "at": point(),
                "rect": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                "from": point(),
                "to": point(),
                "find": { "type": "string", "description": "Text on the page to mark up (case-insensitive)." },
                "all": { "type": "boolean", "description": "Mark every match of `find` on the page, not just the first." },
                "quads": { "type": "array", "items": { "type": "array", "items": { "type": "number" }, "minItems": 8, "maxItems": 8 } },
                "strokes": { "type": "array", "items": { "type": "array", "items": point() } },
                "icon": { "type": "string", "enum": ["Comment", "Note", "Help", "Insert", "Key", "NewParagraph", "Paragraph"] },
                "color": color(),
                "fill": color(),
                "opacity": { "type": "number", "minimum": 0, "maximum": 1 },
                "width": { "type": "number", "minimum": 0, "description": "Line width in points." },
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
        t("comment_edit", "Edit a comment", "Change a comment's text, colour, opacity, line width, rectangle (rectangle/oval/text box) or position (`move` [dx, dy] in points). One undo step.").with(schema(
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
        t("comment_delete", "Delete a comment", "Delete a comment with its pop-up and replies. Undoable.").destructive().with(schema(comment_ref(json!({})), &["doc"])),
        t("edit_undo", "Undo", "Undo the last edit of a document.").cmd("edit.undo").with(schema(json!({ "doc": doc() }), &["doc"])),
        t("edit_redo", "Redo", "Redo the last undone edit of a document.").cmd("edit.redo").with(schema(json!({ "doc": doc() }), &["doc"])),
        t("command_list", "List commands", "Every registered PrintCraft command with its menu, shortcut, whether it is enabled now, and the tool that automates it.")
            .ro()
            .with(schema(json!({ "doc": doc() }), &[])),
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
