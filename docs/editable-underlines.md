# Editable underline associations

PDF draws underlines as ordinary paths; geometric overlap does not identify which text owns a
line. PdfCraft therefore preserves unmarked paths and gives newly authored editable underlines
explicit private marked-content associations. This convention is contributor-original and does
not change PDF rendering or require a PdfCraft viewer.

Each underlined glyph has a unique positive page-local integer ID in two marked-content objects:

```text
/PdfCraftUnderlineText << /Version 1 /ID 17 >> BDC
(A) Tj
EMC

/PdfCraftUnderlinePath << /Version 1 /ID 17 >> BDC
q
1 0 0 1 72 700 cm
0 0 0 RG
0.72 w
0 J
[] 0 d
0 -1.2 m
8 -1.2 l
S
Q
EMC
```

Text state and placement precede the text marker. Decoration paths are outside `BT`/`ET` and
restore their graphics state. Paths use the actual font descent, text advance, rise and transform;
their stroke color follows the supported device text color. Inferred spaces receive an encodable
space when needed so their decoration retains an association. IDs are allocated above all existing
version-1 IDs on the page, with checked arithmetic.

The editor validates marker structure, a single text object and a single matching path per ID,
the exact supported path operator sequence, operand counts and finite coordinates. Missing,
duplicated, nested malformed or unknown-version text associations refuse editing. Parsing joins
content stream pieces before checking associations. Nested malformed ranges are merged with a
single subsequent operator scan; they do not cause repeated scans of inner bodies.

Paragraph replacement removes only the validated text/path pairs belonging to the replaced
operators, then emits new associations for retained underline styles. Ordinary overlapping artwork
and other paragraphs remain intact.

## Deliberate refusals

- **Active path.** An unpainted current path before a paragraph (for example `m … l` with its `S`
  after the text) prevents *every* rewrite of that paragraph, not only underline changes: `q`/`Q`
  does not save the current path, so any inserted drawing or reordering could change what the
  later painting operator paints. This is conservative; once the path is painted or ended with `n`
  before the text, editing works.
- **Legacy line editing.** `text_edit` with `line` (from `text_lines`) refuses lines that carry
  authored underlines, or untrusted associations, with "use paragraph editing to preserve this
  line's editable underlines". This is an interim boundary, not line-edit parity.
- **Untrusted associations.** Unknown versions, non-positive or non-integer IDs, property lists
  given by name, a marker body other than one `Tj`, unterminated markers, duplicate text or path
  IDs, missing paths and any deviation from the exact path sequence never read as underlined, and
  every edit of that paragraph refuses unchanged. A path marker without text is kept as artwork,
  and new IDs are allocated above it. When the largest ID is already used, adding an underline
  refuses ("exhausted underline association IDs"); edits that add none are unaffected.
- **Output size.** Run-preserving edits place every glyph explicitly: each glyph replays its text
  state (8–9 operators), adds `Tm`/`Tj`, and an underlined glyph adds 2 markers and a 12-operator
  marked path. Preparation counts this upper bound per glyph and refuses the whole edit before
  emitting anything once it would exceed 2,000,000 operators (roughly 200,000 plain or 83,000
  underlined characters), with an error asking for smaller paragraphs. Text is never truncated.

## Verified behavior (crates/edit/src/tests.rs)

- `underlines_keep_tj_adjustments_logical_spaces_and_exact_positions`: TJ kerning in both
  directions and a code-less logical space (from a large TJ gap) keep every glyph's exact x
  position through underline on, partial off and off; the logical space gains an encodable space
  to carry its association, and each path spans its glyph's advance including the adjustment.
- `wrapped_and_partially_restyled_underlines_keep_one_path_per_glyph`: a selected underline that is
  partly re-fonted (bold) and rewrapped keeps one path per underlined glyph, on each glyph's row.
- `underline_metadata_survives_split_streams_and_does_not_change_shared_pages`: markers whose
  tokens are spread over many content streams, and two pages sharing one content stream with
  inherited resources (only the edited page changes).
- `forged_or_malformed_underline_markers_refuse_atomically`,
  `orphan_and_nested_markers_are_safe_and_id_exhaustion_refuses`,
  `editable_underline_associations_refuse_missing_duplicate_and_unrecognized_paths_atomically`,
  `an_active_path_refuses_every_paragraph_rewrite_conservatively`,
  `line_editing_refuses_authored_underlines_with_a_paragraph_hint`,
  `run_preserving_edits_refuse_operator_expansion_before_emitting_it`.
- The narrowed fake-bold copy removal keeps the fake-bold cases (`a_line_drawn_twice_is_replaced_everywhere`)
  and overlapping independent labels
  (`character_formatting_never_removes_an_overlapping_independent_text_object`,
  `editing_an_overlapping_label_preserves_an_independent_underlined_run`) passing.

## Not covered

Rotated, skewed or reflected text refuses mixed-run editing, so it cannot be underlined. Stroked,
invisible or clipping text (Tr ≠ 0) and ExtGState (`gs`) or explicit clipping refuse too. Path
geometry sits at half the font descent below the baseline with width 6% of the size; it does not
read font underline metrics.

This is an editing convention, not cryptographic provenance. External editors may preserve it,
remove it, or invalidate it. It does not identify arbitrary imported underline paths or earlier
PdfCraft underlines, and does not claim structure-tag preservation or transformed-text parity.
