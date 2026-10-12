//! End-to-end OCR draft tools on an original, rasterized sentence. Model-backed when installed.
use pdfcraft_automation::{Automation, Content};
use pdfcraft_engine::{DocId, Session, export};
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

fn result(auto: &mut Automation, name: &str, args: Value) -> Value {
    let mut reply = auto.call(name, &args).unwrap_or_else(|e| panic!("{name}: {e}"));
    match reply.remove(0) {
        Content::Json(value) => value,
        other => panic!("expected JSON, got {other:?}"),
    }
}
fn root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("pdfcraft-ocr-review-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}
fn scan() -> Arc<Vec<u8>> {
    let mut session = Session::new();
    let bytes = session.create_from_text("Original OCR test", "Searchable scans with recognized words").unwrap();
    let id = session.open("sentence.pdf", None, bytes, None).unwrap();
    let png = export::Exporter::new(session.get(id).unwrap()).png(0, 150.0).unwrap();
    session.create_from_images(&[("original.png".into(), png)]).unwrap()
}
fn pixels(bytes: Arc<Vec<u8>>) -> Vec<u8> {
    let shot = PageRenderer::new(bytes, RenderConfig::default()).render(RenderRequest { scale: 1.0, ..Default::default() });
    assert!(shot.error.is_none());
    shot.rgba.to_vec()
}

#[test]
fn ocr_review_tools_reject_bad_inputs_without_models_or_document_changes() {
    let path = root("inputs");
    let mut auto = Automation::new().with_root(&path).unwrap();
    let doc = result(&mut auto, "doc_create", json!({"from":"blank"}))["doc"].as_u64().unwrap();
    let before = auto.session().get(DocId(doc)).unwrap().bytes.clone();
    for args in [
        json!({"doc":doc,"dpi":-1}),
        json!({"doc":doc,"dpi":601}),
        json!({"doc":doc,"language":"fr"}),
        json!({"doc":doc,"pages":[1,1]}),
        json!({"doc":doc,"pages":[2]}),
    ] {
        assert!(auto.call("ocr_prepare", &args).is_err());
    }
    for tool in ["ocr_review", "ocr_correct", "ocr_apply"] {
        assert!(auto.call(tool, &json!({"doc":doc,"page":1,"word":1,"text":"valid"})).is_err());
    }
    assert_eq!(result(&mut auto, "ocr_cancel", json!({"doc":doc}))["canceled"], false);
    let unchanged = auto.session().get(DocId(doc)).unwrap();
    assert_eq!(unchanged.bytes, before);
    assert_eq!(unchanged.can_undo(), None);
}

#[test]
fn ocr_review_tools_correct_cancel_apply_undo_and_reopen_exact_image() {
    if !pdfcraft_engine::ocr::Models::required_for_tests().unwrap() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    eprintln!("running actual pinned OCR models for the review/correction tool workflow");
    let path = root("models");
    let original = scan();
    std::fs::write(path.join("scan.pdf"), original.as_slice()).unwrap();
    let mut auto = Automation::new().with_root(&path).unwrap().with_clock(|| 1_700_000_000);
    let id = result(&mut auto, "doc_open", json!({"path":"scan.pdf"}))["doc"].as_u64().unwrap();
    let before_pixels = pixels(original.clone());
    let review = result(&mut auto, "ocr_prepare", json!({"doc":id,"pages":[1]}));
    assert!(review["words"].as_u64().unwrap() >= 4, "{review}");
    assert!(review["pages"][0]["text"].as_str().unwrap().to_lowercase().contains("searchable"), "{review}");
    assert_eq!(auto.session().get(DocId(id)).unwrap().bytes, original);
    assert_eq!(auto.session().get(DocId(id)).unwrap().can_undo(), None);
    let geometry = review["pages"][0]["words"][0].clone();
    let fixed = result(&mut auto, "ocr_correct", json!({"doc":id,"page":1,"word":1,"text":"café"}));
    for key in ["origin", "across", "up"] {
        assert_eq!(fixed["pages"][0]["words"][0][key], geometry[key]);
    }
    assert!(auto.call("ocr_correct", &json!({"doc":id,"page":1,"word":1,"text":"漢字"})).is_err());
    assert_eq!(result(&mut auto, "ocr_review", json!({"doc":id}))["pages"], fixed["pages"]);
    assert_eq!(result(&mut auto, "ocr_cancel", json!({"doc":id}))["canceled"], true);
    assert_eq!(auto.session().get(DocId(id)).unwrap().bytes, original);
    assert!(auto.call("ocr_apply", &json!({"doc":id})).is_err());

    result(&mut auto, "ocr_prepare", json!({"doc":id}));
    result(&mut auto, "ocr_correct", json!({"doc":id,"page":1,"word":1,"text":"café"}));
    assert!(result(&mut auto, "ocr_apply", json!({"doc":id}))["words"].as_u64().unwrap() >= 4);
    assert_eq!(auto.session().get(DocId(id)).unwrap().can_undo(), Some("Recognize text"));
    assert_eq!(pixels(auto.session().get(DocId(id)).unwrap().bytes.clone()), before_pixels);
    assert!(auto.call("doc_save", &json!({"doc":id,"path":"../escape-ocr.pdf"})).is_err());
    assert_eq!(result(&mut auto, "doc_save", json!({"doc":id}))["incremental"], true);
    let saved = std::fs::read(path.join("scan.pdf")).unwrap();
    assert!(saved.starts_with(&original));
    let reopened = result(&mut auto, "doc_open", json!({"path":"scan.pdf"}))["doc"].as_u64().unwrap();
    assert!(result(&mut auto, "text_extract", json!({"doc":reopened}))["pages"][0]["text"].as_str().unwrap().contains("café"));
    assert_eq!(pixels(auto.session().get(DocId(reopened)).unwrap().bytes.clone()), before_pixels);
    result(&mut auto, "edit_undo", json!({"doc":id}));
    assert_eq!(auto.session().get(DocId(id)).unwrap().can_undo(), None);
    assert!(result(&mut auto, "text_extract", json!({"doc":id}))["pages"][0]["text"].as_str().unwrap().trim().is_empty());

    result(&mut auto, "ocr_prepare", json!({"doc":id}));
    result(&mut auto, "doc_set_info", json!({"doc":id,"key":"Title","value":"Changed"}));
    let before = auto.session().get(DocId(id)).unwrap().bytes.clone();
    assert!(auto.call("ocr_review", &json!({"doc":id})).unwrap_err().to_string().contains("changed"));
    assert!(auto.call("ocr_correct", &json!({"doc":id,"page":1,"word":1,"text":"changed"})).is_err());
    assert!(auto.call("ocr_apply", &json!({"doc":id})).is_err());
    assert_eq!(auto.session().get(DocId(id)).unwrap().bytes, before);
    result(&mut auto, "doc_close", json!({"doc":id,"discard_changes":true}));
    assert!(auto.call("ocr_review", &json!({"doc":id})).is_err());
}

/// Protect `source` (a PDF under `path`) through the tools with permissions password `owner`
/// and save it as `name`: copy and changes as given, no open password.
fn protect(auto: &mut Automation, source: &str, name: &str, copy: bool, changes: &str) {
    let id = result(auto, "doc_open", json!({"path":source}))["doc"].as_u64().unwrap();
    result(auto, "doc_protect", json!({"doc":id,"permissions_password":"owner","copy":copy,"changes":changes}));
    result(auto, "doc_save", json!({"doc":id,"path":name}));
    result(auto, "doc_close", json!({"doc":id}));
}

#[test]
fn restricted_documents_refuse_every_ocr_tool_before_recognition() {
    let path = root("restricted");
    std::fs::write(path.join("scan.pdf"), scan().as_slice()).unwrap();
    let mut auto = Automation::new().with_root(&path).unwrap().with_clock(|| 1_700_000_000);
    let cases = [
        ("copy-denied.pdf", false, "any-except-extract", "copying"),
        ("modify-denied.pdf", true, "none", "changes"),
        ("both-denied.pdf", false, "none", "copying"),
    ];
    for (name, copy, changes, _) in cases {
        protect(&mut auto, "scan.pdf", name, copy, changes);
    }
    let models = pdfcraft_engine::ocr::Models::required_for_tests().unwrap();
    for (name, _, _, why) in cases {
        let file = std::fs::read(path.join(name)).unwrap();
        let id = result(&mut auto, "doc_open", json!({"path":name}))["doc"].as_u64().unwrap();
        let before = auto.session().get(DocId(id)).unwrap().bytes.clone();
        // The permission error comes before model loading: without models it is still the
        // permission error, never "models not installed".
        for tool in ["ocr_prepare", "ocr_recognize"] {
            let error = auto.call(tool, &json!({"doc":id})).unwrap_err().to_string();
            assert!(error.contains(why) && error.contains("permissions password"), "{name} {tool}: {error}");
        }
        for tool in ["ocr_review", "ocr_correct", "ocr_apply"] {
            assert!(auto.call(tool, &json!({"doc":id,"page":1,"word":1,"text":"x"})).is_err(), "{name} {tool}");
        }
        if models {
            let batch = result(&mut auto, "ocr_recognize_files", json!({"paths":[name],"folder":"out"}));
            let error = batch["files"][0]["error"].as_str().unwrap();
            assert!(error.contains(why), "{name}: {batch}");
            assert!(!path.join("out").join(name).exists());
        }
        let d = auto.session().get(DocId(id)).unwrap();
        assert_eq!((&d.bytes, d.can_undo(), d.dirty), (&before, None, false));
        assert_eq!(std::fs::read(path.join(name)).unwrap(), file);
        result(&mut auto, "doc_close", json!({"doc":id}));
    }
    if !models {
        eprintln!("skipped owner and batch cases: OCR models not installed");
        return;
    }
    eprintln!("running actual pinned OCR models for owner-password and later-change cases");
    // The owner password lifts every restriction.
    let owner = result(&mut auto, "doc_open", json!({"path":"both-denied.pdf","password":"owner"}))["doc"].as_u64().unwrap();
    let review = result(&mut auto, "ocr_prepare", json!({"doc":owner}));
    assert!(review["words"].as_u64().unwrap() >= 4, "{review}");
    // A later security change (a new revision) invalidates the allowed draft: no words are
    // returned, nothing is applied, and the draft is discarded.
    result(&mut auto, "doc_protect", json!({"doc":owner,"permissions_password":"other","copy":false,"changes":"none"}));
    let before = auto.session().get(DocId(owner)).unwrap();
    let before = (before.bytes.clone(), before.can_undo().map(str::to_owned));
    let error = auto.call("ocr_review", &json!({"doc":owner})).unwrap_err().to_string();
    assert!(error.contains("changed") && !error.to_lowercase().contains("searchable"), "{error}");
    assert!(auto.call("ocr_correct", &json!({"doc":owner,"page":1,"word":1,"text":"x"})).is_err());
    assert!(auto.call("ocr_apply", &json!({"doc":owner})).is_err());
    assert_eq!(result(&mut auto, "ocr_cancel", json!({"doc":owner}))["canceled"], false);
    let after = auto.session().get(DocId(owner)).unwrap();
    assert_eq!((after.bytes.clone(), after.can_undo().map(str::to_owned)), before);
}
