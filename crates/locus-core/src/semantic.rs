//! Semantic layer types and traits — embeddings, vector search, reranking.

use crate::query::{Filter, MatchPointer};
use crate::types::ChunkId;

// ── Types ────────────────────────────────────────────────────────

/// A dense embedding vector (f32 per dimension).
pub type EmbeddingVector = Vec<f32>;

/// How a score was computed.
#[derive(Debug, Clone, serde::Serialize)]
pub enum ScoreSource {
    CosineSimilarity,
    Reranker(String), // model name
}

/// A chunk pointer with a similarity/relevance score.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoredPointer {
    pub pointer: MatchPointer,
    pub chunk_id: ChunkId,
    pub score: f32,
    pub score_source: ScoreSource,
}

/// A semantic query request — always requires a bitmap pre-filter.
#[derive(Debug, Clone)]
pub struct SemanticQueryRequest {
    /// Bitmap pre-filter (mandatory — no full-space vector search).
    pub filter: Filter,
    /// Natural language query for vector similarity.
    pub query_text: String,
    /// Maximum chunk results.
    pub top_k: usize,
    /// Whether to apply reranker after vector search.
    pub rerank: bool,
}

/// Result of a semantic query.
#[derive(Debug, serde::Serialize)]
pub struct SemanticQueryResult {
    pub pointers: Vec<ScoredPointer>,
    /// How many docs the bitmap filter returned.
    pub bitmap_candidates: u32,
    /// How many chunks were vector-searched.
    pub vector_searched: u32,
    /// Bitmap filter time in microseconds.
    pub elapsed_bitmap_us: u64,
    /// Vector search time in microseconds.
    pub elapsed_vector_us: u64,
    /// Reranker time in microseconds (0 if not used).
    pub elapsed_rerank_us: u64,
}

// ── Errors ───────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("embedding model error: {0}")]
    Model(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum VectorError {
    #[error("vector store error: {0}")]
    Store(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum RerankError {
    #[error("reranker error: {0}")]
    Model(String),
}

// ── Traits ───────────────────────────────────────────────────────

/// Generate embeddings for text chunks. Implementations are local-first
/// (e.g. ONNX inference) or API-based (opt-in).
pub trait Embedder: Send + Sync {
    /// Embed one or more texts, returning one vector per input.
    fn embed(&self, texts: &[&str]) -> Result<Vec<EmbeddingVector>, EmbedError>;

    /// The dimensionality of the output vectors.
    fn dimension(&self) -> usize;
}

/// Store and search dense vectors, scoped by chunk ID.
/// All search is bitmap-pre-filtered via `candidate_chunk_ids`.
pub trait VectorStore: Send + Sync {
    /// Insert or update a vector for a chunk.
    fn upsert(&self, chunk_id: ChunkId, vector: &EmbeddingVector) -> Result<(), VectorError>;

    /// Remove a chunk's vector.
    fn delete(&self, chunk_id: ChunkId) -> Result<(), VectorError>;

    /// Search for the top-k most similar vectors, restricted to a candidate set.
    /// Returns (chunk_id, score) pairs sorted by descending score.
    fn search_within(
        &self,
        query: &EmbeddingVector,
        candidate_chunk_ids: &[ChunkId],
        top_k: usize,
    ) -> Result<Vec<(ChunkId, f32)>, VectorError>;
}

/// Optional cross-encoder reranker for higher-quality scoring.
pub trait Reranker: Send + Sync {
    /// Re-score (query, chunk_text) pairs. Returns (chunk_id, score) sorted descending.
    fn rerank(
        &self,
        query: &str,
        candidates: &[(ChunkId, &str)],
    ) -> Result<Vec<(ChunkId, f32)>, RerankError>;
}
