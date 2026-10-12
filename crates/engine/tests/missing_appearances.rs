//! Form widgets and comments without an appearance stream are drawn for display (ISO 32000-2
//! §12.5.5), from their dictionaries, by the generators PdfCraft uses when it creates them. The
//! file itself is never changed. Pixel checks for each kind, at 72 dpi (one pixel per point; rows
//! count down from the top of the 400 × 300 pt page).

use std::sync::Arc;

use pdfcraft_engine::{Session, display_bytes_for};
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RenderedPage, RequestKind};

/// A classic-xref PDF of `objs` (object `i + 1` is `objs[i]`), with object 1 as its catalog.
fn pdf(objs: &[String]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// Page 1: widgets without `/AP` (text, multiline, comb, list box, check box, a rotated text
/// field, and Hidden and NoView ones). Page 2: comments without `/AP`, and a Hidden one.
fn fixture() -> Vec<u8> {
    let w = |body: &str| format!("<< /Type /Annot /Subtype /Widget /F 4 /P 3 0 R {body} >>");
    let a = |body: &str| format!("<< /Type /Annot /P 13 0 R {body} >>");
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R] /DR << /Font << /Helv 4 0 R >> >> /DA (/Helv 0 Tf 0 g) >> >>".into(),
        "<< /Type /Pages /Kids [3 0 R 13 0 R] /Count 2 /MediaBox [0 0 400 300] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Annots [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R] >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
        w("/FT /Tx /T (city) /V (Lisbon) /Rect [20 250 200 272] /DA (/Helv 11 Tf 0 0 0.5 rg) /MK << /BG [1 1 0.85] /BC [0.5 0.5 0] >>"),
        w("/FT /Tx /Ff 4096 /T (notes) /V (Line one\\rLine two) /Rect [20 180 200 230] /DA (/Helv 10 Tf 0 g) /MK << /BC [0 0 0] >>"),
        w("/FT /Tx /Ff 16777216 /MaxLen 6 /T (code) /V (ABC123) /Rect [220 250 380 272] /DA (/Helv 12 Tf 0 g) /MK << /BC [0 0 0] >>"),
        w("/FT /Ch /T (letter) /Opt [(Alpha) (Beta) (Gamma)] /V (Beta) /Rect [220 150 380 230] /DA (/Helv 10 Tf 0 g) /MK << /BC [0 0 0] >>"),
        w("/FT /Btn /T (agree) /V /Yes /Rect [20 140 36 156] /MK << /BC [0 0 0] >>"),
        "<< /Type /Annot /Subtype /Widget /F 2 /P 3 0 R /FT /Tx /T (hidden) /V (x) /Rect [100 140 140 170] /MK << /BG [1 0 0] >> >>".into(),
        "<< /Type /Annot /Subtype /Widget /F 32 /P 3 0 R /FT /Tx /T (noview) /V (x) /Rect [150 140 190 170] /MK << /BG [1 0 0] >> >>".into(),
        w("/FT /Tx /T (up) /V (Up) /Rect [300 140 330 200] /DA (/Helv 9 Tf 0 g) /MK << /R 90 /BG [0 1 0] >>"),
        "<< /Type /Page /Parent 2 0 R /Annots [14 0 R 15 0 R 16 0 R 17 0 R 18 0 R 19 0 R 20 0 R 21 0 R 22 0 R 23 0 R] >>".into(),
        a("/Subtype /Square /F 4 /Rect [20 20 80 60] /C [0 0 1] /IC [0.8 0.9 1] /BS << /W 3 >>"),
        a("/Subtype /Circle /F 4 /Rect [100 20 160 60] /C [0 0.6 0] /BS << /W 2 >>"),
        a("/Subtype /Line /F 4 /Rect [175 25 285 55] /L [180 40 280 40] /LE [/None /OpenArrow] /C [0.6 0 0.6] /BS << /W 2 >>"),
        a("/Subtype /Underline /F 4 /Rect [20 104 140 120] /QuadPoints [20 120 140 120 20 104 140 104] /C [0 0 1]"),
        a("/Subtype /StrikeOut /F 4 /Rect [160 104 280 120] /QuadPoints [160 120 280 120 160 104 280 104] /C [1 0 0]"),
        a("/Subtype /Text /F 28 /Rect [300 20 320 40] /Name /Comment /C [1 0.9 0.3] /Contents (A sticky note)"),
        a("/Subtype /FreeText /F 4 /Rect [20 200 200 240] /DA (/Helv 10 Tf 1 0 0 rg) /Contents (Free text note) /C [1 1 0.9]"),
        a("/Subtype /Ink /F 4 /Rect [220 140 300 200] /InkList [[230 150 260 190 290 150]] /C [0.5 0 0.8] /BS << /W 3 >>"),
        a("/Subtype /Polygon /F 4 /Rect [320 140 395 200] /Vertices [325 145 390 145 380 195 330 190] /C [0 0.5 0.5] /IC [0.8 1 1] /BS << /W 1.5 >>"),
        a("/Subtype /Square /F 2 /Rect [340 240 380 280] /C [1 0 0] /IC [1 0 0]"),
    ])
}

fn render(bytes: &Arc<Vec<u8>>, page: usize, kind: RequestKind) -> RenderedPage {
    let mut r = PageRenderer::new(bytes.clone(), RenderConfig::default());
    let p = r.render(RenderRequest { page, kind, tile: None, scale: 1.0, tag: 0 });
    assert!(p.error.is_none(), "{:?}", p.error);
    p
}

struct Shot(RenderedPage);

impl Shot {
    fn px(&self, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * self.0.width + x) * 4) as usize;
        [self.0.rgba[i], self.0.rgba[i + 1], self.0.rgba[i + 2]]
    }

    fn near(&self, x: u32, y: u32, want: [u8; 3], tol: u8, what: &str) {
        let got = self.px(x, y);
        assert!(got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tol), "{what}: pixel ({x}, {y}) is {got:?}, expected about {want:?}");
    }

    /// Some pixel in columns `xs` and rows `ys` matches `pred`.
    fn any(&self, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>, pred: impl Fn([u8; 3]) -> bool) -> bool {
        ys.clone().any(|y| xs.clone().any(|x| pred(self.px(x, y))))
    }
}

const WHITE: [u8; 3] = [255, 255, 255];

fn dark(c: [u8; 3]) -> bool {
    c.iter().map(|v| u32::from(*v)).sum::<u32>() < 300
}

#[test]
fn widgets_without_appearances_are_drawn() {
    let display = display_bytes_for(&Arc::new(fixture()), None);
    let s = Shot(render(&display, 0, RequestKind::Pixels));
    // Text field: /MK background and border, and the value in the /DA colour.
    s.near(190, 39, [255, 255, 217], 8, "text field background");
    s.near(20, 39, [128, 128, 0], 40, "text field border");
    assert!(s.any(22..80, 30..50, |c| c[2] > 90 && c[0] < 90 && c[1] < 90), "the value 'Lisbon' in dark blue");
    // Multiline: two lines of text.
    assert!(s.any(24..120, 72..84, dark), "first line of the multiline field");
    assert!(s.any(24..120, 85..96, dark), "second line of the multiline field");
    s.near(20, 100, [0, 0, 0], 40, "multiline border");
    // Comb: one character in each of the six cells.
    let cell = 160.0 / 6.0;
    for i in 0..6u32 {
        let x0 = 220 + (cell * i as f64) as u32 + 4;
        let x1 = 220 + (cell * (i + 1) as f64) as u32 - 4;
        assert!(s.any(x0..x1, 30..48, dark), "comb cell {i} has its character");
    }
    // List box: the selected option highlighted, the options drawn.
    s.near(370, 90, [153, 191, 219], 12, "list box selection");
    assert!(s.any(224..280, 72..84, dark), "list box option text");
    // Check box: a check mark inside the border.
    assert!(s.any(23..34, 147..157, dark), "check box mark");
    // Hidden and NoView widgets stay undrawn.
    s.near(120, 145, WHITE, 0, "Hidden widget");
    s.near(170, 145, WHITE, 0, "NoView widget");
    // A field turned by /MK /R 90 still fills its rectangle.
    s.near(328, 102, [0, 255, 0], 8, "rotated field background");
    s.near(302, 102, [0, 255, 0], 8, "rotated field background");
}

#[test]
fn comments_without_appearances_are_drawn() {
    let display = display_bytes_for(&Arc::new(fixture()), None);
    let s = Shot(render(&display, 1, RequestKind::Pixels));
    let purple = |c: [u8; 3]| c[0] > 100 && c[2] > 100 && c[1] < 80;
    s.near(50, 260, [204, 230, 255], 8, "Square interior colour");
    s.near(21, 260, [0, 0, 255], 40, "Square border");
    assert!(s.any(99..103, 258..262, |c| c[1] > 100 && c[0] < 60 && c[2] < 60), "Circle stroke");
    s.near(130, 260, WHITE, 0, "Circle without interior colour");
    s.near(230, 259, [153, 0, 153], 40, "Line");
    assert!(s.any(262..279, 244..256, purple) && s.any(262..279, 264..276, purple), "Line's open arrow at its end");
    assert!(s.any(80..81, 190..199, |c| c[2] > 150 && c[0] < 120), "Underline");
    assert!(s.any(220..221, 185..194, |c| c[0] > 150 && c[1] < 120), "StrikeOut");
    s.near(310, 263, [255, 230, 77], 40, "Text note icon in its colour");
    s.near(190, 95, [255, 255, 230], 8, "FreeText background");
    assert!(s.any(22..120, 60..74, |c| c[0] > 150 && c[1] < 100 && c[2] < 100), "FreeText text in red");
    assert!(s.any(257..264, 108..114, purple), "Ink stroke through its apex");
    s.near(355, 130, [204, 255, 255], 8, "Polygon interior colour");
    s.near(360, 40, WHITE, 0, "Hidden comment");
}

#[test]
fn extracted_text_includes_drawn_values() {
    let display = display_bytes_for(&Arc::new(fixture()), None);
    let text = render(&display, 0, RequestKind::Text).text.map(|t| t.plain_text()).unwrap_or_default();
    for word in ["Lisbon", "Line one", "Line two", "Beta"] {
        assert!(text.contains(word), "{word:?} in {text:?}");
    }
    let text = render(&display, 1, RequestKind::Text).text.map(|t| t.plain_text()).unwrap_or_default();
    assert!(text.contains("Free text note"), "{text:?}");
}

#[test]
fn drawing_appearances_never_changes_the_file() {
    let original = Arc::new(fixture());
    let before = original.as_ref().clone();
    let display = display_bytes_for(&original, None);
    assert_eq!(*original, before, "the input bytes are untouched");
    assert_ne!(*display, before, "appearances were added to the display copy");
    assert!(display.starts_with(&before), "the display copy is an incremental update of the file");
    // A document opened in a session displays them, but its working file and Save are unchanged.
    let mut s = Session::new();
    let id = s.open("form.pdf", None, original.clone(), None).unwrap();
    let doc = s.get(id).unwrap();
    assert!(Arc::ptr_eq(&doc.bytes, &original));
    assert_eq!(*doc.display, *display, "the session displays the same appearances");
    assert_eq!(*s.save_bytes(id).unwrap(), before, "Save writes the file as it was");
    // Nothing to draw: the bytes come back as they are.
    let plain = Arc::new(pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 100 100] >>".into(),
        "<< /Type /Page /Parent 2 0 R >>".into(),
    ]));
    assert!(Arc::ptr_eq(&display_bytes_for(&plain, None), &plain));
    // Not a PDF: returned as is, no panic.
    let junk = Arc::new(b"not a pdf".to_vec());
    assert!(Arc::ptr_eq(&display_bytes_for(&junk, None), &junk));
}
