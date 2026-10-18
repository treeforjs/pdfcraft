use std::sync::Arc;

use pdfcraft_cos::Document;

use super::*;

/// A PDF with `version` from object bodies (object n = bodies[n - 1]).
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

/// A valid document touching the core types: catalog, page tree, page, resources, a font, an
/// image XObject, a link annotation with a URI action, a form field with its widget, an outline,
/// a GoTo open action, a destinations name tree, the structure tree and its parent tree, and the
/// document information dictionary.
fn valid() -> Vec<String> {
    vec![
        // 1
        "<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R /AcroForm << /Fields [9 0 R] /DA (/Helv 0 Tf 0 g) >> /StructTreeRoot 11 0 R \
         /MarkInfo << /Marked true >> /Names << /Dests 13 0 R >> /OpenAction 14 0 R /PageMode /UseOutlines /Lang (en-GB) >>"
            .into(),
        // 2
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>".into(),
        // 3: MediaBox is inherited from the page tree.
        "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> /XObject << /Im1 6 0 R >> /ProcSet [/PDF /Text /ImageC] >> \
         /Contents 5 0 R /Annots [9 0 R 15 0 R] /StructParents 0 /Rotate 90 >>"
            .into(),
        // 4
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
        // 5
        stream("", "BT /F1 12 Tf 72 720 Td (Hello) Tj ET q 10 0 0 10 0 0 cm /Im1 Do Q"),
        // 6
        stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8", "abc"),
        // 7
        "<< /Type /Outlines /First 8 0 R /Last 8 0 R /Count 1 >>".into(),
        // 8
        "<< /Title (Start) /Parent 7 0 R /Dest [3 0 R /XYZ 0 792 0] >>".into(),
        // 9: a text field merged with its widget.
        "<< /FT /Tx /T (name) /V (x) /Type /Annot /Subtype /Widget /Rect [10 10 100 30] /P 3 0 R /F 4 >>".into(),
        // 10: an appearance stream (one /N stream needs no /AS).
        stream("/Type /XObject /Subtype /Form /BBox [0 0 50 50] /Resources << >>", ""),
        // 11
        "<< /Type /StructTreeRoot /K 12 0 R /ParentTree 16 0 R >>".into(),
        // 12
        "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K 0 >>".into(),
        // 13: destinations name tree.
        "<< /Names [(start) [3 0 R /Fit]] >>".into(),
        // 14
        "<< /Type /Action /S /GoTo /D [3 0 R /Fit] >>".into(),
        // 15
        "<< /Type /Annot /Subtype /Link /Rect [0 0 50 50] /A << /S /URI /URI (https://example.com) >> /Border [0 0 0] /AP << /N 10 0 R >> >>".into(),
        // 16: parent tree (number tree).
        "<< /Nums [0 [12 0 R]] >>".into(),
        // 17
        "<< /Title (T) /Producer (P) /CreationDate (D:20260101000000Z) >>".into(),
    ]
}

const TRAILER: &str = "/Info 17 0 R /ID [<0102> <0102>]";

fn run(doc: &Document) -> ArlReport {
    validate_arlington(doc, &ArlOptions::default())
}

fn errors(r: &ArlReport) -> Vec<&ArlFinding> {
    r.findings.iter().filter(|f| f.severity == ArlSeverity::Error).collect()
}

fn find<'r>(r: &'r ArlReport, code: &str, path: &str) -> Option<&'r ArlFinding> {
    r.findings.iter().find(|f| f.code == code && f.path == path)
}

#[test]
fn valid_document_has_no_errors() {
    let doc = pdf("1.7", &valid(), TRAILER);
    let r = run(&doc);
    assert_eq!((r.version, r.version_source), (Version::parse("1.7").unwrap(), "header"));
    assert!(r.valid(), "{:#?}", errors(&r));
    assert_eq!(r.warnings, 0, "{:#?}", r.findings);
    // Every core type was reached and checked as the right Arlington type.
    for ty in [
        "FileTrailer",
        "Catalog",
        "PageTreeNodeRoot",
        "PageObject",
        "Resource",
        "FontType1",
        "XObjectImage",
        "AnnotLink",
        "ActionURI",
        "ActionGoTo",
        "Outline",
        "OutlineItem",
        "InteractiveForm",
        "FieldTx",
        "AnnotWidget",
        "StructTreeRoot",
        "StructElem",
        "DocInfo",
    ] {
        assert!(r.types.contains_key(ty), "{ty} not checked: {:?}", r.types);
    }
    assert!(r.indirect_objects >= 15, "{}", r.indirect_objects);
    assert!(r.findings.is_empty(), "{:#?}", r.findings);
    // The standard-14 font rule (fn:NotStandard14Font) is not evaluated, and says so.
    assert!(r.not_checked.contains_key("fn:NotStandard14Font"), "{:?}", r.not_checked);
    assert!(!r.truncated && !r.limit_reached);
}

#[test]
fn injected_problems_are_found_with_their_paths() {
    let mut b = valid();
    // Pages without /Type, a page /MediaBox that is a name, an /OpenAction that is an integer.
    b[1] = "<< /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>".into();
    b[2] = b[2].replace("/StructParents 0", "/StructParents 0 /MediaBox /Letter");
    b[0] = b[0].replace("/OpenAction 14 0 R", "/OpenAction 42");
    let r = run(&pdf("1.7", &b, TRAILER));
    let f = find(&r, "missing-required", "Trailer/Root/Pages/Type").unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert_eq!((f.object, f.object_type.as_str(), f.key.as_deref()), (Some(2), "PageTreeNodeRoot", Some("Type")));
    let f = find(&r, "wrong-type", "Trailer/Root/Pages/Kids[0]/MediaBox").unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert_eq!((f.object, f.object_type.as_str()), (Some(3), "PageObject"));
    assert!(f.message.contains("rectangle"), "{}", f.message);
    let f = find(&r, "wrong-type", "Trailer/Root/OpenAction").unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert_eq!(f.object, Some(1));
    assert!(f.message.contains("array or dictionary"), "{}", f.message);
    assert_eq!(r.errors, 3, "{:#?}", errors(&r));

    // An /OpenAction dictionary that is no action: its /S isn't an action type.
    let mut b = valid();
    b[13] = "<< /Type /Action /S /Jump /D [3 0 R /Fit] >>".into();
    let r = run(&pdf("1.7", &b, TRAILER));
    let f = find(&r, "value-not-allowed", "Trailer/Root/OpenAction/S").unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert_eq!(f.object, Some(14));
}

#[test]
fn keys_newer_than_the_file_and_deprecated_keys() {
    let mut b = valid();
    // /OutputIntents on a page is PDF 2.0; /AF is PDF 2.0 (or the PDF/A-3 extension from 1.7);
    // /NeedsRendering is deprecated in 2.0.
    b[2] = b[2].replace("/StructParents 0", "/StructParents 0 /OutputIntents []");
    let r = run(&pdf("1.4", &b, TRAILER));
    let f = find(&r, "introduced-later", "Trailer/Root/Pages/Kids[0]/OutputIntents").unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert_eq!((f.severity, f.object, f.key.as_deref()), (ArlSeverity::Warning, Some(3), Some("OutputIntents")));
    assert!(f.message.contains("PDF 2.0") && f.message.contains("PDF 1.4"), "{}", f.message);
    // The same file checked as 2.0 has no such warning; the catalog /Version raises the version.
    let r2 = validate_arlington(&pdf("1.4", &b, TRAILER), &ArlOptions { version: Version::parse("2.0"), ..ArlOptions::default() });
    assert!(find(&r2, "introduced-later", "Trailer/Root/Pages/Kids[0]/OutputIntents").is_none());
    let mut c = b.clone();
    c[0] = c[0].replace("/Type /Catalog", "/Type /Catalog /Version /2.0 /NeedsRendering false");
    let r3 = run(&pdf("1.4", &c, TRAILER));
    assert_eq!((r3.version, r3.version_source), (Version::V2_0, "catalog"));
    assert!(find(&r3, "introduced-later", "Trailer/Root/Pages/Kids[0]/OutputIntents").is_none());
    assert!(find(&r3, "deprecated", "Trailer/Root/NeedsRendering").is_some(), "{:#?}", r3.findings);
    // In PDF 2.0 the trailer needs /ID.
    let r4 = run(&pdf("2.0", &valid(), "/Info 17 0 R"));
    assert!(find(&r4, "missing-required", "Trailer/ID").is_some(), "{:#?}", r4.findings);
    assert!(find(&r, "missing-required", "Trailer/ID").is_none());
}

#[test]
fn conditional_requirements_follow_the_object() {
    // A widget whose /AP /N is a dictionary of appearance states needs /AS
    // (fn:IsRequired(fn:IsPresent(AP::N::*) || …)); with one /N stream it doesn't.
    let mut b = valid();
    b[8] = b[8].replace("/F 4", "/F 4 /AP << /N << /Off 10 0 R >> >>");
    let r = run(&pdf("1.7", &b, TRAILER));
    let f = r.findings.iter().find(|f| f.code == "missing-required" && f.key.as_deref() == Some("AS")).unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert_eq!((f.object, f.object_type.as_str()), (Some(9), "AnnotWidget"));
    assert_eq!(r.errors, 1, "{:#?}", errors(&r));
    b[8] = b[8].replace("/F 4", "/F 4 /AS /Off");
    assert!(run(&pdf("1.7", &b, TRAILER)).valid());
}

#[test]
fn unknown_keys_are_information() {
    let mut b = valid();
    b[3] = b[3].replace("/Encoding", "/MyPrivateKey 1 /Encoding");
    let r = run(&pdf("1.7", &b, TRAILER));
    let f = find(&r, "unknown-key", "Trailer/Root/Pages/Kids[0]/Resources/Font/F1/MyPrivateKey").unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert_eq!((f.severity, f.object, f.object_type.as_str()), (ArlSeverity::Info, Some(4), "FontType1"));
    assert!(r.valid());
}

#[test]
fn values_outside_the_allowed_set_and_failed_conditions() {
    let mut b = valid();
    b[0] = b[0].replace("/PageMode /UseOutlines", "/PageMode /Sideways");
    // /Rotate must be a multiple of 90 (fn:Eval((@Rotate mod 90)==0)).
    b[2] = b[2].replace("/Rotate 90", "/Rotate 45");
    let r = run(&pdf("1.7", &b, TRAILER));
    let f = find(&r, "value-not-allowed", "Trailer/Root/PageMode").unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert!(f.message.contains("/Sideways") && f.message.contains("UseOutlines"), "{}", f.message);
    assert!(find(&r, "value-not-allowed", "Trailer/Root/Pages/Kids[0]/Rotate").is_some(), "{:#?}", r.findings);
}

#[test]
fn cycles_and_deep_graphs_terminate() {
    // Page tree whose kid is the tree itself, a page whose parent chain loops, outline items
    // pointing at each other, a self-referencing structure element.
    let bodies: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /StructTreeRoot 6 0 R >>".into(),
        "<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 1 /Parent 2 0 R >>".into(),
        "<< /Type /Page /Parent 3 0 R /Annots [3 0 R] >>".into(),
        "<< /Type /Outlines /First 5 0 R /Last 5 0 R >>".into(),
        "<< /Title (x) /Parent 4 0 R /Next 5 0 R /Prev 5 0 R /First 5 0 R /Last 5 0 R >>".into(),
        "<< /Type /StructTreeRoot /K 7 0 R >>".into(),
        "<< /Type /StructElem /S /P /P 7 0 R /K [7 0 R 7 0 R] >>".into(),
    ];
    let r = run(&pdf("1.7", &bodies, ""));
    assert!(r.objects_checked < 100, "{}", r.objects_checked);
    // MediaBox can't be found through the looping /Parent chain.
    assert!(find(&r, "missing-required", "Trailer/Root/Pages/Kids[1]/MediaBox").is_some(), "{:#?}", r.findings);

    // A linked list of 3 000 outline items, and a page tree 1 000 levels deep.
    let n = 3000;
    let mut bodies: Vec<String> = vec!["<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R >>".into(), String::new(), String::new()];
    bodies[2] = format!("<< /Type /Outlines /First 4 0 R /Last {} 0 R >>", n + 3);
    for i in 0..n {
        let num = i + 4;
        let next = if i + 1 < n { format!("/Next {} 0 R", num + 1) } else { String::new() };
        bodies.push(format!("<< /Title (i{i}) /Parent 3 0 R {next} >>"));
    }
    let depth = 1000;
    let first_node = bodies.len() + 1;
    bodies[1] = format!("<< /Type /Pages /Kids [{first_node} 0 R] /Count 1 /MediaBox [0 0 10 10] >>");
    for i in 0..depth {
        let num = first_node + i;
        let kid = num + 1;
        let parent = if i == 0 { 2 } else { num - 1 };
        bodies.push(format!("<< /Type /Pages /Kids [{kid} 0 R] /Count 1 /Parent {parent} 0 R >>"));
    }
    bodies.push(format!("<< /Type /Page /Parent {} 0 R /Resources << >> >>", first_node + depth - 1));
    let r = run(&pdf("1.7", &bodies, ""));
    assert!(r.limit_reached);
    assert!(r.findings.iter().any(|f| f.code == "limit"), "{:?}", r.findings.iter().map(|f| f.code).collect::<Vec<_>>());
    assert!(r.valid(), "{:#?}", errors(&r));
}

#[test]
fn hostile_values_do_not_panic() {
    let bodies: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /Names << /Dests << /Kids [3 0 R 3 0 R (x) 99 0 R] >> >> /PageLabels << /Nums [0 1 2] >> \
         /OpenAction [] /AcroForm << /Fields 5 >> /Outlines (x) >>"
            .into(),
        "<< /Type /Pages /Kids 7 /Count -1 /MediaBox [0 0 1e400 (a)] /Rotate 7 >>".into(),
        "<< /Kids [3 0 R] /Names [(a) 3 0 R (b)] >>".into(),
    ];
    let r = run(&pdf("9.9", &bodies, "/Encrypt 99 0 R /Info 3 0 R /ID 5"));
    assert!(!r.valid());
    let r = run(&pdf("junk", &["<< /Type /Catalog /Pages 5 0 R >>".to_string()], ""));
    assert_eq!(r.version_source, "default");
    // A dangling /Pages reads as null: the required key is missing.
    assert!(find(&r, "missing-required", "Trailer/Root/Pages").is_some(), "{:#?}", r.findings);
}

#[test]
fn max_findings_caps_the_output() {
    // 300 pages, each without /Resources and with a wrong /Rotate: 600 errors.
    let n = 300;
    let mut bodies: Vec<String> = vec!["<< /Type /Catalog /Pages 2 0 R >>".into()];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", i + 3)).collect();
    bodies.push(format!("<< /Type /Pages /Kids [{}] /Count {n} /MediaBox [0 0 10 10] >>", kids.join(" ")));
    for i in 0..n {
        bodies.push(format!("<< /Type /Page /Parent 2 0 R /Rotate (r) /Bogus{i} 1 >>"));
    }
    let doc = pdf("1.7", &bodies, "");
    let r = validate_arlington(&doc, &ArlOptions { max_findings: 25, ..ArlOptions::default() });
    assert_eq!(r.findings.len(), 25);
    assert!(r.truncated);
    assert_eq!(r.errors, 2 * n, "{}", r.errors);
    assert_eq!(r.infos, n);
    // Errors are kept before information.
    assert!(r.findings.iter().all(|f| f.severity == ArlSeverity::Error));
    let all = validate_arlington(&doc, &ArlOptions { max_findings: MAX_ARL_FINDINGS * 10, ..ArlOptions::default() });
    assert_eq!(all.findings.len(), 3 * n);
    assert!(!all.truncated);
}
