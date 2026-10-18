use std::sync::Arc;

use super::*;

fn stream(extra: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} {extra} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A one-page document: object 3 is the page, 4 its content; `extra` objects follow from 5.
fn doc_with(page: &str, resources: &str, content: &[u8], extra: &[Vec<u8>]) -> Document {
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] {page} /Resources {resources} /Contents 4 0 R >>").into_bytes(),
        stream("", content),
    ];
    objs.extend(extra.iter().cloned());
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
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

fn doc(content: &str) -> Document {
    doc_with("", "<< >>", content.as_bytes(), &[])
}

fn run(d: &Document) -> Report {
    check(d, &Options::default()).unwrap()
}

fn of(r: &Report, kind: Kind) -> Vec<&Finding> {
    r.findings.iter().filter(|f| f.kind == kind).collect()
}

const FONT: &str = "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>";

#[test]
fn a_clean_page_has_no_findings() {
    let d = doc_with(
        "",
        &format!("<< {FONT} >>"),
        b"0 0 0 1 k 0 0 100 100 re f 0.2 0.3 0.4 0.5 K 10 10 m 200 10 l S BT /F1 9 Tf 72 700 Td (Black text) Tj ET",
        &[],
    );
    let r = run(&d);
    assert!(r.findings.is_empty(), "{:?}", r.findings);
    assert_eq!(r.counts.len(), Kind::ALL.len());
    assert!(r.counts.values().all(|n| *n == 0));
    assert!(!r.findings_truncated && r.limits.is_empty());
    assert_eq!(r.pages, vec![0]);
    assert_eq!(r.warnings(), 0);
}

#[test]
fn overprint_states_and_overprinted_paint_are_reported() {
    let d = doc_with(
        "",
        "<< /ExtGState << /GS1 5 0 R /Off << /OP false >> >> >>",
        b"/GS1 gs 0 1 0 0 k 10 10 50 50 re f 0 1 0 0 k 100 100 20 20 re f /Off gs 0 1 0 0 k 300 300 10 10 re f",
        &[b"<< /Type /ExtGState /OP true /OPM 1 >>".to_vec()],
    );
    let r = run(&d);
    let state = of(&r, Kind::OverprintState);
    assert_eq!(state.len(), 1);
    assert_eq!(state[0].object, Some(ObjRef::new(5, 0)));
    assert_eq!(state[0].overprint_mode, Some(1));
    assert!(state[0].message.contains("fills (op)") && state[0].message.contains("strokes (OP)"), "{}", state[0].message);
    let paint = of(&r, Kind::OverprintPaint);
    assert_eq!(paint.len(), 1, "{paint:?}");
    // Two fills with the same colour and state are one finding; the one after /Off isn't.
    assert_eq!(paint[0].occurrences, 2);
    assert_eq!(paint[0].bbox, Some([10.0, 10.0, 120.0, 120.0]));
    assert_eq!(paint[0].object, Some(ObjRef::new(3, 0)));
    assert_eq!(paint[0].colour.as_ref().unwrap().cmyk, Some([0.0, 100.0, 0.0, 0.0]));
    assert_eq!(r.count(Kind::OverprintWhite), 0);
    assert_eq!(r.occurrences.get("overprint-paint"), Some(&2));
}

#[test]
fn op_only_sets_stroke_overprint_when_fill_is_given() {
    let d = doc_with("", "<< /ExtGState << /S << /OP true /op false >> >> >>", b"/S gs 0 0 0 0 k 0 0 0 0 K 10 10 50 50 re B", &[]);
    let r = run(&d);
    let white = of(&r, Kind::OverprintWhite);
    assert_eq!(white.len(), 1);
    assert_eq!(white[0].paint, Some(PaintKind::Stroke));
    // OPM 0: DeviceCMYK white knocks out per the standard, so the message hedges.
    assert!(white[0].message.contains("knocks out"), "{}", white[0].message);
}

#[test]
fn white_objects_set_to_overprint_are_warnings() {
    let d = doc_with(
        "",
        &format!("<< {FONT} /ExtGState << /OP << /OP true /OPM 1 >> >> /ColorSpace << /Spot [/Separation /Gold /DeviceCMYK 5 0 R] >> >>"),
        b"/OP gs 0 0 0 0 k 0 0 100 100 re f 1 g 200 200 10 10 re f /Spot cs 0 scn 300 300 5 5 re f BT /F1 10 Tf 0 0 0 0 k 50 50 Td (White text) Tj ET",
        &[b"<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 0.9 0] /N 1 >>".to_vec()],
    );
    let r = run(&d);
    let white = of(&r, Kind::OverprintWhite);
    assert_eq!(white.len(), 4, "{white:?}");
    assert!(white.iter().all(|f| f.severity == Severity::Warning));
    let cmyk = white.iter().find(|f| f.paint == Some(PaintKind::Fill) && f.colour.as_ref().unwrap().space == "DeviceCMYK").unwrap();
    assert!(cmyk.message.contains("disappears"), "{}", cmyk.message);
    let spot = white.iter().find(|f| f.colour.as_ref().unwrap().space == "Separation /Gold").unwrap();
    assert!(spot.message.contains("disappears"));
    let gray = white.iter().find(|f| f.colour.as_ref().unwrap().space == "DeviceGray").unwrap();
    assert!(gray.message.contains("knocks out"));
    assert!(white.iter().any(|f| f.paint == Some(PaintKind::Text)));
    assert_eq!(r.count(Kind::OverprintPaint), 0);
    assert_eq!(r.warnings(), 4);
}

#[test]
fn rich_black_is_reported_with_its_coverage() {
    let d = doc("0.6 0.4 0.4 1 k 0 0 100 100 re f 0.05 0 0 1 k 0 0 10 10 re f 0.6 0.6 0.6 0.9 k 0 0 10 10 re f 0.5 0.4 0.3 0.95 K 0 0 m 10 10 l S");
    let r = run(&d);
    let rich = of(&r, Kind::RichBlack);
    assert_eq!(rich.len(), 2, "{rich:?}");
    let fill = rich.iter().find(|f| f.paint == Some(PaintKind::Fill)).unwrap();
    let c = fill.colour.as_ref().unwrap();
    assert_eq!(c.cmyk, Some([60.0, 40.0, 40.0, 100.0]));
    assert_eq!(c.total(), Some(240.0));
    assert!(!c.via_alternate);
    assert!(fill.message.contains("240% ink"), "{}", fill.message);
    assert_eq!(fill.severity, Severity::Info);
    // K exactly at the threshold counts.
    assert!(rich.iter().any(|f| f.paint == Some(PaintKind::Stroke)));
    // Thresholds are parameters.
    let strict = check(&d, &Options { rich_black_k: 90.0, rich_black_cmy: 5.0, ..Options::default() }).unwrap();
    assert_eq!(strict.count(Kind::RichBlack), 4);
}

#[test]
fn small_rich_black_text_uses_the_effective_size() {
    let d = doc_with(
        "",
        &format!("<< {FONT} >>"),
        b"0.5 0.5 0.5 1 k BT /F1 8 Tf 72 700 Td (small) Tj ET \
          BT /F1 8 Tf 2 0 0 2 72 600 Tm (scaled up) Tj ET \
          q 0.5 0 0 0.5 0 0 cm BT /F1 20 Tf 72 500 Td [(scaled) -100 (down)] TJ ET Q \
          BT /F1 8 Tf 3 Tr 72 400 Td (invisible) Tj ET",
        &[],
    );
    let r = run(&d);
    let small = of(&r, Kind::RichBlackSmallText);
    let sizes: Vec<f64> = small.iter().filter_map(|f| f.text_size).collect();
    assert_eq!(sizes, vec![8.0, 10.0], "{small:?}");
    assert!(small.iter().all(|f| f.severity == Severity::Warning && f.paint == Some(PaintKind::Text)));
    assert!(small[0].message.contains("registration risk"));
    let big = of(&r, Kind::RichBlack);
    assert_eq!(big.len(), 1);
    assert_eq!(big[0].text_size, Some(16.0));
    assert_eq!(big[0].paint, Some(PaintKind::Text));
    // A lower small-text size turns the 10 pt text into plain rich black.
    let r = check(&d, &Options { small_text_pt: 9.0, ..Options::default() }).unwrap();
    assert_eq!(r.count(Kind::RichBlackSmallText), 1);
    assert_eq!(r.count(Kind::RichBlack), 2);
}

#[test]
fn spot_and_devicen_colours_resolve_to_process_inks() {
    let sampled = stream("/FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1 0 1] /Size [2] /BitsPerSample 8", &[0, 0, 0, 0, 153, 102, 102, 255]);
    let resources = "<< /ColorSpace << /Rich [/Separation /RichSpot /DeviceCMYK 5 0 R] /Samp [/Separation /S2 /DeviceCMYK 6 0 R] \
        /Pc [/DeviceN [/Cyan /Black] /DeviceCMYK 7 0 R] /K [/Separation /Black /DeviceCMYK 7 0 R] \
        /Calc [/Separation /Calc /DeviceCMYK 8 0 R] /Icc [/ICCBased 9 0 R] /Ix [/Indexed /DeviceCMYK 1 <00000000 996666ff>] >> >>";
    let content = b"/Rich cs 1 scn 0 0 10 10 re f /Samp cs 1 scn 20 0 10 10 re f /Pc cs 0.5 1 scn 40 0 10 10 re f \
        /K cs 1 scn 60 0 10 10 re f /Calc cs 1 scn 80 0 10 10 re f /Icc cs 0.5 0.5 0.5 1 scn 100 0 10 10 re f /Ix cs 1 scn 120 0 10 10 re f \
        0 0 0 rg 140 0 10 10 re f";
    let d = doc_with(
        "",
        resources,
        content,
        &[
            b"<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0.5 0.5 0.5 1] /N 1 >>".to_vec(),
            sampled,
            b"<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0 0 1] /N 1 >>".to_vec(),
            stream("/FunctionType 4 /Domain [0 1] /Range [0 1 0 1 0 1 0 1]", b"{ dup dup dup }"),
            stream("/N 4", b""),
        ],
    );
    let r = run(&d);
    let rich = of(&r, Kind::RichBlack);
    let spaces: Vec<&str> = rich.iter().map(|f| f.colour.as_ref().unwrap().space.as_str()).collect();
    assert_eq!(spaces, vec!["Separation /RichSpot", "Separation /S2", "DeviceN [/Cyan /Black]", "ICCBased (CMYK)", "Indexed DeviceCMYK"], "{rich:?}");
    let spot = rich[0].colour.as_ref().unwrap();
    assert!(spot.via_alternate);
    assert_eq!(spot.cmyk, Some([50.0, 50.0, 50.0, 100.0]));
    assert!(rich[0].message.contains("alternate"));
    assert_eq!(rich[1].colour.as_ref().unwrap().cmyk, Some([60.0, 40.0, 40.0, 100.0]));
    assert!(!rich[2].colour.as_ref().unwrap().via_alternate);
    assert_eq!(rich[2].colour.as_ref().unwrap().cmyk, Some([50.0, 0.0, 0.0, 100.0]));
    assert_eq!(r.unresolved.get("spot colours with an unevaluated tint transform"), Some(&1));
    assert_eq!(r.unresolved.get("RGB and Lab colours (need an output profile)"), Some(&1));
}

#[test]
fn registration_colour_outside_printer_marks_is_a_warning() {
    let resources = "<< /ColorSpace << /All [/Separation /All /DeviceCMYK 5 0 R] >> /Properties << /MC 6 0 R >> >>";
    let content = b"/All cs 1 scn 100 100 10 10 re f \
        q /Artifact << /Type /Page /PCMark /PrinterMarks >> BDC /All cs 1 scn 150 150 10 10 re f EMC Q \
        /OC /MC BDC /All CS 1 SCN 200 200 m 210 210 l S EMC \
        /All cs 0.5 scn 2 2 5 5 re f \
        /All cs 0 scn 300 300 5 5 re f";
    let d = doc_with(
        "/TrimBox [20 20 592 772]",
        resources,
        content,
        &[b"<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 1 1 1] /N 1 >>".to_vec(), b"<< /Type /OCG /Name (Printer marks) >>".to_vec()],
    );
    let r = run(&d);
    let reg = of(&r, Kind::Registration);
    assert_eq!(reg.len(), 1, "{reg:?}");
    assert_eq!(reg[0].bbox, Some([100.0, 100.0, 110.0, 110.0]));
    assert_eq!(reg[0].severity, Severity::Warning);
    assert!(reg[0].message.contains("100%"));
    // The registration colour is not also a rich black.
    assert_eq!(r.count(Kind::RichBlack), 0);
}

#[test]
fn forms_and_patterns_are_followed_and_loops_stop() {
    let form = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /Me 5 0 R >> /Pattern << /P 6 0 R >> >>",
        b"0.6 0.6 0.6 1 k 0 0 10 10 re f /Me Do /Pattern cs /P scn 0 0 50 50 re f",
    );
    let pattern = stream(
        "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << /Pattern << /P 6 0 R >> /XObject << /F 5 0 R >> >>",
        b"0.7 0.7 0.7 1 k 0 0 5 5 re f /Pattern cs /P scn 0 0 10 10 re f /F Do",
    );
    let d = doc_with("", "<< /XObject << /Fm 5 0 R >> >>", b"q 1 0 0 1 100 100 cm /Fm Do Q /Fm Do", &[form, pattern]);
    let r = run(&d);
    let rich = of(&r, Kind::RichBlack);
    let from_form = rich.iter().find(|f| f.object == Some(ObjRef::new(5, 0))).unwrap();
    assert_eq!(from_form.occurrences, 2);
    assert_eq!(from_form.bbox, Some([0.0, 0.0, 110.0, 110.0]));
    assert!(rich.iter().any(|f| f.object == Some(ObjRef::new(6, 0))));
    assert!(r.limits.is_empty(), "{:?}", r.limits);
}

#[test]
fn findings_are_capped_per_kind() {
    let mut content = String::new();
    for i in 0..20 {
        content.push_str(&format!("0.{:02} 0.5 0.5 1 k {i} 0 1 1 re f ", 10 + i));
    }
    let d = doc(&content);
    let r = check(&d, &Options { max_findings: 5, ..Options::default() }).unwrap();
    assert_eq!(of(&r, Kind::RichBlack).len(), 5);
    assert_eq!(r.count(Kind::RichBlack), 20);
    assert!(r.findings_truncated);
    assert_eq!(r.occurrences.get("rich-black"), Some(&20));
}

#[test]
fn bad_options_and_pages_are_rejected() {
    let d = doc("");
    for bad in [
        Options { rich_black_k: 0.0, ..Options::default() },
        Options { rich_black_k: 101.0, ..Options::default() },
        Options { rich_black_k: f64::NAN, ..Options::default() },
        Options { rich_black_cmy: 0.5, ..Options::default() },
        Options { rich_black_cmy: 301.0, ..Options::default() },
        Options { small_text_pt: 0.0, ..Options::default() },
        Options { small_text_pt: f64::INFINITY, ..Options::default() },
        Options { max_findings: 0, ..Options::default() },
        Options { max_findings: MAX_FINDINGS + 1, ..Options::default() },
    ] {
        assert!(check(&d, &bad).is_err(), "{bad:?}");
    }
    let err = check(&d, &Options { pages: vec![1], ..Options::default() }).unwrap_err();
    assert!(err.contains("does not exist"), "{err}");
    assert_eq!(check(&d, &Options { pages: vec![0], ..Options::default() }).unwrap().pages, vec![0]);
}

#[test]
fn hostile_content_does_not_panic() {
    let short_samples = stream("/FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1 0 1] /Size [4096] /BitsPerSample 16", &[1, 2, 3]);
    let huge_samples = stream("/FunctionType 0 /Domain [5 5] /Range [0 1 0 1 0 1 0 1] /Size [1e300] /BitsPerSample 8", &[]);
    let resources = "<< /ColorSpace << /A [/Separation /A /DeviceCMYK 5 0 R] /B [/Separation /B /DeviceCMYK 6 0 R] /C [/Separation /C [/Separation /D /DeviceCMYK 7 0 R] 7 0 R] \
        /N [/DeviceN [] /DeviceCMYK 7 0 R] /I [/Indexed /DeviceCMYK 255 <00>] /Loop /Loop >> \
        /ExtGState << /G << /OP 5 /op (x) /OPM 1e300 /Font [1 2 3] >> >> /XObject << /X 8 0 R >> /Pattern << /P 9 0 R >> >>";
    let content = b"q q 1e300 0 0 1e300 0 0 cm /G gs /A cs 1e300 scn 0 0 1e308 -1e308 re f Q /B cs -1 scn 0 0 1 1 re f /C cs 0.5 scn 0 0 1 1 re B \
        /N cs scn 0 0 1 1 re f /I cs 300 scn 0 0 1 1 re f /Loop cs 1 scn 0 0 1 1 re f /Nope cs 1 2 3 scn n \
        BT /F 1e308 Tf 1e308 0 0 1e308 0 0 Tm (x) Tj [1e308 (y)] TJ ' \" ET 0 0 m 1e309 1e309 1e309 1e309 1e309 1e309 c S \
        /X Do /Pattern cs /P scn 0 0 1 1 re f /Pattern cs /Missing scn 0 0 1 1 re f /Artifact BDC EMC EMC EMC /OC /Nope BDC \
        BI /W 4 /H 4 /IM true ID \x01\x02 EI ) ] >> W n f* Q Q Q Q";
    let self_form = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Matrix [1e300 0 0 1e300 0 0] /Resources << /XObject << /X 8 0 R >> >>",
        b"/X Do /X Do",
    );
    let shading_pattern = b"<< /PatternType 2 /Shading << /ShadingType 2 >> >>".to_vec();
    let d = doc_with(
        "/TrimBox [1 2 3]",
        resources,
        content,
        &[short_samples, huge_samples, b"<< /FunctionType 2 /N (x) >>".to_vec(), self_form, shading_pattern],
    );
    let r = run(&d);
    assert!(r.findings.iter().all(|f| f.colour.as_ref().is_none_or(|c| c.components.iter().all(|v| v.is_finite()))));
    assert!(r.unresolved.contains_key("shading patterns"), "{:?}", r.unresolved);
}

#[test]
fn deep_nesting_and_operator_floods_are_limited() {
    let mut content = "q ".repeat(MAX_STACK + 10);
    content.push_str("0.6 0.6 0.6 1 k 0 0 10 10 re f");
    let r = run(&doc(&content));
    assert!(r.limits.iter().any(|l| l.contains("nested")), "{:?}", r.limits);
    let mut marked = "/Span BMC ".repeat(MAX_MARKED + 1);
    marked.push_str("0.6 0.6 0.6 1 k 0 0 10 10 re f");
    assert!(!run(&doc(&marked)).limits.is_empty());
    // Forms nested deeper than the limit: a chain of distinct forms.
    let mut extra = Vec::new();
    for i in 0..(MAX_DEPTH + 4) {
        let next = 5 + i + 1;
        extra.push(stream(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << /XObject << /N {next} 0 R >> >>"),
            b"0.6 0.6 0.6 1 k 0 0 1 1 re f /N Do",
        ));
    }
    let d = doc_with("", "<< /XObject << /N 5 0 R >> >>", b"/N Do", &extra);
    let r = run(&d);
    assert!(r.limits.iter().any(|l| l.contains("deep")), "{:?}", r.limits);
    assert!(r.count(Kind::RichBlack) >= MAX_DEPTH - 2);
}
