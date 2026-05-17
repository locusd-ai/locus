#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use locus_bitmap::memory::InMemoryBitmapStore;
    use locus_core::types::{ChangeEvent, ChangeKind};
    use locus_ingest::{IngestAction, IngestError, IngestionPipeline};
    use locus_parser::markdown::MarkdownParser;
    use locus_registry::memory::InMemoryRegistry;

    fn make_pipeline() -> IngestionPipeline {
        IngestionPipeline::new(
            vec![Box::new(MarkdownParser)],
            Box::new(InMemoryRegistry::new()),
            Box::new(InMemoryBitmapStore::new()),
        )
    }

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    // ── Created ──────────────────────────────────────────────────

    #[test]
    fn test_created_indexes_doc_and_bitmaps() {
        let mut pipeline = make_pipeline();
        let path = fixture_path("simple.md");
        let event = ChangeEvent {
            path: path.clone(),
            kind: ChangeKind::Created,
        };
        let result = pipeline.process_event(&event).unwrap();
        assert_eq!(result.action, IngestAction::Indexed);
        assert!(result.bitmaps_updated > 0);
    }

    #[test]
    fn test_created_task_note() {
        let mut pipeline = make_pipeline();
        let path = fixture_path("task-note.md");
        let event = ChangeEvent {
            path,
            kind: ChangeKind::Created,
        };
        let result = pipeline.process_event(&event).unwrap();
        assert_eq!(result.action, IngestAction::Indexed);
    }

    // ── Modified ─────────────────────────────────────────────────

    #[test]
    fn test_modified_unchanged_hash_skips() {
        let mut pipeline = make_pipeline();
        let path = fixture_path("simple.md");

        // First index
        pipeline
            .process_event(&ChangeEvent {
                path: path.clone(),
                kind: ChangeKind::Created,
            })
            .unwrap();

        // Same content → skip
        let result = pipeline
            .process_event(&ChangeEvent {
                path,
                kind: ChangeKind::Modified,
            })
            .unwrap();
        assert_eq!(result.action, IngestAction::Skipped);
        assert_eq!(result.bitmaps_updated, 0);
    }

    #[test]
    fn test_modified_changed_content_updates() {
        let mut pipeline = make_pipeline();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.md");
        fs::write(&file, "---\ntags: [a]\n---\n# Hello\nContent").unwrap();

        pipeline
            .process_event(&ChangeEvent {
                path: file.clone(),
                kind: ChangeKind::Created,
            })
            .unwrap();

        // Change content
        fs::write(&file, "---\ntags: [b]\n---\n# Hello\nNew content").unwrap();

        let result = pipeline
            .process_event(&ChangeEvent {
                path: file,
                kind: ChangeKind::Modified,
            })
            .unwrap();
        assert_eq!(result.action, IngestAction::Updated);
        // Should have added tag:b and removed tag:a
        assert!(result.bitmaps_updated > 0);
    }

    // ── Deleted ──────────────────────────────────────────────────

    #[test]
    fn test_deleted_tombstones() {
        let mut pipeline = make_pipeline();
        let path = fixture_path("simple.md");

        pipeline
            .process_event(&ChangeEvent {
                path: path.clone(),
                kind: ChangeKind::Created,
            })
            .unwrap();

        let result = pipeline
            .process_event(&ChangeEvent {
                path,
                kind: ChangeKind::Deleted,
            })
            .unwrap();
        assert_eq!(result.action, IngestAction::Tombstoned);
    }

    // ── Renamed ──────────────────────────────────────────────────

    #[test]
    fn test_renamed_updates_path() {
        let mut pipeline = make_pipeline();
        let dir = tempfile::tempdir().unwrap();
        let old_file = dir.path().join("old.md");
        fs::write(&old_file, "# Test\nContent").unwrap();

        pipeline
            .process_event(&ChangeEvent {
                path: old_file.clone(),
                kind: ChangeKind::Created,
            })
            .unwrap();

        let new_file = dir.path().join("new.md");
        let result = pipeline
            .process_event(&ChangeEvent {
                path: new_file,
                kind: ChangeKind::Renamed {
                    from: old_file,
                },
            })
            .unwrap();
        assert_eq!(result.action, IngestAction::Moved);
    }

    #[test]
    fn test_renamed_different_folder_updates_bitmaps() {
        let mut pipeline = make_pipeline();
        let dir = tempfile::tempdir().unwrap();
        let sub_a = dir.path().join("a");
        let sub_b = dir.path().join("b");
        fs::create_dir_all(&sub_a).unwrap();
        fs::create_dir_all(&sub_b).unwrap();

        let old_file = sub_a.join("note.md");
        fs::write(&old_file, "# Test\nContent").unwrap();

        pipeline
            .process_event(&ChangeEvent {
                path: old_file.clone(),
                kind: ChangeKind::Created,
            })
            .unwrap();

        let new_file = sub_b.join("note.md");
        let result = pipeline
            .process_event(&ChangeEvent {
                path: new_file,
                kind: ChangeKind::Renamed {
                    from: old_file,
                },
            })
            .unwrap();
        assert_eq!(result.action, IngestAction::Moved);
        assert_eq!(result.bitmaps_updated, 2); // old folder removed, new added
    }

    // ── No parser ────────────────────────────────────────────────

    #[test]
    fn test_no_parser_returns_error() {
        let mut pipeline = make_pipeline();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello").unwrap();

        let result = pipeline.process_event(&ChangeEvent {
            path: file,
            kind: ChangeKind::Created,
        });
        assert!(matches!(result, Err(IngestError::NoParser(_))));
    }

    // ── Bulk index ───────────────────────────────────────────────

    #[test]
    fn test_bulk_index_fixtures() {
        let mut pipeline = make_pipeline();
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");

        let result = pipeline.bulk_index(&fixtures).unwrap();
        assert!(result.docs_indexed >= 4); // simple, task-note, moc, no-frontmatter, code-blocks
        assert!(result.bitmaps_created > 0);
    }

    #[test]
    fn test_bulk_index_temp_dir() {
        let mut pipeline = make_pipeline();
        let dir = tempfile::tempdir().unwrap();

        fs::write(dir.path().join("a.md"), "---\ntags: [x]\n---\n# A\nHello").unwrap();
        fs::write(dir.path().join("b.md"), "# B\nWorld").unwrap();
        fs::write(dir.path().join("skip.txt"), "not parsed").unwrap();

        let result = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(result.docs_indexed, 2);
        assert!(result.bitmaps_created > 0);
    }

    // ── Compact ──────────────────────────────────────────────────

    #[test]
    fn test_compact_removes_tombstoned_docs() {

        let registry = Box::new(InMemoryRegistry::new());
        let bitmap_store = Box::new(InMemoryBitmapStore::new());
        let mut pipeline = IngestionPipeline::new(
            vec![Box::new(MarkdownParser)],
            registry,
            bitmap_store,
        );

        // Index two files
        pipeline.process_event(&ChangeEvent {
            kind: ChangeKind::Created,
            path: fixture_path("simple.md"),
        }).unwrap();
        pipeline.process_event(&ChangeEvent {
            kind: ChangeKind::Created,
            path: fixture_path("task-note.md"),
        }).unwrap();

        // Delete one
        pipeline.process_event(&ChangeEvent {
            kind: ChangeKind::Deleted,
            path: fixture_path("simple.md"),
        }).unwrap();

        // Compact
        let compact_result = pipeline.compact().unwrap();
        assert_eq!(compact_result.docs_removed, 1);

        // Verify tombstone bitmap is now empty
        let (_, registry, bitmap_store) = pipeline.into_parts();
        let tombstones = bitmap_store.get_tombstone().unwrap();
        assert!(tombstones.is_empty(), "tombstone bitmap should be empty after compact");

        // Verify the deleted doc is gone from registry
        let lookup = registry.lookup_by_id(1).unwrap();
        assert!(lookup.is_none(), "deleted doc should be gone from registry");

        // The remaining doc should still be queryable
        let lookup = registry.lookup_by_id(2).unwrap();
        assert!(lookup.is_some(), "surviving doc should still exist");
    }

    // ── Idempotent bulk_index ────────────────────────────────────

    #[test]
    fn test_bulk_index_twice_skips_all() {
        let mut pipeline = make_pipeline();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "---\ntags: [x]\n---\n# A\nHello").unwrap();
        fs::write(dir.path().join("b.md"), "# B\nWorld").unwrap();

        let r1 = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(r1.docs_indexed, 2);
        assert_eq!(r1.docs_skipped, 0);

        let r2 = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(r2.docs_indexed, 0);
        assert_eq!(r2.docs_skipped, 2);
        assert_eq!(r2.docs_updated, 0);
        assert_eq!(r2.docs_tombstoned, 0);
    }

    #[test]
    fn test_bulk_index_detects_modified_file() {
        let mut pipeline = make_pipeline();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "---\ntags: [x]\n---\n# A\nHello").unwrap();

        let r1 = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(r1.docs_indexed, 1);

        // Modify the file
        fs::write(dir.path().join("a.md"), "---\ntags: [y]\n---\n# A\nChanged").unwrap();

        let r2 = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(r2.docs_indexed, 0);
        assert_eq!(r2.docs_updated, 1);
        assert_eq!(r2.docs_skipped, 0);
    }

    #[test]
    fn test_bulk_index_tombstones_deleted_file() {
        let mut pipeline = make_pipeline();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "# A\nHello").unwrap();
        fs::write(dir.path().join("b.md"), "# B\nWorld").unwrap();

        pipeline.bulk_index(dir.path()).unwrap();

        // Delete one file
        fs::remove_file(dir.path().join("a.md")).unwrap();

        let r2 = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(r2.docs_indexed, 0);
        assert_eq!(r2.docs_skipped, 1);
        assert_eq!(r2.docs_tombstoned, 1);
    }

    #[test]
    fn test_bulk_index_partial_vault_completes() {
        let mut pipeline = make_pipeline();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "# A\nHello").unwrap();

        let r1 = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(r1.docs_indexed, 1);

        // Add a new file
        fs::write(dir.path().join("b.md"), "# B\nWorld").unwrap();

        let r2 = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(r2.docs_indexed, 1);
        assert_eq!(r2.docs_skipped, 1);
    }
}
