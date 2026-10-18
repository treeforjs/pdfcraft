use std::sync::Arc;

use pdfcraft_cos::Document;

use super::*;
use crate::{XOptions, XReport, XSeverity, verify_pdfx};

fn pdf(version: &str, bodies: &[String], trailer: &str) -> Document {
    let mut out = format!("%PDF-{version}\n").into_bytes();
    let mut offsets = Vec::new();
    for (i, b) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{b}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer << /Size {} /Root 1 0 R {trailer} >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

fn stream(dict: &str, data: &str) -> String {
    format!("<< {dict} /Length {} >>\nstream\n{data}\nendstream", data.len())
}

/// A header-only ICC profile.
pub(crate) fn icc(major: u8, class: &[u8; 4], space: &[u8; 4]) -> Vec<u8> {
    let mut h = vec![0u8; 256];
    h[0..4].copy_from_slice(&256u32.to_be_bytes());
    h[8] = major;
    h[12..16].copy_from_slice(class);
    h[16..20].copy_from_slice(space);
    h[20..24].copy_from_slice(b"XYZ ");
    h[36..40].copy_from_slice(b"acsp");
    h
}

/// RGB colour, a transparent graphics state, an unembedded Helvetica, JavaScript, a hidden
/// comment, a printing link and a transfer function; no PDF/X identification at all.
fn messy() -> Document {
    let bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R /OpenAction 8 0 R /Names << /JavaScript << /Names [(a) 8 0 R] >> >> /AA << /WC 8 0 R >> \
         /AcroForm << /Fields [] >> >>"
            .to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R /Annots [6 0 R 7 0 R] /AA << /O 8 0 R >> \
         /Resources << /Font << /F1 5 0 R >> /ExtGState << /GS 9 0 R /T << /TR /Identity /TR2 10 0 R >> >> >> >>"
            .into(),
        stream("", "1 0 0 rg 10 10 100 100 re f /GS gs 0 0 1 rg 50 50 100 100 re f BT /F1 12 Tf 20 20 Td (Hi) Tj ET"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
        "<< /Type /Annot /Subtype /Text /Rect [20 20 40 40] /F 2 /Contents (note) >>".into(),
        "<< /Type /Annot /Subtype /Link /Rect [50 50 80 80] /F 4 /A 8 0 R >>".into(),
        "<< /Type /Action /S /JavaScript /JS (app.alert(1)) >>".into(),
        "<< /Type /ExtGState /ca 0.5 /BM /Multiply /TR 10 0 R >>".into(),
        "<< /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >>".into(),
    ];
    pdf("1.7", &bodies, "")
}

fn check(doc: &Document, part: XPart) -> XReport {
    verify_pdfx(doc, &XOptions { part: Some(part), max_findings: 100 })
}

fn errors(r: &XReport) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = r.findings.iter().filter(|f| f.severity == XSeverity::Error).map(|f| f.rule).collect();
    v.dedup();
    v
}

fn opts(part: XPart, profile: Option<Vec<u8>>) -> XConvertOptions {
    XConvertOptions {
        intent: Some(XIntent { condition: "FOGRA39".into(), info: Some("Coated FOGRA39".into()), profile: profile.map(Arc::new) }),
        ..XConvertOptions::new(part)
    }
}

fn run(doc: &mut Document, o: &XConvertOptions) -> Vec<String> {
    o.validate(doc).unwrap();
    let mut fixed = pdfx_prepare(doc, o).unwrap();
    fixed.extend(pdfx_finish(doc, o, "D:20261018120000Z").unwrap());
    fixed
}

#[test]
fn structural_fixes_leave_only_colour_and_transparency_for_the_engine() {
    for part in [XPart::X1a2003, XPart::X3_2003, XPart::X4, XPart::X1a2001, XPart::X3_2002] {
        let mut doc = messy();
        let before = check(&doc, part);
        assert!(errors(&before).contains(&"version-key") && errors(&before).contains(&"fonts-embedded"), "{part:?}: {:?}", before.findings);
        let fixed = run(&mut doc, &opts(part, Some(icc(2, b"prtr", b"CMYK"))));
        assert!(fixed.iter().any(|f| f.contains("Embedded 1 standard font: Helvetica")), "{fixed:?}");
        let after = check(&doc, part);
        let left: Vec<_> = errors(&after).into_iter().filter(|r| !matches!(*r, "colour-x1a" | "colour-rgb" | "transparency")).collect();
        assert!(left.is_empty(), "{part:?}: {left:?} {:?}", after.findings);
        // JavaScript and actions are gone, links stay but don't print, the hidden note is gone.
        assert!(after.count("actions") == 0, "{:?}", after.findings);
        let page = &pdfcraft_model::pages(&doc)[0];
        let annots = doc.resolve(page.dict.get(b"Annots").unwrap()).as_array().cloned().unwrap();
        assert_eq!(annots.len(), 1);
        let link = doc.resolve(&annots[0]).as_dict().cloned().unwrap();
        assert_eq!(link.int(b"F"), Some(0));
        assert!(link.get(b"A").is_none());
        let cat = catalog(&doc);
        assert!(!cat.contains(b"AcroForm") && !cat.contains(b"OpenAction") && !cat.contains(b"AA"));
        // The font has a program and widths from the standard metrics.
        let font = doc.get(ObjRef::new(5, 0)).as_dict().cloned().unwrap();
        let widths = doc.resolve(font.get(b"Widths").unwrap()).as_array().cloned().unwrap();
        assert_eq!(widths.len(), 256);
        assert_eq!(widths[b'H' as usize].as_int(), Some(722));
        let fd = doc.resolve(font.get(b"FontDescriptor").unwrap()).as_dict().cloned().unwrap();
        let ff = doc.resolve(fd.get(b"FontFile3").unwrap());
        let Object::Stream(s) = &*ff else { panic!("{ff:?}") };
        assert_eq!(s.dict.name(b"Subtype"), Some(b"Type1C".as_slice()));
        assert!(s.decoded().unwrap().len() > 1000);
        // Boxes default from the MediaBox.
        let page = &pdfcraft_model::pages(&doc)[0];
        assert_eq!(rect(&doc, &page.dict, b"TrimBox"), Some([0.0, 0.0, 300.0, 300.0]));
        assert_eq!(rect(&doc, &page.dict, b"BleedBox"), Some([0.0, 0.0, 300.0, 300.0]));
        let max = part.max_pdf_version();
        assert_eq!(doc.version(), format!("{}.{}", max.0, max.1));
    }
}

#[test]
fn a_second_run_changes_nothing() {
    for part in [XPart::X1a2003, XPart::X4] {
        let mut doc = messy();
        let o = XConvertOptions {
            trim: Some([10.0, 10.0, 290.0, 290.0]),
            bleed: Some([5.0, 5.0, 295.0, 295.0]),
            ..opts(part, Some(icc(2, b"prtr", b"CMYK")))
        };
        run(&mut doc, &o);
        let snapshot: Vec<_> = doc.scan_objects().map(|(r, o)| (r, (*o).clone())).collect();
        let again = run(&mut doc, &o);
        assert!(again.is_empty(), "{part:?}: {again:?}");
        let now: Vec<_> = doc.scan_objects().map(|(r, o)| (r, (*o).clone())).collect();
        assert_eq!(snapshot, now);
        let page = &pdfcraft_model::pages(&doc)[0];
        assert_eq!(rect(&doc, &page.dict, b"TrimBox"), Some([10.0, 10.0, 290.0, 290.0]));
        assert_eq!(rect(&doc, &page.dict, b"BleedBox"), Some([5.0, 5.0, 295.0, 295.0]));
    }
}

#[test]
fn the_document_intent_is_kept_and_x4_metadata_written() {
    let mut doc = messy();
    run(&mut doc, &opts(XPart::X4, Some(icc(4, b"prtr", b"CMYK"))));
    let x = crate::xmp_metadata(&doc).unwrap();
    assert_eq!(crate::xmp_value(&x, "pdfxid:GTS_PDFXVersion").as_deref(), Some("PDF/X-4"));
    assert_eq!(crate::xmp_value(&x, "pdf:Trapped").as_deref(), Some("False"));
    // Keeping the document's intent now (no intent given) changes nothing.
    let keep = XConvertOptions::new(XPart::X4);
    keep.validate(&doc).unwrap();
    assert_eq!(keep.intent_space(&doc), *b"CMYK");
    assert!(keep.intent_profile(&doc).is_some());
    assert!(pdfx_finish(&mut doc, &keep, "D:20261018120000Z").unwrap().is_empty());
}

#[test]
fn bad_options_are_refused() {
    let doc = messy();
    let cmyk = Some(icc(2, b"prtr", b"CMYK"));
    let bad = [
        (XConvertOptions::new(XPart::X4), "no PDF/X output intent"),
        (opts(XPart::X4, None), "embeds the output profile"),
        (opts(XPart::X1a2003, Some(icc(2, b"mntr", b"CMYK"))), "output (printer, prtr) profile"),
        (opts(XPart::X1a2003, Some(icc(2, b"prtr", b"RGB "))), "PDF/X-1a needs"),
        (opts(XPart::X3_2003, Some(icc(4, b"prtr", b"CMYK"))), "ICC version 4"),
        (opts(XPart::X3_2003, Some(b"junk".to_vec())), "too short"),
        (opts(XPart::X3_2003, Some(icc(2, b"prtr", b"XCLR"))), "isn't CMYK"),
        (XConvertOptions { trim: Some([10.0, 10.0, 5.0, 50.0]), ..opts(XPart::X3_2003, cmyk.clone()) }, "right > left"),
        (XConvertOptions { trim: Some([f64::NAN, 0.0, 5.0, 5.0]), ..opts(XPart::X3_2003, cmyk.clone()) }, "right > left"),
        (
            XConvertOptions { trim: Some([0.0, 0.0, 100.0, 100.0]), bleed: Some([10.0, 10.0, 90.0, 90.0]), ..opts(XPart::X3_2003, cmyk.clone()) },
            "contain the trim",
        ),
        (XConvertOptions { title: Some("a\nb".into()), ..opts(XPart::X3_2003, cmyk.clone()) }, "one line"),
        (
            XConvertOptions { intent: Some(XIntent { condition: " ".into(), info: None, profile: None }), ..XConvertOptions::new(XPart::X1a2003) },
            "output_condition",
        ),
    ];
    for (o, want) in bad {
        let e = o.validate(&doc).unwrap_err();
        assert!(e.contains(want), "{e} (wanted {want:?})");
    }
    // A trim box outside a page is refused when the boxes are set.
    let mut d = messy();
    let o = XConvertOptions { trim: Some([0.0, 0.0, 400.0, 400.0]), ..opts(XPart::X3_2003, cmyk) };
    o.validate(&d).unwrap();
    assert!(pdfx_finish(&mut d, &o, "D:2026").unwrap_err().contains("outside page 1"));
}

#[test]
fn levels_parse() {
    assert_eq!(XPart::from_level("x1a"), Some(XPart::X1a2003));
    assert_eq!(XPart::from_level("PDF/X-3"), Some(XPart::X3_2003));
    assert_eq!(XPart::from_level("X-4"), Some(XPart::X4));
    assert_eq!(XPart::from_level("X-1a:2001"), Some(XPart::X1a2001));
    assert_eq!(XPart::from_level("x5"), None);
}

#[test]
fn hostile_documents_do_not_panic() {
    let bodies: Vec<Vec<String>> = vec![
        vec!["<< /Type /Catalog /Pages 2 0 R /OutputIntents 3 0 R /OCProperties << /D 1 0 R /Configs [1 0 R 5] >> >>".into(), "<< /Type /Pages /Kids [2 0 R] /Count 1 >>".into(), "[3 0 R]".into()],
        vec![
            "<< /Type /Catalog /Pages 2 0 R /Names 1 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1e300 -1e300] /TrimBox [1 2] /Annots 3 0 R /Resources << /ExtGState << /G << /HT << >> >> >> >> >>".into(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Courier /FontDescriptor 4 0 R /Widths 4 0 R >>".into(),
        ],
        vec!["<< /Type /Catalog >>".into()],
    ];
    for b in bodies {
        for part in XPart::ALL {
            let mut doc = pdf("2.0", &b, "/Info 1 0 R /ID 3 0 R");
            let o = opts(part, (part == XPart::X4).then(|| icc(2, b"prtr", b"CMYK")));
            let _ = o.validate(&doc);
            let _ = pdfx_prepare(&mut doc, &o);
            let _ = pdfx_finish(&mut doc, &o, "garbage");
            let _ = check(&doc, part);
        }
    }
}
