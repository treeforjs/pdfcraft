# pdfcraft-optimize

Layer L4: the PDF Optimizer (execution plan M11.1; the viewer's Reduce File Size and Optimize PDF ▸
Advanced optimization).

```rust
let report = optimize(&mut doc, &Settings::default())?;   // Reduce File Size's choices
// Or with progress: called before each image and the clean-up; `false` cancels.
let report = optimize_with_progress(&mut doc, &settings, &mut |stage| keep_going)?;
```

- **Images:** the effective resolution of each image is measured where pages draw it (the CTM
  through form XObjects; the smallest over all uses). Colour and grayscale images above a
  threshold are resampled with a bicubic (Catmull-Rom) filter to a target resolution and
  stored as JPEG (the viewer's quality steps) or ZIP; soft masks are resampled with their image.
  An image is replaced only if the result is smaller. Left alone: CMYK and other colour spaces,
  decode arrays, more than 8 bits, JPEG 2000, JBIG2 and CCITT.
- **Monochrome images** (one-bit grey samples and image masks): recompressed losslessly as CCITT
  Group 4 (`pdfcraft_filters::encode_ccitt_g4`, the smaller polarity) or ZIP when smaller, and
  optionally downsampled first by area average (majority, ties to ink; not bicubic). Only
  samples that decode completely through general filters are used; CCITT/JBIG2 images, inline
  images and images with indirect filter descriptions are left alone.
- **Despeckle** (`despeckle_page_images`, module `despeckle`; Scan & OCR's despeckle): isolated
  dark specks that fit a square sized by each image's effective resolution (low 0.13 mm,
  medium 0.21 mm, high 0.34 mm) are removed unless text-sized ink lies within 3 pt, so
  periods and i-dots stay; optional light-hole filling and impulse median. Original
  connected-component code over a bit mask and runs (at most 80 M pixels, 8 M runs). One-bit
  images stay one-bit (CCITT fax is decoded with `hayro-ccitt` and re-encoded as Group 4);
  8-bit gray/RGB are rewritten (JPEG at quality 90 when they were JPEG). JBIG2, JPEG 2000,
  CMYK, decode arrays and masks are left alone and counted as skipped. The method and its
  thresholds are documented in `src/despeckle.rs`.
- **Remove background** (`remove_page_image_backgrounds`, module `background`; Scan & OCR's
  background option): the paper level is the main light peak of each 8-bit gray/RGB image's
  luma histogram (images without one, such as photographs and dark or inverted scans, are
  left alone); pixels within a margin below it (low/medium/high, widened by the paper's
  measured noise, never below 128, or an explicit white point) become white through a levels
  curve that keeps ink and mid-greys. RGB images use per-channel white points from the
  paper's colour, so yellowed paper turns neutral. Images are rewritten with Flate (JPEG at
  quality 90 when they were JPEG); one-bit images are left as they are; JPEG 2000, JBIG2,
  CMYK, decode arrays and colour-key masks are counted as skipped. The method is documented
  in `src/background.rs`.
- **Discard objects:** thumbnails, alternate images, document tags, print settings.
- **Clean up:** Flate for streams with no filter (when smaller); LZW streams re-encoded as Flate
  when they decode completely (`Stream::decoded_complete`: strict, end-of-data code required,
  direct filter description, 32 MiB per stream and 128 MiB per run, as preflight's LZW fix);
  others are left unchanged and counted in `Report::lzw_skipped`. `Settings::compress_structure`
  tells the engine whether to write object streams.

The engine runs Remove Hidden Information (`pdfcraft-redact`) for the user-data categories,
then this, then merges identical objects and writes a full, compressed save
(`pdfcraft_engine::optimizer::OptimizeJob`, which the UI runs on a worker thread with a
progress bar). JPEG decoding and
encoding and resampling come from the `image` crate.
