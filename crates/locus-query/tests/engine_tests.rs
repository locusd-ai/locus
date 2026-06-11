#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use locus_bitmap::memory::InMemoryBitmapStore;
    use locus_core::bitmap::BitmapStore;
    use locus_core::registry::{NewChunk, NewDoc, Registry};
    use locus_core::types::{ChunkKind, ChunkMetadata, SourceType};
    use locus_query::{BitmapQueryEngine, Filter, QueryEngine, QueryRequest};
    use locus_registry::memory::InMemoryRegistry;

    fn make_query_engine() -> BitmapQueryEngine {
        let mut bitmap_store = InMemoryBitmapStore::new();
        let mut registry = InMemoryRegistry::new();

        // Insert 3 docs
        use locus_core::registry::NewDoc;
        use locus_core::types::SourceType;

        let d1 = registry
            .insert_doc(NewDoc {
                file_path: PathBuf::from("/vault/work/task1.md"),
                source_type: SourceType::Obsidian,
                blake3_hash: [1; 32],
                auto_type: Some(locus_core::types::DocType::Task),
            })
            .unwrap();
        let d2 = registry
            .insert_doc(NewDoc {
                file_path: PathBuf::from("/vault/work/note1.md"),
                source_type: SourceType::Obsidian,
                blake3_hash: [2; 32],
                auto_type: None,
            })
            .unwrap();
        let d3 = registry
            .insert_doc(NewDoc {
                file_path: PathBuf::from("/vault/archive/old.md"),
                source_type: SourceType::Obsidian,
                blake3_hash: [3; 32],
                auto_type: None,
            })
            .unwrap();

        // Populate bitmaps
        bitmap_store.insert_id("tag:work", d1).unwrap();
        bitmap_store.insert_id("tag:work", d2).unwrap();
        bitmap_store.insert_id("tag:personal", d3).unwrap();
        bitmap_store.insert_id("type:task", d1).unwrap();
        bitmap_store.insert_id("folder:/vault/work", d1).unwrap();
        bitmap_store.insert_id("folder:/vault/work", d2).unwrap();
        bitmap_store.insert_id("folder:/vault/archive", d3).unwrap();
        bitmap_store.insert_id("source:obsidian", d1).unwrap();
        bitmap_store.insert_id("source:obsidian", d2).unwrap();
        bitmap_store.insert_id("source:obsidian", d3).unwrap();

        BitmapQueryEngine::new(Box::new(bitmap_store), Box::new(registry))
    }

    #[test]
    fn test_single_key_filter() {
        let engine = make_query_engine();
        let result = engine
            .query(QueryRequest {
                filter: Filter::Key("tag:work".into()),
                limit: None,
                offset: None,
            })
            .unwrap();
        assert_eq!(result.total_matching, 2);
        assert_eq!(result.matches.len(), 2);
    }

    #[test]
    fn test_and_filter() {
        let engine = make_query_engine();
        let result = engine
            .query(QueryRequest {
                filter: Filter::And(vec![
                    Filter::Key("tag:work".into()),
                    Filter::Key("type:task".into()),
                ]),
                limit: None,
                offset: None,
            })
            .unwrap();
        assert_eq!(result.total_matching, 1);
        assert_eq!(result.matches[0].file_path, PathBuf::from("/vault/work/task1.md"));
    }

    #[test]
    fn test_or_filter() {
        let engine = make_query_engine();
        let result = engine
            .query(QueryRequest {
                filter: Filter::Or(vec![
                    Filter::Key("tag:work".into()),
                    Filter::Key("tag:personal".into()),
                ]),
                limit: None,
                offset: None,
            })
            .unwrap();
        assert_eq!(result.total_matching, 3);
    }

    #[test]
    fn test_not_filter() {
        let engine = make_query_engine();
        let result = engine
            .query(QueryRequest {
                filter: Filter::Not(Box::new(Filter::Key("tag:work".into()))),
                limit: None,
                offset: None,
            })
            .unwrap();
        // 3 total docs - 2 with tag:work = 1
        assert_eq!(result.total_matching, 1);
        assert_eq!(result.matches[0].file_path, PathBuf::from("/vault/archive/old.md"));
    }

    #[test]
    fn test_nested_compound_filter() {
        let engine = make_query_engine();
        // "work tasks that are NOT in archive"
        let result = engine
            .query(QueryRequest {
                filter: Filter::And(vec![
                    Filter::Key("tag:work".into()),
                    Filter::Not(Box::new(Filter::Key("folder:/vault/archive".into()))),
                ]),
                limit: None,
                offset: None,
            })
            .unwrap();
        assert_eq!(result.total_matching, 2);
    }

    #[test]
    fn test_tombstoned_docs_excluded() {
        let mut bitmap_store = InMemoryBitmapStore::new();
        let mut registry = InMemoryRegistry::new();

        let d1 = registry
            .insert_doc(NewDoc {
                file_path: PathBuf::from("/a.md"),
                source_type: SourceType::Obsidian,
                blake3_hash: [1; 32],
                auto_type: None,
            })
            .unwrap();
        let d2 = registry
            .insert_doc(NewDoc {
                file_path: PathBuf::from("/b.md"),
                source_type: SourceType::Obsidian,
                blake3_hash: [2; 32],
                auto_type: None,
            })
            .unwrap();

        bitmap_store.insert_id("tag:x", d1).unwrap();
        bitmap_store.insert_id("tag:x", d2).unwrap();
        bitmap_store.tombstone(d1).unwrap();

        let engine = BitmapQueryEngine::new(Box::new(bitmap_store), Box::new(registry));
        let result = engine
            .query(QueryRequest {
                filter: Filter::Key("tag:x".into()),
                limit: None,
                offset: None,
            })
            .unwrap();
        assert_eq!(result.total_matching, 1);
        assert_eq!(result.matches[0].file_path, PathBuf::from("/b.md"));
    }

    #[test]
    fn test_limit_and_offset() {
        let engine = make_query_engine();
        let result = engine
            .query(QueryRequest {
                filter: Filter::Key("source:obsidian".into()),
                limit: Some(1),
                offset: Some(1),
            })
            .unwrap();
        assert_eq!(result.total_matching, 3); // total before limit
        assert_eq!(result.matches.len(), 1); // limited to 1
    }

    #[test]
    fn test_empty_result() {
        let engine = make_query_engine();
        let result = engine
            .query(QueryRequest {
                filter: Filter::Key("tag:nonexistent".into()),
                limit: None,
                offset: None,
            })
            .unwrap();
        assert_eq!(result.total_matching, 0);
        assert!(result.matches.is_empty());
    }

    #[test]
    fn test_list_filters() {
        let engine = make_query_engine();
        // No catalog entries populated in this test, so empty
        let filters = engine.list_filters(None).unwrap();
        assert!(filters.is_empty());
    }

    #[test]
    fn test_query_time_populated() {
        let engine = make_query_engine();
        let result = engine
            .query(QueryRequest {
                filter: Filter::Key("tag:work".into()),
                limit: None,
                offset: None,
            })
            .unwrap();
        // query_time_us should be set (may be 0 on very fast machines, but field exists)
        assert!(result.query_time_us < 1_000_000); // less than 1 second
    }

    // ── Nested chunk tree integration tests ───────────────────────

    /// Build an engine that has one document with a hierarchy of chunks:
    ///   Frontmatter (depth 0)
    ///   Section "Title"    (depth 1)
    ///     Section "Sub A"  (depth 2)
    ///     Section "Sub B"  (depth 2)
    ///   Section "Chapter2" (depth 1)
    ///     Section "Sub C"  (depth 2)
    fn make_engine_with_chunks() -> BitmapQueryEngine {
        let mut bitmap_store = InMemoryBitmapStore::new();
        let mut registry = InMemoryRegistry::new();

        let doc_id = registry
            .insert_doc(NewDoc {
                file_path: PathBuf::from("/vault/doc.md"),
                source_type: SourceType::Obsidian,
                blake3_hash: [5; 32],
                auto_type: None,
            })
            .unwrap();

        bitmap_store.insert_id("source:obsidian", doc_id).unwrap();

        registry
            .replace_chunks(
                doc_id,
                vec![
                    NewChunk {
                        doc_id,
                        kind: ChunkKind::Frontmatter,
                        byte_start: 0,
                        byte_end: 20,
                        label: None,
                        depth: 0,
                        metadata: ChunkMetadata::default(),
                    },
                    NewChunk {
                        doc_id,
                        kind: ChunkKind::Section,
                        byte_start: 20,
                        byte_end: 300,
                        label: Some("Title".to_string()),
                        depth: 1,
                        metadata: ChunkMetadata::default(),
                    },
                    NewChunk {
                        doc_id,
                        kind: ChunkKind::Section,
                        byte_start: 40,
                        byte_end: 160,
                        label: Some("Sub A".to_string()),
                        depth: 2,
                        metadata: ChunkMetadata::default(),
                    },
                    NewChunk {
                        doc_id,
                        kind: ChunkKind::Section,
                        byte_start: 160,
                        byte_end: 300,
                        label: Some("Sub B".to_string()),
                        depth: 2,
                        metadata: ChunkMetadata::default(),
                    },
                    NewChunk {
                        doc_id,
                        kind: ChunkKind::Section,
                        byte_start: 300,
                        byte_end: 600,
                        label: Some("Chapter2".to_string()),
                        depth: 1,
                        metadata: ChunkMetadata::default(),
                    },
                    NewChunk {
                        doc_id,
                        kind: ChunkKind::Section,
                        byte_start: 320,
                        byte_end: 600,
                        label: Some("Sub C".to_string()),
                        depth: 2,
                        metadata: ChunkMetadata::default(),
                    },
                ],
            )
            .unwrap();

        BitmapQueryEngine::new(Box::new(bitmap_store), Box::new(registry))
    }

    #[test]
    fn hydrate_produces_nested_chunk_tree() {
        let engine = make_engine_with_chunks();
        let result = engine
            .query(QueryRequest {
                filter: Filter::Key("source:obsidian".into()),
                limit: None,
                offset: None,
            })
            .unwrap();

        assert_eq!(result.matches.len(), 1);
        let chunks = &result.matches[0].chunks;

        // Top level: Frontmatter (depth 0) + "Title" (depth 1) + "Chapter2" (depth 1)
        assert_eq!(chunks.len(), 3, "three roots: Frontmatter, Title, Chapter2");

        // Frontmatter is a root with no children
        assert_eq!(chunks[0].kind, "Frontmatter");
        assert!(chunks[0].children.is_empty());

        // "Title" has two depth-2 children
        assert_eq!(chunks[1].label.as_deref(), Some("Title"));
        assert_eq!(chunks[1].children.len(), 2);
        assert_eq!(chunks[1].children[0].label.as_deref(), Some("Sub A"));
        assert_eq!(chunks[1].children[1].label.as_deref(), Some("Sub B"));

        // "Chapter2" has one depth-2 child
        assert_eq!(chunks[2].label.as_deref(), Some("Chapter2"));
        assert_eq!(chunks[2].children.len(), 1);
        assert_eq!(chunks[2].children[0].label.as_deref(), Some("Sub C"));
    }

    #[test]
    fn inspect_produces_nested_chunk_tree() {
        let engine = make_engine_with_chunks();
        let result = engine
            .inspect(std::path::Path::new("/vault/doc.md"))
            .unwrap()
            .expect("document should be in index");

        let chunks = &result.chunks;
        assert_eq!(chunks.len(), 3, "three roots: Frontmatter, Title, Chapter2");
        assert_eq!(chunks[1].children.len(), 2, "Title has two sub-sections");
        assert_eq!(chunks[2].children.len(), 1, "Chapter2 has one sub-section");
    }
}
