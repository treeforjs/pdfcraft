//! End-to-end Enhance ▸ Camera Image (`scan_camera_image`) on original synthetic camera photos:
//! a PdfCraft-made text page, rendered, is photographed by a pinhole camera written in this file
//! (tilted, turned, unevenly lit) onto a textured table, and wrapped as image-only PDF pages
//! (one pixel per point; Flate and JPEG). Ground truth is the projection of the page's corners.
#![allow(clippy::needless_range_loop)]
use pdfcraft_automation::{Automation, Content, ToolError};
use pdfcraft_cos::{Dict, Document, Object, SaveOptions, Stream, write_full};
use pdfcraft_engine::Session;
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest};
use serde_json::{Value, json};
use std::path::PathBuf;

const TEXT: &str = "Camera images are corrected before reading.\nThe page outline is found in the photo.\nPerspective is undone and the light evened out.\nEvery fixture here was generated for this test.";
/// The photo's size.
const W: usize = 1100;
const H: usize = 850;

fn ok(a: &mut Automation, name: &str, args: Value) -> Value {
    let mut reply = a.call(name, &args).unwrap_or_else(|e| panic!("{name}: {e}"));
    match reply.remove(0) {
        Content::Json(v) => v,
        other => panic!("expected JSON, got {other:?}"),
    }
}

fn root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("pdfcraft-camera-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// The source page's luma at `scale` pixels per point.
fn source(scale: f32) -> (usize, usize, Vec<u8>) {
    let session = Session::new();
    let bytes = session.create_from_text("Camera fixture", TEXT).unwrap();
    let mut r = PageRenderer::new(bytes, RenderConfig::default());
    let shot = r.render(RenderRequest { scale, ..Default::default() });
    assert!(shot.error.is_none());
    let luma = shot.rgba.chunks(4).map(|p| ((77 * u32::from(p[0]) + 150 * u32::from(p[1]) + 29 * u32::from(p[2])) >> 8) as u8).collect();
    (shot.width as usize, shot.height as usize, luma)
}

fn bilinear(px: &[u8], w: usize, h: usize, x: f64, y: f64) -> f64 {
    let (fx, fy) = ((x - 0.5).clamp(0.0, (w - 1) as f64), (y - 0.5).clamp(0.0, (h - 1) as f64));
    let (x0, y0) = (fx as usize, fy as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (fx - x0 as f64, fy - y0 as f64);
    let p = |x: usize, y: usize| f64::from(px[y * w + x]);
    (p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx) * (1.0 - ty) + (p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx) * ty
}

/// The lighting falling on the page: darker towards its bottom-right.
fn shade(u: f64, v: f64) -> f64 {
    1.0 - 0.22 * u * v
}

/// A textured table (smooth value noise in browns, with grain), RGB.
fn table(x: f64, y: f64) -> [f64; 3] {
    let n = ((x * 0.031).sin() * (y * 0.027).cos() + (x * 0.011 + y * 0.017).sin()) * 0.5;
    let grain = (((x as i64).wrapping_mul(73_856_093) ^ (y as i64).wrapping_mul(19_349_663)) & 15) as f64;
    let l = 95.0 + 30.0 * n + grain;
    [l * 1.15, l * 0.92, l * 0.7]
}

/// A pinhole camera (focal length `f` pixels, principal point at the centre) looking at a page
/// of `pw`×`ph` units, tilted by `rx`, `ry` and turned by `rz` (radians) at distance `z`.
/// Returns the page (u, v in 0..1) → photo pixel map.
#[allow(clippy::too_many_arguments)]
fn camera(w: usize, h: usize, pw: f64, ph: f64, rx: f64, ry: f64, rz: f64, z: f64, f: f64) -> impl Fn(f64, f64) -> [f64; 2] {
    let (sx, cx) = rx.sin_cos();
    let (sy, cy) = ry.sin_cos();
    let (sz, cz) = rz.sin_cos();
    move |u, v| {
        let p = [(u - 0.5) * pw, (v - 0.5) * ph, 0.0];
        let p = [p[0] * cz - p[1] * sz, p[0] * sz + p[1] * cz, p[2]];
        let p = [p[0], p[1] * cx - p[2] * sx, p[1] * sx + p[2] * cx];
        let p = [p[0] * cy + p[2] * sy, p[1], -p[0] * sy + p[2] * cy];
        [w as f64 / 2.0 + f * p[0] / (p[2] + z), h as f64 / 2.0 + f * p[1] / (p[2] + z)]
    }
}

/// The 3×3 homography taking the unit square onto `q` (TL, TR, BR, BL), and its inverse.
fn homography(q: [[f64; 2]; 4]) -> ([[f64; 3]; 3], [[f64; 3]; 3]) {
    // Solve the 8 unknowns by Gaussian elimination (independent of the library's closed form).
    let src = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let mut m = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let ([u, v], [x, y]) = (src[i], q[i]);
        m[2 * i] = [u, v, 1.0, 0.0, 0.0, 0.0, -u * x, -v * x, x];
        m[2 * i + 1] = [0.0, 0.0, 0.0, u, v, 1.0, -u * y, -v * y, y];
    }
    for c in 0..8 {
        let p = (c..8).max_by(|a, b| m[*a][c].abs().total_cmp(&m[*b][c].abs())).unwrap();
        m.swap(c, p);
        for r in 0..8 {
            if r != c {
                let k = m[r][c] / m[c][c];
                for j in c..9 {
                    m[r][j] -= k * m[c][j];
                }
            }
        }
    }
    let s: Vec<f64> = (0..8).map(|i| m[i][8] / m[i][i]).collect();
    let h = [[s[0], s[1], s[2]], [s[3], s[4], s[5]], [s[6], s[7], 1.0]];
    let det = h[0][0] * (h[1][1] * h[2][2] - h[1][2] * h[2][1]) - h[0][1] * (h[1][0] * h[2][2] - h[1][2] * h[2][0])
        + h[0][2] * (h[1][0] * h[2][1] - h[1][1] * h[2][0]);
    let c = |a: usize, b: usize, c: usize, d: usize| (h[a][b] * h[c][d] - h[a][d] * h[c][b]) / det;
    let inv = [
        [c(1, 1, 2, 2), -c(0, 1, 2, 2), c(0, 1, 1, 2)],
        [-c(1, 0, 2, 2), c(0, 0, 2, 2), -c(0, 0, 1, 2)],
        [c(1, 0, 2, 1), -c(0, 0, 2, 1), c(0, 0, 1, 1)],
    ];
    (h, inv)
}

/// The `W`×`H` photo (RGB) of the source page, and the page's corners in it.
fn photograph(src: &(usize, usize, Vec<u8>)) -> (Vec<u8>, [[f64; 2]; 4]) {
    photograph_sized(src, W, H)
}

/// A `w`×`h` photo (RGB) of the source page: the same view as [`photograph`], scaled with
/// the photo's size (the focal length grows with it), and the page's corners in it.
fn photograph_sized(src: &(usize, usize, Vec<u8>), w: usize, h: usize) -> (Vec<u8>, [[f64; 2]; 4]) {
    let (sw, sh, luma) = src;
    // The page is as large in the photo whatever the source page's resolution.
    let k = w as f64 / W as f64;
    let page = 765.0 * k / *sw as f64;
    let project = camera(w, h, *sw as f64 * page, *sh as f64 * page, 0.33, -0.27, 0.07, 2300.0 * k, 1500.0 * k);
    let truth = [project(0.0, 0.0), project(1.0, 0.0), project(1.0, 1.0), project(0.0, 1.0)];
    let (_, inv) = homography(truth);
    let mut px = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0; 3];
            for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let (fx, fy) = (x as f64 + ox, y as f64 + oy);
                let d = inv[2][0] * fx + inv[2][1] * fy + inv[2][2];
                let (u, v) = ((inv[0][0] * fx + inv[0][1] * fy + inv[0][2]) / d, (inv[1][0] * fx + inv[1][1] * fy + inv[1][2]) / d);
                let c = if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) {
                    // Slightly warm light on the paper.
                    let l = bilinear(luma, *sw, *sh, u * *sw as f64, v * *sh as f64) * shade(u, v);
                    [l, l * 0.97, l * 0.9]
                } else {
                    table(fx, fy)
                };
                for k in 0..3 {
                    sum[k] += c[k] / 4.0;
                }
            }
            px.extend(sum.map(|v| v.round().clamp(0.0, 255.0) as u8));
        }
    }
    (px, truth)
}

/// An image-only page dictionary drawing `img` over a `w`×`h` media box.
fn photo_page(doc: &mut Document, pages: pdfcraft_cos::ObjRef, img: Stream, rotate: i64, (w, h): (usize, usize)) -> pdfcraft_cos::ObjRef {
    let img = doc.add(Object::Stream(img));
    let c = doc.add(Object::Stream(Stream::from_raw(Dict::new(), format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q").into_bytes())));
    let mut x = Dict::new();
    x.set(b"Im0".to_vec(), Object::Ref(img));
    let mut res = Dict::new();
    res.set(b"XObject".to_vec(), Object::Dict(x));
    let mut p = Dict::new();
    p.set(b"Type".to_vec(), Object::name("Page"));
    p.set(b"Parent".to_vec(), Object::Ref(pages));
    p.set(b"MediaBox".to_vec(), Object::Array(vec![0.into(), 0.into(), (w as i64).into(), (h as i64).into()]));
    p.set(b"Resources".to_vec(), Object::Dict(res));
    p.set(b"Contents".to_vec(), Object::Ref(c));
    if rotate != 0 {
        p.set(b"Rotate".to_vec(), Object::Int(rotate));
    }
    doc.add(Object::Dict(p))
}

fn image_dict((w, h): (usize, usize)) -> Dict {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Image"));
    d.set(b"Width".to_vec(), Object::Int(w as i64));
    d.set(b"Height".to_vec(), Object::Int(h as i64));
    d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
    d.set(b"ColorSpace".to_vec(), Object::name("DeviceRGB"));
    d
}

/// Page 1: the photo (Flate). Page 2: the same photo as JPEG on a page turned 90°. Page 3: a
/// page without an image.
fn camera_pdf(photo: &[u8]) -> Vec<u8> {
    camera_pdf_sized(photo, (W, H))
}

/// [`camera_pdf`] for a `size` photo.
fn camera_pdf_sized(photo: &[u8], size: (usize, usize)) -> Vec<u8> {
    let mut doc = Document::new_empty();
    let pages = doc.root().and_then(|r| doc.get(r).as_dict().and_then(|d| d.reference(b"Pages"))).unwrap();
    let p1 = photo_page(&mut doc, pages, Stream::flate(image_dict(size), photo), 0, size);
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 92)
        .encode(photo, size.0 as u32, size.1 as u32, image::ExtendedColorType::Rgb8)
        .unwrap();
    let mut d = image_dict(size);
    d.set(b"Filter".to_vec(), Object::name("DCTDecode"));
    let p2 = photo_page(&mut doc, pages, Stream::from_raw(d, jpeg), 90, size);
    let c = doc.add(Object::Stream(Stream::from_raw(Dict::new(), b"0 0 1 rg 10 10 100 100 re f".to_vec())));
    let mut p = Dict::new();
    p.set(b"Type".to_vec(), Object::name("Page"));
    p.set(b"Parent".to_vec(), Object::Ref(pages));
    p.set(b"MediaBox".to_vec(), Object::Array(vec![0.into(), 0.into(), 200.into(), 200.into()]));
    p.set(b"Contents".to_vec(), Object::Ref(c));
    let p3 = doc.add(Object::Dict(p));
    doc.update_dict(pages, |d| {
        d.set(b"Kids".to_vec(), Object::Array(vec![Object::Ref(p1), Object::Ref(p2), Object::Ref(p3)]));
        d.set(b"Count".to_vec(), Object::Int(3));
    })
    .unwrap();
    write_full(&doc, &SaveOptions { object_streams: false, ..SaveOptions::default() }).unwrap()
}

/// A page rendered at `dpi`: (width, height, luma).
fn render(a: &mut Automation, doc: u64, page: u64, dpi: f64) -> (usize, usize, Vec<u8>) {
    let out = a.call("page_render", &json!({ "doc": doc, "page": page, "dpi": dpi })).unwrap();
    let Content::Png { data, .. } = &out[0] else { panic!("expected an image") };
    let mut reader = png::Decoder::new(std::io::Cursor::new(data.as_slice())).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    let n = info.color_type.samples();
    let luma = buf[..info.buffer_size()]
        .chunks(n)
        .map(|p| if n >= 3 { ((77 * u32::from(p[0]) + 150 * u32::from(p[1]) + 29 * u32::from(p[2])) >> 8) as u8 } else { p[0] })
        .collect();
    (info.width as usize, info.height as usize, luma)
}

fn corners(v: &Value) -> [[f64; 2]; 4] {
    let c: Vec<[f64; 2]> = v.as_array().unwrap().iter().map(|p| [p[0].as_f64().unwrap(), p[1].as_f64().unwrap()]).collect();
    [c[0], c[1], c[2], c[3]]
}

fn max_error(a: &[[f64; 2]; 4], b: &[[f64; 2]; 4]) -> f64 {
    (0..4).map(|i| (a[i][0] - b[i][0]).hypot(a[i][1] - b[i][1])).fold(0.0, f64::max)
}

#[test]
fn camera_photos_are_found_straightened_and_enhanced_as_one_undoable_step() {
    let src = source(1.25);
    let (photo, truth) = photograph(&src);
    let dir = root("e2e");
    std::fs::write(dir.join("photo.pdf"), camera_pdf(&photo)).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap();
    let doc = ok(&mut a, "doc_open", json!({ "path": "photo.pdf" }))["doc"].as_u64().unwrap();
    let before = render(&mut a, doc, 1, 72.0);

    // Bad arguments change nothing.
    for bad in [
        json!({ "doc": doc, "corners": [[0, 0], [10, 0], [10, 10]] }),
        json!({ "doc": doc, "corners": [[0, 0], [10, 0], [10, 10], "x"] }),
        json!({ "doc": doc, "corners": [[0, 0], [10, 0], [10, 10], [0, 10, 3]] }),
        json!({ "doc": doc, "corners": [[0, 0], [500, 0], [1000, 0], [0, 400]], "pages": [1] }),
        json!({ "doc": doc, "corners": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "pages": [1] }),
        json!({ "doc": doc, "enhance": "yes" }),
        json!({ "doc": doc, "dry_run": 1 }),
        json!({ "doc": doc, "pages": [0] }),
        json!({ "doc": doc, "pages": [4] }),
        json!({ "doc": doc, "pages": [1, 1] }),
    ] {
        assert!(matches!(a.call("scan_camera_image", &bad), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
    assert!(a.call("scan_camera_image", &json!({ "doc": 999 })).is_err());
    assert!(a.call("edit_undo", &json!({ "doc": doc })).is_err(), "nothing was changed");

    // A dry run finds the page in both photos and skips the page without one.
    let r = ok(&mut a, "scan_camera_image", json!({ "doc": doc, "dry_run": true }));
    assert_eq!(
        (r["dry_run"].as_bool(), r["changed"].as_u64(), r["would_change"].as_u64(), r["undoable"].as_bool()),
        (Some(true), Some(0), Some(2), Some(false)),
        "{r}"
    );
    let p1 = &r["pages"][0];
    assert_eq!((p1["found"].as_bool(), p1["manual"].as_bool()), (Some(true), Some(false)), "{p1}");
    assert!(p1["confidence"].as_f64().unwrap() > 0.6, "{p1}");
    // One pixel per point, unrotated: the displayed corners are the photo's.
    let err = max_error(&corners(&p1["corners"]), &truth);
    assert!(err < 2.5, "corners {} vs {truth:?}: {err}", p1["corners"]);
    assert!(max_error(&corners(&p1["image_corners"]), &truth) < 2.5);
    let aspect = src.0 as f64 / src.1 as f64;
    assert!((p1["aspect"].as_f64().unwrap() - aspect).abs() < 0.02 * aspect, "{p1} vs {aspect}");
    assert_eq!(p1["image_size"], json!([W, H]));
    let size = p1["page_size"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
    assert!((size[1] - W as f64).abs() < 1e-6 && (size[0] / size[1] - aspect).abs() < 0.02, "{p1}");
    // Turned 90° (and JPEG): the outline is reported as displayed, and the page comes out
    // upright as displayed, so landscape.
    let p2 = &r["pages"][1];
    assert_eq!(p2["found"].as_bool(), Some(true), "{p2}");
    let turned = truth.map(|[x, y]| [H as f64 - y, x]);
    let shown = corners(&p2["corners"]);
    let err2 = (0..4)
        .map(|i| (0..4).map(|j| (shown[i][0] - turned[j][0]).hypot(shown[i][1] - turned[j][1])).fold(f64::INFINITY, f64::min))
        .fold(0.0, f64::max);
    assert!(err2 < 3.5, "{p2} vs {turned:?}");
    assert!((p2["aspect"].as_f64().unwrap() - 1.0 / aspect).abs() < 0.03 / aspect, "{p2}");
    assert!(r["pages"][2]["skipped"].as_str().unwrap().contains("no image"), "{r}");
    assert!(a.call("edit_undo", &json!({ "doc": doc })).is_err(), "a dry run changes nothing");

    // Straighten page 1 without enhancing: it matches the source page under the photo's light.
    let r = ok(&mut a, "scan_camera_image", json!({ "doc": doc, "pages": [1], "enhance": false }));
    assert_eq!((r["changed"].as_u64(), r["undoable"].as_bool()), (Some(1), Some(true)), "{r}");
    let page_w = r["pages"][0]["page_size"][0].as_f64().unwrap();
    let (rw, rh, out) = render(&mut a, doc, 1, 72.0 * src.0 as f64 / page_w);
    let (mut diff, mut count) = (0.0, 0.0);
    for y in (rh / 25..rh - rh / 25).step_by(2) {
        for x in (rw / 25..rw - rw / 25).step_by(2) {
            let (u, v) = ((x as f64 + 0.5) / rw as f64, (y as f64 + 0.5) / rh as f64);
            let expect = bilinear(&src.2, src.0, src.1, u * src.0 as f64, v * src.1 as f64) * shade(u, v);
            diff += (f64::from(out[y * rw + x]) - expect).abs();
            count += 1.0;
        }
    }
    assert!(diff / count < 10.0, "mean difference from the source page {}", diff / count);
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Correct camera image");
    assert_eq!(render(&mut a, doc, 1, 72.0), before, "undo restores the photo");

    // Both photos, enhanced (the default), in one step.
    let r = ok(&mut a, "scan_camera_image", json!({ "doc": doc }));
    assert_eq!((r["changed"].as_u64(), r["undoable"].as_bool()), (Some(2), Some(true)), "{r}");
    let (rw, rh, out) = render(&mut a, doc, 1, 72.0 * src.0 as f64 / page_w);
    // The shadowed bottom-right paper is white again; the text stays dark.
    let paper: Vec<u8> =
        (rh * 90 / 100..rh * 97 / 100).flat_map(|y| (rw * 85 / 100..rw * 95 / 100).map(move |x| (x, y))).map(|(x, y)| out[y * rw + x]).collect();
    let white = paper.iter().filter(|v| **v >= 245).count() as f64 / paper.len() as f64;
    assert!(white > 0.97, "paper {white}");
    let ink = |img: &[u8]| img.iter().filter(|v| **v < 100).count() as f64;
    let source_ink = ink(&src.2) / (src.0 * src.1) as f64;
    let out_ink = ink(&out) / (rw * rh) as f64;
    assert!(out_ink > 0.6 * source_ink && out_ink < 1.6 * source_ink, "{out_ink} vs {source_ink}");
    let (w2, h2, _) = render(&mut a, doc, 2, 72.0);
    assert!(w2 > h2, "page 2 is landscape as displayed: {w2}x{h2}");
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Correct camera image");
    assert_eq!(render(&mut a, doc, 1, 72.0), before);
    assert_eq!(render(&mut a, doc, 2, 72.0).0, H, "page 2 restored by the same step");

    // Corners given (in any order) are used as they are.
    let shuffled = [truth[2], truth[0], truth[3], truth[1]];
    let r = ok(&mut a, "scan_camera_image", json!({ "doc": doc, "pages": [1], "corners": shuffled, "dry_run": true }));
    let p = &r["pages"][0];
    assert_eq!((p["manual"].as_bool(), p["confidence"].as_f64()), (Some(true), Some(1.0)), "{p}");
    assert!(max_error(&corners(&p["corners"]), &truth) < 0.01, "{p}");
    let r = ok(&mut a, "scan_camera_image", json!({ "doc": doc, "pages": [1], "corners": shuffled }));
    assert_eq!(r["changed"].as_u64(), Some(1));
    // Saved and reopened, the corrected page is still one image on a page of the new size.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "corrected.pdf" }));
    let again = ok(&mut a, "doc_open", json!({ "path": "corrected.pdf" }))["doc"].as_u64().unwrap();
    let (cw, ch, _) = render(&mut a, again, 1, 72.0);
    assert!((cw as f64 / ch as f64 - aspect).abs() < 0.02, "{cw}x{ch}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn camera_photos_without_a_page_are_only_enhanced_or_skipped() {
    // The table alone: nothing to find.
    let photo: Vec<u8> = (0..W * H).flat_map(|i| table((i % W) as f64, (i / W) as f64).map(|v| v as u8)).collect();
    let dir = root("table");
    std::fs::write(dir.join("table.pdf"), camera_pdf(&photo)).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap();
    let doc = ok(&mut a, "doc_open", json!({ "path": "table.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "scan_camera_image", json!({ "doc": doc, "pages": [1], "enhance": false }));
    let p = &r["pages"][0];
    assert_eq!((p["found"].as_bool(), r["changed"].as_u64()), (Some(false), Some(0)), "{r}");
    assert!(p["skipped"].as_str().unwrap().contains("give its corners"), "{p}");
    assert_eq!(p["corners"], json!([[0.0, 0.0], [W as f64, 0.0], [W as f64, H as f64], [0.0, H as f64]]));
    // Enhancing keeps the whole photo.
    let r = ok(&mut a, "scan_camera_image", json!({ "doc": doc, "pages": [1] }));
    assert_eq!(r["changed"].as_u64(), Some(1), "{r}");
    assert_eq!(r["pages"][0]["output_size"], json!([W, H]));
    let _ = std::fs::remove_dir_all(dir);
}

/// The share of `TEXT`'s words found in `text` (lower-case letters and digits compared).
fn recall(text: &str) -> f64 {
    let norm = |w: &str| w.chars().filter(char::is_ascii_alphanumeric).flat_map(char::to_lowercase).collect::<String>();
    let found: std::collections::HashSet<String> = text.split_whitespace().map(norm).collect();
    let words: Vec<String> = TEXT.split_whitespace().map(norm).filter(|w| !w.is_empty()).collect();
    words.iter().filter(|w| found.contains(*w)).count() as f64 / words.len() as f64
}

#[test]
fn recognize_text_can_correct_camera_photos_first() {
    // A photo at phone-camera detail: twice the size of the other tests' (the page about 1000 by
    // 1300 pixels in it, letters some 10 pixels high), the page rendered finer to match.
    let size = (2 * W, 2 * H);
    let src = source(2.5);
    let (photo, _) = photograph_sized(&src, size.0, size.1);
    let dir = root("ocr");
    std::fs::write(dir.join("photo.pdf"), camera_pdf_sized(&photo, size)).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap();
    let doc = ok(&mut a, "doc_open", json!({ "path": "photo.pdf" }))["doc"].as_u64().unwrap();
    for bad in [json!({ "doc": doc, "camera": "yes" }), json!({ "doc": doc, "camera": true, "output": "loud" })] {
        assert!(matches!(a.call("ocr_recognize", &bad), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
    assert!(a.call("edit_undo", &json!({ "doc": doc })).is_err(), "nothing was changed");
    if !pdfcraft_engine::ocr::Models::required_for_tests().unwrap() {
        // Without the models the photo is left alone.
        assert!(a.call("ocr_recognize", &json!({ "doc": doc, "pages": [1], "camera": true })).is_err());
        assert!(a.call("edit_undo", &json!({ "doc": doc })).is_err(), "nothing was changed");
        return;
    }
    // The photo as it is, for comparison (a second copy of the file).
    let plain = ok(&mut a, "doc_open", json!({ "path": "photo.pdf" }))["doc"].as_u64().unwrap();
    let raw = ok(&mut a, "ocr_recognize", json!({ "doc": plain, "pages": [1] }));
    let raw_text = raw["pages"][0]["text"].as_str().unwrap().to_lowercase();
    let r = ok(&mut a, "ocr_recognize", json!({ "doc": doc, "pages": [1], "camera": true }));
    assert_eq!(r["camera"][0]["found"].as_bool(), Some(true), "{r}");
    let text = r["pages"][0]["text"].as_str().unwrap().to_lowercase();
    let (before, after) = (recall(&raw_text), recall(&text));
    eprintln!("recall without correction {before:.2}: {raw_text:?}\nrecall with correction {after:.2}: {text:?}");
    assert!(after >= 0.9, "corrected page read {after:.2} of the words: {text}");
    assert!(after >= before + 0.1, "the correction did not help: {before:.2} -> {after:.2}");
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Recognize text");
    assert!(a.call("edit_undo", &json!({ "doc": doc })).is_err(), "one step");
    let _ = std::fs::remove_dir_all(dir);
}
