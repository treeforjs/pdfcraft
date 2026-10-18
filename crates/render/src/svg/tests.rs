use std::sync::Arc;

use super::*;
use crate::{PageRenderer, RenderConfig};

/// Object `n` (1-based) of a classic-xref PDF built from `objects`.
fn build(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut o = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    o.extend_from_slice(data);
    o.extend_from_slice(b"\nendstream");
    o
}

/// A one-page document: page entries `page`, content `content`, more objects from 5 on.
fn page_pdf(page: &str, content: &str, extra: &[Vec<u8>]) -> Vec<u8> {
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R {page} >>").into_bytes(),
        stream("", content.as_bytes()),
    ];
    objects.extend_from_slice(extra);
    build(&objects)
}

fn helvetica() -> Vec<u8> {
    b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()
}

fn export(bytes: Vec<u8>, options: &SvgOptions) -> SvgPage {
    let renderer = PageRenderer::new(Arc::new(bytes), RenderConfig::default());
    let page = renderer.svg(0, options).unwrap();
    // Every export is well-formed XML with an SVG root.
    let doc = roxmltree::Document::parse(&page.svg).unwrap_or_else(|e| panic!("{e}\n{}", page.svg));
    assert_eq!(doc.root_element().tag_name().name(), "svg");
    assert_eq!(doc.root_element().tag_name().namespace(), Some("http://www.w3.org/2000/svg"));
    page
}

fn svg_of(page: &str, content: &str, extra: &[Vec<u8>]) -> SvgPage {
    export(page_pdf(page, content, extra), &SvgOptions::default())
}

fn elements<'a>(doc: &'a roxmltree::Document<'a>, name: &str) -> Vec<roxmltree::Node<'a, 'a>> {
    doc.descendants().filter(|n| n.tag_name().name() == name).collect()
}

#[test]
fn paths_keep_colour_fill_rule_and_stroke_style() {
    let content = "1 0 0 rg 10 10 50 50 re f \
                   0 0 1 RG 2 w 1 J 2 j [3 2] 1 d 10 10 m 100 50 l S \
                   0 1 0 rg 0 0 0 RG 0 w 120 10 m 190 10 l 190 90 l h 130 20 m 180 20 l 180 80 l h B*";
    let page = svg_of("", content, &[]);
    assert_eq!((page.width, page.height), (200.0, 100.0));
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    let root = doc.root_element();
    assert_eq!(root.attribute("viewBox"), Some("0 0 200 100"));
    assert_eq!(root.attribute("width"), Some("200pt"));
    let paths = elements(&doc, "path");
    // The red square, flipped into SVG's top-left space: y 10–60 in PDF is 40–90.
    let red = paths.iter().find(|p| p.attribute("fill") == Some("#ff0000")).unwrap();
    assert_eq!(red.attribute("d"), Some("M10 90 L60 90 L60 40 L10 40 Z"));
    assert_eq!(red.attribute("fill-rule"), None);
    let line = paths.iter().find(|p| p.attribute("stroke") == Some("#0000ff")).unwrap();
    assert_eq!(line.attribute("fill"), Some("none"));
    assert_eq!(line.attribute("stroke-width"), Some("2"));
    assert_eq!(line.attribute("stroke-linecap"), Some("round"));
    assert_eq!(line.attribute("stroke-linejoin"), Some("bevel"));
    assert_eq!(line.attribute("stroke-dasharray"), Some("3 2"));
    assert_eq!(line.attribute("stroke-dashoffset"), Some("1"));
    // The stroke keeps the CTM as its pen; the page's flip is that CTM here.
    assert_eq!(line.attribute("transform"), Some("matrix(1 0 0 -1 0 100)"));
    assert_eq!(line.attribute("d"), Some("M10 10 L100 50"));
    let ring = paths.iter().find(|p| p.attribute("fill") == Some("#00ff00")).unwrap();
    assert_eq!(ring.attribute("fill-rule"), Some("evenodd"));
    let hairline = paths.iter().find(|p| p.attribute("stroke") == Some("#000000")).unwrap();
    assert_eq!(hairline.attribute("vector-effect"), Some("non-scaling-stroke"));
    assert_eq!(page.stats.paths, 4);
    assert_eq!(page.stats.omitted, 0);
    assert!(!page.stats.truncated);
}

#[test]
fn colours_are_converted_to_srgb() {
    let page = svg_of("", "0.5 g 0 0 10 10 re f 0 1 0 0 k 20 0 10 10 re f /CS0 cs 1 sc 40 0 10 10 re f", &[]);
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    let fills: Vec<&str> = elements(&doc, "path").iter().filter_map(|p| p.attribute("fill")).collect();
    assert_eq!(fills.first(), Some(&"#808080"), "{fills:?}");
    // CMYK magenta goes through the CMYK profile: much red, little green.
    let magenta = fills.get(1).unwrap();
    let r = u8::from_str_radix(&magenta[1..3], 16).unwrap();
    let g = u8::from_str_radix(&magenta[3..5], 16).unwrap();
    assert!(r > 180 && g < 80, "{magenta}");
}

#[test]
fn constant_alpha_blend_modes_and_clips_are_kept() {
    let content = "/GS1 gs 1 0 0 rg 0 0 100 100 re W n 10 10 50 50 re f";
    let page = svg_of("/Resources << /ExtGState << /GS1 << /ca 0.5 /BM /Multiply >> >> >>", content, &[]);
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    let red = elements(&doc, "path").into_iter().find(|p| p.attribute("fill") == Some("#ff0000")).unwrap();
    assert_eq!(red.attribute("fill-opacity"), Some("0.5"));
    assert_eq!(red.attribute("style"), Some("mix-blend-mode:multiply"));
    let clip = elements(&doc, "clipPath");
    assert_eq!(clip.len(), 1);
    // The square is drawn inside the clip's group.
    let id = clip[0].attribute("id").unwrap();
    assert!(red.ancestors().any(|a| a.attribute("clip-path") == Some(format!("url(#{id})").as_str())));
    assert_eq!(page.stats.clips, 1);
}

#[test]
fn rotated_and_cropped_pages_use_the_displayed_box() {
    let page = svg_of("/Rotate 90", "1 0 0 rg 0 0 10 10 re f", &[]);
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    assert_eq!(doc.root_element().attribute("viewBox"), Some("0 0 100 200"));
    assert_eq!((page.width, page.height), (100.0, 200.0));
    // Turned a quarter clockwise, the bottom-left corner of the page is at the top left.
    let red = elements(&doc, "path").into_iter().find(|p| p.attribute("fill") == Some("#ff0000")).unwrap();
    assert_eq!(red.attribute("d"), Some("M0 0 L0 10 L10 10 L10 0 Z"));

    let page = svg_of("/CropBox [50 0 150 100]", "1 0 0 rg 50 0 10 10 re f", &[]);
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    assert_eq!(doc.root_element().attribute("viewBox"), Some("0 0 100 100"));
    let red = elements(&doc, "path").into_iter().find(|p| p.attribute("fill") == Some("#ff0000")).unwrap();
    assert_eq!(red.attribute("d"), Some("M0 100 L10 100 L10 90 L0 90 Z"));
}

#[test]
fn images_are_embedded_as_png() {
    // 2 × 1 RGB: red, blue.
    let image = stream("/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[255, 0, 0, 0, 0, 255]);
    let page = svg_of("/Resources << /XObject << /Im1 5 0 R >> >>", "q 100 0 0 50 10 20 cm /Im1 Do Q", &[image]);
    assert_eq!(page.stats.images, 1);
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    let image = elements(&doc, "image").into_iter().next().unwrap();
    assert_eq!((image.attribute("width"), image.attribute("height")), (Some("2"), Some("1")));
    let href = image.attribute(("http://www.w3.org/1999/xlink", "href")).unwrap();
    let png = base64::engine::general_purpose::STANDARD.decode(href.strip_prefix("data:image/png;base64,").unwrap()).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut pixels).unwrap();
    assert_eq!(&pixels[..8], &[255, 0, 0, 255, 0, 0, 255, 255]);
    // Image space (2 × 1 px) lands on the 100 × 50 pt box at (10, 20) from the bottom left.
    let t: Vec<f64> =
        image.attribute("transform").unwrap().trim_start_matches("matrix(").trim_end_matches(')').split(' ').map(|v| v.parse().unwrap()).collect();
    let map = |x: f64, y: f64| (t[0] * x + t[2] * y + t[4], t[1] * x + t[3] * y + t[5]);
    let ((x0, y0), (x1, y1)) = (map(0.0, 0.0), map(2.0, 1.0));
    assert!((x0 - 10.0).abs() < 1e-3 && (y0 - 30.0).abs() < 1e-3 && (x1 - 110.0).abs() < 1e-3 && (y1 - 80.0).abs() < 1e-3, "{t:?}");
}

#[test]
fn large_images_are_downsampled() {
    let (w, h) = (2100u32, 2100u32);
    let image = stream(
        &format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8"),
        &vec![128; (w * h) as usize],
    );
    let page = svg_of("/Resources << /XObject << /Im1 5 0 R >> >>", "q 100 0 0 50 10 20 cm /Im1 Do Q", &[image]);
    assert_eq!((page.stats.images, page.stats.images_downsampled), (1, 1));
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    let image = elements(&doc, "image").into_iter().next().unwrap();
    assert_eq!(image.attribute("width"), Some("1050"));
}

#[test]
fn text_is_drawn_as_glyph_outlines_with_a_transparent_text_layer() {
    let content = "BT /F1 24 Tf 20 40 Td (Hi <&>) Tj ET";
    let bytes = page_pdf("/Resources << /Font << /F1 5 0 R >> >>", content, &[helvetica()]);
    let page = export(bytes.clone(), &SvgOptions::default());
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    // H, i, <, &, > (the space draws nothing).
    assert_eq!(page.stats.glyphs, 5, "{:?}", page.stats);
    let uses = elements(&doc, "use");
    assert_eq!(uses.len(), 5);
    for u in &uses {
        let href = u.attribute(("http://www.w3.org/1999/xlink", "href")).unwrap();
        let id = href.trim_start_matches('#');
        let def = doc.descendants().find(|n| n.attribute("id") == Some(id)).unwrap();
        assert_eq!(def.tag_name().name(), "path");
        assert!(def.attribute("d").unwrap().starts_with('M'));
        assert_eq!(u.attribute("fill"), Some("#000000"));
    }
    let texts = elements(&doc, "text");
    assert_eq!(texts.len(), 1);
    assert_eq!(texts[0].text(), Some("Hi <&>"));
    assert!(texts[0].ancestors().any(|a| a.attribute("fill-opacity") == Some("0")));
    assert!(page.svg.contains("Hi &lt;&amp;&gt;"));
    assert_eq!(page.stats.text_lines, 1);

    let plain = export(bytes, &SvgOptions { text_layer: false, transparent: true });
    let doc = roxmltree::Document::parse(&plain.svg).unwrap();
    assert!(elements(&doc, "text").is_empty());
    assert!(elements(&doc, "rect").is_empty(), "no paper with transparent");
    assert_eq!(elements(&doc, "use").len(), 5);
}

#[test]
fn glyph_outlines_are_defined_once() {
    let page = svg_of("/Resources << /Font << /F1 5 0 R >> >>", "BT /F1 12 Tf 10 50 Td (aaaa) Tj ET", &[helvetica()]);
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    assert_eq!(elements(&doc, "use").len(), 4);
    assert_eq!(elements(&doc, "defs").len(), 1);
}

#[test]
fn nested_forms_are_flattened_and_looping_forms_end() {
    let inner = stream("/Type /XObject /Subtype /Form /BBox [0 0 200 100]", b"0 0 1 rg 0 0 10 10 re f");
    let outer =
        stream("/Type /XObject /Subtype /Form /BBox [0 0 200 100] /Matrix [1 0 0 1 50 0] /Resources << /XObject << /Inner 5 0 R >> >>", b"/Inner Do");
    let page = svg_of("/Resources << /XObject << /Outer 6 0 R >> >>", "/Outer Do", &[inner, outer]);
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    let blue = elements(&doc, "path").into_iter().find(|p| p.attribute("fill") == Some("#0000ff")).unwrap();
    assert_eq!(blue.attribute("d"), Some("M50 100 L60 100 L60 90 L50 90 Z"));

    // A form that draws itself: the interpreter's nesting limit ends it.
    let looping =
        stream("/Type /XObject /Subtype /Form /BBox [0 0 200 100] /Resources << /XObject << /Self 5 0 R >> >>", b"1 0 0 rg 0 0 5 5 re f /Self Do");
    let page = svg_of("/Resources << /XObject << /Self 5 0 R >> >>", "/Self Do", &[looping]);
    assert!(page.stats.paths >= 1 && page.stats.paths <= 64, "{:?}", page.stats);
    // Two forms drawing each other.
    let a = stream("/Type /XObject /Subtype /Form /BBox [0 0 200 100] /Resources << /XObject << /B 6 0 R >> >>", b"0 0 5 5 re f /B Do");
    let b = stream("/Type /XObject /Subtype /Form /BBox [0 0 200 100] /Resources << /XObject << /A 5 0 R >> >>", b"/A Do");
    let page = svg_of("/Resources << /XObject << /A 5 0 R >> >>", "/A Do", &[a, b]);
    assert!(page.stats.paths <= 64, "{:?}", page.stats);
}

#[test]
fn tiling_patterns_become_svg_patterns() {
    let cell = stream(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>",
        b"1 0 0 rg 0 0 5 5 re f",
    );
    let page = svg_of("/Resources << /Pattern << /P1 5 0 R >> >>", "/Pattern cs /P1 scn 0 0 100 100 re f", &[cell]);
    assert_eq!(page.stats.tiling_patterns, 1);
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    let pattern = elements(&doc, "pattern").into_iter().next().unwrap();
    assert_eq!((pattern.attribute("width"), pattern.attribute("height")), (Some("10"), Some("10")));
    let id = pattern.attribute("id").unwrap();
    // The cell is vector: its red square is a path inside the pattern.
    assert!(pattern.descendants().any(|n| n.attribute("fill") == Some("#ff0000")));
    assert!(elements(&doc, "path").iter().any(|p| p.attribute("fill") == Some(format!("url(#{id})").as_str())));
}

#[test]
fn shadings_are_rasterised_under_the_painted_shape() {
    let shading =
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 200 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >>";
    let pattern = format!("<< /Type /Pattern /PatternType 2 /Shading {shading} >>").into_bytes();
    let page = svg_of(
        "/Resources << /Pattern << /P1 5 0 R >> /Shading << /Sh1 6 0 R >> >>",
        "/Pattern cs /P1 scn 0 0 100 100 re f /Sh1 sh",
        &[pattern, shading.as_bytes().to_vec()],
    );
    assert_eq!(page.stats.shadings_rasterized, 2, "{:?}", page.stats);
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    let images = elements(&doc, "image");
    assert_eq!(images.len(), 2);
    let mask = images[0].attribute("mask").unwrap().trim_start_matches("url(#").trim_end_matches(')').to_string();
    let mask = doc.descendants().find(|n| n.attribute("id") == Some(mask.as_str())).unwrap();
    assert_eq!(mask.tag_name().name(), "mask");
    assert!(mask.descendants().any(|n| n.attribute("fill") == Some("#fff")));
}

#[test]
fn soft_masks_become_svg_masks() {
    let group = stream("/Type /XObject /Subtype /Form /BBox [0 0 200 100] /Group << /S /Transparency /CS /DeviceGray >>", b"1 g 0 0 50 100 re f");
    let page = svg_of(
        "/Resources << /ExtGState << /GS1 << /SMask << /Type /Mask /S /Luminosity /G 5 0 R >> >> >> >>",
        "/GS1 gs 1 0 0 rg 0 0 200 100 re f",
        &[group],
    );
    assert_eq!(page.stats.soft_masks, 1, "{:?}", page.stats);
    let doc = roxmltree::Document::parse(&page.svg).unwrap();
    let mask = elements(&doc, "mask").into_iter().next().unwrap();
    assert!(mask.descendants().any(|n| n.attribute("fill") == Some("#ffffff")));
    let id = mask.attribute("id").unwrap();
    assert!(doc.descendants().any(|n| n.attribute("mask") == Some(format!("url(#{id})").as_str())));
}

#[test]
fn hostile_pages_and_arguments_are_errors() {
    let renderer = PageRenderer::new(Arc::new(page_pdf("", "0 0 10 10 re f", &[])), RenderConfig::default());
    assert!(renderer.svg(1, &SvgOptions::default()).unwrap_err().contains("does not exist"));
    assert!(renderer.svg(usize::MAX, &SvgOptions::default()).is_err());
    let garbage = PageRenderer::new(Arc::new(b"not a pdf".to_vec()), RenderConfig::default());
    assert!(garbage.svg(0, &SvgOptions::default()).is_err());
    // Nonsense operands, huge coordinates and dashes: still a well-formed page.
    let page = svg_of("", "1e38 0 0 1e38 0 0 cm 0 0 1 1 re f [1e30 -1] 0 d 0 0 m 1 1 l S Q Q Q 0 0 0 0 0 0 cm 0 0 1 1 re f", &[]);
    assert!(page.stats.paths <= 1, "{:?}", page.stats);
}

#[test]
fn escaping_is_xml_safe() {
    assert_eq!(escape("a<b>&\"c'\u{1}\u{FFFF}\td"), "a&lt;b&gt;&amp;&quot;c&apos;\td");
    assert_eq!(num(f64::NAN), "0");
    assert_eq!(num(-0.0), "0");
    assert_eq!(num(0.5), "0.5");
}

#[test]
fn the_byte_budget_truncates_instead_of_growing() {
    let mut shared = Shared {
        next_id: 0,
        bytes: MAX_SVG_BYTES - 4,
        stats: SvgStats::default(),
        masks: HashMap::new(),
        glyphs: HashMap::new(),
        page: Rect::new(0.0, 0.0, 10.0, 10.0),
        depth: 0,
        samples: 0,
    };
    let mut device = SvgDevice::new(&mut shared);
    assert!(device.emit("abcd"));
    device.push_clip_path(&ClipPath { path: Rect::new(0.0, 0.0, 1.0, 1.0).to_path(0.1), fill: FillRule::NonZero });
    assert!(!device.emit("e"));
    device.pop_clip_path();
    let out = device.finish();
    assert_eq!(out, "abcd", "a clip that did not fit is not closed either");
    assert!(shared.stats.truncated);
}
