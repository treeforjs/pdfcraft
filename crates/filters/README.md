# pdfcraft-filters

PDF stream filters: the decoders and encoders behind `/Filter` and `/DecodeParms`.

- **Layer:** L0, standalone. No dependency on any other PdfCraft crate. `cos` depends on it (architecture §3 rule 3).
- **Licence:** MIT OR Apache-2.0. Clean-room: written from ISO 32000-2 §7.4, RFC 1950/1951 (through `flate2` with the pure-Rust `miniz_oxide` backend), TIFF 6.0 §14 (predictor 2) and RFC 2083 §6 (PNG filters).
- `#![forbid(unsafe_code)]`. Builds for `wasm32-unknown-unknown`.

## API

```rust
pub enum Filter { Flate, Lzw, AsciiHex, Ascii85, RunLength, Dct, Jpx, Jbig2, CcittFax, Crypt, Unknown(String) }
Filter::from_name(b"FlateDecode" | b"Fl" | ...) -> Filter   // full names and inline-image abbreviations
Filter::name(&self) -> &str; Filter::is_image_codec(&self) -> bool

pub struct Params { predictor, colors, bits_per_component, columns, early_change }  // i64s; Default = 1, 1, 8, 1, 1
pub enum FilterError { Corrupt { filter, detail }, LimitExceeded(usize), Unsupported(String) }

decode(chain, data, max_output) -> Result<Vec<u8>, FilterError>
decode_tolerant(chain, data, max_output) -> Result<(Vec<u8>, bool /*partial*/), FilterError>
encode(filter, params, data) -> Result<Vec<u8>, FilterError>
encode_flate(data) -> Vec<u8>   // zlib, level 6
decode_complete(chain, data, max_output) -> Result<Vec<u8>, FilterError>  // strict + LZW EOD required
encode_ccitt_g4(rows, width, height, black_is_1, max_output) -> Result<Vec<u8>, FilterError>
```

- `chain` is applied first to last, as in a `/Filter` array.
- Image codecs (DCT, JPX, JBIG2, CCITT), `Crypt` and unknown filters anywhere in a chain give `Unsupported`: callers keep such data encoded. Decryption belongs to `crypt`.
- `max_output` bounds every intermediate buffer (including the pre-predictor buffer) and gives `LimitExceeded(max_output)`.
- `decode_complete` is `decode` that also refuses LZW data without its end-of-data code: for lossless re-encoding (preflight's LZW fix, the Optimizer), which must not publish a repaired stream.
- `encode_ccitt_g4` is an original ITU-T T.6 (Group 4, `/K -1`) encoder for one-bit rows in PDF sample layout; output ends with EOFB and is bounded by `max_output`. There is no CCITT decoder here.
- `decode_tolerant` turns `Corrupt` into "what was decoded so far" plus `partial = true`, then runs the rest of the chain on it (predictors are applied to partial output too). `LimitExceeded` and `Unsupported` are still errors.

## Behaviour

| Filter | Decode | Encode |
|---|---|---|
| FlateDecode | zlib header checked, then inflated raw: a missing or wrong Adler-32 is tolerated; no valid header → raw deflate; trailing bytes ignored; preset dictionary → Corrupt; truncated → Corrupt (partial in tolerant mode) | zlib level 6 |
| LZWDecode | 9–12-bit MSB-first codes, clear (256), EOD (257), `EarlyChange` 0/1; missing EOD/initial clear tolerated; a full table stops growing | greedy, leading clear, clear-table when the table fills, EOD |
| Predictors (Flate/LZW) | 2 = TIFF (1/2/4/8/16 bpc), 10–15 = PNG with per-row tags 0–4; partial final rows decoded; bad params or tags → Corrupt | 2, 10 None, 11 Sub, 12 Up, 13 Average, 14 Paeth, 15 per-row optimum (min sum of signed residuals) |
| ASCIIHexDecode | white space ignored, odd digit padded with 0, `>` EOD (optional), other characters → Corrupt | upper case, newline every 64 digits, `>` |
| ASCII85Decode | `<~` prefix, `z`, white space, `~>` (or bare `~`, or none) EOD, final partial group; `z` inside a group, a 1-char final group, overflow or bad characters → Corrupt | `z` for zero groups, newline every 75 characters, `~>` |
| RunLengthDecode | 0–127 literal, 129–255 repeat, 128 EOD (optional); truncated run → Corrupt | runs of ≥2 repeat, literals up to 128, 128 |

## Tests

- Unit tests per filter with hand-computed vectors (the ISO 32000-2 LZW example, RFC 1951 stored blocks, PNG/TIFF rows).
- LZW is cross-checked against `weezl` (dev-dependency) in both directions for both early-change settings.
- The G4 encoder is checked against `hayro-ccitt` (dev-dependency): code tables prefix-free with the expected Kraft sums, a known-answer vector, every bitmap up to 12 pixels in every shape, awkward widths, long runs (make-up and extended make-up codes) and a proptest; `pdfcraft-optimize`'s tests also decode its output with Poppler.
- `tests/proptests.rs`: encode→decode round trips for every encodable filter × predictor × colors × bpc × columns, two-filter chains, output-limit checks, and fuzz-style no-panic tests (random bytes, random/nonsense params, bit-flipped valid streams).
- `fuzz/`: a detached cargo-fuzz crate (`cargo +nightly fuzz run decode`, run from `crates/filters/`).

## Status

M1.1/M1.2 for the general-purpose filters are done. Not yet here: the image codecs (DCT via zune-jpeg, JPX via hayro-jpeg2000, JBIG2, CCITT G3/G4 decode, DCT encode) and Flate encode levels other than 6.
