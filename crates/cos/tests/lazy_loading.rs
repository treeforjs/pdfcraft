//! Very large and hostile documents (`core.lazy-loading`): the parse caches stay bounded, and
//! broken cross-reference data, self-referencing object streams and truncation end in an error
//! or a recovered document, never a crash or unbounded memory. Fixtures are generated here.

use std::sync::Arc;

use pdfcraft_cos::{Document, ObjRef, Object, SaveOptions, write_full};

/// A classic-xref file whose object `i + 1` is `bodies[i]`; `startxref` is `xref_value` when
/// given (to point it somewhere wrong).
fn classic(bodies: &[String], trailer: &str, xref_value: Option<&str>) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(bodies.len());
    for (i, b) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{b}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    let start = xref_value.map_or_else(|| xref.to_string(), str::to_owned);
    out.extend_from_slice(format!("trailer\n<< /Size {} {trailer} >>\nstartxref\n{start}\n%%EOF\n", bodies.len() + 1).as_bytes());
    out
}

/// Catalog 1, page tree 2, then `pages` pages (each with a contents stream) and `extra` small
/// dictionaries nothing references.
fn many_objects(pages: usize, extra: usize) -> Vec<String> {
    let first_page = 3;
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", first_page + 2 * i)).collect();
    let mut bodies = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string(), format!("<< /Type /Pages /Count {pages} /Kids [{}] >>", kids.join(" "))];
    for i in 0..pages {
        bodies.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R >>", first_page + 2 * i + 1));
        let text = format!("BT /F1 12 Tf 72 720 Td (page {}) Tj ET", i + 1);
        bodies.push(format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()));
    }
    for i in 0..extra {
        bodies.push(format!("<< /Value {i} /Name (object {i}) /List [1 2 3 {i}] >>"));
    }
    bodies
}

fn value_of(doc: &Document, num: u32) -> Option<i64> {
    doc.get(ObjRef::new(num, 0)).as_dict().and_then(|d| d.int(b"Value"))
}

#[test]
fn walking_every_object_of_a_large_file_keeps_the_caches_bounded() {
    let pages = 2_000;
    let extra = 70_000;
    let bytes = classic(&many_objects(pages, extra), "/Root 1 0 R", None);
    let doc = Document::open(Arc::new(bytes)).unwrap();
    // Opening reads the cross-reference data and the catalog, not the objects.
    assert!(doc.cache_footprint().objects < 16, "{:?}", doc.cache_footprint());
    let first_extra = (3 + 2 * pages) as u32;
    let limit = Document::CACHE_LIMIT;
    for num in doc.object_numbers() {
        doc.get(ObjRef::new(num, 0));
        let now = doc.cache_footprint();
        assert!(now.objects <= limit.objects && now.bytes <= limit.bytes, "{now:?} over {limit:?}");
    }
    assert!(doc.cache_footprint().objects < extra, "the cache did not keep every object");
    // Evicted objects read again, the same.
    assert_eq!(value_of(&doc, first_extra), Some(0));
    assert_eq!(value_of(&doc, first_extra + 12_345), Some(12_345));
    // Objects in use survive a walk: the catalog is read again and again.
    for num in doc.object_numbers() {
        doc.get(ObjRef::new(num, 0));
        assert!(doc.dict(&Object::Ref(ObjRef::new(1, 0))).is_some_and(|d| d.contains(b"Pages")));
    }
}

#[test]
fn a_broken_xref_on_a_large_file_is_reconstructed_within_the_cache_bounds() {
    let pages = 3_000;
    for wrong in ["999999999", "12", "abc", "-5"] {
        let bytes = classic(&many_objects(pages, 20_000), "/Root 1 0 R", Some(wrong));
        let doc = Document::open(Arc::new(bytes)).unwrap();
        assert!(doc.repair_log().iter().any(|l| l.contains("rebuilt cross-reference index")), "{wrong}: {:?}", doc.repair_log());
        let limit = Document::CACHE_LIMIT;
        let now = doc.cache_footprint();
        assert!(now.objects <= limit.objects && now.bytes <= limit.bytes, "{now:?}");
        let tree = doc.dict(&Object::Ref(ObjRef::new(2, 0))).unwrap();
        assert_eq!(tree.int(b"Count"), Some(pages as i64));
        let last = doc.dict(&Object::Ref(ObjRef::new((3 + 2 * (pages - 1)) as u32, 0))).unwrap();
        assert_eq!(last.name(b"Type"), Some(&b"Page"[..]));
    }
}

/// A file with a cross-reference stream (`/W [1 4 2]`) whose rows are `rows` (type, field 2,
/// field 3) for objects 0..; `objects` are written first, at the offsets type-1 rows name by
/// index into it (field 2 = index of the object in `objects`).
fn with_xref_stream(objects: &[(u32, String)], rows: &[(u8, u32, u16)]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (num, body) in objects {
        offsets.push(out.len());
        out.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref = out.len();
    let mut data = Vec::new();
    for (kind, a, b) in rows {
        let a = if *kind == 1 { offsets.get(*a as usize).copied().unwrap_or(0) as u32 } else { *a };
        data.push(*kind);
        data.extend_from_slice(&a.to_be_bytes());
        data.extend_from_slice(&b.to_be_bytes());
    }
    let num = rows.len();
    out.extend_from_slice(
        format!("{num} 0 obj\n<< /Type /XRef /Size {} /W [1 4 2] /Root 1 0 R /Length {} >>\nstream\n", num + 1, data.len() + 7).as_bytes(),
    );
    out.extend_from_slice(&data);
    // The stream's own row (type 1, its offset) after the others.
    out.push(1);
    out.extend_from_slice(&(xref as u32).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

#[test]
fn object_streams_that_contain_themselves_read_as_missing() {
    // 1 catalog, 2 pages, 3 page (in the file). 4 claims to be object 0 of object stream 4
    // (itself); 5 and 6 are each other's object streams; 7 is in a stream that is a plain
    // dictionary; 8 is in an object stream whose /Length is a reference to that stream itself.
    let objstm = |n: u32, contents: &str| {
        let index = format!("{n} 0 ");
        format!("<< /Type /ObjStm /N 1 /First {} /Length {} >>\nstream\n{index}{contents}\nendstream", index.len(), index.len() + contents.len())
    };
    let objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_string()),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Extra [4 0 R 5 0 R 6 0 R 7 0 R 8 0 R] >>".to_string()),
        (9, "<< /Not (an object stream) >>".to_string()),
        (10, "<< /Type /ObjStm /N 1 /First 4 /Length 10 0 R >>\nstream\n8 0 << >>\nendstream".to_string()),
        (11, objstm(11, "<< /Self true >>")),
    ];
    let rows = [
        (0, 0, 65535),
        (1, 0, 0),  // 1
        (1, 1, 0),  // 2
        (1, 2, 0),  // 3
        (2, 4, 0),  // 4: in object stream 4, itself
        (2, 6, 0),  // 5: in 6
        (2, 5, 0),  // 6: in 5
        (2, 9, 0),  // 7: in 9, not an object stream
        (2, 10, 0), // 8: in 10, whose length is itself
        (1, 3, 0),  // 9
        (1, 4, 0),  // 10
        (2, 11, 0), // 11: in object stream 11 (itself), though the file also holds it
    ];
    let bytes = with_xref_stream(&objects, &rows);
    let doc = Document::open(Arc::new(bytes.clone())).unwrap();
    for num in [4, 5, 6, 7, 8, 11] {
        assert!(doc.try_get(num).is_err() || matches!(*doc.get(ObjRef::new(num, 0)), Object::Null | Object::Dict(_)), "object {num}");
    }
    assert!(doc.dict(&Object::Ref(ObjRef::new(3, 0))).is_some(), "the rest of the document reads");
    // Saving copes with the unreadable objects too.
    let _ = write_full(&doc, &SaveOptions::default());
    let _ = pdfcraft_cos::write_incremental(&doc, &SaveOptions::default());
}

#[test]
fn truncated_files_open_or_fail_cleanly() {
    let bodies = many_objects(40, 200);
    let classic_file = classic(&bodies, "/Root 1 0 R", None);
    // The same document with object streams and a cross-reference stream.
    let packed = write_full(&Document::open(Arc::new(classic_file.clone())).unwrap(), &SaveOptions::default()).unwrap();
    for file in [classic_file, packed] {
        let step = (file.len() / 300).max(1);
        let mut cut = 0;
        while cut < file.len() {
            let part = Arc::new(file[..cut].to_vec());
            if let Ok(doc) = Document::open(part) {
                for num in doc.object_numbers() {
                    let _ = doc.try_get(num);
                }
                let limit = Document::CACHE_LIMIT;
                assert!(doc.cache_footprint().objects <= limit.objects);
            }
            cut += step;
        }
    }
}

#[test]
fn huge_claimed_sizes_do_not_allocate_by_the_claim() {
    // An object stream claiming a million objects and an xref subsection claiming ten million
    // entries, in a tiny file.
    let objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Count 2147483647 /Kids [] >>".to_string()),
        (3, "<< /Type /ObjStm /N 1000000 /First 2147483647 /Length 8 >>\nstream\n4 0 5 0 \nendstream".to_string()),
    ];
    let rows = [(0, 0, 65535), (1, 0, 0), (1, 1, 0), (1, 2, 0), (2, 3, 0), (2, 3, 1)];
    let doc = Document::open(Arc::new(with_xref_stream(&objects, &rows))).unwrap();
    assert!(matches!(*doc.get(ObjRef::new(4, 0)), Object::Null) || doc.try_get(4).is_err());
    let _ = doc.try_get(5);
    let mut bytes = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n".to_vec();
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 10000000\n0000000000 65535 f \ntrailer << /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    let doc = Document::open(Arc::new(bytes)).unwrap();
    assert!(doc.dict(&Object::Ref(ObjRef::new(1, 0))).is_some());
}
