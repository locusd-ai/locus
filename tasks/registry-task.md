# Task: Implement biem-registry (DuckDB)

## Goal
Implement the `Registry` trait from `biem-core` using DuckDB as the storage backend, plus an in-memory implementation for testing.

## Steps

### Step 1: In-memory Registry implementation
- [ ] Create `crates/biem-registry/src/memory.rs`
- [ ] Implement `Registry` trait using `HashMap`/`Vec` in-memory storage
- [ ] Wire up module in `lib.rs`
- **Validate**: `cargo check -p biem-registry`

### Step 2: In-memory Registry tests
- [ ] Create `crates/biem-registry/src/memory_tests.rs` (or `#[cfg(test)]` module)
- [ ] Test `insert_doc` → assigns monotonic `DocId`
- [ ] Test `lookup_by_path` / `lookup_by_id` round-trip
- [ ] Test `bulk_insert_docs` returns correct IDs
- [ ] Test `update_doc` modifies hash/timestamp/auto_type
- [ ] Test `update_path` handles renames
- [ ] Test `replace_chunks` deletes old, inserts new, returns `ChunkId`s
- [ ] Test `get_chunks` retrieves chunks for a doc
- [ ] Test `upsert_catalog_entry` / `get_catalog_entry` round-trip
- [ ] Test `list_catalog` with category filter
- [ ] Test `get_global_state` reflects current counters
- [ ] Test `DuplicatePath` error on duplicate file path
- [ ] Test `NotFound` error on missing doc_id
- **Validate**: `cargo test -p biem-registry`

### Step 3: Add DuckDB dependency
- [ ] Add `duckdb` crate to workspace `Cargo.toml` and `biem-registry/Cargo.toml`
- [ ] Add `tracing` dependency for structured logging
- **Validate**: `cargo check -p biem-registry`

### Step 4: DuckDB schema & migrations
- [ ] Create `crates/biem-registry/src/duckdb.rs`
- [ ] Define `DuckDbRegistry` struct holding a `duckdb::Connection`
- [ ] Implement `fn new(path)` constructor that creates/opens DB file
- [ ] Implement `fn init_schema()` — CREATE TABLE IF NOT EXISTS for:
  - `documents(doc_id, file_path, source_type, blake3_hash, last_indexed, auto_type)`
  - `chunks(chunk_id, doc_id, kind, byte_start, byte_end, label, depth, metadata_json)`
  - `bitmap_catalog(bitmap_key, category, cardinality, last_updated)`
  - `global_state(next_doc_id, next_chunk_id, total_documents)` — single-row
- **Validate**: `cargo check -p biem-registry`

### Step 5: DuckDB Registry — document operations
- [ ] Implement `insert_doc` — increment `next_doc_id`, INSERT, update `total_documents`
- [ ] Implement `bulk_insert_docs` — single transaction
- [ ] Implement `lookup_by_path` — SELECT by file_path
- [ ] Implement `lookup_by_id` — SELECT by doc_id
- [ ] Implement `lookup_by_ids` — SELECT WHERE doc_id IN (...)
- [ ] Implement `update_doc` — UPDATE hash, last_indexed, auto_type
- [ ] Implement `update_path` — UPDATE file_path
- **Validate**: `cargo check -p biem-registry`

### Step 6: DuckDB Registry — chunk & catalog operations
- [ ] Implement `replace_chunks` — DELETE WHERE doc_id, then INSERT
- [ ] Implement `get_chunks` — SELECT WHERE doc_id
- [ ] Implement `upsert_catalog_entry` — INSERT OR REPLACE
- [ ] Implement `bulk_upsert_catalog`
- [ ] Implement `get_catalog_entry` — SELECT by bitmap_key
- [ ] Implement `list_catalog` — SELECT with optional category filter
- [ ] Implement `get_global_state` — SELECT from global_state
- **Validate**: `cargo check -p biem-registry`

### Step 7: DuckDB integration tests
- [ ] Test all trait methods against DuckDB impl (use temp file / `:memory:`)
- [ ] Verify transactions: bulk_insert partial failure rolls back
- [ ] Verify schema idempotency (call init_schema twice)
- **Validate**: `cargo test -p biem-registry`

### Step 8: Review against contracts
- [ ] Compare `Registry` impl against trait in `biem-core/src/registry.rs` — all methods covered
- [ ] Cross-check with `003-contracts.md` §3 (Registry Contract) — signatures, error types, semantics match
- [ ] Cross-check with `001-system-overview.md` — registry diagrams, ER schema, and descriptions still accurate
- [ ] Verify DuckDB table schema matches the ER diagram in the architecture docs
- [ ] Confirm ID assignment is monotonic `u32`, not `u64`
- **Validate**: manual review, no code changes expected

### Step 9: Commit
- [ ] `feat(registry): implement in-memory Registry` (after step 2)
- [ ] `feat(registry): implement DuckDB Registry` (after step 7)
