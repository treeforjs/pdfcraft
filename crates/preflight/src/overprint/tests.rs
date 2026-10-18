use std::sync::Arc;

use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind, device_cmyk_to_rgb};

use super::*;
use crate::coverage::BuiltIn;

fn stream(data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

const RESOURCES: &str = "<< /ColorSpace << \
    /Gold [/Separation /Gold /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 1 0] /N 1 >>] >> \
    /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /FirstChar 72 /LastChar 73 /Widths [722 278] >> >> \
    /ExtGState << /OP << /OP true /op true /OPM 1 >> /OP0 << /OP true /op true /OPM 0 >> >> >>";

/// One 200 × 100 pt page with `content` (and `rotate`); `catalog` and `info` go into the
/// catalog and the trailer's information dictionary.
fn pdf_bytes(content: &[u8], rotate: u32, catalog: &str, info: &str) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>").into_bytes(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Rotate {rotate} /Resources {RESOURCES} /Contents 4 0 R >>").into_bytes(),
        stream(content),
        format!("<< {info} >>").into_bytes(),
    ];
    objs.push(b"<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier (FOGRA39) >>".to_vec());
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
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

struct Page {
    doc: Document,
    rgba: Vec<u8>,
    width: usize,
    height: usize,
}

/// Open and render (at `scale`) a page with `content`.
fn page(content: &[u8], rotate: u32, scale: f32) -> Page {
    let bytes = pdf_bytes(content, rotate, "", "");
    let doc = Document::open(Arc::new(bytes.clone())).unwrap();
    let mut renderer = PageRenderer::new(Arc::new(bytes), RenderConfig::default());
    let out = renderer.render(RenderRequest { page: 0, kind: RequestKind::Pixels, scale, ..Default::default() });
    assert!(out.error.is_none(), "{:?}", out.error);
    Page { doc, rgba: out.rgba.to_vec(), width: out.width as usize, height: out.height as usize }
}

impl Page {
    fn simulate(&mut self, rotation: u16) -> Simulation {
        simulate(&self.doc, 0, rotation, &mut self.rgba, self.width, self.height, &BuiltIn).unwrap()
    }
    /// The pixel at (`x`, `y`) points from the displayed page's top-left.
    fn at(&self, x: f64, y: f64) -> [u8; 3] {
        let s = self.width as f64 / if self.width > self.height { 200.0 } else { 100.0 };
        let (px, py) = ((x * s) as usize, (y * s) as usize);
        let i = (py * self.width + px) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]
    }
}

fn cmyk(c: [f64; 4]) -> [u8; 3] {
    let q = c.map(|v| (v * 255.0).round() as u8);
    let v = device_cmyk_to_rgb(&q).unwrap();
    [v[0], v[1], v[2]]
}

fn close(a: [u8; 3], b: [u8; 3], tolerance: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tolerance)
}

/// Cyan on the left two thirds, then yellow over the right two thirds, with `gs` before it.
fn cyan_then_yellow(gs: &str) -> Vec<u8> {
    format!("1 0 0 0 k 0 0 140 100 re f {gs} 0 0 1 0 k 60 0 140 100 re f").into_bytes()
}

#[test]
fn modes_parse_and_pdfx_files_are_recognised() {
    assert_eq!(Mode::default(), Mode::PdfX);
    assert_eq!(Mode::from_id(" Automatic "), Some(Mode::Automatic));
    assert_eq!(Mode::from_id("sometimes"), None);
    assert!(Mode::ALL.iter().all(|m| Mode::from_id(m.id()) == Some(*m)));
    let plain = Document::open(Arc::new(pdf_bytes(b"", 0, "", ""))).unwrap();
    assert!(!is_pdfx(&plain));
    assert!(!Mode::PdfX.applies(&plain) && Mode::Always.applies(&plain) && Mode::Automatic.applies(&plain) && !Mode::Never.applies(&plain));
    let declared = Document::open(Arc::new(pdf_bytes(b"", 0, "", "/GTS_PDFXVersion (PDF/X-4)"))).unwrap();
    assert!(is_pdfx(&declared) && Mode::PdfX.applies(&declared));
    let intent = Document::open(Arc::new(pdf_bytes(b"", 0, "/OutputIntents [6 0 R]", ""))).unwrap();
    assert!(is_pdfx(&intent));
    assert!(!Mode::Never.applies(&intent));
}

#[test]
fn yellow_overprinting_cyan_prints_green_where_knockout_shows_yellow() {
    let mut p = page(&cyan_then_yellow("/OP gs"), 0, 1.0);
    let (cyan, yellow, green) = (cmyk([1.0, 0.0, 0.0, 0.0]), cmyk([0.0, 0.0, 1.0, 0.0]), cmyk([1.0, 0.0, 1.0, 0.0]));
    assert!(close(p.at(100.0, 50.0), yellow, 2), "knocked out: {:?}", p.at(100.0, 50.0));
    let sim = p.simulate(0);
    assert_eq!(sim.overprint_paints, 1);
    assert!(sim.changed_pixels > 0 && sim.changed_cells > 0);
    // The overlap is 80 of 200 pt wide.
    assert!((sim.changed_percent - 40.0).abs() < 2.0, "{}", sim.changed_percent);
    assert!(close(p.at(100.0, 50.0), green, 3), "overprinted: {:?} vs {green:?}", p.at(100.0, 50.0));
    assert!(close(p.at(20.0, 50.0), cyan, 2), "cyan alone: {:?}", p.at(20.0, 50.0));
    assert!(close(p.at(180.0, 50.0), yellow, 2), "yellow alone: {:?}", p.at(180.0, 50.0));
}

#[test]
fn without_overprint_the_render_is_left_alone_after_one_pass() {
    let mut p = page(&cyan_then_yellow(""), 0, 1.0);
    let before = p.rgba.clone();
    let sim = p.simulate(0);
    assert_eq!((sim.overprint_paints, sim.changed_cells, sim.changed_pixels), (0, 0, 0));
    assert_eq!(p.rgba, before);
}

#[test]
fn overprint_mode_decides_whether_zero_components_keep_the_plates_below() {
    // OPM 1: white DeviceCMYK paints nothing, so it disappears over the cyan.
    let mut p = page(b"1 0 0 0 k 0 0 200 100 re f /OP gs 0 0 0 0 k 50 0 100 100 re f", 0, 1.0);
    assert!(close(p.at(100.0, 50.0), [255, 255, 255], 1));
    p.simulate(0);
    assert!(close(p.at(100.0, 50.0), cmyk([1.0, 0.0, 0.0, 0.0]), 3), "{:?}", p.at(100.0, 50.0));
    // OPM 0: every process plate is painted, so white knocks the cyan out as usual.
    let mut p = page(b"1 0 0 0 k 0 0 200 100 re f /OP0 gs 0 0 0 0 k 50 0 100 100 re f", 0, 1.0);
    let sim = p.simulate(0);
    assert_eq!(sim.changed_pixels, 0);
    assert!(close(p.at(100.0, 50.0), [255, 255, 255], 1));
}

#[test]
fn a_spot_ink_overprinting_process_ink_multiplies() {
    let mut p = page(b"1 0 0 0 k 0 0 200 100 re f /OP0 gs /Gold cs 1 scn 50 0 100 100 re f", 0, 1.0);
    p.simulate(0);
    let cyan = cmyk([1.0, 0.0, 0.0, 0.0]);
    let gold = cmyk([0.0, 0.2, 1.0, 0.0]);
    let want = [0, 1, 2].map(|k| (f32::from(cyan[k]) * f32::from(gold[k]) / 255.0).round() as u8);
    assert!(close(p.at(100.0, 50.0), want, 3), "{:?} vs {want:?}", p.at(100.0, 50.0));
    assert!(close(p.at(20.0, 50.0), cyan, 2));
}

#[test]
fn overprinting_text_keeps_its_glyph_shapes() {
    // Big yellow text overprinting a cyan box: glyphs turn green, the cyan between them stays.
    let content = b"1 0 0 0 k 0 0 200 100 re f /OP gs 0 0 1 0 k BT /F1 60 Tf 10 25 Td (HIH) Tj ET";
    let mut p = page(content, 0, 1.0);
    let before = p.rgba.clone();
    let (cyan, green, yellow) = (cmyk([1.0, 0.0, 0.0, 0.0]), cmyk([1.0, 0.0, 1.0, 0.0]), cmyk([0.0, 0.0, 1.0, 0.0]));
    let sim = p.simulate(0);
    assert!(sim.changed_pixels > 100, "{sim:?}");
    let (mut green_pixels, mut kept, mut cyan_before) = (0, 0, 0);
    for (old, new) in before.as_chunks::<4>().0.iter().zip(p.rgba.as_chunks::<4>().0.iter()) {
        let (old, new) = ([old[0], old[1], old[2]], [new[0], new[1], new[2]]);
        if close(old, cyan, 2) {
            cyan_before += 1;
            kept += usize::from(close(new, cyan, 4));
        }
        if close(old, yellow, 2) {
            green_pixels += usize::from(close(new, green, 6));
        }
    }
    assert!(cyan_before > 1000 && kept == cyan_before, "{kept} of {cyan_before} cyan pixels kept");
    assert!(green_pixels > 100, "{green_pixels}");
    let left: Vec<(usize, usize)> = p
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, px)| close([px[0], px[1], px[2]], yellow, 8))
        .map(|(i, _)| (i % p.width, i / p.width))
        .collect();
    assert!(left.is_empty(), "yellow left at {left:?}");
}

#[test]
fn rotated_pages_map_pixels_to_the_right_cells() {
    // Turned a quarter clockwise the page shows 100 × 200 pt; the user-space overlap (x 60–140)
    // runs down the displayed page.
    let mut p = page(&cyan_then_yellow("/OP gs"), 90, 1.0);
    assert_eq!((p.width, p.height), (100, 200));
    p.simulate(90);
    let green = cmyk([1.0, 0.0, 1.0, 0.0]);
    assert!(close(p.at(50.0, 100.0), green, 3), "{:?}", p.at(50.0, 100.0));
    assert!(close(p.at(50.0, 20.0), cmyk([1.0, 0.0, 0.0, 0.0]), 2), "{:?}", p.at(50.0, 20.0));
}

#[test]
fn listed_objects_say_whether_overprinting_changes_anything() {
    let content = b"1 0 0 0 k 0 0 100 100 re f /OP gs 0 0 1 0 k 50 0 40 100 re f 0 1 0 0 k 150 0 40 100 re f";
    let doc = Document::open(Arc::new(pdf_bytes(content, 0, "", ""))).unwrap();
    let list = objects(&doc, 0, 72.0, 100, &BuiltIn).unwrap();
    assert_eq!(list.objects.len(), 2, "{:?}", list.objects);
    assert_eq!(list.states.len(), 1);
    let over: Vec<Option<bool>> = list.objects.iter().map(|o| o.changes_appearance()).collect();
    // Yellow over cyan changes the page; magenta over bare paper doesn't.
    assert_eq!(over, [Some(true), Some(false)]);
    assert!(list.objects[0].changed_cells.unwrap() > 1000);
    assert_eq!(list.difference.overprint_paints, 2);
    assert!(objects(&doc, 0, 72.0, 0, &BuiltIn).is_err());
    assert!(objects(&doc, 0, f64::NAN, 10, &BuiltIn).is_err());
    assert!(objects(&doc, 3, 72.0, 10, &BuiltIn).is_err());
}

#[test]
fn bad_buffers_and_pages_are_refused() {
    let mut p = page(&cyan_then_yellow("/OP gs"), 0, 1.0);
    let (w, h) = (p.width, p.height);
    assert!(simulate(&p.doc, 0, 0, &mut p.rgba, w + 1, h, &BuiltIn).is_err());
    assert!(simulate(&p.doc, 0, 0, &mut p.rgba, 0, 0, &BuiltIn).is_err());
    assert!(simulate(&p.doc, 5, 0, &mut p.rgba, w, h, &BuiltIn).is_err());
    // A tiny or huge render is separated within the grid's range.
    let mut tiny = vec![255u8; 8];
    let sim = simulate(&p.doc, 0, 0, &mut tiny, 2, 1, &BuiltIn).unwrap();
    assert!(sim.dpi >= MIN_DPI);
    let d = difference(&p.doc, 0, f64::INFINITY, true, &BuiltIn).unwrap();
    assert!(d.dpi <= MAX_DPI && d.backdrop.len() == d.cells.len());
}
