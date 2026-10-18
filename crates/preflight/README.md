# Structural preflight

`analysis::analyze` inspects the current COS graph without changing it. The General and
Print profiles select supported checks for embedded font programs, additional actions,
image interpolation, annotation print flags, external streams, LZW compression,
transparency groups and embedded file specifications. Single checks can be selected.

PDF/A-2b and PDF/A-3b profiles add bounded XMP identification and encryption checks.
These profiles are **not full ISO conformance validators**. In particular, they do not
validate all content operators, font programs, ICC profiles, output intents, associated
files, transparency rules, tagging or the requirements of PDF/X and PDF/UA. No detected
issues never establishes conformance. The older `verify` API has additional structural
checks, but is also incomplete. External veraPDF comparison has not yet been performed.

Findings identify an indirect object, a direct dictionary/array path and known referencing
pages. Shared inherited resources can belong to multiple pages. Unknown extensions remain
untouched. Unreferenced indirect objects are inspected too; a finding with no associated
page is not proof that the object affects rendered output. The scanner caps objects,
visited nodes, path depth, findings, page traversal and retained page links; its report
states when coverage was truncated. Finding IDs stay stable across check selection for
the same unchanged document.

`Report::fixes` prepares explicit selected fixes bound to the original owner object and
path. `apply_fixes` checks every binding before editing a clone and publishes only when
all fixes succeed. Cancellation or any failure leaves the original unchanged. Supported
fixes disable image interpolation, set the annotation print bit while preserving other
flags, remove only the selected additional-action dictionary, or recompress decoded LZW
data as Flate. The first three can change display, printing or interactive behavior; they
are never applied automatically. LZW fixes require direct filter parameters, an end-of-data
marker and successful strict decoding, with a 32 MiB per-stream / 128 MiB per-batch limit.

The engine additionally checks the document revision and normal edit permissions, then
records all selected fixes as one undo step. Analysis and cancelling/closing the dialog do
not create history. Automation exposes `preflight_profiles`, `preflight_analyze`,
`preflight_fix` and `preflight_report`. Text/JSON exports use existing atomic, root-confined
file writing. The native Preflight dialog provides profiles, single checks, grouped object
details, page links, selected fixes and text export. Custom saved profiles, PDF/XML reports,
snap views, droplets and an embedded audit trail remain unimplemented.

For the legacy `pdfa_verify`/`pdfa_convert` JSON response, `compliant` remains a boolean for
compatibility but is conservatively `false`. Clients should use `no_detected_issues` for
the subset result; `validation_complete` is `false` and `conformance` is `not-established`.
Declaring PDF/A in metadata is not validation.

## PDF/A conformance levels a, b and u

`Level` covers PDF/A-1a, 1b, 2a, 2b, 2u, 3a, 3b and 3u (`Level::from_id("2u")`,
`declared_level` reads `pdfaid:part`/`pdfaid:conformance`). `verify` runs the shared rules
for every level, reports ISO 19005-1 clause numbers for PDF/A-1 (plus its own rules: no
embedded files, transparency or optional content) and, for levels a and u, checks that every
glyph shown by page content, form XObjects and annotation appearances maps to Unicode
(`unicode.rs`; ISO 19005-1 §6.3.8, ISO 19005-2 §6.2.11.7): a ToUnicode entry, else a
predefined encoding or a known `/Differences` glyph name, else an Adobe CJK character
collection. Missing mappings, U+0000/U+FEFF/U+FFFE and U+FFFD are errors; Private Use Area
values and, for level a, word breaks made only by `TJ` positioning are warnings. Each
`Issue` has a `severity` (error or warning).

The logical-structure rules of level a live one layer up, in `pdfcraft-a11y`
(`verify_pdfa`, `convert_pdfa`), which reuses the PDF/UA checker; `convert_with` takes that
check as a callback. `convert` refuses (`ConvertError::Refused`, nothing changed) when a
used glyph's meaning can't be derived (composite fonts without a usable CMap, symbolic fonts
without an encoding) and, without a structure check, for level a. Otherwise it writes
ToUnicode CMaps from the encoding for simple fonts whose mappings are missing or bad, and
sets `pdfaid:conformance` to the level's letter. Not yet: Type 3 glyph procedures and
patterns, per-character ActualText for Private Use Area text, the PDF 1.4 limits of
PDF/A-1, PDF/A-4.

## Total area coverage

`coverage::page_coverage` is Output Preview's Total Area Coverage check: the highest total
ink on a page (C+M+Y+K plus spot inks, in percent), the area above a limit (300 % by default)
and the worst connected regions. The page renderer is RGB-only, so the module interprets the
content stream itself into a bounded ink grid (forms, fills, strokes, text string boxes,
sampled images, clip bounding boxes; later objects knock out earlier ones). CMYK, Gray,
Separation, DeviceN and Indexed colours are exact; RGB and Lab use a naive full-GCR
conversion without a press profile and are counted separately; patterns, shadings,
DCT/JPX/JBIG2/CCITT images and annotations are reported as unmeasured. `coverage::METHOD`
states these limits for reports. Automation exposes it as `print_total_area_coverage`.

## PDF/X verification

`pdfx::verify_pdfx` (tool `pdfx_verify`) checks the machine-checkable requirements of
PDF/X-1a:2001, PDF/X-1a:2003, PDF/X-3:2002, PDF/X-3:2003 and PDF/X-4 by rule id (`X_RULES`):
identification, PDF version, document information and XMP keys, Trapped, file ID,
encryption, the GTS_PDFX output intent and its ICC profile header, page boxes, printing
annotations in the bleed area, and a bounded walk of page content (forms, patterns,
shadings, Type 3 glyphs, annotation appearances; seen-set, depth, byte and operator caps)
for fonts, colour spaces, transparency, transfer functions, halftones and PostScript.
Rules whose ISO wording we are not certain of report warnings, which don't affect
`conforms`; `X_NOT_CHECKED` lists what isn't decided. It reads the document only.

## Overprint, rich-black and registration warnings

`colour_warnings::check` (tool `print_color_warnings`) reports Output Preview's colour
warnings per page: ExtGStates that turn overprint on or set `OPM 1`, objects painted with
overprint on, white or zero-tint objects set to overprint (they disappear on press for
DeviceCMYK with `OPM 1` and for spot colours), rich black (K at or above 95 % with C+M+Y at
or above 10 % by default, with the summed ink), rich-black text below 12 pt effective size
(font size × text and current matrices), and the `/Separation /All` registration colour
outside printer marks (PrinterMarks artifacts, a "Printer marks" layer, or outside the
TrimBox). Its own bounded walk follows forms and coloured tiling patterns (seen-set, depth,
byte and operator caps). DeviceCMYK, four-component ICC, Indexed, DeviceN process colorants
and spot colours with Type 0 (one input) or Type 2 tint transforms resolve to CMYK; RGB and
Lab colours, image pixels and shadings are counted as unresolved, and overprint is not
simulated. `colour_warnings::NOT_CHECKED` lists the limits. It reads the document only.

## Flatten transparency

`flatten::flatten_transparency(doc, pages, settings)` (engine `Edit::FlattenTransparency`,
automation `optimize_flatten_transparency`) gives pages that use transparency an opaque
equivalent. Transparency is what the PDF/X transparency rule looks for (the same predicates
as `pdfx`): soft masks, `CA`/`ca` below 1, blend modes other than Normal, transparency groups
and image soft masks, in the resources page content actually names, through forms, tiling and
shading patterns and Type 3 fonts (seen-sets, depth and visit limits). For each such page:

1. resources are replaced by opaque copies (shared objects are copied, never changed) and the
   page `/Group` is removed;
2. the page is rendered before and after at 72 px per unit; the pixels that changed are grouped
   into rectangles (8-px cells, 2-px margin), merged when closer than `(100 - balance)`% of 18 pt;
3. each rectangle is rendered from the original at `resolution` (presets: low 150, medium 300,
   high 600 ppi; 36–1200, 64 MP per page) and drawn on top as an opaque DeviceRGB image in tiles
   of at most 2048 px. Everything else stays vector; text under the images stays searchable;
4. a balance of 0, regions over `balance`% of the page, or content that can't be read or is
   nested too deep make the whole page one image (its text is then gone).

Not handled: annotation appearances (flatten comments first), clipping to object outlines,
text to outlines, CMYK images for PDF/X-1a, optional content other than its default view.

