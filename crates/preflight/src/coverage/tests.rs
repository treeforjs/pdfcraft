use std::sync::Arc;

use super::*;

fn stream(extra: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} {extra} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A one-page document: object 3 is the page, 4 its content; `extra` objects follow from 5.
fn doc_with(media: &str, resources: &str, content: &[u8], extra: &[Vec<u8>]) -> Document {
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox {media} /Resources {resources} /Contents 4 0 R >>").into_bytes(),
        stream("", content),
    ];
    objs.extend(extra.iter().cloned());
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

fn doc(content: &str) -> Document {
    doc_with("[0 0 612 792]", "<< >>", content.as_bytes(), &[])
}

fn measure(d: &Document, options: &Options) -> PageCoverage {
    page_coverage(d, 0, options).unwrap()
}

#[test]
fn rich_black_box_is_flagged_with_its_region() {
    let d = doc("1 1 1 1 k 100 100 100 100 re f");
    let c = measure(&d, &Options::default());
    assert_eq!(c.max, 400.0);
    assert!(c.flagged());
    assert_eq!(c.region_count, 1);
    assert!((c.over_area - 10_000.0).abs() < 1.0, "{}", c.over_area);
    assert!((c.over_percent - 10_000.0 / (612.0 * 792.0) * 100.0).abs() < 0.01);
    let r = &c.regions[0];
    assert_eq!(r.max, 400.0);
    for (got, want) in r.bbox.iter().zip([100.0, 100.0, 200.0, 200.0]) {
        assert!((got - want).abs() <= 1.0, "{:?}", r.bbox);
    }
    assert_eq!((c.exact_paints, c.converted_paints), (1, 0));
    assert!(!c.truncated);
}

#[test]
fn black_text_is_not_flagged() {
    let d = doc_with(
        "[0 0 612 792]",
        "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>",
        b"BT 0 0 0 1 k /F1 24 Tf 72 700 Td (Total area coverage) Tj 0 -30 Td [(Kerned) -200 (text)] TJ ET",
        &[],
    );
    let c = measure(&d, &Options::default());
    assert_eq!(c.max, 100.0);
    assert!(!c.flagged());
    assert_eq!(c.over_area, 0.0);
    assert!(c.regions.is_empty());
    assert_eq!(c.exact_paints, 2);
}

#[test]
fn the_limit_decides_what_is_flagged() {
    let d = doc("0.7 0.7 0.7 0.7 k 0 0 50 50 re f");
    assert!(!measure(&d, &Options::default()).flagged());
    let low = measure(&d, &Options { limit: 250.0, ..Options::default() });
    assert!(low.flagged());
    assert!((low.max - 280.0).abs() < 0.01);
    // Exactly at the limit is not over it.
    assert!(!measure(&d, &Options { limit: 280.0, ..Options::default() }).flagged());
}

#[test]
fn spot_and_devicen_inks_add_to_the_total() {
    let resources = "<< /ColorSpace << /Spot [/Separation /Gold /DeviceCMYK 6 0 R] /N [/DeviceN [/Cyan /Magenta /Yellow /Black /Gold] /DeviceCMYK 6 0 R] /All [/Separation /All /DeviceCMYK 6 0 R] /Off [/Separation /None /DeviceCMYK 6 0 R] >> >>";
    let content =
        b"/Spot cs 0.5 scn 0 0 10 10 re f /N cs 1 1 1 1 1 scn 100 0 10 10 re f /All cs 0.5 scn 200 0 10 10 re f /Off cs 1 scn 300 0 10 10 re f";
    let f = b"<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0 1 0] /N 1 >>".to_vec();
    let d = doc_with("[0 0 612 792]", resources, content, &[Vec::new(), f]);
    let c = measure(&d, &Options::default());
    assert_eq!(c.max, 500.0);
    assert_eq!(c.region_count, 1);
    assert!((c.regions[0].bbox[0] - 100.0).abs() <= 1.0);
    let low = measure(&d, &Options { limit: 40.0, ..Options::default() });
    // Spot 50 %, DeviceN 500 %, All at 50 % = 200 %; None paints no ink.
    assert_eq!(low.region_count, 3);
}

#[test]
fn later_objects_knock_out_earlier_ink() {
    let d = doc("1 1 1 1 k 0 0 100 100 re f 0 0 0 0 k 0 0 50 100 re f");
    let c = measure(&d, &Options::default());
    assert!((c.over_area - 5_000.0).abs() < 1.0, "{}", c.over_area);
    let r = &c.regions[0];
    assert!((r.bbox[0] - 50.0).abs() <= 1.0, "{:?}", r.bbox);
}

#[test]
fn rgb_is_converted_and_says_so() {
    let d = doc("0 0 0 rg 0 0 100 100 re f 1 0 0 rg 200 0 100 100 re f");
    let c = measure(&d, &Options::default());
    assert_eq!(c.max, 200.0);
    assert_eq!((c.exact_paints, c.converted_paints), (0, 2));
    assert!(!c.flagged());
}

#[test]
fn clips_and_form_bounding_boxes_limit_painting() {
    let form =
        stream("/Type /XObject /Subtype /Form /BBox [0 0 20 20] /Resources << /XObject << /Me 5 0 R >> >>", b"1 1 1 1 k 0 0 500 500 re f /Me Do");
    let d = doc_with(
        "[0 0 612 792]",
        "<< /XObject << /Fm 5 0 R >> >>",
        b"q 10 10 30 30 re W n 1 1 1 1 k 0 0 600 600 re f Q q 1 0 0 1 300 300 cm /Fm Do Q",
        &[form],
    );
    let c = measure(&d, &Options::default());
    assert_eq!(c.region_count, 2);
    let total: f64 = c.regions.iter().map(|r| r.area).sum();
    assert!((total - 900.0 - 400.0).abs() < 2.0, "{total}");
}

#[test]
fn strokes_and_curves_are_measured() {
    let d = doc("1 1 1 1 K 10 w 100 100 m 300 100 l S 0.9 0.9 0.9 0.9 k 400 400 m 400 500 500 500 500 400 c h f");
    let c = measure(&d, &Options::default());
    assert_eq!(c.max, 400.0);
    let stroke = c.regions.iter().find(|r| r.max == 400.0).unwrap();
    assert!((stroke.area - 210.0 * 10.0).abs() < 300.0, "{}", stroke.area);
    assert_eq!(c.region_count, 2);
}

#[test]
fn cmyk_and_indexed_images_are_sampled() {
    // 2x1 CMYK image: a 400 % pixel and a white pixel, drawn 200 x 100 at the origin.
    let image =
        stream("/Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceCMYK", &[255, 255, 255, 255, 0, 0, 0, 0]);
    let indexed =
        stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace [/Indexed /DeviceCMYK 1 <00000000ffffff00>]", &[1]);
    let d = doc_with(
        "[0 0 612 792]",
        "<< /XObject << /Im 5 0 R /Ix 6 0 R >> >>",
        b"q 200 0 0 100 0 0 cm /Im Do Q q 50 0 0 50 400 400 cm /Ix Do Q",
        &[image, indexed],
    );
    let c = measure(&d, &Options { limit: 250.0, ..Options::default() });
    assert_eq!(c.max, 400.0);
    assert_eq!(c.region_count, 2);
    let big = c.regions.iter().find(|r| r.max == 400.0).unwrap();
    assert!((big.area - 10_000.0).abs() < 300.0, "{}", big.area);
    assert!(big.bbox[2] <= 101.0);
}

#[test]
fn unmeasurable_content_is_reported_not_guessed() {
    let jpeg =
        stream("/Type /XObject /Subtype /Image /Width 8 /Height 8 /BitsPerComponent 8 /ColorSpace /DeviceCMYK /Filter /DCTDecode", b"not a jpeg");
    let d = doc_with(
        "[0 0 612 792]",
        "<< /XObject << /J 5 0 R >> /Pattern << /P0 6 0 R >> >>",
        b"q 100 0 0 100 0 0 cm /J Do Q /Pattern cs /P0 scn 0 0 10 10 re f /Sh sh",
        &[jpeg, b"<< /PatternType 1 >>".to_vec()],
    );
    let c = measure(&d, &Options::default());
    assert_eq!(c.max, 0.0);
    assert!(c.unmeasured.keys().any(|k| k.contains("DCT")));
    assert_eq!(c.unmeasured.get("pattern fills"), Some(&1));
    assert_eq!(c.unmeasured.get("shadings"), Some(&1));
}

#[test]
fn huge_pages_and_resolutions_are_capped() {
    let d = doc_with("[0 0 14400 14400]", "<< >>", b"1 1 1 1 k 0 0 14400 14400 re f", &[]);
    let c = measure(&d, &Options { dpi: MAX_DPI, ..Options::default() });
    assert!(c.dpi < MAX_DPI);
    let cells = (14400.0 * c.dpi / 72.0).ceil().powi(2);
    assert!(cells <= MAX_CELLS as f64, "{cells}");
    assert_eq!(c.over_percent, 100.0);
    // An absurdly thin page stays within the budget too.
    let thin = doc_with("[0 0 1e9 0.01]", "<< >>", b"", &[]);
    assert!(page_coverage(&thin, 0, &Options::default()).is_ok());
}

#[test]
fn bad_options_and_pages_are_rejected() {
    let d = doc("");
    for bad in [
        Options { limit: 0.0, ..Options::default() },
        Options { limit: f64::NAN, ..Options::default() },
        Options { dpi: 1000.0, ..Options::default() },
        Options { dpi: 1.0, ..Options::default() },
        Options { max_regions: 0, ..Options::default() },
        Options { max_regions: 51, ..Options::default() },
    ] {
        assert!(page_coverage(&d, 0, &bad).is_err(), "{bad:?}");
    }
    assert!(page_coverage(&d, 1, &Options::default()).unwrap_err().contains("does not exist"));
}

#[test]
fn hostile_content_does_not_panic() {
    let self_form = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 1e300 1e300] /Resources << /XObject << /A 5 0 R >> >>",
        b"/A Do /A Do 1 1 1 1 k 0 0 1 1 re f",
    );
    let short_lookup =
        stream("/Type /XObject /Subtype /Image /Width 4 /Height 4 /BitsPerComponent 4 /ColorSpace [/Indexed /DeviceRGB 255 <00>]", &[0x12, 0x34]);
    let huge_image = stream("/Type /XObject /Subtype /Image /Width 2000000000 /Height 2000000000 /BitsPerComponent 16 /ColorSpace /DeviceCMYK", &[]);
    let resources =
        "<< /XObject << /A 5 0 R /B 6 0 R /C 7 0 R >> /Font << /F << /Subtype /Type0 /DescendantFonts [<< /W [0 [1e308 -5] 3 2 1e400] >>] >> >> >>";
    let content = b"q q q 1e300 0 0 1e300 0 0 cm 1 1 1 1 k 0 0 1e308 -1e308 re f Q 5 5 5 5 5 k /A Do /B Do /C Do \
        BT /F 1e9 Tf 1e9 Tz (\xff\xff\xff) Tj [1e308 (x) -1e308] TJ ET 0 0 m 1e309 1e309 1e309 1e309 1e309 1e309 c S \
        /Nope cs 1 2 3 scn 0 0 1 1 re f BI /W 4 /H 4 /CS /CMYK /BPC 8 ID \x01\x02 EI ) ] >> W n f* Q Q Q Q";
    let d = doc_with("[0 0 612 792]", resources, content, &[self_form, short_lookup, huge_image]);
    let c = measure(&d, &Options::default());
    assert!(c.max.is_finite());
    assert!(c.unmeasured.contains_key("oversized images"));
}

#[test]
fn deep_q_stacks_and_operator_floods_are_truncated() {
    let mut content = "q ".repeat(MAX_STACK + 10);
    content.push_str("1 1 1 1 k 0 0 10 10 re f");
    let c = measure(&doc(&content), &Options::default());
    assert!(c.truncated);
}
