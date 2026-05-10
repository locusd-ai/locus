# Task: Make bulk_index idempotent

## Goal
`bulk_index` should be safe to re-run against an already-indexed vault. Currently it fails with "path already registered" because `insert_doc` rejects duplicate paths. This affects both `biem index` (re-running) and `biemd` (restarting).

## Context
We added a quick fix (option A): the daemon checks `total_documents > 0` and skips bulk index entirely if the vault is already populated. This works for the restart case but doesn't handle:
- Partially indexed vaults (interrupted `biem init`)
- Re-indexing after adding new files (without using the watcher)
- `biem index` run twice on the same vault

Making `bulk_index` itself idempotent is the proper fix.

## Steps

### Step 1: Skip already-registered files in bulk_index
- [ ] In `IngestionPipeline::bulk_index`, for each file, call `registry.lookup_by_path`
- [ ] If path exists and hash matches → skip (already indexed, unchanged)
- [ ] If path exists and hash differs → update (re-parse and update registry + bitmaps)
- [ ] If path doesn't exist → insert (new file, normal path)
- [ ] Track counts: `docs_indexed`, `docs_updated`, `docs_skipped`
- **Validate**: `cargo test -p biem-ingest`

### Step 2: Update BulkIndexResult
- [ ] Add `docs_updated: u32` and `docs_skipped: u32` fields
- [ ] Update CLI and daemon to display new fields
- [ ] Update contracts doc §5 if needed
- **Validate**: `cargo build`

### Step 3: Handle removed files during re-index
- [ ] After walking the directory, compare with all registered paths for this vault
- [ ] Files in registry but not on disk → tombstone them
- [ ] Report: `docs_tombstoned: u32`
- **Validate**: delete a fixture file, re-run `biem index`, verify tombstoned

### Step 4: Tests
- [ ] Test: bulk_index twice on same vault → second run skips all, zero inserts
- [ ] Test: bulk_index after modifying a file → detects update
- [ ] Test: bulk_index after deleting a file → tombstones it
- [ ] Test: bulk_index on partially indexed vault → completes remaining files
- **Validate**: `cargo test -p biem-ingest`

### Step 5: Remove daemon skip-if-populated guard
- [ ] Remove the `total_documents > 0` check in `biemd` — no longer needed since bulk_index is idempotent
- [ ] `initial_index` can default to `true` safely
- **Validate**: `biemd tests/fixtures/ --http` works on both first run and restart

### Step 6: Commits
- [ ] `feat(ingest): make bulk_index idempotent`
- [ ] `refactor(daemon): remove skip-if-populated guard`
