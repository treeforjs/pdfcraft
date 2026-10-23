//! `ocr_model_verify`: check model bytes the way the web app checks the text recognition models
//! it downloads or finds in browser storage (exact pinned length, then SHA-256), given as bytes
//! or a path. Nothing is loaded or kept.

use base64::Engine as _;
use pdfcraft_engine::ocr::models;
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// The longest `data_base64` read: [`models::MAX_MODEL_BYTES`] encoded, with slack for padding
/// and line breaks.
const MAX_BASE64: usize = (models::MAX_MODEL_BYTES as usize) / 3 * 4 + 4096;

impl Automation {
    pub(crate) fn ocr_model_verify(&self, a: &Args) -> Result<Value> {
        let file = a.str("file")?;
        let model = models::pinned(file)
            .ok_or_else(|| bad(format!("file must be one of {}", models::PINNED.iter().map(|m| m.file).collect::<Vec<_>>().join(", "))))?;
        let bytes = match (a.opt_str("path")?, a.opt_str("data_base64")?) {
            (Some(p), None) => {
                let path = self.resolve(p, false)?;
                let len = std::fs::metadata(&path).map_err(|e| failed(format!("{p}: {e}")))?.len();
                // Too large to be the model: reported without reading it.
                if len > models::MAX_MODEL_BYTES {
                    return Ok(report(model, len, None, models::check_length(file, len).err().map(|e| e.to_string())));
                }
                std::fs::read(&path).map_err(|e| failed(format!("{p}: {e}")))?
            }
            (None, Some(b)) => {
                if b.len() > MAX_BASE64 {
                    return Err(bad(format!("data_base64 is too long; the models are at most {} MiB", models::MAX_MODEL_BYTES / (1024 * 1024))));
                }
                let clean: String = b.chars().filter(|c| !c.is_ascii_whitespace()).collect();
                base64::engine::general_purpose::STANDARD
                    .decode(clean.as_bytes())
                    .map_err(|e| bad(format!("data_base64 is not valid base64: {e}")))?
            }
            (Some(_), Some(_)) => return Err(bad("pass path or data_base64, not both")),
            (None, None) => return Err(bad("pass the model file's path or its data_base64")),
        };
        let error = models::verify(file, &bytes).err().map(|e| e.to_string());
        Ok(report(model, bytes.len() as u64, Some(models::sha256_hex(&bytes)), error))
    }
}

fn report(model: &models::PinnedModel, bytes: u64, sha256: Option<String>, error: Option<String>) -> Value {
    json!({
        "file": model.file,
        "ok": error.is_none(),
        "bytes": bytes,
        "expected_bytes": model.bytes,
        "sha256": sha256,
        "expected_sha256": model.sha256,
        "error": error,
    })
}

/// The pinned models, for `ocr_status`.
pub(crate) fn pinned_json() -> Value {
    models::PINNED.iter().map(|m| json!({ "file": m.file, "bytes": m.bytes, "sha256": m.sha256, "web_path": format!("models/{}", m.file) })).collect()
}
