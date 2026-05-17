# Task: Implement locus-daemon (Daemon Binary) ✅

## Goal
Implement the `locusd` daemon binary with three modes: watcher + ingestion loop, MCP server (stdio), and HTTP API server. All query operations delegate to `dyn QueryEngine`. Supports `--mcp`, `--http`, or both together.

## Steps

### Step 1: Scaffold daemon with clap + tokio
- [x] Define `Cli` struct with vault path, `--data-dir`, `--memory`, `--debounce-ms`
- [x] Set up tokio runtime, tracing subscriber
- [x] Build persistent or in-memory stores based on flags
- **Validate**: `cargo check -p locus-daemon`

### Step 2: Watcher + ingestion event loop
- [x] Construct `FsWatcher` + `IngestionPipeline`
- [x] Optional initial bulk index (`--initial-index`)
- [x] Spawn watcher on blocking thread, pipe `ChangeEvent` via channel
- [x] Spawn ingestion loop on blocking thread
- [x] Graceful shutdown on Ctrl+C via `StopHandle`
- **Validate**: `cargo run -p locus-daemon -- <vault>`

### Step 3: MCP server (`--mcp`)
- [x] Implement `BiemMcpServer` with `Arc<dyn QueryEngine>`
- [x] 4 tools: `biem_search`, `biem_inspect`, `biem_status`, `biem_filters`
- [x] Uses rmcp `tool_router` + `tool_handler` + `Parameters<T>` wrapper
- [x] Runs over stdio transport
- [x] 9 unit tests
- **Validate**: `cargo test -p locus-daemon`

### Step 4: HTTP API server (`--http`)
- [x] Implement axum router with 4 endpoints:
  - `POST /v1/search` — bitmap-filtered search
  - `GET /v1/inspect?path=...` — file inspection
  - `GET /v1/status` — index health
  - `GET /v1/filters?category=...` — filter key discovery
- [x] All handlers delegate to `Arc<dyn QueryEngine>` via `spawn_blocking`
- [x] Error handling: `QueryError` → 500, not found → 404
- [x] `--port` flag (default 3141)
- [x] 7 integration tests using axum oneshot
- **Validate**: `cargo test -p locus-daemon`

### Step 5: Combined modes
- [x] `--http` only: block on HTTP server
- [x] `--mcp` only: block on MCP stdio
- [x] `--mcp --http`: HTTP in background tokio task, MCP foreground
- [x] Both modes share single `Arc<dyn QueryEngine>`
- **Validate**: `biemd <vault> --mcp --http`

### Step 6: Commits
- [x] `feat(daemon): implement biemd with watcher + ingestion event loop`
- [x] `feat(query): add inspect, status, list_filter_keys to QueryEngine trait`
- [x] `feat(daemon): implement MCP server with dyn QueryEngine`
- [x] `feat(daemon): add HTTP API server with axum`
