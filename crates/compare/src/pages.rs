//! Page alignment: which page of the new document is which page of the old one.
//!
//! Each page has a fingerprint — the hashes of its words, or for a page without text the cells
//! of a coarse picture of it ([`picture_tokens`]). Pages with identical fingerprints are lined
//! up with the same diff the words use (so pages added or removed in the middle do not shift
//! everything after them); the pages left over between two matched runs are paired when they
//! share enough of their words (a modified page); what is still left over is either a page that
//! moved (an identical or near-identical page elsewhere in the other document), an inserted page
//! or a deleted page.
//!
//! Work is bounded: one diff over the page hashes, a fixed window of candidates when pairing
//! leftover pages, and a fixed budget of comparisons when looking for moved modified pages.

use std::collections::HashMap;

use crate::{Op, diff};

/// What happened to a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageStatus {
    /// The same text (or picture) at the same place in the page order.
    Same,
    /// Paired with the page at the same place in the order, but its text changed.
    Modified,
    /// Only in the new document.
    Inserted,
    /// Only in the old document.
    Deleted,
    /// Found at a different place in the page order (possibly also modified: see
    /// [`PageMatch::similarity`]).
    Moved,
}

impl PageStatus {
    pub fn label(self) -> &'static str {
        match self {
            PageStatus::Same => "same",
            PageStatus::Modified => "modified",
            PageStatus::Inserted => "inserted",
            PageStatus::Deleted => "deleted",
            PageStatus::Moved => "moved",
        }
    }
}

/// One row of the page map. Page numbers are 0-based indices into the fingerprint slices given
/// to [`align_pages`].
#[derive(Clone, Debug, PartialEq)]
pub struct PageMatch {
    pub status: PageStatus,
    pub old: Option<usize>,
    pub new: Option<usize>,
    /// Share of distinct words both pages have (Jaccard index, 0–1); 1 for identical pages,
    /// 0 for inserted and deleted ones.
    pub similarity: f64,
}

/// Pages left over between two matched runs are paired when at least this share of their
/// distinct words agree.
const PAIR_SIMILARITY: f64 = 0.5;
/// A gap of exactly one old and one new page (a page rewritten in place) is paired as modified
/// from this lower share, when neither page was found moved; pages sharing less stay deleted
/// and inserted.
const IN_PLACE_SIMILARITY: f64 = 0.25;
/// A modified page found elsewhere in the order counts as moved at this similarity.
const MOVE_SIMILARITY: f64 = 0.8;
/// How many leftover new pages each leftover old page is tried against, in order.
const PAIR_WINDOW: usize = 32;
/// Most page-pair similarities computed when looking for moved, modified pages.
const MOVE_BUDGET: usize = 4096;

fn page_hash(tokens: &[u64]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tokens.hash(&mut h);
    h.finish()
}

/// Jaccard index of two sorted, de-duplicated token lists.
fn jaccard(a: &[u64], b: &[u64]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let (mut i, mut j, mut both) = (0, 0, 0usize);
    while let (Some(x), Some(y)) = (a.get(i), b.get(j)) {
        match x.cmp(y) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both += 1;
                i += 1;
                j += 1;
            }
        }
    }
    let union = a.len() + b.len() - both;
    if union == 0 { 1.0 } else { both as f64 / union as f64 }
}

fn token_set(tokens: &[u64]) -> Vec<u64> {
    let mut s = tokens.to_vec();
    s.sort_unstable();
    s.dedup();
    s
}

/// Line up the pages of two documents from their fingerprints. The result lists every new page
/// in new order, with each deleted old page placed where it used to be.
pub fn align_pages(old: &[Vec<u64>], new: &[Vec<u64>]) -> Vec<PageMatch> {
    let ha: Vec<u64> = old.iter().map(|t| page_hash(t)).collect();
    let hb: Vec<u64> = new.iter().map(|t| page_hash(t)).collect();
    let sa: Vec<Vec<u64>> = old.iter().map(|t| token_set(t)).collect();
    let sb: Vec<Vec<u64>> = new.iter().map(|t| token_set(t)).collect();
    let sim = |i: usize, j: usize| match (sa.get(i), sb.get(j)) {
        (Some(a), Some(b)) => jaccard(a, b),
        _ => 0.0,
    };

    // For each new page: its old partner, status and similarity.
    let mut partner: Vec<Option<(usize, PageStatus, f64)>> = vec![None; new.len()];
    let mut old_used = vec![false; old.len()];
    type Partner = [Option<(usize, PageStatus, f64)>];
    fn pair(i: usize, j: usize, status: PageStatus, s: f64, partner: &mut Partner, old_used: &mut [bool]) {
        if let (Some(p), Some(u)) = (partner.get_mut(j), old_used.get_mut(i)) {
            *p = Some((i, status, s));
            *u = true;
        }
    }

    let mut ops = Vec::with_capacity(old.len().max(new.len()));
    diff(&ha, &hb, &mut ops, 0);
    // One-for-one gaps left unpaired, tried again after the moves.
    let mut in_place: Vec<(usize, usize)> = Vec::new();
    let (mut i, mut j, mut k) = (0usize, 0usize, 0usize);
    while k < ops.len() {
        if ops.get(k) == Some(&Op::Equal) {
            pair(i, j, PageStatus::Same, 1.0, &mut partner, &mut old_used);
            i += 1;
            j += 1;
            k += 1;
            continue;
        }
        let (mut dels, mut ins) = (Vec::new(), Vec::new());
        while let Some(op) = ops.get(k).filter(|op| **op != Op::Equal) {
            if *op == Op::Delete {
                dels.push(i);
                i += 1;
            } else {
                ins.push(j);
                j += 1;
            }
            k += 1;
        }
        // Pair the gap's pages in order: each old page with the most similar of the next few
        // new pages, when similar enough.
        let mut next = 0usize;
        for &d in &dels {
            let window = ins.get(next..ins.len().min(next + PAIR_WINDOW)).unwrap_or(&[]);
            let best = window
                .iter()
                .enumerate()
                .map(|(t, &n)| (t, n, sim(d, n)))
                .filter(|(_, _, s)| *s >= PAIR_SIMILARITY)
                .max_by(|x, y| x.2.total_cmp(&y.2).then(y.0.cmp(&x.0)));
            if let Some((t, n, s)) = best {
                let same = ha.get(d) == hb.get(n);
                pair(d, n, if same { PageStatus::Same } else { PageStatus::Modified }, if same { 1.0 } else { s }, &mut partner, &mut old_used);
                next += t + 1;
            }
        }
        if let ([d], [n]) = (dels.as_slice(), ins.as_slice())
            && partner.get(*n).is_some_and(Option::is_none)
        {
            in_place.push((*d, *n));
        }
    }

    // Moves: an unpaired old page identical to an unpaired new page anywhere…
    let mut by_hash: HashMap<u64, Vec<usize>> = HashMap::new();
    for (n, h) in hb.iter().enumerate().rev() {
        if partner.get(n).is_some_and(Option::is_none) {
            by_hash.entry(*h).or_default().push(n);
        }
    }
    for (d, h) in ha.iter().enumerate() {
        if old_used.get(d).copied().unwrap_or(true) {
            continue;
        }
        if let Some(n) = by_hash.get_mut(h).and_then(Vec::pop) {
            pair(d, n, PageStatus::Moved, 1.0, &mut partner, &mut old_used);
        }
    }
    // …or, within a budget, nearly identical to one.
    let left_old: Vec<usize> = (0..old.len()).filter(|d| !old_used.get(*d).copied().unwrap_or(true)).collect();
    let left_new: Vec<usize> = (0..new.len()).filter(|n| partner.get(*n).is_some_and(Option::is_none)).collect();
    if left_old.len().saturating_mul(left_new.len()) <= MOVE_BUDGET {
        for d in left_old {
            let best = left_new
                .iter()
                .filter(|n| partner.get(**n).is_some_and(Option::is_none))
                .map(|&n| (n, sim(d, n)))
                .filter(|(_, s)| *s >= MOVE_SIMILARITY)
                .max_by(|x, y| x.1.total_cmp(&y.1));
            if let Some((n, s)) = best {
                pair(d, n, PageStatus::Moved, s, &mut partner, &mut old_used);
            }
        }
    }

    // A page rewritten in place, sharing enough words with the one it replaced.
    for (d, n) in in_place {
        let free = !old_used.get(d).copied().unwrap_or(true) && partner.get(n).is_some_and(Option::is_none);
        let s = sim(d, n);
        if free && s >= IN_PLACE_SIMILARITY {
            pair(d, n, PageStatus::Modified, s, &mut partner, &mut old_used);
        }
    }

    // The page map in new order, deleted pages where they were.
    let mut out = Vec::with_capacity(old.len() + new.len());
    let mut deleted_until = 0usize;
    let mut emit_deleted = |upto: usize, out: &mut Vec<PageMatch>| {
        while deleted_until < upto.min(old.len()) {
            if !old_used.get(deleted_until).copied().unwrap_or(true) {
                out.push(PageMatch { status: PageStatus::Deleted, old: Some(deleted_until), new: None, similarity: 0.0 });
            }
            deleted_until += 1;
        }
    };
    // For each new page: the old page of the next in-order pair at or after it, so the pages
    // deleted from a gap come before the pages inserted into it.
    let mut next_old = vec![old.len(); partner.len()];
    let mut upcoming = old.len();
    for (n, p) in partner.iter().enumerate().rev() {
        if let Some((o, status, _)) = p
            && *status != PageStatus::Moved
        {
            upcoming = *o;
        }
        if let Some(x) = next_old.get_mut(n) {
            *x = upcoming;
        }
    }
    for (n, p) in partner.iter().enumerate() {
        match *p {
            Some((o, status, similarity)) => {
                if status != PageStatus::Moved {
                    emit_deleted(o, &mut out);
                }
                out.push(PageMatch { status, old: Some(o), new: Some(n), similarity });
            }
            None => {
                emit_deleted(next_old.get(n).copied().unwrap_or(old.len()), &mut out);
                out.push(PageMatch { status: PageStatus::Inserted, old: None, new: Some(n), similarity: 0.0 });
            }
        }
    }
    emit_deleted(old.len(), &mut out);
    out
}

/// The word fingerprints of pages `first .. first + count` (index 0 = page `first`). Pages
/// without words get an empty list: give them [`picture_tokens`] instead.
pub fn page_fingerprints(words: &[crate::Word], first: usize, count: usize) -> Vec<Vec<u64>> {
    let mut out = vec![Vec::new(); count];
    for w in words {
        if let Some(p) = w.page.checked_sub(first).and_then(|p| out.get_mut(p)) {
            p.push(crate::hash(&w.text));
        }
    }
    out
}

/// A fingerprint for a page without text: a 16 × 16 grid of its picture, each cell's mean
/// lightness in 8 levels. `rgba` is `width` × `height` RGBA8.
pub fn picture_tokens(rgba: &[u8], width: u32, height: u32) -> Vec<u64> {
    const GRID: usize = 16;
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let mut sum = [[0u64; GRID]; GRID];
    let mut count = [[0u64; GRID]; GRID];
    for (n, px) in rgba.as_chunks::<4>().0.iter().take(w.saturating_mul(h)).enumerate() {
        let (x, y) = (n % w, n / w);
        let (cx, cy) = (x * GRID / w, y * GRID / h);
        let l = (u64::from(px[0]) * 30 + u64::from(px[1]) * 59 + u64::from(px[2]) * 11) / 100;
        if let (Some(s), Some(c)) = (sum.get_mut(cy).and_then(|r| r.get_mut(cx)), count.get_mut(cy).and_then(|r| r.get_mut(cx))) {
            *s += l;
            *c += 1;
        }
    }
    let mut out = Vec::with_capacity(GRID * GRID + 1);
    // Marks the fingerprint as a picture, so it never equals a page whose only word hashes alike.
    out.push(u64::MAX);
    for cy in 0..GRID {
        for cx in 0..GRID {
            let level = sum[cy][cx].checked_div(count[cy][cx]).unwrap_or(255) / 32;
            out.push((((cy * GRID + cx) as u64) << 8) | level);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(text: &str) -> Vec<u64> {
        text.split_whitespace().map(crate::hash).collect()
    }

    fn map(m: &[PageMatch]) -> Vec<(PageStatus, Option<usize>, Option<usize>)> {
        m.iter().map(|p| (p.status, p.old, p.new)).collect()
    }

    const A: &str = "alpha one two three four five";
    const B: &str = "bravo six seven eight nine ten";
    const C: &str = "charlie eleven twelve thirteen fourteen fifteen";
    const D: &str = "delta sixteen seventeen eighteen nineteen twenty";

    #[test]
    fn identical_documents_line_up() {
        let pages = [page(A), page(B), page(C)];
        let m = align_pages(&pages, &pages);
        assert!(m.iter().all(|p| p.status == PageStatus::Same && p.old == p.new && p.similarity == 1.0));
    }

    #[test]
    fn inserted_and_deleted_pages() {
        let old = [page(A), page(B), page(C)];
        let new = [page(A), page(D), page(C)];
        // B and D share nothing: B deleted, D inserted, C still matched although it shifted none.
        assert_eq!(
            map(&align_pages(&old, &new)),
            [
                (PageStatus::Same, Some(0), Some(0)),
                (PageStatus::Deleted, Some(1), None),
                (PageStatus::Inserted, None, Some(1)),
                (PageStatus::Same, Some(2), Some(2))
            ]
        );
        // A page inserted at the front does not shift the rest into "modified".
        let new = [page(D), page(A), page(B), page(C)];
        assert_eq!(
            map(&align_pages(&old, &new)),
            [
                (PageStatus::Inserted, None, Some(0)),
                (PageStatus::Same, Some(0), Some(1)),
                (PageStatus::Same, Some(1), Some(2)),
                (PageStatus::Same, Some(2), Some(3))
            ]
        );
    }

    #[test]
    fn modified_pages_pair_in_place() {
        let old = [page(A), page(B), page(C)];
        let new = [page(A), page("bravo six seven eight nine eleventy"), page(C)];
        let m = align_pages(&old, &new);
        assert_eq!(m[1].status, PageStatus::Modified);
        assert_eq!((m[1].old, m[1].new), (Some(1), Some(1)));
        assert!(m[1].similarity > 0.5 && m[1].similarity < 1.0, "{}", m[1].similarity);
    }

    #[test]
    fn a_page_rewritten_in_place_is_modified() {
        // A one-page edit sharing 4 of 11 distinct words: modified, not deleted and inserted.
        let old = [page("delivery within five days. returns accepted.")];
        let new = [page("delivery within three days. returns accepted for a week.")];
        let m = align_pages(&old, &new);
        assert_eq!(map(&m), [(PageStatus::Modified, Some(0), Some(0))]);
        assert!((m[0].similarity - 4.0 / 11.0).abs() < 1e-9, "{}", m[0].similarity);
        // Between matched pages too (2 of 7 words); a page sharing nothing still counts as
        // deleted and inserted (inserted_and_deleted_pages).
        let old = [page(A), page("bravo six seven eight"), page(C)];
        let new = [page(A), page("bravo six one two three"), page(C)];
        assert_eq!(map(&align_pages(&old, &new))[1], (PageStatus::Modified, Some(1), Some(1)));
        // When the old page turns up moved elsewhere, it is not also paired in place.
        let old = [page(A), page(B), page(C), page(D)];
        let new = [page(A), page("bravo six seven twelve"), page(C), page(B)];
        let m = map(&align_pages(&old, &new));
        assert!(m.contains(&(PageStatus::Moved, Some(1), Some(3))), "{m:?}");
        assert!(m.contains(&(PageStatus::Inserted, None, Some(1))), "{m:?}");
    }

    #[test]
    fn moved_pages() {
        let old = [page(A), page(B), page(C), page(D)];
        let new = [page(A), page(C), page(D), page(B)];
        let m = align_pages(&old, &new);
        assert_eq!(
            map(&m),
            [
                (PageStatus::Same, Some(0), Some(0)),
                (PageStatus::Same, Some(2), Some(1)),
                (PageStatus::Same, Some(3), Some(2)),
                (PageStatus::Moved, Some(1), Some(3))
            ]
        );
        // Moved and slightly edited.
        let new = [page(A), page(C), page(D), page("bravo six seven eight nine ten eleven")];
        let m = align_pages(&old, &new);
        assert_eq!(m[3].status, PageStatus::Moved);
        assert!(m[3].similarity < 1.0);
    }

    #[test]
    fn empty_inputs() {
        assert!(align_pages(&[], &[]).is_empty());
        assert_eq!(map(&align_pages(&[page(A)], &[])), [(PageStatus::Deleted, Some(0), None)]);
        assert_eq!(map(&align_pages(&[], &[page(A)])), [(PageStatus::Inserted, None, Some(0))]);
    }

    #[test]
    fn many_pages_finish() {
        let old: Vec<Vec<u64>> = (0..5000).map(|i| page(&format!("p{i} common words here"))).collect();
        let mut new: Vec<Vec<u64>> = (0..5000).rev().map(|i| page(&format!("p{i} common words here x"))).collect();
        new.truncate(4000);
        let m = align_pages(&old, &new);
        assert_eq!(m.iter().filter(|p| p.new.is_some()).count(), 4000);
        assert_eq!(m.iter().filter(|p| p.old.is_some()).count(), 5000);
    }

    #[test]
    fn picture_fingerprints() {
        let white = vec![255u8; 64 * 64 * 4];
        let mut dark = white.clone();
        for px in dark.as_chunks_mut::<4>().0.iter_mut().take(64 * 32) {
            px[..3].copy_from_slice(&[0, 0, 0]);
        }
        assert_eq!(picture_tokens(&white, 64, 64), picture_tokens(&white, 64, 64));
        assert_ne!(picture_tokens(&white, 64, 64), picture_tokens(&dark, 64, 64));
        assert!(picture_tokens(&[], 0, 0).is_empty());
        // Short buffers do not panic.
        assert_eq!(picture_tokens(&white[..10], 64, 64).len(), 257);
    }
}
