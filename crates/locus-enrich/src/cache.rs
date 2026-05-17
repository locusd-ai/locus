//! In-memory tagger cache for tests.

use std::collections::HashMap;
use std::sync::Mutex;

use locus_core::enrich::{EnrichError, EnrichmentCache, TaggerCache};

/// In-memory tagger cache, primarily for testing.
pub struct InMemoryTaggerCache {
    entries: Mutex<HashMap<String, EnrichmentCache>>,
}

impl InMemoryTaggerCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl TaggerCache for InMemoryTaggerCache {
    fn get(&self, content_hash: &str) -> Result<Option<EnrichmentCache>, EnrichError> {
        let map = self.entries.lock().unwrap();
        Ok(map.get(content_hash).cloned())
    }

    fn put(&self, cache: &EnrichmentCache) -> Result<(), EnrichError> {
        let mut map = self.entries.lock().unwrap();
        map.insert(cache.content_hash.clone(), cache.clone());
        Ok(())
    }

    fn invalidate_tagger(&mut self, tagger_name: &str) -> Result<(), EnrichError> {
        let mut map = self.entries.lock().unwrap();
        // Remove entries that contain results from this tagger
        map.retain(|_, entry| {
            !entry.results.iter().any(|r| r.tagger_name == tagger_name)
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use locus_core::enrich::TaggerResult;

    #[test]
    fn round_trip() {
        let cache = InMemoryTaggerCache::new();
        let entry = EnrichmentCache {
            content_hash: "abc123".into(),
            tagger_config_hash: "cfg456".into(),
            results: vec![TaggerResult {
                tagger_name: "test".into(),
                tags: vec!["topic:auth".into()],
                confidence: None,
            }],
            timestamp: 1000,
        };
        cache.put(&entry).unwrap();
        let got = cache.get("abc123").unwrap().unwrap();
        assert_eq!(got.content_hash, "abc123");
        assert_eq!(got.results.len(), 1);
        assert_eq!(got.results[0].tags, vec!["topic:auth"]);
    }

    #[test]
    fn miss_returns_none() {
        let cache = InMemoryTaggerCache::new();
        assert!(cache.get("nonexistent").unwrap().is_none());
    }
}
