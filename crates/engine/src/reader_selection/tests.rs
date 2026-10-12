use super::*;
use crate::{Edit, Protection, Session};
use pdfcraft_render::{TextGlyph, text::layout};
use std::sync::Arc;

fn selection(a: (usize, usize), b: (usize, usize)) -> TextSelection {
    TextSelection::Range { anchor: TextPosition { page: a.0, glyph: a.1 }, head: TextPosition { page: b.0, glyph: b.1 } }
}
fn line(s: &str, x: f32, y: f32) -> Vec<TextGlyph> {
    s.chars().enumerate().map(|(i, c)| TextGlyph { text: c.to_string(), rect: [x + i as f32 * 8.0, y, x + i as f32 * 8.0 + 7.0, y + 10.0] }).collect()
}
fn page(s: &str) -> PageText {
    layout(line(s, 0.0, 0.0))
}

#[test]
fn reverse_ranges_keep_page_line_and_blank_page_boundaries() {
    let pages = [page("alpha"), PageText::default(), page("omega")];
    let forward = selection((0, 2), (2, 2));
    let reverse = selection((2, 2), (0, 2));
    for range in [forward, reverse] {
        assert_eq!(cached_selection_text(range, 3, |p| pages.get(p)).unwrap().as_deref(), Some("pha\n\n\n\nome"));
    }
}
#[test]
fn incomplete_cache_never_returns_a_partial_copy() {
    let first = page("first");
    assert_eq!(cached_selection_text(TextSelection::All, 2, |p| (p == 0).then_some(&first)).unwrap(), None);
}
#[test]
fn out_of_range_endpoints_fail_without_overflow() {
    let p = page("abc");
    assert!(cached_selection_text(selection((0, 0), (usize::MAX, 0)), 1, |_| Some(&p)).is_err());
    assert!(cached_selection_text(selection((0, 0), (0, usize::MAX)), 1, |_| Some(&p)).is_err());
    assert_eq!(selection((0, 0), (0, usize::MAX)).range_on(0, 3), Some(0..3));
}
#[test]
fn glyph_mappings_remain_indivisible_and_metadata_is_defensive() {
    let p = PageText {
        glyphs: vec![TextGlyph { text: "ffi".into(), rect: [0.0; 4] }, TextGlyph { text: "λ".into(), rect: [0.0; 4] }],
        line_of: vec![],
        space_before: vec![],
    };
    assert_eq!(cached_selection_text(selection((0, 0), (0, 1)), 1, |_| Some(&p)).unwrap().as_deref(), Some("ffiλ"));
}
#[test]
fn copy_limit_fails_before_appending_more_bytes() {
    let mut output = "x".repeat(MAX_SELECTION_BYTES - 1);
    assert!(append(&mut output, "λ").is_err());
    assert_eq!(output.len(), MAX_SELECTION_BYTES - 1);
}
#[test]
fn paragraph_stops_at_spacing_and_columns_in_reading_order() {
    let glyphs = [
        line("heading", 0.0, 0.0),
        line("first row", 0.0, 32.0),
        line("second row", 0.0, 47.0),
        line("other column", 150.0, 32.0),
        line("next para", 0.0, 85.0),
    ]
    .concat();
    let text = layout(glyphs);
    let hit = text.glyphs.iter().position(|g| g.rect[1] == 47.0).unwrap();
    let range = paragraph_at(&text, hit).unwrap();
    assert_eq!(text.text_of(range), "first row\nsecond row");
    let range = paragraph_at(&text, 0).unwrap();
    assert_eq!(text.text_of(range), "heading");
}
#[test]
fn paragraph_selection_handles_rotated_line_geometry() {
    let mut text = layout([line("first", 0.0, 0.0), line("second", 0.0, 15.0)].concat());
    for g in &mut text.glyphs {
        let [x0, y0, x1, y1] = g.rect;
        g.rect = [100.0 - y1, x0, 100.0 - y0, x1];
    }
    assert_eq!(text.text_of(paragraph_at(&text, 7).unwrap()), "first\nsecond");
}

fn fixture() -> Vec<u8> {
    let contents = ["BT /F1 14 Tf 20 150 Td (first page) Tj ET", "", "BT /F1 14 Tf 20 150 Td (last page) Tj ET"];
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [4 0 R 6 0 R 8 0 R] /Count 3 /MediaBox [0 0 300 200] >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    for (i, content) in contents.iter().enumerate() {
        objects.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + i * 2));
        objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
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
#[test]
fn document_extraction_preserves_reading_order_without_history_or_bytes_changes() {
    let mut session = Session::new();
    let id = session.open("selection.pdf", None, Arc::new(fixture()), None).unwrap();
    let doc = session.get(id).unwrap();
    let bytes = doc.bytes.clone();
    assert_eq!(doc.selected_reader_text(TextSelection::All).unwrap(), "first page\n\n\n\nlast page");
    assert_eq!(doc.selected_reader_text(selection((2, 3), (0, 6))).unwrap(), "page\n\n\n\nlast");
    assert!(Arc::ptr_eq(&bytes, &doc.bytes));
    assert!(!doc.dirty);
    assert_eq!(doc.can_undo(), None);
    assert_eq!(doc.can_redo(), None);
}
#[test]
fn copy_permission_is_enforced_for_user_and_lifted_for_owner() {
    let mut session = Session::new();
    let id = session.open("selection.pdf", None, Arc::new(fixture()), None).unwrap();
    session.apply(id, Edit::Protect(Protection { permissions_password: Some("Owner-original-test".into()), ..Default::default() })).unwrap();
    let bytes = session.save_bytes(id).unwrap();
    let user = session.open("restricted.pdf", None, bytes.clone(), None).unwrap();
    let owner = session.open("owner.pdf", None, bytes, Some("Owner-original-test")).unwrap();
    let doc = session.get(user).unwrap();
    assert!(!doc.allows_copying());
    assert!(doc.selected_reader_text(TextSelection::All).unwrap_err().contains("does not allow copying"));
    assert!(doc.reader_paragraph_selection(0, 0).is_err());
    let doc = session.get(owner).unwrap();
    assert!(doc.allows_copying());
    assert!(doc.selected_reader_text(TextSelection::All).unwrap().contains("first page"));
}
