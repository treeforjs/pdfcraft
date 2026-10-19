//! OCR engine tools: ocr_engines_list, ocr_engine_set and the engine argument of the
//! recognition tools, with a fake engine registered on the session (no models or Tesseract).
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use pdfcraft_automation::{Automation, Content, ToolError};
use pdfcraft_engine::ocr::{EngineInfo, Line, OcrEngine, OcrError, Word};
use serde_json::{Value, json};

struct Fake {
    calls: AtomicUsize,
}

impl OcrEngine for Fake {
    fn id(&self) -> &str {
        "fake"
    }
    fn name(&self) -> &str {
        "Fake engine"
    }
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: "fake".into(),
            name: "Fake engine".into(),
            available: true,
            version: Some("9.9".into()),
            languages: vec!["en".into(), "fr".into()],
            external: false,
            detail: "registered by the test".into(),
        }
    }
    fn prepare(&self, _: &str) -> Result<(), OcrError> {
        Ok(())
    }
    fn recognize(&self, pixels: &[u8], width: u32, height: u32, language: &str) -> Result<Vec<Line>, OcrError> {
        assert!(width > 0 && height > 0 && !pixels.is_empty());
        self.calls.fetch_add(1, Ordering::SeqCst);
        let word = |text: &str, x: f32| Word { text: text.into(), rect: [x, 10.0, x + 30.0, 25.0], confidence: Some(0.8) };
        Ok(vec![Line { words: vec![word("faked", 10.0), word(language, 45.0)] }])
    }
}

fn ok(a: &mut Automation, name: &str, args: Value) -> Value {
    let mut reply = a.call(name, &args).unwrap_or_else(|e| panic!("{name}: {e}"));
    match reply.remove(0) {
        Content::Json(value) => value,
        other => panic!("expected JSON, got {other:?}"),
    }
}

fn root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("pdfcraft-ocr-engines-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn ocr_engine_tools_list_choose_and_read_with_the_engine() {
    let dir = root("tools");
    let mut a = Automation::new().with_root(&dir).unwrap();
    a.session_mut().set_tesseract_path(Some("/nonexistent/pdfcraft/tesseract".into())).unwrap();
    let list = ok(&mut a, "ocr_engines_list", json!({}));
    assert_eq!((list["selected"].as_str(), list["default"].as_str()), (Some("ocrs"), Some("ocrs")), "{list}");
    let ids: Vec<&str> = list["engines"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["ocrs", "tesseract"]);
    let ocrs = &list["engines"][0];
    assert_eq!((ocrs["selected"].as_bool(), ocrs["default"].as_bool(), ocrs["external"].as_bool()), (Some(true), Some(true), Some(false)));
    assert_eq!(ocrs["languages"], json!([{ "code": "en", "name": "English" }]));
    let tesseract = &list["engines"][1];
    assert_eq!((tesseract["available"].as_bool(), tesseract["external"].as_bool()), (Some(false), Some(true)), "{tesseract}");
    assert!(tesseract["detail"].as_str().unwrap().contains("not installed"));

    // Bad choices are refused and change nothing.
    for bad in [json!({ "engine": "nope" }), json!({}), json!({ "engine": 3 })] {
        assert!(matches!(a.call("ocr_engine_set", &bad), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
    assert!(a.call("ocr_engine_set", &json!({ "engine": "tesseract" })).is_err(), "not installed");
    assert_eq!(ok(&mut a, "ocr_engines_list", json!({}))["selected"], "ocrs");

    let fake = Arc::new(Fake { calls: AtomicUsize::new(0) });
    a.session_mut().register_ocr_engine(fake.clone()).unwrap();
    let set = ok(&mut a, "ocr_engine_set", json!({ "engine": "fake" }));
    assert_eq!((set["selected"].as_str(), set["version"].as_str()), (Some("fake"), Some("9.9")), "{set}");
    assert_eq!(ok(&mut a, "ocr_status", json!({}))["engine"], "fake");

    // ocr_recognize reads with the selected engine, in a language it reads.
    let doc = ok(&mut a, "doc_create", json!({ "from": "blank" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "ocr_recognize", json!({ "doc": doc, "dpi": 72, "language": "fr" }));
    assert_eq!(r["pages"][0]["text"], "faked fr", "{r}");
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    let before = fake.calls.load(Ordering::SeqCst);
    for bad in [
        json!({ "doc": doc, "engine": "nope" }),
        json!({ "doc": doc, "engine": "ocrs", "language": "fr" }),
        json!({ "doc": doc, "language": "de" }),
        json!({ "doc": doc, "language": "xx" }),
        json!({ "doc": doc, "engine": "" }),
    ] {
        for tool in ["ocr_recognize", "ocr_prepare"] {
            assert!(matches!(a.call(tool, &bad), Err(ToolError::InvalidArgs(_))), "{tool} {bad}");
        }
    }
    assert!(a.call("ocr_recognize", &json!({ "doc": doc, "engine": "tesseract" })).is_err(), "not installed");
    assert_eq!(fake.calls.load(Ordering::SeqCst), before, "nothing was read");
    assert!(a.call("edit_undo", &json!({ "doc": doc })).is_err(), "nothing was changed");

    // The draft tools and batch recognition use it too.
    let draft = ok(&mut a, "ocr_prepare", json!({ "doc": doc, "dpi": 72 }));
    assert_eq!(draft["pages"][0]["text"], "faked en", "{draft}");
    ok(&mut a, "ocr_cancel", json!({ "doc": doc }));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "blank.pdf" }));
    let files = ok(&mut a, "ocr_recognize_files", json!({ "paths": ["blank.pdf"], "folder": "out", "dpi": 72 }));
    assert_eq!(files["files"][0]["words"], 2, "{files}");
    assert!(dir.join("out/blank.pdf").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}
