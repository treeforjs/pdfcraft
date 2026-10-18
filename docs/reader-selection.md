# Reader text selection

The reader keeps a selection anchor and head across pages in extracted reading order. Dragging
or Shift-clicking onto another page extends it; reversing direction keeps the original anchor.
Blank intervening pages are retained as page boundaries. Near the top or bottom of a continuous
viewport, a text-owned drag scrolls the document. Comment and form gestures keep their own input.

Ctrl+A (Command+A on macOS) selects document text, including pages not yet visible. Copy waits
for all selected text layers; it never returns the subset that happened to be cached. Escape,
Cancel, a replacement selection, document invalidation, switching tabs, a modal/palette,
window focus loss, or a focused text editor cancels a pending copy. A page extraction
failure is reported rather than silently omitted. Organize and Prepare Form retain their existing
page/field Select All behavior, and focused text editors retain their own keyboard selection.

Double-click selects a word; triple-click selects a line. Four clicks select the page in Single
Page layout and the document in continuous/two-page layouts. The reader's context menu also offers
**Select paragraph**. Untagged paragraph detection conservatively joins adjacent lines with
ordinary spacing, similar height and overlapping width; it stops at columns and larger gaps.
It is geometric, not a claim of semantic tagged-PDF paragraph identity.

Copy and headless extraction honor PDF copy permission, including the owner-password override.
Accessibility permission is distinct and does not grant clipboard copying. Selection does not
modify PDF bytes, dirty state, or undo/redo. Text retains the extractor's Unicode mappings,
including whole multi-character glyph mappings, word gaps and line breaks. Page boundaries are
two newlines, including blank pages. One selection result is limited to 32 MiB UTF-8; oversized
requests fail and can be retried on smaller ranges. Engine extraction interprets pages sequentially;
the UI uses its existing background text cache, progressively stopping selection demand at
2 million glyphs or 32 MiB of selected-page glyph text. At most 32 missing selected pages are
queued at once; an offscreen page that crosses the limit is not retained, and a smaller selection
can be retried. Already-visible rendering and the separate Find workflow keep their own demand.

## Copy permission on every text-returning path

Bit 5 of `/P` ("copy or otherwise extract text and graphics", ISO 32000-2 Table 22) governs
every path that hands page text or embedded graphics to the caller; viewing does not need it.
`Document::allows_copying` is the single check and fails closed: an encrypted document whose
security handler the editor could not read (the renderer opened it, the editor did not) has no
established permissions and is treated as copy-denied. `pdfcraft-cli text` likewise refuses an
encrypted file the engine cannot open (for example an AES-256 file whose plaintext `/P` was
edited without its `/Perms` check value), even with the owner password.

| Path | Copy denied |
| --- | --- |
| `text_selection`, `text_extract`, MCP `pdfcraft://doc/{n}/text` and `/page/{p}/text`, reader copy, `pdfcraft-cli text` | refused |
| `text_find` | allowed as a search (Acrobat searches such files), but matches are locations only: `text` is null, `text_withheld` says why; only the caller's query is echoed |
| `text_lines`, `text_paragraphs`, `content_list`, the `text_edit` result echo | locations, fonts and styles only; `text` is null and `text_withheld` is set |
| `doc_export_text`, `doc_export_office` (Word/HTML/RTF), `doc_export_pptx` in editable mode and Export dialogs, `doc_export_all_images` (graphics) | refused before anything is written (`doc_export_pptx` mode `image` only renders pages, as Export ▸ Image does) |
| `doc_compare`, `doc_compare_report`, `doc_compare_mark` and the Compare panel | refused unless both documents allow copying |
| `ocr_recognize` and OCR review drafts | copy and change authority required — enforced by the OCR lane (`Session::ocr_authority`), pending integration |
| `form_detect_fields`, Prepare a form ▸ Detect form fields, the `detect_form_fields` action step | fields are still detected and added in the same places, but named generically (`Text1`, `Check Box1`, …) instead of from their labels' words; the tool result carries `names_withheld` |
| Edit text & images: opening a paragraph in the in-place editor (its text in a selectable, copyable field) | refused with a notice; paragraph boxes can still be moved and resized, images edited, and `text_edit` can still replace a paragraph headlessly (its echo is withheld, above) |
| `page_render`, `render_preview`, `doc_export_images`, visual compare, the Find panel's on-screen result context | allowed (viewing) |
| `doc_info`/`doc_inspect` metadata, bookmarks, comment contents, field values, alt text, `comments_summarize`, `redact_mark` counts | allowed: not page text |

Accessibility permission (bit 10) is not treated as granting extraction to tools or the clipboard.

The other permission checks (`allows_printing`, `allows_assembly`, `allows_modification`,
`allows_form_filling`, `allows_annotation`, `allows_security_change`) share the same fail-closed
rule through `Document::permitted`: an encrypted document without established permissions
denies printing and every change, owner authentication grants everything, and unencrypted
documents are unaffected. Before this, printing (`print_pdf`, `doc_print`, the Print dialog) was
allowed for such a document; the change checks already required an editor. `doc_info` reports
its security as `{"protected": true, "permissions_verified": false}` instead of unprotected.
Callers audited: the command catalog (`commands::is_enabled`), `Session::apply`, `print_pdf`,
extract/split/summarize (they need an editor's document, so they refuse without one), OCR
authority, and the UI's canvas, panels, dialogs and measure tools all go through these checks.

## Headless use

`text_extract` accepts `include_glyphs: true` to expose one-based glyph indices, their Unicode
mapping, reading-order line and view-space rectangle. This inspection is limited to 200,000
glyphs per request; request fewer pages for larger documents. Copy permission applies to the
existing text-only response too.

```json
{"tool":"text_extract","args":{"doc":1,"pages":[1,3],"include_glyphs":true}}
{"tool":"text_selection","args":{"doc":1,"start_page":1,"start_glyph":5,"end_page":3,"end_glyph":12}}
{"tool":"text_selection","args":{"doc":1,"mode":"paragraph","start_page":1,"start_glyph":5}}
{"tool":"text_selection","args":{"doc":1,"mode":"all"}}
```

Range endpoints are inclusive whole-glyph indices. Reverse ranges normalize to document order.
Paragraph mode takes only its start position; All takes no endpoints. Invalid pages, glyphs,
mode combinations and permission failures return an error without changing the document.
`ui.state.active.text_selection` reports the real reader range, pending-copy state, permission and text;
its text is capped at 65,536 UTF-8 bytes with an explicit `truncated` flag and full `text_bytes`.
The control channel remains opt-in and authenticated.

## References and limits

The public [Reuse PDF content documentation](https://helpx.adobe.com/acrobat/using/reusing-pdf-content.html)
describes text selection, copying, security restrictions and layout-dependent four-click behavior.
PDF standard-security permission bit 5 is defined by ISO 32000-2 Table 22. No proprietary program,
asset, implementation or output was used; fixtures are generated from original source strings.

Reading order remains the existing extractor's geometric model. Complex tagged structure,
ambiguous interleaved columns, arbitrary-angle writing, bidi/shaping recovery, copy-with-layout,
and semantic paragraph reconstruction are not claimed complete by this workflow. Selection is a
reader operation; multi-page annotation creation is a separate workflow.

For example, a narrow column printed beside the first lines of a main column is read between
those lines, not after the main column, because the extractor orders lines by position.

## Validation status

Focused tests pass on Ubuntu 24.04: engine `reader_selection` unit tests, the automation
`reader_selection` integration tests (including an AES-256 open+permissions-password document
with copy denied, where every `text_selection` mode and `text_extract` refuse without echoing
document text), reader UI unit tests in `canvas.rs`, and `crates/ui-egui/tests/text_select.rs`.
`pdfcraft-cli text` applies the same copy permission (`apps/pdfcraft-cli/tests/text.rs`).
The permission table above is covered by passing encrypted regressions (copy denied, owner
password lifts): engine `export_text_images_and_office_refuse_copy_denied_documents_and_owner_lifts`,
`compare_refuses_when_either_document_denies_copying` and the synthetic no-editor
`encrypted_documents_without_an_editor_fail_closed`; automation
`copy_denied_text_paths_withhold_or_refuse_and_owner_lifts` and
`mcp_text_resources_refuse_copy_denied_documents_and_owner_lifts`; and the CLI
`text_fails_closed_when_encryption_permissions_cannot_be_verified` (a tampered `/P`, which
printed the text before this change).
A headless `pdfcraft-cli run --script` workflow on an original four-page fixture exercised range,
reverse, paragraph and all modes, glyph inspection, and refusal for copy-restricted files.
Hardening follow-ups (2026-10-12, wp/hardening), all passing on Ubuntu 24.04: engine
`detected_field_names_withhold_label_text_when_copying_is_denied` and
`encrypted_documents_without_an_editor_deny_printing_and_changes`; automation
`permission_guards.rs` (`detected_field_names_withhold_label_text_when_copying_is_denied`,
`print_and_modify_fail_closed_on_restricted_and_tampered_permissions`, an AES-256 file whose `/P`
was edited to grant everything is refused with either password); UI
`editing.rs::edit_text_does_not_open_paragraphs_of_a_copy_denied_document`.
Still pending: the full workspace suite, WASM, and acceptance on an installed X11/Wayland
desktop with the system clipboard. The control channel's `ui.key` does not synthesize an OS
`Event::Copy` from Ctrl+C, so clipboard checks use the context-menu **Copy**.
