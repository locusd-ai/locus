use std::path::{Path, PathBuf};

use crate::bitmap::BitmapError;
use crate::registry::{BitmapCatalogEntry, RegistryError};
use crate::types::{BitmapCategory, BitmapKey, ChunkId, DocId, Timestamp};

// ── Filter expression ────────────────────────────────────────────

/// A filter expression in a query.
#[derive(Debug, Clone)]
pub enum Filter {
    /// Match a single bitmap key, e.g. `Filter::Key("tag:work")`
    Key(BitmapKey),
    /// Boolean NOT of a filter
    Not(Box<Filter>),
    /// Boolean AND of multiple filters
    And(Vec<Filter>),
    /// Boolean OR of multiple filters
    Or(Vec<Filter>),
}

// ── Request / Response ───────────────────────────────────────────

/// A query request from any interface (CLI, MCP, HTTP).
#[derive(Debug, Clone)]
pub struct QueryRequest {
    /// The filter expression to resolve.
    pub filter: Filter,
    /// Maximum number of results to return.
    pub limit: Option<u32>,
    /// Offset for pagination.
    pub offset: Option<u32>,
}

/// A single match result — a pointer to relevant content.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MatchPointer {
    pub doc_id: DocId,
    pub file_path: PathBuf,
    pub source_type: String,
    pub chunks: Vec<ChunkPointer>,
    pub matched_filters: Vec<BitmapKey>,
    pub auto_type: Option<String>,
    pub score: Option<f32>,
    pub last_modified: Timestamp,
}

/// A pointer to a specific chunk within a document.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChunkPointer {
    pub chunk_id: ChunkId,
    pub kind: String,
    pub byte_start: u32,
    pub byte_end: u32,
    pub label: Option<String>,
}

/// The result of a query.
#[derive(Debug, serde::Serialize)]
pub struct QueryResult {
    pub matches: Vec<MatchPointer>,
    /// Total matching docs before limit/offset.
    pub total_matching: u32,
    /// Query duration in microseconds.
    pub query_time_us: u64,
}

/// The result of inspecting a single document in the index.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InspectResult {
    pub doc_id: DocId,
    pub file_path: PathBuf,
    pub source_type: String,
    pub auto_type: Option<String>,
    pub blake3_hash: String,
    pub last_indexed: Timestamp,
    pub chunks: Vec<ChunkPointer>,
    pub bitmap_keys: Vec<BitmapKey>,
}

/// Summary status of the entire index.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexStatus {
    pub total_documents: u32,
    pub total_bitmaps: usize,
    pub tombstoned: u64,
    pub next_doc_id: DocId,
    pub next_chunk_id: ChunkId,
}

/// A filter entry for discovery.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FilterEntry {
    pub key: BitmapKey,
    pub cardinality: u32,
}

// ── Trait ─────────────────────────────────────────────────────────

/// Query engine that resolves bitmap filters and returns structural pointers.
pub trait QueryEngine: Send + Sync {
    fn query(&self, request: QueryRequest) -> Result<QueryResult, QueryError>;

    /// List available bitmap keys for filter discovery.
    fn list_filters(
        &self,
        category: Option<BitmapCategory>,
    ) -> Result<Vec<BitmapCatalogEntry>, QueryError>;

    /// Inspect a single file in the index by path.
    fn inspect(&self, path: &Path) -> Result<Option<InspectResult>, QueryError>;

    /// Get summary status of the index.
    fn status(&self) -> Result<IndexStatus, QueryError>;

    /// List available filter keys with cardinality, optionally by category prefix.
    fn list_filter_keys(&self, category: Option<&str>) -> Result<Vec<FilterEntry>, QueryError>;
}

// ── Error ─────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("unknown bitmap key: {0}")]
    UnknownFilter(BitmapKey),
    #[error(transparent)]
    Bitmap(#[from] BitmapError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("semantic error: {0}")]
    Semantic(String),
}
