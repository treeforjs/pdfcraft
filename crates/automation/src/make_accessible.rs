//! `accessibility_make_accessible`: Accessibility ▸ Make Accessible, Acrobat's guided action, headless.
//! It drives the pieces the other accessibility tools use: the document language and title
//! (with DisplayDocTitle), OCR of image-only pages, tooltips for form fields, Autotag Document
//! (form fields included), alternate text for figures, tab order by structure, and the Full Check with PDF/UA
//! before and after. Every step is applied (and checked) on its own, so one that fails is
//! reported while the later ones still run; the steps that changed the document are then merged
//! into one undo step.

use std::collections::BTreeMap;

use pdfcraft_engine::a11y::{AutotagOptions, Options, Rule, Status};
use pdfcraft_engine::{DocId, Edit, FieldProps, TabOrder};
use serde_json::{Map, Value, json};

use crate::a11y::status_id;
use crate::autotag::{summary, valid_lang};
use crate::{Args, Automation, Result, ToolError, failed};

/// The undo label of the whole action.
const LABEL: &str = "Make accessible";
/// Entries one `alt_text` or `field_tooltips` map may hold.
const MAX_ENTRIES: usize = 10_000;
/// The longest title, alternate text or tooltip accepted (characters).
const MAX_TEXT_CHARS: usize = 10_000;
/// Items listed per kind under `needs_review` (the counts are always complete).
const MAX_LISTED: usize = 1_000;

#[derive(Clone, Copy, PartialEq)]
enum Ocr {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, PartialEq)]
enum Autotag {
    Auto,
    Replace,
    Never,
}

/// The validated arguments.
struct Plan {
    id: DocId,
    title: Option<String>,
    lang: Option<String>,
    ocr: Ocr,
    autotag: Autotag,
    alt_text: BTreeMap<u32, String>,
    field_tooltips: BTreeMap<String, String>,
}

fn invalid(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// A non-blank text of at most [`MAX_TEXT_CHARS`] characters, trimmed.
fn text(what: &str, v: &Value) -> Result<String> {
    let s = v.as_str().map(str::trim).ok_or_else(|| invalid(format!("{what} must be a string")))?;
    if s.is_empty() {
        return Err(invalid(format!("{what} must not be blank")));
    }
    if s.chars().count() > MAX_TEXT_CHARS {
        return Err(invalid(format!("{what} is longer than {MAX_TEXT_CHARS} characters")));
    }
    Ok(s.to_owned())
}

/// A JSON object argument with at most [`MAX_ENTRIES`] entries.
fn map_arg<'v>(a: &'v Args, key: &str, what: &str) -> Result<Option<&'v Map<String, Value>>> {
    let Some(v) = a.get(key) else { return Ok(None) };
    let m = v.as_object().ok_or_else(|| invalid(format!("{key} must be an object mapping {what}")))?;
    if m.len() > MAX_ENTRIES {
        return Err(invalid(format!("{key} has {} entries; at most {MAX_ENTRIES} are allowed", m.len())));
    }
    Ok(Some(m))
}

fn step(name: &str, status: &str, reason: impl Into<String>) -> Value {
    json!({ "step": name, "status": status, "reason": reason.into() })
}

fn done(name: &str, reason: impl Into<String>) -> Value {
    step(name, "done", reason)
}

fn skipped(name: &str, reason: impl Into<String>) -> Value {
    step(name, "skipped", reason)
}

fn failure(name: &str, reason: impl Into<String>) -> Value {
    step(name, "failed", reason)
}

/// 1-based page numbers, for messages.
fn page_list(pages: &[usize]) -> String {
    let shown: Vec<String> = pages.iter().take(20).map(|p| p.saturating_add(1).to_string()).collect();
    if pages.len() > shown.len() { format!("{}, …", shown.join(", ")) } else { shown.join(", ") }
}

/// The before/after summary: the Full Check's counts and failed rules, and PDF/UA.
fn report(doc: &pdfcraft_engine::Document) -> Value {
    let mut out = summary(doc);
    if let Some(r) = doc.accessibility_check(&Options::default()) {
        out["checker"]["passed"] = json!(r.count(Status::Passed));
        out["checker"]["failed_count"] = json!(r.count(Status::Failed));
        out["checker"]["manual"] = json!(r.count(Status::Manual));
        out["checker"]["manual_rules"] = json!(r.results.iter().filter(|x| x.status == Status::Manual).map(|x| x.rule.id()).collect::<Vec<_>>());
    }
    out
}

impl Automation {
    fn make_accessible_plan(&self, a: &Args) -> Result<Plan> {
        let title = a.get("title").map(|v| text("title", v)).transpose()?;
        let lang = match a.get("lang") {
            None => None,
            Some(v) => {
                let l = v.as_str().map(str::trim).ok_or_else(|| invalid("lang must be a language tag such as en-US"))?;
                if !valid_lang(l) {
                    return Err(invalid(format!("lang must be a language tag such as en-US, not {l:?}")));
                }
                Some(l.to_owned())
            }
        };
        let ocr = match a.opt_str("ocr")? {
            None | Some("auto") => Ocr::Auto,
            Some("always") => Ocr::Always,
            Some("never") => Ocr::Never,
            Some(o) => return Err(invalid(format!("ocr must be auto, always or never, not {o:?}"))),
        };
        let autotag = match a.opt_str("autotag")? {
            None | Some("auto") => Autotag::Auto,
            Some("replace") => Autotag::Replace,
            Some("never") => Autotag::Never,
            Some(o) => return Err(invalid(format!("autotag must be auto, replace or never, not {o:?}"))),
        };
        let mut alt_text = BTreeMap::new();
        for (k, v) in map_arg(a, "alt_text", "figure object numbers to alternate text")?.into_iter().flatten() {
            let figure = k
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| invalid(format!("alt_text keys must be figure object numbers (as from figures_without_alt), not {k:?}")))?;
            alt_text.insert(figure, text(&format!("alt_text[{k:?}]"), v)?);
        }
        let doc = self.doc(a)?;
        if !doc.editable() {
            return Err(failed(format!(
                "this document can't be edited: {}",
                doc.read_only_reason.clone().unwrap_or_else(|| "it couldn't be read for editing".into())
            )));
        }
        let mut field_tooltips = BTreeMap::new();
        for (k, v) in map_arg(a, "field_tooltips", "field names to tooltips")?.into_iter().flatten() {
            if !doc.info.fields.iter().any(|f| f.name == *k) {
                let known: Vec<&str> = doc.info.fields.iter().take(20).map(|f| f.name.as_str()).collect();
                return Err(invalid(format!("field_tooltips: the document has no field {k:?} (its fields include: {})", known.join(", "))));
            }
            field_tooltips.insert(k.clone(), text(&format!("field_tooltips[{k:?}]"), v)?);
        }
        let existing = doc.tags().lang.filter(|l| !l.trim().is_empty());
        if lang.is_none() && existing.is_none() {
            return Err(invalid(
                "the document has no language: give lang (a language tag such as en-US); it is not guessed, because a wrong language misleads screen readers",
            ));
        }
        Ok(Plan { id: doc.id, title, lang, ocr, autotag, alt_text, field_tooltips })
    }

    /// Accessibility ▸ Make Accessible (`accessibility_make_accessible`).
    pub(crate) fn a11y_make_accessible(&mut self, a: &Args) -> Result<Value> {
        let plan = self.make_accessible_plan(a)?;
        let id = plan.id;
        let doc = self.doc(a)?;
        let before = report(doc);
        let since = doc.edit_generation();
        let mut steps = Vec::new();

        steps.push(self.ma_language(id, plan.lang.as_deref()));
        steps.push(self.ma_ocr(id, plan.ocr));
        // Tooltips first: autotagging gives each field's Form element its tooltip as alternate text.
        steps.push(self.ma_tooltips(id, &plan.field_tooltips));
        let (autotag_step, figures_found) = self.ma_autotag(id, plan.autotag);
        steps.push(autotag_step);
        steps.push(self.ma_title(id, plan.title.as_deref()));
        steps.push(self.ma_alt_text(id, &plan.alt_text, figures_found));
        steps.push(self.ma_tables(id, plan.autotag));
        steps.push(self.ma_tab_order(id));

        let merged = self.session.group_undo(id, since, LABEL).map_err(failed)?;
        let doc = self.session.get(id).ok_or_else(|| failed("the document was closed"))?;
        let after = report(doc);
        steps.push(match after["checker"]["failed"].as_array() {
            Some(f) if f.is_empty() => done("full_check", "the Full Check found no failed rules; the manual checks still need a person"),
            Some(f) => {
                failure("full_check", format!("{} rules still fail: {}", f.len(), f.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")))
            }
            None => failure("full_check", "the document couldn't be read for checking"),
        });
        Ok(json!({
            "doc": id.0,
            "steps": steps,
            "changed": merged > 0,
            "undo": if merged > 0 { Some(LABEL) } else { None },
            "needs_review": needs_review(doc),
            "before": before,
            "after": after,
        }))
    }

    fn ma_language(&mut self, id: DocId, lang: Option<&str>) -> Value {
        const S: &str = "language";
        let Some(doc) = self.session.get(id) else { return failure(S, "the document is not open") };
        let existing = doc.tags().lang.filter(|l| !l.trim().is_empty());
        let Some(lang) = lang else {
            return skipped(S, format!("the document language is already {}", existing.unwrap_or_default()));
        };
        if existing.as_deref() == Some(lang) {
            return skipped(S, format!("the document language is already {lang}"));
        }
        let edit = match doc.accessibility_fix(Rule::PrimaryLanguage, Some(lang)) {
            Ok(e) => e,
            Err(e) => return failure(S, e),
        };
        match self.session.apply(id, edit) {
            Ok(()) => done(S, format!("set the document language to {lang}")),
            Err(e) => failure(S, e.to_string()),
        }
    }

    /// Pages with no text that draw an image: scans.
    fn image_only_pages(&mut self, id: DocId) -> Result<Vec<usize>> {
        let doc = self.session.get(id).ok_or_else(|| failed("the document is not open"))?;
        let with_images: Vec<usize> = (0..doc.info.pages.len()).filter(|p| !doc.page_images(*p).is_empty()).collect();
        let mut out = Vec::new();
        self.each_page_text(id, &with_images, |pos, t| {
            if t.plain_text().trim().is_empty()
                && let Some(p) = with_images.get(pos)
            {
                out.push(*p);
            }
            Ok(())
        })?;
        out.sort_unstable();
        Ok(out)
    }

    fn ma_ocr(&mut self, id: DocId, mode: Ocr) -> Value {
        const S: &str = "ocr";
        if mode == Ocr::Never {
            return skipped(S, "ocr is never");
        }
        let pages = match self.image_only_pages(id) {
            Ok(p) => p,
            Err(e) => return failure(S, format!("the pages couldn't be read to find scans: {e}")),
        };
        let mut v = if mode == Ocr::Auto && pages.is_empty() {
            skipped(S, "no page is image-only (every page with an image has text)")
        } else if !pdfcraft_engine::ocr::available() {
            let why = if pages.is_empty() {
                "no OCR models are installed (cargo xtask models, or set PDFCRAFT_MODELS)".to_owned()
            } else {
                format!(
                    "{} image-only page(s) ({}) need OCR, but no OCR models are installed (cargo xtask models, or set PDFCRAFT_MODELS)",
                    pages.len(),
                    page_list(&pages)
                )
            };
            if mode == Ocr::Auto { skipped(S, why) } else { failure(S, why) }
        } else {
            // always: every page without text (the recognizer skips the others); auto: the scans.
            let wanted = if mode == Ocr::Always { Vec::new() } else { pages.clone() };
            match self.session.recognize_text(id, &wanted, pdfcraft_engine::ocr::OcrSettings::default()) {
                Ok(found) => {
                    let read: Vec<usize> = found.iter().filter(|p| p.skipped.is_none()).map(|p| p.page).collect();
                    let words: usize = found.iter().map(|p| p.word_count()).sum();
                    let mut v = if read.is_empty() {
                        skipped(S, "no page needed recognizing")
                    } else {
                        done(S, format!("recognized {words} words on page(s) {}", page_list(&read)))
                    };
                    v["pages"] = json!(read.iter().map(|p| p + 1).collect::<Vec<_>>());
                    v["words"] = json!(words);
                    v
                }
                Err(e) => failure(S, e),
            }
        };
        v["image_only_pages"] = json!(pages.iter().map(|p| p + 1).collect::<Vec<_>>());
        v
    }

    /// The autotag step, and whether it made figures.
    fn ma_autotag(&mut self, id: DocId, mode: Autotag) -> (Value, bool) {
        const S: &str = "autotag";
        let Some(doc) = self.session.get(id) else { return (failure(S, "the document is not open"), false) };
        let tagged = doc.tags().exists;
        match mode {
            Autotag::Never => return (skipped(S, "autotag is never"), false),
            Autotag::Auto if tagged => {
                return (skipped(S, "the document is already tagged; its tags are left as they are (autotag: replace tags it again)"), false);
            }
            _ => {}
        }
        let options = AutotagOptions { replace_existing: mode == Autotag::Replace, ..Default::default() };
        let r = match doc.autotag_preview(&options) {
            Ok(r) => r,
            Err(e) => return (failure(S, e), false),
        };
        if let Err(e) = self.session.apply(id, Edit::Autotag { options }) {
            return (failure(S, e.to_string()), false);
        }
        let mut v = done(S, format!("tagged {} page(s){}", r.pages.len(), if r.replaced { ", replacing the old tags" } else { "" }));
        v["pages"] = json!(r.pages.iter().map(|p| p + 1).collect::<Vec<_>>());
        v["skipped_pages"] = json!(r.skipped.iter().map(|(p, why)| json!({ "page": p + 1, "reason": why })).collect::<Vec<_>>());
        v["counts"] = json!(r.counts);
        v["artifacts"] = json!(r.artifacts);
        v["warnings"] = json!(r.warnings);
        v["form_fields"] = json!(r.fields.len());
        v["fields_needing_tooltip"] = json!(
            r.fields_needing_tooltip
                .iter()
                .take(MAX_LISTED)
                .map(|f| json!({ "page": f.page + 1, "name": f.name, "suggested": f.suggested }))
                .collect::<Vec<_>>()
        );
        (v, !r.figures_needing_alt.is_empty())
    }

    fn ma_title(&mut self, id: DocId, title: Option<&str>) -> Value {
        const S: &str = "title";
        let Some(doc) = self.session.get(id) else { return failure(S, "the document is not open") };
        let existing = doc.info_value("Title").map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
        let shown = doc.initial_view().display_title;
        let (title, source) = match (title, existing.clone()) {
            (Some(t), _) => (t.to_owned(), "given"),
            (None, Some(t)) => (t, "existing"),
            (None, None) => match doc.heading_bookmarks(Some(1)).ok().and_then(|h| h.entries.into_iter().find(|e| e.level == 1)) {
                Some(h) => (h.title, "first H1"),
                None => {
                    let name = doc.name.trim();
                    let stem = name.strip_suffix(".pdf").or_else(|| name.strip_suffix(".PDF")).unwrap_or(name);
                    (stem.to_owned(), "file name")
                }
            },
        };
        if title.trim().is_empty() {
            return failure(S, "no title was given and the document has none, no H1 heading and no file name to take one from");
        }
        if existing.as_deref() == Some(title.as_str()) && shown {
            let mut v = skipped(S, "the title is already set and shown in the title bar (DisplayDocTitle)");
            v["title"] = json!(title);
            return v;
        }
        let edit = match doc.accessibility_fix(Rule::Title, Some(&title)) {
            Ok(e) => e,
            Err(e) => return failure(S, e),
        };
        let mut v = match self.session.apply(id, edit) {
            Ok(()) => done(S, format!("set the title from the {source} and DisplayDocTitle")),
            Err(e) => failure(S, e.to_string()),
        };
        v["title"] = json!(title);
        v["source"] = json!(source);
        v
    }

    fn ma_alt_text(&mut self, id: DocId, alt: &BTreeMap<u32, String>, autotagged_figures: bool) -> Value {
        const S: &str = "alt_text";
        if alt.is_empty() {
            let why = if autotagged_figures {
                "no alt_text given: the figures without alternate text are listed in needs_review"
            } else {
                "no alt_text given"
            };
            return skipped(S, why);
        }
        let figures: Vec<u32> = self.session.get(id).map(|d| d.figures().iter().map(|f| f.obj.num).collect()).unwrap_or_default();
        let mut set = Vec::new();
        let mut errors = Vec::new();
        for (figure, text) in alt {
            if !figures.contains(figure) {
                errors.push(json!({ "figure": figure, "error": "no figure element with this object number" }));
                continue;
            }
            match self.session.apply(id, Edit::SetAltText { figure: *figure, alt: Some(text.clone()) }) {
                Ok(()) => set.push(*figure),
                Err(e) => errors.push(json!({ "figure": figure, "error": e.to_string() })),
            }
        }
        let mut v = if errors.is_empty() {
            done(S, format!("set alternate text on {} figure(s)", set.len()))
        } else {
            failure(S, format!("set alternate text on {} of {} figure(s); see errors", set.len(), alt.len()))
        };
        v["set"] = json!(set);
        v["errors"] = json!(errors);
        v
    }

    fn ma_tooltips(&mut self, id: DocId, tips: &BTreeMap<String, String>) -> Value {
        const S: &str = "field_tooltips";
        if tips.is_empty() {
            let any = self.session.get(id).is_some_and(|d| !d.info.fields.is_empty());
            return skipped(
                S,
                if any { "no field_tooltips given: fields without tooltips are listed in needs_review" } else { "the document has no form fields" },
            );
        }
        let mut set = Vec::new();
        let mut errors = Vec::new();
        for (name, tip) in tips {
            let props = FieldProps { tooltip: Some(tip.clone()), ..Default::default() };
            match self.session.apply(id, Edit::SetFieldProps { name: name.clone(), props: Box::new(props) }) {
                Ok(()) => set.push(name.clone()),
                Err(e) => errors.push(json!({ "field": name, "error": e.to_string() })),
            }
        }
        let mut v = if errors.is_empty() {
            done(S, format!("set the tooltip of {} field(s)", set.len()))
        } else {
            failure(S, format!("set the tooltip of {} of {} field(s); see errors", set.len(), tips.len()))
        };
        v["set"] = json!(set);
        v["errors"] = json!(errors);
        v
    }

    /// Detect tables: autotagging finds ruled tables; this step reports them for review.
    fn ma_tables(&self, id: DocId, mode: Autotag) -> Value {
        const S: &str = "tables";
        let Some(doc) = self.session.get(id) else { return failure(S, "the document is not open") };
        let tags = doc.tags();
        let n = tags.nodes.iter().filter(|n| n.standard.as_deref() == Some("Table")).count();
        let mut v = match (n, mode) {
            (0, Autotag::Never) => skipped(S, "autotag is never and the document has no tables tagged"),
            (0, _) => skipped(S, "no table is tagged (autotagging finds ruled tables only)"),
            (n, _) => done(S, format!("{n} table(s) tagged: check their rows, cells and headers (listed in needs_review)")),
        };
        v["tables"] = json!(n);
        v
    }

    fn ma_tab_order(&mut self, id: DocId) -> Value {
        const S: &str = "tab_order";
        let Some(doc) = self.session.get(id) else { return failure(S, "the document is not open") };
        let pages = doc.info.pages.len();
        if pages == 0 {
            return skipped(S, "the document has no pages");
        }
        match self.session.apply(id, Edit::SetTabOrder { pages: (0..pages).collect(), order: TabOrder::Structure }) {
            Ok(()) => done(S, format!("set the tab order of {pages} page(s) to the document structure")),
            Err(e) => failure(S, e.to_string()),
        }
    }
}

/// What still needs a person.
fn needs_review(doc: &pdfcraft_engine::Document) -> Value {
    let targets = doc.alt_targets();
    let figures: Vec<Value> = targets
        .figures
        .iter()
        .filter(|f| f.needs_alt)
        .take(MAX_LISTED)
        .map(|f| json!({ "figure": f.obj.map(|o| o.num), "page": f.page.map(|p| p + 1) }))
        .collect();
    let figure_count = targets.figures.iter().filter(|f| f.needs_alt).count();
    let images: Vec<Value> = targets.images.iter().take(MAX_LISTED).map(|i| json!({ "page": i.page + 1, "image": i.index + 1 })).collect();
    let no_tip = |f: &&pdfcraft_render::Field| f.tooltip.as_deref().is_none_or(|t| t.trim().is_empty());
    let fields: Vec<&str> = doc.info.fields.iter().filter(no_tip).take(MAX_LISTED).map(|f| f.name.as_str()).collect();
    let field_count = doc.info.fields.iter().filter(no_tip).count();
    let tags = doc.tags();
    let tables: Vec<Value> = tags
        .nodes
        .iter()
        .filter(|n| n.standard.as_deref() == Some("Table"))
        .take(MAX_LISTED)
        .map(|n| json!({ "obj": n.obj.map(|o| o.num), "page": n.page.map(|p| p + 1) }))
        .collect();
    let check = doc.accessibility_check(&Options::default());
    let rule = |r: Rule| check.as_ref().and_then(|c| c.result(r)).map(|x| status_id(x.status));
    let manual: Vec<&str> =
        check.as_ref().map(|c| c.results.iter().filter(|x| x.status == Status::Manual).map(|x| x.rule.id()).collect()).unwrap_or_default();
    json!({
        "figures_without_alt": { "count": figure_count, "figures": figures },
        "untagged_images": { "count": targets.images.len(), "images": images },
        "fields_without_tooltips": { "count": field_count, "fields": fields },
        "form_fields_tagged": rule(Rule::TaggedFormFields),
        "tables_to_review": { "count": tags.nodes.iter().filter(|n| n.standard.as_deref() == Some("Table")).count(), "tables": tables },
        "reading_order": {
            "verify": tags.exists,
            "status": rule(Rule::LogicalReadingOrder),
            "note": "check the reading order of every page (Reading Order / Order panel); autotagging orders blocks by rows and columns",
        },
        "manual_checks": manual,
    })
}
