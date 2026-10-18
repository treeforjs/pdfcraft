use std::sync::Arc;

use pdfcraft_cos::Document;

use super::*;

/// A PDF with `version` from object bodies (object n = bodies[n - 1]).
fn pdf_v(version: &str, bodies: &[String], trailer: &str) -> Document {
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

/// A minimal ICC profile header: version `major`, device class and colour space.
fn icc(major: u8, class: &str, space: &str) -> String {
    let mut h = vec![0u8; 256];
    h[0..4].copy_from_slice(&256u32.to_be_bytes());
    h[8] = major;
    h[12..16].copy_from_slice(class.as_bytes());
    h[16..20].copy_from_slice(space.as_bytes());
    h[20..24].copy_from_slice(b"XYZ ");
    h[36..40].copy_from_slice(b"acsp");
    // Every byte is ASCII, so the profile can be written inside a String body.
    h.into_iter().map(char::from).collect()
}

const X1A_CONTENT: &str = "0 0 0 1 k 10 10 100 100 re f /Spot cs 1 scn 0 0 5 5 re f BT /F1 12 Tf 20 20 Td (Hi) Tj ET /GS gs /Fm Do";

/// A conforming PDF/X-1a:2001 file (objects 1..=16).
fn x1a() -> Vec<String> {
    let mut v = vec![String::from("null"); 16];
    let mut set = |n: usize, s: &str| v[n - 1] = s.to_string();
    set(
        1,
        "<< /Type /Catalog /Pages 2 0 R /OutputIntents [<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier (FOGRA39) \
         /Info (Coated FOGRA39) /RegistryName (http://www.color.org) /DestOutputProfile 7 0 R >>] >>",
    );
    set(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    set(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /TrimBox [10 10 290 290] /BleedBox [5 5 295 295] /Contents 4 0 R \
         /Annots [12 0 R 14 0 R] /Resources << /Font << /F1 5 0 R >> /XObject << /Fm 9 0 R >> /ExtGState << /GS 10 0 R >> \
         /ColorSpace << /Spot 11 0 R >> >> >>",
    );
    set(4, &stream("", X1A_CONTENT));
    set(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FontDescriptor 6 0 R >>");
    set(6, "<< /Type /FontDescriptor /FontName /Helvetica /Flags 32 /FontFile3 8 0 R >>");
    set(7, &stream("/N 4", &icc(2, "prtr", "CMYK")));
    set(8, &stream("/Subtype /Type1C", "font"));
    set(9, &stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1]", "0.5 g 0 0 1 1 re f"));
    set(10, "<< /Type /ExtGState /CA 1 /ca 1 /BM /Normal /OP true >>");
    set(11, "[/Separation /Gold /DeviceCMYK 13 0 R]");
    set(12, "<< /Type /Annot /Subtype /PrinterMark /Rect [0 0 20 20] /F 4 >>");
    set(13, "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 0.8 0.1] /N 1 >>");
    set(14, "<< /Type /Annot /Subtype /Link /Rect [50 50 80 80] /F 0 >>");
    set(
        15,
        "<< /Title (Job) /CreationDate (D:20260101120000Z) /ModDate (D:20260101120000Z) /Trapped /False \
         /GTS_PDFXVersion (PDF/X-1:2001) /GTS_PDFXConformance (PDF/X-1a:2001) >>",
    );
    v
}

const TRAILER: &str = "/Info 15 0 R /ID [<0102> <0102>]";

fn open_x1a(bodies: &[String]) -> Document {
    pdf_v("1.3", bodies, TRAILER)
}

fn check(doc: &Document, part: Option<XPart>) -> XReport {
    verify_pdfx(doc, &XOptions { part, max_findings: 100 })
}

fn errors(r: &XReport) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = r.findings.iter().filter(|f| f.severity == XSeverity::Error).map(|f| f.rule).collect();
    v.dedup();
    v
}

fn warnings(r: &XReport) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = r.findings.iter().filter(|f| f.severity == XSeverity::Warning).map(|f| f.rule).collect();
    v.dedup();
    v
}

fn xmp4(extra: &str) -> String {
    stream(
        "/Type /Metadata /Subtype /XML",
        &format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
             <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" \
             xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\" xmlns:pdfxid=\"http://www.npes.org/pdfx/ns/id/\">\
             <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Job</rdf:li></rdf:Alt></dc:title>\
             <xmp:CreateDate>2026-01-01T12:00:00Z</xmp:CreateDate><xmp:ModifyDate>2026-01-01T12:00:00Z</xmp:ModifyDate>\
             {extra}</rdf:Description></rdf:RDF></x:xmpmeta>"
        ),
    )
}

/// A conforming PDF/X-4 file, with transparency and optional content (objects 1..=16).
fn x4() -> Vec<String> {
    let mut v = x1a();
    v[0] = "<< /Type /Catalog /Pages 2 0 R /Metadata 16 0 R /OCProperties << /OCGs [] /D << /Name (Default) /Order [] >> >> \
            /OutputIntents [<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier (FOGRA39) /DestOutputProfile 7 0 R >>] >>"
        .into();
    v[2] = v[2].replace("/Contents 4 0 R", "/Contents 4 0 R /Group << /S /Transparency /CS /DeviceCMYK >>");
    v[6] = stream("/N 4", &icc(4, "prtr", "CMYK"));
    v[9] = "<< /Type /ExtGState /CA 0.5 /ca 0.5 /BM /Multiply >>".into();
    v[14] = "<< /Title (Job) /Trapped /False /GTS_PDFXVersion (PDF/X-4) >>".into();
    v[15] = xmp4("<pdf:Trapped>False</pdf:Trapped><pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion>");
    v
}

fn open_x4(bodies: &[String]) -> Document {
    pdf_v("1.6", bodies, TRAILER)
}

#[test]
fn a_valid_x1a_file_conforms_and_is_detected() {
    let r = check(&open_x1a(&x1a()), None);
    assert_eq!(r.part, XPart::X1a2001);
    assert_eq!(r.declared.as_deref(), Some("PDF/X-1:2001"));
    assert_eq!(r.declared_part, Some(XPart::X1a2001));
    assert!(r.conforms(), "{:#?}", r.findings);
    assert!(r.findings.is_empty(), "{:#?}", r.findings);
    assert!(r.checked.contains(&"colour-x1a") && r.checked.contains(&"transparency") && !r.checked.contains(&"optional-content"));
    // The same file read as PDF/X-1a:2003 is identified wrongly.
    let r = check(&open_x1a(&x1a()), Some(XPart::X1a2003));
    assert_eq!(errors(&r), ["version-key"], "{:#?}", r.findings);
}

#[test]
fn a_valid_x4_file_with_transparency_conforms() {
    let doc = open_x4(&x4());
    let r = check(&doc, None);
    assert_eq!((r.part, r.declared_part), (XPart::X4, Some(XPart::X4)));
    assert!(r.conforms(), "{:#?}", r.findings);
    assert!(r.findings.is_empty(), "{:#?}", r.findings);
    assert!(!r.checked.contains(&"transparency") && r.checked.contains(&"optional-content"));
    // As PDF/X-1a:2003 the transparency (and the version) break it.
    let r = check(&doc, Some(XPart::X1a2003));
    assert_eq!(r.count("transparency"), 3, "group, alpha, blend: {:#?}", r.findings);
    assert!(errors(&r).contains(&"transparency") && errors(&r).contains(&"pdf-version"), "{:#?}", r.findings);
    // The same transparency in an X-1a file: fails X-1a, not X-4's transparency rule.
    let mut b = x1a();
    b[9] = "<< /Type /ExtGState /ca 0.5 >>".into();
    let doc = open_x1a(&b);
    assert_eq!(errors(&check(&doc, None)), ["transparency"]);
    assert_eq!(check(&doc, Some(XPart::X4)).count("transparency"), 0);
}

#[test]
fn an_undeclared_file_is_checked_as_x4() {
    let mut b = x1a();
    b[14] = "<< /Title (Job) >>".into();
    let r = check(&open_x1a(&b), None);
    assert_eq!((r.part, r.declared.as_deref()), (XPart::X4, None));
    let e = errors(&r);
    assert!(e.contains(&"version-key") && e.contains(&"xmp-metadata"), "{e:?}");
}

/// Each change to the conforming X-1a file breaks the rule named, and only it.
#[test]
fn each_broken_x1a_variant_fails_with_its_rule() {
    let cases: &[(&str, usize, &str, &str)] = &[
        ("colour-x1a", 4, "0 0 0 1 k", "1 0 0 rg"),
        ("colour-x1a", 11, "[/Separation /Gold /DeviceCMYK 13 0 R]", "[/ICCBased 7 0 R]"),
        ("colour-x1a", 9, "0.5 g", "BI /W 1 /H 1 /CS /RGB /BPC 8 ID abc EI"),
        ("page-trim", 3, "/TrimBox [10 10 290 290] ", ""),
        ("page-boxes", 3, "/TrimBox [10 10 290 290]", "/TrimBox [1 1 290 290]"),
        ("page-boxes", 3, "/BleedBox [5 5 295 295]", "/BleedBox [5 5 395 295]"),
        ("output-intent", 1, "/S /GTS_PDFX", "/S /GTS_PDFA1"),
        ("output-condition", 1, "/OutputConditionIdentifier (FOGRA39)", ""),
        ("output-profile", 1, "/RegistryName (http://www.color.org) /DestOutputProfile 7 0 R", ""),
        ("output-profile", 1, "/DestOutputProfile 7 0 R", "/DestOutputProfile 13 0 R"),
        ("trapped", 15, "/Trapped /False", "/Trapped /Unknown"),
        ("trapped", 15, "/Trapped /False", ""),
        ("version-key", 15, "(PDF/X-1:2001)", "(PDF/X-3:2002)"),
        ("conformance-key", 15, "/GTS_PDFXConformance (PDF/X-1a:2001)", ""),
        ("info-keys", 15, "/Title (Job)", ""),
        ("fonts-embedded", 6, "/FontFile3 8 0 R", ""),
        ("transparency", 10, "/BM /Normal", "/BM /Multiply"),
        ("transparency", 10, "/OP true", "/SMask << /S /Luminosity /G 9 0 R >>"),
        ("transfer-functions", 10, "/OP true", "/TR 13 0 R"),
        ("halftones", 10, "/OP true", "/HT << /HalftoneType 2 >>"),
        ("postscript", 4, "/Fm Do", "/Fm Do (x) PS"),
        ("annotations", 14, "/F 0", "/F 4"),
        ("actions", 12, "/F 4", "/F 4 /AA << /E << /S /JavaScript /JS (x) >> >>"),
    ];
    for (rule, obj, from, to) in cases {
        let mut b = x1a();
        let body = &mut b[obj - 1];
        assert!(body.contains(from), "{rule}: object {obj} has no {from:?}");
        *body = body.replacen(from, to, 1);
        let r = check(&open_x1a(&b), Some(XPart::X1a2001));
        let failed = r.failed();
        assert!(failed.contains(rule), "{rule}: {failed:?} {:#?}", r.findings);
        let warning = X_RULES.iter().any(|x| x.id == *rule && x.severity(XPart::X1a2001) == XSeverity::Warning);
        assert_eq!(r.conforms(), warning, "{rule}: {:#?}", r.findings);
        let others: Vec<_> = failed.iter().filter(|f| *f != rule).collect();
        assert!(others.is_empty(), "{rule}: also {others:?}");
    }
}

#[test]
fn output_profiles_are_checked() {
    for (profile, n, want) in
        [(icc(2, "mntr", "CMYK"), 4, "a \"mntr\" profile"), (icc(2, "prtr", "RGB "), 3, "CMYK or gray"), (icc(2, "prtr", "CMYK"), 3, "N is 3")]
    {
        let mut b = x1a();
        b[6] = stream(&format!("/N {n}"), &profile);
        let r = check(&open_x1a(&b), None);
        assert_eq!(errors(&r), ["output-profile-class"], "{want}");
        assert!(r.findings.iter().any(|f| f.message.contains(want)), "{want}: {:#?}", r.findings);
    }
    let mut b = x1a();
    b[6] = stream("/N 4", "too short");
    assert_eq!(errors(&check(&open_x1a(&b), None)), ["output-profile-class"]);
    // ICC version 4 is newer than PDF 1.3 supports: a warning.
    let mut b = x1a();
    b[6] = stream("/N 4", &icc(4, "prtr", "CMYK"));
    let r = check(&open_x1a(&b), None);
    assert!(r.conforms());
    assert_eq!(warnings(&r), ["output-profile-version"]);
    // Two PDF/X output intents.
    let mut b = x1a();
    b[0] = b[0].replace(">>] >>", ">> << /S /GTS_PDFX /OutputConditionIdentifier (Other) /RegistryName (r) >>] >>");
    let r = check(&open_x1a(&b), None);
    assert_eq!(errors(&r), ["output-intent"]);
    assert!(r.findings[0].message.contains("2 GTS_PDFX"), "{:#?}", r.findings);
}

#[test]
fn warnings_do_not_break_conformance() {
    let cases: &[(&str, usize, &str, &str)] = &[
        ("colour-alternate", 11, "/DeviceCMYK", "/DeviceRGB"),
        ("page-trim-art", 3, "/BleedBox", "/ArtBox [10 10 290 290] /BleedBox"),
        ("halftone-type", 10, "/OP true", "/HT << /HalftoneType 6 >>"),
        (
            "embedded-files",
            1,
            "/Pages 2 0 R",
            "/Pages 2 0 R /Names << /EmbeddedFiles << /Names [(a.txt) << /Type /Filespec /F (a.txt) /EF << /F 8 0 R >> >>] >> >>",
        ),
    ];
    for (rule, obj, from, to) in cases {
        let mut b = x1a();
        b[obj - 1] = b[obj - 1].replacen(from, to, 1);
        let r = check(&open_x1a(&b), None);
        assert!(r.conforms(), "{rule}: {:#?}", r.findings);
        assert!(warnings(&r).contains(rule), "{rule}: {:#?}", r.findings);
    }
    // No ID: a warning for 2001, an error for 2003.
    let doc = pdf_v("1.3", &x1a(), "/Info 15 0 R");
    let r = check(&doc, None);
    assert!(r.conforms() && warnings(&r) == ["document-id"], "{:#?}", r.findings);
    let mut b = x1a();
    b[14] = b[14].replace("(PDF/X-1:2001)", "(PDF/X-1a:2003)");
    let doc = pdf_v("1.4", &b, "/Info 15 0 R");
    assert_eq!(errors(&check(&doc, None)), ["document-id"]);
}

#[test]
fn pdf_versions_are_limited_per_part() {
    let r = check(&pdf_v("1.4", &x1a(), TRAILER), None);
    assert_eq!(errors(&r), ["pdf-version"]);
    assert!(r.findings[0].message.contains("PDF 1.4") && r.findings[0].message.contains("1.3"));
    // The catalog's Version counts too.
    let mut b = x1a();
    b[0] = b[0].replace("/Pages 2 0 R", "/Pages 2 0 R /Version /1.5");
    assert_eq!(errors(&check(&open_x1a(&b), None)), ["pdf-version"]);
    assert_eq!(errors(&check(&pdf_v("1.7", &x4(), TRAILER), None)), ["pdf-version"]);
}

#[test]
fn x3_allows_device_independent_colour_but_not_rgb_with_a_cmyk_intent() {
    let mut b = x1a();
    b[14] = b[14].replace("(PDF/X-1:2001)", "(PDF/X-3:2002)").replace("/GTS_PDFXConformance (PDF/X-1a:2001)", "");
    b[10] = "[/Separation /Gold /Lab << /WhitePoint [0.9505 1 1.089] >> 13 0 R]".into();
    b[8] = stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /ColorSpace << /CS0 [/ICCBased 7 0 R] >> >>", "/CS0 cs 1 1 1 1 sc");
    let r = check(&open_x1a(&b), None);
    assert_eq!(r.part, XPart::X3_2002);
    assert!(r.findings.is_empty(), "{:#?}", r.findings);
    // DeviceRGB with a CMYK printing condition.
    b[3] = stream("", &X1A_CONTENT.replace("0 0 0 1 k", "1 0 0 rg"));
    let r = check(&open_x1a(&b), None);
    assert_eq!(errors(&r), ["colour-rgb"]);
    // A DefaultRGB colour space makes it device-independent.
    b[2] = b[2].replace("/Spot 11 0 R", "/Spot 11 0 R /DefaultRGB [/CalRGB << /WhitePoint [0.9505 1 1.089] >>]");
    assert!(check(&open_x1a(&b), None).findings.is_empty());
}

#[test]
fn x4_rules() {
    let cases: &[(&str, usize, &str, &str)] = &[
        ("version-key", 16, "<pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion>", ""),
        ("version-key", 16, "<pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion>", "<pdfxid:GTS_PDFXVersion>PDF/X-4p</pdfxid:GTS_PDFXVersion>"),
        ("trapped", 16, "<pdf:Trapped>False</pdf:Trapped>", "<pdf:Trapped>Unknown</pdf:Trapped>"),
        ("trapped", 15, "/Trapped /False", "/Trapped /True"),
        ("xmp-dates", 16, "<xmp:CreateDate>2026-01-01T12:00:00Z</xmp:CreateDate>", ""),
        ("optional-content", 1, "/D << /Name (Default) /Order [] >>", "/D << /Order [] /AS [] >>"),
        ("colour-rgb", 4, "0 0 0 1 k", "1 0 0 rg"),
        ("annotations", 14, "/F 0", "/F 4"),
        ("fonts-embedded", 6, "/FontFile3 8 0 R", ""),
        ("external-content", 9, "/BBox [0 0 1 1]", "/BBox [0 0 1 1] /Ref << /F (other.pdf) /Page 0 >>"),
        ("opi", 9, "/BBox [0 0 1 1]", "/BBox [0 0 1 1] /OPI << /2.0 << /Type /OPI /Version 2.0 /F (hi.tif) >> >>"),
        ("postscript", 9, "/Subtype /Form", "/Subtype /PS"),
    ];
    for (rule, obj, from, to) in cases {
        let mut b = x4();
        let body = &mut b[obj - 1];
        assert!(body.contains(from), "{rule}: object {obj} has no {from:?}");
        *body = body.replacen(from, to, 1);
        let r = check(&open_x4(&b), Some(XPart::X4));
        assert_eq!(errors(&r), [*rule], "{:#?}", r.findings);
    }
    // No XMP at all.
    let mut b = x4();
    b[0] = b[0].replace("/Metadata 16 0 R ", "");
    let r = check(&open_x4(&b), Some(XPart::X4));
    assert_eq!(errors(&r), ["version-key", "xmp-metadata"], "{:#?}", r.findings);
    // Information and XMP disagree on the version: a warning.
    let mut b = x4();
    b[14] = b[14].replace("(PDF/X-4)", "(PDF/X-4:2010)");
    let r = check(&open_x4(&b), None);
    assert!(r.conforms() && warnings(&r) == ["version-consistent"], "{:#?}", r.findings);
}

#[test]
fn looped_and_deep_forms_are_bounded() {
    // A form that draws itself, and two forms that draw each other.
    let mut b = x1a();
    b[8] = stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /Fm 9 0 R /Fn 16 0 R >> >>", "/Fm Do /Fn Do 1 0 0 rg");
    b[15] = stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /Fm 9 0 R >> >>", "/Fm Do");
    let r = check(&open_x1a(&b), None);
    assert_eq!(errors(&r), ["colour-x1a"]);
    assert_eq!(r.findings[0].object, Some(ObjRef::new(9, 0)));
    // A chain of forms deeper than the limit.
    let mut b = x1a();
    let first = b.len() + 1;
    let depth = MAX_DEPTH + 8;
    b[8] = stream(&format!("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /N {first} 0 R >> >>"), "/N Do");
    for i in 0..depth {
        let next = first + i + 1;
        b.push(stream(&format!("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /N {next} 0 R >> >>"), "/N Do"));
    }
    b.push(stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1]", "1 0 0 rg"));
    let r = check(&open_x1a(&b), None);
    assert_eq!(warnings(&r), ["limits"], "{:#?}", r.findings);
    assert!(r.conforms());
    // A looped Type 3 font and pattern.
    let mut b = x1a();
    b[4] = "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [1 0 0 1 0 0] /CharProcs << /a 16 0 R >> \
            /Resources << /Font << /F1 5 0 R >> /Pattern << /P 16 0 R >> >> >>"
        .into();
    b[15] = stream(
        "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1 1] /XStep 1 /YStep 1",
        "/F1 1 Tf /Pattern cs /P scn 0 0 1 1 re f 0.2 0.3 0.4 RG",
    );
    let r = check(&open_x1a(&b), None);
    assert_eq!(errors(&r), ["colour-x1a"], "{:#?}", r.findings);
}

#[test]
fn findings_are_capped_but_counted() {
    let mut b = x1a();
    b[2] = b[2].replace("/TrimBox [10 10 290 290] ", "");
    b[1] = "<< /Type /Pages /Kids [3 0 R 16 0 R] /Count 2 >>".into();
    b[15] = b[2].clone();
    let r = verify_pdfx(&open_x1a(&b), &XOptions { part: None, max_findings: 1 });
    assert_eq!(r.count("page-trim"), 2);
    assert_eq!(r.findings.iter().filter(|f| f.rule == "page-trim").count(), 1);
    assert_eq!(r.findings[0].page, Some(0));
}

#[test]
fn part_ids_parse() {
    for p in XPart::ALL {
        assert_eq!(XPart::from_id(p.id()), Some(p));
        assert_eq!(XPart::from_id(p.label()), Some(p));
    }
    assert_eq!(XPart::from_id("pdf/x-1A:2001"), Some(XPart::X1a2001));
    assert_eq!(XPart::from_id("X-5"), None);
    assert_eq!(XPart::from_declared("PDF/X-1:2001", Some("PDF/X-1:2001")), None);
    assert_eq!(XPart::from_declared("PDF/X-4p", None), None);
}

#[test]
fn hostile_documents_do_not_panic() {
    for body in [
        "<< /Type /Catalog /Pages 2 0 R /OutputIntents 5 >>",
        "<< /Type /Catalog /Pages 2 0 R /OutputIntents [null 1 0 R << /S /GTS_PDFX /DestOutputProfile (x) >>] /OCProperties << /D 7 /Configs 3 >> >>",
    ] {
        let mut b = x1a();
        b[0] = body.into();
        b[2] = b[2].replace("/MediaBox [0 0 300 300]", "/MediaBox [0 0 1e400 nan]").replace("/Annots [12 0 R 14 0 R]", "/Annots 99 0 R");
        b[3] = stream("", "/Fm Do /Nope Do q q q Q ] >> << BI /CS /RGB /W 1 /H 1 /BPC 8 ID \u{0}\u{0}\u{0} EI sh gs Tf");
        let _ = check(&open_x1a(&b), None);
        let _ = check(&open_x1a(&b), Some(XPart::X4));
    }
}
