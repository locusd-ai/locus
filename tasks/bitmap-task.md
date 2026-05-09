# Task: Implement biem-bitmap (In-memory + LMDB/heed)

## Goal
Implement the `BitmapStore` trait from `biem-core` with an in-memory backend (for tests) and an LMDB/heed backend (for production).

## Steps

### Step 1: In-memory BitmapStore implementation
- [ ] Create `crates/biem-bitmap/src/memory.rs`
- [ ] Implement `BitmapStore` trait using `HashMap<String, RoaringBitmap>`
- [ ] Tombstone stored under a reserved key (e.g. `__tombstone__`)
- [ ] Wire up module in `lib.rs`
- **Validate**: `cargo check -p biem-bitmap`

### Step 2: In-memory BitmapStore tests
- [ ] Create test module in `memory.rs` or separate file
- [ ] Test `put` / `get` round-trip
- [ ] Test `get` returns empty bitmap for missing key
- [ ] Test `insert_id` adds a doc_id to existing bitmap
- [ ] Test `insert_id` creates bitmap if key doesn't exist
- [ ] Test `remove_id` removes a doc_id
- [ ] Test `delete` removes the key entirely
- [ ] Test `exists` returns true/false correctly
- [ ] Test `bulk_put` writes multiple entries
- [ ] Test `tombstone` / `get_tombstone` round-trip
- [ ] Test `list_keys` with no filter returns all keys (excluding `__tombstone__`)
- [ ] Test `list_keys` with prefix filter (e.g. `"tag:"`)
- [ ] Test `cardinality` returns correct count
- **Validate**: `cargo test -p biem-bitmap`

### Step 3: Add heed dependency
- [ ] Add `heed` crate to workspace `Cargo.toml` and `biem-bitmap/Cargo.toml`
- [ ] Add `tracing` dependency for structured logging
- **Validate**: `cargo check -p biem-bitmap`

### Step 4: LMDB/heed BitmapStore implementation
- [ ] Create `crates/biem-bitmap/src/lmdb.rs`
- [ ] Define `LmdbBitmapStore` struct holding `heed::Env` + database handle
- [ ] Implement `fn new(path)` — open/create LMDB environment at path
- [ ] Bitmaps serialized using `RoaringBitmap::serialize_into` (portable format)
- [ ] Bitmaps deserialized using `RoaringBitmap::deserialize_from`
- [ ] Implement all `BitmapStore` trait methods:
  - `get` — read txn, deserialize; return empty bitmap if key missing
  - `put` — write txn, serialize, store
  - `insert_id` — read-modify-write in a write txn
  - `remove_id` — read-modify-write in a write txn
  - `delete` — write txn, delete key
  - `exists` — read txn, check key presence
  - `bulk_put` — single write txn for all entries
  - `tombstone` — read-modify-write on `__tombstone__` key
  - `get_tombstone` — read `__tombstone__` key
  - `list_keys` — iterate all keys with optional prefix filter
  - `cardinality` — deserialize + `.len()` (LMDB doesn't support without deser)
- **Validate**: `cargo check -p biem-bitmap`

### Step 5: LMDB integration tests
- [ ] Test all trait methods against LMDB impl (use `tempdir`)
- [ ] Verify data persists across close/reopen
- [ ] Verify concurrent read transactions work
- [ ] Test with large bitmap (10k+ doc_ids) for serialization correctness
- **Validate**: `cargo test -p biem-bitmap`

### Step 6: Review against contracts
- [ ] Compare `BitmapStore` impl against trait in `biem-core/src/bitmap.rs` — all methods covered
- [ ] Cross-check with `003-contracts.md` §4 (BitmapStore Contract) — signatures, error types, semantics match
- [ ] Cross-check with `001-system-overview.md` — bitmap diagrams and descriptions still accurate
- [ ] Confirm no content is stored/returned (pointers only, per architecture)
- **Validate**: manual review, no code changes expected

### Step 7: Commit
- [ ] `feat(bitmap): implement in-memory BitmapStore` (after step 2)
- [ ] `feat(bitmap): implement LMDB BitmapStore with heed` (after step 5)
