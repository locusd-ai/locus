# Workstream 1: Enrichment Pipeline ✅

> Goal: Pluggable tagger system that produces inferred bitmap keys from parse results, with caching.

## Steps

### Step 1 — Core types in `biem-core`
- [x] Add `TaggerResult`, `EnrichmentCache` types to `biem-core/src/types.rs`
- [x] Add `Enrichment`, `Custom` variants to `BitmapCategory`
- [x] Add `EnrichError` to a new `biem-core/src/enrich.rs` module
- [x] Add `Tagger` trait and `TaggerCache` trait to `biem-core/src/enrich.rs`
- [x] Re-export from `lib.rs`
- [x] Update `003-contracts.md` with enrichment types and traits
- [x] `cargo test` passes

**Commit**: `feat(core): add enrichment types and Tagger/TaggerCache traits`

### Step 2 — Create `biem-enrich` crate with `TagPipeline`
- [x] Create `crates/biem-enrich/` with `Cargo.toml`
- [x] Add to workspace `Cargo.toml`
- [x] Implement `TagPipeline` struct: holds `Vec<Box<dyn Tagger>>` + `Box<dyn TaggerCache>`
- [x] `TagPipeline::enrich(path, content, parse_result) -> Vec<String>` — runs taggers, merges results
- [x] Implement `InMemoryTaggerCache` for tests
- [x] Unit test: pipeline with no taggers returns empty
- [x] Unit test: pipeline with mock tagger returns its tags
- [x] Unit test: cache hit skips tagger execution

**Commit**: `feat(enrich): create biem-enrich crate with TagPipeline`

### Step 3 — Filesystem-backed `TaggerCache`
- [x] Implement `FsTaggerCache` — reads/writes JSON files keyed by blake3 hash
- [x] Cache dir: `<data_dir>/cache/taggers/`
- [x] Cache invalidation: compare `tagger_config_hash`
- [x] Unit test: write → read round-trip
- [x] Unit test: config hash mismatch → cache miss

**Commit**: `feat(enrich): filesystem-backed tagger cache`

### Step 4 — Builtin taggers
- [x] `SizeTagger` — `size:small` (<1KB), `size:medium` (1–10KB), `size:large` (>10KB)
- [x] `ConventionTagger` — path patterns → `convention:test`, `convention:config`, `convention:migration`
- [x] `TopicTagger` — keyword extraction from content → `topic:*` keys
- [x] `ComplexityTagger` — chunk count + heading depth → `complexity:low/medium/high`
- [x] Unit tests for each tagger against fixture content

**Commit**: `feat(enrich): implement builtin taggers (size, convention, topic, complexity)`

### Step 5 — Custom YAML tagger loader
- [x] Define YAML schema: `name`, `version`, `rules[]` with `match` + `add_tags`
- [x] Match conditions: `folder`, `extension`, `has_tag`, `content_contains`
- [x] Load from `.biem/taggers/` and `~/.biem/taggers/` (project-local takes precedence)
- [x] Implement as `Tagger` trait object
- [x] Unit test: YAML rule matches folder pattern
- [x] Unit test: YAML rule matches has_tag condition
- [x] Create fixture YAML tagger for integration tests

**Commit**: `feat(enrich): custom YAML tagger loader and rule evaluator`

### Step 6 — Integrate into `IngestionPipeline`
- [x] Add optional `TagPipeline` to `IngestionPipeline` (None = no enrichment, backwards compatible)
- [x] After parse, before bitmap write: run `tag_pipeline.enrich()` → merge inferred tags into bitmap keys
- [x] Inferred tags get `BitmapCategory::Enrichment` in catalog
- [x] Update `bulk_index` and `process_event` paths
- [x] Integration test: index vault with SizeTagger → query `size:small` returns correct docs
- [x] Integration test: index with custom YAML tagger → query inferred tag

**Commit**: `feat(ingest): integrate TagPipeline into ingestion`

### Step 7 — CLI commands
- [x] `biem taggers` — list active taggers (builtin + custom), show cache stats
- [x] `biem enrich --force` — re-run all taggers, ignore cache
- [x] Wire up enrichment config in `biem init` and `biem index`

**Commit**: `feat(cli): add taggers and enrich commands`

### Step 8 — Config and docs
- [x] Add `[enrichment]` section to config schema
- [x] Update `001-system-overview.md` pipeline diagram to show TagPipeline
- [x] Update `003-contracts.md` with final enrichment types
- [x] Update `002-roadmap.md` to reflect WS1 progress

**Commit**: `docs(arch): update contracts and overview for enrichment pipeline`

## Validation

- [x] All existing Phase 1 tests still pass (no regressions)
- [x] `biem search --filter "size:small AND tag:work"` works on an enriched index
- [x] Cache hit: second `biem index` on unchanged vault skips taggers, same speed as Phase 1
- [x] Custom YAML tagger produces queryable bitmap keys
