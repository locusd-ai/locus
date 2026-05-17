# Task: Implement locus-cli (CLI Binary) ✅

## Goal
Implement the `locus` CLI binary with commands for indexing, searching, inspecting, and managing the BIEM index. Uses clap for argument parsing, delegates to the query engine and ingestion pipeline.

## Steps

### Step 1: Scaffold CLI with clap
- [x] Define `Cli` struct with subcommands: index, search, inspect, status, filters
- [x] Add `--data-dir` and `--memory` global options
- [x] Wire up in `locus-cli/src/main.rs`
- **Validate**: `cargo check -p locus-cli`

### Step 2: Index command
- [x] `locus index <vault>` — bulk index a vault directory
- [x] Construct `IngestionPipeline` with `MarkdownParser` + stores
- [x] Report docs indexed, bitmaps created, duration
- **Validate**: `cargo run -p locus-cli -- index tests/fixtures/`

### Step 3: Search command
- [x] `locus search <filters...>` with `--op` and `--limit`
- [x] Build `Filter` from CLI args, dispatch to `QueryEngine::query`
- [x] Pretty-print results as JSON
- **Validate**: `cargo run -p locus-cli -- search tag:work`

### Step 4: Inspect command
- [x] `locus inspect <path>` — show doc metadata, chunks, bitmap keys
- [x] Delegate to `QueryEngine::inspect`
- **Validate**: `cargo run -p locus-cli -- inspect /vault/note.md`

### Step 5: Status command
- [x] `locus status` — show index health
- [x] Delegate to `QueryEngine::status`
- **Validate**: `cargo run -p locus-cli -- status`

### Step 6: Filters command
- [x] `locus filters [--category tag]` — list bitmap keys with cardinality
- [x] Delegate to `QueryEngine::list_filter_keys`
- **Validate**: `cargo run -p locus-cli -- filters`

### Step 7: Persistent storage wiring
- [x] Open DuckDB + LMDB from `--data-dir` (default `~/.locus`)
- [x] Support `--memory` flag for in-memory stores
- **Validate**: end-to-end index + search with persistent storage

### Step 8: Commit
- [x] `feat(cli): implement locus CLI with index, search, inspect, status, filters`
- [x] `feat(cli,daemon): wire in persistent storage (DuckDB + LMDB)`
