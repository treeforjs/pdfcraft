# pdfcraft-redact

Layer L4 (may use `edit`): redaction, architecture §11.2.

```rust
// Marks are Redact annotations (pdfcraft-annot: Shape::Redact { quads, overlay }).
let marks: Vec<Mark> = marks(&doc);                 // page, areas (user space), fill, overlay text
let report = apply(&mut doc, None)?;                 // everything, or Some(&[pages])
clear_marks(&mut doc, None)?;                        // remove marks without applying
```

`apply` removes, under every marked area:

- **text**: glyphs are cut out of `Tj`/`TJ`/`'`/`"` and replaced by a `TJ` displacement of the
  same advance, so the rest of the line keeps its exact position. Widths come from `/Widths`,
  `/W`/`/DW` (composite fonts, embedded CMaps with codespace and CID ranges), Type 3 font
  matrices, and approximations for the standard 14. A glyph goes when it overlaps an area by
  more than a fifth of its size (or a point) both ways; zero-size text goes when its origin is
  inside;
- **images**: fully covered → removed; partly covered → a copy with the covered pixels cleared
  (8/16-bit and 1/2/4-bit images, image masks; Flate/LZW/RL/A85/AHx). Codecs PdfCraft can't
  re-encode (DCT, JPX, JBIG2, CCITT) are removed whole (fail-closed); inline images under an
  area are removed;
- **vectors**: covered paths are removed; partly covered paths and shadings are clipped so
  nothing paints inside the areas;
- **form XObjects**: rewritten recursively into new objects (pages sharing the original keep it);
- **annotations** whose rectangle overlaps (with their pop-ups), and **form fields** with a
  widget under a mark;
- the marks themselves, replaced by boxes in their fill colour (and overlay text) drawn into the
  page as a `/PCMark /Redaction` stream.

A verification pass re-reads each redacted page and fails the operation if any glyph or inline
image is still under an area. Content that can't be decoded makes the operation fail rather than
leave it unredacted.

## Remove hidden information / Sanitize

```rust
let found = sanitize::scan(&doc);                          // [(Hidden, count)] per category
sanitize::remove_hidden(&mut doc, &[Hidden::Comments])?;   // chosen categories
sanitize::sanitize(&mut doc)?;                             // all of them
```

Categories: metadata (`/Info`, XMP), file attachments, comments, form fields (flattened so the
values stay visible), hidden text (render modes 3 and 7, or wholly off the page), hidden layers
(content of off optional-content groups, then the groups), bookmarks, links/actions/JavaScript,
and `/PieceInfo` private data. Both make the next save a full rewrite.

`patterns` finds phone numbers, e-mail addresses, credit card numbers (Luhn-checked), US Social
Security numbers and dates for Search & Redact, plus locale sets (`patterns::LOCALES`,
`patterns_for("en-GB")`; each set also includes the `intl` patterns):

| Locale | Patterns |
| --- | --- |
| `intl` | phone, e-mail, credit card (Luhn), dates (numeric and English month names), IBAN (ISO 13616 mod 97, bare or in groups of four) |
| `en-US` | SSN, ZIP+4 anywhere, five-digit ZIP only after a state abbreviation |
| `en-GB` | postcodes with their space, National Insurance numbers (HMRC prefix rules, suffix A–D) |
| `en-CA` | postal codes (`K1A 0B1`), SIN in 3-3-3 groups (Luhn, never 0 or 8 first) |
| `fr-FR` | NIR with its key (97 − first 13 digits mod 97), Corsica 2A/2B |
| `ja-JP` | postal codes (`100-0001`), Individual Numbers (My Number) with their check digit |

Test values are synthetic (made up, with valid check digits where needed) or published specimens.

`codes` holds the redaction code sets (U.S. FOIA exemptions, 5 U.S.C. § 552(b); U.S. Privacy Act,
§ 552a; descriptions are our own summaries) and turns chosen codes, or a custom list, into the
mark's overlay text (`/OverlayText`; `/DA`, `/Q` and `/Repeat` carry its look).

Not yet: other pattern locales, country-specific IBAN lengths, saved custom code sets,
re-encoding of DCT images (they are removed instead).
