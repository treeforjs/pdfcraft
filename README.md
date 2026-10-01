<div align="center">

# PrintCraft

**The open-source PDF workbench.**<br>
Read, organize, combine, split and secure PDFs in a fast, native app, written in Rust from the ground up.<br>
macOS · Windows · Linux · the web

<sub>From the artcraft team · <a href="#crafting-apps">part of the Crafting Apps family</a></sub>

<br>

<img src="docs/images/printcraft-viewer.png" alt="PrintCraft showing a typography showcase PDF, with the All tools panel on the left and threaded comments on the right" width="100%">

<br>

[Highlights](#highlights) ·
[Read](#read-anything-beautifully) ·
[Organize](#organize-pages-like-cards-on-a-table) ·
[Combine &amp; split](#combine-and-split-without-losing-a-thing) ·
[Protect](#open-protected-documents-and-respect-their-rules) ·
[Everywhere](#runs-everywhere-stays-yours) ·
[Get started](#get-started) ·
[Roadmap](ROADMAP.md)

</div>

---

## Highlights

<table>
<tr>
<td width="33%" valign="top">

### Faithful
Real-world typography: world scripts, vertical Japanese, colour emoji, gradients, soft masks and transparency. All of it renders the way the author intended.

</td>
<td width="33%" valign="top">

### Fearless
Every save appends your changes and leaves the original bytes untouched. Writes are atomic, undo runs deep, and nothing is lost if you close by mistake.

</td>
<td width="33%" valign="top">

### Yours
No account, no telemetry, no cloud. It works offline and opens instantly. The engine, CLI and app are all open source.

</td>
</tr>
</table>

---

## Read anything, beautifully

PrintCraft renders PDFs with care for the details that make a page feel right: kerning and ligatures, right-to-left and complex scripts, vertical CJK, colour emoji, shadings, blend modes, soft masks and optional content.

<img src="docs/images/printcraft-scripts.png" alt="A page of world scripts — Arabic, Hebrew, Devanagari, Thai, Greek, Cyrillic, Chinese, Korean, IPA, Armenian, Georgian, Tamil and vertical Japanese — rendered crisply" width="100%">

- **Deep zoom stays sharp.** Large pages render in tiles, so text stays crisp at any magnification.
- **Built to survive bad files.** Every page renders in isolation and damaged documents are repaired. Across the 983-file pdf.js test corpus the result is 0 crashes.
- **Layouts for every task:** continuous, single page, two-up, view rotation, full screen and a distraction-free Read mode.
- **Light and dark themes**, both designed to be easy on the eyes for long sessions.

<table>
<tr>
<td width="50%"><img src="docs/images/printcraft-twoup.png" alt="Two-up reading in Read mode with the dark theme"></td>
<td width="50%"><img src="docs/images/printcraft-dark.png" alt="The dark theme with the comments panel"></td>
</tr>
<tr>
<td align="center"><sub>Two-up Read mode</sub></td>
<td align="center"><sub>Dark theme</sub></td>
</tr>
</table>

## Find it, select it, copy it

Search the whole document as you type, step through matches with <kbd>⌘G</kbd>, and select text that comes out in the right reading order. That holds for columns, right-to-left runs and CJK too.

<img src="docs/images/printcraft-find.png" alt="Find bar showing match 10 of 16 for the word 'type', highlighted on the page" width="100%">

## Navigate long documents

Bookmarks, page thumbnails and the document's own page labels (i, ii, 1, 2…) keep you oriented in long documents.

<img src="docs/images/printcraft-bookmarks.png" alt="The bookmarks panel showing a nested outline next to a page of world scripts" width="100%">

---

## Organize pages like cards on a table

Open **Organize pages** to see every page at once:
- **Select pages:** click, <kbd>⌘</kbd>-click or <kbd>⇧</kbd>-click.
- **Change them:** rotate, delete, insert blank pages, insert pages from another file, and move them earlier or later.
- **Undo anything:** <kbd>⌘Z</kbd>, then save.

<img src="docs/images/printcraft-organize.png" alt="The Organize pages grid with three pages selected and the page toolbar above" width="100%">

<table>
<tr>
<td width="50%" valign="top">

**Undo that goes the distance.** Each change is one step in a history you can walk backwards and forwards. The Edit menu names the step ("Undo Rotate pages"), and undo still works after you save.

**Saves you can trust:**
- *Incremental:* the original bytes stay byte-for-byte intact.
- *Atomic:* the file is written to a temporary copy, then swapped in.
- *Verified:* independently checked with qpdf.

Unsaved documents carry a dot on their tab, and closing or quitting asks before anything is lost. Changes are autosaved every minute. If PrintCraft ever quits unexpectedly, it offers to recover your work the next time it opens. Encrypted documents stay encrypted on disk.

</td>
<td width="50%"><img src="docs/images/printcraft-split.png" alt="The Split document dialog over the organize view"></td>
</tr>
</table>

## Combine and split without losing a thing

**Combine files** merges any number of PDFs into one. Each file gets a bookmark, with its own bookmarks nested underneath.

**Extract** copies the pages you select into a new document. **Split** divides a document every *n* pages, or before the pages you choose.

Nothing quietly disappears along the way:
- links and named destinations are rewired to the copied pages;
- form fields stay interactive;
- layers keep their on/off defaults;
- attachments come along.

Every page of a combined document renders pixel-identical to its source.

```sh
printcraft-cli combine report.pdf appendix.pdf --out combined.pdf
printcraft-cli extract report.pdf --pages 1,3,5 --out highlights.pdf
printcraft-cli split   report.pdf --every 10 --out-dir parts/
```

---

## Open protected documents and respect their rules

PrintCraft implements the PDF standard security handler completely:
- every revision, from 40-bit RC4 to AES-256;
- user and owner passwords, including Unicode passwords normalised with SASLprep;
- crypt filters and attachment-only encryption.

Documents restricted by their author show a clear notice, and PrintCraft honours their permissions. Enter the owner password and the restrictions lift. Edits to encrypted documents are saved encrypted, under the same keys.

<table>
<tr>
<td width="50%"><img src="docs/images/printcraft-properties.png" alt="Document Properties with editable title, author, subject and keywords"></td>
<td width="50%" valign="top">

**Document Properties** shows:
- the document's title, author, subject and keywords, which you can edit;
- the fonts it uses and whether each is embedded;
- PDF version, page size, tags, fields, layers and attachments;
- the full security picture: encryption method, which password opened it, and each permission.

</td>
</tr>
</table>

## Comments, forms, layers and attachments

<table>
<tr>
<td width="50%"><img src="docs/images/printcraft-forms.png" alt="An interactive form with highlighted fields and the Fields panel listing every field and value"></td>
<td width="50%"><img src="docs/images/printcraft-layers.png" alt="A review page with markup and a DRAFT watermark layer, and the Layers panel"></td>
</tr>
<tr>
<td valign="top"><b>Forms</b>: every field with its current value, field highlighting, and checkboxes, radio buttons, lists and signatures drawn the way their author designed them.</td>
<td valign="top"><b>Layers</b>: switch optional content on and off and the page re-renders instantly. <b>Comments</b> appear as threaded conversations, and <b>attachments</b> can be opened or saved.</td>
</tr>
</table>

## Every tool, one keystroke away

Press <kbd>⌘K</kbd> to search every tool and command, or browse the **All tools** catalogue. Tools that are still in development are marked with the milestone that will ship them.

<table>
<tr>
<td width="50%"><img src="docs/images/printcraft-palette.png" alt="The command palette searching for page tools"></td>
<td width="50%"><img src="docs/images/printcraft-tools.png" alt="The home screen with recommended tools and the full tool catalogue"></td>
</tr>
</table>

---

## Runs everywhere, stays yours

- **Native on macOS, Windows and Linux**, and **in the browser** through WebAssembly, from the same Rust codebase.
- **Private by design.** Documents never leave your machine. There's no account, no telemetry and no cloud processing.
- **Engine first.** Parsing, rendering and editing live in reusable library crates. The interface is one swappable layer on top.
- **Scriptable.** The `printcraft-cli` tool covers inspecting, rendering, extracting text, editing, combining, extracting pages and splitting. Robustness sweeps run on the same engine as the app.

```sh
printcraft-cli info  form.pdf                                  # structure as JSON
printcraft-cli text  paper.pdf --page 3                        # reading-order text
printcraft-cli edit  in.pdf --rotate 1,2:90 --delete 5 --title "Q3" --out out.pdf
```

---

## How it's built

PrintCraft is a Cargo workspace of focused crates, layered so the core never depends on the UI:

| Crate | What it does |
|---|---|
| `printcraft-filters` | Every PDF stream filter (Flate, LZW, ASCII85, RunLength, predictors), encode and decode, property-tested |
| `printcraft-crypt` | The standard security handler: RC4, AES-128/256, revisions 2–6, permissions |
| `printcraft-cos` | The PDF object layer: tolerant parsing, repair, copy-on-write edits, incremental and full writing |
| `printcraft-organize` | Page operations, combine / extract / split, document information |
| `printcraft-render` | Rendering, inspection and text extraction with reading order |
| `printcraft-engine` | The façade every frontend uses: sessions, edits, undo, saving, the tool catalogue |
| `printcraft-ui-egui` | The desktop and web interface |

**Quality gates.** Every change passes the same automated checks:
- formatting, and Clippy with warnings as errors;
- 200+ unit, property and UI tests;
- crate-layering rules and a WebAssembly build check;
- an asset-licence audit.

On top of those, two corpus sweeps run over real-world files:
- **Opening and rendering:** of the 983 pdf.js test files, 963 open and render cleanly, with 0 crashes.
- **Open, edit and save round trips:** 958 succeed.

The output is verified with independent tools: hayro, qpdf and poppler.

PrintCraft is a clean-room implementation. Its behaviour comes from the ISO 32000 specification and black-box observation, never from anyone else's code. Every icon, font and image is openly licensed and listed in [ATTRIBUTION.md](ATTRIBUTION.md).

## Get started

```sh
git clone https://github.com/storytold/printcraft
cd printcraft
cargo run --release -p printcraft -- some.pdf     # desktop app
cargo xtask demo-pdf                              # build the showcase PDF used in these screenshots
cargo xtask screenshots                           # regenerate every screenshot in this README
```

## What's next

PrintCraft is young and moving fast. **Available today:**
- viewing, search and navigation;
- organizing pages, combining, extracting and splitting;
- document information;
- opening encrypted documents, honouring their permissions, and saving them encrypted;
- undo and safe saving;
- autosave with crash recovery;
- a single command registry behind menus, shortcuts and the palette.

**On the roadmap:**

| Next up | Milestone |
|---|---|
| Bookmark and page-label editing, page boxes | M4 |
| Creating and editing comments, stamps, FDF/XFDF | M5 |
| Filling and authoring forms, JavaScript | M6 |
| Editing text and images in place, headers, watermarks | M7 |
| Adding passwords, redaction | M8 |
| Digital signatures (PAdES) | M9 |
| OCR, export to Office formats, printing | M10 |
| Optimize, preflight, PDF/A | M11 |
| Accessibility, compare, measure | M12 |

The full plan, with progress and estimates, is in **[ROADMAP.md](ROADMAP.md)**.

---

## Crafting Apps

Open-source, clean-room, pure-Rust creative applications. Each is native on macOS, Windows and Linux, runs on the web, and can be driven by agents.

<table>
  <tr>
    <td width="20%" align="center" valign="top">
      <a href="https://github.com/storytold/photocraft"><b>PhotoCraft</b></a><br>
      <sub>Image editing</sub>
    </td>
    <td width="20%" align="center" valign="top">
      <a href="https://github.com/storytold/drawcraft"><b>DrawCraft</b></a><br>
      <sub>Vector illustration</sub>
    </td>
    <td width="20%" align="center" valign="top">
      <a href="https://github.com/storytold/filmcraft"><b>FilmCraft</b></a><br>
      <sub>Video editing</sub>
    </td>
    <td width="20%" align="center" valign="top">
      <a href="https://github.com/storytold/lightcraft"><b>LightCraft</b></a><br>
      <sub>Photo library &amp; raw</sub>
    </td>
    <td width="20%" align="center" valign="top">
      <a href="https://github.com/storytold/printcraft"><b>PrintCraft</b></a><br>
      <sub>PDF documents</sub>
    </td>
  </tr>
  <tr>
    <td valign="top"><sub>Layered raster editor with byte-exact PSD/PSB round trips and 13 formats at 8/16/32-bit. A Photoshop-class workflow.</sub></td>
    <td valign="top"><sub>Vector illustration with the Illustrator-style tools, panels, menus and shortcuts people already know.</sub></td>
    <td valign="top"><sub>Non-linear video editor with frame-exact timing and its own codecs. Aims at Premiere Pro parity.</sub></td>
    <td valign="top"><sub>Photo library and non-destructive raw developer: its own raw decoders, a wide-gamut float pipeline and a local-first catalog.</sub></td>
    <td valign="top"><sub>View, organize, annotate, fill, sign and edit PDFs. Aims at Acrobat Pro parity. <i>You are here.</i></sub></td>
  </tr>
</table>

## Licence

MIT OR Apache-2.0 ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)). Every icon, font and image is openly licensed and listed, with its author and source, in [ATTRIBUTION.md](ATTRIBUTION.md). The policy is in [AGENTS.md](AGENTS.md), and required notices are in [NOTICE](NOTICE). Contributors and agents: read [AGENTS.md](AGENTS.md) and [CLAUDE.md](CLAUDE.md).

<sub>Adobe, Acrobat, Photoshop, Illustrator, Premiere Pro and Lightroom are trademarks of Adobe Inc. PrintCraft is an independent project, not affiliated with or endorsed by Adobe.</sub>
