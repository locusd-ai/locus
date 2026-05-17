# Task: Implement locus-registry (DuckDB) ✅

## Goal
Implement the `Registry` trait from `locus-core` using DuckDB as the storage backend, plus an in-memory implementation for testing.

## Steps

### Step 1: In-memory Registry implementation ✅
- [x] Create `crates/locus-registry/src/memory.rs`
- [x] Implement `Registry` trait using `HashMap`/`Vec` in-memory storage
- [x] Wire up module in `lib.rs`
- **Validate**: `cargo check -p locus-registry` ✅

### Step 2: In-memory Registry tests ✅
- [x] Create `crates/locus-registry/src/memory_tests.rs` (or `#[cfg(test)]` module)
- [x] Test `insert_doc` → assigns monotonic `DocId`
- [x] Test `lookup_by_path` / `lookup_by_id` round-trip
- [x] Test `bulk_insert_docs` returns correct IDs
- [x] Test `update_doc` modifies hash/timestamp/auto_type
- [x] Test `update_path` handles renames
- [x] Test `replace_chunks` deletes old, inserts new, returns `ChunkId`s
- [x] Test `get_chunks` retrieves chunks for a doc
- [x] Test `upsert_catalog_entry` / `get_catalog_entry` round-trip
- [x] Test `list_catalog` with category filter
- [x] Test `get_global_state` reflects current counters
- [x] Test `DuplicatePath` error on duplicate file path
- [x] Test `NotFound` error on missing doc_id
- **Validate**: `cargo test -p locus-registry` ✅ (18 tests)

### Step 3: Add DuckDB dependency ✅
- [x] Add `duckdb` crate to workspace `Cargo.toml` and `locus-registry/Cargo.toml`
- [x] Add `tracing` dependency for structured logging
- **Validate**: `cargo check -p locus-registry` ✅

### Step 4: DuckDB schema & migrations ✅
- [x] Create `crates/locus-registry/src/duckdb.rs`
- [x] Define `DuckDbRegistry` struct holding `Mutex<duckdb::Connection>` (Mutex needed for Sync)
- [x] Implement `fn new(path)` constructor that creates/opens DB file
- [x] Implement `fn init_schema()` — CREATE TABLE IF NOT EXISTS for:
  - `documents(doc_id, file_path, source_type, blake3_hash, last_indexed, auto_type)`
  - `chunks(chunk_id, doc_id, kind, byte_start, byte_end, label, depth, metadata_json)`
  - `bitmap_catalog(bitmap_key, category, cardinality, last_updated)`
  - `global_state(next_doc_id, next_chunk_id, total_documents)` — single-row
- **Validate**: `cargo check -p locus-registry` ✅

### Step 5: DuckDB Registry — document operations ✅
- [x] Implement `insert_doc` — increment `next_doc_id`, INSERT, update `total_documents`
- [x] Implement `bulk_insert_docs` — single transaction
- [x] Implement `lookup_by_path` — SELECT by file_path
- [x] Implement `lookup_by_id` — SELECT by doc_id
- [x] Implement `lookup_by_ids` — SELECT WHERE doc_id IN (...)
- [x] Implement `update_doc` — UPDATE hash, last_indexed, auto_type
- [x] Implement `update_path` — UPDATE file_path
- **Validate**: `cargo check -p locus-registry` ✅

### Step 6: DuckDB Registry — chunk & catalog operations ✅
- [x] Implement `replace_chunks` — DELETE WHERE doc_id, then INSERT
- [x] Implement `get_chunks` — SELECT WHERE doc_id
- [x] Implement `upsert_catalog_entry` — INSERT OR REPLACE
- [x] Implement `bulk_upsert_catalog`
- [x] Implement `get_catalog_entry` — SELECT by bitmap_key
- [x] Implement `list_catalog` — SELECT with optional category filter
- [x] Implement `get_global_state` — SELECT from global_state
- **Validate**: `cargo check -p locus-registry` ✅

### Step 7: DuckDB integration tests ✅
- [x] Test all trait methods against DuckDB impl (use `:memory:`)
- [x] Verify schema idempotency (call init_schema twice)
- **Validate**: `cargo test -p locus-registry` ✅ (14 tests)

### Step 8: Review against contracts ✅
- [x] Compare `Registry` impl against trait in `locus-core/src/registry.rs` — all 13 methods covered
- [x] Cross-check with `003-contracts.md` §3 — signatures, error types, semantics match
- [x] Cross-check with `001-system-overview.md` — diagrams still accurate
- [x] Verify DuckDB table schema matches the ER diagram
- [x] Confirm ID assignment is monotonic `u32`, not `u64`
- **Note**: DuckDB Connection wrapped in Mutex for Send+Sync — aligns with sync-core architecture

### Step 9: Commit ✅
- [x] `feat(registry): implement in-memory Registry` — 18 tests
- [x] `feat(registry): implement DuckDB Registry` — 14 tests
