//! The Reading Order tool and the Order panel: `reading_order_list`,
//! `reading_order_tag`, `reading_order_move` and `reading_order_clear`.

use pdfcraft_engine::a11y::{ArtifactSubtype, ArtifactType, OrderElement, PageOrder, RegionRole, RegionTag};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

fn round(r: [f64; 4]) -> [f64; 4] {
    r.map(|x| (x * 100.0).round() / 100.0)
}

fn element_json(e: &OrderElement) -> Value {
    json!({
        "element": e.node + 1,
        "obj": e.obj.map(|o| o.num),
        "type": e.ty,
        "standard": e.standard,
        "mcids": e.mcids,
        "annotations": e.objects.iter().map(|o| o.num).collect::<Vec<_>>(),
        "rect": e.rect.map(round),
        "alt": e.alt,
        "actual_text": e.actual_text,
    })
}

pub(crate) fn order_json(o: &PageOrder) -> Value {
    let regions: Vec<Value> = o
        .regions
        .iter()
        .enumerate()
        .map(|(i, r)| {
            json!({
                "order": i + 1,
                "element": r.node + 1,
                "obj": r.obj.map(|x| x.num),
                "type": r.ty,
                "standard": r.standard,
                "rect": r.rect.map(round),
                "elements": r.elements.iter().map(element_json).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "page": o.page + 1,
        "tagged": o.tagged,
        "regions": regions,
        "untagged": o.untagged,
        "artifacts": o.artifacts,
        "orphaned": o.orphaned,
        "truncated": o.truncated,
    })
}

fn obj_arg(a: &Args, key: &str) -> Result<Option<u32>> {
    match a.opt_int(key)? {
        None => Ok(None),
        Some(n) => u32::try_from(n)
            .ok()
            .filter(|n| *n >= 1)
            .map(Some)
            .ok_or_else(|| ToolError::InvalidArgs(format!("{key} must be an element's object number (obj) from reading_order_list or tags_tree"))),
    }
}

impl Automation {
    pub(crate) fn a11y_reading_order_list(&self, a: &Args) -> Result<Value> {
        let page = self.page(a)?;
        let o = self.doc(a)?.reading_order(page).map_err(failed)?;
        Ok(order_json(&o))
    }

    pub(crate) fn a11y_reading_order_tag(&mut self, a: &Args) -> Result<Value> {
        let page = self.page(a)?;
        let rect = a
            .nums::<4>("rect")?
            .ok_or_else(|| ToolError::InvalidArgs("give rect: [x0, y0, x1, y1] in points from the top-left of the displayed page".into()))?;
        let role_id = a.str("role")?;
        let role = RegionRole::from_id(role_id)
            .ok_or_else(|| ToolError::InvalidArgs(format!("role must be one of {}, not {role_id:?}", RegionRole::IDS.join(", "))))?;
        let artifact_type = a
            .opt_str("artifact_type")?
            .map(|t| {
                ArtifactType::from_name(t)
                    .ok_or_else(|| ToolError::InvalidArgs(format!("artifact_type must be Pagination, Layout, Page or Background, not {t:?}")))
            })
            .transpose()?;
        let artifact_subtype = a
            .opt_str("artifact_subtype")?
            .map(|t| {
                ArtifactSubtype::from_name(t)
                    .ok_or_else(|| ToolError::InvalidArgs(format!("artifact_subtype must be Header, Footer or Watermark, not {t:?}")))
            })
            .transpose()?;
        let request = RegionTag { page, rect, role, alt: a.opt_str("alt")?.map(str::to_owned), artifact_type, artifact_subtype };
        let doc = self.doc(a)?;
        let r = doc.tag_region_preview(&request).map_err(failed)?;
        let id = doc.id;
        self.session.apply(id, pdfcraft_engine::Edit::TagRegion { request }).map_err(failed)?;
        let order = self.doc(a)?.reading_order(page).map_err(failed)?;
        Ok(json!({
            "role": role.id(),
            "elements": r.elements.iter().map(|o| o.num).collect::<Vec<_>>(),
            "mcids": r.mcids,
            "units": r.units,
            "moved": r.moved,
            "split": r.split,
            "wrapped": r.wrapped,
            "artifacts": r.artifacts,
            "removed": r.removed,
            "widgets": r.widgets,
            "created_tree": r.created_tree,
            "order": order_json(&order),
        }))
    }

    pub(crate) fn a11y_reading_order_move(&mut self, a: &Args) -> Result<Value> {
        let element = obj_arg(a, "element")?.ok_or_else(|| ToolError::InvalidArgs("give element: the obj of the element to move".into()))?;
        let before = obj_arg(a, "before")?;
        let after = obj_arg(a, "after")?;
        let to = a.opt_int("to")?;
        let into = obj_arg(a, "into")?;
        let given = usize::from(before.is_some()) + usize::from(after.is_some()) + usize::from(to.is_some()) + usize::from(into.is_some());
        if given != 1 {
            return Err(ToolError::InvalidArgs(
                "give exactly one of before (an obj), after (an obj), into (an obj), or to (a region number on page)".into(),
            ));
        }
        if let Some(target) = into {
            let id = self.doc(a)?.id;
            self.session.apply(id, pdfcraft_engine::Edit::MoveTagInto { element, target }).map_err(failed)?;
            let order = match a.get("page") {
                Some(_) => Some(order_json(&self.doc(a)?.reading_order(self.page(a)?).map_err(failed)?)),
                None => None,
            };
            return Ok(json!({ "moved": true, "element": element, "into": target, "order": order }));
        }
        let (target, after_it) = match (before, after, to) {
            (Some(t), _, _) => (t, false),
            (_, Some(t), _) => (t, true),
            (_, _, Some(n)) => {
                if a.get("page").is_none() {
                    return Err(ToolError::InvalidArgs("to needs the page whose reading order it is (page, 1-based)".into()));
                }
                let page = self.page(a)?;
                let order = self.doc(a)?.reading_order(page).map_err(failed)?;
                let count = order.regions.len();
                let n = usize::try_from(n)
                    .ok()
                    .filter(|n| (1..=count).contains(n))
                    .ok_or_else(|| ToolError::InvalidArgs(format!("to must be a region number from 1 to {count} on page {}", page + 1)))?;
                let from = order.regions.iter().position(|r| r.obj.is_some_and(|o| o.num == element));
                let target = order.regions.get(n - 1).and_then(|r| r.obj).ok_or_else(|| failed("that region's element can't be moved next to"))?;
                // Moving down puts it after the region now at `to`, moving up before it.
                let down = from.is_some_and(|f| f < n - 1);
                (target.num, down)
            }
            _ => return Err(ToolError::InvalidArgs("give before, after or to".into())),
        };
        if target == element {
            let order = match a.get("page") {
                Some(_) => Some(order_json(&self.doc(a)?.reading_order(self.page(a)?).map_err(failed)?)),
                None => None,
            };
            return Ok(json!({ "moved": false, "element": element, "order": order }));
        }
        let id = self.doc(a)?.id;
        self.session.apply(id, pdfcraft_engine::Edit::MoveTag { element, target, after: after_it }).map_err(failed)?;
        let order = match a.get("page") {
            Some(_) => Some(order_json(&self.doc(a)?.reading_order(self.page(a)?).map_err(failed)?)),
            None => None,
        };
        Ok(json!({ "moved": true, "element": element, "target": target, "after": after_it, "order": order }))
    }

    pub(crate) fn a11y_reading_order_clear(&mut self, a: &Args) -> Result<Value> {
        let page = self.page(a)?;
        let keep_artifacts = a.opt_bool("keep_artifacts")?.unwrap_or(false);
        let doc = self.doc(a)?;
        // Tried on a copy first, for the counts and a clear refusal.
        let r = doc.clear_page_structure_preview(page, keep_artifacts).map_err(failed)?;
        let id = doc.id;
        self.session.apply(id, pdfcraft_engine::Edit::ClearPageStructure { page, keep_artifacts }).map_err(failed)?;
        let order = self.doc(a)?.reading_order(page).map_err(failed)?;
        Ok(json!({
            "mcids": r.mcids,
            "artifacts": r.artifacts,
            "references": r.references,
            "removed": r.removed,
            "annotations": r.annotations,
            "order": order_json(&order),
        }))
    }
}
