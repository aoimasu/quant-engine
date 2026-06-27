//! Ephemeral REST cache — an in-memory memo for immutable closed-window responses.
//!
//! Closed-window historical responses are immutable, so a process-lifetime memo is sufficient and
//! correct (unlike the persistent, sha-verified `qe-ingest::RawCache` for on-disk dumps). It sits *below*
//! the rate-limit handler: a hit costs no weight and no transport call. Only the client decides what is
//! cacheable — the cache itself is a dumb key→bytes map.

use std::cell::RefCell;
use std::collections::HashMap;

/// A canonical cache key for a venue request (endpoint + instrument + params + window).
pub type CacheKey = String;

/// An in-memory, process-lifetime cache of closed-window REST responses.
#[derive(Debug, Default)]
pub struct RestCache {
    entries: RefCell<HashMap<CacheKey, Vec<u8>>>,
}

impl RestCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached bytes for `key`, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.entries.borrow().get(key).cloned()
    }

    /// Store `bytes` under `key` (write-back).
    pub fn put(&self, key: CacheKey, bytes: Vec<u8>) {
        self.entries.borrow_mut().insert(key, bytes);
    }

    /// Number of cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_put_round_trips_and_overwrites() {
        let cache = RestCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.get("k"), None);

        cache.put("k".to_owned(), b"v1".to_vec());
        assert_eq!(cache.get("k").as_deref(), Some(&b"v1"[..]));
        assert_eq!(cache.len(), 1);

        cache.put("k".to_owned(), b"v2".to_vec());
        assert_eq!(cache.get("k").as_deref(), Some(&b"v2"[..]));
        assert_eq!(cache.len(), 1);
    }
}
