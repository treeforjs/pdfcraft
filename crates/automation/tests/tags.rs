//! End-to-end tests of the Tags panel and Set Alternate Text tools on a synthetic tagged PDF,
//! through save and reopen, with poppler's `pdfinfo -struct` as an external oracle for the
//! structure tree when it is installed.

use std::path::{Path, PathBuf};

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

/// Page 1: H1 text (MCID 0), a figure (custom type Photo → Figure, MCID 1, no alt) and an
/// image painted outside marked content. Page 2: an untagged image, no /StructParents.
fn tagged_pdf() -> Vec<u8> {
    let c1 = "/H1 << /MCID 0 >> BDC BT /F1 12 Tf 20 180 Td (Introduction) Tj ET EMC \
              /Figure << /MCID 1 >> BDC q 40 0 0 30 20 100 cm /Im1 Do Q EMC \
              q 50 0 0 40 100 40 cm /Im1 Do Q";
    let c2 = "q 30 0 0 30 10 10 cm /Im1 Do Q";
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R /Lang (en-US) >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 12 0 R] /Count 2 /MediaBox [0 0 200 200] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /StructParents 0 /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 10 0 R >> >> >>"
            .to_vec(),
        format!("<< /Length {} >>\nstream\n{c1}\nendstream", c1.len()).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        b"<< /Type /StructTreeRoot /K 7 0 R /ParentTree 11 0 R /ParentTreeNextKey 1 /RoleMap << /Photo /Figure >> >>".to_vec(),
        b"<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R] /Lang (en) >>".to_vec(),
        b"<< /Type /StructElem /S /H1 /P 7 0 R /Pg 3 0 R /K 0 >>".to_vec(),
        b"<< /Type /StructElem /S /Photo /P 7 0 R /Pg 3 0 R /K 1 >>".to_vec(),
        b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x7f\nendstream"
            .to_vec(),
        b"<< /Nums [0 [8 0 R 9 0 R]] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 13 0 R /Resources << /XObject << /Im1 10 0 R >> >> >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{c2}\nendstream", c2.len()).into_bytes(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-tags-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tagged.pdf"), tagged_pdf()).unwrap();
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

/// (depth, standard type) of every element, in document order.
fn outline(tree: &Value) -> Vec<(u64, String)> {
    tree["elements"].as_array().unwrap().iter().map(|e| (e["depth"].as_u64().unwrap(), e["standard"].as_str().unwrap().to_owned())).collect()
}

fn obj_of(tree: &Value, ty: &str) -> u64 {
    tree["elements"].as_array().unwrap().iter().find(|e| e["type"] == ty).unwrap()["obj"].as_u64().unwrap()
}

#[test]
fn tags_tree_edits_and_alternate_text_through_tools() {
    let dir = workdir("e2e");
    let original = std::fs::read(dir.join("tagged.pdf")).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "tagged.pdf" }))["doc"].as_u64().unwrap();

    // The tree.
    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    assert_eq!((t["tagged"].as_bool(), t["marked"].as_bool(), t["lang"].as_str()), (Some(true), Some(true), Some("en-US")));
    assert_eq!(outline(&t), [(0, "Document".to_owned()), (1, "H1".to_owned()), (1, "Figure".to_owned())]);
    assert_eq!(t["elements"][2]["type"], "Photo");
    assert_eq!(t["elements"][0]["lang"], "en");
    assert_eq!(t["elements"][0]["kids"], json!([{ "element": 2 }, { "element": 3 }]));
    assert_eq!(t["elements"][1]["kids"], json!([{ "mcid": 0, "page": 1, "stream": null }]));
    assert_eq!(t["role_map"], json!({ "Photo": "Figure" }));
    assert!(t["types"].as_array().unwrap().iter().any(|x| x == "Photo"));
    let (h1, photo) = (obj_of(&t, "H1"), obj_of(&t, "Photo"));

    // What Set Alternate Text walks through.
    let l = ok(&mut a, "alt_text_list", json!({ "doc": doc }));
    assert_eq!(l["missing"], 3, "{l}");
    assert_eq!(l["figures"][0]["figure"].as_u64(), Some(photo));
    assert_eq!(l["figures"][0]["rect"], json!([20.0, 70.0, 60.0, 100.0]), "top-left-origin points");
    assert_eq!(
        l["untagged_images"],
        json!([
            { "page": 1, "image": 1, "name": "Im1", "rect": [100.0, 120.0, 150.0, 160.0] },
            { "page": 2, "image": 1, "name": "Im1", "rect": [10.0, 160.0, 40.0, 190.0] },
        ])
    );
    assert_eq!(l["figures_alt_text"], "failed");

    // Refused changes leave the document alone.
    for bad in [
        json!({ "doc": doc, "obj": h1, "type": "Bogus" }),
        json!({ "doc": doc, "obj": h1, "lang": "not a tag" }),
        json!({ "doc": doc, "obj": 3, "alt": "a page is not an element" }),
    ] {
        assert!(matches!(a.call("tags_set", &bad), Err(ToolError::Failed(_))), "{bad}");
    }
    assert!(matches!(a.call("tags_set", &json!({ "doc": doc, "obj": h1 })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("alt_text_set", &json!({ "doc": doc, "page": 1, "image": 9, "alt": "x" })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("alt_text_set", &json!({ "doc": doc, "figure": photo, "image": 1 })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("alt_text_set", &json!({ "doc": doc, "page": 1, "image": 1, "decorative": true })), Err(ToolError::InvalidArgs(_))));
    assert_eq!(ok(&mut a, "doc_list", json!({}))["documents"][0]["dirty"], false);

    // Tag properties: one undo step for the whole change.
    let r = ok(&mut a, "tags_set", json!({ "doc": doc, "obj": h1, "type": "H2", "lang": "en-GB", "title": "Intro" }));
    assert_eq!((r["type"].as_str(), r["lang"].as_str(), r["title"].as_str()), (Some("H2"), Some("en-GB"), Some("Intro")));
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    assert_eq!((t["elements"][1]["type"].as_str(), t["elements"][1]["lang"].as_str()), (Some("H1"), None));
    ok(&mut a, "tags_set", json!({ "doc": doc, "obj": h1, "type": "H2", "lang": "en-GB", "title": "Intro" }));

    // Alternate text for the figure and the untagged images; the checker passes afterwards.
    let l = ok(&mut a, "alt_text_set", json!({ "doc": doc, "figure": photo, "alt": "A bar chart" }));
    assert_eq!((l["missing"].as_u64(), l["figures_alt_text"].as_str()), (Some(2), Some("passed")));
    let l = ok(&mut a, "alt_text_set", json!({ "doc": doc, "page": 2, "image": 1, "alt": "A logo" }));
    assert_eq!(l["missing"], 1);
    let l = ok(&mut a, "alt_text_set", json!({ "doc": doc, "page": 1, "image": 1, "alt": "A photo" }));
    assert_eq!((l["missing"].as_u64(), l["untagged_images"].as_array().map(Vec::len)), (Some(0), Some(0)));
    let all = ok(&mut a, "alt_text_list", json!({ "doc": doc, "all": true }));
    assert_eq!(all["figures"].as_array().unwrap().len(), 3);
    let check = ok(&mut a, "accessibility_check", json!({ "doc": doc, "rules": ["figures-alt-text", "tagged-content", "tagged-pdf"] }));
    assert_eq!(check["failed"], 0, "{check}");

    // Saved in place (incrementally) and reopened.
    let saved = ok(&mut a, "doc_save", json!({ "doc": doc }));
    assert_eq!(saved["incremental"], true);
    let bytes = std::fs::read(dir.join("tagged.pdf")).unwrap();
    assert!(bytes.starts_with(&original), "an incremental update keeps the original bytes");
    ok(&mut a, "doc_close", json!({ "doc": doc }));
    let doc = ok(&mut a, "doc_open", json!({ "path": "tagged.pdf" }))["doc"].as_u64().unwrap();
    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    let expected: Vec<(u64, String)> =
        [(0, "Document"), (1, "H2"), (1, "Figure"), (1, "Figure"), (1, "Figure")].iter().map(|(d, s)| (*d, s.to_string())).collect();
    assert_eq!(outline(&t), expected);
    let alts: Vec<&str> = t["elements"].as_array().unwrap().iter().filter_map(|e| e["alt"].as_str()).collect();
    assert_eq!(alts, ["A bar chart", "A logo", "A photo"]);
    assert_eq!((t["elements"][1]["title"].as_str(), t["elements"][1]["lang"].as_str()), (Some("Intro"), Some("en-GB")));
    // The second page got a parent-tree key; its figure's content is on page 2.
    assert_eq!(t["elements"][3]["kids"], json!([{ "mcid": 0, "page": 2, "stream": null }]));
    assert_eq!(ok(&mut a, "alt_text_list", json!({ "doc": doc }))["missing"], 0);
    let check = ok(&mut a, "accessibility_check", json!({ "doc": doc, "rules": ["figures-alt-text", "tagged-content"] }));
    assert_eq!(check["failed"], 0, "{check}");

    // External oracle: poppler reads the same tree from the saved file.
    match std::process::Command::new("pdfinfo").arg("-struct").arg(dir.join("tagged.pdf")).output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let theirs: Vec<(u64, String)> = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    let indent = l.len() - l.trim_start().len();
                    ((indent / 2) as u64, l.split_whitespace().next().unwrap_or("").to_owned())
                })
                .collect();
            assert_eq!(theirs, expected, "pdfinfo -struct:\n{text}");
            eprintln!("pdfinfo -struct agrees:\n{text}");
        }
        other => eprintln!("pdfinfo is not available ({:?}); the external oracle was skipped", other.map(|o| o.status)),
    }
}

#[test]
fn alternate_text_for_an_image_in_an_untagged_document() {
    let dir = workdir("untagged");
    let body = "q 50 0 0 50 10 10 cm /Im1 Do Q";
    let pdf = format!(
        "%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
         3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R >> >> >> endobj\n\
         4 0 obj << /Length {} >> stream\n{body}\nendstream endobj\n\
         5 0 obj << /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >> stream\n\x7f\nendstream endobj\n\
         trailer << /Root 1 0 R >>\n%%EOF",
        body.len()
    );
    std::fs::write(dir.join("plain.pdf"), pdf).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "plain.pdf" }))["doc"].as_u64().unwrap();
    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    assert_eq!((t["tagged"].as_bool(), t["count"].as_u64()), (Some(false), Some(0)));
    let l = ok(&mut a, "alt_text_set", json!({ "doc": doc, "page": 1, "image": 1, "alt": "A logo" }));
    assert_eq!(l["missing"], 0);
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "out.pdf" }));
    let doc = ok(&mut a, "doc_open", json!({ "path": "out.pdf" }))["doc"].as_u64().unwrap();
    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    assert_eq!(outline(&t), [(0, "Document".to_owned()), (1, "Figure".to_owned())]);
    assert_eq!(t["elements"][1]["alt"], "A logo");
}

/// Two pages of headings. Page 1: H1 "Introduction" (MCID 0, at the top), a paragraph, and an H2
/// whose text is split between its own MCID 2 and a Span's MCID 3 ("Back" + "ground"). Page 2:
/// an H1 with /ActualText (its painted text is ignored), a custom type mapped to H3, and an H2
/// with no text at all (skipped). The page has a second H1 nested in a Sect as plain H.
fn headings_pdf() -> Vec<u8> {
    let c1 = "/H1 << /MCID 0 >> BDC BT /F1 18 Tf 20 360 Td (Introduction) Tj ET EMC \
              /P << /MCID 1 >> BDC BT /F1 10 Tf 20 330 Td (Body text.) Tj ET EMC \
              /H2 << /MCID 2 >> BDC BT /F1 14 Tf 20 300 Td (Back) Tj ET EMC \
              /Span << /MCID 3 >> BDC BT /F1 14 Tf 52 300 Td (ground) Tj ET EMC";
    let c2 = "/H1 << /MCID 0 >> BDC BT /F1 18 Tf 20 360 Td (Painted) Tj ET EMC \
              /Subhead << /MCID 1 >> BDC BT /F1 12 Tf 20 200 Td (Detail) Tj ET EMC \
              /H << /MCID 2 >> BDC BT /F1 12 Tf 20 100 Td (Appendix) Tj ET EMC";
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R >>".to_vec(), // 1
        b"<< /Type /Pages /Kids [3 0 R 12 0 R] /Count 2 /MediaBox [0 0 300 400] >>".to_vec(),             // 2
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /StructParents 0 /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(), // 3
        format!("<< /Length {} >>\nstream\n{c1}\nendstream", c1.len()).into_bytes(),                      // 4
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),                               // 5
        b"<< /Type /StructTreeRoot /K 7 0 R /RoleMap << /Subhead /H3 >> >>".to_vec(),                     // 6
        b"<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R 10 0 R 14 0 R 15 0 R 16 0 R 11 0 R] >>".to_vec(), // 7
        b"<< /Type /StructElem /S /H1 /P 7 0 R /Pg 3 0 R /K 0 >>".to_vec(),                               // 8
        b"<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K 1 >>".to_vec(),                                // 9
        b"<< /Type /StructElem /S /H2 /P 7 0 R /Pg 3 0 R /K [2 17 0 R] >>".to_vec(),                      // 10
        b"<< /Type /StructElem /S /Sect /P 7 0 R /K 18 0 R >>".to_vec(),                                  // 11
        b"<< /Type /Page /Parent 2 0 R /Contents 13 0 R /StructParents 1 /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(), // 12
        format!("<< /Length {} >>\nstream\n{c2}\nendstream", c2.len()).into_bytes(),                      // 13
        b"<< /Type /StructElem /S /H1 /P 7 0 R /Pg 12 0 R /ActualText (Results) /K 0 >>".to_vec(),        // 14
        b"<< /Type /StructElem /S /Subhead /P 7 0 R /Pg 12 0 R /K 1 >>".to_vec(),                         // 15
        b"<< /Type /StructElem /S /H2 /P 7 0 R /Pg 12 0 R >>".to_vec(),                                   // 16: no text
        b"<< /Type /StructElem /S /Span /P 10 0 R /Pg 3 0 R /K 3 >>".to_vec(),                            // 17
        b"<< /Type /StructElem /S /H /P 11 0 R /Pg 12 0 R /K [2 7 0 R] >>".to_vec(),                      // 18: loops back to the root
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// (depth, title, 1-based page) of every bookmark in a `bookmark_list`-style tree.
fn marks(tree: &Value) -> Vec<(usize, String, u64)> {
    fn go(items: &Value, depth: usize, out: &mut Vec<(usize, String, u64)>) {
        for b in items.as_array().unwrap() {
            out.push((depth, b["title"].as_str().unwrap().to_owned(), b["page"].as_u64().unwrap_or(0)));
            go(&b["children"], depth + 1, out);
        }
    }
    let mut out = Vec::new();
    go(tree, 0, &mut out);
    out
}

#[test]
fn bookmarks_from_structure_headings_through_tools() {
    let dir = workdir("headings");
    std::fs::write(dir.join("headings.pdf"), headings_pdf()).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "headings.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Cover", "page": 1 }));

    let r = ok(&mut a, "bookmark_from_structure", json!({ "doc": doc }));
    assert_eq!(
        (r["added"].as_u64(), r["untitled"].as_u64(), r["unplaced"].as_u64(), r["truncated"].as_bool()),
        (Some(5), Some(1), Some(0), Some(false)),
        "{r}"
    );
    let want = |v: &[(usize, &str, u64)]| v.iter().map(|(d, t, p)| (*d, t.to_string(), *p)).collect::<Vec<_>>();
    let expected = want(&[(0, "Cover", 1), (0, "Introduction", 1), (1, "Background", 1), (0, "Results", 2), (1, "Detail", 2), (0, "Appendix", 2)]);
    assert_eq!(marks(&r["bookmarks"]), expected);
    // Each goes to its heading's top, not just the page.
    let listed = ok(&mut a, "bookmark_list", json!({ "doc": doc }));
    assert_eq!(marks(&listed["bookmarks"]), expected);
    let intro = &listed["bookmarks"][1]["action"];
    assert_eq!(intro["type"], "goto", "{intro}");
    let top = intro["view"]["top"].as_f64().unwrap();
    assert!((370.0..=385.0).contains(&top), "the top of 18 pt text at y 360: {intro}");

    // Saved and reopened, the tree is the same.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "out.pdf" }));
    let reopened = ok(&mut a, "doc_open", json!({ "path": "out.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(marks(&ok(&mut a, "bookmark_list", json!({ "doc": reopened }))["bookmarks"]), expected);

    // Undo, then replace the existing bookmarks with the top level only.
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(marks(&ok(&mut a, "bookmark_list", json!({ "doc": doc }))["bookmarks"]), want(&[(0, "Cover", 1)]));
    let r = ok(&mut a, "bookmark_from_structure", json!({ "doc": doc, "replace": true, "max_level": 1 }));
    assert_eq!(marks(&r["bookmarks"]), want(&[(0, "Introduction", 1), (0, "Results", 2), (0, "Appendix", 2)]));

    // Bad arguments are refused before anything changes.
    for bad in [json!({ "doc": doc, "max_level": 0 }), json!({ "doc": doc, "max_level": 65 }), json!({ "doc": doc, "replace": "yes" })] {
        assert!(matches!(a.call("bookmark_from_structure", &bad), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
    assert_eq!(marks(&ok(&mut a, "bookmark_list", json!({ "doc": doc }))["bookmarks"]).len(), 3);
}

#[test]
fn bookmarks_from_structure_refuse_untagged_documents_and_documents_without_headings() {
    let dir = workdir("no-headings");
    let mut a = auto(&dir);
    // tagged.pdf has one H1 (and a figure).
    let tagged = ok(&mut a, "doc_open", json!({ "path": "tagged.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "bookmark_from_structure", json!({ "doc": tagged }));
    assert_eq!(marks(&r["bookmarks"]), [(0, "Introduction".to_owned(), 1)]);
    // An untagged document: a clear error, no change.
    let untagged = ok(&mut a, "doc_create", json!({ "from": "blank", "pages": 1 }))["doc"].as_u64().unwrap();
    match a.call("bookmark_from_structure", &json!({ "doc": untagged })) {
        Err(ToolError::Failed(m)) => assert!(m.contains("not tagged"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(marks(&ok(&mut a, "bookmark_list", json!({ "doc": untagged }))["bookmarks"]).is_empty());
}
