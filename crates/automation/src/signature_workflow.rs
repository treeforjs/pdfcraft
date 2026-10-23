//! Explicit, bounded timestamp requests and local validation evidence. Merely opening or
//! validating a signature never contacts a server or trusts an imported certificate.

use std::io::Read;

use pdfcraft_engine::sign::{self, VerificationTime};
use pdfcraft_engine::signature_workflow::{HttpTimestampAuthority, SignatureCancellation, SignatureResult, TimestampServer, verification_time_utc};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed, write_atomic};

impl Automation {
    pub(crate) fn timestamp_authority(&self, a: &Args) -> Result<Option<HttpTimestampAuthority>> {
        let Some(server) = a.get("timestamp_server") else { return Ok(None) };
        if a.opt_bool("allow_network")? != Some(true) {
            return Err(ToolError::InvalidArgs(
                "A timestamp request requires allow_network: true and an explicitly configured timestamp_server.".into(),
            ));
        }
        // A configured server by name (sign_timestamp_servers_set), or a server given inline.
        let server: TimestampServer = match server {
            Value::String(name) => self
                .session
                .timestamp_server(name)
                .cloned()
                .ok_or_else(|| ToolError::InvalidArgs(format!("no timestamp server is named {name:?} (sign_timestamp_servers_get)")))?,
            v => serde_json::from_value(v.clone()).map_err(|e| ToolError::InvalidArgs(format!("timestamp_server: {e}")))?,
        };
        let password = self.session.timestamp_password(&server.name).map(str::to_string);
        HttpTimestampAuthority::with_trust(server, self.session.trust_store().clone(), password, SignatureCancellation::default())
            .map(Some)
            .map_err(ToolError::InvalidArgs)
    }

    fn verification_time(&self, a: &Args) -> Result<VerificationTime> {
        match a.opt_str("verification_time")?.unwrap_or("best") {
            "best" => Ok(VerificationTime::BestAvailable),
            "claimed" => Ok(VerificationTime::ClaimedSigningTime),
            "now" => Ok(VerificationTime::At(sign::Time::from_unix(self.session.now_secs()))),
            value => verification_time_utc(value).map(VerificationTime::At).map_err(ToolError::InvalidArgs),
        }
    }

    pub(crate) fn save_signature_result(&mut self, a: &Args, result: SignatureResult) -> Result<Value> {
        let out = self.resolve(a.str("out")?, true)?;
        self.session.check_signature_result(&result).map_err(failed)?;
        write_atomic(&out, &result.bytes)?;
        let size = result.bytes.len();
        let path = out.to_string_lossy().into_owned();
        self.session.mark_signed(result.doc, result.bytes, Some(path.clone())).map_err(failed)?;
        Ok(json!({"path":path,"bytes":size,"validation":self.sign_list(a)?}))
    }

    pub(crate) fn sign_timestamp(&mut self, a: &Args) -> Result<Value> {
        // Check the destination before sending a request.
        self.resolve(a.str("out")?, true)?;
        let authority = self.timestamp_authority(a)?.ok_or_else(|| ToolError::InvalidArgs("timestamp_server is required".into()))?;
        let job = self.session.signature_job(self.doc(a)?.id).map_err(failed)?;
        let result = job.timestamp(&authority).map_err(failed)?;
        self.save_signature_result(a, result)
    }

    pub(crate) fn sign_verify(&self, a: &Args) -> Result<Value> {
        let list = self.session.verify_signatures_at(self.doc(a)?.id, self.verification_time(a)?).map_err(failed)?;
        let results = list.iter().map(|s| self.sig_json(a, s)).collect::<Result<Vec<_>>>()?;
        Ok(json!({"signatures":results,"network_used":false,"trust_changed":false}))
    }

    pub(crate) fn sign_embed_evidence(&mut self, a: &Args) -> Result<Value> {
        self.resolve(a.str("out")?, true)?;
        let when = self.verification_time(a)?;
        let mut evidence = sign::dss::Evidence::default();
        let mut count = 0usize;
        let mut size = 0usize;
        for (key, target) in [("certificates", &mut evidence.certs), ("crls", &mut evidence.crls), ("ocsps", &mut evidence.ocsps)] {
            let paths = if a.get(key).is_some() { a.strs(key)? } else { Vec::new() };
            for path in paths {
                count += 1;
                if count > 64 {
                    return Err(ToolError::InvalidArgs("At most 64 evidence files may be supplied.".into()));
                }
                let path = self.resolve(path, false)?;
                let mut bytes = Vec::new();
                std::fs::File::open(&path).map_err(failed)?.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(failed)?;
                size += bytes.len();
                if bytes.len() > 2 * 1024 * 1024 || size > 32 * 1024 * 1024 {
                    return Err(ToolError::InvalidArgs("Evidence is limited to 2 MiB per file and 32 MiB in total.".into()));
                }
                target.push(bytes);
            }
        }
        let authority = self.timestamp_authority(a)?;
        let job = self.session.signature_job(self.doc(a)?.id).map_err(failed)?;
        let (result, report) = job.embed_evidence(&evidence, when, authority.as_ref().map(|a| a as &dyn sign::TimestampAuthority)).map_err(failed)?;
        let mut saved = self.save_signature_result(a, result)?;
        saved["evidence"] = json!({"certificates":report.certificates,"crls":report.crls,"ocsps":report.ocsps,
            "complete_revocation_coverage":report.has_complete_coverage(),"notes":report.notes,
            "coverage":report.coverage.iter().map(|c| json!({"field":c.field,"role":c.role.as_str(),"sha256":c.certificate_sha256,
                "checked_at":c.checked_at.map(|t|t.to_string()),"status":format!("{:?}",c.status)})).collect::<Vec<_>>()});
        Ok(saved)
    }
}
