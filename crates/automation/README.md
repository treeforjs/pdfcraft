# pdfcraft-automation

Agent control for PdfCraft: a headless tool table over the engine, and an opt-in MCP server.

- **Layer:** L7 (architecture §13), but headless. It depends on `engine`, `render` and `organize`, never on a UI toolkit.
- **Status:** over a hundred tools; `pdfcraft-cli tools` lists the current set with their schemas (the count isn't repeated here, so it can't go stale). They cover inspection, rendering, text, page edits and organizing, page labels, bookmarks, comments and their review, form filling and authoring (fields, properties, scripts, data exchange, detection), redaction and sanitizing, password protection and signatures, calibrated 2D measurements and CSV export, adding text, images, links and stamps, optimizing, OCR, printing, export, metadata, undo/redo, save, combine, extract and split. Comment tools take geometry in the same top-left-origin points as everything else, and `comment_add` can mark text by searching for it (`find`). The MCP server also serves the open documents as resources, page images included. The running app's UI is driven separately, through the `ui.*` control channel in `pdfcraft-ui-egui`.

## API

```rust
let mut a = Automation::new().with_root("/work")?;      // optional: confine all paths
let doc = a.call("doc_open", &json!({ "path": "in.pdf" }))?;   // Vec<Content>
a.write_output("page1.png", &png)?;                      // save a result yourself, under the same root rules
tools() -> Vec<ToolDef>                                  // name, title, description, input_schema, read_only, destructive, command
mcp::McpServer::new(a).serve(stdin, stdout)?             // newline-delimited JSON-RPC 2.0
```

`Content` is `Json(Value)` or `Png { data, width, height }`. Errors are `UnknownTool`, `InvalidArgs` (the call didn't match the schema) or `Failed` (a readable message for the agent).

## Conventions

- Tool names are `snake_case` (`page_rotate`): MCP clients reject dots. `ToolDef::command` links a tool to the registry id it automates (`page.rotate`), and `command_list` reports the link.
- Pages and positions are **1-based**. Rectangles are PDF points with the origin at the top-left of the displayed page.
- Unknown arguments are rejected, so typos fail loudly.
- Tools that change a document return its summary (`doc`, `pages`, `dirty`, `undo`, `redo`, …).
- `comment_image_preview` returns JSON and a PNG: `layer: "background"` (the default) gives the page without the selected image signature/initials; `layer: "image"` gives its embedded image with alpha. The metadata supplies its displayed rectangle, document rotation, `image_rotation` (how far clockwise the image is shown turned: 0 for one placed by PdfCraft, which keeps it upright as displayed) and annotation opacity. Each result can be saved with the CLI's `--out`. Cache the background and transform the image for live movement/resizing; `comment_edit` commits the final geometry in one undo step. Previewing never changes the document.
- For proportional image signature resizing, use the embedded image layer's width/height ratio rather than a previously stretched annotation rectangle. The GUI preserves this ratio at corners with the opposite corner anchored; edge midpoint handles change only their own axis. `comment_edit` accepts the resulting rectangle.
- `doc_close` refuses to drop unsaved changes unless `discard_changes: true`. `doc_save` writes atomically, incrementally in place, and in full for a new path. Its remembered save path is checked against the root again on every save, including when no `path` is supplied.
- `doc_split` chooses available output names, adding numeric suffixes for repeated or sanitized bookmark titles and existing files. Exhausting the bounded filename search returns an error instead of replacing an earlier output.
- With a root set, every read and write path must resolve inside it (symlinks and `..` included). Relative paths resolve inside the root, and `..` is resolved by name before the check. Every path outside the root gets the same refusal (`<path> is outside the allowed directory <root>`), whether or not it exists, so a confined agent can't probe the rest of the disk; on Windows another network share or device path (`\\host\share`, `\\?\UNC\…`, `\\.\…`) is refused without being contacted. "Not found" and other filesystem errors are reported only for paths inside the root. Tools that write several files into a folder name them after the document or input file, with separators, colons and control characters replaced by `_`, so a name can't lead them out of the folder. The root itself must be a folder.

## Shared field properties

`form_set_props` accepts either `field` (one name) or `fields` (1–1000 unique names):

```json
{"doc": 1, "fields": ["first_name", "last_name"], "required": true,
 "appearance": {"width": 3, "border": "#1473E6"}}
```

The batch is atomic and creates one undo step. A missing or locked field, invalid option,
unknown argument or malformed appearance refuses the whole edit. Set `locked: false` to
unlock and change fields together. Only supplied properties change: each field/widget keeps
its other colours, fonts, values and unknown PDF dictionary keys. `"none"` removes a border
or fill colour. Names, rectangles and rotations require the single-field form. Results keep
`field` for single edits and return `fields` for batches. CLI and MCP use the same tool.

## Existing text and page fonts

`text_fonts {doc, page, text?}` lists the page's available font resources and tests whether each
can encode the proposed paragraph. `text_edit {doc, page, paragraph, font_resource: "F2", text?}`
selects that exact face, including an embedded subset, with its own advances. Missing resources
or unsupported characters fail without changing the document or its undo history; subsets are
never expanded. The proposal uses the editor's existing whitespace normalization (newlines/tabs
become ordinary spaces; ideographic spaces are preserved).

An exact resource cannot be combined with `font`, `bold`, or `italic`: its face fixes those styles.
Subsequent size/colour changes keep the selected face. Subsequent bold/italic changes select a
standard face matching the source family and preserve the other style; changing `font` similarly
keeps weight/slant unless overridden. The UI reflects the actual selected face after applying.
`text_paragraphs` also reports `line_count` and `runs`, with `start`/`end`, font resource, face,
size, bold, italic and RGB colour. Offsets count Unicode scalar values from zero, with an exclusive
end; they are not UTF-8 byte offsets or grapheme indexes. `text_edit` accepts `range_start` and
`range_end` to format that selection. Font/size/colour/weight/slant affect the selection; width,
position, alignment and line spacing affect the paragraph. With no selection, formatting applies
to the whole paragraph. Unchanged prefix/suffix text keeps its styles when text is replaced.

Selected-range and mixed-run edits retain existing glyph codes, supported Tc/Tw/Tz/Ts state and
TJ advances for unchanged runs, including multi-scalar ToUnicode mappings. A selection that splits
one indivisible existing glyph is refused. Paragraphs with active stroking/invisible/clipping text or extended graphics
state also refuse edits until their complete graphics state can be preserved. Missing subset
characters, invalid ranges, unsupported rotated/skewed/reflected
mixed text and invalid numbers leave bytes and history unchanged. Upright nonuniform scaling is
supported. Paragraph edits accept at most 1,000,000 Unicode characters and 8,192 style segments;
exceeding either limit refuses the edit unchanged. A request already satisfied does not add an
undo step or clear redo.

Use `text_edit {..., preview: true, max_side: 1024}` to receive the proposed page as a PNG without
applying or saving it. The desktop's **Preview PDF** uses this same actual PDF rendering path;
**Continue editing** preserves the draft and **Apply and close** accepts it. The typing surface
uses interface fonts to show run styling; the rendered preview is authoritative for the PDF face.

The Japanese fallback still has no italic outlines. Whole-paragraph edits of one uniform style
still use legacy normalization/reflow; their original character-code and spacing preservation is
not yet equivalent to range/mixed edits. Broader shaping/encodings, transformed text, run decoration
round-trips, structure tags and complex-layout paragraph detection remain open.

## MCP server

**Opt-in only.** Nothing starts it automatically, and it opens no port. It runs while `pdfcraft-cli mcp [--root DIR]` runs, normally launched by an agent from its MCP configuration. It stops when stdin closes. The CLI's `mcp` Cargo feature (on by default) compiles it out entirely when disabled.

`pdfcraft-cli mcp --compact` (or `McpServer::with_compact(true)`) keeps `tools/list` short for agents with small context budgets. It lists only the core tools (`mcp::COMPACT_CORE_TOOLS`: open, info, save, close, render, text extract and find, combine, split, undo) plus the common command/inspection/preview tools and two meta tools: `tool_search` (optional `query` and `category`, the name prefix; or `name` for one tool's full `input_schema`) and `tool_call` (`name` and `arguments`, run through the same `Automation::call`). `tools/call` by name keeps working for every tool, and the server instructions mention the meta tools. Without the flag nothing changes.

Implemented: `initialize` (protocol 2025-06-18, 2025-03-26, 2024-11-05), `ping`, `tools/list` (with `readOnlyHint`/`destructiveHint` annotations), `tools/call` (JSON results also returned as `structuredContent`; images as `image/png`), and `resources/list`, `resources/templates/list` and `resources/read`. The resources expose the open documents read-only: `pdfcraft://doc/{doc}/info` (JSON), `…/text`, `…/page/{page}/text` and `…/page/{page}/image{?dpi}` (PNG, 1–600 dpi). Tool failures come back as `isError: true` results, so the agent can read and recover from them.

## Adding a tool

1. Add the engine capability first, with its tests (the tool is a thin adapter).
2. Add a `ToolDef` in `src/tools.rs`: a precise description, the schema, `ro()`/`destructive()`, and `cmd()` if a registry command exists.
3. Handle it in `Automation::call`, validating pages and positions with the existing helpers.
4. Add an end-to-end test in `tests/automation.rs`. `tool_table_is_well_formed` checks names, schemas and command links.


## Common command tools

Following FilmCraft's MCP conventions, `command_list {doc?, filter?, enabled_only?}` keeps its
`{commands: [...]}` result and adds each mapped tool's parameter schema. `command_run {id, params?}`
runs that command through its existing headless tool: include `doc` and other tool arguments in
`params`. Commands without a headless mapping return a tool error. Unknown parameter keys are
ignored with a `warnings` array; required values and paths still go through the original validation.

`command_batch {steps: [{id, params?}], stop_on_error?}` returns `completed`, `failed`, and per-step
`{ok, result?, error?}` entries. The default is to stop at the first error; earlier edits remain.
MCP marks a batch with any failed steps as `isError`, including through compact `tool_call`.
`doc_inspect {doc?}` returns one document's info, or `{documents: [...]}` for all open documents.
`render_preview {doc?, page?, max_side?}` returns a PNG (page defaults to 1, maximum side defaults
to 1024, allowed 1–4096). Omit `doc` only when exactly one document is open.

All task tools remain available; compact mode includes the five common tools. Every MCP tool has
all four annotation hints. Unknown top-level argument keys return JSON-RPC `-32602` with the
accepted keys; failures in the invoked tool remain `isError` results. An escaped tool panic is
reported as an internal tool error, and the server continues serving.

Exports complete synchronously. `progressToken` and cancellation notifications are harmlessly
ignored; no background-job or cancellation behavior is added.

Session resources `pdfcraft://document` and `pdfcraft://commands`, and the refusal of unknown argument keys, are described in [docs/mcp.md](../../docs/mcp.md).
