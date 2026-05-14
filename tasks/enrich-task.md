# Workstream 1: Enrichment Pipeline

> Goal: Pluggable tagger system that produces inferred bitmap keys from parse results, with caching.

## Steps

### Step 1 — Core types in `biem-core`
- [ ] Add `TaggerResult`, `EnrichmentCache` types to `biem-core/src/types.rs`
- [ ] Add `Enrichment`, `Custom` variants to `BitmapCategory`
- [ ] Add `EnrichError` to a new `biem-core/src/enrich.rs` module
- [ ] Add `Tagger` trait and `TaggerCache` trait to `biem-core/src/enrich.rs`
- [ ] Re-export from `lib.rs`
- [ ] Update `003-contracts.md` with enrichment types and traits
- [ ] `cargo test` passes

**Commit**: `feat(core): add enrichment types and Tagger/TaggerCache traits`

### Step 2 — Create `biem-enrich` crate with `TagPipeline`
- [ ] Create `crates/biem-enrich/` with `Cargo.toml`
- [ ] Add to workspace `Cargo.toml`
- [ ] Implement `TagPipeline` struct: holds `Vec<Box<dyn Tagger>>` + `Box<dyn TaggerCache>`
- [ ] `TagPipeline::enrich(path, content, parse_result) -> Vec<String>` — runs taggers, merges results
- [ ] Implement `InMemoryTaggerCache` for tests
- [ ] Unit test: pipeline with no taggers returns empty
- [ ] Unit test: pipeline with mock tagger returns its tags
- [ ] Unit test: cache hit skips tagger execution

**Commit**: `feat(enrich): create biem-enrich crate with TagPipeline`

### Step 3 — Filesystem-backed `TaggerCache`
- [ ] Implement `FsTaggerCache` — reads/writes JSON files keyed by blake3 hash
- [ ] Cache dir: `<data_dir>/cache/taggers/`
- [ ] Cache invalidation: compare `tagger_config_hash`
- [ ] Unit test: write → read round-trip
- [ ] Unit test: config hash mismatch → cache miss

**Commit**: `feat(enrich): filesystem-backed tagger cache`

### Step 4 — Builtin taggers
- [ ] `SizeTagger` — `size:small` (<1KB), `size:medium` (1–10KB), `size:large` (>10KB)
- [ ] `ConventionTagger` — path patterns → `convention:test`, `convention:config`, `convention:migration`
- [ ] `TopicTagger` — keyword extraction from content → `topic:*` keys
- [ ] `ComplexityTagger` — chunk count + heading depth → `complexity:low/medium/high`
- [ ] Unit tests for each tagger against fixture content

**Commit**: `feat(enrich): implement builtin taggers (size, convention, topic, complexity)`

### Step 5 — Custom YAML tagger loader
- [ ] Define YAML schema: `name`, `version`, `rules[]` with `match` + `add_tags`
- [ ] Match conditions: `folder`, `extension`, `has_tag`, `content_contains`
- [ ] Load from `.biem/taggers/` and `~/.biem/taggers/` (project-local takes precedence)
- [ ] Implement as `Tagger` trait object
- [ ] Unit test: YAML rule matches folder pattern
- [ ] Unit test: YAML rule matches has_tag condition
- [ ] Create fixture YAML tagger for integration tests

**Commit**: `feat(enrich): custom YAML tagger loader and rule evaluator`

### Step 6 — Integrate into `IngestionPipeline`
- [ ] Add optional `TagPipeline` to `IngestionPipeline` (None = no enrichment, backwards compatible)
- [ ] After parse, before bitmap write: run `tag_pipeline.enrich()` → merge inferred tags into bitmap keys
- [ ] Inferred tags get `BitmapCategory::Enrichment` in catalog
- [ ] Update `bulk_index` and `process_event` paths
- [ ] Integration test: index vault with SizeTagger → query `size:small` returns correct docs
- [ ] Integration test: index with custom YAML tagger → query inferred tag

**Commit**: `feat(ingest): integrate TagPipeline into ingestion`

### Step 7 — CLI commands
- [ ] `biem taggers` — list active taggers (builtin + custom), show cache stats
- [ ] `biem enrich --force` — re-run all taggers, ignore cache
- [ ] Wire up enrichment config in `biem init` and `biem index`

**Commit**: `feat(cli): add taggers and enrich commands`

### Step 8 — Config and docs
- [ ] Add `[enrichment]` section to config schema
- [ ] Update `001-system-overview.md` pipeline diagram to show TagPipeline
- [ ] Update `003-contracts.md` with final enrichment types
- [ ] Update `002-roadmap.md` to reflect WS1 progress

**Commit**: `docs(arch): update contracts and overview for enrichment pipeline`

## Validation

- [ ] All existing Phase 1 tests still pass (no regressions)
- [ ] `biem search --filter "size:small AND tag:work"` works on an enriched index
- [ ] Cache hit: second `biem index` on unchanged vault skips taggers, same speed as Phase 1
- [ ] Custom YAML tagger produces queryable bitmap keys
