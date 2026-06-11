use std::path::{Path, PathBuf};

use crate::{
    BitmapCategory, BitmapKey, ChunkId, ChunkKind, ChunkMetadata,
    DocId, DocType, SourceType, Timestamp,
};

// ── Records (output types) ───────────────────────────────────────

/// A document record in the registry.
#[derive(Debug, Clone)]
pub struct DocRecord {
    pub doc_id: DocId,
    pub file_path: PathBuf,
    pub source_type: SourceType,
    pub blake3_hash: [u8; 32],
    pub last_indexed: Timestamp,
    pub auto_type: Option<DocType>,
}

/// A chunk record in the registry.
#[derive(Debug, Clone)]
pub struct ChunkRecord {
    pub chunk_id: ChunkId,
    pub doc_id: DocId,
    pub kind: ChunkKind,
    pub byte_start: u32,
    pub byte_end: u32,
    pub label: Option<String>,
    pub depth: u8,
    pub metadata: ChunkMetadata,
}

/// Metadata about a bitmap, stored in the catalog.
#[derive(Debug, Clone)]
pub struct BitmapCatalogEntry {
    pub bitmap_key: BitmapKey,
    pub category: BitmapCategory,
    pub cardinality: u32,
    pub last_updated: Timestamp,
}

/// Global state tracked by the registry.
#[derive(Debug, Clone)]
pub struct GlobalState {
    pub next_doc_id: DocId,
    pub next_chunk_id: ChunkId,
    pub total_documents: u32,
}

// ── Input types ──────────────────────────────────────────────────

/// Input for registering a new document.
pub struct NewDoc {
    pub file_path: PathBuf,
    pub source_type: SourceType,
    pub blake3_hash: [u8; 32],
    pub auto_type: Option<DocType>,
}

/// Input for registering chunks belonging to a document.
pub struct NewChunk {
    pub doc_id: DocId,
    pub kind: ChunkKind,
    pub byte_start: u32,
    pub byte_end: u32,
    pub label: Option<String>,
    pub depth: u8,
    pub metadata: ChunkMetadata,
}

// ── Registry trait ───────────────────────────────────────────────

/// Pluggable storage backend for document metadata, chunks, and bitmap catalog.
/// DuckDB is the first implementation.
///
/// ## PRE-ORDER INVARIANT
///
/// Chunk ids within a single document are **contiguous and strictly ascending**
/// in document (byte) order.  Every call to `replace_chunks` atomically assigns a
/// fresh contiguous run of `ChunkId`s (starting at `global_state.next_chunk_id`)
/// and stores the inclusive `[chunk_start, chunk_end]` range so it can be queried
/// in O(1) without touching the chunks table.
///
/// Consequences:
/// - `doc_for_chunk(id)` can be answered by a single range-scan on the
///   `doc_chunk_ranges` table — no full chunks table scan required.
/// - Re-indexing a document **invalidates** its old range; old chunk ids no longer
///   resolve to that document via `doc_for_chunk`.
/// - Third-party implementations that do not yet maintain `doc_chunk_ranges` can
///   rely on the default `get_chunks_for_docs` impl, which falls back to one
///   `get_chunks` call per document.
pub trait Registry: Send + Sync {
    // --- Document operations ---

    /// Assign a new doc_id and insert the document. Returns the assigned DocId.
    fn insert_doc(&mut self, doc: NewDoc) -> Result<DocId, RegistryError>;

    /// Bulk insert documents in a single transaction. Returns assigned DocIds in order.
    fn bulk_insert_docs(&mut self, docs: Vec<NewDoc>) -> Result<Vec<DocId>, RegistryError>;

    /// Look up a document by file path.
    fn lookup_by_path(&self, path: &Path) -> Result<Option<DocRecord>, RegistryError>;

    /// Look up a document by ID.
    fn lookup_by_id(&self, doc_id: DocId) -> Result<Option<DocRecord>, RegistryError>;

    /// Look up multiple documents by their IDs.
    fn lookup_by_ids(&self, doc_ids: &[DocId]) -> Result<Vec<DocRecord>, RegistryError>;

    /// Update the hash, timestamp, and auto_type for an existing document.
    fn update_doc(
        &mut self,
        doc_id: DocId,
        hash: [u8; 32],
        auto_type: Option<DocType>,
    ) -> Result<(), RegistryError>;

    /// Update the file path for an existing document (file move/rename).
    fn update_path(&mut self, doc_id: DocId, new_path: PathBuf) -> Result<(), RegistryError>;

    /// Delete a document and all its chunks from the registry.
    fn delete_doc(&mut self, doc_id: DocId) -> Result<(), RegistryError>;

    // --- Chunk operations ---

    /// Replace all chunks for a document (delete old, insert new).
    ///
    /// Implementations MUST maintain the PRE-ORDER INVARIANT: assign a single
    /// contiguous monotonically increasing run of ChunkIds starting at
    /// `global_state.next_chunk_id`, and record the resulting
    /// `[chunk_start, chunk_end]` range in `doc_chunk_ranges` (or the equivalent
    /// in-memory structure).
    fn replace_chunks(
        &mut self,
        doc_id: DocId,
        chunks: Vec<NewChunk>,
    ) -> Result<Vec<ChunkId>, RegistryError>;

    /// Get all chunks for a document, ordered by chunk_id (= document byte order).
    fn get_chunks(&self, doc_id: DocId) -> Result<Vec<ChunkRecord>, RegistryError>;

    /// Return the inclusive `[chunk_start, chunk_end]` range for a document's
    /// current chunk run, or `None` if the document has no chunks.
    ///
    /// This is an O(1) lookup against the pre-order range index; it does **not**
    /// scan the chunks table.
    fn chunk_range(&self, doc_id: DocId) -> Result<Option<(ChunkId, ChunkId)>, RegistryError>;

    /// Reverse lookup: given a ChunkId, return the DocId that owns it.
    ///
    /// Returns `None` if no document's current chunk range includes `chunk_id`.
    /// Because chunk ranges are contiguous, this is a single range-scan.
    ///
    /// Note: after `replace_chunks` the old chunk ids no longer resolve to the
    /// document — only the most-recent run is tracked.
    fn doc_for_chunk(&self, chunk_id: ChunkId) -> Result<Option<DocId>, RegistryError>;

    /// Fetch all chunks for the given documents in a single call, ordered by
    /// `chunk_id` (which equals document byte order within each doc).
    ///
    /// The default implementation loops over `get_chunks` so third-party
    /// implementations keep working without changes.  `DuckDbRegistry` overrides
    /// this with a single SQL `IN (…)` query for efficiency.
    fn get_chunks_for_docs(
        &self,
        doc_ids: &[DocId],
    ) -> Result<Vec<ChunkRecord>, RegistryError> {
        if doc_ids.is_empty() {
            return Ok(vec![]);
        }
        let mut all: Vec<ChunkRecord> = Vec::new();
        for &doc_id in doc_ids {
            let mut chunks = self.get_chunks(doc_id)?;
            all.append(&mut chunks);
        }
        // Sort by chunk_id so the caller sees a globally ordered stream.
        all.sort_by_key(|c| c.chunk_id);
        Ok(all)
    }

    // --- Bitmap catalog ---

    /// Upsert a bitmap catalog entry (set cardinality + timestamp).
    fn upsert_catalog_entry(&mut self, entry: BitmapCatalogEntry) -> Result<(), RegistryError>;

    /// Bulk upsert bitmap catalog entries.
    fn bulk_upsert_catalog(
        &mut self,
        entries: Vec<BitmapCatalogEntry>,
    ) -> Result<(), RegistryError>;

    /// Get a catalog entry by key.
    fn get_catalog_entry(&self, key: &str) -> Result<Option<BitmapCatalogEntry>, RegistryError>;

    /// Get all catalog entries, optionally filtered by category.
    fn list_catalog(
        &self,
        category: Option<BitmapCategory>,
    ) -> Result<Vec<BitmapCatalogEntry>, RegistryError>;

    // --- Global state ---

    /// Get all documents in the registry (for idempotent bulk re-indexing).
    fn list_all_docs(&self) -> Result<Vec<DocRecord>, RegistryError>;

    /// Get the current global state (next IDs, totals).
    fn get_global_state(&self) -> Result<GlobalState, RegistryError>;
}

// ── Errors ───────────────────────────────────────────────────────

/// Errors from registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("document not found: {0}")]
    NotFound(DocId),
    #[error("path already registered: {0}")]
    DuplicatePath(PathBuf),
    #[error("database error: {0}")]
    Database(String),
}
