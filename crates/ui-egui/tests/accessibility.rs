//! Prepare for accessibility ▸ Check for accessibility in the real shell (egui_kittest): the
//! options dialog, the results panel, Fix, Skip Rule and the report.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::a11y::{Rule, Status};
use pdfcraft_ui_egui::{Dialog, PdfCraftApp, RightPanel};

const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 100] >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj
4 0 obj << /Length 37 >> stream
BT /F1 12 Tf 20 50 Td (Hello) Tj ET
endstream endobj
5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
trailer << /Root 1 0 R >>
%%EOF";

fn status(h: &Harness<'static, PdfCraftApp>, rule: Rule) -> Status {
    h.state().a11y.report.as_ref().unwrap().1.result(rule).unwrap().status
}

fn right_click(h: &mut Harness<'static, PdfCraftApp>, label: &str) {
    let at = h.get_by_label(label).rect().center();
    h.hover_at(at);
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed: true, modifiers: Default::default() });
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed: false, modifiers: Default::default() });
    h.run_steps(2);
}

#[test]
fn checking_fixing_skipping_and_reporting() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-a11y-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("notes.pdf", None, FIXTURE.to_vec()).unwrap();
        app.export_dir_override = Some(d.to_string_lossy().into_owned());
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("a11y.check"));
    h.run_steps(2);
    h.get_by_label("Accessibility Checker Options");
    h.get_by_label_contains("31 of 32");
    h.get_by_label("Document title is showing in title bar");
    h.get_by_label("Start Checking").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    assert_eq!(h.state().right, Some(RightPanel::Accessibility));
    h.get_by_label("Document (3 issues)");
    h.get_by_label("Title - Failed");
    h.get_by_label("Color contrast - Skipped");
    // Fix the title: the file name becomes the title and it shows; the description opens to edit it.
    right_click(&mut h, "Title - Failed");
    h.get_by_label("Fix").click();
    h.run_steps(3);
    assert_eq!(status(&h, Rule::Title), Status::Passed);
    assert!(matches!(h.state().dialog, Some(Dialog::Properties(_))));
    assert_eq!(h.state().window_title, "notes — PdfCraft");
    h.state_mut().dialog = None;
    h.run_steps(2);
    // Skip a rule; it stays skipped when checking again.
    right_click(&mut h, "Tagged PDF - Failed");
    h.get_by_label("Skip Rule").click();
    h.run_steps(2);
    h.get_by_label("Check again").click();
    h.run_steps(3);
    assert_eq!(status(&h, Rule::TaggedPdf), Status::Skipped);
    h.get_by_label("Document (1 issue)");
    // Explain shows why the rule matters; findings list the problems.
    right_click(&mut h, "Primary language - Failed");
    h.get_by_label("Explain").click();
    h.run_steps(2);
    h.get_by_label_contains("Screen readers choose their voice");
    h.get_by_label("Primary language - Failed").click();
    h.run_steps(2);
    h.get_by_label("No document language is set");
    // The report.
    h.get_by_label("Report").click();
    h.run_steps(2);
    let html = std::fs::read_to_string(dir.join("notes Accessibility Report.html")).unwrap();
    assert!(html.contains("Accessibility Report") && html.contains("Skipped"));
}

/// Two tagged figures: an image without alternate text and a bar (role-mapped Photo) with.
const FIGURES: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 5 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /StructParents 0 /Resources << /XObject << /Im 10 0 R >> >> >> endobj
4 0 obj << /Length 97 >> stream
/Figure << /MCID 0 >> BDC q 40 0 0 20 10 150 cm /Im Do Q EMC /Photo << /MCID 1 >> BDC 100 100 50 30 re f EMC
endstream endobj
5 0 obj << /Type /StructTreeRoot /K 6 0 R /ParentTree 9 0 R /RoleMap << /Photo /Figure >> >> endobj
6 0 obj << /S /Document /P 5 0 R /K [7 0 R 8 0 R] >> endobj
7 0 obj << /S /Figure /P 6 0 R /Pg 3 0 R /K 0 >> endobj
8 0 obj << /S /Photo /P 6 0 R /Pg 3 0 R /Alt (A bar) /K 1 >> endobj
9 0 obj << /Nums [0 [7 0 R 8 0 R]] >> endobj
10 0 obj << /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >> stream
\x00
endstream endobj
trailer << /Root 1 0 R >>
%%EOF";

#[test]
fn setting_alternate_text_figure_by_figure() {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("figures.pdf", None, FIGURES.to_vec()).unwrap();
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("a11y.alt_text"));
    h.run_steps(2);
    h.get_by_label("Set Alternate Text");
    h.get_by_label("Figure 1 of 2 on page 1");
    h.state_mut().alt_draft.texts[0] = "A grey square".into();
    h.run_steps(2);
    h.get_by_label("Next figure").click();
    h.run_steps(2);
    h.get_by_label("Figure 2 of 2 on page 1");
    h.get_by_label("Decorative figure").click();
    h.run_steps(1);
    h.get_by_label("Save & Close").click();
    h.run_steps(3);
    let s = h.state();
    let doc = s.session.get(s.views[0].id).unwrap();
    assert_eq!(doc.can_undo(), Some("Set alternate text"));
    let f = doc.figures();
    assert_eq!(f.len(), 1, "the decorative one left the tags");
    assert_eq!(f[0].alt.as_deref(), Some("A grey square"));
}

// ───────────────────────────────────────────── Tags, Order, Reading order, Autotag, Make accessible

/// A PDF from object bodies (object n is `bodies[n - 1]`), with a real xref table.
fn pdf(bodies: &[String]) -> Vec<u8> {
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
    out
}

fn stream(data: &str) -> String {
    format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len())
}

/// Document > [H1 "Introduction" (MCID 0), Figure (MCID 1, no alt)], two lines of text.
fn tagged() -> Vec<u8> {
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R /Lang (en-US) >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 300] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /StructParents 0 /Resources << /Font << /F1 5 0 R >> >> >>".into(),
        stream("/H1 << /MCID 0 >> BDC BT /F1 18 Tf 20 260 Td (Introduction) Tj ET EMC /Figure << /MCID 1 >> BDC 20 100 100 80 re f EMC"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        "<< /Type /StructTreeRoot /K 7 0 R /ParentTree 10 0 R /ParentTreeNextKey 1 >>".into(),
        "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R] >>".into(),
        "<< /Type /StructElem /S /H1 /P 7 0 R /Pg 3 0 R /K 0 >>".into(),
        "<< /Type /StructElem /S /Figure /P 7 0 R /Pg 3 0 R /K 1 >>".into(),
        "<< /Nums [0 [8 0 R 9 0 R]] >>".into(),
    ])
}

fn harness(name: &str, bytes: Vec<u8>) -> Harness<'static, PdfCraftApp> {
    let name = name.to_owned();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes(&name, None, bytes).unwrap();
        app
    });
    h.run_steps(4);
    h
}

fn tag_types(h: &Harness<'static, PdfCraftApp>) -> Vec<String> {
    let s = h.state();
    s.session.get(s.views[0].id).unwrap().tags().nodes.iter().map(|n| n.ty.clone()).collect()
}

fn drag(h: &mut Harness<'static, PdfCraftApp>, from: egui::Pos2, to: egui::Pos2) {
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    h.event(egui::Event::PointerMoved(from));
    h.run_steps(1);
    h.event(button(from, true));
    h.run_steps(1);
    for k in 1..=6 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (k as f32 / 6.0)));
        h.run_steps(1);
    }
    h.event(button(to, false));
    h.run_steps(3);
}

#[test]
fn the_tags_panel_selects_edits_reorders_and_deletes_tags() {
    let mut h = harness("tagged.pdf", tagged());
    assert!(h.state_mut().execute("a11y.tags"));
    h.run_steps(3);
    assert_eq!(h.state().right, Some(RightPanel::Tags));
    h.get_by_label("Document");
    h.get_by_label("H1").click();
    h.run_steps(3);
    // The heading's content is highlighted on its page.
    let (page, rects) = h.state().a11y_tools.tags.highlight.clone().unwrap();
    assert_eq!(page, 0);
    assert!(!rects.is_empty(), "the heading's text is located");

    // Tag properties: a title and a language, one undo step.
    assert!(h.state_mut().execute("a11y.tag_properties"));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::TagProperties));
    {
        let d = h.state_mut().a11y_tools.props.as_mut().unwrap();
        d.title = "Intro".into();
        d.lang = "en-GB".into();
    }
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    h.get_by_label("H1 Intro");
    {
        let s = h.state();
        let doc = s.session.get(s.views[0].id).unwrap();
        assert_eq!(doc.can_undo(), Some("Change tag properties"));
        let t = doc.tags();
        assert_eq!((t.nodes[1].title.as_deref(), t.nodes[1].lang.as_deref()), (Some("Intro"), Some("en-GB")));
    }

    // New tag inside the Document; then drag the heading into it.
    h.get_by_label("Document").click();
    h.run_steps(2);
    assert!(h.state_mut().execute("a11y.new_tag"));
    h.run_steps(2);
    h.state_mut().a11y_tools.new_tag.as_mut().unwrap().ty = "Sect".into();
    h.get_by_label("Add tag").click();
    h.run_steps(3);
    assert_eq!(tag_types(&h), ["Document", "H1", "Figure", "Sect"]);
    let from = h.get_by_label("H1 Intro").rect().center();
    let to = h.get_by_label("Sect").rect().center();
    drag(&mut h, from, to);
    assert_eq!(tag_types(&h), ["Document", "Figure", "Sect", "H1"], "the heading went inside the section");

    // Delete the section: the heading comes back up and stays tagged.
    h.get_by_label("Sect").click();
    h.run_steps(2);
    assert!(h.state_mut().execute("a11y.delete_tag"));
    h.run_steps(3);
    assert_eq!(tag_types(&h), ["Document", "Figure", "H1"]);

    // Change the figure to an artifact: it leaves the tags.
    h.get_by_label("Figure").click();
    h.run_steps(2);
    assert!(h.state_mut().execute("a11y.artifact"));
    h.run_steps(3);
    assert_eq!(tag_types(&h), ["Document", "H1"]);
    assert!(h.state_mut().execute("edit.undo"));
    h.run_steps(3);
    assert_eq!(tag_types(&h), ["Document", "Figure", "H1"]);
    // Nothing selected: the commands say what to do.
    h.state_mut().a11y_tools.tags.selected = None;
    assert!(h.state_mut().execute("a11y.delete_tag"));
    h.run_steps(1);
    assert_eq!(tag_types(&h).len(), 3);
}

#[test]
fn the_reading_order_tool_numbers_regions_and_tags_a_drawn_box() {
    let mut h = harness("notes.pdf", FIXTURE.to_vec());
    assert!(h.state_mut().execute("a11y.reading_order"));
    h.run_steps(4);
    assert_eq!(h.state().quick_tool, pdfcraft_ui_egui::QuickTool::ReadingOrder);
    assert_eq!(h.state().right, Some(RightPanel::Order));
    h.get_by_label("Nothing on this page is tagged yet.");
    // Drag a box over the whole page: the roles appear.
    let page = h.state().views[0].page_screen_rect(0).unwrap();
    drag(&mut h, page.left_top() + egui::vec2(2.0, 2.0), page.right_bottom() - egui::vec2(2.0, 2.0));
    let pending = h.state().a11y_tools.reading.pending.clone().expect("a box was drawn");
    assert_eq!(pending.page, 0);
    h.get_by_label("Tag this area as");
    h.get_by_label("Heading 1").click();
    h.run_steps(4);
    assert!(h.state().a11y_tools.reading.pending.is_none());
    assert!(tag_types(&h).contains(&"H1".to_owned()));
    // The Order panel numbers it.
    h.get_by_label("1 H1").click();
    h.run_steps(2);
    assert_eq!(h.state().a11y_tools.region, Some((0, 0)));
    // A box tagged as an artifact leaves the reading order.
    let doc = h.state().views[0].id;
    h.state_mut().a11y_tools.reading.pending = Some(pdfcraft_ui_egui::reading_order_ui::Pending { doc, page: 0, rect: [0.0, 0.0, 200.0, 100.0] });
    h.run_steps(2);
    h.get_by_label("Artifact: footer").click();
    h.run_steps(4);
    assert!(!tag_types(&h).contains(&"H1".to_owned()), "{:?}", tag_types(&h));
    h.get_by_label("Done").click();
    h.run_steps(2);
    assert_eq!(h.state().quick_tool, pdfcraft_ui_egui::QuickTool::Select);
}

#[test]
fn autotag_shows_what_it_made_and_make_accessible_is_one_undo_step() {
    let mut h = harness("notes.pdf", FIXTURE.to_vec());
    assert!(h.state_mut().execute("a11y.autotag"));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::Autotag));
    h.get_by_label("Autotag").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, Some(Dialog::AutotagSummary));
    let s = h.state().a11y_tools.summary.clone().unwrap();
    assert!(s.elements > 0 && s.pages == 1, "{s:?}");
    h.get_by_label("Show tags").click();
    h.run_steps(3);
    assert_eq!((h.state().dialog, h.state().right), (None, Some(RightPanel::Tags)));
    // Autotagging again asks to replace the tags.
    assert!(h.state_mut().execute("a11y.autotag_fields"));
    h.run_steps(2);
    h.get_by_label("Replace the existing tags");
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state_mut().execute("edit.undo"));
    h.run_steps(2);

    // Make accessible: a language is required; then everything is one undo step.
    assert!(h.state_mut().execute("actions.make_accessible"));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::MakeAccessible));
    h.get_by_label("Next").click();
    h.run_steps(2);
    h.get_by_label("Give the document's language as a language tag, such as en-US.");
    h.state_mut().a11y_tools.make.as_mut().unwrap().lang = "en-US".into();
    h.get_by_label("Next").click();
    h.run_steps(4);
    let m = h.state().a11y_tools.make.clone().unwrap();
    assert_eq!(m.step, pdfcraft_ui_egui::make_accessible_ui::Step::Done, "{m:?}");
    assert!(m.failed_rules.is_some());
    {
        let s = h.state();
        let doc = s.session.get(s.views[0].id).unwrap();
        assert_eq!(doc.can_undo(), Some("Make accessible"));
        let t = doc.tags();
        assert!(t.exists);
        assert_eq!(t.lang.as_deref(), Some("en-US"));
        assert_eq!(doc.info_value("Title").as_deref(), Some("notes"));
    }
    h.get_by_label("Show checker results").click();
    h.run_steps(2);
    assert_eq!((h.state().dialog, h.state().right), (None, Some(RightPanel::Accessibility)));
    assert!(h.state_mut().execute("edit.undo"));
    h.run_steps(2);
    let s = h.state();
    assert!(!s.session.get(s.views[0].id).unwrap().tags().exists, "one undo takes back the whole action");
}

#[test]
fn make_accessible_asks_for_field_tooltips_and_figure_alt_text() {
    let bytes = pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R] >> /Lang (en) >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 300] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Annots [6 0 R] /Resources << /XObject << /Im 5 0 R >> /Font << /F1 7 0 R >> >> >>".into(),
        stream("BT /F1 12 Tf 20 260 Td (Name) Tj ET q 60 0 0 60 20 100 cm /Im Do Q"),
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x7f\nendstream"
            .into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (first_name) /Rect [80 250 200 270] /P 3 0 R >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ]);
    let mut h = harness("form.pdf", bytes);
    assert!(h.state_mut().execute("actions.make_accessible"));
    h.run_steps(2);
    h.get_by_label("Next").click();
    h.run_steps(2);
    let m = h.state().a11y_tools.make.clone().unwrap();
    assert_eq!(m.step, pdfcraft_ui_egui::make_accessible_ui::Step::Tooltips);
    assert_eq!(m.fields, [("first_name".to_owned(), "First name".to_owned())]);
    // The dialog settles its size and position over a few frames before the next click.
    h.run_steps(4);
    h.get_by_label("Next").click();
    h.run_steps(4);
    let m = h.state().a11y_tools.make.clone().unwrap();
    assert_eq!(m.step, pdfcraft_ui_egui::make_accessible_ui::Step::AltText, "{m:?}");
    assert_eq!(m.figures.len(), 1);
    h.state_mut().a11y_tools.make.as_mut().unwrap().alts[0] = "A grey square".into();
    h.get_by_label("Finish").click();
    h.run_steps(4);
    let s = h.state();
    let doc = s.session.get(s.views[0].id).unwrap();
    assert_eq!(doc.can_undo(), Some("Make accessible"));
    assert_eq!(doc.info.fields[0].tooltip.as_deref(), Some("First name"));
    assert_eq!(doc.figures()[0].alt.as_deref(), Some("A grey square"));
}

#[test]
fn hostile_and_huge_structure_trees_stay_cheap_and_never_panic() {
    // 50 000 paragraphs under one Document element.
    let n = 50_000;
    let mut bodies: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 4 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>".into(),
        "<< /Type /Page /Parent 2 0 R >>".into(),
        "<< /Type /StructTreeRoot /K 5 0 R >>".into(),
    ];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 6 + i)).collect();
    bodies.push(format!("<< /Type /StructElem /S /Document /P 4 0 R /K [{}] >>", kids.join(" ")));
    for _ in 0..n {
        bodies.push("<< /Type /StructElem /S /P /P 5 0 R /Pg 3 0 R >>".into());
    }
    let mut h = harness("huge.pdf", pdf(&bodies));
    assert!(h.state_mut().execute("a11y.tags"));
    h.run_steps(3);
    let reads = h.state().a11y_tools.tags.reads;
    assert_eq!(h.state().a11y_tools.tags.rows.len(), n + 1);
    let start = std::time::Instant::now();
    h.run_steps(10);
    assert_eq!(h.state().a11y_tools.tags.reads, reads, "frames don't read the tree again");
    assert!(h.query_all_by_label("P").count() < 200, "only the rows on screen are laid out");
    assert!(start.elapsed() < std::time::Duration::from_secs(20), "frames stay cheap: {:?}", start.elapsed());
    // Scrolled to the end, the last rows show.
    let last = h.state().a11y_tools.tags.tree.nodes.len() - 1;
    h.state_mut().a11y_tools.tags.select(last, None);
    h.run_steps(4);
    assert_eq!(h.state().a11y_tools.tags.selected, Some(last));

    // A cyclic, deep tree with kids that aren't elements.
    let mut bodies: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 4 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>".into(),
        "<< /Type /StructTreeRoot /K [5 0 R 5 0 R (x) 9 null] /ParentTree << /Nums [0 6 0 R] >> >>".into(),
        "<< /Type /StructElem /S /Div /P 4 0 R /K [6 0 R 5 0 R] >>".into(),
        "<< /Type /StructElem /S /Sect /P 5 0 R /K [5 0 R 8 0 R 0 << /Type /MCR /MCID -4 >>] /Pg 3 0 R >>".into(),
        stream("/P << /MCID 0 >> BDC 0 0 m 10 10 l S EMC"),
    ];
    let deep = 3000;
    for i in 0..deep {
        let me = 8 + i;
        bodies.push(format!("<< /Type /StructElem /S /Span /P {} 0 R /K [{} 0 R] /T (level {i}) >>", me - 1, me + 1));
    }
    bodies.push("<< /Type /StructElem /S /Span /K 99999999999 >>".into());
    let mut h = harness("cyclic.pdf", pdf(&bodies));
    for panel in ["a11y.tags", "a11y.order", "a11y.reading_order"] {
        assert!(h.state_mut().execute(panel));
        h.run_steps(3);
    }
    assert!(h.state_mut().execute("a11y.tags"));
    h.run_steps(2);
    h.get_by_label("Expand all tags").click();
    h.run_steps(3);
    let rows = h.state().a11y_tools.tags.rows.len();
    assert!(rows > 3 && rows <= pdfcraft_ui_egui::tags_ui::MAX_ROWS);
    let deepest = h.state().a11y_tools.tags.tree.nodes.len() - 1;
    h.state_mut().a11y_tools.tags.select(deepest, None);
    h.run_steps(3);
    for cmd in ["a11y.tag_properties", "a11y.delete_tag", "a11y.artifact", "a11y.new_tag"] {
        h.state_mut().execute(cmd);
        h.run_steps(2);
        h.state_mut().dialog = None;
        h.run_steps(1);
    }
}
