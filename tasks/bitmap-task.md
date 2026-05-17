# Task: Implement locus-bitmap (In-memory + LMDB/heed) ✅

## Goal
Implement the `BitmapStore` trait from `locus-core` with an in-memory backend (for tests) and an LMDB/heed backend (for production).

## Steps

### Step 1: In-memory BitmapStore implementation ✅
- [x] Create `crates/locus-bitmap/src/memory.rs`
- [x] Implement `BitmapStore` trait using `HashMap<String, RoaringBitmap>`
- [x] Tombstone stored under a reserved key (e.g. `__tombstone__`)
- [x] Wire up module in `lib.rs`
- **Validate**: `cargo check -p locus-bitmap` ✅

### Step 2: In-memory BitmapStore tests ✅
- [x] Create test module in `memory.rs` or separate file
- [x] Test `put` / `get` round-trip
- [x] Test `get` returns empty bitmap for missing key
- [x] Test `insert_id` adds a doc_id to existing bitmap
- [x] Test `insert_id` creates bitmap if key doesn't exist
- [x] Test `remove_id` removes a doc_id
- [x] Test `delete` removes the key entirely
- [x] Test `exists` returns true/false correctly
- [x] Test `bulk_put` writes multiple entries
- [x] Test `tombstone` / `get_tombstone` round-trip
- [x] Test `list_keys` with no filter returns all keys (excluding `__tombstone__`)
- [x] Test `list_keys` with prefix filter (e.g. `"tag:"`)
- [x] Test `cardinality` returns correct count
- **Validate**: `cargo test -p locus-bitmap` ✅ (13 tests)

### Step 3: Add heed dependency ✅
- [x] Add `heed` crate to workspace `Cargo.toml` and `locus-bitmap/Cargo.toml`
- [x] Add `tracing` dependency for structured logging
- **Validate**: `cargo check -p locus-bitmap` ✅

### Step 4: LMDB/heed BitmapStore implementation ✅
- [x] Create `crates/locus-bitmap/src/lmdb.rs`
- [x] Define `LmdbBitmapStore` struct holding `heed::Env` + database handle
- [x] Implement `fn new(path)` — open/create LMDB environment at path
- [x] Bitmaps serialized using `RoaringBitmap::serialize_into` (portable format)
- [x] Bitmaps deserialized using `RoaringBitmap::deserialize_from`
- [x] Implement all `BitmapStore` trait methods
- **Validate**: `cargo check -p locus-bitmap` ✅

### Step 5: LMDB integration tests ✅
- [x] Test all trait methods against LMDB impl (use `tempdir`)
- [x] Verify data persists across close/reopen
- [x] Test with large bitmap (10k+ doc_ids) for serialization correctness
- **Validate**: `cargo test -p locus-bitmap` ✅ (10 tests)

### Step 6: Review against contracts ✅
- [x] Compare `BitmapStore` impl against trait in `locus-core/src/bitmap.rs` — all methods covered
- [x] Cross-check with `003-contracts.md` §4 — signatures, error types, semantics match
- [x] Cross-check with `001-system-overview.md` — bitmap diagrams still accurate
- [x] Confirm no content is stored/returned (pointers only, per architecture)

### Step 7: Commit ✅
- [x] `feat(bitmap): implement in-memory BitmapStore` — 13 tests
- [x] `feat(bitmap): implement LMDB BitmapStore with heed` — 10 tests
