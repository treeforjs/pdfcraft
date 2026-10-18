# pdfcraft-a11y

Layer L4: accessibility (execution plan M12.3). Today: the Accessibility Checker's full check.

```rust
let report = check(&doc, &Options::default());          // 31 of 32 rules (colour contrast off)
let html = report_html(&report, "file.pdf", "2026-10-02");
```

- **32 rules in 7 categories**, as in Acrobat's full check: Document (permission flag,
  image-only, tagged, reading order, language, title, bookmarks, colour contrast), Page Content
  (tagged content, tagged annotations, tab order, character encoding, tagged multimedia, and
  four manual checks), Forms (tagged fields, descriptions), Alternate Text (figures, nested,
  associated with content, hides annotation, other elements), Tables (rows, cells, headers,
  regularity with row/column spans, summary), Lists (items, Lbl/LBody) and Headings (nesting).
- Each result is Passed, Failed (with findings and pages), Needs manual check, or Skipped.
- **Content checks** walk the page content streams (and form XObjects) tracking marked
  content: painting outside MCID-marked or `/Artifact` content is untagged.
- **Structure checks** read the structure tree through the role map; table regularity counts
  `ColSpan`/`RowSpan` attributes.
- The report is a self-contained HTML page. Fixes that edit the document (language, title, tab
  order) live in the engine (`Document::accessibility_fix`).

- **PDF/UA** (`verify_pdfua`): the machine-checkable PDF/UA-1 and PDF/UA-2 requirements by
  Matterhorn checkpoint (`UA_CHECKPOINTS`); what needs a person is listed in `UA_NOT_CHECKED`.
- **PDF/A level a** (`pdfa_structure`, `verify_pdfa`, `pdfa_blockers`, `convert_pdfa`): the
  PDF/UA structure checks mapped to ISO 19005-1 §6.8 / ISO 19005-2 §6.7 clauses (MarkInfo,
  structure tree, tagged-or-artifact content, role mapping, Lang, figure alternate text;
  formulas warn), combined with `pdfcraft-preflight`'s level-b and Unicode rules. Conversion
  to level a never tags: it refuses with the findings until the document is tagged.

Not yet: autotagging, the Tags/Order/Content panels, the Reading Order tool (M12.1–M12.2).
