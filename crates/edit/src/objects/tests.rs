use std::sync::Arc;

use pdfcraft_cos::{Document, Object, SaveOptions, write_incremental};

use super::*;

/// A one-page PDF (600×800, Helvetica as /F1) whose content is `content`; `extra` adds page
/// dictionary entries.
fn page_with(content: &str, extra: &str) -> Document {
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 600 800] >>".into(),
        format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> {extra} >>"),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
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

fn reopen(doc: &Document) -> Document {
    let bytes = write_incremental(doc, &SaveOptions::default()).unwrap();
    Document::open(Arc::new(bytes)).unwrap()
}

/// The page's content streams joined.
fn joined(doc: &Document) -> String {
    let p = pdfcraft_model::pages(doc).swap_remove(0);
    content_streams(doc, &p.dict).into_iter().map(|(_, d)| String::from_utf8_lossy(&d).into_owned()).collect::<Vec<_>>().join("\n")
}

/// Red square, then a blue square over it that leaves the fill blue, then text drawn in that blue.
const THREE: &str = "q 1 0 0 rg 10 10 100 100 re f Q\n0 0 1 rg 50 50 100 100 re f\nBT /F1 12 Tf 300 400 Td (Hello) Tj ET";

fn kinds(doc: &Document) -> Vec<(ObjectKind, [i64; 4])> {
    page_objects(doc, 0).unwrap().iter().map(|o| (o.kind, o.rect.map(|r| r.map(|v| v.round() as i64)).unwrap_or([0; 4]))).collect()
}

fn texts(doc: &Document) -> Vec<String> {
    crate::text::text_lines(doc, 0).unwrap().into_iter().map(|l| l.text).collect()
}

#[test]
fn objects_are_listed_in_paint_order_with_boxes() {
    let doc = page_with(THREE, "");
    let objects = page_objects(&doc, 0).unwrap();
    assert_eq!(objects.len(), 3);
    assert_eq!(kinds(&doc)[..2], [(ObjectKind::Path, [10, 10, 110, 110]), (ObjectKind::Path, [50, 50, 150, 150])]);
    let text = &objects[2];
    assert_eq!((text.kind, text.text.as_str()), (ObjectKind::Text, "Hello"));
    let r = text.rect.unwrap();
    assert!((r[0] - 300.0).abs() < 0.5 && r[2] > 320.0 && r[1] < 401.0 && r[3] > 405.0, "{r:?}");
}

#[test]
fn arranging_reorders_paint_and_keeps_each_objects_state() {
    let mut doc = page_with(THREE, "");
    // Send the blue square to the back: it's now drawn first, and still blue.
    assert!(edit_objects(&mut doc, 0, &[1], &ObjectsEdit::Arrange(Arrange::Back)).unwrap());
    assert_eq!(kinds(&doc)[0], (ObjectKind::Path, [50, 50, 150, 150]));
    assert_eq!(kinds(&doc)[1], (ObjectKind::Path, [10, 10, 110, 110]));
    let c = joined(&doc);
    let blue = c.find("50 50 100 100 re").unwrap();
    let red = c.find("10 10 100 100 re").unwrap();
    assert!(blue < red, "{c}");
    // Each object carries the colour it was painted with: the blue square its own blue, the text
    // the blue it inherited from the square.
    assert!(c[..blue].contains("0 0 1 rg"), "{c}");
    let text = c.find("BT").unwrap();
    assert!(c[red..text].contains("0 0 1 rg"), "the text keeps the fill it inherited: {c}");
    assert_eq!(texts(&doc), ["Hello"]);
    // Bring Forward puts the blue square back above the red one it overlaps.
    assert!(edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Arrange(Arrange::Forward)).unwrap());
    assert_eq!(kinds(&doc)[0], (ObjectKind::Path, [10, 10, 110, 110]));
    // Already at the front: nothing changes.
    assert!(!edit_objects(&mut doc, 0, &[2], &ObjectsEdit::Arrange(Arrange::Front)).unwrap());
    assert!(!objects_edit_changes(&doc, 0, &[0], &ObjectsEdit::Arrange(Arrange::Back)).unwrap());
    // Text to the back, then forward again (nothing above it overlaps: one place up).
    assert!(edit_objects(&mut doc, 0, &[2], &ObjectsEdit::Arrange(Arrange::Back)).unwrap());
    assert_eq!(kinds(&doc)[0].0, ObjectKind::Text);
    assert!(edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Arrange(Arrange::Forward)).unwrap());
    assert_eq!(kinds(&doc)[1].0, ObjectKind::Text);
    assert!(edit_objects(&mut doc, 0, &[1], &ObjectsEdit::Arrange(Arrange::Backward)).unwrap());
    assert_eq!(kinds(&doc)[0].0, ObjectKind::Text);
    let again = reopen(&doc);
    assert_eq!(page_objects(&again, 0).unwrap().len(), 3);
    assert_eq!(texts(&again), ["Hello"]);
}

#[test]
fn several_objects_keep_their_relative_order() {
    let content = "0 g 0 0 10 10 re f 0.5 g 0 0 20 20 re f 1 g 0 0 30 30 re f 0 1 0 rg 0 0 40 40 re f";
    let mut doc = page_with(content, "");
    edit_objects(&mut doc, 0, &[0, 1], &ObjectsEdit::Arrange(Arrange::Front)).unwrap();
    let rights: Vec<i64> = kinds(&doc).iter().map(|k| k.1[2]).collect();
    assert_eq!(rights, [30, 40, 10, 20]);
    edit_objects(&mut doc, 0, &[2, 3], &ObjectsEdit::Arrange(Arrange::Backward)).unwrap();
    let rights: Vec<i64> = kinds(&doc).iter().map(|k| k.1[2]).collect();
    assert_eq!(rights, [30, 10, 20, 40]);
}

#[test]
fn clips_transforms_and_nesting_carry_over() {
    // A clip and a scale apply to the first square; the second is drawn after the Q.
    let content = "q 0 0 50 50 re W n 2 0 0 2 0 0 cm 1 0 0 rg 0 0 40 40 re f Q 0 0 1 rg 100 100 10 10 re f";
    let mut doc = page_with(content, "");
    assert_eq!(kinds(&doc), [(ObjectKind::Path, [0, 0, 80, 80]), (ObjectKind::Path, [100, 100, 110, 110])]);
    edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Arrange(Arrange::Front)).unwrap();
    assert_eq!(kinds(&doc), [(ObjectKind::Path, [100, 100, 110, 110]), (ObjectKind::Path, [0, 0, 80, 80])]);
    let c = joined(&doc);
    let last = c.rfind("q\n").unwrap();
    let tail = &c[last..];
    assert!(tail.contains("W\nn") && tail.contains("2 0 0 2 0 0 cm") && tail.contains("1 0 0 rg"), "{c}");
    assert!(tail.find("W\nn").unwrap() < tail.find("cm").unwrap(), "the clip comes before the scale, as before: {c}");
}

#[test]
fn tagged_content_moves_as_one_object_and_keeps_its_mcid() {
    let content = "/P <</MCID 0>> BDC BT /F1 12 Tf 10 700 Td (One) Tj ET BT /F1 12 Tf 10 680 Td (Two) Tj ET EMC \
                   /Artifact BMC 0 0 1 rg 0 0 600 20 re f EMC /Span <</MCID 1>> BDC EMC";
    let mut doc = page_with(content, "/StructParents 0");
    let objects = page_objects(&doc, 0).unwrap();
    assert_eq!(objects.len(), 2, "{objects:?}");
    assert_eq!((objects[0].kind, objects[0].mcid, objects[0].text.as_str()), (ObjectKind::Text, Some(0), "One Two"));
    edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Arrange(Arrange::Front)).unwrap();
    let c = joined(&doc);
    assert_eq!(c.matches("/MCID 0").count(), 1, "one sequence for the tagged object: {c}");
    assert_eq!(c.matches("/MCID 1").count(), 1, "the empty sequence stays: {c}");
    assert!(c.find("/Artifact").unwrap() < c.find("/MCID 0").unwrap(), "{c}");
    assert_eq!(c.matches("BDC").count() + c.matches("BMC").count(), c.matches("EMC").count(), "{c}");
    assert_eq!(texts(&doc), ["One", "Two"]);
}

#[test]
fn aligning_and_distributing_move_objects_as_displayed() {
    let content = "0 g 10 10 20 20 re f 0 g 100 50 40 40 re f 0 g 300 30 10 10 re f";
    let mut doc = page_with(content, "");
    edit_objects(&mut doc, 0, &[0, 1, 2], &ObjectsEdit::Align(AlignTo::Top)).unwrap();
    let tops: Vec<i64> = kinds(&doc).iter().map(|k| k.1[3]).collect();
    assert_eq!(tops, [90, 90, 90]);
    edit_objects(&mut doc, 0, &[0, 1, 2], &ObjectsEdit::Align(AlignTo::HorizontalCenter)).unwrap();
    let centres: Vec<i64> = kinds(&doc).iter().map(|k| (k.1[0] + k.1[2]) / 2).collect();
    assert_eq!(centres, [160, 160, 160]);
    assert!(!objects_edit_changes(&doc, 0, &[0, 1, 2], &ObjectsEdit::Align(AlignTo::HorizontalCenter)).unwrap());

    let mut doc = page_with(content, "");
    edit_objects(&mut doc, 0, &[2, 0, 1], &ObjectsEdit::Distribute(Axis::Horizontal)).unwrap();
    let k = kinds(&doc);
    // Outer objects stay; the gaps are equal: (300 - 30 - 40) / 2 = 115.
    assert_eq!(k[0].1, [10, 10, 30, 30]);
    assert_eq!(k[2].1, [300, 30, 310, 40]);
    assert_eq!(k[1].1, [145, 50, 185, 90]);
    assert!(edit_objects(&mut doc, 0, &[0, 1], &ObjectsEdit::Distribute(Axis::Vertical)).is_err());

    // One object aligns to the page.
    edit_objects(&mut doc, 0, &[1], &ObjectsEdit::Align(AlignTo::Right)).unwrap();
    assert_eq!(kinds(&doc)[1].1, [560, 50, 600, 90]);

    // On a page turned 90° clockwise, "left" as displayed is the bottom of user space.
    let mut turned = page_with(content, "/Rotate 90");
    edit_objects(&mut turned, 0, &[0, 1], &ObjectsEdit::Align(AlignTo::Left)).unwrap();
    let k = kinds(&turned);
    assert_eq!((k[0].1[1], k[1].1[1]), (10, 10), "{k:?}");
    assert_eq!(k[1].1[0], 100, "{k:?}");
}

#[test]
fn text_moves_turns_and_flips_in_place_and_stays_text() {
    let content = "BT /F1 12 Tf 0 0 1 rg 100 100 Td (Moved) Tj ET BT (Same) Tj ET 0 g 400 400 50 50 re f";
    let mut doc = page_with(content, "");
    let before = page_objects(&doc, 0).unwrap();
    edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Move { dx: 50.0, dy: -20.0 }).unwrap();
    let after = page_objects(&doc, 0).unwrap();
    let (b, a) = (before[0].rect.unwrap(), after[0].rect.unwrap());
    assert!((a[0] - b[0] - 50.0).abs() < 0.01 && (a[1] - b[1] + 20.0).abs() < 0.01, "{b:?} → {a:?}");
    // The second text object still gets the font and colour the first one set.
    assert_eq!(after[1].rect, before[1].rect);
    assert_eq!(texts(&doc), ["Moved", "Same"]);
    let c = joined(&doc);
    assert!(c.contains("BT (Same) Tj ET"), "untouched bytes stay: {c}");
    let q = c.find("Q\n").unwrap();
    assert!(c[q..c.find("(Same)").unwrap()].contains("/F1 12 Tf"), "the font carries on after the moved text: {c}");

    edit_objects(&mut doc, 0, &[2], &ObjectsEdit::Rotate(1)).unwrap();
    assert_eq!(kinds(&doc)[2].1, [400, 400, 450, 450]);
    edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Rotate(1)).unwrap();
    let r = page_objects(&doc, 0).unwrap()[0].rect.unwrap();
    assert!(r[3] - r[1] > r[2] - r[0], "turned text is taller than wide: {r:?}");
    edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Flip { horizontal: true }).unwrap();
    assert_eq!(texts(&doc).len(), 2);
    let again = reopen(&doc);
    assert_eq!(page_objects(&again, 0).unwrap().len(), 3);
}

#[test]
fn added_items_and_marks_are_left_alone() {
    let mut doc = page_with(THREE, "");
    crate::add_content(
        &mut doc,
        0,
        &crate::Content::Text(crate::AddedText { text: "Added".into(), rect: [100.0, 600.0, 300.0, 650.0], ..Default::default() }),
    )
    .unwrap();
    let before = crate::list_added(&doc);
    assert_eq!(page_objects(&doc, 0).unwrap().len(), 3, "added items aren't page objects");
    edit_objects(&mut doc, 0, &[2], &ObjectsEdit::Arrange(Arrange::Back)).unwrap();
    edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Move { dx: 5.0, dy: 5.0 }).unwrap();
    assert_eq!(crate::list_added(&doc), before);
    assert_eq!(page_objects(&doc, 0).unwrap()[0].kind, ObjectKind::Text);
}

#[test]
fn bad_requests_are_refused_and_change_nothing() {
    let mut doc = page_with(THREE, "");
    let original = joined(&doc);
    for (objects, edit) in [
        (vec![], ObjectsEdit::Arrange(Arrange::Front)),
        (vec![3], ObjectsEdit::Arrange(Arrange::Front)),
        (vec![usize::MAX], ObjectsEdit::Align(AlignTo::Left)),
        (vec![0], ObjectsEdit::Move { dx: f64::NAN, dy: 0.0 }),
        (vec![0], ObjectsEdit::Move { dx: 1e300, dy: 0.0 }),
    ] {
        assert!(edit_objects(&mut doc, 0, &objects, &edit).is_err(), "{objects:?} {edit:?}");
    }
    assert!(edit_objects(&mut doc, 4, &[0], &ObjectsEdit::Rotate(1)).is_err());
    assert!(!edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Rotate(4)).unwrap());
    assert_eq!(joined(&doc), original);

    // Text used as a clip can't be rearranged, but the page is still listed.
    let mut clip = page_with("BT /F1 40 Tf 7 Tr 10 10 Td (Clip) Tj ET 0 g 0 0 600 800 re f", "");
    assert_eq!(page_objects(&clip, 0).unwrap().len(), 2);
    assert!(edit_objects(&mut clip, 0, &[1], &ObjectsEdit::Arrange(Arrange::Back)).is_err());
    assert!(edit_objects(&mut clip, 0, &[0], &ObjectsEdit::Move { dx: 1.0, dy: 1.0 }).is_err());
}

#[test]
fn hostile_content_never_panics() {
    for content in [
        "",
        "Q Q Q EMC EMC BT",
        "BT ET ET q q q 0 0 m",
        "/P <</MCID 0>> BDC BT EMC ET",
        "1 2 re 3 4 5 6 7 8 9 c f W f* n sh /X Do BI /W 1 /H 1 ID x EI MP DP",
        "1e308 1e308 1e308 1e308 1e308 1e308 cm 0 0 10 10 re f",
        "0 0 0 0 0 0 cm 0 0 10 10 re f",
        "BT \" ' TJ Tf Tr 9 Tr ET (",
    ] {
        let mut doc = page_with(content, "");
        let n = page_objects(&doc, 0).map(|o| o.len()).unwrap_or(0);
        let all: Vec<usize> = (0..n).collect();
        for edit in [
            ObjectsEdit::Arrange(Arrange::Front),
            ObjectsEdit::Arrange(Arrange::Forward),
            ObjectsEdit::Arrange(Arrange::Backward),
            ObjectsEdit::Arrange(Arrange::Back),
            ObjectsEdit::Align(AlignTo::Left),
            ObjectsEdit::Distribute(Axis::Vertical),
            ObjectsEdit::Rotate(3),
            ObjectsEdit::Flip { horizontal: false },
            ObjectsEdit::Move { dx: 1.0, dy: 1.0 },
        ] {
            let _ = edit_objects(&mut doc, 0, &all, &edit);
            let _ = page_objects(&doc, 0);
        }
    }
}

#[test]
fn stray_operators_in_a_text_object_stop_rewrites() {
    let mut doc = page_with("BT q /F1 12 Tf 10 10 Td (x) Tj Q ET 0 g 0 0 5 5 re f", "");
    assert!(edit_objects(&mut doc, 0, &[1], &ObjectsEdit::Arrange(Arrange::Back)).is_err());
    let r = edit_objects(&mut doc, 0, &[1], &ObjectsEdit::Move { dx: 1.0, dy: 0.0 });
    assert_eq!(r, Ok(true), "other objects can still move");
    assert!(matches!(joined(&doc).as_str(), c if c.contains("Q\n")));
    let _ = Object::Null;
}

fn style(f: impl FnOnce(&mut PathStyle)) -> ObjectsEdit {
    let mut s = PathStyle::default();
    f(&mut s);
    ObjectsEdit::Style(s)
}

#[test]
fn styling_paths_rewrites_only_their_paint_and_line_state() {
    // A stroked line under a 2× CTM, a filled square, then text that inherits the square's fill.
    let content = "q 2 0 0 2 0 0 cm 1 0 0 RG 10 10 m 100 10 l S Q 0 0 1 rg 300 300 50 50 re f BT /F1 12 Tf 300 400 Td (Hi) Tj ET";
    let mut doc = page_with(content, "");
    let edit = style(|s| {
        s.stroke = Some(Paint::Cmyk([0.0, 1.0, 1.0, 0.0]));
        s.line_width = Some(4.0);
        s.dash = Some(Dash { pattern: vec![6.0, 2.0], phase: 0.0 });
        s.cap = Some(LineCap::Round);
        s.join = Some(LineJoin::Bevel);
    });
    assert!(edit_objects(&mut doc, 0, &[0], &edit).unwrap());
    let c = joined(&doc);
    // Displayed points over the CTM's scale of 2.
    assert!(c.contains("q 2 w [3 1] 0 d 1 J 2 j 0 1 1 0 K \n10 10 m\n100 10 l\nS\nQ"), "{c}");
    // Fill a stroke-only path: S becomes B; turn the square's fill off and stroke it: f becomes S.
    edit_objects(&mut doc, 0, &[0], &style(|s| s.fill = Some(Paint::Gray(0.5)))).unwrap();
    assert!(joined(&doc).contains("0.5 g \n10 10 m\n100 10 l\nB\nQ"), "{}", joined(&doc));
    edit_objects(&mut doc, 0, &[1], &style(|s| (s.fill, s.stroke) = (Some(Paint::None), Some(Paint::Rgb([0.0, 1.0, 0.0]))))).unwrap();
    let c = joined(&doc);
    assert!(c.contains("q 0 1 0 RG \n300 300 50 50 re\nS\nQ"), "{c}");
    // The text still paints with the blue the square set.
    assert!(c.ends_with("BT /F1 12 Tf 300 400 Td (Hi) Tj ET") || c.contains("BT /F1 12 Tf 300 400 Td (Hi) Tj ET"), "{c}");
    assert_eq!(texts(&doc), ["Hi"]);
    // Nothing painted is refused; text isn't a path.
    assert!(edit_objects(&mut doc, 0, &[1], &style(|s| s.stroke = Some(Paint::None))).is_err());
    assert!(edit_objects(&mut doc, 0, &[2], &style(|s| s.fill = Some(Paint::Gray(0.0)))).is_err());
    assert_eq!(page_objects(&reopen(&doc), 0).unwrap().len(), 3);
}

#[test]
fn opacity_adds_an_ext_g_state_and_current_space_colours_are_checked() {
    let content = "/CS0 cs 0.5 scn 10 10 100 100 re f";
    let mut doc = page_with(content, "");
    // A Separation colour space resource on the page.
    let page = pdfcraft_model::pages(&doc)[0].obj;
    let mut res = resources(&doc, &pdfcraft_model::pages(&doc)[0].dict);
    let mut spaces = Dict::new();
    spaces.set(b"CS0".to_vec(), Object::Array(vec![Object::name("Separation"), Object::name("Spot"), Object::name("DeviceCMYK"), Object::Null]));
    res.set(b"ColorSpace".to_vec(), Object::Dict(spaces));
    doc.update_dict(page, |d| d.set(b"Resources".to_vec(), Object::Dict(res))).unwrap();

    assert!(edit_objects(&mut doc, 0, &[0], &style(|s| s.fill = Some(Paint::Current(vec![0.2, 0.3])))).is_err(), "one component");
    assert!(edit_objects(&mut doc, 0, &[0], &style(|s| s.fill = Some(Paint::Current(vec![1.5])))).is_err(), "out of range");
    edit_objects(&mut doc, 0, &[0], &style(|s| (s.fill, s.opacity) = (Some(Paint::Current(vec![0.8])), Some(0.25)))).unwrap();
    let c = joined(&doc);
    assert!(c.contains("q /PCOpacity250 gs 0.8 scn \n"), "{c}");
    let doc = reopen(&doc);
    let p = pdfcraft_model::pages(&doc).swap_remove(0);
    let res = resources(&doc, &p.dict);
    let gs = sub_dict(&doc, &res, b"ExtGState");
    let state = gs.get(b"PCOpacity250").and_then(|s| s.as_dict().cloned()).unwrap();
    assert_eq!((state.get(b"ca").and_then(Object::as_f64), state.get(b"CA").and_then(Object::as_f64)), (Some(0.25), Some(0.25)));
    assert!(sub_dict(&doc, &res, b"ColorSpace").contains(b"CS0"), "other resources kept");
}

#[test]
fn bad_styles_are_refused_and_change_nothing() {
    let mut doc = page_with(THREE, "");
    let original = joined(&doc);
    for edit in [
        style(|_| {}),
        style(|s| s.fill = Some(Paint::Rgb([1.0, f64::NAN, 0.0]))),
        style(|s| s.fill = Some(Paint::Gray(2.0))),
        style(|s| s.stroke = Some(Paint::Cmyk([0.0, 0.0, 0.0, -1.0]))),
        style(|s| s.fill = Some(Paint::Current(vec![]))),
        style(|s| s.fill = Some(Paint::Current(vec![0.5]))),
        style(|s| s.line_width = Some(f64::INFINITY)),
        style(|s| s.line_width = Some(-1.0)),
        style(|s| s.dash = Some(Dash { pattern: vec![0.0, 0.0], phase: 0.0 })),
        style(|s| s.dash = Some(Dash { pattern: vec![1.0; 17], phase: 0.0 })),
        style(|s| s.dash = Some(Dash { pattern: vec![1.0], phase: f64::NAN })),
        style(|s| s.opacity = Some(1.5)),
        ObjectsEdit::Resize { size: Size::Scale { sx: 0.0, sy: 1.0 }, anchor: Anchor::Center },
        ObjectsEdit::Resize { size: Size::Scale { sx: f64::NAN, sy: 1.0 }, anchor: Anchor::Center },
        ObjectsEdit::Resize { size: Size::To { width: None, height: None }, anchor: Anchor::Center },
        ObjectsEdit::Resize { size: Size::To { width: Some(-5.0), height: None }, anchor: Anchor::Center },
    ] {
        assert!(edit_objects(&mut doc, 0, &[0], &edit).is_err(), "{edit:?}");
    }
    assert_eq!(joined(&doc), original);
}

#[test]
fn resizing_scales_about_the_anchor_as_displayed() {
    let mut doc = page_with("0 g 100 100 50 20 re f BT /F1 12 Tf 300 400 Td (Hi) Tj ET", "");
    let edit = ObjectsEdit::Resize { size: Size::Scale { sx: 2.0, sy: 3.0 }, anchor: Anchor::BottomLeft };
    edit_objects(&mut doc, 0, &[0], &edit).unwrap();
    assert_eq!(kinds(&doc)[0], (ObjectKind::Path, [100, 100, 200, 160]));
    let edit = ObjectsEdit::Resize { size: Size::To { width: Some(50.0), height: None }, anchor: Anchor::Center };
    edit_objects(&mut doc, 0, &[0], &edit).unwrap();
    assert_eq!(kinds(&doc)[0], (ObjectKind::Path, [125, 115, 175, 145]), "in proportion about the centre");
    // Text scales and stays text.
    let before = page_objects(&doc, 0).unwrap()[1].rect.unwrap();
    edit_objects(&mut doc, 0, &[1], &ObjectsEdit::Resize { size: Size::Scale { sx: 2.0, sy: 2.0 }, anchor: Anchor::TopLeft }).unwrap();
    let after = page_objects(&doc, 0).unwrap()[1].rect.unwrap();
    assert!(((after[2] - after[0]) - 2.0 * (before[2] - before[0])).abs() < 0.5, "{before:?} → {after:?}");
    assert!((after[0] - before[0]).abs() < 0.01 && (after[3] - before[3]).abs() < 0.01, "top-left stays: {before:?} → {after:?}");
    assert_eq!(texts(&doc), ["Hi"]);
    // On a page turned a quarter, "top" is the displayed top.
    let mut turned = page_with("0 g 100 100 50 20 re f", "/Rotate 90");
    let edit = ObjectsEdit::Resize { size: Size::Scale { sx: 1.0, sy: 2.0 }, anchor: Anchor::Top };
    edit_objects(&mut turned, 0, &[0], &edit).unwrap();
    // Displayed height is user width; displayed top (Rotate 90) is user left (x = 100).
    assert_eq!(kinds(&turned)[0], (ObjectKind::Path, [100, 100, 200, 120]));
}

#[test]
fn deleting_objects_keeps_the_state_they_leave() {
    // The first square sets the blue the text paints with and clips nothing; the second clips.
    let content = "0 0 1 rg 10 10 50 50 re f 100 100 200 200 re W f BT /F1 12 Tf 150 150 Td (In) Tj ET";
    let mut doc = page_with(content, "");
    edit_objects(&mut doc, 0, &[0, 1], &ObjectsEdit::Delete).unwrap();
    let objects = page_objects(&doc, 0).unwrap();
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].kind, ObjectKind::Text);
    let c = joined(&doc);
    assert!(c.starts_with("0 0 1 rg") && c.contains("100 100 200 200 re\nW\nn\n"), "colour and clip stay: {c}");
    assert_eq!(texts(&reopen(&doc)), ["In"]);
    // Tagged: the sequence empties and stays balanced.
    let mut tagged = page_with("/P <</MCID 0>> BDC 0 g 0 0 5 5 re f EMC 0 g 9 9 5 5 re f", "/StructParents 0");
    edit_objects(&mut tagged, 0, &[0], &ObjectsEdit::Delete).unwrap();
    let c = joined(&tagged);
    assert_eq!(page_objects(&tagged, 0).unwrap().len(), 1, "{c}");
    assert_eq!(c.matches("BDC").count(), c.matches("EMC").count(), "{c}");
}

#[test]
fn hostile_styles_and_resizes_never_panic() {
    for content in [
        "",
        "1 2 re W f* /Nope cs 1 2 3 scn 0 0 m 1 1 l B",
        "/Pattern cs /P1 scn 0 0 10 10 re f",
        "1e308 1e308 1e308 1e308 1e308 1e308 cm 0 0 10 10 re S",
        "0 0 0 0 0 0 cm 0 0 10 10 re S",
        "cs CS 0 0 m S q Q Q",
    ] {
        let mut doc = page_with(content, "");
        let n = page_objects(&doc, 0).map(|o| o.len()).unwrap_or(0);
        let all: Vec<usize> = (0..n).collect();
        for edit in [
            style(|s| s.fill = Some(Paint::Current(vec![0.5, 0.5, 0.5]))),
            style(|s| (s.line_width, s.dash) = (Some(3.0), Some(Dash { pattern: vec![1.0, 2.0], phase: 1.0 }))),
            style(|s| s.opacity = Some(0.5)),
            ObjectsEdit::Resize { size: Size::Scale { sx: 1000.0, sy: 0.001 }, anchor: Anchor::BottomRight },
            ObjectsEdit::Resize { size: Size::To { width: Some(1e6), height: Some(1e-300) }, anchor: Anchor::Center },
            ObjectsEdit::Delete,
        ] {
            let _ = edit_objects(&mut doc, 0, &all, &edit);
            let _ = page_objects(&doc, 0);
        }
    }
}

/// A one-page PDF whose page uses `resources` and whose objects 5… are `extra`.
fn page_with_objects(content: &str, resources: &str, extra: &[String]) -> Document {
    let mut objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 600 800] >>".into(),
        format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources {resources} >>"),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    objs.extend(extra.iter().cloned());
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
fn rotating_by_degrees_turns_about_the_centre_and_leaves_the_rest() {
    let mut doc = page_with(THREE, "");
    let before = joined(&doc);
    assert!(edit_objects(&mut doc, 0, &[0], &ObjectsEdit::RotateBy(45.0)).unwrap());
    let r = page_objects(&doc, 0).unwrap()[0].rect.unwrap();
    let half = 50.0 * 2f64.sqrt();
    for (v, want) in r.iter().zip([60.0 - half, 60.0 - half, 60.0 + half, 60.0 + half]) {
        assert!((v - want).abs() < 0.01, "{r:?}");
    }
    let after = joined(&doc);
    assert!(after.contains("0.707107 -0.707107 0.707107 0.707107"), "{after}");
    // The other objects keep their bytes.
    let tail = &before[before.find("0 0 1 rg").unwrap()..];
    assert!(after.ends_with(tail), "{after}");
    // A whole turn changes nothing; a quarter turn is the same as Rotate.
    assert_eq!(edit_objects(&mut doc, 0, &[1], &ObjectsEdit::RotateBy(-720.0)), Ok(false));
    let (mut a, mut b) = (page_with(THREE, ""), page_with(THREE, ""));
    edit_objects(&mut a, 0, &[1], &ObjectsEdit::RotateBy(90.0)).unwrap();
    edit_objects(&mut b, 0, &[1], &ObjectsEdit::Rotate(1)).unwrap();
    assert_eq!(joined(&a), joined(&b));
    assert_eq!(kinds(&reopen(&a)), kinds(&b));
}

#[test]
fn hostile_angles_are_refused_and_change_nothing() {
    let mut doc = page_with(THREE, "");
    let before = joined(&doc);
    for d in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e7, -1e300] {
        assert!(edit_objects(&mut doc, 0, &[0, 1, 2], &ObjectsEdit::RotateBy(d)).is_err(), "{d}");
        assert!(objects_edit_changes(&doc, 0, &[0], &ObjectsEdit::RotateBy(d)).is_err());
    }
    assert_eq!(joined(&doc), before);
}

#[test]
fn objects_report_how_they_are_painted() {
    let doc = page_with(&format!("{THREE}\n0.5 G 3 w 2 0 0 2 0 0 cm 0 0 10 10 re S\n/Pattern cs /P1 scn 0 0 5 5 re B"), "");
    let o = page_objects(&doc, 0).unwrap();
    let a = o[0].appearance.clone().unwrap();
    assert_eq!((a.fill, a.stroke, a.fill_opacity), (Paint::Rgb([1.0, 0.0, 0.0]), Paint::None, 1.0));
    assert_eq!(o[1].appearance.as_ref().unwrap().fill, Paint::Rgb([0.0, 0.0, 1.0]));
    assert_eq!((o[2].kind, o[2].appearance.clone(), o[2].opacity), (ObjectKind::Text, None, 1.0));
    let s = o[3].appearance.clone().unwrap();
    assert_eq!((&s.fill, &s.stroke), (&Paint::None, &Paint::Gray(0.5)));
    assert!((s.line_width - 6.0).abs() < 1e-9, "{s:?}");
    let p = o[4].appearance.clone().unwrap();
    assert_eq!((p.fill, p.stroke), (Paint::Current(Vec::new()), Paint::Gray(0.5)));
}

#[test]
fn opacity_applies_to_any_object_other_styles_only_to_paths() {
    let mut doc = page_with(THREE, "");
    let lines = texts(&doc);
    assert!(edit_objects(&mut doc, 0, &[2], &style(|s| s.opacity = Some(0.25))).unwrap());
    let doc = reopen(&doc);
    let o = page_objects(&doc, 0).unwrap();
    assert_eq!((o.len(), o[2].kind, o[2].opacity), (3, ObjectKind::Text, 0.25));
    assert_eq!(texts(&doc), lines, "the text stays text");
    assert_eq!((o[0].opacity, o[1].opacity), (1.0, 1.0));
    let mut doc = doc;
    assert!(edit_objects(&mut doc, 0, &[2], &style(|s| s.fill = Some(Paint::Gray(0.0)))).is_err());
    // A path's opacity shows on it too.
    assert!(edit_objects(&mut doc, 0, &[0], &style(|s| s.opacity = Some(0.5))).unwrap());
    let a = page_objects(&doc, 0).unwrap()[0].appearance.clone().unwrap();
    assert_eq!((a.fill_opacity, a.stroke_opacity), (0.5, 0.5));
}

#[test]
fn the_index_finds_the_topmost_object_and_marquee_hits() {
    let doc = page_with(THREE, "");
    let objects = page_objects(&doc, 0).unwrap();
    let index = ObjectIndex::new(&objects);
    assert_eq!(index.len(), 3);
    assert_eq!(index.hit(60.0, 60.0, 0.0), Some(1), "the blue square is on top");
    assert_eq!(index.hits(60.0, 60.0, 0.0), vec![1, 0]);
    assert_eq!(index.hit(20.0, 20.0, 0.0), Some(0));
    assert_eq!(index.hit(9.0, 20.0, 2.0), Some(0), "within the slop");
    assert_eq!(index.hit(500.0, 20.0, 2.0), None);
    assert_eq!(index.touching([0.0, 0.0, 55.0, 55.0]), vec![0, 1]);
    assert_eq!(index.touching([305.0, 0.0, 290.0, 800.0]), vec![2], "corners in either order");
    assert!(index.touching([f64::NAN, 0.0, 1.0, 1.0]).is_empty());
    assert_eq!(index.hit(f64::NAN, 1.0, 1.0), None);
}

#[test]
fn hostile_boxes_never_break_the_index() {
    let o = |rect| PageObject { kind: ObjectKind::Path, rect, text: String::new(), mcid: None, appearance: None, opacity: 1.0 };
    let objects = vec![
        o(Some([f64::NAN, 0.0, 1.0, 1.0])),
        o(Some([-f64::MAX, -f64::MAX, f64::MAX, f64::MAX])),
        o(Some([5.0, 5.0, 5.0, 5.0])),
        o(None),
        o(Some([10.0, 10.0, 0.0, 0.0])),
    ];
    let index = ObjectIndex::new(&objects);
    assert_eq!(index.hit(5.0, 5.0, f64::INFINITY), Some(2));
    assert_eq!(index.touching([0.0, 0.0, 6.0, 6.0]), vec![1, 2]);
    // Many objects: each is entered in a bounded number of cells.
    let many: Vec<PageObject> = (0..20_000).map(|k| o(Some([k as f64, 0.0, k as f64 + 1.0, 1.0]))).chain([o(Some([0.0, 0.0, 1e6, 1e6]))]).collect();
    let index = ObjectIndex::new(&many);
    assert!(index.cells.iter().map(Vec::len).sum::<usize>() <= many.len() * MAX_CELLS);
    assert_eq!(index.hit(100.5, 0.5, 0.0), Some(20_000), "the large one is on top");
    assert_eq!(index.hits(100.5, 0.5, 0.0), vec![20_000, 100]);
    assert!(ObjectIndex::new(&[]).hit(0.0, 0.0, 1.0).is_none());
}

#[test]
fn deeply_nested_forms_are_one_object_and_move_whole() {
    // Form k draws form k+1, two thousand deep; the last is a square.
    let depth = 2000;
    let extra: Vec<String> = (0..depth)
        .map(|k| {
            let (body, res) = if k + 1 == depth {
                ("0 0 10 10 re f".to_string(), "<< >>".to_string())
            } else {
                ("/X Do".to_string(), format!("<< /XObject << /X {} 0 R >> >>", k + 6))
            };
            format!("<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources {res} /Length {} >>\nstream\n{body}\nendstream", body.len())
        })
        .collect();
    let mut doc = page_with_objects("q 1 0 0 1 100 100 cm /X Do Q", "<< /XObject << /X 5 0 R >> >>", &extra);
    let o = page_objects(&doc, 0).unwrap();
    assert_eq!((o.len(), o[0].kind), (1, ObjectKind::Form));
    assert_eq!(o[0].rect.map(|r| r.map(|v| v.round() as i64)), Some([100, 100, 110, 110]));
    assert!(edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Move { dx: 5.0, dy: 0.0 }).unwrap());
    assert!(edit_objects(&mut doc, 0, &[0], &ObjectsEdit::RotateBy(30.0)).unwrap());
    assert!(edit_objects(&mut doc, 0, &[0], &style(|s| s.opacity = Some(0.5))).unwrap());
    assert_eq!(page_objects(&reopen(&doc), 0).unwrap().len(), 1);
}

#[test]
fn hostile_matrices_and_degenerate_paths_never_panic() {
    for content in [
        "1e308 0 0 1e308 0 0 cm 0 0 10 10 re f",
        "1e308 1e308 -1e308 -1e308 1e308 1e308 cm 0 0 m 1 1 l S",
        "0 0 0 0 0 0 cm 0 0 10 10 re f",
        "1e-300 0 0 1e-300 0 0 cm 0 0 10 10 re f",
        "0 0 m f 0 0 0 0 re S 5 5 m h B",
        "1e400 w 1e400 0 0 1e400 0 0 cm 0 0 1 1 re S",
        "/GS1 gs 2 w 0 0 m 1e308 1e308 l S",
    ] {
        let mut doc = page_with(content, "");
        let objects = page_objects(&doc, 0).unwrap_or_default();
        let index = ObjectIndex::new(&objects);
        let _ = (index.hit(0.0, 0.0, 4.0), index.touching([-1e308, -1e308, 1e308, 1e308]));
        let all: Vec<usize> = (0..objects.len()).collect();
        for edit in [
            ObjectsEdit::RotateBy(33.0),
            ObjectsEdit::RotateBy(-1e6),
            ObjectsEdit::Resize { size: Size::Scale { sx: 2.0, sy: 0.5 }, anchor: Anchor::TopLeft },
            style(|s| s.opacity = Some(0.3)),
            ObjectsEdit::Move { dx: 1e6, dy: -1e6 },
            ObjectsEdit::Arrange(Arrange::Back),
            ObjectsEdit::Delete,
        ] {
            let _ = edit_objects(&mut doc, 0, &all, &edit);
            let _ = page_objects(&doc, 0);
        }
    }
}

#[test]
fn a_matrix_transforms_several_objects_together() {
    let mut doc = page_with(THREE, "");
    // Scale both squares by 2 about (10, 10): their combined box doubles.
    assert!(edit_objects(&mut doc, 0, &[0, 1], &ObjectsEdit::Matrix([2.0, 0.0, 0.0, 2.0, -10.0, -10.0])).unwrap());
    assert_eq!(kinds(&doc)[..2], [(ObjectKind::Path, [10, 10, 210, 210]), (ObjectKind::Path, [90, 90, 290, 290])]);
    assert_eq!(texts(&doc), vec!["Hello".to_string()]);
    let before = joined(&doc);
    for m in [
        [f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0],
        [1e9, 0.0, 0.0, 1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0, 1.0, 1e300, 0.0],
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0, 1.0, 0.0, 0.0],
    ] {
        assert!(edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Matrix(m)).is_err(), "{m:?}");
    }
    assert_eq!(joined(&doc), before);
    assert_eq!(edit_objects(&mut doc, 0, &[0], &ObjectsEdit::Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])), Ok(false));
}

#[test]
fn a_shading_is_as_large_as_the_clip_it_fills() {
    let doc = page_with("q 10 10 100 50 re W n /Sh0 sh Q /Sh0 sh q 2 0 0 2 0 0 cm 0 0 5 5 re W n 1 1 2 2 re W n /Sh0 sh Q", "");
    let o = page_objects(&doc, 0).unwrap();
    assert_eq!(o.iter().map(|o| o.kind).collect::<Vec<_>>(), vec![ObjectKind::Shading; 3]);
    let boxes: Vec<[i64; 4]> = o.iter().map(|o| o.rect.unwrap().map(|v| v.round() as i64)).collect();
    assert_eq!(boxes, vec![[10, 10, 110, 60], [0, 0, 600, 800], [2, 2, 6, 6]]);
    assert_eq!(ObjectIndex::new(&o).hit(50.0, 30.0, 0.0), Some(1), "the unclipped one is on top");
}
