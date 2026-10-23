//! Preferences → Signatures tools: saved signature appearances (text, a picture or drawn
//! signature, a logo), per-certificate trust settings, the timestamp servers, and adding
//! verification information (LTV) with revocation evidence fetched where internet access allows.

use std::io::Read;

use pdfcraft_engine::revocation_fetch::HttpTransport;
use pdfcraft_engine::sign::{self, Appearance, CertTrust};
use pdfcraft_engine::signature_image::SignatureImage;
use pdfcraft_engine::signature_settings::{AppearanceGraphic, AppearanceImage, SavedAppearance};
use pdfcraft_engine::signature_workflow::{SignatureCancellation, TimestampServer};
use serde_json::{Value, json};

use crate::signing::cert_json;
use crate::{Args, Automation, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

fn appearance_json(a: &SavedAppearance) -> Value {
    json!({
        "name": a.name,
        "show_name": a.show_name,
        "date": a.date,
        "reason": a.reason,
        "location": a.location,
        "distinguished_name": a.distinguished_name,
        "labels": a.labels,
        "graphic": match &a.graphic {
            None => Value::Null,
            Some(AppearanceGraphic::Image(i)) => json!({"kind": "image", "width": i.0.size()[0], "height": i.0.size()[1]}),
            Some(AppearanceGraphic::Drawn(s)) => json!({"kind": "drawn", "strokes": s.len(), "points": s.iter().map(Vec::len).sum::<usize>()}),
        },
        "logo": a.logo.as_ref().map(|i| json!({"width": i.0.size()[0], "height": i.0.size()[1]})),
    })
}

fn server_json(s: &TimestampServer, password_held: bool) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
    if s.username.is_some() {
        v["password_held"] = json!(password_held);
    }
    v
}

impl Automation {
    /// A picture from a file the caller names (PNG or JPEG, at most 4 MiB), with near-white
    /// paper made transparent when `remove_white` is set.
    fn picture(&self, path: &str, remove_white: bool) -> Result<AppearanceImage> {
        let path = self.resolve(path, false)?;
        let file = std::fs::File::open(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
        let mut img = SignatureImage::read(file).map_err(|e| bad(e.to_string()))?;
        if remove_white {
            img = img.remove_white_background(235, 25).map_err(|e| bad(e.to_string()))?;
        }
        Ok(AppearanceImage(img))
    }

    /// An appearance from the tools' `appearance` object (or a saved appearance's name).
    fn saved_appearance_arg(&self, name: &str, v: &Value) -> Result<SavedAppearance> {
        let o = v.as_object().ok_or_else(|| bad("appearance must be an object, or the name of a saved appearance"))?;
        let mut base = match o.get("saved").and_then(Value::as_str) {
            Some(n) => self
                .session
                .signature_appearance(n)
                .cloned()
                .ok_or_else(|| bad(format!("no saved appearance is named {n:?} (sign_appearance_list)")))?,
            None => SavedAppearance {
                name: name.to_string(),
                show_name: true,
                date: true,
                reason: false,
                location: false,
                distinguished_name: false,
                labels: true,
                graphic: None,
                logo: None,
            },
        };
        base.name = name.to_string();
        let flag = |k: &str| -> Result<Option<bool>> {
            match o.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Bool(b)) => Ok(Some(*b)),
                Some(_) => Err(bad(format!("appearance.{k} must be true or false"))),
            }
        };
        for (k, slot) in [
            ("show_name", &mut base.show_name),
            ("date", &mut base.date),
            ("reason", &mut base.reason),
            ("location", &mut base.location),
            ("distinguished_name", &mut base.distinguished_name),
            ("labels", &mut base.labels),
        ] {
            if let Some(b) = flag(k)? {
                *slot = b;
            }
        }
        let remove_white = flag("remove_white")?.unwrap_or(false);
        match (o.get("image"), o.get("strokes")) {
            (Some(Value::String(_)), Some(s)) if !s.is_null() => return Err(bad("give appearance.image or appearance.strokes, not both")),
            (Some(Value::String(p)), _) => base.graphic = Some(AppearanceGraphic::Image(self.picture(p, remove_white)?)),
            (Some(Value::Null) | None, Some(s)) if !s.is_null() => {
                let raw: Vec<Vec<[f32; 2]>> = serde_json::from_value(s.clone())
                    .map_err(|_| bad("appearance.strokes must be a list of strokes, each a list of [x, y] points"))?;
                if raw.len() > sign::appearance::MAX_INK_STROKES {
                    return Err(bad(format!("appearance.strokes: at most {} strokes", sign::appearance::MAX_INK_STROKES)));
                }
                let ink = sign::appearance::InkGraphic::fit(&raw).map_err(|e| bad(e.to_string()))?;
                base.graphic = Some(AppearanceGraphic::Drawn(ink.strokes().to_vec()));
            }
            (Some(Value::Null), _) => base.graphic = None,
            (None, _) => {}
            (Some(_), _) => return Err(bad("appearance.image must be a file path")),
        }
        match o.get("logo") {
            None => {}
            Some(Value::Null) => base.logo = None,
            Some(Value::String(p)) => base.logo = Some(self.picture(p, false)?),
            Some(_) => return Err(bad("appearance.logo must be a file path")),
        }
        Ok(base)
    }

    /// `sign_document`'s appearance: a saved appearance's name, an object, or the default.
    pub(crate) fn signing_appearance(&self, a: &Args) -> Result<Appearance> {
        match a.get("appearance") {
            None | Some(Value::Null) => Ok(Appearance::default()),
            Some(Value::String(n)) => self
                .session
                .signature_appearance(n)
                .ok_or_else(|| bad(format!("no saved appearance is named {n:?} (sign_appearance_list)")))?
                .appearance()
                .map_err(|e| bad(e.to_string())),
            Some(v) => self.saved_appearance_arg("appearance", v)?.appearance().map_err(|e| bad(e.to_string())),
        }
    }

    pub(crate) fn sign_appearance_list(&self) -> Result<Value> {
        Ok(json!({ "appearances": self.session.signature_appearances().iter().map(appearance_json).collect::<Vec<_>>() }))
    }

    pub(crate) fn sign_appearance_save(&mut self, a: &Args) -> Result<Value> {
        let name = a.str("name")?;
        let v = a.get("appearance").cloned().unwrap_or_else(|| json!({}));
        let saved = self.saved_appearance_arg(name, &v)?;
        self.session.save_signature_appearance(saved).map_err(|e| bad(e.to_string()))?;
        self.sign_appearance_list()
    }

    pub(crate) fn sign_appearance_delete(&mut self, a: &Args) -> Result<Value> {
        let name = a.str("name")?;
        if !self.session.delete_signature_appearance(name) {
            return Err(bad(format!("no saved appearance is named {name:?}")));
        }
        self.sign_appearance_list()
    }

    pub(crate) fn sign_trust_list(&self) -> Result<Value> {
        let store = self.session.trust_store();
        let list: Vec<Value> = store
            .certs
            .iter()
            .map(|c| {
                let t = store.trust_of(c);
                let mut v = cert_json(c);
                v["trust"] = json!({"root": t.root, "signatures": t.signatures, "certified_documents": t.certified_documents});
                v
            })
            .collect();
        Ok(json!({ "trusted": list }))
    }

    pub(crate) fn sign_trust_set(&mut self, a: &Args) -> Result<Value> {
        let fp = a.str("sha256")?;
        if a.opt_bool("remove")? == Some(true) {
            if !self.session.remove_trusted_certificate(fp) {
                return Err(bad("that certificate is not in the trusted certificates (sign_trust_list)"));
            }
            return self.sign_trust_list();
        }
        let (_, now) =
            self.session.certificate_trust(fp).ok_or_else(|| bad("that certificate is not in the trusted certificates (sign_trust_list)"))?;
        let next = CertTrust {
            root: a.opt_bool("root")?.unwrap_or(now.root),
            signatures: a.opt_bool("signatures")?.unwrap_or(now.signatures),
            certified_documents: a.opt_bool("certified_documents")?.unwrap_or(now.certified_documents),
        };
        self.session.set_certificate_trust(fp, next).map_err(bad)?;
        self.sign_trust_list()
    }

    pub(crate) fn sign_timestamp_servers_get(&self) -> Result<Value> {
        let s = &self.session;
        Ok(json!({ "servers": s.timestamp_servers().iter().map(|x| server_json(x, s.timestamp_password(&x.name).is_some())).collect::<Vec<_>>() }))
    }

    pub(crate) fn sign_timestamp_servers_set(&mut self, a: &Args) -> Result<Value> {
        if let Some(v) = a.get("servers") {
            let servers: Vec<TimestampServer> = serde_json::from_value(v.clone()).map_err(|e| bad(format!("servers: {e}")))?;
            self.session.set_timestamp_servers(servers).map_err(bad)?;
        }
        if let Some(name) = a.opt_str("password_for")? {
            let password = match a.get("password") {
                None | Some(Value::Null) => None,
                Some(Value::String(p)) => Some(p.clone()),
                Some(_) => return Err(bad("password must be text, or null to forget it")),
            };
            self.session.set_timestamp_password(name, password).map_err(bad)?;
        }
        self.sign_timestamp_servers_get()
    }

    pub(crate) fn sign_add_verification(&mut self, a: &Args) -> Result<Value> {
        self.resolve(a.str("out")?, true)?;
        let mut local = sign::dss::Evidence::default();
        let mut count = 0usize;
        let mut size = 0usize;
        for (key, target) in [("certificates", &mut local.certs), ("crls", &mut local.crls), ("ocsps", &mut local.ocsps)] {
            let paths = if a.get(key).is_some() { a.strs(key)? } else { Vec::new() };
            for path in paths {
                count += 1;
                if count > 32 {
                    return Err(bad("At most 32 local evidence files may be supplied."));
                }
                let path = self.resolve(path, false)?;
                let mut bytes = Vec::new();
                std::fs::File::open(&path).map_err(failed)?.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(failed)?;
                size += bytes.len();
                if bytes.len() > 2 * 1024 * 1024 || size > 8 * 1024 * 1024 {
                    return Err(bad("Local evidence is limited to 2 MiB per file and 8 MiB in total."));
                }
                target.push(bytes);
            }
        }
        let network = a.opt_bool("allow_network")?.unwrap_or(false);
        let mut policy = self.session.revocation_fetch_policy(network, a.opt_bool("allow_prompted")?.unwrap_or(false));
        if let Some(t) = a.opt_int("timeout_seconds")? {
            policy.timeout_seconds = t.clamp(1, 30) as u32;
        }
        let authority = self.timestamp_authority(a)?;
        let job = self.session.signature_job(self.doc(a)?.id).map_err(failed)?;
        let transport = HttpTransport { cancellation: SignatureCancellation::default() };
        let (result, report) = job
            .add_verification_information(&local, &policy, &transport, authority.as_ref().map(|a| a as &dyn sign::TimestampAuthority))
            .map_err(failed)?;
        let mut saved = self.save_signature_result(a, result)?;
        let e = &report.evidence;
        saved["verification"] = json!({
            "network_allowed": policy.network,
            "requests": report.fetched.attempts.iter().map(|t| json!({"certificate": t.certificate, "url": t.url, "kind": t.kind, "outcome": t.outcome, "detail": t.detail})).collect::<Vec<_>>(),
            "certificates_needing_evidence": report.fetched.needed,
            "certificates_covered_online": report.fetched.covered,
            "embedded": {"certificates": e.certificates, "crls": e.crls, "ocsps": e.ocsps},
            "complete_revocation_coverage": e.has_complete_coverage(),
            "coverage": e.coverage.iter().map(|c| json!({"field": c.field, "role": c.role.as_str(), "sha256": c.certificate_sha256,
                "checked_at": c.checked_at.map(|t| t.to_string()), "status": format!("{:?}", c.status)})).collect::<Vec<_>>(),
            "notes": e.notes,
        });
        Ok(saved)
    }
}
