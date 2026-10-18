//! Preferences ▸ Color Management: read and change the colour settings (`color_settings_get`,
//! `color_settings_set`), list the profiles they can name (`color_profiles_list`) and load ICC
//! profiles from files under `--root` (`color_profile_load`). Rendering, Output Preview and the
//! defaults of Convert Colors and Convert to PDF/X follow them.

use pdfcraft_engine::RenderingIntent;
use pdfcraft_engine::color_settings::{ColorManagement, ColorSettings, ProfileInfo, UntaggedPolicy, family_id};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

fn profile_json(p: &ProfileInfo, cm: &ColorManagement) -> Value {
    let s = cm.settings();
    let working = [&s.rgb, &s.cmyk, &s.gray].contains(&&p.id);
    json!({
        "id": p.id,
        "family": family_id(p.family),
        "description": p.description,
        "built_in": p.built_in,
        "source": p.source,
        "size": p.size,
        "displays": p.displays,
        "converts_into": p.converts_into,
        "working": working,
    })
}

fn settings_json(cm: &ColorManagement) -> Value {
    let s = cm.settings();
    json!({
        "rgb": s.rgb,
        "cmyk": s.cmyk,
        "gray": s.gray,
        "intent": s.intent.name(),
        "black_point_compensation": s.black_point_compensation,
        "untagged": s.untagged.id(),
        "untagged_label": s.untagged.label(),
        "default": *s == ColorSettings::default(),
    })
}

impl Automation {
    fn color_out(&self, a: &Args) -> Result<Value> {
        let cm = self.session.color_management();
        let mut out = json!({
            "settings": settings_json(cm),
            "intents": RenderingIntent::NAMES,
            "untagged_policies": UntaggedPolicy::ALL.iter().map(|p| json!({ "id": p.id(), "label": p.label() })).collect::<Vec<_>>(),
        });
        if a.opt_int("doc")?.is_some() {
            let doc = self.doc(a)?;
            let spaces: Vec<Value> = doc
                .color_spaces()
                .iter()
                .map(|u| {
                    json!({
                        "family": family_id(u.family),
                        "source": u.source,
                        "description": u.description,
                        "working": u.working,
                        "note": u.note,
                    })
                })
                .collect();
            out["doc"] = json!(doc.id.0);
            out["spaces"] = json!(spaces);
            out["render_problem"] = json!(doc.color_problem());
            if let Ok(c) = doc.press_conversion() {
                out["separation"] = json!({ "conversion": c.describe(), "uses_profile": c.uses_profile() });
            }
        }
        Ok(out)
    }

    /// The colour settings, and with `doc` how they resolve for it.
    pub(crate) fn color_settings_get(&self, a: &Args) -> Result<Value> {
        self.color_out(a)
    }

    /// Change the colour settings for this session.
    pub(crate) fn color_settings_set(&mut self, a: &Args) -> Result<Value> {
        let mut s = if a.opt_bool("reset")?.unwrap_or(false) { ColorSettings::default() } else { self.session.color_management().settings().clone() };
        for (key, slot) in [("rgb", &mut s.rgb), ("cmyk", &mut s.cmyk), ("gray", &mut s.gray)] {
            if let Some(id) = a.opt_str(key)? {
                *slot = id.to_string();
            }
        }
        if let Some(i) = a.opt_str("intent")? {
            s.intent = RenderingIntent::from_name(i).ok_or_else(|| bad(format!("unknown intent {i:?} ({})", RenderingIntent::NAMES.join(", "))))?;
        }
        if let Some(b) = a.opt_bool("black_point_compensation")? {
            s.black_point_compensation = b;
        }
        if let Some(p) = a.opt_str("untagged")? {
            let ids: Vec<&str> = UntaggedPolicy::ALL.iter().map(|p| p.id()).collect();
            s.untagged = UntaggedPolicy::from_id(p).ok_or_else(|| bad(format!("untagged must be one of {}, not {p:?}", ids.join(", "))))?;
        }
        self.session.set_color_settings(s).map_err(|e| bad(e.to_string()))?;
        self.color_out(a)
    }

    /// The profiles the colour settings can name.
    pub(crate) fn color_profiles_list(&self, a: &Args) -> Result<Value> {
        let family = a.opt_str("family")?;
        if let Some(f) = family
            && !["rgb", "cmyk", "gray"].contains(&f)
        {
            return Err(bad(format!("family must be rgb, cmyk or gray, not {f:?}")));
        }
        let cm = self.session.color_management();
        let profiles: Vec<Value> =
            cm.profiles().iter().filter(|p| family.is_none_or(|f| family_id(p.family) == f)).map(|p| profile_json(p, cm)).collect();
        Ok(json!({ "profiles": profiles, "count": profiles.len(), "settings": settings_json(cm) }))
    }

    /// Load an ICC profile from a file (under `--root`) as `user:<name>`; `working` also makes
    /// it the working space of its family.
    pub(crate) fn color_profile_load(&mut self, a: &Args) -> Result<Value> {
        let rel = a.str("path")?;
        let path = self.resolve(rel, false)?;
        let name = pdfcraft_engine::color_settings::profile_name(a.opt_str("name")?, rel).map_err(bad)?;
        let max = pdfcraft_engine::color_profile_limit();
        let size = std::fs::metadata(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?.len();
        if size > max as u64 {
            return Err(bad(format!("profile is larger than {} MB", max >> 20)));
        }
        let bytes = std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
        let info = self.session.load_color_profile(&name, rel, bytes).map_err(|e| bad(e.to_string()))?;
        if a.opt_bool("working")?.unwrap_or(false) {
            let mut s = self.session.color_management().settings().clone();
            match info.family {
                pdfcraft_engine::color_settings::ProfileFamily::Gray => s.gray = info.id.clone(),
                pdfcraft_engine::color_settings::ProfileFamily::Rgb => s.rgb = info.id.clone(),
                pdfcraft_engine::color_settings::ProfileFamily::Cmyk => s.cmyk = info.id.clone(),
            }
            self.session.set_color_settings(s).map_err(|e| bad(e.to_string()))?;
        }
        let cm = self.session.color_management();
        Ok(json!({ "profile": profile_json(&info, cm), "settings": settings_json(cm) }))
    }
}
