//! `sign_id_inspect`: open a digital ID file given as bytes (as the web app gets a file picked
//! in the browser) or a path, and report what it would sign as. Nothing is kept: not the bytes,
//! not the password.

use base64::Engine as _;
use pdfcraft_engine::sign::{self, PublicKey};
use serde_json::{Value, json};

use crate::signing::cert_json;
use crate::{Args, Automation, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// The longest `data_base64` read: [`sign::id_file::MAX_ID_FILE_BYTES`] encoded, with slack
/// for padding and line breaks.
const MAX_BASE64: usize = sign::id_file::MAX_ID_FILE_BYTES / 3 * 4 + 4096;

impl Automation {
    pub(crate) fn sign_id_inspect(&self, a: &Args) -> Result<Value> {
        let bytes = match (a.opt_str("path")?, a.opt_str("data_base64")?) {
            (Some(p), None) => {
                let path = self.resolve(p, false)?;
                let len = std::fs::metadata(&path).map_err(|e| failed(format!("{p}: {e}")))?.len();
                if len > sign::id_file::MAX_ID_FILE_BYTES as u64 {
                    return Err(bad(format!("{p} is larger than {} KiB; it is not a digital ID file", sign::id_file::MAX_ID_FILE_BYTES / 1024)));
                }
                std::fs::read(&path).map_err(|e| failed(format!("{p}: {e}")))?
            }
            (None, Some(b)) => {
                if b.len() > MAX_BASE64 {
                    return Err(bad(format!(
                        "data_base64 is too long; digital ID files are at most {} KiB",
                        sign::id_file::MAX_ID_FILE_BYTES / 1024
                    )));
                }
                let clean: String = b.chars().filter(|c| !c.is_ascii_whitespace()).collect();
                base64::engine::general_purpose::STANDARD
                    .decode(clean.as_bytes())
                    .map_err(|e| bad(format!("data_base64 is not valid base64: {e}")))?
            }
            (Some(_), Some(_)) => return Err(bad("pass path or data_base64, not both")),
            (None, None) => return Err(bad("pass the digital ID file's path or its data_base64")),
        };
        let password = a.opt_str("password")?.unwrap_or("");
        let id = pdfcraft_engine::guard(|| sign::id_file::open(&bytes, password))
            .map_err(|m| failed(format!("the digital ID couldn't be read ({m})")))?
            .map_err(|e| match e {
                sign::SignError::WrongPassword => bad("the digital ID password is incorrect"),
                e => failed(e),
            })?;
        let now = self.session.now_secs();
        let c = &id.certificate;
        let valid_now = c.not_before.unix() <= now && now <= c.not_after.unix();
        // Every key a .p12 can hold here signs in the browser too: RSA through the pure-Rust
        // signer, P-256 and P-384 through RustCrypto.
        let browser = matches!(id.key.public_key(), PublicKey::Rsa { .. } | PublicKey::P256(_) | PublicKey::P384(_));
        let name = a.opt_str("name")?.map(sign::id_file::display_name);
        Ok(json!({
            "name": name,
            "friendly_name": id.friendly_name,
            "certificate": cert_json(c),
            "key": id.key.public_key().describe(),
            "digest": id.key.preferred_digest().name(),
            "chain": id.chain.iter().map(cert_json).collect::<Vec<_>>(),
            "valid_now": valid_now,
            "signs_in_browser": browser,
            "bytes": bytes.len(),
        }))
    }
}
