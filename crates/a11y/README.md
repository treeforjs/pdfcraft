# pdfcraft-a11y

Layer L4: accessibility (execution plan M12.3). Today: the Accessibility Checker's full check.

```rust
let report = check(&doc, &Options::default());          // 31 of 32 rules (colour contrast off)
let html = report_html(&report, "file.pdf", "2026-10-02");
```

- **32 rules in 7 categories**, as in the viewer's full check: Document (permission flag,
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

- **PDF 2.0 structure namespaces** (`namespaces`, `add_namespace`, `TagChange::namespace`):
  `/Namespaces` and `/NS`, namespace dictionaries (`/NS`, `/Schema`, `/RoleMapNS`), the PDF 1.7
  (default), PDF 2.0 and MathML namespaces. One resolver maps an element's type to a standard
  type across namespaces (bounded chains, cycle detection, missing namespaces reported) for the
  Tags listing, the full check, PDF/UA, reading order, figures and headings. Adding a namespace
  refuses mappings that are circular, end at a non-standard type, or remap PDF 2.0 types.

- **Autotag** (`autotag`): tags an untagged document from its layout. Each painting
  operation is a unit; a provisional copy puts each unit in its own MCID and the caller's text
  extractor ([`AutotagText`]; the engine passes the renderer's) returns glyphs per unit, so this
  crate only does layout: lines and blocks, headings by size and weight, lists, ruled tables,
  figures, links and annotations, running headers/footers/page numbers as artifacts, and a
  row/column reading order. The content is rewritten with balanced marked content and the tree,
  parent tree, StructParents, Tabs and MarkInfo are written. Bounded throughout; a tagged
  document is refused unless `replace_existing`. Form fields (`detect_fields`,
  `autotag_fields.rs`): each widget becomes a Form element (OBJR, StructParent) next to its
  label in reading order, with Alt from its tooltip, name or label; fields without a tooltip
  are reported with a suggestion, which `FieldTooltips::Fill` writes as `/TU`. Not yet:
  unruled tables, captions, vector figures, splitting a text run between elements.

- **Keep tags on edit** (`snapshot` + `keep_tags`, `pages_snapshot` + `drop_page_tags`, `keep.rs`):
  the engine snapshots a tagged page's marked content before a content edit and brings the tree
  back in step after it, in the same undo step. Content changed in place keeps its MCID (stale
  ActualText goes; changed figures are reported for an alt review); new untagged content takes
  over the MCID of emptied content of the same kind, else gets a new `P`/`Figure` next to the
  nearest tagged content; new vector-only content becomes an artifact; emptied or deleted
  content leaves its elements, empty elements are unlinked, and the page's parent-tree entry is
  rebuilt (deleted pages' entries removed). Content streams are spliced in place per stream.
  Returns a `TagUpdate` report. Not followed: form XObject content, edits other than text,
  images, added items and page deletion.

- **Reading Order tool and Order panel** (`tag_region`, `page_order`, `move_element`,
  `clear_page_structure`, `reading_order.rs`): a rectangle on a page (points from the top-left
  of the displayed page) is tagged as Text (`P`), H1–H6, Figure (with `/Alt`), Figure/Caption
  (`Figure` holding a `Caption`), Table (`Table` > `TR` > `TH`/`TD` from where the content lies,
  rules as layout artifacts), Cell (`TD`, in the row of the nearest cell), Formula, Form field
  (`Form` + OBJR + `/StructParent`) or Background (an artifact). Painting operations are inside
  when their box's centre is (text boxes from the caller's glyphs on a provisional copy, as
  Autotag). Untagged content gets new MCIDs; tagged or artifact marked content wholly inside
  changes as a whole; marked content cut by the rectangle is split (the outside part keeps its
  tag, element and first MCID). New sequences never cross `q`/`Q`, `BT`/`ET` or other marked
  content; emptied elements leave the tree; the new element goes beside the nearest tagged
  content before it; the parent tree is rebuilt; untagged documents get a tree. `page_order`
  lists a page's regions (outermost elements below a grouping element) in reading order with
  boxes and the elements inside them; `move_element` puts an element before or after another;
  `clear_page_structure` takes a page's tags (optionally keeping artifacts) out of the content,
  the tree and the parent tree. All or nothing, bounded. Not yet: content inside form XObjects
  (a form is tagged whole), splitting inside one text-showing operator, nested tagged content,
  table spans or header detection beyond the first row, the Order panel's drag UI.

Not yet: the Tags/Content panels' UI.
