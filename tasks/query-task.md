# Task: Implement biem-query (Query Engine)

## Goal
Implement the `QueryEngine` trait and `BitmapQueryEngine` struct that resolves boolean filter expressions against the bitmap store, then hydrates results with metadata from the registry. Returns pointers (not content).

## Steps

### Step 1: Core types in biem-core
- [ ] Add `Filter`, `QueryRequest`, `MatchPointer`, `ChunkPointer`, `QueryResult`, `QueryError` to biem-core
- [ ] Add `QueryEngine` trait
- [ ] Wire up in biem-core lib.rs
- **Validate**: `cargo check -p biem-core`

### Step 2: BitmapQueryEngine struct and constructor
- [ ] Define `BitmapQueryEngine` holding `&dyn BitmapStore` + `&dyn Registry` (or Box)
- [ ] Implement `fn new(bitmap_store, registry) -> Self`
- **Validate**: `cargo check -p biem-query`

### Step 3: Filter resolution (recursive bitmap ops)
- [ ] `fn resolve_filter(&self, filter: &Filter) -> Result<RoaringBitmap, QueryError>`
- [ ] Key → get bitmap, subtract tombstones
- [ ] Not → get full universe, andnot resolved inner
- [ ] And → intersect all children
- [ ] Or → union all children
- **Validate**: `cargo check -p biem-query`

### Step 4: Query execution and result hydration
- [ ] Resolve filter → get matching doc_ids
- [ ] Apply limit/offset
- [ ] Lookup docs from registry, get chunks
- [ ] Build MatchPointer + ChunkPointer for each doc
- [ ] Measure query_time_us
- **Validate**: `cargo check -p biem-query`

### Step 5: list_filters implementation
- [ ] Delegate to registry.list_catalog(category)
- **Validate**: `cargo check -p biem-query`

### Step 6: Unit tests (in-memory backends)
- [ ] Single key filter
- [ ] AND of two filters
- [ ] OR of two filters
- [ ] NOT filter
- [ ] Nested compound filter
- [ ] Tombstoned docs excluded
- [ ] Limit and offset
- [ ] Unknown filter key
- [ ] list_filters
- **Validate**: `cargo test -p biem-query`

### Step 7: Review against contracts
- [ ] Verify against 003-contracts.md §7
- [ ] Verify QueryResult serialization matches contract
- **Validate**: manual review

### Step 8: Commit
- [ ] `feat(query): implement bitmap query engine with filter resolution`
