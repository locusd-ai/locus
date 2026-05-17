//! Enrichment types and traits for the tagger pipeline.
//!
//! Taggers produce additional bitmap keys from parse results.
//! Results are cached per file content hash to avoid redundant work.

use std::path::Path;

use crate::{ParseResult, SourceType, Timestamp};

// ── Types ────────────────────────────────────────────────────────

/// Result from a single tagger run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaggerResult {
    /// Name of the tagger that produced these tags.
    pub tagger_name: String,
    /// Inferred bitmap keys (e.g. "topic:auth", "size:small").
    pub tags: Vec<String>,
    /// Confidence score (0.0–1.0). `None` for deterministic/rule-based taggers.
    pub confidence: Option<f32>,
}

/// Combined enrichment output for a document, stored in cache.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnrichmentCache {
    /// blake3 hex hash of the file content.
    pub content_hash: String,
    /// blake3 hex hash of the tagger configuration (for invalidation).
    pub tagger_config_hash: String,
    /// Results from each tagger.
    pub results: Vec<TaggerResult>,
    /// When this cache entry was created.
    pub timestamp: Timestamp,
}

// ── Traits ───────────────────────────────────────────────────────

/// A tagger produces additional bitmap keys from a parse result.
///
/// Taggers are pure functions over parse results and content bytes —
/// they must not perform filesystem I/O. Content is provided as `&[u8]`.
pub trait Tagger: Send + Sync {
    /// Unique name for this tagger (used as cache key and in CLI output).
    fn name(&self) -> &str;

    /// Whether this tagger should run for the given source type.
    fn applies_to(&self, source: &SourceType) -> bool;

    /// Generate tags from the parse result and raw content.
    fn tag(
        &self,
        path: &Path,
        content: &[u8],
        parse_result: &ParseResult,
    ) -> Result<TaggerResult, EnrichError>;
}

/// Cache for tagger results, keyed by content hash.
pub trait TaggerCache: Send + Sync {
    /// Look up cached enrichment results for a content hash.
    fn get(&self, content_hash: &str) -> Result<Option<EnrichmentCache>, EnrichError>;

    /// Store enrichment results.
    fn put(&self, cache: &EnrichmentCache) -> Result<(), EnrichError>;

    /// Invalidate all cached results for a specific tagger (e.g. after config change).
    fn invalidate_tagger(&mut self, tagger_name: &str) -> Result<(), EnrichError>;
}

// ── Error ────────────────────────────────────────────────────────

/// Errors from the enrichment pipeline.
#[derive(Debug, thiserror::Error)]
pub enum EnrichError {
    #[error("tagger '{0}' failed: {1}")]
    TaggerFailed(String, String),
    #[error("cache I/O error: {0}")]
    CacheIo(String),
    #[error("cache serialization error: {0}")]
    CacheSerialization(String),
}
