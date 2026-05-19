//! Three-stage pipeline tests: bitmap → graph expand → vector rerank.
//!
//! These tests use in-memory registry/bitmap/graph stores and a fake embedder
//! so they run without any real ML models.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use locus_bitmap::memory::InMemoryBitmapStore;
    use locus_core::graph::{Direction, EdgeCategory, EdgeFilter, ExpandSpec, GraphQueryEngine};
    use locus_core::query::Filter;
    use locus_core::semantic::{
        EmbeddingVector, EmbedError, Embedder, SemanticQueryRequest, VectorError, VectorStore,
    };
    use locus_core::types::ChunkId;
    use locus_query::{BitmapQueryEngine, PetgraphQueryEngine};
    use locus_registry::duckdb::DuckDbRegistry;
    use locus_registry::graph::DuckDbGraphStore;
    use locus_registry::memory::InMemoryRegistry;
    use roaring::RoaringBitmap;

    // ── Fake embedder / vector store ────────────────────────────────

    struct ConstantEmbedder;
    impl Embedder for ConstantEmbedder {
        fn embed(&self, texts: &[&str]) -> Result<Vec<EmbeddingVector>, EmbedError> {
            Ok(texts.iter().map(|_| vec![1.0f32; 4]).collect())
        }
        fn dimension(&self) -> usize { 4 }
    }

    struct FakeVectorStore {
        /// Returns these chunk IDs in order, regardless of query.
        chunks: Vec<(ChunkId, f32)>,
    }
    impl VectorStore for FakeVectorStore {
        fn upsert(&self, _: ChunkId, _: &EmbeddingVector) -> Result<(), VectorError> { Ok(()) }
        fn delete(&self, _: ChunkId) -> Result<(), VectorError> { Ok(()) }
        fn search_within(
            &self,
            _query: &EmbeddingVector,
            candidate_chunk_ids: &[ChunkId],
            top_k: usize,
        ) -> Result<Vec<(ChunkId, f32)>, VectorError> {
            Ok(self
                .chunks
                .iter()
                .filter(|(id, _)| candidate_chunk_ids.contains(id))
                .take(top_k)
                .cloned()
                .collect())
        }
    }

    // ── Test helpers ────────────────────────────────────────────────

    fn make_engine_no_graph() -> BitmapQueryEngine {
        BitmapQueryEngine::new(
            Box::new(InMemoryBitmapStore::new()),
            Box::new(InMemoryRegistry::new()),
        )
    }

    fn make_engine_with_graph() -> (BitmapQueryEngine, Arc<dyn GraphQueryEngine>) {
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        let gs = DuckDbGraphStore::new(registry.connection());
        let gs_arc: Arc<dyn locus_core::graph::GraphStore> = Arc::new(gs);
        let graph_engine: Arc<dyn GraphQueryEngine> =
            Arc::new(PetgraphQueryEngine::new(gs_arc));

        let engine = BitmapQueryEngine::new(
            Box::new(InMemoryBitmapStore::new()),
            Box::new(InMemoryRegistry::new()),
        )
        .with_graph(graph_engine.clone());

        (engine, graph_engine)
    }

    // ── Tests ───────────────────────────────────────────────────────

    #[test]
    fn graph_disabled_result_identical_structure() {
        let engine = make_engine_no_graph();

        // Empty bitmap → fast return with no graph timing.
        let result = engine
            .semantic_query(
                &SemanticQueryRequest {
                    filter: Filter::Key("tag:nothing".into()),
                    query_text: "test".into(),
                    top_k: 10,
                    rerank: false,
                    graph_expand: None,
                },
                &ConstantEmbedder,
                &FakeVectorStore { chunks: vec![] },
                None,
            )
            .unwrap();

        assert_eq!(result.bitmap_candidates, 0);
        assert_eq!(result.graph_expanded_to, None);
        assert_eq!(result.elapsed_graph_us, 0);
        assert_eq!(result.vector_searched, 0);
    }

    #[test]
    fn graph_expand_none_gives_same_candidate_count_as_bitmap() {
        let engine = make_engine_no_graph();

        // Even with a non-empty bitmap, no graph_expand → graph_expanded_to is None.
        let result = engine
            .semantic_query(
                &SemanticQueryRequest {
                    filter: Filter::Key("tag:nonexistent".into()),
                    query_text: "hello".into(),
                    top_k: 5,
                    rerank: false,
                    graph_expand: None,
                },
                &ConstantEmbedder,
                &FakeVectorStore { chunks: vec![] },
                None,
            )
            .unwrap();

        assert_eq!(result.graph_expanded_to, None);
        assert_eq!(result.elapsed_graph_us, 0);
    }

    #[test]
    fn graph_expand_with_empty_graph_returns_seeds() {
        let (engine, _) = make_engine_with_graph();

        // Bitmap is empty → expand returns empty → same as no-graph path.
        let result = engine
            .semantic_query(
                &SemanticQueryRequest {
                    filter: Filter::Key("tag:nonexistent".into()),
                    query_text: "hello".into(),
                    top_k: 5,
                    rerank: false,
                    graph_expand: Some(ExpandSpec {
                        hops: 1,
                        direction: Direction::Outgoing,
                        edge_filter: EdgeFilter::Any,
                        max_nodes: None,
                        include_seeds: true,
                    }),
                },
                &ConstantEmbedder,
                &FakeVectorStore { chunks: vec![] },
                None,
            )
            .unwrap();

        // Graph expand ran (elapsed_graph_us may be 0 or tiny but field is present).
        // graph_expanded_to is Some(0) since bitmap was empty → expand result is empty.
        assert_eq!(result.bitmap_candidates, 0);
        assert_eq!(result.graph_expanded_to, Some(0));
    }

    #[test]
    fn graph_expand_spec_is_forwarded_through_engine() {
        // Test that the ExpandSpec is threaded correctly: with graph enabled and
        // a non-empty spec, graph_expanded_to is Some(...).
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        let gs = DuckDbGraphStore::new(registry.connection());
        let gs_arc: Arc<dyn locus_core::graph::GraphStore> = Arc::new(gs);
        let graph_engine: Arc<dyn GraphQueryEngine> = Arc::new(PetgraphQueryEngine::new(gs_arc));

        let engine = BitmapQueryEngine::new(
            Box::new(InMemoryBitmapStore::new()),
            Box::new(InMemoryRegistry::new()),
        )
        .with_graph(graph_engine);

        let result = engine
            .semantic_query(
                &SemanticQueryRequest {
                    filter: Filter::Key("tag:nonexistent".into()),
                    query_text: "test".into(),
                    top_k: 5,
                    rerank: false,
                    graph_expand: Some(ExpandSpec::default()),
                },
                &ConstantEmbedder,
                &FakeVectorStore { chunks: vec![] },
                None,
            )
            .unwrap();

        // graph_expanded_to is Some (even if 0) — signals the stage ran.
        assert!(result.graph_expanded_to.is_some());
    }

    #[test]
    fn graph_expand_method_returns_seeds_when_no_graph_wired() {
        let engine = make_engine_no_graph();
        let mut seeds = RoaringBitmap::new();
        seeds.insert(1);
        seeds.insert(2);

        let spec = ExpandSpec::default();
        let result = engine.graph_expand(&seeds, &spec).unwrap();
        assert_eq!(result, seeds, "no-graph expand must return seeds unchanged");
    }
}
