# Task: Implement biem-ingest (Ingestion Pipeline)

## Goal
Implement the `IngestionPipeline` as a concrete coordinator that connects parsers, registry, and bitmap store. Handles both incremental (single event) and bulk (full directory) indexing.

## Steps

### Step 1: Core types and error enum
- [x] Add `ChangeEvent`, `ChangeKind`, `IngestResult`, `IngestAction`, `BulkIndexResult` to biem-ingest (or re-export from biem-core)
- [x] Add `IngestError` with `#[from]` conversions for ParseError, RegistryError, BitmapError, io::Error
- [x] Wire up module in `lib.rs`
- **Validate**: `cargo check -p biem-ingest`

### Step 2: IngestionPipeline struct and constructor
- [x] Define `IngestionPipeline` holding `Vec<Box<dyn Parser>>`, `Box<dyn Registry>`, `Box<dyn BitmapStore>`
- [x] Implement `fn new(parsers, registry, bitmap_store) -> Self`
- [x] Implement `fn find_parser(&self, path) -> Option<&dyn Parser>`
- **Validate**: `cargo check -p biem-ingest`

### Step 3: Bitmap key generation helpers
- [x] `fn tag_key(tag: &str) -> BitmapKey` → `"tag:<tag>"`
- [x] `fn folder_key(path: &Path) -> BitmapKey` → `"folder:<parent_dir>"`
- [x] `fn link_key(target: &str) -> BitmapKey` → `"link:<target>"`
- [x] `fn type_key(note_type: &NoteType) -> BitmapKey` → `"type:<type>"`
- [x] `fn source_key(source_type: &SourceType) -> BitmapKey` → `"source:<source>"`
- **Validate**: `cargo check -p biem-ingest`

### Step 4: process_event — Created (new file)
- [x] Read file content, compute blake3 hash
- [x] Find parser via `can_parse`, parse content
- [x] Insert doc into registry → get DocId
- [x] Replace chunks in registry
- [x] Insert doc_id into bitmap for each tag, folder, link, auto_type, source
- [x] Return `IngestResult { action: Indexed, bitmaps_updated }`
- **Validate**: `cargo check -p biem-ingest`

### Step 5: process_event — Modified (existing file changed)
- [x] Lookup existing doc by path, compute new hash
- [x] If hash unchanged → return `Skipped`
- [x] Parse new content
- [x] Diff old vs new: compute added/removed tags, links, type
- [x] Update doc hash in registry, replace chunks
- [x] Add doc_id to new bitmap keys, remove from old bitmap keys
- [x] Return `IngestResult { action: Updated, bitmaps_updated }`
- **Validate**: `cargo check -p biem-ingest`

### Step 6: process_event — Deleted
- [x] Lookup doc by path
- [x] Add doc_id to tombstone bitmap
- [x] Remove doc_id from all associated bitmaps (tags, links, folder, type, source)
- [x] Return `IngestResult { action: Tombstoned }`
- **Validate**: `cargo check -p biem-ingest`

### Step 7: process_event — Renamed
- [x] Lookup doc by old path (`event.kind.from`)
- [x] Update path in registry
- [x] Update folder bitmap if parent dir changed
- [x] Return `IngestResult { action: Moved }`
- **Validate**: `cargo check -p biem-ingest`

### Step 8: Unit tests (using in-memory backends)
- [x] Test Created: new file → doc in registry, bitmaps populated
- [x] Test Modified (changed): doc updated, bitmap diff correct
- [x] Test Modified (unchanged hash): returns Skipped
- [x] Test Deleted: doc tombstoned, removed from bitmaps
- [x] Test Renamed: path updated, folder bitmap updated
- [x] Test NoParser error for unsupported file type
- **Validate**: `cargo test -p biem-ingest`

### Step 9: bulk_index implementation
- [x] Walk directory tree, collect parseable files
- [x] Hash all files, parse all files
- [x] Bulk insert docs, bulk replace chunks
- [x] Accumulate bitmap entries, bulk_put to bitmap store
- [x] Update bitmap catalog in registry
- [x] Return `BulkIndexResult` with counts and duration
- **Validate**: `cargo check -p biem-ingest`

### Step 10: bulk_index tests
- [x] Create temp dir with fixture files, run bulk_index
- [x] Verify correct doc count, bitmap count
- [x] Verify registry state matches indexed files
- **Validate**: `cargo test -p biem-ingest`

### Step 11: Review against contracts
- [x] Compare implementation against `003-contracts.md` §5
- [x] Verify bitmap key namespace matches contracts (tag:, folder:, link:, type:, source:)
- [x] Verify IngestError has all #[from] conversions per contracts
- [x] Cross-check with `001-system-overview.md` — ingestion diagrams still accurate
- **Validate**: manual review

### Step 12: Commit
- [x] `feat(ingest): implement incremental ingestion pipeline` (after step 8)
- [x] `feat(ingest): implement bulk indexing` (after step 10)
