# Workstream 2: Semantic Layer (Vectors)

> Goal: Add vector-based semantic search as a complement to bitmap filtering. Bitmaps pre-filter, vectors rank by similarity. Local-first by default.

## Steps

### Step 1 — Core types and traits in `biem-core`
- [ ] Add `EmbeddingVector` type alias (`Vec<f32>`)
- [ ] Add `ScoredPointer` and `ScoreSource` types
- [ ] Add `Embedder` trait: `embed(&[&str]) -> Vec<EmbeddingVector>`, `dimension() -> usize`
- [ ] Add `VectorStore` trait: `upsert`, `delete`, `search_within` (bitmap-scoped)
- [ ] Add `EmbedError`, `VectorError` error types
- [ ] Add `SemanticQueryRequest` and `SemanticQueryResult` types
- [ ] Re-export from `lib.rs`
- [ ] Update `003-contracts.md` with semantic types and traits
- [ ] `cargo test` passes

**Commit**: `feat(core): add semantic/vector types and Embedder/VectorStore traits`

### Step 2 — Create `biem-embed` crate with in-memory impls
- [ ] Create `crates/biem-embed/` with `Cargo.toml`
- [ ] Add to workspace `Cargo.toml`
- [ ] Implement `InMemoryVectorStore` (brute-force cosine similarity, for tests)
- [ ] Unit test: upsert → search round-trip
- [ ] Unit test: search_within respects candidate set
- [ ] Unit test: delete removes from results

**Commit**: `feat(embed): create biem-embed crate with InMemoryVectorStore`

### Step 3 — FastEmbed embedder (local model)
- [ ] Add `fastembed` crate dependency
- [ ] Implement `FastEmbedEmbedder` (BGE-small-en-v1.5, local ONNX inference)
- [ ] Handle model download/cache in `~/.biem/models/`
- [ ] Unit test: embed a few texts, verify dimension and non-zero vectors
- [ ] Unit test: similar texts have higher cosine similarity than dissimilar ones

**Commit**: `feat(embed): implement FastEmbedEmbedder with local BGE model`

### Step 4 — Persistent vector store (usearch)
- [ ] Add `usearch` crate dependency
- [ ] Implement `UsearchVectorStore` — on-disk HNSW index
- [ ] Store at `<data_dir>/vectors.usearch`
- [ ] `search_within`: filter HNSW results to candidate chunk IDs
- [ ] Unit test: persist → reopen → search returns same results
- [ ] Unit test: search_within scopes correctly

**Commit**: `feat(embed): implement UsearchVectorStore for persistent vector search`

### Step 5 — Integrate embeddings into ingestion pipeline
- [ ] Add optional `Embedder` + `VectorStore` to `IngestionPipeline`
- [ ] After parse + enrich: embed each chunk's text, upsert into vector store
- [ ] On file update: re-embed changed chunks, delete removed chunks
- [ ] On file delete: delete chunk vectors
- [ ] Skip embedding if embedder not configured (backwards compatible)
- [ ] Integration test: index vault with embedder → vectors present for all chunks

**Commit**: `feat(ingest): integrate embedding generation into ingestion pipeline`

### Step 6 — Semantic query in query engine
- [ ] Add `semantic_query` method to `QueryEngine` trait
- [ ] Implementation: bitmap filter → get matching doc IDs → resolve chunk IDs → vector `search_within` → build `ScoredPointer` results
- [ ] Bitmap filter is mandatory (no full-space vector search)
- [ ] Unit test: bitmap pre-filter reduces vector search candidates
- [ ] Integration test: ingest → semantic query → ranked chunk results

**Commit**: `feat(query): implement bitmap-scoped semantic search`

### Step 7 — CLI and interface commands
- [ ] `biem search --semantic "query text" [filters...]` — semantic search with bitmap pre-filter
- [ ] JSON output support for semantic results
- [ ] Config: `[semantic]` section in config (embedder model, vector store path)
- [ ] Wire up embedder/vector store in `biem init` and `biem index`

**Commit**: `feat(cli): add semantic search CLI command`

### Step 8 — Docs and benchmarks
- [ ] Update `001-system-overview.md` with semantic layer in pipeline diagram
- [ ] Update `003-contracts.md` with semantic types
- [ ] Update `002-roadmap.md` to reflect WS2 progress
- [ ] Benchmark: bitmap-only vs bitmap+vector query latency
- [ ] Document benchmark results

**Commit**: `docs(arch): update docs for semantic layer`

## Validation

- [ ] All existing tests still pass (no regressions)
- [ ] `biem search --semantic "retry logic" tag:work` returns ranked results
- [ ] Vector search is scoped to bitmap-filtered docs (verified by candidate count)
- [ ] Embedding generation works offline (local model, no API calls)
- [ ] Second index run skips unchanged chunk embeddings
