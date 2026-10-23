//! Compare files: the page map (inserted, deleted, moved, modified pages), the text and visual
//! differences between two open documents, a report, and the differences marked as comments in
//! the newer one or in a marked-up copy of it.

use std::sync::Arc;

pub use pdfcraft_compare::{Change, Comparison, Kind, PageMatch, PageStatus, Side};

use crate::{DocId, Edit, EditError, Markup, NewAnnotation, NoteIcon, Session, Shape};

/// Acrobat's compare colours: replaced blue, inserted green, deleted red.
pub fn colour(kind: Kind) -> crate::Rgb {
    match kind {
        Kind::Replaced => [0.2, 0.45, 0.95],
        Kind::Inserted => [0.2, 0.75, 0.3],
        Kind::Deleted => [0.9, 0.25, 0.25],
    }
}

impl crate::Document {
    /// Standards ▸ Verify PDF/A: the rules the document breaks for `level` (levels a and u
    /// include Unicode mappings; level a the logical structure, language and alternate text).
    pub fn pdfa_verify(&self, level: pdfcraft_preflight::Level) -> Vec<pdfcraft_preflight::Issue> {
        self.editor.as_ref().map(|e| pdfcraft_a11y::verify_pdfa(&e.cos, level)).unwrap_or_default()
    }

    /// What stops Save as PDF/A for `level` before anything changes (glyphs whose meaning
    /// can't be known for levels a and u, missing tags for level a); empty when it can go ahead.
    pub fn pdfa_blockers(&self, level: pdfcraft_preflight::Level) -> Vec<pdfcraft_preflight::Issue> {
        self.editor.as_ref().map(|e| pdfcraft_a11y::pdfa_blockers(&e.cos, level)).unwrap_or_default()
    }

    /// The PDF/A level the document declares in its XMP metadata, when supported here.
    pub fn pdfa_declared_level(&self) -> Option<pdfcraft_preflight::Level> {
        self.editor.as_ref().and_then(|e| pdfcraft_preflight::declared_level(&e.cos))
    }

    /// The standards the document declares (Standards panel).
    pub fn standards(&self) -> pdfcraft_preflight::Declared {
        self.editor.as_ref().map(|e| pdfcraft_preflight::declared(&e.cos)).unwrap_or_default()
    }

    /// Every word of the document in reading order, with page and box.
    pub fn words(&self) -> Vec<pdfcraft_compare::Word> {
        let mut r = renderer(self);
        let mut out = Vec::new();
        for (page, info) in self.info.pages.iter().enumerate() {
            out.extend(page_words(&mut r, page, info));
        }
        out
    }
}

fn page_words(r: &mut pdfcraft_render::PageRenderer, page: usize, info: &pdfcraft_render::PageInfo) -> Vec<pdfcraft_compare::Word> {
    let res = r.render(pdfcraft_render::RenderRequest { page, kind: pdfcraft_render::RequestKind::Text, scale: 1.0, ..Default::default() });
    res.text
        .map(|t| crate::js::page_words(&t, info).into_iter().map(|(text, rect)| pdfcraft_compare::Word { text, page, rect }).collect())
        .unwrap_or_default()
}

/// Compare Files options.
#[derive(Clone, Debug, PartialEq)]
pub struct CompareOptions {
    /// 0-based inclusive page range of the old document to compare (`None` = every page).
    pub old_pages: Option<(usize, usize)>,
    /// The same for the new document.
    pub new_pages: Option<(usize, usize)>,
    /// Also compare how the paired pages look.
    pub visual: bool,
    /// Resolution of the visual compare (clamped to 18–150 dpi).
    pub dpi: f32,
    /// A pixel differs when a colour channel differs by more than this (0–255).
    pub tolerance: u8,
    /// A page looks different when more than this share (0–1) of its pixels differ.
    pub threshold: f64,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self { old_pages: None, new_pages: None, visual: false, dpi: 72.0, tolerance: 24, threshold: 0.0 }
    }
}

/// A pair of pages that looks different.
#[derive(Clone, Debug, PartialEq)]
pub struct PageVisual {
    /// 0-based pages.
    pub old_page: usize,
    pub new_page: usize,
    /// Share of the page's pixels that differ (0–1).
    pub fraction: f64,
    /// Differing regions as user-space boxes on the new page.
    pub regions: Vec<[f64; 4]>,
}

/// Everything Compare Files found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileComparison {
    /// The page map (0-based document pages), in new-document order with deleted pages where
    /// they were.
    pub pages: Vec<PageMatch>,
    /// Word-level text changes; moved pages are compared with their counterpart.
    pub text: Comparison,
    /// Paired pages that look different (over the threshold), when a visual compare was asked.
    pub visual: Vec<PageVisual>,
    /// Paired pages not compared visually (render failure or past [`MAX_VISUAL_PAGES`]).
    pub visual_skipped: usize,
}

impl FileComparison {
    pub fn count(&self, status: PageStatus) -> usize {
        self.pages.iter().filter(|p| p.status == status).count()
    }

    /// No text, page or visual difference.
    pub fn identical(&self) -> bool {
        self.text.identical() && self.visual.is_empty() && self.pages.iter().all(|p| p.status == PageStatus::Same)
    }
}

fn r2(r: &[f64; 4]) -> [f64; 4] {
    r.map(|v| (v * 100.0).round() / 100.0)
}

fn side_json(s: &Side) -> serde_json::Value {
    serde_json::json!({ "text": s.text, "page": s.page + 1, "rects": s.rects.iter().map(r2).collect::<Vec<_>>() })
}

/// The structured Compare Files report (the `compare_files` tool's JSON and the Compare
/// panel's Save JSON report): summary counts, the page map (1-based), at most `limit` text
/// changes and the pages that look different.
pub fn report_json(c: &FileComparison, opts: &CompareOptions, limit: usize) -> serde_json::Value {
    use serde_json::{Value, json};
    let one = |p: Option<usize>| p.map(|p| p + 1);
    let pages: Vec<Value> = c
        .pages
        .iter()
        .map(|p| json!({ "status": p.status.label(), "old": one(p.old), "new": one(p.new), "similarity": (p.similarity * 1000.0).round() / 1000.0 }))
        .collect();
    let changes: Vec<Value> = c
        .text
        .changes
        .iter()
        .take(limit)
        .map(|ch| json!({ "kind": ch.kind.label().to_lowercase(), "old": side_json(&ch.old), "new": side_json(&ch.new) }))
        .collect();
    let visual: Vec<Value> = c
        .visual
        .iter()
        .map(|v| {
            json!({
                "old_page": v.old_page + 1,
                "new_page": v.new_page + 1,
                "changed_percent": (v.fraction * 100_000.0).round() / 1000.0,
                "regions": v.regions.iter().map(r2).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "identical": c.identical(),
        "summary": {
            "pages_inserted": c.count(PageStatus::Inserted),
            "pages_deleted": c.count(PageStatus::Deleted),
            "pages_moved": c.count(PageStatus::Moved),
            "pages_modified": c.count(PageStatus::Modified),
            "pages_same": c.count(PageStatus::Same),
            "replaced": c.text.count(Kind::Replaced),
            "inserted": c.text.count(Kind::Inserted),
            "deleted": c.text.count(Kind::Deleted),
            "visual_pages": c.visual.len(),
            "visual_skipped": c.visual_skipped,
        },
        "old_words": c.text.old_words,
        "new_words": c.text.new_words,
        "pages": pages,
        "changes": changes,
        "changes_total": c.text.changes.len(),
        "truncated": c.text.changes.len() > limit,
        "visual": visual,
        "options": {
            "old_pages": opts.old_pages.map(|(a, b)| [a + 1, b + 1]),
            "new_pages": opts.new_pages.map(|(a, b)| [a + 1, b + 1]),
            "visual": opts.visual,
            "dpi": opts.dpi,
            "tolerance": opts.tolerance,
            "threshold": opts.threshold,
        },
    })
}

/// Whether `r` overlaps one of the text changes' boxes on new page `page` (a visual
/// difference that the text change already explains).
pub fn explained_by_text(c: &Comparison, page: usize, r: &[f64; 4]) -> bool {
    c.changes
        .iter()
        .filter(|ch| ch.new.page == page)
        .flat_map(|ch| ch.new.rects.iter())
        .any(|m| m[0] <= r[2] && r[0] <= m[2] && m[1] <= r[3] && r[1] <= m[3])
}

/// At most this many page pairs are rendered for a visual compare.
pub const MAX_VISUAL_PAGES: usize = 2000;

/// Visual differences are marked orange.
pub const VISUAL_COLOUR: crate::Rgb = [0.94, 0.55, 0.1];

fn page_range(n: usize, r: Option<(usize, usize)>, which: &str) -> Result<std::ops::Range<usize>, EditError> {
    match r {
        None => Ok(0..n),
        Some((a, b)) if a <= b && b < n => Ok(a..b + 1),
        Some((a, b)) => {
            Err(EditError::Source(format!("pages {}–{} of the {which} file: it has {n} pages", a.saturating_add(1), b.saturating_add(1))))
        }
    }
}

fn renderer(d: &crate::Document) -> pdfcraft_render::PageRenderer {
    pdfcraft_render::PageRenderer::new(
        d.bytes.clone(),
        pdfcraft_render::RenderConfig { password: d.password.as_deref().map(Arc::from), ..Default::default() },
    )
}

/// What a comparison reads from one document: shared bytes and page geometry, so the work can
/// run away from the session (on a worker thread).
#[derive(Clone)]
struct JobDoc {
    bytes: Arc<Vec<u8>>,
    password: Option<String>,
    pages: Vec<pdfcraft_render::PageInfo>,
    range: std::ops::Range<usize>,
}

/// Reports work done; `false` from the callback stops the comparison.
struct Ticker<'a> {
    done: usize,
    total: usize,
    report: &'a mut dyn FnMut(usize, usize) -> bool,
}

impl Ticker<'_> {
    fn tick(&mut self) -> bool {
        self.done = self.done.saturating_add(1);
        self.total = self.total.max(self.done);
        (self.report)(self.done, self.total)
    }
}

impl JobDoc {
    fn of(d: &crate::Document, range: std::ops::Range<usize>) -> JobDoc {
        JobDoc { bytes: d.bytes.clone(), password: d.password.clone(), pages: d.info.pages.clone(), range }
    }

    fn renderer(&self) -> pdfcraft_render::PageRenderer {
        pdfcraft_render::PageRenderer::new(
            self.bytes.clone(),
            pdfcraft_render::RenderConfig { password: self.password.as_deref().map(Arc::from), ..Default::default() },
        )
    }

    /// The words of the range and each page's fingerprint (its words, or for a page without
    /// text a coarse picture of it). `None` when stopped.
    fn input(&self, ticker: &mut Ticker) -> Option<(Vec<pdfcraft_compare::Word>, Vec<Vec<u64>>)> {
        let mut r = self.renderer();
        let mut words = Vec::new();
        for page in self.range.clone() {
            if let Some(info) = self.pages.get(page) {
                words.extend(page_words(&mut r, page, info));
            }
            if !ticker.tick() {
                return None;
            }
        }
        let mut prints = pdfcraft_compare::page_fingerprints(&words, self.range.start, self.range.len());
        for (k, print) in prints.iter_mut().enumerate() {
            if print.is_empty() {
                let res = r.render(pdfcraft_render::RenderRequest { page: self.range.start + k, scale: 0.25, ..Default::default() });
                if res.error.is_none() {
                    *print = pdfcraft_compare::picture_tokens(&res.rgba, res.width, res.height);
                }
            }
        }
        Some((words, prints))
    }
}

/// A Compare Files run detached from the session ([`Session::compare_job`]): it can run on
/// another thread, report progress and be stopped. Each page's text is read once and serves
/// both the page map and the text differences.
#[derive(Clone)]
pub struct CompareJob {
    old: JobDoc,
    new: JobDoc,
    opts: CompareOptions,
}

impl CompareJob {
    pub fn options(&self) -> &CompareOptions {
        &self.opts
    }

    /// Compare. `progress(done, total)` is called after each page read or rendered (`total`
    /// grows to the real number of page pairs once the page map is known); returning `false`
    /// stops the run and gives `None`.
    pub fn run(&self, mut progress: impl FnMut(usize, usize) -> bool) -> Option<FileComparison> {
        let (a, b, opts) = (&self.old, &self.new, &self.opts);
        let reading = a.range.len().saturating_add(b.range.len());
        let guess = if opts.visual { a.range.len().min(b.range.len()).min(MAX_VISUAL_PAGES) } else { 0 };
        let mut ticker = Ticker { done: 0, total: reading.saturating_add(guess), report: &mut progress };
        if !(ticker.report)(0, ticker.total) {
            return None;
        }
        let (old_words, old_prints) = a.input(&mut ticker)?;
        let (new_words, new_prints) = b.input(&mut ticker)?;
        let mut pages = pdfcraft_compare::align_pages(&old_prints, &new_prints);
        for p in &mut pages {
            p.old = p.old.map(|o| o + a.range.start);
            p.new = p.new.map(|n| n + b.range.start);
        }
        let text = pdfcraft_compare::compare_aligned(&old_words, &new_words, &pages);
        let mut out = FileComparison { pages, text, visual: Vec::new(), visual_skipped: 0 };
        if opts.visual {
            let (mut va, mut vb) = (a.renderer(), b.renderer());
            let scale = opts.dpi.clamp(18.0, 150.0) / 72.0;
            let threshold = if opts.threshold.is_finite() { opts.threshold.clamp(0.0, 1.0) } else { 0.0 };
            let pairs: Vec<(usize, usize)> = out.pages.iter().filter_map(|p| Some((p.old?, p.new?))).collect();
            out.visual_skipped = pairs.len().saturating_sub(MAX_VISUAL_PAGES);
            ticker.total = ticker.done.saturating_add(pairs.len().min(MAX_VISUAL_PAGES));
            for (po, pn) in pairs.into_iter().take(MAX_VISUAL_PAGES) {
                if !ticker.tick() {
                    return None;
                }
                let Some(info) = b.pages.get(pn) else { continue };
                let x = va.render(pdfcraft_render::RenderRequest { page: po, scale, ..Default::default() });
                let y = vb.render(pdfcraft_render::RenderRequest { page: pn, scale, ..Default::default() });
                if x.error.is_some() || y.error.is_some() {
                    out.visual_skipped += 1;
                    continue;
                }
                let d = pdfcraft_compare::visual_diff((&y.rgba, y.width, y.height), (&x.rgba, x.width, x.height), opts.tolerance);
                if d.changed == 0 || d.fraction() <= threshold {
                    continue;
                }
                let s = y.width as f32 / info.width.max(1e-3);
                let regions = d
                    .regions
                    .iter()
                    .map(|r| {
                        let p = info.view_to_user(r[0] as f32 / s, r[1] as f32 / s);
                        let q = info.view_to_user(r[2] as f32 / s, r[3] as f32 / s);
                        [p[0].min(q[0]) as f64, p[1].min(q[1]) as f64, p[0].max(q[0]) as f64, p[1].max(q[1]) as f64]
                    })
                    .collect();
                out.visual.push(PageVisual { old_page: po, new_page: pn, fraction: d.fraction(), regions });
            }
        }
        Some(out)
    }
}

impl Session {
    /// Compare files: the text differences from document `old` to document `new`. The result
    /// (and the report and comments built from it) carries both documents' text, so both must
    /// allow copying ([`crate::Document::allows_copying`]).
    pub fn compare(&self, old: DocId, new: DocId) -> Result<Comparison, EditError> {
        Ok(self.compare_files(old, new, &CompareOptions::default())?.text)
    }

    /// Compare Files: the page map (inserted, deleted, moved and modified pages), the
    /// word-level text changes and, with `opts.visual`, the pages that look different.
    pub fn compare_files(&self, old: DocId, new: DocId, opts: &CompareOptions) -> Result<FileComparison, EditError> {
        self.compare_job(old, new, opts)?.run(|_, _| true).ok_or_else(|| EditError::Source("the comparison was stopped".into()))
    }

    /// A Compare Files run that can happen away from the session (see [`CompareJob::run`]).
    /// The page ranges and copy permission are checked now: the result (page map, text changes
    /// and everything built from it) carries both documents' text, so both must allow copying.
    /// Every Compare Files path goes through here.
    pub fn compare_job(&self, old: DocId, new: DocId, opts: &CompareOptions) -> Result<CompareJob, EditError> {
        let a = self.get(old).ok_or(EditError::NoDocument)?;
        let b = self.get(new).ok_or(EditError::NoDocument)?;
        for d in [a, b] {
            if !d.allows_copying() {
                return Err(EditError::Protection(format!(
                    "{} does not allow copying text, so its text can't be compared; open it with the permissions password",
                    d.name
                )));
            }
        }
        let ra = page_range(a.info.pages.len(), opts.old_pages, "old")?;
        let rb = page_range(b.info.pages.len(), opts.new_pages, "new")?;
        Ok(CompareJob { old: JobDoc::of(a, ra), new: JobDoc::of(b, rb), opts: opts.clone() })
    }

    /// The structured Compare Files report ([`report_json`] with every change) with both
    /// files' names and page counts.
    pub fn compare_json(&self, old: DocId, new: DocId, c: &FileComparison, opts: &CompareOptions, limit: usize) -> serde_json::Value {
        let mut v = report_json(c, opts, limit);
        for (key, id) in [("old", old), ("new", new)] {
            let (name, pages) = self.get(id).map_or((String::new(), 0), |d| (d.name.clone(), d.info.pages.len()));
            v[key] = serde_json::json!({ "name": name, "pages": pages });
        }
        v
    }

    /// Visual compare: regions where the pages of `new` look different from their counterpart
    /// in `old` (pages lined up by the page map, rendered at `dpi`), as (page, user-space box
    /// in `new`).
    pub fn compare_visual(&self, old: DocId, new: DocId, dpi: f32) -> Result<Vec<(usize, [f64; 4])>, EditError> {
        let c = self.compare_files(old, new, &CompareOptions { visual: true, dpi, ..Default::default() })?;
        Ok(c.visual.iter().flat_map(|v| v.regions.iter().map(move |r| (v.new_page, *r))).collect())
    }

    /// The compare report as a new PDF (not opened).
    pub fn compare_report(&self, old: DocId, new: DocId) -> Result<Arc<Vec<u8>>, EditError> {
        self.compare_files_report(old, new, &self.compare_files(old, new, &CompareOptions::default())?)
    }

    /// The compare report of an earlier [`Session::compare_files`] as a new PDF (not opened):
    /// the page map, the pages that look different, then every text change.
    pub fn compare_files_report(&self, old: DocId, new: DocId, c: &FileComparison) -> Result<Arc<Vec<u8>>, EditError> {
        let name = |id| self.get(id).map(|d| d.name.clone()).unwrap_or_default();
        let visual: Vec<(usize, usize)> = c.visual.iter().map(|v| (v.old_page, v.new_page)).collect();
        self.create_from_text("Compare Report", &pdfcraft_compare::report_with_pages(&c.text, &c.pages, &visual, &name(old), &name(new)))
    }

    /// Mark the differences in `new` as comments: highlights over replaced and inserted text
    /// (blue, green), a note where text was deleted (red) and a note on each inserted or moved
    /// page and where a page was deleted, authored "Compare". One undoable step; returns how
    /// many comments were added.
    pub fn mark_differences(&mut self, old: DocId, new: DocId) -> Result<usize, EditError> {
        let c = self.compare_files(old, new, &CompareOptions::default())?;
        self.mark_file_differences(new, &c)
    }

    /// Mark an earlier comparison's differences in `new` (see [`Session::mark_differences`]);
    /// visual differences no text change explains become orange rectangles.
    pub fn mark_file_differences(&mut self, new: DocId, c: &FileComparison) -> Result<usize, EditError> {
        let doc = self.get(new).ok_or(EditError::NoDocument)?;
        let edits = difference_edits(c, &doc.info.pages);
        let n = edits.len();
        if n > 0 {
            self.apply(new, Edit::Batch { label: "Mark differences".into(), edits })?;
        }
        Ok(n)
    }

    /// Compare Files results as a PDF: a copy of `new` with every difference marked as a
    /// comment ([`Session::mark_file_differences`]). The open documents are not changed.
    pub fn compare_overlay(&mut self, new: DocId, c: &FileComparison) -> Result<Arc<Vec<u8>>, EditError> {
        let (bytes, name, password) = {
            let d = self.get(new).ok_or(EditError::NoDocument)?;
            (d.bytes.clone(), d.name.clone(), d.password.clone())
        };
        let copy = self.open(format!("{name} (compared)"), None, bytes, password.as_deref()).map_err(|e| EditError::Reopen(e.to_string()))?;
        let result = self.mark_file_differences(copy, c).and_then(|_| self.save_bytes(copy));
        self.close(copy);
        result
    }
}

/// The comments that mark a comparison's differences on the new document's pages.
fn difference_edits(c: &FileComparison, pages: &[pdfcraft_render::PageInfo]) -> Vec<Edit> {
    let mut edits = Vec::new();
    let mut add = |page: usize, shape: Shape, color: crate::Rgb, contents: String| {
        let mut style = crate::Style::default_for(&shape);
        style.color = color;
        edits.push(Edit::AddAnnotation(NewAnnotation { page, shape, style, contents, author: "Compare".into() }));
    };
    for ch in &c.text.changes {
        let contents = match ch.kind {
            Kind::Replaced => format!("Replaced: \"{}\" with \"{}\"", ch.old.text, ch.new.text),
            Kind::Inserted => format!("Inserted: \"{}\"", ch.new.text),
            Kind::Deleted => format!("Deleted: \"{}\"", ch.old.text),
        };
        let shape = if ch.new.rects.is_empty() {
            let Some(r) = ch.new.near else { continue };
            Shape::Note { at: [r[0], r[3]], icon: NoteIcon::Note }
        } else {
            Shape::TextMarkup {
                kind: Markup::Highlight,
                quads: ch.new.rects.iter().map(|r| [r[0], r[3], r[2], r[3], r[0], r[1], r[2], r[1]]).collect(),
            }
        };
        add(ch.new.page, shape, colour(ch.kind), contents);
    }
    // Page notes in the top-left corner of the new page.
    let corner = |page: usize| pages.get(page).map(|i| i.view_to_user(8.0, 8.0)).map(|p| [p[0] as f64, p[1] as f64]);
    for (k, p) in c.pages.iter().enumerate() {
        let (page, contents, color) = match (p.status, p.old, p.new) {
            (PageStatus::Inserted, _, Some(n)) => (n, "Inserted page: not in the old file".to_string(), colour(Kind::Inserted)),
            (PageStatus::Moved, Some(o), Some(n)) => (n, format!("Moved page: page {} in the old file", o + 1), colour(Kind::Replaced)),
            (PageStatus::Deleted, Some(o), _) => {
                // The next page that is in the new document, else the one before.
                let near = c
                    .pages
                    .get(k + 1..)
                    .into_iter()
                    .flatten()
                    .find_map(|q| q.new)
                    .or_else(|| c.pages.get(..k).into_iter().flatten().rev().find_map(|q| q.new));
                let Some(n) = near else { continue };
                (n, format!("Deleted page: page {} of the old file was here", o + 1), colour(Kind::Deleted))
            }
            _ => continue,
        };
        let Some(at) = corner(page) else { continue };
        add(page, Shape::Note { at, icon: NoteIcon::Note }, color, contents);
    }
    for v in &c.visual {
        // Regions a text change already marks are not marked twice.
        for r in v.regions.iter().filter(|r| !explained_by_text(&c.text, v.new_page, r)) {
            add(
                v.new_page,
                Shape::Rectangle { rect: *r },
                VISUAL_COLOUR,
                format!("Looks different ({:.2}% of the page changed)", v.fraction * 100.0),
            );
        }
    }
    edits
}

/// Export a PDF ▸ Word, HTML, RTF or Excel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfficeFormat {
    Docx,
    Html,
    Rtf,
    /// One worksheet per page, detected tables as cell grids.
    Xlsx,
}

impl OfficeFormat {
    pub fn extension(self) -> &'static str {
        match self {
            OfficeFormat::Docx => "docx",
            OfficeFormat::Html => "html",
            OfficeFormat::Rtf => "rtf",
            OfficeFormat::Xlsx => "xlsx",
        }
    }

    pub fn from_extension(ext: &str) -> Option<OfficeFormat> {
        match ext.to_ascii_lowercase().as_str() {
            "docx" => Some(OfficeFormat::Docx),
            "html" | "htm" => Some(OfficeFormat::Html),
            "rtf" => Some(OfficeFormat::Rtf),
            "xlsx" => Some(OfficeFormat::Xlsx),
            _ => None,
        }
    }
}

/// Office exports read at most this many pages; later pages are left out and reported.
pub const MAX_OFFICE_PAGES: usize = 5000;
/// Images embedded in one office export stop at this many bytes; later images are left out and
/// reported (the text is still exported).
pub const MAX_OFFICE_IMAGE_BYTES: usize = 512 << 20;

/// The pages read for an office export, and what the caps left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OfficePages {
    pub pages: Vec<pdfcraft_export::Page>,
    /// Pages past [`MAX_OFFICE_PAGES`] that were left out.
    pub pages_left_out: usize,
    /// Images past [`MAX_OFFICE_IMAGE_BYTES`] that were left out.
    pub images_left_out: usize,
}

impl OfficePages {
    /// What the caps left out, as warnings.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if self.pages_left_out > 0 {
            w.push(format!("only the first {MAX_OFFICE_PAGES} pages were exported ({} left out)", self.pages_left_out));
        }
        if self.images_left_out > 0 {
            w.push(format!("{} images were left out: the export holds {} MiB of images at most", self.images_left_out, MAX_OFFICE_IMAGE_BYTES >> 20));
        }
        w
    }
}

/// A paragraph for export: its text, style and inline style runs (char offsets become bytes).
fn export_block(b: pdfcraft_edit::TextBlock) -> pdfcraft_export::Block {
    let style = |base_font: &str| {
        let f = base_font.to_ascii_lowercase();
        (f.contains("bold") || f.contains("black") || f.contains("heavy"), f.contains("italic") || f.contains("oblique"))
    };
    // Byte offset of each char boundary (and the end).
    let bounds: Vec<usize> = b.text.char_indices().map(|(i, _)| i).chain(std::iter::once(b.text.len())).collect();
    let byte = |c: usize| bounds.get(c).copied().unwrap_or(b.text.len());
    let runs = if b.runs.len() > 1 {
        b.runs
            .iter()
            .map(|r| {
                let (bold, italic) = style(&r.base_font);
                pdfcraft_export::Run {
                    start: byte(r.start),
                    end: byte(r.end),
                    font: pdfcraft_export::font_family(&r.base_font),
                    size: r.size,
                    bold: bold || r.bold,
                    italic: italic || r.italic,
                    color: r.color,
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let (bold, italic) = style(&b.base_font);
    pdfcraft_export::Block {
        font: pdfcraft_export::font_family(&b.base_font),
        text: b.text,
        rect: b.rect,
        size: b.size,
        bold: bold || b.bold,
        italic: italic || b.italic,
        color: b.color,
        runs,
    }
}

impl crate::Document {
    /// The pages as paragraphs and images (for Word, HTML and RTF export), including what
    /// form XObjects draw.
    pub fn export_pages(&self) -> Vec<pdfcraft_export::Page> {
        self.office_pages(&[], &mut |_, _| true).map(|p| p.pages).unwrap_or_default()
    }

    /// [`Document::export_pages`] for `pages` (0-based; empty = all), capped at
    /// [`MAX_OFFICE_PAGES`] pages and [`MAX_OFFICE_IMAGE_BYTES`] of images. `progress(done,
    /// total)` is called before each page; returning false stops with an error.
    pub fn office_pages(&self, pages: &[usize], progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<OfficePages, String> {
        let n = self.info.pages.len();
        if let Some(p) = pages.iter().find(|p| **p >= n) {
            return Err(format!("page {} does not exist (the document has {n} pages)", p.saturating_add(1)));
        }
        // In document order, each page once.
        let mut wanted: Vec<usize> = if pages.is_empty() { (0..n).collect() } else { pages.to_vec() };
        wanted.sort_unstable();
        wanted.dedup();
        let mut out = OfficePages { pages_left_out: wanted.len().saturating_sub(MAX_OFFICE_PAGES), ..Default::default() };
        wanted.truncate(MAX_OFFICE_PAGES);
        let Some(cos) = self.editor.as_ref().map(|e| &e.cos) else { return Ok(out) };
        let mut image_bytes = 0usize;
        let total = wanted.len();
        for (k, i) in wanted.into_iter().enumerate() {
            if !progress(k, total) {
                return Err(CANCELLED.into());
            }
            let Some(info) = self.info.pages.get(i) else { continue };
            let blocks = pdfcraft_edit::reading_paragraphs(cos, i)
                .unwrap_or_default()
                .into_iter()
                .filter(|b| !b.text.trim().is_empty())
                .map(export_block)
                .collect();
            let mut images = Vec::new();
            for im in pdfcraft_edit::reading_images(cos, i).unwrap_or_default() {
                let Some(object) = im.object else { continue };
                if image_bytes >= MAX_OFFICE_IMAGE_BYTES {
                    out.images_left_out += 1;
                    continue;
                }
                let Ok((ext, bytes)) = pdfcraft_create::image_file(cos, object) else { continue };
                if image_bytes.saturating_add(bytes.len()) > MAX_OFFICE_IMAGE_BYTES {
                    out.images_left_out += 1;
                    image_bytes = MAX_OFFICE_IMAGE_BYTES;
                    continue;
                }
                image_bytes += bytes.len();
                images.push(pdfcraft_export::Image { ext: if ext == "jpg" { "jpg" } else { "png" }, bytes, rect: im.rect });
            }
            out.pages.push(pdfcraft_export::Page { width: info.width as f64, height: info.height as f64, blocks, images });
        }
        progress(total, total);
        Ok(out)
    }

    /// The document as a Word, HTML, RTF or Excel file. Refused when the document does not allow
    /// copying: the file carries its text and images.
    pub fn export_office(&self, format: OfficeFormat) -> Result<Vec<u8>, String> {
        self.export_office_pages(format, &[], &mut |_, _| true).map(|(bytes, _)| bytes)
    }

    /// [`Document::export_office`] of `pages` (0-based; empty = all), with progress and cancel
    /// as [`Document::office_pages`]; also returns what the caps left out.
    pub fn export_office_pages(
        &self,
        format: OfficeFormat,
        pages: &[usize],
        progress: &mut dyn FnMut(usize, usize) -> bool,
    ) -> Result<(Vec<u8>, OfficePages), String> {
        self.require_copying()?;
        let mut read = self.office_pages(pages, progress)?;
        let title = self.info.title.clone().unwrap_or_else(|| self.name.trim_end_matches(".pdf").to_string());
        let doc_pages = std::mem::take(&mut read.pages);
        let bytes = match format {
            OfficeFormat::Docx => pdfcraft_export::docx(&doc_pages, &title),
            OfficeFormat::Html => pdfcraft_export::html(&doc_pages, &title).into_bytes(),
            OfficeFormat::Rtf => pdfcraft_export::rtf(&doc_pages).into_bytes(),
            OfficeFormat::Xlsx => pdfcraft_export::xlsx(&doc_pages, &title),
        };
        read.pages = doc_pages;
        Ok((bytes, read))
    }
}

/// The error an export stopped by its progress callback returns.
pub const CANCELLED: &str = "the export was cancelled";
