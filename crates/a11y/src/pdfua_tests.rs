use std::sync::Arc;

use pdfcraft_cos::Document;

use super::*;

/// A PDF with `header` version from object bodies (object n = bodies[n - 1]).
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

fn pdf(bodies: &[String]) -> Document {
    pdf_v("1.7", bodies, "/Info 8 0 R")
}

fn stream(data: &str) -> String {
    format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len())
}

fn xmp(part: u8, rev: Option<&str>) -> String {
    let rev = rev.map(|r| format!("<pdfuaid:rev>{r}</pdfuaid:rev>")).unwrap_or_default();
    stream(&format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:pdfuaid=\"http://www.aiim.org/pdfua/ns/id/\">\
         <pdfuaid:part>{part}</pdfuaid:part>{rev}<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Quarterly report</rdf:li></rdf:Alt></dc:title>\
         </rdf:Description></rdf:RDF></x:xmpmeta>"
    ))
}

const CONTENT: &str = "/H1 << /MCID 0 >> BDC BT /F1 12 Tf 20 180 Td (Title) Tj ET EMC \
     /Figure << /MCID 1 >> BDC q 10 0 0 10 20 120 cm /Im Do Q EMC \
     /Artifact BMC 0 0 200 2 re f EMC \
     /TH << /MCID 2 >> BDC BT /F1 12 Tf (A) Tj ET EMC /TH << /MCID 3 >> BDC BT /F1 12 Tf (B) Tj ET EMC \
     /TD << /MCID 4 >> BDC BT /F1 12 Tf (1) Tj ET EMC /TD << /MCID 5 >> BDC BT /F1 12 Tf (2) Tj ET EMC \
     /Lbl << /MCID 6 >> BDC BT /F1 12 Tf (1.) Tj ET EMC /LBody << /MCID 7 >> BDC BT /F1 12 Tf (Item) Tj ET EMC";

/// A well-tagged PDF/UA-1 document (objects 1..=30).
fn good() -> Vec<String> {
    let mut v = vec![String::from("null"); 30];
    let mut set = |n: usize, s: &str| v[n - 1] = s.to_string();
    set(
        1,
        "<< /Type /Catalog /Pages 2 0 R /Lang (en-US) /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R \
         /ViewerPreferences << /DisplayDocTitle true >> /Metadata 12 0 R >>",
    );
    set(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    set(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Tabs /S /Annots [9 0 R 10 0 R] /StructParents 0 \
         /Resources << /Font << /F1 5 0 R >> /XObject << /Im 11 0 R >> >> >>",
    );
    set(4, &stream(CONTENT));
    set(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FontDescriptor 13 0 R >>");
    set(6, "<< /Type /StructTreeRoot /K 7 0 R /RoleMap << /Heading1 /H1 >> /ParentTree 14 0 R >>");
    set(7, "<< /Type /StructElem /S /Document /P 6 0 R /Pg 3 0 R /K [15 0 R 16 0 R 17 0 R 29 0 R 23 0 R 27 0 R] >>");
    set(8, "<< /Title (Quarterly report) >>");
    set(9, "<< /Type /Annot /Subtype /Link /Rect [20 20 80 40] /Contents (Example website) /StructParent 1 >>");
    set(10, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /TU (Your name) /Rect [20 60 120 80] /StructParent 2 >>");
    set(
        11,
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n\u{0}\nendstream",
    );
    set(12, &xmp(1, None));
    set(13, "<< /Type /FontDescriptor /FontName /Helvetica /Flags 32 /FontFile 24 0 R >>");
    set(14, "<< /Nums [0 [15 0 R 16 0 R 19 0 R 20 0 R 21 0 R 22 0 R 25 0 R 26 0 R] 1 23 0 R 2 27 0 R] >>");
    set(15, "<< /Type /StructElem /S /Heading1 /P 7 0 R /K 0 >>");
    set(16, "<< /Type /StructElem /S /Figure /P 7 0 R /Alt (A red square) /K 1 >>");
    set(17, "<< /Type /StructElem /S /Table /P 7 0 R /A << /O /Table /Summary (Two columns) >> /K [18 0 R 28 0 R] >>");
    set(18, "<< /Type /StructElem /S /TR /P 17 0 R /K [19 0 R 20 0 R] >>");
    set(19, "<< /Type /StructElem /S /TH /P 18 0 R /A << /O /Table /Scope /Column >> /K 2 >>");
    set(20, "<< /Type /StructElem /S /TH /P 18 0 R /A << /O /Table /Scope /Column >> /K 3 >>");
    set(21, "<< /Type /StructElem /S /TD /P 28 0 R /K 4 >>");
    set(22, "<< /Type /StructElem /S /TD /P 28 0 R /K 5 >>");
    set(23, "<< /Type /StructElem /S /Link /P 7 0 R /K << /Type /OBJR /Obj 9 0 R >> >>");
    set(24, &stream("fake font program"));
    set(25, "<< /Type /StructElem /S /Lbl /P 30 0 R /K 6 >>");
    set(26, "<< /Type /StructElem /S /LBody /P 30 0 R /K 7 >>");
    set(27, "<< /Type /StructElem /S /Form /P 7 0 R /K << /Type /OBJR /Obj 10 0 R >> >>");
    set(28, "<< /Type /StructElem /S /TR /P 17 0 R /K [21 0 R 22 0 R] >>");
    set(29, "<< /Type /StructElem /S /L /P 7 0 R /K [30 0 R] >>");
    set(30, "<< /Type /StructElem /S /LI /P 29 0 R /K [25 0 R 26 0 R] >>");
    v
}

fn ua1(doc: &Document) -> UaReport {
    verify_pdfua(doc, &UaOptions::default())
}

fn ua2(doc: &Document) -> UaReport {
    verify_pdfua(doc, &UaOptions { part: UaPart::Ua2, ..UaOptions::default() })
}

/// Replace `from` (which must be there) with `to` in object `n`.
fn edit(objs: &mut [String], n: usize, from: &str, to: &str) {
    assert!(objs[n - 1].contains(from), "object {n} has no {from:?}: {}", objs[n - 1]);
    objs[n - 1] = objs[n - 1].replacen(from, to, 1);
}

#[test]
fn a_well_tagged_ua1_document_conforms() {
    let doc = pdf(&good());
    let r = ua1(&doc);
    assert!(r.conforms(), "{:#?}", r.findings);
    assert_eq!(r.declared, Some(1));
    assert!(r.failed().is_empty());
    assert!(r.checked.contains(&"06-002") && r.checked.contains(&"14-003") && !r.checked.contains(&"ua2-namespace"));
    // The same file isn't PDF/UA-2: wrong version, identifier and no revision.
    let r2 = ua2(&doc);
    assert!(!r2.conforms());
    assert_eq!(r2.failed(), ["06-002", "ua2-rev", "ua2-version"]);
}

#[test]
fn a_well_tagged_ua2_document_conforms_and_namespaces_are_checked() {
    let mut objs = good();
    objs[11] = xmp(2, Some("2024"));
    let doc = pdf_v("2.0", &objs, "/Info 8 0 R");
    let r = ua2(&doc);
    assert!(r.conforms(), "{:#?}", r.findings);
    assert_eq!(r.declared, Some(2));
    // A Note (PDF 1.7 only) breaks UA-2; mapped through a 1.7 namespace to FENote it doesn't.
    edit(&mut objs, 7, "27 0 R]", "27 0 R << /S /Note /ID (n1) >>]");
    let r = ua2(&pdf_v("2.0", &objs, "/Info 8 0 R"));
    assert_eq!(r.failed(), ["ua2-namespace"], "{:#?}", r.findings);
    objs.push("<< /Type /Namespace /NS (http://iso.org/pdf/ssn) /RoleMapNS << /Note [/FENote 32 0 R] >> >>".into());
    objs.push("<< /Type /Namespace /NS (http://iso.org/pdf2/ssn) >>".into());
    edit(&mut objs, 7, "/S /Note", "/S /Note /NS 31 0 R");
    let r = ua2(&pdf_v("2.0", &objs, "/Info 8 0 R"));
    assert!(r.conforms(), "{:#?}", r.findings);
    // A custom namespace type with no mapping.
    edit(&mut objs, 7, "/S /Note /NS 31 0 R", "/S /Callout /NS 32 0 R");
    let r = ua2(&pdf_v("2.0", &objs, "/Info 8 0 R"));
    assert_eq!(r.failed(), ["02-001"], "{:#?}", r.findings);
}

#[test]
fn each_broken_variant_fails_with_its_checkpoint() {
    type Mutation = fn(&mut Vec<String>);
    let cases: Vec<(&str, Mutation)> = vec![
        ("06-001", |o| edit(o, 1, " /Metadata 12 0 R", "")),
        ("06-002", |o| o[11] = xmp(2, None)),
        ("06-003", |o| o[11] = o[11].replace("<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Quarterly report</rdf:li></rdf:Alt></dc:title>", "")),
        ("07-001", |o| edit(o, 1, "/ViewerPreferences << /DisplayDocTitle true >>", "")),
        ("07-002", |o| edit(o, 1, "/DisplayDocTitle true", "/DisplayDocTitle false")),
        ("mark-info", |o| edit(o, 1, "/Marked true", "/Marked false")),
        ("01-007", |o| edit(o, 1, "/Marked true", "/Marked true /Suspects true")),
        ("11-001", |o| edit(o, 1, "/Lang (en-US) ", "")),
        ("13-003", |o| edit(o, 16, " /Alt (A red square)", "")),
        ("14-002", |o| edit(o, 6, "/Heading1 /H1", "/Heading1 /H2")),
        ("15-003", |o| edit(o, 19, " /A << /O /Table /Scope /Column >>", "")),
        ("09-004", |o| edit(o, 28, "/S /TR", "/S /P")),
        ("09-005", |o| edit(o, 29, "/S /L", "/S /Div")),
        ("02-001", |o| edit(o, 6, "/Heading1 /H1", "/Heading1 /Fancy")),
        ("02-002", |o| edit(o, 6, "/Heading1 /H1", "/Heading1 /Fancy /Fancy /Heading1")),
        ("02-003", |o| edit(o, 6, "/Heading1 /H1", "/Heading1 /H1 /P /Span")),
        ("28-012", |o| edit(o, 9, " /Contents (Example website)", "")),
        ("28-011", |o| edit(o, 23, "/S /Link", "/S /P")),
        ("28-010", |o| edit(o, 27, "/S /Form", "/S /Span")),
        ("28-005", |o| edit(o, 10, " /TU (Your name)", "")),
        ("28-008", |o| edit(o, 3, " /Tabs /S", "")),
        ("28-009", |o| edit(o, 3, "/Tabs /S", "/Tabs /R")),
        ("28-002", |o| {
            edit(o, 3, "10 0 R]", "10 0 R 31 0 R]");
            o.push("<< /Type /Annot /Subtype /Text /Rect [150 150 170 170] /Contents (Note) >>".into());
        }),
        ("28-004", |o| {
            edit(o, 3, "10 0 R]", "10 0 R 31 0 R]");
            o.push("<< /Type /Annot /Subtype /Square /Rect [150 150 170 170] >>".into());
        }),
        ("28-007", |o| {
            edit(o, 3, "10 0 R]", "10 0 R 31 0 R]");
            o.push("<< /Type /Annot /Subtype /TrapNet /Rect [0 0 1 1] >>".into());
        }),
        ("01-005", |o| o[3] = stream(&format!("{CONTENT} 0 0 10 10 re f"))),
        ("01-003", |o| o[3] = stream(&CONTENT.replace("(Title) Tj ET EMC", "(Title) Tj ET /Artifact BMC 0 0 1 1 re f EMC EMC"))),
        ("01-004", |o| {
            o[3] = stream(&CONTENT.replace("/Artifact BMC 0 0 200 2 re f EMC", "/Artifact BMC /Span << /MCID 8 >> BDC 0 0 200 2 re f EMC EMC"))
        }),
        ("mcid-unreferenced", |o| o[3] = stream(&format!("{CONTENT} /P << /MCID 8 >> BDC BT /F1 12 Tf (More) Tj ET EMC"))),
        ("struct-missing-content", |o| edit(o, 26, "/K 7", "/K [7 9]")),
        ("parent-tree", |o| edit(o, 14, "1 23 0 R 2 27 0 R", "2 27 0 R")),
        ("fonts-embedded", |o| edit(o, 5, " /FontDescriptor 13 0 R", "")),
        ("10-001", |o| edit(o, 13, "/Flags 32", "/Flags 4")),
        ("17-002", |o| edit(o, 7, "27 0 R]", "27 0 R << /S /Formula /K 9 >>]")),
        ("14-003", |o| edit(o, 7, "27 0 R]", "27 0 R << /S /H3 >>]")),
        ("14-006", |o| edit(o, 7, "27 0 R]", "27 0 R << /S /H >> << /S /H >>]")),
        ("19-003", |o| edit(o, 7, "27 0 R]", "27 0 R << /S /Note >>]")),
        ("19-004", |o| edit(o, 7, "27 0 R]", "27 0 R << /S /Note /ID (n) >> << /S /Note /ID (n) >>]")),
        ("09-006", |o| edit(o, 7, "27 0 R]", "27 0 R << /S /TOCI >>]")),
        ("20-002", |o| edit(o, 1, "/Metadata 12 0 R", "/Metadata 12 0 R /OCProperties << /OCGs [] /D << /Order [] >> >>")),
        ("20-001", |o| {
            edit(o, 1, "/Metadata 12 0 R", "/Metadata 12 0 R /OCProperties << /OCGs [] /D << /Name (Default) >> /Configs [<< /Order [] >>] >>")
        }),
        ("20-003", |o| edit(o, 1, "/Metadata 12 0 R", "/Metadata 12 0 R /OCProperties << /OCGs [] /D << /Name (Default) /AS [] >> >>")),
        ("21-001", |o| o.push("<< /Type /Filespec /F (a.txt) /EF << /F 24 0 R >> >>".into())),
        ("ps-xobject", |o| o.push("<< /Type /XObject /Subtype /PS /Length 4 >>\nstream\nshow\nendstream".into())),
        ("30-001", |o| {
            o.push("<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Ref << /F (x.pdf) /Page 0 >> /Length 0 >>\nstream\n\nendstream".into())
        }),
        ("25-001", |o| {
            edit(o, 1, "/Metadata 12 0 R", "/Metadata 12 0 R /AcroForm << /Fields [] /XFA 31 0 R >>");
            o.push(stream("<config><acrobat><acrobat7><dynamicRender>\n required\n</dynamicRender></acrobat7></acrobat></config>"));
        }),
    ];
    for (want, mutate) in cases {
        let mut objs = good();
        mutate(&mut objs);
        let r = ua1(&pdf(&objs));
        assert!(!r.conforms(), "{want}: should fail");
        assert!(r.count(want) > 0, "{want}: failed {:?}\n{:#?}", r.failed(), r.findings);
        assert!(r.findings.iter().any(|f| f.checkpoint == want && !f.message.is_empty()), "{want}");
    }
}

#[test]
fn findings_carry_pages_objects_and_are_capped() {
    let mut objs = good();
    edit(&mut objs, 16, " /Alt (A red square)", "");
    edit(&mut objs, 9, " /Contents (Example website)", "");
    let r = ua1(&pdf(&objs));
    let link = r.findings.iter().find(|f| f.checkpoint == "28-012").unwrap();
    assert_eq!((link.page, link.object.map(|o| o.num)), (Some(0), Some(9)));
    assert_eq!(r.findings.iter().find(|f| f.checkpoint == "13-003").unwrap().page, Some(0));

    let figures = " << /S /Figure /K 9 >>".repeat(25);
    let mut objs = good();
    edit(&mut objs, 7, "27 0 R]", &format!("27 0 R{figures}]"));
    let r = verify_pdfua(&pdf(&objs), &UaOptions { part: UaPart::Ua1, max_findings: 3 });
    assert_eq!(r.count("13-003"), 25);
    assert_eq!(r.findings.iter().filter(|f| f.checkpoint == "13-003").count(), 3);
    // max_findings 0 still keeps one per checkpoint, so a failure is never hidden.
    let r = verify_pdfua(&pdf(&objs), &UaOptions { part: UaPart::Ua1, max_findings: 0 });
    assert_eq!(r.findings.iter().filter(|f| f.checkpoint == "13-003").count(), 1);
    assert!(!r.conforms());
}

#[test]
fn a_looped_structure_tree_is_reported_not_followed() {
    let mut objs = good();
    // The list item points back at its list, and the list at the document.
    edit(&mut objs, 30, "/K [25 0 R 26 0 R]", "/K [25 0 R 26 0 R 29 0 R 7 0 R]");
    let r = ua1(&pdf(&objs));
    assert_eq!(r.failed(), ["struct-tree-shape"], "{:#?}", r.findings);
    let f = r.findings.iter().find(|f| f.checkpoint == "struct-tree-shape").unwrap();
    assert!(f.message.starts_with("2 structure references"), "{}", f.message);
    // A root that is its own kid.
    let mut objs = good();
    edit(&mut objs, 7, "/K [15 0 R", "/K [7 0 R 15 0 R");
    assert_eq!(ua1(&pdf(&objs)).failed(), ["struct-tree-shape"]);
}

#[test]
fn an_untagged_document_fails_cleanly() {
    let doc = pdf_v(
        "1.4",
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
            stream("BT /F1 12 Tf 20 100 Td (Hello) Tj ET 0 0 50 50 re f"),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        ],
        "",
    );
    let r = ua1(&doc);
    assert!(!r.conforms());
    assert_eq!(r.declared, None);
    assert_eq!(r.failed(), ["01-005", "06-001", "07-001", "11-001", "fonts-embedded", "mark-info", "struct-tree-root"]);
    // An empty or broken file: no pages, no catalog.
    let empty = Document::new_empty();
    let r = ua2(&empty);
    assert!(!r.conforms());
    assert!(r.failed().contains(&"struct-tree-root"));
}

#[test]
fn hostile_structure_values_do_not_crash() {
    let mut objs = good();
    // Negative and huge MCIDs, a parent tree that loops, a role map pointing at itself,
    // attributes of the wrong type, namespaces that aren't dictionaries.
    edit(&mut objs, 26, "/K 7", "/K [-1 9223372036854775807 << /Type /MCR /MCID -5 >>]");
    edit(&mut objs, 14, "<< /Nums", "<< /Kids [14 0 R] /Nums");
    edit(&mut objs, 6, "/Heading1 /H1", "/Heading1 /Heading1");
    edit(&mut objs, 17, "/A << /O /Table /Summary (Two columns) >>", "/A 5 /NS 3 0 R");
    objs[3] = stream(&format!("{CONTENT} /P << /MCID -3 >> BDC EMC EMC EMC Q Q 3 Tr"));
    for part in [UaPart::Ua1, UaPart::Ua2] {
        let r = verify_pdfua(&pdf(&objs), &UaOptions { part, max_findings: usize::MAX });
        assert!(!r.conforms());
    }
}

#[test]
fn invisible_text_needs_no_embedded_font() {
    let mut objs = good();
    edit(&mut objs, 5, " /FontDescriptor 13 0 R", "");
    // OCR-style text in rendering mode 3 only.
    objs[3] = stream(&CONTENT.replace("BT /F1 12 Tf", "BT 3 Tr /F1 12 Tf"));
    let r = ua1(&pdf(&objs));
    assert_eq!(r.count("fonts-embedded"), 0, "{:#?}", r.findings);
    assert!(r.conforms(), "{:#?}", r.findings);
}

#[test]
fn encryption_must_let_assistive_technology_read() {
    use pdfcraft_cos::{Algorithm, NewEncryption, SaveOptions, write_full};
    let encrypt = |permissions: i32| {
        let mut doc = pdf(&good());
        doc.set_encryption(&NewEncryption {
            algorithm: Algorithm::Aes128,
            user_password: "",
            owner_password: "owner",
            permissions,
            encrypt_metadata: true,
            seed: [7; 32],
        })
        .unwrap();
        let bytes = Arc::new(write_full(&doc, &SaveOptions::default()).unwrap());
        Document::open(bytes).unwrap()
    };
    let r = ua1(&encrypt(-1));
    assert!(r.conforms(), "everything allowed: {:#?}", r.findings);
    // Bit 10 (extract for accessibility) cleared.
    let r = ua1(&encrypt(!(1 << 9)));
    assert_eq!(r.failed(), ["26-002"], "{:#?}", r.findings);
}

#[test]
fn language_on_the_structure_is_enough() {
    let mut objs = good();
    edit(&mut objs, 1, "/Lang (en-US) ", "");
    edit(&mut objs, 7, "/S /Document", "/S /Document /Lang (en-US)");
    let r = ua1(&pdf(&objs));
    assert!(r.conforms(), "{:#?}", r.findings);
    // Only the part without a language is reported.
    edit(&mut objs, 7, "/S /Document /Lang (en-US)", "/S /Document");
    edit(&mut objs, 17, "/S /Table", "/S /Table /Lang (fr)");
    let r = ua1(&pdf(&objs));
    assert_eq!(r.count("11-001"), 4, "H1, Figure, Lbl and LBody; the table cells inherit fr: {:#?}", r.findings);
}
