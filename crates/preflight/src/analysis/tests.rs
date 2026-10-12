use super::*;

fn fixture() -> Document {
    crate::tests::build(
        &[
            "<< /Type /Catalog /Pages 2 0 R /Private << /Keep (untouched) >> >>",
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> /XObject << /Im 6 0 R >> >> >>",
            "<< /Type /Page /Parent 2 0 R /Annots [<< /Type /Annot /Subtype /Link /Rect [1 2 3 4] /F 2 /Private (keep) /AA << /E << /S /URI /URI (https://example.invalid) >> >> >>] >>",
            "<< /Type /Page /Parent 2 0 R >>",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
            "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Interpolate true /Private [1 2 3] /Length 1 >>\nstream\nx\nendstream",
        ],
        "",
    )
}

fn bytes(doc: &Document) -> Vec<u8> {
    pdfcraft_cos::write_full(doc, &Default::default()).unwrap()
}

#[test]
fn profiles_find_inline_objects_and_shared_inherited_resources_without_mutation() {
    let doc = fixture();
    let before = bytes(&doc);
    let report = analyze(&doc, &Settings::default()).unwrap();
    assert!(!report.truncated);
    let font = report.findings.iter().find(|f| f.rule == Rule::FontEmbedding).unwrap();
    assert_eq!(font.location.as_ref().unwrap().object, ObjRef::new(5, 0));
    assert_eq!(font.location.as_ref().unwrap().pages, [0, 1]);
    let image = report.findings.iter().find(|f| f.rule == Rule::ImageInterpolation).unwrap();
    assert_eq!(image.location.as_ref().unwrap().pages, [0, 1]);
    let annot = report.findings.iter().find(|f| f.rule == Rule::AnnotationPrint).unwrap();
    assert_eq!(annot.location.as_ref().unwrap().display(), "3 0 R/Annots[0]");
    assert_eq!(annot.location.as_ref().unwrap().pages, [0]);
    assert_eq!(before, bytes(&doc));
    assert!(report.text().contains("does not establish"));
    let single = analyze(&doc, &Settings { checks: Some(vec![Rule::ImageInterpolation]), ..Default::default() }).unwrap();
    assert_eq!(single.findings.len(), 1);
    assert_eq!(single.findings[0].id, image.id, "Check selection must not reinterpret finding IDs");
    assert!(analyze(&doc, &Settings { checks: Some(vec![Rule::PdfaStructure]), ..Default::default() }).is_err());
}

#[test]
fn selected_fixes_preserve_unknown_keys_stream_bytes_and_other_flags() {
    let mut doc = fixture();
    let report = analyze(&doc, &Settings::default()).unwrap();
    let ids = report.findings.iter().filter(|f| f.fixup.is_some()).map(|f| f.id).collect::<Vec<_>>();
    let original_image = doc.get(ObjRef::new(6, 0));
    let fixes = report.fixes(&ids).unwrap();
    apply_fixes(&mut doc, &fixes, || false).unwrap();
    let after = analyze(&doc, &Settings::default()).unwrap();
    assert!(after.findings.iter().all(|f| f.fixup.is_none()));
    let page = doc.get(ObjRef::new(3, 0));
    let annot = page.as_dict().unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_dict().unwrap();
    assert_eq!(annot.int(b"F"), Some(6), "Hidden flag is preserved; this does not claim the annotation will become visible");
    assert!(annot.get(b"Private").is_some());
    assert!(annot.get(b"AA").is_none());
    let image = doc.get(ObjRef::new(6, 0));
    let (Object::Stream(old), Object::Stream(new)) = (&*original_image, &*image) else {
        panic!("image stream");
    };
    assert_eq!(old.raw, new.raw);
    assert_eq!(old.dict.get(b"Private"), new.dict.get(b"Private"));
    assert!(doc.get(ObjRef::new(1, 0)).as_dict().unwrap().get(b"Private").is_some());
    let reopened = Document::open(Arc::new(bytes(&doc))).unwrap();
    assert!(analyze(&reopened, &Settings::default()).unwrap().findings.iter().all(|f| f.fixup.is_none()));
}

#[test]
fn stale_duplicate_invalid_and_cancelled_fixes_are_atomic() {
    let mut doc = fixture();
    let report = analyze(&doc, &Settings::default()).unwrap();
    let ids = report.findings.iter().filter(|f| f.fixup.is_some()).map(|f| f.id).collect::<Vec<_>>();
    assert!(report.fixes(&[]).is_err());
    assert!(report.fixes(&[ids[0], ids[0]]).is_err());
    assert!(report.fixes(&[usize::MAX]).is_err());
    let fixes = report.fixes(&ids).unwrap();
    let before = bytes(&doc);
    let mut ticks = 0;
    assert!(
        apply_fixes(&mut doc, &fixes, || {
            ticks += 1;
            ticks == fixes.len() + 2
        })
        .is_err()
    );
    assert_eq!(before, bytes(&doc));
    doc.update_dict(ObjRef::new(6, 0), |d| d.set(b"Private".to_vec(), Object::Int(9))).unwrap();
    let before = bytes(&doc);
    assert!(apply_fixes(&mut doc, &fixes, || false).unwrap_err().contains("changed"));
    assert_eq!(before, bytes(&doc));
}

#[test]
fn malformed_lzw_aborts_other_selected_changes() {
    let mut doc = fixture();
    let mut dict = Dict::new();
    dict.set(b"Filter".to_vec(), Object::name("LZWDecode"));
    doc.add(Object::Stream(Stream::from_raw(dict, vec![0xff])));
    let report = analyze(&doc, &Settings::default()).unwrap();
    let mut ids = report.findings.iter().filter(|f| f.fixup.is_some() && f.rule != Rule::LzwCompression).map(|f| f.id).collect::<Vec<_>>();
    ids.push(report.findings.iter().find(|f| f.rule == Rule::LzwCompression).unwrap().id);
    let before = bytes(&doc);
    assert!(apply_fixes(&mut doc, &report.fixes(&ids).unwrap(), || false).is_err());
    assert_eq!(before, bytes(&doc));
}

#[test]
fn lzw_fix_preserves_decoded_bytes_and_private_stream_dictionary() {
    let mut doc = fixture();
    let data = b"Synthetic stream bytes: unchanged after recompression.\n";
    let encoded = pdfcraft_filters::encode(&pdfcraft_filters::Filter::Lzw, &Default::default(), data).unwrap();
    let mut dict = Dict::new();
    dict.set(b"Filter".to_vec(), Object::name("LZWDecode"));
    dict.set(b"Private".to_vec(), Object::Int(17));
    let reference = doc.add(Object::Stream(Stream::from_raw(dict, encoded)));
    let report = analyze(&doc, &Settings::default()).unwrap();
    let id = report.findings.iter().find(|f| f.rule == Rule::LzwCompression).unwrap().id;
    apply_fixes(&mut doc, &report.fixes(&[id]).unwrap(), || false).unwrap();
    let object = doc.get(reference);
    let Object::Stream(stream) = &*object else {
        panic!("stream");
    };
    assert_eq!(stream.decoded_strict().unwrap(), data);
    assert_eq!(stream.dict.name(b"Filter"), Some(b"FlateDecode".as_slice()));
    assert_eq!(stream.dict.int(b"Private"), Some(17));
}

#[test]
fn lzw_end_marker_validation_handles_code_width_changes_and_refuses_truncation() {
    for early_change in [0, 1] {
        let data = (0..50_000).map(|i| ((i * 31 + i / 257) % 256) as u8).collect::<Vec<_>>();
        let params = pdfcraft_filters::Params { early_change, ..Default::default() };
        let encoded = pdfcraft_filters::encode(&pdfcraft_filters::Filter::Lzw, &params, &data).unwrap();
        let mut parms = Dict::new();
        parms.set(b"EarlyChange".to_vec(), Object::Int(early_change));
        let mut dictionary = Dict::new();
        dictionary.set(b"Filter".to_vec(), Object::name("LZWDecode"));
        dictionary.set(b"DecodeParms".to_vec(), Object::Dict(parms));
        assert_eq!(decode_for_fix(&Stream::from_raw(dictionary.clone(), encoded.clone())).unwrap(), data);
        // Cut inside the end-of-data code: refused, not repaired.
        assert!(decode_for_fix(&Stream::from_raw(dictionary, encoded[..encoded.len() - 1].to_vec())).unwrap_err().contains("end-of-data"));
    }
    let mut dictionary = Dict::new();
    dictionary.set(b"Filter".to_vec(), Object::name("LZWDecode"));
    assert!(decode_for_fix(&Stream::from_raw(dictionary, vec![0x80, 0x00])).unwrap_err().contains("end-of-data"));
}

#[test]
fn bounded_cycles_and_deep_direct_objects_report_incomplete_coverage() {
    let mut doc = fixture();
    let mut value = Object::Int(1);
    for _ in 0..100 {
        value = Object::Array(vec![value]);
    }
    doc.add(value);
    let r = doc.add(Object::Null);
    let mut cycle = Dict::new();
    cycle.set(b"Loop".to_vec(), Object::Ref(r));
    doc.set(r, Object::Dict(cycle));
    let report = analyze(&doc, &Settings::default()).unwrap();
    assert!(report.truncated);
    assert!(report.limitations.iter().any(|s| s.contains("limit")));
    assert!(report.findings.len() <= MAX_FINDINGS);
}

#[test]
fn queued_wide_paths_share_deep_ancestry_instead_of_cloning_keys() {
    let mut path = None;
    for _ in 0..40 {
        path = Some(extend_path(&path, PathPart::Key(vec![b'x'; 256])));
    }
    let ancestor = path.as_ref().unwrap();
    let children = (0..4096).map(|i| extend_path(&path, PathPart::Index(i))).collect::<Vec<_>>();
    assert!(children.iter().all(|child| Arc::ptr_eq(child.parent.as_ref().unwrap(), ancestor)));
    assert_eq!(Arc::strong_count(ancestor), children.len() + 1);
    assert_eq!(materialize_path(&Some(children[12].clone())).len(), 41);
    assert!(matches!(children[12].part, PathPart::Index(12)));
    // 4096 siblings retain one 10 KiB ancestor key chain, not 4096 copies of it.
    assert_eq!(ancestor.bytes, 40 * 256);
}

#[test]
fn indirect_filter_parameters_and_malformed_flags_are_not_offered_fixes() {
    let mut doc = fixture();
    let filter = doc.add(Object::name("LZWDecode"));
    let mut dict = Dict::new();
    dict.set(b"Filter".to_vec(), Object::Ref(filter));
    doc.add(Object::Stream(Stream::from_raw(dict, vec![0xff])));
    let mut annot = Dict::new();
    annot.set(b"Type".to_vec(), Object::name("Annot"));
    annot.set(b"Subtype".to_vec(), Object::name("Text"));
    annot.set(b"F".to_vec(), Object::name("bad"));
    let a = doc.add(annot);
    let report = analyze(&doc, &Settings::default()).unwrap();
    assert!(report.findings.iter().find(|f| f.rule == Rule::LzwCompression).unwrap().fixup.is_none());
    assert!(report.findings.iter().find(|f| f.location.as_ref().is_some_and(|l| l.object == a)).unwrap().fixup.is_none());
}
