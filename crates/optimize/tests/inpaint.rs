//! Cleaning recognized text out of page images (Editable Text and Images). Synthetic fixtures,
//! checked through PdfCraft's renderer at one image pixel per device pixel.

use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, Stream, write_full};
use pdfcraft_optimize::inpaint_page_image_regions;

const W: usize = 120;
const H: usize = 60;

/// Ink: a block of strokes at x 20..60, y 10..30 (image pixels, y down) and a "picture" at
/// x 80..110, y 35..55.
fn ink(x: usize, y: usize) -> bool {
    ((20..60).contains(&x) && (10..30).contains(&y) && x % 6 < 3) || ((80..110).contains(&x) && (35..55).contains(&y))
}

fn image(doc: &mut Document, mono: Option<bool>) -> ObjRef {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Image"));
    d.set(b"Width".to_vec(), Object::Int(W as i64));
    d.set(b"Height".to_vec(), Object::Int(H as i64));
    d.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
    let s = match mono {
        None => {
            d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
            // Off-white paper with a little texture.
            let px: Vec<u8> = (0..H).flat_map(|y| (0..W).map(move |x| if ink(x, y) { 20 } else { 228 + ((x + y) % 3) as u8 })).collect();
            Stream::flate(d, &px)
        }
        Some(ccitt) => {
            d.set(b"BitsPerComponent".to_vec(), Object::Int(1));
            let stride = W.div_ceil(8);
            let mut bits = vec![0u8; stride * H];
            for y in 0..H {
                for x in 0..W {
                    if !ink(x, y) {
                        bits[y * stride + x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            if ccitt {
                let data = pdfcraft_filters::encode_ccitt_g4(&bits, W, H, false, 1 << 20).unwrap();
                let mut p = Dict::new();
                p.set(b"K".to_vec(), Object::Int(-1));
                p.set(b"Columns".to_vec(), Object::Int(W as i64));
                p.set(b"Rows".to_vec(), Object::Int(H as i64));
                d.set(b"Filter".to_vec(), Object::name("CCITTFaxDecode"));
                d.set(b"DecodeParms".to_vec(), Object::Dict(p));
                Stream::from_raw(d, data)
            } else {
                Stream::flate(d, &bits)
            }
        }
    };
    doc.add(Object::Stream(s))
}

/// A W × H pt page drawing `image` over all of it with `content` (default: one pt per pixel).
fn document(mono: Option<bool>, content: &str) -> (Document, ObjRef) {
    let mut doc = Document::new_empty();
    let img = image(&mut doc, mono);
    let pages = doc.root().and_then(|r| doc.get(r).as_dict().and_then(|d| d.reference(b"Pages"))).unwrap();
    let mut x = Dict::new();
    x.set(b"Im0".to_vec(), Object::Ref(img));
    let mut res = Dict::new();
    res.set(b"XObject".to_vec(), Object::Dict(x));
    let c = doc.add(Object::Stream(Stream::from_raw(Dict::new(), content.as_bytes().to_vec())));
    let mut p = Dict::new();
    p.set(b"Type".to_vec(), Object::name("Page"));
    p.set(b"Parent".to_vec(), Object::Ref(pages));
    p.set(b"MediaBox".to_vec(), Object::Array(vec![0.into(), 0.into(), Object::Int(W as i64), Object::Int(H as i64)]));
    p.set(b"Resources".to_vec(), Object::Dict(res));
    p.set(b"Contents".to_vec(), Object::Ref(c));
    let r = doc.add(Object::Dict(p));
    doc.update_dict(pages, |d| {
        d.set(b"Count".to_vec(), Object::Int(1));
        d.set(b"Kids".to_vec(), Object::Array(vec![Object::Ref(r)]));
    })
    .unwrap();
    (doc, img)
}

const FULL: &str = "q 120 0 0 60 0 0 cm /Im0 Do Q";

/// Luma of each pixel at 72 dpi.
fn render(doc: &Document) -> Vec<u8> {
    let bytes = write_full(doc, &SaveOptions { object_streams: false, ..SaveOptions::default() }).unwrap();
    let out = pdfcraft_render::PageRenderer::new(Arc::new(bytes), pdfcraft_render::RenderConfig::default())
        .render(pdfcraft_render::RenderRequest { scale: 1.0, ..Default::default() });
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!((out.width as usize, out.height as usize), (W, H));
    out.rgba.chunks(4).map(|p| p[1]).collect()
}

/// The text block as a user-space quad (y up), padded by a pixel.
fn text_region() -> [[f64; 2]; 4] {
    let (l, r, top, bottom) = (19.0, 61.0, H as f64 - 9.0, H as f64 - 31.0);
    [[l, bottom], [r, bottom], [r, top], [l, top]]
}

fn check_cleaned(before: &[u8], after: &[u8], paper: impl Fn(u8) -> bool) {
    for y in 0..H {
        for x in 0..W {
            let (b, a) = (before[y * W + x], after[y * W + x]);
            if (20..60).contains(&x) && (10..30).contains(&y) {
                assert!(paper(a), "text pixel ({x}, {y}) is paper: {a}");
            } else if !(18..62).contains(&x) || !(8..32).contains(&y) {
                assert_eq!(a, b, "pixel ({x}, {y}) outside the region is kept");
            }
        }
    }
}

#[test]
fn gray_text_areas_are_filled_with_the_local_paper_and_the_rest_is_kept() {
    let (mut doc, img) = document(None, FULL);
    let before = render(&doc);
    let outside = [[1000.0, 1000.0], [1010.0, 1000.0], [1010.0, 1010.0], [1000.0, 1010.0]];
    let report = inpaint_page_image_regions(&mut doc, 0, &[text_region(), outside]).unwrap();
    assert_eq!((report.images, report.images_changed, report.images_skipped), (1, 1, 0), "{report:?}");
    assert_eq!(report.covered, [true, false]);
    assert!(report.pixels_changed > 300);
    // The paper is about 228–230: the fill is the ring's lighter-half median, not white.
    check_cleaned(&before, &render(&doc), |v| (226..=232).contains(&v));
    let Object::Stream(s) = &*doc.get(img) else { panic!() };
    assert_eq!(s.dict.name(b"Filter"), Some(&b"FlateDecode"[..]));
}

#[test]
fn one_bit_images_keep_their_encoding_and_get_paper_bits() {
    for ccitt in [false, true] {
        let (mut doc, img) = document(Some(ccitt), FULL);
        let before = render(&doc);
        let report = inpaint_page_image_regions(&mut doc, 0, &[text_region()]).unwrap();
        assert_eq!((report.images_changed, report.covered.clone()), (1, vec![true]), "{report:?}");
        check_cleaned(&before, &render(&doc), |v| v == 255);
        let Object::Stream(s) = &*doc.get(img) else { panic!() };
        assert_eq!(s.dict.name(b"Filter"), Some(if ccitt { &b"CCITTFaxDecode"[..] } else { &b"FlateDecode"[..] }));
    }
}

#[test]
fn regions_follow_the_image_placement() {
    // The image drawn at half size in the page's upper right: the region maps into it.
    let (mut doc, _) = document(None, "q 60 0 0 30 60 30 cm /Im0 Do Q");
    let before = render(&doc);
    let quad = [[69.5, 44.5], [90.5, 44.5], [90.5, 55.5], [69.5, 55.5]];
    let report = inpaint_page_image_regions(&mut doc, 0, &[quad]).unwrap();
    assert_eq!(report.covered, [true]);
    let after = render(&doc);
    assert!((5..15).any(|y| (70..90).any(|x| before[y * W + x] < 128)), "the fixture has ink there");
    for y in 5..15 {
        for x in 71..89 {
            assert!(after[y * W + x] > 200, "({x}, {y}) cleaned: {}", after[y * W + x]);
        }
    }
    // The picture, drawn at (100..115, 32..42) in device pixels, is kept.
    assert_eq!(after[37 * W + 105], before[37 * W + 105]);
}

#[test]
fn images_it_cannot_rewrite_are_skipped_and_regions_left_uncovered() {
    let (mut doc, img) = document(None, FULL);
    // Claim 16 bits per sample: not something the cleaner rewrites.
    let Object::Stream(mut s) = (*doc.get(img)).clone() else { panic!() };
    s.dict.set(b"BitsPerComponent".to_vec(), Object::Int(16));
    doc.set(img, Object::Stream(s));
    let report = inpaint_page_image_regions(&mut doc, 0, &[text_region()]).unwrap();
    assert_eq!((report.images_skipped, report.covered.clone()), (1, vec![false]), "{report:?}");
}

#[test]
fn hostile_regions_and_placements_are_refused_or_harmless() {
    let (mut doc, _) = document(None, FULL);
    assert!(inpaint_page_image_regions(&mut doc, 1, &[text_region()]).is_err());
    assert!(inpaint_page_image_regions(&mut doc, 0, &[[[f64::NAN, 0.0]; 4]]).is_err());
    assert!(inpaint_page_image_regions(&mut doc, 0, &[[[1e300, 0.0]; 4]]).is_err());
    assert!(inpaint_page_image_regions(&mut doc, 0, &vec![text_region(); 200_001]).is_err());
    // Degenerate, huge and inside-out quads, many of them: bounded work, no panic.
    let huge = [[-1e6, -1e6], [1e6, -1e6], [1e6, 1e6], [-1e6, 1e6]];
    let report = inpaint_page_image_regions(&mut doc, 0, &[[[5.0, 5.0]; 4], huge, [[60.0, 50.0], [20.0, 50.0], [60.0, 30.0], [20.0, 30.0]]]).unwrap();
    assert_eq!(report.covered.len(), 3);
    let _ = inpaint_page_image_regions(&mut doc, 0, &vec![huge; 2000]).unwrap();
    // A singular placement draws nothing and is ignored.
    let (mut doc, _) = document(None, "q 0 0 0 0 0 0 cm /Im0 Do Q q 1 0 0 1 0 0 cm /Missing Do Q");
    let report = inpaint_page_image_regions(&mut doc, 0, &[text_region()]).unwrap();
    assert_eq!((report.images_changed, report.covered.clone()), (0, vec![false]));
}
