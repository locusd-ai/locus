use roaring::RoaringBitmap;

use crate::{BitmapKey, DocId};

/// Pluggable bitmap storage backend.
/// LMDB/heed is the primary implementation; in-memory for tests.
pub trait BitmapStore: Send + Sync {
    // --- Single bitmap operations ---

    /// Get a bitmap by key. Returns an empty bitmap if the key doesn't exist.
    fn get(&self, key: &str) -> Result<RoaringBitmap, BitmapError>;

    /// Write a bitmap to the store (full replace), serialized in portable format.
    fn put(&mut self, key: &str, bitmap: &RoaringBitmap) -> Result<(), BitmapError>;

    /// Insert a single doc_id into a bitmap (deserialize → insert → serialize).
    fn insert_id(&mut self, key: &str, doc_id: DocId) -> Result<(), BitmapError>;

    /// Remove a single doc_id from a bitmap.
    fn remove_id(&mut self, key: &str, doc_id: DocId) -> Result<(), BitmapError>;

    /// Delete a bitmap key entirely.
    fn delete(&mut self, key: &str) -> Result<(), BitmapError>;

    /// Check if a bitmap key exists.
    fn exists(&self, key: &str) -> bool;

    // --- Batch operations (for initial indexing) ---

    /// Write multiple bitmaps in a single transaction.
    fn bulk_put(&mut self, entries: Vec<(BitmapKey, RoaringBitmap)>) -> Result<(), BitmapError>;

    // --- Tombstone operations ---

    /// Add a doc_id to the tombstone bitmap.
    fn tombstone(&mut self, doc_id: DocId) -> Result<(), BitmapError>;

    /// Get the current tombstone bitmap.
    fn get_tombstone(&self) -> Result<RoaringBitmap, BitmapError>;

    /// Clear the tombstone bitmap entirely.
    fn clear_tombstone(&mut self) -> Result<(), BitmapError>;

    // --- Query helpers ---

    /// List all bitmap keys, optionally filtered by prefix (e.g. "tag:").
    fn list_keys(&self, prefix: Option<&str>) -> Result<Vec<BitmapKey>, BitmapError>;

    /// Get the cardinality of a bitmap without deserializing the full bitmap.
    /// Falls back to deserialize + len() if format doesn't support it.
    fn cardinality(&self, key: &str) -> Result<u32, BitmapError>;

    // --- Jaccard similarity ---

    /// Compute Jaccard similarity between two bitmap keys: |A ∩ B| / |A ∪ B|.
    /// Returns 0.0 if both bitmaps are empty, 1.0 if identical.
    /// Default implementation uses get() — backends may override for efficiency.
    fn jaccard_keys(&self, key_a: &str, key_b: &str) -> Result<f64, BitmapError> {
        let a = self.get(key_a)?;
        let b = self.get(key_b)?;
        Ok(jaccard(&a, &b))
    }

    /// Compute Jaccard similarity between two pre-loaded bitmaps.
    /// Useful for doc-as-keyset similarity where bitmaps are already in memory.
    fn jaccard_bitmaps(&self, a: &RoaringBitmap, b: &RoaringBitmap) -> f64 {
        jaccard(a, b)
    }
}

/// Compute Jaccard similarity: |A ∩ B| / |A ∪ B|.
/// Returns 0.0 when both sets are empty.
pub fn jaccard(a: &RoaringBitmap, b: &RoaringBitmap) -> f64 {
    let intersection = a.intersection_len(b);
    let union = a.union_len(b);
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Errors from bitmap store operations.
#[derive(Debug, thiserror::Error)]
pub enum BitmapError {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("serialization error: {0}")]
    Serialization(String),
}
