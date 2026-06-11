# Locus Feature Set

> Status: **Living document** — defines what Locus does today and where it's heading.
> Reference: `001-system-overview.md` for architecture, `002-roadmap.md` for phased delivery

## Vision

Locus is a **universal pointer system for LLMs**. It answers "where is the thing I need?" in constant time, across any source — code, docs, wikis, infra configs, tickets — without ever returning content. The LLM gets precise structural pointers (file, byte range, chunk kind, metadata) and decides what to read.

Think of Locus as the **index at the back of the book** — it doesn't contain the chapters, it tells you exactly which page to turn to. Except the book is your entire digital workspace, and the reader is an LLM that needs to minimise context pollution.

---

## What Locus Is (and Isn't)

| Locus is | Locus is not |
|---------|-------------|
| A pre-filter / pointer layer | A knowledge graph (no traversal, no relationship edges) |
| Constant-time set operations (AND/OR/NOT) | A full-text search engine (no term indexing) |
| Structural + semantic metadata index (tags, types, inferred concepts) | A vector database (no embeddings — yet, Phase 2) |
| Source-agnostic (any parseable content) | A content store (never returns file content) |
| Local-first, single-user | A collaboration platform |
| Persistent, incremental, cached | A batch-only tool |

### How Locus relates to knowledge graph tools

Knowledge graph tools build a **graph** — nodes are concepts, edges are relationships, communities are detected via clustering. They're optimised for *understanding structure* ("what connects auth to the database?", "explain RateLimiter").

Locus builds a **bitmap index** — keys are structural attributes, values are compressed bitsets of document IDs. It's optimised for *filtering at speed* ("all Rust files tagged `async` in `src/network/` that are functions" → 19µs, returns pointers).

They're complementary layers:

```
┌─────────────────────────────────────────────┐
│  LLM / AI Assistant                         │
├─────────────────────────────────────────────┤
│  Graph:    "explain how auth works"         │  ← understanding
│  Locus:    "find all auth-related files"    │  ← filtering
│  Vector:   "find similar to this function"  │  ← similarity
├─────────────────────────────────────────────┤
│  Source: code, docs, wiki, infra, tickets   │
└─────────────────────────────────────────────┘
```

---

## Source Types

### Phase 1 ✅ — Obsidian / Markdown Wikis

Covers Obsidian vaults, Karpathy-style wikis, any folder of `.md` files.

| Feature | Status |
|---------|--------|
| YAML frontmatter extraction (tags, aliases, custom fields) | ✅ |
| `[[wikilink]]` and `[markdown](link)` extraction | ✅ |
| Header hierarchy → chunk boundaries (byte ranges) | ✅ |
| Auto-type detection (note, task list, MOC, reference) | ✅ |
| Hierarchical tag flattening (`#a/b/c` → 3 bitmaps) | ✅ |
| Folder-based bitmaps | ✅ |
| Incremental re-index (blake3 hash, skip unchanged) | ✅ |
| File watcher (live updates via `notify`) | ✅ |
| Tombstoning + compaction for deletes | ✅ |

### Phase 3 — Codebases

Index any repository. AST-aware chunking via Tree-sitter.

| Feature | Status |
|---------|--------|
| Tree-sitter parsing (Rust, TypeScript, Python) | ✅ |
| Tree-sitter parsing (Go, Java, …) | Planned |
| AST-aware chunks: function, method, class, module, impl block | ✅ |
| `lang:*`, `kind:*`, `visibility:*`, `async:true` bitmap keys | ✅ |
| Import/dependency extraction → `import:*` link bitmaps | ✅ |
| Multi-repo support: global ID namespace, repo-scoped bitmaps | Planned |
| Cross-repo queries (`repo:backend AND tag:auth AND kind:function`) | Planned |
| `.locusignore` (gitignore syntax) | ✅ |

**Why Tree-sitter?** Local AST extraction, no LLM calls, 29+ language support. Locus doesn't build a call graph; it builds **attribute bitmaps** from the AST. The query "all exported async functions in `src/` that import `tokio`" becomes a bitmap AND — not a graph traversal.

### Phase 4 — Extended Sources

| Source | Parser | Bits indexed | Status |
|--------|--------|-------------|--------|
| **Confluence** | REST API → HTML → chunks | space, label, author, page type | ✅ |
| **Jira** | REST API → structured fields | project, status, assignee, label, sprint | ✅ |
| **Slack** | Bot API polling | channel, author, thread, reaction | ✅ |
| **Webhook ingest** | Push endpoint (POST /v1/webhook/ingest) | any source that can POST JSON | ✅ |
| **Filesystem (generic)** | Extension-based routing | folder, extension, size class, modified date | Planned |
| **Git history** | `git log` parsing | author, date range, changed-file bitmaps | Planned |
| **Terraform / Pulumi** | HCL / YAML parser | resource type, provider, module, environment | Future |
| **Kubernetes** | YAML manifests | namespace, kind, label selectors | Future |
| **OpenAPI / Proto** | Schema parser | endpoint, method, entity, version | Future |

### Future — Infrastructure Traversal

The long-term vision: an LLM can ask "which code deploys to the `payments` namespace?" and Locus resolves it:

```
repo:backend AND folder:src/payments    → code pointers
  ↕ (linked via deploy manifest)
infra:terraform AND resource:k8s_namespace AND tag:payments  → infra pointers
  ↕ (linked via service name)
source:datadog AND service:payments     → observability pointers
```

This isn't graph traversal — it's **bitmap intersection across source boundaries**. Each source has its own bitmaps; cross-source links are just shared bitmap keys (`service:payments` appears in code, infra, and monitoring sources).

---

## Core Query Features

### Available Now ✅

| Feature | Example |
|---------|---------|
| Single-key filter | `tag:work` |
| AND composition | `tag:work AND type:task` |
| OR composition | `tag:meeting OR tag:standup` |
| NOT exclusion | `tag:work AND NOT tag:archived` |
| Nested combinators | `(tag:work OR tag:personal) AND type:note` |
| Tombstone masking | Automatic — deleted docs excluded |
| Cardinality-sorted intersection | Smallest bitmap first for early termination |
| Limit / pagination | `limit=20, offset=40` |
| Bitmap catalog (discovery) | "What tags exist? What's the cardinality of each?" |

### Planned

| Feature | Description |
|---------|-------------|
| **Prefix / wildcard keys** | `tag:project/*` matches `project/backend`, `project/frontend` |
| **Date-range filters** | `modified:>2026-01-01` via date-bucketed bitmaps |
| **Cardinality filters** | `cardinality:>100` — only high-frequency tags |
| **Cross-source queries** | `source:obsidian AND source:confluence AND tag:auth` |
| **Saved filters** | Named filter presets for LLM tool discovery |
| **Aggregation** | `COUNT BY tag` — tag distribution without content |

---

## Jaccard Similarity

Locus supports **Jaccard similarity** (`|A ∩ B| / |A ∪ B|`) as a native bitmap operation. Since Roaring Bitmaps already provide `intersection_len()` and `union_len()` in constant time, Jaccard costs ~nanoseconds on top of existing queries.

### Two modes

| Mode | Method | Input | Use case |
|------|--------|-------|----------|
| **Key-vs-key** | `jaccard_keys(key_a, key_b)` | Two bitmap key names | Compare overlap between any two indexed attributes |
| **Bitmap-vs-bitmap** | `jaccard_bitmaps(a, b)` | Two pre-loaded `RoaringBitmap`s | Compare doc keysets or query results already in memory |

### Use Cases

#### 1. Tag deduplication & alias detection

```
jaccard_keys("tag:auth", "tag:authentication") → 0.92
jaccard_keys("tag:k8s", "tag:kubernetes")      → 0.88
```

High Jaccard (> 0.8) between two tag bitmaps means they index nearly the same documents — likely synonyms. Surface this in `locus bitmaps` or `locus status` so users can merge/alias tags.

#### 2. Document similarity (without embeddings)

Represent each document as the **set of bitmap keys it belongs to**. Two docs that share many tags, folders, types, and concepts will have high Jaccard — a cheap structural similarity signal.

```
doc_A_keys = { "tag:auth", "kind:function", "folder:src/auth", "lang:rust" }
doc_B_keys = { "tag:auth", "kind:function", "folder:src/auth", "lang:rust", "tag:jwt" }
J(doc_A, doc_B) = 4/5 = 0.80
```

This is not semantic similarity (that's vectors), but it's useful for "find structurally similar files" queries — e.g. finding test files that should exist but don't.

#### 3. Query refinement feedback

After a query returns a result set, compute Jaccard between the result bitmap and each candidate filter:

- **J ≈ 1.0**: adding this filter won't change results (redundant)
- **J ≈ 0.0**: this filter would eliminate almost everything (too aggressive)
- **J ∈ [0.3, 0.7]**: this filter meaningfully narrows results (useful suggestion)

An LLM tool can use this to auto-suggest refinements: "Adding `tag:payments` (J=0.35) would narrow from 200 → 70 results."

#### 4. Tagger quality signals

When a tagger produces a new key, check Jaccard against all existing keys:

- **J > 0.9** with an existing key → redundant tagger output, warn or suppress
- **J < 0.1** with all keys → highly specific, good discriminator
- Useful in `locus status` to surface tagger health and bitmap hygiene

#### 5. Bitmap catalog clustering

Group similar keys together in `locus bitmaps` output so LLMs can discover filters without scanning hundreds of keys. Keys with high mutual Jaccard form natural clusters.

---

## Interface Features

### Available Now ✅

| Interface | Features |
|-----------|----------|
| **CLI (`locus`)** | `init`, `config`, `status`, `search`, `inspect`, `bitmaps`, `compact` |
| **MCP Server** | `locus_search`, `locus_inspect`, `locus_status`, `locus_filters` tools |
| **HTTP API** | `POST /search`, `GET /status`, `GET /inspect/:path`, `GET /bitmaps` |
| **Daemon (`locusd`)** | File watcher + MCP + HTTP in one process |

### Planned

| Feature | Description |
|---------|-------------|
| **`locus init <path> --type code`** | Register a code repository, run Tree-sitter indexing |
| **`locus add <url>`** | Fetch and index a remote source (Confluence page, Git repo) |
| **`locus sources`** | List registered sources with index health per source |
| **`locus diff <file>`** | Show what changed since last index (tags added/removed) |
| **MCP resource: vault metadata** | Expose bitmap catalog as MCP resource for discovery |
| **MCP hooks** | Auto-rebuild on file save via MCP PreToolUse hooks |
| **Global graph** | `~/.locus/global.json` — cross-project bitmap namespace |

---

## What Locus Returns

Locus never returns content. Every query response is a list of **MatchPointers**:

```rust
MatchPointer {
    doc_id: u32,
    file_path: String,
    source: String,           // "obsidian", "repo:backend", "confluence"
    doc_type: String,          // "note", "task", "function", "class", "page"
    tags: Vec<String>,
    chunks: Vec<ChunkPointer>,
}

ChunkPointer {
    chunk_id: u32,
    kind: ChunkKind,           // Heading, Function, Class, Paragraph, ...
    byte_start: u64,
    byte_end: u64,
    metadata: ChunkMetadata,   // heading depth, function name, visibility, ...
}
```

The LLM uses these pointers to decide which files (and which *parts* of files) to read. At 100K files, this takes **19µs** for a compound AND query — vs 4.7ms for SQL or 4ms for graph traversal.

---

## Performance Characteristics

| Metric | Target | Measured |
|--------|--------|----------|
| Single-key query (100K docs) | < 50µs | **16µs** |
| AND query, 2 filters (100K docs) | < 50µs | **19µs** |
| AND query, N filters | O(1) per bitmap AND | Constant ~16–20µs |
| Bulk index throughput | > 10K files/s | **20K files/s** |
| Incremental update (single file) | < 50ms | **~30µs** |
| Memory footprint (10K docs) | < 100MB | Well under |
| Storage overhead | < 10% of source size | ✅ |

See [`docs/benchmarks/REPORT.md`](../benchmarks/REPORT.md) for full comparison against grep, parse+filter, HashMap, HashSet, Graph (petgraph), and SQL (DuckDB).

---

## Comparison with Knowledge Graph Tools

| Dimension | Knowledge Graph | Locus |
|-----------|----------------|------|
| **Core data structure** | Node/edge graph | Roaring Bitmap inverted index |
| **Query model** | Graph traversal, path finding, explain | Bitmap AND/OR/NOT, cardinality sort |
| **Query speed (AND, 100K)** | ~4ms (neighbor-set intersection) | **19µs** (compressed bitwise AND) |
| **What it returns** | Concepts, relationships, communities | File pointers with byte ranges |
| **Relationship inference** | Yes (LLM-driven or rule-based) | No — attributes only, not relationships |
| **Community detection** | Yes (clustering algorithms) | No — flat bitmap catalog |
| **Traversal** | Yes (path, explain, hub nodes) | No — set filtering only |
| **Persistence** | Varies (often rebuild on change) | LMDB (incremental, survives restart) |
| **Incremental updates** | Varies | blake3 hash diff, sub-millisecond |
| **LLM dependency** | Often required for semantic extraction | None (pure structural parsing) |

### When to use each

- **"What connects auth to the database?"** → Knowledge graph (relationship traversal)
- **"Find all auth functions in src/network/ that are exported"** → Locus (bitmap filter)
- **"Explain how RateLimiter works"** → Knowledge graph (subgraph + community context)
- **"Which files changed tag:critical this week?"** → Locus (bitmap diff + date filter)
- **"Show me the architecture of this repo"** → Knowledge graph (hub nodes, call flow)
- **"Give me the 12 files an LLM needs to answer this question"** → Locus (multi-filter, pointers only)

### Could they work together?

Yes. A knowledge graph builds understanding; Locus pre-filters the candidate set:

1. LLM asks: "How does authentication work in the payments service?"
2. **Locus** (19µs): `tag:auth AND source:repo:payments AND kind:function` → 8 file pointers
3. **Knowledge graph**: build subgraph from those 8 files → relationships, call flow
4. LLM reads only the relevant chunks from those 8 files

Without Locus, the graph tool queries the full graph (285+ nodes). With Locus as pre-filter, it queries a subgraph of 8 nodes. That's the composability story.

---

## Enrichment: Semantic & Custom Taggers

Locus's structural parsing (frontmatter, AST, schema) is fast and deterministic but limited — it can't infer that a file is "about authentication" unless someone tagged it `#auth`. Semantic taggers close this gap by running an enrichment pass after parsing, producing **inferred bitmap keys** that get indexed alongside structural ones.

### Architecture

```
file bytes ──→ Parser (structural)
                  │
                  ▼
             ParseResult { tags, links, type, chunks }
                  │
                  ▼
             TagPipeline (pluggable, cached by blake3 hash)
                  ├── BuiltinTaggers (no LLM, deterministic)
                  │     ├── TopicTagger      — TF-IDF / keyword extraction → topic:auth, topic:payments
                  │     ├── ComplexityTagger  — cyclomatic complexity, nesting depth → complexity:high
                  │     ├── PatternTagger     — design patterns from AST shape → pattern:singleton, pattern:builder
                  │     └── ConventionTagger  — naming conventions → convention:test, convention:migration
                  │
                  ├── LlmTagger (optional, requires API key)
                  │     ├── ConceptTagger     — "what is this file about?" → concept:rate-limiting, concept:retry-logic
                  │     ├── IntentTagger      — "what does this do?" → intent:validation, intent:serialization
                  │     └── QualityTagger     — "any issues?" → quality:todo, quality:dead-code, quality:missing-tests
                  │
                  └── CustomTaggers (user-defined, from .locus/taggers/)
                        ├── YAML rule files   — pattern matching on paths, content, existing tags
                        └── Script taggers    — executable that reads ParseResult JSON, emits tags
                  │
                  ▼
             EnrichedResult { ...ParseResult, inferred_tags: Vec<String> }
                  │
                  ▼
             Bitmap index (all tags — structural + inferred — are equal bitmap keys)
```

### Cache Design

The tagger cache ensures expensive enrichment runs **once per file version**:

```
.locus/cache/taggers/
  ├── <blake3_hash_1>.json    ← { "builtin": ["topic:auth", "complexity:low"], "llm": ["concept:jwt-validation"], "custom": ["team:platform"] }
  ├── <blake3_hash_2>.json
  └── ...
```

| Scenario | What happens |
|----------|-------------|
| First index | Parse → run all taggers → cache results → index |
| Re-index, file unchanged | blake3 match → load cached tags → skip taggers → index |
| Re-index, file changed | New blake3 → run taggers on new content → cache → index |
| Tagger config changed | Invalidate affected tagger layer only → re-run that layer |
| LLM tagger disabled | Builtin + custom still run; LLM tags loaded from cache if available |

This means:
- **Bulk index with LLM tagger** on 10K files: slow first time (~minutes, depends on model), but subsequent re-indexes are the same 20K files/s as today — only changed files hit the LLM.
- **Builtin taggers** add negligible overhead (keyword extraction on already-parsed content).
- **Cache survives restarts** — it's on disk alongside the bitmap store.

### Custom Tagger Rules (`.locus/taggers/`)

Users define domain-specific tagging without writing code:

```yaml
# .locus/taggers/team-ownership.yaml
name: team-ownership
rules:
  - match:
      folder: "src/payments/**"
    add_tags: ["team:payments", "domain:billing"]
  - match:
      folder: "src/auth/**"
      has_tag: "kind:function"
    add_tags: ["team:platform", "domain:identity"]
  - match:
      extension: ".tf"
      content_contains: "aws_lambda"
    add_tags: ["infra:serverless", "cost:variable"]
```

```yaml
# .locus/taggers/priority.yaml
name: priority-signals
rules:
  - match:
      has_tag: "quality:todo"
      has_tag: "complexity:high"
    add_tags: ["priority:tech-debt"]
  - match:
      has_tag: "concept:auth"
      modified_within: "7d"
    add_tags: ["attention:recent-auth-change"]
```

This turns Locus into a **programmable metadata layer**. The LLM can discover these tags via `locus bitmaps` and use them in queries:

```
locus search --filter "team:payments AND priority:tech-debt AND kind:function"
→ 3 file pointers in 19µs
```

### What This Fills

| Gap (from "Locus is not") | How taggers fill it |
|--------------------------|-------------------|
| No relationship inference | LLM tagger infers `concept:*` and `intent:*` → queryable as bitmap keys |
| No community detection | Custom taggers define `team:*`, `domain:*` → same effect, explicit |
| No full-text understanding | Topic tagger extracts `topic:*` from content keywords |
| No quality signals | Quality tagger flags `quality:dead-code`, `quality:missing-tests` |
| No traversal ("what connects X to Y?") | Still no — but "find everything tagged `concept:auth`" gets you 90% there |

The key insight: **you don't need graph traversal if you have rich enough bitmap keys**. A query like `concept:auth AND domain:payments AND kind:function AND NOT quality:dead-code` gives the LLM exactly the files it needs — without building or traversing a graph.

### Global vs Local Taggers

| Scope | Location | Use case |
|-------|----------|----------|
| **Project-local** | `.locus/taggers/` | Team ownership, domain mapping, project-specific conventions |
| **Global** | `~/.locus/taggers/` | Cross-project standards (language patterns, quality signals) |
| **Built-in** | Compiled into Locus | Topic, complexity, pattern detection — always available |

Global taggers apply to all registered sources. A company could distribute a shared `~/.locus/taggers/company-standards.yaml` that tags everything with `org:*`, `compliance:*`, `data-classification:*` keys.

---

## Design Principles

1. **Pointers, not content** — Locus never stores or returns file content. It's an index, not a cache.
2. **Constant-time queries** — Bitmap operations don't degrade with corpus size. 100K files = same speed as 1K.
3. **Source-agnostic** — Any parseable content gets the same bitmap treatment. A Terraform module and an Obsidian note are both documents with tags, types, and chunks.
4. **No LLM dependency** — Indexing is pure structural parsing (frontmatter, AST, schema). No API calls, no embeddings, no inference. Deterministic and reproducible.
5. **Incremental by default** — blake3 hash-based change detection. Only touched files re-index.
6. **Composable** — Locus is a layer, not a platform. It feeds into knowledge graphs, vector DBs, LLM tool calls, or anything that needs "which files match these criteria?"
