# Task: Implement biem-ingest (Ingestion Pipeline)

## Goal
Implement the `IngestionPipeline` as a concrete coordinator that connects parsers, registry, and bitmap store. Handles both incremental (single event) and bulk (full directory) indexing.

## Steps

### Step 1: Core types and error enum
- [ ] Add `ChangeEvent`, `ChangeKind`, `IngestResult`, `IngestAction`, `BulkIndexResult` to biem-ingest (or re-export from biem-core)
- [ ] Add `IngestError` with `#[from]` conversions for ParseError, RegistryError, BitmapError, io::Error
- [ ] Wire up module in `lib.rs`
- **Validate**: `cargo check -p biem-ingest`

### Step 2: IngestionPipeline struct and constructor
- [ ] Define `IngestionPipeline` holding `Vec<Box<dyn Parser>>`, `Box<dyn Registry>`, `Box<dyn BitmapStore>`
- [ ] Implement `fn new(parsers, registry, bitmap_store) -> Self`
- [ ] Implement `fn find_parser(&self, path) -> Option<&dyn Parser>`
- **Validate**: `cargo check -p biem-ingest`

### Step 3: Bitmap key generation helpers
- [ ] `fn tag_key(tag: &str) -> BitmapKey` → `"tag:<tag>"`
- [ ] `fn folder_key(path: &Path) -> BitmapKey` → `"folder:<parent_dir>"`
- [ ] `fn link_key(target: &str) -> BitmapKey` → `"link:<target>"`
- [ ] `fn type_key(note_type: &NoteType) -> BitmapKey` → `"type:<type>"`
- [ ] `fn source_key(source_type: &SourceType) -> BitmapKey` → `"source:<source>"`
- **Validate**: `cargo check -p biem-ingest`

### Step 4: process_event — Created (new file)
- [ ] Read file content, compute blake3 hash
- [ ] Find parser via `can_parse`, parse content
- [ ] Insert doc into registry → get DocId
- [ ] Replace chunks in registry
- [ ] Insert doc_id into bitmap for each tag, folder, link, auto_type, source
- [ ] Return `IngestResult { action: Indexed, bitmaps_updated }`
- **Validate**: `cargo check -p biem-ingest`

### Step 5: process_event — Modified (existing file changed)
- [ ] Lookup existing doc by path, compute new hash
- [ ] If hash unchanged → return `Skipped`
- [ ] Parse new content
- [ ] Diff old vs new: compute added/removed tags, links, type
- [ ] Update doc hash in registry, replace chunks
- [ ] Add doc_id to new bitmap keys, remove from old bitmap keys
- [ ] Return `IngestResult { action: Updated, bitmaps_updated }`
- **Validate**: `cargo check -p biem-ingest`

### Step 6: process_event — Deleted
- [ ] Lookup doc by path
- [ ] Add doc_id to tombstone bitmap
- [ ] Remove doc_id from all associated bitmaps (tags, links, folder, type, source)
- [ ] Return `IngestResult { action: Tombstoned }`
- **Validate**: `cargo check -p biem-ingest`

### Step 7: process_event — Renamed
- [ ] Lookup doc by old path (`event.kind.from`)
- [ ] Update path in registry
- [ ] Update folder bitmap if parent dir changed
- [ ] Return `IngestResult { action: Moved }`
- **Validate**: `cargo check -p biem-ingest`

### Step 8: Unit tests (using in-memory backends)
- [ ] Test Created: new file → doc in registry, bitmaps populated
- [ ] Test Modified (changed): doc updated, bitmap diff correct
- [ ] Test Modified (unchanged hash): returns Skipped
- [ ] Test Deleted: doc tombstoned, removed from bitmaps
- [ ] Test Renamed: path updated, folder bitmap updated
- [ ] Test NoParser error for unsupported file type
- **Validate**: `cargo test -p biem-ingest`

### Step 9: bulk_index implementation
- [ ] Walk directory tree, collect parseable files
- [ ] Hash all files, parse all files
- [ ] Bulk insert docs, bulk replace chunks
- [ ] Accumulate bitmap entries, bulk_put to bitmap store
- [ ] Update bitmap catalog in registry
- [ ] Return `BulkIndexResult` with counts and duration
- **Validate**: `cargo check -p biem-ingest`

### Step 10: bulk_index tests
- [ ] Create temp dir with fixture files, run bulk_index
- [ ] Verify correct doc count, bitmap count
- [ ] Verify registry state matches indexed files
- **Validate**: `cargo test -p biem-ingest`

### Step 11: Review against contracts
- [ ] Compare implementation against `003-contracts.md` §5
- [ ] Verify bitmap key namespace matches contracts (tag:, folder:, link:, type:, source:)
- [ ] Verify IngestError has all #[from] conversions per contracts
- [ ] Cross-check with `001-system-overview.md` — ingestion diagrams still accurate
- **Validate**: manual review

### Step 12: Commit
- [ ] `feat(ingest): implement incremental ingestion pipeline` (after step 8)
- [ ] `feat(ingest): implement bulk indexing` (after step 10)
