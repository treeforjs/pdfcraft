# Fidelity harness

`cargo xtask fidelity` measures how PdfCraft renders pages and extracts text on a set of synthetic
PDFs, compared with Poppler. It is the first measured answer to ROADMAP gap 3, "measure fidelity".
The reference is Poppler, not Acrobat. Acrobat comparisons still need the clean-room observation
process (`plan/acrobat/`). Treat the numbers as "how far from a mature independent renderer", not
as "how far from Acrobat".

## What it does

1. Generates 13 fixture PDFs (20 pages) from code in `xtask/src/fidelity/fixtures.rs`:
   hand-written COS, procedural images, the standard 14 fonts by name, and the repository's OFL
   fonts from `assets/fonts/` (already in `ATTRIBUTION.toml`). No third-party documents are used.
   The fixtures go to `<target>/fidelity/fixtures/` at run time and are never committed.

   | Fixture | Covers |
   |---|---|
   | `text-standard-fonts` | Standard 14 fonts with WinAnsi accents, 7–28 pt, and a two-column page (reading order) |
   | `text-embedded-fonts` | Embedded TrueType: simple WinAnsi (Inter, Dancing Script), and Type0 Identity-H with `/W` and ToUnicode (JetBrains Mono) |
   | `text-operators` | `Tc`, `Tw`, `Tz`, `Ts`, `TJ` kerning, render modes 1/2/3/7, rotated, mirrored and vertical text, `T*`, `'` and `"` |
   | `type3-fonts` | Uncolored (`d1`) glyphs with and without ToUnicode, a coloured (`d0`) glyph, rotated runs |
   | `vector-art` | Line widths (including 0), caps, joins, miter limit, dashes, even-odd and nonzero fills, clipping, curves, nested Form XObjects with BBox clips and transforms |
   | `shadings-patterns` | Axial, radial, stitched-function and Gouraud-mesh shadings; shading pattern; coloured and uncoloured tiling patterns |
   | `color-spaces` | Gray, RGB, CMYK, Separation, Lab, CalRGB, CalGray, Indexed, DeviceN with a type 4 function |
   | `images` | Flate RGB, non-interpolated upscale, stencil mask, soft mask, 2-bit Indexed, `/Decode` inversion, CMYK, 1-bit gray, inline ASCIIHex image, rotated image |
   | `transparency` | Fill and stroke alpha, eight blend modes, knockout and non-knockout groups, group opacity, luminosity soft mask |
   | `forms` | AcroForm widgets with appearance streams (text, check box, radio, combo box, push button) and without (text, multiline, comb, list box) |
   | `annotations` | Markup with an appearance stream (Square, Stamp) and without (Square, Circle, Line, Highlight, Underline, StrikeOut, Text, FreeText, Ink, Polygon); Hidden and NoView flags |
   | `rotated-pages` | `/Rotate` 0, 90, 180, 270, −90 and 450 on landscape and portrait media |
   | `page-boxes` | CropBox inside, larger than and offset from the MediaBox; crop plus rotation; inherited MediaBox |

2. Renders every page with `pdfcraft-cli render` (`PageRenderer` pixels, the path the
   `page_render` automation tool uses) and with `pdftoppm -cropbox`. Poppler runs only as an
   external oracle process.
3. For each page, compares the two rasters over the union of their sizes, composited on white.
   It reports the mean and maximum channel difference (0–255). It also reports **diff %**, the
   share of the page whose largest channel difference exceeds the tolerance (default 32), and
   **ink diff %**, the same share counted only over pixels that are not white in either raster.
   It writes `renders/<fixture>-p<N>-compare.png`: PdfCraft, Poppler, then the differing pixels in
   red over a faded copy of PdfCraft's render.
4. Extracts text with `pdfcraft-cli text` and `pdftotext -cropbox`. Reports word-level F1 between
   the two (bag of words) and an order-aware F1 (longest common subsequence of words). Each
   extractor is also scored against the fixture's **ground truth**: the words its content streams
   show, in reading order. Invisible text counts. Annotation and field appearance text does not.
   Text outside the crop box does not count either.
5. Writes `report.json` and `summary.md` under `<target>/fidelity/` (`--out DIR` to change).

## Running

```sh
cargo xtask fidelity                    # builds pdfcraft-cli (dev profile), runs, compares to the baseline
cargo xtask fidelity --only forms       # one fixture (substring match)
cargo xtask fidelity --dpi 150 --tolerance 16 --out /tmp/fid
cargo xtask fidelity --cli path/to/pdfcraft-cli   # skip the build
cargo xtask fidelity --update-baseline  # accept the current numbers
```

Without `pdftoppm`/`pdftotext` on `PATH` the command prints `fidelity: SKIPPED …` and succeeds.

## Baseline and failure thresholds

`xtask/baselines/fidelity.json` holds only numbers keyed by fixture name and page, plus the dpi,
tolerance and Poppler version. It contains no PDFs, images or text. The command fails if:

- a page's diff % exceeds its baseline by more than max(0.5 percentage points, 10 %),
- a page's mean Δ exceeds its baseline by more than max(0.5, 15 %),
- a page that rendered in the baseline now fails, or
- text F1 against Poppler or against the ground truth drops by more than 0.02.

Improvements beyond the slack are listed as notes, which suggest `--update-baseline`. A baseline
recorded at a different dpi or tolerance is refused. The baseline was recorded on Ubuntu 24.04
with Poppler 24.02.0 and its default font configuration. Poppler substitutes system fonts for the
non-embedded standard 14, so another Poppler version or font setup can move the text-heavy pages.
In that case the command notes a version mismatch; re-record locally rather than loosening the
thresholds.

## Reading the numbers

Most differences on text pages are expected and are **not** PdfCraft defects:

- **Standard 14 fonts.** This Poppler draws non-embedded Helvetica, Times and Courier with
  DejaVu fallbacks, which have no italics. PdfCraft uses metric-compatible faces. This alone
  accounts for most of the roughly 60–67 % ink difference on `text-standard-fonts`.
- **Glyph anti-aliasing and hinting.** These differ on every text page, including embedded fonts.
- **Stroke adjustment.** Poppler snaps thin lines to whole device pixels. PdfCraft spreads a 1 px
  line across two partly covered rows.
- **Colour management.** CMYK, Separation and Lab conversions differ by a few levels, because
  Poppler converts without ICC. Poppler also tints CalGray, which is an oracle quirk.
- **Pixel snapping** at tile, image and rectangle edges at non-integer scales.

For defects, look at the comparison images and at the ground-truth text columns.

## Findings (2026-10-12, 96 dpi, tolerance 32, Poppler 24.02.0)

Measured with the run whose numbers are in the committed baseline. These are defects in
PdfCraft's `page_render` and text paths. The renderer was not changed in this work. Findings 1
and 2 were fixed later the same day, outside the renderer. The baseline now holds the numbers
after that fix.

1. **Fixed 2026-10-12: widgets without an appearance stream were not drawn** (`forms`). The text
   field "Lisbon", the multiline field, the comb field and the list box showed nothing, and their
   values were missing from extracted text. They are now drawn, in memory only, by the generators
   used for filling (`pdfcraft_forms::with_missing_appearances`). `pdfcraft-cli render` and `text`,
   the page view, `page_render`, exports and printing all show them, and the file is unchanged.
   `forms` p1 went from 6.85 % to 5.52 % diff, and text F1 against Poppler from 0.650 to 0.857.
   Mean Δ rose from 5.99 to 7.64 because the styles differ. PdfCraft draws the `/MK /BC` border
   and a light-blue list selection. Poppler draws no border when the widget has no `/BS` or
   `/Border`, and its list selection is black. Truth F1 fell from 0.818 to 0.581 only because the
   ground truth leaves out field text. Poppler scores 0.500 against the same truth.
2. **Fixed 2026-10-12: markup annotations without an appearance stream were not drawn**
   (`annotations`). This affected Square, Circle, Line with line endings, Underline, StrikeOut,
   Text (note icon), FreeText, Ink and Polygon. They are now drawn by
   `pdfcraft_annot::with_missing_appearances` on the same paths. `annotations` p1 went from
   10.20 % to 4.43 % diff and mean Δ from 7.62 to 4.30. Text F1 against Poppler went from 0.914 to
   1.000. Truth F1 fell from 0.968 to 0.882 because of the same truth rule, and Poppler also
   scores 0.882. Style differences remain:
   - PdfCraft smooths Ink strokes into curves. Poppler joins the points with straight lines.
   - PdfCraft draws no FreeText border without `/BS` or `/Border`. Poppler draws a 1 pt border.
   - The note icons differ: PdfCraft uses its own drawings.
3. **Knockout groups are not honoured** (`transparency`). Inside a `/K true` group, the second
   circle should replace the first where they overlap. PdfCraft's overlap pixel is (102, 41, 194),
   the same as the non-knockout group. The expected value is about (102, 102, 255). Poppler has
   the same gap, so the pixel metric does not flag it. It was found from the spec.
4. **Clip-mode text was not extracted** (`text-operators`). Fixed: text drawn with `Tr 7`
   ("CLIPPED") is now extracted, searched and copied, and still paints nothing (vendored
   hayro-interpret patch 23; `text::tests::text_in_every_rendering_mode_is_extracted`).
5. **Text with a negative rise was moved out of reading order** (`text-operators`). Fixed: in
   "Baseline rises falls back", the part with `Ts -4` ("falls back") now stays on its line
   (`text::tests::raised_and_lowered_text_stays_on_its_line`). With 4 and 5 fixed, text F1 vs
   Poppler went from 0.855 to 0.865 (order F1 0.764 to 0.811) and F1 vs truth from 0.990 to 1.000;
   the rest of the gap is Poppler splitting rotated, mirrored and vertical runs.
6. **No stroke adjustment for thin lines** (`vector-art`, minor). Hairlines and lines up to 1 px
   come out as two grey rows instead of one solid pixel row, so they look lighter than in Poppler
   or Acrobat.

PdfCraft does better than Poppler on Type 3 text. Poppler loses word boundaries and order (F1
against truth: Poppler 0.615, PdfCraft 1.000). PdfCraft also extracts rotated runs and
negative-rise text whole, where Poppler splits them into fragments. Both extractors include text
that lies outside the crop box ("OUTSIDE" in `page-boxes`). Rotation, page boxes, shadings,
tiling patterns, images, blend modes, soft masks and group opacity agree with Poppler apart from
the anti-aliasing and colour effects listed above.

## Gaps in the harness

- Poppler is not Acrobat, so where both share a gap (knockout, for instance) only the spec reveals
  it.
- There are no JPEG, JBIG2, CCITT or JPX images and no CJK or vertical (Identity-V) text yet.
  Optional content, overprint and form behaviour (calculation, formatting) are not covered either.
- Thresholds guard against regressions. They do not set an absolute quality bar.
