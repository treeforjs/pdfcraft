//! pdfcraft-compare — Compare files: text, page and visual differences (L4).
//!
//! Both documents' words (in reading order, with page and box) are diffed with Myers'
//! O((N+M)·D) algorithm after the common prefix and suffix are set aside; the edit script is
//! grouped into [`Change`]s — inserted, deleted or replaced runs of words — each with the
//! rectangles to highlight on the old and new pages. [`align_pages`] works out which pages were
//! inserted, deleted, moved or modified, and [`visual_diff`] finds where two renderings of a
//! page differ and by how much.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod pages;

pub use pages::{PageMatch, PageStatus, align_pages, page_fingerprints, picture_tokens};

/// A word of one document.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    /// 0-based page.
    pub page: usize,
    /// [x0, y0, x1, y1] in the page's user space.
    pub rect: [f64; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Inserted,
    Deleted,
    Replaced,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Inserted => "Inserted",
            Kind::Deleted => "Deleted",
            Kind::Replaced => "Replaced",
        }
    }
}

/// One side of a change: its words' text, page and rectangles (merged per line).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Side {
    pub text: String,
    /// The page the change is on (for a pure insertion or deletion, where the other side's
    /// words would have been).
    pub page: usize,
    pub rects: Vec<[f64; 4]>,
    /// For an empty side (the other side's words were inserted or deleted): the box of the
    /// word next to where they would be.
    pub near: Option<[f64; 4]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub kind: Kind,
    pub old: Side,
    pub new: Side,
}

/// What the comparison found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Comparison {
    pub changes: Vec<Change>,
    /// Words in each document.
    pub old_words: usize,
    pub new_words: usize,
}

impl Comparison {
    pub fn count(&self, kind: Kind) -> usize {
        self.changes.iter().filter(|c| c.kind == kind).count()
    }

    /// Whether the documents' text is the same.
    pub fn identical(&self) -> bool {
        self.changes.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Equal,
    Delete,
    Insert,
}

/// Segments longer than this (old + new words) are first split at anchors, so Myers' saved
/// frontiers (O(D²) memory) stay small: at most about DIRECT² machine words (≈ 32 MB) for one
/// segment, freed before the next.
const DIRECT: usize = 2000;

/// The edit script for `a` → `b`.
pub(crate) fn diff(a: &[u64], b: &[u64], out: &mut Vec<Op>, depth: usize) {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    out.extend(std::iter::repeat_n(Op::Equal, prefix));
    let (ma, mb) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let replace = |out: &mut Vec<Op>| out.extend(std::iter::repeat_n(Op::Delete, ma.len()).chain(std::iter::repeat_n(Op::Insert, mb.len())));
    if ma.len() + mb.len() <= DIRECT {
        out.extend(myers(ma, mb));
    } else {
        let anchors = if depth > 8 { Vec::new() } else { anchors(ma, mb) };
        if anchors.is_empty() {
            // A long stretch with nothing in common: one replacement.
            replace(out);
        } else {
            let (mut i, mut j) = (0, 0);
            for (x, y) in anchors {
                diff(&ma[i..x], &mb[j..y], out, depth + 1);
                out.push(Op::Equal);
                i = x + 1;
                j = y + 1;
            }
            diff(&ma[i..], &mb[j..], out, depth + 1);
        }
    }
    out.extend(std::iter::repeat_n(Op::Equal, suffix));
}

/// Patience anchors: words that occur once in each side, in an order both sides agree on
/// (the longest increasing subsequence of their positions).
fn anchors(a: &[u64], b: &[u64]) -> Vec<(usize, usize)> {
    use std::collections::HashMap;
    let mut count: HashMap<u64, (usize, usize, usize, usize)> = HashMap::new();
    for (i, h) in a.iter().enumerate() {
        let e = count.entry(*h).or_insert((0, 0, 0, 0));
        e.0 += 1;
        e.2 = i;
    }
    for (j, h) in b.iter().enumerate() {
        if let Some(e) = count.get_mut(h) {
            e.1 += 1;
            e.3 = j;
        }
    }
    let mut pairs: Vec<(usize, usize)> = count.values().filter(|e| e.0 == 1 && e.1 == 1).map(|e| (e.2, e.3)).collect();
    pairs.sort();
    // LIS on the b positions (patience sorting with back links).
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; pairs.len()];
    for (k, &(_, y)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&t| pairs[t].1 < y);
        if pos > 0 {
            prev[k] = tails[pos - 1];
        }
        if pos == tails.len() {
            tails.push(k);
        } else {
            tails[pos] = k;
        }
    }
    let mut out = Vec::new();
    let mut k = tails.last().copied().unwrap_or(usize::MAX);
    while k != usize::MAX {
        out.push(pairs[k]);
        k = prev[k];
    }
    out.reverse();
    out
}

/// Myers' diff of `a` and `b`: the edit script.
fn myers(a: &[u64], b: &[u64]) -> Vec<Op> {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return std::iter::repeat_n(Op::Delete, n).chain(std::iter::repeat_n(Op::Insert, m)).collect();
    }
    let max = n + m;
    let off = max as isize + 1;
    let mut v = vec![0usize; 2 * max + 3];
    // Frontier d only spans diagonals -d..=d: save just that slice.
    let mut trace: Vec<Vec<usize>> = Vec::new();
    let mut found = false;
    'outer: for d in 0..=max as isize {
        trace.push(v[(off - d - 1).max(0) as usize..=(off + d + 1) as usize].to_vec());
        let mut k = -d;
        while k <= d {
            let i = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) { v[i + 1] } else { v[i - 1] + 1 };
            let mut y = (x as isize - k) as usize;
            while x < n && y < m && a[x] == b[y] {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x >= n && y >= m {
                found = true;
                break 'outer;
            }
            k += 2;
        }
    }
    if !found {
        // Too different: replace everything.
        return std::iter::repeat_n(Op::Delete, n).chain(std::iter::repeat_n(Op::Insert, m)).collect();
    }
    // Walk back through the saved frontiers.
    let mut ops = Vec::new();
    let (mut x, mut y) = (n as isize, m as isize);
    for d in (0..trace.len() as isize).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let base = (off - d - 1).max(0);
        let i = |k: isize| (k + off - base) as usize;
        let prev_k = if k == -d || (k != d && v[i(k - 1)] < v[i(k + 1)]) { k + 1 } else { k - 1 };
        let prev_x = v[i(prev_k)] as isize;
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops.push(Op::Equal);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            ops.push(if x == prev_x { Op::Insert } else { Op::Delete });
        }
        x = prev_x;
        y = prev_y;
    }
    ops.reverse();
    ops
}

pub(crate) fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Merge word boxes on the same page and line into one rectangle per line.
fn rects(words: &[&Word]) -> Vec<[f64; 4]> {
    let mut out: Vec<[f64; 4]> = Vec::new();
    for w in words {
        let r = w.rect;
        match out.last_mut() {
            Some(l) if (l[1] - r[1]).abs() < (r[3] - r[1]).max(1.0) * 0.5 && r[0] - l[2] < (r[3] - r[1]).max(4.0) * 2.0 && r[0] >= l[0] - 1.0 => {
                l[0] = l[0].min(r[0]);
                l[1] = l[1].min(r[1]);
                l[2] = l[2].max(r[2]);
                l[3] = l[3].max(r[3]);
            }
            _ => out.push(r),
        }
    }
    out
}

fn side(words: &[&Word], neighbour: Option<&Word>) -> Side {
    Side {
        text: words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "),
        page: words.first().or(neighbour.as_ref()).map_or(0, |w| w.page),
        rects: rects(words),
        near: if words.is_empty() { neighbour.map(|w| w.rect) } else { None },
    }
}

/// Compare two documents' words.
pub fn compare(old: &[Word], new: &[Word]) -> Comparison {
    let old: Vec<&Word> = old.iter().collect();
    let new: Vec<&Word> = new.iter().collect();
    Comparison { changes: changes(&old, &new), old_words: old.len(), new_words: new.len() }
}

/// Compare two documents' words knowing how their pages line up ([`align_pages`]): the words of
/// pages that moved are compared with their counterpart only, so a moved page is not reported
/// as all its text deleted in one place and inserted in another. Everything else is compared
/// as one stream, so text that flows from one page to the next still diffs cleanly. Changes
/// come in new-document page order.
pub fn compare_aligned(old: &[Word], new: &[Word], pages: &[PageMatch]) -> Comparison {
    let moved: Vec<(usize, usize)> = pages.iter().filter(|p| p.status == PageStatus::Moved).filter_map(|p| Some((p.old?, p.new?))).collect();
    let moved_old: std::collections::HashSet<usize> = moved.iter().map(|m| m.0).collect();
    let moved_new: std::collections::HashSet<usize> = moved.iter().map(|m| m.1).collect();
    let stream_old: Vec<&Word> = old.iter().filter(|w| !moved_old.contains(&w.page)).collect();
    let stream_new: Vec<&Word> = new.iter().filter(|w| !moved_new.contains(&w.page)).collect();
    let mut all = changes(&stream_old, &stream_new);
    for (o, n) in &moved {
        let a: Vec<&Word> = old.iter().filter(|w| w.page == *o).collect();
        let b: Vec<&Word> = new.iter().filter(|w| w.page == *n).collect();
        all.extend(changes(&a, &b));
    }
    if !moved.is_empty() {
        all.sort_by_key(|c| c.new.page);
    }
    Comparison { changes: all, old_words: old.len(), new_words: new.len() }
}

fn changes(old: &[&Word], new: &[&Word]) -> Vec<Change> {
    let a: Vec<u64> = old.iter().map(|w| hash(&w.text)).collect();
    let b: Vec<u64> = new.iter().map(|w| hash(&w.text)).collect();
    let mut ops = Vec::with_capacity(a.len().max(b.len()));
    diff(&a, &b, &mut ops, 0);

    let mut changes = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut k = 0;
    while k < ops.len() {
        if ops.get(k) == Some(&Op::Equal) {
            i += 1;
            j += 1;
            k += 1;
            continue;
        }
        let (mut del, mut ins): (Vec<&Word>, Vec<&Word>) = (Vec::new(), Vec::new());
        while let Some(op) = ops.get(k).filter(|op| **op != Op::Equal) {
            if *op == Op::Delete {
                if let Some(w) = old.get(i) {
                    del.push(w);
                }
                i += 1;
            } else {
                if let Some(w) = new.get(j) {
                    ins.push(w);
                }
                j += 1;
            }
            k += 1;
        }
        let kind = match (del.is_empty(), ins.is_empty()) {
            (false, false) => Kind::Replaced,
            (false, true) => Kind::Deleted,
            _ => Kind::Inserted,
        };
        // Where a one-sided change sits in the other document: next to the neighbouring word.
        let old_near = old.get(i).or_else(|| old.last()).copied();
        let new_near = new.get(j).or_else(|| new.last()).copied();
        changes.push(Change { kind, old: side(&del, old_near), new: side(&ins, new_near) });
    }
    changes
}

/// The compare report's text: a summary, then each change with its pages.
pub fn report(c: &Comparison, old_name: &str, new_name: &str) -> String {
    let mut s = format!(
        "Compare Report\n\nOld file: {old_name}\nNew file: {new_name}\n\n{} changes: {} replaced, {} inserted, {} deleted.\n",
        c.changes.len(),
        c.count(Kind::Replaced),
        c.count(Kind::Inserted),
        c.count(Kind::Deleted)
    );
    if c.identical() {
        s.push_str("\nThe documents' text is identical.\n");
    }
    push_changes(&mut s, c);
    s
}

/// The compare report with the page map first: which pages were inserted, deleted, moved or
/// modified (and, when given, which look different), then the text changes.
pub fn report_with_pages(c: &Comparison, pages: &[PageMatch], visual_pages: &[(usize, usize)], old_name: &str, new_name: &str) -> String {
    let count = |st: PageStatus| pages.iter().filter(|p| p.status == st).count();
    let mut s = format!(
        "Compare Report\n\nOld file: {old_name}\nNew file: {new_name}\n\nPages: {} inserted, {} deleted, {} moved, {} modified, {} unchanged text.\n{} changes: {} replaced, {} inserted, {} deleted.\n",
        count(PageStatus::Inserted),
        count(PageStatus::Deleted),
        count(PageStatus::Moved),
        count(PageStatus::Modified),
        count(PageStatus::Same),
        c.changes.len(),
        c.count(Kind::Replaced),
        c.count(Kind::Inserted),
        c.count(Kind::Deleted)
    );
    if !visual_pages.is_empty() {
        s.push_str(&format!("{} page(s) look different.\n", visual_pages.len()));
    }
    let page = |p: Option<usize>| p.map_or("-".to_string(), |p| (p + 1).to_string());
    let changed: Vec<&PageMatch> = pages.iter().filter(|p| p.status != PageStatus::Same).collect();
    if !changed.is_empty() {
        s.push_str("\nPage map (old → new):\n");
        for p in changed {
            s.push_str(&format!("   {} → {}: {}\n", page(p.old), page(p.new), p.status.label()));
        }
    }
    for (o, n) in visual_pages {
        s.push_str(&format!("   {} → {}: looks different\n", o + 1, n + 1));
    }
    if c.identical() {
        s.push_str("\nThe documents' text is identical.\n");
    }
    push_changes(&mut s, c);
    s
}

fn push_changes(s: &mut String, c: &Comparison) {
    for (n, ch) in c.changes.iter().enumerate() {
        s.push_str(&format!("\n{}. {} (old page {}, new page {})\n", n + 1, ch.kind.label(), ch.old.page + 1, ch.new.page + 1));
        if !ch.old.text.is_empty() {
            s.push_str(&format!("   Old: {}\n", ch.old.text));
        }
        if !ch.new.text.is_empty() {
            s.push_str(&format!("   New: {}\n", ch.new.text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str, page: usize) -> Vec<Word> {
        text.split_whitespace()
            .enumerate()
            .map(|(i, t)| Word { text: t.into(), page, rect: [10.0 + 40.0 * i as f64, 700.0, 45.0 + 40.0 * i as f64, 710.0] })
            .collect()
    }

    #[test]
    fn identical_text_has_no_changes() {
        let c = compare(&words("the same words here", 0), &words("the same words here", 0));
        assert!(c.identical());
    }

    #[test]
    fn insertions_deletions_and_replacements() {
        let old = words("The quick brown fox jumps over the lazy dog", 0);
        let new = words("The quick red fox leaps over the very lazy dog today", 0);
        let c = compare(&old, &new);
        let got: Vec<(Kind, &str, &str)> = c.changes.iter().map(|x| (x.kind, x.old.text.as_str(), x.new.text.as_str())).collect();
        assert_eq!(
            got,
            [(Kind::Replaced, "brown", "red"), (Kind::Replaced, "jumps", "leaps"), (Kind::Inserted, "", "very"), (Kind::Inserted, "", "today"),]
        );
        assert_eq!(c.changes[0].new.rects, [[90.0, 700.0, 125.0, 710.0]]);
        assert_eq!(c.count(Kind::Inserted), 2);
    }

    #[test]
    fn deletions_across_pages_keep_their_pages() {
        let mut old = words("alpha beta gamma", 0);
        old.extend(words("delta epsilon", 1));
        let mut new = words("alpha gamma", 0);
        new.extend(words("delta", 1));
        let c = compare(&old, &new);
        let got: Vec<(Kind, usize, &str)> = c.changes.iter().map(|x| (x.kind, x.old.page, x.old.text.as_str())).collect();
        assert_eq!(got, [(Kind::Deleted, 0, "beta"), (Kind::Deleted, 1, "epsilon")]);
        // The deletion's place in the new document: the page of the words around it.
        assert_eq!(c.changes[1].new.page, 1);
        let r = report(&c, "v1.pdf", "v2.pdf");
        assert!(r.contains("2 changes: 0 replaced, 0 inserted, 2 deleted.") && r.contains("Old: epsilon"), "{r}");
    }

    #[test]
    fn adjacent_words_merge_into_one_rectangle_per_line() {
        let old = words("a b c", 0);
        let new = words("x y z", 0);
        let c = compare(&old, &new);
        assert_eq!(c.changes.len(), 1);
        assert_eq!(c.changes[0].new.rects.len(), 1);
    }

    #[test]
    fn long_documents_with_scattered_edits() {
        let text: Vec<String> = (0..20_000).map(|i| format!("w{i}")).collect();
        let old: Vec<Word> = text.iter().map(|t| Word { text: t.clone(), page: 0, rect: [0.0; 4] }).collect();
        let mut new = old.clone();
        for k in [100usize, 5000, 12_000, 19_000] {
            new[k].text = format!("changed{k}");
        }
        new.remove(8000);
        let c = compare(&old, &new);
        assert_eq!(c.changes.len(), 5, "{:?}", c.changes.iter().map(|x| (x.kind, &x.old.text, &x.new.text)).collect::<Vec<_>>());
        assert_eq!(c.count(Kind::Replaced), 4);
    }

    #[test]
    fn very_different_documents_finish() {
        let old: Vec<Word> = (0..30_000).map(|i| Word { text: format!("a{i}"), page: 0, rect: [0.0; 4] }).collect();
        let new: Vec<Word> = (0..30_000).map(|i| Word { text: format!("b{i}"), page: 0, rect: [0.0; 4] }).collect();
        let c = compare(&old, &new);
        assert_eq!(c.changes.len(), 1);
        assert_eq!(c.changes[0].kind, Kind::Replaced);
    }
}

/// Visual compare: the regions where two renderings of a page differ, as pixel boxes
/// `[x0, y0, x1, y1]` (y down) in `a`'s image. Both images are RGBA; `b` is sampled at the
/// same relative position when sizes differ. Pixels count as different when a channel differs
/// by more than `tolerance`; differing cells of a coarse grid are joined into regions.
pub fn visual_regions(a: (&[u8], u32, u32), b: (&[u8], u32, u32), tolerance: u8) -> Vec<[u32; 4]> {
    visual_diff(a, b, tolerance).regions
}

/// How two renderings of a page differ.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VisualDiff {
    /// Differing regions as pixel boxes `[x0, y0, x1, y1]` (y down) in the first image.
    pub regions: Vec<[u32; 4]>,
    /// Pixels of the first image that differ by more than the tolerance.
    pub changed: u64,
    /// Pixels compared.
    pub total: u64,
}

impl VisualDiff {
    /// Share of the page's pixels that differ (0–1).
    pub fn fraction(&self) -> f64 {
        if self.total == 0 { 0.0 } else { self.changed as f64 / self.total as f64 }
    }
}

/// [`visual_regions`] with the number of differing pixels, so a caller can apply a threshold
/// (e.g. ignore pages where less than 0.1 % of the pixels changed). Images larger than
/// 2²⁶ pixels (≈ 8192 × 8192) are not compared.
pub fn visual_diff(a: (&[u8], u32, u32), b: (&[u8], u32, u32), tolerance: u8) -> VisualDiff {
    let (pa, wa, ha) = a;
    let (pb, wb, hb) = b;
    let (wa, ha, wb, hb) = (wa as usize, ha as usize, wb as usize, hb as usize);
    if wa == 0 || ha == 0 || wb == 0 || hb == 0 || wa.saturating_mul(ha) > 1 << 26 || wb.saturating_mul(hb) > 1 << 26 {
        return VisualDiff::default();
    }
    const CELL: usize = 8;
    let (gw, gh) = (wa.div_ceil(CELL), ha.div_ceil(CELL));
    let mut grid = vec![false; gw * gh];
    let mut changed = 0u64;
    for y in 0..ha {
        let yb = y * hb / ha;
        for x in 0..wa {
            let xb = x * wb / wa;
            let ia = (y * wa + x) * 4;
            let ib = (yb * wb + xb) * 4;
            let (Some(ca), Some(cb)) = (pa.get(ia..ia + 3), pb.get(ib..ib + 3)) else { continue };
            if ca.iter().zip(cb).any(|(p, q)| p.abs_diff(*q) > tolerance) {
                changed += 1;
                if let Some(c) = grid.get_mut((y / CELL) * gw + x / CELL) {
                    *c = true;
                }
            }
        }
    }
    let regions = grid_regions(&grid, gw, gh, CELL, wa, ha);
    VisualDiff { regions, changed, total: (wa * ha) as u64 }
}

/// Connected differing cells → pixel boxes.
fn grid_regions(grid: &[bool], gw: usize, gh: usize, cell: usize, wa: usize, ha: usize) -> Vec<[u32; 4]> {
    // The caller bounds the image to 2²⁶ pixels, so every coordinate fits in u32.
    let (gw, gh, cell, wa, ha) = (gw as u32, gh as u32, cell as u32, wa as u32, ha as u32);
    // Connected cells (8-neighbourhood, with a one-cell gap bridged) become one region.
    let mut seen = vec![false; grid.len()];
    let mut out = Vec::new();
    for start in 0..grid.len() {
        if !grid[start] || seen[start] {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(c) = stack.pop() {
            let (cx, cy) = (c as u32 % gw, c as u32 / gw);
            x0 = x0.min(cx);
            y0 = y0.min(cy);
            x1 = x1.max(cx);
            y1 = y1.max(cy);
            for dy in -2i64..=2 {
                for dx in -2i64..=2 {
                    let (nx, ny) = (cx as i64 + dx, cy as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= gw as i64 || ny >= gh as i64 {
                        continue;
                    }
                    let n = (ny as u32 * gw + nx as u32) as usize;
                    if grid[n] && !seen[n] {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        out.push([x0 * cell, y0 * cell, ((x1 + 1) * cell).min(wa), ((y1 + 1) * cell).min(ha)]);
    }
    out
}

#[cfg(test)]
mod visual_tests {
    use super::*;

    fn image(w: u32, h: u32, boxes: &[[u32; 4]]) -> Vec<u8> {
        let mut px = vec![255u8; (w * h * 4) as usize];
        for b in boxes {
            for y in b[1]..b[3] {
                for x in b[0]..b[2] {
                    let i = ((y * w + x) * 4) as usize;
                    px[i..i + 3].copy_from_slice(&[0, 0, 0]);
                }
            }
        }
        px
    }

    #[test]
    fn finds_changed_regions() {
        let a = image(200, 200, &[[10, 10, 50, 20]]);
        let b = image(200, 200, &[[10, 10, 50, 20], [120, 150, 160, 170]]);
        assert!(visual_regions((&a, 200, 200), (&a, 200, 200), 16).is_empty());
        let r = visual_regions((&a, 200, 200), (&b, 200, 200), 16);
        assert_eq!(r, [[120, 144, 160, 176]]);
        // Two nearby marks join; distant ones don't.
        let c = image(200, 200, &[[10, 10, 50, 20], [100, 100, 104, 104], [110, 100, 114, 104], [10, 180, 14, 184]]);
        assert_eq!(visual_regions((&a, 200, 200), (&c, 200, 200), 16).len(), 2);
    }

    #[test]
    fn counts_changed_pixels_for_a_threshold() {
        let a = image(100, 100, &[]);
        let b = image(100, 100, &[[0, 0, 10, 5]]);
        let d = visual_diff((&a, 100, 100), (&b, 100, 100), 16);
        assert_eq!((d.changed, d.total), (50, 10_000));
        assert!((d.fraction() - 0.005).abs() < 1e-12);
        // A faint change under the tolerance is not counted.
        let mut faint = a.clone();
        faint[0] = 250;
        assert_eq!(visual_diff((&a, 100, 100), (&faint, 100, 100), 16).changed, 0);
        assert_eq!(visual_diff((&a, 100, 100), (&faint, 100, 100), 0).changed, 1);
        // Mismatched sizes are sampled, short buffers and empty or huge images are refused quietly.
        let half = image(50, 50, &[]);
        assert_eq!(visual_diff((&a, 100, 100), (&half, 50, 50), 16).changed, 0);
        assert_eq!(visual_diff((&a, 100, 100), (&a[..400], 100, 100), 16).changed, 0);
        assert_eq!(visual_diff((&a, 0, 100), (&a, 100, 100), 16), VisualDiff::default());
        assert_eq!(visual_diff((&a, 100_000, 100_000), (&a, 100, 100), 16), VisualDiff::default());
    }

    #[test]
    fn moved_pages_are_not_reported_as_deleted_and_inserted_text() {
        let page = |text: &str, page: usize| -> Vec<Word> {
            text.split_whitespace()
                .enumerate()
                .map(|(i, t)| Word { text: t.into(), page, rect: [40.0 * i as f64, 700.0, 40.0 * i as f64 + 30.0, 710.0] })
                .collect()
        };
        let texts = ["alpha one two three", "bravo four five six", "charlie seven eight nine ten eleven twelve thirteen fourteen fifteen"];
        let old: Vec<Word> = texts.iter().enumerate().flat_map(|(p, t)| page(t, p)).collect();
        // New order: charlie (edited), alpha, bravo.
        let new: Vec<Word> =
            [("charlie seven EIGHT nine ten eleven twelve thirteen fourteen fifteen", 0), ("alpha one two three", 1), ("bravo four five six", 2)]
                .iter()
                .flat_map(|(t, p)| page(t, *p))
                .collect();
        let tokens =
            |w: &[Word], n: usize| -> Vec<Vec<u64>> { (0..n).map(|p| w.iter().filter(|x| x.page == p).map(|x| hash(&x.text)).collect()).collect() };
        let pages = align_pages(&tokens(&old, 3), &tokens(&new, 3));
        let moved: Vec<(Option<usize>, Option<usize>)> = pages.iter().filter(|p| p.status == PageStatus::Moved).map(|p| (p.old, p.new)).collect();
        assert_eq!(moved, [(Some(2), Some(0))], "{pages:?}");
        let c = compare_aligned(&old, &new, &pages);
        let got: Vec<(Kind, &str, &str, usize)> = c.changes.iter().map(|x| (x.kind, x.old.text.as_str(), x.new.text.as_str(), x.new.page)).collect();
        assert_eq!(got, [(Kind::Replaced, "eight", "EIGHT", 0)]);
        // Without the page map the moved page is a deletion plus an insertion.
        assert!(compare(&old, &new).changes.len() >= 2);
        let r = report_with_pages(&c, &pages, &[(2, 0)], "a.pdf", "b.pdf");
        assert!(r.contains("Pages: 0 inserted, 0 deleted, 1 moved") && r.contains("3 → 1: moved") && r.contains("1 page(s) look different"), "{r}");
    }
}
