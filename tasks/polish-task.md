# Task: ✅ Polish — tag flattening, cardinality sort, JSON output, compact

## Goal
Four small improvements to round out Phase 1:
1. **Hierarchical tag flattening** — `#a/b/c` inserts into `tag:a`, `tag:a/b`, `tag:a/b/c`
2. **Cardinality-sorted intersection** — sort AND filters smallest-first for faster intersection
3. **JSON output** — `--json` flag on CLI commands for machine-readable output
4. **`locus compact`** — remove tombstoned doc IDs from all bitmaps and registry

## Steps

### Step 1: Hierarchical tag flattening (locus-parser)
- [x] In `MarkdownParser`, when extracting a tag like `#work/project/alpha`, emit all prefixes: `tag:work`, `tag:work/project`, `tag:work/project/alpha`
- [x] Update inline tag extraction and frontmatter tag extraction
- [x] Add test fixture `tests/fixtures/nested-tags.md` with hierarchical tags
- [x] Add unit tests: nested tag produces all prefixes, already-flat tags unchanged
- **Validate**: `cargo test -p locus-parser`
- **Commit**: `feat(parser): flatten hierarchical tags into prefix bitmaps`

### Step 2: Cardinality-sorted intersection (locus-query)
- [x] In `BitmapQueryEngine::resolve_filter`, for `Filter::And`, look up cardinality of each child before fetching
- [x] Sort children ascending by cardinality
- [x] Intersect sequentially (smallest first), short-circuit if result is empty
- [x] Add test: AND with different-cardinality filters produces same result but exercises the sort path
- **Validate**: `cargo test -p locus-query`
- **Commit**: `feat(query): cardinality-sorted AND intersection`

### Step 3: JSON output flag (locus-cli)
- [x] Add `--json` global flag to CLI
- [x] When set, search/inspect/status/filters output `serde_json::to_string_pretty` instead of human-readable
- [x] search → QueryResult JSON, inspect → InspectResult JSON, status → IndexStatus JSON, filters → Vec<FilterEntry> JSON
- [x] Reuse the Serialize impls already on core types
- **Validate**: `locus search tag:work --json | jq .`
- **Commit**: `feat(cli): add --json flag for machine-readable output`

### Step 4: `locus compact` (locus-ingest)
- [x] Add `compact` method to `IngestionPipeline`
- [x] Read tombstone bitmap, get all tombstoned doc IDs
- [x] For each bitmap key: remove tombstoned IDs, update cardinality
- [x] Delete tombstoned docs from registry (and their chunks)
- [x] Clear the tombstone bitmap
- [x] Report: docs removed, bitmaps cleaned, time elapsed
- [x] Add `Compact` CLI command
- [x] Add unit test: tombstone → compact → tombstone bitmap empty, docs gone
- **Validate**: `cargo test && locus compact`
- **Commit**: `feat(ingest): implement compaction of tombstoned documents`
- **Commit**: `feat(cli): add compact command`
