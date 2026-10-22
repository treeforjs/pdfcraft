//! The side-by-side compare view's model: the rows both panes share (one per page-map entry,
//! so moved, inserted and deleted pages line up with their counterpart or a gap) and every
//! change in the order next / previous visit them, with where each one is on the old and the
//! new side.
//!
//! Built once per comparison in O(rows + changes + regions); the UI and the automation tool
//! (`doc_compare_view`) read the same model, so a headless agent sees what the panes show.

use std::collections::HashMap;

use crate::compare::{FileComparison, Kind, PageStatus, explained_by_text};

/// What a change in the view is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKind {
    /// A text change (replaced, inserted or deleted words).
    Text(Kind),
    /// A region that looks different and no text change explains.
    Visual,
    /// A whole page inserted, deleted or moved.
    Page(PageStatus),
}

impl ViewKind {
    pub fn name(self) -> &'static str {
        match self {
            ViewKind::Text(Kind::Replaced) => "replaced",
            ViewKind::Text(Kind::Inserted) => "inserted",
            ViewKind::Text(Kind::Deleted) => "deleted",
            ViewKind::Visual => "visual",
            ViewKind::Page(PageStatus::Inserted) => "page_inserted",
            ViewKind::Page(PageStatus::Deleted) => "page_deleted",
            ViewKind::Page(PageStatus::Moved) => "page_moved",
            ViewKind::Page(_) => "page",
        }
    }
}

/// Where a change is on one side.
#[derive(Clone, Debug, PartialEq)]
pub struct Spot {
    /// 0-based document page.
    pub page: usize,
    /// The view row the page is on.
    pub row: usize,
    /// Boxes to highlight (user space); empty for a gap (inserted or deleted words) or a page.
    pub rects: Vec<[f64; 4]>,
    /// The box to scroll to: the first highlight, the word beside a gap, or `None` for the
    /// top of the page.
    pub focus: Option<[f64; 4]>,
}

/// One change, as the view lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewChange {
    pub kind: ViewKind,
    pub old: Option<Spot>,
    pub new: Option<Spot>,
    /// Index into [`crate::compare::Comparison::changes`] for a text change.
    pub text: Option<usize>,
}

impl ViewChange {
    /// The row both panes go to: the new side's, else the old side's (`None` when neither
    /// side is in the view).
    pub fn row(&self) -> Option<usize> {
        self.new.as_ref().or(self.old.as_ref()).map(|s| s.row)
    }
}

/// One row of the view: the old page and the new page shown side by side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewRow {
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub status: PageStatus,
}

/// The side-by-side model of one comparison.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompareView {
    pub rows: Vec<ViewRow>,
    pub changes: Vec<ViewChange>,
    old_row: HashMap<usize, usize>,
    new_row: HashMap<usize, usize>,
    /// Change indices with something to draw, per page of each side.
    old_marks: HashMap<usize, Vec<usize>>,
    new_marks: HashMap<usize, Vec<usize>>,
}

/// At most this many changes are listed and navigated (a comparison of two unrelated
/// thousand-page files can find far more; the report and the JSON have them all).
pub const MAX_VIEW_CHANGES: usize = 50_000;

impl CompareView {
    pub fn new(c: &FileComparison) -> Self {
        let mut v = CompareView::default();
        for p in &c.pages {
            let row = v.rows.len();
            // A page appears in one row; a hostile or inconsistent map keeps the first.
            if let Some(o) = p.old {
                v.old_row.entry(o).or_insert(row);
            }
            if let Some(n) = p.new {
                v.new_row.entry(n).or_insert(row);
            }
            v.rows.push(ViewRow { old: p.old, new: p.new, status: p.status });
        }
        // Each change carries the row it sorts by and whether it is a page entry (first in its
        // row), a text change (reading order) or a visual region (after the text).
        let mut list: Vec<(usize, u8, ViewChange)> = Vec::new();
        for (row, r) in v.rows.iter().enumerate() {
            if matches!(r.status, PageStatus::Inserted | PageStatus::Deleted | PageStatus::Moved) {
                let spot = |page: Option<usize>| page.map(|page| Spot { page, row, rects: Vec::new(), focus: None });
                list.push((row, 0, ViewChange { kind: ViewKind::Page(r.status), old: spot(r.old), new: spot(r.new), text: None }));
            }
        }
        for (i, ch) in c.text.changes.iter().enumerate().take(MAX_VIEW_CHANGES) {
            let side = |s: &crate::compare::Side, rows: &HashMap<usize, usize>| {
                rows.get(&s.page).map(|&row| Spot { page: s.page, row, rects: s.rects.clone(), focus: s.rects.first().copied().or(s.near) })
            };
            let (old, new) = (side(&ch.old, &v.old_row), side(&ch.new, &v.new_row));
            let row = new.as_ref().or(old.as_ref()).map_or(usize::MAX, |s| s.row);
            list.push((row, 1, ViewChange { kind: ViewKind::Text(ch.kind), old, new, text: Some(i) }));
        }
        let mut regions = 0usize;
        for pv in &c.visual {
            for r in &pv.regions {
                if regions >= MAX_VIEW_CHANGES {
                    break;
                }
                if explained_by_text(&c.text, pv.new_page, r) {
                    continue;
                }
                regions += 1;
                let spot =
                    |page: usize, rows: &HashMap<usize, usize>| rows.get(&page).map(|&row| Spot { page, row, rects: vec![*r], focus: Some(*r) });
                let (old, new) = (spot(pv.old_page, &v.old_row), spot(pv.new_page, &v.new_row));
                let row = new.as_ref().or(old.as_ref()).map_or(usize::MAX, |s| s.row);
                list.push((row, 2, ViewChange { kind: ViewKind::Visual, old, new, text: None }));
            }
        }
        // Stable: text changes keep their reading order within a row.
        list.sort_by_key(|(row, rank, _)| (*row, *rank));
        list.truncate(MAX_VIEW_CHANGES);
        v.changes = list.into_iter().map(|(_, _, ch)| ch).collect();
        for (i, ch) in v.changes.iter().enumerate() {
            if let Some(s) = &ch.old {
                v.old_marks.entry(s.page).or_default().push(i);
            }
            if let Some(s) = &ch.new {
                v.new_marks.entry(s.page).or_default().push(i);
            }
        }
        v
    }

    /// The row an old (`new == false`) or new page is on.
    pub fn row_of(&self, new: bool, page: usize) -> Option<usize> {
        if new { self.new_row.get(&page) } else { self.old_row.get(&page) }.copied()
    }

    /// The changes on a page of one side (indices into [`Self::changes`]).
    pub fn marks(&self, new: bool, page: usize) -> &[usize] {
        if new { self.new_marks.get(&page) } else { self.old_marks.get(&page) }.map_or(&[], Vec::as_slice)
    }

    /// The change after (or before) `from`, wrapping around; the first (or last) one when
    /// nothing is selected. `None` when there are no changes.
    pub fn step(&self, from: Option<usize>, forward: bool) -> Option<usize> {
        let n = self.changes.len();
        if n == 0 {
            return None;
        }
        Some(match from.filter(|&i| i < n) {
            None if forward => 0,
            None => n - 1,
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
        })
    }

    /// The first change at or after `row`, for a pane scrolled there (wraps to the first).
    pub fn change_at_row(&self, row: usize) -> Option<usize> {
        let i = self.changes.partition_point(|c| c.row().unwrap_or(usize::MAX) < row);
        if i < self.changes.len() { Some(i) } else { self.step(None, true) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::{Change, Comparison, PageMatch, PageVisual, Side};

    fn side(page: usize, rects: Vec<[f64; 4]>) -> Side {
        Side { text: "x".into(), page, rects, near: None }
    }

    fn sample() -> FileComparison {
        let m = |status, old, new| PageMatch { status, old, new, similarity: 0.0 };
        FileComparison {
            pages: vec![
                m(PageStatus::Moved, Some(2), Some(0)),
                m(PageStatus::Same, Some(0), Some(1)),
                m(PageStatus::Deleted, Some(1), None),
                m(PageStatus::Inserted, None, Some(2)),
                m(PageStatus::Same, Some(3), Some(3)),
            ],
            text: Comparison {
                changes: vec![
                    Change { kind: Kind::Replaced, old: side(2, vec![[10.0, 700.0, 40.0, 712.0]]), new: side(0, vec![[10.0, 700.0, 40.0, 712.0]]) },
                    Change {
                        kind: Kind::Inserted,
                        old: Side { near: Some([5.0, 5.0, 9.0, 9.0]), ..side(1, vec![]) },
                        new: side(2, vec![[1.0, 1.0, 2.0, 2.0]]),
                    },
                ],
                old_words: 10,
                new_words: 10,
            },
            visual: vec![
                // Explained by the text change on new page 0: not listed.
                PageVisual { old_page: 2, new_page: 0, fraction: 0.1, regions: vec![[12.0, 702.0, 30.0, 710.0]] },
                PageVisual { old_page: 3, new_page: 3, fraction: 0.1, regions: vec![[0.0, 0.0, 100.0, 100.0]] },
            ],
            visual_skipped: 0,
        }
    }

    #[test]
    fn rows_follow_the_page_map_and_changes_go_in_row_order() {
        let v = CompareView::new(&sample());
        assert_eq!(v.rows.len(), 5);
        assert_eq!((v.row_of(false, 2), v.row_of(true, 0)), (Some(0), Some(0)));
        assert_eq!((v.row_of(false, 1), v.row_of(true, 2)), (Some(2), Some(3)));
        assert_eq!(v.row_of(true, 9), None);
        let kinds: Vec<(ViewKind, usize)> = v.changes.iter().filter_map(|c| Some((c.kind, c.row()?))).collect();
        assert_eq!(
            kinds,
            [
                (ViewKind::Page(PageStatus::Moved), 0),
                (ViewKind::Text(Kind::Replaced), 0),
                (ViewKind::Page(PageStatus::Deleted), 2),
                (ViewKind::Page(PageStatus::Inserted), 3),
                (ViewKind::Text(Kind::Inserted), 3),
                (ViewKind::Visual, 4),
            ]
        );
        // The inserted words' old side points at the word beside the gap, on the deleted row.
        let ins = &v.changes[4];
        assert_eq!(ins.old.as_ref().map(|s| (s.row, s.focus)), Some((2, Some([5.0, 5.0, 9.0, 9.0]))));
        assert_eq!(v.marks(true, 0), &[0, 1]);
        assert_eq!(v.marks(false, 3), &[5]);
        assert!(v.marks(false, 42).is_empty());
    }

    #[test]
    fn stepping_wraps_and_starts_from_either_end() {
        let v = CompareView::new(&sample());
        assert_eq!(v.step(None, true), Some(0));
        assert_eq!(v.step(None, false), Some(5));
        assert_eq!(v.step(Some(5), true), Some(0));
        assert_eq!(v.step(Some(0), false), Some(5));
        assert_eq!(v.step(Some(usize::MAX), true), Some(0));
        assert_eq!(v.change_at_row(3), Some(3));
        assert_eq!(v.change_at_row(99), Some(0));
        assert_eq!(CompareView::new(&FileComparison::default()).step(None, true), None);
    }

    /// Changes naming pages the page map does not have (an inconsistent result) and a map
    /// listing a page twice are kept out of the panes rather than trusted.
    #[test]
    fn hostile_results_do_not_point_outside_the_rows() {
        let mut c = sample();
        c.pages.push(PageMatch { status: PageStatus::Same, old: Some(0), new: Some(1), similarity: 1.0 });
        c.text.changes.push(Change { kind: Kind::Deleted, old: side(usize::MAX, vec![[f64::NAN; 4]]), new: side(usize::MAX, vec![]) });
        c.visual.push(PageVisual { old_page: 77, new_page: 88, fraction: 1.0, regions: vec![[0.0; 4]] });
        let v = CompareView::new(&c);
        assert_eq!(v.row_of(true, 1), Some(1), "the first row a page is on wins");
        for ch in &v.changes {
            for s in ch.old.iter().chain(&ch.new) {
                assert!(s.row < v.rows.len());
            }
        }
        // Spot-less changes sort last and still navigate.
        assert!(v.changes.last().is_some_and(|c| c.old.is_none() && c.new.is_none()));
    }
}
