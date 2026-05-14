# Workstream 3: Code Intelligence

> Goal: Index codebases using Tree-Sitter. AST-aware chunking, language/kind bitmaps, multi-repo support. Rust first (dogfooding), then TypeScript and Python.

## Pre-work: Type system alignment (from 006-phase2-alignment.md)

### Step 0a — Extend `SourceType` and rename `VaultEntry` → `SourceEntry`
- [ ] Add `Code` variant to `SourceType` in `biem-core/src/types.rs`
- [ ] Update `source_key()` match in `biem-ingest/src/pipeline.rs`
- [ ] Rename `VaultEntry` → `SourceEntry`, add `source_type: SourceType` field in `biem-core/src/config.rs`
- [ ] Rename `vaults` → `sources` in `BiemConfig`
- [ ] Update `register_vault` → `register_source`, `resolve_vault` → `resolve_source`
- [ ] Update state directory: `~/.biem/sources/<name>/` instead of `~/.biem/vaults/<hash>/`
- [ ] Update all callers in `biem-cli`, `biem-daemon`
- [ ] Update config TOML format: `[vaults.*]` → `[sources.*]`
- [ ] Add migration note for existing configs
- [ ] `cargo test` passes, `cargo check` zero warnings

**Commit**: `refactor(core): rename VaultEntry to SourceEntry, vaults to sources`

### Step 0b — Rename `NoteType` → `DocType`, add code variants
- [ ] Rename `NoteType` → `DocType` in `biem-core/src/types.rs`
- [ ] Add variants: `SourceFile`, `TestFile`, `ConfigFile`
- [ ] Update `ParseResult.auto_type` type
- [ ] Update all match arms and references across crates
- [ ] Update `003-contracts.md`
- [ ] `cargo test` passes

**Commit**: `refactor(core): rename NoteType to DocType, add code variants`

### Step 0c — Add `Code` variant to `BitmapCategory`
- [ ] Already present in `types.rs` ✅ (from WS1 prep)
- [ ] Verify it's used correctly in bitmap catalog operations

**Commit**: (skip if already done)

---

## Steps

### Step 1 — Create `biem-code` crate with `CodeParser` skeleton
- [ ] Create `crates/biem-code/` with `Cargo.toml`
- [ ] Add to workspace `Cargo.toml`
- [ ] Add `tree-sitter` and `tree-sitter-rust` dependencies
- [ ] Implement `CodeParser` struct with language registry (`HashMap<String, Language>`)
- [ ] Implement `Parser` trait: `can_parse` checks extension against registered languages
- [ ] Implement `parse` skeleton: returns empty `ParseResult` for now
- [ ] Unit test: `can_parse` returns true for `.rs`, false for `.md`

**Commit**: `feat(code): create biem-code crate with CodeParser skeleton`

### Step 2 — Rust grammar: AST walking and chunk extraction
- [ ] Parse Rust source with `tree-sitter-rust`
- [ ] Walk AST to extract: `fn`, `impl`, `struct`, `enum`, `trait`, `mod`, `use`, `const`/`static`
- [ ] Map AST nodes to `ChunkKind` variants
- [ ] Extract labels: function names, struct names, impl target names
- [ ] Extract depth from scope nesting
- [ ] Extract metadata: `signature` (fn params + return type), `visibility` (pub/pub(crate)/private)
- [ ] Unit test: parse a Rust file → correct chunks with correct kinds, labels, signatures
- [ ] Unit test: nested functions/methods get correct depth
- [ ] Unit test: impl blocks produce `Method` chunks for their functions

**Commit**: `feat(code): implement Rust AST chunk extraction`

### Step 3 — Code-specific bitmap key generation
- [ ] Generate `lang:rust` from file extension
- [ ] Generate `kind:function`, `kind:method`, `kind:class`, etc. from `ChunkKind`
- [ ] Generate `visibility:public`, `visibility:private` from `ChunkMetadata`
- [ ] Generate `async:true` for async functions (needs AST check)
- [ ] Generate `import:<crate>` from `use` statements
- [ ] Generate `repo:<name>` from source registration config
- [ ] Wire key generation into ingestion pipeline (code path)
- [ ] Integration test: index a Rust file → verify bitmap keys exist in store

**Commit**: `feat(code): generate code-specific bitmap keys`

### Step 4 — `.biemignore` support
- [ ] Parse `.biemignore` file (`.gitignore` syntax)
- [ ] Check during directory walk in `bulk_index` — skip matched paths
- [ ] Check during incremental indexing — skip matched events
- [ ] Default ignore patterns: `target/`, `node_modules/`, `.git/`, `__pycache__/`
- [ ] Unit test: ignore patterns correctly filter paths
- [ ] Integration test: file in ignored dir is not indexed

**Commit**: `feat(ingest): add .biemignore support`

### Step 5 — `biem init --type code` and multi-repo registration
- [ ] `biem init <path> --type code` registers a code source
- [ ] Config stores `source_type: Code` in `SourceEntry`
- [ ] Ingestion pipeline selects `CodeParser` when `source_type == Code`
- [ ] Support multiple sources (both Obsidian vaults and code repos in same config)
- [ ] `biem status` shows per-source stats
- [ ] Integration test: init a code repo, verify it indexes `.rs` files

**Commit**: `feat(cli): add code source registration with biem init --type code`

### Step 6 — TypeScript grammar
- [ ] Add `tree-sitter-typescript` dependency
- [ ] Register `.ts` and `.tsx` extensions
- [ ] Extract: `function`, `class`, `interface`, `type`, `import`, `export`
- [ ] Map to `ChunkKind` variants (Function, Class, Module, Import, Constant)
- [ ] Extract metadata: export/default-export → `visibility:public`
- [ ] Generate `import:react`, `import:express` etc. from import statements
- [ ] Unit test: parse a TS file → correct chunks
- [ ] Unit test: JSX/TSX files parse without errors

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
