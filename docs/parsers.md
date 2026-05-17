# Writing a Custom Parser

This guide covers everything needed to add a new source type to Locus — from implementing the trait to wiring it into the pipeline.

## The `Parser` trait

```rust
pub trait Parser: Send + Sync {
    fn can_parse(&self, path: &Path) -> bool;
    fn parse(&self, path: &Path, content: &[u8]) -> Result<ParseResult, ParseError>;
}
```

Both methods are **pure functions** — no I/O, no state, no side effects. The pipeline handles all reading and writing.

- `can_parse` — fast path filter. Return `true` if this parser handles the given file extension.
- `parse` — receive file bytes, return a `ParseResult`. Called only when `can_parse` returns `true`.

## `ParseResult` fields

```rust
pub struct ParseResult {
    pub chunks: Vec<Chunk>,
    pub tags: Vec<String>,
    pub links: Vec<LinkRef>,
    pub auto_type: Option<DocType>,
    pub frontmatter: HashMap<String, serde_json::Value>,
}
```

| Field | What it feeds | Notes |
|-------|--------------|-------|
| `chunks` | Registry (chunk table for retrieval) | At minimum, emit one chunk covering the whole file |
| `tags` | Bitmap index | All strings ending up as bitmap keys — see naming conventions below |
| `links` | Bitmap index (`link:*`) + graph layer | Outbound references to other documents |
| `auto_type` | Bitmap index (`type:*`) | Use `DocType::Custom("your-type")` for new types |
| `frontmatter` | Registry metadata | Arbitrary key-value pairs; not bitmap-indexed |

## Signalling source type

Emit a `source:<name>` string in `ParseResult.tags`. The pipeline reads this tag and records the document under `SourceType::Custom("name")`.

```rust
tags: vec![
    "source:confluence".to_string(),  // signals SourceType::Custom("confluence")
    "space:engineering".to_string(),   // any other namespaced key
],
```

The `source:<name>` tag is automatically written as a bitmap key, so `filter: "source:confluence"` works in queries without any pipeline changes.

If no `source:*` tag is present, the pipeline falls back to `lang:` heuristics (code) or `source:obsidian` (default).

## Bitmap key naming conventions

Tags in `ParseResult.tags` are indexed as bitmap keys. Tags containing `:` are stored verbatim; tags without `:` are prefixed with `tag:`.

| Prefix | Meaning | Example |
|--------|---------|---------|
| `source:` | Source system | `source:confluence` |
| `tag:` | User-facing tag | `tag:work` (added automatically if no prefix) |
| `type:` | Document type | written from `auto_type` field, not from tags |
| `lang:` | Programming language | `lang:rust` — also triggers code heuristic |
| `kind:` | Code symbol kind | `kind:function`, `kind:class` |
| `visibility:` | Code visibility | `visibility:public` |
| `link:` | Outbound link | written from `links` field, not from tags |
| `folder:` | File path parent | written automatically by pipeline |
| `import:` | Code import target | `import:std::collections` |

Use your own namespace for source-specific keys: `space:`, `project:`, `status:`, etc. — they are indexed as-is.

## Chunk model

A `Chunk` describes a retrievable segment of the file.

```rust
pub struct Chunk {
    pub byte_range: Range<usize>,   // byte range within source file
    pub kind: ChunkKind,            // Section, Body, Function, Class, …
    pub label: Option<String>,      // heading text, function name, etc.
    pub depth: u8,                  // nesting depth (0 = top-level)
    pub metadata: ChunkMetadata,    // language, signature, visibility
}
```

For a flat document (e.g. a Confluence page), a single `Body` chunk covering the whole file is sufficient:

```rust
chunks: vec![Chunk {
    byte_range: 0..content.len(),
    kind: ChunkKind::Body,
    label: Some("Page Title".to_string()),
    depth: 0,
    metadata: ChunkMetadata::default(),
}],
```

## Worked example — minimal Confluence parser

```rust
use std::path::Path;
use locus_core::parser::{ParseError, Parser};
use locus_core::types::{Chunk, ChunkKind, ChunkMetadata, DocType, ParseResult};

pub struct ConfluenceParser;

impl Parser for ConfluenceParser {
    fn can_parse(&self, path: &Path) -> bool {
        path.extension().map(|e| e == "confluence").unwrap_or(false)
    }

    fn parse(&self, _path: &Path, content: &[u8]) -> Result<ParseResult, ParseError> {
        let text = std::str::from_utf8(content)
            .map_err(|_| ParseError::InvalidUtf8)?;

        let title = text.lines().next().unwrap_or("Untitled").to_string();

        Ok(ParseResult {
            chunks: vec![Chunk {
                byte_range: 0..content.len(),
                kind: ChunkKind::Body,
                label: Some(title),
                depth: 0,
                metadata: ChunkMetadata::default(),
            }],
            tags: vec![
                "source:confluence".to_string(),
                "space:engineering".to_string(),
            ],
            links: vec![],
            auto_type: Some(DocType::Custom("page".to_string())),
            frontmatter: Default::default(),
        })
    }
}
```

## Wiring into the pipeline

```rust
use locus_ingest::IngestionPipeline;

let pipeline = IngestionPipeline::new(
    vec![
        Box::new(MarkdownParser),       // existing
        Box::new(ConfluenceParser),     // new — order determines priority
    ],
    Box::new(registry),
    Box::new(bitmap_store),
);
```

The pipeline calls `can_parse` on each parser in order and uses the first match. If no parser matches a file, it is skipped.

## Config: registering a custom source

In `~/.locus/config.toml`, custom source types serialize as plain strings:

```toml
[sources.my-confluence]
path = "/data/confluence-export"
source_type = "confluence"
storage = "global"
data_dir = "/Users/me/.locus/sources/my-confluence"
```

Via the CLI:

```bash
locus init /data/confluence-export --type confluence
```

The CLI will use the markdown parser as a fallback for unknown types; for production use, wire the concrete parser via the library API or a `locusd` plugin.
