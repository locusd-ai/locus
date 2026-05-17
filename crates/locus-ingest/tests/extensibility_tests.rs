//! Tests for parser extensibility — SourceType::Custom and DocType::Custom.

#[cfg(test)]
mod tests {
    use std::path::Path;

    use locus_bitmap::memory::InMemoryBitmapStore;
    use locus_core::parser::{ParseError, Parser};
    use locus_core::query::{Filter, QueryEngine, QueryRequest};
    use locus_core::types::{Chunk, ChunkKind, ChunkMetadata, DocType, ParseResult};
    use locus_ingest::IngestionPipeline;
    use locus_query::BitmapQueryEngine;
    use locus_registry::memory::InMemoryRegistry;

    // ── Minimal custom parser ────────────────────────────────────────────────

    /// A synthetic "Confluence" parser that emits `source:confluence` in tags.
    struct ConfluenceParser;

    impl Parser for ConfluenceParser {
        fn can_parse(&self, path: &Path) -> bool {
            path.extension().map(|e| e == "confluence").unwrap_or(false)
        }

        fn parse(&self, _path: &Path, content: &[u8]) -> Result<ParseResult, ParseError> {
            let text = std::str::from_utf8(content).unwrap_or("");
            let tags = vec![
                "source:confluence".to_string(),
                "space:engineering".to_string(),
            ];
            Ok(ParseResult {
                chunks: vec![Chunk {
                    byte_range: 0..content.len(),
                    kind: ChunkKind::Body,
                    label: Some(text.lines().next().unwrap_or("").to_string()),
                    depth: 0,
                    metadata: ChunkMetadata::default(),
                }],
                tags,
                links: vec![],
                auto_type: Some(DocType::Custom("page".to_string())),
                frontmatter: Default::default(),
            })
        }
    }

    fn make_confluence_pipeline() -> IngestionPipeline {
        IngestionPipeline::new(
            vec![Box::new(ConfluenceParser)],
            Box::new(InMemoryRegistry::new()),
            Box::new(InMemoryBitmapStore::new()),
        )
    }

    // ── SourceType inference ─────────────────────────────────────────────────

    #[test]
    fn custom_parser_source_tag_infers_custom_source_type() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("page.confluence");
        std::fs::write(&file, "Engineering Onboarding\nWelcome to the team.").unwrap();

        let mut pipeline = make_confluence_pipeline();
        let result = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(result.docs_indexed, 1);

        // source:confluence bitmap should contain this doc
        let (_, registry, bitmap_store) = pipeline.into_parts();
        let engine = BitmapQueryEngine::new(bitmap_store, registry);
        let results = engine
            .query(QueryRequest {
                filter: Filter::Key("source:confluence".into()),
                limit: Some(10),
                offset: None,
            })
            .unwrap();
        assert_eq!(results.matches.len(), 1, "expected one match for source:confluence");
    }

    #[test]
    fn custom_parser_does_not_write_source_obsidian() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("page.confluence");
        std::fs::write(&file, "A page").unwrap();

        let mut pipeline = make_confluence_pipeline();
        pipeline.bulk_index(dir.path()).unwrap();

        let (_, registry, bitmap_store) = pipeline.into_parts();
        let engine = BitmapQueryEngine::new(bitmap_store, registry);

        // source:obsidian must be empty — confluence docs must not fall back to obsidian
        let results = engine
            .query(QueryRequest {
                filter: Filter::Key("source:obsidian".into()),
                limit: Some(10),
                offset: None,
            })
            .unwrap();
        assert_eq!(results.matches.len(), 0, "confluence doc should not appear under source:obsidian");
    }

    // ── DocType::Custom ──────────────────────────────────────────────────────

    #[test]
    fn custom_doc_type_writes_type_bitmap_key() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("page.confluence"), "A confluence page").unwrap();

        let mut pipeline = make_confluence_pipeline();
        pipeline.bulk_index(dir.path()).unwrap();

        let (_, registry, bitmap_store) = pipeline.into_parts();
        let engine = BitmapQueryEngine::new(bitmap_store, registry);

        let results = engine
            .query(QueryRequest {
                filter: Filter::Key("type:page".into()),
                limit: Some(10),
                offset: None,
            })
            .unwrap();
        assert_eq!(results.matches.len(), 1, "expected one match for type:page");
    }

    // ── Multi-source coexistence ─────────────────────────────────────────────

    #[test]
    fn markdown_and_custom_parsers_coexist() {
        use locus_parser::markdown::MarkdownParser;

        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("wiki");
        std::fs::create_dir_all(&sub).unwrap();

        std::fs::write(
            dir.path().join("note.md"),
            "---\ntags: [work]\n---\n# My Note\n",
        )
        .unwrap();
        std::fs::write(sub.join("page.confluence"), "A confluence page").unwrap();

        let mut pipeline = IngestionPipeline::new(
            vec![Box::new(MarkdownParser), Box::new(ConfluenceParser)],
            Box::new(InMemoryRegistry::new()),
            Box::new(InMemoryBitmapStore::new()),
        );
        let result = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(result.docs_indexed, 2);

        let (_, registry, bitmap_store) = pipeline.into_parts();
        let engine = BitmapQueryEngine::new(bitmap_store, registry);

        let obsidian = engine
            .query(QueryRequest {
                filter: Filter::Key("source:obsidian".into()),
                limit: Some(10),
                offset: None,
            })
            .unwrap();
        assert_eq!(obsidian.matches.len(), 1, "one obsidian doc");

        let confluence = engine
            .query(QueryRequest {
                filter: Filter::Key("source:confluence".into()),
                limit: Some(10),
                offset: None,
            })
            .unwrap();
        assert_eq!(confluence.matches.len(), 1, "one confluence doc");
    }

    // ── SourceTypeConfig round-trip ──────────────────────────────────────────

    #[test]
    fn source_type_config_custom_round_trips_via_serde() {
        use locus_core::config::SourceTypeConfig;
        use serde::{Deserialize, Serialize};

        // TOML requires a top-level table — wrap in a struct for the round-trip.
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            source_type: SourceTypeConfig,
        }

        let original = Wrapper {
            source_type: SourceTypeConfig::Custom("confluence".to_string()),
        };
        let serialized = toml::to_string(&original).unwrap();
        assert!(serialized.contains("confluence"), "got: {serialized}");
        let deserialized: Wrapper = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized, original);
    }

    #[test]
    fn source_type_config_known_types_still_round_trip() {
        use locus_core::config::SourceTypeConfig;
        use serde::{Deserialize, Serialize};

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            source_type: SourceTypeConfig,
        }

        for (config, expected_value) in [
            (SourceTypeConfig::Obsidian, "obsidian"),
            (SourceTypeConfig::Code, "code"),
        ] {
            let wrapper = Wrapper { source_type: config };
            let serialized = toml::to_string(&wrapper).unwrap();
            assert!(serialized.contains(expected_value), "got: {serialized}");
            let deserialized: Wrapper = toml::from_str(&serialized).unwrap();
            assert_eq!(deserialized, wrapper);
        }
    }
}
