//! A bounded cache of parsed objects (`core.lazy-loading`).
//!
//! Two generations approximate least-recently-used eviction in O(1): new entries go into the
//! young generation; when it reaches its entry or byte budget it becomes the old generation (the
//! previous old one is dropped) and a new young one starts. A hit in the old generation moves the
//! entry back into the young one, so objects in use (the catalog, the page tree, the current
//! page's resources) survive while a walk over thousands of pages streams through. At most two
//! generations' worth is held, whatever the size of the file.

use std::collections::HashMap;

pub(crate) struct BoundedCache<V> {
    young: HashMap<u32, (V, usize)>,
    old: HashMap<u32, (V, usize)>,
    young_bytes: usize,
    old_bytes: usize,
    /// Per generation.
    max_entries: usize,
    /// Per generation; one entry larger than this still fits (alone).
    max_bytes: usize,
}

impl<V: Clone> BoundedCache<V> {
    pub(crate) fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self { young: HashMap::new(), old: HashMap::new(), young_bytes: 0, old_bytes: 0, max_entries: max_entries.max(1), max_bytes }
    }

    pub(crate) fn get(&mut self, key: &u32) -> Option<V> {
        if let Some((v, _)) = self.young.get(key) {
            return Some(v.clone());
        }
        let (v, bytes) = self.old.remove(key)?;
        self.old_bytes = self.old_bytes.saturating_sub(bytes);
        self.insert_weighed(*key, v.clone(), bytes);
        Some(v)
    }

    /// Add `value`, which holds `bytes` of memory of its own.
    pub(crate) fn insert_weighed(&mut self, key: u32, value: V, bytes: usize) {
        if let Some((_, old)) = self.old.remove(&key) {
            self.old_bytes = self.old_bytes.saturating_sub(old);
        }
        if let Some((_, old)) = self.young.remove(&key) {
            self.young_bytes = self.young_bytes.saturating_sub(old);
        }
        if !self.young.is_empty() && (self.young.len() >= self.max_entries || self.young_bytes.saturating_add(bytes) > self.max_bytes) {
            self.old = std::mem::take(&mut self.young);
            self.old_bytes = std::mem::replace(&mut self.young_bytes, 0);
        }
        self.young_bytes = self.young_bytes.saturating_add(bytes);
        self.young.insert(key, (value, bytes));
    }

    pub(crate) fn len(&self) -> usize {
        self.young.len() + self.old.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.young.is_empty() && self.old.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn contains_key(&self, key: &u32) -> bool {
        self.young.contains_key(key) || self.old.contains_key(key)
    }

    /// Memory the entries hold of their own, as weighed when inserted.
    pub(crate) fn bytes(&self) -> usize {
        self.young_bytes.saturating_add(self.old_bytes)
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.young.values().chain(self.old.values()).map(|(v, _)| v)
    }

    pub(crate) fn clear(&mut self) {
        self.young.clear();
        self.old.clear();
        self.young_bytes = 0;
        self.old_bytes = 0;
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&u32) -> bool) {
        self.young.retain(|k, _| keep(k));
        self.old.retain(|k, _| keep(k));
        self.young_bytes = self.young.values().fold(0usize, |n, (_, b)| n.saturating_add(*b));
        self.old_bytes = self.old.values().fold(0usize, |n, (_, b)| n.saturating_add(*b));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_and_bytes_stay_within_two_generations() {
        let mut c = BoundedCache::new(4, 1000);
        for k in 0..100u32 {
            c.insert_weighed(k, k, 10);
            assert!(c.len() <= 8, "at most two generations of entries");
        }
        // The most recent entries are kept.
        assert_eq!(c.get(&99), Some(99));
        assert_eq!(c.get(&0), None);
        // A hit in the old generation survives the next rotation.
        let mut c = BoundedCache::new(3, usize::MAX);
        for k in 0..3 {
            c.insert_weighed(k, k, 0);
        }
        c.insert_weighed(3, 3, 0); // 0..3 old, 3 young
        assert_eq!(c.get(&0), Some(0)); // promoted
        c.insert_weighed(4, 4, 0); // young: 3, 0, 4
        c.insert_weighed(5, 5, 0); // rotates: old 3, 0, 4; young 5
        assert!(c.contains_key(&0), "a used entry is not evicted with its generation");
        // Bytes: one large entry rotates the rest out, and is held alone.
        let mut c = BoundedCache::new(100, 100);
        c.insert_weighed(1, 1, 60);
        c.insert_weighed(2, 2, 60);
        c.insert_weighed(3, 3, 500);
        assert!(c.bytes() <= 560 && !c.contains_key(&1));
        c.retain(|k| *k == 3);
        assert_eq!((c.len(), c.bytes()), (1, 500));
        c.clear();
        assert!(c.is_empty() && c.bytes() == 0);
    }
}
