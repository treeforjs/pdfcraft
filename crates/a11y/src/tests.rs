use std::sync::Arc;

use pdfcraft_cos::Document;

use super::*;

/// A PDF from object bodies (object n = bodies[n - 1]) and extra trailer entries.
fn pdf(bodies: &[String], trailer: &str) -> Document {
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
    out.extend_from_slice(format!("trailer << /Size {} /Root 1 0 R {trailer} >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

fn stream(data: &str) -> String {
    format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len())
}

fn status(r: &Report, rule: Rule) -> Status {
    r.result(rule).unwrap().status
}

fn failed(r: &Report) -> Vec<Rule> {
    r.results.iter().filter(|x| x.status == Status::Failed).map(|x| x.rule).collect()
}

const FONT: &str = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";

#[test]
fn an_untagged_document_fails_the_document_rules() {
    let doc = pdf(
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
            stream("BT /F1 12 Tf 20 100 Td (Hello) Tj ET 0 0 50 50 re f"),
            FONT.into(),
        ],
        "",
    );
    let r = check(&doc, &Options::default());
    assert_eq!(r.results.len(), 32);
    assert_eq!(failed(&r), [Rule::TaggedPdf, Rule::PrimaryLanguage, Rule::Title, Rule::TaggedContent]);
    assert_eq!(status(&r, Rule::ColorContrast), Status::Skipped, "off by default");
    assert_eq!(status(&r, Rule::LogicalReadingOrder), Status::Manual);
    assert_eq!(status(&r, Rule::ImageOnly), Status::Passed);
    assert_eq!(r.result(Rule::TaggedPdf).unwrap().findings.len(), 2);
    // Only the chosen rules run.
    let only = check(&doc, &Options { rules: [Rule::Title].into(), pages: None });
    assert_eq!((only.count(Status::Failed), only.count(Status::Skipped)), (1, 31));
}

/// A tagged document that passes every automatic rule.
fn good() -> Vec<String> {
    vec![
        // 1 catalog, 2 pages, 3 page, 4 content, 5 font
        "<< /Type /Catalog /Pages 2 0 R /Lang (en-US) /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R /ViewerPreferences << /DisplayDocTitle true >> >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Tabs /S /Annots [9 0 R 10 0 R] /StructParents 0 \
         /Resources << /Font << /F1 5 0 R >> /XObject << /Im 11 0 R >> >> >>"
            .into(),
        stream(
            "/H1 << /MCID 0 >> BDC BT /F1 12 Tf 20 180 Td (Title) Tj ET EMC \
             /Figure << /MCID 1 >> BDC q 10 0 0 10 20 120 cm /Im Do Q EMC \
             /Artifact BMC 0 0 200 2 re f EMC \
             /TH << /MCID 2 >> BDC BT (A) Tj ET EMC /TH << /MCID 3 >> BDC BT (B) Tj ET EMC \
             /TD << /MCID 4 >> BDC BT (1) Tj ET EMC /TD << /MCID 5 >> BDC BT (2) Tj ET EMC \
             /Lbl << /MCID 6 >> BDC BT (1.) Tj ET EMC /LBody << /MCID 7 >> BDC BT (Item) Tj ET EMC",
        ),
        FONT.into(),
        // 6 struct tree root, 7 Document element, 8 info
        "<< /Type /StructTreeRoot /K 7 0 R /RoleMap << /Heading1 /H1 >> >>".into(),
        "<< /S /Document /P 6 0 R /Pg 3 0 R /K [\
         << /S /Heading1 /Pg 3 0 R /K 0 >> \
         << /S /Figure /Pg 3 0 R /Alt (A red square) /K 1 >> \
         << /S /Table /Pg 3 0 R /A << /O /Table /Summary (Two columns) >> /K [\
            << /S /TR /K [ << /S /TH /K 2 >> << /S /TH /K 3 >> ] >> \
            << /S /TR /K [ << /S /TD /K 4 >> << /S /TD /K 5 >> ] >> ] >> \
         << /S /L /K [ << /S /LI /K [ << /S /Lbl /K 6 >> << /S /LBody /K 7 >> ] >> ] >> \
         << /S /Link /K << /Type /OBJR /Obj 9 0 R >> >> \
         << /S /Form /K << /Type /OBJR /Obj 10 0 R >> >> ] >>"
            .into(),
        "<< /Title (Quarterly report) >>".into(),
        // 9 link, 10 field, 11 image
        "<< /Type /Annot /Subtype /Link /Rect [20 20 80 40] /StructParent 1 >>".into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /TU (Your name) /Rect [20 60 120 80] /StructParent 2 >>".into(),
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n\u{0}\nendstream"
            .into(),
    ]
}

#[test]
fn a_well_tagged_document_passes() {
    let doc = pdf(&good(), "/Info 8 0 R");
    let r = check(&doc, &Options::default());
    assert!(failed(&r).is_empty(), "{:?}", r.results.iter().filter(|x| x.status == Status::Failed).collect::<Vec<_>>());
    assert_eq!((r.count(Status::Manual), r.count(Status::Skipped), r.count(Status::Passed)), (5, 1, 26));
}

#[test]
fn structure_problems_are_found() {
    let mut objs = good();
    objs[6] = "<< /S /Document /P 6 0 R /Pg 3 0 R /K [\
         << /S /H1 /K 0 >> << /S /H3 /K 2 >> \
         << /S /Figure /K 1 >> \
         << /S /Sect /Alt (Outer) /K [ << /S /P /Alt (Inner) /K 3 >> ] >> \
         << /S /Div /Alt (Nothing) >> \
         << /S /P /Alt (Over a link) /K << /Type /OBJR /Obj 9 0 R >> >> \
         << /S /Formula /K 4 >> \
         << /S /Table /K [ << /S /TR /K [ << /S /TD /K 5 >> << /S /TD >> ] >> << /S /TR /K [ << /S /TD /K 6 >> ] >> ] >> \
         << /S /TD /K 7 >> << /S /LI >> << /S /LBody >> << /S /TR >> ] >>"
        .into();
    let doc = pdf(&objs, "/Info 8 0 R");
    let r = check(&doc, &Options::default());
    let mut want = vec![
        Rule::FiguresAltText,
        Rule::NestedAltText,
        Rule::AltTextAssociated,
        Rule::AltTextHidesAnnotation,
        Rule::OtherElementsAltText,
        Rule::TableRows,
        Rule::TableCells,
        Rule::TableHeaders,
        Rule::TableRegularity,
        Rule::TableSummary,
        Rule::ListItems,
        Rule::LblLBody,
        Rule::HeadingNesting,
    ];
    want.sort();
    let mut got = failed(&r);
    got.sort();
    assert_eq!(got, want);
    let h = &r.result(Rule::HeadingNesting).unwrap().findings[0].message;
    assert!(h.contains("H3") && h.contains("H1"), "{h}");
    assert!(r.result(Rule::TableRegularity).unwrap().findings[0].message.contains("between 1 and 2"));
}

#[test]
fn row_and_column_spans_keep_a_table_regular() {
    let mut objs = good();
    // Row 1: a cell spanning two rows and one spanning two columns; row 2 then has one cell.
    objs[6] = "<< /S /Document /P 6 0 R /K [ << /S /Table /A << /O /Table /Summary (s) >> /K [\
         << /S /TR /K [ << /S /TH /A << /O /Table /RowSpan 2 >> /K 0 >> << /S /TH /A [ << /O /Table /ColSpan 2 >> ] /K 1 >> ] >> \
         << /S /TR /K [ << /S /TD /K 2 >> << /S /TD /K 3 >> ] >> ] >> ] >>"
        .into();
    let r = check(&pdf(&objs, "/Info 8 0 R"), &Options { rules: [Rule::TableRegularity].into(), pages: None });
    assert_eq!(status(&r, Rule::TableRegularity), Status::Passed, "{:?}", r.result(Rule::TableRegularity));
}

#[test]
fn page_content_and_annotation_problems_are_found() {
    let mut objs = good();
    // Untagged text, no tab order, an untagged link and a field without a tooltip.
    objs[2] = objs[2].replace("/Tabs /S ", "");
    objs[3] = stream("BT /F1 12 Tf (stray) Tj ET /P << /MCID 0 >> BDC BT (ok) Tj ET EMC");
    objs[8] = "<< /Type /Annot /Subtype /Link /Rect [20 20 80 40] >>".into();
    objs[9] = "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [20 60 120 80] /StructParent 2 >>".into();
    let r = check(&pdf(&objs, "/Info 8 0 R"), &Options::default());
    let mut got = failed(&r);
    got.sort();
    assert_eq!(got, [Rule::TaggedContent, Rule::TaggedAnnotations, Rule::TabOrder, Rule::FieldDescriptions]);
    assert_eq!(r.result(Rule::TaggedContent).unwrap().findings[0].message, "Page 1: 1 untagged content item");
    assert!(r.result(Rule::FieldDescriptions).unwrap().findings[0].message.contains("\"name\""));
}

#[test]
fn scans_identity_fonts_and_long_documents() {
    // An image-only page.
    let doc = pdf(
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im 5 0 R >> >> >>".into(),
            stream("q 200 0 0 200 0 0 cm /Im Do Q"),
            "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n\u{0}\nendstream"
                .into(),
        ],
        "",
    );
    assert_eq!(status(&check(&doc, &Options::default()), Rule::ImageOnly), Status::Failed);
    // An Identity-H font without ToUnicode; 21 pages without bookmarks.
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{}] /Count 21 >>", (0..21).map(|i| format!("{} 0 R", 6 + i)).collect::<Vec<_>>().join(" ")),
        stream("BT /F1 12 Tf <0001> Tj ET"),
        "<< /Type /Font /Subtype /Type0 /BaseFont /Custom /Encoding /Identity-H /DescendantFonts [5 0 R] >>".into(),
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Custom /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>".into(),
    ];
    for _ in 0..21 {
        objs.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>".into());
    }
    let r = check(&pdf(&objs, ""), &Options::default());
    assert_eq!(status(&r, Rule::Bookmarks), Status::Failed);
    let enc = r.result(Rule::CharacterEncoding).unwrap();
    assert_eq!((enc.status, enc.findings.len()), (Status::Failed, 1), "each font once");
    assert!(enc.findings[0].message.contains("Custom"));
    // A page range limits the page rules.
    let some = check(&pdf(&objs, ""), &Options { pages: Some(vec![3]), ..Options::default() });
    assert_eq!(some.result(Rule::CharacterEncoding).unwrap().findings[0].page, Some(3));
}

#[test]
fn rules_have_ids_and_the_report_lists_them() {
    assert_eq!(Rule::ALL.len(), 32);
    for r in Rule::ALL {
        assert_eq!(Rule::from_id(r.id()), Some(r));
    }
    let per: Vec<usize> = Category::ALL.iter().map(|c| Rule::ALL.iter().filter(|r| r.category() == *c).count()).collect();
    assert_eq!(per, [8, 9, 2, 5, 5, 2, 1]);
    let doc = pdf(&good(), "/Info 8 0 R");
    let html = report_html(&check(&doc, &Options::default()), "a<b>.pdf", "2026-10-02");
    assert!(html.contains("a&lt;b&gt;.pdf") && html.contains("Accessibility permission flag") && html.contains("Needs manual check: 5"));
    assert!(html.contains("<h3>Headings</h3>"));
}

fn figures_doc() -> Document {
    pdf(
        &[
            "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 5 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /StructParents 0 /Resources << /XObject << /Im 10 0 R >> >> >>"
                .into(),
            stream(
                "/Figure << /MCID 0 >> BDC q 40 0 0 20 10 150 cm /Im Do Q EMC \
                 /Photo << /MCID 1 >> BDC 100 100 50 30 re f EMC",
            ),
            "<< /Type /StructTreeRoot /K 6 0 R /ParentTree 9 0 R /RoleMap << /Photo /Figure >> >>".into(),
            "<< /S /Document /P 5 0 R /K [7 0 R 8 0 R] >>".into(),
            "<< /S /Figure /P 6 0 R /Pg 3 0 R /K 0 >>".into(),
            "<< /S /Photo /P 6 0 R /Pg 3 0 R /Alt (A bar) /K 1 >>".into(),
            "<< /Nums [0 [7 0 R 8 0 R]] >>".into(),
            "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n\u{0}\nendstream"
                .into(),
        ],
        "",
    )
}

#[test]
fn figures_are_listed_with_their_alt_text_and_place() {
    let doc = figures_doc();
    let f = figures(&doc);
    assert_eq!(f.len(), 2, "role-mapped Photo counts");
    assert_eq!((f[0].page, f[0].alt.as_deref(), f[0].bbox), (Some(0), None, Some([10.0, 150.0, 50.0, 170.0])));
    assert_eq!((f[1].alt.as_deref(), f[1].bbox), (Some("A bar"), Some([100.0, 100.0, 150.0, 130.0])));
}

#[test]
fn setting_alt_text_and_marking_decorative() {
    let mut doc = figures_doc();
    let f = figures(&doc);
    set_alt(&mut doc, f[0].obj, Some("  A grey square ")).unwrap();
    assert_eq!(figures(&doc)[0].alt.as_deref(), Some("A grey square"));
    let r = check(&doc, &Options { rules: [Rule::FiguresAltText].into(), pages: None });
    assert_eq!(r.result(Rule::FiguresAltText).unwrap().status, Status::Passed);
    set_alt(&mut doc, f[0].obj, None).unwrap();
    assert_eq!(figures(&doc)[0].alt, None);
    assert_eq!(set_alt(&mut doc, pdfcraft_cos::ObjRef::new(6, 0), Some("x")), Err(AltError::NotAFigure(6)));
    // Decorative: the figure leaves the tags and its content becomes an artifact.
    mark_decorative(&mut doc, f[0].obj).unwrap();
    let left = figures(&doc);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].alt.as_deref(), Some("A bar"));
    let r = check(&doc, &Options { rules: [Rule::FiguresAltText, Rule::TaggedContent].into(), pages: None });
    assert_eq!(failed(&r), Vec::<Rule>::new(), "{:?}", r.results);
    let pages = pdfcraft_model::pages(&doc);
    let c = doc.resolve(pages[0].dict.get(b"Contents").unwrap());
    let pdfcraft_cos::Object::Stream(c) = &*c else { panic!("one content stream") };
    let text = String::from_utf8(c.decoded().unwrap()).unwrap();
    assert!(text.contains("/Artifact BMC") && !text.contains("/MCID 0"), "{text}");
}

/// A tagged page: Document > H1 (MCID 0), P (an MCR for MCID 1), Photo (role-mapped to
/// Figure, MCID 2, no alt), Link (an OBJR to the page's link annotation); a cycle back to the
/// Document element; an image (Im1) painted outside marked content, one inside an Artifact, and
/// an inline image outside marked content.
fn tags_doc() -> Document {
    let content = "/H1 << /MCID 0 >> BDC BT /F1 12 Tf 20 180 Td (Title) Tj ET EMC \
        /P << /MCID 1 >> BDC BT /F1 10 Tf 20 160 Td (Body) Tj ET EMC \
        /Figure << /MCID 2 >> BDC q 40 0 0 30 20 100 cm /Im1 Do Q EMC \
        q 50 0 0 40 100 40 cm /Im1 Do Q \
        /Artifact BMC q 10 0 0 10 0 0 cm /Im1 Do Q EMC \
        q 20 0 0 20 150 150 cm BI /W 1 /H 1 /BPC 8 /CS /G ID \x7f EI Q";
    pdf(
        &[
            "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R /Lang (en-US) >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /StructParents 0 /Tabs /S \
              /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 13 0 R >> >> /Annots [12 0 R] >>"
                .into(),
            stream(content),
            FONT.into(),
            "<< /Type /StructTreeRoot /K 7 0 R /ParentTree 14 0 R /ParentTreeNextKey 2 /RoleMap << /Photo /Figure /Loop /Loop2 /Loop2 /Loop >> >>"
                .into(),
            "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R 10 0 R 11 0 R 7 0 R] /Lang (en) >>".into(),
            "<< /Type /StructElem /S /H1 /P 7 0 R /Pg 3 0 R /K 0 /T (Heading) >>".into(),
            "<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K << /Type /MCR /MCID 1 >> >>".into(),
            "<< /Type /StructElem /S /Photo /P 7 0 R /Pg 3 0 R /K [2] >>".into(),
            "<< /Type /StructElem /S /Link /P 7 0 R /Pg 3 0 R /K [<< /Type /OBJR /Obj 12 0 R >> 10 0 R] >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [20 150 60 170] /StructParent 1 >>".into(),
            "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x7f\nendstream"
                .into(),
            "<< /Nums [0 [8 0 R 9 0 R 10 0 R] 1 11 0 R] >>".into(),
        ],
        "",
    )
}

fn reopen(doc: &Document) -> Document {
    let bytes = pdfcraft_cos::write_incremental(doc, &Default::default()).unwrap();
    assert!(bytes.starts_with(doc.bytes()), "incremental save keeps the original bytes");
    Document::open(Arc::new(bytes)).unwrap()
}

#[test]
fn the_tags_tree_reads_elements_content_and_object_references() {
    let doc = tags_doc();
    let t = tags(&doc);
    assert!(t.exists && t.marked && !t.truncated);
    assert_eq!(t.lang.as_deref(), Some("en-US"));
    let types: Vec<(&str, Option<&str>, usize)> = t.nodes.iter().map(|n| (n.ty.as_str(), n.standard.as_deref(), n.depth)).collect();
    // The Document's kid 7 0 R (itself) and the Link's second reference to the Photo are a
    // cycle and a repeat: each element is read once.
    assert_eq!(
        types,
        [("Document", Some("Document"), 0), ("H1", Some("H1"), 1), ("P", Some("P"), 1), ("Photo", Some("Figure"), 1), ("Link", Some("Link"), 1)]
    );
    assert_eq!(t.roots, [0]);
    assert_eq!(t.nodes[0].lang.as_deref(), Some("en"));
    assert_eq!(t.nodes[1].title.as_deref(), Some("Heading"));
    assert_eq!(t.nodes[1].kids, [TagKid::Content { page: Some(0), mcid: 0, stream: None }]);
    assert_eq!(t.nodes[2].kids, [TagKid::Content { page: Some(0), mcid: 1, stream: None }]);
    assert!(matches!(&t.nodes[4].kids[..], [TagKid::Object { subtype: Some(s), page: Some(0), .. }] if s == "Link"));
    assert_eq!(t.nodes[0].kids.len(), 4);
    assert_eq!(t.role_map.len(), 3);
    assert_eq!(t.find(pdfcraft_cos::ObjRef::new(10, 0)), Some(3));
    assert!(!t.covered_by_alternate(3));
    // A role-map cycle maps to no standard type, so it can't be chosen.
    let allowed = allowed_types(&doc);
    assert!(allowed.iter().any(|a| a == "Photo") && !allowed.iter().any(|a| a == "Loop" || a == "Artifact"));
    // No tree, no nodes.
    let untagged = pdf(&["<< /Type /Catalog /Pages 2 0 R >>".into(), "<< /Type /Pages /Kids [] /Count 0 >>".into()], "");
    assert_eq!(tags(&untagged), Tags::default());
}

#[test]
fn a_hostile_tree_is_read_bounded() {
    // A 3000-deep chain of direct elements, and a kids array that contains itself.
    let mut deep = String::from("<< /S /Span /K 0 >>");
    for _ in 0..3000 {
        deep = format!("<< /S /Span /K {deep} >>");
    }
    let doc = pdf(
        &[
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 3 0 R >>".into(),
            "<< /Type /Pages /Kids [] /Count 0 >>".into(),
            "<< /Type /StructTreeRoot /K [4 0 R 5 0 R] >>".into(),
            deep,
            "<< /S /Div /K 6 0 R >>".into(),
            "[6 0 R 5 0 R << /S /P >>]".into(),
        ],
        "",
    );
    let t = tags(&doc);
    assert!(t.nodes.iter().any(|n| n.ty == "Div") && t.nodes.iter().any(|n| n.ty == "P"), "{} nodes", t.nodes.len());
    assert!(t.nodes.len() <= 1024, "{} nodes", t.nodes.len());
}

#[test]
fn tag_properties_change_atomically_and_validate_types() {
    let mut doc = tags_doc();
    let photo = pdfcraft_cos::ObjRef::new(10, 0);
    let p = pdfcraft_cos::ObjRef::new(9, 0);
    // The checker fails the role-mapped figure without alternate text.
    assert_eq!(status(&check(&doc, &Options::default()), Rule::FiguresAltText), Status::Failed);
    // Invalid changes change nothing.
    let bad_type = TagChange { ty: Some("Bogus".into()), ..Default::default() };
    assert!(matches!(set_tag(&mut doc, p, &bad_type), Err(TagError::Invalid(_))));
    let bad_lang = TagChange { alt: Some(Some("x".into())), lang: Some(Some("en US".into())), ..Default::default() };
    assert!(matches!(set_tag(&mut doc, photo, &bad_lang), Err(TagError::Invalid(_))));
    assert!(matches!(set_tag(&mut doc, photo, &TagChange { ty: Some("Artifact".into()), ..Default::default() }), Err(TagError::Invalid(_))));
    assert!(matches!(set_tag(&mut doc, photo, &TagChange::default()), Err(TagError::Invalid(_))));
    let page = pdfcraft_cos::ObjRef::new(3, 0);
    assert_eq!(set_tag(&mut doc, page, &TagChange { alt: Some(Some("x".into())), ..Default::default() }), Err(TagError::NotAnElement(3)));
    let long = "x".repeat(MAX_TEXT + 1);
    assert!(matches!(set_tag(&mut doc, photo, &TagChange { alt: Some(Some(long)), ..Default::default() }), Err(TagError::Invalid(_))));
    assert!(!doc.is_modified());
    // Valid changes.
    set_tag(&mut doc, p, &TagChange { ty: Some("H2".into()), lang: Some(Some("fr-CA".into())), ..Default::default() }).unwrap();
    let described = TagChange { alt: Some(Some("  A bar chart ".into())), actual_text: Some(Some("Été".into())), ..Default::default() };
    set_tag(&mut doc, photo, &described).unwrap();
    set_tag(&mut doc, pdfcraft_cos::ObjRef::new(8, 0), &TagChange { ty: Some("Photo".into()), title: Some(None), ..Default::default() }).unwrap();
    let doc = reopen(&doc);
    let t = tags(&doc);
    assert_eq!((t.nodes[2].ty.as_str(), t.nodes[2].lang.as_deref()), ("H2", Some("fr-CA")));
    assert_eq!((t.nodes[3].alt.as_deref(), t.nodes[3].actual_text.as_deref()), (Some("A bar chart"), Some("Été")));
    assert_eq!((t.nodes[1].ty.as_str(), t.nodes[1].standard.as_deref(), t.nodes[1].title.as_deref()), ("Photo", Some("Figure"), None));
    // The H1 became a figure without alternate text; the described figure passes.
    let r = check(&doc, &Options { rules: [Rule::FiguresAltText].into(), pages: None });
    assert_eq!(r.result(Rule::FiguresAltText).unwrap().findings.len(), 1);
    // Clearing.
    let mut doc = doc;
    set_tag(&mut doc, photo, &TagChange { alt: Some(None), actual_text: Some(Some("  ".into())), ..Default::default() }).unwrap();
    assert!(!tags(&doc).nodes[3].has_alternate());
}

#[test]
fn set_alternate_text_lists_figures_and_untagged_images() {
    let doc = tags_doc();
    let a = alt_targets(&doc);
    assert_eq!(a.figures.len(), 1);
    let f = &a.figures[0];
    assert_eq!((f.obj.map(|o| o.num), f.page, f.needs_alt), (Some(10), Some(0), true));
    assert_eq!(f.bbox, Some([20.0, 100.0, 60.0, 130.0]));
    // The image outside marked content and the inline image; not the tagged or artifact ones.
    let imgs: Vec<(usize, usize, Option<&str>, [f64; 4])> = a.images.iter().map(|i| (i.page, i.index, i.name.as_deref(), i.bbox)).collect();
    assert_eq!(imgs, [(0, 0, Some("Im1"), [100.0, 40.0, 150.0, 80.0]), (0, 1, None, [150.0, 150.0, 170.0, 170.0])]);
}

#[test]
fn tagging_an_untagged_image_with_alternate_text() {
    let mut doc = tags_doc();
    assert!(matches!(tag_image(&mut doc, 0, 5, "x"), Err(TagError::Invalid(_))));
    assert!(matches!(tag_image(&mut doc, 3, 0, "x"), Err(TagError::Invalid(_))));
    assert!(matches!(tag_image(&mut doc, 0, 0, "   "), Err(TagError::Invalid(_))));
    assert!(!doc.is_modified());
    let fig = tag_image(&mut doc, 0, 1, "A grey square").unwrap();
    let fig2 = tag_image(&mut doc, 0, 0, "A photo").unwrap();
    let doc = reopen(&doc);
    assert!(untagged_images(&doc).is_empty());
    let t = tags(&doc);
    let (i, j) = (t.find(fig).unwrap(), t.find(fig2).unwrap());
    assert_eq!(t.nodes[i].parent, Some(0), "added to the Document element");
    assert_eq!(t.nodes[i].kids, [TagKid::Content { page: Some(0), mcid: 3, stream: None }]);
    assert_eq!(t.nodes[j].kids, [TagKid::Content { page: Some(0), mcid: 4, stream: None }]);
    assert_eq!(t.nodes[j].alt.as_deref(), Some("A photo"));
    // The parent tree maps the new MCIDs to the figures.
    let pt = doc.get(pdfcraft_cos::ObjRef::new(14, 0));
    let nums = pt.as_dict().unwrap().get(b"Nums").unwrap().as_array().unwrap().clone();
    let arr = nums[1].as_array().unwrap();
    assert_eq!((arr.len(), arr[3].as_ref(), arr[4].as_ref()), (5, Some(fig), Some(fig2)));
    // The checker: the new figures have alternate text; only the role-mapped figure without it
    // is a finding.
    let r = check(&doc, &Options::default());
    assert_eq!(r.result(Rule::FiguresAltText).unwrap().findings.len(), 1);
    assert_eq!(alt_targets(&doc).figures.iter().filter(|f| f.needs_alt).count(), 1);
}

#[test]
fn tagging_an_image_in_an_untagged_document_creates_the_tree() {
    let mut doc = pdf(
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R >> >> >>".into(),
            stream("q 50 0 0 50 10 10 cm /Im1 Do Q"),
            "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x7f\nendstream"
                .into(),
        ],
        "",
    );
    assert_eq!(untagged_images(&doc).len(), 1);
    let fig = tag_image(&mut doc, 0, 0, "A logo").unwrap();
    let doc = reopen(&doc);
    let t = tags(&doc);
    assert!(t.exists);
    assert_eq!(t.nodes.iter().map(|n| n.ty.as_str()).collect::<Vec<_>>(), ["Document", "Figure"]);
    assert_eq!(t.find(fig), Some(1));
    assert!(untagged_images(&doc).is_empty());
    let r = check(&doc, &Options::default());
    assert_eq!(status(&r, Rule::FiguresAltText), Status::Passed);
    assert_eq!(status(&r, Rule::TaggedContent), Status::Passed, "{:?}", r.result(Rule::TaggedContent));
}

#[test]
fn heading_levels_from_type_names() {
    assert_eq!(heading_level("H1"), Some(HeadingKind::Numbered(1)));
    assert_eq!(heading_level("H6"), Some(HeadingKind::Numbered(6)));
    assert_eq!(heading_level("H12"), Some(HeadingKind::Numbered(12)));
    assert_eq!(heading_level("H99999999999999999999999"), Some(HeadingKind::Numbered(MAX_HEADING_LEVEL)));
    assert_eq!(heading_level("H"), Some(HeadingKind::Nested));
    for not in ["H0", "H01", "Hx", "Header", "P", "", "h1", "H-1"] {
        assert_eq!(heading_level(not), None, "{not}");
    }
}

#[test]
fn headings_in_document_order_with_levels_text_and_content() {
    let doc = pdf(
        &[
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 5 0 R /MarkInfo << /Marked true >> >>".into(), // 1
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into(),                                      // 2
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".into(),                              // 3
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".into(),                              // 4
            "<< /Type /StructTreeRoot /K 6 0 R /RoleMap << /MyHead /H2 >> >>".into(),                      // 5
            "<< /S /Document /K [7 0 R 8 0 R 9 0 R 10 0 R] >>".into(),                                     // 6
            "<< /S /H1 /Pg 3 0 R /K [0 << /Type /MCR /Pg 3 0 R /MCID 1 >>] >>".into(),                     // 7
            "<< /S /Sect /K [11 0 R 12 0 R] >>".into(),                                                    // 8
            "<< /S /MyHead /Pg 3 0 R /Alt (Alt title) /K 2 >>".into(),                                     // 9
            "<< /S /P /Pg 3 0 R /K [3 6 0 R] >>".into(),                                                   // 10: loops back
            "<< /S /H /Pg 4 0 R /ActualText (  Plain  ) /K 0 >>".into(),                                   // 11
            "<< /S /Sect /K 13 0 R >>".into(),                                                             // 12
            "<< /S /H /Pg 4 0 R /K [14 0 R << /Type /MCR /Pg 4 0 R /MCID 5 /Stm 3 0 R >>] >>".into(),      // 13
            "<< /S /Span /Pg 4 0 R /K 1 >>".into(),                                                        // 14
        ],
        "",
    );
    let tags = tags(&doc);
    let h = headings(&tags);
    type Row<'a> = (usize, Option<&'a str>, Option<usize>, Vec<(usize, i64)>);
    let summary: Vec<Row> = h.iter().map(|x| (x.level, x.text.as_deref(), x.page, x.content.clone())).collect();
    assert_eq!(
        summary,
        vec![
            (1, None, Some(0), vec![(0, 0), (0, 1)]),
            (1, Some("Plain"), Some(1), vec![(1, 0)]),
            (2, None, Some(1), vec![(1, 1)]), // H inside two sections; the /Stm content is left out
            (2, Some("Alt title"), Some(0), vec![(0, 2)]),
        ]
    );
    // An untagged document has none.
    let plain = pdf(&["<< /Type /Catalog /Pages 2 0 R >>".into(), "<< /Type /Pages /Kids [] /Count 0 >>".into()], "");
    assert!(headings(&super::tags(&plain)).is_empty());
}
