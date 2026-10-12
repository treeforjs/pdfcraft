//! Structural preflight tools: read-only analysis and explicit revision-bound fixes.

use crate::{Args, Automation, Result, ToolError, failed, write_atomic};
use pdfcraft_engine::preflight::{Profile, ResultSet, Rule, Settings};
use serde_json::{Value, json};

fn settings(a: &Args) -> Result<Settings> {
    let id = a.opt_str("profile")?.unwrap_or("general");
    let profile = Profile::from_id(id).ok_or_else(|| ToolError::InvalidArgs(format!("Unknown preflight profile {id:?}; use preflight_profiles")))?;
    let checks =
        a.0.get("checks")
            .map(|v| {
                v.as_array()
                    .ok_or_else(|| ToolError::InvalidArgs("checks must be an array of check ids".into()))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .and_then(Rule::from_id)
                            .ok_or_else(|| ToolError::InvalidArgs(format!("Unknown preflight check {v}; use preflight_profiles")))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
    let result = Settings { profile, checks };
    result.rules().map_err(ToolError::InvalidArgs)?;
    Ok(result)
}

pub(crate) fn result_json(result: &ResultSet) -> Value {
    let report = &result.report;
    let findings: Vec<_> = report.findings.iter().map(|f| json!({
        "id": f.id, "check": f.rule.id(), "severity": f.severity.id(), "message": f.message,
        "clause": f.clause,
        "location": f.location.as_ref().map(|l| json!({"object":l.object.num,"generation":l.object.generation,"path":l.display(),"pages":l.pages.iter().map(|p|p+1).collect::<Vec<_>>()})),
        "fix": f.fixup.map(|fix|json!({"id":fix.id(),"description":fix.label()})),
    })).collect();
    json!({ "doc":result.document.0,"revision":result.revision,"profile":report.settings.profile.id(),"checks":report.checks.iter().map(|r|r.id()).collect::<Vec<_>>(),"findings":findings,"objects_checked":report.objects_checked,"truncated":report.truncated,"validation_complete":false,"conformance":"not-established","limitations":report.limitations })
}

impl Automation {
    pub(crate) fn preflight_profiles(&self) -> Value {
        json!({"profiles":Profile::ALL.iter().map(|p|json!({"id":p.id(),"label":p.label(),"checks":p.rules().iter().map(|r|r.id()).collect::<Vec<_>>()})).collect::<Vec<_>>(),"checks":Rule::ALL.iter().map(|r|json!({"id":r.id(),"label":r.label()})).collect::<Vec<_>>(),"limitation":pdfcraft_engine::pdfa::analysis::LIMITATION})
    }

    pub(crate) fn preflight_analyze(&self, a: &Args) -> Result<Value> {
        Ok(result_json(&self.doc(a)?.preflight(&settings(a)?).map_err(failed)?))
    }

    pub(crate) fn preflight_fix(&mut self, a: &Args) -> Result<Value> {
        let result = self.doc(a)?.preflight(&settings(a)?).map_err(failed)?;
        let revision = u64::try_from(a.int("revision")?).map_err(|_| ToolError::InvalidArgs("revision must not be negative".into()))?;
        if revision != result.revision {
            return Err(failed("Preflight results are stale; analyze again before selecting fixes"));
        }
        let ids = a
            .opt_ints("findings")?
            .ok_or_else(|| ToolError::InvalidArgs("findings is required".into()))?
            .into_iter()
            .map(|n| usize::try_from(n).map_err(|_| ToolError::InvalidArgs("finding ids must be positive integers".into())))
            .collect::<Result<Vec<_>>>()?;
        Ok(result_json(&self.session.preflight_fix(&result, &ids).map_err(failed)?))
    }

    pub(crate) fn preflight_report(&self, a: &Args) -> Result<Value> {
        let result = self.doc(a)?.preflight(&settings(a)?).map_err(failed)?;
        let format = a.opt_str("format")?.unwrap_or("text");
        let bytes = match format {
            "text" => result.report.text().into_bytes(),
            "json" => serde_json::to_vec_pretty(&result_json(&result)).map_err(failed)?,
            _ => return Err(ToolError::InvalidArgs("format must be text or json".into())),
        };
        let path = self.resolve(a.str("path")?, true)?;
        write_atomic(&path, &bytes)?;
        Ok(json!({"path":path.to_string_lossy(),"bytes":bytes.len(),"format":format,"revision":result.revision}))
    }
}
