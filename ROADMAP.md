# PdfCraft roadmap

PdfCraft provides local document viewing, editing, forms, signatures, redaction, recognition and automation. `parity/features.toml` tracks implementation and evidence for offline PDF workflows. It is an inventory, not a conformance certificate or a completion estimate.

## Honest assessment — 2026-10-20

The current inventory has **797 entries: 435 shipped, 147 partial and 215 planned**. Hosted-service placeholders are outside this inventory. A shipped label means specific implementation and test references exist. The validation gate checks those references; it does not prove that a test ran, establishes general fidelity, or covers every platform.

- Rendering still uses the patched hayro bootstrap. The local COS/model layers and a future display-list renderer have not replaced every dependency path.
- Native release packages target Windows and Linux. Ordinary workspace CI currently runs on Linux; Windows execution, graphics drivers and platform integrations need independent acceptance.
- Existing-text editing retains supported glyph codes, source runs and spacing, but broader encodings, complex shaping, reflected/skewed text and layout fidelity remain incomplete.
- OCR's bundled recognizer is validated for English. Installed Tesseract is an optional external engine for supported Latin-script languages. A changed alphabet alone does not add trained recognition.
- Standards and archival-signature workflows report supported checks and limitations. A structural check or an embedded evidence store is not full PDF/A, PDF/X, PDF/UA or archival conformance.
- The app remains a pre-1.0 workbench. Claims must be supported by executable workflow, persisted-output, interoperability, visual, accessibility, resource and platform evidence.

## Where we're lacking and where we're going

1. **Hardening and recovery:** bound untrusted allocations and work; protect private files; retry failed autosaves; keep fuzz findings as synthetic regressions. Basic document JavaScript still needs complete memory/time isolation, the opt-in control transport needs connection and message budgets, and imported identity files need KDF work limits.
2. **Large-document foundations:** implement lazy byte sources and measurable memory budgets instead of reading every input file whole. Continue the model/display-list renderer and broad font/image/transparency fidelity checks.
3. **Editing existing content:** improve source-preserving paragraph identity, transforms, text state, complex shaping and RTL/CJK coverage; expose existing headless object/list tools in the GUI without overstating supported cases.
4. **Recognition and conversion:** multilingual models and Unicode text layers, correction of saved recognition, richer editable output and layout-faithful Office import/export. Use original synthetic fixtures and independent format readers.
5. **Signatures and standards:** trusted-time and evidence freshness, complete archival policies, hardware tokens, broader PDF/A/X/UA validators and explicit supported fixups.
6. **Desktop completeness:** full keyboard operation, screen-reader and locale audits, native-platform tests, reliable update/install paths and release-profile performance budgets.

## Evidence and milestones

Milestone areas remain M0 infrastructure, M1 document syntax/security, M2 model/rendering, M3 viewer, M4 document/history/organization, M5 comments, M6 forms/scripts, M7 editing, M8 redaction/protection, M9 signatures, M10 recognition/conversion/printing, M11 optimization/standards, M12 advanced documents, M13 automation and M14 polish.

`cargo xtask parity --partial` lists current boundaries. `docs/performance.md`, `docs/fidelity.md`, crate READMEs and manifest evidence describe narrower measurements. Historical throughput estimates are not release dates. Complete the high-priority gaps before adding peripheral features.

Before committing, run format, clippy, workspace tests, the browser check, asset provenance, parity validation and layering gates. Platform-specific and optional-input tests must say when they did not execute.

## Current maintenance checkpoint

The October audit identified an outdated embedded updater, unbounded repeated redaction overlays, unchecked 32-bit automation indices, private-file permission changes and recovery validation/retry defects. The maintenance patch addresses those bounded paths and removes unavailable catalog placeholders. Larger runtime isolation and missing feature work remain explicitly open.
