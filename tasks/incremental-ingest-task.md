# Task: Incremental ingest — doc→keys, lazy deletes, universe bitmap (B2) ✅

> Phase 2 of the v0.2 plan. Foundation: chunk-range registry (B1,
> `tasks/chunk-range-registry-task.md`). See `001-system-overview.md` §4.

## Goal

Stop scanning the world on every change: re-index diffs and compaction are
O(doc's keys); unchanged files cost nothing on bulk re-index; NOT queries
read one bitmap instead of unioning all of them.

## Steps

- [x] Registry: `set_doc_keys` / `get_doc_keys` / `delete_doc_keys` +
      `doc_bitmap_keys` table (DuckDB) and map (in-memory); cleared by
      `delete_doc`
- [x] `ALL_DOCS_KEY` (`__all_docs__`) reserved universe bitmap, hidden from
      `list_keys` in both stores, maintained on create/modify/bulk paths
- [x] Event handlers: modified/upsert diff via recorded keys (scan fallback
      for pre-B2 docs); renamed keeps mapping in sync; deleted is tombstone-only
- [x] `bulk_index`: unchanged = true skip (no parse); changed = targeted
      diff; pending inserts merged with stored bitmaps before one `bulk_put`
      (fixes stale ids surviving in keys that vanished from every doc)
- [x] `compact()`: targeted scrub via doc→keys, universe maintenance,
      `delete_doc` clears bookkeeping
- [x] Query: `Filter::Not` uses `__all_docs__` (union-of-all fallback)
- [x] Tests: 6 incremental-ingest integration tests + 2 DuckDB doc-keys
      tests; e2e verified (re-index 121ms → 3ms on 2-doc vault; NOT query)

## Out of scope (next phases)

- B3: chunk-granularity bitmap space + query resolution (the big flip)
- B4: migration story, bench refresh, README
