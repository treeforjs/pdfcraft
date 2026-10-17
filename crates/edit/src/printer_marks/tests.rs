use std::sync::Arc;

use pdfcraft_cos::{Document, Object, SaveOptions, write_incremental};

use super::*;

/// Page 1 upright 600×800 with text; page 2 the same turned 90°; page 3 with a trim and a wider
/// bleed (9 pt) in an inherited, shared resource dictionary; page 4 with a /UserUnit of 2.
fn fixture() -> Document {
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 9 0 R] /Count 4 /MediaBox [0 0 600 800] /Resources 6 0 R >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>".into(),
        "<< /Type /Page /Parent 2 0 R /Rotate 90 /Contents [7 0 R] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 7 0 R /TrimBox [20 20 580 780] /BleedBox [11 11 589 789] >>".into(),
        "<< /Font << /F1 8 0 R >> >>".into(),
        "<< /Length 37 >>\nstream\n2 0 0 2 0 0 cm BT /F1 9 Tf (Hi) Tj ET\nendstream".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 7 0 R /UserUnit 2 /MediaBox [0 0 300 400] >>".into(),
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
    Document::open(Arc::new(out)).unwrap()
}

fn page_rect(doc: &Document, page: usize, key: &[u8]) -> Option<[f64; 4]> {
    let p = &pdfcraft_model::pages(doc)[page];
    rect(doc, p.dict.get(key))
}

/// The decoded printer-marks stream of a page, if any (and how many there are).
fn marks_stream(doc: &Document, page: usize) -> (Option<String>, usize) {
    let p = &pdfcraft_model::pages(doc)[page];
    let list = match p.dict.get(b"Contents").map(|c| doc.resolve(c)).as_deref() {
        Some(Object::Array(a)) => a.clone(),
        _ => vec![],
    };
    let ours: Vec<String> = list
        .iter()
        .filter(|o| stream_tag(doc, o).as_deref() == Some(TAG))
        .map(|o| match &*doc.resolve(o) {
            Object::Stream(s) => String::from_utf8(s.decoded().unwrap()).unwrap(),
            _ => panic!("a stream"),
        })
        .collect();
    (ours.first().cloned(), ours.len())
}

fn mul(m: [f64; 6], t: [f64; 6]) -> [f64; 6] {
    [
        m[0] * t[0] + m[1] * t[2],
        m[0] * t[1] + m[1] * t[3],
        m[2] * t[0] + m[3] * t[2],
        m[2] * t[1] + m[3] * t[3],
        m[4] * t[0] + m[5] * t[2] + t[4],
        m[4] * t[1] + m[5] * t[3] + t[5],
    ]
}

/// Every point the stream's paths go through, in user space, with the operator that used it.
fn points(content: &str) -> Vec<([f64; 2], String)> {
    let mut ctm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut nums: Vec<f64> = Vec::new();
    let mut out = Vec::new();
    let map = |ctm: [f64; 6], x: f64, y: f64| [ctm[0] * x + ctm[2] * y + ctm[4], ctm[1] * x + ctm[3] * y + ctm[5]];
    for tok in content.split_whitespace() {
        if let Ok(v) = tok.parse::<f64>() {
            nums.push(v);
            continue;
        }
        let k = nums.len();
        match tok {
            "cm" if k >= 6 => {
                let m: [f64; 6] = nums[k - 6..].try_into().unwrap();
                ctm = mul(m, ctm);
            }
            "m" | "l" if k >= 2 => out.push((map(ctm, nums[k - 2], nums[k - 1]), tok.to_string())),
            "c" if k >= 6 => {
                for i in 0..3 {
                    out.push((map(ctm, nums[k - 6 + 2 * i], nums[k - 5 + 2 * i]), tok.to_string()));
                }
            }
            "re" if k >= 4 => {
                let [x, y, w, h] = [nums[k - 4], nums[k - 3], nums[k - 2], nums[k - 1]];
                for (px, py) in [(x, y), (x + w, y), (x, y + h), (x + w, y + h)] {
                    out.push((map(ctm, px, py), tok.to_string()));
                }
            }
            _ => {}
        }
        nums.clear();
    }
    out
}

fn inside(p: [f64; 2], r: [f64; 4]) -> bool {
    p[0] > r[0] + 1e-6 && p[0] < r[2] - 1e-6 && p[1] > r[1] + 1e-6 && p[1] < r[3] - 1e-6
}

fn within(p: [f64; 2], r: [f64; 4]) -> bool {
    p[0] >= r[0] - 1e-6 && p[0] <= r[2] + 1e-6 && p[1] >= r[1] - 1e-6 && p[1] <= r[3] + 1e-6
}

fn close(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
}

#[test]
fn marks_are_drawn_outside_the_trim_and_the_media_box_grows() {
    let mut doc = fixture();
    let done = add_printer_marks(&mut doc, &[0], &PrinterMarks::default(), (2026, 10, 17)).unwrap();
    // No trim box: the crop (= media) box is the trim; 6 pt offset + 18 pt marks + 0.25 pt line.
    let m = 6.0 + MARK_LENGTH + 0.25;
    assert_eq!(done.len(), 1);
    assert!((done[0].margin - m).abs() < 1e-9);
    let media = [-m, -m, 600.0 + m, 800.0 + m];
    assert!(close(page_rect(&doc, 0, b"MediaBox").unwrap(), media));
    assert!(close(page_rect(&doc, 0, b"CropBox").unwrap(), media));
    assert!(close(page_rect(&doc, 0, b"TrimBox").unwrap(), [0.0, 0.0, 600.0, 800.0]));
    assert!(close(page_rect(&doc, 0, b"BleedBox").unwrap(), [0.0, 0.0, 600.0, 800.0]));
    let (content, count) = marks_stream(&doc, 0);
    assert_eq!(count, 1);
    let content = content.unwrap();
    assert!(content.contains("/OC /PCMarksOC BDC"), "{content}");
    assert!(content.contains("/Artifact <</Type /Page"), "{content}");
    assert!(content.contains("/PCAll CS"), "registration colour");
    assert!(content.contains(" k "), "colour bars");
    let pts = points(&content);
    assert!(pts.len() > 50, "trim marks, targets, bars and outlined information");
    for (p, op) in &pts {
        assert!(!inside(*p, [0.0, 0.0, 600.0, 800.0]), "{op} at {p:?} is inside the trim");
        assert!(within(*p, media), "{op} at {p:?} is outside the media box");
    }
    // Trim marks: on the trim lines, starting 6 pt out.
    assert!(content.contains("-6 0 m -24 0 l"), "{content}");
    // Colour bars above the trim, page information below it.
    assert!(pts.iter().filter(|(_, op)| op == "re").all(|(p, _)| p[1] > 800.0));
    // The original content is wrapped so its graphics state can't leak into the marks.
    let p = &pdfcraft_model::pages(&doc)[0];
    assert!(matches!(p.dict.get(b"Contents"), Some(Object::Array(a)) if a.len() == 4));
    // The layer is registered once, on for viewing and printing.
    let root = doc.root().unwrap();
    let cat = doc.get(root).as_dict().cloned().unwrap();
    let props = doc.resolve(cat.get(b"OCProperties").unwrap()).as_dict().cloned().unwrap();
    let ocgs = doc.resolve(props.get(b"OCGs").unwrap()).as_array().cloned().unwrap();
    assert_eq!(ocgs.len(), 1);
    let ocg = doc.resolve(&ocgs[0]).as_dict().cloned().unwrap();
    assert_eq!(ocg.get(b"Name").and_then(|n| n.as_string()).map(|s| s.to_text()).as_deref(), Some(LAYER_NAME));
}

#[test]
fn the_bleed_pushes_the_marks_out_and_the_trim_box_is_used() {
    let mut doc = fixture();
    add_printer_marks(&mut doc, &[2], &PrinterMarks::default(), (2026, 1, 2)).unwrap();
    // Bleed 9 pt is wider than the 6 pt offset: marks start 9 pt from the trim.
    let m = 9.0 + MARK_LENGTH + 0.25;
    let trim = [20.0, 20.0, 580.0, 780.0];
    assert!(close(page_rect(&doc, 2, b"MediaBox").unwrap(), [20.0 - m, 20.0 - m, 580.0 + m, 780.0 + m]));
    assert!(close(page_rect(&doc, 2, b"TrimBox").unwrap(), trim));
    assert!(close(page_rect(&doc, 2, b"BleedBox").unwrap(), [11.0, 11.0, 589.0, 789.0]));
    let content = marks_stream(&doc, 2).0.unwrap();
    for (p, op) in points(&content) {
        assert!(!inside(p, [11.0, 11.0, 589.0, 789.0]), "{op} at {p:?} prints into the bleed");
    }
    // Bleed marks on the bleed edge (9 pt below the trim, in the frame whose origin is the trim corner).
    assert!(content.contains("-9 -9 m -18 -9 l"), "{content}");
    // The other pages, sharing the inherited resources, are untouched.
    assert!(marks_stream(&doc, 0).0.is_none());
    assert!(page_rect(&doc, 0, b"TrimBox").is_none());
}

#[test]
fn rotated_pages_get_their_bars_on_top_as_displayed() {
    let mut doc = fixture();
    add_printer_marks(&mut doc, &[1], &PrinterMarks::default(), (2026, 1, 2)).unwrap();
    let m = 6.0 + MARK_LENGTH + 0.25;
    assert!(close(page_rect(&doc, 1, b"MediaBox").unwrap(), [-m, -m, 600.0 + m, 800.0 + m]));
    let content = marks_stream(&doc, 1).0.unwrap();
    assert!(content.starts_with("q\n0 1 -1 0 600 0 cm"), "{content}");
    let pts = points(&content);
    // Turned 90° clockwise, the displayed top is the user-space left edge.
    let bars: Vec<_> = pts.iter().filter(|(_, op)| op == "re").collect();
    assert!(!bars.is_empty());
    assert!(bars.iter().all(|(p, _)| p[0] < 0.0), "{bars:?}");
    for (p, op) in &pts {
        assert!(!inside(*p, [0.0, 0.0, 600.0, 800.0]), "{op} at {p:?}");
    }
}

#[test]
fn user_units_scale_the_marks_to_points() {
    let mut doc = fixture();
    add_printer_marks(&mut doc, &[3], &PrinterMarks::default(), (2026, 1, 2)).unwrap();
    // 24.25 pt is 12.125 user units of 2 pt.
    let m = (6.0 + MARK_LENGTH + 0.25) / 2.0;
    assert!(close(page_rect(&doc, 3, b"MediaBox").unwrap(), [-m, -m, 300.0 + m, 400.0 + m]));
    let content = marks_stream(&doc, 3).0.unwrap();
    assert!(content.contains("0.5 0 0 0.5 0 0 cm"), "{content}");
}

#[test]
fn adding_marks_again_replaces_them() {
    let mut doc = fixture();
    add_printer_marks(&mut doc, &[0], &PrinterMarks::default(), (2026, 1, 2)).unwrap();
    let first = page_rect(&doc, 0, b"MediaBox").unwrap();
    let only_trim = PrinterMarks { bleed: false, registration: false, color_bars: false, page_info: false, ..Default::default() };
    add_printer_marks(&mut doc, &[0, 0], &only_trim, (2026, 1, 2)).unwrap();
    let (content, count) = marks_stream(&doc, 0);
    assert_eq!(count, 1);
    assert!(!content.unwrap().contains(" k "), "the colour bars went with the old marks");
    // Same trim, same margin: the media box doesn't grow again.
    assert!(close(page_rect(&doc, 0, b"MediaBox").unwrap(), first));
    let root = doc.root().unwrap();
    let cat = doc.get(root).as_dict().cloned().unwrap();
    let props = doc.resolve(cat.get(b"OCProperties").unwrap()).as_dict().cloned().unwrap();
    assert_eq!(doc.resolve(props.get(b"OCGs").unwrap()).as_array().unwrap().len(), 1, "one layer");
}

#[test]
fn eastern_style_draws_double_corner_lines_and_centre_crosses() {
    let mut doc = fixture();
    let marks = PrinterMarks { style: MarkStyle::Eastern, color_bars: false, page_info: false, ..Default::default() };
    add_printer_marks(&mut doc, &[0], &marks, (2026, 1, 2)).unwrap();
    let content = marks_stream(&doc, 0).0.unwrap();
    // No bleed box: tombo use a 3 mm bleed, and the marks start beyond it.
    let b = n(EASTERN_BLEED);
    assert!(content.contains(&format!("-{b} -{b} m")), "{content}");
    assert!(!content.contains(" c "), "no round targets");
    let m = EASTERN_BLEED + MARK_LENGTH + 0.25;
    assert!(close(page_rect(&doc, 0, b"MediaBox").unwrap(), [-m, -m, 600.0 + m, 800.0 + m]));
}

#[test]
fn marks_survive_save_and_reopen() {
    let mut doc = fixture();
    add_printer_marks(&mut doc, &[0, 2], &PrinterMarks { file_name: "Brochure.pdf".into(), ..Default::default() }, (2026, 1, 2)).unwrap();
    let bytes = write_incremental(&doc, &SaveOptions::default()).unwrap();
    hayro_syntax::Pdf::new(bytes.clone()).expect("parses");
    let again = Document::open(Arc::new(bytes)).unwrap();
    assert!(marks_stream(&again, 0).0.unwrap().contains("/Artifact <</Type /Page"));
    assert!(marks_stream(&again, 2).0.is_some());
    assert_eq!(page_rect(&again, 0, b"MediaBox"), page_rect(&doc, 0, b"MediaBox"));
}

#[test]
fn bad_settings_and_pages_change_nothing() {
    let doc = fixture();
    let bad = [
        PrinterMarks { line_weight: 0.0, ..Default::default() },
        PrinterMarks { line_weight: f64::NAN, ..Default::default() },
        PrinterMarks { line_weight: 5.0, ..Default::default() },
        PrinterMarks { offset: -1.0, ..Default::default() },
        PrinterMarks { offset: f64::INFINITY, ..Default::default() },
        PrinterMarks { offset: 100.0, ..Default::default() },
        PrinterMarks { trim: false, bleed: false, registration: false, color_bars: false, page_info: false, ..Default::default() },
        PrinterMarks { file_name: "x".repeat(MAX_FILE_NAME_CHARS + 1), ..Default::default() },
        PrinterMarks { file_name: "a\nb".into(), ..Default::default() },
    ];
    for marks in bad {
        let mut d = doc.clone();
        assert!(add_printer_marks(&mut d, &[0], &marks, (2026, 1, 2)).is_err(), "{marks:?}");
        assert!(!d.is_modified());
    }
    let mut d = doc.clone();
    assert_eq!(add_printer_marks(&mut d, &[0, 9], &PrinterMarks::default(), (2026, 1, 2)), Err(EditError::NoSuchPage(9)));
    assert!(add_printer_marks(&mut d, &[], &PrinterMarks::default(), (2026, 1, 2)).is_err());
    assert!(!d.is_modified());
}

#[test]
fn a_page_at_the_size_limit_is_refused_whole() {
    let mut doc = fixture();
    let p = pdfcraft_model::pages(&doc)[2].obj;
    doc.update_dict(p, |d| {
        d.set(b"MediaBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(0), Object::Int(14_400), Object::Int(100)]));
        d.remove(b"TrimBox");
        d.remove(b"BleedBox");
    })
    .unwrap();
    let before = doc.clone();
    let err = add_printer_marks(&mut doc, &[0, 2], &PrinterMarks::default(), (2026, 1, 2)).unwrap_err();
    assert!(err.to_string().contains("page 3"), "{err}");
    // Page 1 was fine but nothing changed: all or nothing.
    assert_eq!(page_rect(&doc, 0, b"MediaBox"), page_rect(&before, 0, b"MediaBox"));
    assert!(marks_stream(&doc, 0).0.is_none());
}

#[test]
fn hostile_boxes_fall_back_without_panicking() {
    let mut doc = fixture();
    let p = pdfcraft_model::pages(&doc)[0].obj;
    doc.update_dict(p, |d| {
        d.set(b"TrimBox".to_vec(), Object::Array(vec![Object::Real(f64::NAN), Object::Int(0)]));
        d.set(b"BleedBox".to_vec(), Object::Array(vec![Object::Int(5000), Object::Int(5000), Object::Int(6000), Object::Int(6000)]));
        d.set(b"Rotate".to_vec(), Object::Int(-450));
        d.set(b"UserUnit".to_vec(), Object::Real(-3.0));
    })
    .unwrap();
    add_printer_marks(&mut doc, &[0], &PrinterMarks::default(), (2026, 1, 2)).unwrap();
    // A broken trim box falls back to the crop box; a bleed box outside the media box to the trim.
    assert!(close(page_rect(&doc, 0, b"TrimBox").unwrap(), [0.0, 0.0, 600.0, 800.0]));
}
