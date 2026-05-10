#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use biem_bitmap::memory::InMemoryBitmapStore;
    use biem_core::types::{ChangeEvent, ChangeKind};
    use biem_ingest::{IngestAction, IngestError, IngestionPipeline};
    use biem_parser::markdown::MarkdownParser;
    use biem_registry::memory::InMemoryRegistry;

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
}
