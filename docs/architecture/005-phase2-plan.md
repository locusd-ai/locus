# BIEM Phase 2 — Enrichment, Vectors & Code Intelligence

> Status: **Planning**
> Depends on: Phase 1 (complete)
> Reference: `004-feature-set.md` for feature vision, `003-contracts.md` for trait interfaces

## Goals

Phase 2 transforms BIEM from an Obsidian-only structural index into a **multi-source enriched pointer system**. Three workstreams run in parallel:

1. **Enrichment pipeline** — pluggable taggers (builtin, LLM, custom) that infer semantic metadata and cache it per file hash
2. **Semantic layer** — vector embeddings as a complement to bitmap filtering, with bitmaps as pre-filter
3. **Code intelligence** — Tree-sitter parsing for codebases, AST-aware chunking, language/kind bitmaps

By the end of Phase 2, an LLM can query: `concept:auth AND lang:rust AND kind:function AND NOT quality:dead-code` across both a markdown wiki and a Rust codebase, in constant time.

---

## Architecture Changes

### New crates

| Crate | Responsibility |
|-------|---------------|
| `biem-enrich` | TagPipeline trait, builtin taggers, tagger cache, custom tagger loader |
| `biem-embed` | Embedding trait, model integration, vector store trait |
| `biem-code` | CodeParser (Tree-sitter), AST chunking, language detection |

### Modified crates

| Crate | Changes |
|-------|---------|
| `biem-core` | New types: `TaggerResult`, `EmbeddingVector`, `CodeChunkKind`. Extended `ChunkKind` enum |
| `biem-ingest` | Integrate `TagPipeline` after parsing, before bitmap write. Embed chunks if embedder configured |
| `biem-query` | New `SemanticQuery` mode: bitmap pre-filter → vector rerank → merged result |
| `biem-cli` | New commands: `biem init --type code`, `biem taggers`, `biem enrich` |
| `biem-daemon` | Register code watchers, serve enriched results |
| `biem-registry` | Schema additions: `tagger_cache`, `embeddings` tables |

### Pipeline flow (Phase 2)

```
file bytes
  │
  ▼
Parser (structural)          ← biem-parser (markdown) or biem-code (tree-sitter)
  │
  ▼
ParseResult { tags, links, type, chunks }
  │
  ▼
TagPipeline (cached)         ← biem-enrich
  ├── BuiltinTaggers         (deterministic, no I/O)
  ├── LlmTagger              (optional, API key required)
  └── CustomTaggers           (.biem/taggers/*.yaml)
  │
  ▼
EnrichedResult { ...ParseResult, inferred_tags }
  │
  ├──→ Bitmap Store          (all tags — structural + inferred)
  ├──→ Registry              (doc metadata, chunk metadata, tagger cache)
  └──→ Embedding Store       (optional, chunk embeddings for semantic search)
```

---

## Workstream 1: Enrichment Pipeline

### 1.1 Core trait and types

```rust
// biem-core additions

/// Result from a single tagger run, cached per blake3 hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaggerResult {
    pub tagger_name: String,
    pub tags: Vec<String>,
    pub confidence: Option<f32>,  // 0.0–1.0, None for rule-based taggers
}

/// Combined enrichment output for a document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentCache {
    pub content_hash: String,       // blake3 hex
    pub tagger_config_hash: String, // blake3 of tagger config, for invalidation
    pub results: Vec<TaggerResult>,
    pub timestamp: Timestamp,
}
```

```rust
// biem-enrich

/// A tagger produces additional bitmap keys from a parse result.
pub trait Tagger: Send + Sync {
    fn name(&self) -> &str;

    /// Whether this tagger should run for the given source type.
    fn applies_to(&self, source: SourceType) -> bool;

    /// Generate tags from the parse result. Content bytes provided for
    /// taggers that need to inspect raw text (topic extraction, etc.).
    fn tag(
        &self,
        parse_result: &ParseResult,
        content: &[u8],
    ) -> Result<TaggerResult, EnrichError>;
}

/// Orchestrates taggers with caching.
pub struct TagPipeline {
    taggers: Vec<Box<dyn Tagger>>,
    cache: Box<dyn TaggerCache>,
}

pub trait TaggerCache: Send + Sync {
    fn get(&self, content_hash: &str) -> Option<EnrichmentCache>;
    fn put(&self, cache: &EnrichmentCache);
    fn invalidate_tagger(&self, tagger_name: &str);
}
```

### 1.2 Builtin taggers

| Tagger | Input | Output keys | Implementation |
|--------|-------|-------------|----------------|
| **TopicTagger** | Raw content bytes | `topic:auth`, `topic:database`, ... | TF-IDF against a curated keyword vocabulary. No LLM. ~1ms per file |
| **ComplexityTagger** | Chunks + AST metadata | `complexity:low/medium/high` | Heuristic: chunk count, nesting depth, cyclomatic complexity (code) |
| **ConventionTagger** | File path + parse result | `convention:test`, `convention:migration`, `convention:config` | Path pattern matching (`*_test.rs`, `migrations/`, `*.config.*`) |
| **SizeTagger** | Content bytes | `size:small/medium/large` | Byte count buckets: <1KB, 1–10KB, >10KB |

All builtin taggers are deterministic, no I/O, fast enough to not need caching (but cached anyway for consistency).

### 1.3 LLM tagger (optional)

```rust
pub struct LlmTagger {
    client: Box<dyn LlmClient>,  // trait object — supports OpenAI, Anthropic, Ollama
    prompt_template: String,
    max_tokens: usize,
}
```

| Tagger | Prompt strategy | Output keys |
|--------|----------------|-------------|
| **ConceptTagger** | "List 3–5 concepts this file is about. Return as comma-separated tags." | `concept:jwt-validation`, `concept:retry-logic` |
| **IntentTagger** | "What is the primary purpose of this code/document? One word." | `intent:validation`, `intent:serialization` |
| **QualityTagger** | "Flag any code quality issues: dead code, missing tests, TODOs." | `quality:todo`, `quality:dead-code` |

Design constraints:
- LLM tagger is **opt-in** — requires `[enrichment.llm]` in config with API key
- Content sent is **chunk summaries**, not raw source (privacy-preserving)
- Results cached by blake3 hash — unchanged files never re-call the LLM
- Confidence field populated (LLM self-rates confidence)
- Rate limiting built in — configurable `max_concurrency` for API calls

### 1.4 Custom taggers (`.biem/taggers/*.yaml`)

```yaml
# .biem/taggers/team-ownership.yaml
name: team-ownership
version: 1  # bump to invalidate cache
rules:
  - match:
      folder: "src/payments/**"
    add_tags: ["team:payments", "domain:billing"]
  - match:
      folder: "src/auth/**"
      has_tag: "kind:function"
    add_tags: ["team:platform", "domain:identity"]
  - match:
      extension: ".tf"
      content_contains: "aws_lambda"
    add_tags: ["infra:serverless"]
```

```yaml
# .biem/taggers/priority.yaml
name: priority-signals
version: 1
rules:
  - match:
      all:
        - has_tag: "quality:todo"
        - has_tag: "complexity:high"
    add_tags: ["priority:tech-debt"]
  - match:
      has_tag: "concept:auth"
      modified_within: "7d"
    add_tags: ["attention:recent-auth-change"]
```

Custom tagger rules are loaded from both `.biem/taggers/` (project-local) and `~/.biem/taggers/` (global). Project-local takes precedence on name collision.

Cache invalidation: `version` field in the YAML. Bump it → all cached results for that tagger are invalidated on next index.

### 1.5 Task breakdown

- [ ] Define `Tagger` trait and `TaggerResult` types in `biem-core`
- [ ] Create `biem-enrich` crate with `TagPipeline` orchestrator
- [ ] Implement `TaggerCache` backed by filesystem (`.biem/cache/taggers/`)
- [ ] Implement `TopicTagger` (TF-IDF keyword extraction)
- [ ] Implement `ConventionTagger` (path pattern matching)
- [ ] Implement `ComplexityTagger` (heuristic scoring)
- [ ] Implement `SizeTagger` (byte count buckets)
- [ ] YAML custom tagger loader and rule evaluator
- [ ] Integrate `TagPipeline` into `IngestionPipeline` (after parse, before bitmap write)
- [ ] `biem taggers` CLI command — list active taggers, show cache stats
- [ ] `biem enrich --force` CLI command — re-run taggers, ignore cache
- [ ] Unit tests: each builtin tagger against fixture files
- [ ] Integration test: custom YAML tagger + bitmap query on inferred tags
- [ ] LLM tagger: `LlmClient` trait + OpenAI/Anthropic/Ollama implementations
- [ ] LLM tagger: rate limiting, chunk summarisation, confidence scoring
- [ ] Config: `[enrichment]` section in `config.toml`
- [ ] Docs: update `003-contracts.md` with enrichment types

---

## Workstream 2: Semantic Layer (Vectors)

### 2.1 Design

Bitmaps answer "which docs match these structural/semantic tags?" — vectors answer "which chunks are most similar to this text?". The composition:

```
User query: "How does retry logic work in the payments service?"
  │
  ▼
BIEM bitmap pre-filter (19µs):
  concept:retry-logic AND source:repo:payments AND kind:function
  → 6 doc IDs
  │
  ▼
Vector search (scoped to those 6 docs' chunks):
  embed(query) → top-5 chunks by cosine similarity
  → 5 ChunkPointers with scores
  │
  ▼
Optional re-ranker:
  cross-encoder scores on (query, chunk_text) pairs
  → re-ordered ChunkPointers
```

Bitmaps reduce the vector search space from 100K chunks to ~20 chunks. This makes vector search both faster and more accurate (less noise in the candidate set).

### 2.2 Types and traits

```rust
// biem-core additions

pub type EmbeddingVector = Vec<f32>;

/// A scored chunk pointer from semantic search.
#[derive(Debug, Clone)]
pub struct ScoredPointer {
    pub pointer: MatchPointer,
    pub chunk_id: ChunkId,
    pub score: f32,        // 0.0–1.0, cosine similarity or reranker score
    pub score_source: ScoreSource,
}

#[derive(Debug, Clone)]
pub enum ScoreSource {
    CosineSimilarity,
    Reranker(String),  // model name
}
```

```rust
// biem-embed

/// Generate embeddings for text chunks.
pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[&str]) -> Result<Vec<EmbeddingVector>, EmbedError>;
    fn dimension(&self) -> usize;
}

/// Store and search embeddings.
pub trait VectorStore: Send + Sync {
    fn upsert(&self, chunk_id: ChunkId, vector: &EmbeddingVector) -> Result<(), VectorError>;
    fn delete(&self, chunk_id: ChunkId) -> Result<(), VectorError>;

    /// Search within a subset of chunk IDs (bitmap pre-filtered).
    fn search_within(
        &self,
        query: &EmbeddingVector,
        candidate_chunk_ids: &[ChunkId],
        top_k: usize,
    ) -> Result<Vec<(ChunkId, f32)>, VectorError>;
}

/// Optional cross-encoder reranker.
pub trait Reranker: Send + Sync {
    fn rerank(
        &self,
        query: &str,
        candidates: &[(ChunkId, &str)],  // (id, chunk_text)
    ) -> Result<Vec<(ChunkId, f32)>, RerankError>;
}
```

### 2.3 Implementation choices

| Component | Primary | Alternative |
|-----------|---------|-------------|
| **Embedder** | `fastembed` crate (BGE-small-en-v1.5, local, no API) | OpenAI `text-embedding-3-small` via API |
| **VectorStore** | In-process with `usearch` or `hnsw_rs` | Qdrant (external, for scale) |
| **Reranker** | `fastembed` cross-encoder (local) | Skip for v1 |

Design constraint: **local-first**. The default config runs everything on-device with no API calls. API-based embedders are opt-in via `[semantic.embedder]` config.

### 2.4 Query engine extension

```rust
// biem-query additions

pub struct SemanticQueryRequest {
    /// Bitmap pre-filter (required — never search the full vector space)
    pub filter: Filter,
    /// Natural language query for vector similarity
    pub query_text: String,
    /// Max results
    pub top_k: usize,
    /// Whether to apply reranker
    pub rerank: bool,
}

pub struct SemanticQueryResult {
    pub pointers: Vec<ScoredPointer>,
    pub bitmap_candidates: u32,  // how many docs the bitmap filter returned
    pub vector_searched: u32,    // how many chunks were vector-searched
    pub elapsed_bitmap_us: u64,
    pub elapsed_vector_us: u64,
    pub elapsed_rerank_us: u64,
}
```

The key contract: **bitmap filter is mandatory for semantic queries**. There is no "search everything" mode. This ensures vector search stays fast and focused.

### 2.5 Task breakdown

- [ ] Define `Embedder`, `VectorStore`, `Reranker` traits in `biem-core`
- [ ] Create `biem-embed` crate with trait implementations
- [ ] Implement `FastEmbedEmbedder` (local, `fastembed` crate)
- [ ] Implement `InMemoryVectorStore` for tests
- [ ] Implement `UsearchVectorStore` (persistent, on-disk)
- [ ] Integrate embedding generation into ingestion pipeline (per chunk)
- [ ] Implement `SemanticQueryRequest` in `BitmapQueryEngine`
- [ ] Add `search_within` to vector store (bitmap-scoped search)
- [ ] Implement `FastEmbedReranker` (optional cross-encoder)
- [ ] `biem search --semantic "query text" --filter "tag:X"` CLI
- [ ] MCP tool: `biem_semantic_search`
- [ ] HTTP: `POST /search/semantic`
- [ ] Benchmarks: latency and recall — bitmap-only vs bitmap+vector vs vector-only
- [ ] Config: `[semantic]` section in `config.toml` (embedder, vector store, reranker)
- [ ] Docs: update contracts with semantic types

---

## Workstream 3: Code Intelligence

### 3.1 CodeParser

```rust
// biem-code

pub struct CodeParser {
    /// Map of extension → tree-sitter Language
    languages: HashMap<String, tree_sitter::Language>,
}

impl Parser for CodeParser {
    fn can_parse(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|ext| self.languages.contains_key(ext))
            .unwrap_or(false)
    }

    fn parse(&self, path: &Path, content: &[u8]) -> Result<ParseResult, ParseError> {
        // 1. Detect language from extension
        // 2. Parse with tree-sitter → AST
        // 3. Walk AST → extract chunks (functions, classes, impls, modules)
        // 4. Extract metadata per chunk (visibility, async, generic params)
        // 5. Extract imports → link refs
        // 6. Generate tags: lang:X, kind:function, kind:class, is_exported, is_test, etc.
        // 7. Return ParseResult with code-specific ChunkKinds
    }
}
```

### 3.2 Extended ChunkKind

```rust
// biem-core additions to ChunkKind

pub enum ChunkKind {
    // Existing (markdown)
    Heading,
    Frontmatter,
    Paragraph,

    // New (code)
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,    // trait in Rust, interface in TS/Java
    Module,
    Import,
    Constant,
    TypeAlias,
    ImplBlock,    // Rust-specific
    Test,         // #[test], describe(), it()
}
```

### 3.3 Code-specific bitmap keys

| Key pattern | Source | Example |
|------------|--------|---------|
| `lang:rust` | File extension | Identifies language |
| `kind:function` | AST node type | All functions across all languages |
| `kind:test` | AST + naming convention | Test functions/blocks |
| `visibility:public` | AST (pub, export) | Exported API surface |
| `async:true` | AST modifier | Async functions |
| `import:tokio` | Import statement | Files that depend on tokio |
| `import:react` | Import statement | Files that depend on React |
| `repo:backend` | Config registration | Multi-repo scoping |
| `has_side_effects:true` | Heuristic (I/O, mutation) | Functions with side effects |

### 3.4 Language support (initial)

| Language | Tree-sitter grammar | Priority |
|----------|-------------------|----------|
| Rust | `tree-sitter-rust` | P0 (dogfooding) |
| TypeScript/JavaScript | `tree-sitter-typescript` | P0 |
| Python | `tree-sitter-python` | P0 |
| Go | `tree-sitter-go` | P1 |
| Java/Kotlin | `tree-sitter-java` | P1 |
| YAML/TOML/JSON | `tree-sitter-yaml`, etc. | P1 (config files) |
| HCL (Terraform) | `tree-sitter-hcl` | P2 (infra) |
| SQL | `tree-sitter-sql` | P2 (schema) |

### 3.5 `.biemignore`

Same syntax as `.gitignore`, checked before parsing:

```
# .biemignore
target/
node_modules/
dist/
*.generated.*
vendor/
__pycache__/
```

### 3.6 Task breakdown

- [ ] Create `biem-code` crate
- [ ] Implement `CodeParser` with Tree-sitter integration
- [ ] Rust grammar: function, impl, struct, enum, trait, mod, use extraction
- [ ] TypeScript grammar: function, class, interface, import extraction
- [ ] Python grammar: function, class, import, decorator extraction
- [ ] Extended `ChunkKind` variants in `biem-core`
- [ ] Code-specific bitmap key generation (lang, kind, visibility, async, import)
- [ ] `.biemignore` loader and path filtering
- [ ] `biem init <path> --type code` CLI command
- [ ] Multi-repo registration in config (repo name → path mapping)
- [ ] Integration test: index a Rust crate, query by `lang:rust AND kind:function AND visibility:public`
- [ ] Integration test: index TS project, query by `import:react AND kind:class`
- [ ] Benchmarks: code parsing throughput (files/s, lines/s)
- [ ] Docs: update contracts with code types

---

## Cross-cutting concerns

### Config additions (`config.toml`)

```toml
[enrichment]
enabled = true
builtin_taggers = ["topic", "convention", "complexity", "size"]

[enrichment.llm]
enabled = false
backend = "openai"          # openai | anthropic | ollama
model = "gpt-4o-mini"
max_concurrency = 4
# API key from environment: OPENAI_API_KEY, ANTHROPIC_API_KEY, etc.

[enrichment.custom]
local_dir = ".biem/taggers"
global_dir = "~/.biem/taggers"

[semantic]
enabled = false
embedder = "fastembed"      # fastembed | openai
model = "BAAI/bge-small-en-v1.5"
vector_store = "usearch"    # memory | usearch | qdrant
reranker = "none"           # none | fastembed

[sources.code]
# Registered code repos
# [sources.code.backend]
# path = "/home/user/repos/backend"
# languages = ["rust", "toml"]
```

### Global vs local state

| Location | What lives there |
|----------|-----------------|
| `~/.biem/` | Global config, global taggers, cross-project bitmap namespace |
| `.biem/` (project root) | Project config overrides, local taggers, tagger cache, bitmap store, vector store |
| In-process | Registry (DuckDB), bitmap store (LMDB), vector store (usearch) |

### Performance targets

| Metric | Target |
|--------|--------|
| Enriched index throughput (builtin taggers only) | > 15K files/s |
| Enriched index throughput (with LLM tagger) | Limited by API (cached: same as builtin) |
| Semantic query (bitmap pre-filter + vector rerank, 100K docs) | < 50ms |
| Code parsing throughput (Rust/TS/Python) | > 10K files/s |
| Incremental re-index with tagger cache hit | Same as Phase 1 (~20K files/s) |

---

## Module build order

```mermaid
gantt
    title Phase 2 — Enrichment, Vectors & Code
    dateFormat YYYY-MM-DD
    axisFormat Week %W

    section Enrichment
    Tagger trait + types (biem-core)               :e1, 2026-05-19, 1w
    biem-enrich crate + TagPipeline                :e2, after e1, 1w
    Builtin taggers (topic, convention, complexity) :e3, after e2, 2w
    Custom YAML tagger loader                      :e4, after e2, 1w
    Tagger cache (filesystem-backed)               :e5, after e2, 1w
    Integrate into IngestionPipeline               :e6, after e3, 1w
    LLM tagger (opt-in)                            :e7, after e6, 2w
    CLI: biem taggers, biem enrich                 :e8, after e6, 1w

    section Vectors
    Embedder + VectorStore traits (biem-core)       :v1, after e1, 1w
    biem-embed crate + FastEmbed                    :v2, after v1, 2w
    InMemory + Usearch vector stores                :v3, after v2, 2w
    Integrate embedding into ingestion              :v4, after v3, 1w
    SemanticQuery in query engine                   :v5, after v4, 2w
    Reranker (optional)                             :v6, after v5, 1w
    CLI + MCP + HTTP semantic search                :v7, after v5, 1w

    section Code
    biem-code crate + CodeParser trait              :c1, after e1, 1w
    Rust grammar + chunk extraction                 :c2, after c1, 2w
    TypeScript grammar                              :c3, after c2, 1w
    Python grammar                                  :c4, after c3, 1w
    Code bitmap key generation                      :c5, after c2, 1w
    .biemignore                                     :c6, after c1, 1w
    biem init --type code                           :c7, after c5, 1w
    Multi-repo registration                         :c8, after c7, 1w
```

### Dependency graph

```
biem-core (types)
  ├── biem-enrich (taggers, cache)
  │     └── integrates into biem-ingest
  ├── biem-embed (embeddings, vector store)
  │     └── integrates into biem-ingest + biem-query
  └── biem-code (tree-sitter parsing)
        └── plugs into biem-ingest as a Parser impl
```

All three workstreams share `biem-core` types but are otherwise independent — they can be developed and shipped incrementally.

---

## Validation criteria

Phase 2 is complete when:

1. **Enrichment**: `biem search --filter "concept:auth AND team:payments"` returns results on an Obsidian vault with LLM tagger + custom tagger
2. **Vectors**: `biem search --semantic "retry logic" --filter "kind:function"` returns scored chunk pointers with bitmap pre-filtering
3. **Code**: `biem init ~/repos/biem --type code` indexes BIEM itself, and `biem search --filter "lang:rust AND kind:function AND visibility:public"` returns correct results
4. **Performance**: enriched index (builtin taggers) at >15K files/s; semantic query <50ms; code parsing >10K files/s
5. **Cache**: second run of `biem enrich` on unchanged files hits cache, completes at Phase 1 speed
6. **All Phase 1 tests still pass** — no regressions
