# Workstream 2: Semantic Layer (Vectors) ✅

> Goal: Add vector-based semantic search as a complement to bitmap filtering. Bitmaps pre-filter, vectors rank by similarity. Local-first by default.

## Steps

### Step 1 — Core types and traits in `locus-core`
- [x] Add `EmbeddingVector` type alias (`Vec<f32>`)
- [x] Add `ScoredPointer` and `ScoreSource` types
- [x] Add `Embedder` trait: `embed(&[&str]) -> Vec<EmbeddingVector>`, `dimension() -> usize`
- [x] Add `VectorStore` trait: `upsert`, `delete`, `search_within` (bitmap-scoped)
- [x] Add `EmbedError`, `VectorError` error types
- [x] Add `SemanticQueryRequest` and `SemanticQueryResult` types
- [x] Re-export from `lib.rs`
- [x] Update `003-contracts.md` with semantic types and traits
- [x] `cargo test` passes

**Commit**: `feat(core): add semantic/vector types and Embedder/VectorStore traits`

### Step 2 — Create `locus-embed` crate with in-memory impls
- [x] Create `crates/locus-embed/` with `Cargo.toml`
- [x] Add to workspace `Cargo.toml`
- [x] Implement `InMemoryVectorStore` (brute-force cosine similarity, for tests)
- [x] Unit test: upsert → search round-trip
- [x] Unit test: search_within respects candidate set
- [x] Unit test: delete removes from results

**Commit**: `feat(embed): create locus-embed crate with InMemoryVectorStore`

### Step 3 — FastEmbed embedder (local model)
- [x] Add `fastembed` crate dependency
- [x] Implement `FastEmbedEmbedder` (BGE-small-en-v1.5, local ONNX inference)
- [x] Handle model download/cache in `~/.cache/fastembed/`
- [x] Unit test: embed a few texts, verify dimension and non-zero vectors
- [x] Unit test: similar texts have higher cosine similarity than dissimilar ones

**Commit**: `feat(embed): implement FastEmbedEmbedder with local BGE model`

### Step 4 — Persistent vector store (usearch)
- [x] Add `usearch` crate dependency
- [x] Implement `UsearchVectorStore` — on-disk HNSW index
- [x] Store at `<data_dir>/vectors.usearch`
- [x] `search_within`: filter HNSW results to candidate chunk IDs
- [x] Unit test: persist → reopen → search returns same results
- [x] Unit test: search_within scopes correctly

**Commit**: `feat(embed): implement UsearchVectorStore for persistent vector search`

### Step 5 — Integrate embeddings into ingestion pipeline
- [x] Add optional `Embedder` + `VectorStore` to `IngestionPipeline`
- [x] After parse + enrich: embed each chunk's text, upsert into vector store
- [x] On file update: re-embed changed chunks, delete removed chunks
- [x] On file delete: delete chunk vectors
- [x] Skip embedding if embedder not configured (backwards compatible)
- [x] Integration test: index vault with embedder → vectors present for all chunks

**Commit**: `feat(ingest): integrate embedding generation into ingestion pipeline`

### Step 6 — Semantic query in query engine
- [x] Add `semantic_query` method to `BitmapQueryEngine`
- [x] Implementation: bitmap filter → get matching doc IDs → resolve chunk IDs → vector `search_within` → build `ScoredPointer` results
- [x] Bitmap filter is mandatory (no full-space vector search)
- [x] Unit test: bitmap pre-filter reduces vector search candidates
- [x] Integration test: ingest → semantic query → ranked chunk results

**Commit**: `feat(query): implement bitmap-scoped semantic search`

### Step 7 — CLI and interface commands
- [x] `locus semantic "query text" --filter tag:X` — semantic search with bitmap pre-filter
- [x] JSON output support for semantic results
- [x] Wire up embedder/vector store in persistent mode

**Commit**: `feat(cli): add semantic search CLI command`

### Step 8 — Docs and benchmarks
- [x] Update `001-system-overview.md` with semantic layer in pipeline diagram
- [x] Update `003-contracts.md` with semantic types
- [x] Update `002-roadmap.md` to reflect WS2 progress
- [ ] Benchmark: bitmap-only vs bitmap+vector query latency
- [ ] Document benchmark results

**Commit**: `docs(arch): update docs for semantic layer`

### Step 9 — Reranker integration
- [x] Implement `FastEmbedReranker` in `locus-embed` (BGE-Reranker-Base, local ONNX cross-encoder)
- [x] Implements `Reranker` trait from `locus-core`
- [x] Wire reranker into `BitmapQueryEngine::semantic_query()` as optional 4th arg
- [x] Chunk text resolved from disk via registry metadata (doc path + byte range)
- [x] Update all callers (CLI, tests) to pass `None` or reranker instance
- [x] Update `002-roadmap.md` and `003-contracts.md`
- [x] All tests pass

**Commit**: `feat(embed): implement FastEmbedReranker cross-encoder`

## Validation

- [x] All existing tests still pass (no regressions)
- [x] `locus semantic "query" --filter tag:work` returns ranked results
- [x] Vector search is scoped to bitmap-filtered docs (verified by candidate count)
- [x] Embedding generation works offline (local model, no API calls)
- [x] Second index run skips unchanged chunk embeddings
