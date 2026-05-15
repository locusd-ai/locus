//! Integration tests for .biemignore and default ignore patterns.

#[cfg(test)]
mod tests {
    use std::fs;

    use biem_bitmap::memory::InMemoryBitmapStore;
    use biem_core::types::{ChangeEvent, ChangeKind};
    use biem_ingest::IngestionPipeline;
    use biem_parser::markdown::MarkdownParser;
    use biem_registry::memory::InMemoryRegistry;

    fn make_pipeline() -> IngestionPipeline {
        IngestionPipeline::new(
            vec![Box::new(MarkdownParser)],
            Box::new(InMemoryRegistry::new()),
            Box::new(InMemoryBitmapStore::new()),
        )
    }

    #[test]
    fn test_default_ignored_dirs_skipped_in_bulk_index() {
        let dir = tempfile::tempdir().unwrap();

        // Create a normal file
        let good = dir.path().join("note.md");
        fs::write(&good, "# Hello\nSome content").unwrap();

        // Create files inside default-ignored directories
        let target_dir = dir.path().join("target");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("output.md"), "# Target\nIgnored").unwrap();

        let node_dir = dir.path().join("node_modules");
        fs::create_dir_all(&node_dir).unwrap();
        fs::write(node_dir.join("dep.md"), "# Dep\nIgnored").unwrap();

        let git_dir = dir.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("config.md"), "# Git\nIgnored").unwrap();

        let mut pipeline = make_pipeline();
        let result = pipeline.bulk_index(dir.path()).unwrap();

        // Only the one good file should be indexed
        assert_eq!(result.docs_indexed, 1, "only note.md should be indexed");
    }

    #[test]
    fn test_biemignore_file_excludes_patterns() {
        let dir = tempfile::tempdir().unwrap();

        // Create .biemignore
        fs::write(dir.path().join(".biemignore"), "drafts/\n*.tmp.md\n").unwrap();

        // Normal file
        fs::write(dir.path().join("note.md"), "# Note\nContent").unwrap();

        // File matching ignore pattern
        let drafts = dir.path().join("drafts");
        fs::create_dir_all(&drafts).unwrap();
        fs::write(drafts.join("wip.md"), "# WIP\nDraft").unwrap();

        // File matching glob pattern
        fs::write(dir.path().join("scratch.tmp.md"), "# Temp").unwrap();

        let mut pipeline = make_pipeline();
        let result = pipeline.bulk_index(dir.path()).unwrap();

        // Only note.md should be indexed (drafts/ and *.tmp.md excluded)
        assert_eq!(result.docs_indexed, 1, "only note.md should be indexed");
    }

    #[test]
    fn test_incremental_event_in_ignored_dir_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("target");
        fs::create_dir_all(&target_dir).unwrap();
        let ignored_file = target_dir.join("generated.md");
        fs::write(&ignored_file, "# Generated\nContent").unwrap();

        let mut pipeline = make_pipeline();
        let event = ChangeEvent {
            path: ignored_file,
            kind: ChangeKind::Created,
        };
        let result = pipeline.process_event(&event).unwrap();
        assert_eq!(result.action, biem_ingest::IngestAction::Skipped);
    }
}
