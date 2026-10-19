//! Named destinations and go-to targets: `dest_list`, `dest_set`, `dest_rename`, `dest_delete`,
//! `bookmark_set_action`, and the `view` / `dest` options of the link tools.
//!
//! A view is `{ fit, left, top, zoom, rect }`: `fit` is XYZ, Fit, FitH, FitV, FitR, FitB, FitBH or
//! FitBV (default XYZ when a position or zoom is given, else Fit); `left`, `top` and `rect`
//! (`[left, bottom, right, top]`) are PDF user-space coordinates exactly as stored in the
//! destination (origin at the bottom-left of the unrotated page, as `pdfinfo -dests` prints
//! them); `zoom` is a percentage (150 = 150%).

use pdfcraft_engine::{BookmarkAction, DestFit, DestSource, DestTarget, DestView, Edit, NamedDest};
use serde_json::{Map, Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// A view as JSON (1-based page; zoom as a percentage).
pub(crate) fn view_json(v: &DestView) -> Value {
    let mut o = Map::new();
    o.insert("page".into(), json!(v.page + 1));
    o.insert("fit".into(), json!(v.fit.kind()));
    let mut put = |k: &str, x: Option<f64>| {
        if let Some(x) = x {
            o.insert(k.into(), json!((x * 1000.0).round() / 1000.0));
        }
    };
    match v.fit {
        DestFit::Xyz { left, top, zoom } => {
            put("left", left);
            put("top", top);
            put("zoom", zoom.map(|z| z * 100.0));
        }
        DestFit::FitH { top } | DestFit::FitBH { top } => put("top", top),
        DestFit::FitV { left } | DestFit::FitBV { left } => put("left", left),
        DestFit::FitR { rect } => {
            o.insert("rect".into(), json!(rect.map(|x| (x * 1000.0).round() / 1000.0)));
        }
        DestFit::Fit | DestFit::FitB => {}
    }
    Value::Object(o)
}

/// Parse a `view` object for 0-based `page`.
fn parse_view(v: &Value, page: usize) -> Result<DestView> {
    let o = v.as_object().ok_or_else(|| bad("view must be an object { fit, left, top, zoom, rect }"))?;
    if let Some(k) = o.keys().find(|k| !["fit", "left", "top", "zoom", "rect"].contains(&k.as_str())) {
        return Err(bad(format!("unknown view option {k:?}: use fit, left, top, zoom, rect")));
    }
    let num = |k: &str| -> Result<Option<f64>> {
        o.get(k)
            .filter(|x| !x.is_null())
            .map(|x| x.as_f64().filter(|f| f.is_finite()).ok_or_else(|| bad(format!("view.{k} must be a number"))))
            .transpose()
    };
    let (left, top, zoom) = (num("left")?, num("top")?, num("zoom")?);
    let rect = match o.get("rect").filter(|x| !x.is_null()) {
        None => None,
        Some(r) => {
            let r: Vec<f64> = r.as_array().map(|a| a.iter().filter_map(Value::as_f64).filter(|f| f.is_finite()).collect()).unwrap_or_default();
            Some(<[f64; 4]>::try_from(r).map_err(|_| bad("view.rect must be 4 numbers [left, bottom, right, top]"))?)
        }
    };
    let fit = match o.get("fit").filter(|x| !x.is_null()) {
        Some(f) => f.as_str().ok_or_else(|| bad("view.fit must be a string"))?.to_string(),
        None if rect.is_some() => "FitR".into(),
        None if left.is_some() || top.is_some() || zoom.is_some() => "XYZ".into(),
        None => "Fit".into(),
    };
    let zoom = zoom.map(|z| z / 100.0);
    let fit = DestFit::from_parts(&fit, left, top, zoom, rect).map_err(|e| bad(e.to_string()))?;
    Ok(DestView { page, fit })
}

fn source_name(s: DestSource) -> &'static str {
    match s {
        DestSource::NameTree => "names",
        DestSource::Legacy => "dests",
    }
}

fn dest_json(d: &NamedDest, references: usize) -> Value {
    let mut v = json!({ "name": d.name, "stored_in": source_name(d.source), "references": references });
    match &d.view {
        Some(view) => {
            v["to_page"] = json!(view.page + 1);
            v["view"] = view_json(view);
        }
        None => v["unresolved"] = json!(true),
    }
    v
}

/// The JSON of a go-to target (what links and bookmarks report).
pub(crate) fn target_json(a: &Automation, id: pdfcraft_engine::DocId, t: &DestTarget, v: &mut Value) {
    match t {
        DestTarget::View(view) => {
            v["to_page"] = json!(view.page + 1);
            v["view"] = view_json(view);
        }
        DestTarget::Named(n) => {
            v["dest"] = json!(n);
            match a.session.destination(id, n).ok().flatten().and_then(|d| d.view) {
                Some(view) => {
                    v["to_page"] = json!(view.page + 1);
                    v["view"] = view_json(&view);
                }
                None => v["unresolved"] = json!(true),
            }
        }
    }
}

impl Automation {
    /// A go-to target from `to_page` (+ optional `view`) or `dest`; `None` when neither is given.
    pub(crate) fn goto_target(&self, a: &Args) -> Result<Option<DestTarget>> {
        let n = self.doc(a)?.info.pages.len() as i64;
        match (a.opt_int("to_page")?, a.opt_str("dest")?, a.get("view")) {
            (Some(_), Some(_), _) => Err(bad("pass to_page or dest, not both")),
            (None, Some(_), Some(_)) => Err(bad("view goes with to_page; a named destination has its own view")),
            (None, None, Some(_)) => Err(bad("view needs to_page")),
            (Some(p), None, view) => {
                if p < 1 || p > n {
                    return Err(bad(format!("to_page must be 1–{n}")));
                }
                let page = p as usize - 1;
                Ok(Some(DestTarget::View(match view {
                    Some(v) => parse_view(v, page)?,
                    None => DestView::page(page),
                })))
            }
            (None, Some(d), None) => {
                let id = self.doc(a)?.id;
                if self.session.destination(id, d).map_err(failed)?.is_none() {
                    return Err(failed(format!("there is no named destination {d:?} (see dest_list)")));
                }
                Ok(Some(DestTarget::Named(d.to_string())))
            }
            (None, None, None) => Ok(None),
        }
    }

    pub(crate) fn dest_list(&self, a: &Args) -> Result<Value> {
        let id = self.doc(a)?.id;
        let all = self.session.destinations(id).map_err(failed)?;
        let items: Vec<Value> = all.iter().map(|(d, n)| dest_json(d, *n)).collect();
        Ok(json!({ "count": items.len(), "destinations": items }))
    }

    pub(crate) fn dest_set(&mut self, a: &Args) -> Result<Value> {
        let name = a.str("name")?.to_string();
        let page = self.page(a)?;
        let mut spec = Map::new();
        for k in ["fit", "left", "top", "zoom", "rect"] {
            if let Some(v) = a.get(k) {
                spec.insert(k.into(), v.clone());
            }
        }
        let view = parse_view(&Value::Object(spec), page)?;
        let id = self.doc(a)?.id;
        let existed = self.session.destination(id, &name).map_err(failed)?.is_some();
        let mut out = self.apply(a, Edit::SetDestination { name: name.clone(), view })?;
        out["created"] = json!(!existed);
        if let Some(d) = self.session.destination(id, &name).map_err(failed)? {
            let n = self.session.destination_references(id, &name).map_err(failed)?;
            out["destination"] = dest_json(&d, n);
        }
        Ok(out)
    }

    pub(crate) fn dest_rename(&mut self, a: &Args) -> Result<Value> {
        let (from, to) = (a.str("name")?.to_string(), a.str("new_name")?.to_string());
        let id = self.doc(a)?.id;
        let before = self.session.destination_references(id, &from).map_err(failed)?;
        let mut out = self.apply(a, Edit::RenameDestination { from, to: to.clone() })?;
        out["references_updated"] = json!(before);
        Ok(out)
    }

    pub(crate) fn dest_delete(&mut self, a: &Args) -> Result<Value> {
        let name = a.str("name")?.to_string();
        let id = self.doc(a)?.id;
        let mut out = self.apply(a, Edit::DeleteDestination { name: name.clone() })?;
        out["still_referenced"] = json!(self.session.destination_references(id, &name).map_err(failed)?);
        Ok(out)
    }

    pub(crate) fn bookmark_set_action(&mut self, a: &Args) -> Result<Value> {
        let path = a.path("path")?;
        let action = match (self.goto_target(a)?, a.opt_str("url")?, a.opt_str("javascript")?) {
            (Some(t), None, None) => BookmarkAction::GoTo(t),
            (None, Some(u), None) => BookmarkAction::Uri(u.to_string()),
            (None, None, Some(js)) => BookmarkAction::JavaScript(js.to_string()),
            (None, None, None) => return Err(bad("pass to_page (optionally with view), dest, url or javascript")),
            _ => return Err(bad("pass one of to_page/dest, url or javascript")),
        };
        self.apply(a, Edit::SetBookmarkAction { path, action })
    }

    /// Each bookmark's action JSON by 0-based path.
    pub(crate) fn bookmark_action_map(&self, a: &Args) -> Result<std::collections::HashMap<Vec<usize>, Value>> {
        let id = self.doc(a)?.id;
        let Ok(all) = self.session.bookmark_actions(id) else { return Ok(Default::default()) };
        Ok(all
            .into_iter()
            .map(|(path, act)| {
                let v = match act {
                    BookmarkAction::GoTo(t) => {
                        let mut v = json!({ "type": "goto" });
                        target_json(self, id, &t, &mut v);
                        v
                    }
                    BookmarkAction::Uri(u) => json!({ "type": "uri", "url": u }),
                    BookmarkAction::JavaScript(js) => json!({ "type": "javascript", "script": js }),
                    BookmarkAction::Other(s) => json!({ "type": "other", "action": s }),
                    BookmarkAction::None => json!({ "type": "none" }),
                };
                (path, v)
            })
            .collect())
    }
}
