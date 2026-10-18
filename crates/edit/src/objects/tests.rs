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
