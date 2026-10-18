//! Despeckling the images pages draw: CCITT and Flate one-bit scans, 8-bit gray, and images
//! left alone. All fixtures are synthetic.

use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, Stream, write_full};
use pdfcraft_optimize::despeckle::{Level, Options};
use pdfcraft_optimize::{DespeckleReport, despeckle_page_images};

const W: usize = 600;
const H: usize = 300;

/// 600 × 300 px of "text" at 300 ppi: stems, an i with its dot, a period, and four specks of
/// 1–3 px far from the text. Returns ink pixels, the specks and points that must stay ink.
type Points = Vec<(usize, usize)>;

fn scan() -> (Vec<bool>, Points, Points) {
    let mut ink = vec![false; W * H];
    let mut rect = |x0: usize, y0: usize, w: usize, h: usize| {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                ink[y * W + x] = true;
            }
        }
    };
    for i in 0..8 {
        rect(60 + i * 14, 120, 4, 30);
    }
    rect(180, 128, 4, 22); // i
    rect(180, 120, 4, 4); // its dot
    rect(187, 146, 4, 4); // a period
    let specks = vec![(20, 20), (400, 40), (500, 250), (300, 260)];
    rect(20, 20, 1, 1);
    rect(400, 40, 2, 2);
    rect(500, 250, 3, 3);
    rect(300, 260, 2, 1);
    (ink, specks, vec![(61, 130), (181, 121), (188, 147), (181, 140)])
}

/// Packed rows with ink as sample 0 (DeviceGray, white = 1).
fn pack(ink: &[bool]) -> Vec<u8> {
    let stride = W.div_ceil(8);
    let mut out = vec![0xFFu8; stride * H];
    for (i, _) in ink.iter().enumerate().filter(|(_, b)| **b) {
        out[(i / W) * stride + (i % W) / 8] &= !(0x80 >> (i % W % 8));
    }
    out
}

fn image_dict(bpc: i64) -> Dict {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Image"));
    d.set(b"Width".to_vec(), Object::Int(W as i64));
    d.set(b"Height".to_vec(), Object::Int(H as i64));
    d.set(b"BitsPerComponent".to_vec(), Object::Int(bpc));
    d.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
    d
}

/// A 144 × 72 pt page drawing `image` over all of it (300 ppi).
fn page(doc: &mut Document, image: ObjRef) {
    let pages = doc.root().and_then(|r| doc.get(r).as_dict().and_then(|d| d.reference(b"Pages"))).unwrap();
    let mut x = Dict::new();
    x.set(b"Im0".to_vec(), Object::Ref(image));
    let mut res = Dict::new();
    res.set(b"XObject".to_vec(), Object::Dict(x));
    let c = doc.add(Object::Stream(Stream::from_raw(Dict::new(), b"q 144 0 0 72 0 0 cm /Im0 Do Q".to_vec())));
    let mut p = Dict::new();
    p.set(b"Type".to_vec(), Object::name("Page"));
    p.set(b"Parent".to_vec(), Object::Ref(pages));
    p.set(b"MediaBox".to_vec(), Object::Array(vec![0.into(), 0.into(), 144.into(), 72.into()]));
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
    ccitt: ObjRef,
    flate: ObjRef,
    gray: ObjRef,
    jbig2: ObjRef,
}

/// Page 1: the scan as CCITT G4; page 2: as Flate one-bit; page 3: as 8-bit gray Flate; page 4:
/// an image with a JBIG2 filter (not decodable here).
fn fixture() -> Fixture {
    let (ink, ..) = scan();
    let mut doc = Document::new_empty();
    let bits = pack(&ink);
    let mut d = image_dict(1);
    let mut parms = Dict::new();
    parms.set(b"K".to_vec(), Object::Int(-1));
    parms.set(b"Columns".to_vec(), Object::Int(W as i64));
    parms.set(b"Rows".to_vec(), Object::Int(H as i64));
    d.set(b"Filter".to_vec(), Object::name("CCITTFaxDecode"));
    d.set(b"DecodeParms".to_vec(), Object::Dict(parms));
    let ccitt = doc.add(Object::Stream(Stream::from_raw(d, pdfcraft_filters::encode_ccitt_g4(&bits, W, H, false, 1 << 20).unwrap())));
    page(&mut doc, ccitt);
    let flate = doc.add(Object::Stream(Stream::flate(image_dict(1), &bits)));
    page(&mut doc, flate);
    let gray: Vec<u8> = ink.iter().map(|b| if *b { 10 } else { 245 }).collect();
    let gray = doc.add(Object::Stream(Stream::flate(image_dict(8), &gray)));
    page(&mut doc, gray);
    let mut d = image_dict(1);
    d.set(b"Filter".to_vec(), Object::name("JBIG2Decode"));
    let jbig2 = doc.add(Object::Stream(Stream::from_raw(d, vec![0; 64])));
    page(&mut doc, jbig2);
    Fixture { doc, ccitt, flate, gray, jbig2 }
}

fn stream(doc: &Document, r: ObjRef) -> Stream {
    match &*doc.get(r) {
        Object::Stream(s) => s.clone(),
        _ => panic!("not a stream"),
    }
}

/// Ink pixels of a one-bit image (ink = 0), decoding CCITT with an independent decoder.
fn mono_ink(s: &Stream) -> Vec<bool> {
    let stride = W.div_ceil(8);
    let bits = if s.dict.name(b"Filter") == Some(b"CCITTFaxDecode") {
        struct Sink(Vec<bool>);
        impl hayro_ccitt::Decoder for Sink {
            fn push_pixel(&mut self, white: bool) {
                self.0.push(!white);
            }
            fn push_pixel_chunk(&mut self, white: bool, chunks: u32) {
                self.0.extend(std::iter::repeat_n(!white, chunks as usize * 8));
            }
            fn next_line(&mut self) {}
        }
        let settings = hayro_ccitt::DecodeSettings {
            columns: W as u32,
            rows: H as u32,
            end_of_block: true,
            end_of_line: false,
            rows_are_byte_aligned: false,
            encoding: hayro_ccitt::EncodingMode::Group4,
            invert_black: false,
        };
        let mut sink = Sink(Vec::new());
        hayro_ccitt::decode(&s.raw, &mut sink, &mut hayro_ccitt::DecoderContext::new(settings)).unwrap();
        return sink.0;
    } else {
        s.decoded().unwrap()
    };
    (0..W * H).map(|i| bits[(i / W) * stride + (i % W) / 8] & (0x80 >> (i % W % 8)) == 0).collect()
}

#[test]
fn scans_lose_their_specks_and_keep_text_and_punctuation() {
    let Fixture { mut doc, ccitt, flate, gray, jbig2 } = fixture();
    let jbig2_before = stream(&doc, jbig2);
    let report = despeckle_page_images(&mut doc, &[0, 1, 2, 3], Options { level: Level::High, ..Default::default() }).unwrap();
    assert_eq!((report.images, report.images_changed, report.images_skipped), (4, 3, 1), "{report:?}");
    assert_eq!(report.stats.dark_specks, 12, "{report:?}");
    let (_, specks, kept) = scan();
    let new = stream(&doc, ccitt);
    assert_eq!(new.dict.name(b"Filter"), Some(&b"CCITTFaxDecode"[..]), "CCITT stays CCITT");
    for s in [new, stream(&doc, flate)] {
        let ink = mono_ink(&s);
        assert!(specks.iter().all(|(x, y)| !ink[y * W + x]), "speck kept");
        assert!(kept.iter().all(|(x, y)| ink[y * W + x]), "text or punctuation removed");
        assert_eq!(ink.iter().filter(|b| **b).count(), scan().0.iter().filter(|b| **b).count() - 1 - 4 - 9 - 2);
    }
    let g = stream(&doc, gray).decoded().unwrap();
    assert!(specks.iter().all(|(x, y)| g[y * W + x] == 245));
    assert!(kept.iter().all(|(x, y)| g[y * W + x] == 10));
    assert_eq!(stream(&doc, jbig2), jbig2_before, "undecodable images are left alone");
    // The rewritten document saves and renders.
    let bytes = write_full(&doc, &SaveOptions::default()).unwrap();
    let mut r = pdfcraft_render::PageRenderer::new(Arc::new(bytes), pdfcraft_render::RenderConfig::default());
    for page in 0..3 {
        let out = r.render(pdfcraft_render::RenderRequest { page, scale: 1.0, ..Default::default() });
        assert!(out.error.is_none(), "{:?}", out.error);
    }
}

#[test]
fn off_changes_nothing_and_bad_pages_are_refused() {
    let Fixture { mut doc, ccitt, .. } = fixture();
    let before = stream(&doc, ccitt);
    assert_eq!(despeckle_page_images(&mut doc, &[0], Options::default()).unwrap(), DespeckleReport::default());
    assert_eq!(stream(&doc, ccitt), before);
    assert!(despeckle_page_images(&mut doc, &[9], Options { level: Level::Low, ..Default::default() }).is_err());
    // Low keeps the 3 × 3 speck (larger than 2 px at 300 ppi) and removes the rest.
    let report = despeckle_page_images(&mut doc, &[0], Options { level: Level::Low, ..Default::default() }).unwrap();
    assert_eq!(report.stats.dark_specks, 3, "{report:?}");
}

#[test]
fn truncated_ccitt_data_is_left_alone() {
    let Fixture { mut doc, ccitt, .. } = fixture();
    let full = stream(&doc, ccitt);
    let s = Stream::from_raw(full.dict.clone(), full.raw[..full.raw.len() / 3].to_vec());
    doc.set(ccitt, Object::Stream(s.clone()));
    let report = despeckle_page_images(&mut doc, &[0], Options { level: Level::High, ..Default::default() }).unwrap();
    assert_eq!((report.images_changed, report.images_skipped), (0, 1), "{report:?}");
    assert_eq!(stream(&doc, ccitt), s);
}
