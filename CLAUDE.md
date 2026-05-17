# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Locus is

Locus is a local-first indexing and retrieval engine for LLMs. Its core goal is to **minimise context pollution** by returning precise structural pointers (not content) via bitmap pre-filtering. Phase 1 scope: Obsidian vault + codebase indexing.

Two binaries: `locus` (CLI) and `locusd` (daemon with watcher/ingestion + optional MCP/HTTP).

## Commands

```bash
# Build everything
cargo build

# Build release binaries
cargo build --release

# Run all tests
cargo test

# Run tests for a single crate
cargo test -p locus-query

# Run a single test by name
cargo test -p locus-ingest pipeline_tests::test_bulk_index

# Run with logging
RUST_LOG=debug cargo test -p locus-ingest

# CLI usage (after build)
cargo run --bin locus -- init /path/to/vault
cargo run --bin locus -- index /path/to/vault
cargo run --bin locus -- query --filter "tag:work AND type:task"
cargo run --bin locus -- semantic "how do I configure authentication?"
cargo run --bin locus -- inspect /path/to/file.md
```

## Crate Architecture

```
locus-core      — shared types, traits (Parser, Registry, BitmapStore, Embedder, etc.)
locus-parser    — markdown parser (pure function, no I/O)
locus-code      — code parser via tree-sitter (Rust, TypeScript, Python)
locus-registry  — DuckDB implementation of Registry trait; InMemoryRegistry for tests
locus-bitmap    — LMDB/heed implementation of BitmapStore; InMemoryBitmapStore for tests
locus-enrich    — TagPipeline: builtin taggers + YAML-defined custom taggers
locus-ingest    — IngestionPipeline: hash → diff → parse → enrich → embed → write
locus-watcher   — filesystem watcher (notify + debounce) feeding IngestionPipeline
locus-embed     — FastEmbedEmbedder (local ONNX), UsearchVectorStore (HNSW), FastEmbedReranker
locus-query     — BitmapQueryEngine: filter resolution → bitmap intersection → vector rerank
locus-cli       — clap CLI wiring up all crates into `locus` binary
locus-daemon    — `locusd` binary: watcher loop + MCP server + HTTP API (axum)
```

### Key design decisions

- **Trait objects (`Box<dyn Trait>`) not generics** — all pluggable boundaries use trait objects
- **Sync core, async at boundary only** — tokio for MCP/HTTP; sync storage is called via `spawn_blocking`
- **IDs are `u32`** — both `DocId` and `ChunkId`; bitmaps index `DocId` only, chunks resolved via registry lookup
- **Registry** (DuckDB) stores: documents, chunks, bitmap_catalog, global_state
- **BitmapStore** (LMDB + Roaring) stores namespaced keys like `tag:work`, `folder:/projects`, `type:task`, `source:obsidian`
- **Tombstone bitmap** for lazy deletes — subtracted from all query results; compaction cleans up later
- **Parsers are pure functions** — `can_parse(path)` + `parse(path, &[u8]) -> ParseResult`, no I/O or state
- **Returns pointers, not content** — `MatchPointer { doc_id, path, chunks, match_reason }`
- **State directory**: global `~/.locus/` by default; per-vault `.locus/` with `--local` flag

### Enrichment (locus-enrich)

`TagPipeline` runs a list of `Tagger` impls against each `ParseResult` to produce inferred `BitmapKey`s:
- Builtin taggers: `TopicTagger`, `ComplexityTagger`, `SizeTagger`, `ConventionTagger`
- YAML-defined taggers: loaded from `taggers/*.yaml` in the vault
- Results are cached (in-memory or fs-backed) to avoid recomputation on unchanged files

### Semantic layer (locus-embed)

- `FastEmbedEmbedder` — local ONNX inference, no external service required
- `UsearchVectorStore` — persistent HNSW index for cosine similarity search
- `FastEmbedReranker` — cross-encoder reranking of bitmap-filtered candidates
- Features are gated: `fastembed-embedder`, `usearch-store` — not compiled by default in tests

## Testing conventions

- Unit tests use `InMemoryRegistry` and `InMemoryBitmapStore` — never hit disk
- Integration tests use fixture vault files in `tests/fixtures/` (markdown) and `tempfile` for state dirs
- Test against the trait interface, not the concrete implementation
- Code parser tests in `locus-code/src/parser.rs`; pipeline tests in `locus-ingest/tests/`

## Code style

- Per-module error enums with `thiserror`; `anyhow` only in binary crates (`locus-cli`, `locus-daemon`)
- `#[from]` for error conversions across module boundaries
- Structured logging via `tracing` (`info!`, `warn!`, `#[instrument]`)
- Never use generics for trait boundaries — always `Box<dyn Trait>`
- Never store Locus state inside the vault by default — use global `~/.locus/`
- Never return content from query responses — return pointers only

## Task-driven development

Every feature has a task file in `tasks/<crate>-task.md`. Check for an existing task before starting work; create one if none exists. Mark steps completed as work progresses. After a feature is done, add ✅ to the task title.

## Documentation

Architecture decisions live in `docs/architecture/`:
- `001-system-overview.md` — module decomposition, diagrams, key decisions
- `002-roadmap.md` — phase-by-phase build plan with task checklists
- `003-contracts.md` — Rust trait signatures and shared types at every module boundary

**Changes to types or traits must be reflected in both `001-system-overview.md` and `003-contracts.md`.** Use Mermaid for all diagrams.

## Git conventions

Conventional commits: `type(scope): description`

- Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `ci`
- Scopes: `arch`, `core`, `parser`, `registry`, `bitmap`, `ingest`, `watcher`, `query`, `cli`, `daemon`, `embed`, `enrich`, `code`

Example: `feat(query): wire reranker into semantic_query pipeline`
