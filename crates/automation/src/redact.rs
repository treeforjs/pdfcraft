//! Redaction tools: mark areas, text, patterns or whole pages for redaction, apply the marks
//! (removing what they cover for good) or clear them.

use pdfcraft_engine::{
    Edit, Hidden, NewAnnotation, OverlayFont, OverlayLook, REDACT_PATTERN_LOCALES, REDACT_PATTERNS, REDACTION_CODE_SETS, RedactPattern,
    RedactionCodeSet, Shape, Style, find_pattern, rect_quad, redact_patterns_for, redaction_codes,
};
use serde_json::{Value, json};

use crate::comments::{DEFAULT_AUTHOR, parse_color};
use crate::{Args, Automation, Result, ToolError, failed};

/// One redaction mark to add: (0-based page, its quadrilaterals).
type Mark = (usize, Vec<[f64; 8]>);

impl Automation {
    pub(crate) fn redact_mark(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let (id, n) = (doc.id, doc.info.pages.len());
        let pages: Vec<usize> = match a.opt_ints("pages")? {
            Some(_) => self.pages(a, "pages")?,
            None => (0..n).collect(),
        };
        let overlay = overlay_text(a)?;
        let look = overlay_look(a)?;
        let author = a.opt_str("author")?.unwrap_or(DEFAULT_AUTHOR).to_string();
        let mut style = Style::default_for(&Shape::Redact { quads: Vec::new(), overlay: String::new(), look: Default::default() });
        if let Some(c) = a.opt_str("fill")? {
            style.fill = Some(parse_color(c)?);
        }
        // (page, quads) per mark.
        let mut marks: Vec<Mark> = Vec::new();
        let chosen = [
            a.get("rect").is_some(),
            a.get("find").is_some(),
            a.get("pattern").is_some(),
            a.get("locale").is_some(),
            a.opt_bool("whole_pages")?.unwrap_or(false),
        ];
        if chosen.iter().filter(|c| **c).count() != 1 {
            return Err(ToolError::InvalidArgs("pass exactly one of rect (with page), find, pattern, locale or whole_pages: true".into()));
        }
        if a.get("rect").is_some() {
            let page = self.page(a)?;
            let info = &self.doc(a)?.info.pages[page];
            let r: Vec<f64> = a.get("rect").and_then(Value::as_array).map(|x| x.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
            let r = <[f64; 4]>::try_from(r).map_err(|_| ToolError::InvalidArgs("rect must be 4 numbers".into()))?;
            let q = info.view_rect_to_quad([r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32]);
            marks.push((page, vec![q]));
        } else if a.opt_bool("whole_pages")?.unwrap_or(false) {
            let info = &self.doc(a)?.info;
            for &p in &pages {
                let c = info.pages[p].crop;
                marks.push((p, vec![rect_quad([c[0] as f64, c[1] as f64, c[2] as f64, c[3] as f64])]));
            }
        } else {
            let find = a.opt_str("find")?.map(str::to_owned);
            let mut patterns: Vec<RedactPattern> = Vec::new();
            if let Some(p) = a.opt_str("pattern")? {
                let ids: Vec<&str> = REDACT_PATTERNS.iter().map(|p| p.id()).collect();
                patterns.push(
                    RedactPattern::from_id(p).ok_or_else(|| ToolError::InvalidArgs(format!("unknown pattern {p:?} (one of {})", ids.join(", "))))?,
                );
            }
            if let Some(l) = a.opt_str("locale")? {
                let ids: Vec<&str> = REDACT_PATTERN_LOCALES.iter().map(|l| l.0).collect();
                patterns =
                    redact_patterns_for(l).ok_or_else(|| ToolError::InvalidArgs(format!("unknown locale {l:?} (one of {})", ids.join(", "))))?;
            }
            let info = self.doc(a)?.info.clone();
            // Pages are streamed (not all held at once) and may arrive out of order: marks are
            // gathered per position in `pages` and kept in that order.
            let mut found: std::collections::BTreeMap<usize, Vec<Mark>> = std::collections::BTreeMap::new();
            // Every pattern's matches, each place once (patterns can overlap).
            let matcher = |chars: &[char]| {
                let mut all: Vec<std::ops::Range<usize>> = patterns.iter().flat_map(|pat| find_pattern(*pat, chars)).collect();
                all.sort_by_key(|r| (r.start, r.end));
                all.dedup();
                all
            };
            // The search index skips pages that cannot match; only the rest need geometry.
            let may_match = |s: &crate::text_cache::SearchText| match &find {
                Some(f) => s.may_find(f),
                None => s.may_match(matcher),
            };
            self.each_matching_page_text(id, &pages, may_match, |pos, text| {
                let p = pages.get(pos).copied().unwrap_or_default();
                let hits = match &find {
                    Some(f) => text.find(f),
                    None => text.find_with(matcher),
                };
                let Some(page) = info.pages.get(p) else { return Ok(()) };
                let mut on_page = Vec::new();
                for h in hits {
                    let quads: Vec<[f64; 8]> = text.line_rects(h).into_iter().map(|r| page.view_rect_to_quad(r)).collect();
                    if !quads.is_empty() {
                        on_page.push((p, quads));
                    }
                }
                if !on_page.is_empty() {
                    found.insert(pos, on_page);
                }
                Ok(())
            })?;
            marks.extend(found.into_values().flatten());
            if marks.is_empty() {
                return Err(failed("nothing matched, so nothing was marked"));
            }
        }
        let count = marks.len();
        let edits: Vec<Edit> = marks
            .iter()
            .map(|(page, quads)| {
                let shape = Shape::Redact { quads: quads.clone(), overlay: overlay.clone(), look };
                Edit::AddAnnotation(NewAnnotation { page: *page, shape, style: style.clone(), contents: String::new(), author: author.clone() })
            })
            .collect();
        let edit = match <[Edit; 1]>::try_from(edits) {
            Ok([one]) => one,
            Err(edits) => Edit::Batch { label: "Mark for redaction".into(), edits },
        };
        let mut out = self.apply(a, edit)?;
        out["marked"] = json!(count);
        out["pages_marked"] = json!(marks.iter().map(|m| m.0 + 1).collect::<std::collections::BTreeSet<_>>());
        out["marks_pending"] = json!(self.doc(a)?.redaction_marks());
        if !overlay.is_empty() {
            out["overlay"] = json!(overlay);
        }
        Ok(out)
    }

    /// The redaction code sets (Redaction Properties ▸ Redaction Code).
    pub(crate) fn redact_codes(&self, a: &Args) -> Result<Value> {
        let sets: Vec<&RedactionCodeSet> = match a.opt_str("code_set")? {
            Some(id) => vec![code_set(id)?],
            None => REDACTION_CODE_SETS.iter().collect(),
        };
        let sets: Vec<Value> = sets
            .into_iter()
            .map(|s| json!({ "id": s.id, "name": s.name, "codes": s.codes.iter().map(|c| json!({ "code": c.code, "description": c.description })).collect::<Vec<_>>() }))
            .collect();
        Ok(json!({ "code_sets": sets }))
    }

    /// The Search & Redact patterns, by locale set.
    pub(crate) fn redact_patterns(&self, a: &Args) -> Result<Value> {
        let locales: Vec<(&str, &str)> = match a.opt_str("locale")? {
            Some(l) => {
                let found = REDACT_PATTERN_LOCALES.iter().find(|(id, _)| id.eq_ignore_ascii_case(l));
                vec![*found.ok_or_else(|| ToolError::InvalidArgs(format!("unknown locale {l:?}")))?]
            }
            None => REDACT_PATTERN_LOCALES.to_vec(),
        };
        let out: Vec<Value> = locales
            .into_iter()
            .map(|(id, name)| {
                let patterns: Vec<Value> =
                    REDACT_PATTERNS.iter().filter(|p| p.locale() == id).map(|p| json!({ "id": p.id(), "label": p.label() })).collect();
                json!({ "locale": id, "name": name, "patterns": patterns })
            })
            .collect();
        Ok(json!({ "locales": out, "note": "a locale set also includes the intl patterns" }))
    }

    pub(crate) fn redact_apply(&mut self, a: &Args) -> Result<Value> {
        let pages = match a.opt_ints("pages")? {
            Some(_) => Some(self.pages(a, "pages")?),
            None => None,
        };
        let before = self.doc(a)?.redaction_marks();
        if before == 0 {
            return Err(failed("there are no redaction marks to apply (see redact_mark)"));
        }
        let mut out = self.apply(a, Edit::ApplyRedactions { pages })?;
        let after = self.doc(a)?.redaction_marks();
        out["applied"] = json!(before - after);
        out["marks_pending"] = json!(after);
        Ok(out)
    }

    pub(crate) fn redact_clear(&mut self, a: &Args) -> Result<Value> {
        if self.doc(a)?.redaction_marks() == 0 {
            return Err(failed("there are no redaction marks"));
        }
        self.apply(a, Edit::ClearRedactions)
    }

    pub(crate) fn doc_hidden_info(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let items: Vec<Value> = doc.hidden_info().into_iter().map(|(h, n)| json!({ "category": h.id(), "label": h.label(), "count": n })).collect();
        let total: usize = doc.hidden_info().iter().map(|c| c.1).sum();
        Ok(json!({ "total": total, "categories": items }))
    }

    pub(crate) fn doc_remove_hidden(&mut self, a: &Args) -> Result<Value> {
        let edit = match a.get("categories") {
            None => Edit::Sanitize,
            Some(_) => {
                let which = a
                    .strs("categories")?
                    .into_iter()
                    .map(|c| Hidden::from_id(c).ok_or_else(|| ToolError::InvalidArgs(format!("unknown category {c:?} (see doc_hidden_info)"))))
                    .collect::<Result<Vec<_>>>()?;
                if which.is_empty() {
                    return Err(ToolError::InvalidArgs("categories is empty".into()));
                }
                Edit::RemoveHidden { which }
            }
        };
        let before: usize = self.doc(a)?.hidden_info().iter().map(|c| c.1).sum();
        let mut out = self.apply(a, edit)?;
        let after: usize = self.doc(a)?.hidden_info().iter().map(|c| c.1).sum();
        out["removed"] = json!(before.saturating_sub(after));
        Ok(out)
    }
}

fn code_set(id: &str) -> Result<&'static RedactionCodeSet> {
    RedactionCodeSet::from_id(id).ok_or_else(|| {
        let ids: Vec<&str> = REDACTION_CODE_SETS.iter().map(|s| s.id).collect();
        ToolError::InvalidArgs(format!("unknown code_set {id:?} (one of {}; see redact_codes)", ids.join(", ")))
    })
}

/// The overlay text of new marks: `overlay` (custom text), or `codes` of `code_set` (a built-in
/// set; without one the codes are a custom set).
fn overlay_text(a: &Args) -> Result<String> {
    let text = a.opt_str("overlay")?;
    let set = a.opt_str("code_set")?.map(code_set).transpose()?;
    let codes = if a.get("codes").is_some() { Some(a.strs("codes")?) } else { None };
    match (text, set, codes) {
        (Some(_), _, Some(_)) | (Some(_), Some(_), _) => {
            Err(ToolError::InvalidArgs("pass overlay (custom text) or codes (with code_set), not both".into()))
        }
        (None, Some(_), None) => Err(ToolError::InvalidArgs("code_set needs codes: the codes to show (see redact_codes)".into())),
        (Some(t), None, None) => {
            if t.chars().count() > redaction_codes::MAX_OVERLAY_CHARS {
                return Err(ToolError::InvalidArgs(format!("overlay is longer than {} characters", redaction_codes::MAX_OVERLAY_CHARS)));
            }
            Ok(t.to_string())
        }
        (None, Some(set), Some(codes)) => redaction_codes::overlay_text(set, &codes).map_err(|e| ToolError::InvalidArgs(e.to_string())),
        (None, None, Some(codes)) => redaction_codes::custom_overlay_text(&codes).map_err(|e| ToolError::InvalidArgs(e.to_string())),
        (None, None, None) => Ok(String::new()),
    }
}

/// How the overlay text is drawn (`/DA`, `/Q`, `/Repeat`).
fn overlay_look(a: &Args) -> Result<OverlayLook> {
    let mut look = OverlayLook::default();
    if let Some(f) = a.opt_str("overlay_font")? {
        look.font = match f.to_ascii_lowercase().as_str() {
            "helvetica" => OverlayFont::Helvetica,
            "times" | "times-roman" => OverlayFont::Times,
            "courier" => OverlayFont::Courier,
            _ => return Err(ToolError::InvalidArgs(format!("unknown overlay_font {f:?} (helvetica, times or courier)"))),
        };
    }
    if let Some(size) = a.opt_num("overlay_size")? {
        if size != 0.0 && !(2.0..=144.0).contains(&size) {
            return Err(ToolError::InvalidArgs("overlay_size must be 0 (auto-size to fit) or 2 to 144 points".into()));
        }
        look.size = size;
    }
    if let Some(c) = a.opt_str("overlay_color")? {
        look.color = parse_color(c)?;
    }
    if let Some(al) = a.opt_str("overlay_align")? {
        look.align = match al {
            "left" => 0,
            "center" | "centre" => 1,
            "right" => 2,
            _ => return Err(ToolError::InvalidArgs(format!("unknown overlay_align {al:?} (left, center or right)"))),
        };
    }
    if let Some(r) = a.opt_bool("overlay_repeat")? {
        look.repeat = r;
    }
    Ok(look)
}
