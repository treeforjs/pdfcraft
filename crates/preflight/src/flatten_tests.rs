use std::sync::Arc;

use pdfcraft_cos::{Document, SaveOptions};

use super::*;
use crate::{XOptions, XPart, verify_pdfx};

/// A PDF from object bodies (object n = bodies[n - 1]).
fn pdf(bodies: &[String]) -> Document {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, b) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{b}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

fn stream(dict: &str, data: &str) -> String {
    format!("<< {dict} /Length {} >>\nstream\n{data}\nendstream", data.len())
}

/// One 300 × 300 page with `content` and `resources`, plus `extra` objects from number 5 on.
fn one_page(content: &str, resources: &str, page_extra: &str, extra: &[String]) -> Document {
    let mut b = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R /Resources {resources} {page_extra} >>"),
        stream("", content),
    ];
    b.extend_from_slice(extra);
    pdf(&b)
}

const FONT: &str = "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>";

/// Text, an opaque blue square far from it, and a half-transparent red fill over the text.
fn alpha_over_text() -> Document {
    one_page(
        "BT /F1 24 Tf 20 200 Td (Hello flattening) Tj ET 0 0 1 rg 200 20 60 60 re f /GS0 gs 1 0 0 rg 10 190 200 40 re f",
        &format!("<< {FONT} /ExtGState << /GS0 << /Type /ExtGState /ca 0.5 /CA 0.5 >> >> >>"),
        "",
        &[],
    )
}

fn saved(doc: &Document) -> Arc<Vec<u8>> {
    Arc::new(pdfcraft_cos::write_full(doc, &SaveOptions::default()).unwrap())
}

fn render(doc: &Document, page: usize) -> (u32, u32, Vec<u8>) {
    let mut r = renderer(saved(doc));
    let p = r.render(RenderRequest { page, scale: 1.0, ..Default::default() });
    assert!(p.error.is_none(), "{:?}", p.error);
    (p.width, p.height, p.rgba.to_vec())
}

/// The share of pixels that differ by more than `tol` in some channel.
fn differing(a: &(u32, u32, Vec<u8>), b: &(u32, u32, Vec<u8>), tol: u8) -> f64 {
    assert_eq!((a.0, a.1), (b.0, b.1));
    let n =
        a.2.as_chunks::<4>()
            .0
            .iter()
            .zip(b.2.as_chunks::<4>().0.iter())
            .filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > tol))
            .count();
    n as f64 / (a.0 as f64 * a.1 as f64)
}

fn x1a_transparency(doc: &Document) -> usize {
    verify_pdfx(doc, &XOptions { part: Some(XPart::X1a2001), max_findings: 0 }).count("transparency")
}

fn reopened(doc: &Document) -> Document {
    Document::open(saved(doc)).unwrap()
}

#[test]
fn alpha_over_text_is_patched_and_keeps_its_look() {
    let original = alpha_over_text();
    assert_eq!(transparent_pages(&original, &[]), vec![0]);
    assert!(x1a_transparency(&original) > 0);
    let mut doc = original.clone();
    let report = flatten_transparency(&mut doc, &[], &FlattenSettings::default()).unwrap();
    assert_eq!(report.pages.len(), 1);
    let p = &report.pages[0];
    assert_eq!(p.action, PageAction::Patched, "{report:?}");
    assert_eq!(p.regions, 1);
    assert!(p.raster_fraction > 0.05 && p.raster_fraction < 0.3, "{report:?}");
    assert_eq!(p.resolution, 300.0);

    let back = reopened(&doc);
    assert!(transparent_pages(&back, &[]).is_empty());
    assert_eq!(x1a_transparency(&back), 0);
    // Same look, within the resampling of the patch's edges.
    let (before, after) = (render(&original, 0), render(&back, 0));
    assert!(differing(&before, &after, 40) < 0.01, "{}", differing(&before, &after, 40));
    // The opaque square far from the transparency stays vector: its pixels are exact.
    let at = |img: &(u32, u32, Vec<u8>), x: u32, y: u32| img.2[((y * img.0 + x) * 4) as usize..][..4].to_vec();
    assert_eq!(at(&after, 230, 250), vec![0, 0, 255, 255]);
    assert_eq!(at(&before, 230, 250), at(&after, 230, 250));
    // The text is still there (under the image).
    let content = pdfcraft_model::pages(&back)[0].dict.get(b"Contents").cloned().unwrap();
    let Object::Array(items) = &*back.resolve(&content) else { panic!("contents became {content:?}") };
    assert_eq!(items.len(), 3);
    let mut all = Vec::new();
    for i in items {
        let Object::Stream(s) = &*back.resolve(i) else { panic!() };
        all.extend(s.decoded().unwrap());
    }
    let text = String::from_utf8_lossy(&all);
    assert!(text.contains("(Hello flattening) Tj"), "{text}");
    assert!(text.contains("/PcFlat0 Do"), "{text}");
}

#[test]
fn soft_masked_images_and_blend_modes_are_flattened() {
    let image = stream(
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /ASCIIHexDecode /SMask 5 0 R",
        "FF000000FF000000FFFFFF00>",
    );
    let mask =
        stream("/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /ASCIIHexDecode", "FF80FF00>");
    let doc = one_page(
        "0 0 1 rg 0 0 300 300 re f q 100 0 0 100 50 50 cm /Im Do Q /Mul gs 1 1 0 rg 180 180 70 70 re f",
        "<< /XObject << /Im 6 0 R >> /ExtGState << /Mul << /BM /Multiply >> >> >>",
        "/Group << /S /Transparency /CS /DeviceRGB >>",
        &[mask, image],
    );
    assert!(x1a_transparency(&doc) > 0);
    let mut flat = doc.clone();
    let report = flatten_transparency(&mut flat, &[0], &FlattenSettings { resolution: 150.0, balance: 100 }).unwrap();
    assert_eq!(report.pages[0].action, PageAction::Patched, "{report:?}");
    assert_eq!(report.pages[0].regions, 2);
    let back = reopened(&flat);
    assert_eq!(x1a_transparency(&back), 0);
    assert!(!pdfcraft_model::pages(&back)[0].dict.contains(b"Group"));
    // The original image keeps its soft mask; the page uses an opaque copy.
    let Object::Stream(s) = &*doc.get(ObjRef::new(6, 0)) else { panic!() };
    assert!(s.dict.contains(b"SMask"));
    assert!(differing(&render(&doc, 0), &render(&back, 0), 40) < 0.01);
}

/// Two pages drawing one form that holds a transparency group and alpha.
fn shared_form() -> Document {
    let form = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency >> /Resources << /ExtGState << /A << /ca 0.4 >> >> >>",
        "/A gs 0 1 0 rg 0 0 100 100 re f",
    );
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R 8 0 R] /Count 3 /MediaBox [0 0 200 200] /Resources << /XObject << /Fm 6 0 R >> >> >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 5 0 R >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>".into(),
        stream("", "1 0 0 rg 0 0 120 120 re f q 1 0 0 1 50 50 cm /Fm Do Q"),
        form,
        stream("", "0 0 1 rg 80 80 120 120 re f /Fm Do"),
        "<< /Type /Page /Parent 2 0 R /Contents 9 0 R >>".into(),
        stream("", "0 0 0 rg 10 10 20 20 re f"),
    ])
}

#[test]
fn a_shared_form_is_copied_and_page_subsets_are_honoured() {
    let doc = shared_form();
    assert_eq!(transparent_pages(&doc, &[]), vec![0, 1]);
    let untouched_page = doc.get(ObjRef::new(8, 0));

    let mut first = doc.clone();
    let report = flatten_transparency(&mut first, &[0, 2], &FlattenSettings::default()).unwrap();
    assert_eq!(report.pages.iter().map(|p| p.page).collect::<Vec<_>>(), vec![0]);
    assert_eq!(report.untouched, vec![2]);
    let back = reopened(&first);
    assert_eq!(transparent_pages(&back, &[]), vec![1]);
    // The page without transparency is exactly as it was.
    assert_eq!(first.get(ObjRef::new(8, 0)), untouched_page);
    assert!(!first.is_edited(8) && !first.is_edited(9));

    let mut all = doc.clone();
    let report = flatten_transparency(&mut all, &[], &FlattenSettings::default()).unwrap();
    assert_eq!(report.pages.len(), 2);
    let back = reopened(&all);
    assert!(transparent_pages(&back, &[]).is_empty());
    assert_eq!(x1a_transparency(&back), 0);
    for page in 0..3 {
        assert!(differing(&render(&doc, page), &render(&back, page), 40) < 0.01, "page {}", page + 1);
    }
}

#[test]
fn balance_zero_rasterizes_the_whole_page() {
    let original = alpha_over_text();
    let mut doc = original.clone();
    let report = flatten_transparency(&mut doc, &[], &FlattenSettings { resolution: 72.0, balance: 0 }).unwrap();
    let p = &report.pages[0];
    assert_eq!((p.action, p.raster_fraction, p.images), (PageAction::Rasterized, 1.0, 1));
    let back = reopened(&doc);
    let page = &pdfcraft_model::pages(&back)[0];
    let res = back.dict(page.dict.get(b"Resources").unwrap()).unwrap();
    assert!(!res.contains(b"Font") && !res.contains(b"ExtGState"));
    assert!(differing(&render(&original, 0), &render(&back, 0), 48) < 0.02);
    assert_eq!(x1a_transparency(&back), 0);
}

#[test]
fn a_large_transparent_area_rasterizes_the_page_unless_balance_keeps_vectors() {
    let doc = one_page("0 0 1 rg 0 0 300 300 re f /GS0 gs 1 0 0 rg 0 0 300 280 re f", "<< /ExtGState << /GS0 << /ca 0.5 >> >> >>", "", &[]);
    let mut low = doc.clone();
    assert_eq!(
        flatten_transparency(&mut low, &[], &FlattenSettings { resolution: 72.0, balance: 50 }).unwrap().pages[0].action,
        PageAction::Rasterized
    );
    let mut high = doc.clone();
    assert_eq!(
        flatten_transparency(&mut high, &[], &FlattenSettings { resolution: 72.0, balance: 100 }).unwrap().pages[0].action,
        PageAction::Patched
    );
}

#[test]
fn group_only_and_invisible_transparency_need_no_image() {
    let doc = one_page("0 0 1 rg 0 0 50 50 re f", "<< >>", "/Group << /S /Transparency >>", &[]);
    let mut flat = doc.clone();
    let report = flatten_transparency(&mut flat, &[], &FlattenSettings::default()).unwrap();
    assert_eq!(report.pages[0].action, PageAction::GroupRemoved);
    assert_eq!(x1a_transparency(&flat), 0);
    // Transparency that changes nothing (white over the white page): made opaque, nothing to
    // rasterize.
    let doc = one_page("0 0 1 rg 0 0 50 50 re f /G gs 1 g 100 100 50 50 re f", "<< /ExtGState << /G << /ca 0.2 >> >> >>", "", &[]);
    let mut flat = doc.clone();
    let report = flatten_transparency(&mut flat, &[], &FlattenSettings::default()).unwrap();
    assert_eq!(report.pages[0].action, PageAction::Cleaned);
    assert!(transparent_pages(&flat, &[]).is_empty());
}

#[test]
fn pages_without_transparency_are_left_alone() {
    // Opaque states only, and a transparent one the content never uses.
    let doc = one_page("/G gs 0 0 1 rg 0 0 50 50 re f", "<< /ExtGState << /G << /ca 1 /BM /Normal >> /Unused << /ca 0.2 >> >> >>", "", &[]);
    let mut flat = doc.clone();
    let report = flatten_transparency(&mut flat, &[], &FlattenSettings::default()).unwrap();
    assert!(!report.changed());
    assert_eq!(report.untouched, vec![0]);
    assert!(!flat.is_modified());
}

#[test]
fn unbalanced_content_is_closed_before_the_images() {
    // Extra Q, a leftover cm and an open text object.
    let doc = one_page(
        "Q Q 2 0 0 2 0 0 cm /GS0 gs 1 0 0 rg 10 10 50 50 re f BT /F1 12 Tf 0 0 Td (x) Tj",
        &format!("<< {FONT} /ExtGState << /GS0 << /ca 0.5 >> >> >>"),
        "",
        &[],
    );
    let mut flat = doc.clone();
    let report = flatten_transparency(&mut flat, &[], &FlattenSettings { resolution: 72.0, balance: 100 }).unwrap();
    assert_eq!(report.pages[0].action, PageAction::Patched);
    let back = reopened(&flat);
    assert!(differing(&render(&doc, 0), &render(&back, 0), 48) < 0.02);
}

#[test]
fn settings_and_pages_are_checked() {
    let mut doc = alpha_over_text();
    for bad in [
        FlattenSettings { resolution: 10.0, balance: 50 },
        FlattenSettings { resolution: f64::NAN, balance: 50 },
        FlattenSettings { resolution: 5000.0, balance: 50 },
        FlattenSettings { resolution: 300.0, balance: 101 },
    ] {
        assert!(flatten_transparency(&mut doc, &[], &bad).is_err());
    }
    assert!(flatten_transparency(&mut doc, &[1], &FlattenSettings::default()).unwrap_err().contains("out of range"));
    assert!(!doc.is_modified());
    assert_eq!(FlattenPreset::from_id("high").map(FlattenPreset::resolution), Some(600.0));
    assert_eq!(FlattenSettings::preset(FlattenPreset::Low).resolution, 150.0);
}

#[test]
fn hostile_documents_do_not_panic() {
    // A resource cycle (a form drawing itself through the page's resources), garbage values,
    // hostile boxes and huge pages.
    let looped = stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources 5 0 R", "/Fm Do /G gs 0 0 5 5 re f");
    for (media, content, res, extra) in [
        (
            "[0 0 1e400 nan]",
            "/G gs 0 0 10 10 re f /Fm Do",
            "5 0 R",
            vec!["<< /XObject << /Fm 6 0 R >> /ExtGState << /G << /ca 0.5 >> >> >>".to_string(), looped.clone()],
        ),
        (
            "[0 0 300 300]",
            "/G gs 0 0 10 10 re f /Fm Do",
            "5 0 R",
            vec!["<< /XObject << /Fm 6 0 R >> /ExtGState << /G << /ca (x) /BM [1 /Screen] /SMask 7 >> >> >>".to_string(), looped.clone()],
        ),
        ("[0 0 1e9 1e9]", "/G gs 0 0 10 10 re f", "<< /ExtGState << /G << /ca 0.5 >> >> >>", vec![]),
        ("[5 5 5 5]", "/G gs 0 0 10 10 re f", "<< /ExtGState << /G << /CA -3 >> >> >>", vec![]),
        (
            "7",
            "/G gs ] >> << q q",
            "<< /ExtGState 99 0 R /XObject [1 2] /Pattern << /P << /PatternType 2 /ExtGState << /BM /Darken >> >> >> >>",
            vec![],
        ),
    ] {
        let mut b = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox {media} /Contents 4 0 R /Resources {res} /Group << /S /Transparency >> >>"),
            stream("", content),
        ];
        b.extend(extra);
        let mut doc = pdf(&b);
        let _ = transparent_pages(&doc, &[]);
        if flatten_transparency(&mut doc, &[], &FlattenSettings { resolution: 72.0, balance: 100 }).is_ok() {
            assert!(transparent_pages(&doc, &[]).is_empty());
        }
    }
    // Forms nested deeper than the limit: the page is rasterized whole.
    let mut b = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /XObject << /F 5 0 R >> >> >>".to_string(),
        stream("", "/F Do"),
    ];
    for i in 0..40 {
        let n = 5 + i;
        b.push(stream(
            &format!(
                "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /F {} 0 R >> /ExtGState << /G << /ca 0.5 >> >> >>",
                n + 1
            ),
            "/G gs 0 0 1 rg 0 0 50 50 re f /F Do",
        ));
    }
    b.push(stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1]", ""));
    let mut doc = pdf(&b);
    let report = flatten_transparency(&mut doc, &[], &FlattenSettings { resolution: 72.0, balance: 100 }).unwrap();
    assert_eq!(report.pages[0].action, PageAction::Rasterized);
    assert!(transparent_pages(&doc, &[]).is_empty());
}

fn page_content(doc: &Document, page: usize) -> String {
    let p = &pdfcraft_model::pages(doc)[page];
    let mut all = Vec::new();
    match &*doc.resolve(p.dict.get(b"Contents").unwrap()) {
        Object::Stream(s) => all.extend(s.decoded().unwrap()),
        Object::Array(items) => {
            for i in items {
                let Object::Stream(s) = &*doc.resolve(i) else { panic!() };
                all.extend(s.decoded().unwrap());
            }
        }
        o => panic!("contents are {o:?}"),
    }
    String::from_utf8_lossy(&all).into_owned()
}

#[test]
fn rasterize_pages_makes_one_picture_per_page_at_the_resolution() {
    let original = alpha_over_text();
    for (dpi, side) in [(72.0, 300), (150.0, 625), (36.0, 150)] {
        let mut doc = original.clone();
        let report = rasterize_pages(&mut doc, &[], dpi).unwrap();
        let [p] = report.as_slice() else { panic!("{report:?}") };
        assert_eq!((p.page, p.width, p.height, p.images), (0, side, side, 1));
        assert!((p.resolution - dpi).abs() < 1e-9, "{report:?}");
        let back = reopened(&doc);
        let pages = pdfcraft_model::pages(&back);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].display_size(&back), (300.0, 300.0));
        let text = page_content(&back, 0);
        assert!(!text.contains("BT") && !text.contains("Tj") && !text.contains(" re"), "{text}");
        assert!(text.contains("/PcImg0 Do"), "{text}");
        // Only the picture is left: no fonts, no graphics states, no transparency.
        let res = back.resolve(pages[0].dict.get(b"Resources").unwrap()).as_dict().cloned().unwrap();
        assert!(res.len() == 1 && res.contains(b"XObject"), "{res:?}");
        let xo = back.resolve(res.get(b"XObject").unwrap()).as_dict().cloned().unwrap();
        let Object::Stream(img) = &*back.resolve(xo.get(b"PcImg0").unwrap()) else { panic!() };
        assert_eq!(img.dict.get(b"Width").and_then(Object::as_int), Some(side as i64));
        assert_eq!(img.dict.get(b"Height").and_then(Object::as_int), Some(side as i64));
        assert!(transparent_pages(&back, &[]).is_empty());
        assert_eq!(x1a_transparency(&back), 0);
        // It looks the same.
        assert!(differing(&render(&original, 0), &render(&back, 0), 60) < 0.05);
    }
}

#[test]
fn rasterize_pages_honours_pages_and_refuses_bad_resolutions() {
    let two = pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << {FONT} >> >>"),
        stream("", "BT /F1 12 Tf 10 50 Td (One) Tj ET"),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [10 20 110 220] /Rotate 90 /Contents 6 0 R /Resources << {FONT} >> >>"),
        stream("", "BT /F1 12 Tf 20 50 Td (Two) Tj ET"),
    ]);
    let mut doc = two.clone();
    let r = rasterize_pages(&mut doc, &[1], 144.0).unwrap();
    assert_eq!(r.iter().map(|p| (p.page, p.width, p.height, p.images)).collect::<Vec<_>>(), vec![(1, 200, 400, 1)]);
    let back = reopened(&doc);
    assert!(page_content(&back, 0).contains("(One) Tj"));
    let second = page_content(&back, 1);
    assert!(!second.contains("Tj"), "{second}");
    // Placed over the media box, which (like the rotation) is kept.
    assert!(second.contains("q 100 0 0 200 10 20 cm /PcImg0 Do Q"), "{second}");
    assert_eq!(pdfcraft_model::pages(&back)[1].display_size(&back), (200.0, 100.0));

    let mut doc = two.clone();
    for bad in [0.0, 10.0, -300.0, f64::NAN, f64::INFINITY, 5000.0] {
        assert!(rasterize_pages(&mut doc, &[], bad).unwrap_err().contains("resolution"), "{bad}");
    }
    assert!(rasterize_pages(&mut doc, &[2], 300.0).unwrap_err().contains("out of range"));
    assert!(!doc.is_modified());
}

#[test]
fn rasterize_pages_caps_pixels() {
    // One huge page: the resolution is lowered to the page budget.
    let mut big = one_page("0 0 1 rg 0 0 100 100 re f", "<< >>", "", &[]);
    let obj = pdfcraft_model::pages(&big)[0].obj;
    big.update_dict(obj, |d| d.set(b"MediaBox".to_vec(), rect_object([0.0, 0.0, 14_400.0, 14_400.0]))).unwrap();
    let r = rasterize_pages(&mut big.clone(), &[], 600.0).unwrap();
    assert!(r[0].resolution < 600.0 && (r[0].width * r[0].height) as f64 <= MAX_PAGE_PIXELS, "{r:?}");
    assert!(r[0].images > 1);
    // Many pages at a high resolution: refused before rendering anything.
    let kids: Vec<String> = (0..20).map(|i| format!("{} 0 R", i + 3)).collect();
    let mut bodies = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string(), format!("<< /Type /Pages /Kids [{}] /Count 20 >>", kids.join(" "))];
    bodies.extend((0..20).map(|_| "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 2000 2000] >>".to_string()));
    let mut many = pdf(&bodies);
    let e = rasterize_pages(&mut many, &[], 1200.0).unwrap_err();
    assert!(e.contains("lower the resolution"), "{e}");
    assert!(!many.is_modified());
}
