# Locus

**A local-first context engine for AI agents. Pointers, not context soup.**

Your coding agent answers "where is X?" by grepping, guessing, and dumping whole files into its context window. Locus indexes your world once — code, notes, docs — and answers structural queries in milliseconds with *precise pointers*: file, chunk, function label, byte range, and why it matched. The agent reads exactly what it needs. Nothing leaves your machine.

```
$ locus search "tag:work"
2 matches (12ms)

  /vault/auth-design.md (doc_id=5)
    chunk 10 (Section) bytes 35..139  Authentication design
  /vault/session-handling.md (doc_id=6)
    chunk 12 (Section) bytes 21..85   Session handling
```

## The numbers

Locating code across 8 "where is…?" questions on this repo (72 documents — [full benchmark](docs/benchmarks/token-bench.md), reproduce with `scripts/token-bench.py`):

| Strategy | Tokens into context |
|---|---|
| grep + read matching files (agent default) | 568,502 |
| grep -C20 (disciplined agent) | 587,629 |
| **Locus pointer digest** | **79,587 (7× fewer)** |

And some queries — `complexity:high`, `visibility:public`, `convention:test` — have no grep equivalent at all.

## How it works

Three stages, each cheaper than the last one is precise:

1. **Bitmap pre-filter** — every tag, folder, topic, language and convention is a Roaring bitmap. `topic:database AND folder:crates/locus-query` is a handful of bitmap ops — microseconds over millions of documents.
2. **Semantic rerank** — local ONNX embeddings (fastembed) + HNSW vector search over *only the pre-filtered candidates*. No GPU, no API key, no data leaving the machine.
3. **Graph expansion** — wikilinks, imports and references form a graph. `locus graph expand` pulls in the n-hop neighbourhood; `locus graph central` finds the load-bearing documents.

## For AI agents (MCP)

`locusd` exposes the engine over MCP (`locus_search`, `locus_semantic`, `locus_graph`, `locus_inspect`, `locus_filters`, `locus_status`, `locus_remote_ingest`). One command registers it with Claude Code:

```sh
locus mcp install ~/your-repo    # writes .mcp.json — the agent now has a map
```

Agents stop re-discovering your codebase every session: queries return labeled pointers, and the agent fetches only the byte ranges it picks.

## Sources

- **Markdown / Obsidian vaults** — frontmatter, tags, wikilinks, sections
- **Code** — Rust, TypeScript, Python via tree-sitter (functions, classes, imports, test/async/visibility conventions)
- **Remote** — Confluence, Jira, Slack, webhooks (daemon polling loop)

## Quickstart

```sh
git clone https://github.com/locusd-ai/locus && cd locus
cargo install --path crates/locus-cli
cargo install --path crates/locus-daemon

locus init ~/notes                  # Obsidian/markdown vault
locus init ~/code/myrepo --type code
locus index ~/code/myrepo
locus search "topic:database"
locus semantic "how do we handle auth?"
locus mcp install ~/code/myrepo     # hook it up to Claude Code
```

## Design principles

- **Local-first.** Your index lives in `~/.locus/`. No cloud, no telemetry, no API keys.
- **Pointers, not content.** Query responses never include file content — agents and humans fetch the bytes they choose.
- **Rust, embedded-grade.** DuckDB registry, LMDB + Roaring bitmaps, usearch HNSW, tree-sitter parsing. Sync core, async only at the daemon edge.

## License

MIT.
