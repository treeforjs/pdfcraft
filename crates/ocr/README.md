# Text recognition

`pdfcraft-ocr` runs the bundled `ocrs` model and writes invisible searchable text. `engine::ocr` renders PDF pages, bounds the input, and binds each recognition draft to the source document version. The UI reviews words before applying by default. The original page image stays intact.

The validated recognition language is **English**. The bundled recognition model has an ASCII output alphabet. Editing the language list or supplying a different alphabet does not train the model to recognize accented letters or other scripts. Corrected searchable words may contain WinAnsi characters, including Latin accents; unsupported characters are rejected instead of becoming question marks. The recognition API does not expose calibrated word confidence, so the review lists every word rather than inventing “suspect” scores.

Fetch the existing, manifest-pinned models with `cargo xtask models`, or set `PDFCRAFT_MODELS` to a directory containing both `.rten` files. Desktop packages ship both models, their license texts and attribution. Tests that print `skipped: OCR models not installed` are **not** evidence that recognition ran. Set `PDFCRAFT_MODELS_REQUIRED=1` (Linux CI does, after `cargo xtask models`) to turn that skip into a failure (`Models::required_for_tests`), so a passing run means the actual models ran.

## Permissions

Recognition copies a document's content (the recognized words) and adds a text layer, so it needs both the copy permission used by text extraction (`Document::allows_copying`) and the modification permission used by other content edits (`Document::allows_modification`). Opening a restricted encrypted file with its permissions (owner) password lifts both. `Session::ocr_authority` checks them before a job is built, so nothing is rendered and no model runs for a restricted document. `validate_ocr_review` checks them again, against the document as it is now, whenever a draft is shown, corrected or applied, before checking the revision. Automation discards a draft that fails these checks without returning its words; the UI discards it without showing them.

## Review workflow

- `Session::ocr_job_checked` validates the language, resolution, selected pages and permissions before rendering or loading models.
- `OcrJob::run` produces an `OcrReview`. It never mutates the document. Returning `false` from any progress callback cancels the entire draft, including completed pages. Inference currently finishes before cancellation is observed; it is not interruptible inside a model call.
- `OcrReview::correct` changes a word or omits it when empty, preserving its placement. Corrections have per-word, per-page and per-draft limits.
- `Session::apply_ocr_review` rejects a closed or changed document, including edits followed by undo. Accepted results create one undoable edit. Save remains incremental.
- The lower-level `apply_ocr`/`Edit::AddOcrText` APIs also validate public word geometry and text before generating PDF syntax. The invisible layer uses `q`/`Q` so its text rendering mode cannot affect later content.

Recognition images are limited to 40 million pixels and 16,384 pixels per side. Resolution must be finite and 72–600 dpi. Packed gray/RGB/RGBA lengths are checked before allocating conversions; RGBA input is composited over white for inference. A job accepts at most one million words / 64 MiB of text, with additional per-page and per-word bounds. Oversized pages are reported as skipped with a reason; they are not silently shrunk. Backend panics are contained and produce no committed text.

## Headless control

`ocr_prepare` creates a draft; `ocr_review` returns words, geometry and skip reasons, or an error (discarding the draft) when the document changed, closed or no longer allows copying and changes; `ocr_correct` changes one 1-based page/word; `ocr_apply` commits once; `ocr_cancel` discards it. At most eight drafts, two million recognized words and 128 MiB of text may be retained together in an automation session. Batch recognition (`ocr_recognize_files`, the UI's multiple-files command) cannot be canceled yet and reads each input file whole. Closing a document releases its draft. Existing `ocr_recognize` remains a synchronous recognize-and-apply operation, and batch recognition continues to write searchable copies.

## Remaining OCR work

This workflow does not complete Acrobat OCR parity. Automatic orientation detection, deskew, image cleanup, confidence-based suspects, correction of an already-saved OCR layer, arbitrary Unicode text layers, editable-text reconstruction, multilingual recognition, scanner capture and MRC compression remain separate tasks. Existing PDF `/Rotate` and crop geometry are respected when placing recognized words; that is not automatic orientation detection.

For multilingual work, both the recognition model and searchable-text font/encoding must support the target script. An independently licensed model with verified trained alphabets, reproducible original fixtures and a compatible Rust/WASM runtime is required. Tesseract and its official trained data offer an Apache-2.0 native reference/backend option, but a system executable alone does not satisfy this application's Rust/WASM deployment. Paddle-family models require model-specific license and runtime compatibility verification. No new model or backend is bundled by this change.

## Tests

- `crates/ocr/src/lib.rs`: hostile dimensions/channel lengths, transparent input, word encoding/geometry and balanced invisible text state.
- `crates/engine/tests/ocr_review.rs`: all cancellation boundaries, stale authority, correction/save/reopen pixel equality, one undo, rotated page mapping, oversized pages and panic containment using a deterministic test recognizer; encrypted fixtures made with the engine's own Protect (copy denied, modify denied, both, owner password, a draft whose permissions or revision later change) asserting refusal before the counting recognizer runs and no data or history change.
- `crates/automation/tests/ocr_review.rs`: real-model review/correct/cancel/apply/save/reopen, unsupported corrections, stale drafts and output root confinement; the same encrypted cases through `doc_protect`/`doc_save` for every OCR tool, including batch recognition, owner password and a later security change.
- `crates/ui-egui/tests/ocr.rs`: actual recognition in the shell, review/apply, corrected word navigation, Escape cancellation and stale-document refusal.
