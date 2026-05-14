//! Integration test: ingest with embedder → semantic query returns ranked results.

use std::path::PathBuf;

use biem_bitmap::memory::InMemoryBitmapStore;
use biem_core::bitmap::BitmapStore;
use biem_core::registry::{NewChunk, NewDoc, Registry};
use biem_core::semantic::{
    EmbedError, Embedder, EmbeddingVector, SemanticQueryRequest, VectorStore,
};
use biem_core::types::{ChunkKind, ChunkMetadata, SourceType};
use biem_embed::InMemoryVectorStore;
use biem_query::{BitmapQueryEngine, Filter};
use biem_registry::memory::InMemoryRegistry;

/// A fake embedder that produces deterministic vectors from text length,
/// so we don't need the real ONNX model in tests.
struct FakeEmbedder;

impl Embedder for FakeEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<EmbeddingVector>, EmbedError> {
        Ok(texts
            .iter()
            .map(|t| {
                let len = t.len() as f32;
                // 4-dim vector derived from text length for deterministic similarity
                vec![len.sin(), len.cos(), (len * 0.5).sin(), (len * 0.5).cos()]
            })
            .collect())
    }

    fn dimension(&self) -> usize {
        4
    }
}

#[test]
fn test_end_to_end_semantic_query() {
    // Setup stores
    let mut registry = InMemoryRegistry::new();
    let mut bitmap_store = InMemoryBitmapStore::new();
    let vector_store = InMemoryVectorStore::new();
    let embedder = FakeEmbedder;

    // Insert 2 docs, one tagged "work", one tagged "personal"
    let d1 = registry
        .insert_doc(NewDoc {
            file_path: PathBuf::from("/vault/project-plan.md"),
            source_type: SourceType::Obsidian,
            blake3_hash: [1; 32],
            auto_type: None,
        })
        .unwrap();
    let d2 = registry
        .insert_doc(NewDoc {
            file_path: PathBuf::from("/vault/diary.md"),
            source_type: SourceType::Obsidian,
            blake3_hash: [2; 32],
            auto_type: None,
        })
        .unwrap();

    bitmap_store.insert_id("tag:work", d1).unwrap();
    bitmap_store.insert_id("tag:personal", d2).unwrap();
    bitmap_store.insert_id("source:obsidian", d1).unwrap();
    bitmap_store.insert_id("source:obsidian", d2).unwrap();

    // Insert chunks for each doc
    let d1_chunks = registry
        .replace_chunks(
            d1,
            vec![
                NewChunk {
                    doc_id: d1,
                    kind: ChunkKind::Section,
                    byte_start: 0,
                    byte_end: 100,
                    label: Some("Project Plan".into()),
                    depth: 1,
                    metadata: ChunkMetadata::default(),
                },
                NewChunk {
                    doc_id: d1,
                    kind: ChunkKind::Section,
                    byte_start: 100,
                    byte_end: 200,
                    label: Some("Timeline".into()),
                    depth: 2,
                    metadata: ChunkMetadata::default(),
                },
            ],
        )
        .unwrap();
    let (c1, c2) = (d1_chunks[0], d1_chunks[1]);

    let d2_chunks = registry
        .replace_chunks(
            d2,
            vec![NewChunk {
                doc_id: d2,
                kind: ChunkKind::Section,
                byte_start: 0,
                byte_end: 150,
                label: Some("Dear Diary".into()),
                depth: 1,
                metadata: ChunkMetadata::default(),
            }],
        )
        .unwrap();
    let c3 = d2_chunks[0];

    // Embed chunks into vector store
    let texts = ["Project Plan", "Timeline", "Dear Diary"];
    let vectors = embedder.embed(&texts.iter().map(|s| *s).collect::<Vec<_>>()).unwrap();
    vector_store.upsert(c1, &vectors[0]).unwrap();
    vector_store.upsert(c2, &vectors[1]).unwrap();
    vector_store.upsert(c3, &vectors[2]).unwrap();

    // Build query engine
    let engine = BitmapQueryEngine::new(Box::new(bitmap_store), Box::new(registry));

    // Semantic query scoped to tag:work — should only return chunks from d1
    let result = engine
        .semantic_query(
            &SemanticQueryRequest {
                filter: Filter::Key("tag:work".into()),
                query_text: "project planning".into(),
                top_k: 10,
                rerank: false,
            },
            &embedder,
            &vector_store,
        )
        .unwrap();

    assert_eq!(result.bitmap_candidates, 1, "bitmap should match 1 doc");
    assert_eq!(result.vector_searched, 2, "d1 has 2 chunks");
    assert_eq!(result.pointers.len(), 2, "should return both chunks from d1");

    // Verify scores are present and sorted descending
    assert!(result.pointers[0].score >= result.pointers[1].score);

    // Semantic query scoped to source:obsidian — all 3 chunks
    let result2 = engine
        .semantic_query(
            &SemanticQueryRequest {
                filter: Filter::Key("source:obsidian".into()),
                query_text: "diary entry".into(),
                top_k: 10,
                rerank: false,
            },
            &embedder,
            &vector_store,
        )
        .unwrap();

    assert_eq!(result2.bitmap_candidates, 2);
    assert_eq!(result2.vector_searched, 3);
    assert_eq!(result2.pointers.len(), 3);

    // Empty filter result → no vector search
    let result3 = engine
        .semantic_query(
            &SemanticQueryRequest {
                filter: Filter::Key("tag:nonexistent".into()),
                query_text: "anything".into(),
                top_k: 10,
                rerank: false,
            },
            &embedder,
            &vector_store,
        )
        .unwrap();

    assert_eq!(result3.bitmap_candidates, 0);
    assert_eq!(result3.vector_searched, 0);
    assert!(result3.pointers.is_empty());
}
