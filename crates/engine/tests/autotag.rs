//! Autotag Document on original synthetic untagged PDFs: structure, reading order (through the
//! accessible text export), artifacts, figures, links, tables, lists, undo/redo, save and
//! reload, the PDF/UA checker before and after, and hostile content.

use std::sync::Arc;

use pdfcraft_engine::a11y::{self, AutotagOptions, Options, Rule, Status, UaOptions};
use pdfcraft_engine::{DocId, Edit, Session};

/// A page: its content, and extra entries for its dictionary (`/Annots [...]`).
struct Page<'a> {
    content: &'a str,
    extra: &'a str,
}

fn page(content: &str) -> Page<'_> {
    Page { content, extra: "" }
}

/// Objects: 1 catalog, 2 pages, 3 Helvetica (F1), 4 Helvetica-Bold (F2), 5 an image (Im1),
/// then each page and its content (6, 7; 8, 9; …), then `extra` objects. `catalog` goes into
/// the catalog dictionary.
fn pdf(pages: &[Page], extra: &[String], catalog: &str) -> Vec<u8> {
    let n = pages.len();
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 6 + 2 * i)).collect();
    let mut objs: Vec<Vec<u8>> = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>").into_bytes(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} /MediaBox [0 0 612 792] >>", kids.join(" ")).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>".to_vec(),
        b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x7f\nendstream"
            .to_vec(),
    ];
    for (i, p) in pages.iter().enumerate() {
        objs.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R /F2 4 0 R >> /XObject << /Im1 5 0 R >> >> {} >>",
                7 + 2 * i,
                p.extra
            )
            .into_bytes(),
        );
        objs.push(format!("<< /Length {} >>\nstream\n{}\nendstream", p.content.len(), p.content).into_bytes());
    }
    objs.extend(extra.iter().map(|e| e.clone().into_bytes()));
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R /Info << /Title (Fixture) >> >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes(),
    );
    out
}

fn open(bytes: Vec<u8>) -> (Session, DocId) {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("fixture.pdf", None, Arc::new(bytes), None).unwrap();
    (s, id)
}

fn autotag(s: &mut Session, id: DocId, options: AutotagOptions) -> a11y::AutotagReport {
    let report = s.get(id).unwrap().autotag_preview(&options).unwrap();
    s.apply(id, Edit::Autotag { options }).unwrap();
    report
}

/// The structure types in document order.
fn types(s: &Session, id: DocId) -> Vec<String> {
    s.get(id).unwrap().tags().nodes.iter().map(|n| n.ty.clone()).collect()
}

fn text(s: &Session, id: DocId) -> String {
    s.get(id).unwrap().accessible_text().unwrap().text
}

/// `parts` appear in `t` in this order.
fn in_order(t: &str, parts: &[&str]) {
    let mut at = 0;
    for p in parts {
        let found = t.get(at..).and_then(|rest| rest.find(p)).unwrap_or_else(|| panic!("{p:?} missing after byte {at} in {t:?}"));
        at += found + p.len();
    }
}

fn ua_count(s: &Session, id: DocId, checkpoint: &str) -> usize {
    s.get(id).unwrap().pdfua_verify(&UaOptions::default()).unwrap().count(checkpoint)
}

fn rule(s: &Session, id: DocId, r: Rule) -> Status {
    s.get(id).unwrap().accessibility_check(&Options::default()).unwrap().result(r).unwrap().status
}

const SINGLE: &str = "BT /F1 24 Tf 72 720 Td (Annual Report) Tj ET \
    BT /F1 11 Tf 72 690 Td (This report describes the year.) Tj 0 -13 Td (It has two lines.) Tj ET \
    BT /F1 16 Tf 72 650 Td (Results) Tj ET \
    BT /F1 11 Tf 72 625 Td (Revenue grew in every quarter.) Tj ET \
    BT /F2 11 Tf 72 595 Td (Outlook) Tj ET \
    BT /F1 11 Tf 72 580 Td (We expect more growth next year.) Tj ET";

#[test]
fn a_single_column_gets_headings_and_paragraphs_in_order() {
    let (mut s, id) = open(pdf(&[page(SINGLE)], &[], ""));
    assert!(!s.get(id).unwrap().tags().exists);
    let r = autotag(&mut s, id, AutotagOptions { lang: Some("en-US".into()), ..Default::default() });
    assert_eq!(types(&s, id), ["Document", "H1", "P", "H2", "P", "H3", "P"]);
    assert_eq!((r.count("H1"), r.count("H2"), r.count("H3"), r.count("P")), (1, 1, 1, 3));
    assert_eq!(r.pages, [0]);
    assert_eq!(r.lang.as_deref(), Some("en-US"));
    assert!(r.figures_needing_alt.is_empty());
    let t = text(&s, id);
    assert_eq!(
        t,
        "Annual Report\n\nThis report describes the year. It has two lines.\n\nResults\n\nRevenue grew in every quarter.\n\nOutlook\n\nWe expect more growth next year.\n"
    );
    // The checker's tagging rules pass now.
    for r in [Rule::TaggedPdf, Rule::TaggedContent, Rule::PrimaryLanguage, Rule::HeadingNesting, Rule::TabOrder] {
        assert_eq!(rule(&s, id, r), Status::Passed, "{r:?}");
    }
    let tags = s.get(id).unwrap().tags();
    assert!(tags.marked && tags.lang.as_deref() == Some("en-US"));
}

#[test]
fn the_pdfua_checker_improves_and_the_result_survives_save_and_undo() {
    let (mut s, id) = open(pdf(&[page(SINGLE)], &[], ""));
    for cp in ["struct-tree-root", "mark-info"] {
        assert!(ua_count(&s, id, cp) > 0, "{cp} fails before");
    }
    autotag(&mut s, id, AutotagOptions::default());
    for cp in [
        "struct-tree-root",
        "mark-info",
        "01-005",
        "01-003",
        "01-004",
        "mcid-unreferenced",
        "struct-missing-content",
        "parent-tree",
        "struct-tree-shape",
        "14-002",
        "14-003",
    ] {
        assert_eq!(ua_count(&s, id, cp), 0, "{cp} passes after");
    }
    let after = text(&s, id);
    // Undo and redo.
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Autotag document"));
    assert_eq!(s.undo(id).unwrap(), "Autotag document");
    assert!(!s.get(id).unwrap().tags().exists);
    assert_eq!(s.redo(id).unwrap(), "Autotag document");
    assert_eq!(text(&s, id), after);
    // Save and reload.
    let saved = s.save_bytes(id).unwrap();
    let (s2, id2) = open(saved.to_vec());
    assert_eq!(types(&s2, id2), types(&s, id));
    assert_eq!(text(&s2, id2), after);
    assert_eq!(ua_count(&s2, id2, "parent-tree"), 0);
    assert_eq!(ua_count(&s2, id2, "01-005"), 0);
}

#[test]
fn an_already_tagged_document_is_refused_unless_replaced() {
    let (mut s, id) = open(pdf(&[page(SINGLE)], &[], ""));
    autotag(&mut s, id, AutotagOptions::default());
    let first = types(&s, id);
    let err = s.get(id).unwrap().autotag_preview(&AutotagOptions::default()).unwrap_err();
    assert!(err.contains("already tagged") && err.contains("replace_existing"), "{err}");
    assert!(s.apply(id, Edit::Autotag { options: AutotagOptions::default() }).is_err());
    assert_eq!(s.get(id).unwrap().can_undo(), Some("Autotag document"), "the refusal left no undo step");
    // Replacing a tree retags the whole document.
    let partial = AutotagOptions { replace_existing: true, pages: Some(vec![0]), ..Default::default() };
    assert!(s.get(id).unwrap().autotag_preview(&partial).is_ok(), "all pages listed");
    let r = autotag(&mut s, id, AutotagOptions { replace_existing: true, ..Default::default() });
    assert!(r.replaced);
    assert_eq!(types(&s, id), first);
    // The old marked content went: every MCID belongs to the new tree.
    assert_eq!(ua_count(&s, id, "mcid-unreferenced"), 0);
    assert_eq!(ua_count(&s, id, "struct-missing-content"), 0);
    // Bad options.
    let bad = |o: AutotagOptions| s.get(id).unwrap().autotag_preview(&o).unwrap_err();
    assert!(bad(AutotagOptions { replace_existing: true, lang: Some("en US".into()), ..Default::default() }).contains("language tag"));
    assert!(bad(AutotagOptions { replace_existing: true, pages: Some(vec![3]), ..Default::default() }).contains("no page 4"));
    assert!(bad(AutotagOptions { replace_existing: true, pages: Some(vec![]), ..Default::default() }).contains("at least one"));
}

#[test]
fn two_columns_read_down_the_left_then_the_right() {
    // Painted out of order: the right column, the title, then the left column.
    let c = "BT /F1 11 Tf 320 700 Td (Right one first line) Tj 0 -13 Td (right one second line.) Tj ET \
             BT /F1 11 Tf 320 650 Td (Right two first line) Tj 0 -13 Td (right two second line) Tj 0 -13 Td (right two third line.) Tj ET \
             BT /F1 18 Tf 72 740 Td (Two Column Title) Tj ET \
             BT /F1 11 Tf 72 700 Td (Left one first line) Tj 0 -13 Td (left one second line) Tj 0 -13 Td (left one third line.) Tj ET \
             BT /F1 11 Tf 72 650 Td (Left two first line) Tj 0 -13 Td (left two second line.) Tj ET";
    let (mut s, id) = open(pdf(&[page(c)], &[], "/Lang (en)"));
    autotag(&mut s, id, AutotagOptions::default());
    assert_eq!(types(&s, id), ["Document", "H1", "P", "P", "P", "P"]);
    let t = text(&s, id);
    in_order(
        &t,
        &["Two Column Title", "Left one first line", "left one third line.", "Left two", "Right one", "right one second", "Right two", "third line."],
    );
}

#[test]
fn bulleted_and_numbered_lists() {
    let c = "BT /F1 11 Tf 72 720 Td (Shopping list for the week:) Tj ET \
             BT /F1 11 Tf 72 700 Td (\\225) Tj 12 0 Td (Apples) Tj ET \
             BT /F1 11 Tf 72 686 Td (\\225) Tj 12 0 Td (Bread and butter) Tj ET \
             BT /F1 11 Tf 72 672 Td (\\225) Tj 12 0 Td (Cheese) Tj ET \
             BT /F1 11 Tf 72 640 Td (Then the steps to follow:) Tj ET \
             BT /F1 11 Tf 72 620 Td (1. Wash the apples) Tj ET \
             BT /F1 11 Tf 72 606 Td (2. Slice the bread) Tj ET";
    let (mut s, id) = open(pdf(&[page(c)], &[], ""));
    let r = autotag(&mut s, id, AutotagOptions::default());
    assert_eq!((r.count("L"), r.count("LI"), r.count("Lbl"), r.count("LBody")), (2, 5, 3, 5));
    assert_eq!(
        types(&s, id),
        ["Document", "P", "L", "LI", "Lbl", "LBody", "LI", "Lbl", "LBody", "LI", "Lbl", "LBody", "P", "L", "LI", "LBody", "LI", "LBody"]
    );
    let t = text(&s, id);
    in_order(&t, &["Shopping list", "Apples", "Bread and butter", "Cheese", "steps to follow", "1. Wash", "2. Slice"]);
    for r in [Rule::ListItems, Rule::LblLBody] {
        assert_eq!(rule(&s, id, r), Status::Passed, "{r:?}");
    }
    assert_eq!(ua_count(&s, id, "09-005"), 0);
    // Without list detection they are paragraphs.
    let (mut s, id) = open(pdf(&[page(c)], &[], ""));
    let r = autotag(&mut s, id, AutotagOptions { detect_lists: false, ..Default::default() });
    assert_eq!(r.count("L"), 0);
}

#[test]
fn a_ruled_table_becomes_rows_of_header_and_data_cells() {
    let mut c = String::from("BT /F1 11 Tf 72 730 Td (Staff by city) Tj ET 0.5 w ");
    for y in [700, 680, 660, 640] {
        c.push_str(&format!("72 {y} m 372 {y} l S "));
    }
    for x in [72, 172, 272, 372] {
        c.push_str(&format!("{x} 640 m {x} 700 l S "));
    }
    for (y, row) in [(686, ["Name", "Age", "City"]), (666, ["Ann", "34", "Oslo"]), (646, ["Bob", "51", "Rome"])] {
        for (k, cell) in row.iter().enumerate() {
            c.push_str(&format!("BT /F1 11 Tf {} {y} Td ({cell}) Tj ET ", 78 + 100 * k));
        }
    }
    c.push_str("BT /F1 11 Tf 72 610 Td (After the table.) Tj ET");
    let (mut s, id) = open(pdf(&[page(&c)], &[], ""));
    let r = autotag(&mut s, id, AutotagOptions::default());
    assert_eq!((r.count("Table"), r.count("TR"), r.count("TH"), r.count("TD")), (1, 3, 3, 6));
    let t = text(&s, id);
    assert!(t.contains("Name\tAge\tCity\nAnn\t34\tOslo\nBob\t51\tRome"), "{t:?}");
    in_order(&t, &["Staff by city", "Name", "Bob", "After the table."]);
    for r in [Rule::TableRows, Rule::TableCells, Rule::TableHeaders, Rule::TableRegularity, Rule::TaggedContent] {
        assert_eq!(rule(&s, id, r), Status::Passed, "{r:?}");
    }
    assert_eq!((ua_count(&s, id, "09-004"), ua_count(&s, id, "15-003")), (0, 0));
    // Without table detection the cells are paragraphs and the rules artifacts.
    let (mut s, id) = open(pdf(&[page(&c)], &[], ""));
    let r = autotag(&mut s, id, AutotagOptions { detect_tables: false, ..Default::default() });
    assert_eq!(r.count("Table"), 0);
    assert_eq!(rule(&s, id, Rule::TaggedContent), Status::Passed);
}

#[test]
fn images_become_figures_flagged_for_alternate_text() {
    let c = "BT /F1 11 Tf 72 720 Td (A photo follows.) Tj ET q 100 0 0 80 72 500 cm /Im1 Do Q \
             BT /F1 11 Tf 72 480 Td (Text below the photo.) Tj ET";
    let (mut s, id) = open(pdf(&[page(c)], &[], ""));
    let r = autotag(&mut s, id, AutotagOptions::default());
    assert_eq!(types(&s, id), ["Document", "P", "Figure", "P"]);
    let [f] = r.figures_needing_alt.as_slice() else { panic!("{:?}", r.figures_needing_alt) };
    assert_eq!(f.page, 0);
    let want = [72.0, 212.0, 172.0, 292.0];
    assert!(f.rect.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.5), "{:?}", f.rect);
    // The checker and PDF/UA flag the missing alternate text, and the figure is listed for it.
    assert_eq!(rule(&s, id, Rule::FiguresAltText), Status::Failed);
    assert_eq!(ua_count(&s, id, "13-003"), 1);
    let targets = s.get(id).unwrap().alt_targets();
    assert_eq!((targets.figures.len(), targets.images.len()), (1, 0));
    assert_eq!(targets.figures[0].obj.map(|o| o.num), Some(f.element.num));
    s.apply(id, Edit::SetAltText { figure: f.element.num, alt: Some("A mountain lake".into()) }).unwrap();
    assert_eq!(rule(&s, id, Rule::FiguresAltText), Status::Passed);
    in_order(&text(&s, id), &["A photo follows.", "A mountain lake", "Text below"]);
}

#[test]
fn running_headers_footers_and_page_numbers_become_artifacts() {
    let pages: Vec<String> = (1..=3)
        .map(|n| {
            format!(
                "BT /F1 9 Tf 72 760 Td (ACME Quarterly Review) Tj ET \
                 BT /F1 11 Tf 72 700 Td (Body text of page {n} goes here.) Tj ET \
                 BT /F1 9 Tf 300 30 Td (Page {n} of 3) Tj ET"
            )
        })
        .collect();
    let list: Vec<Page> = pages.iter().map(|c| page(c)).collect();
    let (mut s, id) = open(pdf(&list, &[], ""));
    let r = autotag(&mut s, id, AutotagOptions::default());
    assert_eq!(r.pagination_artifacts, 6);
    assert_eq!(r.pages, [0, 1, 2]);
    assert_eq!(r.count("P"), 3);
    let t = text(&s, id);
    assert!(!t.contains("ACME") && !t.contains("of 3"), "{t:?}");
    in_order(&t, &["page 1", "page 2", "page 3"]);
    assert_eq!(rule(&s, id, Rule::TaggedContent), Status::Passed);
    assert_eq!(ua_count(&s, id, "01-005"), 0);
    // A lone page number on a one-page document is an artifact too.
    let (mut s, id) = open(pdf(&[page("BT /F1 11 Tf 72 700 Td (Only page.) Tj ET BT /F1 9 Tf 300 30 Td (- 1 -) Tj ET")], &[], ""));
    let r = autotag(&mut s, id, AutotagOptions::default());
    assert_eq!((r.pagination_artifacts, r.count("P")), (1, 1));
}

#[test]
fn links_become_link_elements_with_object_references() {
    // Object 8: the link over "website"; 9: a sticky note; 10: a widget-less popup.
    let c = "BT /F1 11 Tf 72 700 Td (Visit our ) Tj (website) Tj ( for more.) Tj ET";
    let extra = [
        "<< /Type /Annot /Subtype /Link /Rect [118 696 160 710] /Border [0 0 0] /A << /S /URI /URI (https://example.com/) >> >>".to_string(),
        "<< /Type /Annot /Subtype /Text /Rect [500 700 520 720] /Contents (A note) >>".to_string(),
        "<< /Type /Annot /Subtype /Link /Rect [72 300 120 320] /A << /S /URI /URI (https://example.org/) >> >>".to_string(),
    ];
    let p = Page { content: c, extra: "/Annots [8 0 R 9 0 R 10 0 R]" };
    let (mut s, id) = open(pdf(&[p], &extra, ""));
    assert!(rule(&s, id, Rule::TaggedAnnotations) == Status::Failed);
    let r = autotag(&mut s, id, AutotagOptions::default());
    assert_eq!((r.links, r.annotations, r.count("Link"), r.count("Annot")), (2, 1, 2, 1));
    assert_eq!(r.link_descriptions, 2, "both links had no Contents");
    let tags = s.get(id).unwrap().tags();
    // The note (higher on the page) comes before the second link.
    assert_eq!(types(&s, id), ["Document", "P", "Link", "Annot", "Link"]);
    // The first link holds the word and its annotation, inside the paragraph.
    let link = &tags.nodes[2];
    assert_eq!(link.parent, Some(1));
    assert!(link.kids.iter().any(|k| matches!(k, a11y::TagKid::Content { .. })));
    assert!(link.kids.iter().any(|k| matches!(k, a11y::TagKid::Object { obj, subtype, .. } if obj.num == 8 && subtype.as_deref() == Some("Link"))));
    // The second has no text under it: only its annotation.
    assert!(matches!(tags.nodes[4].kids.as_slice(), [a11y::TagKid::Object { obj, .. }] if obj.num == 10));
    for r in [Rule::TaggedAnnotations, Rule::TabOrder, Rule::TaggedContent] {
        assert_eq!(rule(&s, id, r), Status::Passed, "{r:?}");
    }
    for cp in ["28-002", "28-008", "28-009", "28-011", "28-012", "parent-tree"] {
        assert_eq!(ua_count(&s, id, cp), 0, "{cp}");
    }
    assert_eq!(text(&s, id), "Visit our website for more.\n");
}

#[test]
fn only_the_chosen_pages_are_tagged() {
    let (mut s, id) = open(pdf(&[page(SINGLE), page(SINGLE)], &[], ""));
    let r = autotag(&mut s, id, AutotagOptions { pages: Some(vec![1]), ..Default::default() });
    assert_eq!(r.pages, [1]);
    let report = s.get(id).unwrap().accessibility_check(&Options::default()).unwrap();
    let findings = &report.result(Rule::TaggedContent).unwrap().findings;
    assert!(findings.iter().all(|f| f.page == Some(0)), "{findings:?}");
}

#[test]
fn hostile_content_never_panics() {
    let nested = "q ".repeat(6000) + &"Q ".repeat(6000);
    let marked = "/X BMC ".repeat(5000);
    let cases: Vec<String> = vec![
        // Leftover MCIDs without a tree, an unbalanced EMC, a property-list name, an artifact with an MCID.
        "EMC EMC /P << /MCID 4 >> BDC BT /F1 11 Tf 72 700 Td (Hello) Tj ET EMC /Artifact << /MCID 9 >> BDC 0 0 10 10 re f EMC /P /MC0 BDC (x) Tj EMC"
            .into(),
        nested,
        marked,
        // Absurd matrices, missing resources, paths with no current point, text outside BT.
        "1e308 0 0 1e308 0 0 cm BT /F9 1e30 Tf (Big) Tj ET /Nope Do 0 0 0 0 0 0 cm /Im1 Do l l c h f S sh (stray) Tj".into(),
        // A path broken by q, inline image, NaN-like operands, Tm with junk.
        "0 0 m q 10 10 l Q S BI /W 1 /H 1 /BPC 8 /CS /G ID \x7f EI BT /F1 11 Tf (a) (b) Tm 72 700 Td [(A) -250 (B)] TJ ET".into(),
        // Unreadable bytes: the page is left untagged.
        "BT /F1 11 Tf 72 700 Td (ok) Tj ET ) } ]".into(),
        // A grid of many thin lines with no text: not a table.
        (0..200).map(|i| format!("{} 100 m {} 600 l S 72 {} m 500 {} l S ", 72 + i, 72 + i, 100 + i, 100 + i)).collect(),
        String::new(),
    ];
    for (k, c) in cases.iter().enumerate() {
        let extra = [
            "<< /Subtype /Link /Rect [(a) /b null 1] >>".to_string(),
            "<< /Subtype /Widget /Rect [0 0 10 10] /StructParent 99 >>".to_string(),
            "42".to_string(),
        ];
        let p = Page { content: c, extra: "/Annots [8 0 R 9 0 R 10 0 R << /Subtype /Text /Rect [0 0 1 1] >> 999 0 R] /Rotate 90" };
        let (mut s, id) = open(pdf(&[p, page("BT /F1 11 Tf 72 700 Td (Second page.) Tj ET")], &extra, "/MarkInfo 5"));
        let options = AutotagOptions { lang: Some("de".into()), ..Default::default() };
        match s.get(id).unwrap().autotag_preview(&options) {
            Ok(r) => {
                s.apply(id, Edit::Autotag { options }).unwrap_or_else(|e| panic!("case {k}: {e}"));
                assert!(r.pages.contains(&1), "case {k}: {r:?}");
                let _ = s.get(id).unwrap().pdfua_verify(&UaOptions::default());
                let _ = s.get(id).unwrap().accessible_text();
                if k == 5 {
                    assert_eq!(r.skipped.len(), 1, "{r:?}");
                    assert!(r.warnings.iter().any(|w| w.contains("page 1 was left untagged")), "{r:?}");
                }
                if k == 6 {
                    assert_eq!(r.count("Table"), 0);
                }
            }
            Err(e) => panic!("case {k}: {e}"),
        }
    }
}

// ---- Form fields (a11y.autotag-fields) ------------------------------------------------------

/// A one-page form (objects 8…): `name` (text, with a tooltip), `email` (text, none), `agree`
/// (a checkbox labelled on its right) and `pay` (two radio buttons labelled on their right,
/// with a caption on the left), then a paragraph.
fn form_pdf() -> Vec<u8> {
    let c = "BT /F1 11 Tf 72 700 Td (Name:) Tj ET \
             BT /F1 11 Tf 72 670 Td (Email:) Tj ET \
             BT /F1 11 Tf 90 638 Td (I agree to the terms) Tj ET \
             BT /F1 11 Tf 72 608 Td (Payment:) Tj ET \
             BT /F1 11 Tf 156 608 Td (Card) Tj ET \
             BT /F1 11 Tf 216 608 Td (Cash) Tj ET \
             BT /F1 11 Tf 72 560 Td (After the form.) Tj ET";
    let extra = [
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /TU (Your full name) /Rect [120 695 300 712] /P 6 0 R >>".to_string(), // 8
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (email) /Rect [120 665 300 682] /P 6 0 R >>".to_string(),                     // 9
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [72 636 84 648] /AS /Off /AP << /N << /Yes 13 0 R /Off 13 0 R >> >> /P 6 0 R >>"
            .to_string(), // 10
        "<< /FT /Btn /Ff 49152 /T (pay) /Kids [12 0 R 14 0 R] >>".to_string(),                                                     // 11
        "<< /Type /Annot /Subtype /Widget /Parent 11 0 R /Rect [140 606 152 618] /AS /Off /AP << /N << /Card 13 0 R /Off 13 0 R >> >> /P 6 0 R >>"
            .to_string(), // 12
        "<< /Type /XObject /Subtype /Form /BBox [0 0 12 12] /Length 0 >>\nstream\n\nendstream".to_string(),                        // 13
        "<< /Type /Annot /Subtype /Widget /Parent 11 0 R /Rect [200 606 212 618] /AS /Off /AP << /N << /Cash 13 0 R /Off 13 0 R >> >> /P 6 0 R >>"
            .to_string(), // 14
    ];
    // Listed out of reading order.
    let p = Page { content: c, extra: "/Annots [14 0 R 10 0 R 8 0 R 12 0 R 9 0 R]" };
    pdf(&[p], &extra, "/Lang (en) /AcroForm << /Fields [8 0 R 9 0 R 10 0 R 11 0 R] >>")
}

/// The alternate text of each Form element, in document order.
fn form_alts(s: &Session, id: DocId) -> Vec<String> {
    s.get(id).unwrap().tags().nodes.iter().filter(|n| n.ty == "Form").map(|n| n.alt.clone().unwrap_or_default()).collect()
}

/// The structure type of each Form element's parent.
fn form_parents(s: &Session, id: DocId) -> Vec<String> {
    let tags = s.get(id).unwrap().tags();
    tags.nodes.iter().filter(|n| n.ty == "Form").map(|n| n.parent.map(|p| tags.nodes[p].ty.clone()).unwrap_or_default()).collect()
}

#[test]
fn form_fields_become_form_elements_next_to_their_labels() {
    let (mut s, id) = open(form_pdf());
    for r in [Rule::TaggedFormFields, Rule::FieldDescriptions] {
        assert_eq!(rule(&s, id, r), Status::Failed, "{r:?} before");
    }
    assert!(ua_count(&s, id, "28-010") > 0);
    let r = autotag(&mut s, id, AutotagOptions::default());
    assert_eq!(r.count("Form"), 5);
    assert_eq!(r.fields.len(), 5);
    // Each widget sits with its label: after "Name:" and "Email:", before "I agree…", "Card", "Cash".
    assert_eq!(
        types(&s, id),
        ["Document", "P", "Form", "P", "Form", "P", "Form", "P", "P", "Form", "P", "Form", "P"],
        "{:?}",
        s.get(id).unwrap().tags().nodes
    );
    assert_eq!(form_alts(&s, id), ["Your full name", "email", "agree", "pay: Card", "pay: Cash"]);
    let by_name = |n: &str| r.fields.iter().find(|f| f.name == n).unwrap();
    assert_eq!((by_name("name").kind.as_str(), by_name("name").alt_source.as_deref()), ("text", Some("tooltip")));
    assert_eq!((by_name("email").alt_source.as_deref(), by_name("email").label.as_deref()), (Some("name"), Some("Email")));
    assert_eq!((by_name("agree").kind.as_str(), by_name("agree").label.as_deref()), ("checkbox", Some("I agree to the terms")));
    let radios: Vec<_> = r.fields.iter().filter(|f| f.name == "pay").collect();
    assert_eq!(radios.len(), 2);
    assert!(radios.iter().all(|f| f.kind == "radio" && f.field.num == 11), "{radios:?}");
    // Fields without a tooltip, with suggestions from their labels.
    let needing: Vec<(&str, Option<&str>, bool)> =
        r.fields_needing_tooltip.iter().map(|f| (f.name.as_str(), f.suggested.as_deref(), f.filled)).collect();
    assert_eq!(needing, [("email", Some("Email"), false), ("agree", Some("I agree to the terms"), false), ("pay", Some("Payment"), false)]);
    assert_eq!(r.tooltips_filled, 0);
    assert!(r.warnings.iter().any(|w| w.contains("no tooltip")), "{:?}", r.warnings);
    // The reading order holds each field's description right next to its label.
    in_order(
        &text(&s, id),
        &["Name:", "Your full name", "Email:", "email", "agree", "I agree", "Payment", "pay: Card", "Card", "pay: Cash", "Cash", "After the form."],
    );
    // The checker: fields are tagged and tab in structure order; descriptions are still missing.
    for r in [Rule::TaggedFormFields, Rule::TaggedAnnotations, Rule::TabOrder, Rule::TaggedContent] {
        assert_eq!(rule(&s, id, r), Status::Passed, "{r:?}");
    }
    assert_eq!(rule(&s, id, Rule::FieldDescriptions), Status::Failed);
    // PDF/UA: widgets in Form elements, with alternate text, in the parent tree.
    for cp in ["28-005", "28-008", "28-009", "28-010", "parent-tree", "01-005", "struct-tree-shape"] {
        assert_eq!(ua_count(&s, id, cp), 0, "{cp}");
    }
    // Every widget has a StructParent, and the field tree is untouched.
    let doc = s.get(id).unwrap();
    for f in &r.fields {
        assert!(doc.form.iter().any(|x| x.name == f.name), "{} lost from the form", f.name);
    }
}

#[test]
fn fill_writes_suggested_tooltips() {
    let (mut s, id) = open(form_pdf());
    let r = autotag(&mut s, id, AutotagOptions { field_tooltips: a11y::FieldTooltips::Fill, ..Default::default() });
    assert_eq!(r.tooltips_filled, 3);
    assert!(r.fields_needing_tooltip.iter().all(|f| f.filled));
    assert_eq!(form_alts(&s, id), ["Your full name", "Email", "I agree to the terms", "Payment: Card", "Payment: Cash"]);
    assert!(r.fields.iter().filter(|f| f.name != "name").all(|f| f.alt_source.as_deref() == Some("suggested")));
    for r in [Rule::FieldDescriptions, Rule::TaggedFormFields] {
        assert_eq!(rule(&s, id, r), Status::Passed, "{r:?}");
    }
    let tips: Vec<(String, Option<String>)> = s.get(id).unwrap().form.iter().map(|f| (f.name.clone(), f.tooltip.clone())).collect();
    assert!(tips.contains(&("email".into(), Some("Email".into()))), "{tips:?}");
    assert!(tips.contains(&("pay".into(), Some("Payment".into()))), "{tips:?}");
    // Undo takes the tooltips back with the tags.
    s.undo(id).unwrap();
    assert_eq!(rule(&s, id, Rule::FieldDescriptions), Status::Failed);
    assert!(!s.get(id).unwrap().tags().exists);
}

#[test]
fn form_tags_survive_undo_redo_save_and_reload() {
    let (mut s, id) = open(form_pdf());
    autotag(&mut s, id, AutotagOptions::default());
    let (tys, alts) = (types(&s, id), form_alts(&s, id));
    s.undo(id).unwrap();
    assert_eq!(rule(&s, id, Rule::TaggedFormFields), Status::Failed);
    s.redo(id).unwrap();
    assert_eq!((types(&s, id), form_alts(&s, id)), (tys.clone(), alts.clone()));
    let saved = s.save_bytes(id).unwrap();
    let (mut s2, id2) = open(saved.to_vec());
    assert_eq!((types(&s2, id2), form_alts(&s2, id2)), (tys, alts));
    assert_eq!(rule(&s2, id2, Rule::TaggedFormFields), Status::Passed);
    for cp in ["28-010", "parent-tree"] {
        assert_eq!(ua_count(&s2, id2, cp), 0, "{cp}");
    }
    // Retagging the reloaded document replaces the widgets' StructParents cleanly.
    let r = autotag(&mut s2, id2, AutotagOptions { replace_existing: true, ..Default::default() });
    assert_eq!(r.count("Form"), 5);
    assert_eq!(ua_count(&s2, id2, "parent-tree"), 0);
    // Without field detection the widgets are left untagged (and lose their old StructParent).
    let r = autotag(&mut s2, id2, AutotagOptions { replace_existing: true, detect_fields: false, ..Default::default() });
    assert_eq!((r.count("Form"), r.fields.len()), (0, 0));
    assert_eq!(rule(&s2, id2, Rule::TaggedFormFields), Status::Failed);
    assert_eq!(ua_count(&s2, id2, "parent-tree"), 0);
}

#[test]
fn fields_in_two_columns_read_down_each_column() {
    // Labels above their fields, in two columns; one unlabelled field at the top of the page and
    // one at the bottom of the right column.
    let c = "BT /F1 18 Tf 72 730 Td (Two Column Form) Tj ET \
             BT /F1 11 Tf 320 700 Td (City) Tj ET \
             BT /F1 11 Tf 320 660 Td (Country) Tj ET \
             BT /F1 11 Tf 72 700 Td (First name) Tj ET \
             BT /F1 11 Tf 72 660 Td (Last name) Tj ET";
    let w = |t: &str, tu: &str, r: &str| format!("<< /Type /Annot /Subtype /Widget /FT /Tx /T ({t}) /TU ({tu}) /Rect [{r}] /P 6 0 R >>");
    let extra = [
        w("city", "City field", "320 680 500 696"),       // 8
        w("country", "Country field", "320 640 500 656"), // 9
        w("first", "First name field", "72 680 250 696"), // 10
        w("last", "Last name field", "72 640 250 656"),   // 11
        w("notes", "Notes field", "320 560 500 576"),     // 12: below Country, nothing near
        w("stamp", "Stamp field", "72 770 200 786"),      // 13: above everything
    ];
    let p = Page { content: c, extra: "/Annots [8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 13 0 R]" };
    let (mut s, id) = open(pdf(&[p], &extra, "/Lang (en) /AcroForm << /Fields [8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 13 0 R] >>"));
    let r = autotag(&mut s, id, AutotagOptions::default());
    assert_eq!(r.count("Form"), 6);
    assert!(r.fields_needing_tooltip.is_empty());
    assert_eq!(
        form_alts(&s, id),
        ["Stamp field", "First name field", "Last name field", "City field", "Country field", "Notes field"],
        "{:?}",
        s.get(id).unwrap().tags().nodes
    );
    // Labelled fields go inside their label's paragraph; the others stand on their own.
    assert_eq!(form_parents(&s, id), ["Document", "P", "P", "P", "P", "Document"]);
    in_order(
        &text(&s, id),
        &[
            "Stamp field",
            "Two Column Form",
            "First name",
            "First name field",
            "Last name",
            "Last name field",
            "City",
            "City field",
            "Country",
            "Country field",
            "Notes field",
        ],
    );
    let label = |n: &str| r.fields.iter().find(|f| f.name == n).and_then(|f| f.label.clone());
    assert_eq!((label("first").as_deref(), label("notes")), (Some("First name"), None));
    assert_eq!(rule(&s, id, Rule::TabOrder), Status::Passed);
    assert_eq!(ua_count(&s, id, "28-010"), 0);
}

#[test]
fn a_field_labelled_by_a_heading_goes_after_it() {
    let c = "BT /F1 20 Tf 72 700 Td (Comments) Tj ET BT /F1 11 Tf 72 600 Td (Thank you.) Tj ET";
    let extra = ["<< /Type /Annot /Subtype /Widget /FT /Tx /T (comments) /Rect [72 640 400 690] /P 6 0 R >>".to_string()];
    let p = Page { content: c, extra: "/Annots [8 0 R]" };
    let (mut s, id) = open(pdf(&[p], &extra, "/AcroForm << /Fields [8 0 R] >>"));
    let r = autotag(&mut s, id, AutotagOptions::default());
    assert_eq!(types(&s, id), ["Document", "H1", "Form", "P"]);
    assert_eq!(r.fields_needing_tooltip.first().and_then(|f| f.suggested.as_deref()), Some("Comments"));
}

#[test]
fn hostile_field_trees_never_panic() {
    let c = "BT /F1 11 Tf 72 700 Td (Label:) Tj ET";
    // Two pages, so the extra objects start at 10. 10 ↔ 11: a /Parent cycle; 12 ↔ 13: a /Kids
    // cycle (13 is also a widget); 14: junk Rect, T, TU and FT, and a font as its Parent; 10 is listed
    // twice and also on page 2.
    let extra = [
        "<< /Type /Annot /Subtype /Widget /Parent 11 0 R /Rect [120 695 300 712] /StructParent 7 >>".to_string(), // 10
        "<< /Parent 10 0 R /Kids [10 0 R 11 0 R] >>".to_string(),                                                 // 11
        "<< /FT /Btn /Ff 32768 /T (loop) /Kids [13 0 R] /Parent 13 0 R >>".to_string(),                           // 12
        "<< /Type /Annot /Subtype /Widget /Parent 12 0 R /Kids [12 0 R] /Rect [72 600 84 612] /AP << /N 5 >> >>".to_string(), // 13
        "<< /Type /Annot /Subtype /Widget /Parent 3 0 R /T 7 /TU [1 2] /Rect [(a) /b 1e400 null] /FT 4 >>".to_string(), // 14
    ];
    let annots = "/Annots [10 0 R 10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 99 0 R << /Subtype /Widget /Rect [0 0 1 1] >>]";
    let catalog = "/AcroForm << /Fields [11 0 R 12 0 R 14 0 R 12 0 R] >>";
    for fill in [a11y::FieldTooltips::Report, a11y::FieldTooltips::Fill] {
        let p = Page { content: c, extra: annots };
        let p2 = Page { content: "BT /F1 11 Tf 72 700 Td (Second page.) Tj ET", extra: "/Annots [10 0 R 13 0 R]" };
        let (mut s, id) = open(pdf(&[p, p2], &extra, catalog));
        let r = autotag(&mut s, id, AutotagOptions { field_tooltips: fill, ..Default::default() });
        // Each widget once, wherever it is listed.
        let mut widgets: Vec<u32> = r.fields.iter().map(|f| f.widget.num).collect();
        widgets.sort_unstable();
        assert_eq!(widgets, [10, 13, 14], "{r:?}");
        assert!(r.warnings.iter().any(|w| w.contains("inside the page's /Annots")), "{:?}", r.warnings);
        let _ = s.get(id).unwrap().pdfua_verify(&UaOptions::default());
        let _ = s.get(id).unwrap().accessibility_check(&Options::default());
        let _ = s.get(id).unwrap().accessible_text();
        assert_eq!(ua_count(&s, id, "parent-tree"), 0);
        let saved = s.save_bytes(id).unwrap();
        let (s2, id2) = open(saved.to_vec());
        assert_eq!(s2.get(id2).unwrap().tags().nodes.iter().filter(|n| n.ty == "Form").count(), 3);
    }
}

#[test]
fn thousands_of_widgets_are_tagged_within_bounds() {
    let n = 3000;
    let mut extra = Vec::new();
    let mut refs = Vec::new();
    for i in 0..n {
        let (x, y) = (20 + (i % 50) * 11, 20 + (i / 50) * 11);
        extra.push(format!("<< /Type /Annot /Subtype /Widget /FT /Btn /T (c{i}) /Rect [{x} {y} {} {}] >>", x + 10, y + 10));
        refs.push(format!("{} 0 R", 8 + i));
    }
    let annots = format!("/Annots [{}]", refs.join(" "));
    let p = Page { content: "BT /F1 11 Tf 72 760 Td (Many boxes) Tj ET", extra: &annots };
    let (mut s, id) = open(pdf(&[p], &extra, ""));
    let r = autotag(&mut s, id, AutotagOptions::default());
    assert_eq!((r.count("Form"), r.fields.len(), r.fields_needing_tooltip.len()), (n, n, n));
    assert_eq!(ua_count(&s, id, "28-010"), 0);
    assert_eq!(ua_count(&s, id, "parent-tree"), 0);
}
