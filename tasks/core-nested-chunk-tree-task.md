# Task: Nested Chunk Tree Output ✅

## Goal
Emit chunk pointers as a nested tree (table-of-contents shape) derived at
hydration time from `depth + byte order`, instead of the existing flat list.
Output-shape change only — no parser, registry schema, or storage changes.

## Steps

### Step 1: Add `depth` + `children` to `ChunkPointer` (locus-core)
- [x] Add `depth: u8` field (populated from `ChunkRecord.depth`)
- [x] Add `children: Vec<ChunkPointer>` with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`

### Step 2: `nest_chunks` pure function (locus-core)
- [x] Implement `pub fn nest_chunks(flat: Vec<ChunkPointer>) -> Vec<ChunkPointer>`
- [x] Sort input by `byte_start` (defensive)
- [x] Stack-based nesting: each chunk becomes last child of nearest ancestor with strictly smaller depth
- [x] Depth-0 chunks (Frontmatter, Body) are roots and never serve as parents
- [x] Unit tests: empty, only-body, simple hierarchy, skipped levels, depth drops >1, same-depth roots, equal-depth siblings, code Class+Methods, unsorted input, frontmatter+sections

### Step 3: Wire `nest_chunks` into `BitmapQueryEngine` (locus-query)
- [x] `hydrate()`: populate `depth` from `ChunkRecord`, call `nest_chunks` before returning
- [x] `inspect()`: same treatment

### Step 4: CLI tree rendering (locus-cli)
- [x] Add `print_chunk_tree(chunks, base_indent)` helper (recursive, 2-space indent per level)
- [x] Replace flat chunk loop in `cmd_search`
- [x] Replace flat chunk loop in `cmd_inspect`

### Step 5: Fix / verify existing tests
- [x] Existing engine tests: no chunk structure assertions — no changes needed
- [x] Add integration test `hydrate_produces_nested_chunk_tree` via `BitmapQueryEngine` + `InMemoryRegistry` + `InMemoryBitmapStore`
- [x] Add integration test `inspect_produces_nested_chunk_tree`

### Step 6: Documentation
- [x] Update `003-contracts.md`: reflect `depth` + `children` on `ChunkPointer`
- [x] Update `001-system-overview.md`: add sentence on tree-shaped pointer output
