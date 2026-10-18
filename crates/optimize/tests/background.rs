//! Removing the paper background of the images pages draw: 8-bit gray (Flate), RGB (JPEG),
//! one-bit and undecodable (JPEG 2000) images. All fixtures are synthetic.

use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};
use pdfcraft_optimize::background::{Options, Strength};
use pdfcraft_optimize::{BackgroundReport, remove_page_image_backgrounds};

const W: usize = 300;
const H: usize = 150;

/// Text stems on grey paper (with a little dither).
fn gray_scan() -> Vec<u8> {
    (0..W * H)
        .map(|i| {
            let (x, y) = (i % W, i / W);
            if (0..6).any(|k| (40 + k * 20..44 + k * 20).contains(&x)) && (50..100).contains(&y) { 15 } else { (196 + (x * 3 + y * 5) % 9) as u8 }
        })
        .collect()
}

fn image_dict(bpc: i64, cs: &str) -> Dict {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Image"));
    d.set(b"Width".to_vec(), Object::Int(W as i64));
    d.set(b"Height".to_vec(), Object::Int(H as i64));
    d.set(b"BitsPerComponent".to_vec(), Object::Int(bpc));
    d.set(b"ColorSpace".to_vec(), Object::name(cs));
    d
}

/// A 72 × 36 pt page drawing `image` over all of it (300 ppi).
fn page(doc: &mut Document, image: ObjRef) {
    let pages = doc.root().and_then(|r| doc.get(r).as_dict().and_then(|d| d.reference(b"Pages"))).unwrap();
    let mut x = Dict::new();
    x.set(b"Im0".to_vec(), Object::Ref(image));
    let mut res = Dict::new();
    res.set(b"XObject".to_vec(), Object::Dict(x));
    let c = doc.add(Object::Stream(Stream::from_raw(Dict::new(), b"q 72 0 0 36 0 0 cm /Im0 Do Q".to_vec())));
    let mut p = Dict::new();
    p.set(b"Type".to_vec(), Object::name("Page"));
    p.set(b"Parent".to_vec(), Object::Ref(pages));
    p.set(b"MediaBox".to_vec(), Object::Array(vec![0.into(), 0.into(), 72.into(), 36.into()]));
    p.set(b"Resources".to_vec(), Object::Dict(res));
    p.set(b"Contents".to_vec(), Object::Ref(c));
    let r = doc.add(Object::Dict(p));
    doc.update_dict(pages, |d| {
        let mut kids = d.get(b"Kids").and_then(|k| k.as_array().cloned()).unwrap_or_default();
        kids.push(Object::Ref(r));
        d.set(b"Count".to_vec(), Object::Int(kids.len() as i64));
        d.set(b"Kids".to_vec(), Object::Array(kids));
    })
    .unwrap();
}

struct Fixture {
    doc: Document,
    gray: ObjRef,
    jpeg: ObjRef,
    mono: ObjRef,
    jpx: ObjRef,
}

/// Page 1: gray Flate scan; page 2: the same as an RGB JPEG on yellowed paper; page 3: a
/// one-bit image; page 4: a JPEG 2000 image (not decodable here).
fn fixture() -> Fixture {
    let mut doc = Document::new_empty();
    let gray = doc.add(Object::Stream(Stream::flate(image_dict(8, "DeviceGray"), &gray_scan())));
    page(&mut doc, gray);
    let rgb: Vec<u8> = gray_scan().iter().flat_map(|v| if *v < 100 { [20, 20, 20] } else { [*v, v - 6, v - 40] }).collect();
    let mut jpg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 95).encode(&rgb, W as u32, H as u32, image::ExtendedColorType::Rgb8).unwrap();
    let mut d = image_dict(8, "DeviceRGB");
    d.set(b"Filter".to_vec(), Object::name("DCTDecode"));
    let jpeg = doc.add(Object::Stream(Stream::from_raw(d, jpg)));
    page(&mut doc, jpeg);
    let mono = doc.add(Object::Stream(Stream::flate(image_dict(1, "DeviceGray"), &vec![0xF0; W.div_ceil(8) * H])));
    page(&mut doc, mono);
    let mut d = image_dict(8, "DeviceGray");
    d.set(b"Filter".to_vec(), Object::name("JPXDecode"));
    let jpx = doc.add(Object::Stream(Stream::from_raw(d, vec![0; 64])));
    page(&mut doc, jpx);
    Fixture { doc, gray, jpeg, mono, jpx }
}

fn stream(doc: &Document, r: ObjRef) -> Stream {
    match &*doc.get(r) {
        Object::Stream(s) => s.clone(),
        _ => panic!("not a stream"),
    }
}

fn medium() -> Options {
    Options { strength: Strength::Medium, ..Default::default() }
}

#[test]
fn scans_get_white_paper_and_keep_their_text() {
    let Fixture { mut doc, gray, jpeg, mono, jpx } = fixture();
    let (mono_before, jpx_before) = (stream(&doc, mono), stream(&doc, jpx));
    let report = remove_page_image_backgrounds(&mut doc, &[0, 1, 2, 3], medium()).unwrap();
    assert_eq!((report.images, report.images_changed, report.images_skipped, report.images_unchanged), (4, 2, 1, 1), "{report:?}");
    assert!(report.pixels_whitened > (W * H) as u64, "{report:?}");
    let px = stream(&doc, gray).decoded().unwrap();
    assert_eq!(px.len(), W * H);
    assert_eq!(px[10 * W + 10], 255, "paper still grey");
    assert_eq!(px[70 * W + 41], 15, "text changed");
    let s = stream(&doc, jpeg);
    assert_eq!(s.dict.name(b"Filter"), Some(&b"DCTDecode"[..]), "JPEG stays JPEG");
    let rgb = image::load_from_memory_with_format(&s.raw, image::ImageFormat::Jpeg).unwrap().into_rgb8();
    let paper = rgb.get_pixel(10, 10).0;
    assert!(paper.iter().all(|v| *v >= 250), "yellowed paper not white: {paper:?}");
    let ink = rgb.get_pixel(41, 70).0;
    assert!(ink.iter().all(|v| *v < 60), "text washed out: {ink:?}");
    assert_eq!(stream(&doc, mono), mono_before);
    assert_eq!(stream(&doc, jpx), jpx_before);
    // A second pass finds white paper.
    assert_eq!(remove_page_image_backgrounds(&mut doc, &[0], medium()).unwrap().images_changed, 0);
}

#[test]
fn off_changes_nothing_and_bad_arguments_are_refused() {
    let Fixture { mut doc, gray, .. } = fixture();
    let before = stream(&doc, gray);
    assert_eq!(remove_page_image_backgrounds(&mut doc, &[0], Options::default()).unwrap(), BackgroundReport::default());
    assert!(remove_page_image_backgrounds(&mut doc, &[7], medium()).is_err());
    assert!(remove_page_image_backgrounds(&mut doc, &[0], Options { white_point: Some(20), ..Default::default() }).is_err());
    assert_eq!(stream(&doc, gray), before);
}

#[test]
fn truncated_samples_are_left_alone() {
    let mut doc = Document::new_empty();
    let mut short = gray_scan();
    short.truncate(W * H / 2);
    let r = doc.add(Object::Stream(Stream::flate(image_dict(8, "DeviceGray"), &short)));
    page(&mut doc, r);
    let before = stream(&doc, r);
    let report = remove_page_image_backgrounds(&mut doc, &[0], medium()).unwrap();
    assert_eq!((report.images, report.images_skipped), (1, 1));
    assert_eq!(stream(&doc, r), before);
}
