//! Issue #295: quick clicks on text widen the selection, as in Acrobat: two clicks select a
//! word, three the line, four all the text on the page.

use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use pdfcraft_ui_egui::PdfCraftApp;

/// One 300×200 page with two lines of Helvetica text.
fn two_lines() -> Vec<u8> {
    let content = "BT /F1 14 Tf 20 150 Td (The quick brown fox) Tj 0 -24 Td (jumps over the dog) Tj ET";
    format!(
        "%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj
4 0 obj << /Length {} >> stream
{content}
endstream endobj
5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
trailer << /Root 1 0 R >>
%%EOF",
        content.len()
    )
    .into_bytes()
}

/// The page on screen and the pointer over the "u" of "quick". Frames are 1/60 s: a click takes
/// two frames (press, release), and egui counts a double-click only within 0.3 s.
fn harness(setup: impl FnOnce(&mut PdfCraftApp) + 'static) -> (Harness<'static, PdfCraftApp>, Pos2) {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_step_dt(1.0 / 60.0).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("lines.pdf", None, two_lines()).expect("opens");
        app.set_option("left", "closed").unwrap();
        app.set_option("author", "Tester").unwrap();
        setup(&mut app);
        app
    });
    for _ in 0..200 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let at = h.state().views[0].glyph_screen_pos(0, 5).expect("text layer loaded");
    h.hover_at(at);
    h.run_steps(1);
    (h, at)
}

/// Press and release without moving or leaving the page.
fn click(h: &mut Harness<'static, PdfCraftApp>, pos: Pos2) {
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    }
    h.run_steps(1);
}

#[test]
fn quick_clicks_select_a_word_then_the_line_then_the_page() {
    let (mut h, at) = harness(|_| {});
    let page = "The quick brown fox\njumps over the dog";
    for (n, want) in [None, Some("quick"), Some("The quick brown fox"), Some(page), Some(page)].into_iter().enumerate() {
        click(&mut h, at);
        assert_eq!(h.state().views[0].selected_text().as_deref(), want, "after {} clicks", n + 1);
    }
    // A pause ends the run: the next double-click selects a word again.
    h.run_steps(60);
    click(&mut h, at);
    click(&mut h, at);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("quick"));
}

#[test]
fn quick_clicks_with_the_highlighter_mark_only_the_word() {
    let (mut h, at) = harness(|app| assert!(app.execute("comment.highlight")));
    for _ in 0..4 {
        click(&mut h, at);
    }
    h.run_steps(3);
    let app = h.state();
    let doc = app.session.get(app.views[0].id).unwrap();
    let marks: Vec<(&str, usize)> = doc.info.annotations.iter().map(|a| (a.subtype.as_str(), a.quads.len())).collect();
    assert_eq!(marks, [("Highlight", 1)], "one highlight, on the double-clicked word");
    assert!(app.views[0].selected_text().is_none());
}

/// Press at `from`, move to `to` and release there.
fn drag(h: &mut Harness<'static, PdfCraftApp>, from: Pos2, to: Pos2) {
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    for k in 1..=4 {
        h.hover_at(from + (to - from) * (k as f32 / 4.0));
        h.run_steps(1);
    }
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(1);
}

/// Click at `pos` with Shift held.
fn shift_click(h: &mut Harness<'static, PdfCraftApp>, pos: Pos2) {
    h.event(Event::ModifiersChanged(Modifiers::SHIFT));
    h.hover_at(pos);
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::SHIFT });
    }
    h.run_steps(1);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(1);
}

/// Issue #527: ⇧-click extends the text selection to the click, keeping its anchor.
#[test]
fn shift_click_extends_the_selection_from_its_anchor() {
    let (mut h, _) = harness(|_| {});
    let glyph = |h: &Harness<'static, PdfCraftApp>, g: usize| h.state().views[0].glyph_screen_pos(0, g).expect("glyph on screen");
    // Drag over "quick" (glyphs 4..=8).
    let (q, k) = (glyph(&h, 4), glyph(&h, 8));
    drag(&mut h, q, k);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("quick"));
    h.run_steps(60);
    // ⇧-click on the "n" of "brown": the selection runs from the anchor to it.
    let n = glyph(&h, 14);
    shift_click(&mut h, n);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("quick brown"));
    h.run_steps(60);
    // ⇧-click before the anchor: the anchor stays, the selection now runs back to the click.
    let t = glyph(&h, 0);
    shift_click(&mut h, t);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("The q"));
    h.run_steps(60);
    // A plain click still clears it.
    click(&mut h, n);
    assert_eq!(h.state().views[0].selected_text(), None);
    h.run_steps(60);
    // ⇧-click with nothing selected selects nothing.
    shift_click(&mut h, n);
    assert_eq!(h.state().views[0].selected_text(), None);
}

/// Original compact multipage fixture; page 2 is deliberately blank.
fn reader_pages() -> Vec<u8> {
    reader_pages_with_comment(false)
}
fn reader_pages_with_comment(comment: bool) -> Vec<u8> {
    let contents = [
        "BT /F1 14 Tf 20 160 Td (Alpha first line) Tj 0 -18 Td (Alpha next line) Tj 0 -44 Td (Separate paragraph) Tj ET",
        "",
        "BT /F1 14 Tf 20 160 Td (Omega final page) Tj ET",
    ];
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [4 0 R 6 0 R 8 0 R] /Count 3 /MediaBox [0 0 300 200] >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    for (i, content) in contents.iter().enumerate() {
        objects.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + i * 2));
        objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
    }
    if comment {
        objects[3] = objects[3].replace("/Contents", "/Annots [10 0 R] /Contents");
        objects.push("<< /Type /Annot /Subtype /Square /Rect [180 50 250 100] /C [1 0 0] /Contents (Original test comment) >>".into());
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![];
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend(format!("trailer << /Root 1 0 R /Size {} >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}
fn reader_harness(bytes: Vec<u8>, single: bool, rotation: u16) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 1000.0)).with_step_dt(1.0 / 60.0).build_eframe(move |_| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("reader.pdf", None, bytes).unwrap();
        app.set_option("left", "closed").unwrap();
        let view = &mut app.views[0];
        view.set_zoom(0.65);
        if single {
            view.set_layout(pdfcraft_ui_egui::canvas::PageLayout::Single);
        }
        for _ in 0..rotation / 90 {
            view.rotate_view(true);
        }
        app
    });
    settle_reader(&mut h);
    h
}
fn settle_reader(h: &mut Harness<'static, PdfCraftApp>) {
    for _ in 0..300 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
}
const FIRST_PAGE: &str = "Alpha first line\nAlpha next line\nSeparate paragraph";
const ALL_PAGES: &str = "Alpha first line\nAlpha next line\nSeparate paragraph\n\n\n\nOmega final page";

#[test]
fn dragging_across_blank_pages_works_both_directions_and_in_every_rotation() {
    for rotation in [0, 90, 180, 270] {
        let mut h = reader_harness(reader_pages(), false, rotation);
        let a = h.state().views[0].glyph_screen_pos(0, 0).unwrap();
        let b = h.state().views[0].glyph_screen_pos(2, 4).unwrap();
        drag(&mut h, a, b);
        settle_reader(&mut h);
        assert_eq!(h.state().views[0].selected_text().as_deref(), Some(format!("{FIRST_PAGE}\n\n\n\nOmega").as_str()), "rotation {rotation}");
        h.run_steps(60);
        drag(&mut h, b, a);
        settle_reader(&mut h);
        assert_eq!(h.state().views[0].selected_text().as_deref(), Some(format!("{FIRST_PAGE}\n\n\n\nOmega").as_str()), "reverse rotation {rotation}");
    }
}
#[test]
fn shift_click_extends_to_another_page_and_keeps_the_original_anchor() {
    let mut h = reader_harness(reader_pages(), true, 0);
    h.state_mut().views[0].select_text(0, 6, 10);
    h.state_mut().views[0].go_to_page(2);
    settle_reader(&mut h);
    let final_glyph = h.state().views[0].glyph_screen_pos(2, 4).unwrap();
    shift_click(&mut h, final_glyph);
    settle_reader(&mut h);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("first line\nAlpha next line\nSeparate paragraph\n\n\n\nOmega"));
    h.state_mut().views[0].go_to_page(0);
    settle_reader(&mut h);
    h.run_steps(60);
    let first_glyph = h.state().views[0].glyph_screen_pos(0, 0).unwrap();
    shift_click(&mut h, first_glyph);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("Alpha f"));
}
#[test]
fn select_all_and_immediate_copy_wait_for_offscreen_text_without_partial_output() {
    let mut h = reader_harness(reader_pages(), true, 0);
    let id = h.state().views[0].id;
    let bytes = h.state().session.get(id).unwrap().bytes.clone();
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
    h.event(Event::Copy);
    let mut copied = Vec::new();
    for _ in 0..300 {
        h.run_steps(1);
        copied.extend(h.output().platform_output.commands.iter().filter_map(|command| match command {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        }));
        if !copied.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    assert_eq!(copied, [ALL_PAGES]);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some(ALL_PAGES));
    let doc = h.state().session.get(id).unwrap();
    assert!(std::sync::Arc::ptr_eq(&bytes, &doc.bytes));
    assert!(!doc.dirty);
    assert!(doc.can_undo().is_none());
}
#[test]
fn paragraph_context_action_selects_wrapped_lines_without_the_next_paragraph() {
    use egui_kittest::kittest::Queryable;
    let mut h = reader_harness(reader_pages(), false, 0);
    let at = h.state().views[0].glyph_screen_pos(0, 3).unwrap();
    h.hover_at(at);
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE });
    }
    h.run_steps(2);
    h.get_by_label("Select paragraph").click();
    h.run_steps(2);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("Alpha first line\nAlpha next line"));
}
#[test]
fn four_clicks_select_page_in_single_layout_and_document_in_continuous_layout() {
    for single in [true, false] {
        let mut h = reader_harness(reader_pages(), single, 0);
        let at = h.state().views[0].glyph_screen_pos(0, 3).unwrap();
        for _ in 0..4 {
            click(&mut h, at);
        }
        settle_reader(&mut h);
        assert_eq!(h.state().views[0].selected_text().as_deref(), Some(if single { FIRST_PAGE } else { ALL_PAGES }));
    }
}
#[test]
fn restricted_reader_copy_does_not_write_clipboard_and_explains_permission() {
    use egui_kittest::kittest::Queryable;
    let mut session = pdfcraft_engine::Session::new();
    let id = session.open("original.pdf", None, std::sync::Arc::new(reader_pages()), None).unwrap();
    session
        .apply(
            id,
            pdfcraft_engine::Edit::Protect(pdfcraft_engine::Protection {
                permissions_password: Some("Original-owner-test".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    let bytes = session.save_bytes(id).unwrap();
    let mut h = reader_harness(bytes.as_ref().clone(), false, 0);
    h.state_mut().views[0].select_text(0, 0, 4);
    h.event(Event::Copy);
    h.run_steps(1);
    assert!(!h.output().platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(_))));
    assert!(h.state().views[0].selected_text().is_none());
    h.run_steps(1);
    h.get_by_label_contains("does not allow copying text");
}

#[test]
fn dragging_a_comment_does_not_extend_an_existing_reader_selection() {
    let mut h = reader_harness(reader_pages_with_comment(true), false, 0);
    h.state_mut().views[0].select_text(2, 0, 4);
    let rect = h.state().views[0].page_screen_rect(0).unwrap();
    let point = |x: f32, y: f32| egui::pos2(rect.left() + x / 300.0 * rect.width(), rect.top() + (200.0 - y) / 200.0 * rect.height());
    let start = point(215.0, 75.0);
    let end = point(245.0, 75.0);
    drag(&mut h, start, end);
    settle_reader(&mut h);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("Omega"));
    let doc = h.state().session.get(h.state().views[0].id).unwrap();
    assert_eq!(doc.can_undo(), Some("Move comment"));
}
#[test]
fn escape_cancels_queued_copy_before_any_offscreen_text_is_returned() {
    let mut h = reader_harness(reader_pages(), true, 0);
    h.state_mut().views[0].select_all();
    h.event(Event::Copy);
    h.key_press(egui::Key::Escape);
    for _ in 0..30 {
        h.run_steps(1);
        assert!(!h.output().platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(_))));
    }
    assert!(h.state().views[0].selected_text().is_none());
    assert_eq!(h.state().views[0].text_selection_state()["copy_pending"], false);
}
