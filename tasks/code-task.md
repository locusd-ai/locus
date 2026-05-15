# Workstream 3: Code Intelligence

> Goal: Index codebases using Tree-Sitter. AST-aware chunking, language/kind bitmaps, multi-repo support. Rust first (dogfooding), then TypeScript and Python.

## Pre-work: Type system alignment (from 006-phase2-alignment.md)

### Step 0a — Extend `SourceType` and rename `VaultEntry` → `SourceEntry`
- [x] Add `Code` variant to `SourceType` in `biem-core/src/types.rs`
- [x] Update `source_key()` match in `biem-ingest/src/pipeline.rs`
- [x] Rename `VaultEntry` → `SourceEntry`, add `source_type: SourceType` field in `biem-core/src/config.rs`
- [x] Rename `vaults` → `sources` in `BiemConfig`
- [x] Update `register_vault` → `register_source`, `resolve_vault` → `resolve_source`
- [x] Update state directory: `~/.biem/sources/<name>/` instead of `~/.biem/vaults/<hash>/`
- [x] Update all callers in `biem-cli`, `biem-daemon`
- [x] Update config TOML format: `[vaults.*]` → `[sources.*]`
- [x] Add migration note for existing configs
- [x] `cargo test` passes, `cargo check` zero warnings

**Commit**: `refactor(core): rename VaultEntry to SourceEntry, vaults to sources`

### Step 0b — Rename `NoteType` → `DocType`, add code variants
- [x] Rename `NoteType` → `DocType` in `biem-core/src/types.rs`
- [x] Add variants: `SourceFile`, `TestFile`, `ConfigFile`
- [x] Update `ParseResult.auto_type` type
- [x] Update all match arms and references across crates
- [ ] Update `003-contracts.md`
- [x] `cargo test` passes

**Commit**: `refactor(core): rename NoteType to DocType, add code variants`

### Step 0c — Add `Code` variant to `BitmapCategory`
- [x] Already present in `types.rs` ✅ (from WS1 prep)
- [x] Verify it's used correctly in bitmap catalog operations

**Commit**: (skip — already done)

---

## Steps

### Step 1 — Create `biem-code` crate with `CodeParser` skeleton
- [x] Create `crates/biem-code/` with `Cargo.toml`
- [x] Add to workspace `Cargo.toml`
- [x] Add `tree-sitter` and `tree-sitter-rust` dependencies
- [x] Implement `CodeParser` struct with language registry (`HashMap<String, Language>`)
- [x] Implement `Parser` trait: `can_parse` checks extension against registered languages
- [x] Implement `parse` skeleton: returns empty `ParseResult` for now
- [x] Unit test: `can_parse` returns true for `.rs`, false for `.md`

**Commit**: `feat(code): create biem-code crate with CodeParser skeleton`

### Step 2 — Rust grammar: AST walking and chunk extraction
- [x] Parse Rust source with `tree-sitter-rust`
- [x] Walk AST to extract: `fn`, `impl`, `struct`, `enum`, `trait`, `mod`, `use`, `const`/`static`
- [x] Map AST nodes to `ChunkKind` variants
- [x] Extract labels: function names, struct names, impl target names
- [x] Extract depth from scope nesting
- [x] Extract metadata: `signature` (fn params + return type), `visibility` (pub/pub(crate)/private)
- [x] Unit test: parse a Rust file → correct chunks with correct kinds, labels, signatures
- [x] Unit test: nested functions/methods get correct depth
- [x] Unit test: impl blocks produce `Method` chunks for their functions

**Commit**: `feat(code): implement Rust AST chunk extraction`

### Step 3 — Code-specific bitmap key generation
- [x] Generate `lang:rust` from file extension
- [x] Generate `kind:function`, `kind:method`, `kind:class`, etc. from `ChunkKind`
- [x] Generate `visibility:public`, `visibility:private` from `ChunkMetadata`
- [x] Generate `async:true` for async functions (needs AST check)
- [x] Generate `import:<crate>` from `use` statements
- [x] Generate `repo:<name>` from source registration config
- [x] Wire key generation into ingestion pipeline (code path)
- [x] Integration test: index a Rust file → verify bitmap keys exist in store

**Commit**: `feat(code): generate code-specific bitmap keys`

### Step 4 — `.biemignore` support
- [x] Parse `.biemignore` file (`.gitignore` syntax)
- [x] Check during directory walk in `bulk_index` — skip matched paths
- [x] Check during incremental indexing — skip matched events
- [x] Default ignore patterns: `target/`, `node_modules/`, `.git/`, `__pycache__/`
- [x] Unit test: ignore patterns correctly filter paths
- [x] Integration test: file in ignored dir is not indexed

**Commit**: `feat(ingest): add .biemignore support`

### Step 5 — `biem init --type code` and multi-repo registration
- [x] `biem init <path> --type code` registers a code source
- [x] Config stores `source_type: Code` in `SourceEntry`
- [x] Ingestion pipeline selects `CodeParser` when `source_type == Code`
- [x] Support multiple sources (both Obsidian vaults and code repos in same config)
- [x] `biem status` shows per-source stats
- [ ] Integration test: init a code repo, verify it indexes `.rs` files

**Commit**: `feat(cli): add code source registration with biem init --type code`

### Step 6 — TypeScript grammar
- [x] Add `tree-sitter-typescript` dependency
- [x] Register `.ts` and `.tsx` extensions
- [x] Extract: `function`, `class`, `interface`, `type`, `import`, `export`
- [x] Map to `ChunkKind` variants (Function, Class, Module, Import, Constant)
- [x] Extract metadata: export/default-export → `visibility:public`
- [x] Generate `import:react`, `import:express` etc. from import statements
- [x] Unit test: parse a TS file → correct chunks
- [x] Unit test: JSX/TSX files parse without errors

**Commit**: `feat(code): add TypeScript grammar support`

### Step 7 — Python grammar
- [ ] Add `tree-sitter-python` dependency
- [ ] Register `.py` extension
- [ ] Extract: `def`, `class`, `import`/`from...import`, module-level constants
- [ ] Map decorators: `@pytest.fixture` → convention tag, `@app.route` → convention tag
- [ ] Visibility heuristic: `_private` prefix → `visibility:private`
- [ ] Unit test: parse a Python file → correct chunks
- [ ] Unit test: decorated functions get correct metadata

**Commit**: `feat(code): add Python grammar support`

### Step 8 — Integration tests and dogfooding
- [ ] Index BIEM's own codebase: `biem init ./crates --type code`
- [ ] Query: `lang:rust AND kind:function AND visibility:public` → returns public functions
- [ ] Query: `import:roaring AND kind:function` → functions in files using roaring
- [ ] Query: `kind:test` → test functions across all crates
- [ ] Semantic query: `biem semantic "bitmap intersection" --filter "lang:rust AND kind:function"`
- [ ] Verify chunk labels match actual function/struct names
- [ ] Verify byte ranges are accurate (can extract correct source from file)
- [ ] Performance: measure parsing throughput (files/s)

**Commit**: `test(code): add integration tests and dogfood BIEM codebase`

### Step 9 — Docs update
- [ ] Update `001-system-overview.md` with code intelligence in pipeline diagram
- [ ] Update `003-contracts.md` with CodeParser, code bitmap keys, extended types
- [ ] Update `002-roadmap.md` to reflect WS3 progress
- [ ] Update Mermaid diagrams (ER, class, pipeline flow)

**Commit**: `docs(arch): update docs for code intelligence`

---

## Validation

- [ ] All existing tests still pass (no regressions)
- [ ] `biem init <rust-project> --type code` indexes Rust source files
- [ ] `biem search --filter "lang:rust AND kind:function"` returns functions
- [ ] `biem search --filter "visibility:public AND kind:function"` filters correctly
- [ ] `biem search --filter "import:serde"` finds files using serde
- [ ] `biem semantic "error handling" --filter "lang:rust"` returns ranked results
- [ ] TypeScript and Python files parse and index correctly
- [ ] `.biemignore` excludes `target/`, `node_modules/` etc.
- [ ] Multiple sources (Obsidian + code) coexist in config and query independently
- [ ] Code parsing throughput > 5K files/s (Rust grammar)
