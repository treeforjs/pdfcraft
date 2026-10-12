use super::*;
use crate::{PageRenderer, RenderConfig, RenderRequest, Tile};
use hayro::hayro_syntax::Pdf;
use std::sync::{Arc, atomic::AtomicBool};

fn fixture(content: &str, page_extra: &str) -> Vec<u8> {
    fixture_bytes(content.as_bytes(), page_extra, "")
}

fn fixture_bytes(content: &[u8], page_extra: &str, stream_extra: &str) -> Vec<u8> {
    fixture_contents("4 0 R", content, page_extra, stream_extra)
}

fn fixture_contents(contents: &str, content: &[u8], page_extra: &str, stream_extra: &str) -> Vec<u8> {
    let mut stream = format!("<< /Length {} {stream_extra} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 100 80] >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /Contents {contents} {page_extra} >>").into_bytes(),
        stream,
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        bytes.extend_from_slice(object);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    bytes
}

fn pixel(bytes: &[u8], width: usize, x: usize, y: usize) -> &[u8] {
    &bytes[(y * width + x) * 4..(y * width + x + 1) * 4]
}

#[test]
fn rectangle_fill_and_even_odd_hole_render_on_the_native_device() {
    let pdf = Pdf::new(fixture("1 0 0 rg 10 10 60 50 re 25 25 20 20 re f*", "")).unwrap();
    let settings = InterpreterSettings::default();
    let page = &pdf.pages()[0];
    let list = compile_page(page, &settings).unwrap();
    let pixels =
        raster(&list, page, &settings, &hayro::RenderSettings { bg_color: hayro::vello_cpu::color::palette::css::WHITE, ..Default::default() })
            .unwrap();
    assert_eq!(pixel(&pixels, 100, 15, 30), [255, 0, 0, 255]);
    assert_eq!(pixel(&pixels, 100, 30, 45), [255; 4]);
    assert_eq!(pixel(&pixels, 100, 5, 5), [255; 4]);
}

#[test]
fn native_device_clips_and_restores_graphics_state() {
    let pdf = Pdf::new(fixture("q 0 0 30 80 re W n 0 0 1 rg 0 0 100 80 re f Q 1 0 0 rg 60 10 20 20 re f", "")).unwrap();
    let settings = InterpreterSettings::default();
    let page = &pdf.pages()[0];
    let pixels = raster(
        &compile_page(page, &settings).unwrap(),
        page,
        &settings,
        &hayro::RenderSettings { bg_color: hayro::vello_cpu::color::palette::css::WHITE, ..Default::default() },
    )
    .unwrap();
    assert_eq!(pixel(&pixels, 100, 20, 30), [0, 0, 255, 255]);
    assert_eq!(pixel(&pixels, 100, 50, 30), [255; 4]);
    assert_eq!(pixel(&pixels, 100, 65, 55), [255, 0, 0, 255]);
}

#[test]
fn crop_rotation_and_tiles_match_whole_native_render() {
    for rotation in [0, 90, 180, 270] {
        let bytes = Arc::new(fixture("0 0 1 rg 15 20 30 25 re f", &format!("/CropBox [10 10 90 70] /Rotate {rotation}")));
        let mut renderer = PageRenderer::new(bytes, RenderConfig { native_vector: true, ..Default::default() });
        let whole = renderer.render(RenderRequest { scale: 2.0, ..Default::default() });
        assert!(whole.error.is_none());
        // A cache hit at the same page must still honor tile offsets and dimensions.
        let tile = renderer.render(RenderRequest { scale: 2.0, tile: Some(Tile { x: 12, y: 8, w: 32, h: 24 }), ..Default::default() });
        assert!(tile.error.is_none());
        for y in 0..24 {
            for x in 0..32 {
                assert_eq!(pixel(&tile.rgba, 32, x, y), pixel(&whole.rgba, whole.width as usize, x + 12, y + 8), "rotation {rotation}");
            }
        }
        assert_eq!(renderer.stats().native_vector_rasterizations, 2, "rotation {rotation}");
    }
}

#[test]
fn unsupported_page_semantics_use_bootstrap_without_losing_late_content() {
    for extra in ["/Annots []", "/Group << /S /Transparency >>", "/UserUnit 2", "/Resources << /ColorSpace << /DefaultRGB /DeviceRGB >> >>"] {
        let pdf = Pdf::new(fixture("1 0 0 rg 10 10 20 20 re f", extra)).unwrap();
        assert!(compile_page(&pdf.pages()[0], &InterpreterSettings::default()).is_err(), "{extra}");
    }
    let config = RenderConfig { native_vector: true, ..Default::default() };
    let mut renderer = PageRenderer::new(Arc::new(fixture("1 0 0 rg 0 0 100 80 re f 0 1 0 0 k 10 10 20 20 re f", "")), config);
    let pixels = renderer.render(RenderRequest { scale: 1.0, ..Default::default() });
    assert!(pixels.error.is_none());
    assert_eq!((renderer.native_vector_attempts(), renderer.stats().native_vector_rasterizations), (1, 0));
    // The unsupported CMYK operator must still affect the second rectangle, not disappear
    // after the first native-compatible red rectangle was drawn.
    assert_ne!(pixel(&pixels.rgba, 100, 15, 65), [255, 0, 0, 255]);
}

const CANVAS: Rect = Rect::new(0.0, 0.0, 100.0, 80.0);

fn white() -> hayro::RenderSettings {
    hayro::RenderSettings { bg_color: hayro::vello_cpu::color::palette::css::WHITE, ..Default::default() }
}

fn native(content: &str) -> Pixels {
    let pdf = Pdf::new(fixture(content, "")).unwrap();
    let settings = InterpreterSettings::default();
    let page = &pdf.pages()[0];
    raster(&compile_page(page, &settings).unwrap(), page, &settings, &white()).unwrap()
}

#[test]
fn native_work_limits_refuse_tiny_dash_period_and_extreme_geometry() {
    for content in ["[0.00000000001] 0 d 0 0 m 100 0 l S", "0 0 0 0 0 0 cm 0 0 m 1 1 l S", "0 0 100000000 100000000 re f"] {
        let list = display_list::compile(content.as_bytes(), || false).unwrap();
        assert!(plan(&list, Affine::IDENTITY, CANVAS, || false).is_err(), "{content}");
    }
}

#[test]
fn huge_dash_phase_over_a_tiny_period_strokes_quickly() {
    // A 1e9 phase over a 0.002 period is reduced to one period when the pattern is set, so no
    // device step depends on how many periods the phase spans.
    let started = std::time::Instant::now();
    let pixels = native("[0.5] 999999999 d 5 w 10 40 m 90 40 l 10 60 m 90 60 l S");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    // Half-pixel dashes: every pixel along the line is about half covered.
    let gray = pixel(&pixels, 100, 50, 80 - 40)[0];
    assert!((32..224).contains(&gray), "{gray}");
    let list = display_list::compile(b"[0.001] 1000000000 d 0 0 m 1 0 l S", || false).unwrap();
    let Paint::Stroke { style, .. } = &list.draws[0].paint else { panic!("stroke") };
    assert!(style.dash_offset >= 0.0 && style.dash_offset < 0.002, "{}", style.dash_offset);
    let started = std::time::Instant::now();
    native("[0.001] 1000000000 d 0 0 m 1 0 l S");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn wide_round_strokes_under_a_shrinking_pen_are_stroked_in_device_sized_units() {
    // A pen scaled by 1e-6 and a 2e7-unit round-joined line are 20 device pixels wide: the
    // stroke is expanded in uniformly rescaled pen space, where that width is 20 units.
    let mut content = String::from("0.000001 0 0 0.000001 0 0 cm 20000000 w 1 j 1 J 10000000 10000000 m ");
    for i in 0..400 {
        content.push_str(&format!("{} {} l ", 10_000_000 + (i % 2) * 70_000_000, 10_000_000 + i * 150_000));
    }
    content.push('S');
    let list = display_list::compile(content.as_bytes(), || false).unwrap();
    let steps = plan(&list, Affine::IDENTITY, CANVAS, || false).unwrap();
    let Some(Step::Stroke(_, setup)) = steps.first() else { panic!("stroke") };
    assert!((setup.style.width - 20.0).abs() < 1e-6, "{}", setup.style.width);
    let started = std::time::Instant::now();
    let pixels = native(&content);
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(pixel(&pixels, 100, 10, 80 - 10), [0, 0, 0, 255]);
}

#[test]
fn hairlines_and_thin_strokes_are_one_device_pixel_and_may_be_dashed() {
    let pixels = native("0 w 0 39.5 m 100 39.5 l S 0.01 w 50.5 0 m 50.5 80 l S [10 10] 0 d 0 w 0 20.5 m 100 20.5 l S");
    // Each line covers one device pixel row/column fully.
    assert_eq!(pixel(&pixels, 100, 20, 80 - 40), [0, 0, 0, 255]);
    assert_eq!(pixel(&pixels, 100, 50, 10), [0, 0, 0, 255]);
    // The dashed hairline: on for x in 0..10, off in 10..20.
    assert_eq!(pixel(&pixels, 100, 5, 80 - 21), [0, 0, 0, 255]);
    assert_eq!(pixel(&pixels, 100, 15, 80 - 21), [255; 4]);
}

#[test]
fn a_clip_shared_by_many_draws_is_pushed_once_and_offscreen_draws_are_skipped() {
    let mut content = String::from("0 0 100 80 re W n ");
    for i in 0..2000 {
        content.push_str(&format!("{} 10 1 1 re f ", i % 90));
    }
    content.push_str("1000 1000 5 5 re f 2 w 2000 2000 m 2100 2000 l S");
    let list = display_list::compile(content.as_bytes(), || false).unwrap();
    // Scaled up, every draw pushing its clip again charged 2000 × the clip's perimeter.
    let transform = Affine::scale(40.0);
    let canvas = Rect::new(0.0, 0.0, 4000.0, 3200.0);
    let steps = plan(&list, transform, canvas, || false).unwrap();
    assert_eq!(steps.iter().filter(|s| matches!(s, Step::Push(_))).count(), 1);
    assert_eq!(steps.iter().filter(|s| matches!(s, Step::Pop)).count(), 1);
    assert_eq!(steps.iter().filter(|s| matches!(s, Step::Fill(..))).count(), 2000);
    assert!(!steps.iter().any(|s| matches!(s, Step::Stroke(..))));
}

#[test]
fn nested_clips_are_popped_and_pushed_as_the_clipping_path_changes() {
    let content = "q 0 0 50 80 re W n q 0 0 100 40 re W n 1 0 0 rg 0 0 100 80 re f Q 0 0 1 rg 0 40 100 40 re f Q 0 1 0 rg 60 50 10 10 re f";
    let list = display_list::compile(content.as_bytes(), || false).unwrap();
    let steps = plan(&list, Affine::IDENTITY, CANVAS, || false).unwrap();
    let kinds: String = steps
        .iter()
        .map(|s| match s {
            Step::Push(_) => 'u',
            Step::Pop => 'o',
            Step::Fill(..) => 'f',
            Step::Stroke(..) => 's',
        })
        .collect();
    assert_eq!(kinds, "uufofof");
    let pixels = native(content);
    assert_eq!(pixel(&pixels, 100, 20, 80 - 20), [255, 0, 0, 255]);
    assert_eq!(pixel(&pixels, 100, 20, 80 - 60), [0, 0, 255, 255]);
    assert_eq!(pixel(&pixels, 100, 75, 80 - 20), [255; 4]);
    assert_eq!(pixel(&pixels, 100, 65, 80 - 55), [0, 255, 0, 255]);
}

#[test]
fn the_native_path_is_opt_in_and_the_default_never_attempts_it() {
    // Nothing in the render crate's tests turns the process-wide default on.
    assert!(!crate::native_vector_default());
    assert!(!RenderConfig::default().native_vector);
    let bytes = Arc::new(fixture("0 0 1 rg 10 10 20 20 re f", ""));
    let mut native = PageRenderer::new(bytes.clone(), RenderConfig { native_vector: true, ..Default::default() });
    let mut default = PageRenderer::new(bytes, RenderConfig::default());
    let a = native.render(RenderRequest { scale: 1.0, ..Default::default() });
    let b = default.render(RenderRequest { scale: 1.0, ..Default::default() });
    // Zoom and tile requests of the default renderer stay off the native path too.
    let tile = default.render(RenderRequest { scale: 2.0, tile: Some(Tile { x: 0, y: 0, w: 64, h: 64 }), ..Default::default() });
    assert!(a.error.is_none() && b.error.is_none() && tile.error.is_none());
    assert_eq!(native.stats().native_vector_rasterizations, 1);
    assert_eq!(native.native_vector_attempts(), 1);
    assert_eq!(default.stats().native_vector_rasterizations, 0);
    assert_eq!(default.native_vector_attempts(), 0, "the default path must not reach native code");
    // A solid axis-aligned rectangle is the same on both devices.
    assert_eq!(a.rgba, b.rgba);
}

#[test]
fn application_settings_name_the_opt_in() {
    for on in ["1", "true", " YES ", "On"] {
        assert!(crate::native_vector_requested(Some(on)), "{on:?}");
    }
    for off in [None, Some(""), Some("0"), Some("false"), Some("no"), Some("2"), Some("native")] {
        assert!(!crate::native_vector_requested(off), "{off:?}");
    }
}

#[test]
fn fallback_reasons_name_what_the_page_needs() {
    let reason = |content: &str| crate::display_list::compile(content.as_bytes(), || false).unwrap_err().0;
    assert!(reason("BT /F1 12 Tf ET").contains("unsupported content operator BT"), "{}", reason("BT /F1 12 Tf ET"));
    assert!(reason("q /GS0 gs Q").contains("name operand"));
    assert!(reason("0 0 m (x) Tj").contains("string or dictionary operand"));
}

#[test]
fn cancelled_candidate_does_not_poison_the_cached_page() {
    let pdf = Pdf::new(fixture("0 0 1 rg 10 10 20 20 re f", "")).unwrap();
    let stop = Arc::new(AtomicBool::new(true));
    let settings = InterpreterSettings { cancelled: Some(stop.clone()), ..Default::default() };
    let cache = Cache::default();
    let rs = hayro::RenderSettings::default();
    assert!(cache.render(0, &pdf.pages()[0], &settings, &rs).is_none());
    assert!(cache.page.borrow().is_none());
    stop.store(false, Ordering::Relaxed);
    assert!(cache.render(0, &pdf.pages()[0], &settings, &rs).is_some());
    assert!(cache.page.borrow().as_ref().unwrap().1.is_some());
}

#[test]
fn strict_native_decoding_supports_bounded_filter_chains() {
    use pdfcraft_filters::{Filter, Params, encode};
    let source = b"1 0 0 rg 10 10 20 20 re f";
    let params = Params::default();
    // The bootstrap reader accepts the inline-image abbreviation in streams too.
    for name in ["FlateDecode", "Fl"] {
        let data = encode(&Filter::Flate, &params, source).unwrap();
        let pdf = Pdf::new(fixture_bytes(&data, "", &format!("/Filter /{name}"))).unwrap();
        assert_eq!(compile_page(&pdf.pages()[0], &InterpreterSettings::default()).unwrap().draws.len(), 1);
    }
    let data = encode(&Filter::Flate, &params, &encode(&Filter::Flate, &params, source).unwrap()).unwrap();
    let pdf = Pdf::new(fixture_bytes(&data, "", "/Filter [/FlateDecode /FlateDecode] /DecodeParms [null null]")).unwrap();
    assert_eq!(compile_page(&pdf.pages()[0], &InterpreterSettings::default()).unwrap().draws.len(), 1);
}

#[test]
fn native_decoder_rejects_partial_intermediates_before_next_filter() {
    let mut decoded = pdfcraft_filters::encode_flate(b"0 0 1 1 re f");
    decoded.extend(std::iter::repeat_n(b' ', display_list::MAX_CONTENT + 16));
    let data = pdfcraft_filters::encode_flate(&decoded);
    let pdf = Pdf::new(fixture_bytes(&data, "", "/Filter [/FlateDecode /FlateDecode]")).unwrap();
    assert!(compile_page(&pdf.pages()[0], &InterpreterSettings::default()).is_err());
    for attributes in ["/Filter /Unknown", "/Filter [/FlateDecode 2]", "/Filter /FlateDecode /DecodeParms << /Columns 1.5 >>"] {
        let pdf = Pdf::new(fixture_bytes(b"0 0 1 1 re f", "", attributes)).unwrap();
        assert!(compile_page(&pdf.pages()[0], &InterpreterSettings::default()).is_err(), "{attributes}");
    }
}

#[test]
fn unvalidated_end_marker_filters_and_external_stream_keys_fall_back() {
    for attributes in [
        "/Filter /LZWDecode",
        "/Filter /RunLengthDecode",
        "/Filter /ASCIIHexDecode",
        "/Filter /ASCII85Decode",
        "/FFilter /FlateDecode",
        "/FDecodeParms << >>",
        "/F (external.bin)",
        "/DP << >>",
        "/Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 4 >>",
        "/Filter /Crypt",
    ] {
        let pdf = Pdf::new(fixture_bytes(b"0 0 1 1 re f", "", attributes)).unwrap();
        assert!(compile_page(&pdf.pages()[0], &InterpreterSettings::default()).is_err(), "{attributes}");
    }
}

#[test]
fn actual_encoded_size_is_checked_before_copy_or_decryption() {
    let data = vec![b' '; 4 * 1024 * 1024 + 1];
    let pdf = Pdf::new(fixture_bytes(&data, "", "")).unwrap();
    let stream = pdf.pages()[0].raw().get::<Stream<'_>>(b"Contents").unwrap();
    assert!(stream.raw_data_within(4 * 1024 * 1024).is_none());
    assert!(compile_page(&pdf.pages()[0], &InterpreterSettings::default()).is_err());
}

#[test]
fn one_stream_named_many_times_shares_the_page_encoded_budget() {
    // Raw deflate made of empty stored blocks: about 1 MiB that inflates to nothing.
    let mut empty = [0u8, 0, 0, 0xff, 0xff].repeat(200_000);
    empty.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    for (count, ok) in [(15, true), (17, false)] {
        let contents = format!("[{}]", "4 0 R ".repeat(count));
        let pdf = Pdf::new(fixture_contents(&contents, &empty, "", "/Filter /FlateDecode")).unwrap();
        let list = compile_page(&pdf.pages()[0], &InterpreterSettings::default());
        assert_eq!(list.is_ok(), ok, "{count}: {:?}", list.err());
    }
}

#[test]
fn zero_length_dashes_with_round_caps_are_dots() {
    // Each zero-length dash is a 4-unit dot (ISO 32000-2 §8.4.3.6); kurbo dropped exact ones.
    let pixels = native("4 w 1 J [0 8] 0 d 10 40.5 m 90 40.5 l S [0 8] 4 d 2 J 10 20.5 m 90 20.5 l S");
    for x in [10, 18, 26, 50, 82] {
        assert_eq!(pixel(&pixels, 100, x, 80 - 41), [0, 0, 0, 255], "round dot at {x}");
        assert_eq!(pixel(&pixels, 100, x + 4, 80 - 41), [255; 4], "gap after {x}");
    }
    // With phase 4 the square dots sit at 14, 22, …
    for x in [14, 22, 86] {
        assert_eq!(pixel(&pixels, 100, x, 80 - 21), [0, 0, 0, 255], "square dot at {x}");
    }
}

#[test]
fn zero_width_and_zero_height_fills_are_hairlines_like_the_bootstrap_renderer() {
    // Table rules drawn as fills of zero-height and zero-width rectangles under a flipped,
    // scaled CTM (the shape of pdf.js issue4260_reduced.pdf, rewritten): the native path drew
    // nothing where the bootstrap renderer, pdf.js and Poppler draw one-pixel lines.
    let content = "q 0.5 0 0 -0.5 0 80 cm 0 0 1 rg 20 40 160 0 re f 30 20 0 100 re f 1 0 0 rg 20 130 m 180 130 l h f* \
                   0 0 0 rg 20 20 0 0 re f 20 150 160 0.0001 re B Q";
    let bytes = Arc::new(fixture(content, ""));
    let mut native = PageRenderer::new(bytes.clone(), RenderConfig { native_vector: true, ..Default::default() });
    let mut bootstrap = PageRenderer::new(bytes, RenderConfig::default());
    for scale in [1.0, 2.5] {
        let a = native.render(RenderRequest { scale, ..Default::default() });
        let b = bootstrap.render(RenderRequest { scale, ..Default::default() });
        assert!(a.error.is_none() && b.error.is_none());
        let w = a.width as usize;
        // The horizontal rule (user y 40 → page y 60) and the vertical one (user x 30 → page x 15).
        let row = ((80.0 - 60.0) * scale) as usize;
        let col = (15.0 * scale) as usize;
        assert!(
            pixel(&a.rgba, w, (50.0 * scale) as usize, row)[2] > 128 && pixel(&a.rgba, w, (50.0 * scale) as usize, row)[0] < 255,
            "scale {scale}"
        );
        assert!(pixel(&a.rgba, w, col, (50.0 * scale) as usize)[0] < 255, "scale {scale}");
        let max = a.rgba.iter().zip(b.rgba.iter()).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        assert!(max <= 2, "native and bootstrap differ by {max} at scale {scale}");
    }
    assert_eq!(native.stats().native_vector_rasterizations, 2);
}

#[test]
fn degenerate_fill_detection_uses_user_space_bounds() {
    let list = crate::display_list::compile(b"2 0 0 2 0 0 cm 10 10 50 0 re f 10 20 50 1 re f", || false).unwrap();
    assert!(matches!(list.draws[0].paint, Paint::Stroke { .. }), "a zero-height fill is a hairline");
    assert!(matches!(list.draws[1].paint, Paint::Fill { .. }), "a fill with area stays a fill");
    // The hairline is the user box diagonal mapped by the CTM.
    let points: Vec<_> = list.draws[0].path.elements().to_vec();
    assert_eq!(points, vec![PathEl::MoveTo(Point::new(20.0, 20.0)), PathEl::LineTo(Point::new(120.0, 20.0))]);
}
