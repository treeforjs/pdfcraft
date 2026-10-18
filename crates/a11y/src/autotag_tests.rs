use std::sync::Arc;

use super::*;
use crate::tags::tags;

fn doc(content: &str, catalog: &str) -> Document {
    let objs = [
        format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>"),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-BoldOblique >>".into(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, b) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{b}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

/// A stand-in extractor: one glyph per provisional unit, each on its own line 20pt apart.
fn lines(_: &Document, pages: &[usize]) -> Vec<Option<Vec<AutotagGlyph>>> {
    pages
        .iter()
        .map(|_| {
            Some(
                (0..50)
                    .map(|k| AutotagGlyph { text: format!("w{k}"), rect: [20.0, 20.0 + 20.0 * k as f64, 60.0, 31.0 + 20.0 * k as f64], mcid: k })
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn list_markers_and_page_numbers() {
    for t in ["• Apples", "•", "  - dash item", "1. One", "12) Twelve", "(a) Letter", "iv. Roman", "b) bee"] {
        assert!(list_marker(t).is_some(), "{t}");
    }
    for t in ["Apples", "-5 degrees", "2024. A year", "e.g. this", "Dr. Who", "I am", "1.5 metres", "(12 items"] {
        assert!(list_marker(t).is_none(), "{t}");
    }
    assert_eq!(list_marker("• Apples"), Some("•".len()));
    for t in ["7", "- 7 -", "Page 7", "page 7 of 12", "7/12", "vii", " 12 "] {
        assert!(is_page_number(t), "{t}");
    }
    for t in ["Chapter 7", "7 apples", "", "Page", "1234567"] {
        assert!(!is_page_number(t), "{t}");
    }
    assert_eq!(running_key("  Report  2024 page 3 "), "report #### page #");
}

#[test]
fn reading_order_follows_columns_and_rows() {
    // A title over two columns whose paragraph gaps line up, a left column one block longer,
    // then a footer note across both.
    let rects = [
        [72.0, 40.0, 400.0, 60.0],    // 0 title
        [320.0, 80.0, 540.0, 120.0],  // 1 right one
        [72.0, 80.0, 290.0, 120.0],   // 2 left one
        [72.0, 130.0, 290.0, 160.0],  // 3 left two
        [320.0, 130.0, 540.0, 170.0], // 4 right two
        [72.0, 180.0, 290.0, 200.0],  // 5 left three
        [72.0, 300.0, 540.0, 320.0],  // 6 across
    ];
    assert_eq!(reading_order(&rects, (0..rects.len()).collect(), 0), [0, 2, 3, 5, 1, 4, 6]);
    // Three columns.
    let three = [[10.0, 10.0, 90.0, 50.0], [210.0, 10.0, 290.0, 50.0], [110.0, 10.0, 190.0, 50.0], [10.0, 60.0, 90.0, 90.0]];
    assert_eq!(reading_order(&three, vec![0, 1, 2, 3], 0), [0, 3, 2, 1]);
    // A left column starting below the right one's first block still reads first.
    let late = [[320.0, 10.0, 540.0, 40.0], [72.0, 50.0, 290.0, 90.0], [320.0, 50.0, 540.0, 90.0]];
    assert_eq!(reading_order(&late, vec![0, 1, 2], 0), [1, 0, 2]);
    // Hostile boxes don't panic and keep every block.
    let bad = [[f64::NAN, 0.0, 1.0, 1.0], [0.0, f64::INFINITY, 1.0, f64::NEG_INFINITY], [5.0, 5.0, 1.0, 1.0]];
    let mut got = reading_order(&bad, vec![0, 1, 2], 0);
    got.sort_unstable();
    assert_eq!(got, [0, 1, 2]);
    // Deep recursion is bounded: many staggered columns.
    let many: Vec<[f64; 4]> = (0..2000).map(|i| [i as f64 * 10.0, i as f64, i as f64 * 10.0 + 5.0, i as f64 + 3.0]).collect();
    assert_eq!(reading_order(&many, (0..2000).collect(), 0).len(), 2000);
}

#[test]
fn ruled_grids_are_found() {
    let mut rules = Vec::new();
    for y in [100.0, 120.0, 140.0] {
        rules.push(Rule { horizontal: true, at: y, from: 50.0, to: 250.0 });
    }
    for x in [50.0, 150.0, 250.0] {
        rules.push(Rule { horizontal: false, at: x, from: 100.0, to: 140.0 });
    }
    // A stray line elsewhere is not part of it.
    rules.push(Rule { horizontal: true, at: 400.0, from: 0.0, to: 100.0 });
    let g = grids(&rules);
    assert_eq!(g.len(), 1);
    assert_eq!((g[0].1.len(), g[0].2.len()), (3, 3));
    // A box (two lines each way) is not a table.
    let boxed = [
        Rule { horizontal: true, at: 0.0, from: 0.0, to: 100.0 },
        Rule { horizontal: true, at: 100.0, from: 0.0, to: 100.0 },
        Rule { horizontal: false, at: 0.0, from: 0.0, to: 100.0 },
        Rule { horizontal: false, at: 100.0, from: 0.0, to: 100.0 },
    ];
    assert!(grids(&boxed).is_empty());
    assert!(grids(&[Rule { horizontal: true, at: f64::NAN, from: f64::NAN, to: 1.0 }; 10]).is_empty());
}

#[test]
fn leftover_marked_content_is_cleaned() {
    let d = doc("", "");
    let ops =
        pdfcraft_content::parse(b"EMC /P <</MCID 3>> BDC (a) Tj EMC /Artifact <</MCID 4 /Type /Pagination>> BDC (b) Tj EMC /OC /x BDC (c) Tj EMC")
            .ops;
    let out = clean_ops(&d, ops, &Dict::new()).unwrap();
    let names: Vec<&[u8]> = out.iter().map(|o| o.op.as_slice()).collect();
    assert_eq!(names, [&b"Tj"[..], b"BDC", b"Tj", b"EMC", b"BDC", b"Tj", b"EMC"]);
    let Some(Object::Dict(props)) = out[1].operands.get(1) else { panic!() };
    assert!(!props.contains(b"MCID") && props.name(b"Type") == Some(b"Pagination"));
    let deep = "/X BMC ".repeat(MAX_NESTING + 1);
    assert!(clean_ops(&d, pdfcraft_content::parse(deep.as_bytes()).ops, &Dict::new()).is_err());
}

#[test]
fn autotag_with_a_stand_in_extractor() {
    let content = "BT /F1 11 Tf 20 180 Td (a) Tj (b) Tj ET 0 0 10 10 re f q 5 0 0 5 50 50 cm BI /W 1 /H 1 /BPC 8 /CS /G ID \x7f EI Q";
    let mut d = doc(content, "");
    let r = autotag(&mut d, &AutotagOptions { lang: Some("fr".into()), ..Default::default() }, &mut lines).unwrap();
    assert_eq!(r.pages, [0]);
    // Bold font, one line each, no larger text: headings; the inline image is a figure.
    assert_eq!((r.count("Document"), r.count("Figure")), (1, 1));
    assert_eq!(r.figures_needing_alt.len(), 1);
    assert_eq!(r.artifacts, 1, "the rectangle");
    let t = tags(&d);
    assert!(t.exists && t.marked && t.lang.as_deref() == Some("fr"));
    // Every MCID on the page is in the parent tree and the tree.
    let ua = crate::verify_pdfua(&d, &crate::UaOptions::default());
    for cp in ["01-005", "mcid-unreferenced", "struct-missing-content", "parent-tree"] {
        assert_eq!(ua.count(cp), 0, "{cp}: {:?}", ua.findings);
    }
    // Tagged now: refused.
    assert_eq!(autotag(&mut d, &AutotagOptions::default(), &mut lines), Err(AutotagError::AlreadyTagged));
}

#[test]
fn refusals_change_nothing() {
    let content = "BT /F1 11 Tf 20 180 Td (a) Tj ET";
    let mut d = doc(content, "/Lang (en)");
    let mut none = |_: &Document, p: &[usize]| p.iter().map(|_| None).collect::<Vec<_>>();
    let err = autotag(&mut d, &AutotagOptions::default(), &mut none).unwrap_err();
    assert!(err.to_string().contains("no page could be tagged"), "{err}");
    let bad_lang = autotag(&mut d, &AutotagOptions { lang: Some("-x".into()), ..Default::default() }, &mut lines).unwrap_err();
    assert!(bad_lang.to_string().contains("language tag"));
    let bad_page = autotag(&mut d, &AutotagOptions { pages: Some(vec![1]), ..Default::default() }, &mut lines).unwrap_err();
    assert!(bad_page.to_string().contains("no page 2"));
    assert!(!tags(&d).exists, "nothing changed");
    // A wrong-sized answer from the extractor is not trusted past its end.
    let mut short = |_: &Document, _: &[usize]| Vec::new();
    assert!(autotag(&mut d, &AutotagOptions::default(), &mut short).is_err());
    // Glyphs pointing at units that don't exist, or with absurd boxes, are ignored.
    let mut hostile = |_: &Document, p: &[usize]| {
        p.iter()
            .map(|_| {
                Some(vec![
                    AutotagGlyph { text: "x".into(), rect: [f64::NAN, 0.0, 1.0, 1.0], mcid: 0 },
                    AutotagGlyph { text: "y".into(), rect: [0.0, 0.0, 1e9, 1.0], mcid: 0 },
                    AutotagGlyph { text: "z".into(), rect: [0.0, 0.0, 1.0, 1.0], mcid: -1 },
                    AutotagGlyph { text: "w".into(), rect: [0.0, 0.0, 1.0, 1.0], mcid: i64::MAX },
                ])
            })
            .collect()
    };
    let r = autotag(&mut d, &AutotagOptions::default(), &mut hostile).unwrap();
    assert_eq!(r.count("P"), 0, "no readable text: the run is an artifact");
}

#[test]
fn field_labels_and_names_become_tooltips() {
    use super::fields::{clean_label, humanize_name};
    assert_eq!(clean_label("  Email address:  ").as_deref(), Some("Email address"));
    assert_eq!(clean_label("Signature: ________").as_deref(), Some("Signature"));
    assert_eq!(clean_label("Name *").as_deref(), Some("Name"));
    assert_eq!(clean_label("____ : *"), None);
    assert_eq!(clean_label(&"x".repeat(10_000)).map(|s| s.chars().count()), Some(120));
    assert_eq!(humanize_name("first_name").as_deref(), Some("First name"));
    assert_eq!(humanize_name("ünits-sold").as_deref(), Some("Ünits sold"));
    assert_eq!(humanize_name("__"), None);
}
