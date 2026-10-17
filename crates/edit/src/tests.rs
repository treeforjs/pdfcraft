use std::sync::Arc;

use pdfcraft_cos::{Document, SaveOptions, write_incremental};

use super::*;

/// Three pages: an upright page with content that leaves the graphics state changed (an
/// unbalanced `cm`), a page rotated 90° and one with inherited, shared resources.
fn fixture() -> Document {
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 600 800] /Resources 6 0 R >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>".into(),
        "<< /Type /Page /Parent 2 0 R /Rotate 90 /Contents [7 0 R] >>".into(),
        "<< /Type /Page /Parent 2 0 R >>".into(),
        "<< /Font << /F1 8 0 R >> >>".into(),
        "<< /Length 37 >>\nstream\n2 0 0 2 0 0 cm BT /F1 9 Tf (Hi) Tj ET\nendstream".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>".into(),
    ];
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
    hayro_syntax::Pdf::new(bytes.clone()).expect("parses");
    Document::open(Arc::new(bytes)).unwrap()
}

/// The decoded content streams of a page, in order.
fn streams(doc: &Document, page: usize) -> Vec<String> {
    let p = &pdfcraft_model::pages(doc)[page];
    let c = p.dict.get(b"Contents").cloned();
    let list = match c.map(|c| doc.resolve(&c)).as_deref() {
        Some(Object::Array(a)) => a.clone(),
        Some(_) => vec![p.dict.get(b"Contents").cloned().unwrap()],
        None => vec![],
    };
    list.iter().map(|o| String::from_utf8_lossy(&stream_bytes(doc, o).unwrap()).into_owned()).collect()
}

fn cx() -> Context {
    Context { date: (2026, 10, 1) }
}

#[test]
fn tokens_expand() {
    let c = cx();
    assert_eq!(expand("Page <<1>> of <<n>>", 3, 10, 0, &c), "Page 3 of 10");
    assert_eq!(expand("<<1 of n>> · <<Page 1>> · <<1/n>>", 2, 5, 0, &c), "2 of 5 · Page 2 · 2/5");
    assert_eq!(expand("<<m/d/yyyy>> <<yyyy-mm-dd>> <<mmmm d, yyyy>>", 1, 1, 0, &c), "10/1/2026 2026-10-01 October 1, 2026");
    assert_eq!(expand("<<Bates Number#6#100#ABC#-X>>", 1, 1, 104, &c), "ABC000104-X");
    assert_eq!(expand("keep <<unknown>> and <<unclosed", 1, 1, 0, &c), "keep <<unknown>> and <<unclosed");
    assert_eq!(bates_start("x <<Bates Number#6#100#A#B>>"), Some(100));
}

#[test]
fn header_and_footer_are_drawn_in_display_space_and_wrap_the_original_content() {
    let mut doc = fixture();
    let hf = HeaderFooter {
        text: ["Left".into(), String::new(), "<<Page 1 of n>>".into(), String::new(), "Confidential (draft)".into(), String::new()],
        ..HeaderFooter::default()
    };
    add_header_footer(&mut doc, &[0, 1, 2], &hf, false, &cx()).unwrap();
    let doc = reopen(&doc);
    let s0 = streams(&doc, 0);
    // q-wrapper, original, Q-wrapper, header/footer.
    assert_eq!(s0.len(), 4, "{s0:?}");
    assert_eq!((s0[0].as_str(), s0[2].as_str()), ("q %PdfCraft\n", "Q %PdfCraft\n"));
    let mark = &s0[3];
    assert!(mark.contains("/PCMark /HeaderFooter") && mark.contains("(Page 1 of 3) Tj") && mark.contains("(Confidential \\(draft\\)) Tj"), "{mark}");
    assert!(mark.contains("1 0 0 1 0 0 cm"), "upright page: identity");
    // The rotated page maps display space onto its rotated crop box.
    let s1 = streams(&doc, 1);
    assert!(s1.last().unwrap().contains("0 1 -1 0 600 0 cm") && s1.last().unwrap().contains("(Page 2 of 3)"), "{s1:?}");
    // A page without content gets just the mark; its inherited resources are copied, not changed.
    assert_eq!(streams(&doc, 2).len(), 1);
    let p2 = &pdfcraft_model::pages(&doc)[2];
    let res = doc.resolve(p2.dict.get(b"Resources").unwrap());
    let fonts = doc.resolve(res.as_dict().unwrap().get(b"Font").unwrap());
    assert!(fonts.as_dict().unwrap().contains(b"PCHelv") && fonts.as_dict().unwrap().contains(b"F1"));
    let shared = doc.get(pdfcraft_cos::ObjRef::new(6, 0));
    let shared_fonts = doc.resolve(shared.as_dict().unwrap().get(b"Font").unwrap());
    assert!(!shared_fonts.as_dict().unwrap().contains(b"PCHelv"), "the shared dictionary is untouched");
    assert_eq!(marks_present(&doc), [MarkKind::HeaderFooter]);
}

#[test]
fn replace_and_remove_restore_the_original_content() {
    let mut doc = fixture();
    let original = streams(&doc, 0);
    let mut hf = HeaderFooter::default();
    hf.text[1] = "First".into();
    add_header_footer(&mut doc, &[0], &hf, false, &cx()).unwrap();
    hf.text[1] = "Second".into();
    add_header_footer(&mut doc, &[0], &hf, true, &cx()).unwrap();
    let s = streams(&doc, 0);
    assert_eq!(s.iter().filter(|x| x.contains("/PCMark")).count(), 1, "replaced, not added");
    assert!(s.last().unwrap().contains("(Second)"));
    add_watermark(&mut doc, &[0], &Watermark { text: "DRAFT".into(), ..Watermark::default() }, false).unwrap();
    add_background(&mut doc, &[0], &Background { color: [1.0, 1.0, 0.9], opacity: 1.0, ..Background::default() }, false).unwrap();
    assert_eq!(marks_present(&doc).len(), 3);
    assert_eq!(remove_marks(&mut doc, &[0], MarkKind::HeaderFooter).unwrap(), 1);
    assert_eq!(remove_marks(&mut doc, &[0], MarkKind::Watermark).unwrap(), 1);
    let s = streams(&doc, 0);
    assert!(s[0].contains("/PCMark /Background"), "the background stays behind: {s:?}");
    assert_eq!(remove_marks(&mut doc, &[0], MarkKind::Background).unwrap(), 1);
    assert_eq!(streams(&reopen(&doc), 0), original, "back to exactly the original content");
}

#[test]
fn watermarks_rotate_fade_and_can_go_behind() {
    let mut doc = fixture();
    let wm = Watermark { text: "CONFIDENTIAL\nDo not copy".into(), opacity: 0.3, rotation: 45.0, behind: true, ..Watermark::default() };
    add_watermark(&mut doc, &[0], &wm, false).unwrap();
    let s = streams(&doc, 0);
    assert!(s[0].contains("/PCMark /Watermark"), "behind: first");
    assert!(s[0].contains("/PCGS30 gs") && s[0].contains("0.707 0.707 -0.707 0.707 300 400 cm"), "{}", s[0]);
    assert!(s[0].contains("(CONFIDENTIAL) Tj") && s[0].contains("(Do not copy) Tj"));
    assert_eq!(s.len(), 2, "no wrapper needed for content behind");
}

#[test]
fn invalid_requests_change_nothing() {
    let mut doc = fixture();
    assert!(matches!(add_header_footer(&mut doc, &[0], &HeaderFooter::default(), false, &cx()), Err(EditError::Invalid(_))));
    assert!(matches!(add_watermark(&mut doc, &[0], &Watermark::default(), false), Err(EditError::Invalid(_))));
    assert_eq!(
        add_background(&mut doc, &[9], &Background { color: [1.0; 3], opacity: 1.0, ..Background::default() }, false),
        Err(EditError::NoSuchPage(9))
    );
    assert!(!doc.is_modified());
}

#[test]
fn flattening_draws_appearances_into_the_page_and_removes_the_comments() {
    use pdfcraft_annot::{Meta, NewAnnotation, NoteIcon, Shape, Style, add_annotation, add_reply};
    let mut doc = fixture();
    let meta = Meta { date: None, id: "x".into() };
    let add = |doc: &mut Document, shape: Shape| {
        let style = Style::default_for(&shape);
        add_annotation(doc, &NewAnnotation { page: 0, shape, style, contents: "c".into(), author: "a".into() }, &meta).unwrap()
    };
    add(&mut doc, Shape::Rectangle { rect: [10.0, 10.0, 110.0, 60.0] });
    let note = add(&mut doc, Shape::Note { at: [200.0, 700.0], icon: NoteIcon::Comment });
    add_reply(&mut doc, 0, note, "reply", "b", &meta).unwrap();
    let before = streams(&doc, 0);
    let n = flatten(&mut doc, &[0], true, false).unwrap();
    assert_eq!(n, 2, "the rectangle and the note icon are drawn; the reply has nothing to draw");
    let doc = reopen(&doc);
    let p = &pdfcraft_model::pages(&doc)[0];
    assert!(!p.dict.contains(b"Annots"), "comments, pop-up and reply are gone");
    let s = streams(&doc, 0);
    assert_eq!(s.len(), before.len() + 3, "wrapped original + flattened content: {s:?}");
    let flat = s.last().unwrap();
    assert!(flat.contains("/PCFl0 Do") && flat.contains("/PCFl1 Do"), "{flat}");
    // The rectangle's appearance is drawn at its rectangle (bbox = rect, identity mapping).
    assert!(flat.contains("q 1 0 0 1 0 0 cm /PCFl0 Do Q"), "{flat}");
    // The note's 20×20 icon box is mapped onto its rect at (200, 680).
    assert!(flat.contains("1 0 0 1 200 680 cm /PCFl1 Do"), "{flat}");
    let res = doc.resolve(p.dict.get(b"Resources").unwrap());
    let xo = doc.resolve(res.as_dict().unwrap().get(b"XObject").unwrap());
    assert!(xo.as_dict().unwrap().contains(b"PCFl1"));
    assert!(marks_present(&doc).is_empty(), "flattened content is not a removable mark");
}

#[test]
fn added_text_and_images_are_page_content_that_stays_editable() {
    let mut doc = fixture();
    let text = AddedText { rect: [72.0, 600.0, 300.0, 700.0], text: "Approved by Ada\nSecond line".into(), size: 14.0, ..AddedText::default() };
    assert_eq!(add_content(&mut doc, 0, &Content::Text(text.clone())).unwrap(), 0);
    // An 1×1 gray image, placed on the rotated page.
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Image"));
    d.set(b"Width".to_vec(), Object::Int(1));
    d.set(b"Height".to_vec(), Object::Int(1));
    d.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
    d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
    let img = doc.add(Object::Stream(Stream::from_raw(d, vec![128])));
    add_content(&mut doc, 1, &Content::Image(AddedImage::new([10.0, 10.0, 110.0, 60.0], img))).unwrap();
    let doc2 = reopen(&doc);
    let all = list_added(&doc2);
    assert_eq!(all.len(), 2);
    let Content::Text(t) = &all[0].content else { panic!() };
    assert_eq!((t.text.as_str(), t.size), ("Approved by Ada\nSecond line", 14.0));
    assert_eq!(t.rect, [72.0, 700.0 - 2.0 * 14.0 * 1.2, 300.0, 700.0], "the box height follows the two lines");
    let page0 = streams(&doc2, 0).join("\n");
    assert!(page0.contains("(Approved by Ada) Tj") && page0.contains("(Second line) Tj") && page0.contains("/PCFHelvetica 14 Tf"), "{page0}");
    // The rotated page draws in display space: the view matrix comes first.
    let page1 = streams(&doc2, 1).join("\n");
    assert!(page1.contains("q 0 1 -1 0 600 0 cm") && page1.contains(&format!("/PCImg{} Do", img.num)), "{page1}");
    // Edit: move, restyle, retype; then delete.
    let mut doc = doc2;
    let moved = AddedText {
        rect: [100.0, 500.0, 300.0, 520.0],
        text: "Approved".into(),
        bold: true,
        family: Family::Times,
        align: Align::Right,
        color: [1.0, 0.0, 0.0],
        ..text
    };
    update_content(&mut doc, 0, 0, &Content::Text(moved)).unwrap();
    let page0 = streams(&doc, 0).join("\n");
    assert!(page0.contains("/PCFTimesBold 14 Tf 1 0 0 rg") && !page0.contains("Second line"), "{page0}");
    assert!(
        update_content(&mut doc, 1, 0, &Content::Text(AddedText { text: "x".into(), rect: [0.0, 0.0, 50.0, 10.0], ..AddedText::default() })).is_err(),
        "kinds don't change"
    );
    delete_content(&mut doc, 0, 0).unwrap();
    assert_eq!(list_added(&doc).len(), 1);
    assert!(!streams(&doc, 0).join("").contains("Approved"));
    assert!(add_content(&mut doc, 0, &Content::Text(AddedText { rect: [0.0, 0.0, 100.0, 10.0], ..AddedText::default() })).is_err(), "empty text");
    // Page marks ignore added items.
    assert!(marks_present(&doc).is_empty());
}

#[test]
fn added_arabic_text_is_shaped_in_display_order_and_stays_searchable() {
    let mut doc = fixture();
    let text =
        AddedText { rect: [72.0, 600.0, 400.0, 700.0], text: "مرحبا World".into(), size: 14.0, align: Align::Right, ..AddedText::default() };
    if pdfcraft_fonts::document_arabic_font().is_none() {
        eprintln!("skipping the Arabic drawing checks: built without a craft-fonts Arab face (set CRAFT_FONTS_DIR)");
        let err = add_content(&mut doc, 0, &Content::Text(text)).unwrap_err();
        assert!(matches!(&err, EditError::Invalid(m) if m.contains("CRAFT_FONTS_DIR")), "{err:?}");
        assert!(!doc.is_modified(), "nothing is written when the font is missing");
        return;
    }
    add_content(&mut doc, 0, &Content::Text(text.clone())).unwrap();
    // A second Arabic item on the same page keeps its own fonts.
    add_content(&mut doc, 0, &Content::Text(AddedText { text: "شكرا".into(), rect: [72.0, 500.0, 400.0, 520.0], ..text.clone() })).unwrap();
    let doc = reopen(&doc);
    let added = list_added(&doc);
    let Content::Text(t) = &added[0].content else { panic!() };
    assert_eq!(t.text, "مرحبا World", "the logical text is kept for editing");
    let s = streams(&doc, 0);
    let item = s.iter().find(|s| s.contains("(World) Tj")).unwrap();
    // A right-to-left paragraph: "World" is drawn left of the Arabic word.
    let x_before = |s: &str, at: usize| -> f64 {
        let tm = s[..at].rfind("1 0 0 1 ").unwrap() + 8;
        s[tm..].split(' ').next().unwrap().parse().unwrap()
    };
    let latin_x = x_before(item, item.find("(World) Tj").unwrap());
    let arabic_at = item.find("/PCAr").unwrap();
    let arabic_x = x_before(item, item[arabic_at..].find(" Tj").unwrap() + arabic_at);
    assert!(latin_x < arabic_x, "{item}");
    // The Arabic glyphs come from Type 3 fonts whose ToUnicode maps give back each letter.
    let p = &pdfcraft_model::pages(&doc)[0];
    let res = doc.resolve(p.dict.get(b"Resources").unwrap());
    let fonts = doc.resolve(res.as_dict().unwrap().get(b"Font").unwrap()).as_dict().unwrap().clone();
    let arabic: Vec<_> = fonts.iter().filter(|(k, _)| k.starts_with(b"PCAr")).map(|(_, v)| doc.resolve(v).as_dict().unwrap().clone()).collect();
    assert_eq!(arabic.len(), 2, "one font per item");
    let mut mapped = String::new();
    for f in &arabic {
        assert_eq!(f.name(b"Subtype"), Some(&b"Type3"[..]));
        mapped += &String::from_utf8_lossy(&stream_bytes(&doc, f.get(b"ToUnicode").unwrap()).unwrap());
    }
    for letter in ["0645", "0631", "062D", "0628", "0627", "0634", "0643"] {
        assert!(mapped.contains(&format!("<{letter}>")), "{letter} in {mapped}");
    }
}

/// The `PCAr` font names in page `page`'s resources.
fn arabic_font_names(doc: &Document, page: usize) -> Vec<String> {
    let p = &pdfcraft_model::pages(doc)[page];
    let Some(res) = p.dict.get(b"Resources").map(|r| doc.resolve(r)) else { return vec![] };
    let Some(fonts) = res.as_dict().and_then(|r| r.get(b"Font")).map(|f| doc.resolve(f)) else { return vec![] };
    fonts.as_dict().unwrap().iter().map(|(k, _)| String::from_utf8_lossy(k).into_owned()).filter(|k| k.starts_with("PCAr")).collect()
}

#[test]
fn arabic_items_keep_their_own_fonts_through_saves_updates_and_deletes() {
    let mut doc = fixture();
    let text = AddedText { rect: [72.0, 600.0, 400.0, 700.0], text: "مرحبا".into(), size: 14.0, ..AddedText::default() };
    if pdfcraft_fonts::document_arabic_font().is_none() {
        // An item that already holds Arabic (stored by an older version, drawn as "?") can still
        // be moved and retyped without the face.
        add_content(&mut doc, 0, &Content::Text(AddedText { text: "x".into(), ..text.clone() })).unwrap();
        update_content(&mut doc, 0, 0, &Content::Text(text)).unwrap();
        assert!(streams(&doc, 0).join("").contains("(?????) Tj"));
        return;
    }
    // A full save renumbers objects but keeps resource names, so a font named after its object
    // number may meet a name already on the page: here, every name it could take.
    let mut taken = Dict::new();
    for num in 1..2_000 {
        taken.set(format!("PCAr{num}").into_bytes(), Object::name("Taken"));
    }
    let mut res = Dict::new();
    res.set(b"Font".to_vec(), Object::Dict(taken));
    let page = pdfcraft_model::pages(&doc)[0].obj;
    doc.update_dict(page, |d| d.set(b"Resources".to_vec(), Object::Dict(res))).unwrap();
    add_content(&mut doc, 0, &Content::Text(text.clone())).unwrap();
    add_content(&mut doc, 0, &Content::Text(AddedText { text: "شكرا".into(), rect: [72.0, 500.0, 400.0, 520.0], ..text.clone() })).unwrap();
    let names = arabic_font_names(&doc, 0);
    assert_eq!(names.len(), 1_999 + 2, "nothing replaced");
    let p = &pdfcraft_model::pages(&doc)[0];
    let fonts = doc.resolve(p.dict.get(b"Resources").unwrap()).as_dict().unwrap().get(b"Font").cloned().unwrap();
    let fonts = doc.resolve(&fonts).as_dict().unwrap().clone();
    assert_eq!(fonts.iter().filter(|(_, v)| v.as_name() == Some(&b"Taken"[..])).count(), 1_999);
    let first = own(&doc, 0);
    assert!(first.len() == 1 && first[0].contains('_'), "a free name, not a taken one: {first:?}");
    // Updating an item swaps its fonts; deleting it removes them.
    update_content(&mut doc, 0, 0, &Content::Text(AddedText { text: "أهلا".into(), ..text })).unwrap();
    let names = arabic_font_names(&doc, 0);
    assert!(names.len() == 2_001 && !names.contains(&first[0]), "the replaced item's font is gone");
    delete_content(&mut doc, 0, 0).unwrap();
    delete_content(&mut doc, 0, 0).unwrap();
    assert_eq!(arabic_font_names(&doc, 0).len(), 1_999, "only the page's own names are left");
    reopen(&doc);
}

/// The `PCAr` fonts the stream of added item `index` on page 0 shows text with.
fn own(doc: &Document, index: usize) -> Vec<String> {
    let s = String::from_utf8_lossy(&stream_bytes(doc, &Object::Ref(list_added(doc)[index].obj)).unwrap()).into_owned();
    let mut names: Vec<String> =
        s.split_whitespace().filter_map(|w| w.strip_prefix('/')).filter(|w| w.starts_with("PCAr")).map(String::from).collect();
    names.dedup();
    names
}

#[test]
fn odd_arabic_text_never_panics() {
    let mut doc = fixture();
    let long = "بسم الله ".repeat(2_000);
    for s in ["\u{202E}ب\u{064B}\u{064B} (]", "\u{064B}", "ا\u{200F}\u{2067}b\u{2069}", "ا\n\nب\tc", "ﷺ ١٢٣ 456", long.as_str()] {
        let r = add_content(&mut doc, 0, &Content::Text(AddedText { rect: [0.0, 800.0, 300.0, 780.0], text: s.into(), ..AddedText::default() }));
        assert_eq!(r.is_ok(), pdfcraft_fonts::document_arabic_font().is_some(), "{:?}: {r:?}", s.chars().take(12).collect::<String>());
    }
    // U+2029 and U+0085 (paragraph and line separators) have no WinAnsi code, so the standard font
    // would draw them as `?`: refused, by name, rather than written (#125). Laying it out still
    // mustn't panic.
    let r = add_content(
        &mut doc,
        0,
        &Content::Text(AddedText { rect: [0.0, 800.0, 300.0, 780.0], text: "ا\u{2029}ب\rc\u{85}د\n".into(), ..AddedText::default() }),
    );
    assert!(r.is_err(), "{r:?}");
    if pdfcraft_fonts::document_arabic_font().is_some() {
        assert!(matches!(&r, Err(EditError::Invalid(m)) if m.contains("U+2029") || m.contains("U+0085")), "{r:?}");
    }
    // A character the face lacks is an error that names it, not a box or a crash.
    if pdfcraft_fonts::document_arabic_font().is_some() && !pdfcraft_fonts::arabic_has('\u{FDFD}') {
        let r = add_content(
            &mut doc,
            0,
            &Content::Text(AddedText { rect: [0.0, 800.0, 300.0, 780.0], text: "ب \u{FDFD}".into(), ..AddedText::default() }),
        );
        assert!(matches!(&r, Err(EditError::Invalid(m)) if m.contains('\u{FDFD}')), "{r:?}");
    }
}

/// One page with Helvetica (WinAnsi) and a subset font that has only the glyphs it uses.
fn text_page(content: &str) -> Document {
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
        // Subset: glyphs for a (97) and b (98) only.
        "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Arial /FirstChar 97 /LastChar 99 /Widths [500 520 0] /Encoding /WinAnsiEncoding >>"
            .into(),
    ];
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

fn descriptor_text_page() -> Document {
    let content = "BT /F1 12 Tf 72 700 Td (ab) Tj ET";
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+NeutralFace /FirstChar 97 /LastChar 98 /Widths [500 500] /FontDescriptor 6 0 R /Encoding /WinAnsiEncoding >>".into(),
        "<< /Type /FontDescriptor /FontName /NeutralFace /Flags 262208 /ItalicAngle -12 /FontWeight 700 /Ascent 900 /Descent -250 >>".into(),
    ];
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

/// The Japanese fallback face comes from craft-fonts, an optional build input.
fn without_craft_fonts(test: &str) -> bool {
    if pdfcraft_fonts::document_japanese_font().is_some() {
        return false;
    }
    eprintln!("skipping {test}: built without craft-fonts (set CRAFT_FONTS_DIR to run it)");
    true
}

#[test]
fn japanese_line_uses_unicode_type3_fallback() {
    if without_craft_fonts("japanese_line_uses_unicode_type3_fallback") {
        return;
    }
    let mut doc = text_page("BT /F2 12 Tf 72 700 Td (ab) Tj ET");
    let replacement = "25362738こんにちは、お元気ですか？ 2.3444";
    let result = text::replace_line(&mut doc, 0, 0, replacement).unwrap();
    assert_eq!(result.substituted.as_deref(), Some("BIZ UDPGothic Type3"));
    let bytes = page_content_bytes(&doc, 0);
    let content = String::from_utf8_lossy(&bytes);
    assert!(content.contains("/PCJp"), "{content}");
    let reopened = reopen(&doc);
    assert_eq!(text::text_lines(&reopened, 0).unwrap()[0].text, replacement);
}

fn styled_text_page(base: &str) -> Document {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Original) Tj ET");
    let r = pdfcraft_cos::ObjRef::new(5, 0);
    let mut font = doc.get(r).as_dict().unwrap().clone();
    font.set(b"BaseFont".to_vec(), pdfcraft_cos::Object::name(base));
    doc.set(r, pdfcraft_cos::Object::Dict(font));
    doc
}

fn fallback_paths(doc: &Document) -> Vec<Vec<u8>> {
    let p = pdfcraft_model::pages(doc).swap_remove(0);
    let res = doc.resolve(p.dict.get(b"Resources").unwrap());
    let fonts = doc.resolve(res.as_dict().unwrap().get(b"Font").unwrap());
    let font = doc.resolve(fonts.as_dict().unwrap().get(b"PCJp").unwrap());
    let procs = doc.resolve(font.as_dict().unwrap().get(b"CharProcs").unwrap());
    procs
        .as_dict()
        .unwrap()
        .iter()
        .map(|(_, object)| match &*doc.resolve(object) {
            pdfcraft_cos::Object::Stream(s) => s.decoded().unwrap(),
            other => panic!("expected a glyph stream, got {other:?}"),
        })
        .collect()
}

#[test]
fn japanese_line_fallback_uses_real_source_family_and_weight() {
    if without_craft_fonts("japanese_line_fallback_uses_real_source_family_and_weight") {
        return;
    }
    let mut sans = styled_text_page("Helvetica");
    let mut bold = descriptor_text_page();
    let mut serif = styled_text_page("ABCDEF+ShipporiMincho-Regular");
    let replacement = "見本商会　御中";
    text::replace_line(&mut sans, 0, 0, replacement).unwrap();
    text::replace_line(&mut bold, 0, 0, replacement).unwrap();
    text::replace_line(&mut serif, 0, 0, replacement).unwrap();
    assert_ne!(fallback_paths(&sans), fallback_paths(&serif), "sans source must not use Mincho outlines");
    if pdfcraft_fonts::CRAFT_FONTS.iter().any(|f| f.family == "BIZ UDPGothic" && f.style == "Bold") {
        assert_ne!(fallback_paths(&sans), fallback_paths(&bold), "bold must change the actual glyph paths");
    }
    for doc in [&sans, &bold, &serif] {
        assert_eq!(text::text_lines(&reopen(doc), 0).unwrap()[0].text, replacement);
    }
}

#[test]
fn japanese_paragraph_fallback_honors_requested_family_and_weight() {
    if without_craft_fonts("japanese_paragraph_fallback_honors_requested_family_and_weight") {
        return;
    }
    let replacement = "見本商会　御中";
    let mut regular = styled_text_page("Helvetica");
    let mut bold = styled_text_page("Helvetica");
    let mut serif = styled_text_page("Helvetica");
    text::replace_block(&mut regular, 0, 0, replacement).unwrap();
    text::rewrite_block(
        &mut bold,
        0,
        0,
        Some(replacement),
        &text::BlockStyle { family: Some((added::Family::Helvetica, true, false)), ..Default::default() },
    )
    .unwrap();
    text::rewrite_block(
        &mut serif,
        0,
        0,
        Some(replacement),
        &text::BlockStyle { family: Some((added::Family::Times, false, false)), ..Default::default() },
    )
    .unwrap();
    assert_ne!(fallback_paths(&regular), fallback_paths(&serif), "requested serif must change actual outlines");
    if pdfcraft_fonts::CRAFT_FONTS.iter().any(|f| f.family == "BIZ UDPGothic" && f.style == "Bold") {
        assert_ne!(fallback_paths(&regular), fallback_paths(&bold), "requested bold must change actual outlines");
        let mut source_bold = styled_text_page("Helvetica-Bold");
        text::replace_block(&mut source_bold, 0, 0, replacement).unwrap();
        assert_eq!(fallback_paths(&bold), fallback_paths(&source_bold));
    }
    for doc in [&regular, &bold, &serif] {
        assert_eq!(text::text_blocks(&reopen(doc), 0).unwrap()[0].text, replacement);
    }
}

#[test]
fn japanese_fallback_reports_the_face_after_save_and_reopen() {
    if without_craft_fonts("japanese_fallback_reports_the_face_after_save_and_reopen") {
        return;
    }
    // F2 is (a subset of) Arial: a regular sans face, so the fallback is the regular Gothic.
    let face = pdfcraft_fonts::document_japanese_font_for_style(false, false).unwrap();
    let expected = format!("{}-{}", face.family, face.style).replace(' ', "");
    for paragraph in [false, true] {
        let mut doc = text_page("BT /F2 12 Tf 72 700 Td (ab) Tj ET");
        let replacement = "見本商会　御中";
        if paragraph {
            text::replace_block(&mut doc, 0, 0, replacement).unwrap();
        } else {
            text::replace_line(&mut doc, 0, 0, replacement).unwrap();
        }
        for d in [&doc, &reopen(&doc)] {
            let lines = text::text_lines(d, 0).unwrap();
            assert_eq!(lines[0].text, replacement);
            assert_eq!(lines[0].base_font, expected, "report the real fallback face, not an empty BaseFont");
            assert_eq!(text::text_blocks(d, 0).unwrap()[0].base_font, expected);
        }
    }
}

#[test]
fn cyrillic_in_a_serif_line_falls_back_to_a_face_that_has_it() {
    if without_craft_fonts("cyrillic_in_a_serif_line_falls_back_to_a_face_that_has_it") {
        return;
    }
    let replacement = "Привет, world";
    let has_all = |f: &pdfcraft_fonts::CraftFont| replacement.chars().all(|ch| pdfcraft_fonts::japanese_glyph_from(f, ch).is_ok());
    if !pdfcraft_fonts::CRAFT_FONTS.iter().any(|f| f.covers("Jpan") && has_all(f)) {
        eprintln!("skipping: no Japanese fallback face has Cyrillic");
        return;
    }
    for paragraph in [false, true] {
        let mut doc = styled_text_page("Times-Roman");
        if paragraph {
            text::replace_block(&mut doc, 0, 0, replacement).unwrap();
        } else {
            text::replace_line(&mut doc, 0, 0, replacement).unwrap();
        }
        assert_eq!(text::text_lines(&reopen(&doc), 0).unwrap()[0].text, replacement);
    }
}

#[test]
fn japanese_paragraph_uses_unicode_type3_fallback() {
    if without_craft_fonts("japanese_paragraph_uses_unicode_type3_fallback") {
        return;
    }
    let mut doc = text_page("BT /F2 12 Tf 72 700 Td (ab) Tj ET");
    let replacement = "こんにちは、お元気ですか？ 2.3444 日本語の文章";
    text::replace_block(&mut doc, 0, 0, replacement).unwrap();
    let reopened = reopen(&doc);
    assert_eq!(text::text_blocks(&reopened, 0).unwrap()[0].text, replacement);
}

/// Every Type 3 fallback glyph starts with a well-formed `d1`: six operands, the second 0, and a
/// box that encloses every point of the glyph. Acrobat shows a bullet for a glyph whose `d1`
/// has the wrong operand count.
#[test]
fn type3_fallback_glyphs_declare_a_well_formed_d1() {
    if without_craft_fonts("type3_fallback_glyphs_declare_a_well_formed_d1") {
        return;
    }
    // Cyrillic is respaced (shifted by its side bearing), and its box must move with it.
    let mut glyphs = Vec::new();
    for replacement in ["見本商会 御中", "София 2027"] {
        let mut doc = text_page("BT /F2 12 Tf 72 700 Td (ab) Tj ET");
        text::replace_line(&mut doc, 0, 0, replacement).unwrap();
        glyphs.extend(fallback_paths(&reopen(&doc)));
    }
    assert!(!glyphs.is_empty());
    for glyph in glyphs {
        let text = String::from_utf8(glyph).unwrap();
        let mut lines = text.lines();
        let d1: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
        assert_eq!(d1.len(), 7, "six operands and d1: {d1:?}");
        assert_eq!(d1[6], "d1");
        let n: Vec<f64> = d1[..6].iter().map(|v| v.parse().unwrap()).collect();
        assert_eq!(n[1], 0.0, "wy");
        assert!(n[2] <= n[4] && n[3] <= n[5], "box: {n:?}");
        for line in lines.filter(|l| l.ends_with(" m") || l.ends_with(" l")) {
            let p: Vec<f64> = line.split_whitespace().take(2).map(|v| v.parse().unwrap()).collect();
            assert!(p[0] >= n[2] && p[0] <= n[4] && p[1] >= n[3] && p[1] <= n[5], "{line} outside {n:?}");
        }
    }
}

/// Shippori Mincho, the serif fallback, has no Cyrillic: Bulgarian replacement text in a serif
/// line takes a fallback face that has every letter instead of failing on the first one.
#[test]
fn cyrillic_replacement_in_a_serif_line_uses_a_face_with_the_letters() {
    if without_craft_fonts("cyrillic_replacement_in_a_serif_line_uses_a_face_with_the_letters") {
        return;
    }
    let replacement = "София 2027";
    for paragraph in [false, true] {
        let mut doc = styled_text_page("ABCDEF+Garamond");
        if paragraph {
            text::replace_block(&mut doc, 0, 0, replacement).unwrap();
        } else {
            text::replace_line(&mut doc, 0, 0, replacement).unwrap();
        }
        let lines = text::text_lines(&reopen(&doc), 0).unwrap();
        assert_eq!(lines[0].text, replacement);
        // Letters are spaced by their shape, not one em each (the faces' full-width Cyrillic).
        let widths = fallback_widths(&doc);
        assert!(widths.iter().all(|w| *w > 300.0 && *w < 900.0), "{widths:?}");
        assert!(widths.windows(2).any(|w| w[0] != w[1]), "proportional: {widths:?}");
    }
}

/// The `/Widths` of the page's Type 3 fallback font, for its Cyrillic letters.
fn fallback_widths(doc: &Document) -> Vec<f64> {
    let p = pdfcraft_model::pages(doc).swap_remove(0);
    let res = doc.resolve(p.dict.get(b"Resources").unwrap());
    let fonts = doc.resolve(res.as_dict().unwrap().get(b"Font").unwrap());
    let font = doc.resolve(fonts.as_dict().unwrap().get(b"PCJp").unwrap());
    let font = font.as_dict().unwrap();
    let widths = doc.resolve(font.get(b"Widths").unwrap()).as_array().unwrap().iter().filter_map(|w| w.as_f64()).collect::<Vec<_>>();
    // "София 2027": the first five codes are the letters.
    widths[..5].to_vec()
}

#[test]
fn japanese_paragraph_keeps_ideographic_spaces() {
    if without_craft_fonts("japanese_paragraph_keeps_ideographic_spaces") {
        return;
    }
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Original paragraph) Tj ET");
    let replacement = "　見本商会　　御中　";
    let style = text::BlockStyle { width: Some(400.0), ..Default::default() };
    text::rewrite_block(&mut doc, 0, 0, Some(replacement), &style).unwrap();
    let reopened = reopen(&doc);
    let lines = text::text_lines(&reopened, 0).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, replacement);
    assert_eq!(text::text_blocks(&reopened, 0).unwrap()[0].text, replacement);
    // Restyling the saved paragraph without new text must also preserve its spacing.
    let mut doc = reopened;
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { size: Some(14.0), ..style }).unwrap();
    assert_eq!(text::text_lines(&reopen(&doc), 0).unwrap()[0].text, replacement);
}

/// Without craft-fonts there is no Japanese face: editing in Japanese is a clear error that
/// leaves the page untouched, never a panic; Latin edits work as before.
#[test]
fn japanese_edit_without_craft_fonts_is_a_clear_error() {
    let mut doc = text_page("BT /F2 12 Tf 72 700 Td (ab) Tj ET");
    let before = page_content_bytes(&doc, 0);
    let line = text::replace_line(&mut doc, 0, 0, "日本語の文字");
    let block = text::replace_block(&mut doc, 0, 0, "日本語の文字");
    if pdfcraft_fonts::document_japanese_font().is_some() {
        eprintln!("built with craft-fonts: the Japanese edits succeed (checked by the tests above)");
        assert!(line.is_ok() && block.is_ok());
        return;
    }
    for result in [line.map(|_| ()), block.map(|_| ())] {
        let Err(EditError::Invalid(msg)) = result else { panic!("expected a clear error, got {result:?}") };
        assert!(msg.contains("Japanese fallback font") && msg.contains("CRAFT_FONTS_DIR"), "{msg}");
    }
    assert_eq!(page_content_bytes(&doc, 0), before);
    let latin = text::replace_line(&mut doc, 0, 0, "Hello").unwrap();
    assert_eq!(text::text_lines(&reopen(&doc), 0).unwrap()[0].text, "Hello", "{latin:?}");
}
#[test]
fn font_descriptor_style_is_exposed_even_with_a_neutral_name() {
    let doc = descriptor_text_page();
    let lines = text::text_lines(&doc, 0).unwrap();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].bold && lines[0].italic, "{:?}", lines[0]);
    let blocks = text::text_blocks(&doc, 0).unwrap();
    assert!(blocks[0].bold && blocks[0].italic, "{:?}", blocks[0]);
}

#[test]
fn mixed_font_runs_are_not_merged_into_one_source_style() {
    let doc = text_page("BT /F1 12 Tf 72 700 Td (Regular) Tj /F2 12 Tf 150 700 Td (Subset) Tj ET");
    let lines = text::text_lines(&doc, 0).unwrap();
    assert_eq!(lines.len(), 2);
    assert_ne!(lines[0].font, lines[1].font);
}

#[test]
fn unsupported_text_graphics_are_refused_without_touching_supported_neighbours() {
    for graphics in ["1 Tr 1 0 0 RG 2 w", "2 Tr", "3 Tr", "4 Tr", "5 Tr", "6 Tr", "7 Tr", "/Opacity gs", "0 0 600 800 re W n"] {
        let source = format!("q BT /F1 12 Tf {graphics} 72 700 Td (Special) Tj ET Q BT /F1 12 Tf 72 600 Td (Normal) Tj ET");
        let mut doc = text_page(&source);
        if graphics.contains("gs") {
            let mut opacity = pdfcraft_cos::Dict::new();
            opacity.set(b"ca".to_vec(), Object::Real(0.4));
            let mut states = pdfcraft_cos::Dict::new();
            states.set(b"Opacity".to_vec(), Object::Dict(opacity));
            doc.update_dict(pdfcraft_cos::ObjRef::new(3, 0), |page| {
                let mut resources = page.get(b"Resources").unwrap().as_dict().unwrap().clone();
                resources.set(b"ExtGState".to_vec(), Object::Dict(states));
                page.set(b"Resources".to_vec(), Object::Dict(resources));
            })
            .unwrap();
        }
        let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
        for range in [None, Some([0, 1])] {
            let style = text::BlockStyle { range, size: Some(18.0), ..Default::default() };
            let error = text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap_err();
            assert!(error.to_string().contains("appearance is preserved unchanged"), "{graphics}: {error}");
            assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before, "{graphics}");
        }
        text::rewrite_block(&mut doc, 0, 1, None, &text::BlockStyle { range: Some([0, 1]), color: Some([0.0, 0.0, 1.0]), ..Default::default() })
            .unwrap();
        let reopened = reopen(&doc);
        let content = String::from_utf8_lossy(&page_content_bytes(&reopened, 0)).into_owned();
        assert!(content.contains(&format!("{graphics} 72 700 Td (Special) Tj ET Q")), "{content}");
        let blocks = text::text_blocks(&reopened, 0).unwrap();
        assert_eq!(blocks[0].text, "Special");
        assert_eq!(blocks[1].text, "Normal");
        assert_eq!(blocks[1].runs[0].color, [0.0, 0.0, 1.0]);
    }
}

#[test]
fn text_clip_outlives_the_text_object_until_graphics_restore() {
    let mut doc = text_page("BT /F1 12 Tf 7 Tr 72 700 Td (Clip) Tj ET 0 Tr BT /F1 12 Tf 72 600 Td (Clipped) Tj ET");
    let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
    assert!(text::rewrite_block(&mut doc, 0, 1, None, &text::BlockStyle { range: Some([0, 1]), size: Some(20.0), ..Default::default() }).is_err());
    assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before);
}

#[test]
fn missing_glyphs_preserve_source_style_in_fallback() {
    let mut doc = descriptor_text_page();
    let result = text::replace_line(&mut doc, 0, 0, "Styled €").unwrap();
    assert_eq!(result.substituted.as_deref(), Some("Helvetica-BoldOblique"));
    let bytes = page_content_bytes(&doc, 0);
    let content = String::from_utf8_lossy(&bytes);
    assert!(content.contains("/PCEdHelveticaBoldOblique 12 Tf"), "{content}");
}

#[test]
fn text_lines_are_found_and_replaced_in_place() {
    let mut doc =
        text_page("BT /F1 12 Tf 72 700 Td (Hello) Tj 40 0 Td [(wor) -20 (ld)] TJ 0 -20 Td (Second line) Tj ET BT /F2 10 Tf 72 600 Td (ab) Tj ET");
    let lines = text::text_lines(&doc, 0).unwrap();
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["Hello world", "Second line", "ab"]);
    assert!((lines[0].rect[0] - 72.0).abs() < 0.01 && lines[0].rect[1] < 700.0 && lines[0].rect[3] > 700.0, "{:?}", lines[0].rect);
    assert!((lines[0].size - 12.0).abs() < 1e-9 && lines[0].base_font == "Helvetica");
    let second = lines[1].rect;
    let r = text::replace_line(&mut doc, 0, 0, "Goodbye, café").unwrap();
    assert_eq!(r.substituted, None);
    let doc = reopen(&doc);
    let lines = text::text_lines(&doc, 0).unwrap();
    assert_eq!(lines[0].text, "Goodbye, café");
    assert_eq!(lines[1].text, "Second line");
    assert_eq!(lines[1].rect, second);
}

/// A page whose content is one stream in three pieces, split between tokens: a marked-content
/// dictionary closes at the start of the middle piece, and the `TJ` array that ends it is shown
/// by the operator that starts the last one.
fn split_streams_page() -> Document {
    let piece = |s: &str| format!("<< /Length {} >>\nstream\n{s}\nendstream", s.len());
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents [4 0 R 5 0 R 6 0 R] /Resources << /Font << /F1 7 0 R >> >> >>".into(),
        piece("/P << /MCID 0"),
        piece(">> BDC BT /F1 12 Tf 72 700 Td (Target) Tj 0 -20 Td [(After) -20 (wards)]"),
        piece("TJ ET EMC"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
    ];
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

/// The page's pieces joined back into one stream: every operator must still have its operands.
fn assert_split_tokens_kept(doc: &Document, new_text: &str) {
    let joined = streams(doc, 0).join("\n");
    assert!(joined.contains(new_text) && !joined.contains("Target"), "{joined}");
    assert!(joined.contains("[(After) -20 (wards)]"), "the text after the line is gone: {joined}");
    for op in pdfcraft_content::parse(joined.as_bytes()).ops {
        let want = match op.op.as_slice() {
            b"BDC" => 2,
            b"TJ" => 1,
            _ => continue,
        };
        assert_eq!(op.operands.len(), want, "{} lost its operands: {joined}", String::from_utf8_lossy(&op.op));
    }
}

#[test]
fn editing_text_keeps_the_tokens_a_stream_shares_with_its_neighbours() {
    let mut doc = split_streams_page();
    assert_eq!(text::text_lines(&doc, 0).unwrap()[0].text, "Target");
    text::replace_line(&mut doc, 0, 0, "Edited").unwrap();
    assert_split_tokens_kept(&reopen(&doc), "Edited");
    // A paragraph rewrite: "Target" and the line after it (split across pieces) are one
    // paragraph, rewritten whole without stray tokens.
    let mut doc = split_streams_page();
    assert_eq!(text::text_blocks(&doc, 0).unwrap().len(), 1);
    text::replace_block(&mut doc, 0, 0, "Rewrapped").unwrap();
    let (joined, ops) = joined_ops(&reopen(&doc));
    assert!(joined.contains("Rewrapped") && !joined.contains("Target") && !joined.contains("wards"), "{joined}");
    assert!(ops.iter().find(|o| o.is("BDC")).is_some_and(|o| o.operands.len() == 2), "{joined}");
}

/// The operators of the page's pieces joined back into one stream, each checked for operands.
fn joined_ops(doc: &Document) -> (String, Vec<pdfcraft_content::Op>) {
    let joined = streams(doc, 0).join("\n");
    let ops = pdfcraft_content::parse(joined.as_bytes());
    assert_eq!(ops.skipped, 0, "stray tokens: {joined}");
    (joined, ops.ops)
}

#[test]
fn a_line_whose_operator_starts_the_next_stream_can_be_edited() {
    // #155: `[(After) -20 (wards)]` ends one piece and its `TJ` starts the next.
    let doc = split_streams_page();
    let lines = text::text_lines(&doc, 0).unwrap();
    assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["Target", "Afterwards"]);
    assert!(lines[1].rect[1] < lines[0].rect[1] && lines[1].rect[2] > lines[1].rect[0] + 40.0, "{:?}", lines[1].rect);

    // Retyping it replaces the operator whole, in both pieces it spans.
    let mut doc = split_streams_page();
    text::replace_line(&mut doc, 0, 1, "Later").unwrap();
    let doc = reopen(&doc);
    let (joined, ops) = joined_ops(&doc);
    assert!(!joined.contains("After") && !joined.contains("wards") && joined.contains("Target"), "{joined}");
    assert!(ops.iter().filter(|o| o.is("TJ")).count() == 0, "{joined}");
    assert!(ops.iter().find(|o| o.is("BDC")).is_some_and(|o| o.operands.len() == 2), "{joined}");
    let lines = text::text_lines(&doc, 0).unwrap();
    assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["Target", "Later"]);
    // The pieces stay pieces: the first is untouched, the marked content still closes.
    let pieces = streams(&doc, 0);
    assert_eq!(pieces.len(), 3);
    assert_eq!(pieces[0].trim(), "/P << /MCID 0");
    assert!(pieces[2].contains("EMC") && !pieces[2].contains("TJ"), "{pieces:?}");

    // As a paragraph with the line before it: both lines are rewritten.
    let mut doc = split_streams_page();
    let blocks = text::text_blocks(&doc, 0).unwrap();
    let last = blocks.iter().position(|b| b.text.contains("Afterwards")).unwrap();
    text::replace_block(&mut doc, 0, last, "Rewrapped").unwrap();
    let doc = reopen(&doc);
    let (joined, ops) = joined_ops(&doc);
    assert!(!joined.contains("wards") && joined.contains("Rewrapped"), "{joined}");
    assert!(ops.iter().find(|o| o.is("BDC")).is_some_and(|o| o.operands.len() == 2), "{joined}");
    assert!(text::text_lines(&doc, 0).unwrap().iter().any(|l| l.text == "Rewrapped"));
}

#[test]
fn win_ansi_high_glyphs_use_helvetica_not_the_fallback_font() {
    // š ž Š Ž Œ Ÿ ƒ † ‰ (WinAnsi 0x80–0x9F) are standard-font glyphs (#347): no Type3 fallback.
    let text = "Šumava žaba œuvre Ÿ ƒ † ‰ €";
    let mut doc = text_page("BT /F2 10 Tf 72 600 Td (ab) Tj ET");
    let r = text::replace_line(&mut doc, 0, 0, text).unwrap();
    assert_eq!(r.substituted.as_deref(), Some("Helvetica"));
    let lines = text::text_lines(&reopen(&doc), 0).unwrap();
    assert_eq!((lines[0].text.as_str(), lines[0].base_font.as_str()), (text, "Helvetica"));
    let mut doc = text_page("BT /F2 10 Tf 72 600 Td (ab) Tj ET");
    let r = text::replace_block(&mut doc, 0, 0, text).unwrap();
    assert!(r.substituted.as_deref().is_none_or(|s| !s.contains("Type3")), "{:?}", r.substituted);
    let lines = text::text_lines(&reopen(&doc), 0).unwrap();
    assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" "), text);
}

#[test]
fn missing_glyphs_substitute_helvetica_and_impossible_text_is_refused() {
    let mut doc = text_page("BT /F2 10 Tf 72 600 Td (ab) Tj ET");
    // "c" has no glyph in the subset: Helvetica takes over for this line.
    let r = text::replace_line(&mut doc, 0, 0, "abc").unwrap();
    assert_eq!(r.substituted.as_deref(), Some("Helvetica"));
    let doc2 = reopen(&doc);
    let lines = text::text_lines(&doc2, 0).unwrap();
    assert_eq!((lines[0].text.as_str(), lines[0].base_font.as_str()), ("abc", "Helvetica"));
    // Neither the standard font nor the craft-fonts Japanese faces have this emoji.
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
    assert!(text::replace_line(&mut doc, 0, 0, "\u{1f4a9}").is_err());
    assert!(text::replace_line(&mut doc, 0, 5, "x").is_err(), "no such line");
}

#[test]
fn paragraphs_are_found_and_rewrapped() {
    let mut doc = text_page(
        "BT 0 0 1 rg /F1 10 Tf 12 TL 72 700 Td (The quick brown fox) Tj T* (jumps over the) Tj T* (lazy dog.) Tj ET \
         BT /F1 10 Tf 72 600 Td (Next paragraph) Tj ET",
    );
    let blocks = text::text_blocks(&doc, 0).unwrap();
    let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(texts, ["The quick brown fox jumps over the lazy dog.", "Next paragraph"]);
    assert_eq!(blocks[0].lines, [0, 1, 2]);
    let width = blocks[0].rect[2] - blocks[0].rect[0];
    let next = blocks[1].rect;
    let long = "PdfCraft rewraps a paragraph to its own width when its text changes, keeping the font, size, colour and line spacing.";
    assert_eq!(text::replace_block(&mut doc, 0, 0, long).unwrap().substituted, None);
    let doc = reopen(&doc);
    let lines = text::text_lines(&doc, 0).unwrap();
    let blocks = text::text_blocks(&doc, 0).unwrap();
    assert_eq!(blocks[0].text, long);
    assert!(blocks[0].lines.len() > 3, "more lines: {}", blocks[0].lines.len());
    for i in &blocks[0].lines {
        assert!(lines[*i].rect[2] - lines[*i].rect[0] <= width + 1.0, "line {} fits", lines[*i].text);
    }
    // Same left edge, same spacing (12 pt).
    assert!((lines[0].rect[0] - 72.0).abs() < 0.01);
    let spacing = lines[blocks[0].lines[0]].origin_baseline() - lines[blocks[0].lines[1]].origin_baseline();
    assert!((spacing - 12.0).abs() < 0.01, "{spacing}");
    // The next paragraph is untouched, and the new text is blue like the old.
    assert_eq!(blocks[1].text, "Next paragraph");
    assert_eq!(blocks[1].rect, next);
    let content = String::from_utf8_lossy(&page_content_bytes(&doc, 0)).into_owned();
    assert!(content.contains("0 0 1 rg"), "{content}");
    // Shorter text: fewer lines.
    let mut doc = doc;
    text::replace_block(&mut doc, 0, 0, "Short.").unwrap();
    let blocks = text::text_blocks(&doc, 0).unwrap();
    assert_eq!((blocks[0].text.as_str(), blocks[0].lines.len()), ("Short.", 1));
}

fn page_content_bytes(doc: &Document, page: usize) -> Vec<u8> {
    let p = pdfcraft_model::pages(doc).swap_remove(page);
    let c = p.dict.get(b"Contents").unwrap();
    match &*doc.resolve(c) {
        pdfcraft_cos::Object::Stream(s) => s.decoded().unwrap(),
        pdfcraft_cos::Object::Array(a) => a
            .iter()
            .flat_map(|x| match &*doc.resolve(x) {
                pdfcraft_cos::Object::Stream(s) => s.decoded().unwrap(),
                _ => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A page drawing image XObject /Im0 at 100,100 size 200 × 100 (pixels 4 × 2).
fn image_page() -> Document {
    let content = "q 200 0 0 100 100 100 cm /Im0 Do Q BT /F1 12 Tf 72 700 Td (Caption) Tj ET";
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /Im0 6 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        "<< /Type /XObject /Subtype /Image /Width 4 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 8 >>\nstream\n\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\nendstream".into(),
    ];
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

fn close(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
}

#[test]
fn page_images_move_turn_replace_and_delete() {
    let mut doc = image_page();
    let imgs = images::page_images(&doc, 0).unwrap();
    assert_eq!(imgs.len(), 1);
    assert!(close(imgs[0].rect, [100.0, 100.0, 300.0, 200.0]), "{:?}", imgs[0].rect);
    assert_eq!((imgs[0].width, imgs[0].height, imgs[0].name.as_str()), (4, 2, "Im0"));
    // Move and resize.
    let t = images::rect_to_rect(imgs[0].rect, [50.0, 400.0, 150.0, 450.0]);
    images::change_image(&mut doc, 0, 0, &images::ImageChange::Transform(t)).unwrap();
    let doc = reopen(&doc);
    let r = images::page_images(&doc, 0).unwrap()[0].rect;
    assert!(close(r, [50.0, 400.0, 150.0, 450.0]), "{r:?}");
    // A quarter turn about the centre: 100 × 50 becomes 50 × 100 around (100, 425).
    let mut doc = doc;
    let t = images::turn_about_centre(r, 1, false, false);
    images::change_image(&mut doc, 0, 0, &images::ImageChange::Transform(t)).unwrap();
    let r = images::page_images(&doc, 0).unwrap()[0].rect;
    assert!(close(r, [75.0, 375.0, 125.0, 475.0]), "{r:?}");
    // The text after it is untouched.
    assert_eq!(text::text_lines(&doc, 0).unwrap()[0].text, "Caption");
    // Replace with another image object, in the same place.
    let mut d = pdfcraft_cos::Dict::new();
    for (k, v) in [
        (&b"Type"[..], pdfcraft_cos::Object::name("XObject")),
        (b"Subtype", pdfcraft_cos::Object::name("Image")),
        (b"Width", pdfcraft_cos::Object::Int(1)),
        (b"Height", pdfcraft_cos::Object::Int(1)),
    ] {
        d.set(k.to_vec(), v);
    }
    let other = doc.add(pdfcraft_cos::Object::Stream(pdfcraft_cos::Stream::from_raw(d, vec![0])));
    images::change_image(&mut doc, 0, 0, &images::ImageChange::Replace(other)).unwrap();
    let imgs = images::page_images(&doc, 0).unwrap();
    assert_eq!((imgs[0].object, imgs[0].width), (Some(other), 1));
    assert!(close(imgs[0].rect, r));
    // Delete.
    images::change_image(&mut doc, 0, 0, &images::ImageChange::Delete).unwrap();
    assert!(images::page_images(&doc, 0).unwrap().is_empty());
    assert!(images::change_image(&mut doc, 0, 0, &images::ImageChange::Delete).is_err());
}

#[test]
fn cropping_a_page_image_hides_the_rest_and_moves_with_it() {
    let mut doc = image_page();
    images::change_image(&mut doc, 0, 0, &images::ImageChange::Crop([150.0, 120.0, 250.0, 500.0])).unwrap();
    let doc = reopen(&doc);
    let imgs = images::page_images(&doc, 0).unwrap();
    assert_eq!(imgs.len(), 1);
    // Clamped to the image; still the same 4 × 2 bitmap (Replace and Save Image As work).
    assert!(close(imgs[0].rect, [150.0, 120.0, 250.0, 200.0]), "{:?}", imgs[0].rect);
    assert!(!imgs[0].is_form);
    assert_eq!((imgs[0].object, imgs[0].width, imgs[0].height), (Some(pdfcraft_cos::ObjRef::new(6, 0)), 4, 2));
    assert_eq!(text::text_lines(&doc, 0).unwrap()[0].text, "Caption");
    // Moving it moves the crop too.
    let mut doc = doc;
    let t = images::rect_to_rect(imgs[0].rect, [0.0, 0.0, 50.0, 40.0]);
    images::change_image(&mut doc, 0, 0, &images::ImageChange::Transform(t)).unwrap();
    let r = images::page_images(&doc, 0).unwrap()[0].rect;
    assert!(close(r, [0.0, 0.0, 50.0, 40.0]), "{r:?}");
    // Cropping again narrows the same wrapper rather than nesting another one.
    images::change_image(&mut doc, 0, 0, &images::ImageChange::Crop([10.0, 0.0, 40.0, 40.0])).unwrap();
    let doc = reopen(&doc);
    let imgs = images::page_images(&doc, 0).unwrap();
    assert!(close(imgs[0].rect, [10.0, 0.0, 40.0, 40.0]), "{:?}", imgs[0].rect);
    let page = pdfcraft_model::pages(&doc)[0].clone();
    let res = doc.resolve(page.dict.get(b"Resources").unwrap()).as_dict().cloned().unwrap();
    let xo = doc.resolve(res.get(b"XObject").unwrap()).as_dict().cloned().unwrap();
    let wrapper = doc.resolve(xo.get(imgs[0].name.as_bytes()).unwrap());
    let Object::Stream(w) = &*wrapper else { panic!("a wrapper stream") };
    let inner = doc.resolve(w.dict.get(b"Resources").unwrap()).as_dict().cloned().unwrap();
    let inner = doc.resolve(inner.get(b"XObject").unwrap()).as_dict().cloned().unwrap();
    assert_eq!(inner.get(b"Im0").and_then(|o| o.as_ref()), Some(pdfcraft_cos::ObjRef::new(6, 0)), "draws the bitmap directly");
    // Replacing a cropped image keeps the crop.
    let mut doc = doc;
    let mut d = Dict::new();
    for (k, v) in [(&b"Subtype"[..], Object::name("Image")), (b"Width", Object::Int(1)), (b"Height", Object::Int(1))] {
        d.set(k.to_vec(), v);
    }
    let other = doc.add(Object::Stream(Stream::from_raw(d, vec![0])));
    images::change_image(&mut doc, 0, 0, &images::ImageChange::Replace(other)).unwrap();
    let imgs = images::page_images(&doc, 0).unwrap();
    assert_eq!((imgs[0].object, imgs[0].width), (Some(other), 1));
    assert!(close(imgs[0].rect, [10.0, 0.0, 40.0, 40.0]), "{:?}", imgs[0].rect);
    // A crop that keeps nothing is refused and changes nothing.
    assert!(images::change_image(&mut doc, 0, 0, &images::ImageChange::Crop([300.0, 300.0, 400.0, 400.0])).is_err());
    assert!(images::change_image(&mut doc, 0, 0, &images::ImageChange::Crop([f64::NAN, 0.0, 1.0, 1.0])).is_err());
    assert_eq!(images::page_images(&doc, 0).unwrap().len(), 1);
}

#[test]
fn form_artwork_edits_only_the_selected_placement() {
    let mut doc = fixture(); // three pages share resources; pages 0 and 1 also share contents
    let mut form = Dict::new();
    form.set(b"Subtype".to_vec(), Object::name("Form"));
    form.set(b"BBox".to_vec(), Object::Array([10, 20, 110, 70].map(Object::Int).to_vec()));
    form.set(b"Matrix".to_vec(), Object::Array([2, 0, 0, 3, -20, -60].map(Object::Int).to_vec()));
    let artwork = doc.add(Object::Stream(Stream::flate(form, b"1 0 0 rg 10 20 100 50 re f")));
    doc.update_dict(pdfcraft_cos::ObjRef::new(6, 0), |d| {
        let mut xo = Dict::new();
        xo.set(b"Figure".to_vec(), Object::Ref(artwork));
        d.set(b"XObject".to_vec(), Object::Dict(xo));
    })
    .unwrap();
    let content = b"q 1 0 0 1 40 80 cm /Figure Do Q q 1 0 0 1 300 400 cm /Figure Do Q BT /F1 9 Tf (Caption) Tj ET";
    doc.set(pdfcraft_cos::ObjRef::new(7, 0), Object::Stream(Stream::flate(Dict::new(), content)));
    let mut doc = reopen(&doc);
    let original = doc.get(artwork);
    let imgs = images::page_images(&doc, 0).unwrap();
    assert_eq!(imgs.len(), 2);
    assert!(imgs[0].is_form);
    assert_eq!((imgs[0].width, imgs[0].height), (0, 0));
    assert!(close(imgs[0].rect, [40.0, 80.0, 240.0, 230.0]));
    let target = [60.0, 100.0, 160.0, 175.0];
    images::change_image(&mut doc, 0, 0, &images::ImageChange::Transform(images::rect_to_rect(imgs[0].rect, target))).unwrap();
    let mut doc = reopen(&doc);
    let moved = images::page_images(&doc, 0).unwrap();
    assert!(close(moved[0].rect, target));
    assert_eq!(moved[1].rect, imgs[1].rect);
    assert_eq!(images::page_images(&doc, 1).unwrap()[0].rect, imgs[0].rect);
    assert_eq!(*doc.get(artwork), *original, "the shared Form and its unknown data are untouched");
    assert_eq!(text::text_lines(&doc, 0).unwrap()[0].text, "Caption");
    let t = images::turn_about_centre(target, 1, false, false);
    images::change_image(&mut doc, 0, 0, &images::ImageChange::Transform(t)).unwrap();
    assert!(close(images::page_images(&doc, 0).unwrap()[0].rect, [72.5, 87.5, 147.5, 187.5]));
    assert!(images::change_image(&mut doc, 0, 0, &images::ImageChange::Replace(artwork)).is_err());
    images::change_image(&mut doc, 0, 0, &images::ImageChange::Delete).unwrap();
    let doc = reopen(&doc);
    assert_eq!(images::page_images(&doc, 0).unwrap().len(), 1);
    assert_eq!(images::page_images(&doc, 1).unwrap().len(), 2);
    // Invalid bounds are skipped without interpreting the Form stream.
    let mut doc = doc;
    let mut broken = (*doc.get(artwork)).clone();
    if let Object::Stream(s) = &mut broken {
        s.dict.set(b"BBox".to_vec(), Object::Array(vec![Object::Int(0)]));
    }
    doc.set(artwork, broken);
    assert!(images::page_images(&doc, 0).unwrap().is_empty());
}

#[test]
fn paragraphs_take_new_formatting() {
    let mut doc = text_page("BT /F1 10 Tf 12 TL 100 700 Td (One two three four five six seven) Tj T* (eight nine ten eleven twelve) Tj ET");
    let before = text::text_blocks(&doc, 0).unwrap()[0].clone();
    let style = text::BlockStyle {
        family: Some((added::Family::Times, true, false)),
        size: Some(12.0),
        color: Some([1.0, 0.0, 0.0]),
        align: Some(added::Align::Center),
        ..Default::default()
    };
    text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap();
    let doc = reopen(&doc);
    let blocks = text::text_blocks(&doc, 0).unwrap();
    let lines = text::text_lines(&doc, 0).unwrap();
    assert_eq!(blocks[0].text, before.text, "same words");
    assert_eq!((blocks[0].base_font.as_str(), blocks[0].size), ("Times-Bold", 12.0));
    // Centred in the paragraph's width.
    let mid = (before.rect[0] + before.rect[2]) / 2.0;
    for i in &blocks[0].lines {
        let r = lines[*i].rect;
        assert!(((r[0] + r[2]) / 2.0 - mid).abs() < 2.0, "line {:?} centred on {mid}", lines[*i].text);
    }
    assert!(String::from_utf8_lossy(&page_content_bytes(&doc, 0)).contains("1 0 0 rg"));
    // Right alignment keeps lines flush with the right edge.
    let mut doc = text_page("BT /F1 10 Tf 12 TL 100 700 Td (One two three four five six seven) Tj T* (eight nine ten eleven twelve) Tj ET");
    let right = text::text_blocks(&doc, 0).unwrap()[0].rect[2];
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { align: Some(added::Align::Right), ..Default::default() }).unwrap();
    let lines = text::text_lines(&doc, 0).unwrap();
    for l in &lines {
        assert!((l.rect[2] - right).abs() < 1.0, "{:?} ends at {} not {right}", l.text, l.rect[2]);
    }
}

#[test]
fn existing_page_fonts_are_selected_exactly_and_missing_glyphs_are_refused() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (abba) Tj ET");
    let fonts = text::page_fonts(&doc, 0, "abba").unwrap();
    assert!(fonts.iter().find(|f| f.resource == "F2").is_some_and(|f| f.subset && f.encodable));
    let source = doc.get(pdfcraft_cos::ObjRef::new(6, 0));
    let style = text::BlockStyle { font_resource: Some("F2".into()), ..Default::default() };
    assert_eq!(text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap().substituted, None);
    let mut doc = reopen(&doc);
    let line = &text::text_lines(&doc, 0).unwrap()[0];
    assert_eq!((line.text.as_str(), line.font.as_str(), line.base_font.as_str()), ("abba", "F2", "ABCDEF+Arial"));
    assert!((line.rect[2] - line.rect[0] - 24.48).abs() < 0.001, "the selected subset's own widths");
    assert_eq!(*doc.get(pdfcraft_cos::ObjRef::new(6, 0)), *source, "no subset expansion or resource mutation");
    let before = page_content_bytes(&doc, 0);
    for bad in ["abc", "a😀"] {
        assert!(text::rewrite_block(&mut doc, 0, 0, Some(bad), &style).is_err());
        assert_eq!(page_content_bytes(&doc, 0), before);
    }
    assert!(!text::page_fonts(&doc, 0, "abc").unwrap().iter().find(|f| f.resource == "F2").unwrap().encodable);
    assert!(text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { bold: Some(true), ..style.clone() }).is_err());
    assert!(text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { font_resource: Some("Missing".into()), ..Default::default() }).is_err());
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { size: Some(18.0), color: Some([0.0, 0.0, 1.0]), ..Default::default() }).unwrap();
    let line = &text::text_lines(&reopen(&doc), 0).unwrap()[0];
    assert_eq!((line.base_font.as_str(), line.size, line.color), ("ABCDEF+Arial", 18.0, [0.0, 0.0, 1.0]));
}

#[test]
fn mixed_paragraph_selection_keeps_surrounding_faces_and_reopens() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Before ) Tj /F2 12 Tf (abba) Tj /F1 12 Tf ( after.) Tj ET");
    let blocks = text::text_blocks(&doc, 0).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "Before abba after.");
    assert_eq!(blocks[0].runs.iter().map(|r| (r.start, r.end, r.font.as_str())).collect::<Vec<_>>(), [(0, 7, "F1"), (7, 11, "F2"), (11, 18, "F1")]);
    text::rewrite_block(
        &mut doc,
        0,
        0,
        None,
        &text::BlockStyle { range: Some([7, 11]), italic: Some(true), color: Some([1.0, 0.0, 0.0]), ..Default::default() },
    )
    .unwrap();
    let mut doc = reopen(&doc);
    let b = &text::text_blocks(&doc, 0).unwrap()[0];
    assert_eq!(b.text, "Before abba after.");
    assert_eq!(b.runs.len(), 3);
    assert_eq!((b.runs[0].font.as_str(), b.runs[2].font.as_str()), ("F1", "F1"));
    assert!(b.runs[1].italic);
    assert_eq!(b.runs[1].color, [1.0, 0.0, 0.0]);
    text::rewrite_block(&mut doc, 0, 0, Some("Before edited after."), &text::BlockStyle { width: Some(70.0), ..Default::default() }).unwrap();
    let b = &text::text_blocks(&reopen(&doc), 0).unwrap()[0];
    assert_eq!(b.text, "Before edited after.");
    assert!(b.line_count > 1);
    assert!(b.runs.iter().filter(|r| r.start >= 7 && r.end <= 14).all(|r| r.italic && r.color == [1.0, 0.0, 0.0]));
    assert_eq!(b.runs.last().unwrap().font, "F1");
}

#[test]
fn range_colour_keeps_kerning_spacing_and_nonuniform_geometry() {
    for matrix in ["1 0 0 1 72 700 Tm", "2 0 0 1 72 700 Tm", "0.5 0 0 1 72 700 Tm"] {
        let mut doc = text_page(&format!("BT /F1 12 Tf {matrix} [(A) 120 (V  )] TJ 2 Tc (B) Tj 120 Tz (C) Tj ET"));
        let before = text::text_blocks(&doc, 0).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].text, "AV  BC");
        assert_eq!(before[0].runs.len(), 3, "Tc/Tz changes retain distinct runs");
        let rect = before[0].rect;
        text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([0, 1]), color: Some([0.0, 0.0, 1.0]), ..Default::default() })
            .unwrap();
        let b = &text::text_blocks(&reopen(&doc), 0).unwrap()[0];
        assert_eq!(b.text, "AV  BC");
        assert_eq!(b.runs[0].color, [0.0, 0.0, 1.0]);
        assert!(b.runs.iter().skip(1).all(|r| r.color == [0.0; 3]));
        for (a, b) in rect.into_iter().zip(b.rect) {
            assert!((a - b).abs() < 0.001, "{rect:?} != {:?}", b);
        }
    }
}

#[test]
fn selected_colour_preserves_positioned_show_gaps_on_each_horizontal_scale() {
    for scale in [1.0, 2.0] {
        for middle_font in ["F1", "F2"] {
            let source = format!(
                "q {scale} 0 0 1 0 0 cm BT /F1 12 Tf 1 0 0 1 36 700 Tm (A) Tj /{middle_font} 12 Tf 1 0 0 1 52 700 Tm (a) Tj /F1 12 Tf 1 0 0 1 68 700 Tm (Z) Tj ET Q"
            );
            let mut doc = text_page(&source);
            let original = text::text_blocks(&doc, 0).unwrap().remove(0);
            assert_eq!(original.text, "A a Z");
            text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([0, 1]), color: Some([0.0, 0.0, 1.0]), ..Default::default() })
                .unwrap();
            let reopened = reopen(&doc);
            let edited = text::text_blocks(&reopened, 0).unwrap().remove(0);
            assert_eq!(edited.text, original.text);
            for (before, after) in original.rect.into_iter().zip(edited.rect) {
                assert!((before - after).abs() < 1e-6, "scale {scale}, font {middle_font}: {:?} -> {:?}", original.rect, edited.rect);
            }
            // Every emitted glyph gets its own placement. Read the actual Tm at a and Z,
            // independently of paragraph inference, rather than accepting the new bounding box alone.
            let (_, ops) = joined_ops(&reopened);
            let mut tm = [0.0; 6];
            let (mut a_seen, mut z_seen) = (0, 0);
            for op in ops {
                if op.is("Tm") {
                    tm = op.nums::<6>().unwrap();
                }
                if op.is("Tj")
                    && let Some(Object::String(text)) = op.operands.first()
                {
                    match text.bytes.as_slice() {
                        b"a" => {
                            a_seen += 1;
                            assert!((tm[4] - 52.0).abs() < 1e-6);
                        }
                        b"Z" => {
                            z_seen += 1;
                            assert!((tm[4] - 68.0).abs() < 1e-6);
                        }
                        _ => {}
                    }
                }
            }
            assert_eq!((a_seen, z_seen), (1, 1), "both positioned glyphs must be observed exactly once");
        }
    }
}

#[test]
fn selected_color_restores_default_black_after_q_saved_device_colors() {
    for red in ["1 0 0 rg", "/DeviceRGB cs 1 0 0 scn", "/DeviceCMYK cs 0 1 1 0 sc"] {
        let source = format!("BT /F1 12 Tf 72 700 Td (A) Tj ET q {red} BT /F1 12 Tf 80.004 700 Td (B) Tj ET Q BT /F1 12 Tf 88.008 700 Td (C) Tj ET");
        let mut doc = text_page(&source);
        let before = text::text_blocks(&doc, 0).unwrap().remove(0);
        assert_eq!(before.text, "ABC");
        assert_eq!(before.runs.iter().map(|run| run.color).collect::<Vec<_>>(), [[0.0; 3], [1.0, 0.0, 0.0], [0.0; 3]]);
        text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([0, 1]), color: Some([0.0, 0.0, 1.0]), ..Default::default() })
            .unwrap();
        let reopened = reopen(&doc);
        let after = text::text_blocks(&reopened, 0).unwrap().remove(0);
        assert_eq!(after.text, before.text);
        assert_eq!(after.runs.iter().map(|run| run.color).collect::<Vec<_>>(), [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0; 3]]);
        assert_eq!(after.rect, before.rect);
    }
}

#[test]
fn selected_color_preserves_implicitly_selected_device_spaces() {
    for (initial, next, expected) in
        [("0.2 g", "0.7 sc", [0.7; 3]), ("1 0 0 rg", "0 1 0 sc", [0.0, 1.0, 0.0]), ("0 1 1 0 k", "1 0 0 0 scn", [0.0, 1.0, 1.0])]
    {
        let source = format!(
            "BT /F1 12 Tf 72 700 Td (A) Tj ET q {initial} BT /F1 12 Tf 80.004 700 Td (B) Tj {next} 1 0 0 1 88.008 700 Tm (C) Tj ET Q BT /F1 12 Tf 96.672 700 Td (D) Tj ET"
        );
        let mut doc = text_page(&source);
        let before = text::text_blocks(&doc, 0).unwrap().remove(0);
        assert_eq!(before.text, "ABCD");
        assert_eq!(before.runs[2].color, expected);
        text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([1, 2]), color: Some([0.0, 0.0, 1.0]), ..Default::default() })
            .unwrap();
        let after = text::text_blocks(&reopen(&doc), 0).unwrap().remove(0);
        assert_eq!(after.text, before.text);
        assert_eq!(after.runs.iter().map(|run| run.color).collect::<Vec<_>>(), [[0.0; 3], [0.0, 0.0, 1.0], expected, [0.0; 3]]);
    }
}

#[test]
fn custom_color_spaces_are_not_assumed_to_be_already_black() {
    let mut doc = text_page("BT /F1 12 Tf /Spot cs 0.75 scn 72 700 Td (Custom) Tj ET");
    let mut tint = Dict::new();
    tint.set(b"FunctionType".to_vec(), Object::Int(2));
    tint.set(b"Domain".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(1)]));
    tint.set(b"C0".to_vec(), Object::Array(vec![Object::Int(1); 3]));
    tint.set(b"C1".to_vec(), Object::Array(vec![Object::Int(1), Object::Int(0), Object::Int(0)]));
    tint.set(b"N".to_vec(), Object::Int(1));
    let mut spaces = Dict::new();
    spaces.set(
        b"Spot".to_vec(),
        Object::Array(vec![Object::name("Separation"), Object::name("ContributorSpot"), Object::name("DeviceRGB"), Object::Dict(tint)]),
    );
    doc.update_dict(pdfcraft_cos::ObjRef::new(3, 0), |page| {
        let mut resources = page.get(b"Resources").unwrap().as_dict().unwrap().clone();
        resources.set(b"ColorSpace".to_vec(), Object::Dict(spaces));
        page.set(b"Resources".to_vec(), Object::Dict(resources));
    })
    .unwrap();
    let style = text::BlockStyle { range: Some([0, 1]), color: Some([0.0; 3]), ..Default::default() };
    assert!(!text::is_noop(&doc, 0, 0, "Custom", &style).unwrap());
    text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap();
    let (_, ops) = joined_ops(&doc);
    assert!(ops.iter().any(|op| op.is("rg") && op.nums::<3>() == Some([0.0; 3])));
    assert!(ops.iter().any(|op| op.is("cs") && op.name(0) == Some(b"Spot")));
}

#[test]
fn excessive_edit_text_and_source_style_segments_are_refused_atomically() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Small) Tj ET");
    let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
    let error = text::replace_block(&mut doc, 0, 0, &"x".repeat(1_000_001)).unwrap_err();
    assert!(error.to_string().contains("1,000,000 characters"));
    assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before);
    let mut content = String::from("BT /F1 12 Tf 72 700 Td ");
    for i in 0..8193 {
        content.push_str(&format!("{} Tc (a) Tj ", i % 2));
    }
    content.push_str("ET");
    let mut doc = text_page(&content);
    let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
    let error =
        text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([0, 1]), color: Some([0.0, 0.0, 1.0]), ..Default::default() })
            .unwrap_err();
    assert!(error.to_string().contains("8,192 style segments"), "{error}");
    assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before);
}

#[test]
fn run_preserving_edits_refuse_operator_expansion_before_emitting_it() {
    // Every underlined character is placed, marked and given its own path: 16 operators. Within
    // the 1,000,000-character ceiling that can exceed the 2,000,000-operator budget; the count is
    // checked before anything is emitted and the whole edit is refused, nothing is truncated and
    // the document is unchanged (whole paragraph and selected range alike).
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Small) Tj ET");
    let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
    let underlined = "ab ".repeat(45_000);
    let all = underlined.chars().count();
    for style in [
        text::BlockStyle { underline: Some(true), ..Default::default() },
        text::BlockStyle { range: Some([0, all]), underline: Some(true), ..Default::default() },
    ] {
        let error = text::rewrite_block(&mut doc, 0, 0, Some(&underlined), &style).unwrap_err();
        assert!(error.to_string().contains("more than 2000000 PDF operators"), "{error}");
        assert!(error.to_string().contains("smaller paragraphs"), "{error}");
        assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before);
        assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, "Small");
    }
    // The same underline at a size well inside the budget succeeds and reads back.
    let mut small = doc.clone();
    let text = "ab ".repeat(2_000);
    text::rewrite_block(&mut small, 0, 0, Some(&text), &text::BlockStyle { underline: Some(true), ..Default::default() }).unwrap();
    let small = reopen(&small);
    let blocks = text::text_blocks(&small, 0).unwrap();
    assert!(blocks.iter().flat_map(|b| &b.runs).all(|r| r.underline));
    assert_eq!(blocks.iter().map(|b| b.text.split_whitespace().count()).sum::<usize>(), 2_000);
    // Plain characters are shown together, one placement per row and face, so even the
    // character ceiling stays far inside the budget instead of expanding per glyph.
    let plain = "x".repeat(1_000_000);
    text::rewrite_block(&mut doc, 0, 0, Some(&plain), &text::BlockStyle { range: Some([0, 1]), color: Some([0.0, 0.0, 1.0]), ..Default::default() })
        .unwrap();
    let ops = pdfcraft_content::parse(&page_content_bytes(&doc, 0)).ops;
    assert!(ops.len() < 64, "{} operators", ops.len());
    assert_eq!(shows(&doc).iter().map(|(bytes, _)| bytes.len()).sum::<usize>(), 1_000_000);
}

#[test]
fn selected_subset_and_bad_ranges_fail_without_low_level_mutation() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (A abba Z) Tj ET");
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([2, 6]), font_resource: Some("F2".into()), ..Default::default() })
        .unwrap();
    let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
    for style in [
        text::BlockStyle { range: Some([0, 1]), font_resource: Some("F2".into()), ..Default::default() },
        text::BlockStyle { range: Some([9, 10]), bold: Some(true), ..Default::default() },
        text::BlockStyle { range: Some([2, 2]), bold: Some(true), ..Default::default() },
        text::BlockStyle { range: Some([0, 1]), size: Some(f64::NAN), ..Default::default() },
        // Selected underlines are supported now; an invalid range still refuses atomically.
        text::BlockStyle { range: Some([3, 99]), underline: Some(true), ..Default::default() },
    ] {
        assert!(text::rewrite_block(&mut doc, 0, 0, None, &style).is_err());
        assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before);
    }
    // Skewed and reflected text cannot be placed by the run path yet (rotated text can).
    for transform in ["1 0 0.3 1 100 200 Tm", "1 0 0 -1 100 600 Tm", "-1 0 0 1 300 200 Tm"] {
        let mut transformed = text_page(&format!("BT /F1 12 Tf {transform} (Turned) Tj ET"));
        let before = write_incremental(&transformed, &SaveOptions::default()).unwrap();
        let style = text::BlockStyle { range: Some([0, 1]), color: Some([1.0, 0.0, 0.0]), ..Default::default() };
        let refused = text::rewrite_block(&mut transformed, 0, 0, None, &style);
        assert!(matches!(&refused, Err(EditError::Invalid(m)) if m.contains("skewed or reflected")), "{transform}: {refused:?}");
        assert_eq!(write_incremental(&transformed, &SaveOptions::default()).unwrap(), before);
    }
}

#[test]
fn mixed_ranges_keep_unicode_glyph_codes_and_refuse_ligature_splits() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (AB) Tj ET");
    let cmap = doc.add(Object::Stream(Stream::flate(
        Dict::new(),
        b"1 begincodespacerange <00> <ff> endcodespacerange 2 beginbfchar <41> <00660069> <42> <0058> endbfchar",
    )));
    doc.update_dict(pdfcraft_cos::ObjRef::new(5, 0), |d| d.set(b"ToUnicode".to_vec(), Object::Ref(cmap))).unwrap();
    assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, "fiX");
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([0, 2]), color: Some([1.0, 0.0, 0.0]), ..Default::default() }).unwrap();
    let mut doc = reopen(&doc);
    assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, "fiX");
    let shown: Vec<Vec<u8>> = pdfcraft_content::parse(&page_content_bytes(&doc, 0))
        .ops
        .iter()
        .filter(|op| op.is("Tj"))
        .filter_map(|op| op.operands.first().and_then(Object::as_string).map(|s| s.bytes.clone()))
        .collect();
    assert_eq!(shown, [b"A".to_vec(), b"B".to_vec()], "source character codes preserved");
    let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
    assert!(
        text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([0, 1]), color: Some([0.0, 1.0, 0.0]), ..Default::default() })
            .is_err()
    );
    assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before);
}

#[test]
fn character_formatting_never_removes_an_overlapping_independent_text_object() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Target) Tj ET BT /F1 12 Tf 72 700 Td (Overlay) Tj ET");
    assert_eq!(text::text_blocks(&doc, 0).unwrap().len(), 2);
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([0, 1]), color: Some([1.0, 0.0, 0.0]), ..Default::default() }).unwrap();
    assert_eq!(text::text_blocks(&reopen(&doc), 0).unwrap().iter().map(|b| b.text.as_str()).collect::<Vec<_>>(), ["Target", "Overlay"]);
}

#[test]
fn formatting_uses_exact_advances_and_preserves_independent_face_styles() {
    let mut doc = text_page("BT /F1 12 Tf 100 700 Td (Wi Mi) Tj ET");
    let style = text::BlockStyle {
        family: Some((added::Family::Times, true, false)),
        width: Some(80.0),
        align: Some(added::Align::Right),
        ..Default::default()
    };
    text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap();
    let mut doc = reopen(&doc);
    let line = &text::text_lines(&doc, 0).unwrap()[0];
    // Times-Bold W/i/space/M/i: 1000 + 278 + 250 + 944 + 278 = 2750 units.
    assert!((line.rect[0] - 147.0).abs() < 0.001 && (line.rect[2] - 180.0).abs() < 0.001, "{:?}", line.rect);
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { italic: Some(true), ..Default::default() }).unwrap();
    assert_eq!(text::text_blocks(&reopen(&doc), 0).unwrap()[0].base_font, "Times-BoldItalic");
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { bold: Some(false), ..Default::default() }).unwrap();
    assert_eq!(text::text_blocks(&reopen(&doc), 0).unwrap()[0].base_font, "Times-Italic");
}

#[test]
fn paragraph_font_resources_never_restyle_other_text_or_shared_pages() {
    let mut doc = fixture();
    let resource_ref = pdfcraft_cos::ObjRef::new(6, 0);
    let original_font = doc.get(pdfcraft_cos::ObjRef::new(8, 0));
    doc.update_dict(resource_ref, |res| {
        let mut fonts = res.get(b"Font").unwrap().as_dict().unwrap().clone();
        fonts.set(b"PCEdHelveticaBold".to_vec(), (*original_font).clone());
        res.set(b"Font".to_vec(), Object::Dict(fonts));
    })
    .unwrap();
    let contents = b"BT /F1 12 Tf 72 700 Td (Target) Tj ET BT /PCEdHelveticaBold 12 Tf 72 600 Td (Unchanged) Tj ET";
    doc.set(pdfcraft_cos::ObjRef::new(7, 0), Object::Stream(Stream::flate(Dict::new(), contents)));
    let before_resources = doc.get(resource_ref);
    let before_other = text::text_lines(&doc, 1).unwrap();
    let before_caption = text::text_lines(&doc, 0).unwrap()[1].clone();
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { family: Some((added::Family::Helvetica, true, false)), ..Default::default() })
        .unwrap();
    let mut doc = reopen(&doc);
    assert_eq!(text::text_lines(&doc, 0).unwrap()[0].font, "PCEdHelveticaBold1");
    let caption = &text::text_lines(&doc, 0).unwrap()[1];
    assert_eq!(
        (&caption.text, caption.rect, &caption.base_font, caption.color),
        (&before_caption.text, before_caption.rect, &before_caption.base_font, before_caption.color)
    );
    assert_eq!(text::text_lines(&doc, 1).unwrap(), before_other);
    assert_eq!(*doc.get(resource_ref), *before_resources);
    // A second paragraph fallback must also leave the first generated face intact.
    text::rewrite_block(&mut doc, 0, 1, None, &text::BlockStyle { italic: Some(true), ..Default::default() }).unwrap();
    let lines = text::text_lines(&reopen(&doc), 0).unwrap();
    assert_eq!(lines[0].base_font, "Helvetica-Bold");
    assert_eq!(lines[1].base_font, "Courier-Oblique");
}

#[test]
fn independent_paragraph_substitutions_keep_their_own_faces() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (First) Tj ET BT /F2 12 Tf 72 600 Td (ab) Tj ET");
    doc.update_dict(pdfcraft_cos::ObjRef::new(6, 0), |font| {
        font.set(b"BaseFont".to_vec(), Object::name("ABCDEF+Times-Roman"));
    })
    .unwrap();
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { bold: Some(true), ..Default::default() }).unwrap();
    let mut doc = reopen(&doc);
    text::rewrite_block(&mut doc, 0, 1, None, &text::BlockStyle { italic: Some(true), ..Default::default() }).unwrap();
    let lines = text::text_lines(&reopen(&doc), 0).unwrap();
    assert_eq!((lines[0].text.as_str(), lines[0].base_font.as_str()), ("First", "Helvetica-Bold"));
    assert_eq!((lines[1].text.as_str(), lines[1].base_font.as_str()), ("ab", "Times-Italic"));
}

#[test]
fn page_font_coverage_matches_paragraph_whitespace_normalization() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (First) Tj ET");
    let fonts = text::page_fonts(&doc, 0, "  AB\r\n\t BA ").unwrap();
    assert!(fonts.iter().find(|f| f.resource == "F1").unwrap().encodable);
    let style = text::BlockStyle { font_resource: Some("F1".into()), ..Default::default() };
    text::rewrite_block(&mut doc, 0, 0, Some("  AB\r\n\t BA "), &style).unwrap();
    // Typed spaces are kept; a line break (CR LF counts once) and a tab each become one space.
    assert_eq!(text::text_blocks(&reopen(&doc), 0).unwrap()[0].text, "  AB   BA ");
    let fonts = text::page_fonts(&doc, 0, "AB\u{3000}BA").unwrap();
    assert!(!fonts.iter().find(|f| f.resource == "F1").unwrap().encodable, "ideographic space is not silently replaced");
    assert!(text::rewrite_block(&mut doc, 0, 0, Some("AB\u{3000}BA"), &style).is_err());
}

#[test]
fn reflow_and_alignment_keep_source_word_spacing() {
    for family in [None, Some((added::Family::Times, true, false))] {
        let mut doc = text_page("BT /F1 12 Tf 9 Tw 72 700 Td (A B) Tj ET");
        let style = text::BlockStyle { family, width: Some(100.0), align: Some(added::Align::Right), ..Default::default() };
        text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap();
        let doc = reopen(&doc);
        let line = &text::text_lines(&doc, 0).unwrap()[0];
        let width = pdfcraft_fonts::standard_text_width(&line.base_font, "A B", 12.0).unwrap() + 9.0;
        assert!((line.rect[2] - line.rect[0] - width).abs() < 0.001, "{:?}", line.rect);
        assert!((line.rect[2] - 172.0).abs() < 0.001, "source Tw must agree with the alignment measure");
    }
}

#[test]
fn a_new_paragraph_colour_does_not_spill_into_the_text_after_it() {
    let mut doc = text_page("BT /F1 10 Tf 72 700 Td (First paragraph) Tj 0 -40 Td (Second paragraph) Tj ET");
    let style = text::BlockStyle { color: Some([1.0, 0.0, 0.0]), ..Default::default() };
    text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap();
    let doc = reopen(&doc);
    // The fill colour in force where the second paragraph is shown (q/Q nest it).
    let ops = pdfcraft_content::parse(&page_content_bytes(&doc, 0)).ops;
    let (mut fill, mut stack) = (String::from("0 g"), Vec::new());
    for op in &ops {
        match op.op.as_slice() {
            b"q" => stack.push(fill.clone()),
            b"Q" => fill = stack.pop().unwrap_or_default(),
            b"g" | b"rg" | b"k" => fill = String::from_utf8_lossy(&pdfcraft_content::serialize_ops(std::slice::from_ref(op))).trim().to_string(),
            b"Tj" if op.operands.first().and_then(|o| o.as_string()).is_some_and(|s| s.to_text() == "Second paragraph") => break,
            _ => {}
        }
    }
    assert_eq!(fill, "0 g", "{ops:?}");
    assert_eq!(text::text_blocks(&doc, 0).unwrap()[1].text, "Second paragraph");
}

#[test]
fn a_rewritten_paragraph_keeps_its_place_in_a_shared_text_object() {
    // Three paragraphs in one BT … ET: rewriting the second keeps the order (paragraph numbers
    // and reading order) and every paragraph's position.
    let src = "BT /F1 10 Tf 72 700 Td (First paragraph) Tj 0 -40 Td (Second paragraph) Tj 0 -40 Td (Third paragraph) Tj ET";
    let mut doc = text_page(src);
    let before = text::text_blocks(&doc, 0).unwrap();
    text::replace_block(&mut doc, 0, 1, "Edited second").unwrap();
    let doc = reopen(&doc);
    let after = text::text_blocks(&doc, 0).unwrap();
    let texts: Vec<&str> = after.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(texts, ["First paragraph", "Edited second", "Third paragraph"]);
    assert_eq!(after[0].rect, before[0].rect);
    assert_eq!(after[2].rect, before[2].rect);
    assert!((after[1].rect[0] - before[1].rect[0]).abs() < 0.01 && (after[1].rect[1] - before[1].rect[1]).abs() < 0.01, "{:?}", after[1].rect);
}

#[test]
fn recolouring_a_paragraph_in_a_shared_text_object_keeps_order_and_nesting() {
    // Splitting the text object (to keep the order) and q … Q (to keep the colour in) together:
    // q/Q stay outside text objects, and the paragraph after it keeps the original colour.
    let src = "BT /F1 10 Tf 72 700 Td (First paragraph) Tj 0 -40 Td (Second paragraph) Tj 0 -40 Td (Third paragraph) Tj ET";
    let mut doc = text_page(src);
    let style = text::BlockStyle { color: Some([1.0, 0.0, 0.0]), ..Default::default() };
    text::rewrite_block(&mut doc, 0, 1, None, &style).unwrap();
    let doc = reopen(&doc);
    let texts: Vec<String> = text::text_blocks(&doc, 0).unwrap().into_iter().map(|b| b.text).collect();
    assert_eq!(texts, ["First paragraph", "Second paragraph", "Third paragraph"]);
    let ops = pdfcraft_content::parse(&page_content_bytes(&doc, 0)).ops;
    let (mut in_text, mut depth, mut red) = (false, 0usize, Vec::new());
    for op in &ops {
        match op.op.as_slice() {
            b"BT" => in_text = true,
            b"ET" => in_text = false,
            b"q" | b"Q" => {
                assert!(!in_text, "q/Q inside a text object: {ops:?}");
                depth = if op.is("q") { depth + 1 } else { depth.saturating_sub(1) };
            }
            b"rg" => red.push(depth),
            b"Tj" if op.operands.first().and_then(|o| o.as_string()).is_some_and(|s| s.to_text() == "Third paragraph") => {
                assert!(red.iter().all(|d| *d > depth), "red still in force for the third paragraph: {ops:?}");
            }
            _ => {}
        }
    }
    assert!(!red.is_empty(), "{ops:?}");
}

#[test]
fn selected_underlines_survive_reflow_restyle_and_reopen_without_touching_artwork() {
    let artwork = "q 0 1 0 RG 1 w 65 695 m 170 695 l S Q";
    let mut doc = text_page(&format!("{artwork} BT /F1 12 Tf 72 700 Td (Before abba after.) Tj ET"));
    let style = text::BlockStyle { range: Some([7, 11]), underline: Some(true), ..Default::default() };
    text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap();
    let mut doc = reopen(&doc);
    let block = text::text_blocks(&doc, 0).unwrap().remove(0);
    assert_eq!(block.text, "Before abba after.");
    assert_eq!(block.runs.iter().map(|r| (r.start, r.end, r.underline)).collect::<Vec<_>>(), [(0, 7, false), (7, 11, true), (11, 18, false)]);
    let first = page_content_bytes(&doc, 0);
    assert!(String::from_utf8_lossy(&first).contains(artwork));
    assert_eq!(String::from_utf8_lossy(&first).matches("/PdfCraftUnderlinePath ").count(), 4);
    // Applying the already-present style has no byte-level effect.
    assert!(text::is_noop(&doc, 0, 0, &block.text, &style).unwrap());
    text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap();
    assert_eq!(page_content_bytes(&doc, 0), first);
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([7, 11]), italic: Some(true), ..Default::default() }).unwrap();
    text::rewrite_block(&mut doc, 0, 0, Some("Before added after."), &text::BlockStyle { width: Some(70.0), ..Default::default() }).unwrap();
    let mut doc = reopen(&doc);
    let block = text::text_blocks(&doc, 0).unwrap().remove(0);
    assert_eq!(block.text, "Before added after.");
    assert!(block.line_count > 1);
    let selected: Vec<_> = block.runs.iter().filter(|r| r.start >= 7 && r.end <= 12).collect();
    assert!(!selected.is_empty());
    assert!(selected.iter().all(|r| r.underline && r.italic));
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(false), ..Default::default() }).unwrap();
    let doc = reopen(&doc);
    assert!(text::text_blocks(&doc, 0).unwrap()[0].runs.iter().all(|r| !r.underline));
    let content = String::from_utf8_lossy(&page_content_bytes(&doc, 0)).into_owned();
    assert!(!content.contains("/PdfCraftUnderline"), "obsolete paths and text markers were removed: {content}");
    assert!(content.contains(artwork), "unmarked artwork remains byte-for-byte");
}

#[test]
fn editable_underline_associations_refuse_missing_duplicate_and_unrecognized_paths_atomically() {
    let good = "BT /F1 12 Tf 72 700 Td /PdfCraftUnderlineText << /Version 1 /ID 1 >> BDC (A) Tj EMC ET";
    let path = "/PdfCraftUnderlinePath << /Version 1 /ID 1 >> BDC q 1 0 0 1 72 700 cm 0 0 0 RG 1 w 0 J [] 0 d 0 -2 m 8 -2 l S Q EMC";
    for source in [good.to_owned(), format!("{good} {path} {path}"), format!("{good} {}", path.replace(" S Q", " S 0 0 20 20 re f Q"))] {
        let mut doc = text_page(&source);
        let original = write_incremental(&doc, &SaveOptions::default()).unwrap();
        let error = text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(false), ..Default::default() }).unwrap_err();
        assert!(error.to_string().contains("ambiguous underline"), "{error}");
        assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), original);
    }
    // q/Q is independent of BDC/EMC.
    let mut doc = text_page(&format!("{good} {path}"));
    assert!(text::text_blocks(&doc, 0).unwrap()[0].runs[0].underline);
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(false), ..Default::default() }).unwrap();
    assert_eq!(text::text_blocks(&reopen(&doc), 0).unwrap()[0].text, "A");
    assert!(!String::from_utf8_lossy(&page_content_bytes(&doc, 0)).contains("PdfCraftUnderline"));
}

#[test]
fn editing_an_overlapping_label_preserves_an_independent_underlined_run() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (AB) Tj ET BT /F1 12 Tf 76 700 Td (CD) Tj ET");
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(true), ..Default::default() }).unwrap();
    let before = text::text_blocks(&doc, 0).unwrap()[0].clone();
    text::replace_block(&mut doc, 0, 1, "EF").unwrap();
    let mut doc = reopen(&doc);
    let blocks = text::text_blocks(&doc, 0).unwrap();
    assert_eq!(blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(), ["AB", "EF"]);
    assert_eq!(blocks[0].runs, before.runs);
    assert_eq!(String::from_utf8_lossy(&page_content_bytes(&doc, 0)).matches("/PdfCraftUnderlinePath ").count(), 2);
    let line = text::text_lines(&doc, 0).unwrap().iter().position(|l| l.text == "EF").unwrap();
    text::replace_line(&mut doc, 0, line, "GH").unwrap();
    let blocks = text::text_blocks(&reopen(&doc), 0).unwrap();
    assert_eq!(blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(), ["AB", "GH"]);
    assert_eq!(blocks[0].runs, before.runs);
}

#[test]
fn underline_metadata_survives_split_streams_and_does_not_change_shared_pages() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (X) Tj ET");
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(true), ..Default::default() }).unwrap();
    let bytes = page_content_bytes(&doc, 0);
    // Streams may be split only between tokens (ISO 32000-2 §7.8.2). Split at whitespace roughly
    // every 17 bytes so markers, their property dictionaries and bodies span several streams.
    // The fixture has no literal string containing whitespace, so each split is between tokens.
    let mut chunks = Vec::new();
    let mut start = 0;
    for (i, byte) in bytes.iter().enumerate() {
        if byte.is_ascii_whitespace() && i - start >= 17 {
            chunks.push(&bytes[start..i]);
            start = i;
        }
    }
    chunks.push(&bytes[start..]);
    assert!(chunks.len() > 10, "{chunks:?}");
    let pieces: Vec<Object> = chunks.into_iter().map(|chunk| Object::Ref(doc.add(Object::Stream(Stream::flate(Dict::new(), chunk))))).collect();
    let page = pdfcraft_model::pages(&doc)[0].obj;
    doc.update_dict(page, |page| page.set(b"Contents".to_vec(), Object::Array(pieces))).unwrap();
    let mut doc = reopen(&doc);
    assert!(text::text_blocks(&doc, 0).unwrap()[0].runs[0].underline);
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(false), ..Default::default() }).unwrap();
    assert!(!text::text_blocks(&reopen(&doc), 0).unwrap()[0].runs[0].underline);
    assert!(!String::from_utf8_lossy(&page_content_bytes(&doc, 0)).contains("PdfCraftUnderline"));

    let mut doc = fixture();
    let other = page_content_bytes(&doc, 1);
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([0, 1]), underline: Some(true), ..Default::default() }).unwrap();
    let doc = reopen(&doc);
    assert_eq!(page_content_bytes(&doc, 1), other, "the other placement of a shared stream is untouched");
    assert!(text::text_blocks(&doc, 0).unwrap()[0].runs[0].underline);
    assert!(!text::text_blocks(&doc, 1).unwrap()[0].runs[0].underline);
}

#[test]
fn underlining_refuses_active_paths_and_uses_solid_lines_without_changing_stroke_state() {
    let mut doc = text_page("[3 2] 1 d 2 J BT /F1 12 Tf 72 700 Td (Text) Tj ET 0 650 m 100 650 l S");
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(true), ..Default::default() }).unwrap();
    let content = String::from_utf8_lossy(&page_content_bytes(&doc, 0)).into_owned();
    assert!(content.contains("0 J\n[] 0 d\n"), "authored underline uses a solid butt-capped stroke");
    assert!(content.contains("0 650 m 100 650 l S"), "later artwork remains intact");
    assert!(text::text_blocks(&reopen(&doc), 0).unwrap()[0].runs[0].underline);
    for path in ["0 650 m 100 650 l", "0 650 m q 100 650 l Q"] {
        let mut doc = text_page(&format!("{path} BT /F1 12 Tf 72 700 Td (Text) Tj ET S"));
        let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
        let error = text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(true), ..Default::default() }).unwrap_err();
        assert!(error.to_string().contains("active drawing path"));
        assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before);
    }
}

#[test]
fn underlining_one_of_two_paragraphs_keeps_the_others_paths_and_unique_ids() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (First) Tj 0 -45 Td (Second) Tj ET");
    for block in [0, 1] {
        text::rewrite_block(&mut doc, 0, block, None, &text::BlockStyle { underline: Some(true), ..Default::default() }).unwrap();
    }
    let mut doc = reopen(&doc);
    assert!(text::text_blocks(&doc, 0).unwrap().iter().all(|b| b.runs.iter().all(|r| r.underline)));
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(false), ..Default::default() }).unwrap();
    let doc = reopen(&doc);
    let blocks = text::text_blocks(&doc, 0).unwrap();
    assert_eq!(blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(), ["First", "Second"]);
    assert!(blocks[0].runs.iter().all(|r| !r.underline));
    assert!(blocks[1].runs.iter().all(|r| r.underline));
    assert_eq!(String::from_utf8_lossy(&page_content_bytes(&doc, 0)).matches("/PdfCraftUnderlinePath ").count(), 6);
}

/// One authored underline association read straight from page 0's content.
#[derive(Debug)]
struct Association {
    id: i64,
    bytes: Vec<u8>,
    /// The text matrix the associated glyph is shown with.
    tm: [f64; 6],
    /// The path's `cm` and its single horizontal segment in that space.
    cm: [f64; 6],
    x0: f64,
    x1: f64,
    y: f64,
}

/// Every text marker paired with exactly one path marker of the same ID (and no orphans), with
/// geometry checked against its glyph: the path starts where the glyph is placed and lies just
/// below that glyph's baseline.
fn underline_associations(doc: &Document) -> Vec<Association> {
    let ops = pdfcraft_content::parse(&page_content_bytes(doc, 0)).ops;
    let mut text = Vec::new();
    let mut paths = std::collections::HashMap::new();
    let mut tm = [0.0; 6];
    for (i, op) in ops.iter().enumerate() {
        if op.is("Tm") {
            tm = op.nums::<6>().unwrap();
        }
        if !op.is("BDC") {
            continue;
        }
        let id = || op.operands[1].as_dict().unwrap().int(b"ID").unwrap();
        match op.name(0).unwrap_or_default() {
            b"PdfCraftUnderlineText" => {
                let show = &ops[i + 1];
                assert!(show.is("Tj") && ops[i + 2].is("EMC"), "{show:?}");
                let Some(Object::String(s)) = show.operands.first() else { panic!("{show:?}") };
                text.push((id(), s.bytes.clone(), tm));
            }
            b"PdfCraftUnderlinePath" => {
                let names: Vec<&[u8]> = ops[i + 1..i + 12].iter().map(|o| o.op.as_slice()).collect();
                assert_eq!(names, [&b"q"[..], b"cm", b"RG", b"w", b"J", b"d", b"m", b"l", b"S", b"Q", b"EMC"]);
                let (m, l) = (ops[i + 7].nums::<2>().unwrap(), ops[i + 8].nums::<2>().unwrap());
                assert_eq!(m[1], l[1], "horizontal");
                assert!(paths.insert(id(), (ops[i + 2].nums::<6>().unwrap(), m[0], l[0], m[1])).is_none(), "duplicate path ID");
            }
            _ => {}
        }
    }
    assert_eq!(text.len(), paths.len(), "no orphan paths");
    let out: Vec<Association> = text
        .into_iter()
        .map(|(id, bytes, tm)| {
            let (cm, x0, x1, y) = paths.remove(&id).expect("each text marker has its own path");
            Association { id, bytes, tm, cm, x0, x1, y }
        })
        .collect();
    for a in &out {
        // The glyph's Tm is [1 0 0 1 x y] · cm (cm is the paragraph's text matrix, upright or
        // turned): the path lies in that glyph's text space, starting where it is placed.
        assert!(a.tm.iter().zip(&a.cm).take(4).all(|(t, c)| (t - c).abs() < 1e-6), "{a:?}");
        let (x, y) = pdfcraft_content::Matrix(a.cm).invert().unwrap().apply(a.tm[4], a.tm[5]);
        assert!((x - a.x0).abs() < 2e-3, "{a:?}");
        assert!(a.x1 > a.x0, "{a:?}");
        let below = y - a.y;
        assert!(below > 0.0 && below < 12.0, "{a:?}");
    }
    let ids: std::collections::HashSet<i64> = out.iter().map(|a| a.id).collect();
    assert_eq!(ids.len(), out.len(), "unique IDs");
    out
}

/// The x where every glyph shown on page 0 starts, interpreting `Tm`, `Tf`, `Tj` and `TJ`
/// (Helvetica widths, no Tc/Tw/Tz, unscaled upright text matrices: the fixtures that use it).
fn shown_x(doc: &Document) -> Vec<f64> {
    let (mut tm, mut size, mut out) = ([0.0; 6], 0.0, Vec::new());
    for op in pdfcraft_content::parse(&page_content_bytes(doc, 0)).ops {
        match op.op.as_slice() {
            b"Tm" => tm = op.nums::<6>().unwrap(),
            b"Tf" => size = op.num(1).unwrap(),
            b"Tj" | b"TJ" => {
                let parts = match op.operands.first() {
                    Some(Object::Array(parts)) => parts.clone(),
                    other => other.cloned().into_iter().collect(),
                };
                for part in parts {
                    match part {
                        Object::String(s) => {
                            for byte in s.bytes {
                                out.push(tm[4]);
                                tm[4] += pdfcraft_fonts::standard_text_width("Helvetica", &char::from(byte).to_string(), size).unwrap();
                            }
                        }
                        number => tm[4] -= number.as_f64().unwrap() / 1000.0 * size,
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Every `Tj`/`TJ` on page 0 with its strings joined and its TJ numbers, in content order.
fn shows(doc: &Document) -> Vec<(Vec<u8>, Vec<f64>)> {
    pdfcraft_content::parse(&page_content_bytes(doc, 0))
        .ops
        .iter()
        .filter(|op| op.is("Tj") || op.is("TJ"))
        .map(|op| {
            let parts = match op.operands.first() {
                Some(Object::Array(parts)) => parts.clone(),
                other => other.cloned().into_iter().collect(),
            };
            let bytes = parts.iter().filter_map(Object::as_string).flat_map(|s| s.bytes.clone()).collect();
            (bytes, parts.iter().filter_map(Object::as_f64).collect())
        })
        .collect()
}

#[test]
fn underlines_keep_tj_adjustments_logical_spaces_and_exact_positions() {
    // Helvetica 12 pt: A and V advance 8.004. -150 moves right 1.8 (kerning stays a glyph
    // adjustment), 120 moves left 1.44, and -1000 (12) reads as a logical space without codes.
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td [(A) -150 (V) 120 (A) -1000 (B)] TJ ET");
    assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, "AVA B");
    let starts = [72.0, 81.804, 88.368, 96.372, 108.372];
    let ends = [81.804, 88.368, 96.372, 108.372, 116.376];
    let close = |a: f64, b: f64| (a - b).abs() < 2e-3;
    let same = |xs: &[f64], want: &[f64]| xs.len() == want.len() && xs.iter().zip(want).all(|(a, b)| close(*a, *b));

    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(true), ..Default::default() }).unwrap();
    let mut doc = reopen(&doc);
    let block = text::text_blocks(&doc, 0).unwrap().remove(0);
    assert_eq!(block.text, "AVA B");
    assert!(block.runs.iter().all(|r| r.underline), "{:?}", block.runs);
    let a = underline_associations(&doc);
    assert_eq!(a.len(), 5, "{a:?}");
    for (i, assoc) in a.iter().enumerate() {
        assert!(close(assoc.tm[4], starts[i]) && close(assoc.tm[5], 700.0), "{i}: {assoc:?}");
        assert!(close(assoc.cm[4] + assoc.x1, ends[i]), "{i}: the underline covers the glyph and its adjustment: {assoc:?}");
    }
    assert_eq!(a[3].bytes, b" ", "the code-less logical space gained an encodable space to carry its association");

    // Remove the underline from "VA" only: the other three associations stay, nothing moves.
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([1, 3]), underline: Some(false), ..Default::default() }).unwrap();
    let mut doc = reopen(&doc);
    let block = text::text_blocks(&doc, 0).unwrap().remove(0);
    assert_eq!(block.text, "AVA B");
    assert_eq!(block.runs.iter().map(|r| (r.start, r.end, r.underline)).collect::<Vec<_>>(), [(0, 1, true), (1, 3, false), (3, 5, true)]);
    let a = underline_associations(&doc);
    assert!(same(&a.iter().map(|a| a.tm[4]).collect::<Vec<_>>(), &[starts[0], starts[3], starts[4]]), "{a:?}");
    assert!(same(&shown_x(&doc), &starts), "{:?}", shown_x(&doc));

    // And off everywhere: no markers remain and every glyph is still exactly where it was.
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(false), ..Default::default() }).unwrap();
    let doc = reopen(&doc);
    assert!(underline_associations(&doc).is_empty());
    assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, "AVA B");
    assert!(same(&shown_x(&doc), &starts), "{:?}", shown_x(&doc));
}

#[test]
fn wrapped_and_partially_restyled_underlines_keep_one_path_per_glyph() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Alpha beta gamma delta epsilon) Tj ET");
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([6, 22]), underline: Some(true), ..Default::default() }).unwrap();
    // A different font for part of the underlined range, then a narrow rewrap.
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([11, 16]), bold: Some(true), ..Default::default() }).unwrap();
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { width: Some(60.0), ..Default::default() }).unwrap();
    let doc = reopen(&doc);
    let block = text::text_blocks(&doc, 0).unwrap().remove(0);
    assert_eq!(block.text, "Alpha beta gamma delta epsilon");
    assert!(block.line_count > 2, "{block:?}");
    let underlined: String = block.runs.iter().filter(|r| r.underline).flat_map(|r| block.text.chars().skip(r.start).take(r.end - r.start)).collect();
    assert_eq!(underlined, "beta gamma delta");
    assert!(block.runs.iter().any(|r| r.underline && r.bold), "{:?}", block.runs);
    let a = underline_associations(&doc);
    // One association per underlined character shown (the font maps each code to one
    // character). The space a row wraps at is not drawn when it has the style of the character
    // before it ("beta "), so no underline runs into the margin there; a space whose style
    // differs (the regular one after bold "gamma") is kept so styles read back unchanged.
    let letters: Vec<u8> = a.iter().map(|a| a.bytes[0]).filter(|b| *b != b' ').collect();
    assert_eq!(letters, b"betagammadelta", "{a:?}");
    assert_eq!(a.len(), "beta gamma delta".chars().count() - 1, "{a:?}");
    let rows: std::collections::BTreeSet<i64> = a.iter().map(|a| (a.tm[5] * 1000.0).round() as i64).collect();
    assert!(rows.len() > 1, "the underlined range wraps over several rows and each path follows its row");
}

#[test]
fn forged_or_malformed_underline_markers_refuse_atomically() {
    let path =
        |id: &str| format!("/PdfCraftUnderlinePath << /Version 1 /ID {id} >> BDC q 1 0 0 1 72 700 cm 0 0 0 RG 1 w 0 J [] 0 d 0 -2 m 8 -2 l S Q EMC");
    let text = |props: &str| format!("BT /F1 12 Tf 72 700 Td /PdfCraftUnderlineText {props} BDC (A) Tj EMC ET");
    let good = text("<< /Version 1 /ID 1 >>");
    let cases = [
        format!("{} {}", text("<< /Version 2 /ID 1 >>"), path("1")),
        format!("{} {}", text("<< /Version 1 /ID 0 >>"), path("0")),
        format!("{} {}", text("<< /Version 1 /ID -4 >>"), path("-4")),
        format!("{} {}", text("<< /Version 1 /ID 1.5 >>"), path("1.5")),
        format!("{} {}", text("<< /Version 1 >>"), path("1")),
        format!("{} {}", text("/Props1"), path("1")),
        format!("BT /F1 12 Tf 72 700 Td /PdfCraftUnderlineText << /Version 1 /ID 1 >> BDC (A) Tj (B) Tj EMC ET {}", path("1")),
        format!("BT /F1 12 Tf 72 700 Td /PdfCraftUnderlineText << /Version 1 /ID 1 >> BDC (A) Tj ET {}", path("1")),
        format!("{good} BT /F1 12 Tf 72 680 Td /PdfCraftUnderlineText << /Version 1 /ID 1 >> BDC (B) Tj EMC ET {}", path("1")),
        format!("{good} {}", path("1").replace("[] 0 d", "[3 2] 0 d")),
        format!("{good} {}", path("1").replace("0 J", "1 J")),
        format!("{good} {}", path("1").replace("1 w", "0 w")),
        format!("{good} {}", path("1").replace("0 0 0 RG", "0 0 2 RG")),
        format!("{good} {}", path("1").replace("72 700 cm", "72 cm")),
        format!("{good} {}", path("1").replace("Q EMC", "EMC Q")),
        good.clone(),
    ];
    for source in cases {
        let mut doc = text_page(&source);
        let original = write_incremental(&doc, &SaveOptions::default()).unwrap();
        let blocks = text::text_blocks(&doc, 0).unwrap();
        assert!(blocks.iter().flat_map(|b| &b.runs).all(|r| !r.underline), "an untrusted association never reads as underlined: {source}");
        for style in [
            text::BlockStyle { underline: Some(false), ..Default::default() },
            text::BlockStyle { underline: Some(true), ..Default::default() },
            text::BlockStyle { color: Some([1.0, 0.0, 0.0]), ..Default::default() },
        ] {
            assert!(!text::is_noop(&doc, 0, 0, &blocks[0].text, &style).unwrap(), "{source}");
            let error = text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap_err();
            assert!(error.to_string().contains("ambiguous underline"), "{source}: {error}");
            assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), original, "{source}");
        }
        let error = text::replace_block(&mut doc, 0, 0, "Changed").unwrap_err();
        assert!(error.to_string().contains("ambiguous underline"), "{source}: {error}");
        let error = text::replace_line(&mut doc, 0, 0, "Changed").unwrap_err();
        assert!(error.to_string().contains("paragraph editing"), "{source}: {error}");
        assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), original, "{source}");
    }
}

#[test]
fn orphan_and_nested_markers_are_safe_and_id_exhaustion_refuses() {
    // A path marker without text is artwork; new IDs never reuse its ID.
    let orphan = "/PdfCraftUnderlinePath << /Version 1 /ID 41 >> BDC q 1 0 0 1 10 10 cm 0 0 0 RG 1 w 0 J [] 0 d 0 0 m 8 0 l S Q EMC";
    let mut doc = text_page(&format!("{orphan} BT /F1 12 Tf 72 700 Td (Hi) Tj ET"));
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(true), ..Default::default() }).unwrap();
    let content = String::from_utf8_lossy(&page_content_bytes(&doc, 0)).into_owned();
    assert!(content.starts_with(orphan), "{content}");
    let ops = pdfcraft_content::parse(content.as_bytes()).ops;
    let ids: Vec<i64> = ops
        .iter()
        .filter(|o| o.is("BDC") && o.name(0) == Some(b"PdfCraftUnderlineText"))
        .map(|o| o.operands[1].as_dict().unwrap().int(b"ID").unwrap())
        .collect();
    assert_eq!(ids, [42, 43]);
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(false), ..Default::default() }).unwrap();
    let content = String::from_utf8_lossy(&page_content_bytes(&doc, 0)).into_owned();
    assert!(content.starts_with(orphan) && content.matches("PdfCraftUnderline").count() == 1, "{content}");

    // An association nested in other marked content is still recognised and removed cleanly.
    let nested = "BT /F1 12 Tf 72 700 Td /Span << /Lang (en) >> BDC /PdfCraftUnderlineText << /Version 1 /ID 1 >> BDC (A) Tj EMC EMC ET \
                  /PdfCraftUnderlinePath << /Version 1 /ID 1 >> BDC q 1 0 0 1 72 700 cm 0 0 0 RG 1 w 0 J [] 0 d 0 -2 m 8 -2 l S Q EMC";
    let mut doc = text_page(nested);
    assert!(text::text_blocks(&doc, 0).unwrap()[0].runs[0].underline);
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(false), ..Default::default() }).unwrap();
    let doc = reopen(&doc);
    let content = String::from_utf8_lossy(&page_content_bytes(&doc, 0)).into_owned();
    assert!(!content.contains("PdfCraftUnderline"), "{content}");
    assert_eq!(content.matches("BDC").count(), content.matches("EMC").count(), "marked content stays balanced: {content}");
    assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, "A");

    // The largest ID is taken: a new association cannot be numbered, so the edit refuses.
    let mut doc = text_page(&format!("{} BT /F1 12 Tf 72 700 Td (Hi) Tj ET", orphan.replace("/ID 41", "/ID 9223372036854775807")));
    let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
    let error = text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(true), ..Default::default() }).unwrap_err();
    assert!(error.to_string().contains("exhausted underline association IDs"), "{error}");
    assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before);
    // Edits that add no underline are unaffected by the exhausted ID space.
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { color: Some([0.0, 0.0, 1.0]), ..Default::default() }).unwrap();
}

#[test]
fn an_active_path_refuses_every_paragraph_rewrite_conservatively() {
    // A current path is not saved by q/Q; inserting text and paths before its painting operator
    // could change what it paints. Any rewrite of such a paragraph is refused, not only
    // underlines, and refusal leaves the document untouched.
    let mut doc = text_page("0 650 m 100 650 l BT /F1 12 Tf 72 700 Td (Text) Tj ET S");
    let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
    for style in [
        text::BlockStyle { underline: Some(true), ..Default::default() },
        text::BlockStyle { color: Some([1.0, 0.0, 0.0]), ..Default::default() },
        text::BlockStyle { range: Some([0, 2]), bold: Some(true), ..Default::default() },
        text::BlockStyle { width: Some(30.0), ..Default::default() },
    ] {
        let error = text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap_err();
        assert!(error.to_string().contains("active drawing path"), "{error}");
        assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before);
    }
    let error = text::replace_block(&mut doc, 0, 0, "Other").unwrap_err();
    assert!(error.to_string().contains("active drawing path"), "{error}");
    assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before);
    // Once the path is painted (or discarded with n) before the text, editing works.
    for painted in ["0 650 m 100 650 l S", "0 650 m 100 650 l n"] {
        let mut doc = text_page(&format!("{painted} BT /F1 12 Tf 72 700 Td (Text) Tj ET"));
        text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(true), color: Some([1.0, 0.0, 0.0]), ..Default::default() })
            .unwrap();
        assert!(String::from_utf8_lossy(&page_content_bytes(&doc, 0)).starts_with(painted));
    }
}

#[test]
fn line_editing_refuses_authored_underlines_with_a_paragraph_hint() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Under) Tj 0 -40 Td (Plain) Tj ET");
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { underline: Some(true), ..Default::default() }).unwrap();
    let mut doc = reopen(&doc);
    let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
    let lines = text::text_lines(&doc, 0).unwrap();
    let under = lines.iter().position(|l| l.text == "Under").unwrap();
    let error = text::replace_line(&mut doc, 0, under, "Over").unwrap_err();
    assert!(error.to_string().contains("use paragraph editing to preserve this line's editable underlines"), "{error}");
    assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before);
    // The plain neighbour is still line-editable, and the underlined paragraph keeps its paths.
    let plain = lines.iter().position(|l| l.text == "Plain").unwrap();
    text::replace_line(&mut doc, 0, plain, "Other").unwrap();
    let doc = reopen(&doc);
    assert_eq!(underline_associations(&doc).len(), 5);
    let blocks = text::text_blocks(&doc, 0).unwrap();
    assert_eq!(blocks.iter().map(|b| (b.text.as_str(), b.runs.iter().all(|r| r.underline))).collect::<Vec<_>>(), [("Under", true), ("Other", false)]);
}

#[test]
fn justify_underline_and_spacing() {
    let src = "BT /F1 10 Tf 12 TL 100 700 Td (One two three four five six seven) Tj T* (eight nine ten eleven twelve) Tj T* (end) Tj ET";
    // Justified: every line but the last reaches the right edge.
    let mut doc = text_page(src);
    let right = text::text_blocks(&doc, 0).unwrap()[0].rect[2];
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { align: Some(added::Align::Justify), ..Default::default() }).unwrap();
    let lines = text::text_lines(&doc, 0).unwrap();
    for l in &lines[..lines.len() - 1] {
        assert!((l.rect[2] - right).abs() < 1.0, "{:?} ends at {} not {right}", l.text, l.rect[2]);
    }
    // Double line spacing, underlined.
    let mut doc = text_page(src);
    let style = text::BlockStyle { line_spacing: Some(2.0), underline: Some(true), ..Default::default() };
    text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap();
    let lines = text::text_lines(&doc, 0).unwrap();
    assert!((lines[0].origin_baseline() - lines[1].origin_baseline() - 20.0).abs() < 0.01);
    let content = String::from_utf8_lossy(&page_content_bytes(&doc, 0)).into_owned();
    assert!(content.contains(" l\nS\n") || content.contains(" l S"), "{content}");
    // Character spacing widens, horizontal scale narrows.
    let mut doc = text_page("BT /F1 10 Tf 100 700 Td (Wide text) Tj ET");
    let w0 = text::text_lines(&doc, 0).unwrap()[0].rect;
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { char_spacing: Some(2.0), ..Default::default() }).unwrap();
    let w1 = text::text_lines(&doc, 0).unwrap()[0].rect;
    assert!((w1[2] - w1[0]) - (w0[2] - w0[0]) > 15.0, "{w0:?} → {w1:?}");
    let mut doc = text_page("BT /F1 10 Tf 100 700 Td (Wide text) Tj ET");
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { scale: Some(50.0), ..Default::default() }).unwrap();
    let w2 = text::text_lines(&doc, 0).unwrap()[0].rect;
    assert!(((w2[2] - w2[0]) - (w0[2] - w0[0]) / 2.0).abs() < 1.0, "{w0:?} → {w2:?}");
}

#[test]
fn a_line_drawn_twice_is_replaced_everywhere() {
    // Fake bold: a line drawn twice at the same spot (each copy its own text object). The two
    // copies group apart, but editing one replaces both — a surviving copy would show the old
    // text under the new.
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Hello) Tj ET BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
    assert_eq!(text::text_blocks(&doc, 0).unwrap().len(), 2, "the copies group apart");
    text::replace_block(&mut doc, 0, 0, "Hello world").unwrap();
    let doc = reopen(&doc);
    let lines = text::text_lines(&doc, 0).unwrap();
    assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["Hello world"], "one line, replaced");
    let content = String::from_utf8_lossy(&page_content_bytes(&doc, 0)).into_owned();
    assert_eq!(content.matches("(Hello").count(), 1, "the second copy's operators are gone: {content}");
    // Copies in one text object (the matrix re-issued at the same spot) go too.
    let mut doc = text_page("BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hi) Tj 1 0 0 1 72.3 700 Tm (Hi) Tj ET");
    text::replace_line(&mut doc, 0, 0, "Hey").unwrap();
    let doc = reopen(&doc);
    let lines = text::text_lines(&doc, 0).unwrap();
    assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["Hey"], "one line, replaced");
    // A neighbouring line is not coincident and stays (40 pt spacing groups apart at 12 pt).
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Hello) Tj 0 -40 Td (world) Tj ET");
    text::replace_block(&mut doc, 0, 0, "Hello there").unwrap();
    let doc = reopen(&doc);
    let lines = text::text_lines(&doc, 0).unwrap();
    assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["Hello there", "world"]);
}

#[test]
fn paragraphs_move_and_rewrap_to_a_new_width() {
    let para = "BT /F1 10 Tf 12 TL 100 700 Td (One two three four five six seven) Tj T* (eight nine ten eleven twelve) Tj ET";
    let close = |a: f64, b: f64| (a - b).abs() < 0.5;
    // Moved 50 right and 200 down, same words and wrapping.
    let mut doc = text_page(para);
    let before = text::text_blocks(&doc, 0).unwrap()[0].clone();
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { offset: Some([50.0, -200.0]), ..Default::default() }).unwrap();
    let doc = reopen(&doc);
    let after = text::text_blocks(&doc, 0).unwrap()[0].clone();
    assert_eq!((after.text.as_str(), after.lines.len()), (before.text.as_str(), before.lines.len()));
    assert!(close(after.rect[0], before.rect[0] + 50.0) && close(after.rect[1], before.rect[1] - 200.0), "{:?} → {:?}", before.rect, after.rect);
    // Drawn at half scale: the move is still in page space.
    let mut doc = text_page(&format!("q 0.5 0 0 0.5 0 0 cm {} Q", para.replace("100 700 Td", "200 1400 Td")));
    let before = text::text_blocks(&doc, 0).unwrap()[0].clone();
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { offset: Some([30.0, 40.0]), ..Default::default() }).unwrap();
    let after = text::text_blocks(&reopen(&doc), 0).unwrap()[0].clone();
    assert!(close(after.rect[0], before.rect[0] + 30.0) && close(after.rect[1], before.rect[1] + 40.0), "{:?} → {:?}", before.rect, after.rect);
    // A narrower box rewraps into more lines that fit it; a wider one into fewer.
    let mut doc = text_page(para);
    let left = text::text_blocks(&doc, 0).unwrap()[0].rect[0];
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { width: Some(80.0), ..Default::default() }).unwrap();
    let narrow = text::text_blocks(&reopen(&doc), 0).unwrap()[0].clone();
    assert!(narrow.lines.len() > 2, "{narrow:?}");
    assert!(narrow.rect[2] <= left + 80.5, "{narrow:?}");
    let mut doc = text_page(para);
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { width: Some(400.0), ..Default::default() }).unwrap();
    assert_eq!(text::text_blocks(&reopen(&doc), 0).unwrap()[0].lines.len(), 1);
    // Untrusted numbers (the automation tools pass them through) are refused, not drawn.
    let mut doc = text_page(para);
    for style in [
        text::BlockStyle { offset: Some([f64::NAN, 0.0]), ..Default::default() },
        text::BlockStyle { offset: Some([0.0, f64::INFINITY]), ..Default::default() },
        text::BlockStyle { width: Some(f64::NAN), ..Default::default() },
    ] {
        assert!(text::rewrite_block(&mut doc, 0, 0, None, &style).is_err(), "{style:?}");
    }
    // A zero width is clamped to one character's width rather than looping or vanishing.
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { width: Some(0.0), ..Default::default() }).unwrap();
    assert_eq!(text::text_blocks(&reopen(&doc), 0).unwrap().iter().map(|b| b.text.split_whitespace().count()).sum::<usize>(), 12);
}

/// A page split into several content streams, as AutoCAD writes them: the first scales the
/// page (`0.12 0 0 0.12 0 0 cm`, no `q`), the text comes in a later one.
fn split_page(first: &str, second: &str) -> Document {
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents [4 0 R 5 0 R] /Resources << /Font << /F1 6 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{first}\nendstream", first.len()),
        format!("<< /Length {} >>\nstream\n{second}\nendstream", second.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
    ];
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

#[test]
fn the_graphics_state_carries_over_between_content_streams() {
    let close = |a: f64, b: f64| (a - b).abs() < 0.5;
    // Text at (1000, 2000) in a space scaled by 0.12: on the page at (120, 240), 12 pt.
    let mut doc = split_page("0.12 0 0 0.12 0 0 cm", "BT /F1 100 Tf 1000 2000 Td (Site plan) Tj ET");
    let lines = text::text_lines(&doc, 0).unwrap();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let l = &lines[0];
    assert!(close(l.rect[0], 120.0) && l.rect[1] > 230.0 && l.rect[3] < 260.0, "{:?}", l.rect);
    assert!(close(l.size, 12.0), "{}", l.size);
    // Editing keeps the paragraph where it was, at its size; moving it moves it in page space.
    text::rewrite_block(&mut doc, 0, 0, Some("Site plan, rev. B"), &text::BlockStyle { offset: Some([10.0, -20.0]), ..Default::default() }).unwrap();
    let after = text::text_blocks(&reopen(&doc), 0).unwrap();
    assert_eq!(after[0].text, "Site plan, rev. B");
    assert!(close(after[0].rect[0], 130.0) && close(after[0].rect[1], l.rect[1] - 20.0), "{:?}", after[0].rect);
    assert!(close(after[0].size, 12.0), "{}", after[0].size);
    // A `q` left open in one stream and closed in the next still restores the state.
    let doc = split_page("q 0.5 0 0 0.5 0 0 cm", "Q BT /F1 10 Tf 100 100 Td (Footpath) Tj ET");
    let l = &text::text_lines(&doc, 0).unwrap()[0];
    assert!(close(l.rect[0], 100.0) && close(l.size, 10.0), "{l:?}");
}

/// #125: page-content tools refuse text the standard fonts can't draw instead of writing `?`,
/// and leave the document untouched.
#[test]
fn page_text_the_standard_fonts_cant_draw_is_refused() {
    let mut doc = fixture();
    let before: Vec<Vec<String>> = (0..3).map(|p| streams(&reopen(&doc), p)).collect();
    let text = AddedText { rect: [72.0, 600.0, 300.0, 700.0], text: "日本語のテキスト".into(), ..AddedText::default() };
    let err = add_content(&mut doc, 0, &Content::Text(text)).unwrap_err().to_string();
    assert!(err.contains("\"日\" (U+65E5)"), "{err}");
    let hf = HeaderFooter {
        text: ["Ελληνικά".into(), String::new(), String::new(), String::new(), String::new(), String::new()],
        ..HeaderFooter::default()
    };
    assert!(add_header_footer(&mut doc, &[0, 1], &hf, true, &cx()).unwrap_err().to_string().contains("U+0395"));
    let wm = Watermark { text: "机密".into(), ..Watermark::default() };
    assert!(add_watermark(&mut doc, &[0], &wm, true).unwrap_err().to_string().contains("U+673A"));
    let after: Vec<Vec<String>> = (0..3).map(|p| streams(&reopen(&doc), p)).collect();
    assert_eq!(before, after, "a refused edit changes nothing");
    // Western European text, including what only WinAnsi (not Latin-1) has, still works.
    let text = AddedText { rect: [72.0, 600.0, 300.0, 700.0], text: "Café — 5€ ™".into(), ..AddedText::default() };
    add_content(&mut doc, 0, &Content::Text(text.clone())).unwrap();
    // Retyping existing text is refused the same way, and the whole document is left as it was.
    let saved = write_incremental(&doc, &SaveOptions::default()).unwrap();
    let retyped = AddedText { text: "Café 中".into(), ..text.clone() };
    assert!(update_content(&mut doc, 0, 0, &Content::Text(retyped)).unwrap_err().to_string().contains("U+4E2D"));
    assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), saved, "a refused update changes nothing");
    // A carriage return is checked where it would be drawn: added text splits on LF only, so
    // CRLF would leave a `?`; headers and watermarks split CRLF into lines, but not a lone CR.
    let crlf = AddedText { text: "A\r\nB".into(), ..text };
    assert!(add_content(&mut doc, 0, &Content::Text(crlf)).unwrap_err().to_string().contains("U+000D"));
    let hf = |t: &str| HeaderFooter {
        text: [t.into(), String::new(), String::new(), String::new(), String::new(), String::new()],
        ..HeaderFooter::default()
    };
    add_header_footer(&mut doc, &[0], &hf("Left\r\n<<Bates Number#6#1#ACME-#>>"), true, &cx()).unwrap();
    assert!(add_header_footer(&mut doc, &[0], &hf("A\rB"), true, &cx()).unwrap_err().to_string().contains("U+000D"));
    // Arabic is shaped with the craft-fonts Arabic face (#403): drawable when that face is built
    // in, refused (never written as `?`) when it isn't. Text left to the standard font still counts.
    let arabic = |text: &str| AddedText { rect: [72.0, 600.0, 300.0, 700.0], text: text.into(), ..AddedText::default() };
    // U+061C is in the Arabic block but is a direction mark that's dropped, not shaped: it never
    // makes text drawable, nor (with the face) undrawable.
    assert!(add_content(&mut doc, 0, &Content::Text(arabic("\u{061C}中"))).is_err());
    if pdfcraft_fonts::arabic_has('م') {
        add_content(&mut doc, 0, &Content::Text(arabic("مرحبا Hello"))).unwrap();
        add_content(&mut doc, 0, &Content::Text(arabic("\u{061C}Hello"))).unwrap();
        for text in ["مرحبا 中", "\u{061C}中"] {
            let err = add_content(&mut doc, 0, &Content::Text(arabic(text))).unwrap_err().to_string();
            assert!(err.contains("U+4E2D"), "{text:?}: {err}");
        }
    } else {
        // #403's own refusal says what's missing; the Add-text editor still keeps the draft.
        let err = add_content(&mut doc, 0, &Content::Text(arabic("مرحبا Hello"))).unwrap_err().to_string();
        assert!(err.contains("CRAFT_FONTS_DIR"), "{err}");
        assert_eq!(first_undrawable(&arabic("مرحبا Hello")), Some('م'));
        // An existing item may still take Arabic (#403 keeps it movable), but nothing else that
        // the standard font would write as `?`.
        let n = list_added(&doc).iter().filter(|a| a.page == 0).count();
        add_content(&mut doc, 0, &Content::Text(arabic("x"))).unwrap();
        let err = update_content(&mut doc, 0, n, &Content::Text(arabic("ب中"))).unwrap_err().to_string();
        assert!(err.contains("U+4E2D"), "{err}");
        update_content(&mut doc, 0, n, &Content::Text(arabic("ب"))).unwrap();
    }
}

fn xobject(doc: &Document, page: usize, name: &[u8]) -> Option<pdfcraft_cos::ObjRef> {
    let p = &pdfcraft_model::pages(doc)[page];
    let res = doc.resolve(p.dict.get(b"Resources")?);
    let xo = doc.resolve(res.as_dict()?.get(b"XObject")?);
    xo.as_dict()?.get(name).and_then(Object::as_ref)
}

fn annot_at(doc: &Document, page: usize, index: usize) -> pdfcraft_cos::ObjRef {
    let p = &pdfcraft_model::pages(doc)[page];
    let list = doc.resolve(p.dict.get(b"Annots").unwrap());
    list.as_array().unwrap()[index].as_ref().unwrap()
}

#[test]
fn fill_sign_flatten_bakes_marks_keeps_other_comments_and_does_not_reuse_pcfl0() {
    use pdfcraft_annot::{FillMark, Markup, Meta, NewAnnotation, Shape, Style, add_annotation, summaries};
    let mut doc = fixture();
    assert_eq!(flatten_fill_sign(&mut doc, &[0]).unwrap(), 0);
    assert!(!doc.is_modified(), "nothing to flatten leaves the file alone");

    let meta = Meta { date: None, id: "x".into() };
    let add = |doc: &mut Document, shape: Shape, contents: &str| {
        let style = Style::default_for(&shape);
        add_annotation(doc, &NewAnnotation { page: 0, shape, style, contents: contents.into(), author: "a".into() }, &meta).unwrap()
    };
    add(&mut doc, Shape::Rectangle { rect: [10.0, 10.0, 80.0, 40.0] }, "box");
    assert_eq!(flatten(&mut doc, &[0], true, false).unwrap(), 1);
    let kept = xobject(&doc, 0, b"PCFl0").expect("the first flatten's XObject");

    add(&mut doc, Shape::Typewriter { rect: [20.0, 500.0, 140.0, 520.0], font_size: 10.0 }, "Hello");
    doc.update_dict(annot_at(&doc, 0, 0), |d| {
        d.remove(b"PCFillSign");
    })
    .unwrap();
    add(&mut doc, Shape::Mark { rect: [100.0, 100.0, 120.0, 120.0], mark: FillMark::Check }, "");
    add(&mut doc, Shape::Signature { strokes: vec![vec![[10.0, 50.0], [40.0, 80.0], [70.0, 50.0]]] }, "");
    doc.update_dict(annot_at(&doc, 0, 2), |d| d.set(b"Subj".to_vec(), pdfcraft_cos::PdfString::text("Pencil"))).unwrap();
    add(&mut doc, Shape::TypedSignature { rect: [20.0, 40.0, 90.0, 70.0], contours: vec![vec![[0.1, 0.2], [0.5, 0.9], [0.9, 0.2]]] }, "");
    add(&mut doc, Shape::TextMarkup { kind: Markup::Highlight, quads: vec![[20.0, 400.0, 80.0, 400.0, 20.0, 390.0, 80.0, 390.0]] }, "keep");
    add(&mut doc, Shape::TextBox { rect: [20.0, 300.0, 140.0, 340.0], font_size: 12.0 }, "Note");
    add(&mut doc, Shape::Ink { strokes: vec![vec![[200.0, 200.0], [220.0, 220.0], [240.0, 200.0]]] }, "pencil");
    let old = add(&mut doc, Shape::Signature { strokes: vec![vec![[300.0, 50.0], [330.0, 80.0], [360.0, 50.0]]] }, "");
    doc.update_dict(annot_at(&doc, 0, old), |d| {
        d.remove(b"PCFillSign");
    })
    .unwrap();
    let hidden = add(&mut doc, Shape::Typewriter { rect: [20.0, 600.0, 140.0, 620.0], font_size: 10.0 }, "secret");
    doc.update_dict(annot_at(&doc, 0, hidden), |d| d.set(b"F".to_vec(), Object::Int(2))).unwrap();

    let n = flatten_fill_sign(&mut doc, &[0]).unwrap();
    assert_eq!(n, 5, "text, check, both signatures and the typed signature; the hidden one stays");
    assert_eq!(xobject(&doc, 0, b"PCFl0"), Some(kept), "an existing PCFl0 is not replaced");
    let newest = streams(&doc, 0).last().unwrap().clone();
    assert!(newest.contains("/PCFl1 Do") && !newest.contains("/PCFl0 Do"), "{newest}");

    let doc = reopen(&doc);
    let mut left: Vec<String> = summaries(&doc).iter().map(|s| s.contents.clone().unwrap_or_default()).collect();
    left.sort();
    assert_eq!(left, ["Note", "keep", "pencil", "secret"]);
    assert!(summaries(&doc).iter().any(|s| s.intent.as_deref() == Some("FreeTextTypeWriter")), "the hidden typewriter stays");
    assert_eq!(summaries(&doc).iter().filter(|s| s.intent.as_deref() == Some("FreeTextTypeWriter")).count(), 1);
}

/// A one-page document from its objects (object 1 is the catalog).
fn build(objs: &[&str]) -> Document {
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

fn stream(dict: &str, data: &str) -> String {
    format!("<< {dict} /Length {} >>\nstream\n{data}\nendstream", data.len())
}

/// #314: text and images drawn by (nested) form XObjects are read for export, placed through
/// every form matrix and set in each form's own fonts; editing still sees only the page's text.
#[test]
fn reading_follows_form_xobjects() {
    let page = stream("", "q 1 0 0 1 10 0 cm /Fm1 Do Q BT /F1 12 Tf 72 100 Td (Page text) Tj ET");
    let fm1 = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 600 800] /Matrix [1 0 0 1 0 50] /Resources << /Font << /F1 7 0 R >> /XObject << /Fm2 8 0 R >> >>",
        "BT /F1 18 Tf 72 700 Td (Hello from a test invoice) Tj ET /Fm2 Do",
    );
    let fm2 = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 300 400] /Matrix [2 0 0 2 0 0] /Resources << /Font << /F2 9 0 R >> /XObject << /Im1 10 0 R >> >>",
        "BT /F2 10 Tf 50 100 Td (Nested) Tj ET q 20 0 0 10 5 5 cm /Im1 Do Q",
    );
    let image = stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", "A");
    let doc = build(&[
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 6 0 R >> >> /Contents 4 0 R >>",
        &page,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>",
        &fm1,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        &fm2,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Bold >>",
        &image,
    ]);
    let close = |a: f64, b: f64| (a - b).abs() < 0.01;

    let blocks = text::reading_blocks(&doc, 0).unwrap();
    let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(texts, ["Hello from a test invoice", "Nested", "Page text"]);
    let (hello, nested, own) = (&blocks[0], &blocks[1], &blocks[2]);
    // Hello: (72, 700) moved by the form's (0, 50) and the page's (10, 0).
    assert_eq!(hello.base_font, "Helvetica", "the form's own /F1, not the page's");
    assert!(close(hello.rect[0], 82.0) && close(hello.size, 18.0), "{hello:?}");
    // Nested: (50, 100) doubled by Fm2, then Fm1 and the page: (110, 250), 20 pt.
    assert_eq!(nested.base_font, "Times-Bold");
    assert!(nested.bold);
    assert!(close(nested.rect[0], 110.0) && close(nested.size, 20.0), "{nested:?}");
    assert_eq!(own.base_font, "Courier");

    let images = images::reading_images(&doc, 0).unwrap();
    assert_eq!(images.len(), 1, "{images:?}");
    let r = images[0].rect;
    assert!(close(r[0], 20.0) && close(r[1], 60.0) && close(r[2], 60.0) && close(r[3], 80.0), "{r:?}");
    assert_eq!((images[0].width, images[0].height), (1, 1));

    // Editing works on the page's own streams only, unchanged.
    let editable: Vec<String> = text::text_blocks(&doc, 0).unwrap().into_iter().map(|b| b.text).collect();
    assert_eq!(editable, ["Page text"]);
    // The page draws one figure of its own (Fm1, edited as a whole); the image inside Fm2 is not
    // a page image.
    let own = images::page_images(&doc, 0).unwrap();
    assert_eq!(own.len(), 1, "{own:?}");
    assert!(own[0].is_form && own[0].object == Some(pdfcraft_cos::ObjRef::new(6, 0)), "{own:?}");
}

/// A chain of forms, each drawing the next twenty times, twelve deep: within the depth cap but
/// 20^11 visits without a budget. Reading stops at the page's visit budget instead of hanging.
#[test]
fn form_xobjects_that_fan_out_stop_at_the_visit_budget() {
    let font = "/Font << /F1 5 0 R >>";
    let levels = 12;
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << /Font << /F1 5 0 R >> /XObject << /N 6 0 R >> >> /Contents 4 0 R >>"
            .to_string(),
        stream("", "/N Do"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    for level in 0..levels {
        let next = 7 + level;
        objs.push(if level + 1 == levels {
            stream(&format!("/Subtype /Form /BBox [0 0 9 9] /Resources << {font} >>"), "BT /F1 10 Tf 10 10 Td (leaf) Tj ET")
        } else {
            stream(&format!("/Subtype /Form /BBox [0 0 9 9] /Resources << {font} /XObject << /N {next} 0 R >> >>"), &"/N Do ".repeat(20))
        });
    }
    let refs: Vec<&str> = objs.iter().map(String::as_str).collect();
    let doc = build(&refs);
    let start = std::time::Instant::now();
    let _ = text::reading_blocks(&doc, 0).unwrap();
    assert!(images::reading_images(&doc, 0).unwrap().is_empty());
    assert!(start.elapsed() < std::time::Duration::from_secs(30), "took {:?}", start.elapsed());
}

/// Forms that draw themselves or each other, have no resources, a malformed matrix, or are
/// missing are read once (or skipped) without panicking or looping.
#[test]
fn hostile_form_xobjects_are_read_once() {
    let page = stream("", "/Loop Do /A Do /NoRes Do /Bad Do /Missing Do /F1 Do");
    let font = "/Font << /F1 5 0 R >>";
    let lp = stream(
        &format!("/Subtype /Form /BBox [0 0 9 9] /Resources << {font} /XObject << /Loop 6 0 R >> >>"),
        "/Loop Do BT /F1 10 Tf 10 10 Td (once) Tj ET",
    );
    let a = stream(
        &format!("/Subtype /Form /BBox [0 0 9 9] /Resources << {font} /XObject << /B 8 0 R >> >>"),
        "/B Do BT /F1 10 Tf 10 100 Td (from a) Tj ET",
    );
    let b = stream(
        &format!("/Subtype /Form /BBox [0 0 9 9] /Resources << {font} /XObject << /A 7 0 R >> >>"),
        "/A Do BT /F1 10 Tf 10 200 Td (from b) Tj ET",
    );
    let nores = stream("/Subtype /Form /BBox [0 0 9 9]", "BT /F1 10 Tf 10 300 Td (inherited) Tj ET");
    let bad = stream(&format!("/Subtype /Form /BBox [0 0 9 9] /Matrix [1 0 0] /Resources << {font} >>"), "BT /F1 10 Tf 10 400 Td (bad matrix) Tj ET");
    let doc = build(&[
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << /Font << /F1 5 0 R >> /XObject << /Loop 6 0 R /A 7 0 R /NoRes 9 0 R /Bad 10 0 R >> >> /Contents 4 0 R >>",
        &page,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        &lp,
        &a,
        &b,
        &nores,
        &bad,
    ]);
    let texts: Vec<String> = text::reading_blocks(&doc, 0).unwrap().into_iter().map(|b| b.text).collect();
    assert_eq!(texts, ["once", "from b", "from a", "inherited", "bad matrix"]);
    assert!(images::reading_images(&doc, 0).unwrap().is_empty());
}

// --- M7 (WP5): whole-paragraph edits through the run-preserving path ---

#[test]
fn whole_paragraph_edits_keep_untouched_codes_tj_kerning_and_repeated_spaces() {
    // Kerned TJ pairs, a run joined from a following Tj, and doubled spaces.
    let source = "BT /F1 12 Tf 1 0 0 1 72 700 Tm [(AV) -150 (AW) 80 (ay)] TJ (  double  gap end) Tj ET";
    let mut doc = text_page(source);
    let before_x = shown_x(&doc);
    let block = text::text_blocks(&doc, 0).unwrap().remove(0);
    assert_eq!(block.text, "AVAWay  double  gap end");
    let fonts_before = text::page_fonts(&doc, 0, "").unwrap();
    let edited = "AVAWay  double  gap finish";
    assert_eq!(text::replace_block(&mut doc, 0, 0, edited).unwrap().substituted, None);
    let doc = reopen(&doc);
    assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, edited, "repeated spaces survive");
    // One show in the source font: the original codes, and the source kerning written back as
    // the same TJ numbers (not re-encoded from Unicode, not one placement per glyph).
    assert_eq!(shows(&doc), [(edited.as_bytes().to_vec(), vec![-150.0, 80.0])]);
    // Every untouched glyph starts exactly where it did.
    let untouched = "AVAWay  double  gap ".len();
    let after_x = shown_x(&doc);
    for (i, (a, b)) in before_x.iter().zip(&after_x).take(untouched).enumerate() {
        assert!((a - b).abs() < 1e-3, "glyph {i} moved from {a} to {b}");
    }
    assert_eq!(after_x.len(), edited.len());
    // No font resource is added or replaced for text the source font can show.
    assert_eq!(text::page_fonts(&doc, 0, "").unwrap(), fonts_before);
}

#[test]
fn whole_paragraph_edits_keep_codes_that_unicode_would_re_encode_differently() {
    // Code 0x41 shows "fi" (a ligature glyph); re-encoding "fi" from Unicode would draw two
    // glyphs, or substitute the face. The untouched ligature keeps its single source code.
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (AB) Tj ET");
    let cmap = doc.add(Object::Stream(Stream::flate(
        Dict::new(),
        b"1 begincodespacerange <00> <ff> endcodespacerange 2 beginbfchar <41> <00660069> <42> <0058> endbfchar",
    )));
    doc.update_dict(pdfcraft_cos::ObjRef::new(5, 0), |d| d.set(b"ToUnicode".to_vec(), Object::Ref(cmap))).unwrap();
    assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, "fiX");
    text::replace_block(&mut doc, 0, 0, "fiX and more").unwrap();
    let doc = reopen(&doc);
    assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, "fiX and more");
    let shown = shows(&doc);
    assert!(shown.first().is_some_and(|(bytes, _)| bytes.starts_with(b"AB")), "{shown:?}");
}

#[test]
fn whole_paragraph_reflow_keeps_leading_scales_it_with_size_and_wraps_cleanly() {
    let para = "BT /F1 10 Tf 14 TL 100 700 Td (One two three four five six seven) Tj T* (eight nine ten eleven twelve) Tj ET";
    let mut doc = text_page(para);
    let width = {
        let b = &text::text_blocks(&doc, 0).unwrap()[0];
        b.rect[2] - b.rect[0]
    };
    let text = "One two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen";
    text::replace_block(&mut doc, 0, 0, text).unwrap();
    let doc = reopen(&doc);
    let block = text::text_blocks(&doc, 0).unwrap().remove(0);
    assert_eq!(block.text, text);
    assert!(block.line_count >= 3, "{block:?}");
    let lines = text::text_lines(&doc, 0).unwrap();
    let rows: Vec<&text::TextLine> = block.lines.iter().map(|i| &lines[*i]).collect();
    for pair in rows.windows(2) {
        assert!((pair[0].origin_baseline() - pair[1].origin_baseline() - 14.0).abs() < 0.01, "the source leading is kept");
    }
    for row in &rows {
        assert!(row.rect[2] - row.rect[0] <= width + 0.01, "{:?} fits {width}", row.text);
        assert!((row.rect[0] - 100.0).abs() < 1e-6, "{:?} starts at the left edge", row.text);
    }
    // The space a row wraps at is not drawn (the reader joins rows with one space again).
    assert!(rows.iter().all(|r| !r.text.ends_with(' ')), "{:?}", rows.iter().map(|r| &r.text).collect::<Vec<_>>());
    // Doubling the size doubles the line spacing; the words stay the same.
    let mut doc = doc;
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { size: Some(20.0), ..Default::default() }).unwrap();
    let doc = reopen(&doc);
    let block = text::text_blocks(&doc, 0).unwrap().remove(0);
    assert_eq!(block.text, text);
    let lines = text::text_lines(&doc, 0).unwrap();
    let (a, b) = (&lines[block.lines[0]], &lines[block.lines[1]]);
    assert!((a.origin_baseline() - b.origin_baseline() - 28.0).abs() < 0.01, "{} {}", a.origin_baseline(), b.origin_baseline());
    // A single line with a TL reflows at that TL when it has to wrap.
    let mut doc = text_page("BT /F1 10 Tf 16 TL 100 700 Td (Short) Tj ET");
    text::rewrite_block(&mut doc, 0, 0, Some("Short text that now wraps"), &text::BlockStyle { width: Some(60.0), ..Default::default() }).unwrap();
    let lines = text::text_lines(&reopen(&doc), 0).unwrap();
    assert!(lines.len() > 1 && (lines[0].origin_baseline() - lines[1].origin_baseline() - 16.0).abs() < 0.01, "{lines:?}");
}

#[test]
fn whole_paragraph_edits_remove_only_exact_fake_bold_copies() {
    // A fake-bold copy (same codes, state and nearly the same place), a red shadow at the same
    // spot, and an unrelated label drawn over it.
    let source = "BT /F1 12 Tf 72 700 Td (Hello) Tj ET BT /F1 12 Tf 72.3 700 Td (Hello) Tj ET \
                  q BT 1 0 0 rg /F1 12 Tf 72.5 699.5 Td (Hello) Tj ET Q BT /F1 12 Tf 72 700 Td (Label) Tj ET";
    let mut doc = text_page(source);
    assert_eq!(text::text_blocks(&doc, 0).unwrap().len(), 4);
    text::replace_block(&mut doc, 0, 0, "Hello world").unwrap();
    let doc = reopen(&doc);
    let blocks = text::text_blocks(&doc, 0).unwrap();
    let texts: Vec<(&str, [f64; 3])> = blocks.iter().map(|b| (b.text.as_str(), b.color)).collect();
    assert_eq!(texts, [("Hello world", [0.0; 3]), ("Hello", [1.0, 0.0, 0.0]), ("Label", [0.0; 3])], "only the exact copy goes");
    // A selected-range edit never removes a copy, exact or not.
    let mut doc = text_page(source);
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([0, 1]), color: Some([0.0, 0.0, 1.0]), ..Default::default() }).unwrap();
    assert_eq!(text::text_blocks(&reopen(&doc), 0).unwrap().len(), 4);
    // Nor does a whole edit of a paragraph whose runs differ in style.
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Hi) Tj 2 Tc (Yo) Tj ET BT /F1 12 Tf 72 700 Td (Hi) Tj ET");
    text::replace_block(&mut doc, 0, 0, "Hi there").unwrap();
    let texts: Vec<String> = text::text_blocks(&reopen(&doc), 0).unwrap().into_iter().map(|b| b.text).collect();
    assert_eq!(texts, ["Hi there", "Hi"]);
}

#[test]
fn whole_paragraph_text_edits_keep_underline_associations_of_untouched_words() {
    let mut doc = text_page("BT /F1 12 Tf 72 700 Td (Before abba after.) Tj ET");
    text::rewrite_block(&mut doc, 0, 0, None, &text::BlockStyle { range: Some([7, 11]), underline: Some(true), ..Default::default() }).unwrap();
    let mut doc = reopen(&doc);
    let before: Vec<f64> = underline_associations(&doc).iter().map(|a| a.tm[4]).collect();
    assert_eq!(before.len(), 4);
    text::replace_block(&mut doc, 0, 0, "Before abba later!").unwrap();
    let doc = reopen(&doc);
    let block = text::text_blocks(&doc, 0).unwrap().remove(0);
    assert_eq!(block.text, "Before abba later!");
    let underlined: String = block.runs.iter().filter(|r| r.underline).flat_map(|r| block.text.chars().skip(r.start).take(r.end - r.start)).collect();
    assert_eq!(underlined, "abba");
    let after = underline_associations(&doc);
    assert_eq!(after.iter().map(|a| a.bytes.clone()).collect::<Vec<_>>(), [b"a".to_vec(), b"b".to_vec(), b"b".to_vec(), b"a".to_vec()]);
    for (a, b) in before.iter().zip(after.iter().map(|a| a.tm[4])) {
        assert!((a - b).abs() < 1e-3, "{before:?} -> {after:?}");
    }
}

#[test]
fn whole_paragraph_edits_leave_shared_resources_alone_and_transformed_text_on_the_legacy_path() {
    // Page 0 inherits its resources; reusing its own font must not copy them onto the page.
    let mut doc = fixture();
    text::replace_block(&mut doc, 0, 0, "Hi  there").unwrap();
    let doc = reopen(&doc);
    assert!(doc.get(pdfcraft_cos::ObjRef::new(3, 0)).as_dict().unwrap().get(b"Resources").is_none());
    assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, "Hi  there");
    // Rotated text goes through the run path (typed repeated spaces survive). Skewed text
    // cannot be placed by it yet: whole edits keep working through the legacy re-encoding
    // (which still normalizes whitespace), selected ranges refuse.
    let mut rotated = text_page("BT /F1 12 Tf 0 1 -1 0 100 200 Tm (Rotated) Tj ET");
    text::replace_block(&mut rotated, 0, 0, "Turned  text").unwrap();
    assert_eq!(text::text_blocks(&reopen(&rotated), 0).unwrap()[0].text, "Turned  text");
    let mut skewed = text_page("BT /F1 12 Tf 1 0 0.3 1 100 200 Tm (Slanted) Tj ET");
    text::replace_block(&mut skewed, 0, 0, "Slanted  text").unwrap();
    assert_eq!(text::text_blocks(&reopen(&skewed), 0).unwrap()[0].text, "Slanted text");
}

#[test]
fn repeated_spaces_at_a_wrap_point_read_back_unchanged() {
    let mut doc = text_page("BT /F1 10 Tf 100 700 Td (Alpha beta) Tj ET");
    for text in ["Alpha  beta  gamma", "Alpha beta gamma", "Alpha   beta gamma"] {
        text::rewrite_block(&mut doc, 0, 0, Some(text), &text::BlockStyle { width: Some(40.0), ..Default::default() }).unwrap();
        doc = reopen(&doc);
        let block = text::text_blocks(&doc, 0).unwrap().remove(0);
        assert!(block.line_count > 1, "{block:?}");
        assert_eq!(block.text, text);
    }
}

#[test]
fn whole_paragraph_edits_keep_the_source_rows_they_do_not_reach() {
    // Three source rows; row 2 is not greedy-full (row 3 starts with a word that would fit).
    let para = "BT /F1 10 Tf 12 TL 100 700 Td (One two three four five six seven) Tj T* (eight nine) Tj T* (ten eleven twelve) Tj ET";
    let rows_of = |doc: &Document| -> Vec<(String, f64, f64)> {
        let lines = text::text_lines(doc, 0).unwrap();
        let block = text::text_blocks(doc, 0).unwrap().remove(0);
        block.lines.iter().map(|i| (lines[*i].text.clone(), lines[*i].rect[0], lines[*i].origin_baseline())).collect()
    };
    let source = rows_of(&text_page(para));
    assert_eq!(source.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["One two three four five six seven", "eight nine", "ten eleven twelve"]);
    for (text, rows) in [
        // A word changed inside row 2: every row keeps its words and place.
        (
            "One two three four five six seven eight NINE ten eleven twelve",
            vec!["One two three four five six seven", "eight NINE", "ten eleven twelve"],
        ),
        // A deletion does not pull the next row up.
        ("One two three four five six seven eight ten eleven twelve", vec!["One two three four five six seven", "eight", "ten eleven twelve"]),
        // Text that no longer fits wraps; the rows after it keep their words.
        (
            "One two three four five six seven eight nine and a much longer insertion ten eleven twelve",
            vec!["One two three four five six seven", "eight nine and a much longer", "insertion", "ten eleven twelve"],
        ),
    ] {
        let mut doc = text_page(para);
        text::replace_block(&mut doc, 0, 0, text).unwrap();
        let doc = reopen(&doc);
        assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, text);
        let after = rows_of(&doc);
        assert_eq!(after.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), rows, "{text}");
        assert_eq!(after[0], source[0], "the first row is untouched");
        let last = after.last().unwrap();
        assert!((last.1 - 100.0).abs() < 1e-6, "{last:?}");
        assert!((source[1].2 - after[1].2).abs() < 1e-6, "rows stay on the source leading");
    }
    // Removing the space that joins two rows joins their words: no break is forced there.
    let mut doc = text_page(para);
    let joined = "One two three four five six seveneight nine ten eleven twelve";
    text::replace_block(&mut doc, 0, 0, joined).unwrap();
    assert_eq!(text::text_blocks(&reopen(&doc), 0).unwrap()[0].text, joined);
}

/// A glyph shown on page 0: its code, where its origin lands in user space (rise included),
/// the linear part of its text rendering matrix, and its fill operator.
#[derive(Clone, Debug, PartialEq)]
struct Placed {
    byte: u8,
    at: [f64; 2],
    trm: [f64; 4],
    fill: String,
}

/// An independent interpreter for the fixtures' Helvetica (WinAnsi) text: q/Q, cm, the text
/// state operators, Td/TD/Tm/T*, Tj/TJ/'. It finds where every glyph lands in user space,
/// whatever the text or graphics transform.
fn placed_glyphs(doc: &Document) -> Vec<Placed> {
    use pdfcraft_content::Matrix;
    #[derive(Clone)]
    struct State {
        ctm: Matrix,
        size: f64,
        tc: f64,
        tw: f64,
        tz: f64,
        tl: f64,
        rise: f64,
        fill: String,
    }
    let mut gs = State { ctm: Matrix::IDENTITY, size: 0.0, tc: 0.0, tw: 0.0, tz: 1.0, tl: 0.0, rise: 0.0, fill: "[0.0] g".into() };
    let mut stack = Vec::new();
    let (mut tm, mut tlm) = (Matrix::IDENTITY, Matrix::IDENTITY);
    let mut out = Vec::new();
    for op in pdfcraft_content::parse(&page_content_bytes(doc, 0)).ops {
        let name = String::from_utf8_lossy(&op.op).into_owned();
        match name.as_str() {
            "q" => stack.push(gs.clone()),
            "Q" => gs = stack.pop().unwrap(),
            "cm" => gs.ctm = Matrix(op.nums::<6>().unwrap()).then(&gs.ctm),
            "BT" => (tm, tlm) = (Matrix::IDENTITY, Matrix::IDENTITY),
            "g" | "rg" | "k" => gs.fill = format!("{:?} {name}", op.operands.iter().map(|o| o.as_f64().unwrap()).collect::<Vec<_>>()),
            "Tf" => gs.size = op.num(1).unwrap(),
            "Tc" => gs.tc = op.num(0).unwrap(),
            "Tw" => gs.tw = op.num(0).unwrap(),
            "Tz" => gs.tz = op.num(0).unwrap() / 100.0,
            "TL" => gs.tl = op.num(0).unwrap(),
            "Ts" => gs.rise = op.num(0).unwrap(),
            "Td" | "TD" => {
                let [x, y] = op.nums::<2>().unwrap();
                if name == "TD" {
                    gs.tl = -y;
                }
                tlm = Matrix::translate(x, y).then(&tlm);
                tm = tlm;
            }
            "Tm" => (tm, tlm) = (Matrix(op.nums::<6>().unwrap()), Matrix(op.nums::<6>().unwrap())),
            "T*" => {
                tlm = Matrix::translate(0.0, -gs.tl).then(&tlm);
                tm = tlm;
            }
            "Tj" | "TJ" | "'" => {
                if name == "'" {
                    tlm = Matrix::translate(0.0, -gs.tl).then(&tlm);
                    tm = tlm;
                }
                let parts = match op.operands.last() {
                    Some(Object::Array(parts)) => parts.clone(),
                    other => other.cloned().into_iter().collect(),
                };
                for part in parts {
                    match part {
                        Object::String(s) => {
                            for byte in s.bytes {
                                let trm = tm.then(&gs.ctm);
                                let (x, y) = trm.apply(0.0, gs.rise);
                                out.push(Placed { byte, at: [x, y], trm: [trm.0[0], trm.0[1], trm.0[2], trm.0[3]], fill: gs.fill.clone() });
                                let w = pdfcraft_fonts::standard_text_width("Helvetica", &char::from(byte).to_string(), gs.size).unwrap();
                                let advance = (w + gs.tc + if byte == b' ' { gs.tw } else { 0.0 }) * gs.tz;
                                tm = Matrix::translate(advance, 0.0).then(&tm);
                            }
                        }
                        number => tm = Matrix::translate(-number.as_f64().unwrap() / 1000.0 * gs.size * gs.tz, 0.0).then(&tm),
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Glyphs `indexes` of `after` are where the same glyphs of `before` were: same code, same place
/// (to a thousandth of a point), same orientation and scale.
fn assert_in_place(before: &[Placed], after: &[Placed], indexes: impl IntoIterator<Item = usize>, label: &str) {
    for i in indexes {
        let (a, b) = (&before[i], &after[i]);
        assert_eq!(a.byte, b.byte, "{label}: glyph {i} changed code");
        assert!(a.at.iter().zip(&b.at).all(|(p, q)| (p - q).abs() < 1e-3), "{label}: glyph {i} moved from {:?} to {:?}", a.at, b.at);
        assert!(a.trm.iter().zip(&b.trm).all(|(p, q)| (p - q).abs() < 1e-6), "{label}: glyph {i} turned or scaled: {a:?} {b:?}");
    }
}

/// Rotations by quarter turns and arbitrary angles, through the text matrix, the CTM or both,
/// with different horizontal and vertical scales along the text's own axes:
/// (label, cm, Tm, vertical scale).
const TURNED: &[(&str, &str, &str, f64)] = &[
    ("90° Tm", "1 0 0 1 0 0", "0 1 -1 0 300 200", 1.0),
    ("180° Tm", "1 0 0 1 0 0", "-1 0 0 -1 450 600", 1.0),
    ("270° Tm", "1 0 0 1 0 0", "0 -1 1 0 200 650", 1.0),
    ("30° CTM", "0.866025 0.5 -0.5 0.866025 150 200", "1 0 0 1 10 20", 1.0),
    ("30° CTM, 2:1 Tm scale", "0.866025 0.5 -0.5 0.866025 100 150", "2 0 0 1 10 20", 1.0),
    ("-45° Tm, 90° CTM, 1:1.5 scale", "0 1 -1 0 500 100", "0.707107 -0.707107 1.06066 1.06066 100 50", 1.5),
];

#[test]
fn turned_paragraph_edits_keep_untouched_glyphs_codes_kerning_and_gaps() {
    for (label, cm, tm, _) in TURNED {
        // Kerned TJ pairs, a run joined from a following Tj, doubled spaces, and a word placed
        // by its own Td (a gap the reader turns into a code-less space).
        let source = format!("q {cm} cm BT /F1 12 Tf {tm} Tm [(AV) -150 (AW) 80 (ay)] TJ (  double  gap) Tj 130 0 Td (end) Tj ET Q");
        let mut doc = text_page(&source);
        let before = placed_glyphs(&doc);
        let blocks = text::text_blocks(&doc, 0).unwrap();
        assert_eq!(blocks.len(), 1, "{label}: {blocks:?}");
        assert_eq!(blocks[0].text, "AVAWay  double  gap end", "{label}");
        let edited = "AVAWay  double  gap fin";
        assert_eq!(text::replace_block(&mut doc, 0, 0, edited).unwrap().substituted, None, "{label}");
        let doc = reopen(&doc);
        assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, edited, "{label}: repeated spaces survive");
        let after = placed_glyphs(&doc);
        assert_eq!(after.len(), before.len(), "{label}");
        // Every untouched glyph, and the first replaced one, starts exactly where it did.
        let untouched = "AVAWay  double  gap".len();
        assert_in_place(&before, &after, 0..untouched, label);
        assert!(before[untouched].at.iter().zip(&after[untouched].at).all(|(p, q)| (p - q).abs() < 1e-3), "{label}: the new word moved");
        assert_eq!(after[untouched..].iter().map(|g| g.byte).collect::<Vec<_>>(), b"fin");
        // The source kerning is written back as the same TJ numbers.
        let shown = shows(&doc);
        assert_eq!(shown[0], (b"AVAWay  double  gap".to_vec(), vec![-150.0, 80.0]), "{label}: {shown:?}");
    }
}

#[test]
fn turned_paragraphs_group_rows_and_keep_the_rows_an_edit_does_not_reach() {
    for (label, cm, tm, vertical) in TURNED {
        let para =
            format!("q {cm} cm BT /F1 10 Tf 12 TL {tm} Tm (One two three four five six seven) Tj T* (eight nine) Tj T* (ten eleven twelve) Tj ET Q");
        let doc = text_page(&para);
        let block = text::text_blocks(&doc, 0).unwrap().remove(0);
        assert_eq!(block.text, "One two three four five six seven eight nine ten eleven twelve", "{label}");
        assert_eq!(block.line_count, 3, "{label}");
        // Baselines are measured along the text's own up direction, in user-space units.
        let lines = text::text_lines(&doc, 0).unwrap();
        assert!((lines[0].origin_baseline() - lines[1].origin_baseline() - 12.0 * vertical).abs() < 1e-3, "{label}: {lines:?}");
        let before = placed_glyphs(&doc);
        // A same-length change: every other glyph keeps its index and its place.
        let mut changed = doc.clone();
        let text = "One two three four five six seven eight NINE ten eleven twelve";
        text::replace_block(&mut changed, 0, 0, text).unwrap();
        let changed = reopen(&changed);
        let block = text::text_blocks(&changed, 0).unwrap().remove(0);
        assert_eq!((block.text.as_str(), block.line_count), (text, 3), "{label}");
        let after = placed_glyphs(&changed);
        assert_eq!(after.len(), before.len(), "{label}");
        let nine = "One two three four five six seven".len() + "eight ".len();
        assert_in_place(&before, &after, (0..before.len()).filter(|i| !(nine..nine + 4).contains(i)), label);
        // Text that no longer fits wraps along the text's own axes; later rows keep their words.
        let mut reflowed = doc.clone();
        let text = "One two three four five six seven eight nine and a much longer insertion ten eleven twelve";
        text::replace_block(&mut reflowed, 0, 0, text).unwrap();
        let reflowed = reopen(&reflowed);
        assert_eq!(text::text_blocks(&reflowed, 0).unwrap()[0].text, text, "{label}");
        let rows: Vec<String> = text::text_lines(&reflowed, 0).unwrap().into_iter().map(|l| l.text).collect();
        assert_eq!(rows, ["One two three four five six seven", "eight nine and a much longer", "insertion", "ten eleven twelve"], "{label}");
        let after = placed_glyphs(&reflowed);
        assert_in_place(&before, &after, 0.."One two three four five six seven".len(), label);
        // Each row starts one line further along the text's own down direction.
        let t = before[0].trm;
        for (row, first) in b"eit".iter().copied().enumerate() {
            let steps = (row + 1) as f64 * 12.0;
            let want = [before[0].at[0] - t[2] * steps, before[0].at[1] - t[3] * steps];
            assert!(
                after.iter().any(|g| g.byte == first && (g.at[0] - want[0]).abs() < 1e-3 && (g.at[1] - want[1]).abs() < 1e-3),
                "{label}: row {} does not start at {want:?}",
                row + 1
            );
        }
    }
}

#[test]
fn turned_selections_recolor_and_underline_in_the_text_frame() {
    for (label, cm, tm, _) in TURNED {
        let mut doc = text_page(&format!("q {cm} cm BT /F1 12 Tf {tm} Tm (Before abba after.) Tj ET Q"));
        let before = placed_glyphs(&doc);
        let style = text::BlockStyle { range: Some([7, 11]), underline: Some(true), color: Some([1.0, 0.0, 0.0]), ..Default::default() };
        text::rewrite_block(&mut doc, 0, 0, None, &style).unwrap();
        let doc = reopen(&doc);
        let block = text::text_blocks(&doc, 0).unwrap().remove(0);
        assert_eq!(block.text, "Before abba after.", "{label}");
        let underlined: String =
            block.runs.iter().filter(|r| r.underline).flat_map(|r| block.text.chars().skip(r.start).take(r.end - r.start)).collect();
        assert_eq!(underlined, "abba", "{label}");
        assert!(block.runs.iter().filter(|r| r.underline).all(|r| r.color == [1.0, 0.0, 0.0]), "{label}");
        // Every glyph, selected or not, stays where it was; only the selection changes colour.
        let after = placed_glyphs(&doc);
        assert_eq!(after.len(), before.len(), "{label}");
        assert_in_place(&before, &after, 0..before.len(), label);
        for (i, (a, b)) in before.iter().zip(&after).enumerate() {
            assert_eq!(a.fill == b.fill, !(7..11).contains(&i), "{label}: glyph {i} {a:?} {b:?}");
        }
        // Each underline is drawn in its glyph's own (turned) text space, just below it.
        let associations = underline_associations(&doc);
        assert_eq!(associations.iter().map(|a| a.bytes.clone()).collect::<Vec<_>>(), [b"a".to_vec(), b"b".to_vec(), b"b".to_vec(), b"a".to_vec()]);
    }
}

#[test]
fn turned_runs_keep_their_own_rises() {
    for (label, cm, tm, _) in TURNED {
        let mut doc = text_page(&format!("q {cm} cm BT /F1 12 Tf {tm} Tm (Base ) Tj 0.8 Ts (up) Tj -0.8 Ts (down) Tj 0 Ts ( tail) Tj ET Q"));
        assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, "Base updown tail", "{label}");
        let before = placed_glyphs(&doc);
        text::replace_block(&mut doc, 0, 0, "Base updown end").unwrap();
        let doc = reopen(&doc);
        assert_eq!(text::text_blocks(&doc, 0).unwrap()[0].text, "Base updown end", "{label}");
        let after = placed_glyphs(&doc);
        assert_in_place(&before, &after, 0.."Base updown ".len(), label);
    }
}

#[test]
fn a_turned_single_line_grows_towards_the_margin_it_runs_to() {
    // About 450 pt of Helvetica 12.
    let mut text = String::from("Start");
    while pdfcraft_fonts::standard_text_width("Helvetica", &text, 12.0).unwrap() < 450.0 {
        text.push_str(" word");
    }
    // Running up from y = 200 on an 800 pt page there are 564 pt before the margin: one row.
    let mut up = text_page("BT /F1 12 Tf 0 1 -1 0 300 200 Tm (Start) Tj ET");
    text::replace_block(&mut up, 0, 0, &text).unwrap();
    assert_eq!(text::text_blocks(&reopen(&up), 0).unwrap()[0].line_count, 1);
    // Running left from x = 450 there are 414 pt: it wraps.
    let mut left = text_page("BT /F1 12 Tf -1 0 0 -1 450 600 Tm (Start) Tj ET");
    text::replace_block(&mut left, 0, 0, &text).unwrap();
    let block = text::text_blocks(&reopen(&left), 0).unwrap().remove(0);
    assert_eq!((block.text.as_str(), block.line_count), (text.as_str(), 2));
}

#[test]
fn skewing_transforms_refuse_selections_and_keep_whole_edits_on_the_legacy_path() {
    // A nonuniform CTM scale after a rotation skews the text axes; so does a skewed Tm.
    for source in [
        "q 2 0 0 1 0 0 cm BT /F1 12 Tf 0.866025 0.5 -0.5 0.866025 100 100 Tm (Skewed by scale) Tj ET Q",
        "BT /F1 12 Tf 1 0.2 0 1 100 100 Tm (Skewed by scale) Tj ET",
    ] {
        let mut doc = text_page(source);
        let before = write_incremental(&doc, &SaveOptions::default()).unwrap();
        let style = text::BlockStyle { range: Some([0, 6]), bold: Some(true), ..Default::default() };
        assert!(text::rewrite_block(&mut doc, 0, 0, None, &style).is_err(), "{source}");
        assert_eq!(write_incremental(&doc, &SaveOptions::default()).unwrap(), before, "refused atomically");
        text::replace_block(&mut doc, 0, 0, "Still  editable").unwrap();
        assert_eq!(text::text_blocks(&reopen(&doc), 0).unwrap()[0].text, "Still editable", "{source}");
    }
}

#[test]
fn bates_numbers_are_kept_and_removed_apart_from_headers_and_footers() {
    let mut doc = fixture();
    let original = streams(&doc, 0);
    let mut header = HeaderFooter::default();
    header.text[1] = "Confidential".into();
    add_header_footer(&mut doc, &[0, 1], &header, false, &cx()).unwrap();
    let mut bates = HeaderFooter::default();
    bates.text[5] = "<<Bates Number#6#40#ACME-#>>".into();
    add_header_footer(&mut doc, &[0, 1], &bates, false, &cx()).unwrap();
    assert!(streams(&doc, 1).iter().any(|s| s.contains("/PCMark /Bates") && s.contains("(ACME-000041) Tj")));
    assert_eq!(marks_present(&doc), [MarkKind::HeaderFooter, MarkKind::Bates]);
    // Replacing the header leaves the Bates numbers, and the other way round.
    header.text[1] = "Public".into();
    add_header_footer(&mut doc, &[0, 1], &header, true, &cx()).unwrap();
    bates.text[5] = "<<Bates Number#4#1#X-#>>".into();
    add_header_footer(&mut doc, &[0, 1], &bates, true, &cx()).unwrap();
    let s = streams(&doc, 0);
    assert_eq!(s.iter().filter(|x| x.contains("/PCMark")).count(), 2, "{s:?}");
    assert!(s.iter().any(|x| x.contains("(Public) Tj")) && s.iter().any(|x| x.contains("(X-0001) Tj")), "{s:?}");
    assert_eq!(remove_marks(&mut doc, &[0, 1], MarkKind::Bates).unwrap(), 2);
    assert_eq!(marks_present(&doc), [MarkKind::HeaderFooter]);
    assert_eq!(remove_marks(&mut doc, &[0, 1], MarkKind::HeaderFooter).unwrap(), 2);
    assert_eq!(streams(&reopen(&doc), 0), original);
}

#[test]
fn bates_numbering_continues_from_a_given_start() {
    assert_eq!(with_bates_start("Ref <<Bates Number#6#1#ACME-#>> p<<1>>", 41), "Ref <<Bates Number#6#41#ACME-#>> p<<1>>");
    assert_eq!(with_bates_start("<<Bates Number>> <<Bates Number#4>>", 7), "<<Bates Number#6#7>> <<Bates Number#4#7>>");
    assert_eq!(with_bates_start("no tokens <<1>> <<Bates Number", 3), "no tokens <<1>> <<Bates Number");
    assert_eq!(bates_start(&with_bates_start("<<Bates Number#6#1#A#B>>", u64::MAX)), Some(u64::MAX));
    // A start at the top of the range doesn't overflow on later pages.
    let mut doc = fixture();
    let mut bates = HeaderFooter::default();
    bates.text[5] = with_bates_start("<<Bates Number#6#1#>>", u64::MAX);
    add_header_footer(&mut doc, &[0, 1], &bates, false, &cx()).unwrap();
    assert!(streams(&doc, 1).iter().any(|s| s.contains(&format!("({}) Tj", u64::MAX))));
}

fn doc_of(objs: &[&str]) -> Document {
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

/// Strokes thinner than the threshold as printed (width × transform scale, from `w` or an
/// ExtGState, in the page and in the forms it draws) are thickened; the rest draws as before.
#[test]
fn hairlines_are_thickened_as_printed() {
    let page = "0 w 0 0 m 10 10 l S 2 w 0 0 m 5 5 l S 0.5 0 0 0.5 0 0 cm 0.4 w 1 1 10 10 re S q /GS1 gs 0 0 m 1 1 l S Q /Fm Do 1 1 m 2 2 l f";
    let form = "1 w 0 0 m 9 9 l S";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /ExtGState << /GS1 << /LW 0.1 >> >> /XObject << /Fm 5 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{page}\nendstream", page.len()),
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] /Matrix [0.1 0 0 0.1 0 0] /Length {} >>\nstream\n{form}\nendstream",
            form.len()
        ),
    ];
    let mut doc = doc_of(&objs.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(fix_hairlines(&mut doc, &[0], &Hairlines::default()).unwrap(), 4);
    let doc = reopen(&doc);
    let s = streams(&doc, 0).join("");
    let words: Vec<&str> = s.split_whitespace().collect();
    let expected = "0 w 0.25 w 0 0 m 10 10 l S 0 w 2 w 0 0 m 5 5 l S 0.5 0 0 0.5 0 0 cm 0.4 w 0.5 w 1 1 10 10 re S 0.4 w q /GS1 gs 0.5 w 0 0 m 1 1 l S 0.1 w Q /Fm Do 1 1 m 2 2 l f";
    assert_eq!(words.join(" "), expected);
    let form = String::from_utf8(stream_bytes(&doc, &Object::Ref(pdfcraft_cos::ObjRef { num: 5, generation: 0 })).unwrap()).unwrap();
    assert_eq!(form.split_whitespace().collect::<Vec<_>>().join(" "), "1 w 5 w 0 0 m 9 9 l S");
    // Nothing thin is left, so a second pass changes nothing.
    let mut again = reopen(&doc);
    assert_eq!(fix_hairlines(&mut again, &[0], &Hairlines::default()).unwrap(), 0);
    let mut bad = reopen(&doc);
    for s in [
        Hairlines { thinner_than: 0.0, replace_with: 1.0 },
        Hairlines { thinner_than: 1.0, replace_with: f64::NAN },
        Hairlines { thinner_than: 1.0, replace_with: 100.0 },
    ] {
        assert!(fix_hairlines(&mut bad, &[0], &s).is_err());
    }
    assert!(matches!(fix_hairlines(&mut bad, &[3], &Hairlines::default()), Err(EditError::NoSuchPage(3))));
}
