//! The Optimizer's lossless re-encodings: one-bit images as CCITT Group 4 and LZW streams as
//! Flate, checked against PdfCraft's renderer and, when installed, Poppler (`pdftoppm`,
//! `pdfimages`) as an external oracle. All fixtures are synthetic.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, Stream, write_full};
use pdfcraft_filters::{Filter, Params};
use pdfcraft_optimize::{MonoCompression, Settings, optimize};

/// Scan-like marks on white: per 23 x 31 cell a pseudo-random glyph built from 2-pixel
/// strokes (stems, bars, a diagonal), a rule across the middle, scattered specks.
fn scan_bitmap(w: usize, h: usize) -> Vec<bool> {
    let hash = |a: usize, b: usize| {
        let mut x = (a as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (b as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        x ^= x >> 29;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^ (x >> 32)
    };
    let mut px = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let (cx, cy, g) = (x % 23, y % 31, hash(x / 23, y / 31));
            let inside = (3..17).contains(&cx) && (5..24).contains(&cy);
            let stroke = (g & 1 != 0 && cx < 5)
                || (g & 2 != 0 && cx >= 15)
                || (g & 4 != 0 && cy < 7)
                || (g & 8 != 0 && (14..16).contains(&cy))
                || (g & 16 != 0 && cy >= 22)
                || (g & 32 != 0 && (cx + 31 - cy) % 19 < 2);
            let blank = (g >> 8) % 7 == 0; // word spaces
            px[y * w + x] = (inside && stroke && !blank) || y == h / 2 || hash(x, y) % 1500 == 0;
        }
    }
    px
}

/// Packed rows, black = sample 0 (DeviceGray), padding bits set to 1 to catch misuse.
fn pack(px: &[bool], w: usize, black_bit: bool) -> Vec<u8> {
    let stride = w.div_ceil(8);
    let h = px.len() / w;
    let mut out = vec![0u8; stride * h];
    for y in 0..h {
        for x in 0..stride * 8 {
            let bit = px.get(y * w + x).filter(|_| x < w).map_or(true, |&b| b == black_bit);
            if bit {
                out[y * stride + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    out
}

fn mono_image(doc: &mut Document, w: usize, h: usize, px: &[bool], mask: bool, encode: Option<(&str, Params)>) -> ObjRef {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Image"));
    d.set(b"Width".to_vec(), Object::Int(w as i64));
    d.set(b"Height".to_vec(), Object::Int(h as i64));
    d.set(b"BitsPerComponent".to_vec(), Object::Int(1));
    let samples = if mask {
        // Ink = sample 1 through /Decode [1 0].
        d.set(b"ImageMask".to_vec(), Object::Bool(true));
        d.set(b"Decode".to_vec(), Object::Array(vec![1.into(), 0.into()]));
        pack(px, w, true)
    } else {
        d.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
        pack(px, w, false)
    };
    let s = match encode {
        None => Stream::from_raw(d, samples),
        Some((name, p)) => {
            let filter = Filter::from_name(name.as_bytes());
            let mut parms = Dict::new();
            if p.predictor != 1 {
                parms.set(b"Predictor".to_vec(), Object::Int(p.predictor));
                parms.set(b"Columns".to_vec(), Object::Int(p.columns));
                parms.set(b"BitsPerComponent".to_vec(), Object::Int(p.bits_per_component));
            }
            if !parms.is_empty() {
                d.set(b"DecodeParms".to_vec(), Object::Dict(parms));
            }
            d.set(b"Filter".to_vec(), Object::name(name));
            Stream::from_raw(d, pdfcraft_filters::encode(&filter, &p, &samples).unwrap())
        }
    };
    doc.add(Object::Stream(s))
}

/// A page `w_pt` × `h_pt` drawing `content` with XObject /Im0 = `image`.
fn page(doc: &mut Document, image: ObjRef, content: Stream, size: [f64; 2]) -> ObjRef {
    let pages = doc.root().and_then(|r| doc.get(r).as_dict().and_then(|d| d.reference(b"Pages"))).unwrap();
    let mut x = Dict::new();
    x.set(b"Im0".to_vec(), Object::Ref(image));
    let mut res = Dict::new();
    res.set(b"XObject".to_vec(), Object::Dict(x));
    let c = doc.add(Object::Stream(content));
    let mut p = Dict::new();
    p.set(b"Type".to_vec(), Object::name("Page"));
    p.set(b"Parent".to_vec(), Object::Ref(pages));
    p.set(b"MediaBox".to_vec(), Object::Array(vec![0.into(), 0.into(), Object::Real(size[0]), Object::Real(size[1])]));
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
    r
}

/// The image covering the whole page at one point per pixel (72 ppi), so a 72 dpi rendering
/// maps image pixels to device pixels exactly, without resampling.
fn full_page(doc: &mut Document, image: ObjRef, w: usize, h: usize, fill: &str) -> ObjRef {
    let (pw, ph) = (w as f64, h as f64);
    let content = format!("q {fill} {pw} 0 0 {ph} 0 0 cm /Im0 Do Q");
    page(doc, image, Stream::from_raw(Dict::new(), content.into_bytes()), [pw, ph])
}

fn stream(doc: &Document, r: ObjRef) -> Stream {
    match &*doc.get(r) {
        Object::Stream(s) => s.clone(),
        _ => panic!("not a stream"),
    }
}

fn save(doc: &Document) -> Vec<u8> {
    write_full(doc, &SaveOptions { object_streams: false, ..SaveOptions::default() }).unwrap()
}

/// Every page rendered by PdfCraft's renderer at 72 dpi, as RGBA.
fn render_all(bytes: &[u8], pages: usize) -> Vec<Vec<u8>> {
    let mut r = pdfcraft_render::PageRenderer::new(Arc::new(bytes.to_vec()), pdfcraft_render::RenderConfig::default());
    (0..pages)
        .map(|page| {
            let out = r.render(pdfcraft_render::RenderRequest { page, scale: 1.0, ..Default::default() });
            assert!(out.error.is_none(), "{:?}", out.error);
            out.rgba.to_vec()
        })
        .collect()
}

struct Fixture {
    doc: Document,
    scan: ObjRef,
    mask: ObjRef,
    scan_px: Vec<bool>,
    mask_px: Vec<bool>,
}

const SCAN: (usize, usize) = (1700, 220);
const MASK: (usize, usize) = (333, 77);

/// Page 1: a 1700 × 220 grey scan stored as plain Flate. Page 2: a 333 × 77 red image mask
/// (odd width, inverted /Decode) stored as LZW with a PNG predictor.
fn fixture() -> Fixture {
    let mut doc = Document::new_empty();
    let scan_px = scan_bitmap(SCAN.0, SCAN.1);
    let scan = mono_image(&mut doc, SCAN.0, SCAN.1, &scan_px, false, Some(("FlateDecode", Params::default())));
    full_page(&mut doc, scan, SCAN.0, SCAN.1, "");
    let mask_px: Vec<bool> = scan_bitmap(MASK.0, MASK.1).iter().enumerate().map(|(i, b)| *b ^ (i % MASK.0 > 300)).collect();
    let p = Params { predictor: 15, columns: MASK.0 as i64, bits_per_component: 1, ..Params::default() };
    let mask = mono_image(&mut doc, MASK.0, MASK.1, &mask_px, true, Some(("LZWDecode", p)));
    full_page(&mut doc, mask, MASK.0, MASK.1, "1 0 0 rg");
    Fixture { doc, scan, mask, scan_px, mask_px }
}

#[test]
fn one_bit_images_become_ccitt_g4_with_the_same_samples() {
    let f = fixture();
    let before = save(&f.doc);
    let sizes = (stream(&f.doc, f.scan).raw.len(), stream(&f.doc, f.mask).raw.len());
    let mut doc = f.doc.clone();
    let report = optimize(&mut doc, &Settings::default()).unwrap();
    assert_eq!((report.images, report.images_ccitt, report.images_recompressed, report.images_resampled), (2, 2, 2, 0), "{report:?}");
    for (r, (w, h), old) in [(f.scan, SCAN, sizes.0), (f.mask, MASK, sizes.1)] {
        let s = stream(&doc, r);
        assert_eq!(s.dict.name(b"Filter"), Some(&b"CCITTFaxDecode"[..]));
        let parms = s.dict.get(b"DecodeParms").and_then(Object::as_dict).unwrap();
        assert_eq!((parms.int(b"K"), parms.int(b"Columns"), parms.int(b"Rows")), (Some(-1), Some(w as i64), Some(h as i64)));
        assert_eq!((s.dict.int(b"Width"), s.dict.int(b"Height")), (Some(w as i64), Some(h as i64)));
        assert!(s.raw.len() < old, "{} vs {old}", s.raw.len());
    }
    let m = stream(&doc, f.mask);
    assert_eq!(m.dict.get(b"ImageMask"), Some(&Object::Bool(true)));
    assert_eq!(m.dict.get(b"Decode"), Some(&Object::Array(vec![1.into(), 0.into()])), "the decode array is kept");
    assert!(report.image_bytes_after < report.image_bytes_before);
    // PdfCraft's renderer draws both pages identically before and after.
    let after = save(&doc);
    assert_eq!(render_all(&before, 2), render_all(&after, 2));
}

/// Poppler's PGM/PPM output for every page at 72 dpi, or `None` if it isn't installed.
fn pdftoppm(dir: &Path, name: &str, bytes: &[u8], gray: bool) -> Option<Vec<Vec<u8>>> {
    let pdf = dir.join(format!("{name}.pdf"));
    std::fs::write(&pdf, bytes).unwrap();
    let prefix = dir.join(name);
    let mut cmd = Command::new("pdftoppm");
    cmd.args(["-r", "72"]);
    if gray {
        cmd.arg("-gray");
    }
    let status = cmd.arg(&pdf).arg(&prefix).status().ok()?;
    assert!(status.success(), "pdftoppm failed on {}", pdf.display());
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(&format!("{name}-")))
        .collect();
    files.sort();
    Some(files.iter().map(|p| std::fs::read(p).unwrap()).collect())
}

/// The images Poppler's `pdfimages` decodes from page 1 onwards, as PBM files (P4, 1 = black).
fn pdfimages(dir: &Path, name: &str) -> Vec<Vec<u8>> {
    let prefix = dir.join(format!("{name}-img"));
    let status = Command::new("pdfimages").arg(dir.join(format!("{name}.pdf"))).arg(&prefix).status().unwrap();
    assert!(status.success());
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(&format!("{name}-img")))
        .collect();
    files.sort();
    files.iter().map(|p| std::fs::read(p).unwrap()).collect()
}

/// The pixels of a P4 PBM (`true` = 1 = black).
fn pbm_pixels(pbm: &[u8]) -> (usize, usize, Vec<bool>) {
    let text = String::from_utf8_lossy(&pbm[..pbm.len().min(64)]).into_owned();
    let mut fields = text.split_ascii_whitespace();
    assert_eq!(fields.next(), Some("P4"));
    let (w, h): (usize, usize) = (fields.next().unwrap().parse().unwrap(), fields.next().unwrap().parse().unwrap());
    let header = format!("P4\n{w} {h}\n").len();
    let stride = w.div_ceil(8);
    let data = &pbm[header..];
    assert_eq!(data.len(), stride * h);
    (w, h, (0..w * h).map(|i| (data[(i / w) * stride + (i % w) / 8] >> (7 - (i % w) % 8)) & 1 == 1).collect())
}

fn oracle_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn poppler_decodes_and_renders_the_ccitt_copy_exactly_like_the_original() {
    let f = fixture();
    let before = save(&f.doc);
    let mut doc = f.doc.clone();
    optimize(&mut doc, &Settings::default()).unwrap();
    let after = save(&doc);
    let dir = oracle_dir("ccitt-oracle");
    let Some(old) = pdftoppm(&dir, "before", &before, true) else {
        eprintln!("skipped: pdftoppm (Poppler) is not installed");
        return;
    };
    // Rendering: every page pixel-identical before and after.
    let new = pdftoppm(&dir, "after", &after, true).unwrap();
    assert_eq!((old.len(), new.len()), (2, 2));
    assert!(old == new, "Poppler renders the optimized copy differently");
    // Decoding: Poppler's own CCITT decoder gives back exactly the source bitmaps. The scan
    // (DeviceGray, sample 0 = black) comes out as the source; the stencil mask (ink = sample 1
    // under /Decode [1 0]) as the same bitmap as Poppler extracts from the LZW original.
    let (old_images, new_images) = (pdfimages(&dir, "before"), pdfimages(&dir, "after"));
    assert_eq!((old_images.len(), new_images.len()), (2, 2));
    let (w, h, scan) = pbm_pixels(&new_images[0]);
    assert_eq!((w, h), SCAN);
    assert!(scan == f.scan_px, "the decoded scan differs from the source bitmap");
    let (w, h, mask) = pbm_pixels(&new_images[1]);
    assert_eq!((w, h), MASK);
    assert_eq!(mask, pbm_pixels(&old_images[1]).2);
    assert!(mask == f.mask_px || mask.iter().zip(&f.mask_px).all(|(a, b)| a != b), "the decoded mask differs from the source bitmap");
    // pdfimages lists both images as CCITT after, none before.
    let list = |name: &str| String::from_utf8(Command::new("pdfimages").args(["-list"]).arg(dir.join(name)).output().unwrap().stdout).unwrap();
    let (b, a) = (list("before.pdf"), list("after.pdf"));
    assert_eq!(a.lines().filter(|l| l.contains(" ccitt ")).count(), 2, "{a}");
    assert_eq!(b.lines().filter(|l| l.contains(" ccitt ")).count(), 0, "{b}");
    eprintln!("bytes before {} after {}\n{a}", before.len(), after.len());
}

#[test]
fn mono_settings_choose_flate_retain_and_downsampling() {
    let f = fixture();
    // Retain without downsampling: nothing changes.
    let mut doc = f.doc.clone();
    let mut s = Settings::default();
    s.mono.compression = MonoCompression::Retain;
    s.lzw_to_flate = false;
    let r = optimize(&mut doc, &s).unwrap();
    assert_eq!(r.images_recompressed + r.images_resampled, 0);
    assert_eq!(stream(&doc, f.mask), stream(&f.doc, f.mask));
    // Flate: the LZW mask becomes smaller Flate data with identical samples.
    s.mono.compression = MonoCompression::Flate;
    let mut doc = f.doc.clone();
    let r = optimize(&mut doc, &s).unwrap();
    assert_eq!(r.images_ccitt, 0);
    // Each image is either Flate with identical samples (when smaller) or left as it was.
    let mut flated = 0;
    for r in [f.scan, f.mask] {
        let (new, old) = (stream(&doc, r), stream(&f.doc, r));
        if new != old {
            flated += 1;
            assert_eq!(new.dict.name(b"Filter"), Some(&b"FlateDecode"[..]));
            assert!(new.raw.len() < old.raw.len());
            assert_eq!(new.decoded_strict().unwrap(), old.decoded_strict().unwrap());
        }
    }
    assert_eq!(flated, r.images_recompressed);
    // Downsampling: a 2400 × 2400 image drawn 2 inches wide (1200 ppi) goes to 300 ppi. Black
    // 8 × 8 squares on a 16-pixel grid become exact 2 × 2 squares on a 4-pixel grid.
    let mut doc = Document::new_empty();
    let n = 2400;
    let px: Vec<bool> = (0..n * n).map(|i| (i % n) % 16 < 8 && (i / n) % 16 < 8).collect();
    let img = mono_image(&mut doc, n, n, &px, false, None);
    page(&mut doc, img, Stream::from_raw(Dict::new(), b"q 144 0 0 144 0 0 cm /Im0 Do Q".to_vec()), [144.0, 144.0]);
    let mut s = Settings::default();
    s.mono.compression = MonoCompression::Flate;
    let r = optimize(&mut doc, &s).unwrap();
    assert_eq!(r.images_resampled, 1, "{r:?}");
    let out = stream(&doc, img);
    assert_eq!((out.dict.int(b"Width"), out.dict.int(b"Height")), (Some(600), Some(600)));
    let expect: Vec<bool> = (0..600 * 600).map(|i| (i % 600) % 4 < 2 && (i / 600) % 4 < 2).collect();
    assert_eq!(out.decoded_strict().unwrap(), pack(&expect, 600, false));
    // With the default CCITT compression the downsampled image is G4.
    let mut doc2 = Document::new_empty();
    let img2 = mono_image(&mut doc2, n, n, &px, false, None);
    page(&mut doc2, img2, Stream::from_raw(Dict::new(), b"q 144 0 0 144 0 0 cm /Im0 Do Q".to_vec()), [144.0, 144.0]);
    let r = optimize(&mut doc2, &Settings::default()).unwrap();
    assert_eq!((r.images_resampled, r.images_ccitt), (1, 1));
    assert_eq!(stream(&doc2, img2).dict.name(b"Filter"), Some(&b"CCITTFaxDecode"[..]));
    // Downsampling off: the samples stay at full resolution.
    let mut doc3 = Document::new_empty();
    let img3 = mono_image(&mut doc3, n, n, &px, false, None);
    page(&mut doc3, img3, Stream::from_raw(Dict::new(), b"q 144 0 0 144 0 0 cm /Im0 Do Q".to_vec()), [144.0, 144.0]);
    let mut s = Settings::default();
    s.mono.downsample = false;
    let r = optimize(&mut doc3, &s).unwrap();
    assert_eq!((r.images_resampled, r.images_ccitt), (0, 1));
    assert_eq!(stream(&doc3, img3).dict.int(b"Width"), Some(n as i64));
}

fn lzw_stream(dict: Dict, data: &[u8], filter: Object, parms: Option<Object>, p: &Params) -> Stream {
    let mut d = dict;
    d.set(b"Filter".to_vec(), filter);
    if let Some(parms) = parms {
        d.set(b"DecodeParms".to_vec(), parms);
    }
    Stream::from_raw(d, pdfcraft_filters::encode(&Filter::Lzw, p, data).unwrap())
}

#[test]
fn lzw_streams_become_flate_and_incomplete_ones_are_left_alone() {
    let mut doc = Document::new_empty();
    let content = b"BT /F1 12 Tf 72 700 Td (LZW content stream) Tj ET\n".repeat(40);
    let mut private = Dict::new();
    private.set(b"Private".to_vec(), Object::Int(7));
    // 1. A plain LZW stream with a private key, EarlyChange 0.
    let p0 = Params { early_change: 0, ..Params::default() };
    let mut parms = Dict::new();
    parms.set(b"EarlyChange".to_vec(), Object::Int(0));
    let plain = doc.add(Object::Stream(lzw_stream(private.clone(), &content, Object::name("LZWDecode"), Some(Object::Dict(parms)), &p0)));
    // 2. ASCIIHex over LZW with a predictor.
    let pp = Params { predictor: 12, columns: 10, ..Params::default() };
    let lzw = pdfcraft_filters::encode(&Filter::Lzw, &pp, &content).unwrap();
    let hex = pdfcraft_filters::encode(&Filter::AsciiHex, &Params::default(), &lzw).unwrap();
    let mut d = Dict::new();
    d.set(b"Filter".to_vec(), Object::Array(vec![Object::name("AHx"), Object::name("LZW")]));
    let mut pr = Dict::new();
    pr.set(b"Predictor".to_vec(), Object::Int(12));
    pr.set(b"Columns".to_vec(), Object::Int(10));
    d.set(b"DecodeParms".to_vec(), Object::Array(vec![Object::Null, Object::Dict(pr)]));
    let chained = doc.add(Object::Stream(Stream::from_raw(d, hex)));
    // 3. Truncated: the end-of-data code is missing (viewers read it; we must not "repair" it).
    let full = lzw_stream(Dict::new(), &content, Object::name("LZWDecode"), None, &Params::default());
    let mut cut = full.clone();
    cut.raw = full.raw[..full.raw.len() - 1].to_vec().into();
    let truncated = doc.add(Object::Stream(cut));
    // 4. Indirect filter name. 5. LZW then DCT (an image codec). 6. Corrupt codes.
    let name = doc.add(Object::name("LZWDecode"));
    let indirect = doc.add(Object::Stream(lzw_stream(Dict::new(), &content, Object::Ref(name), None, &Params::default())));
    let with_dct = doc.add(Object::Stream(lzw_stream(Dict::new(), &content, Object::Array(vec![Object::name("LZWDecode"), Object::name("DCTDecode")]), None, &Params::default())));
    let mut junk = Dict::new();
    junk.set(b"Filter".to_vec(), Object::name("LZWDecode"));
    let corrupt = doc.add(Object::Stream(Stream::from_raw(junk, vec![0x80, 0x0f, 0xff, 0xff, 0xff, 0x01])));
    // Keep every stream reachable so the saved copy writes them.
    let all = vec![plain, chained, truncated, indirect, with_dct, corrupt].into_iter().map(Object::Ref).collect();
    let root = doc.root().unwrap();
    doc.update_dict(root, |c| c.set(b"PdfCraftTestStreams".to_vec(), Object::Array(all))).unwrap();
    let originals: Vec<(ObjRef, Stream)> = [plain, chained, truncated, indirect, with_dct, corrupt].iter().map(|r| (*r, stream(&doc, *r))).collect();

    // Off: untouched.
    let mut off = doc.clone();
    let mut s = Settings::default();
    s.lzw_to_flate = false;
    let r = optimize(&mut off, &s).unwrap();
    assert_eq!((r.lzw_reencoded, r.lzw_skipped), (0, 0));
    for (r, s) in &originals {
        assert_eq!(&stream(&off, *r), s);
    }

    let r = optimize(&mut doc, &Settings::default()).unwrap();
    assert_eq!((r.lzw_reencoded, r.lzw_skipped), (2, 4), "{r:?}");
    for re in [plain, chained] {
        let s = stream(&doc, re);
        assert_eq!(s.dict.name(b"Filter"), Some(&b"FlateDecode"[..]));
        assert!(!s.dict.contains(b"DecodeParms"));
        assert_eq!(s.decoded_strict().unwrap(), content);
    }
    assert_eq!(stream(&doc, plain).dict.int(b"Private"), Some(7), "other keys are kept");
    for (r, s) in &originals[2..] {
        assert_eq!(&stream(&doc, *r), s, "object {} must be left exactly as it was", r.num);
    }
    // The saved copy opens and decodes the same.
    let reopened = Document::open(Arc::new(save(&doc))).unwrap();
    let back = reopened.object_numbers().into_iter().filter_map(|n| match &*reopened.get(ObjRef::new(n, reopened.generation(n))) {
        Object::Stream(s) if s.dict.int(b"Private") == Some(7) => Some(s.decoded_strict().unwrap()),
        _ => None,
    });
    assert_eq!(back.collect::<Vec<_>>(), vec![content]);
}

#[test]
fn hostile_one_bit_images_never_panic_and_stay_unchanged() {
    let mut doc = Document::new_empty();
    let mut refs = Vec::new();
    let base = |w: Object, h: Object| {
        let mut d = Dict::new();
        d.set(b"Subtype".to_vec(), Object::name("Image"));
        d.set(b"Width".to_vec(), w);
        d.set(b"Height".to_vec(), h);
        d.set(b"BitsPerComponent".to_vec(), Object::Int(1));
        d.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
        d
    };
    for (w, h, raw) in [
        (Object::Int(-5), Object::Int(10), vec![0u8; 10]),
        (Object::Int(0), Object::Int(10), vec![0u8; 10]),
        (Object::Int(i64::MAX), Object::Int(i64::MAX), vec![0u8; 10]),
        (Object::Int(1 << 40), Object::Int(1), vec![0u8; 10]),
        (Object::Int(100), Object::Int(100), vec![0u8; 3]), // too few samples
        (Object::Real(-3.5), Object::Int(4), vec![0u8; 3]),
        (Object::name("x"), Object::Int(4), vec![0u8; 3]),
    ] {
        refs.push(doc.add(Object::Stream(Stream::from_raw(base(w, h), raw))));
    }
    // Already CCITT; corrupt Flate; truncated LZW; indirect filter; colour one-bit (not mono).
    let mut ccitt = base(Object::Int(64), Object::Int(64));
    ccitt.set(b"Filter".to_vec(), Object::name("CCITTFaxDecode"));
    refs.push(doc.add(Object::Stream(Stream::from_raw(ccitt, vec![0x80; 512]))));
    let mut bad = base(Object::Int(64), Object::Int(64));
    bad.set(b"Filter".to_vec(), Object::name("FlateDecode"));
    refs.push(doc.add(Object::Stream(Stream::from_raw(bad, vec![0x78, 0x9c, 0xff, 0x00, 0x13]))));
    let full = lzw_stream(base(Object::Int(64), Object::Int(64)), &[0x55; 512], Object::name("LZWDecode"), None, &Params::default());
    let mut cut = full.clone();
    cut.raw = full.raw[..full.raw.len() - 1].to_vec().into();
    refs.push(doc.add(Object::Stream(cut)));
    let n = doc.add(Object::name("FlateDecode"));
    let mut ind = base(Object::Int(64), Object::Int(64));
    ind.set(b"Filter".to_vec(), Object::Ref(n));
    refs.push(doc.add(Object::Stream(Stream::from_raw(ind, pdfcraft_filters::encode_flate(&[0u8; 512])))));
    let mut rgb = base(Object::Int(64), Object::Int(64));
    rgb.set(b"ColorSpace".to_vec(), Object::name("DeviceRGB"));
    refs.push(doc.add(Object::Stream(Stream::from_raw(rgb, vec![0u8; 1536]))));
    let mut content = String::from("q ");
    let mut x = Dict::new();
    for (i, r) in refs.iter().enumerate() {
        x.set(format!("I{i}").into_bytes(), Object::Ref(*r));
        content.push_str(&format!("q 10 0 0 10 {} 0 cm /I{i} Do Q ", i * 10));
    }
    content.push('Q');
    let first = page(&mut doc, refs[0], Stream::from_raw(Dict::new(), content.into_bytes()), [612.0, 792.0]);
    doc.update_dict(first, |p| {
        let mut res = Dict::new();
        res.set(b"XObject".to_vec(), Object::Dict(x));
        p.set(b"Resources".to_vec(), Object::Dict(res));
    })
    .unwrap();
    let before: Vec<Stream> = refs.iter().map(|r| stream(&doc, *r)).collect();
    // (Flate-encoding unfiltered streams would legitimately change some of them.)
    let s = Settings { flate_unencoded: false, ..Settings::default() };
    let r = optimize(&mut doc, &s).unwrap();
    assert_eq!(r.images_ccitt + r.images_recompressed + r.images_resampled, 0, "{r:?}");
    for (r, s) in refs.iter().zip(&before) {
        assert_eq!(&stream(&doc, *r), s, "object {}", r.num);
    }
}

/// The encoder alone against Poppler: images of awkward widths and densities (single pixels,
/// odd widths, long runs needing extended make-up codes, noise) encoded as G4 with both
/// polarities, one per page; `pdfimages` must decode each to exactly the source bitmap.
#[test]
fn poppler_decodes_g4_streams_of_awkward_shapes_exactly() {
    let mut doc = Document::new_empty();
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut sources = Vec::new();
    for (i, &(w, h)) in [(1usize, 1usize), (1, 9), (2, 3), (7, 5), (8, 8), (9, 4), (17, 13), (63, 7), (64, 3), (65, 9), (127, 5), (1727, 3), (1728, 4), (1729, 3), (2561, 3), (5200, 4), (333, 77)]
        .iter()
        .enumerate()
    {
        let density = [0u64, 3, 50, 500, 970, 1000][i % 6];
        let px: Vec<bool> = (0..w * h).map(|j| if i % 4 == 3 { (j % w) < w / 2 + j / w } else { next() % 1000 < density }).collect();
        let black_is_1 = i % 2 == 1;
        // Samples: 0 = black (DeviceGray). BlackIs1 only changes which value G4 calls black; the
        // decoded samples must be the same either way.
        let data = pdfcraft_filters::encode_ccitt_g4(&pack(&px, w, false), w, h, black_is_1, usize::MAX).unwrap();
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("XObject"));
        d.set(b"Subtype".to_vec(), Object::name("Image"));
        d.set(b"Width".to_vec(), Object::Int(w as i64));
        d.set(b"Height".to_vec(), Object::Int(h as i64));
        d.set(b"BitsPerComponent".to_vec(), Object::Int(1));
        d.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
        d.set(b"Filter".to_vec(), Object::name("CCITTFaxDecode"));
        let mut parms = Dict::new();
        parms.set(b"K".to_vec(), Object::Int(-1));
        parms.set(b"Columns".to_vec(), Object::Int(w as i64));
        parms.set(b"Rows".to_vec(), Object::Int(h as i64));
        parms.set(b"BlackIs1".to_vec(), Object::Bool(black_is_1));
        d.set(b"DecodeParms".to_vec(), Object::Dict(parms));
        let img = doc.add(Object::Stream(Stream::from_raw(d, data)));
        full_page(&mut doc, img, w, h, "");
        sources.push((w, h, px));
    }
    let dir = oracle_dir("ccitt-shapes");
    std::fs::write(dir.join("shapes.pdf"), save(&doc)).unwrap();
    if Command::new("pdfimages").arg("-v").output().is_err() {
        eprintln!("skipped: pdfimages (Poppler) is not installed");
        return;
    }
    let images = pdfimages(&dir, "shapes");
    assert_eq!(images.len(), sources.len());
    for (pbm, (w, h, px)) in images.iter().zip(&sources) {
        let (pw, ph, got) = pbm_pixels(pbm);
        assert_eq!((pw, ph), (*w, *h));
        assert!(&got == px, "{w}x{h} decodes differently in Poppler");
    }
}
