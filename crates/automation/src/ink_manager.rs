//! Ink Manager: list a document's inks with where they are used, set aliases, spot-to-process
//! conversion and Lab previews for the session (honoured by `print_separations_*` and
//! `print_total_area_coverage`), and apply the settings to the document as one undoable edit
//! (`print_ink_manager_*`).

use pdfcraft_engine::preflight::{InkManager, InkRoute};
use serde_json::{Map, Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

const PROCESS: [&str; 4] = ["Cyan", "Magenta", "Yellow", "Black"];

/// The settings as tool output.
pub(crate) fn settings_json(m: &InkManager) -> Value {
    json!({
        "aliases": m.aliases,
        "to_process": m.to_process,
        "all_to_process": m.all_to_process,
        "use_lab": m.use_lab,
        "default": m.is_default(),
    })
}

fn route_json(m: &InkManager, name: &str) -> Value {
    match m.route(name) {
        InkRoute::Own => json!({ "route": "own", "prints_on": name }),
        InkRoute::Ink(to) => json!({ "route": "alias", "prints_on": to, "aliased_to": m.aliases.get(name) }),
        InkRoute::Process => json!({ "route": "process", "prints_on": "process", "aliased_to": m.aliases.get(name) }),
    }
}

impl Automation {
    /// The document's inks (ignoring the Ink Manager): names, with spot inks first-met order.
    fn document_ink_names(&self, a: &Args) -> Result<Option<(Vec<String>, Vec<String>)>> {
        let doc = self.doc(a)?;
        if doc.info.pages.len() > pdfcraft_engine::pdfa::separations::MAX_LIST_PAGES {
            return Ok(None);
        }
        let list = doc.document_inks(&[]).map_err(failed)?;
        let (process, spots): (Vec<_>, Vec<_>) = list.inks.iter().partition(|u| u.info.process);
        Ok(Some((process.iter().map(|u| u.info.name.clone()).collect(), spots.iter().map(|u| u.info.name.clone()).collect())))
    }

    /// Ink Manager: every ink of the document with usage and where it prints.
    pub(crate) fn ink_manager_list(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let pages = if a.get("pages").is_some() { self.pages(a, "pages")? } else { Vec::new() };
        let max = pdfcraft_engine::pdfa::separations::MAX_LIST_PAGES;
        if pages.len() > max {
            return Err(bad(format!("pages may list at most {max} pages per call")));
        }
        let list = doc.document_inks(&pages).map_err(failed)?;
        let m = doc.ink_manager();
        let one_based = |p: &[usize]| p.iter().map(|p| p + 1).collect::<Vec<_>>();
        let inks: Vec<Value> = list
            .inks
            .iter()
            .map(|u| {
                let mut v = crate::separations::ink_json(&u.info);
                v["pages"] = json!(one_based(&u.pages));
                v["declared_pages"] = json!(one_based(&u.declared_pages));
                v["used"] = json!(!u.pages.is_empty());
                v["objects"] = json!(pdfcraft_engine::pdfa::separations::kind_names(u.kinds));
                if let (Value::Object(o), Value::Object(r)) = (&mut v, route_json(m, &u.info.name)) {
                    o.extend(r);
                }
                v
            })
            .collect();
        let count = doc.info.pages.len();
        Ok(json!({
            "doc": doc.id.0,
            "inks": inks,
            "process_inks": list.inks.iter().filter(|u| u.info.process).count(),
            "spot_inks": list.inks.iter().filter(|u| !u.info.process).count(),
            "settings": settings_json(m),
            "pages_checked": one_based(&list.pages),
            "pages_truncated": pages.is_empty() && count > max,
            "converted_pages": one_based(&list.converted_pages),
            "conversion": list.conversion,
            "unmeasured": list.unmeasured,
            "truncated_pages": one_based(&list.truncated_pages),
            "method": pdfcraft_engine::pdfa::coverage::METHOD,
        }))
    }

    /// Change the session's Ink Manager settings for a document.
    pub(crate) fn ink_manager_set(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let id = doc.id;
        let mut m = if a.opt_bool("reset")?.unwrap_or(false) { InkManager::default() } else { doc.ink_manager().clone() };
        let known = self.document_ink_names(a)?;
        let check_spot = |name: &str, what: &str| -> Result<()> {
            if PROCESS.contains(&name) {
                return Err(bad(format!("{name} is a process ink; {what} takes spot inks")));
            }
            if let Some((_, spots)) = &known
                && !spots.iter().any(|s| s == name)
            {
                let list = if spots.is_empty() { "none".to_string() } else { spots.join(", ") };
                return Err(bad(format!("the document has no spot ink {name:?} ({what}); its spot inks are {list}")));
            }
            Ok(())
        };
        let mut touched = false;
        if let Some(v) = a.get("aliases") {
            let Value::Object(map) = v else {
                return Err(bad("aliases must be an object: {\"spot ink\": \"ink it prints on\" (or null to remove)}"));
            };
            for (from, to) in map {
                match to {
                    Value::Null => {
                        m.aliases.remove(from);
                    }
                    Value::String(t) if t.is_empty() => {
                        m.aliases.remove(from);
                    }
                    Value::String(t) => {
                        check_spot(from, "aliases")?;
                        if let Some((process, spots)) = &known
                            && !process.iter().chain(spots).any(|s| s == t)
                        {
                            return Err(bad(format!(
                                "{from} can only be aliased to an ink of the document: {}",
                                process.iter().chain(spots).filter(|s| *s != from).cloned().collect::<Vec<_>>().join(", ")
                            )));
                        }
                        m.aliases.insert(from.clone(), t.clone());
                    }
                    _ => return Err(bad(format!("aliases.{from} must be an ink name or null"))),
                }
            }
            touched = true;
        }
        if a.get("to_process").is_some() {
            for name in a.strs("to_process")? {
                check_spot(name, "to_process")?;
                m.to_process.insert(name.to_string());
            }
            touched = true;
        }
        if a.get("own_plate").is_some() {
            for name in a.strs("own_plate")? {
                m.to_process.remove(name);
                m.aliases.remove(name);
            }
            touched = true;
        }
        if let Some(all) = a.opt_bool("all_to_process")? {
            m.all_to_process = all;
            touched = true;
        }
        if let Some(lab) = a.opt_bool("use_lab")? {
            m.use_lab = lab;
            touched = true;
        }
        if !touched && !a.opt_bool("reset")?.unwrap_or(false) {
            return Err(bad("nothing to set: pass aliases, to_process, own_plate, all_to_process, use_lab or reset"));
        }
        m.validate().map_err(bad)?;
        self.session.set_ink_manager(id, m.clone()).map_err(failed)?;
        let mut routes = Map::new();
        if let Some((_, spots)) = &known {
            for s in spots {
                routes.insert(s.clone(), route_json(&m, s));
            }
        }
        Ok(json!({
            "doc": id.0,
            "settings": settings_json(&m),
            "spot_inks": routes,
            "checked_against_document": known.is_some(),
        }))
    }

    /// Make the Ink Manager settings permanent (one undo step).
    pub(crate) fn ink_manager_apply(&mut self, a: &Args) -> Result<Value> {
        let id = self.doc(a)?.id;
        let dry_run = a.opt_bool("dry_run")?.unwrap_or(false);
        let settings = self.doc(a)?.ink_manager().clone();
        if !settings.moves_inks() {
            return Err(bad("the Ink Manager moves no ink: set aliases or spot inks to process with print_ink_manager_set first"));
        }
        let report = if dry_run { self.session.apply_ink_manager_preview(id) } else { self.session.apply_ink_manager(id) }.map_err(failed)?;
        let mut out = crate::summary(self.doc(a)?);
        out["dry_run"] = json!(dry_run);
        out["changed"] = json!(report.changed());
        out["settings"] = settings_json(&settings);
        out["aliases"] = json!(report.aliases);
        out["renamed"] = json!({
            "colorants": report.renamed.renamed,
            "objects": report.renamed.objects,
            "skipped": report.renamed.skipped,
        });
        out["to_process"] = match &report.to_process {
            Some(names) => json!(names),
            None if settings.all_to_process => json!("all"),
            None => json!([]),
        };
        out["destination"] = json!(report.destination);
        out["converted"] = match &report.conversion {
            Some(c) => json!({
                "pages_changed": c.pages.iter().map(|p| p + 1).collect::<Vec<_>>(),
                "colors": c.colors,
                "images": c.images,
                "inline_images": c.inline_images,
                "shadings": c.shadings,
                "forms": c.forms,
                "patterns": c.patterns,
                "annotations": c.annotations,
                "approximate": c.approximate,
                "skipped": c.skipped,
            }),
            None => Value::Null,
        };
        Ok(out)
    }
}
