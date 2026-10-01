# Vendored crates

Permissively licensed crates we carry small patches for. Each directory keeps the upstream
licence files unchanged. Every patch is marked `PrintCraft patch:` in the source, is covered by a
PrintCraft test, and should be proposed upstream. Remove the vendored copy once upstream releases
the fix. Vendoring copyleft code is never allowed (plan/adr/0001).

| Crate | Version | Licence | Patches | Test |
|---|---|---|---|---|
| hayro-interpret | 0.7.0 | Apache-2.0 OR MIT | (1) select `/AP /N` appearance states via `/AS` (checkboxes, radios, stateful icons) and honour the NoView flag; (2) Type3 Unicode fallback to `/Encoding` glyph names, then printable ASCII; (3) `Type3Glyph::advance_width`; (4) `InterpreterSettings::ocg_overrides` for viewer layer toggles; (5) soft masks whose group lacks `/CS` were dropped (Chrome gradient text) | (1) `raster::tests::widget_appearance_states`; (2, 3) `xtask text-oracle` on corpus/pdfjs issue918; (4) `raster::tests::layer_override_hides_content`; (5) `raster::tests::soft_mask_without_group_colour_space_is_applied` |

| hayro | 0.7.1 | Apache-2.0 OR MIT | `RenderSettings::{x_offset, y_offset}` to render one tile of a large page | `raster::tests::tiles_match_full_render` |
| hayro-syntax | 0.7.2 | Apache-2.0 OR MIT | Page-tree cycle guard in `resolve_pages` (visited set + depth cap): a `/Kids` loop among compressed objects overflowed the stack and aborted the process | `cos/tests/writer.rs::page_tree_loops_in_object_streams_do_not_crash_readers` |

Temporary: hayro is the bootstrap renderer (ADR-0004); M2.6 replaces it with our own devices.
