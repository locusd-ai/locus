# Task: Implement locus-parser (Markdown) ✅

## Goal
Implement the `Parser` trait from `locus-core` with a `MarkdownParser` that extracts chunks, tags, links, frontmatter, and auto-type from Obsidian-flavour markdown files. Pure function — no I/O, no state.

## Steps

### Step 1: Add dependencies
- [ ] Add `serde_yaml` (or `serde_yml`) to workspace for frontmatter parsing
- [ ] Add `serde_json` to locus-parser for frontmatter value conversion
- [ ] Wire up in `locus-parser/Cargo.toml`
- **Validate**: `cargo check -p locus-parser`

### Step 2: Frontmatter extraction
- [ ] Create `crates/locus-parser/src/markdown.rs`
- [ ] Implement `fn extract_frontmatter(content: &str) -> Option<(HashMap<String, Value>, Range<usize>)>`
- [ ] Detect `---` delimiters, parse YAML between them
- [ ] Extract `tags` array from frontmatter if present
- [ ] Return byte range of frontmatter block (for Frontmatter chunk)
- [ ] Wire up module in `lib.rs`
- **Validate**: `cargo check -p locus-parser`

### Step 3: Frontmatter tests
- [ ] Test valid frontmatter with tags, title, arbitrary keys
- [ ] Test missing frontmatter → None
- [ ] Test malformed YAML → BadFrontmatter error
- [ ] Test frontmatter byte range is correct
- [ ] Test tags extracted from frontmatter `tags:` field
- **Validate**: `cargo test -p locus-parser`

### Step 4: Chunk extraction (heading-based sectioning)
- [ ] Implement `fn extract_chunks(content: &str, fm_range: Option<Range<usize>>) -> Vec<Chunk>`
- [ ] Split on `# `, `## `, `### `, etc. (ATX headings)
- [ ] Each heading starts a new `Section` chunk with label = heading text, depth = heading level
- [ ] If frontmatter exists, first chunk is `Frontmatter`
- [ ] If no headings, single `Body` chunk covering entire content
- [ ] Byte ranges are non-overlapping and cover the full file
- **Validate**: `cargo check -p locus-parser`

### Step 5: Chunk extraction tests
- [ ] Test file with multiple headings → correct Section chunks with depths
- [ ] Test file with no headings → single Body chunk
- [ ] Test file with frontmatter + headings → Frontmatter + Section chunks
- [ ] Test nested headings (h1 > h2 > h3) → correct depth values
- [ ] Test byte ranges are contiguous and cover full file
- **Validate**: `cargo test -p locus-parser`

### Step 6: Link extraction
- [ ] Implement `fn extract_links(content: &str) -> Vec<LinkRef>`
- [ ] Parse `[[target]]` and `[[target|display]]` wikilinks
- [ ] Record byte offset of each link
- [ ] Ignore links inside code blocks (fenced ``` and indented)
- **Validate**: `cargo check -p locus-parser`

### Step 7: Link extraction tests
- [ ] Test `[[simple]]` → target="simple", display=None
- [ ] Test `[[target|display text]]` → target="target", display=Some("display text")
- [ ] Test multiple links in one file
- [ ] Test links inside code blocks are ignored
- [ ] Test byte offsets are correct
- **Validate**: `cargo test -p locus-parser`

### Step 8: Inline tag extraction
- [ ] Implement `fn extract_inline_tags(content: &str) -> Vec<String>`
- [ ] Parse `#tag` patterns (not inside code blocks, not `# headings`)
- [ ] Support nested tags: `#parent/child`
- [ ] Merge with frontmatter tags, deduplicate
- **Validate**: `cargo check -p locus-parser`

### Step 9: Inline tag tests
- [ ] Test `#simple` tag
- [ ] Test `#parent/child` nested tag
- [ ] Test tags inside code blocks are ignored
- [ ] Test `# heading` is not a tag
- [ ] Test deduplication with frontmatter tags
- **Validate**: `cargo test -p locus-parser`

### Step 10: Auto-type detection
- [ ] Implement `fn detect_auto_type(content: &str, links: &[LinkRef], frontmatter: &HashMap<String, Value>) -> Option<NoteType>`
- [ ] Task: high density of `- [ ]` / `- [x]` patterns
- [ ] Moc: high density of `[[links]]` relative to content length
- [ ] Reference: has `url`, `isbn`, or `source` keys in frontmatter
- [ ] Note: default / no strong signal
- **Validate**: `cargo check -p locus-parser`

### Step 11: Auto-type tests
- [ ] Test task-heavy file → Some(Task)
- [ ] Test link-heavy file → Some(Moc)
- [ ] Test file with `url` in frontmatter → Some(Reference)
- [ ] Test normal note → None or Some(Note)
- **Validate**: `cargo test -p locus-parser`

### Step 12: Assemble MarkdownParser
- [ ] Implement `Parser` trait for `MarkdownParser` struct
- [ ] `can_parse` → checks `.md` extension
- [ ] `parse` → orchestrates: UTF-8 decode → frontmatter → chunks → links → tags → auto_type → `ParseResult`
- [ ] Add integration-level tests with realistic Obsidian-style fixtures
- **Validate**: `cargo test -p locus-parser`

### Step 13: Test fixtures
- [ ] Create `tests/fixtures/simple.md` — basic note with heading, tags, links
- [ ] Create `tests/fixtures/task-note.md` — task-heavy note
- [ ] Create `tests/fixtures/moc.md` — map of content with many links
- [ ] Create `tests/fixtures/no-frontmatter.md` — plain markdown
- [ ] Create `tests/fixtures/code-blocks.md` — tags/links inside code blocks (should be ignored)
- [ ] Write integration tests in `crates/locus-parser/tests/` using fixtures
- **Validate**: `cargo test -p locus-parser`

### Step 14: Review against contracts
- [ ] Compare `MarkdownParser` against `Parser` trait in `locus-core/src/parser.rs`
- [ ] Cross-check `ParseResult` fields with `003-contracts.md` §2
- [ ] Verify parser is pure — no I/O, no state, content provided as `&[u8]`
- [ ] Verify `Chunk` uses `ChunkKind` + `ChunkMetadata` per contracts
- [ ] Cross-check with `001-system-overview.md` — parser diagrams still accurate
- **Validate**: manual review

### Step 15: Commit
- [ ] `feat(parser): implement frontmatter and chunk extraction` (after step 5)
- [ ] `feat(parser): implement link and tag extraction` (after step 9)
- [ ] `feat(parser): implement auto-type detection and MarkdownParser` (after step 13)
