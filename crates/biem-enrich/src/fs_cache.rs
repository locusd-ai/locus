//! Filesystem-backed tagger cache.
//!
//! Stores enrichment results as JSON files keyed by blake3 content hash.
//! Cache directory: `<data_dir>/cache/taggers/`

use std::fs;
use std::path::{Path, PathBuf};

use biem_core::enrich::{EnrichError, EnrichmentCache, TaggerCache};

/// Filesystem-backed tagger cache. One JSON file per content hash.
pub struct FsTaggerCache {
    cache_dir: PathBuf,
}

impl FsTaggerCache {
    pub fn new(data_dir: &Path) -> Result<Self, EnrichError> {
        let cache_dir = data_dir.join("cache").join("taggers");
        fs::create_dir_all(&cache_dir).map_err(|e| EnrichError::CacheIo(e.to_string()))?;
        Ok(Self { cache_dir })
    }

    fn path_for(&self, content_hash: &str) -> PathBuf {
        self.cache_dir.join(format!("{content_hash}.json"))
    }
}

impl TaggerCache for FsTaggerCache {
    fn get(&self, content_hash: &str) -> Result<Option<EnrichmentCache>, EnrichError> {
        let path = self.path_for(content_hash);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path).map_err(|e| EnrichError::CacheIo(e.to_string()))?;
        let entry: EnrichmentCache =
            serde_json::from_str(&data).map_err(|e| EnrichError::CacheSerialization(e.to_string()))?;
        Ok(Some(entry))
    }

    fn put(&self, cache: &EnrichmentCache) -> Result<(), EnrichError> {
        let path = self.path_for(&cache.content_hash);
        let data =
            serde_json::to_string_pretty(cache).map_err(|e| EnrichError::CacheSerialization(e.to_string()))?;
        fs::write(&path, data).map_err(|e| EnrichError::CacheIo(e.to_string()))?;
        Ok(())
    }

    fn invalidate_tagger(&mut self, tagger_name: &str) -> Result<(), EnrichError> {
        // Read each cache file, remove entries containing the tagger, rewrite or delete
        let entries = fs::read_dir(&self.cache_dir).map_err(|e| EnrichError::CacheIo(e.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|e| EnrichError::CacheIo(e.to_string()))?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                let data = fs::read_to_string(&path).map_err(|e| EnrichError::CacheIo(e.to_string()))?;
                if let Ok(mut cached) = serde_json::from_str::<EnrichmentCache>(&data) {
                    let before = cached.results.len();
                    cached.results.retain(|r| r.tagger_name != tagger_name);
                    if cached.results.len() != before {
                        if cached.results.is_empty() {
                            fs::remove_file(&path).map_err(|e| EnrichError::CacheIo(e.to_string()))?;
                        } else {
                            let data = serde_json::to_string_pretty(&cached)
                                .map_err(|e| EnrichError::CacheSerialization(e.to_string()))?;
                            fs::write(&path, data).map_err(|e| EnrichError::CacheIo(e.to_string()))?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use biem_core::enrich::TaggerResult;
    use tempfile::TempDir;

    fn make_cache() -> (TempDir, FsTaggerCache) {
        let tmp = TempDir::new().unwrap();
        let cache = FsTaggerCache::new(tmp.path()).unwrap();
        (tmp, cache)
    }

    fn sample_entry(hash: &str) -> EnrichmentCache {
        EnrichmentCache {
            content_hash: hash.into(),
            tagger_config_hash: "cfg1".into(),
            results: vec![TaggerResult {
                tagger_name: "size".into(),
                tags: vec!["size:small".into()],
                confidence: None,
            }],
            timestamp: 1000,
        }
    }

    #[test]
    fn round_trip() {
        let (_tmp, cache) = make_cache();
        let entry = sample_entry("abc123");
        cache.put(&entry).unwrap();
        let got = cache.get("abc123").unwrap().unwrap();
        assert_eq!(got.content_hash, "abc123");
        assert_eq!(got.results[0].tags, vec!["size:small"]);
    }

    #[test]
    fn miss_returns_none() {
        let (_tmp, cache) = make_cache();
        assert!(cache.get("nonexistent").unwrap().is_none());
    }

    #[test]
    fn config_hash_mismatch_still_returns_entry() {
        // Cache returns the entry regardless — the pipeline checks config_hash
        let (_tmp, cache) = make_cache();
        let entry = sample_entry("abc123");
        cache.put(&entry).unwrap();
        let got = cache.get("abc123").unwrap().unwrap();
        assert_eq!(got.tagger_config_hash, "cfg1");
    }

    #[test]
    fn invalidate_removes_tagger_results() {
        let (_tmp, mut cache) = make_cache();
        let entry = EnrichmentCache {
            content_hash: "abc123".into(),
            tagger_config_hash: "cfg1".into(),
            results: vec![
                TaggerResult {
                    tagger_name: "size".into(),
                    tags: vec!["size:small".into()],
                    confidence: None,
                },
                TaggerResult {
                    tagger_name: "topic".into(),
                    tags: vec!["topic:auth".into()],
                    confidence: None,
                },
            ],
            timestamp: 1000,
        };
        cache.put(&entry).unwrap();
        cache.invalidate_tagger("size").unwrap();
        let got = cache.get("abc123").unwrap().unwrap();
        assert_eq!(got.results.len(), 1);
        assert_eq!(got.results[0].tagger_name, "topic");
    }
}
