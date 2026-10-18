//! End to end: Combine Files and Insert Pages from File merge the files' structure trees, and the
//! Tags tree, the Accessibility Checker and PDF/UA verification judge the result as they judged
//! the sources.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn stream(data: &str) -> String {
    format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len())
}

fn build(bodies: &[String]) -> Vec<u8> {
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
    out.extend_from_slice(format!("trailer << /Size {} /Root 1 0 R /Info 8 0 R >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).as_bytes());
    out
}

const CONTENT: &str = "/H1 << /MCID 0 >> BDC BT /F1 12 Tf 20 180 Td (Title) Tj ET EMC \
     /Figure << /MCID 1 >> BDC q 10 0 0 10 20 120 cm /Im Do Q EMC \
     /Artifact BMC 0 0 200 2 re f EMC \
     /TH << /MCID 2 >> BDC BT /F1 12 Tf (A) Tj ET EMC /TH << /MCID 3 >> BDC BT /F1 12 Tf (B) Tj ET EMC \
     /TD << /MCID 4 >> BDC BT /F1 12 Tf (1) Tj ET EMC /TD << /MCID 5 >> BDC BT /F1 12 Tf (2) Tj ET EMC \
     /Lbl << /MCID 6 >> BDC BT /F1 12 Tf (1.) Tj ET EMC /LBody << /MCID 7 >> BDC BT /F1 12 Tf (Item) Tj ET EMC";

/// A well-tagged one-page PDF/UA-1 document: heading (custom type Heading1), figure, table,
/// list, link and form field. `aside` is what the unused custom type Aside maps to; `field` the
/// form field's name; `lang` the language.
fn good(aside: &str, field: &str, lang: &str) -> Vec<u8> {
    let xmp = stream(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:pdfuaid=\"http://www.aiim.org/pdfua/ns/id/\">\
         <pdfuaid:part>1</pdfuaid:part><dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Quarterly report</rdf:li></rdf:Alt></dc:title>\
         </rdf:Description></rdf:RDF></x:xmpmeta>",
    );
    let mut v = vec![String::from("null"); 30];
    let mut set = |n: usize, s: &str| v[n - 1] = s.to_string();
    set(
        1,
        &format!(
            "<< /Type /Catalog /Pages 2 0 R /Lang ({lang}) /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R \
             /ViewerPreferences << /DisplayDocTitle true >> /Metadata 12 0 R >>"
        ),
    );
    set(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    set(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Tabs /S /Annots [9 0 R 10 0 R] /StructParents 0 \
         /Resources << /Font << /F1 5 0 R >> /XObject << /Im 11 0 R >> >> >>",
    );
    set(4, &stream(CONTENT));
    set(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FontDescriptor 13 0 R >>");
    set(6, &format!("<< /Type /StructTreeRoot /K 7 0 R /RoleMap << /Heading1 /H1 /Aside /{aside} >> /ParentTree 14 0 R /ParentTreeNextKey 3 >>"));
    set(7, "<< /Type /StructElem /S /Document /P 6 0 R /Pg 3 0 R /K [15 0 R 16 0 R 17 0 R 29 0 R 23 0 R 27 0 R] >>");
    set(8, "<< /Title (Quarterly report) >>");
    set(
        9,
        "<< /Type /Annot /Subtype /Link /Rect [20 20 80 40] /Contents (Example website) /StructParent 1 /A << /S /URI /URI (https://example.com) >> >>",
    );
    set(10, &format!("<< /Type /Annot /Subtype /Widget /FT /Tx /T ({field}) /TU (Your name) /Rect [20 60 120 80] /StructParent 2 >>"));
    set(
        11,
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n\u{0}\nendstream",
    );
    set(12, &xmp);
    set(13, "<< /Type /FontDescriptor /FontName /Helvetica /Flags 32 /FontFile 24 0 R >>");
    set(14, "<< /Nums [0 [15 0 R 16 0 R 19 0 R 20 0 R 21 0 R 22 0 R 25 0 R 26 0 R] 1 23 0 R 2 27 0 R] >>");
    set(15, "<< /Type /StructElem /S /Heading1 /P 7 0 R /K 0 >>");
    set(16, "<< /Type /StructElem /S /Figure /P 7 0 R /Alt (A red square) /K 1 >>");
    set(17, "<< /Type /StructElem /S /Table /P 7 0 R /A << /O /Table /Summary (Two columns) >> /K [18 0 R 28 0 R] >>");
    set(18, "<< /Type /StructElem /S /TR /P 17 0 R /K [19 0 R 20 0 R] >>");
    set(19, "<< /Type /StructElem /S /TH /P 18 0 R /A << /O /Table /Scope /Column >> /K 2 >>");
    set(20, "<< /Type /StructElem /S /TH /P 18 0 R /A << /O /Table /Scope /Column >> /K 3 >>");
    set(21, "<< /Type /StructElem /S /TD /P 28 0 R /K 4 >>");
    set(22, "<< /Type /StructElem /S /TD /P 28 0 R /K 5 >>");
    set(23, "<< /Type /StructElem /S /Link /P 7 0 R /K << /Type /OBJR /Obj 9 0 R >> >>");
    set(24, &stream("fake font program"));
    set(25, "<< /Type /StructElem /S /Lbl /P 30 0 R /K 6 >>");
    set(26, "<< /Type /StructElem /S /LBody /P 30 0 R /K 7 >>");
    set(27, "<< /Type /StructElem /S /Form /P 7 0 R /K << /Type /OBJR /Obj 10 0 R >> >>");
    set(28, "<< /Type /StructElem /S /TR /P 17 0 R /K [21 0 R 22 0 R] >>");
    set(29, "<< /Type /StructElem /S /L /P 7 0 R /K [30 0 R] >>");
    set(30, "<< /Type /StructElem /S /LI /P 29 0 R /K [25 0 R 26 0 R] >>");
    build(&v)
}

fn untagged() -> Vec<u8> {
    build(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
        stream("BT /F1 12 Tf 20 20 Td (Plain) Tj ET"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        "null".into(),
        "null".into(),
        "<< /Title (Plain) >>".into(),
    ])
}

fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-structmerge-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.pdf"), good("Span", "name", "en-US")).unwrap();
    std::fs::write(dir.join("b.pdf"), good("P", "name", "en-US")).unwrap();
    std::fs::write(dir.join("plain.pdf"), untagged()).unwrap();
    dir
}

fn auto(dir: &Path) -> Automation {
    Automation::new().with_root(dir).unwrap().with_clock(|| 1_700_000_000)
}

fn ok(a: &mut Automation, tool: &str, args: Value) -> Value {
    match a.call(tool, &args) {
        Ok(mut c) => match c.remove(0) {
            Content::Json(v) => v,
            other => panic!("{tool}: expected JSON, got {other:?}"),
        },
        Err(e) => panic!("{tool} {args}: {e}"),
    }
}

fn open(a: &mut Automation, path: &str) -> u64 {
    ok(a, "doc_open", json!({ "path": path }))["doc"].as_u64().unwrap()
}

/// (depth, standard type) of every element.
fn outline(tree: &Value) -> Vec<(u64, String)> {
    tree["elements"].as_array().unwrap().iter().map(|e| (e["depth"].as_u64().unwrap(), e["standard"].as_str().unwrap_or("?").to_owned())).collect()
}

/// The failed PDF/UA checkpoints.
fn ua_failed(a: &mut Automation, doc: u64) -> BTreeSet<String> {
    let r = ok(a, "pdfua_verify", json!({ "doc": doc }));
    r["counts"].as_object().unwrap().iter().filter(|(_, v)| v.as_u64().unwrap_or(0) > 0).map(|(k, _)| k.clone()).collect()
}

/// The failed Accessibility Checker rules.
fn checker_failed(a: &mut Automation, doc: u64) -> BTreeSet<String> {
    let r = ok(a, "accessibility_check", json!({ "doc": doc }));
    let mut out = BTreeSet::new();
    for rule in r["results"].as_array().unwrap() {
        if rule["status"] == "failed" {
            out.insert(rule["rule"].as_str().unwrap().to_owned());
        }
    }
    out
}

#[test]
fn combined_tagged_files_keep_a_valid_tree_and_pass_the_same_checks() {
    let dir = workdir("combine");
    let mut a = auto(&dir);
    let src = open(&mut a, "a.pdf");
    let src_tree = ok(&mut a, "tags_tree", json!({ "doc": src }));
    let (src_ua, src_checker) = (ua_failed(&mut a, src), checker_failed(&mut a, src));
    assert!(src_ua.is_empty(), "the fixture conforms: {src_ua:?}");

    let r = ok(&mut a, "doc_combine", json!({ "paths": ["a.pdf", "b.pdf"], "out": "ab.pdf", "open": false }));
    let tags = &r["tags"];
    assert_eq!((tags["merged"].as_bool(), tags["marked"].as_bool(), tags["partially_tagged"].as_bool()), (Some(true), Some(true), Some(false)));
    assert_eq!(tags["elements"], 2 * (src_tree["elements"].as_array().unwrap().len() - 1));
    assert_eq!(tags["role_map_conflicts"], json!([{ "role": "Aside", "kept": "Span", "ignored": "P", "source": "b" }]));

    let doc = open(&mut a, "ab.pdf");
    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    assert_eq!((t["tagged"].as_bool(), t["marked"].as_bool(), t["lang"].as_str()), (Some(true), Some(true), Some("en-US")));
    // One Document element holding each file's elements in turn.
    let one: Vec<(u64, String)> = outline(&src_tree).into_iter().skip(1).collect();
    let mut expected = vec![(0, "Document".to_owned())];
    expected.extend(one.iter().cloned());
    expected.extend(one.iter().cloned());
    assert_eq!(outline(&t), expected);
    let pages: Vec<u64> =
        t["elements"].as_array().unwrap().iter().filter_map(|e| e["kids"].as_array()).flatten().filter_map(|k| k["page"].as_u64()).collect();
    assert!(pages.windows(2).all(|w| w[0] <= w[1]) && pages.first() == Some(&1) && pages.last() == Some(&2), "{pages:?}");

    // The checks judge the merged file as they judged the sources, apart from what Combine
    // Files never carries over (the XMP identification and title, the title display).
    let ua = ua_failed(&mut a, doc);
    let checker = checker_failed(&mut a, doc);
    let metadata: BTreeSet<String> = ["06-002", "06-003", "07-001", "06-001"].iter().map(|s| s.to_string()).collect();
    assert!(ua.is_subset(&metadata), "structure checkpoints still pass: {ua:?}");
    let doc_rules: BTreeSet<String> = ["title"].iter().map(|s| s.to_string()).collect();
    assert!(checker.difference(&src_checker).all(|r| doc_rules.contains(r)), "{checker:?} vs {src_checker:?}");

    // Off: the old behaviour, no tags at all.
    let r = ok(&mut a, "doc_combine", json!({ "paths": ["a.pdf", "b.pdf"], "out": "untagged.pdf", "open": false, "merge_tags": false }));
    assert_eq!(r["tags"]["merged"], false);
    let doc = open(&mut a, "untagged.pdf");
    assert_eq!(ok(&mut a, "tags_tree", json!({ "doc": doc }))["tagged"], false);
}

#[test]
fn tagged_with_untagged_and_inserted_pages_report_partial_tagging() {
    let dir = workdir("partial");
    let mut a = auto(&dir);
    let r = ok(&mut a, "doc_combine", json!({ "paths": ["a.pdf", "plain.pdf"], "out": "mixed.pdf", "open": false }));
    let tags = &r["tags"];
    assert_eq!((tags["marked"].as_bool(), tags["partially_tagged"].as_bool()), (Some(false), Some(true)));
    assert_eq!((tags["tagged_parts"].as_u64(), tags["untagged_parts"].as_u64()), (Some(1), Some(1)));
    assert!(tags["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("plain")), "{tags}");
    let doc = open(&mut a, "mixed.pdf");
    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    assert_eq!((t["tagged"].as_bool(), t["marked"].as_bool()), (Some(true), Some(false)));

    // Insert Pages from File: b's page into a, before its page; the conflict is reported.
    let doc = open(&mut a, "a.pdf");
    let before = ok(&mut a, "tags_tree", json!({ "doc": doc }))["elements"].as_array().unwrap().len();
    let r = ok(&mut a, "page_insert_file", json!({ "doc": doc, "path": "b.pdf", "at": 1 }));
    let tags = &r["tags"];
    assert_eq!((tags["marked"].as_bool(), tags["partially_tagged"].as_bool()), (Some(true), Some(false)), "{tags}");
    assert_eq!(tags["role_map_conflicts"][0]["role"], "Aside");
    assert_eq!(tags["elements_in_document"], 2 * before - 1);
    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    let first = t["elements"].as_array().unwrap().iter().find_map(|e| e["kids"].as_array().and_then(|k| k.iter().find_map(|k| k["page"].as_u64())));
    assert_eq!(first, Some(1), "the inserted page's elements come first");
    assert!(ua_failed(&mut a, doc).is_empty(), "still PDF/UA");
    // Undo puts the tree back.
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(ok(&mut a, "tags_tree", json!({ "doc": doc }))["elements"].as_array().unwrap().len(), before);
    // An untagged file: the document is no longer marked tagged.
    let r = ok(&mut a, "page_insert_file", json!({ "doc": doc, "path": "plain.pdf", "at": 2 }));
    assert_eq!((r["tags"]["marked"].as_bool(), r["tags"]["partially_tagged"].as_bool()), (Some(false), Some(true)));
}

#[test]
fn extracting_pages_keeps_only_their_elements() {
    let dir = workdir("extract");
    let mut a = auto(&dir);
    let r = ok(&mut a, "doc_combine", json!({ "paths": ["a.pdf", "plain.pdf", "b.pdf"], "out": "three.pdf", "open": false }));
    assert_eq!(r["tags"]["partially_tagged"], true);
    let doc = open(&mut a, "three.pdf");
    let all = ok(&mut a, "tags_tree", json!({ "doc": doc }))["elements"].as_array().unwrap().len();
    ok(&mut a, "page_extract", json!({ "doc": doc, "pages": [3], "out": "last.pdf", "open": false }));
    let part = open(&mut a, "last.pdf");
    let t = ok(&mut a, "tags_tree", json!({ "doc": part }));
    assert_eq!(t["elements"].as_array().unwrap().len(), (all - 1) / 2 + 1, "one file's elements under one Document");
    assert!(
        t["elements"].as_array().unwrap().iter().filter_map(|e| e["kids"].as_array()).flatten().filter_map(|k| k["page"].as_u64()).all(|p| p == 1)
    );
    // The part came from a partially tagged file, so it isn't marked tagged either.
    assert_eq!(t["marked"], false);
}
