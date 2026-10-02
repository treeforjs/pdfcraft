# PrintCraft roadmap

The milestones, current progress and time estimates to Acrobat Pro feature parity. Keep this up to date:
- **Every session:** update the progress column and add a line to the log.
- **Every milestone:** re-estimate.

Detailed task lists and acceptance tests are in `plan/execution-plan.md` (local-only). This file is the public summary.

**What "parity" means here:** the offline feature set of Acrobat Pro, milestones M0–M14. It excludes Adobe's cloud services (Document Cloud storage, Adobe Sign, the Adobe AI Assistant). Those have no clean-room equivalent; PrintCraft's alternatives are local-first, plus opt-in providers (M13).

## Estimate summary

The unit is **wall-clock hours of agent work**: Claude Opus-class models coding continuously, with a person reviewing at milestone boundaries. Time spent waiting for that review is not included.

| Scenario | Hours to parity (M0–M14) | Continuous calendar time |
|---|---|---|
| One agent, 24/7 | **2,000–4,000 h** | ≈ 3–6 months |
| 3–5 agents in parallel on separate crates after M4 | **800–1,500 h** | ≈ 5–9 weeks |
| Realistic, with human review, integration and pauses | — | ≈ 4–9 months (see `plan/execution-plan.md` §9) |

**How these numbers are built:**
- **Size:** about 450–700k lines of Rust at parity, including tests. The repo has about 15k today.
- **Rate:** so far, about 1.0–1.5k lines of *kept, tested* code per agent-hour. That includes debugging against the oracles (qpdf, poppler, pdf.js corpus).
- **Why the total is 3–6× the raw typing time:** the hard work is not the line count. Most of the time goes into fidelity work: text editing and reflow, font embedding, redaction that really removes content, signature validation, PDF/A and PDF/UA, XFA, OCR and web performance. Each of these needs repeated oracle checks and visual comparison.
- **Parallelism is limited early:** M0–M4 are mostly sequential, because the core crates must settle first. Extra agents pay off from M5 onwards.
- **The long tail:** the last 5–10% (odd real-world files, pixel-level polish) costs about as much as the first 50%.

## Milestones

Hours are for a single agent (low–high). "Done" is the estimated fraction of that milestone's *acceptance criteria* that are met. It is not a count of lines of code.

| M | Milestone | Est. hours | Done | Remaining (h) | Notes |
|---|---|---|---|---|---|
| M0 | Skeleton: workspace, xtask gates, CI | 15–30 | 92% | 2–4 | GitHub workflow, `deny.toml` (licence audit of every dependency), parity checklist (826 features, `xtask parity`) done. Missing: remaining crate stubs, testkit/oracle crates |
| M1 | COS: filters, crypt, parser, xref, writer | 120–200 | 72% | 35–60 | Done:<br>- filters and crypt: every standard-security revision R2–R6 (RC4, AES-128/256), SASLprep, permissions, creating encryption;<br>- cos: parse and repair, decrypt on load, re-encrypt on save, incremental and full writing.<br>Corpus: open/edit/save passes on 958 files, and all 7 password-protected files open.<br>Full saves now pack objects into compressed object streams. Fuzzing runs nightly (`xtask fuzz`). Missing: ≥ 250 tests, own image codecs |
| M2 | Model, render, text | 200–350 | 15% | 170–300 | hayro bootstrap renderer (vendored patches). Text extraction reaches word-F1 0.98 against pdftotext. Missing: model crate, fonts, DisplayList, renderer independent of hayro |
| M3 | Viewer app (native + web) | 80–150 | 72% | 22–42 | Acrobat-style shell, find, select, panels, tiles, web build, UI control channel for agents (opt-in). Missing: 60 fps test on a 500-page document, snapshot tests of every panel |
| M4 | Engine, history, save, organize | 100–180 | 80% | 20–36 | Done:<br>- command registry (menus, shortcuts and palette all use it);<br>- undo/redo; incremental, atomic and encrypted saves;<br>- autosave and crash recovery;<br>- organize, combine, extract, split and insert-from-file, with identical fonts and images stored once;<br>- CLI `edit/combine/extract/split`.<br>Done since: bookmark editing, page labels (Number pages), CLI `run`. Missing: page boxes/crop, recovery on the web |
| M5 | Comments (all annotation types, XFDF) | 120–200 | 40% | 70–120 | Done: notes, highlight/underline/strikeout/squiggly, text boxes, ink, lines, arrows, rectangles, ovals, with appearance streams; replies, status, move/resize/restyle/delete; quick-bar tools and Comments panel; agent tools. Missing: callouts, clouds, polygons, stamps, carets, FDF/XFDF, summaries, flatten |
| M6 | Forms + JavaScript | 160–320 | 15% | 135–270 | Done: filling text, check box, radio, combo and list fields with regenerated appearances, Tab order, Clear form, agent tools. Missing: JavaScript (AF functions, events), form authoring, FDF/XFDF data |
| M7 | Content editing (text, images, header/footer, watermark) | 250–500 | 0% | 250–500 | Longest pole |
| M8 | Security + redaction | 100–180 | 25% | 75–135 | Done: opening protected documents, honouring permissions, Protect Using Password (open and permissions passwords, all compatibility levels, Advanced options), Remove security. Missing: certificate security, sanitize, redaction |
| M9 | Signatures (PAdES, validation) | 160–280 | 0% | 160–280 | |
| M10 | OCR, create, export, print | 200–350 | 0% | 200–350 | |
| M11 | Optimize, preflight, PDF/A/X/UA, print production | 200–350 | 0% | 200–350 | |
| M12 | Accessibility, compare, measure, search, XFA | 200–380 | 0% | 200–380 | |
| M13 | Automation (MCP, Action Wizard, CLI) + AI providers | 60–120 | 22% | 45–95 | Done: headless tool table (38 tools: pages, bookmarks, labels, comments, forms, protection…), opt-in MCP server over stdio, CLI `run`/`tools`, UI control channel with drag. Missing: Action Wizard, more tools as features land, AI providers |
| M14 | 1.0 polish: performance, localization, installers | 120–250 | 0% | 120–250 | |
| | **Total** | **2,085–3,840** | **≈ 13%** | **≈ 1,800–3,350** | |

**Overall progress: about 13% of the effort.** The viewer and the core are far ahead of the editing features, because the viewer was built first so progress could be seen.

## Critical path

M0 → M1 → M2 → M3 → M4 must happen in order. After M4, M5–M12 can run in parallel across crates. The long poles are M7 (content editing), M9 (signatures) and M12 (XFA).

## Risks most likely to push estimates up

- Fidelity of text editing: fonts, subsets, reflow.
- XFA dynamic layout.
- Real-world signature chains and revocation checks.
- The rendering long tail: Type3 fonts, broken fonts, shadings.
- Correctness of PDF/A and PDF/UA conversion.
- CPU rendering performance on the web.

## Log

Newest first. One line per session: the date, what moved, and the new overall percentage.

- **2026-10-01 (session 8):**
  - Commenting: sticky notes, highlights, underline, strikethrough, text boxes, freehand, lines, arrows, rectangles and ovals, with replies, status, colours, moving and resizing; Acrobat-style quick bar and Comments panel; six agent tools (an agent can highlight a phrase by naming it).
  - Protect Using Password, with Acrobat's permission levels and compatibility options; Remove security.
  - Filling in forms: text, check boxes, radio buttons, combo and list boxes, Tab between fields, Clear form; three agent tools.
  - Performance: opening a 190 MB manual went from 172 s to under 2 s; comment and form edits on it are 6× faster.
  - Fixed: saving a password-protected document failed; encrypted documents were not reported as encrypted.
  - Overall ≈ 13%.

- **2026-10-01 (session 7):**
  - Bookmark editing and page numbering (Number pages), with undo, agent tools and UI.
  - A render watchdog: pathological pages are skipped after 20 s instead of spinning forever.
  - The agent control channel no longer reports success when the window is hidden.
  - Community links: Discord button in the title bar, plus Home, About, Help menu, CLI and README.
  - Overall ≈ 10%.

- **2026-10-01 (session 6):**
  - UI control channel: agents can inspect the widget tree, click, type, press keys, run commands and take screenshots of the running app. Off unless the app is started with `--control`; loopback only, token-authenticated.
  - Combine and insert store identical fonts, images and other resources once. 40 copies of the showcase: 130 MB → 3.9 MB, 3× faster, pixel-identical.
  - Full saves use compressed object streams (showcase 3.4 → 2.8 MB; the 40× combine is now 2.3 MB). Fixed opening encrypted files whose catalog is compressed, and a crash on looped page trees.
  - Fuzzing (`cargo xtask fuzz`, nightly in CI): about 150,000 mutated files tried. Seven crash and hang bugs found and fixed, each with a regression test: two in our code, five in the temporary renderer.
  - Parity checklist: 826 Acrobat Pro features tracked in `parity/acrobat-features.toml`; `cargo xtask parity` checks every claim against code and tests. 11.8% shipped overall, 31.6% of P0.
  - Overall ≈ 9.5%.

- **2026-09-30 (session 5, after a machine crash; no work lost):**
  - Agent control: new `printcraft-automation` crate with 20 JSON-Schema tools, an opt-in MCP server (`printcraft-cli mcp`, stdio only, can be compiled out), and `printcraft-cli run`/`tools`.
  - Text on rotated pages now reads along its lines; the pdf.js oracle is unchanged at median 0.980.
  - Finding text in 520 pages: 17.9 s → 3.7 s (parallel), about 20 ms when repeated (cached).
  - CI: dependency licence audit (`deny.toml`, `xtask deny`) and a GitHub workflow for macOS, Windows, Linux and wasm.
  - Overall ≈ 8%.

- **2026-09-30 (session 4):**
  - Combine, extract and split, with links, named destinations, fields, layers, attachments and nested bookmarks all carried over. Combined pages render pixel-identical to their sources.
  - Fixed a precision bug in how reals were written.
  - Encryption: a new `crypt` crate covering R2–R6. Encrypted documents can be opened, edited and saved; permissions are honoured; the Security tab is real. Checked against hayro and qpdf.
  - Autosave and crash recovery.
  - Command registry.
  - Asset policy (`AGENTS.md`, `ATTRIBUTION.toml`, `xtask assets`) and a README with 13 reproducible screenshots (`xtask screenshots`).
  - Overall ≈ 7%.
- **2026-09-30 (session 3):**
  - New crates: `filters` (every non-image filter, 57 tests) and `cos` (object parser, xref reader, repair, incremental and full writer).
  - New `organize` crate: page operations and info edits.
  - Engine: editing with undo/redo and save. CLI: `edit`.
  - UI: organize toolbar with multi-select, ⌘Z/⇧⌘Z/⌘S/⇧⌘S, Edit menu, editable Description properties, a dot on tabs with unsaved changes, and a save prompt on close and quit. 13 new kittest tests.
  - Corpus: open, edit and save round-trip on 946 of 951 files. 182 of 182 sampled saved outputs pass `qpdf --check`.
  - Overall ≈ 5%.
- **2026-09-30 (session 2):**
  - Robustness sweep (963 of 983 files open, 0 crashes), text layer, find and select, tiles, web build, polish.
  - Overall ≈ 3–4%.
- **2026-09-30 (session 1):** planning complete; viewer vertical slice.
