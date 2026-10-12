use std::sync::Arc;

use pdfcraft_cos::{Document, Object, SaveOptions, write_full, write_incremental};

use super::*;

/// Build a PDF from numbered object bodies (object `i + 1` is `objs[i]`).
fn build(objs: &[String]) -> Document {
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

/// Three pages (objects 3, 4, 5); a two-level name tree (chap1, chap2 with a `/D` dictionary);
/// a legacy `/Dests` entry `old`; a link on page 1 to (chap1), a link with an indirect GoTo
/// action to /old, and a bookmark to (chap2).
fn sample() -> Document {
    let s = |x: &str| x.to_string();
    build(&[
        s("<< /Type /Catalog /Pages 2 0 R /Names << /Dests 6 0 R >> /Dests 9 0 R /Outlines 12 0 R >>"),
        s("<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 600 800] >>"),
        s("<< /Type /Page /Parent 2 0 R /Annots [10 0 R 11 0 R] >>"),
        s("<< /Type /Page /Parent 2 0 R >>"),
        s("<< /Type /Page /Parent 2 0 R >>"),
        s("<< /Kids [7 0 R 8 0 R] >>"),
        s("<< /Limits [(chap1) (chap1)] /Names [(chap1) [3 0 R /XYZ 10 700 1.5]] >>"),
        s("<< /Limits [(chap2) (chap2)] /Names [(chap2) << /D [4 0 R /FitH 500] /SD [null] >>] >>"),
        s("<< /old [5 0 R /FitV 120] >>"),
        s("<< /Type /Annot /Subtype /Link /Rect [0 0 50 50] /Dest (chap1) >>"),
        s("<< /Type /Annot /Subtype /Link /Rect [0 60 50 90] /A 14 0 R >>"),
        s("<< /Type /Outlines /First 13 0 R /Last 13 0 R /Count 1 >>"),
        s("<< /Title (Two) /Parent 12 0 R /Dest (chap2) >>"),
        s("<< /S /GoTo /D /old >>"),
    ])
}

fn reopen(doc: &Document, incremental: bool) -> Document {
    let bytes = if incremental { write_incremental(doc, &SaveOptions::default()) } else { write_full(doc, &SaveOptions::default()) };
    Document::open(Arc::new(bytes.unwrap())).unwrap()
}

#[test]
fn reads_the_name_tree_and_the_legacy_dictionary() {
    let doc = sample();
    let all = list(&doc);
    assert_eq!(all.len(), 3);
    assert_eq!(
        all[0],
        NamedDest {
            name: "chap1".into(),
            source: DestSource::NameTree,
            view: Some(View { page: 0, fit: Fit::Xyz { left: Some(10.0), top: Some(700.0), zoom: Some(1.5) } })
        }
    );
    assert_eq!(all[1].view, Some(View { page: 1, fit: Fit::FitH { top: Some(500.0) } }));
    assert_eq!(
        all[2],
        NamedDest { name: "old".into(), source: DestSource::Legacy, view: Some(View { page: 2, fit: Fit::FitV { left: Some(120.0) } }) }
    );
    assert_eq!(lookup(&doc, "old").map(|d| d.source), Some(DestSource::Legacy));
    assert_eq!(named_ref(&doc, "chap1").unwrap(), Object::String(PdfString::literal(b"chap1".to_vec())));
    assert_eq!(named_ref(&doc, "old").unwrap(), Object::Name(b"old".to_vec()));
    assert!(matches!(named_ref(&doc, "nope"), Err(DestError::NoSuchName(_))));
    assert_eq!(references(&doc, "chap1"), 1);
    assert_eq!(references(&doc, "chap2"), 1);
    assert_eq!(references(&doc, "old"), 1);
}

#[test]
fn create_retarget_rename_and_delete_survive_save_and_reopen() {
    for incremental in [false, true] {
        let mut doc = sample();
        // Create: goes into the name tree, which becomes one flat sorted node.
        let v = View { page: 2, fit: Fit::FitR { rect: [100.0, 50.0, 10.0, 200.0] } };
        assert!(set(&mut doc, "intro", &v).unwrap());
        // Re-target chap2: keeps its dictionary form and /SD.
        assert!(!set(&mut doc, "chap2", &View { page: 0, fit: Fit::FitB }).unwrap());
        // Re-target the legacy entry where it lives.
        assert!(!set(&mut doc, "old", &View { page: 1, fit: Fit::FitBH { top: None } }).unwrap());
        // Rename: references follow.
        assert_eq!(rename(&mut doc, "chap1", "Chapter one").unwrap(), 1);
        assert_eq!(rename(&mut doc, "old", "older").unwrap(), 1);
        let doc = reopen(&doc, incremental);
        let names: Vec<(String, DestSource)> = list(&doc).into_iter().map(|d| (d.name, d.source)).collect();
        assert_eq!(
            names,
            [
                ("Chapter one".to_string(), DestSource::NameTree),
                ("chap2".to_string(), DestSource::NameTree),
                ("intro".to_string(), DestSource::NameTree),
                ("older".to_string(), DestSource::Legacy),
            ]
        );
        assert_eq!(lookup(&doc, "intro").unwrap().view, Some(View { page: 2, fit: Fit::FitR { rect: [10.0, 50.0, 100.0, 200.0] } }));
        assert_eq!(lookup(&doc, "chap2").unwrap().view, Some(View { page: 0, fit: Fit::FitB }));
        assert_eq!(lookup(&doc, "older").unwrap().view, Some(View { page: 1, fit: Fit::FitBH { top: None } }));
        let tree = read_tree(&doc).entries;
        let chap2 = doc.resolve(&tree[1].1);
        assert!(chap2.as_dict().is_some_and(|d| d.contains(b"SD")), "the /D dictionary keeps its other keys");
        // The link now says (Chapter one); the indirect GoTo action says /older.
        // (A full save renumbers objects: find them from the page.)
        let annots = doc.resolve(crate::pages(&doc)[0].dict.get(b"Annots").unwrap()).as_array().unwrap().clone();
        let link = doc.resolve(&annots[0]);
        assert_eq!(link.as_dict().unwrap().get(b"Dest"), Some(&Object::String(PdfString::literal(b"Chapter one".to_vec()))));
        let action = doc.resolve(doc.resolve(&annots[1]).as_dict().unwrap().get(b"A").unwrap());
        assert!(matches!(doc.resolve(&annots[1]).as_dict().unwrap().get(b"A"), Some(Object::Ref(_))), "the action stays indirect");
        assert_eq!(action.as_dict().unwrap().get(b"D"), Some(&Object::Name(b"older".to_vec())));
        assert_eq!(references(&doc, "Chapter one"), 1);
        // Delete: the bookmark keeps its dangling name and is reported.
        let mut doc = doc;
        assert_eq!(delete(&mut doc, "chap2").unwrap(), 1);
        assert_eq!(delete(&mut doc, "older").unwrap(), 1);
        assert!(matches!(delete(&mut doc, "chap2"), Err(DestError::NoSuchName(_))));
        let doc = reopen(&doc, incremental);
        assert_eq!(list(&doc).len(), 2);
        assert_eq!(references(&doc, "chap2"), 1, "dangling references are still counted");
    }
}

#[test]
fn renaming_onto_an_existing_name_or_bad_names_is_refused() {
    let mut doc = sample();
    assert!(matches!(rename(&mut doc, "chap1", "chap2"), Err(DestError::Exists(_))));
    assert!(matches!(rename(&mut doc, "chap1", "old"), Err(DestError::Exists(_))));
    assert!(matches!(rename(&mut doc, "chap1", "  "), Err(DestError::Invalid(_))));
    assert!(matches!(rename(&mut doc, "chap1", "a\u{0}b"), Err(DestError::Invalid(_))));
    assert!(matches!(set(&mut doc, &"x".repeat(MAX_NAME_CHARS + 1), &View::page(0)), Err(DestError::Invalid(_))));
    assert!(matches!(rename(&mut doc, "missing", "x"), Err(DestError::NoSuchName(_))));
    // Non-ASCII names are stored as text strings and found again by their text.
    assert!(set(&mut doc, "Kapitel ü", &View::page(1)).unwrap());
    assert_eq!(lookup(&doc, "Kapitel ü").unwrap().view, Some(View::page(1)));
    assert_eq!(list(&doc).len(), 4, "nothing else changed");
}

#[test]
fn views_are_validated_before_anything_changes() {
    let mut doc = sample();
    let bad = [
        View { page: 3, fit: Fit::Fit },
        View { page: 0, fit: Fit::Xyz { left: Some(f64::NAN), top: None, zoom: None } },
        View { page: 0, fit: Fit::Xyz { left: None, top: None, zoom: Some(65.0) } },
        View { page: 0, fit: Fit::Xyz { left: None, top: None, zoom: Some(-1.0) } },
        View { page: 0, fit: Fit::FitH { top: Some(f64::INFINITY) } },
        View { page: 0, fit: Fit::FitR { rect: [10.0, 10.0, 10.0, 50.0] } },
        View { page: 0, fit: Fit::FitV { left: Some(1e300) } },
    ];
    for v in bad {
        assert!(set(&mut doc, "x", &v).is_err(), "{v:?}");
    }
    assert_eq!(list(&doc).len(), 3);
    // Zoom 0 is "keep the current zoom", written as null.
    set(&mut doc, "z", &View { page: 0, fit: Fit::Xyz { left: None, top: Some(5.0), zoom: Some(0.0) } }).unwrap();
    assert_eq!(lookup(&doc, "z").unwrap().view, Some(View { page: 0, fit: Fit::Xyz { left: None, top: Some(5.0), zoom: None } }));
}

#[test]
fn fit_parts_are_checked() {
    assert_eq!(Fit::from_parts("xyz", Some(1.0), None, Some(2.0), None).unwrap(), Fit::Xyz { left: Some(1.0), top: None, zoom: Some(2.0) });
    assert_eq!(Fit::from_parts("FitBV", Some(3.0), None, None, None).unwrap(), Fit::FitBV { left: Some(3.0) });
    assert!(Fit::from_parts("fit", None, Some(1.0), None, None).is_err());
    assert!(Fit::from_parts("fitr", None, None, None, None).is_err());
    assert!(Fit::from_parts("fith", Some(1.0), None, None, None).is_err());
    assert!(Fit::from_parts("zoomy", None, None, None, None).is_err());
    for k in ["XYZ", "Fit", "FitH", "FitV", "FitB", "FitBH", "FitBV"] {
        assert_eq!(Fit::from_parts(k, None, None, None, None).unwrap().kind(), k);
    }
}

#[test]
fn hostile_name_trees_are_bounded_and_never_rewritten() {
    let s = |x: &str| x.to_string();
    // /Kids loops back to the root; a node is not a dictionary; odd /Names; non-string key.
    let looping = build(&[
        s("<< /Type /Catalog /Pages 2 0 R /Names << /Dests 4 0 R >> >>"),
        s("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        s("<< /Type /Page /Parent 2 0 R >>"),
        s("<< /Kids [4 0 R 5 0 R 6 0 R] /Names [(a) [3 0 R /Fit] (b)] >>"),
        s("42"),
        s("<< /Names [/notastring [3 0 R /Fit]] >>"),
    ]);
    let all = list(&looping);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].view, Some(View::page(0)));
    let mut d = looping;
    assert!(matches!(set(&mut d, "new", &View::page(0)), Err(DestError::Unwritable(_))));
    assert!(matches!(delete(&mut d, "a"), Err(DestError::Unwritable(_))));
    // A chain of nested /Kids deeper than the limit.
    let mut objs = vec![
        s("<< /Type /Catalog /Pages 2 0 R /Names << /Dests 4 0 R >> >>"),
        s("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        s("<< /Type /Page /Parent 2 0 R >>"),
    ];
    for i in 0..(MAX_DEPTH + 10) {
        objs.push(format!("<< /Kids [{} 0 R] /Names [(n{i}) [3 0 R /Fit]] >>", objs.len() + 2));
    }
    objs.push(s("<< /Names [(leaf) [3 0 R /Fit]] >>"));
    let mut deep = build(&objs);
    assert_eq!(list(&deep).len(), MAX_DEPTH + 1);
    assert!(matches!(set(&mut deep, "n0", &View::page(0)), Err(DestError::Unwritable(_))));
}

#[test]
fn lenient_reading_of_odd_destinations() {
    let s = |x: &str| x.to_string();
    let doc = build(&[
        s(
            "<< /Type /Catalog /Pages 2 0 R /Names << /Dests << /Names [(int) [0 /XYZ null null 0] (unknown) [3 0 R /Zoomy 1] (bad) [99 0 R /Fit] (short) [3 0 R /FitR 1 2] (str) (x)] >> >> >>",
        ),
        s("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        s("<< /Type /Page /Parent 2 0 R >>"),
    ]);
    let all = list(&doc);
    assert_eq!(all[0].view, Some(View { page: 0, fit: Fit::Xyz { left: None, top: None, zoom: None } }), "page number and zoom 0");
    assert_eq!(all[1].view, Some(View::page(0)), "unknown fit reads as Fit");
    assert_eq!(all[2].view, None, "not a page of this document");
    assert_eq!(all[3].view, Some(View::page(0)), "FitR without its rectangle");
    assert_eq!(all[4].view, None);
    assert_eq!(read_target(&doc, &Object::Name(b"q".to_vec()), &page_refs(&doc)), Some(Target::Named("q".into())));
    assert_eq!(read_target(&doc, &Object::Int(3), &page_refs(&doc)), None);
}

#[test]
fn first_destination_in_a_document_without_names_creates_the_tree() {
    let s = |x: &str| x.to_string();
    let mut doc =
        build(&[s("<< /Type /Catalog /Pages 2 0 R >>"), s("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"), s("<< /Type /Page /Parent 2 0 R >>")]);
    assert!(list(&doc).is_empty());
    assert!(set(&mut doc, "top", &View { page: 0, fit: Fit::Xyz { left: Some(0.0), top: Some(792.0), zoom: Some(2.0) } }).unwrap());
    let doc = reopen(&doc, false);
    assert_eq!(list(&doc).len(), 1);
    // Deleting the last one removes the tree.
    let mut doc = doc;
    delete(&mut doc, "top").unwrap();
    assert!(tree_root(&doc).is_none());
}
