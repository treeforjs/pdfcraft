//! Signature tools: list and validate signatures, create a self-signed digital ID, sign a
//! document, and manage trusted certificates. Rectangles are points from the top-left of the
//! displayed page.

use pdfcraft_engine::sign::token::{self as tok, Login, Pin, TokenError};
use pdfcraft_engine::sign::{self, Certificate, DigitalId, Modification, Name, PrivateKey};
use pdfcraft_engine::{SignOptions, SignatureInfo, SignatureStatus};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed, write_atomic};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// Seconds since 1970 → a DER time.
fn time(secs: i64) -> sign::Time {
    sign::Time::from_unix(secs)
}

pub(crate) fn cert_json(c: &Certificate) -> Value {
    json!({
        "name": c.display_name(),
        "subject": c.subject.display(),
        "issuer": c.issuer.display(),
        "email": c.subject.email(),
        "serial": c.serial_hex(),
        "valid_from": c.not_before.to_string(),
        "valid_to": c.not_after.to_string(),
        "key": c.public_key.describe(),
        "self_signed": c.is_self_signed(),
        "sha256": c.fingerprint(),
    })
}

/// A token error as a tool error: what the caller can fix (PIN, slot) is an argument error.
fn token_err(e: TokenError) -> ToolError {
    match e {
        TokenError::WrongPin { .. } | TokenError::PinRequired | TokenError::PinTooLong | TokenError::NoSuchSlot(_) | TokenError::NoIdentity(_) => {
            bad(e.to_string())
        }
        e => failed(e),
    }
}

fn slot_json(s: &tok::SlotInfo) -> Value {
    json!({
        "slot": s.id,
        "description": s.description,
        "token": s.token.as_ref().map(|t| json!({
            "label": t.label,
            "manufacturer": t.manufacturer,
            "model": t.model,
            "serial": t.serial,
            "login_required": t.login_required,
            "pin_pad": t.protected_path,
            "pin_locked": t.pin_locked,
            "pin_count_low": t.pin_count_low,
            "pin_final_try": t.pin_final_try,
        })),
    })
}

impl Automation {
    /// The module a token tool names (`module`), or the first one found. With a root, only a
    /// module found in the usual install locations or one inside the root is loaded: loading a
    /// module runs its code.
    fn token_module(&self, a: &Args) -> Result<(String, std::sync::Arc<dyn tok::TokenModule>)> {
        let module = match a.opt_str("module")?.map(str::trim).filter(|m| !m.is_empty()) {
            Some(m) if self.root.is_some() && !self.token_provider.discover().iter().any(|d| d == m) => {
                Some(self.resolve(m, false)?.to_string_lossy().into_owned())
            }
            Some(m) => Some(m.to_string()),
            None => None,
        };
        tok::load_module(self.token_provider.as_ref(), module.as_deref()).map_err(token_err)
    }

    /// Unlock the token in `slot` (or the only slot with a token) with `pin`.
    fn token_login(&self, a: &Args) -> Result<Login> {
        let (_, module) = self.token_module(a)?;
        let slot = match a.opt_int("slot")? {
            Some(n) => u64::try_from(n).map_err(|_| bad("slot must not be negative"))?,
            None => {
                let with_token: Vec<u64> =
                    tok::slots(module.as_ref()).map_err(token_err)?.iter().filter(|s| s.token.is_some()).map(|s| s.id).collect();
                match with_token.as_slice() {
                    [one] => *one,
                    [] => return Err(failed(TokenError::TokenNotPresent)),
                    _ => return Err(bad("several slots have a token: give slot (see sign_token_slots)")),
                }
            }
        };
        let pin = a.opt_str("pin")?.map(|p| Pin::from_bytes(p.as_bytes().to_vec()));
        tok::login(module.as_ref(), slot, pin).map_err(token_err)
    }

    /// sign_token_modules: the PKCS #11 modules found in the usual places.
    pub(crate) fn sign_token_modules(&self) -> Result<Value> {
        let modules = self.token_provider.discover();
        let available = !matches!(self.token_provider.load(""), Err(TokenError::NotAvailable));
        Ok(json!({ "available": available, "count": modules.len(), "modules": modules }))
    }

    /// sign_token_slots: a module's slots and the tokens in them.
    pub(crate) fn sign_token_slots(&self, a: &Args) -> Result<Value> {
        let (path, module) = self.token_module(a)?;
        let info = module.info().ok();
        let slots = tok::slots(module.as_ref()).map_err(token_err)?;
        Ok(json!({
            "module": path,
            "description": info.as_ref().map(|i| i.description.clone()),
            "manufacturer": info.as_ref().map(|i| i.manufacturer.clone()),
            "slots": slots.iter().map(slot_json).collect::<Vec<_>>(),
        }))
    }

    /// sign_token_ids: unlock a token and list the certificates that can sign on it.
    pub(crate) fn sign_token_ids(&self, a: &Args) -> Result<Value> {
        let login = self.token_login(a)?;
        let found = login.identities().map_err(token_err)?;
        Ok(json!({
            "slot": login.slot,
            "token": login.token.label,
            "count": found.ids.len(),
            "ids": found.ids.iter().map(|i| json!({ "id": i.reference, "label": i.label, "chain": i.chain.len(), "certificate": cert_json(&i.certificate) })).collect::<Vec<_>>(),
            "skipped": found.skipped.iter().map(|s| json!({ "label": s.label, "reason": s.reason })).collect::<Vec<_>>(),
        }))
    }

    /// Signature field properties ▸ Signed: what an unsigned signature field locks when signed.
    pub(crate) fn sign_field_lock(&mut self, a: &Args) -> Result<Value> {
        let field = a.str("field")?.to_string();
        let lock = field_lock(a)?;
        let id = self.doc(a)?.id;
        self.session.apply(id, pdfcraft_engine::Edit::SetSignatureLock { field: field.clone(), lock }).map_err(failed)?;
        let lock = self.doc(a)?.signature_field_lock(&field);
        Ok(json!({ "field": field, "lock": lock.as_ref().map(lock_json) }))
    }

    pub(crate) fn sig_json(&self, a: &Args, s: &SignatureInfo) -> Result<Value> {
        let doc = self.doc(a)?;
        let rect = match (s.page, s.rect) {
            (Some(p), Some(r)) if s.visible => doc.info.pages.get(p).map(|pi| {
                let (a0, a1) = (pi.user_to_view(r[0] as f32, r[3] as f32), pi.user_to_view(r[2] as f32, r[1] as f32));
                json!([a0[0].min(a1[0]), a0[1].min(a1[1]), a0[0].max(a1[0]), a0[1].max(a1[1])])
            }),
            _ => None,
        };
        let (modification, changes) = match &s.modification {
            Modification::None => ("none", Vec::new()),
            Modification::Allowed(c) => ("allowed", c.clone()),
            Modification::Disallowed(c) => ("disallowed", c.clone()),
        };
        Ok(json!({
            "field": s.field,
            "signed": s.signed,
            "status": if !s.signed { "unsigned" } else { match s.status { SignatureStatus::Valid => "valid", SignatureStatus::Unknown => "unknown", SignatureStatus::Invalid => "invalid" } },
            "summary": s.summary(),
            "signer": s.signer,
            "certificate": s.certificate.as_ref().map(cert_json),
            "chain": s.chain.iter().skip(1).map(cert_json).collect::<Vec<_>>(),
            "trust_source": s.trust_source.map(|t| match t {
                sign::TrustSource::TrustedCertificates => "trusted_certificates",
                sign::TrustSource::OperatingSystem => "operating_system",
            }),
            "date": s.date,
            "reason": s.reason,
            "location": s.location,
            "contact": s.contact,
            "certify": s.certify,
            "lock": if s.signed { s.lock.as_ref().map(lock_json) } else { doc.signature_field_lock(&s.field).as_ref().map(lock_json) },
            "page": s.page.map(|p| p + 1),
            "rect": rect,
            "visible": s.visible,
            "revision": s.revision,
            "sub_filter": s.sub_filter,
            "algorithm": s.algorithm,
            "timestamp": s.timestamp,
            "timestamp_time": s.timestamp_time.map(|t| t.to_string()),
            "verification_time": s.verification_time.map(|t| t.to_string()),
            "verification_time_source": match s.verification_time_source {
                sign::VerificationTimeSource::TrustedTimestamp => "trusted_timestamp",
                sign::VerificationTimeSource::ClaimedSigningTime => "claimed_signing_time",
                sign::VerificationTimeSource::ChosenTime => "chosen_time",
                sign::VerificationTimeSource::Unavailable => "unavailable",
            },
            "modification": modification,
            "changes": changes,
            "details": s.details,
        }))
    }

    pub(crate) fn sign_list(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let list: Vec<Value> = doc.signatures.iter().map(|s| self.sig_json(a, s)).collect::<Result<_>>()?;
        let signed = doc.signatures.iter().filter(|s| s.signed).count();
        let all_valid = signed > 0 && doc.signatures.iter().filter(|s| s.signed).all(|s| s.status == SignatureStatus::Valid);
        // The per-revision outline: what each saved revision changed, page by page.
        let revisions: Vec<Value> = doc
            .revisions
            .iter()
            .map(|r| {
                let by = doc.signatures.iter().find(|s| s.signed && s.revision == r.revision).map(|s| s.field.as_str());
                let changes: Vec<Value> = r.changes.iter().map(|c| json!({ "page": c.page.map(|p| p + 1), "kind": c.kind })).collect();
                json!({ "revision": r.revision, "length": r.end, "signature": by, "changes": changes })
            })
            .collect();
        Ok(json!({ "count": list.len(), "signed": signed, "all_valid": all_valid, "signatures": list, "revisions": revisions }))
    }

    pub(crate) fn sign_id_create(&mut self, a: &Args) -> Result<Value> {
        let name = a.str("name")?.trim().to_string();
        if name.is_empty() {
            return Err(bad("name must not be empty"));
        }
        let password = a.str("password")?;
        if password.chars().count() < 6 {
            return Err(bad("the password must have at least 6 characters"));
        }
        let path = self.resolve(a.str("path")?, true)?;
        let key = match a.opt_str("key")?.unwrap_or("rsa2048") {
            "rsa2048" => PrivateKey::generate_rsa(2048),
            "rsa3072" => PrivateKey::generate_rsa(3072),
            "rsa4096" => PrivateKey::generate_rsa(4096),
            "p256" => PrivateKey::generate_p256(),
            other => return Err(bad(format!("unknown key {other:?} (rsa2048, rsa3072, rsa4096, p256)"))),
        }
        .map_err(failed)?;
        let dn = Name::build(
            &name,
            a.opt_str("unit")?.unwrap_or(""),
            a.opt_str("organization")?.unwrap_or(""),
            a.opt_str("email")?.unwrap_or(""),
            a.opt_str("country")?.unwrap_or(""),
        );
        let years = a.opt_int("years")?.unwrap_or(5).clamp(1, 50) as u32;
        let now = self.session.now_secs();
        let serial = sign::keys::DigestAlg::Sha256.digest(&[name.as_bytes(), &now.to_be_bytes()])[..8].to_vec();
        let cert = Certificate::self_signed(&dn, &key, time(now), years, &serial).map_err(failed)?;
        let id = DigitalId { key, certificate: cert, chain: Vec::new(), friendly_name: Some(name) };
        let p12 = sign::pkcs12::write(&id, password).map_err(failed)?;
        write_atomic(&path, &p12)?;
        Ok(json!({ "path": path.to_string_lossy(), "certificate": cert_json(&id.certificate) }))
    }

    fn open_id(&self, a: &Args) -> Result<DigitalId> {
        // A macOS Keychain identity: "keychain:<fingerprint>" or "keychain:<common name>".
        if let Some(r) = a.str("id")?.strip_prefix("keychain:") {
            #[cfg(target_os = "macos")]
            return sign::keychain::find(&format!("keychain:{r}")).map_err(failed);
            #[cfg(not(target_os = "macos"))]
            return Err(failed(format!("keychain:{r}: Keychain identities are only available on macOS")));
        }
        // A smart card or token: "pkcs11:<fingerprint or common name>", with module, slot and pin.
        if a.str("id")?.starts_with("pkcs11:") {
            let login = self.token_login(a)?;
            return login.find(a.str("id")?).map_err(token_err);
        }
        if let Some(r) = a.str("id")?.strip_prefix("windows:") {
            #[cfg(target_os = "windows")]
            return sign::windows::find(&format!("windows:{r}")).map_err(failed);
            #[cfg(not(target_os = "windows"))]
            return Err(failed(format!("windows:{r}: Windows certificate store identities are only available on Windows")));
        }
        let path = self.resolve(a.str("id")?, false)?;
        let bytes = std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
        sign::pkcs12::open(&bytes, a.opt_str("password")?.unwrap_or("")).map_err(|e| match e {
            sign::SignError::WrongPassword => bad("the digital ID password is incorrect"),
            e => failed(e),
        })
    }

    pub(crate) fn sign_document(&mut self, a: &Args) -> Result<Value> {
        let id = self.open_id(a)?;
        let doc = self.doc(a)?;
        let doc_id = doc.id;
        let field = a.opt_str("field")?.map(str::to_string);
        let page = match a.opt_int("page")? {
            Some(p) if p >= 1 && (p as usize) <= doc.info.pages.len() => p as usize - 1,
            Some(p) => return Err(bad(format!("page {p} does not exist"))),
            None => 0,
        };
        let rect = match a.nums::<4>("rect")? {
            Some(r) => {
                let p = &doc.info.pages[page];
                let (u0, u1) = (p.view_to_user(r[0] as f32, r[1] as f32), p.view_to_user(r[2] as f32, r[3] as f32));
                let u = [u0[0].min(u1[0]) as f64, u0[1].min(u1[1]) as f64, u0[0].max(u1[0]) as f64, u0[1].max(u1[1]) as f64];
                if u[2] - u[0] < 4.0 || u[3] - u[1] < 4.0 {
                    return Err(bad("the signature rectangle is too small"));
                }
                Some(u)
            }
            None => None,
        };
        let certify = match a.get("certify") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(match s.as_str() {
                "no_changes" => 1,
                "form_fill" => 2,
                "comments" => 3,
                other => return Err(bad(format!("unknown certify level {other:?} (no_changes, form_fill, comments)"))),
            }),
            Some(v) => Some(v.as_u64().filter(|n| (1..=3).contains(n)).ok_or_else(|| bad("certify must be 1, 2 or 3"))? as u8),
        };
        let out = self.resolve(a.str("out")?, true)?;
        let opts = SignOptions {
            field,
            page,
            rect,
            // Preferences ▸ Signatures: the default reason and location, when not given.
            reason: a.opt_str("reason")?.map(str::to_string).or_else(|| nonempty(&self.session.prefs().signing_reason)),
            location: a.opt_str("location")?.map(str::to_string).or_else(|| nonempty(&self.session.prefs().signing_location)),
            contact: a.opt_str("contact")?.map(str::to_string),
            certify,
            lock: field_lock(a)?,
            ..SignOptions::default()
        };
        let authority = self.timestamp_authority(a)?;
        let job = self.session.signature_job(doc_id).map_err(failed)?;
        let result = job.sign(&id, opts, authority.as_ref().map(|a| a as &dyn sign::TimestampAuthority)).map_err(failed)?;
        self.session.check_signature_result(&result).map_err(failed)?;
        let signed = result.bytes;
        write_atomic(&out, &signed)?;
        let path = out.to_string_lossy().into_owned();
        self.session.mark_signed(doc_id, signed.clone(), Some(path.clone())).map_err(failed)?;
        let doc = self.doc(a)?;
        let newest = doc.signatures.iter().filter(|s| s.signed).max_by_key(|s| s.revision).cloned();
        Ok(json!({ "path": path, "bytes": signed.len(), "signature": newest.map(|s| self.sig_json(a, &s)).transpose()? }))
    }

    pub(crate) fn sign_trust(&mut self, a: &Args) -> Result<Value> {
        let mut certs: Vec<Certificate> =
            if a.opt_bool("clear")?.unwrap_or(false) { Vec::new() } else { self.session.trusted_certificates().to_vec() };
        let paths: Vec<String> = match a.get("paths") {
            None => Vec::new(),
            Some(v) => v
                .as_array()
                .ok_or_else(|| bad("paths must be an array"))?
                .iter()
                .map(|p| p.as_str().map(str::to_string).ok_or_else(|| bad("paths must be strings")))
                .collect::<Result<_>>()?,
        };
        for p in paths {
            let path = self.resolve(&p, false)?;
            let bytes = std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
            let lower = p.to_ascii_lowercase();
            let found = if lower.ends_with(".p12") || lower.ends_with(".pfx") {
                let id = sign::pkcs12::open(&bytes, a.opt_str("password")?.unwrap_or("")).map_err(failed)?;
                std::iter::once(id.certificate).chain(id.chain).collect()
            } else {
                sign::x509::load_certificates(&bytes).map_err(failed)?
            };
            for c in found {
                if !certs.iter().any(|t| t.raw == c.raw) {
                    certs.push(c);
                }
            }
        }
        self.session.set_trusted_certificates(certs);
        Ok(json!({ "trusted": self.session.trusted_certificates().iter().map(cert_json).collect::<Vec<_>>() }))
    }
}

/// At most this many roots are listed per `sign_os_roots` call.
const MAX_LISTED_ROOTS: usize = 1000;

impl Automation {
    fn os_trust_json(&self) -> Value {
        let s = self.session.os_trust_settings();
        json!({
            "signatures": s.signatures,
            "certified_documents": s.certified_documents,
            // As in Acrobat, trusting them for certified documents needs the signatures box too.
            "certified_documents_effective": s.signatures && s.certified_documents,
            "source": self.session.os_trust_source().describe(),
        })
    }

    /// Preferences ▸ Signatures: trust the operating system's root certificates.
    pub(crate) fn sign_os_trust_get(&self) -> Result<Value> {
        Ok(self.os_trust_json())
    }

    pub(crate) fn sign_os_trust_set(&mut self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::os_trust::{OsRootSource, OsTrustSettings};
        let source = match a.opt_str("source")? {
            None => None,
            Some("system") => {
                if a.get("path").is_some_and(|v| !v.is_null()) {
                    return Err(bad("path is only for source bundle or directory"));
                }
                Some(OsRootSource::System)
            }
            Some(kind @ ("bundle" | "directory")) => {
                let path = self.resolve(a.opt_str("path")?.ok_or_else(|| bad(format!("source {kind} needs path")))?, false)?;
                Some(if kind == "bundle" { OsRootSource::Bundle(path) } else { OsRootSource::Directory(path) })
            }
            Some(other) => return Err(bad(format!("unknown source {other:?} (system, bundle, directory)"))),
        };
        if source.is_none() && a.get("path").is_some_and(|v| !v.is_null()) {
            return Err(bad("path needs source bundle or directory"));
        }
        let current = self.session.os_trust_settings();
        let settings = OsTrustSettings {
            signatures: a.opt_bool("signatures")?.unwrap_or(current.signatures),
            certified_documents: a.opt_bool("certified_documents")?.unwrap_or(current.certified_documents),
        };
        if let Some(source) = source {
            self.session.set_os_trust_source(source).map_err(failed)?;
        }
        self.session.set_os_trust(settings).map_err(failed)?;
        let mut out = self.os_trust_json();
        if settings.signatures || settings.certified_documents {
            let roots = self.session.os_roots(false).map_err(failed)?;
            out["roots"] = json!(roots.certificates.len());
            out["skipped"] = json!(roots.skipped);
        }
        Ok(out)
    }

    /// The operating system's root certificates as read from the configured source.
    pub(crate) fn sign_os_roots(&mut self, a: &Args) -> Result<Value> {
        let reload = a.opt_bool("reload")?.unwrap_or(false);
        let query = a.opt_str("query")?.map(str::to_lowercase);
        let offset = usize::try_from(a.opt_int("offset")?.unwrap_or(0)).map_err(|_| bad("offset must be 0 or more"))?;
        let limit = a.opt_int("limit")?.unwrap_or(100);
        if !(1..=MAX_LISTED_ROOTS as i64).contains(&limit) {
            return Err(bad(format!("limit must be 1 to {MAX_LISTED_ROOTS}")));
        }
        let roots = self.session.os_roots(reload).map_err(failed)?;
        let matching: Vec<&Certificate> = roots
            .certificates
            .iter()
            .filter(|c| {
                query.as_deref().is_none_or(|q| {
                    c.subject.display().to_lowercase().contains(q)
                        || c.display_name().to_lowercase().contains(q)
                        || c.fingerprint().to_lowercase().contains(q)
                })
            })
            .collect();
        let listed: Vec<Value> = matching.iter().skip(offset).take(limit as usize).map(|c| cert_json(c)).collect();
        let mut out = self.os_trust_json();
        out["supported"] = json!(roots.supported);
        out["count"] = json!(roots.certificates.len());
        out["matching"] = json!(matching.len());
        out["offset"] = json!(offset);
        out["skipped"] = json!(roots.skipped);
        out["duplicates"] = json!(roots.duplicates);
        out["truncated"] = json!(roots.truncated);
        out["errors"] = json!(roots.errors);
        out["roots"] = json!(listed);
        Ok(out)
    }
}

fn lock_json(l: &sign::FieldLock) -> Value {
    let (action, fields) = match &l.action {
        sign::LockAction::All => ("all", Vec::new()),
        sign::LockAction::Include(f) => ("include", f.clone()),
        sign::LockAction::Exclude(f) => ("exclude", f.clone()),
    };
    json!({ "action": action, "fields": fields, "lock_document": l.permissions == Some(1), "permissions": l.permissions })
}

/// A field lock from `lock` (all, include or exclude, with `lock_fields` naming the fields) and
/// `lock_document` (no changes after signing). `lock: "none"` (or nothing) is no lock.
fn field_lock(a: &Args) -> Result<Option<sign::FieldLock>> {
    let fields = || -> Result<Vec<String>> {
        let names: Vec<String> = match a.get("lock_fields") {
            None | Some(Value::Null) => Vec::new(),
            Some(_) => a.strs("lock_fields")?.into_iter().map(str::to_string).collect(),
        };
        if names.is_empty() {
            return Err(bad("lock include and exclude need lock_fields"));
        }
        Ok(names)
    };
    let action = match a.opt_str("lock")?.unwrap_or("none") {
        "none" => None,
        "all" => Some(sign::LockAction::All),
        "include" => Some(sign::LockAction::Include(fields()?)),
        "exclude" => Some(sign::LockAction::Exclude(fields()?)),
        other => return Err(bad(format!("unknown lock {other:?} (none, all, include, exclude)"))),
    };
    let document = a.opt_bool("lock_document")?.unwrap_or(false);
    Ok(match (action, document) {
        (None, false) => None,
        (action, document) => Some(sign::FieldLock { action: action.unwrap_or(sign::LockAction::All), permissions: document.then_some(1) }),
    })
}

/// `s`, unless it is empty.
fn nonempty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}
