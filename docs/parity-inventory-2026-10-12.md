# PdfCraft parity inventory — 2026-10-12

Scope: the offline Acrobat Pro workflows in the committed parity manifest. Adobe cloud services are excluded. This inventory records implementation labels, not proven equivalence.

The starting point has **830 entries: 420 shipped, 73 partial, 314 planned, and 23 not applicable**. Of the 807 in-scope entries, 387 remain partial or planned. 141 shipped entries cite exactly one test; 96 have no automation-tool link. Missing links require investigation rather than proving the feature is inaccessible.

`cargo xtask parity` validates identifiers and evidence references. It does not establish that a cited function ran, tests all expected behavior, or matches a reference implementation. Full parity requires executable workflow, interoperability, visual, accessibility, performance, and platform evidence.

| Area | Shipped | Partial | Planned | Out of scope |
|---|---:|---:|---:|---:|
| A: core | 42 | 2 | 12 | 0 |
| B: view | 52 | 11 | 29 | 0 |
| C: edit | 29 | 7 | 25 | 1 |
| D: organize | 44 | 3 | 29 | 0 |
| E: comment | 46 | 9 | 10 | 4 |
| F: form | 58 | 8 | 13 | 3 |
| G: protect | 22 | 1 | 18 | 1 |
| H: sign | 22 | 10 | 12 | 3 |
| I: ocr | 5 | 2 | 19 | 1 |
| J: create | 12 | 3 | 19 | 1 |
| K: optimize | 15 | 10 | 33 | 0 |
| L: print | 15 | 1 | 23 | 0 |
| M: a11y | 38 | 1 | 22 | 0 |
| N: misc | 20 | 5 | 50 | 9 |

## Work queue and acceptance

1. **Existing text:** preserve mixed source runs, character codes and spacing; format selected Unicode ranges; render a nonmutating draft preview; reflow, undo/redo, and save/reopen. Continue with transformed text, run decorations, robust paragraph identity, tagged content, find/replace, broader fonts and shaping.
2. **Rendering and document foundations:** replace bootstrap renderer dependencies through the model/display-list path; validate transparency, patterns, Type 3/CID fonts and codecs; finish lazy loading; keep fuzzing, repair reporting and resource budgets observable.
3. **OCR and conversion:** multilingual and accented recognition, correction and editable output, image preprocessing, Office import, and layout-faithful Word/Excel/PowerPoint export; evaluate original synthetic fixtures and public format specifications.
4. **Signatures and standards:** complete timestamp/LTV and validation workflows, revision and certificate UX, PDF/A/X/UA conformance checks and fixes, printing/production options; use independent external validators for each claimed profile.
5. **Forms and advanced documents:** wider JavaScript object model, XFA fidelity, comparison, layers/actions and structure preservation.
6. **Desktop completeness:** keyboard-only operation, screen-reader and locale audits, reliable web persistence, native printing and installers across supported platforms, measured latency/memory budgets.

Each checkpoint needs engine and headless automation, real controls where applicable, error/no-op/history tests, actual persisted output, and an external oracle where one exists. One passing synthetic fixture is not proof of general PDF or Acrobat conformance.

## Current evidence boundaries

- Rendering still uses hayro; parser/rendering/fidelity labels must not imply an independent completed renderer.
- No systematic Acrobat visual/behavioral comparison has been performed in this environment. Development uses public specifications and contributor-original synthetic PDFs; no Adobe execution, code, fonts or assets are used.
- The 2026-10-11 installed-app matrix covered Ubuntu 24.04 and 26.04, X11 and Wayland. It did not validate physical GPU drivers or Windows execution. New source needs fresh installed-app acceptance.
- Optional craft-fonts inputs are absent from the default local build. Tests that conditionally skip without those inputs do not establish multilingual font coverage.
- Existing text remains partial until arbitrary source encodings, transforms, layouts, decoration/structure preservation, and comprehensive Unicode editing are supported and independently checked.

The roadmap effort estimates remain historical estimates, not completion dates or evidence of full parity.
