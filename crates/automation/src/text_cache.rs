//! Extracted page text kept between tool calls, bounded by an estimate of its heap size.
//!
//! `text_extract`, `text_find`, `redact_mark` and `comment_add`'s `find` read page text with
//! glyph geometry (`PageText`, roughly 150 KiB for a page of 40 dense lines). Keeping every
//! page for the life of a document grew a 5 000-page session by ~770 MiB that was never
//! released (docs/performance.md). Two bounded least-recently-used caches replace it:
//!
//! - page text with geometry, at most [`DEFAULT_BUDGET`] bytes across all documents;
//! - a geometry-free search index ([`SearchText`], the chars page search runs on, about two
//!   bytes per glyph), at most [`SEARCH_BUDGET`] bytes. `text_find` and `redact_mark` use it to
//!   skip pages that cannot match, so a repeated whole-document search only re-extracts the
//!   geometry of pages that do match.
//!
//! Both are keyed by the document's working bytes, so an edit or a save drops a document's
//! entries. Results are unchanged.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use pdfcraft_engine::DocId;
use pdfcraft_render::{PageText, TextGlyph};

/// Upper bound on the estimated heap size of cached page text, across all open documents
/// (about 600 pages of 40 dense lines, so a 500-page document's text stays cached).
pub(crate) const DEFAULT_BUDGET: usize = 96 << 20;

/// Upper bound on the search index across all open documents (about 9 000 pages of 40 dense
/// lines).
pub(crate) const SEARCH_BUDGET: usize = 32 << 20;

/// Estimated heap memory a cached value holds.
pub(crate) trait Footprint {
    fn footprint(&self) -> usize;
}

/// The geometry-free text page search runs on (see `PageText::search_text`): case-folded for
/// `find`, original case for pattern matchers (`find_with`).
pub(crate) struct SearchText {
    folded: String,
    raw: String,
}

impl SearchText {
    pub(crate) fn of(t: &PageText) -> Self {
        let mut folded = t.search_text(true);
        let mut raw = t.search_text(false);
        folded.shrink_to_fit();
        raw.shrink_to_fit();
        Self { folded, raw }
    }

    /// Whether `PageText::find(needle)` finds anything on the page this was made from.
    pub(crate) fn may_find(&self, needle: &str) -> bool {
        let chars: Vec<char> = self.folded.chars().collect();
        !pdfcraft_render::find_in_chars(&chars, needle, false, false).is_empty()
    }

    /// Whether `PageText::find_with(matcher)` finds anything on the page this was made from.
    pub(crate) fn may_match(&self, matcher: impl Fn(&[char]) -> Vec<std::ops::Range<usize>>) -> bool {
        let chars: Vec<char> = self.raw.chars().collect();
        matcher(&chars).iter().any(|r| r.start < r.end && r.end <= chars.len())
    }
}

impl Footprint for SearchText {
    fn footprint(&self) -> usize {
        size_of::<Self>().saturating_add(block(self.folded.capacity())).saturating_add(block(self.raw.capacity()))
    }
}

impl Footprint for PageText {
    /// Its vectors and every glyph's own string.
    fn footprint(&self) -> usize {
        let strings = self.glyphs.iter().fold(0usize, |n, g| n.saturating_add(block(g.text.capacity())));
        size_of::<PageText>()
            .saturating_add(block(self.glyphs.capacity().saturating_mul(size_of::<TextGlyph>())))
            .saturating_add(strings)
            .saturating_add(block(self.line_of.capacity().saturating_mul(size_of::<u32>())))
            .saturating_add(block(self.space_before.capacity()))
    }
}

struct Entry<T> {
    value: Arc<T>,
    cost: usize,
    stamp: u64,
}

/// The pages cached for one document version (the working bytes they were extracted from).
struct DocPages<T> {
    bytes: Arc<Vec<u8>>,
    pages: HashMap<usize, Entry<T>>,
}

/// Per-page values for open documents, least recently used evicted beyond a byte budget.
pub(crate) struct PageCache<T> {
    docs: HashMap<DocId, DocPages<T>>,
    /// Least recently used first: use stamp → (document, page).
    order: BTreeMap<u64, (DocId, usize)>,
    used: usize,
    budget: usize,
    clock: u64,
}

/// Page text with glyph geometry.
pub(crate) type TextCache = PageCache<PageText>;
/// The geometry-free search index.
pub(crate) type SearchCache = PageCache<SearchText>;

impl<T: Footprint> PageCache<T> {
    pub(crate) fn new(budget: usize) -> Self {
        Self { docs: HashMap::new(), order: BTreeMap::new(), used: 0, budget, clock: 0 }
    }

    /// Estimated bytes held.
    #[cfg(test)]
    pub(crate) fn used(&self) -> usize {
        self.used
    }

    /// Pages held, across documents.
    #[cfg(test)]
    pub(crate) fn pages(&self) -> usize {
        self.order.len()
    }

    pub(crate) fn remove_doc(&mut self, id: DocId) {
        if let Some(doc) = self.docs.remove(&id) {
            for e in doc.pages.into_values() {
                self.order.remove(&e.stamp);
                self.used = self.used.saturating_sub(e.cost);
            }
        }
    }

    /// Forget `id`'s pages unless they were extracted from `bytes` (an edit or a save replaced
    /// the working bytes).
    pub(crate) fn sync(&mut self, id: DocId, bytes: &Arc<Vec<u8>>) {
        if self.docs.get(&id).is_some_and(|d| !Arc::ptr_eq(&d.bytes, bytes)) {
            self.remove_doc(id);
        }
        self.docs.entry(id).or_insert_with(|| DocPages { bytes: bytes.clone(), pages: HashMap::new() });
    }

    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    /// Whether `page` is cached (without marking it used).
    pub(crate) fn contains(&self, id: DocId, page: usize) -> bool {
        self.docs.get(&id).is_some_and(|d| d.pages.contains_key(&page))
    }

    /// The cached value of `page`, marked as just used.
    pub(crate) fn get(&mut self, id: DocId, page: usize) -> Option<Arc<T>> {
        let stamp = self.tick();
        let entry = self.docs.get_mut(&id)?.pages.get_mut(&page)?;
        self.order.remove(&entry.stamp);
        entry.stamp = stamp;
        self.order.insert(stamp, (id, page));
        Some(entry.value.clone())
    }

    /// Keep `value` for `page` of the version last passed to [`Self::sync`], then evict the
    /// least recently used pages until the budget holds. A value larger than the whole budget
    /// is not kept.
    pub(crate) fn insert(&mut self, id: DocId, page: usize, value: Arc<T>) {
        let cost = value.footprint();
        if cost > self.budget {
            return;
        }
        let stamp = self.tick();
        let Some(doc) = self.docs.get_mut(&id) else { return };
        if let Some(old) = doc.pages.insert(page, Entry { value, cost, stamp }) {
            self.order.remove(&old.stamp);
            self.used = self.used.saturating_sub(old.cost);
        }
        self.order.insert(stamp, (id, page));
        self.used = self.used.saturating_add(cost);
        while self.used > self.budget {
            let Some((_, (doc, page))) = self.order.pop_first() else { break };
            if let Some(e) = self.docs.get_mut(&doc).and_then(|d| d.pages.remove(&page)) {
                self.used = self.used.saturating_sub(e.cost);
            }
        }
    }
}

/// A heap block of `bytes`, as the allocator spends it (16-byte granules plus a header).
fn block(bytes: usize) -> usize {
    if bytes == 0 { 0 } else { bytes.div_ceil(16).saturating_mul(16).saturating_add(16) }
}

/// Estimated memory held by `t`.
#[cfg(test)]
pub(crate) fn cost(t: &PageText) -> usize {
    t.footprint()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Automation;
    use serde_json::json;

    fn page(glyphs: usize) -> Arc<PageText> {
        let glyphs = (0..glyphs).map(|i| TextGlyph { text: "x".into(), rect: [i as f32, 0.0, i as f32 + 1.0, 1.0], mcid: None }).collect();
        Arc::new(pdfcraft_render::text::layout(glyphs))
    }

    #[test]
    fn least_recently_used_pages_are_evicted_within_the_budget() {
        let one = cost(&page(100));
        let mut c = TextCache::new(one * 3);
        let bytes = Arc::new(vec![1u8]);
        c.sync(DocId(1), &bytes);
        for p in 0..3 {
            c.insert(DocId(1), p, page(100));
        }
        assert_eq!(c.pages(), 3);
        assert!(c.get(DocId(1), 0).is_some(), "page 0 is now the most recently used");
        c.insert(DocId(1), 3, page(100));
        assert_eq!(c.pages(), 3);
        assert!(c.used() <= one * 3);
        assert!(c.get(DocId(1), 1).is_none(), "the least recently used page went first");
        assert!(c.get(DocId(1), 0).is_some() && c.get(DocId(1), 3).is_some());
        // A page bigger than the whole budget is served but never kept.
        c.insert(DocId(1), 9, page(1000));
        assert!(c.get(DocId(1), 9).is_none());
        // New working bytes (an edit) drop the old version's pages; closing drops the rest.
        c.sync(DocId(1), &Arc::new(vec![2u8]));
        assert_eq!((c.pages(), c.used()), (0, 0));
        c.insert(DocId(1), 0, page(10));
        c.remove_doc(DocId(1));
        assert_eq!((c.pages(), c.used()), (0, 0));
    }

    #[test]
    fn page_cost_counts_every_glyph_string() {
        let small = cost(&page(10));
        let large = cost(&page(1000));
        assert!(large > 1000 * (size_of::<TextGlyph>() + 16), "{large}");
        assert!(small < large);
    }

    fn call(a: &mut Automation, tool: &str, args: serde_json::Value) -> serde_json::Value {
        let out = a.call(tool, &args).unwrap_or_else(|e| panic!("{tool}: {e}"));
        out.iter()
            .find_map(|c| match c {
                crate::Content::Json(v) => Some(v.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The same tool results with budgets that hold nothing as with unbounded ones, for both the
    /// geometry cache and the search index (including only the index: searches then skip pages
    /// it proves match-free), while neither exceeds its budget, and edits invalidate both.
    #[test]
    fn bounded_cache_keeps_tool_results_identical() {
        // Twelve pages (a form feed starts a page).
        let text = (1..=12)
            .map(|i| format!("Page {i} alpha beta gamma marker{i}{}", if i % 5 == 0 { format!(" mail{i}@example.org") } else { String::new() }))
            .collect::<Vec<_>>()
            .join("\u{c}");
        let mut runs = Vec::new();
        for (budget, search) in [(usize::MAX, usize::MAX), (1, 1), (1, usize::MAX)] {
            let mut a = Automation::new();
            a.texts = TextCache::new(budget);
            a.search = SearchCache::new(search);
            let created = call(&mut a, "doc_create", json!({ "from": "text", "text": text }));
            let doc = created["doc"].as_u64().unwrap();
            assert_eq!(created["pages"], json!(12), "{created}");
            let mut results = vec![
                call(&mut a, "text_extract", json!({ "doc": doc })),
                call(&mut a, "text_extract", json!({ "doc": doc, "pages": [2, 1, 2], "include_glyphs": true })),
                call(&mut a, "text_find", json!({ "doc": doc, "query": "alpha" })),
                call(&mut a, "text_find", json!({ "doc": doc, "query": "alpha", "limit": 2 })),
                call(&mut a, "text_find", json!({ "doc": doc, "query": "marker1" })),
                call(&mut a, "text_find", json!({ "doc": doc, "query": "MARKER12" })),
                call(&mut a, "text_find", json!({ "doc": doc, "query": "nowhere" })),
            ];
            assert!(a.texts.used() <= budget && a.search.used() <= search);
            if budget == 1 {
                assert_eq!(a.texts.pages(), 0, "no page fits a one-byte budget");
            }
            if search == usize::MAX {
                assert_eq!(a.search.pages(), 12, "every extracted page is indexed");
            }
            // Pattern search through the index (marks only, then undone).
            results.push(call(&mut a, "redact_mark", json!({ "doc": doc, "pattern": "email" }))["marked"].clone());
            call(&mut a, "edit_undo", json!({ "doc": doc }));
            // An edit changes the text: nothing stale may be served.
            call(&mut a, "redact_mark", json!({ "doc": doc, "find": "gamma" }));
            call(&mut a, "redact_apply", json!({ "doc": doc }));
            results.push(call(&mut a, "text_find", json!({ "doc": doc, "query": "gamma" })));
            results.push(call(&mut a, "text_extract", json!({ "doc": doc, "pages": [1] })));
            runs.push(results);
        }
        assert_eq!(runs[0], runs[1]);
        assert_eq!(runs[0], runs[2]);
        let r = &runs[0];
        assert_eq!(r[2]["count"], json!(12), "{}", r[2]);
        assert_eq!(r[3]["count"], json!(2));
        assert_eq!(r[3]["matches"][1]["page"], r[2]["matches"][1]["page"], "the first matches in page order");
        assert_eq!(r[4]["matches"][0]["page"], json!(1), "{}", r[4]);
        assert_eq!(r[5]["matches"][0]["page"], json!(12), "{}", r[5]);
        assert_eq!(r[6]["count"], json!(0));
        assert_eq!(r[7], json!(2), "email marks on pages 5 and 10");
        assert_eq!(r[8]["count"], json!(0), "redacted text is gone: {}", r[8]);
        assert_eq!(r[1]["pages"][0]["page"], json!(2));
        assert_eq!(r[1]["pages"][1]["page"], json!(1));
        assert_eq!(r[1]["pages"][0], r[1]["pages"][2]);
    }
}
