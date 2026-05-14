#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use biem_bitmap::memory::InMemoryBitmapStore;
    use biem_core::bitmap::BitmapStore;
    use biem_core::registry::{NewDoc, Registry};
    use biem_core::types::SourceType;
    use biem_query::{BitmapQueryEngine, Filter, QueryEngine, QueryRequest};
    use biem_registry::memory::InMemoryRegistry;

    fn make_query_engine() -> BitmapQueryEngine {
        let mut bitmap_store = InMemoryBitmapStore::new();
        let mut registry = InMemoryRegistry::new();

        // Insert 3 docs
        use biem_core::registry::NewDoc;
        use biem_core::types::SourceType;

        let d1 = registry
            .insert_doc(NewDoc {
                file_path: PathBuf::from("/vault/work/task1.md"),
                source_type: SourceType::Obsidian,
                blake3_hash: [1; 32],
                auto_type: Some(biem_core::types::DocType::Task),
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
}
