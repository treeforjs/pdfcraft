use super::*;
use pdfcraft_cos::{Dict, Document, Object, Stream};
use std::sync::Arc;

fn session() -> (Session, DocId) {
    let mut session = Session::new();
    let bytes = session.create_blank(200.0, 200.0, 1).unwrap();
    let mut cos = Document::open(bytes).unwrap();
    let mut dict = Dict::new();
    dict.set(b"Type".to_vec(), Object::name("XObject"));
    dict.set(b"Subtype".to_vec(), Object::name("Image"));
    dict.set(b"Width".to_vec(), Object::Int(1));
    dict.set(b"Height".to_vec(), Object::Int(1));
    dict.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
    dict.set(b"BitsPerComponent".to_vec(), Object::Int(8));
    dict.set(b"Interpolate".to_vec(), Object::Bool(true));
    dict.set(b"Private".to_vec(), Object::Int(99));
    let image = cos.add(Object::Stream(Stream::from_raw(dict, vec![127])));
    let page = pdfcraft_model::pages(&cos)[0].obj;
    let mut images = Dict::new();
    images.set(b"Im".to_vec(), Object::Ref(image));
    let mut resources = Dict::new();
    resources.set(b"XObject".to_vec(), Object::Dict(images));
    cos.update_dict(page, |d| d.set(b"Resources".to_vec(), Object::Dict(resources))).unwrap();
    let bytes = Arc::new(pdfcraft_cos::write_full(&cos, &Default::default()).unwrap());
    let id = session.open("preflight.pdf", None, bytes, None).unwrap();
    (session, id)
}

#[test]
fn analysis_is_view_only_and_selected_fixes_are_one_undoable_step() {
    let (mut session, id) = session();
    let original = session.get(id).unwrap().bytes.clone();
    let result = session.get(id).unwrap().preflight(&Settings::default()).unwrap();
    assert!(!session.get(id).unwrap().dirty);
    assert!(session.get(id).unwrap().can_undo().is_none());
    let finding = result.report.findings.iter().find(|f| f.fixup == Some(Fixup::DisableInterpolation)).unwrap();
    let reference = finding.location.as_ref().unwrap().object;
    let after = session.preflight_fix(&result, &[finding.id]).unwrap();
    assert!(after.report.findings.iter().all(|f| f.fixup != Some(Fixup::DisableInterpolation)));
    assert_eq!(session.get(id).unwrap().can_undo(), Some("Apply preflight fixes"));
    let working = session.get(id).unwrap().bytes.clone();
    let cos = Document::open(working).unwrap();
    assert_eq!(cos.get(reference).as_dict().unwrap().int(b"Private"), Some(99));
    session.undo(id).unwrap();
    assert!(
        session
            .get(id)
            .unwrap()
            .preflight(&Settings::default())
            .unwrap()
            .report
            .findings
            .iter()
            .any(|f| f.fixup == Some(Fixup::DisableInterpolation))
    );
    assert!(session.get(id).unwrap().can_undo().is_none());
    session.redo(id).unwrap();
    assert!(
        session
            .get(id)
            .unwrap()
            .preflight(&Settings::default())
            .unwrap()
            .report
            .findings
            .iter()
            .all(|f| f.fixup != Some(Fixup::DisableInterpolation))
    );
    assert!(!original.is_empty());
}

#[test]
fn stale_invalid_and_denied_fixes_leave_document_and_history_untouched() {
    let (mut session, id) = session();
    let result = session.get(id).unwrap().preflight(&Settings::default()).unwrap();
    let finding = result.report.findings.iter().find(|f| f.fixup.is_some()).unwrap().id;
    let before = session.get(id).unwrap().bytes.clone();
    for ids in [vec![], vec![usize::MAX], vec![finding, finding]] {
        assert!(session.preflight_fix(&result, &ids).is_err());
    }
    assert_eq!(session.get(id).unwrap().bytes, before);
    assert!(session.get(id).unwrap().can_undo().is_none());
    session.apply(id, Edit::SetInfo { key: "Title".into(), value: "New revision".into() }).unwrap();
    let before = session.get(id).unwrap().bytes.clone();
    let history = session.get(id).unwrap().can_undo().map(str::to_owned);
    assert!(session.preflight_fix(&result, &[finding]).unwrap_err().to_string().contains("stale"));
    assert_eq!(session.get(id).unwrap().bytes, before);
    assert_eq!(session.get(id).unwrap().can_undo(), history.as_deref());
    let edits = result.report.fixes(&[finding]).unwrap();
    let permissions = pdfcraft_cos::Permissions { bits: 0, owner: false };
    assert!(crate::check_permission(&Edit::PreflightFix { fixes: edits }, &permissions).is_err());
    // The low-level value binding refuses stale content even if a caller bypasses ResultSet.
    let fresh = session.get(id).unwrap().preflight(&Settings::default()).unwrap();
    let fix = fresh.report.findings.iter().find(|f| f.fixup.is_some()).unwrap();
    let requests = fresh.report.fixes(&[fix.id]).unwrap();
    session.apply(id, Edit::PreflightFix { fixes: requests.clone() }).unwrap();
    let before = session.get(id).unwrap().bytes.clone();
    assert!(session.apply(id, Edit::PreflightFix { fixes: requests }).is_err());
    assert_eq!(session.get(id).unwrap().bytes, before);
}

#[test]
fn ink_coverage_reads_the_working_document_without_editing_it() {
    let mut session = Session::new();
    let bytes = session.create_blank(200.0, 200.0, 2).unwrap();
    let mut cos = Document::open(bytes).unwrap();
    let page = pdfcraft_model::pages(&cos)[0].obj;
    let content = cos.add(Object::Stream(Stream::from_raw(Dict::new(), b"1 1 1 1 k 10 10 50 50 re f".to_vec())));
    cos.update_dict(page, |d| d.set(b"Contents".to_vec(), Object::Ref(content))).unwrap();
    let bytes = Arc::new(pdfcraft_cos::write_full(&cos, &Default::default()).unwrap());
    let id = session.open("coverage.pdf", None, bytes, None).unwrap();
    let doc = session.get(id).unwrap();
    let pages = doc.ink_coverage(&[0, 1], &CoverageOptions::default()).unwrap();
    assert!(pages[0].flagged());
    assert_eq!(pages[0].max, 400.0);
    assert!(!pages[1].flagged());
    assert!(doc.ink_coverage(&[2], &CoverageOptions::default()).is_err());
    assert!(doc.ink_coverage(&[0], &CoverageOptions { limit: -1.0, ..CoverageOptions::default() }).is_err());
    assert!(!doc.dirty && doc.can_undo().is_none());
}

#[test]
fn colour_warnings_read_the_working_document_without_editing_it() {
    let mut session = Session::new();
    let bytes = session.create_blank(200.0, 200.0, 2).unwrap();
    let mut cos = Document::open(bytes).unwrap();
    let page = pdfcraft_model::pages(&cos)[1].obj;
    let content = cos.add(Object::Stream(Stream::from_raw(Dict::new(), b"0.6 0.4 0.4 1 k 10 10 50 50 re f".to_vec())));
    cos.update_dict(page, |d| d.set(b"Contents".to_vec(), Object::Ref(content))).unwrap();
    let bytes = Arc::new(pdfcraft_cos::write_full(&cos, &Default::default()).unwrap());
    let id = session.open("colour.pdf", None, bytes, None).unwrap();
    let doc = session.get(id).unwrap();
    let r = doc.colour_warnings(&ColourWarningOptions::default()).unwrap();
    assert_eq!(r.pages, vec![0, 1]);
    assert_eq!(r.count(ColourWarningKind::RichBlack), 1);
    assert_eq!(r.findings[0].page, 1);
    let first = doc.colour_warnings(&ColourWarningOptions { pages: vec![0], ..ColourWarningOptions::default() }).unwrap();
    assert!(first.findings.is_empty());
    assert!(doc.colour_warnings(&ColourWarningOptions { pages: vec![2], ..ColourWarningOptions::default() }).is_err());
    assert!(doc.colour_warnings(&ColourWarningOptions { rich_black_k: 0.0, ..ColourWarningOptions::default() }).is_err());
    assert!(!doc.dirty && doc.can_undo().is_none());
}

/// A CMYK output profile (lut16, Lab PCS) that prints everything on black: K = 1 − L*/100.
fn black_only_profile() -> Vec<u8> {
    let grid = 9usize;
    let mut lut = b"mft2\0\0\0\0".to_vec();
    lut.extend([3u8, 4, grid as u8, 0]);
    for v in [1i32, 0, 0, 0, 1, 0, 0, 0, 1] {
        lut.extend((v << 16).to_be_bytes());
    }
    lut.extend(2u16.to_be_bytes());
    lut.extend(2u16.to_be_bytes());
    for _ in 0..3 {
        lut.extend(0u16.to_be_bytes());
        lut.extend(65535u16.to_be_bytes());
    }
    for l in 0..grid {
        let ll = (l as f64 / (grid - 1) as f64) * 65535.0 / 652.8;
        let k = (1.0 - ll / 100.0).clamp(0.0, 1.0);
        for _ in 0..grid * grid {
            for v in [0.0, 0.0, 0.0, k] {
                lut.extend(((v * 65535.0f64).round() as u16).to_be_bytes());
            }
        }
    }
    for _ in 0..4 {
        lut.extend(0u16.to_be_bytes());
        lut.extend(65535u16.to_be_bytes());
    }
    let tags: [(&[u8; 4], Vec<u8>); 2] = [(b"B2A0", lut.clone()), (b"B2A1", lut)];
    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut data: Vec<u8> = Vec::new();
    let mut offset = 128 + 4 + tags.len() * 12;
    for (sig, body) in &tags {
        table.extend(*sig);
        table.extend((offset as u32).to_be_bytes());
        table.extend((body.len() as u32).to_be_bytes());
        data.extend(body);
        offset += body.len();
    }
    let size = 128 + table.len() + data.len();
    let mut h = (size as u32).to_be_bytes().to_vec();
    h.extend([0u8; 4]);
    h.extend([2, 0x10, 0, 0]);
    h.extend(b"prtrCMYKLab ");
    h.extend([0u8; 12]);
    h.extend(b"acsp");
    h.resize(128, 0);
    h.extend(table);
    h.extend(data);
    h
}

/// A one-page document painting RGB black over 10..60, with an output intent profile.
fn press_doc(profile: Option<Vec<u8>>) -> (Session, DocId) {
    let mut session = Session::new();
    let bytes = session.create_blank(200.0, 200.0, 1).unwrap();
    let mut cos = Document::open(bytes).unwrap();
    let page = pdfcraft_model::pages(&cos)[0].obj;
    let content = cos.add(Object::Stream(Stream::from_raw(Dict::new(), b"0 0 0 rg 10 10 50 50 re f 0.2 0 0 0 k 100 100 50 50 re f".to_vec())));
    cos.update_dict(page, |d| d.set(b"Contents".to_vec(), Object::Ref(content))).unwrap();
    if let Some(profile) = profile {
        let mut pd = Dict::new();
        pd.set(b"N".to_vec(), Object::Int(4));
        let p = cos.add(Object::Stream(Stream::from_raw(pd, profile)));
        let mut oi = Dict::new();
        oi.set(b"Type".to_vec(), Object::name("OutputIntent"));
        oi.set(b"S".to_vec(), Object::name("GTS_PDFX"));
        oi.set(b"DestOutputProfile".to_vec(), Object::Ref(p));
        let root = cos.root().unwrap();
        cos.update_dict(root, |d| d.set(b"OutputIntents".to_vec(), Object::Array(vec![Object::Dict(oi)]))).unwrap();
    }
    let bytes = Arc::new(pdfcraft_cos::write_full(&cos, &Default::default()).unwrap());
    let id = session.open("press.pdf", None, bytes, None).unwrap();
    (session, id)
}

#[test]
fn separations_convert_rgb_through_the_output_intent_and_say_so() {
    let (session, id) = press_doc(Some(black_only_profile()));
    let doc = session.get(id).unwrap();
    let seps = doc.separations(0, 72.0, true).unwrap();
    assert!(seps.conversion_from_profile, "{}", seps.conversion);
    assert!(seps.conversion.contains("GTS_PDFX output intent"), "{}", seps.conversion);
    let black = pdfcraft_preflight::separations::sample(&seps, 30.0, 30.0).unwrap();
    assert_eq!(black.inks[0], ("Cyan".to_string(), 0.0));
    assert!((black.total - 100.0).abs() < 1.0, "{black:?}");
    let tac = doc.ink_coverage(&[0], &CoverageOptions::default()).unwrap();
    assert!((tac[0].max - 100.0).abs() < 1.0 && tac[0].conversion.contains("output intent"), "{:?}", tac[0]);
    let inks = doc.separation_inks(&[]).unwrap();
    assert_eq!(inks.inks.iter().find(|i| i.info.name == "Black").unwrap().pages, vec![0]);
    assert!(doc.separations(1, 72.0, true).is_err());
    assert!(doc.separations(0, 1000.0, true).is_err());
    assert!(!doc.dirty && doc.can_undo().is_none());

    // Without an output intent, or with one that can't separate, the built-in conversion
    // says why.
    let (session, id) = press_doc(None);
    let seps = session.get(id).unwrap().separations(0, 72.0, true).unwrap();
    assert!(!seps.conversion_from_profile && seps.conversion.contains("no output intent"), "{}", seps.conversion);
    let black = pdfcraft_preflight::separations::sample(&seps, 30.0, 30.0).unwrap();
    assert_eq!(black.total, 190.0);
    let (session, id) = press_doc(Some(b"garbage".to_vec()));
    let seps = session.get(id).unwrap().separations(0, 72.0, true).unwrap();
    assert!(seps.conversion.contains("could not be read"), "{}", seps.conversion);
}

#[test]
fn press_snapshots_analyse_as_the_document_does_on_another_thread() {
    let (mut session, id) = press_doc(None);
    let mut settings = session.get(id).unwrap().output_preview().clone();
    settings.coverage_warning = true;
    settings.coverage_limit = 150.0;
    session.set_output_preview(id, settings).unwrap();
    let doc = session.get(id).unwrap();
    let snapshot = doc.press_snapshot().unwrap();
    let (direct, _) = doc.render_output_preview(0, 72.0).unwrap();
    let (copied, info) = std::thread::spawn(move || snapshot.render_output_preview(0, 72.0)).join().unwrap().unwrap();
    assert_eq!(direct.rgb, copied.rgb);
    assert_eq!(direct.coverage, copied.coverage);
    assert!(copied.coverage.warning && copied.coverage.over_cells > 0, "{:?}", copied.coverage);
    assert_eq!(info.used, "working");
    let snapshot = doc.press_snapshot().unwrap();
    assert_eq!(snapshot.settings().coverage_limit, 150.0);
    let found = snapshot.inspect_objects(0, 30.0, 30.0, 72.0).unwrap();
    assert_eq!(found.objects.len(), 1);
    assert_eq!(found.objects[0].fill.as_ref().unwrap().space, "DeviceRGB");
    assert!(snapshot.colour_warnings(0).unwrap().findings.is_empty());
    // Pages past the end and hostile points are refused, never a panic.
    assert!(snapshot.render_output_preview(1, 72.0).is_err());
    assert!(snapshot.render_output_preview(usize::MAX, 72.0).is_err());
    assert!(snapshot.render_output_preview(0, f64::NAN).is_err());
    assert!(snapshot.inspect_objects(0, f64::INFINITY, 0.0, 72.0).is_err());
    assert!(snapshot.colour_warnings(9).is_err());
    let inks = snapshot.document_inks(&[]).unwrap();
    assert_eq!(inks.inks.len(), 4);
    assert!(snapshot.document_inks(&[3]).is_err());
    assert_eq!(snapshot.page_count(), 1);
    assert!(!doc.dirty && doc.can_undo().is_none());
}
