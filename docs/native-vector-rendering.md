# Native vector rendering

PdfCraft has an original PDF vector interpreter and display list for solid DeviceGray and
DeviceRGB paths. It is **opt-in and off by default**: unless a caller asks for it, every page is
drawn by the bootstrap renderer (hayro) and the native path is never attempted (no content is
compiled for it). When opted in, supported pages use it wherever pages are rasterized through
`pdfcraft-render` (`PageRenderer`, `RenderPool`: the desktop app, print/export callers and the
`page_render` automation tool); every other page still uses the bootstrap renderer as a whole,
and a successfully interpreted prefix never replaces the rest of a page.

How to opt in:

- `RenderConfig::native_vector = true` for one renderer or pool.
- `pdfcraft_render::set_native_vector_default(true)` makes every `RenderConfig::default()`
  created afterwards opt in (engine documents, export, print, automation). Applications call it
  once at start-up; the renderer itself never reads the environment.
- `PDFCRAFT_NATIVE_VECTOR=1` (also `true`, `yes`, `on`) is read once at start-up by the desktop
  app and by `pdfcraft-cli` (whose `check` children and therefore `cargo xtask fuzz` inherit it).
- `pdfcraft-cli render FILE --page N --out x.png --native-vector`; its report ends with
  `(native vector)` when the native path drew the page.

`RenderStats::native_vector_rasterizations` counts the pages it drew. There is no preferences
or menu switch yet: it is a testing option.

This is an M2 foundation, not an independent PDF renderer. The bootstrap syntax reader still
supplies dictionaries, page geometry and (decrypted) encoded streams; `pdfcraft-filters` decodes
content; the existing permissively licensed Vello CPU rasterizer (through hayro's dependency)
draws the display list. Text/fonts, images, forms, patterns, shadings, transparency, graphics
state dictionaries, annotations, marked content, replacement colour spaces, CMYK and explicit
`/UserUnit` all stay on the bootstrap path. Rendering parity rows stay partial.

## What the native path accepts

- Operators `m l c v y h re`, `S s f F f* B B* b b* n`, `W W*`, `q Q cm`, `w J j M d`, `g G rg RG`.
  Anything else (including any name, string, dictionary or keyword operand) rejects the page.
- Path coordinates are mapped to default page space at each construction operator; a stroke
  keeps the CTM at painting as its pen. A `cm` (or a `Q` restoring a different CTM) inside a
  path object is not conforming PDF and readers disagree (hayro and Poppler apply the painting
  CTM to the whole path), so such a page falls back.
- A fill (`f`, `f*`, `F`, or the fill of `B`/`b`) whose path has no width or no height in user
  space (its control box within 1/4096 of zero in either direction, the bootstrap renderer's
  rule) is painted as a one-pixel stroke of the box diagonal, as the bootstrap renderer, pdf.js
  and Poppler do (pdf.js issue 4260: table rules drawn as `w 0 re f`).
- A clip set by `W`/`W*` takes effect after the painting operator; `q`/`Q` restore it, not the
  current path. A segment after `h` starts a new subpath at the closed start.
- PDF stroke defaults (butt caps, miter joins, limit 10). Like the bootstrap renderer, strokes
  are never thinner than one device pixel (so `0 w` is a one-pixel hairline, dashes included)
  and are capped at four canvases wide. Zero-length dashes are drawn as dots with round/square
  caps (except one falling exactly on a subpath's end).
- Page `/CropBox`, `/Rotate`, zoom and tile offsets use the same transform as the bootstrap
  renderer. Pages with `/Annots`, `/Group`, `/UserUnit` or `/DefaultGray`/`/DefaultRGB` fall back.

## Bounds

- Content streams: unfiltered or `FlateDecode` only, and Flate must reach the deflate stream end
  (no truncated prefix is ever treated as a complete page). LZW, RunLength, ASCIIHex, ASCII85,
  predictors, `Crypt`, `/F`, `/FFilter`, `/FDecodeParms` and `/DP` fall back. At most 16 filters,
  256 `/Contents` entries (each must resolve), 4 MiB encoded per stream and 16 MiB encoded per page
  (checked on the actual bytes before decryption allocates, through the vendored
  `Stream::raw_data_within`), 1 MiB decoded per page.
- Lexer: linear and strict; numbers at most 64 bytes without repair, at most 6 operands, dash
  arrays of at most 256 numbers, cancellation checked per token and every 1024 bytes of comments
  or whitespace.
- Compiler: 100,000 operators and path segments, 25,000 painted paths, 256 graphics-state levels,
  256 clipping paths, 65,536 retained clip references, coordinates within 1e12 page units, dash
  phases reduced to one period.
- Device: each raster is planned before anything is drawn. Coordinates must stay within 1e6
  device pixels. Work is charged in device pixels of outline: fill and clip control-polygon
  length (a clip is charged only when pushed; it stays pushed while it is the current clip),
  stroke outline (twice the length, plus width × miter limit per vertex), and every dash piece;
  more than 4,000,000 falls back. Draws and clips whose conservative device box misses the
  canvas are skipped, so tiles do not pay for the rest of the page. Strokes are expanded in pen
  space rescaled uniformly to device size, so widths, dash pieces, the work estimate and vello's
  expansion tolerance (at most 0.25 device pixels) do not depend on how the CTM is scaled. Area
  painted is not charged (the bootstrap renderer has the same cost for the same page).
- Each worker keeps one display list or unsupported-page decision per parser generation;
  zoom and tile requests reuse it. Cancellation never caches a decision. A panic inside the
  native path is contained: the page is remembered as unsupported and the bootstrap renderer
  draws it.

## Evidence (2026-10-12, WSL Ubuntu 24.04)

Fixtures are contributor-original PDFs written by `cargo run -p pdfcraft-render --example
vector_oracle -- fixtures DIR`: winding rules, anisotropic and sheared pens, a CTM changed inside
a path, caps/joins/miter limits, dash patterns and phases (odd, zero-length, across `h`, on
curves), nested and even-odd clips, a rotated and cropped page, hairlines, colours on fractional
edges and a 3,000-path mixed page. `compare DIR 2` renders each at 144 dpi natively, through the
bootstrap renderer and through Poppler 24.02 `pdftoppm -cropbox -r 144` (an external process),
writes diff images and reports the maximum and mean channel delta, the share of differing
pixels, the share differing by more than 64, and that share again when a pixel may match any
pixel within one pixel (edges placed differently by antialiasing).

Acceptance: native against bootstrap at most 0.05 % of pixels over 64 after the one-pixel
allowance, native against Poppler no worse than bootstrap against Poppler plus 0.05 %, and
stitched tiles within 1 of the whole render. All twelve fixtures met it. Native and bootstrap
differ by at most 1 per channel on all but two: `dashes` (dots placed more exactly by the native
path; it is closer to Poppler than the bootstrap renderer, 0.013 % against 0.023 %) and
`vector-stress` (at most 5 per channel since zero-area fills became hairlines; before that,
about 3 of 128,000 pixels differed by up to 94: some of its rectangles are `0.00` wide). The remaining differences from Poppler are the same for both devices:
Poppler draws thin lines and fractional edges without antialiasing (`hairlines` 1.6 %, `colors`
0.86 %, `vector-stress` 3.4 % after the allowance). One corner pixel of a dashed rectangle differs
from Poppler where the pattern meets `h`. The page with a CTM change inside a path falls back and
is identical to the bootstrap renderer and Poppler.

Timing (release build, medians of 15 interleaved runs at 2× on a shared 8-core machine): on
pages that fall back, the native attempt added 0.6–0.8 ms to a text page and an image page
(p10s within 0.5 ms) and 1.6–3.5 % to the medians of two 900 KB Flate text pages, one of whose
vector content is compiled almost entirely before late text rejects it; that is within the
run-to-run spread (p90 above 1.3 s for both modes). A cached negative decision keeps later zoom
and tile renders of the page free of it. On a 3,000-path page the first native render took
10.0 ms against 8.7 ms for the bootstrap renderer, and repeated renders 7.6 ms against 8.8 ms.
The native attempt changes no parser or document state; the shared lazy parser and pool tests
pass unchanged.

Unit tests in `display_list/tests.rs` and `vector_raster/tests.rs` cover the lexer, compiler,
decoding, bounds, clip planning, culling, stroke widths, dots, tiles, cancellation and fallback.

### Real-world survey (pdf.js corpus, 2026-10-12)

`vector_oracle survey PDF 1.5 400` ran once per file over the pinned pdf.js corpus
(`cargo xtask corpus`, commit 5d923c96, read from the fuzz work package's checkout), one file
at a time under a 4 GiB address-space cap and a 60 s limit
(`~/artcraft/logs/rendering/survey/run.sh`). Each page is probed with a 1×1 tile through the
opted-in renderer; pages the native path accepts are rendered whole at 1.5× natively and by the
bootstrap renderer and compared, and the first three pages of each file that fall back are also
rendered whole both ways.

- 988 of the 989 PDFs: `issue19517.pdf` was left out because its 12608 × 16806 JPEG 2000 tile
  makes the bootstrap renderer on this branch ask for 3.4 GB and abort (each abort writes a
  multi-GB core dump on the Windows host; an uncapped earlier survey run in parallel helped
  exhaust the host's memory). It is fixed on the integration line by hayro-syntax patch 13
  (`jpx_size_ok`, a286932); it is not a native-path issue. 19 files did not open without a
  password or are unreadable; `bug1978317.pdf` and `issue16263.pdf` hit the time limit in the
  bootstrap renderer (known `timeout` entries of `xtask/baselines/pdfjs.json`).
- 1,813 pages attempted: **49 pages in 26 files were drawn natively**, 1,764 fell back. Fallback
  reasons: annotations/groups/`/UserUnit` 593, text (`BT`) 585, a name operand (resources: fonts,
  images, XObjects, `gs`, colour spaces) 364, `i` 183, unsupported content decoder 17, `BI` 5,
  damaged content 5, `k`/`K`/`sc` 5, replaced device colour space 3, invalid `/Contents` 3,
  oversized token 1. No crash, panic or abort on the native path.
- Native against bootstrap on the 49 native pages (after the fix below): 47 identical, the worst
  differs by 2 per channel on 0.05 % of pixels; no pixel differs by more than 64. On the 1,076
  fallback pages rendered both ways, the opted-in output was identical to the bootstrap
  renderer's.
- One material divergence was found and fixed: `issue4260_reduced.pdf` (table rules drawn as fills
  of zero-height and zero-width rectangles) was blank natively, 6.6 % of pixels off by up to
  255. Zero-area fills are now hairlines (above); synthetic regressions
  `zero_width_and_zero_height_fills_are_hairlines_like_the_bootstrap_renderer` and
  `degenerate_fill_detection_uses_user_space_bounds`.
- Timing (release, single runs, fresh renderer each): native pages median 0.43 ms both ways
  (per-page ratio median 1.00, p10 0.64, p90 1.28). On fallback pages the failed native attempt
  cost a median 0.06 ms (p90 0.43 ms), a median ratio of 1.09 (p90 1.30) on renders that are
  themselves around a millisecond.

The corpus says little about fidelity on real vector artwork: most of the 49 native pages are
blank or nearly blank pages of test files, because real pages almost always carry text, images,
annotations or `gs`. The synthetic fixtures above remain the main fidelity evidence.

`PDFCRAFT_NATIVE_VECTOR=1 cargo xtask fuzz --time 300 --jobs 1 --timeout 10 --seed 7` (784 seeds:
the synthetic ones, including a new vector-only seed, plus corpus files under 64 KB): 45,660 runs,
0 crashes, 0 hangs.

## What would justify turning it on by default

Today it is a correct but narrow optimisation-in-waiting: few real pages qualify, and on those
it is no faster than the bootstrap renderer. Default-on should wait for all of:

1. **Coverage that matters**: the native path accepts a meaningful share of real pages (at least
   text with embedded fonts, `gs` with opacity 1, and images), measured on the pdf.js corpus and
   a larger real-world set (e.g. office, CAD and map exports), not mostly blank pages.
2. **Fidelity at that coverage**: every accepted page within the acceptance bounds above
   against the bootstrap renderer and no worse than it against Poppler, on the whole corpus,
   with every divergence understood and either fixed or documented as the bootstrap renderer
   being wrong.
3. **Robustness**: full-corpus surveys and several hour-long fuzz runs with the native path on
   (`PDFCRAFT_NATIVE_VECTOR=1 cargo xtask fuzz`) without crashes, hangs or memory growth, under
   the same 4 GiB cap.
4. **A reason**: a measured win (speed on vector-heavy pages, memory, or a fidelity fix the
   bootstrap renderer cannot get) that outweighs carrying two renderers, and a rollback switch
   (this opt-in, inverted) kept for at least one release.
