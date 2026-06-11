# Task: Chunk-range registry foundations (B1) ✅

> Phase 1 of the v0.2 chunk-level bitmap index. See `001-system-overview.md`
> §4 for the pre-order invariant and `003-contracts.md` for the trait surface.

## Goal

Make doc⇄chunk resolution O(1) so the query engine can stop scanning:
chunk ids within a doc are a contiguous ascending run, tracked in a
`doc_chunk_ranges` table.

## Steps

- [x] Document pre-order invariant on the `Registry` trait
- [x] `doc_chunk_ranges` table + index in DuckDB schema, with backfill
      migration for pre-B1 databases
- [x] `replace_chunks` maintains the range row (DuckDB); in-memory impl
      satisfies the invariant via monotonic id assignment
- [x] `delete_doc` clears the range row
- [x] New trait methods: `chunk_range`, `doc_for_chunk`,
      `get_chunks_for_docs` (default impl loops; DuckDB overrides with a
      single `IN (…)` query)
- [x] Tests: in-memory (5) + DuckDB (4) covering range contiguity, reverse
      lookup, re-index invalidation, delete cleanup, batched ordering
- [x] Docs: 001-system-overview.md ER diagram + invariant; 003-contracts.md
      trait surface

## Out of scope (next phases)

- B2 (ingest): chunk-level bitmap writes, doc→keys table, `__all_docs__`
  bitmap, tombstone ranges
- B3 (query): chunk-granularity filter resolution; replace the
  per-candidate `get_chunks` loops in `semantic_query` with
  `get_chunks_for_docs` / `doc_for_chunk`
