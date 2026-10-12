//! Certificate (public-key) security tools (ISO 32000-2 §7.6.5): open a certificate-encrypted
//! document with a digital ID (`doc_open` with `certificate`) and encrypt a document for
//! recipient certificates with per-recipient permissions (`security_protect_certificates`).

use pdfcraft_engine::sign::{self, Certificate, DigitalId};
use pdfcraft_engine::{Algorithm, CertificateProtection, CertificateRecipient, Changes, Edit, MAX_CERTIFICATE_RECIPIENTS, Printing, RecipientAccess};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// The most bytes read from one certificate or digital ID file.
const MAX_CERTIFICATE_FILE: u64 = 1 << 20;

fn printing_of(v: Option<&str>) -> Result<Printing> {
    Ok(match v {
        None | Some("high") => Printing::High,
        Some("low") => Printing::Low,
        Some("none") => Printing::None,
        Some(o) => return Err(bad(format!("unknown printing {o:?} (none, low or high)"))),
    })
}

fn changes_of(v: Option<&str>) -> Result<Changes> {
    Ok(match v {
        None | Some("none") => Changes::None,
        Some("pages") => Changes::Pages,
        Some("fill-sign") => Changes::FillSign,
        Some("comment-fill-sign") => Changes::CommentFillSign,
        Some("any-except-extract") => Changes::AnyExceptExtract,
        Some(o) => return Err(bad(format!("unknown changes {o:?}"))),
    })
}

/// What `access` allows, as JSON (the same keys `doc_info` uses for security).
fn access_json(a: &RecipientAccess) -> Value {
    let printing = match a.printing {
        _ if a.change_security => "high",
        Printing::None => "none",
        Printing::Low => "low",
        Printing::High => "high",
    };
    let changes = match a.changes {
        _ if a.change_security => "any",
        Changes::None => "none",
        Changes::Pages => "pages",
        Changes::FillSign => "fill-sign",
        Changes::CommentFillSign => "comment-fill-sign",
        Changes::AnyExceptExtract => "any-except-extract",
    };
    json!({
        "printing": printing,
        "changes": changes,
        "copy": a.copy || a.change_security,
        "accessibility": a.accessibility || a.change_security,
        "change_security": a.change_security,
    })
}

impl Automation {
    /// Read a file named by a tool argument, bounded in size.
    fn read_small(&self, path: &str) -> Result<Vec<u8>> {
        let p = self.resolve(path, false)?;
        let len = std::fs::metadata(&p).map_err(|e| failed(format!("{}: {e}", p.display())))?.len();
        if len > MAX_CERTIFICATE_FILE {
            return Err(bad(format!("{path} is too large for a certificate or digital ID ({len} bytes)")));
        }
        std::fs::read(&p).map_err(|e| failed(format!("{}: {e}", p.display())))
    }

    /// A digital ID file (.p12/.pfx) and its password.
    pub(crate) fn digital_id_file(&self, path: &str, password: Option<&str>) -> Result<DigitalId> {
        let bytes = self.read_small(path)?;
        sign::pkcs12::open(&bytes, password.unwrap_or("")).map_err(|e| match e {
            sign::SignError::WrongPassword => bad("the digital ID password is incorrect"),
            e => failed(format!("{path}: {e}")),
        })
    }

    /// `doc_open` with `certificate`: open a certificate-encrypted document with a digital ID.
    pub(crate) fn open_with_certificate(&mut self, a: &Args) -> Result<Value> {
        if a.get("password").is_some() {
            return Err(bad("pass either password or certificate, not both"));
        }
        let id = self.digital_id_file(a.str("certificate")?, a.opt_str("certificate_password")?)?;
        let path = self.resolve(a.str("path")?, false)?;
        let bytes = std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let doc =
            self.session.open_with_certificate(name, Some(path.to_string_lossy().into_owned()), std::sync::Arc::new(bytes), &id).map_err(failed)?;
        let d = self.session.get(doc).ok_or_else(|| failed("the document vanished"))?;
        let mut out = crate::summary(d);
        out["security"] = crate::security(d);
        Ok(out)
    }

    /// One recipient certificate: a certificate file (PEM or DER), or the certificate of a
    /// digital ID file (with `password`).
    fn recipient_certificate(&self, path: &str, password: Option<&str>) -> Result<Certificate> {
        let bytes = self.read_small(path)?;
        if password.is_none()
            && let Ok(mut certs) = sign::x509::load_certificates(&bytes)
            && !certs.is_empty()
        {
            if certs.len() > 1 {
                return Err(bad(format!("{path} holds {} certificates; give each recipient its own file", certs.len())));
            }
            return Ok(certs.remove(0));
        }
        match sign::pkcs12::open(&bytes, password.unwrap_or("")) {
            Ok(id) => Ok(id.certificate),
            Err(sign::SignError::WrongPassword) => Err(bad(format!("{path}: the digital ID password is incorrect (or not a certificate file)"))),
            Err(e) => Err(bad(format!("{path}: not a certificate (.cer, .crt, .pem, .der) or digital ID (.p12, .pfx): {e}"))),
        }
    }

    /// `security_protect_certificates`.
    pub(crate) fn protect_certificates(&mut self, a: &Args) -> Result<Value> {
        let list = a.get("recipients").and_then(Value::as_array).ok_or_else(|| bad("recipients must be an array of objects"))?;
        if list.is_empty() || list.len() > MAX_CERTIFICATE_RECIPIENTS {
            return Err(bad(format!("give 1 to {MAX_CERTIFICATE_RECIPIENTS} recipients")));
        }
        let mut recipients = Vec::with_capacity(list.len());
        for (i, r) in list.iter().enumerate() {
            let r = Args(r);
            let at = |e: ToolError| match e {
                ToolError::InvalidArgs(m) => bad(format!("recipients[{i}]: {m}")),
                other => other,
            };
            if !r.0.is_object() {
                return Err(bad(format!("recipients[{i}] must be an object")));
            }
            let certificate = self.recipient_certificate(r.str("certificate").map_err(at)?, r.opt_str("password").map_err(at)?).map_err(at)?;
            let access = RecipientAccess {
                printing: printing_of(r.opt_str("printing").map_err(at)?).map_err(at)?,
                changes: changes_of(r.opt_str("changes").map_err(at)?).map_err(at)?,
                copy: r.opt_bool("copy").map_err(at)?.unwrap_or(false),
                accessibility: r.opt_bool("accessibility").map_err(at)?.unwrap_or(true),
                change_security: r.opt_bool("change_security").map_err(at)?.unwrap_or(false),
            };
            recipients.push(CertificateRecipient { certificate, access });
        }
        let algorithm = match a.opt_str("compatibility")? {
            None | Some("aes-256") => Algorithm::Aes256,
            Some("aes-128") => Algorithm::Aes128,
            Some(o) => return Err(bad(format!("unknown compatibility {o:?} (aes-256 or aes-128)"))),
        };
        let listed: Vec<Value> = recipients
            .iter()
            .map(|r| json!({ "name": r.certificate.display_name(), "serial": r.certificate.serial_hex(), "permissions": access_json(&r.access) }))
            .collect();
        let encrypt_metadata = a.opt_bool("encrypt_metadata")?.unwrap_or(true);
        let mut out = self.apply(a, Edit::ProtectCertificates(CertificateProtection { recipients, algorithm, encrypt_metadata }))?;
        out["security"] = crate::security(self.doc(a)?);
        out["recipients"] = Value::Array(listed);
        Ok(out)
    }
}
