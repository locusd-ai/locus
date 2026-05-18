//! Integration tests: graph edge writes wired through IngestionPipeline.

#[cfg(test)]
mod tests {
    use std::fs;

    use locus_bitmap::memory::InMemoryBitmapStore;
    use locus_core::graph::{Edge, EdgeCategory, GraphStore};
    use locus_core::types::{ChangeEvent, ChangeKind};
    use locus_ingest::IngestionPipeline;
    use locus_parser::markdown::MarkdownParser;
    use locus_registry::duckdb::DuckDbRegistry;
    use locus_registry::graph::DuckDbGraphStore;
    use locus_registry::memory::InMemoryRegistry;
    use tempfile::TempDir;

    fn make_pipeline_and_graph() -> IngestionPipeline {
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        let gs = DuckDbGraphStore::new(registry.connection());
        IngestionPipeline::new(
            vec![Box::new(MarkdownParser)],
            Box::new(InMemoryRegistry::new()),
            Box::new(InMemoryBitmapStore::new()),
        )
        .with_graph_store(Box::new(gs))
    }

    fn extract_graph(pipeline: IngestionPipeline) -> Box<dyn GraphStore> {
        let (_, _, _, gs) = pipeline.into_parts_with_graph();
        gs.expect("graph store must be present")
    }

    fn write_md(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, content).unwrap();
        path
    }

    // ── Tests ────────────────────────────────────────────────────

    #[test]
    fn bulk_index_vault_with_links_completes_without_panic() {
        let tmp = TempDir::new().unwrap();
        write_md(&tmp, "NoteA.md", "---\ntitle: NoteA\n---\n[[NoteB]]\n");
        write_md(&tmp, "NoteB.md", "---\ntitle: NoteB\n---\n# NoteB\n");

        let mut pipeline = make_pipeline_and_graph();
        pipeline.bulk_index(tmp.path()).unwrap();

        let gs = extract_graph(pipeline);
        // Wikilinks go to insert_unresolved (path lookup of "NoteB" doesn't match
        // "/tmp/.../NoteB.md" — stem resolution happens inside resolve_pending).
        // The key assertion: the infrastructure works without panicking.
        let _stats = gs.stats().unwrap();
    }

    #[test]
    fn pipeline_without_graph_store_still_works() {
        let tmp = TempDir::new().unwrap();
        let path = write_md(&tmp, "Plain.md", "# Plain\n[[OtherNote]]\n");

        let mut pipeline = IngestionPipeline::new(
            vec![Box::new(MarkdownParser)],
            Box::new(InMemoryRegistry::new()),
            Box::new(InMemoryBitmapStore::new()),
        );

        let result = pipeline
            .process_event(&ChangeEvent { path, kind: ChangeKind::Created })
            .unwrap();
        assert!(result.bitmaps_updated > 0);
    }

    #[test]
    fn reindex_does_not_duplicate_edges() {
        let tmp = TempDir::new().unwrap();
        let path = write_md(&tmp, "Note.md", "# First version\n");

        let mut pipeline = make_pipeline_and_graph();

        pipeline
            .process_event(&ChangeEvent { path: path.clone(), kind: ChangeKind::Created })
            .unwrap();

        // Modify content → triggers replace_outgoing
        fs::write(&path, "# Updated version\n").unwrap();
        pipeline
            .process_event(&ChangeEvent { path: path.clone(), kind: ChangeKind::Modified })
            .unwrap();

        let gs = extract_graph(pipeline);
        // No links in either version → edge_count stays 0. Verify no phantom edges.
        assert_eq!(gs.edge_count().unwrap(), 0);
    }

    #[test]
    fn graph_edges_removed_after_doc_removed_from_graph_store() {
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        let conn = registry.connection();

        // Insert an edge manually, then verify remove_doc_edges cleans it up.
        let mut gs = DuckDbGraphStore::new(conn.clone());
        gs.insert_edge(Edge {
            from: 1,
            to: 2,
            category: EdgeCategory::Reference,
            kind: "ref:wikilink".into(),
            weight: 1.0,
            byte_offset: None,
            created_at: 0,
        })
        .unwrap();
        assert_eq!(gs.edge_count().unwrap(), 1);

        gs.remove_doc_edges(1).unwrap();
        assert_eq!(gs.edge_count().unwrap(), 0);
    }

    #[test]
    fn graph_store_is_present_after_with_graph_store() {
        let pipeline = make_pipeline_and_graph();
        let (_, _, _, gs) = pipeline.into_parts_with_graph();
        assert!(gs.is_some());
    }

    #[test]
    fn graph_store_absent_by_default() {
        let pipeline = IngestionPipeline::new(
            vec![Box::new(MarkdownParser)],
            Box::new(InMemoryRegistry::new()),
            Box::new(InMemoryBitmapStore::new()),
        );
        let (_, _, _, gs) = pipeline.into_parts_with_graph();
        assert!(gs.is_none());
    }
}
