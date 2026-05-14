//! TagPipeline — orchestrates taggers with caching.

use std::path::Path;

use tracing::{info, instrument, warn};

use biem_core::enrich::{EnrichError, EnrichmentCache, Tagger, TaggerCache};
use biem_core::types::{ParseResult, SourceType, Timestamp};

/// Orchestrates taggers with caching. Runs all applicable taggers
/// on a parse result and returns the merged set of inferred tags.
pub struct TagPipeline {
    taggers: Vec<Box<dyn Tagger>>,
    cache: Box<dyn TaggerCache>,
    force: bool,
}

impl TagPipeline {
    pub fn new(taggers: Vec<Box<dyn Tagger>>, cache: Box<dyn TaggerCache>) -> Self {
        Self { taggers, cache, force: false }
    }

    /// Set force mode — skip cache on all enrichment calls.
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// Compute a config hash that changes when the tagger set changes.
    /// Used for cache invalidation.
    fn config_hash(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        for t in &self.taggers {
            hasher.update(t.name().as_bytes());
        }
        hasher.finalize().to_hex()[..16].to_string()
    }

    /// Run all applicable taggers on a file, returning inferred tags.
    /// Uses cache when available; caches results on miss.
    #[instrument(skip(self, content, parse_result), fields(path = %path.display()))]
    pub fn enrich(
        &self,
        path: &Path,
        content: &[u8],
        parse_result: &ParseResult,
        source_type: &SourceType,
    ) -> Result<Vec<String>, EnrichError> {
        let content_hash = blake3::hash(content).to_hex().to_string();
        let config_hash = self.config_hash();

        // Check cache (unless force mode)
        if !self.force {
            if let Some(cached) = self.cache.get(&content_hash)? {
                if cached.tagger_config_hash == config_hash {
                    let tags: Vec<String> = cached
                        .results
                        .iter()
                        .flat_map(|r| r.tags.iter().cloned())
                        .collect();
                    info!(tags = tags.len(), "cache hit");
                    return Ok(tags);
                }
            }
        }

        // Cache miss — run taggers
        let mut results = Vec::new();
        for tagger in &self.taggers {
            if !tagger.applies_to(source_type) {
                continue;
            }
            match tagger.tag(path, content, parse_result) {
                Ok(result) => {
                    info!(tagger = tagger.name(), tags = result.tags.len(), "tagger ran");
                    results.push(result);
                }
                Err(e) => {
                    warn!(tagger = tagger.name(), error = %e, "tagger failed, skipping");
                }
            }
        }

        // Merge all tags
        let tags: Vec<String> = results
            .iter()
            .flat_map(|r| r.tags.iter().cloned())
            .collect();

        // Cache the results
        let cache_entry = EnrichmentCache {
            content_hash,
            tagger_config_hash: config_hash,
            results,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as Timestamp,
        };
        if let Err(e) = self.cache.put(&cache_entry) {
            warn!(error = %e, "failed to cache tagger results");
        }

        Ok(tags)
    }

    /// List the names of all registered taggers.
    pub fn tagger_names(&self) -> Vec<&str> {
        self.taggers.iter().map(|t| t.name()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryTaggerCache;
    use biem_core::enrich::TaggerResult;
    use std::collections::HashMap;

    /// A mock tagger that always returns fixed tags.
    struct MockTagger {
        name: String,
        tags: Vec<String>,
    }

    impl Tagger for MockTagger {
        fn name(&self) -> &str {
            &self.name
        }
        fn applies_to(&self, _source: &SourceType) -> bool {
            true
        }
        fn tag(
            &self,
            _path: &Path,
            _content: &[u8],
            _parse_result: &ParseResult,
        ) -> Result<TaggerResult, EnrichError> {
            Ok(TaggerResult {
                tagger_name: self.name.clone(),
                tags: self.tags.clone(),
                confidence: None,
            })
        }
    }

    fn empty_parse_result() -> ParseResult {
        ParseResult {
            chunks: vec![],
            tags: vec![],
            links: vec![],
            auto_type: None,
            frontmatter: HashMap::new(),
        }
    }

    #[test]
    fn empty_pipeline_returns_no_tags() {
        let pipeline = TagPipeline::new(vec![], Box::new(InMemoryTaggerCache::new()));
        let result = pipeline
            .enrich(
                Path::new("test.md"),
                b"hello",
                &empty_parse_result(),
                &SourceType::Obsidian,
            )
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn pipeline_returns_tagger_tags() {
        let tagger = MockTagger {
            name: "mock".into(),
            tags: vec!["topic:auth".into(), "size:small".into()],
        };
        let pipeline = TagPipeline::new(
            vec![Box::new(tagger)],
            Box::new(InMemoryTaggerCache::new()),
        );
        let result = pipeline
            .enrich(
                Path::new("test.md"),
                b"hello",
                &empty_parse_result(),
                &SourceType::Obsidian,
            )
            .unwrap();
        assert_eq!(result, vec!["topic:auth", "size:small"]);
    }

    #[test]
    fn cache_hit_skips_tagger() {
        let tagger = MockTagger {
            name: "mock".into(),
            tags: vec!["topic:auth".into()],
        };
        let cache = InMemoryTaggerCache::new();
        let pipeline = TagPipeline::new(vec![Box::new(tagger)], Box::new(cache));
        let path = Path::new("test.md");
        let content = b"hello world";
        let pr = empty_parse_result();

        // First call: cache miss, runs tagger
        let r1 = pipeline.enrich(path, content, &pr, &SourceType::Obsidian).unwrap();
        assert_eq!(r1, vec!["topic:auth"]);

        // Second call: same content → cache hit (tagger not re-run,
        // but returns same result from cache)
        let r2 = pipeline.enrich(path, content, &pr, &SourceType::Obsidian).unwrap();
        assert_eq!(r2, vec!["topic:auth"]);
    }

    #[test]
    fn multiple_taggers_merge_results() {
        let t1 = MockTagger {
            name: "size".into(),
            tags: vec!["size:small".into()],
        };
        let t2 = MockTagger {
            name: "topic".into(),
            tags: vec!["topic:auth".into(), "topic:jwt".into()],
        };
        let pipeline = TagPipeline::new(
            vec![Box::new(t1), Box::new(t2)],
            Box::new(InMemoryTaggerCache::new()),
        );
        let result = pipeline
            .enrich(
                Path::new("test.md"),
                b"hello",
                &empty_parse_result(),
                &SourceType::Obsidian,
            )
            .unwrap();
        assert_eq!(result, vec!["size:small", "topic:auth", "topic:jwt"]);
    }
}
