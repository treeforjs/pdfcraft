//! Accessibility tools: the full check, the accessibility report, and the automatic fixes.

use pdfcraft_engine::a11y::{Category, MAX_NS_MAPPINGS, Namespace, NamespaceSpec, NsMapping, Options, Report, Rule, Status, TagChange, TagKid};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, child, failed, write_atomic};

pub(crate) fn status_id(s: Status) -> &'static str {
    match s {
        Status::Passed => "passed",
        Status::Failed => "failed",
        Status::Manual => "manual",
        Status::Skipped => "skipped",
    }
}

fn category_id(c: Category) -> &'static str {
    match c {
        Category::Document => "document",
        Category::PageContent => "page_content",
        Category::Forms => "forms",
        Category::AlternateText => "alternate_text",
        Category::Tables => "tables",
        Category::Lists => "lists",
        Category::Headings => "headings",
    }
}

fn namespace_json(n: &Namespace) -> Value {
    let role_map: Vec<Value> = n.role_map.iter().map(|m| json!({ "type": m.from, "maps_to": m.to, "namespace": m.namespace })).collect();
    json!({ "obj": n.obj.num, "uri": n.uri, "kind": n.kind.id(), "schema": n.schema, "listed": n.listed, "role_map": role_map })
}

pub(crate) fn report_json(r: &Report) -> Value {
    let results: Vec<Value> = r
        .results
        .iter()
        .map(|x| {
            let findings: Vec<Value> = x.findings.iter().map(|f| json!({ "page": f.page.map(|p| p + 1), "message": f.message })).collect();
            json!({ "rule": x.rule.id(), "name": x.rule.name(), "category": category_id(x.rule.category()), "status": status_id(x.status), "findings": findings })
        })
        .collect();
    json!({
        "passed": r.count(Status::Passed),
        "failed": r.count(Status::Failed),
        "manual": r.count(Status::Manual),
        "skipped": r.count(Status::Skipped),
        "results": results,
    })
}

impl Automation {
    fn a11y_options(&self, a: &Args) -> Result<Options> {
        let mut o = Options::default();
        if a.opt_bool("all")?.unwrap_or(false) {
            o.rules = Rule::ALL.into_iter().collect();
        }
        if a.get("rules").is_some() {
            o.rules = a
                .strs("rules")?
                .iter()
                .map(|id| Rule::from_id(id).ok_or_else(|| ToolError::InvalidArgs(format!("unknown rule {id:?}"))))
                .collect::<Result<_>>()?;
        }
        if a.get("categories").is_some() {
            let cats: Vec<&str> = a.strs("categories")?;
            for c in &cats {
                if !Category::ALL.iter().any(|k| category_id(*k) == *c) {
                    return Err(ToolError::InvalidArgs(format!("unknown category {c:?}")));
                }
            }
            o.rules.retain(|r| cats.contains(&category_id(r.category())));
        }
        if a.opt_ints("pages")?.is_some() {
            o.pages = Some(self.pages(a, "pages")?);
        }
        Ok(o)
    }

    pub(crate) fn a11y_check(&self, a: &Args) -> Result<Value> {
        let o = self.a11y_options(a)?;
        let r = self.doc(a)?.accessibility_check(&o).ok_or_else(|| failed("the document can't be read for checking"))?;
        Ok(report_json(&r))
    }

    pub(crate) fn a11y_report(&self, a: &Args) -> Result<Value> {
        let o = self.a11y_options(a)?;
        let doc = self.doc(a)?;
        let r = doc.accessibility_check(&o).ok_or_else(|| failed("the document can't be read for checking"))?;
        let path = self.resolve(a.str("path")?, true)?;
        let (y, m, d) = self.session.today();
        let html = pdfcraft_engine::a11y::report_html(&r, &doc.name, &format!("{y}-{m:02}-{d:02}"));
        write_atomic(&path, html.as_bytes())?;
        let mut out = report_json(&r);
        out["path"] = json!(path.to_string_lossy());
        Ok(out)
    }

    pub(crate) fn a11y_fix(&mut self, a: &Args) -> Result<Value> {
        let id = a.str("rule")?;
        let rule = Rule::from_id(id).ok_or_else(|| ToolError::InvalidArgs(format!("unknown rule {id:?}")))?;
        let edit = self.doc(a)?.accessibility_fix(rule, a.opt_str("value")?).map_err(failed)?;
        let doc = self.doc(a)?.id;
        self.session.apply(doc, edit).map_err(failed)?;
        let r =
            self.doc(a)?.accessibility_check(&Options { rules: [rule].into(), pages: None }).ok_or_else(|| failed("the document can't be read"))?;
        Ok(json!({ "rule": id, "status": status_id(r.results.iter().find(|x| x.rule == rule).map_or(Status::Skipped, |x| x.status)) }))
    }

    pub(crate) fn accessibility_figures(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let list: Vec<Value> = doc
            .figures()
            .iter()
            .map(|f| {
                // Top-left-origin points on the displayed page.
                let rect = f.page.zip(f.bbox).and_then(|(p, b)| {
                    let info = doc.info.pages.get(p)?;
                    let (u, v) = (info.user_to_view(b[0] as f32, b[1] as f32), info.user_to_view(b[2] as f32, b[3] as f32));
                    let r = |x: f32| (x as f64 * 100.0).round() / 100.0;
                    Some([r(u[0].min(v[0])), r(u[1].min(v[1])), r(u[0].max(v[0])), r(u[1].max(v[1]))])
                });
                json!({ "figure": f.obj.num, "page": f.page.map(|p| p + 1), "alt": f.alt, "rect": rect })
            })
            .collect();
        Ok(json!({ "count": list.len(), "figures": list }))
    }

    pub(crate) fn accessibility_set_alt(&mut self, a: &Args) -> Result<Value> {
        let figure = u32::try_from(a.int("figure")?)
            .map_err(|_| ToolError::InvalidArgs("figure must be a figure number from accessibility_figures".into()))?;
        let edit = if a.opt_bool("decorative")?.unwrap_or(false) {
            pdfcraft_engine::Edit::MarkDecorative { figure }
        } else {
            pdfcraft_engine::Edit::SetAltText { figure, alt: a.opt_str("alt")?.map(str::to_owned) }
        };
        let id = self.doc(a)?.id;
        self.session.apply(id, edit).map_err(failed)?;
        self.accessibility_figures(a)
    }

    pub(crate) fn tags_tree(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let limit = match a.opt_int("limit")? {
            None => 5000,
            Some(n) if (1..=100_000).contains(&n) => n as usize,
            Some(_) => return Err(ToolError::InvalidArgs("limit must be between 1 and 100000".into())),
        };
        let t = doc.tags();
        let kid = |k: &TagKid| match k {
            TagKid::Element(i) => json!({ "element": i + 1 }),
            TagKid::Content { page, mcid, stream } => json!({ "mcid": mcid, "page": page.map(|p| p + 1), "stream": stream.map(|s| s.num) }),
            TagKid::Object { obj, page, subtype } => json!({ "object": obj.num, "subtype": subtype, "page": page.map(|p| p + 1) }),
        };
        let elements: Vec<Value> = t
            .nodes
            .iter()
            .take(limit)
            .enumerate()
            .map(|(i, n)| {
                json!({
                    "element": i + 1,
                    "obj": n.obj.map(|o| o.num),
                    "parent": n.parent.map(|p| p + 1),
                    "depth": n.depth,
                    "type": n.ty,
                    "standard": n.standard,
                    "namespace": n.namespace,
                    "standard_namespace": n.standard_namespace,
                    "namespace_missing": n.namespace_missing,
                    "title": n.title,
                    "alt": n.alt,
                    "actual_text": n.actual_text,
                    "lang": n.lang,
                    "id": n.id,
                    "page": n.page.map(|p| p + 1),
                    "kids": n.kids.iter().map(kid).collect::<Vec<_>>(),
                })
            })
            .collect();
        let role_map: serde_json::Map<String, Value> = t.role_map.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        Ok(json!({
            "tagged": t.exists,
            "marked": t.marked,
            "lang": t.lang,
            "count": t.nodes.len(),
            "truncated": t.truncated || t.nodes.len() > limit,
            "roots": t.roots.iter().map(|r| r + 1).collect::<Vec<_>>(),
            "role_map": role_map,
            "namespaces": t.namespaces.iter().map(namespace_json).collect::<Vec<_>>(),
            "types": doc.tag_types(),
            "elements": elements,
        }))
    }

    pub(crate) fn tags_set(&mut self, a: &Args) -> Result<Value> {
        let element = u32::try_from(a.int("obj")?)
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| ToolError::InvalidArgs("obj must be an element's object number (obj) from tags_tree".into()))?;
        let text = |k: &str| -> Result<Option<Option<String>>> { Ok(a.opt_str(k)?.map(|s| Some(s.to_owned()))) };
        let change = TagChange {
            ty: a.opt_str("type")?.map(str::to_owned),
            alt: text("alt")?,
            actual_text: text("actual_text")?,
            lang: text("lang")?,
            title: text("title")?,
            namespace: text("namespace")?,
        };
        if change.is_empty() {
            return Err(ToolError::InvalidArgs("give at least one of type, namespace, alt, actual_text, lang or title".into()));
        }
        let id = self.doc(a)?.id;
        self.session.apply(id, pdfcraft_engine::Edit::SetTag { element, change }).map_err(failed)?;
        let t = self.doc(a)?.tags();
        let (i, n) = t
            .nodes
            .iter()
            .enumerate()
            .find(|(_, n)| n.obj.is_some_and(|o| o.num == element))
            .ok_or_else(|| failed("the element is no longer in the structure tree"))?;
        Ok(json!({
            "element": i + 1, "obj": element, "type": n.ty, "standard": n.standard, "namespace": n.namespace,
            "standard_namespace": n.standard_namespace, "title": n.title, "alt": n.alt, "actual_text": n.actual_text, "lang": n.lang,
        }))
    }

    pub(crate) fn tags_new(&mut self, a: &Args) -> Result<Value> {
        let ty = a.str("type")?.trim().to_owned();
        let parent = match a.opt_int("parent")? {
            None => None,
            Some(n) => Some(
                u32::try_from(n)
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| ToolError::InvalidArgs("parent must be an element's object number (obj) from tags_tree".into()))?,
            ),
        };
        let doc = self.doc(a)?;
        let before: std::collections::HashSet<u32> = doc.tags().nodes.iter().filter_map(|n| n.obj.map(|o| o.num)).collect();
        let id = doc.id;
        self.session.apply(id, pdfcraft_engine::Edit::NewTag { parent, ty: ty.clone() }).map_err(failed)?;
        let t = self.doc(a)?.tags();
        let new = t.nodes.iter().filter_map(|n| n.obj.map(|o| o.num)).find(|n| !before.contains(n));
        Ok(json!({ "obj": new, "type": ty, "parent": parent, "count": t.nodes.len() }))
    }

    pub(crate) fn tags_delete(&mut self, a: &Args) -> Result<Value> {
        let element = u32::try_from(a.int("obj")?)
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| ToolError::InvalidArgs("obj must be an element's object number (obj) from tags_tree".into()))?;
        let doc = self.doc(a)?;
        let t = doc.tags();
        let node = t.nodes.iter().find(|n| n.obj.is_some_and(|o| o.num == element)).ok_or_else(|| {
            ToolError::InvalidArgs(format!("object {element} is not an element of the structure tree (see tags_tree for the obj numbers)"))
        })?;
        let lifted = node.kids.len();
        let parent = node.parent.and_then(|p| t.nodes.get(p)).and_then(|p| p.obj).map(|o| o.num);
        let id = doc.id;
        self.session.apply(id, pdfcraft_engine::Edit::DeleteTag { element }).map_err(failed)?;
        let count = self.doc(a)?.tags().nodes.len();
        Ok(json!({ "deleted": element, "lifted": lifted, "parent": parent, "count": count }))
    }

    pub(crate) fn tags_add_namespace(&mut self, a: &Args) -> Result<Value> {
        let uri = a.str("uri")?.to_owned();
        let schema = a.opt_str("schema")?.map(str::to_owned);
        let mut role_map = Vec::new();
        if let Some(v) = a.get("role_map") {
            let list = v.as_array().ok_or_else(|| ToolError::InvalidArgs("role_map must be an array of {type, maps_to, namespace}".into()))?;
            if list.len() > MAX_NS_MAPPINGS {
                return Err(ToolError::InvalidArgs(format!("give at most {MAX_NS_MAPPINGS} role mappings at once")));
            }
            for m in list {
                let field = |k: &str| -> Result<Option<String>> {
                    match m.get(k) {
                        None | Some(Value::Null) => Ok(None),
                        Some(Value::String(s)) => Ok(Some(s.clone())),
                        Some(_) => Err(ToolError::InvalidArgs(format!("role_map {k} must be a string"))),
                    }
                };
                if !m.is_object() {
                    return Err(ToolError::InvalidArgs("each role_map entry must be an object {type, maps_to, namespace}".into()));
                }
                let from =
                    field("type")?.ok_or_else(|| ToolError::InvalidArgs("each role_map entry needs type (the namespace's type to map)".into()))?;
                let to = field("maps_to")?
                    .ok_or_else(|| ToolError::InvalidArgs("each role_map entry needs maps_to (empty removes the mapping)".into()))?;
                role_map.push(NsMapping { from, to, namespace: field("namespace")? });
            }
        }
        if role_map.is_empty() && schema.is_none() && a.get("role_map").is_some() {
            return Err(ToolError::InvalidArgs("role_map is empty: give at least one mapping, or leave it out".into()));
        }
        let id = self.doc(a)?.id;
        self.session.apply(id, pdfcraft_engine::Edit::AddNamespace { spec: NamespaceSpec { uri: uri.clone(), schema, role_map } }).map_err(failed)?;
        let uri = pdfcraft_engine::a11y::namespace_uri(&uri);
        let t = self.doc(a)?.tags();
        let ns = t.namespaces.iter().find(|n| n.uri == uri).ok_or_else(|| failed("the namespace is not in the structure tree"))?;
        Ok(json!({ "namespace": namespace_json(ns), "namespaces": t.namespaces.len() }))
    }

    pub(crate) fn tags_mark_artifact(&mut self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::a11y::{ArtifactSelection, ArtifactSubtype, ArtifactType, MAX_ARTIFACT_SELECTION};
        let rect = a.nums::<4>("rect")?;
        let mcids = a.opt_ints("mcids")?.unwrap_or_default();
        let elements = a.opt_ints("elements")?.unwrap_or_default();
        if rect.is_none() && mcids.is_empty() && elements.is_empty() {
            return Err(ToolError::InvalidArgs(
                "select the content: give page with rect and/or mcids, or elements (obj numbers from tags_tree)".into(),
            ));
        }
        if mcids.len().saturating_add(elements.len()) > MAX_ARTIFACT_SELECTION {
            return Err(ToolError::InvalidArgs(format!("select at most {MAX_ARTIFACT_SELECTION} mcids and elements at once")));
        }
        let page = if a.get("page").is_some() {
            Some(self.page(a)?)
        } else if rect.is_some() || !mcids.is_empty() {
            return Err(ToolError::InvalidArgs("rect and mcids need the page they are on (page, 1-based)".into()));
        } else {
            None
        };
        let ty = a
            .opt_str("type")?
            .map(|t| {
                ArtifactType::from_name(t)
                    .ok_or_else(|| ToolError::InvalidArgs(format!("type must be Pagination, Layout, Page or Background, not {t:?}")))
            })
            .transpose()?;
        let subtype = a
            .opt_str("subtype")?
            .map(|t| {
                ArtifactSubtype::from_name(t).ok_or_else(|| ToolError::InvalidArgs(format!("subtype must be Header, Footer or Watermark, not {t:?}")))
            })
            .transpose()?;
        let mut elements_refs = Vec::with_capacity(elements.len());
        for e in elements {
            let n = u32::try_from(e)
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| ToolError::InvalidArgs("elements must be object numbers (obj) from tags_tree".into()))?;
            elements_refs.push(n);
        }
        let doc = self.doc(a)?;
        // The rectangle is in points from the top-left of the displayed page.
        let rect = match (rect, page) {
            (Some(r), Some(p)) => {
                let info = doc.info.pages.get(p).ok_or_else(|| failed("the page is not in the document"))?;
                let (u, v) = (info.view_to_user(r[0] as f32, r[1] as f32), info.view_to_user(r[2] as f32, r[3] as f32));
                let user = [u[0].min(v[0]), u[1].min(v[1]), u[0].max(v[0]), u[1].max(v[1])].map(f64::from);
                Some(user)
            }
            _ => None,
        };
        let selection = ArtifactSelection { page, rect, mcids, elements: elements_refs, ty, subtype };
        let preview = doc.artifact_preview(&selection).map_err(failed)?;
        let id = doc.id;
        self.session.apply(id, pdfcraft_engine::Edit::MarkArtifact { selection }).map_err(failed)?;
        let count = self.doc(a)?.tags().nodes.len();
        Ok(json!({
            "marked": preview.marked.iter().map(|(p, m)| json!({ "page": p + 1, "mcid": m })).collect::<Vec<_>>(),
            "untagged": preview.untagged,
            "elements_removed": preview.elements_removed,
            "pages": preview.pages.iter().map(|p| p + 1).collect::<Vec<_>>(),
            "elements": count,
        }))
    }

    pub(crate) fn alt_text_list(&self, a: &Args) -> Result<Value> {
        let all = a.opt_bool("all")?.unwrap_or(false);
        let doc = self.doc(a)?;
        let targets = doc.alt_targets();
        let missing = targets.figures.iter().filter(|f| f.needs_alt).count() + targets.images.len();
        let figures: Vec<Value> = targets
            .figures
            .iter()
            .filter(|f| all || f.needs_alt)
            .map(|f| {
                json!({
                    "figure": f.obj.map(|o| o.num),
                    "element": f.node + 1,
                    "page": f.page.map(|p| p + 1),
                    "alt": f.alt,
                    "actual_text": f.actual_text,
                    "needs_alt": f.needs_alt,
                    "rect": f.page.zip(f.bbox).and_then(|(p, b)| view_rect(doc, p, b)),
                })
            })
            .collect();
        let images: Vec<Value> = targets
            .images
            .iter()
            .map(|i| json!({ "page": i.page + 1, "image": i.index + 1, "name": i.name, "rect": view_rect(doc, i.page, i.bbox) }))
            .collect();
        let check = doc
            .accessibility_check(&Options { rules: [Rule::FiguresAltText].into(), pages: None })
            .and_then(|r| r.result(Rule::FiguresAltText).map(|x| status_id(x.status)));
        Ok(json!({ "missing": missing, "figures": figures, "untagged_images": images, "figures_alt_text": check }))
    }

    pub(crate) fn alt_text_set(&mut self, a: &Args) -> Result<Value> {
        let decorative = a.opt_bool("decorative")?.unwrap_or(false);
        let alt = a.opt_str("alt")?;
        let edit = match (a.opt_int("figure")?, a.opt_int("image")?) {
            (Some(f), None) => {
                let figure = u32::try_from(f)
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| ToolError::InvalidArgs("figure must be a figure number from alt_text_list".into()))?;
                if decorative {
                    pdfcraft_engine::Edit::MarkDecorative { figure }
                } else {
                    let alt = alt.ok_or_else(|| ToolError::InvalidArgs("give alt (empty clears it) or decorative: true".into()))?;
                    pdfcraft_engine::Edit::SetAltText { figure, alt: Some(alt.to_owned()) }
                }
            }
            (None, Some(i)) => {
                if decorative {
                    return Err(ToolError::InvalidArgs("decorative applies to tagged figures; give alt for an untagged image".into()));
                }
                let page = self.page(a)?;
                let index = usize::try_from(i)
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .ok_or_else(|| ToolError::InvalidArgs("image must be a 1-based image number from alt_text_list".into()))?;
                let alt = alt.filter(|s| !s.trim().is_empty()).ok_or_else(|| ToolError::InvalidArgs("give the alternate text (alt)".into()))?;
                pdfcraft_engine::Edit::TagImage { page, index, alt: alt.to_owned() }
            }
            _ => return Err(ToolError::InvalidArgs("give either figure, or page and image".into())),
        };
        let id = self.doc(a)?.id;
        self.session.apply(id, edit).map_err(failed)?;
        self.alt_text_list(a)
    }
}

/// Top-left-origin points on the displayed page for a user-space box.
fn view_rect(doc: &pdfcraft_engine::Document, page: usize, b: [f64; 4]) -> Option<[f64; 4]> {
    let info = doc.info.pages.get(page)?;
    let (u, v) = (info.user_to_view(b[0] as f32, b[1] as f32), info.user_to_view(b[2] as f32, b[3] as f32));
    let r = |x: f32| (x as f64 * 100.0).round() / 100.0;
    Some([r(u[0].min(v[0])), r(u[1].min(v[1])), r(u[0].max(v[0])), r(u[1].max(v[1]))])
}

impl Automation {
    pub(crate) fn ocr_settings(&self, a: &Args) -> Result<pdfcraft_engine::ocr::OcrSettings> {
        let mut settings = pdfcraft_engine::ocr::OcrSettings::default();
        if let Some(d) = a.opt_num("dpi")? {
            settings.dpi = d as f32;
        }
        if let Some(l) = a.opt_str("language")? {
            if !pdfcraft_engine::ocr::KNOWN_LANGUAGES.iter().any(|x| x.0 == l) {
                return Err(ToolError::InvalidArgs(format!("unsupported language {l:?}")));
            }
            settings.language = l.into();
        }
        if let Some(e) = a.opt_str("engine")? {
            if self.session.ocr_engine_registry().get(e).is_none() {
                return Err(ToolError::InvalidArgs(format!(
                    "unknown OCR engine {e:?}; use one of {} (ocr_engines_list)",
                    self.session.ocr_engine_registry().ids().join(", ")
                )));
            }
            settings.engine = Some(e.into());
        }
        if let Some(s) = a.opt_bool("skip_text_pages")? {
            settings.skip_text_pages = s;
        }
        if let Some(s) = a.opt_bool("auto_rotate")? {
            settings.auto_rotate = s;
        }
        if let Some(s) = a.opt_bool("deskew")? {
            settings.deskew = s;
        }
        settings.despeckle = despeckle_options(a, "despeckle", "despeckle_light", "despeckle_median")?;
        settings.background = background_options(a, "background", "background_white_point")?;
        match a.opt_str("output")? {
            None | Some("exact") => {}
            Some("straighten") => settings.straighten = true,
            Some("editable") => settings.editable = Some(pdfcraft_engine::ocr::EditableOptions::default()),
            Some(other) => return Err(ToolError::InvalidArgs(format!("unknown output {other:?}; use exact, straighten or editable"))),
        }
        if let Some(c) = a.opt_num("min_confidence")? {
            let Some(e) = settings.editable.as_mut() else {
                return Err(ToolError::InvalidArgs("min_confidence applies to output editable".into()));
            };
            if !(c.is_finite() && (0.0..=1.0).contains(&c)) {
                return Err(ToolError::InvalidArgs("min_confidence must be between 0 and 1".into()));
            }
            e.min_confidence = c as f32;
        }
        settings.validate().map_err(ToolError::InvalidArgs)?;
        // The engine reads the language: a language it doesn't read is a bad argument, an
        // engine that can't run a failure.
        if let Err(why) = self.session.ocr_engine_for(&settings) {
            return Err(if why.starts_with("unsupported recognition language") { ToolError::InvalidArgs(why) } else { failed(why) });
        }
        Ok(settings)
    }

    /// The engine `settings` read with, readied (models loaded, program and language found).
    pub(crate) fn ocr_ready(&self, settings: &pdfcraft_engine::ocr::OcrSettings) -> Result<std::sync::Arc<dyn pdfcraft_engine::ocr::OcrEngine>> {
        let engine = self.session.ocr_engine_for(settings).map_err(failed)?;
        engine.prepare(&settings.language).map_err(|e| failed(e.to_string()))?;
        Ok(engine)
    }

    /// `ocr_engines_list`: every registered engine.
    pub(crate) fn ocr_engines_list(&self) -> Result<Value> {
        let selected = self.session.ocr_engine_id().to_owned();
        let engines: Vec<Value> = self
            .session
            .ocr_engines()
            .into_iter()
            .map(|e| {
                let languages: Vec<Value> = e
                    .languages
                    .iter()
                    .map(|c| json!({ "code": c, "name": pdfcraft_engine::ocr::KNOWN_LANGUAGES.iter().find(|l| l.0 == c).map(|l| l.2) }))
                    .collect();
                json!({
                    "id": e.id, "name": e.name, "available": e.available, "version": e.version, "languages": languages,
                    "external": e.external, "detail": e.detail, "selected": e.id == selected,
                    "default": e.id == pdfcraft_engine::ocr::DEFAULT_ENGINE,
                })
            })
            .collect();
        Ok(json!({ "selected": selected, "default": pdfcraft_engine::ocr::DEFAULT_ENGINE, "engines": engines }))
    }

    /// `ocr_engine_set`: choose the engine recognition uses from now on in this session.
    pub(crate) fn ocr_engine_set(&mut self, a: &Args) -> Result<Value> {
        let id = a.str("engine")?;
        if self.session.ocr_engine_registry().get(id).is_none() {
            return Err(ToolError::InvalidArgs(format!(
                "unknown OCR engine {id:?}; use one of {} (ocr_engines_list)",
                self.session.ocr_engine_registry().ids().join(", ")
            )));
        }
        let info = self.session.set_ocr_engine(id).map_err(failed)?;
        Ok(json!({ "selected": info.id, "name": info.name, "version": info.version, "languages": info.languages }))
    }

    pub(crate) fn ocr_recognize_files(&mut self, a: &Args) -> Result<Value> {
        let settings = self.ocr_settings(a)?;
        let folder = self.resolve(a.str("folder")?, true)?;
        std::fs::create_dir_all(&folder).map_err(|e| failed(e.to_string()))?;
        // Every source must be inside the root before any file is read or written; a missing
        // file inside it is reported for that file alone.
        let sources = a.strs("paths")?.into_iter().map(|p| Ok((p, self.resolve(p, true)?))).collect::<Result<Vec<_>>>()?;
        let ocr = self.ocr_ready(&settings)?;
        let mut out = Vec::new();
        for (p, src) in sources {
            let name = std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document.pdf".into());
            let result = std::fs::read(&src)
                .map_err(|e| e.to_string())
                .and_then(|bytes| pdfcraft_engine::ocr::recognize_file(&name, std::sync::Arc::new(bytes), None, settings.clone(), &ocr, |_, _| true));
            out.push(match result {
                Ok(r) => {
                    let target = child(&folder, &name);
                    write_atomic(&target, &r.bytes)?;
                    let skipped: Vec<usize> = r.pages.iter().filter(|p| p.skipped.is_some()).map(|p| p.page + 1).collect();
                    let orientation: Vec<Value> =
                        r.pages.iter().filter(|p| p.orientation.is_some()).map(crate::ocr_review::orientation_json).collect();
                    let editable: Vec<Value> = r
                        .pages
                        .iter()
                        .filter(|p| p.editable.is_some())
                        .map(|p| json!({ "page": p.page + 1, "editable": crate::ocr_review::editable_json(p) }))
                        .collect();
                    json!({ "path": p, "output": target.to_string_lossy(), "words": r.words(), "skipped_pages": skipped, "orientation": orientation, "editable": editable })
                }
                Err(e) => json!({ "path": p, "error": e }),
            });
        }
        Ok(json!({ "files": out }))
    }

    pub(crate) fn ocr_recognize(&mut self, a: &Args) -> Result<Value> {
        let settings = self.ocr_settings(a)?;
        let pages = if a.opt_ints("pages")?.is_some() { self.pages(a, "pages")? } else { Vec::new() };
        let id = self.doc(a)?.id;
        let found = self.session.recognize_text(id, &pages, settings).map_err(failed)?;
        let list: Vec<Value> = found
            .iter()
            .map(|p| match &p.skipped {
                Some(why) => json!({ "page": p.page + 1, "skipped": why }),
                None => json!({
                    "page": p.page + 1, "words": p.word_count(), "text": p.text(),
                    "orientation": crate::ocr_review::orientation_json(p), "despeckle": crate::ocr_review::despeckle_json(p.despeckle.as_ref()),
                    "background": crate::ocr_review::background_json(p.background.as_ref()),
                    "editable": crate::ocr_review::editable_json(p),
                }),
            })
            .collect();
        Ok(json!({ "words": found.iter().map(|p| p.word_count()).sum::<usize>(), "pages": list }))
    }

    /// Scan & OCR ▸ Enhance scan ▸ Despeckle (`scan_despeckle`).
    pub(crate) fn scan_despeckle(&mut self, a: &Args) -> Result<Value> {
        let mut options = despeckle_options(a, "level", "light", "median")?;
        if a.opt_str("level")?.is_none() {
            options.level = pdfcraft_engine::ocr::DespeckleLevel::Medium;
        }
        if !options.any() {
            return Err(ToolError::InvalidArgs(
                "level off with median false would change nothing; choose level low, medium or high, or median true".into(),
            ));
        }
        let pages = if a.opt_ints("pages")?.is_some() { self.pages(a, "pages")? } else { Vec::new() };
        let id = self.doc(a)?.id;
        let r = self.session.despeckle_scans(id, &pages, options).map_err(|e| failed(e.to_string()))?;
        Ok(json!({
            "images": r.images, "images_changed": r.images_changed, "images_skipped": r.images_skipped,
            "dark_specks": r.stats.dark_specks, "light_specks": r.stats.light_specks, "impulses": r.stats.impulses,
            "pixels_changed": r.stats.pixels_changed, "undoable": r.images_changed > 0,
        }))
    }

    /// Scan & OCR ▸ Enhance scan ▸ Remove background (`scan_remove_background`).
    pub(crate) fn scan_remove_background(&mut self, a: &Args) -> Result<Value> {
        let mut options = background_options(a, "strength", "white_point")?;
        if a.opt_str("strength")?.is_none() && options.white_point.is_none() {
            options.strength = pdfcraft_engine::ocr::BackgroundStrength::Medium;
        }
        if !options.any() {
            return Err(ToolError::InvalidArgs(
                "strength off without a white_point would change nothing; choose strength low, medium or high, or give white_point".into(),
            ));
        }
        let pages = if a.opt_ints("pages")?.is_some() { self.pages(a, "pages")? } else { Vec::new() };
        let id = self.doc(a)?.id;
        let r = self.session.remove_scan_backgrounds(id, &pages, options).map_err(|e| failed(e.to_string()))?;
        Ok(json!({
            "images": r.images, "images_changed": r.images_changed, "images_skipped": r.images_skipped,
            "images_unchanged": r.images_unchanged, "pixels_whitened": r.pixels_whitened,
            "pixels_changed": r.pixels_changed, "undoable": r.images_changed > 0,
        }))
    }

    pub(crate) fn ocr_status(&self) -> Result<Value> {
        use pdfcraft_engine::ocr;
        let dirs: Vec<String> = ocr::Models::search_dirs().iter().map(|d| d.to_string_lossy().into_owned()).collect();
        let langs: Vec<Value> = ocr::LANGUAGES.iter().map(|(c, n)| json!({ "code": c, "name": n })).collect();
        Ok(json!({
            "available": ocr::available(),
            "search_dirs": dirs,
            "languages": langs,
            "engine": self.session.ocr_engine_id(),
            "loaded": ocr::OcrsEngine::loaded(),
            "models": crate::web_ocr::pinned_json(),
        }))
    }
}

/// The export tools' text recognition arguments: `ocr` (auto, on, off; default auto) and
/// `ocr_language` (default en).
pub(crate) fn export_ocr_options(a: &Args) -> Result<pdfcraft_engine::export_ocr::ExportOcr> {
    use pdfcraft_engine::export_ocr::{ExportOcr, ExportOcrMode};
    let mut options = ExportOcr::default();
    if let Some(m) = a.opt_str("ocr")? {
        options.mode = ExportOcrMode::from_id(m).ok_or_else(|| ToolError::InvalidArgs(format!("ocr must be auto, on or off, not {m:?}")))?;
    }
    if let Some(l) = a.opt_str("ocr_language")? {
        options.language = l.into();
    }
    options.validate().map_err(ToolError::InvalidArgs)?;
    Ok(options)
}

/// What text recognition did for an export (1-based pages).
pub(crate) fn export_ocr_json(r: &pdfcraft_engine::export_ocr::ExportOcrReport) -> Value {
    let one = |v: &[usize]| v.iter().map(|p| p + 1).collect::<Vec<_>>();
    let skipped: Vec<Value> = r.skipped_pages.iter().map(|(p, why)| json!({ "page": p + 1, "reason": why })).collect();
    json!({
        "mode": r.mode.id(),
        "language": r.language,
        "ran": r.ran(),
        "pages_without_text": one(&r.pages_without_text),
        "scanned_pages": one(&r.scanned_pages),
        "recognized_pages": one(&r.recognized),
        "skipped_pages": skipped,
        "words": r.words,
        "not_run": r.not_run,
    })
}

/// A script's request; an address carries the internet access decision for it (`url_access`).
pub(crate) fn request_json(session: &pdfcraft_engine::Session, r: &pdfcraft_engine::js::Request) -> Value {
    use pdfcraft_engine::js::Request as R;
    let access = |u: &str| crate::url_access::check_json(u, &session.check_url(u));
    match r {
        R::Reset(n) => json!({ "reset": n }),
        R::Print => json!({ "print": true }),
        R::GoToPage(p) => json!({ "page": p + 1 }),
        R::LaunchUrl(u) => json!({ "url": u, "url_access": access(u) }),
        R::Submit(u) => json!({ "submit": u, "url_access": access(u) }),
        R::Focus(f) => json!({ "focus": f }),
        R::Beep => json!({ "beep": true }),
        R::SaveAs => json!({ "save_as": true }),
        R::SetLayer { index, on } => json!({ "layer": index, "visible": on }),
    }
}

/// A dialog a script waits on, for `js_run`.
fn question_json(q: &pdfcraft_engine::js::Question) -> Value {
    use pdfcraft_engine::js::Question as Q;
    let buttons: Vec<&str> = q
        .buttons()
        .iter()
        .map(|b| match b {
            1 => "ok",
            2 => "cancel",
            3 => "no",
            _ => "yes",
        })
        .collect();
    match q {
        Q::Alert { message, title, .. } => json!({ "kind": "alert", "message": message, "title": title, "buttons": buttons }),
        Q::Response { question, title, default, label, password } => {
            json!({ "kind": "response", "message": question, "title": title, "default": default, "label": label, "password": password })
        }
    }
}

/// A page trigger id (page_open, page_close).
fn page_trigger(id: &str) -> Result<pdfcraft_engine::PageTrigger> {
    pdfcraft_engine::PageTrigger::from_id(id).ok_or_else(|| ToolError::InvalidArgs(format!("unknown trigger {id:?} (one of page_open, page_close)")))
}

/// `js_run`'s `answers`: a button name (yes, no, ok, cancel) for an alert, text or null for
/// app.response.
fn answers(a: &Args) -> Result<Vec<pdfcraft_engine::js::Answer>> {
    use pdfcraft_engine::js::Answer;
    let Some(list) = a.get("answers") else { return Ok(Vec::new()) };
    let list = list.as_array().ok_or_else(|| Args::wrong("answers", "an array"))?;
    list.iter()
        .map(|v| match v {
            Value::Null => Ok(Answer::Text(None)),
            Value::Object(o) => match o.get("text") {
                Some(Value::String(t)) => Ok(Answer::Text(Some(t.clone()))),
                Some(Value::Null) => Ok(Answer::Text(None)),
                _ => Err(Args::wrong("answers", "button names, {\"text\": …} objects or null")),
            },
            Value::String(b) => match b.to_ascii_lowercase().as_str() {
                "ok" => Ok(Answer::Button(1)),
                "cancel" => Ok(Answer::Button(2)),
                "no" => Ok(Answer::Button(3)),
                "yes" => Ok(Answer::Button(4)),
                _ => Err(Args::wrong("answers", "button names (yes, no, ok, cancel), {\"text\": …} objects or null")),
            },
            _ => Err(Args::wrong("answers", "button names, {\"text\": …} objects or null")),
        })
        .collect()
}

impl Automation {
    pub(crate) fn js_run(&mut self, a: &Args) -> Result<Value> {
        let id = self.doc(a)?.id;
        let dialogs = pdfcraft_engine::js::Dialogs { interactive: true, answers: answers(a)? };
        let o = self.session.run_javascript_with(id, a.str("script")?, a.opt_str("field")?, &dialogs).map_err(failed)?;
        if let Some(q) = &o.question {
            return Ok(json!({ "question": question_json(q), "answered": dialogs.answers.len() }));
        }
        let reqs: Vec<Value> = o.requests.iter().map(|r| request_json(&self.session, r)).collect();
        Ok(json!({ "alerts": o.alerts, "console": o.console, "requests": reqs, "error": o.error, "result": o.result }))
    }

    pub(crate) fn js_document_scripts(&self, a: &Args) -> Result<Value> {
        let list: Vec<Value> = self.doc(a)?.document_scripts().into_iter().map(|(n, s)| json!({ "name": n, "script": s })).collect();
        Ok(json!({ "scripts": list }))
    }

    pub(crate) fn js_set_document_script(&mut self, a: &Args) -> Result<Value> {
        let edit = pdfcraft_engine::Edit::SetDocumentScript { name: a.str("name")?.into(), script: a.opt_str("script")?.map(str::to_string) };
        let id = self.doc(a)?.id;
        self.session.apply(id, edit).map_err(failed)?;
        self.js_document_scripts(a)
    }

    pub(crate) fn js_document_actions(&self, a: &Args) -> Result<Value> {
        let actions = self.doc(a)?.document_actions();
        let list: Vec<Value> = pdfcraft_engine::DocTrigger::ALL
            .iter()
            .map(|t| {
                let script = actions.iter().find(|(x, _)| x == t).map(|(_, s)| s.as_str());
                json!({ "trigger": t.id(), "label": t.label(), "script": script })
            })
            .collect();
        Ok(json!({ "actions": list }))
    }

    pub(crate) fn js_set_document_action(&mut self, a: &Args) -> Result<Value> {
        let ids: Vec<&str> = pdfcraft_engine::DocTrigger::ALL.iter().map(|t| t.id()).collect();
        let t = a.str("trigger")?;
        let trigger = pdfcraft_engine::DocTrigger::from_id(t)
            .ok_or_else(|| crate::ToolError::InvalidArgs(format!("unknown trigger {t:?} (one of {})", ids.join(", "))))?;
        let edit = pdfcraft_engine::Edit::SetDocumentAction { trigger, script: a.opt_str("script")?.map(str::to_string) };
        let id = self.doc(a)?.id;
        self.session.apply(id, edit).map_err(failed)?;
        self.js_document_actions(a)
    }

    /// Page Properties ▸ Actions for every page (or `page`): the Page Open and Page Close
    /// scripts that are set.
    pub(crate) fn js_page_actions(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let only = match a.opt_int("page")? {
            None => None,
            Some(_) => Some(self.page(a)?),
        };
        let list: Vec<Value> = match only {
            // One page: only its own /AA is read.
            Some(p) => pdfcraft_engine::PageTrigger::ALL
                .iter()
                .map(|t| json!({ "page": p + 1, "trigger": t.id(), "label": t.label(), "script": doc.page_action(p, *t) }))
                .collect(),
            None => doc
                .page_actions()
                .into_iter()
                .map(|(p, t, js)| json!({ "page": p + 1, "trigger": t.id(), "label": t.label(), "script": js }))
                .collect(),
        };
        Ok(json!({ "actions": list }))
    }

    pub(crate) fn js_set_page_action(&mut self, a: &Args) -> Result<Value> {
        let page = self.page(a)?;
        let trigger = page_trigger(a.str("trigger")?)?;
        let edit = pdfcraft_engine::Edit::SetPageAction { page, trigger, script: a.opt_str("script")?.map(str::to_string) };
        let id = self.doc(a)?.id;
        self.session.apply(id, edit).map_err(failed)?;
        let script = self.doc(a)?.page_action(page, trigger);
        Ok(json!({ "page": page + 1, "trigger": trigger.id(), "script": script }))
    }

    /// Run the JavaScript action of a link, a bookmark or a page trigger, as a click or the
    /// page opening or closing would.
    pub(crate) fn js_run_action(&mut self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::js::ActionSource;
        let id = self.doc(a)?.id;
        let given = [a.get("link").is_some(), a.get("bookmark").is_some(), a.get("trigger").is_some()].iter().filter(|x| **x).count();
        if given != 1 {
            return Err(ToolError::InvalidArgs("pass exactly one of link (with page and index), bookmark (a path) or trigger (with page)".into()));
        }
        let source = if a.get("link").is_some() {
            let link = a.get("link").and_then(Value::as_object).ok_or_else(|| Args::wrong("link", "an object { page, index }"))?;
            let n = |k: &str| {
                link.get(k)
                    .and_then(Value::as_u64)
                    .and_then(|v| usize::try_from(v).ok())
                    .and_then(|v| v.checked_sub(1))
                    .ok_or_else(|| Args::wrong("link", "{ page, index } with 1-based numbers (see link_list)"))
            };
            ActionSource::Link { page: n("page")?, index: n("index")? }
        } else if a.get("bookmark").is_some() {
            ActionSource::Bookmark { path: a.path("bookmark")? }
        } else {
            ActionSource::Page { page: self.page(a)?, trigger: page_trigger(a.str("trigger")?)? }
        };
        let dialogs = pdfcraft_engine::js::Dialogs { interactive: true, answers: answers(a)? };
        let o = self.session.run_action_javascript(id, &source, &dialogs).map_err(failed)?;
        if let Some(q) = &o.question {
            return Ok(json!({ "question": question_json(q), "answered": dialogs.answers.len() }));
        }
        let reqs: Vec<Value> = o.requests.iter().map(|r| request_json(&self.session, r)).collect();
        Ok(json!({ "alerts": o.alerts, "console": o.console, "requests": reqs, "error": o.error, "result": o.result }))
    }

    pub(crate) fn form_set_script(&mut self, a: &Args) -> Result<Value> {
        let edit = pdfcraft_engine::Edit::SetFieldScript {
            name: a.str("field")?.into(),
            event: a.str("event")?.into(),
            script: a.opt_str("script")?.map(str::to_string),
        };
        let id = self.doc(a)?.id;
        self.session.apply(id, edit).map_err(failed)?;
        let out = self.session.take_js_output(id);
        Ok(json!({ "field": a.str("field")?, "event": a.str("event")?, "console": out.console, "errors": out.errors }))
    }

    pub(crate) fn form_merge_data(&mut self, a: &Args) -> Result<Value> {
        let mut files = Vec::new();
        for p in a.strs("paths")? {
            let path = self.resolve(p, false)?;
            files.push((p.to_string(), std::fs::read(&path).map_err(|e| failed(format!("{p}: {e}")))?));
        }
        let csv = pdfcraft_engine::merge_data_files(&files).map_err(failed)?;
        let target = self.resolve(a.str("path")?, true)?;
        write_atomic(&target, csv.as_bytes())?;
        let columns = csv.lines().next().map_or(0, |h| h.split(',').count());
        Ok(json!({ "path": target.to_string_lossy(), "rows": files.len(), "columns": columns }))
    }

    pub(crate) fn doc_export_office(&self, a: &Args) -> Result<Value> {
        let path = self.resolve(a.str("path")?, true)?;
        let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let format = pdfcraft_engine::compare::OfficeFormat::from_extension(&ext)
            .ok_or_else(|| ToolError::InvalidArgs(format!("unsupported extension {ext:?} (docx, html, rtf or xlsx)")))?;
        let options = export_ocr_options(a)?;
        let id = self.doc(a)?.id;
        let (bytes, report) = self.session.with_export_ocr(id, &[], &options, |d| d.export_office(format)).map_err(failed)?;
        write_atomic(&path, &bytes)?;
        Ok(json!({ "path": path.to_string_lossy(), "bytes": bytes.len(), "format": format.extension(), "ocr": export_ocr_json(&report) }))
    }

    /// `doc_export_text` (with `ocr`/`ocr_language`).
    pub(crate) fn doc_export_text(&self, a: &Args, pages: &[usize]) -> Result<Value> {
        let path = self.resolve(a.str("path")?, true)?;
        let options = export_ocr_options(a)?;
        let id = self.doc(a)?.id;
        let (text, report) =
            self.session.with_export_ocr(id, pages, &options, |d| pdfcraft_engine::export::Exporter::new(d).text_of(pages)).map_err(failed)?;
        write_atomic(&path, text.as_bytes())?;
        Ok(json!({ "path": path.to_string_lossy(), "pages": pages.len(), "bytes": text.len(), "ocr": export_ocr_json(&report) }))
    }

    pub(crate) fn doc_export_pptx(&self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::pptx_export::{PptxMode, PptxOptions};
        let path = self.resolve(a.str("path")?, true)?;
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if ext != "pptx" {
            return Err(ToolError::InvalidArgs(format!("path must end in .pptx, not {:?}", ext)));
        }
        let mode = match a.opt_str("mode")? {
            None => PptxMode::Editable,
            Some(m) => PptxMode::from_id(m).ok_or_else(|| ToolError::InvalidArgs(format!("unknown mode {m:?} (editable or image)")))?,
        };
        let dpi = a.opt_num("dpi")?.unwrap_or(150.0);
        if !(36.0..=600.0).contains(&dpi) {
            return Err(ToolError::InvalidArgs(format!("dpi must be between 36 and 600, not {dpi}")));
        }
        let pages = match a.opt_ints("pages")? {
            Some(_) => self.pages(a, "pages")?,
            None => Vec::new(),
        };
        let mut ocr = export_ocr_options(a)?;
        if mode == PptxMode::Image {
            // Pictures of the pages carry no text to recognize.
            ocr.mode = pdfcraft_engine::export_ocr::ExportOcrMode::Off;
        }
        let id = self.doc(a)?.id;
        let options = PptxOptions { pages, mode, dpi };
        let (out, report) = self.session.with_export_ocr(id, &options.pages, &ocr, |d| d.export_pptx(&options)).map_err(failed)?;
        write_atomic(&path, &out.bytes)?;
        let s = out.stats;
        Ok(json!({
            "path": path.to_string_lossy(),
            "bytes": out.bytes.len(),
            "mode": mode.id(),
            "slides": s.slides,
            "text_boxes": s.text_boxes,
            "pictures": s.pictures,
            "backgrounds": s.backgrounds,
            "omitted": s.omitted,
            "warnings": out.warnings,
            "ocr": export_ocr_json(&report),
        }))
    }

    /// The level asked for, else the one the document declares, else 2b; and where it came from.
    fn pdfa_level(&self, a: &Args) -> Result<(pdfcraft_engine::pdfa::Level, &'static str)> {
        use pdfcraft_engine::pdfa::Level;
        match a.opt_str("level")? {
            Some(l) => {
                let level = Level::from_id(l).ok_or_else(|| {
                    let ids: Vec<&str> = Level::ALL.iter().map(|l| l.id()).collect();
                    let hint = if l.trim().eq_ignore_ascii_case("1u") { " (PDF/A-1 has no level u)" } else { "" };
                    ToolError::InvalidArgs(format!("unknown PDF/A level {l:?}{hint}: use one of {}", ids.join(", ")))
                })?;
                Ok((level, "argument"))
            }
            None => Ok(match self.doc(a)?.pdfa_declared_level() {
                Some(level) => (level, "declared"),
                None => (Level::A2b, "default"),
            }),
        }
    }

    fn pdfa_issue_json(i: &pdfcraft_engine::pdfa::Issue) -> Value {
        json!({ "clause": i.clause, "message": i.message, "page": i.page.map(|p| p + 1), "fixable": i.fixable, "severity": i.severity.id() })
    }

    pub(crate) fn pdfa_verify(&self, a: &Args) -> Result<Value> {
        let (level, source) = self.pdfa_level(a)?;
        self.pdfa_verify_at(a, level, source)
    }

    fn pdfa_verify_at(&self, a: &Args, level: pdfcraft_engine::pdfa::Level, source: &str) -> Result<Value> {
        let doc = self.doc(a)?;
        let found = doc.pdfa_verify(level);
        let errors = found.iter().filter(|i| i.is_error()).count();
        let issues: Vec<Value> = found.iter().map(Self::pdfa_issue_json).collect();
        let d = doc.standards();
        let mut limitations = vec![pdfcraft_engine::pdfa::analysis::LIMITATION];
        if level.needs_unicode() {
            limitations.push("Unicode: glyphs shown by page content, form XObjects and annotation appearances are checked; Type 3 glyph procedures, patterns and per-character ActualText are not.");
        }
        if level.needs_structure() {
            limitations.push("Level a: structure, tagging, role mapping, language and figure alternate text are machine-checked; whether tags, reading order and alternate text are appropriate needs a person.");
        }
        Ok(json!({
            "level": level.label(),
            "level_id": level.id(),
            "level_source": source,
            "conformance_level": level.conformance().letter(),
            "compliant": false,
            "no_detected_issues": errors == 0,
            "errors": errors,
            "warnings": found.len() - errors,
            "validation_complete": false,
            "conformance": "not-established",
            "limitations": limitations,
            "issues": issues,
            "declared": { "pdfa": d.pdfa.map(|(p, c)| format!("PDF/A-{p}{}", c.to_lowercase())), "pdfua": d.pdfua, "output_intents": d.output_intents },
        }))
    }

    pub(crate) fn pdfua_verify(&self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::a11y::{MAX_UA_FINDINGS, UA_NOT_CHECKED, UaOptions, UaPart};
        let part = match a.opt_int("part")? {
            None => UaPart::Ua1,
            Some(n) => UaPart::from_number(n).ok_or_else(|| ToolError::InvalidArgs(format!("part must be 1 (PDF/UA-1) or 2 (PDF/UA-2), not {n}")))?,
        };
        let max_findings = match a.opt_int("max_findings")? {
            None => UaOptions::default().max_findings,
            Some(n) => usize::try_from(n)
                .ok()
                .filter(|n| (1..=MAX_UA_FINDINGS).contains(n))
                .ok_or_else(|| ToolError::InvalidArgs(format!("max_findings must be 1 to {MAX_UA_FINDINGS}, not {n}")))?,
        };
        let r = self.doc(a)?.pdfua_verify(&UaOptions { part, max_findings }).ok_or_else(|| failed("the document can't be read for checking"))?;
        let counts: serde_json::Map<String, Value> = r.checked.iter().map(|id| ((*id).to_owned(), json!(r.count(id)))).collect();
        let kept = |id: &str| r.findings.iter().filter(|f| f.checkpoint == id).count();
        let truncated = r.counts.iter().any(|(id, n)| *n > kept(id));
        let findings: Vec<Value> = r
            .findings
            .iter()
            .map(|f| {
                json!({
                    "checkpoint": f.checkpoint,
                    "severity": f.severity.id(),
                    "message": f.message,
                    "page": f.page.map(|p| p + 1),
                    "object": f.object.map(|o| o.num),
                })
            })
            .collect();
        Ok(json!({
            "part": part.label(),
            "conforms": r.conforms(),
            "declared_part": r.declared,
            "failed_checkpoints": r.failed(),
            "counts": counts,
            "findings": findings,
            "findings_truncated": truncated,
            "checked": r.checked,
            "not_checked": UA_NOT_CHECKED,
        }))
    }

    pub(crate) fn doc_validate_arlington(&self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::pdfa::{ARL_NOT_CHECKED, ArlOptions, ArlSeverity, DEFAULT_ARL_FINDINGS, MAX_ARL_FINDINGS, PdfVersion};
        let version = match a.opt_str("version")? {
            None => None,
            Some(s) => Some(
                PdfVersion::parse(s)
                    .filter(|v| (v.major == 1 && v.minor <= 7) || (v.major == 2 && v.minor == 0))
                    .ok_or_else(|| ToolError::InvalidArgs(format!("version must be a PDF version from 1.0 to 1.7 or 2.0, not {s:?}")))?,
            ),
        };
        let max_findings = match a.opt_int("max_findings")? {
            None => DEFAULT_ARL_FINDINGS,
            Some(n) => usize::try_from(n)
                .ok()
                .filter(|n| (1..=MAX_ARL_FINDINGS).contains(n))
                .ok_or_else(|| ToolError::InvalidArgs(format!("max_findings must be 1 to {MAX_ARL_FINDINGS}, not {n}")))?,
        };
        let doc = self.doc(a)?;
        let r = doc
            .validate_arlington(&ArlOptions { version, max_findings })
            .ok_or_else(|| failed("the document can't be read for checking"))?
            .map_err(|m| failed(format!("validation failed unexpectedly ({m})")))?;
        let findings: Vec<Value> = r
            .findings
            .iter()
            .map(|f| {
                json!({
                    "severity": f.severity.id(), "code": f.code, "object": f.object, "path": f.path, "key": f.key,
                    "object_type": f.object_type, "message": f.message,
                })
            })
            .collect();
        let kept = |s: ArlSeverity| r.findings.iter().filter(|f| f.severity == s).count();
        Ok(json!({
            "valid": r.valid(),
            "pdf_version": r.version.to_string(),
            "version_source": r.version_source,
            "errors": r.errors,
            "warnings": r.warnings,
            "infos": r.infos,
            "findings": findings,
            "kept": { "errors": kept(ArlSeverity::Error), "warnings": kept(ArlSeverity::Warning), "infos": kept(ArlSeverity::Info) },
            "truncated": r.truncated,
            "objects_checked": r.objects_checked,
            "indirect_objects": r.indirect_objects,
            "types": r.types,
            "limit_reached": r.limit_reached,
            "predicates_evaluated": pdfcraft_engine::pdfa::arlington::EVALUATED_PREDICATES,
            "not_evaluated": r.not_checked,
            "not_checked": ARL_NOT_CHECKED,
            "model": { "source": pdfcraft_engine::pdfa::arlington::MODEL_SOURCE, "commit": pdfcraft_engine::pdfa::arlington::MODEL_COMMIT },
            "unsaved_changes_included": doc.dirty,
        }))
    }

    pub(crate) fn pdfx_verify(&self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::pdfa::{MAX_X_FINDINGS, XOptions, XPart};
        let part = match a.opt_str("standard")? {
            None => None,
            Some(s) => Some(XPart::from_id(s).ok_or_else(|| {
                let ids: Vec<&str> = XPart::ALL.iter().map(|p| p.id()).collect();
                ToolError::InvalidArgs(format!("unknown PDF/X standard {s:?} ({})", ids.join(", ")))
            })?),
        };
        let max_findings = match a.opt_int("max_findings")? {
            None => 100,
            Some(n) => usize::try_from(n)
                .ok()
                .filter(|n| (1..=MAX_X_FINDINGS).contains(n))
                .ok_or_else(|| ToolError::InvalidArgs(format!("max_findings must be 1 to {MAX_X_FINDINGS}, not {n}")))?,
        };
        let r = self.doc(a)?.pdfx_verify(&XOptions { part, max_findings }).ok_or_else(|| failed("the document can't be read for checking"))?;
        Ok(Self::pdfx_report_json(&r))
    }

    /// A PDF/X verification as `pdfx_verify` returns it.
    pub(crate) fn pdfx_report_json(r: &pdfcraft_engine::pdfa::XReport) -> Value {
        use pdfcraft_engine::pdfa::{X_NOT_CHECKED, X_RULES};
        let counts: serde_json::Map<String, Value> = r.checked.iter().map(|id| ((*id).to_owned(), json!(r.count(id)))).collect();
        let kept = |id: &str| r.findings.iter().filter(|f| f.rule == id).count();
        let truncated = r.counts.iter().any(|(id, n)| *n > kept(id));
        let findings: Vec<Value> = r
            .findings
            .iter()
            .map(|f| json!({ "rule": f.rule, "severity": f.severity.id(), "message": f.message, "page": f.page.map(|p| p + 1), "object": f.object.map(|o| o.num) }))
            .collect();
        let rules: Vec<Value> = X_RULES
            .iter()
            .filter(|x| x.applies(r.part))
            .map(|x| json!({ "rule": x.id, "severity": x.severity(r.part).id(), "description": x.description }))
            .collect();
        let errors = r.findings.iter().filter(|f| f.severity == pdfcraft_engine::pdfa::XSeverity::Error).count();
        json!({
            "standard": r.part.label(),
            "conforms": r.conforms(),
            "declared": r.declared,
            "declared_standard": r.declared_part.map(|p| p.label()),
            "failed_rules": r.failed(),
            "errors": errors,
            "warnings": r.findings.len().saturating_sub(errors),
            "counts": counts,
            "findings": findings,
            "findings_truncated": truncated,
            "checked": rules,
            "not_checked": X_NOT_CHECKED,
        })
    }

    /// Preflight ▸ Convert to PDF/X.
    pub(crate) fn pdfx_convert(&mut self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::pdfa::{FlattenPreset, FlattenSettings, XConvertOptions, XIntent, XPart};
        let bad = |m: String| ToolError::InvalidArgs(m);
        let level = a.str("level")?;
        let part = XPart::from_level(level).ok_or_else(|| bad(format!("unknown level {level:?} (x1a, x3, x4, or X-1a:2001, X-3:2002)")))?;
        let profile = match a.opt_str("profile")? {
            Some(p) => {
                let path = self.resolve(p, false)?;
                let max = pdfcraft_engine::pdfa::MAX_PROFILE_BYTES;
                let size = std::fs::metadata(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?.len();
                if size > max as u64 {
                    return Err(bad(format!("profile is larger than {} MB", max >> 20)));
                }
                Some(std::sync::Arc::new(std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?))
            }
            None => None,
        };
        let condition = a.opt_str("output_condition")?.map(str::to_owned);
        let info = a.opt_str("output_info")?.map(str::to_owned);
        let id = self.doc(a)?.id;
        let mut from_settings = None;
        let intent = match (condition, profile) {
            (Some(condition), profile) => Some(XIntent { condition, info, profile }),
            (None, Some(profile)) => Some(XIntent { condition: "Custom".into(), info, profile: Some(profile) }),
            // No intent given and none in the document: the working CMYK space, when it is an
            // output profile (Preferences ▸ Color Management).
            (None, None) => match self.session.color_management().working_output_profile() {
                Some((profile, description)) if !self.session.has_pdfx_output_intent(id).map_err(failed)? => {
                    from_settings = Some(description.clone());
                    Some(XIntent { condition: "Custom".into(), info: info.or(Some(description)), profile: Some(profile) })
                }
                _ => None,
            },
        };
        let mut flatten = FlattenSettings::default();
        if let Some(p) = a.opt_str("flatten_preset")? {
            flatten =
                FlattenSettings::preset(FlattenPreset::from_id(p).ok_or_else(|| bad(format!("unknown flatten_preset {p:?} (low, medium, high)")))?);
        }
        let options = XConvertOptions {
            part,
            intent,
            trim: a.nums::<4>("trim")?,
            bleed: a.nums::<4>("bleed")?,
            title: a.opt_str("title")?.map(str::to_owned),
            flatten,
            allow_partial: a.opt_bool("allow_partial")?.unwrap_or(false),
        };
        let r = self.session.pdfx_convert(id, options).map_err(|e| match e {
            pdfcraft_engine::EditError::Invalid(m) => ToolError::InvalidArgs(m),
            e => failed(e),
        })?;
        let brief = |x: &pdfcraft_engine::pdfa::XReport| {
            let full = Self::pdfx_report_json(x);
            json!({
                "conforms": full["conforms"],
                "failed_rules": full["failed_rules"],
                "errors": full["errors"],
                "warnings": full["warnings"],
                "findings": full["findings"],
            })
        };
        let unfixable: Vec<Value> = r
            .unfixable()
            .iter()
            .map(|f| json!({ "rule": f.rule, "message": f.message, "page": f.page.map(|p| p + 1), "object": f.object.map(|o| o.num) }))
            .collect();
        let mut out = crate::summary(self.doc(a)?);
        out["standard"] = json!(r.part.label());
        out["converted"] = json!(r.converted);
        out["conforms"] = json!(r.after.conforms());
        out["changed"] = json!(r.converted && !r.fixed.is_empty());
        out["fixed"] = json!(r.fixed);
        out["notes"] = json!(r.notes);
        out["output_intent_from_settings"] = json!(from_settings);
        out["unfixable"] = json!(unfixable);
        out["before"] = brief(&r.before);
        out["after"] = brief(&r.after);
        if !r.converted {
            out["refused"] = json!(format!(
                "Can't reach {} automatically: nothing was changed. Fix what unfixable lists, or pass allow_partial to convert anyway.",
                r.part.label()
            ));
        }
        Ok(out)
    }

    pub(crate) fn pdfa_convert(&mut self, a: &Args) -> Result<Value> {
        let (level, source) = self.pdfa_level(a)?;
        let doc = self.doc(a)?;
        let id = doc.id;
        let blocking = doc.pdfa_blockers(level);
        if !blocking.is_empty() {
            let hint = if level.needs_structure() && blocking.iter().any(|i| !i.clause.starts_with("6.2.11") && !i.clause.starts_with("6.3.8")) {
                "Tag the document first (structure tree, tagged content, Lang, alternate text), or choose level u or b."
            } else {
                "Some glyphs' Unicode meaning can't be derived from their fonts: fix or replace those fonts, or choose level b."
            };
            return Ok(json!({
                "level": level.label(),
                "level_id": level.id(),
                "level_source": source,
                "converted": false,
                "blocking": blocking.iter().map(Self::pdfa_issue_json).collect::<Vec<_>>(),
                "hint": hint,
            }));
        }
        self.session.apply(id, pdfcraft_engine::Edit::ConvertPdfA { level }).map_err(failed)?;
        let mut r = self.pdfa_verify_at(a, level, source)?;
        if let Some(o) = r.as_object_mut() {
            o.insert("converted".into(), json!(true));
        }
        Ok(r)
    }

    pub(crate) fn action_list(&self) -> Result<Value> {
        use pdfcraft_engine::actions::{Step, builtin};
        let step_json = |s: &Step| json!({ "step": s.id(), "arg": s.arg() });
        let actions: Vec<Value> = builtin()
            .iter()
            .map(|a| json!({ "name": a.name, "description": a.description, "steps": a.steps.iter().map(step_json).collect::<Vec<_>>() }))
            .collect();
        let steps: Vec<Value> = Step::all().iter().map(|s| json!({ "step": s.id(), "label": s.label(), "takes_arg": s.arg().is_some() })).collect();
        Ok(json!({ "actions": actions, "steps": steps }))
    }

    pub(crate) fn action_run(&mut self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::actions::{Action, Step, builtin, run_on};
        let action = if let Some(name) = a.opt_str("action")? {
            builtin()
                .into_iter()
                .find(|x| x.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| ToolError::InvalidArgs(format!("no built-in action {name:?}")))?
        } else {
            let items = a.get("steps").and_then(Value::as_array).ok_or_else(|| ToolError::InvalidArgs("give action or steps".into()))?;
            let steps = items
                .iter()
                .map(|it| {
                    let id = it["step"].as_str().unwrap_or("");
                    Step::from_id(id, it["arg"].as_str().unwrap_or("")).ok_or_else(|| ToolError::InvalidArgs(format!("unknown step {id:?}")))
                })
                .collect::<Result<Vec<_>>>()?;
            Action { name: "Custom".into(), description: String::new(), steps, builtin: false }
        };
        let folder = self.resolve(a.str("folder")?, true)?;
        std::fs::create_dir_all(&folder).map_err(|e| failed(e.to_string()))?;
        let mut out = Vec::new();
        for p in a.strs("paths")? {
            let name = std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document.pdf".into());
            let result = self
                .resolve(p, false)
                .map_err(|e| e.to_string())
                .and_then(|src| std::fs::read(&src).map_err(|e| e.to_string()))
                .and_then(|bytes| run_on(&action, &name, std::sync::Arc::new(bytes), |_, _| {}));
            out.push(match result {
                Ok(r) => {
                    let target = child(&folder, &name);
                    write_atomic(&target, &r.bytes)?;
                    json!({ "path": p, "output": target.to_string_lossy(), "log": r.log })
                }
                Err(e) => json!({ "path": p, "error": e }),
            });
        }
        Ok(json!({ "action": action.name, "files": out }))
    }

    pub(crate) fn compare_ids(&self, a: &Args) -> Result<(pdfcraft_engine::DocId, pdfcraft_engine::DocId)> {
        let new = self.doc(a)?.id;
        let other = a.int("other")?;
        let old = u64::try_from(other)
            .ok()
            .and_then(|o| self.session.get(pdfcraft_engine::DocId(o)))
            .map(|d| d.id)
            .ok_or_else(|| ToolError::InvalidArgs(format!("no open document {other}")))?;
        Ok((old, new))
    }

    pub(crate) fn doc_compare_report(&self, a: &Args) -> Result<Value> {
        let (old, new) = self.compare_ids(a)?;
        let bytes = self.session.compare_report(old, new).map_err(failed)?;
        let path = self.resolve(a.str("path")?, true)?;
        write_atomic(&path, &bytes)?;
        Ok(json!({ "path": path.to_string_lossy(), "bytes": bytes.len() }))
    }

    pub(crate) fn doc_compare_mark(&mut self, a: &Args) -> Result<Value> {
        let (old, new) = self.compare_ids(a)?;
        let n = self.session.mark_differences(old, new).map_err(failed)?;
        Ok(json!({ "comments": n }))
    }

    pub(crate) fn form_detect_fields(&mut self, a: &Args) -> Result<Value> {
        let pages = if a.opt_ints("pages")?.is_some() { self.pages(a, "pages")? } else { Vec::new() };
        let doc = self.doc(a)?;
        let (id, copy) = (doc.id, doc.allows_copying());
        // Without copy permission the engine names fields generically (Text1, Check Box1, …)
        // instead of from their labels' words.
        let found = self.session.detect_fields(id, &pages);
        let list: Vec<Value> = found
            .iter()
            .map(|(p, c)| {
                let kind = match c.kind {
                    pdfcraft_engine::detect::Kind::Text => "text",
                    pdfcraft_engine::detect::Kind::CheckBox => "checkbox",
                };
                json!({ "page": p + 1, "kind": kind, "name": c.name, "rect": c.rect.map(|v| (v * 100.0).round() / 100.0) })
            })
            .collect();
        if a.opt_bool("add")?.unwrap_or(true) && !found.is_empty() {
            self.session.auto_detect_fields(id, &pages).map_err(failed)?;
        }
        let mut out = json!({ "fields": list });
        if !copy {
            out["names_withheld"] =
                json!("this document does not allow copying text, so fields are not named from their labels; open it with the permissions password");
        }
        Ok(out)
    }

    pub(crate) fn form_actions(&self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::FieldAction as A;
        let list: Vec<Value> = self
            .doc(a)?
            .field_actions(a.str("field")?)
            .iter()
            .map(|(t, act)| {
                let mut v = match act {
                    A::JavaScript(j) => json!({ "javascript": j }),
                    A::Uri(u) => json!({ "url": u }),
                    A::Reset(f) => json!({ "reset": f }),
                    A::Named(n) => json!({ "menu": n }),
                    A::GoTo(p) => json!({ "page": p + 1 }),
                    A::ShowHide { fields, hide: true } => json!({ "hide": fields }),
                    A::ShowHide { fields, hide: false } => json!({ "show": fields }),
                    A::Submit(u) => json!({ "submit": u }),
                    A::Other(s) => json!({ "other": s }),
                };
                v["trigger"] = json!(t.id());
                v
            })
            .collect();
        Ok(json!({ "field": a.str("field")?, "actions": list }))
    }

    pub(crate) fn form_set_actions(&mut self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::{FieldAction as A, FieldTrigger as T};
        let items = a.get("actions").and_then(Value::as_array).ok_or_else(|| ToolError::InvalidArgs("actions must be an array".into()))?;
        let bad = |m: String| ToolError::InvalidArgs(m);
        let names = |v: &Value| -> Vec<String> {
            v.as_array().map(|x| x.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default()
        };
        let mut acts = Vec::new();
        for it in items {
            let tid = it["trigger"].as_str().unwrap_or("");
            let t = T::from_id(tid).ok_or_else(|| bad(format!("unknown trigger {tid:?}")))?;
            let act = if let Some(j) = it["javascript"].as_str() {
                A::JavaScript(j.into())
            } else if let Some(u) = it["url"].as_str() {
                A::Uri(u.into())
            } else if it.get("reset").is_some() {
                A::Reset(names(&it["reset"]))
            } else if let Some(m) = it["menu"].as_str() {
                A::Named(m.into())
            } else if let Some(p) = it["page"].as_u64() {
                A::GoTo((p.max(1) - 1) as usize)
            } else if it.get("show").is_some() {
                A::ShowHide { fields: names(&it["show"]), hide: false }
            } else if it.get("hide").is_some() {
                A::ShowHide { fields: names(&it["hide"]), hide: true }
            } else if let Some(u) = it["submit"].as_str() {
                A::Submit(u.into())
            } else if let Some(k) = it["other"].as_str() {
                A::Other(k.into())
            } else {
                return Err(bad(format!("{tid}: give one action (javascript, url, reset, menu, page, show, hide, submit or other)")));
            };
            acts.push((t, act));
        }
        let name = a.str("field")?.to_string();
        let edit =
            pdfcraft_engine::Edit::SetFieldProps { name, props: Box::new(pdfcraft_engine::FieldProps { actions: Some(acts), ..Default::default() }) };
        let id = self.doc(a)?.id;
        self.session.apply(id, edit).map_err(failed)?;
        self.form_actions(a)
    }

    pub(crate) fn form_run_trigger(&mut self, a: &Args) -> Result<Value> {
        let tid = a.str("trigger")?;
        let trigger = pdfcraft_engine::FieldTrigger::from_id(tid).ok_or_else(|| ToolError::InvalidArgs(format!("unknown trigger {tid:?}")))?;
        let id = self.doc(a)?.id;
        let o = self.session.run_field_trigger(id, a.str("field")?, trigger).map_err(failed)?;
        let reqs: Vec<Value> = o.requests.iter().map(|r| request_json(&self.session, r)).collect();
        Ok(json!({
            "field": a.str("field")?, "trigger": tid, "ran": o.ran, "skipped": o.skipped, "alerts": o.alerts,
            "console": o.console, "errors": o.errors, "requests": reqs, "menu": o.menu,
        }))
    }

    pub(crate) fn js_enabled(&mut self, a: &Args) -> Result<Value> {
        if let Some(on) = a.opt_bool("enabled")? {
            self.session.set_javascript(on);
        }
        Ok(json!({ "enabled": self.session.javascript() }))
    }
}

/// Background-removal options from tool arguments: `strength` off/low/medium/high (default
/// off) and an optional integer `white_point` (128–255).
fn background_options(a: &Args, strength: &str, white_point: &str) -> Result<pdfcraft_engine::ocr::BackgroundOptions> {
    use pdfcraft_engine::ocr::{BackgroundOptions, BackgroundStrength};
    let min = pdfcraft_engine::optimize::background::MIN_WHITE_POINT;
    let strength = match a.opt_str(strength)? {
        None => BackgroundStrength::Off,
        Some(name) => BackgroundStrength::parse(name)
            .ok_or_else(|| ToolError::InvalidArgs(format!("unknown {strength} {name:?}; use {}", BackgroundStrength::NAMES.join(", "))))?,
    };
    let white_point = match a.opt_int(white_point)? {
        None => None,
        Some(v) => Some(
            u8::try_from(v)
                .ok()
                .filter(|w| *w >= min)
                .ok_or_else(|| ToolError::InvalidArgs(format!("{white_point} must be an integer from {min} to 255, got {v}")))?,
        ),
    };
    Ok(BackgroundOptions { strength, white_point })
}

/// Despeckle options from tool arguments: `level` off/low/medium/high (default off), and the
/// booleans `light` (also fill light holes) and `median` (impulse median filter).
fn despeckle_options(a: &Args, level: &str, light: &str, median: &str) -> Result<pdfcraft_engine::ocr::DespeckleOptions> {
    use pdfcraft_engine::ocr::{DespeckleLevel, DespeckleOptions};
    let level = match a.opt_str(level)? {
        None => DespeckleLevel::Off,
        Some(name) => DespeckleLevel::parse(name)
            .ok_or_else(|| ToolError::InvalidArgs(format!("unknown {level} {name:?}; use {}", DespeckleLevel::NAMES.join(", "))))?,
    };
    Ok(DespeckleOptions { level, light: a.opt_bool(light)?.unwrap_or(false), median: a.opt_bool(median)?.unwrap_or(false) })
}
