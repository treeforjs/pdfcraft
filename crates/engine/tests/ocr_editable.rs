//! Recognize text ▸ Editable Text and Images. Original synthetic fixtures: lines set in the
//! standard Helvetica, Times and Courier are rendered to a 300 ppi scan; a deterministic
//! recognizer that finds the words by their ink stands in for the models.
use std::cell::RefCell;
use std::sync::Arc;

use pdfcraft_engine::{
    DocId, Edit, Session,
    ocr::{EditableOptions, OcrPage, OcrReview, OcrSettings, Recognizer, SuspectReason},
};
use pdfcraft_ocr::{Line, OcrError, Word};
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind};

const W: f64 = 360.0;
const H: f64 = 200.0;
const SCALE: f64 = 300.0 / 72.0;

/// The fixture's lines, top to bottom.
const LINES: [&str; 4] = ["Editable text from scans", "Serif words in a line", "mono code here", "keep smudge safe"];

/// A one-page PDF with `content` and the three standard fonts as /F1 /F2 /F3.
fn pdf(content: &str) -> Arc<Vec<u8>> {
    let font = |base: &str| format!("<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding >>");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {W} {H}] /Contents 4 0 R /Resources << /Font << /F1 {} /F2 {} /F3 {} >> >> >>",
            font("Helvetica"),
            font("Times-Roman"),
            font("Courier")
        ),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    Arc::new(out)
}

/// The vector original: four lines (one in dark blue) and a green "picture" below them.
fn original() -> Arc<Vec<u8>> {
    pdf(&format!(
        "BT /F1 16 Tf 0 0 0 rg 20 160 Td ({}) Tj ET\nBT /F2 16 Tf 0.1 0.2 0.6 rg 20 125 Td ({}) Tj ET\n\
         BT /F3 14 Tf 0 0 0 rg 20 90 Td ({}) Tj ET\nBT /F1 16 Tf 0 0 0 rg 20 55 Td ({}) Tj ET\n0.3 0.6 0.3 rg 230 6 100 28 re f\n",
        LINES[0], LINES[1], LINES[2], LINES[3]
    ))
}

/// The original rendered at 300 ppi as an RGB PNG.
fn scan_png() -> (Vec<u8>, u32, u32) {
    let shot = PageRenderer::new(original(), RenderConfig::default()).render(RenderRequest { scale: SCALE as f32, ..Default::default() });
    assert!(shot.error.is_none(), "{:?}", shot.error);
    let rgb: Vec<u8> = shot.rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let img = image::RgbImage::from_raw(shot.width, shot.height, rgb).unwrap();
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    (png, shot.width, shot.height)
}

/// A scanned page: the rendering as the page's only image, at 300 ppi.
fn scan() -> (Session, DocId) {
    let (png, ..) = scan_png();
    let mut session = Session::new().with_clock(|| 1_700_000_000);
    let bytes = session.create_from_images_with_resolution(&[("scan.png".into(), png)], pdfcraft_engine::ImageResolution::Dpi(300.0)).unwrap();
    let id = session.open("scan.pdf", None, bytes, None).unwrap();
    (session, id)
}

fn luma(p: &[u8]) -> u32 {
    (77 * u32::from(p[0]) + 150 * u32::from(p[1]) + 29 * u32::from(p[2])) >> 8
}

/// Finds the words of [`LINES`] by their ink: rows with ink make lines, column gaps wider than
/// a quarter of the line's height separate words. Remembers the boxes it reported.
struct InkReader {
    boxes: RefCell<Vec<(String, [f32; 4])>>,
    calls: RefCell<usize>,
}

impl InkReader {
    fn new() -> Self {
        InkReader { boxes: RefCell::new(Vec::new()), calls: RefCell::new(0) }
    }
    fn word_box(&self, text: &str) -> [f32; 4] {
        self.boxes.borrow().iter().find(|(t, _)| t == text).map(|(_, r)| *r).unwrap()
    }
}

impl Recognizer for InkReader {
    fn recognize(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<Line>, OcrError> {
        *self.calls.borrow_mut() += 1;
        let channels = pdfcraft_ocr::validate_image(pixels, width, height)?;
        let (w, h) = (width as usize, height as usize);
        let ink = |x: usize, y: usize| {
            let p = &pixels[(y * w + x) * channels..][..channels];
            (if channels >= 3 { luma(p) } else { u32::from(p[0]) }) < 160
        };
        let mut bands = Vec::new();
        let mut start = None;
        for y in 0..=h {
            let any = y < h && (0..w).any(|x| ink(x, y));
            match (any, start) {
                (true, None) => start = Some(y),
                (false, Some(s)) => {
                    bands.push((s, y));
                    start = None;
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for ((t, b), text) in bands.into_iter().zip(LINES) {
            let gap = (b - t) / 4;
            let mut words = Vec::new();
            let (mut left, mut right, mut blank) = (None::<usize>, 0usize, 0usize);
            for x in 0..=w {
                let any = x < w && (t..b).any(|y| ink(x, y));
                if any {
                    if left.is_none() {
                        left = Some(x);
                    }
                    right = x + 1;
                    blank = 0;
                } else if let Some(l) = left {
                    blank += 1;
                    if blank > gap || x == w {
                        words.push([l as f32, t as f32, right as f32, b as f32]);
                        left = None;
                    }
                }
            }
            let texts: Vec<&str> = text.split(' ').collect();
            assert_eq!(words.len(), texts.len(), "words found in {text:?}: {words:?}");
            out.push(Line {
                words: texts
                    .iter()
                    .zip(words)
                    .map(|(t, rect)| {
                        self.boxes.borrow_mut().push(((*t).into(), rect));
                        Word { text: (*t).into(), rect, confidence: Some(if *t == "smudge" { 0.2 } else { 0.95 }) }
                    })
                    .collect(),
            });
        }
        Ok(out)
    }
}

fn editable() -> OcrSettings {
    OcrSettings { editable: Some(EditableOptions::default()), ..Default::default() }
}

fn draft(session: &Session, id: DocId, reader: &InkReader) -> OcrReview {
    session.ocr_job_checked(id, &[], editable()).unwrap().run(reader, |_, _| true).unwrap()
}

fn render(bytes: Arc<Vec<u8>>) -> (Vec<u8>, u32) {
    let shot = PageRenderer::new(bytes, RenderConfig::default()).render(RenderRequest { scale: SCALE as f32, ..Default::default() });
    assert!(shot.error.is_none(), "{:?}", shot.error);
    (shot.rgba.to_vec(), shot.width)
}

fn text(bytes: Arc<Vec<u8>>) -> String {
    let t = PageRenderer::new(bytes, RenderConfig::default()).render(RenderRequest { kind: RequestKind::Text, scale: 1.0, ..Default::default() });
    t.text.unwrap().plain_text().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Dark pixels (luma below 128) of an RGBA or RGB image `width` wide inside `rect`.
fn dark(px: &[u8], channels: usize, width: u32, rect: [f32; 4]) -> usize {
    let [l, t, r, b] = rect.map(|v| v as usize);
    (t..b).flat_map(|y| (l..r).map(move |x| (x, y))).filter(|(x, y)| luma(&px[(y * width as usize + x) * channels..]) < 128).count()
}

/// The page image's RGB samples after the edit.
fn image_rgb(session: &Session, id: DocId) -> (Vec<u8>, u32) {
    let (ext, bytes) = session.get(id).unwrap().page_image_file(0, 0).unwrap();
    assert_eq!(ext, "png");
    let img = image::load_from_memory(&bytes).unwrap().into_rgb8();
    (img.as_raw().clone(), img.width())
}

#[test]
fn editable_output_replaces_text_with_matching_visible_text_and_cleans_the_scan() {
    let (mut session, id) = scan();
    let before = session.get(id).unwrap().bytes.clone();
    let (scan_pixels, scan_width) = image_rgb(&session, id);
    let reader = InkReader::new();
    let review = draft(&session, id, &reader);
    let page = &review.pages()[0];
    let layout = page.editable.as_ref().unwrap();
    assert_eq!(layout.lines.len(), 4, "{layout:#?}");
    let fonts: Vec<&str> = layout.lines.iter().map(|l| l.font.base_font()).collect();
    assert_eq!(fonts, ["Helvetica", "Times-Roman", "Courier", "Helvetica"], "{layout:#?}");
    for (line, size) in layout.lines.iter().zip([16.0, 16.0, 14.0, 16.0]) {
        assert!((line.size - size).abs() <= size * 0.12, "size {} for {size}: {line:?}", line.size);
        assert!((line.origin[0] - 20.0).abs() < 2.0 && line.direction[0] > 0.999 && line.up[1] > 0.999, "{line:?}");
    }
    let [r, g, b] = layout.lines[1].color;
    assert!(b > r + 60 && b > g + 40, "the serif line is dark blue: {:?}", layout.lines[1].color);
    assert!(layout.lines[0].color.iter().all(|c| *c < 80), "{:?}", layout.lines[0].color);
    assert!(layout.lines.iter().all(|l| l.paper.iter().all(|c| *c > 240)));
    // The low-confidence word stays as image.
    let suspects: Vec<_> = layout.suspects.iter().map(|s| (page.words[s.word].text.as_str(), s.reason)).collect();
    assert_eq!(suspects, [("smudge", SuspectReason::LowConfidence)]);
    assert_eq!(session.get(id).unwrap().bytes, before, "a draft changes nothing");

    assert_eq!(session.apply_ocr_review(&review).unwrap(), 15);
    let doc = session.get(id).unwrap();
    assert_eq!(doc.can_undo(), Some("Recognize text"));
    let after = doc.bytes.clone();
    // Visible text runs whose extracted text matches, in the chosen fonts.
    assert_eq!(text(after.clone()), LINES.join(" "));
    let runs = doc.text_lines(0);
    for (line, font) in LINES.iter().take(3).zip(["Helvetica", "Times-Roman", "Courier"]) {
        assert!(runs.iter().any(|r| r.text.trim() == *line && r.base_font == font), "{line} in {font}: {runs:#?}");
    }
    // The low-confidence word is invisible text in its place in the line (its own run).
    let last: Vec<&str> = runs.iter().skip(3).map(|r| r.text.trim()).collect();
    assert_eq!(last, ["keep", "smudge", "safe"], "{runs:#?}");
    // The background image no longer holds the ink of the converted words; the low-confidence
    // word and the picture are untouched.
    let (cleaned, width) = image_rgb(&session, id);
    assert_eq!(width, scan_width);
    for word in LINES.iter().flat_map(|l| l.split(' ')).filter(|w| *w != "smudge") {
        let rect = reader.word_box(word);
        assert!(dark(&scan_pixels, 3, width, rect) > 20, "{word} had ink");
        assert_eq!(dark(&cleaned, 3, width, rect), 0, "{word} was cleaned out of the image");
    }
    let smudge = reader.word_box("smudge");
    assert_eq!(dark(&cleaned, 3, width, smudge), dark(&scan_pixels, 3, width, smudge));
    let picture = [(230.0 * SCALE) as f32 + 4.0, ((H - 34.0) * SCALE) as f32 + 4.0, (330.0 * SCALE) as f32 - 4.0, ((H - 6.0) * SCALE) as f32 - 4.0];
    let [l, t, r, b] = picture.map(|v| v as usize);
    for y in t..b {
        let row = (y * width as usize + l) * 3..(y * width as usize + r) * 3;
        assert_eq!(cleaned[row.clone()], scan_pixels[row], "picture row {y} kept");
    }
    // The page still shows the words: now drawn as text.
    let (shown, shown_width) = render(after.clone());
    for word in ["Editable", "Serif", "mono", "safe"] {
        let rect = reader.word_box(word);
        assert!(dark(&shown, 4, shown_width, rect) * 2 > dark(&scan_pixels, 3, width, rect), "{word} is visible");
    }

    // One undo restores the page exactly.
    session.undo(id).unwrap();
    assert_eq!(session.get(id).unwrap().bytes, before);
    assert_eq!(image_rgb(&session, id).0, scan_pixels);
    assert_eq!(session.get(id).unwrap().can_undo(), None);
    // And the result saves, reopens and stays editable text.
    session.redo(id).unwrap();
    let saved = session.save_bytes(id).unwrap();
    let reopened = session.open("saved.pdf", None, saved, None).unwrap();
    assert_eq!(text(session.get(reopened).unwrap().bytes.clone()), LINES.join(" "));
}

#[test]
fn converted_lines_can_be_edited_like_normal_text() {
    let (mut session, id) = scan();
    let review = draft(&session, id, &InkReader::new());
    session.apply_ocr_review(&review).unwrap();
    let line = session.get(id).unwrap().text_lines(0).iter().position(|l| l.text.trim() == LINES[1]).unwrap();
    session.apply(id, Edit::EditTextLine { page: 0, line, text: "Serif words edited".into() }).unwrap();
    let t = text(session.get(id).unwrap().bytes.clone());
    assert!(t.contains("Serif words edited") && !t.contains(LINES[1]), "{t}");
}

#[test]
fn corrections_apply_to_visible_text_and_emptied_words_stay_as_image() {
    let (mut session, id) = scan();
    let reader = InkReader::new();
    let mut review = draft(&session, id, &reader);
    let words: Vec<String> = review.pages()[0].words.iter().map(|w| w.text.clone()).collect();
    let scans = words.iter().position(|w| w == "scans").unwrap();
    let from = words.iter().position(|w| w == "from").unwrap();
    review.correct(0, scans, "scan").unwrap();
    review.correct(0, from, "").unwrap();
    let (before, width) = image_rgb(&session, id);
    session.apply_ocr_review(&review).unwrap();
    let t = text(session.get(id).unwrap().bytes.clone());
    assert!(t.starts_with("Editable text scan "), "{t}");
    let (after, _) = image_rgb(&session, id);
    let rect = reader.word_box("from");
    assert_eq!(dark(&after, 3, width, rect), dark(&before, 3, width, rect), "an emptied word keeps its ink");
}

#[test]
fn min_confidence_zero_converts_every_word() {
    let (session, id) = scan();
    let settings = OcrSettings { editable: Some(EditableOptions { min_confidence: 0.0 }), ..Default::default() };
    let review = session.ocr_job_checked(id, &[], settings).unwrap().run(&InkReader::new(), |_, _| true).unwrap();
    let layout = review.pages()[0].editable.as_ref().unwrap();
    assert!(layout.suspects.is_empty(), "{:?}", layout.suspects);
    assert_eq!(layout.lines.iter().map(|l| l.words.len()).sum::<usize>(), 15);
}

#[test]
fn bad_editable_settings_are_refused_before_rendering() {
    let (session, id) = scan();
    for settings in [
        OcrSettings { editable: Some(EditableOptions { min_confidence: f32::NAN }), ..Default::default() },
        OcrSettings { editable: Some(EditableOptions { min_confidence: -0.1 }), ..Default::default() },
        OcrSettings { editable: Some(EditableOptions { min_confidence: 1.5 }), ..Default::default() },
        OcrSettings { editable: Some(EditableOptions::default()), straighten: true, ..Default::default() },
    ] {
        assert!(session.ocr_job_checked(id, &[], settings.clone()).is_err(), "{settings:?}");
    }
}

/// Returns whatever it was built with, however hostile.
struct Hostile(Vec<Word>);
impl Recognizer for Hostile {
    fn recognize(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<Line>, OcrError> {
        pdfcraft_ocr::validate_image(pixels, width, height)?;
        Ok(vec![Line { words: self.0.clone() }])
    }
}

#[test]
fn hostile_recognition_results_and_layouts_do_not_panic_or_change_the_document() {
    let w = |text: &str, rect: [f32; 4], confidence: Option<f32>| Word { text: text.into(), rect, confidence };
    let words = vec![
        w("nan", [f32::NAN, 0.0, 10.0, 10.0], None),
        w("inf", [0.0, 0.0, f32::INFINITY, 10.0], Some(f32::INFINITY)),
        w("huge", [-1e30, -1e30, 1e30, 1e30], Some(2.0)),
        w("flipped", [100.0, 100.0, 10.0, 10.0], Some(-1.0)),
        w("zero", [50.0, 50.0, 50.0, 50.0], Some(f32::NAN)),
        w("Editable", [0.0, 0.0, 1500.0, 900.0], Some(1.0)),
        w("Editable", [0.0, 0.0, 1500.0, 900.0], Some(1.0)),
        w("漢字", [80.0, 60.0, 200.0, 100.0], Some(1.0)),
        w("", [80.0, 60.0, 200.0, 100.0], Some(1.0)),
        w("edge", [1490.0, 820.0, 1600.0, 900.0], Some(1.0)),
    ];
    let (mut session, id) = scan();
    let before = session.get(id).unwrap().bytes.clone();
    let result = session.ocr_job_checked(id, &[], editable()).unwrap().run(&Hostile(words), |_, _| true);
    if let Ok(review) = result {
        // Applying either works or is refused; it never panics.
        let _ = session.apply_ocr_review(&review);
        let _ = session.undo(id);
    }
    assert_eq!(session.get(id).unwrap().bytes, before);

    // Hand-made layouts with bad indexes or geometry are refused before any change.
    let review = draft(&session, id, &InkReader::new());
    let good = review.pages()[0].clone();
    let mut bad_pages: Vec<OcrPage> = Vec::new();
    let mut p = good.clone();
    p.editable.as_mut().unwrap().lines[0].words[0].index = 10_000;
    bad_pages.push(p);
    let mut p = good.clone();
    p.editable.as_mut().unwrap().lines[0].origin = [f64::NAN, 0.0];
    bad_pages.push(p);
    let mut p = good.clone();
    p.editable.as_mut().unwrap().lines[0].size = 1e9;
    bad_pages.push(p);
    let mut p = good.clone();
    p.editable.as_mut().unwrap().lines[0].direction = [0.0, 0.0];
    bad_pages.push(p);
    let mut p = good.clone();
    p.editable.as_mut().unwrap().lines[0].words[0].region = [[f64::INFINITY, 0.0]; 4];
    bad_pages.push(p);
    let mut p = good.clone();
    p.editable.as_mut().unwrap().suspects[0].word = usize::MAX;
    bad_pages.push(p);
    let mut p = good;
    p.skipped = Some("skipped".into());
    p.words.clear();
    bad_pages.push(p);
    for bad in bad_pages {
        assert!(session.apply_ocr(id, &[bad]).is_err());
        assert_eq!(session.get(id).unwrap().bytes, before);
        assert_eq!(session.get(id).unwrap().can_undo(), None);
    }
}

#[test]
fn text_over_vector_artwork_is_covered_with_paper_colour() {
    // No image to clean: the recognized words lie over vector text, which is painted over.
    let mut session = Session::new().with_clock(|| 1_700_000_000);
    let id = session.open("vector.pdf", None, original(), None).unwrap();
    let settings = OcrSettings { skip_text_pages: false, ..editable() };
    let review = session.ocr_job_checked(id, &[], settings).unwrap().run(&InkReader::new(), |_, _| true).unwrap();
    session.apply_ocr_review(&review).unwrap();
    let lines = session.get(id).unwrap().text_lines(0);
    assert!(lines.iter().filter(|l| l.text.trim() == LINES[1]).count() >= 2, "the original and the new line: {lines:#?}");
    let (shown, width) = render(session.get(id).unwrap().bytes.clone());
    // The words are still visible (drawn once, over the paper-coloured covers).
    assert!(dark(&shown, 4, width, [80.0, 80.0, 1400.0, 820.0]) > 1000);
}
