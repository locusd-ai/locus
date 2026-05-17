# Graph Layer Task — Phase 4

Full design: `docs/architecture/008-graph-layer.md`
Roadmap entry: `docs/architecture/002-roadmap.md` (Phase 4)

## Context

The graph layer is the third pillar of the retrieval pipeline:

```
bitmap pre-filter (~16µs) → graph expand (~100µs–1ms) → vector rerank (~6ms)
```

Every stage operates on `RoaringBitmap<DocId>` as the carrier — the graph stage is
opt-in and composable. All source types (Obsidian, Code, Confluence, Jira, Slack,
Custom) share the same `DocId` space, so **cross-source edges work natively**.

## Cross-source linkage design

This is worth calling out explicitly before implementation:

- **Uniform DocId space**: all docs, regardless of source type, share one `DocId`
  sequence. A Confluence page and an Obsidian note can be `from=42, to=91` in the
  same edge.

- **Resolution is source-agnostic**: `resolve_pending(doc_id, target_ref)` searches
  the full registry across all sources. It must try multiple resolution strategies in
  order:
  1. Exact path match (e.g. `path.md`)
  2. Title/stem match (Obsidian wikilinks use note title, not path)
  3. Module-path match (code imports use `serde::de::Deserialize`, not file paths)
  4. External ID match (Confluence page IDs, Jira issue keys — when those sources land)
  
  The resolution strategy is source-specific but the resolution *target* registry is
  global. Store the resolution strategy as part of the `UnresolvedEdge` if needed.

- **Traversal ignores source boundaries by default**: `expand(seeds, spec)` follows
  edges regardless of what source `from` and `to` came from. Source-scoped traversal is
  opt-in via an edge filter like `edge_filter: SourceScoped("obsidian")` — which means
  "only traverse edges whose endpoints both have `source:obsidian`".

- **Cross-source traversal example**: an Obsidian note links to a code file
  (`[[auth/parser.rs]]`) and that code file imports a function from another module.
  A 2-hop `ref+dep` expansion from the note reaches both the code file and its
  dependencies. This works out of the box — no special handling needed.

- **Pending edge lifetime**: an `UnresolvedEdge` stays pending until the target doc is
  indexed, which may happen in a later source registration (e.g. Confluence indexed a
  week after Obsidian). `resolve_pending` must be called on every new doc insert from
  any source.

---

## Step 1 — Core graph types (`locus-core`) [ ]

File: `crates/locus-core/src/graph.rs` (new)

- [ ] Add `EdgeCategory` enum (`Reference`, `Dependency`, `Hierarchy`, `Workflow`, `Provenance`)
- [ ] Add `Edge` struct (`from`, `to`, `category`, `kind`, `weight`, `byte_offset`, `created_at`)
- [ ] Add `UnresolvedEdge` struct (`from`, `category`, `kind`, `target_ref`, `byte_offset`)
- [ ] Add `Direction` enum (`Outgoing`, `Incoming`, `Both`)
- [ ] Add `EdgeFilter` enum (`Any`, `Category`, `Kinds`, `And`, `Or`)
- [ ] Add `ExpandSpec` struct (`hops`, `direction`, `edge_filter`, `max_nodes`, `include_seeds`)
- [ ] Add `GraphQueryRequest` + `GraphOp` enum (see §4 of design doc)
- [ ] Add `CentralityAlgorithm` enum
- [ ] Add `GraphQueryResult`, `GraphNodeRef`, `EdgeRef`
- [ ] Add `GraphError` (with `#[from]` for `BitmapError` and `RegistryError`)
- [ ] Add `GraphStats`
- [ ] Add `GraphStore` trait (§4.2 of design doc)
- [ ] Add `GraphQueryEngine` trait (§4.3 of design doc)
- [ ] Re-export from `locus-core/src/lib.rs` under `pub mod graph`
- [ ] Add `graph.rs` to `locus-core/Cargo.toml` deps: `roaring`, `serde`

Tests:
- [ ] Unit tests for `EdgeFilter::matches(edge)` logic
- [ ] Unit tests for `ExpandSpec` default values

---

## Step 2 — DuckDB persistence (`locus-registry`) [ ]

File: `crates/locus-registry/src/graph.rs` (new)

- [ ] DDL: `CREATE TABLE IF NOT EXISTS doc_links (...)` per §5.1 of design doc
- [ ] DDL: `CREATE TABLE IF NOT EXISTS doc_links_pending (...)` per §5.1
- [ ] DDL: three indexes on `doc_links`, one on `doc_links_pending`
- [ ] Call DDL from `DuckDbRegistry::new()` so the tables always exist
- [ ] Implement `DuckDbGraphStore` struct wrapping the `DuckDB::Connection` arc
- [ ] Implement `GraphStore::insert_edge`
- [ ] Implement `GraphStore::bulk_insert_edges` (single transaction)
- [ ] Implement `GraphStore::insert_unresolved`
- [ ] Implement `GraphStore::resolve_pending(doc_id, link_target)` — cross-source:
  - Try exact path match first (cheapest)
  - Try stem/title match second (`SELECT doc_id FROM documents WHERE file_path LIKE ?`)
  - Promote matched pending edges to `doc_links`, delete from `doc_links_pending`
- [ ] Implement `GraphStore::remove_doc_edges`
- [ ] Implement `GraphStore::replace_outgoing` (delete-then-bulk-insert in one transaction)
- [ ] Implement read methods: `neighbours`, `doc_edges`, `edge_count`, `node_count`
- [ ] Stub `rebuild_in_memory` and `drop_in_memory` (no-ops until Step 3)

Tests:
- [ ] `insert_edge` + `neighbours` round-trip
- [ ] `bulk_insert_edges` + `edge_count`
- [ ] `insert_unresolved` + `resolve_pending` — resolve finds by title match
- [ ] Cross-source `resolve_pending`: two docs from different source types, edge resolves correctly
- [ ] `remove_doc_edges` cleans both directions
- [ ] `replace_outgoing` is atomic: re-index a doc, verify old edges gone and new edges present

---

## Step 3 — In-memory petgraph (`locus-registry`) [ ]

Extend `DuckDbGraphStore` with `StableGraph<DocId, Edge, Directed>`.

- [ ] Add `petgraph` dependency to `locus-registry/Cargo.toml` (already workspace dep)
- [ ] Add `graph: Option<StableGraph<DocId, Edge, Directed>>` + `node_index: HashMap<DocId, NodeIndex>` + `Arc<RwLock<...>>` wrapper to `DuckDbGraphStore`
- [ ] Implement `rebuild_in_memory`: scan `doc_links`, populate graph, record `last_rebuilt`
- [ ] Implement `drop_in_memory`
- [ ] All write methods (`insert_edge`, `bulk_insert_edges`, `remove_doc_edges`, `replace_outgoing`) write through to both DuckDB and the in-memory graph
- [ ] `neighbours` uses in-memory graph when available, falls back to DuckDB query

Benchmarks (add to `locus-bench`):
- [ ] Cold-start rebuild at 10K, 100K edges (target: <100ms at 100K)
- [ ] `neighbours` in-memory vs DuckDB fallback latency comparison

---

## Step 4 — Ingestion edge writes (`locus-ingest`) [ ]

Wire graph writes into `IngestionPipeline`. `GraphStore` is optional (backwards-compat).

- [ ] Add `graph_store: Option<Box<dyn GraphStore>>` to `IngestionPipeline`
- [ ] Add `IngestionPipeline::with_graph_store(mut self, gs: Box<dyn GraphStore>) -> Self`
- [ ] In `process_event` / `bulk_index`, after writing bitmap link keys:
  - For each `LinkRef` in `parse_result.links`, derive `Edge { category, kind }` from
    the source type (see edge kind table in §2.2 of design doc)
  - Look up target by path/title: if found → `insert_edge`; if not → `insert_unresolved`
  - After every new doc insert: `graph_store.resolve_pending(new_doc_id, title)`
- [ ] On doc tombstone: `graph_store.remove_doc_edges(doc_id)`
- [ ] On doc re-index: `graph_store.replace_outgoing(doc_id, new_edges)`
- [ ] Add `edge_kind_for_link(source_type, link_ref) -> (EdgeCategory, &'static str)` helper
  - Obsidian wikilinks → `(Reference, "ref:wikilink")`
  - Markdown links → `(Reference, "ref:mdlink")`
  - Code imports → `(Dependency, "dep:import")`
  - Custom source → `(Reference, "ref:link")` as default

Cross-source edge resolution:
- [ ] `resolve_pending` is called after *every* doc insert, not just same-source inserts.
  This ensures an Obsidian note that links to a not-yet-indexed Confluence page
  promotes its pending edge when the Confluence page is later indexed.

Tests:
- [ ] Index small vault → assert `edge_count > 0`
- [ ] Re-index a doc (change its links) → assert edges updated, not duplicated
- [ ] Tombstone a doc → assert its edges removed from both directions
- [ ] Two sources indexed in sequence: source A has pending edge to source B's doc;
  after indexing source B, the edge is resolved.

---

## Step 5 — `GraphQueryEngine` implementation (`locus-query`) [ ]

File: `crates/locus-query/src/graph_engine.rs` (new)

- [ ] `PetgraphQueryEngine` struct wrapping `Arc<dyn GraphStore>`
- [ ] Add `locus-query/Cargo.toml`: `petgraph` dep
- [ ] Implement `GraphQueryEngine::query(GraphQueryRequest)` dispatcher
- [ ] Implement `expand(seeds, spec)`:
  - BFS from seed set, respecting `direction`, `edge_filter`, `hops`
  - `max_nodes` cap: return `GraphError::ExpansionLimit` when reached
  - `include_seeds` flag: whether seeds appear in output bitmap
- [ ] Implement `centrality(algorithm, restrict_to)`:
  - `InDegree` / `OutDegree`: count edges per node
  - `PageRank`: use `petgraph::algo::page_rank` with configurable iterations + damping
  - `restrict_to`: if provided, only score docs in the candidate bitmap
- [ ] Implement `shortest_path(from, to, filter)`:
  - Use `petgraph::algo::dijkstra` on the in-memory graph
  - Apply `edge_filter` as predicate
- [ ] Implement `reachable(from, direction, filter)`:
  - BFS/DFS from seed, collect all reachable nodes into a `RoaringBitmap`
- [ ] Implement `stats()`

Tests (synthetic graphs):
- [ ] Linear chain: `expand` from node 0, hops=2 → nodes 0,1,2
- [ ] Hub graph: `max_nodes` cap fires as expected
- [ ] `shortest_path` finds the right path
- [ ] `centrality` PageRank assigns higher score to in-degree-heavy nodes
- [ ] `reachable` finds transitive closure correctly

---

## Step 6 — Three-stage pipeline composition (`locus-query`) [ ]

- [ ] Add `graph: Option<Arc<dyn GraphQueryEngine>>` to `BitmapQueryEngine`
- [ ] Add `BitmapQueryEngine::with_graph(self, g: Arc<dyn GraphQueryEngine>) -> Self`
- [ ] Add `BitmapQueryEngine::graph_expand(seeds, spec) -> Result<RoaringBitmap>`:
  - Delegates to `graph.expand(seeds, spec)` if graph is set; returns `seeds.clone()` if not
- [ ] Add `graph_expand: Option<ExpandSpec>` to `SemanticQueryRequest`
- [ ] Add `graph_expanded_to: Option<u32>` and `elapsed_graph_us: u64` to `SemanticQueryResult`
- [ ] In `semantic_query`: insert graph expand stage between bitmap filter and vector search
- [ ] Update `003-contracts.md` and `001-system-overview.md` ← done by parallel agent

Tests:
- [ ] Pipeline with graph disabled: identical results to current `semantic_query`
- [ ] Pipeline with graph enabled + hops=1: result set is superset of bitmap-only result
- [ ] `elapsed_graph_us` is populated, `graph_expanded_to` reflects post-expand count

---

## Step 7 — CLI commands (`locus-cli`) [ ]

Subcommand: `locus graph <subcommand>`

- [ ] `locus graph neighbours <path> [--incoming] [--both] [--category <cat>] [--limit N]`
- [ ] `locus graph expand <path> --hops N [--category <cat>] [--max-nodes N]`
- [ ] `locus graph path <from-path> <to-path> [--category <cat>]`
- [ ] `locus graph central [--algorithm pagerank|indegree|outdegree] [--filter <bitmap-filter>] [--limit N]`
- [ ] `locus graph stats`
- [ ] `locus provenance <path> [--mode sources|derivatives|cohort|session]`
- [ ] All commands support `--json` flag
- [ ] `locus status` includes graph stats when graph store is initialised

---

## Step 8 — MCP tools (`locus-daemon`) [ ]

- [ ] `locus_graph` tool — operation-dispatched (§7.1 of design doc)
  - ops: `neighbours`, `expand`, `shortest_path`, `top_central`, `reachable`
  - JSON input schema using `schemars`
- [ ] `locus_provenance` tool (§7.2)
  - modes: `sources`, `derivatives`, `cohort`, `session`
- [ ] `locus_declare_provenance` tool (§8.3)
  - Inserts `prov:generated` edges from asset to source docs
  - Creates synthetic session-doc if `session_id` provided
- [ ] Extend `locus_search` with optional `graph_expand` parameter
- [ ] Extend `locus_status` response with `graph` section (§7.4)
- [ ] Session read-tracking in `locus_search` / `locus_inspect` (opt-in via `session_id`)

Integration tests:
- [ ] MCP client calls `locus_graph` with `op=neighbours`, gets correct result
- [ ] `locus_declare_provenance` inserts edges, `locus_provenance` retrieves them

---

## Step 9 — Provenance and session tracking (`locus-daemon`, `locus-ingest`) [ ]

- [ ] Remove the bitmap-based `provenance:<source_doc_id>` design from `002-roadmap.md`
  (replace with note that provenance uses graph edges)
- [ ] Remove `BitmapCategory::Provenance` / `BitmapCategory::Session` from roadmap plans
  (session *membership* stays as `session:<id>` bitmap; edges handle lineage)
- [ ] HTTP endpoint: `POST /v1/provenance` mirrors `locus_declare_provenance` MCP tool
- [ ] Synthetic session-doc: `DocRecord` with `source_type: Custom("session")`, no file on disk;
  stored in registry with a synthetic path `session://<session_id>`

---

## Step 10 — Benchmarks (`locus-bench`) [ ]

Add to `crates/locus-bench/src/bin/locus_bench_phase2.rs` or a new `locus_bench_graph.rs`:

- [ ] Cold-start rebuild time (10K, 100K, 1M edges)
- [ ] `expand` latency vs hops (1, 2, 3) at 10K docs
- [ ] `centrality` (PageRank) wall time at 1K, 10K, 100K docs
- [ ] Cross-source edge resolution rate (how many pending edges resolve on second-source index)
- [ ] Full three-stage pipeline (bitmap → graph 2-hop → vector rerank) end-to-end

Record results in `docs/benchmarks/REPORT.md` Phase 4 section.

---

## Dependency order

```
Step 1 (core types)
  └── Step 2 (DuckDB persistence)
        └── Step 3 (in-memory petgraph)
              ├── Step 4 (ingestion writes)      ← also needs Step 1
              └── Step 5 (GraphQueryEngine)      ← also needs Step 3
                    └── Step 6 (pipeline composition)
                          ├── Step 7 (CLI)
                          └── Step 8 (MCP)
                                └── Step 9 (provenance / session)
                                      └── Step 10 (benchmarks)
```

Steps 7–9 can proceed in parallel once Step 6 is done.

## Notes

- petgraph is already a workspace dependency (`petgraph = "0.7"`) in `Cargo.toml`.
- `locus-registry` already holds the DuckDB connection — `DuckDbGraphStore` should
  share it rather than opening a second connection.
- All new fields on `SemanticQueryRequest` / `SemanticQueryResult` are additive —
  no breaking changes.
- `GraphStore` lives in `locus-registry`, not `locus-core`, because the concrete impl
  (DuckDB + petgraph) is storage-layer knowledge. The *trait* lives in `locus-core`.
