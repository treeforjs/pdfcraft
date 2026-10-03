//! Accessibility tools: the full check, the accessibility report, and the automatic fixes.

use printcraft_engine::a11y::{Category, Options, Report, Rule, Status};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed, write_atomic};

fn status_id(s: Status) -> &'static str {
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
        let html = printcraft_engine::a11y::report_html(&r, &doc.name, &format!("{y}-{m:02}-{d:02}"));
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
            printcraft_engine::Edit::MarkDecorative { figure }
        } else {
            printcraft_engine::Edit::SetAltText { figure, alt: a.opt_str("alt")?.map(str::to_owned) }
        };
        let id = self.doc(a)?.id;
        self.session.apply(id, edit).map_err(failed)?;
        self.accessibility_figures(a)
    }
}

impl Automation {
    fn ocr_settings(&self, a: &Args) -> Result<printcraft_engine::ocr::OcrSettings> {
        let mut settings = printcraft_engine::ocr::OcrSettings::default();
        if let Some(d) = a.opt_num("dpi")? {
            settings.dpi = d.clamp(72.0, 600.0) as f32;
        }
        if let Some(l) = a.opt_str("language")? {
            if !printcraft_engine::ocr::LANGUAGES.iter().any(|x| x.0 == l) {
                return Err(ToolError::InvalidArgs(format!("unsupported language {l:?}")));
            }
            settings.language = l.into();
        }
        if let Some(s) = a.opt_bool("skip_text_pages")? {
            settings.skip_text_pages = s;
        }
        Ok(settings)
    }

    pub(crate) fn ocr_recognize_files(&mut self, a: &Args) -> Result<Value> {
        let settings = self.ocr_settings(a)?;
        let folder = self.resolve(a.str("folder")?, true)?;
        std::fs::create_dir_all(&folder).map_err(|e| failed(e.to_string()))?;
        let ocr = printcraft_engine::ocr::engine().map_err(failed)?;
        let mut out = Vec::new();
        for p in a.strs("paths")? {
            let name = std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document.pdf".into());
            let result =
                self.resolve(p, false).map_err(|e| e.to_string()).and_then(|src| std::fs::read(&src).map_err(|e| e.to_string())).and_then(|bytes| {
                    printcraft_engine::ocr::recognize_file(&name, std::sync::Arc::new(bytes), None, settings.clone(), &ocr, |_, _| true)
                });
            out.push(match result {
                Ok(r) => {
                    let target = folder.join(&name);
                    write_atomic(&target, &r.bytes)?;
                    let skipped: Vec<usize> = r.pages.iter().filter(|p| p.skipped.is_some()).map(|p| p.page + 1).collect();
                    json!({ "path": p, "output": target.to_string_lossy(), "words": r.words(), "skipped_pages": skipped })
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
                None => json!({ "page": p.page + 1, "words": p.words.len(), "text": p.text() }),
            })
            .collect();
        Ok(json!({ "words": found.iter().map(|p| p.words.len()).sum::<usize>(), "pages": list }))
    }

    pub(crate) fn ocr_status(&self) -> Result<Value> {
        use printcraft_engine::ocr;
        let dirs: Vec<String> = ocr::Models::search_dirs().iter().map(|d| d.to_string_lossy().into_owned()).collect();
        let langs: Vec<Value> = ocr::LANGUAGES.iter().map(|(c, n)| json!({ "code": c, "name": n })).collect();
        Ok(json!({ "available": ocr::available(), "search_dirs": dirs, "languages": langs }))
    }
}

fn request_json(r: &printcraft_engine::js::Request) -> Value {
    use printcraft_engine::js::Request as R;
    match r {
        R::Reset(n) => json!({ "reset": n }),
        R::Print => json!({ "print": true }),
        R::GoToPage(p) => json!({ "page": p + 1 }),
        R::LaunchUrl(u) => json!({ "url": u }),
        R::Submit(u) => json!({ "submit": u }),
        R::Focus(f) => json!({ "focus": f }),
        R::Beep => json!({ "beep": true }),
    }
}

impl Automation {
    pub(crate) fn js_run(&mut self, a: &Args) -> Result<Value> {
        let id = self.doc(a)?.id;
        let o = self.session.run_javascript(id, a.str("script")?, a.opt_str("field")?).map_err(failed)?;
        let reqs: Vec<Value> = o.requests.iter().map(request_json).collect();
        Ok(json!({ "alerts": o.alerts, "console": o.console, "requests": reqs, "error": o.error, "result": o.result }))
    }

    pub(crate) fn js_document_scripts(&self, a: &Args) -> Result<Value> {
        let list: Vec<Value> = self.doc(a)?.document_scripts().into_iter().map(|(n, s)| json!({ "name": n, "script": s })).collect();
        Ok(json!({ "scripts": list }))
    }

    pub(crate) fn js_set_document_script(&mut self, a: &Args) -> Result<Value> {
        let edit = printcraft_engine::Edit::SetDocumentScript { name: a.str("name")?.into(), script: a.opt_str("script")?.map(str::to_string) };
        let id = self.doc(a)?.id;
        self.session.apply(id, edit).map_err(failed)?;
        self.js_document_scripts(a)
    }

    pub(crate) fn form_set_script(&mut self, a: &Args) -> Result<Value> {
        let edit = printcraft_engine::Edit::SetFieldScript {
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
        let csv = printcraft_engine::merge_data_files(&files).map_err(failed)?;
        let target = self.resolve(a.str("path")?, true)?;
        write_atomic(&target, csv.as_bytes())?;
        let columns = csv.lines().next().map_or(0, |h| h.split(',').count());
        Ok(json!({ "path": target.to_string_lossy(), "rows": files.len(), "columns": columns }))
    }

    pub(crate) fn js_enabled(&mut self, a: &Args) -> Result<Value> {
        if let Some(on) = a.opt_bool("enabled")? {
            self.session.set_javascript(on);
        }
        Ok(json!({ "enabled": self.session.javascript() }))
    }
}
