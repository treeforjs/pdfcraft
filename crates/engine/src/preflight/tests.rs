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
