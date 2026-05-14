//! In-memory vector store — brute-force cosine similarity for testing.

use std::collections::HashMap;
use std::sync::RwLock;

use biem_core::semantic::{EmbeddingVector, VectorError, VectorStore};
use biem_core::types::ChunkId;

/// Brute-force in-memory vector store. Not suitable for production —
/// use for unit/integration tests.
pub struct InMemoryVectorStore {
    vectors: RwLock<HashMap<ChunkId, EmbeddingVector>>,
}

impl InMemoryVectorStore {
    pub fn new() -> Self {
        Self {
            vectors: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryVectorStore {
    fn default() -> Self {
        Self::new()
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

impl VectorStore for InMemoryVectorStore {
    fn upsert(&self, chunk_id: ChunkId, vector: &EmbeddingVector) -> Result<(), VectorError> {
        let mut store = self.vectors.write().map_err(|e| VectorError::Store(e.to_string()))?;
        store.insert(chunk_id, vector.clone());
        Ok(())
    }

    fn delete(&self, chunk_id: ChunkId) -> Result<(), VectorError> {
        let mut store = self.vectors.write().map_err(|e| VectorError::Store(e.to_string()))?;
        store.remove(&chunk_id);
        Ok(())
    }

    fn search_within(
        &self,
        query: &EmbeddingVector,
        candidate_chunk_ids: &[ChunkId],
        top_k: usize,
    ) -> Result<Vec<(ChunkId, f32)>, VectorError> {
        let store = self.vectors.read().map_err(|e| VectorError::Store(e.to_string()))?;

        let mut scored: Vec<(ChunkId, f32)> = candidate_chunk_ids
            .iter()
            .filter_map(|&id| {
                store.get(&id).map(|vec| (id, cosine_similarity(query, vec)))
            })
            .collect();

        // Sort descending by score
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);

        Ok(scored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_and_search_round_trip() {
        let store = InMemoryVectorStore::new();
        store.upsert(1, &vec![1.0, 0.0, 0.0]).unwrap();
        store.upsert(2, &vec![0.0, 1.0, 0.0]).unwrap();
        store.upsert(3, &vec![0.9, 0.1, 0.0]).unwrap();

        // Query similar to chunk 1
        let results = store
            .search_within(&vec![1.0, 0.0, 0.0], &[1, 2, 3], 2)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 1); // exact match first
        assert_eq!(results[1].0, 3); // most similar second
        assert!((results[0].1 - 1.0).abs() < 0.001);
    }

    #[test]
    fn search_within_respects_candidate_set() {
        let store = InMemoryVectorStore::new();
        store.upsert(1, &vec![1.0, 0.0]).unwrap();
        store.upsert(2, &vec![1.0, 0.0]).unwrap(); // same vector, but not in candidates
        store.upsert(3, &vec![0.0, 1.0]).unwrap();

        let results = store
            .search_within(&vec![1.0, 0.0], &[1, 3], 10)
            .unwrap();

        assert_eq!(results.len(), 2);
        // chunk 2 should not appear even though it's a perfect match
        assert!(results.iter().all(|(id, _)| *id != 2));
    }

    #[test]
    fn delete_removes_from_results() {
        let store = InMemoryVectorStore::new();
        store.upsert(1, &vec![1.0, 0.0]).unwrap();
        store.upsert(2, &vec![0.0, 1.0]).unwrap();

        store.delete(1).unwrap();

        let results = store
            .search_within(&vec![1.0, 0.0], &[1, 2], 10)
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 2);
    }

    #[test]
    fn empty_candidates_returns_empty() {
        let store = InMemoryVectorStore::new();
        store.upsert(1, &vec![1.0, 0.0]).unwrap();

        let results = store
            .search_within(&vec![1.0, 0.0], &[], 10)
            .unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn upsert_overwrites() {
        let store = InMemoryVectorStore::new();
        store.upsert(1, &vec![1.0, 0.0]).unwrap();
        store.upsert(1, &vec![0.0, 1.0]).unwrap();

        let results = store
            .search_within(&vec![0.0, 1.0], &[1], 1)
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!((results[0].1 - 1.0).abs() < 0.001); // should match updated vector
    }
}
