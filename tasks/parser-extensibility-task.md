# Parser Extensibility Task ✅

Prerequisite for Phase 5 (Extended Sources). Opens the closed enums in `locus-core` that
currently prevent third-party parsers from expressing a new source type without forking core.

## Problem

The `Parser` trait is clean and pluggable. The blocker is several exhaustive enums:

- `SourceType` — only `Obsidian` / `Code`. Anything else infers as `Obsidian`, writing a
  `source:obsidian` bitmap key for e.g. a Confluence document (wrong).
- `DocType` — no way to express a Confluence page, Jira ticket, etc.
- `infer_source_type` in `locus-ingest/src/pipeline.rs:185` — heuristic based on `lang:` tags;
  ignores any `source:*` the parser may have emitted.

`BitmapCategory::Custom` already exists — no change needed there.

## Tasks

- [x] **1. Open `SourceType`** — add `Custom(String)` variant to `locus-core/src/types.rs`
  - Update all match arms (compiler will flag exhaustiveness errors)
  - `source_key` in `pipeline.rs`: `SourceType::Custom(s) => format!("source:{s}")`

- [x] **2. Open `DocType`** — add `Custom(String)` variant to `locus-core/src/types.rs`
  - Update all match arms
  - `type_key` in `pipeline.rs`: `DocType::Custom(s) => format!("type:{s}")`

- [x] **3. Update `infer_source_type`** (`locus-ingest/src/pipeline.rs:185`)
  - Check `ParseResult.tags` for a `source:*` tag first; if found, return `SourceType::Custom(name)`
  - Keep existing `lang:` heuristic as fallback for code; `Obsidian` as final default
  - This means parsers signal their source type by emitting `source:<name>` in tags — no pipeline
    changes required to add a new source

- [x] **4. Update docs**
  - `docs/architecture/003-contracts.md` — reflect `SourceType::Custom` and `DocType::Custom`
  - `docs/architecture/001-system-overview.md` — note that types are now open

- [x] **5. Add `docs/parsers.md`** — guide for parser implementors covering:
  - The `Parser` trait (`can_parse` + `parse`, pure function, no I/O)
  - `ParseResult` fields and what each one feeds (tags → bitmap keys, chunks → registry, links → graph)
  - Bitmap key naming conventions: `tag:`, `source:`, `type:`, `kind:`, `lang:`, `folder:`
  - How to signal source type: emit `source:<name>` in `ParseResult.tags`
  - Minimal worked example (e.g., a plain-text parser)
  - How to wire a parser into `IngestionPipeline::new()`

- [x] **6. Tests**
  - Unit test: parser emitting `source:confluence` → `SourceType::Custom("confluence")` inferred
  - Unit test: parser emitting `DocType::Custom("ticket")` → `type:ticket` bitmap key written
  - Integration test: index a file with a custom parser, query `source:mytype`, verify result
