//! A small least-recently-used cache of rendered pages.
//!
//! Cheap to add now, painful to retrofit: once the GUI is scrolling a document
//! it will ask for the same page at the same zoom many times a second, and
//! re-rasterising each time is the difference between smooth and unusable.

use std::collections::HashMap;
use std::sync::Arc;

use super::RenderedPage;

/// `(page index, scale key)`.
pub(super) type Key = (usize, (u8, i64));

/// Hit and miss counts, for tuning the capacity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub entries: usize,
    pub capacity: usize,
}

impl CacheStats {
    /// Proportion of lookups served from the cache, or `None` before any
    /// lookup has happened.
    pub fn hit_rate(&self) -> Option<f64> {
        let total = self.hits + self.misses;
        (total > 0).then(|| self.hits as f64 / total as f64)
    }
}

pub(super) struct RenderCache {
    capacity: usize,
    entries: HashMap<Key, Entry>,
    /// Monotonic counter standing in for a clock: the lowest value is the least
    /// recently used. A counter avoids both the cost of reading a clock and the
    /// tie-breaking problem when two lookups land in the same instant.
    tick: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
}

struct Entry {
    page: Arc<RenderedPage>,
    last_used: u64,
}

impl RenderCache {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            tick: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    pub(super) fn get(&mut self, key: &Key) -> Option<Arc<RenderedPage>> {
        self.tick += 1;
        let tick = self.tick;

        match self.entries.get_mut(key) {
            Some(entry) => {
                entry.last_used = tick;
                self.hits += 1;
                Some(Arc::clone(&entry.page))
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    pub(super) fn insert(&mut self, key: Key, page: Arc<RenderedPage>) {
        if self.capacity == 0 {
            return;
        }

        self.tick += 1;
        let last_used = self.tick;

        if !self.entries.contains_key(&key) && self.entries.len() >= self.capacity {
            self.evict_oldest();
        }

        self.entries.insert(key, Entry { page, last_used });
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(super) fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            entries: self.entries.len(),
            capacity: self.capacity,
        }
    }

    fn evict_oldest(&mut self) {
        let oldest = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| *key);

        if let Some(key) = oldest {
            self.entries.remove(&key);
            self.evictions += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(width: u32) -> Arc<RenderedPage> {
        Arc::new(RenderedPage {
            width,
            height: 1,
            channels: 4,
            pixels: vec![0; width as usize * 4],
        })
    }

    fn key(page: usize) -> Key {
        (page, (0, 150_000))
    }

    #[test]
    fn a_stored_page_comes_back() {
        let mut cache = RenderCache::new(4);
        cache.insert(key(0), page(10));

        assert_eq!(cache.get(&key(0)).unwrap().width, 10);
        assert!(cache.get(&key(1)).is_none());
    }

    #[test]
    fn the_least_recently_used_entry_is_evicted() {
        let mut cache = RenderCache::new(2);
        cache.insert(key(0), page(1));
        cache.insert(key(1), page(2));

        // Touching page 0 makes page 1 the oldest.
        assert!(cache.get(&key(0)).is_some());
        cache.insert(key(2), page(3));

        assert!(
            cache.get(&key(0)).is_some(),
            "recently used page was evicted"
        );
        assert!(cache.get(&key(1)).is_none(), "oldest page should be gone");
        assert!(cache.get(&key(2)).is_some());
    }

    #[test]
    fn replacing_an_entry_does_not_evict() {
        let mut cache = RenderCache::new(2);
        cache.insert(key(0), page(1));
        cache.insert(key(1), page(2));
        cache.insert(key(1), page(3));

        assert_eq!(cache.stats().entries, 2);
        assert_eq!(cache.stats().evictions, 0);
        assert_eq!(cache.get(&key(1)).unwrap().width, 3);
    }

    #[test]
    fn a_zero_capacity_cache_stores_nothing() {
        let mut cache = RenderCache::new(0);
        cache.insert(key(0), page(1));
        assert!(cache.get(&key(0)).is_none());
    }

    #[test]
    fn statistics_track_hits_and_misses() {
        let mut cache = RenderCache::new(1);
        assert_eq!(cache.stats().hit_rate(), None);

        cache.insert(key(0), page(1));
        cache.get(&key(0));
        cache.get(&key(9));

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hit_rate(), Some(0.5));
    }

    #[test]
    fn clearing_keeps_the_counters() {
        let mut cache = RenderCache::new(2);
        cache.insert(key(0), page(1));
        cache.get(&key(0));
        cache.clear();

        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().hits, 1);
    }
}
