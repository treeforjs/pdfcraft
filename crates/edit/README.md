# pdfcraft-edit

Layer L4: editing page content in place. Page marks (header & footer, watermark, background,
Bates numbers), added text and images, editing existing text lines and paragraphs, editing
images and grouped artwork, flattening, and the Print Production fixes: Fix Hairlines
(`hairlines`), Add Printer Marks (`printer_marks`) and Convert Colors (`convert_colors`).

Every entry point takes a `pdfcraft_cos::Document` and changes it in place; the engine runs them
on a copy, so an error leaves the open document untouched, and each is one undo step there.

## Right-to-left and CJK text

Pages store text in display order. `text_lines` puts right-to-left lines (Hebrew, Arabic) into
logical order, glyphs and text alike (`bidi::unit_order` with `bidi::display_order_rtl`: the
outermost strong characters, else the majority, decide the line's direction), reads Arabic
presentation forms as letters (`bidi::normalize`), and orders the different-font fragments of
one row the same way; `TextLine::rtl` / `TextBlock::rtl` say so. Paragraph edits (the run path)
write each row back in display order by UAX #9 (`unicode-bidi`) with the paragraph's
first-strong base direction; a right-to-left paragraph keeps its right edge and a single line
grows to the left margin. New Arabic text takes the joining forms its neighbours call for: in
the run's own font when its ToUnicode maps presentation forms (`bidi::present`), otherwise shaped
with the craft-fonts `Arab` face (harfrust) into a Type 3 font (one code per cluster, ToUnicode
kept), otherwise as letters; an edit next to an Arabic letter re-encodes the whole word so its
neighbours re-join. Re-encoded segments split where Arabic meets other text, so each piece gets
a face that shows it. CJK rows break between characters (not before closing punctuation, small
kana or ー, not after opening brackets), and the Type 3 fallback tries Japanese, then Chinese
and Korean faces (`pdfcraft_fonts::document_cjk_fonts_for_style`) for one with every character.
Vertical writing (Type 0 fonts with `/WMode 1`: `Identity-V`, `-V` CMaps; `/W2`, `/DW2`) is read
as columns (`TextLine::vertical`) and rewritten in place in its own font (`replace_line`);
formatting, moving or a font that lacks a character is refused. `bidi::caret_move` is the visual
caret movement the editor uses. Bidi work is capped at `bidi::MAX_BIDI_CHARS` and guarded.

Not yet: mirrored brackets in right-to-left runs, Hebrew without a font that has it (craft-fonts
has no Hebrew face), vertical fallback fonts and reflow of vertical paragraphs, and shaping with
the PDF's own embedded Arabic font program.

## Bulleted and numbered lists

`lists::edit_list(doc, page, &ListTarget::Added { index, paragraphs }, &ListChange::Set { style, start })`
formats paragraphs (the `\n`-separated lines) of an added text box as list items; `Remove` and
`Indent(levels)` take them out or change their level (0–8). `AddedText::list` holds one
`Option<ListItem>` per paragraph and is stored with the box, so lists survive save and reopen and
labels are generated at draw time: numbering continues per level, restarts after an ordinary
paragraph or at `start`, and `update_content` reconciles retyped text so new paragraphs continue
the list they were typed into. Items have a hanging indent (label at the level's indent, every
row of the paragraph at the body indent past it). `ListTarget::Paragraphs { first, count }` moves
page paragraphs into a new list box in the closest standard font. Labels and bodies are drawn in
`/PCList` marked content for `pdfcraft_a11y::keep_list_tags`. Not yet: lists inside existing
paragraphs in their own fonts, Arabic text, bullets outside WinAnsi (squares, checks).

## Convert Colors

```rust
let settings = ColorConversion {
    destination: Destination::Icc(Arc::new(std::fs::read("press.icc")?)),
    objects: ObjectKinds { text: true, vector: true, images: false },
    convert_spots: false,
    output_intent: true,
    ..Default::default()           // all source spaces, DeviceCMYK, relative colorimetric, preserve black
};
let report = convert_colors(&mut doc, &[0, 1, 2], &settings)?;
```

- **Sources**: DeviceGray, DeviceRGB, DeviceCMYK, ICCBased, CalGray, CalRGB, Lab (Indexed and
  uncoloured patterns follow their base); spot colours (Separation, DeviceN) only with
  `convert_spots`, through their tint transforms (function Types 0, 2, 3, 4). `/All` and `/None`
  never change.
- **Destinations**: DeviceCMYK, DeviceGray, DeviceRGB, or an ICC profile (gray, RGB or CMYK).
  With a profile, its `BToA` table for the rendering intent (lut8/lut16, Lab or XYZ PCS) or its
  gray/RGB matrix-TRC converts from CIE XYZ; DeviceRGB and 3-component ICC sources are read as
  sRGB (embedded source profiles are not evaluated). Without one, and for profiles with only v4
  `mBA` tables (`report.approximate`), a built-in approximation: RGB → CMYK with full black
  generation and 70% under-colour removal, naive CMYK → RGB, luma for gray. Gray goes to K only
  and CMYK numbers carry over between CMYK spaces. Preserve black maps pure black to pure black.
- **What is rewritten**: page content, the forms it draws (once each, seen-set), coloured tiling
  patterns, shadings (functions resampled to a Type 0 function, backgrounds), image XObjects
  (pixels at 1–16 bits re-encoded at 8 bits; JPEG stays JPEG for gray/RGB results, otherwise
  Flate; Indexed images get a converted palette), inline images (ASCIIHex), annotation
  appearances, and transparency group blending spaces. Colour operators in converted spaces are
  removed and the right colour is set just before each painting operator (before its path), so
  object types can be chosen independently; everything else is copied byte for byte.
- **Left alone, and reported in `skipped`**: JPX, JBIG2, CCITT and CMYK JPEG images, images with
  colour-key masks, images over 40 million pixels, mesh shadings with per-vertex colours,
  unevaluable tint transforms. Not visited: Type 3 glyph procedures and soft-mask groups.
- `output_intent` makes the destination profile the catalog's `GTS_PDFX` output intent.

## Page objects: arrange, align, distribute

`objects::page_objects(doc, page)` lists what the page's own content draws, in paint order:
text objects, paths, images, Forms, inline images and shadings (everything inside one
marked-content sequence with an MCID is one object). `objects::edit_objects(doc, page,
&[indexes], &ObjectsEdit::…)` arranges them (`Arrange::{Front, Forward, Backward, Back}`: the
original, untagged content becomes one stream where each object is wrapped in `q … Q` after
replaying the graphics state it was painted with, and marked content is reopened as needed),
or aligns, distributes, rotates, flips or moves them (each selected object gets a `q <cm> … Q`
wrapper followed by the state it leaves for later content; everything else is copied byte for
byte). Directions are as displayed, after `/Rotate`. Added items and page marks are not listed.
