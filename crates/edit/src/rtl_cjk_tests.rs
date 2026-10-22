//! Editing right-to-left (Hebrew, Arabic) and CJK text, including vertical writing, on
//! synthetic pages built here. Fonts are Type 0 / Identity fonts with ToUnicode maps and no
//! font program: enough for reading, re-encoding and the geometry PdfCraft computes.

use std::collections::HashMap;
use std::sync::Arc;

use pdfcraft_cos::{Document, SaveOptions, write_incremental};

use super::*;

/// A Type 0 font: `chars` get codes 0x0101, 0x0102, … (two bytes), each 1000 units wide.
struct TestFont {
    name: &'static str,
    encoding: &'static str,
    chars: Vec<char>,
    to_unicode: bool,
}

impl TestFont {
    fn code(&self, c: char) -> Option<u16> {
        self.chars.iter().position(|x| *x == c).map(|i| 0x0101 + i as u16)
    }

    /// `<…>` hex string drawing `text` as given (callers pass display order).
    fn hex(&self, text: &str) -> String {
        let codes: String = text.chars().map(|c| format!("{:04X}", self.code(c).unwrap())).collect();
        format!("<{codes}>")
    }
}

fn hebrew_font() -> TestFont {
    let mut chars: Vec<char> = ('\u{05D0}'..='\u{05EA}').collect();
    chars.extend(" 0123456789.,()".chars());
    chars.extend('a'..='z');
    TestFont { name: "F1", encoding: "Identity-H", chars, to_unicode: true }
}

/// Arabic presentation forms (as many Arabic PDFs map their glyphs) and a space.
fn arabic_font() -> TestFont {
    let mut chars: Vec<char> = ('\u{FE80}'..='\u{FEFC}').collect();
    chars.push(' ');
    TestFont { name: "F2", encoding: "Identity-H", chars, to_unicode: true }
}

fn japanese_chars() -> Vec<char> {
    "日本語縦書き文字列のテストです。、「」".chars().collect()
}

fn page(content: &str, fonts: &[TestFont]) -> Document {
    let mut objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        String::new(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let mut resources = String::new();
    for f in fonts {
        let font = objs.len() + 1;
        let cid = font + 1;
        let cmap = font + 2;
        let mut bf = String::new();
        for (i, c) in f.chars.iter().enumerate() {
            let hex: String = c.encode_utf16(&mut [0; 2]).iter().map(|u| format!("{u:04X}")).collect();
            bf.push_str(&format!("<{:04X}> <{hex}>\n", 0x0101 + i));
        }
        let program = format!(
            "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapType 2 def 1 begincodespacerange <0000> <FFFF> endcodespacerange {} beginbfchar\n{bf}endbfchar endcmap CMapName currentdict /CMap defineresource pop end end",
            f.chars.len()
        );
        let to_unicode = if f.to_unicode { format!(" /ToUnicode {cmap} 0 R") } else { String::new() };
        objs.push(format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /Test{} /Encoding /{} /DescendantFonts [{cid} 0 R]{to_unicode} >>",
            f.name, f.encoding
        ));
        objs.push(format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Test{} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 1000 /DW2 [880 -1000] >>",
            f.name
        ));
        objs.push(format!("<< /Length {} >>\nstream\n{program}\nendstream", program.len()));
        resources.push_str(&format!("/{} {font} 0 R ", f.name));
    }
    objs[2] = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R /Resources << /Font << {resources}>> >> >>");
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

fn reopen(doc: &Document) -> Document {
    let bytes = write_incremental(doc, &SaveOptions::default()).unwrap();
    Document::open(Arc::new(bytes)).unwrap()
}

/// What the page shows in font `font`, in content (display) order, decoded with its codes.
fn shown(doc: &Document, font: &TestFont) -> String {
    let p = pdfcraft_model::pages(doc).swap_remove(0);
    let data: Vec<u8> = text::content_streams(doc, &p.dict).into_iter().flat_map(|(_, d)| d).collect();
    let back: HashMap<u16, char> = font.chars.iter().enumerate().map(|(i, c)| (0x0101 + i as u16, *c)).collect();
    let mut current = Vec::new();
    let mut out = String::new();
    for op in pdfcraft_content::parse(&data).ops {
        if op.is("Tf") {
            current = op.name(0).unwrap_or_default().to_vec();
            continue;
        }
        if current != font.name.as_bytes() || !(op.is("Tj") || op.is("TJ")) {
            continue;
        }
        let strings: Vec<Vec<u8>> = match op.operands.first() {
            Some(Object::Array(a)) => a.iter().filter_map(|o| if let Object::String(s) = o { Some(s.bytes.clone()) } else { None }).collect(),
            Some(Object::String(s)) => vec![s.bytes.clone()],
            _ => vec![],
        };
        for s in strings {
            for pair in s.chunks(2) {
                if let [a, b] = pair {
                    out.extend(back.get(&u16::from_be_bytes([*a, *b])));
                }
            }
        }
        out.push('|');
    }
    out
}

fn logical(doc: &Document) -> Vec<String> {
    text::text_blocks(doc, 0).unwrap().into_iter().map(|b| b.text).collect()
}

#[test]
fn hebrew_reads_in_logical_order_and_is_written_in_display_order() {
    let f = hebrew_font();
    // "שלום" shown from the left: ם ו ל ש.
    let mut doc = page(&format!("BT /F1 12 Tf 300 700 Td {} Tj ET", f.hex("םולש")), &[hebrew_font()]);
    let blocks = text::text_blocks(&doc, 0).unwrap();
    assert_eq!(blocks[0].text, "שלום");
    assert!(blocks[0].rtl && !blocks[0].vertical);
    let lines = text::text_lines(&doc, 0).unwrap();
    assert!(lines[0].rtl);
    let right = blocks[0].rect[2];
    text::replace_block(&mut doc, 0, 0, "שלום עולם 2026").unwrap();
    // Display order: the number reads left to right at the left of the right-to-left words.
    assert_eq!(shown(&doc, &f).replace('|', ""), "2026 םלוע םולש");
    let doc = reopen(&doc);
    let after = text::text_blocks(&doc, 0).unwrap();
    assert_eq!(after[0].text, "שלום עולם 2026");
    // The paragraph keeps its right edge and grows to the left.
    assert!((after[0].rect[2] - right).abs() < 0.5, "{} {right}", after[0].rect[2]);
    assert!(after[0].rect[0] < blocks[0].rect[0] - 100.0);
}

#[test]
fn editing_inside_a_right_to_left_line_keeps_the_rest_of_it() {
    let f = hebrew_font();
    let mut doc = page(&format!("BT /F1 12 Tf 300 700 Td {} Tj ET", f.hex("םלוע םולש")), &[hebrew_font()]);
    assert_eq!(logical(&doc), ["שלום עולם"]);
    // Insert a word in the middle (logically), as typing would.
    text::replace_block(&mut doc, 0, 0, "שלום רב עולם").unwrap();
    assert_eq!(shown(&doc, &f).replace('|', ""), "םלוע בר םולש");
    assert_eq!(logical(&doc), ["שלום רב עולם"]);
    // Delete it again.
    text::replace_block(&mut doc, 0, 0, "שלום עולם").unwrap();
    assert_eq!(logical(&doc), ["שלום עולם"]);
    // A left-to-right paragraph with an embedded Hebrew word keeps its left edge.
    let mut doc = page(&format!("BT /F1 12 Tf 72 700 Td {} Tj ET", f.hex("abc םולש def")), &[hebrew_font()]);
    assert_eq!(logical(&doc), ["abc שלום def"]);
    let left = text::text_blocks(&doc, 0).unwrap()[0].rect[0];
    text::replace_block(&mut doc, 0, 0, "abc שלום רב def").unwrap();
    assert_eq!(shown(&doc, &f).replace('|', ""), "abc בר םולש def");
    assert!((text::text_blocks(&doc, 0).unwrap()[0].rect[0] - left).abs() < 0.5);
}

#[test]
fn arabic_in_presentation_forms_reads_as_letters_and_rejoins_when_edited() {
    let f = arabic_font();
    // "سلام" shown from the left in presentation forms: isolated meem (lam-alef doesn't join
    // forwards), final lam-alef, initial seen.
    let mut doc = page(&format!("BT /F2 12 Tf 300 700 Td {} Tj ET", f.hex("\u{FEE1}\u{FEFC}\u{FEB3}")), &[arabic_font()]);
    assert_eq!(logical(&doc), ["سلام"]);
    // Typed letters take the forms their neighbours call for, in display order.
    text::replace_block(&mut doc, 0, 0, "سلام بيت").unwrap();
    assert_eq!(shown(&doc, &f).replace('|', ""), "\u{FE96}\u{FEF4}\u{FE91} \u{FEE1}\u{FEFC}\u{FEB3}");
    assert_eq!(logical(&reopen(&doc)), ["سلام بيت"]);
    // Typing inside a word re-forms the letters around it: "بت" → "بيت".
    let mut doc = page(&format!("BT /F2 12 Tf 300 700 Td {} Tj ET", f.hex("\u{FE96}\u{FE91}")), &[arabic_font()]);
    assert_eq!(logical(&doc), ["بت"]);
    text::replace_block(&mut doc, 0, 0, "بيت").unwrap();
    assert_eq!(shown(&doc, &f).replace('|', ""), "\u{FE96}\u{FEF4}\u{FE91}");
    // Deleting a letter makes its neighbours stand alone again: "بيت" → "بت" (joined: ﺑﺖ).
    text::replace_block(&mut doc, 0, 0, "بت").unwrap();
    assert_eq!(shown(&doc, &f).replace('|', ""), "\u{FE96}\u{FE91}");
}

#[test]
fn arabic_a_font_cannot_show_is_shaped_with_the_fallback_face_or_refused() {
    let f = hebrew_font();
    let mut doc = page(&format!("BT /F1 12 Tf 300 700 Td {} Tj ET", f.hex("םולש")), &[hebrew_font()]);
    let result = text::replace_block(&mut doc, 0, 0, "שלום سلام");
    if pdfcraft_fonts::document_arabic_font().is_none() {
        eprintln!("skipping the Arabic fallback face check: built without a craft-fonts Arab face (set CRAFT_FONTS_DIR)");
        assert!(matches!(&result, Err(EditError::Invalid(m)) if m.contains("Arabic font")), "{result:?}");
        assert_eq!(logical(&doc), ["שלום"], "a refused edit changes nothing");
        return;
    }
    let edit = result.unwrap();
    assert!(edit.substituted.as_deref().is_some_and(|s| s.ends_with("Type3")), "{edit:?}");
    // The shaped clusters carry their letters in ToUnicode: the paragraph reads back as typed.
    let doc = reopen(&doc);
    assert_eq!(logical(&doc), ["שלום سلام"]);
    // The Hebrew (and the space after it) stays in its own font, still in display order, to the
    // right of the Arabic.
    assert_eq!(shown(&doc, &f).replace('|', ""), " םולש");
}

#[test]
fn hostile_bidi_controls_and_huge_runs_are_edited_or_refused_without_panicking() {
    let f = hebrew_font();
    let base = format!("BT /F1 12 Tf 300 700 Td {} Tj ET", f.hex("םולש"));
    let controls = [
        "\u{202E}abc\u{202C}שלום",
        "\u{202B}\u{202B}\u{202B}שלום",
        "\u{2069}\u{2069}\u{2067}שלום\u{2066}",
        &"\u{202A}".repeat(400),
        "\u{200F}\u{061C}",
    ];
    for typed in controls {
        let mut doc = page(&base, &[hebrew_font()]);
        let _ = text::replace_block(&mut doc, 0, 0, typed);
        let _ = text::replace_line(&mut doc, 0, 0, typed);
        let _ = text::text_blocks(&doc, 0).unwrap();
    }
    // A long right-to-left paragraph wraps into rows that each read right to left.
    let long: String = std::iter::repeat_n("שלום עולם ", 2000).collect::<String>().trim_end().to_string();
    let mut doc = page(&base, &[hebrew_font()]);
    text::rewrite_block(&mut doc, 0, 0, Some(&long), &BlockStyle { width: Some(200.0), ..Default::default() }).unwrap();
    let blocks = text::text_blocks(&doc, 0).unwrap();
    assert!(blocks.len() + blocks.iter().map(|b| b.line_count).sum::<usize>() > 100);
    assert!(blocks.iter().all(|b| b.text.starts_with("שלום") || b.text.starts_with("עולם")), "{:?}", &blocks[..2]);
    // Visual text read back from a page in display order with odd controls in it.
    let odd = format!("BT /F1 12 Tf 300 700 Td {} Tj ET", f.hex("((םו)ל)ש 12,3."));
    let doc = page(&odd, &[hebrew_font()]);
    assert_eq!(text::text_blocks(&doc, 0).unwrap().len(), 1);
}

#[test]
fn text_without_to_unicode_is_left_alone() {
    let mut no_map = arabic_font();
    no_map.to_unicode = false;
    let hex = no_map.hex("\u{FEE1}\u{FEFC}\u{FEB3}");
    let content = format!("BT /F2 12 Tf 300 700 Td {hex} Tj ET BT /F1 12 Tf 300 650 Td {} Tj ET", hebrew_font().hex("םולש"));
    let mut doc = page(&content, &[hebrew_font(), no_map]);
    // The undecodable line has no text to edit; the Hebrew one still works.
    let blocks = text::text_blocks(&doc, 0).unwrap();
    assert_eq!(blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(), ["שלום"]);
    text::replace_block(&mut doc, 0, 0, "עולם").unwrap();
    assert!(page_has(&doc, &hex), "the undecodable line is kept byte for byte");
}

fn page_has(doc: &Document, needle: &str) -> bool {
    let p = pdfcraft_model::pages(doc).swap_remove(0);
    let data: Vec<u8> = text::content_streams(doc, &p.dict).into_iter().flat_map(|(_, d)| d).collect();
    // Content is rewritten with literal strings; compare the codes.
    let codes: Vec<u8> = (1..needle.len() - 1).step_by(2).filter_map(|i| u8::from_str_radix(needle.get(i..i + 2)?, 16).ok()).collect();
    let ops = pdfcraft_content::parse(&data).ops;
    ops.iter().any(|op| op.is("Tj") && matches!(op.operands.first(), Some(Object::String(s)) if s.bytes == codes))
        || String::from_utf8_lossy(&data).contains(needle)
}

#[test]
fn vertical_identity_v_text_is_a_column_rewritten_in_its_own_font() {
    let f = TestFont { name: "F3", encoding: "Identity-V", chars: japanese_chars(), to_unicode: true };
    let mut doc = page(&format!("BT /F3 12 Tf 300 700 Td {} Tj ET", f.hex("日本語")), &[f]);
    let lines = text::text_lines(&doc, 0).unwrap();
    assert!(lines[0].vertical && !lines[0].rtl);
    // A column: one em wide, three em tall, running down from the first glyph's origin.
    let r = lines[0].rect;
    assert!((r[2] - r[0] - 12.0).abs() < 0.01 && (r[3] - r[1] - 36.0).abs() < 0.01 && (r[3] - 700.0).abs() < 0.01, "{r:?}");
    let blocks = text::text_blocks(&doc, 0).unwrap();
    assert!(blocks[0].vertical);
    text::replace_block(&mut doc, 0, 0, "縦書きの文字列").unwrap();
    let f = TestFont { name: "F3", encoding: "Identity-V", chars: japanese_chars(), to_unicode: true };
    assert_eq!(shown(&doc, &f).replace('|', ""), "縦書きの文字列");
    let doc2 = reopen(&doc);
    let lines = text::text_lines(&doc2, 0).unwrap();
    assert_eq!(lines[0].text, "縦書きの文字列");
    assert!((lines[0].rect[3] - lines[0].rect[1] - 84.0).abs() < 0.01, "{:?}", lines[0].rect);
    // Characters the font doesn't have, and formatting, are refused with the document unchanged.
    let mut doc3 = reopen(&doc);
    let missing = text::replace_block(&mut doc3, 0, 0, "漢");
    assert!(matches!(&missing, Err(EditError::Invalid(m)) if m.contains("vertical")), "{missing:?}");
    let styled = text::rewrite_block(&mut doc3, 0, 0, None, &BlockStyle { size: Some(20.0), ..Default::default() });
    assert!(matches!(&styled, Err(EditError::Invalid(m)) if m.contains("vertical")), "{styled:?}");
    assert_eq!(text::text_lines(&doc3, 0).unwrap()[0].text, "縦書きの文字列");
}

#[test]
fn cjk_paragraphs_wrap_between_characters_and_keep_punctuation_off_line_starts() {
    let f = TestFont { name: "F4", encoding: "Identity-H", chars: japanese_chars(), to_unicode: true };
    let mut doc = page(&format!("BT /F4 12 Tf 72 700 Td {} Tj ET", f.hex("日本語")), &[f]);
    let typed = "日本語の文字列のテストです。日本語の文字列のテストです。日本語の文字列のテストです。";
    text::rewrite_block(&mut doc, 0, 0, Some(typed), &BlockStyle { width: Some(120.0), ..Default::default() }).unwrap();
    let lines = text::text_lines(&doc, 0).unwrap();
    assert!(lines.len() >= 3, "{:?}", lines.iter().map(|l| &l.text).collect::<Vec<_>>());
    assert!(lines.iter().all(|l| !l.text.starts_with('。')), "{:?}", lines.iter().map(|l| &l.text).collect::<Vec<_>>());
    assert!(lines.iter().all(|l| l.rect[2] - l.rect[0] <= 120.5));
    let joined: String = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(joined, typed);
}

#[test]
fn chinese_and_korean_fall_back_to_a_cjk_face_that_has_every_character() {
    let f = hebrew_font();
    for typed in ["欢迎光临", "한국어 문서"] {
        let mut doc = page(&format!("BT /F1 12 Tf 72 700 Td {} Tj ET", f.hex("abc")), &[hebrew_font()]);
        let faces = pdfcraft_fonts::document_cjk_fonts_for_style(false, false);
        let covered = faces.iter().any(|face| typed.chars().filter(|c| *c != ' ').all(|c| pdfcraft_fonts::japanese_glyph_from(face, c).is_ok()));
        let result = text::replace_block(&mut doc, 0, 0, typed);
        if !covered {
            eprintln!("skipping the {typed:?} fallback check: no craft-fonts face has all of it (set CRAFT_FONTS_DIR)");
            assert!(result.is_err(), "{result:?}");
            assert_eq!(logical(&doc), ["abc"]);
            continue;
        }
        assert!(result.unwrap().substituted.is_some_and(|s| s.ends_with("Type3")));
        assert_eq!(logical(&reopen(&doc)), [typed]);
    }
}
