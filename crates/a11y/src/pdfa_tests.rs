use std::sync::Arc;

use pdfcraft_cos::Document;
use pdfcraft_preflight::{ConvertError, Level, Severity, declared, declared_level};

use super::*;

fn pdf(bodies: &[String]) -> Document {
    let mut out = b"%PDF-1.7\n".to_vec();
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
    out.extend_from_slice(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

fn stream(data: &str) -> String {
    format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len())
}

fn xmp(part: u8, conformance: &str) -> String {
    stream(&format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description rdf:about=\"\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\">\
         <pdfaid:part>{part}</pdfaid:part><pdfaid:conformance>{conformance}</pdfaid:conformance></rdf:Description></rdf:RDF></x:xmpmeta>"
    ))
}

const CONTENT: &str = "/P << /MCID 0 >> BDC BT /F1 12 Tf 20 150 Td (Hello world) Tj ET EMC /Figure << /MCID 1 >> BDC 20 20 50 50 re f EMC";

/// A tagged one-page document declaring PDF/A-`part``conformance`; `edit` rewrites the bodies.
fn doc_with(part: u8, conformance: &str, edit: impl Fn(&mut Vec<String>)) -> Document {
    let mut v: Vec<String> = [
        "<< /Type /Catalog /Pages 2 0 R /Lang (en) /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R /Metadata 9 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> /StructParents 0 >>",
        "CONTENT",
        "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Sans /Encoding /WinAnsiEncoding /FontDescriptor 7 0 R >>",
        "<< /Type /StructTreeRoot /K 8 0 R /ParentTree 10 0 R >>",
        "<< /Type /FontDescriptor /FontName /ABCDEF+Sans /Flags 32 /FontFile2 11 0 R >>",
        "<< /Type /StructElem /S /Document /P 6 0 R /K [12 0 R 13 0 R] >>",
        "XMP",
        "<< /Nums [0 [12 0 R 13 0 R]] >>",
        "<< /Length 4 >>\nstream\nfake\nendstream",
        "<< /Type /StructElem /S /P /P 8 0 R /Pg 3 0 R /K 0 >>",
        "<< /Type /StructElem /S /Figure /P 8 0 R /Pg 3 0 R /Alt (A grey square) /K 1 >>",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v[3] = stream(CONTENT);
    v[8] = xmp(part, conformance);
    edit(&mut v);
    pdf(&v)
}

fn errors(issues: &[Issue]) -> Vec<&Issue> {
    issues.iter().filter(|i| i.is_error()).collect()
}

#[test]
fn a_clean_2a_file_conforms() {
    let doc = doc_with(2, "A", |_| {});
    assert_eq!(declared_level(&doc), Some(Level::A2a));
    let issues = verify_pdfa(&doc, Level::A2a);
    assert!(errors(&issues).is_empty(), "{issues:#?}");
    // The structure checks only run for level a.
    assert!(pdfa_structure(&doc, Level::A2u).is_empty());
    let d1 = doc_with(1, "A", |_| {});
    assert!(errors(&verify_pdfa(&d1, Level::A1a)).is_empty(), "{:#?}", verify_pdfa(&d1, Level::A1a));
}

#[test]
fn untagged_content_fails_level_a() {
    let doc = doc_with(2, "A", |v| v[3] = stream(&format!("{CONTENT} BT /F1 12 Tf 20 100 Td (Loose) Tj ET")));
    let issues = verify_pdfa(&doc, Level::A2a);
    let e = errors(&issues);
    assert!(
        e.iter().any(|i| i.clause == "6.7.3.3" && i.message.contains("neither tagged nor marked as an artifact") && i.page == Some(0)),
        "{issues:#?}"
    );
    // Level u is fine with it.
    assert!(errors(&verify_pdfa(&doc, Level::A2u)).iter().all(|i| i.clause != "6.7.3.3"));
    // A document with no structure at all.
    let doc = doc_with(2, "A", |v| v[0] = "<< /Type /Catalog /Pages 2 0 R /Lang (en) /Metadata 9 0 R >>".into());
    let c: Vec<&str> = errors(&verify_pdfa(&doc, Level::A2a)).iter().map(|i| i.clause).collect();
    assert!(c.contains(&"6.7.2.2") && c.contains(&"6.7.3.3"), "{c:?}");
    let c: Vec<&str> = errors(&verify_pdfa(&doc, Level::A1a)).iter().map(|i| i.clause).collect();
    assert!(c.contains(&"6.8.2.2") && c.contains(&"6.8.3.3"), "{c:?}");
}

#[test]
fn missing_language_fails_level_a() {
    let doc = doc_with(3, "A", |v| v[0] = v[0].replace("/Lang (en) ", ""));
    let issues = verify_pdfa(&doc, Level::A3a);
    assert!(errors(&issues).iter().any(|i| i.clause == "6.7.4" && i.message.contains("natural language")), "{issues:#?}");
    // A Lang on the elements is enough.
    let doc = doc_with(3, "A", |v| {
        v[0] = v[0].replace("/Lang (en) ", "");
        v[7] = v[7].replace("/S /Document", "/S /Document /Lang (de)");
    });
    assert!(errors(&verify_pdfa(&doc, Level::A3a)).is_empty(), "{:#?}", verify_pdfa(&doc, Level::A3a));
}

#[test]
fn a_figure_without_alt_fails_level_a() {
    let doc = doc_with(2, "A", |v| v[12] = v[12].replace("/Alt (A grey square) ", ""));
    let issues = verify_pdfa(&doc, Level::A2a);
    assert!(errors(&issues).iter().any(|i| i.clause == "6.7" && i.message.contains("§14.9.3")), "{issues:#?}");
    // ActualText will do.
    let doc = doc_with(2, "A", |v| v[12] = v[12].replace("/Alt (A grey square)", "/ActualText (Square)"));
    assert!(errors(&verify_pdfa(&doc, Level::A2a)).is_empty());
}

#[test]
fn non_standard_types_need_a_role_map() {
    let doc = doc_with(2, "A", |v| v[11] = v[11].replace("/S /P", "/S /Para"));
    assert!(errors(&verify_pdfa(&doc, Level::A2a)).iter().any(|i| i.clause == "6.7.3.4"));
    let doc = doc_with(2, "A", |v| {
        v[11] = v[11].replace("/S /P", "/S /Para");
        v[5] = v[5].replace("/ParentTree", "/RoleMap << /Para /P >> /ParentTree");
    });
    assert!(errors(&verify_pdfa(&doc, Level::A2a)).is_empty());
}

#[test]
fn conversion_to_2a_needs_tags() {
    // Untagged: refused, nothing changes.
    let mut doc = doc_with(2, "B", |v| {
        v[0] = "<< /Type /Catalog /Pages 2 0 R /Lang (en) /Metadata 9 0 R >>".into();
        v[3] = stream("BT /F1 12 Tf 20 150 Td (Hello) Tj ET");
    });
    assert!(!pdfa_blockers(&doc, Level::A2a).is_empty());
    match convert_pdfa(&mut doc, Level::A2a) {
        Err(ConvertError::Refused { blocking, .. }) => assert!(blocking.iter().all(|i| i.is_error() && !i.fixable), "{blocking:#?}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(declared(&doc).pdfa, Some((2, "B".into())));
    // Level u is still possible.
    let r = convert_pdfa(&mut doc, Level::A2u).unwrap();
    assert!(errors(&r.remaining).is_empty(), "{:#?}", r.remaining);
    assert_eq!(declared(&doc).pdfa, Some((2, "U".into())));

    // Tagged: converted and identified as level a.
    let mut doc = doc_with(2, "B", |_| {});
    assert!(pdfa_blockers(&doc, Level::A2a).is_empty());
    let r = convert_pdfa(&mut doc, Level::A2a).unwrap();
    assert!(errors(&r.remaining).is_empty(), "{:#?}", r.remaining);
    assert_eq!(declared(&doc).pdfa, Some((2, "A".into())));
    assert_eq!(declared_level(&doc), Some(Level::A2a));
    assert!(errors(&verify_pdfa(&doc, Level::A2a)).is_empty());
}

#[test]
fn formulas_without_alt_only_warn() {
    let doc = doc_with(2, "A", |v| v[12] = v[12].replace("/S /Figure", "/S /Formula").replace("/Alt (A grey square) ", ""));
    let issues = verify_pdfa(&doc, Level::A2a);
    assert!(errors(&issues).is_empty(), "{issues:#?}");
    assert!(issues.iter().any(|i| i.severity == Severity::Warning && i.message.contains("§14.9.3")), "{issues:#?}");
}

#[test]
fn hostile_structure_does_not_panic() {
    let edits: [fn(&mut Vec<String>); 4] = [
        |v| v[5] = "<< /Type /StructTreeRoot /K 6 0 R /RoleMap << /P /Para /Para /P >> /ParentTree 6 0 R >>".into(),
        |v| v[7] = "<< /Type /StructElem /S /Document /P 8 0 R /K [8 0 R 8 0 R 12 0 R] >>".into(),
        |v| v[9] = "<< /Nums [0 7 -1 [] 99999999999 null] /Kids [10 0 R] >>".into(),
        |v| v[3] = stream("/P << /MCID -5 >> BDC /Artifact BMC /P << /MCID 99999999999 >> BDC EMC EMC EMC EMC (x) Tj"),
    ];
    for e in edits {
        let doc = doc_with(2, "A", e);
        for level in Level::ALL {
            let _ = verify_pdfa(&doc, level);
            let mut d = doc.clone();
            let _ = convert_pdfa(&mut d, level);
        }
    }
}
