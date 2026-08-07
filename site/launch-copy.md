# Launch copy

Drafts for announcing Locus. Every number here is from `docs/benchmarks/` — if
you edit a figure, edit it there first.

---

## One-liner

> Locus is a local-first index for AI agents. It answers "where is X?" with
> precise pointers — file, symbol, byte range — instead of dumping whole files
> into the context window.

## Two sentences

> Your coding agent answers "where is X?" by grepping, guessing, and reading
> nineteen files to learn where to look. Locus indexes your code and notes once
> and answers the same question with a digest of labelled pointers — 7× fewer
> tokens on our benchmark, and nothing leaves your machine.

---

## Show HN

**Title:** Show HN: Locus – a local-first index that gives coding agents pointers, not file dumps

Coding agents burn most of their context rediscovering the codebase. Ask one
"where is database access implemented?" and it greps, gets 19 hits, and reads
all 19 files — 80,824 tokens spent to find out where to look. Do that eight
times in a session and the window is gone.

Locus indexes the repo once and answers the same question with a 20,940-token
digest of pointers: file, symbol, byte range, and why it matched. Across eight
"where is…?" questions on its own source tree, grep-and-read costs 568,502
tokens; Locus costs 79,587. The agent then reads only the ranges it picks.

How it works — three stages, cheapest first:

1. **Bitmap pre-filter.** Every tag, folder, language, kind and convention is a
   Roaring bitmap over doc IDs. `topic:database AND folder:crates/locus-query`
   is a few compressed bitwise ops: ~19µs for an AND at 100K documents,
   in-memory, and it barely moves with corpus size. (On-disk LMDB is ~9ms cold
   and converges toward that once the daemon has warmed the page cache.)
2. **Semantic rerank.** Local ONNX embeddings + HNSW, scoring only what the
   bitmap kept. No GPU, no API key, no network.
3. **Graph expansion.** Wikilinks and imports form a graph; expand the n-hop
   neighbourhood or find the load-bearing files.

The part I think matters most isn't the speed, it's that some queries have no
grep equivalent at all — `complexity:high`, `visibility:public`,
`convention:test`, `async:true`. Those aren't strings in the file, they're
facts about the syntax tree, indexed at ingest.

It speaks MCP, so `locus mcp install ~/repo` and your agent has seven tools
including `locus_filters`, which lets it ask what keys exist before it asks
anything else — it stops guessing at your directory names.

Rust: DuckDB registry, LMDB + Roaring bitmaps, usearch HNSW, tree-sitter
parsing (Rust/TS/Python today). Also indexes Obsidian vaults, Confluence, Jira
and Slack. MIT. Benchmarks and method are in the repo — `scripts/token-bench.py`
reproduces the token numbers.

A query response has no field for file content. That's the whole design.

---

## Short social post

> Your agent spends 80,824 tokens grepping to answer "where is database access
> implemented?"
>
> Locus answers it in 20,940 — file, symbol, byte range, and why it matched.
> The agent reads only what it picks.
>
> Local-first, Rust, MIT. Speaks MCP.

## Alternate hook, for the perf-minded

> Compound filter query at 100,000 documents:
>
> SQL (DuckDB) — 4,758µs
> Property graph — 4,075µs
> HashSet intersection — 111µs
> Roaring bitmaps — 19µs, and it persists
>
> That's the pre-filter stage of Locus, a local-first index that hands AI
> agents pointers instead of file dumps.

---

## Things to say carefully

- **"Microseconds over millions of documents"** — the benchmark goes to 100,000.
  Say 100,000.
- **Bitmap timings are in-memory.** Always pair them with the on-disk caveat;
  someone will run it cold and get 9ms, and they'll be right.
- **The 10K files/s parsing target was missed** (~9,500). `REPORT.md` says so
  plainly. Don't quote the target as though it were the result.
- **Multi-repo and cross-repo queries are planned, not shipped.** Same for Go
  and Java parsing, prefix/wildcard keys and date-range filters. The feature
  table in `docs/architecture/004-feature-set.md` marks what's real.
- **7.1× and 7.4×** are the precise token ratios; "7×" is the fair round number.
