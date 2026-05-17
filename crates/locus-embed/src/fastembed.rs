//! FastEmbed-based local embedder using ONNX runtime.
//!
//! Uses BGE-small-en-v1.5 by default — 384 dimensions, ~33MB model.
//! Models are downloaded and cached in `~/.cache/fastembed/` automatically.

use std::sync::Mutex;

use locus_core::semantic::{EmbedError, Embedder, EmbeddingVector};
use locus_core::semantic::{RerankError, Reranker};
use locus_core::types::ChunkId;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use fastembed::{RerankerModel, RerankInitOptions, TextRerank};
use tracing::info;

/// Local embedder using fastembed (ONNX inference, no API calls).
pub struct FastEmbedEmbedder {
    model: Mutex<TextEmbedding>,
    dimension: usize,
}

impl FastEmbedEmbedder {
    /// Create a new FastEmbedEmbedder with the default model (BGE-small-en-v1.5, 384d).
    pub fn new() -> Result<Self, EmbedError> {
        Self::with_model(EmbeddingModel::BGESmallENV15)
    }

    /// Create with a specific fastembed model.
    pub fn with_model(model: EmbeddingModel) -> Result<Self, EmbedError> {
        let dimension = match model {
            EmbeddingModel::BGESmallENV15 => 384,
            EmbeddingModel::AllMiniLML6V2 => 384,
            EmbeddingModel::BGEBaseENV15 => 768,
            _ => 384, // default fallback
        };

        info!(model = ?model, dimension, "loading embedding model");

        let options = InitOptions::new(model).with_show_download_progress(true);
        let text_embedding = TextEmbedding::try_new(options)
            .map_err(|e| EmbedError::Model(e.to_string()))?;

        Ok(Self {
            model: Mutex::new(text_embedding),
            dimension,
        })
    }
}

impl Embedder for FastEmbedEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<EmbeddingVector>, EmbedError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let texts_owned: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        let mut model = self.model.lock().map_err(|e| EmbedError::Model(e.to_string()))?;
        let embeddings = model
            .embed(texts_owned, None)
            .map_err(|e| EmbedError::Model(e.to_string()))?;

        Ok(embeddings)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

/// Local cross-encoder reranker using fastembed (ONNX inference, no API calls).
pub struct FastEmbedReranker {
    model: Mutex<TextRerank>,
    model_name: String,
}

impl FastEmbedReranker {
    /// Create with the default reranker model (BGE-Reranker-Base).
    pub fn new() -> Result<Self, RerankError> {
        Self::with_model(RerankerModel::BGERerankerBase)
    }

    /// Create with a specific fastembed reranker model.
    pub fn with_model(model: RerankerModel) -> Result<Self, RerankError> {
        let model_name = format!("{:?}", model);
        info!(model = %model_name, "loading reranker model");

        let options = RerankInitOptions::new(model).with_show_download_progress(true);
        let reranker = TextRerank::try_new(options)
            .map_err(|e| RerankError::Model(e.to_string()))?;

        Ok(Self {
            model: Mutex::new(reranker),
            model_name,
        })
    }

    /// The model name (for ScoreSource tagging).
    pub fn model_name(&self) -> &str {
        &self.model_name
    }
}

impl Reranker for FastEmbedReranker {
    fn rerank(
        &self,
        query: &str,
        candidates: &[(ChunkId, &str)],
    ) -> Result<Vec<(ChunkId, f32)>, RerankError> {
        if candidates.is_empty() {
            return Ok(vec![]);
        }

        let documents: Vec<&str> = candidates.iter().map(|(_, text)| *text).collect();
        let mut model = self.model.lock().map_err(|e| RerankError::Model(e.to_string()))?;

        let results = model
            .rerank(query, &documents, false, None)
            .map_err(|e| RerankError::Model(e.to_string()))?;

        // Map back to (ChunkId, score) using the index from RerankResult
        let mut scored: Vec<(ChunkId, f32)> = results
            .into_iter()
            .map(|r| (candidates[r.index].0, r.score))
            .collect();

        // Sort descending by score
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(scored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: these tests require model download (~33MB) on first run.
    // They are ignored by default for CI — run with `cargo test -- --ignored`.

    #[test]
    #[ignore = "requires model download"]
    fn embed_returns_correct_dimension() {
        let embedder = FastEmbedEmbedder::new().unwrap();
        let results = embedder.embed(&["hello world"]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].len(), 384);
    }

    #[test]
    #[ignore = "requires model download"]
    fn similar_texts_score_higher() {
        let embedder = FastEmbedEmbedder::new().unwrap();
        let results = embedder
            .embed(&[
                "authentication and login",
                "user auth with JWT tokens",
                "cooking recipes for pasta",
            ])
            .unwrap();

        let sim_related = cosine(&results[0], &results[1]);
        let sim_unrelated = cosine(&results[0], &results[2]);

        assert!(
            sim_related > sim_unrelated,
            "related texts ({sim_related:.3}) should be more similar than unrelated ({sim_unrelated:.3})"
        );
    }

    #[test]
    #[ignore = "requires model download"]
    fn empty_input_returns_empty() {
        let embedder = FastEmbedEmbedder::new().unwrap();
        let results = embedder.embed(&[]).unwrap();
        assert!(results.is_empty());
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
    }
}
