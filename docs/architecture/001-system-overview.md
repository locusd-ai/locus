# BIEM System Architecture — Overview

> Status: **DRAFT — Iteration 2 (decisions resolved)**
> Scope: Obsidian vault use case first, designed for extensibility to code/confluence/other sources
> See also: `002-roadmap.md` for phased build plan

## Decisions from Iteration 1

| # | Decision |
|---|----------|
| Q1 | **Parser is a separate module** — decoupled from ingestion so different parsers (markdown, code, etc.) can plug into the same ingestion pipeline |
| Q2 | **Chunking is flexible** — the registry stores chunk metadata with a configurable granularity strategy per source type |
| Q3 | **Tombstone bitmap for deletes** — lazy cleanup, most scalable. File moves update the registry path + folder bitmaps, same ID retained |
| Q4 | **BIEM returns metadata pointers, not content** — the LLM decides what to read. Multiple interface modes (MCP, HTTP API, CLI). See §6 for exploration |
| Q5 | **Semantic layer (Qdrant/vector/reranker) is a follow-on phase** — contracts designed to accommodate it |

---

## 1. High-Level System Boundary (Revised)

BIEM is a **local indexing and filtering service**. It does not retrieve content — it tells you *where* the relevant content is and *why* it matched, then the consumer decides what to read.

```mermaid
graph LR
    subgraph Sources["Data Sources"]
        OV[("Obsidian Vault")]
        CS[("Codebase<br/>(future)")]
        OTHER[("Confluence, etc.<br/>(future)")]
    end

    subgraph BIEM["BIEM Service"]
        IDX["Index Engine"]
        QE["Query Engine"]
    end

    subgraph Consumers["Consumers"]
        MCP_C["MCP Client<br/>(Claude, Cursor)"]
        CLI_C["CLI"]
        API_C["HTTP Client<br/>(scripts, plugins)"]
    end

    Sources -- "fs events / webhooks" --> IDX
    IDX -- "reads" --> Sources

    MCP_C -- "MCP" --> QE
    CLI_C -- "CLI" --> QE
    API_C -- "HTTP API" --> QE

    QE -- "pointers + metadata" --> Consumers
```

Key shift from v1: BIEM is a **pointer service**. It returns structured metadata (file path, chunk range, tags, type, match reason) — not the content itself.

---

## 2. Module Decomposition (Revised)

Seven modules, with clear separation between parsing and ingestion:

```mermaid
graph TB
    subgraph BIEM["BIEM Service"]
        direction TB

        WATCH["Watcher<br/>─────────<br/>fs events → change queue"]

        subgraph Parsing["Parser Layer (pluggable)"]
            P_MD["Markdown Parser<br/>(Obsidian)"]
            P_CODE["Code Parser<br/>(Tree-Sitter, future)"]
            P_OTHER["Other Parsers<br/>(future)"]
        end

        INGEST["Ingestion Pipeline<br/>─────────<br/>hash → diff → route to parser<br/>→ update stores"]

        BITMAP["Bitmap Store<br/>─────────<br/>LMDB + Roaring (portable)<br/>+ tombstone bitmap"]

        REG["Registry<br/>─────────<br/>DuckDB<br/>IDs, paths, hashes,<br/>chunk metadata, bitmap catalog"]

        QUERY["Query Engine<br/>─────────<br/>filter resolution<br/>→ bitmap intersection<br/>→ metadata assembly"]

        IFACE["Interface Layer<br/>─────────<br/>MCP Server │ HTTP API │ CLI"]
    end

    WATCH --> INGEST
    INGEST --> Parsing
    Parsing --> INGEST
    INGEST --> REG
    INGEST --> BITMAP
    IFACE --> QUERY
    QUERY --> BITMAP
    QUERY --> REG
```

### Module contracts summary

| Module | Input | Output | Owns |
|--------|-------|--------|------|
| **Watcher** | fs events | `ChangeEvent { path, kind }` | Nothing persistent |
| **Parser (trait)** | Raw file bytes + path | `ParseResult { frontmatter, chunks, links, tags, auto_type }` | Nothing — pure function |
| **Ingestion** | `ChangeEvent` | Writes to Registry + Bitmap Store | Diffing logic, BLAKE3 hashing |
| **Registry** | CRUD operations | `DocRecord`, `ChunkRecord`, `BitmapCatalogEntry` | DuckDB |
| **Bitmap Store** | key → bitmap ops | Roaring Bitmaps (serialized portable) | LMDB |
| **Query Engine** | `QueryRequest { filters, limit }` | `QueryResult { matches: [MatchPointer] }` | Query planning |
| **Interface Layer** | Protocol-specific requests | Protocol-specific responses | MCP/HTTP/CLI translation |

---

## 3. The Parser Trait

The parser is a **pluggable contract**. Ingestion doesn't know what kind of file it's processing — it delegates to a parser that implements a common trait.

```mermaid
classDiagram
    class Parser {
        <<trait>>
        +can_parse(path: &Path) bool
        +parse(path: &Path, content: &[u8]) ParseResult
    }

    class ParseResult {
        +chunks: Vec~Chunk~
        +tags: Vec~String~
        +links: Vec~LinkRef~
        +auto_type: Option~NoteType~
        +frontmatter: HashMap~String, Value~
    }

    class Chunk {
        +byte_range: Range
        +heading: Option~String~
        +depth: u8
    }

    class MarkdownParser {
        +can_parse() bool
        +parse() ParseResult
    }

    class CodeParser {
        <<future>>
        +can_parse() bool
        +parse() ParseResult
    }

    Parser <|.. MarkdownParser
    Parser <|.. CodeParser
    Parser --> ParseResult
    ParseResult --> Chunk
```

For the Obsidian phase, we build `MarkdownParser`. It extracts:
- YAML frontmatter (tags, aliases, custom fields)
- `[[wikilinks]]` and `[markdown](links)`
- Header hierarchy (for chunk boundaries)
- Structural patterns → auto-type (task list density, link density, etc.)

---

## 4. Registry Schema (DuckDB)

The registry supports both file-level and chunk-level records, plus the bitmap catalog.

```mermaid
erDiagram
    DOCUMENTS {
        u32 doc_id PK
        string file_path
        string source_type "obsidian | code | confluence"
        bytes blake3_hash
        timestamp last_indexed
        string auto_type "task | moc | note | reference"
    }

    CHUNKS {
        u32 chunk_id PK
        u32 doc_id FK
        u32 byte_start
        u32 byte_end
        string heading
        u8 heading_depth
    }

    BITMAP_CATALOG {
        string bitmap_key PK "tag:work, folder:/projects, type:task"
        string category "tag | folder | link | type | source"
        u32 cardinality
        timestamp last_updated
    }

    GLOBAL_STATE {
        u32 next_doc_id
        u32 next_chunk_id
        u32 total_documents
        bytes tombstone_bitmap_ref
    }

    DOCUMENTS ||--o{ CHUNKS : "has"
    BITMAP_CATALOG ||--|| LMDB_STORE : "references"
```

### File move handling

A rename/move is:
1. `UPDATE documents SET file_path = new_path WHERE doc_id = X`
2. Remove `doc_id` from old `folder:{old_path}` bitmap
3. Insert `doc_id` into `folder:{new_path}` bitmap
4. All other bitmaps (tags, links, type) unchanged — same ID

This is cheap. The ID is stable. No downstream impact.

### File delete handling

1. Insert `doc_id` into the **tombstone bitmap**
2. Registry row stays (marked deleted or just masked out by tombstone)
3. Every query automatically applies `AND NOT tombstone` before returning results
4. Periodic compaction job: actually removes tombstoned IDs from all bitmaps and the registry

---

## 5. Bitmap Store — Key Schema

Every bitmap in LMDB is keyed by a namespaced string. The bitmap catalog in DuckDB tracks what exists and its cardinality.

```
LMDB Keys:
  tag:work           → Roaring Bitmap (portable serialized)
  tag:urgent         → Roaring Bitmap
  tag:work/project/a → Roaring Bitmap
  folder:/projects   → Roaring Bitmap
  folder:/daily      → Roaring Bitmap
  link:ProjectAlpha  → Roaring Bitmap (all docs that link TO ProjectAlpha)
  type:task          → Roaring Bitmap
  type:moc           → Roaring Bitmap
  _tombstone         → Roaring Bitmap (special: deleted IDs)
```

### Hierarchical tag flattening

For a note tagged `#work/project/alpha`, ingestion inserts the doc_id into **three** bitmaps:
- `tag:work`
- `tag:work/project`
- `tag:work/project/alpha`

A filter for `tag:work` catches everything underneath without any traversal.

---

## 6. Interface Layer — Three Modes

BIEM exposes the same query engine through three interfaces:

```mermaid
graph TB
    subgraph Interface["Interface Layer"]
        direction LR
        MCP["MCP Server<br/>─────────<br/>AI agents call BIEM<br/>as a tool"]
        API["HTTP API<br/>─────────<br/>Obsidian plugin,<br/>scripts, webhooks"]
        CLI["CLI<br/>─────────<br/>Developer workflow,<br/>debugging, scripting"]
    end

    QE["Query Engine"]

    MCP --> QE
    API --> QE
    CLI --> QE
```

| Mode | Use case | Why |
|------|----------|-----|
| **MCP** | AI agent calls `biem_search` as a tool | Primary AI integration. Agent gets pointers, decides what to read. |
| **HTTP API** | Obsidian plugin, VS Code extension, scripts | Non-MCP integrations. Could power a sidebar showing "related notes by bitmap". |
| **CLI** | `biem search --tag work --type task` | Debugging, shell pipelines, demos. Essential for development. |

### Response payload (all modes return the same core data)

```
MatchPointer {
    doc_id: u32,
    file_path: String,
    source_type: "obsidian",
    chunk: Option<{ byte_start, byte_end, heading }>,
    matched_filters: ["tag:work", "type:task"],
    auto_type: "task",
    score: Option<f32>,          // future: semantic score
    last_modified: Timestamp,
}
```

The consumer (LLM, CLI user, script) decides whether to read the file, read a chunk, or use it as context for a follow-up.

---

## 7. Query Engine — Filter Resolution

```mermaid
flowchart TD
    INPUT["QueryRequest<br/>{filters: [tag:work, type:task], op: AND, limit: 10}"]
    CATALOG["Lookup bitmap catalog<br/>for cardinality"]
    ORDER["Sort filters by cardinality ASC<br/>(smallest first)"]
    FETCH["Fetch bitmaps from LMDB<br/>(memory-mapped, zero-copy)"]
    INTERSECT["Sequential AND intersection<br/>(smallest → largest)"]
    TOMBSTONE["AND NOT _tombstone"]
    RESOLVE["Resolve matching IDs<br/>against Registry"]
    OUTPUT["QueryResult<br/>{matches: [MatchPointer]}"]

    INPUT --> CATALOG --> ORDER --> FETCH --> INTERSECT --> TOMBSTONE --> RESOLVE --> OUTPUT
```

The cardinality sort is the key optimisation: if `type:task` has 12 entries and `tag:work` has 50,000, we start with the 12-entry bitmap. CPU work is proportional to the smallest set.

---

## 8. Decisions from Iteration 2

| # | Decision |
|---|----------|
| Q1 | **Yes — SourceFeed trait** with filesystem watcher as the first implementation |
| Q2 | **Single-threaded sequential for Phase 1** — concurrency is a roadmap item (see `002-roadmap.md`) |
| Q3 | **CLI design approved** — first pass as shown below |
| Q4 | **Configurable via `biem config`** — supports `~/.biem/` (global) and `.biem/` (local/co-located). See §8.1 |
| Q5 | **Needs further exploration** — see §8.2 |

### 8.1 Configuration & State Directory

BIEM uses a two-tier config model:

```
~/.biem/                          # Global config
  config.toml                     # Default settings, registered vaults
  vaults/
    <vault-hash>/                 # Per-vault state (default location)
      registry.duckdb
      bitmaps.lmdb/
      index.lock

<vault-path>/.biem/               # Local state (opt-in via biem config)
  registry.duckdb
  bitmaps.lmdb/
  index.lock
```

The `biem config` command controls this:
```
biem config                       # show current config
biem config --storage local       # store state inside the vault (.biem/)
biem config --storage global      # store state in ~/.biem/vaults/<hash>/
```

**Default is global** (`~/.biem/`) — keeps the vault clean, avoids polluting git/sync. Local is available for portability or when the vault and index should travel together.

The global config tracks registered vaults:
```toml
# ~/.biem/config.toml
[vaults.my-notes]
path = "/Users/me/Documents/Obsidian"
storage = "global"  # or "local"
source_type = "obsidian"

[vaults.work-vault]
path = "/Users/me/Work/Notes"
storage = "local"
source_type = "obsidian"
```

CLI flow:
```
biem init /path/to/vault          # registers vault, creates state dir
biem init /path/to/vault --local  # same but stores state inside vault
```

### 8.2 Initial Indexing vs Incremental — Exploration

There are two fundamentally different ingestion scenarios:

**Incremental** (steady state): A file changes → watcher fires → parse one file → update stores. Simple, well-understood.

**Initial** (`biem init` on a 10k+ note vault): Walk the entire directory tree, parse everything, populate the registry and bitmap store from scratch.

The challenge with initial indexing is **write amplification**:
- Each file produces 1 registry insert + N bitmap updates
- 10,000 files × ~5 tags average = 50,000 bitmap mutations
- Each LMDB write for a bitmap requires deserialize → mutate → serialize → write

**Proposed approach — Batch-then-flush:**

```mermaid
flowchart TD
    WALK["Walk vault directory tree<br/>collect all .md paths"]
    PARSE["Parse all files<br/>(single-threaded, sequential)"]
    COLLECT["Collect in-memory:<br/>- Vec of DocRecords<br/>- HashMap of bitmap_key → Vec of doc_ids"]
    BULK_REG["Bulk INSERT into DuckDB<br/>(single transaction)"]
    BULK_BMP["For each bitmap key:<br/>build Roaring from Vec of ids<br/>serialize once → write to LMDB"]
    CATALOG["Bulk INSERT bitmap catalog<br/>(cardinalities)"]
    DONE["Index complete"]

    WALK --> PARSE --> COLLECT --> BULK_REG --> BULK_BMP --> CATALOG --> DONE
```

Key difference: instead of deserialize-mutate-serialize per file, we **accumulate all IDs per bitmap key in memory** and build each Roaring Bitmap once at the end. For 10k files this is trivially small in memory (~a few MB of integer vectors) but avoids tens of thousands of LMDB round-trips.

After initial indexing completes, the watcher starts and all subsequent changes go through the incremental path.

| Aspect | Initial (batch) | Incremental (per-file) |
|--------|-----------------|----------------------|
| Trigger | `biem init` | Watcher event |
| Parse | All files, sequential | Single file |
| Registry writes | Bulk INSERT (one txn) | Single INSERT/UPDATE |
| Bitmap writes | Build from scratch, serialize once | Deserialize → mutate → serialize |
| Expected perf (10k files) | Seconds | Milliseconds per file |

---

## 9. Phase 1 Module Build Sequence (Proposed)

```mermaid
gantt
    title BIEM Phase 1 — Obsidian Core
    dateFormat YYYY-MM-DD
    axisFormat Week %W

    section Foundation
    Registry (DuckDB schema + CRUD)          :a1, 2026-05-12, 2w
    Bitmap Store (LMDB + Roaring ops)        :a2, 2026-05-12, 2w

    section Parsing
    Markdown Parser (frontmatter, links, auto-type) :a3, after a1, 3w

    section Ingestion
    Ingestion Pipeline (hash, diff, write)   :a4, after a3, 2w
    Watcher (notify crate, debounce)         :a5, after a3, 1w

    section Query
    Query Engine (filter resolution, cardinality opt) :a6, after a4, 2w

    section Interface
    CLI (basic search + inspect)             :a7, after a6, 1w
    MCP Server                               :a8, after a6, 2w
    HTTP API                                 :a9, after a8, 1w
```

Foundation (Registry + Bitmap Store) comes first because everything depends on them. Parser next because ingestion depends on it. Query engine once there's data to query. Interfaces last as thin layers over the query engine.
