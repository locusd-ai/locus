//! Persistent vector store backed by USearch (HNSW index).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use locus_core::semantic::{EmbeddingVector, VectorError, VectorStore};
use locus_core::types::ChunkId;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

/// Persistent HNSW vector store using USearch.
/// Stores vectors on disk at the specified path.
pub struct UsearchVectorStore {
    index: Index,
    path: PathBuf,
    dimensions: usize,
}

impl UsearchVectorStore {
    /// Create a new vector store or open an existing one at the given path.
    pub fn new(path: &Path, dimensions: usize) -> Result<Self, VectorError> {
        let mut options = IndexOptions::default();
        options.dimensions = dimensions;
        options.metric = MetricKind::Cos;
        options.quantization = ScalarKind::F32;

        let index = Index::new(&options)
            .map_err(|e| VectorError::Store(format!("failed to create index: {e}")))?;

        // Reserve some initial capacity
        index
            .reserve(10_000)
            .map_err(|e| VectorError::Store(format!("failed to reserve capacity: {e}")))?;

        // Load existing data if file exists
        let path_buf = path.to_path_buf();
        if path_buf.exists() {
            let path_str = path_buf.to_str().ok_or_else(|| {
                VectorError::Store("invalid path encoding".into())
            })?;
            index
                .load(path_str)
                .map_err(|e| VectorError::Store(format!("failed to load index: {e}")))?;
        }

        Ok(Self {
            index,
            path: path_buf,
            dimensions,
        })
    }

    /// Save the index to disk.
    pub fn save(&self) -> Result<(), VectorError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let path_str = self.path.to_str().ok_or_else(|| {
            VectorError::Store("invalid path encoding".into())
        })?;
        self.index
            .save(path_str)
            .map_err(|e| VectorError::Store(format!("failed to save index: {e}")))?;
        Ok(())
    }

    /// Number of vectors in the index.
    pub fn size(&self) -> usize {
        self.index.size()
    }

    /// The dimensionality of stored vectors.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }
}

impl VectorStore for UsearchVectorStore {
    fn upsert(&self, chunk_id: ChunkId, vector: &EmbeddingVector) -> Result<(), VectorError> {
        let key = chunk_id as u64;

        // Remove existing if present
        if self.index.contains(key) {
            self.index
                .remove(key)
                .map_err(|e| VectorError::Store(format!("failed to remove old vector: {e}")))?;
        }

        // Grow capacity if needed
        if self.index.size() + 1 >= self.index.capacity() {
            self.index
                .reserve(self.index.capacity() * 2 + 1)
                .map_err(|e| VectorError::Store(format!("failed to reserve capacity: {e}")))?;
        }

        self.index
            .add(key, vector.as_slice())
            .map_err(|e| VectorError::Store(format!("failed to add vector: {e}")))?;

        Ok(())
    }

    fn delete(&self, chunk_id: ChunkId) -> Result<(), VectorError> {
        let key = chunk_id as u64;
        if self.index.contains(key) {
            self.index
                .remove(key)
                .map_err(|e| VectorError::Store(format!("failed to remove vector: {e}")))?;
        }
        Ok(())
    }

    fn search_within(
        &self,
        query: &EmbeddingVector,
        candidate_chunk_ids: &[ChunkId],
        top_k: usize,
    ) -> Result<Vec<(ChunkId, f32)>, VectorError> {
        if candidate_chunk_ids.is_empty() || self.index.size() == 0 {
            return Ok(vec![]);
        }

        let candidates: HashSet<u64> = candidate_chunk_ids.iter().map(|&id| id as u64).collect();

        // Use filtered_search with a predicate that only accepts candidate IDs
        let results = self
            .index
            .filtered_search(query.as_slice(), top_k, |key| candidates.contains(&key))
            .map_err(|e| VectorError::Store(format!("search failed: {e}")))?;

        // Convert distances to similarity scores (cosine distance → similarity)
        // USearch returns cosine distances in [0, 2], similarity = 1 - distance
        let scored: Vec<(ChunkId, f32)> = results
            .keys
            .iter()
            .zip(results.distances.iter())
            .map(|(&key, &dist)| (key as ChunkId, 1.0 - dist))
            .collect();

        Ok(scored)
    }
}

impl Drop for UsearchVectorStore {
    fn drop(&mut self) {
        // Best-effort save on drop
        let _ = self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_persist_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.usearch");

        // Create and populate
        {
            let store = UsearchVectorStore::new(&path, 3).unwrap();
            store.upsert(1, &vec![1.0, 0.0, 0.0]).unwrap();
            store.upsert(2, &vec![0.0, 1.0, 0.0]).unwrap();
            store.upsert(3, &vec![0.9, 0.1, 0.0]).unwrap();
            store.save().unwrap();
        }

        // Reopen and search
        {
            let store = UsearchVectorStore::new(&path, 3).unwrap();
            assert_eq!(store.size(), 3);

            let results = store
                .search_within(&vec![1.0, 0.0, 0.0], &[1, 2, 3], 2)
                .unwrap();

            assert_eq!(results.len(), 2);
            assert_eq!(results[0].0, 1); // exact match first
        }
    }

    #[test]
    fn search_within_scopes_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.usearch");
        let store = UsearchVectorStore::new(&path, 3).unwrap();

        store.upsert(1, &vec![1.0, 0.0, 0.0]).unwrap();
        store.upsert(2, &vec![1.0, 0.0, 0.0]).unwrap(); // same but excluded
        store.upsert(3, &vec![0.0, 1.0, 0.0]).unwrap();

        let results = store
            .search_within(&vec![1.0, 0.0, 0.0], &[1, 3], 10)
            .unwrap();

        // chunk 2 should not appear
        assert!(results.iter().all(|(id, _)| *id != 2));
    }

    #[test]
    fn delete_removes_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.usearch");
        let store = UsearchVectorStore::new(&path, 3).unwrap();

        store.upsert(1, &vec![1.0, 0.0, 0.0]).unwrap();
        store.delete(1).unwrap();

        let results = store
            .search_within(&vec![1.0, 0.0, 0.0], &[1], 10)
            .unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn upsert_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.usearch");
        let store = UsearchVectorStore::new(&path, 3).unwrap();

        store.upsert(1, &vec![1.0, 0.0, 0.0]).unwrap();
        store.upsert(1, &vec![0.0, 1.0, 0.0]).unwrap();

        assert_eq!(store.size(), 1);

        let results = store
            .search_within(&vec![0.0, 1.0, 0.0], &[1], 1)
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].1 > 0.9); // should be very similar to updated vector
    }
}
