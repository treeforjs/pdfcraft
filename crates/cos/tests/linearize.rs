//! Linearized saves (Fast Web View, ISO 32000 Annex F): synthetic documents written linearized,
//! read back by our reader and by hayro, and checked with `check_linearization`, including
//! hostile and damaged files given to the checker.

use std::sync::Arc;

use pdfcraft_cos::{
    Algorithm, CosError, Document, NewEncryption, Object, SaveOptions, check_linearization, fast_web_view, write_full, write_incremental,
    write_linearized,
};

/// A classic-xref file whose object `i + 1` is `objs[i]`.
fn build(objs: &[String], trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} {trailer} >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn stream(data: &str) -> String {
    format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len())
}

#[derive(Default, Clone, Copy)]
struct Extras {
    outlines: bool,
    annotations: bool,
    form: bool,
}

/// `pages` pages under one tree node that supplies inherited resources (font 4, used by every
/// page). Even pages after the first also draw a shared image (object 5, part 8), odd pages a
/// private one. Optionally outlines, a text annotation on every other page, and a form field
/// whose widget is on the last page.
fn document(pages: usize, extras: Extras) -> Vec<u8> {
    let mut objs: Vec<String> = vec![String::new(); 6];
    let mut kids = Vec::new();
    for p in 0..pages {
        let page_num = objs.len() + 1;
        kids.push(format!("{page_num} 0 R"));
        let content_num = page_num + 1;
        let mut xobjects = String::new();
        let mut draw = String::new();
        let mut extra_objs = Vec::new();
        if p > 0 && p % 2 == 0 {
            xobjects = "/XObject << /Shared 5 0 R >>".into();
            draw = " q 10 0 0 10 0 0 cm /Shared Do Q".into();
        } else if p % 2 == 1 {
            let img = content_num + 1;
            xobjects = format!("/XObject << /Own {img} 0 R >>");
            draw = " q 10 0 0 10 0 0 cm /Own Do Q".into();
            extra_objs.push(format!(
                "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n{}\nendstream",
                char::from(b'a' + (p % 26) as u8)
            ));
        }
        let mut annots = String::new();
        if extras.annotations && p % 2 == 0 {
            let annot = content_num + 1 + extra_objs.len();
            annots = format!("/Annots [{annot} 0 R]");
            extra_objs.push(format!("<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (Note on page {}) /P {page_num} 0 R >>", p + 1));
        }
        if extras.form && p + 1 == pages {
            let widget = content_num + 1 + extra_objs.len();
            annots = format!("/Annots [{widget} 0 R]");
            extra_objs.push(format!("<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (Ada) /Rect [20 20 120 40] /P {page_num} 0 R >>"));
            objs[5] = format!("<< /Fields [{widget} 0 R] >>");
        }
        let resources = if xobjects.is_empty() { String::new() } else { format!("/Resources << /Font << /F1 4 0 R >> {xobjects} >>") };
        objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {content_num} 0 R {resources} {annots} >>"));
        objs.push(stream(&format!("BT /F1 18 Tf 20 100 Td (Page {}) Tj ET{draw}", p + 1)));
        objs.extend(extra_objs);
    }
    let mut catalog = String::from("<< /Type /Catalog /Pages 2 0 R");
    if extras.outlines {
        let root = objs.len() + 1;
        let first = root + 1;
        let last = root + 2;
        let last_page = kids.last().cloned().unwrap_or_default();
        objs.push(format!("<< /Type /Outlines /First {first} 0 R /Last {last} 0 R /Count 2 >>"));
        objs.push(format!("<< /Title (Start) /Parent {root} 0 R /Next {last} 0 R /Dest [{} /Fit] >>", kids[0]));
        objs.push(format!("<< /Title (End) /Parent {root} 0 R /Prev {first} 0 R /Dest [{last_page} /Fit] >>"));
        catalog.push_str(&format!(" /Outlines {root} 0 R /PageMode /UseOutlines"));
    }
    if extras.form {
        catalog.push_str(" /AcroForm 6 0 R");
    } else {
        objs[5] = "<< /Unused true >>".into();
    }
    catalog.push_str(&format!(" /OpenAction [{} /Fit] >>", kids[0]));
    objs[0] = catalog;
    objs[1] = format!("<< /Type /Pages /Kids [{}] /Count {pages} /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> >>", kids.join(" "));
    objs[2] = "<< /Title (Linearization test) /Producer (pdfcraft tests) >>".into();
    objs[3] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into();
    objs[4] =
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\nS\nendstream".into();
    build(&objs, "/Root 1 0 R /Info 3 0 R")
}

fn open(bytes: &[u8]) -> Document {
    Document::open(Arc::new(bytes.to_vec())).expect("opens")
}

fn hayro_pages(bytes: &[u8]) -> usize {
    hayro_syntax::Pdf::new(Arc::new(bytes.to_vec())).expect("hayro opens").pages().len()
}

/// The pages in order, each as its decoded content.
fn page_contents(doc: &Document) -> Vec<String> {
    let catalog = doc.get(doc.root().unwrap()).as_dict().cloned().unwrap();
    let pages = doc.get(catalog.reference(b"Pages").unwrap()).as_dict().cloned().unwrap();
    pages
        .get(b"Kids")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|k| {
            let page = doc.resolve(k).as_dict().cloned().unwrap();
            match doc.resolve(page.get(b"Contents").unwrap()).as_ref() {
                Object::Stream(s) => String::from_utf8_lossy(&s.decoded().unwrap()).into_owned(),
                _ => panic!("no content stream"),
            }
        })
        .collect()
}

fn linearize(bytes: &[u8]) -> Vec<u8> {
    write_linearized(&open(bytes), &SaveOptions::default()).expect("linearizes")
}

/// Linearize `source` and check everything a reader relies on.
fn assert_linearized(source: &[u8], pages: usize) -> Vec<u8> {
    let out = linearize(source);
    let report = check_linearization(&out, None);
    assert!(report.linearized, "{report:?}");
    assert!(report.valid, "{:#?}", report.problems);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    let params = report.params.clone().unwrap();
    assert_eq!(params.page_count, Some(pages as i64));
    assert_eq!(params.file_length, Some(out.len() as i64));
    assert_eq!(report.pages_found, Some(pages));
    assert!(fast_web_view(&out));
    // The parameter dictionary is the first object, within the first 1024 bytes.
    let head = String::from_utf8_lossy(&out[..out.len().min(1024)]).into_owned();
    let lin_at = head.find("/Linearized 1").unwrap();
    assert!(head[..lin_at].matches(" obj").count() == 1, "{head}");
    // Same pages, same content, by our reader and by hayro.
    let reopened = open(&out);
    assert!(reopened.repair_log().is_empty(), "{:?}", reopened.repair_log());
    assert_eq!(page_contents(&reopened), page_contents(&open(source)));
    assert_eq!(hayro_pages(&out), pages);
    // /O is the first page.
    let catalog = reopened.get(reopened.root().unwrap()).as_dict().cloned().unwrap();
    let kids = reopened.get(catalog.reference(b"Pages").unwrap()).as_dict().cloned().unwrap();
    let first = kids.get(b"Kids").unwrap().as_array().unwrap()[0].as_ref().unwrap();
    assert_eq!(params.first_page_object, Some(i64::from(first.num)));
    // The independent checker, when installed.
    qpdf_agrees(&out);
    out
}

/// `qpdf --check-linearization` as an optional oracle: skipped when qpdf isn't installed.
fn qpdf_agrees(bytes: &[u8]) {
    let Ok(probe) = std::process::Command::new("qpdf").arg("--version").output() else { return };
    if !probe.status.success() {
        return;
    }
    let path = std::env::temp_dir().join(format!("pdfcraft-lin-{}-{}.pdf", std::process::id(), bytes.len()));
    std::fs::write(&path, bytes).unwrap();
    let out = std::process::Command::new("qpdf").arg("--check-linearization").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && !text.contains("WARNING"), "qpdf: {text}");
}

#[test]
fn multi_page_document_with_shared_resources_is_linearized() {
    let source = document(6, Extras::default());
    let out = assert_linearized(&source, 6);
    // The font every page inherits is with page 1; the image pages 3 and 5 share is in part 8.
    let report = check_linearization(&out, None);
    let params = report.params.unwrap();
    let e = params.first_page_end.unwrap() as usize;
    assert!(find(&out[..e], b"/Helvetica").is_some(), "the inherited font is in the first-page section");
    assert!(find(&out[..e], b"(Page 2)").is_none(), "page 2's content comes after the first page");
    assert!(find(&out[e..], b"(Page 2)").is_some());
    // Inherited attributes were copied into the pages.
    let doc = open(&out);
    let catalog = doc.get(doc.root().unwrap()).as_dict().cloned().unwrap();
    let tree = doc.get(catalog.reference(b"Pages").unwrap()).as_dict().cloned().unwrap();
    assert!(!tree.contains(b"Resources") && !tree.contains(b"MediaBox"));
    let first = doc.resolve(&tree.get(b"Kids").unwrap().as_array().unwrap()[0]).as_dict().cloned().unwrap();
    assert!(first.contains(b"Resources") && first.contains(b"MediaBox"));
    let text = String::from_utf8_lossy(&out);
    // Classic tables, no object streams.
    assert!(!text.contains("/ObjStm") && !text.contains("/XRef"));
}

#[test]
fn single_page_document_is_linearized() {
    assert_linearized(&document(1, Extras::default()), 1);
}

#[test]
fn many_pages_are_linearized() {
    assert_linearized(&document(300, Extras { annotations: true, ..Extras::default() }), 300);
}

#[test]
fn outlines_annotations_and_forms_survive_linearization() {
    let out = assert_linearized(&document(5, Extras { outlines: true, annotations: true, form: true }), 5);
    let doc = open(&out);
    let catalog = doc.get(doc.root().unwrap()).as_dict().cloned().unwrap();
    let outlines = doc.get(catalog.reference(b"Outlines").unwrap()).as_dict().cloned().unwrap();
    assert_eq!(outlines.int(b"Count"), Some(2));
    let form = doc.get(catalog.reference(b"AcroForm").unwrap()).as_dict().cloned().unwrap();
    let widget = doc.resolve(&form.get(b"Fields").unwrap().as_array().unwrap()[0]).as_dict().cloned().unwrap();
    assert_eq!(widget.get(b"V").and_then(|v| v.as_string()).map(|s| s.to_text()), Some("Ada".into()));
    // The widget is needed to open the document (the form), so it's in the first-page section.
    let e = check_linearization(&out, None).params.unwrap().first_page_end.unwrap() as usize;
    assert!(String::from_utf8_lossy(&out[..e]).contains("/T (name)"));
    let info = doc.get(doc.trailer().reference(b"Info").unwrap()).as_dict().cloned().unwrap();
    assert_eq!(info.get(b"Title").and_then(|v| v.as_string()).map(|s| s.to_text()), Some("Linearization test".into()));
}

#[test]
fn linearizing_twice_and_after_edits_stays_valid() {
    let once = linearize(&document(4, Extras::default()));
    let twice = assert_linearized(&once, 4);
    // An edit then a linearized save.
    let mut doc = open(&twice);
    let info = doc.trailer().reference(b"Info").unwrap();
    doc.update_dict(info, |d| d.set(b"Title".to_vec(), Object::String(pdfcraft_cos::PdfString::text("Edited")))).unwrap();
    let edited = write_linearized(&doc, &SaveOptions { mod_date: Some("D:20260101000000Z".into()), ..SaveOptions::default() }).unwrap();
    let report = check_linearization(&edited, None);
    assert!(report.valid, "{:?}", report.problems);
    let reopened = open(&edited);
    let info = reopened.get(reopened.trailer().reference(b"Info").unwrap()).as_dict().cloned().unwrap();
    assert_eq!(info.get(b"Title").and_then(|v| v.as_string()).map(|s| s.to_text()), Some("Edited".into()));
}

#[test]
fn an_incremental_update_ends_fast_web_view() {
    let out = linearize(&document(3, Extras::default()));
    let mut doc = open(&out);
    let info = doc.trailer().reference(b"Info").unwrap();
    doc.update_dict(info, |d| d.set(b"Subject".to_vec(), Object::String(pdfcraft_cos::PdfString::text("later")))).unwrap();
    let updated = write_incremental(&doc, &SaveOptions::default()).unwrap();
    assert!(!fast_web_view(&updated));
    let report = check_linearization(&updated, None);
    assert!(report.linearized && !report.valid);
    assert!(report.problems.iter().any(|p| p.contains("/L")), "{:?}", report.problems);
    // Still a readable file with every page.
    assert_eq!(page_contents(&open(&updated)).len(), 3);
}

#[test]
fn ordinary_files_are_not_linearized() {
    let plain = document(2, Extras::default());
    let report = check_linearization(&plain, None);
    assert!(!report.linearized && !report.valid && report.problems.is_empty());
    assert!(!fast_web_view(&plain));
    let packed = write_full(&open(&plain), &SaveOptions::default()).unwrap();
    assert!(!check_linearization(&packed, None).linearized);
    assert!(!check_linearization(b"", None).linearized);
    assert!(!check_linearization(b"not a pdf at all", None).linearized);
}

#[test]
fn encrypted_documents_are_linearized_with_their_security() {
    for alg in [Algorithm::Rc4_128, Algorithm::Aes256] {
        let mut doc = open(&document(4, Extras::default()));
        doc.set_encryption(&NewEncryption {
            algorithm: alg,
            user_password: "user",
            owner_password: "owner",
            permissions: -1,
            encrypt_metadata: true,
            seed: [7; 32],
        })
        .unwrap();
        let out = write_linearized(&doc, &SaveOptions::default()).unwrap();
        assert!(matches!(Document::open(Arc::new(out.clone())), Err(CosError::NeedsPassword)));
        let opened = Document::open_with_password(Arc::new(out.clone()), Some("user")).unwrap();
        assert_eq!(page_contents(&opened).len(), 4);
        assert!(page_contents(&opened)[1].contains("(Page 2)"));
        // With the password the hint tables are checked too; without it, everything else is.
        let full = check_linearization(&out, Some(&opened));
        assert!(full.valid && full.notes.is_empty(), "{alg:?}: {full:?}");
        let partial = check_linearization(&out, None);
        assert!(partial.valid, "{alg:?}: {:?}", partial.problems);
        assert!(partial.notes.iter().any(|n| n.contains("encrypted")), "{:?}", partial.notes);
        // Saving it again (now opened with its password) keeps it linearized and protected.
        let again = write_linearized(&opened, &SaveOptions::default()).unwrap();
        let reopened = Document::open_with_password(Arc::new(again.clone()), Some("owner")).unwrap();
        assert!(check_linearization(&again, Some(&reopened)).valid);
        qpdf_agrees_encrypted(&again);
    }
}

/// qpdf needs the password for an encrypted file; only checks that it opens when installed.
fn qpdf_agrees_encrypted(bytes: &[u8]) {
    let Ok(probe) = std::process::Command::new("qpdf").arg("--version").output() else { return };
    if !probe.status.success() {
        return;
    }
    let path = std::env::temp_dir().join(format!("pdfcraft-lin-enc-{}-{}.pdf", std::process::id(), bytes.len()));
    std::fs::write(&path, bytes).unwrap();
    let out = std::process::Command::new("qpdf").arg("--password=user").arg("--check-linearization").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && !text.contains("WARNING"), "qpdf: {text}");
}

#[test]
fn documents_without_pages_or_with_a_broken_page_tree_are_refused() {
    let no_pages = build(&["<< /Type /Catalog /Pages 2 0 R >>".into(), "<< /Type /Pages /Kids [] /Count 0 >>".into()], "/Root 1 0 R");
    assert!(matches!(write_linearized(&open(&no_pages), &SaveOptions::default()), Err(CosError::Linearize(_))));
    let twice = build(
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".into(),
        ],
        "/Root 1 0 R",
    );
    let err = write_linearized(&open(&twice), &SaveOptions::default()).unwrap_err();
    assert!(err.to_string().contains("more than once"), "{err}");
    let looped = build(
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [4 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".into(),
            "<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 1 >>".into(),
        ],
        "/Root 1 0 R",
    );
    assert!(matches!(write_linearized(&open(&looped), &SaveOptions::default()), Err(CosError::Linearize(_))));
    let no_catalog = build(&["<< /Type /Pages /Kids [] /Count 0 >>".into()], "");
    if let Ok(doc) = Document::open(Arc::new(no_catalog)) {
        assert!(write_linearized(&doc, &SaveOptions::default()).is_err());
    }
}

/// Every damaged variant is examined without panicking, and none passes as valid.
#[test]
fn the_checker_survives_truncation_and_damage() {
    let good = linearize(&document(5, Extras { annotations: true, ..Extras::default() }));
    for cut in (0..good.len()).step_by(37).chain([good.len() - 1, good.len() - 6]) {
        let report = check_linearization(&good[..cut], None);
        assert!(!report.valid, "truncated at {cut} passed");
    }
    // Bogus /L (same width, so nothing else moves).
    let l = format!("/L {}", good.len());
    let at = find(&good, l.as_bytes()).unwrap();
    let mut bogus = good.clone();
    bogus[at + 3] = if bogus[at + 3] == b'9' { b'1' } else { b'9' };
    let report = check_linearization(&bogus, None);
    assert!(report.linearized && !report.valid);
    assert!(report.problems.iter().any(|p| p.contains("/L")), "{:?}", report.problems);
    // Every other parameter wrong in turn.
    for key in ["/O ", "/E ", "/N ", "/T ", "/H["] {
        let at = find(&good, key.as_bytes()).unwrap() + key.len();
        let mut bad = good.clone();
        bad[at] = if bad[at] == b'1' { b'2' } else { b'1' };
        let report = check_linearization(&bad, None);
        assert!(!report.valid, "{key} damage passed");
    }
    // Garbage in the hint stream (keeping its length) and an empty /S.
    let report = check_linearization(&good, None).params.unwrap();
    let (h, len) = (report.hint_offset.unwrap() as usize, report.hint_length.unwrap() as usize);
    let body = find(&good[h..], b"stream\n").unwrap() + h + 7;
    let end = find(&good[h..], b"\nendstream").unwrap() + h;
    for fill in [0x00u8, 0xFF, 0x78] {
        let mut garbage = good.clone();
        garbage[body..end].fill(fill);
        let report = check_linearization(&garbage, None);
        assert!(!report.valid, "garbage hint stream passed");
    }
    let mut no_s = good.clone();
    let s_at = find(&no_s[h..h + len], b"/S ").unwrap() + h;
    no_s[s_at + 1] = b'X';
    assert!(!check_linearization(&no_s, None).valid);
    // Random byte damage anywhere.
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for _ in 0..400 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let mut bad = good.clone();
        let at = (x as usize) % bad.len();
        bad[at] = (x >> 32) as u8;
        let _ = check_linearization(&bad, None);
        let _ = fast_web_view(&bad);
    }
}

/// Hand-made hostile parameter dictionaries: huge, negative and wrongly typed values, and hint
/// tables that claim enormous counts with zero-width fields.
#[test]
fn hostile_parameters_and_hint_tables_are_reported() {
    let header = b"%PDF-1.7\n";
    for dict in [
        "<< /Linearized 1 >>",
        "<< /Linearized (x) /L -5 /H [1] /O -1 /E 99999999999999999999 /N 1 /T 0 >>",
        "<< /Linearized 1 /L 10 /H [9223372036854775807 9223372036854775807] /O 1 /E 1 /N 1 /T 1 >>",
        "<< /Linearized 1 /L 200 /H [5 5] /O 1 /E 1 /N 4294967295 /T 18446744073709551615 >>",
    ] {
        let mut f = header.to_vec();
        f.extend_from_slice(
            format!("1 0 obj\n{dict}\nendobj\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 2 /Prev 0 >>\nstartxref\n9\n%%EOF\n").as_bytes(),
        );
        let report = check_linearization(&f, None);
        assert!(report.linearized && !report.valid, "{dict}: {report:?}");
    }
    // A hint stream whose page table claims 2^32 - 1 shared references per page in zero bits.
    let good = linearize(&document(2, Extras::default()));
    let p = check_linearization(&good, None).params.unwrap();
    let (h, len) = (p.hint_offset.unwrap() as usize, p.hint_length.unwrap() as usize);
    let mut table = vec![0u8; 36];
    table[28..30].copy_from_slice(&32u16.to_be_bytes()); // item 10, bits per shared-reference count: 32
    let mut data = table.clone();
    data.extend_from_slice(&[0xFF; 8]); // two pages, each "4294967295 references"
    let compressed = pdfcraft_filters::encode_flate(&data);
    let num = std::str::from_utf8(&good[h..h + 12]).unwrap().split(' ').next().unwrap().to_string();
    let mut replacement = format!("{num} 0 obj\n<< /S 44 /Filter /FlateDecode /Length {} >>\nstream\n", compressed.len()).into_bytes();
    replacement.extend_from_slice(&compressed);
    replacement.extend_from_slice(b"\nendstream\nendobj\n");
    assert!(replacement.len() <= len);
    let mut hostile = good.clone();
    let pad = len - replacement.len();
    replacement.splice(replacement.len() - 1..replacement.len() - 1, std::iter::repeat_n(b' ', pad));
    hostile[h..h + len].copy_from_slice(&replacement);
    let report = check_linearization(&hostile, None);
    assert!(!report.valid);
    assert!(report.problems.iter().any(|p| p.contains("shared object references")), "{:?}", report.problems);
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
