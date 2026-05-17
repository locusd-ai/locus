# Locus — Copilot Instructions

## Project overview

Locus is a local-first indexing and retrieval engine. Core goal: **minimise context pollution for LLMs** by returning precise structural pointers (not content) via bitmap pre-filtering.

- Language: **Rust**
- Architecture docs: `docs/architecture/001-system-overview.md`, `002-roadmap.md`, `003-contracts.md`
- Phase 1 scope: Obsidian vault indexing (markdown only)

## Architecture at a glance

- **Two binaries**: `locus` (CLI) and `locusd` (daemon with watcher/ingestion + optional MCP/HTTP)
- **9 crates**: `locus-core`, `locus-parser`, `locus-registry`, `locus-bitmap`, `locus-ingest`, `locus-watcher`, `locus-query`, `locus-cli`, `locus-daemon`
- **Trait objects** (`Box<dyn Trait>`) for pluggability — not generics
- **Sync core**, async only at interface boundary (tokio for MCP/HTTP, `spawn_blocking` into sync)
- **Registry** is pluggable (DuckDB first). **BitmapStore** is pluggable (LMDB/heed first, in-memory for tests)
- IDs: `u32` for both `DocId` and `ChunkId` — not configurable
- Bitmaps index `DocId` only; chunks resolved via registry lookup

## Ways of working

### Task-driven development
- Every feature starts with a task file in `tasks/<crate>-task.md`
- Tasks define the goal, break work into steps with validation criteria, and list the expected commit(s)
- Steps are checked off as completed — the task file is the source of truth for progress
- Before starting work, check for an existing task; if none exists, create one
- After completing a feature, mark all steps done and add ✅ to the title

### Documentation first
- Architecture decisions are documented before implementation
- Changes to contracts or schemas must be reflected in **both** `001-system-overview.md` and `003-contracts.md` — keep them aligned
- When making a decision, record it with rationale and alternatives considered
- Per-module design docs (`docs/modules/<crate>.md`) only when internal complexity warrants it — don't preemptively document trivial modules

### Diagrams
- Use Mermaid for all diagrams (class, ER, sequence, flowchart, gantt)
- After editing types/schemas, check that diagrams in both docs still match the code definitions

### Git conventions
- **Conventional commits**: `type(scope): description`
  - Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `ci`
  - Scopes: `arch`, `core`, `parser`, `registry`, `bitmap`, `ingest`, `watcher`, `query`, `cli`, `daemon`
  - Examples: `docs(arch): add system overview`, `feat(registry): implement DuckDB CRUD`, `test(bitmap): add LMDB round-trip tests`
- Commit frequently at logical checkpoints
- Keep commits atomic — one concern per commit

### Code style (Rust)
- Per-module error enums with `thiserror` — no `anyhow` in library crates (`anyhow` OK in binaries)
- Structured logging via `tracing` (`info!`, `warn!`, `instrument`)
- `#[from]` for error conversions across module boundaries
- Parsers are pure functions — no I/O, no state, content provided as `&[u8]`
- Chunk model uses `ChunkKind` + `ChunkMetadata` (not heading/depth only) — supports both document and code chunks

### Testing
- In-memory implementations of `Registry` and `BitmapStore` for unit tests
- Integration tests use fixture vault files in `tests/fixtures/`
- Test against the trait interface, not the concrete implementation

### Communication style
- The user has Scala experience, not deep Rust expertise — explain Rust-specific concepts with Scala parallels where helpful
- Be direct and concise — present options with trade-offs, give a recommendation, let the user decide
- When exploring a question, structure as: context → options → analysis → recommendation
- Don't over-ask — gather context, propose, execute
- If something is a small decision, just make it and note it; escalate only genuinely ambiguous choices

### What not to do
- Don't use generics for trait boundaries in v1 (trait objects instead)
- Don't wrap sync storage in async — use `spawn_blocking` at the boundary
- Don't store BIEM state inside the vault by default (global `~/.locus/` is default)
- Don't put content in query responses — Locus returns pointers, not content
- Don't use `u64` for IDs
