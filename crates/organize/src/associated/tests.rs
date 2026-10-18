use std::sync::Arc;

use pdfcraft_cos::{Document, SaveOptions, write_full, write_incremental};

use super::*;

/// Build a classic-xref PDF from numbered object bodies (object n is `objs[n - 1]`).
fn build(objs: &[Vec<u8>], trailer_extra: &str) -> Vec<u8> {
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
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R {trailer_extra}>>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn stream(dict: &str, body: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict} /Length {} >>\nstream\n", body.len()).into_bytes();
    v.extend_from_slice(body);
    v.extend_from_slice(b"\nendstream");
    v
}

/// Two pages; page 1 has an indirect annotation (5) and a direct one; a form XObject (7), a
/// structure element (9) and an existing attachment (10, 11) in /EmbeddedFiles.
fn fixture_objs() -> Vec<Vec<u8>> {
    vec![
        b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 8 0 R /Names << /EmbeddedFiles << /Names [(old.txt) 10 0 R] >> >> >>".to_vec(), // 1
        b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 200 200] >>".to_vec(), // 2
        b"<< /Type /Page /Parent 2 0 R /Contents 6 0 R /Resources << /XObject << /X1 7 0 R >> >> /Annots [5 0 R << /Subtype /Square /Rect [0 0 10 10] >>] >>".to_vec(), // 3
        b"<< /Type /Page /Parent 2 0 R >>".to_vec(),                                // 4
        b"<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (Hi) >>".to_vec(), // 5
        stream("", b"q /X1 Do Q"),                                                   // 6
        stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", b"0 0 10 10 re f"), // 7
        b"<< /Type /StructTreeRoot /K 9 0 R >>".to_vec(),                            // 8
        b"<< /Type /StructElem /S /Figure /P 8 0 R >>".to_vec(),                     // 9
        b"<< /Type /Filespec /F (old.txt) /UF (old.txt) /EF << /F 11 0 R >> >>".to_vec(), // 10
        stream("/Type /EmbeddedFile", b"old"),                                       // 11
    ]
}

fn open(bytes: Vec<u8>) -> Document {
    Document::open(Arc::new(bytes)).unwrap()
}

fn fixture() -> Document {
    open(build(&fixture_objs(), ""))
}

fn file(name: &str, data: &[u8], rel: Relationship) -> NewAssociatedFile {
    NewAssociatedFile {
        name: name.into(),
        data: Arc::new(data.to_vec()),
        mime: Some("text/csv".into()),
        relationship: rel,
        description: Some("Figures".into()),
    }
}

fn reopen(doc: &Document) -> Document {
    let bytes = write_incremental(doc, &SaveOptions::default()).unwrap();
    open(bytes)
}

fn tree_names(doc: &Document) -> Vec<String> {
    tree_entries(doc).0.into_iter().map(|(k, _)| PdfString::literal(k).to_text()).collect()
}

#[test]
fn a_document_file_is_embedded_associated_and_an_attachment() {
    let mut doc = fixture();
    let i =
        add_associated_file(&mut doc, &AfTarget::Document, &file("data.csv", b"a,b\n1,2\n", Relationship::Data), Some("D:20261018120000Z")).unwrap();
    assert_eq!(i, 0);
    let doc = reopen(&doc);
    let list = associated_files(&doc, &AfTarget::Document).unwrap();
    assert!(list.issues.is_empty());
    let [f] = list.files.as_slice() else { panic!("{list:?}") };
    assert_eq!(f.name, "data.csv");
    assert_eq!(f.relationship.as_deref(), Some("Data"));
    assert_eq!(f.mime.as_deref(), Some("text/csv"));
    assert_eq!(f.size, Some(8));
    assert_eq!(f.description.as_deref(), Some("Figures"));
    assert_eq!(f.mod_date.as_deref(), Some("D:20261018120000Z"));
    assert_eq!(f.checksum.as_deref(), Some(hex(&Md5::digest(b"a,b\n1,2\n")).as_str()));
    assert!(f.embedded && f.in_embedded_files && f.problem.is_none());
    assert_eq!(tree_names(&doc), ["data.csv", "old.txt"], "registered next to the existing attachment, sorted");
    let (_, data) = associated_file_data(&doc, &AfTarget::Document, &AfSelector::Name("data.csv".into()), usize::MAX).unwrap();
    assert_eq!(data, b"a,b\n1,2\n");
    // The file specification carries /F, /UF and an /EF with both.
    let fs = doc.get(ObjRef::new(f.object.unwrap(), 0));
    let fs = fs.as_dict().unwrap();
    assert_eq!(fs.name(b"Type"), Some(&b"Filespec"[..]));
    assert!(fs.contains(b"F") && fs.contains(b"UF"));
    let ef = doc.dict(fs.get(b"EF").unwrap()).unwrap();
    assert!(ef.contains(b"F") && ef.contains(b"UF"));
}

#[test]
fn page_annotation_structure_and_xobject_files_stay_out_of_the_name_tree() {
    let mut doc = fixture();
    let targets = [
        AfTarget::Page(1),
        AfTarget::Annotation { page: 0, index: 0 },
        AfTarget::Annotation { page: 0, index: 1 },
        AfTarget::Object(9),
        AfTarget::Object(7),
    ];
    for (n, t) in targets.iter().enumerate() {
        add_associated_file(&mut doc, t, &file(&format!("f{n}.csv"), b"x", Relationship::Source), None).unwrap();
    }
    let doc = reopen(&doc);
    for (n, t) in targets.iter().enumerate() {
        let list = associated_files(&doc, t).unwrap();
        let [f] = list.files.as_slice() else { panic!("{t:?}: {list:?}") };
        assert_eq!(f.name, format!("f{n}.csv"));
        assert_eq!(f.relationship.as_deref(), Some("Source"));
        assert!(!f.in_embedded_files);
    }
    assert_eq!(tree_names(&doc), ["old.txt"]);
    assert!(associated_files(&doc, &AfTarget::Document).unwrap().files.is_empty());
    // The direct annotation became indirect, and the page still lists two annotations.
    let page = crate::pages(&doc).unwrap()[0].obj;
    let (annots, _) = annots(&doc, page);
    assert_eq!(annots.len(), 2);
    assert!(annots.iter().all(|a| a.as_ref().is_some()));
    // The scan finds pages and annotations (not the structure element or XObject).
    let all = all_associated_files(&doc).unwrap();
    let owners: Vec<&AfTarget> = all.iter().map(|(t, _)| t).collect();
    assert_eq!(
        owners,
        [&AfTarget::Document, &AfTarget::Annotation { page: 0, index: 0 }, &AfTarget::Annotation { page: 0, index: 1 }, &AfTarget::Page(1)]
    );
}

#[test]
fn removing_drops_the_entry_and_its_attachment() {
    let mut doc = fixture();
    add_associated_file(&mut doc, &AfTarget::Document, &file("a.csv", b"1", Relationship::Data), None).unwrap();
    add_associated_file(&mut doc, &AfTarget::Document, &file("b.xml", b"2", Relationship::Schema), None).unwrap();
    add_associated_file(&mut doc, &AfTarget::Page(0), &file("p.csv", b"3", Relationship::Supplement), None).unwrap();
    let mut doc = reopen(&doc);
    let gone = remove_associated_file(&mut doc, &AfTarget::Document, &AfSelector::Name("a.csv".into())).unwrap();
    assert_eq!(gone.name, "a.csv");
    let doc2 = reopen(&doc);
    let names: Vec<String> = associated_files(&doc2, &AfTarget::Document).unwrap().files.into_iter().map(|f| f.name).collect();
    assert_eq!(names, ["b.xml"]);
    assert_eq!(tree_names(&doc2), ["b.xml", "old.txt"]);
    let mut doc = doc2;
    remove_associated_file(&mut doc, &AfTarget::Document, &AfSelector::Index(0)).unwrap();
    let root = doc.root().unwrap();
    assert!(!doc.get(root).as_dict().unwrap().contains(b"AF"), "an empty /AF goes away");
    assert_eq!(tree_names(&doc), ["old.txt"], "attachments that weren't associated stay");
    remove_associated_file(&mut doc, &AfTarget::Page(0), &AfSelector::Index(0)).unwrap();
    let doc = open(write_full(&doc, &SaveOptions::default()).unwrap());
    assert!(all_associated_files(&doc).unwrap().iter().all(|(_, l)| l.files.is_empty()));
    assert_eq!(tree_names(&doc), ["old.txt"]);
}

#[test]
fn bad_selectors_targets_and_files_are_refused() {
    let mut doc = fixture();
    add_associated_file(&mut doc, &AfTarget::Document, &file("same.csv", b"1", Relationship::Data), None).unwrap();
    add_associated_file(&mut doc, &AfTarget::Document, &file("same.csv", b"2", Relationship::Data), None).unwrap();
    assert_eq!(tree_names(&doc), ["old.txt", "same.csv", "same.csv (2)"]);
    let before = doc.clone();
    let err = remove_associated_file(&mut doc, &AfTarget::Document, &AfSelector::Name("same.csv".into())).unwrap_err().to_string();
    assert!(err.contains("pass index"), "{err}");
    assert!(remove_associated_file(&mut doc, &AfTarget::Document, &AfSelector::Index(5)).unwrap_err().to_string().contains("has 2"));
    assert!(remove_associated_file(&mut doc, &AfTarget::Document, &AfSelector::Name("nope".into())).is_err());
    assert!(remove_associated_file(&mut doc, &AfTarget::Page(1), &AfSelector::Index(0)).is_err());
    let f = file("x.csv", b"1", Relationship::Data);
    for t in [
        AfTarget::Page(9),
        AfTarget::Annotation { page: 0, index: 7 },
        AfTarget::Object(6),
        AfTarget::Object(1),
        AfTarget::Object(999),
        AfTarget::Object(2),
    ] {
        assert!(add_associated_file(&mut doc, &t, &f, None).is_err(), "{t:?}");
    }
    let bad = [
        NewAssociatedFile { name: "  ".into(), ..f.clone() },
        NewAssociatedFile { name: "a\u{7}b".into(), ..f.clone() },
        NewAssociatedFile { name: "n".repeat(300), ..f.clone() },
        NewAssociatedFile { mime: Some("text plain".into()), ..f.clone() },
        NewAssociatedFile { mime: Some("nope".into()), ..f.clone() },
        NewAssociatedFile { description: Some("d".repeat(5000)), ..f.clone() },
        NewAssociatedFile { data: Arc::new(vec![0; MAX_FILE_BYTES + 1]), ..f.clone() },
    ];
    for b in &bad {
        assert!(add_associated_file(&mut doc, &AfTarget::Document, b, None).is_err());
    }
    assert_eq!(doc.modified_objects(), before.modified_objects(), "refused edits change nothing");
}

#[test]
fn relationship_names_parse_case_insensitively_and_reject_others() {
    for r in Relationship::ALL {
        assert_eq!(Relationship::from_name(r.name()), Some(r));
        assert_eq!(Relationship::from_name(&r.name().to_lowercase()), Some(r));
    }
    assert_eq!(Relationship::from_name("Source "), Some(Relationship::Source));
    assert_eq!(Relationship::from_name("Original"), None);
    assert_eq!(Relationship::from_name(""), None);
    assert!(Relationship::names().contains("EncryptedPayload"));
    assert_eq!(guess_mime("Report.XML"), Some("application/xml"));
    assert_eq!(guess_mime("noext"), None);
    assert!(validate_mime("application/vnd.ms-excel").is_ok());
    assert!(validate_mime("a/b/c").is_err());
}

#[test]
fn hostile_af_arrays_are_reported_not_fatal() {
    // Catalog /AF is a number; page 1 /AF an indirect array that contains itself, a missing
    // object, a string, a page and a filespec whose /EF loops to itself; page 2 /AF refers to
    // a non-array.
    let mut objs = fixture_objs();
    objs[0] = b"<< /Type /Catalog /Pages 2 0 R /AF 42 >>".to_vec();
    objs[2] = b"<< /Type /Page /Parent 2 0 R /AF 12 0 R >>".to_vec();
    objs[3] = b"<< /Type /Page /Parent 2 0 R /AF 5 0 R >>".to_vec();
    objs.push(b"[12 0 R 99 0 R (loose.txt) 3 0 R 13 0 R]".to_vec()); // 12
    objs.push(b"<< /Type /Filespec /UF (loop) /EF << /F 13 0 R >> /AFRelationship /Bogus >>".to_vec()); // 13
    let mut doc = open(build(&objs, ""));
    let d = associated_files(&doc, &AfTarget::Document).unwrap();
    assert!(d.files.is_empty() && d.issues.len() == 1, "{d:?}");
    let p = associated_files(&doc, &AfTarget::Page(0)).unwrap();
    assert_eq!(p.files.len(), 5);
    assert!(p.files[..4].iter().all(|f| f.problem.is_some()), "{p:?}");
    assert_eq!(p.files[2].name, "loose.txt");
    assert_eq!(p.files[4].name, "loop");
    assert_eq!(p.files[4].relationship.as_deref(), Some("Bogus"), "unknown relationships are reported as written");
    assert!(!p.files[4].embedded);
    assert!(associated_file_data(&doc, &AfTarget::Page(0), &AfSelector::Index(4), usize::MAX).is_err());
    assert!(associated_files(&doc, &AfTarget::Page(1)).unwrap().issues.len() == 1);
    // Junk entries can be removed by index; adding replaces a non-array /AF.
    remove_associated_file(&mut doc, &AfTarget::Page(0), &AfSelector::Index(1)).unwrap();
    assert_eq!(associated_files(&doc, &AfTarget::Page(0)).unwrap().files.len(), 4);
    assert!(remove_associated_file(&mut doc, &AfTarget::Document, &AfSelector::Index(0)).is_err());
    add_associated_file(&mut doc, &AfTarget::Document, &file("ok.csv", b"1", Relationship::Data), None).unwrap();
    let doc = reopen(&doc);
    let d = associated_files(&doc, &AfTarget::Document).unwrap();
    assert_eq!(d.files.len(), 1);
    assert!(d.issues.is_empty());
    assert!(all_associated_files(&doc).is_ok());
}

#[test]
fn huge_af_arrays_are_capped() {
    let mut objs = fixture_objs();
    let entries = vec!["10 0 R"; MAX_AF_ENTRIES + 10].join(" ");
    objs[3] = format!("<< /Type /Page /Parent 2 0 R /AF [{entries}] >>").into_bytes();
    let mut doc = open(build(&objs, ""));
    let p = associated_files(&doc, &AfTarget::Page(1)).unwrap();
    assert_eq!(p.files.len(), MAX_AF_ENTRIES);
    assert_eq!(p.issues.len(), 1);
    assert!(add_associated_file(&mut doc, &AfTarget::Page(1), &file("x.csv", b"1", Relationship::Data), None).is_err());
    // Removing keeps the entries past the cap.
    remove_associated_file(&mut doc, &AfTarget::Page(1), &AfSelector::Index(0)).unwrap();
    let page = crate::pages(&doc).unwrap()[1].obj;
    let len = doc.get(page).as_dict().unwrap().get(b"AF").unwrap().as_array().unwrap().len();
    assert_eq!(len, MAX_AF_ENTRIES + 9);
}

#[test]
fn a_cyclic_embedded_files_tree_is_never_rewritten() {
    let mut objs = fixture_objs();
    objs[0] = b"<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles 12 0 R >> >>".to_vec();
    objs.push(b"<< /Kids [12 0 R 13 0 R] >>".to_vec()); // 12: contains itself
    objs.push(b"<< /Names [(old.txt) 10 0 R] /Limits [(old.txt) (old.txt)] >>".to_vec()); // 13
    let mut doc = open(build(&objs, ""));
    let before = doc.modified_objects();
    let err = add_associated_file(&mut doc, &AfTarget::Document, &file("x.csv", b"1", Relationship::Data), None).unwrap_err().to_string();
    assert!(err.contains("name tree"), "{err}");
    assert_eq!(doc.modified_objects(), before);
    // Pages don't need the tree.
    add_associated_file(&mut doc, &AfTarget::Page(0), &file("x.csv", b"1", Relationship::Data), None).unwrap();
    // A nested, well-formed tree is flattened with every entry kept.
    let mut objs = fixture_objs();
    objs[0] = b"<< /Type /Catalog /Pages 2 0 R /Names 14 0 R >>".to_vec();
    objs.push(b"<< /Kids [13 0 R] >>".to_vec()); // 12
    objs.push(b"<< /Names [(old.txt) 10 0 R] /Limits [(old.txt) (old.txt)] >>".to_vec()); // 13
    objs.push(b"<< /EmbeddedFiles 12 0 R /Dests << >> >>".to_vec()); // 14: indirect /Names
    let mut doc = open(build(&objs, ""));
    add_associated_file(&mut doc, &AfTarget::Document, &file("new.csv", b"1", Relationship::Data), None).unwrap();
    let doc = reopen(&doc);
    assert_eq!(tree_names(&doc), ["new.csv", "old.txt"]);
    let names = doc.get(ObjRef::new(14, 0));
    assert!(names.as_dict().unwrap().contains(b"Dests"), "the rest of /Names is kept");
}
