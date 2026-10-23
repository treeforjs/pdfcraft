//! OCR in the web app (ocr.ocr-web): `ocr_model_verify` checks model bytes as the browser checks
//! its download and stored copy (exact pinned length, then SHA-256), and `ocr_status` lists the
//! pinned models. Hostile input is an error or a failed check, never a panic.

use base64::Engine as _;
use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn ok(a: &mut Automation, tool: &str, args: Value) -> Value {
    match a.call(tool, &args) {
        Ok(mut c) => match c.remove(0) {
            Content::Json(v) => v,
            other => panic!("{tool}: expected JSON, got {other:?}"),
        },
        Err(e) => panic!("{tool} {args}: {e}"),
    }
}

fn err(a: &mut Automation, tool: &str, args: Value) -> String {
    match a.call(tool, &args) {
        Ok(_) => panic!("{tool} {args} should fail"),
        Err(e) => e.to_string(),
    }
}

fn root(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-web-ocr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn status_lists_the_pinned_models() {
    let mut a = Automation::new();
    let s = ok(&mut a, "ocr_status", json!({}));
    let models = s["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0]["file"], "text-detection.rten");
    assert_eq!(models[0]["bytes"], 2_510_284);
    assert_eq!(models[1]["web_path"], "models/text-recognition.rten");
    assert_eq!(models[1]["sha256"].as_str().unwrap().len(), 64);
    assert!(s["loaded"].is_boolean());
}

#[test]
fn wrong_bytes_fail_the_check_and_hostile_arguments_are_errors() {
    let dir = root("verify");
    let mut a = Automation::new().with_root(&dir).unwrap();
    let r = ok(&mut a, "ocr_model_verify", json!({ "file": "text-detection.rten", "data_base64": b64(b"not a model") }));
    assert_eq!((r["ok"].as_bool(), r["bytes"].as_u64()), (Some(false), Some(11)));
    assert!(r["error"].as_str().unwrap().contains("pinned model is 2510284 bytes"), "{r}");

    // Right length, wrong bytes: the checksum fails.
    std::fs::write(dir.join("zeros.rten"), vec![0u8; 2_510_284]).unwrap();
    let r = ok(&mut a, "ocr_model_verify", json!({ "file": "text-detection.rten", "path": "zeros.rten" }));
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("checksum"), "{r}");
    assert_eq!(r["sha256"].as_str().unwrap().len(), 64);

    // Larger than any model: not read.
    let big = std::fs::File::create(dir.join("big.rten")).unwrap();
    big.set_len(64 * 1024 * 1024).unwrap();
    let r = ok(&mut a, "ocr_model_verify", json!({ "file": "text-recognition.rten", "path": "big.rten" }));
    assert_eq!((r["ok"].as_bool(), r["sha256"].is_null()), (Some(false), true));

    for args in [
        json!({ "file": "text-detection.rten" }),
        json!({ "file": "../etc/passwd", "data_base64": "AA==" }),
        json!({ "file": "text-detection.rten", "data_base64": "not base64!" }),
        json!({ "file": "text-detection.rten", "data_base64": "AA==", "path": "zeros.rten" }),
        json!({ "file": "text-detection.rten", "data_base64": "A".repeat(48 * 1024 * 1024) }),
        json!({ "data_base64": "AA==" }),
    ] {
        err(&mut a, "ocr_model_verify", args);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The installed models (`cargo xtask models`) pass the browser's check.
#[test]
fn installed_models_pass() {
    let Some(models) = pdfcraft_engine::ocr::Models::find() else {
        eprintln!("skipped: OCR models not installed");
        return;
    };
    let mut a = Automation::new();
    for (file, path) in [("text-detection.rten", &models.detection), ("text-recognition.rten", &models.recognition)] {
        let data = std::fs::read(path).unwrap();
        let r = ok(&mut a, "ocr_model_verify", json!({ "file": file, "data_base64": b64(&data) }));
        assert_eq!(r["ok"], true, "{r}");
    }
}
