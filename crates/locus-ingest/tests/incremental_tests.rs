//! B2 incremental-ingest semantics: doc→keys bookkeeping, lazy deletes,
//! the __all_docs__ universe bitmap, and true skip on bulk re-index.

use std::fs;

use locus_bitmap::memory::InMemoryBitmapStore;
use locus_core::bitmap::ALL_DOCS_KEY;
use locus_core::types::{ChangeEvent, ChangeKind};
use locus_ingest::IngestionPipeline;
use locus_parser::markdown::MarkdownParser;
use locus_registry::memory::InMemoryRegistry;

fn make_pipeline() -> IngestionPipeline {
    IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        Box::new(InMemoryRegistry::new()),
        Box::new(InMemoryBitmapStore::new()),
    )
}

#[test]
fn created_records_doc_keys_and_universe_membership() {
    let mut pipeline = make_pipeline();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.md");
    fs::write(&file, "# Note\n#alpha text\n").unwrap();

    pipeline
        .process_event(&ChangeEvent { path: file, kind: ChangeKind::Created })
        .unwrap();

    let (_, registry, bitmap_store) = pipeline.into_parts();
    let keys = registry.get_doc_keys(1).unwrap();
    assert!(keys.contains(&"tag:alpha".to_string()), "keys recorded: {keys:?}");
    assert!(bitmap_store.get(ALL_DOCS_KEY).unwrap().contains(1));
    // Reserved key is hidden from the catalog listing
    assert!(!bitmap_store.list_keys(None).unwrap().iter().any(|k| k == ALL_DOCS_KEY));
}

#[test]
fn modified_diffs_via_recorded_keys() {
    let mut pipeline = make_pipeline();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.md");
    fs::write(&file, "# Note\n#alpha\n").unwrap();
    pipeline
        .process_event(&ChangeEvent { path: file.clone(), kind: ChangeKind::Created })
        .unwrap();

    fs::write(&file, "# Note\n#beta\n").unwrap();
    pipeline
        .process_event(&ChangeEvent { path: file, kind: ChangeKind::Modified })
        .unwrap();

    let (_, registry, bitmap_store) = pipeline.into_parts();
    assert!(!bitmap_store.get("tag:alpha").unwrap().contains(1), "stale tag removed");
    assert!(bitmap_store.get("tag:beta").unwrap().contains(1));
    let keys = registry.get_doc_keys(1).unwrap();
    assert!(keys.contains(&"tag:beta".to_string()));
    assert!(!keys.contains(&"tag:alpha".to_string()));
}

#[test]
fn delete_is_lazy_and_compact_scrubs_targeted_keys() {
    let mut pipeline = make_pipeline();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.md");
    fs::write(&file, "# Note\n#alpha\n").unwrap();
    pipeline
        .process_event(&ChangeEvent { path: file.clone(), kind: ChangeKind::Created })
        .unwrap();
    pipeline
        .process_event(&ChangeEvent { path: file, kind: ChangeKind::Deleted })
        .unwrap();

    // Lazy: the id stays in attribute bitmaps until compaction; the
    // tombstone is what hides it from query results.
    {
        let bs = pipeline.bitmap_store();
        assert!(bs.get("tag:alpha").unwrap().contains(1), "delete leaves bitmaps untouched");
        assert!(bs.get_tombstone().unwrap().contains(1));
    }

    let result = pipeline.compact().unwrap();
    assert_eq!(result.docs_removed, 1);
    assert!(result.bitmaps_cleaned > 0);

    let (_, registry, bitmap_store) = pipeline.into_parts();
    assert!(!bitmap_store.get("tag:alpha").unwrap().contains(1), "compact scrubbed the id");
    assert!(!bitmap_store.get(ALL_DOCS_KEY).unwrap().contains(1), "universe updated");
    assert!(registry.get_doc_keys(1).unwrap().is_empty(), "mapping removed with doc");
}

#[test]
fn bulk_reindex_skips_unchanged_and_preserves_bitmaps() {
    let mut pipeline = make_pipeline();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "# A\n#alpha\n").unwrap();
    fs::write(dir.path().join("b.md"), "# B\n#beta\n").unwrap();

    let r1 = pipeline.bulk_index(dir.path()).unwrap();
    assert_eq!(r1.docs_indexed, 2);

    let r2 = pipeline.bulk_index(dir.path()).unwrap();
    assert_eq!(r2.docs_skipped, 2, "second run is a pure skip");
    assert_eq!(r2.docs_indexed + r2.docs_updated, 0);
    assert_eq!(r2.bitmaps_created, 0, "no bitmap writes queued on a clean re-index");

    let bs = pipeline.bitmap_store();
    assert!(!bs.get("tag:alpha").unwrap().is_empty(), "bitmaps survive the skip");
    assert!(!bs.get("tag:beta").unwrap().is_empty());
    assert_eq!(bs.get(ALL_DOCS_KEY).unwrap().len(), 2);
}

#[test]
fn bulk_reindex_removes_stale_keys_with_no_other_members() {
    // Regression: the old rebuild-the-world bulk_put left stale doc ids in
    // bitmaps whose key vanished from every document in the run.
    let mut pipeline = make_pipeline();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.md");
    fs::write(&file, "# A\n#temporary\n").unwrap();
    pipeline.bulk_index(dir.path()).unwrap();
    assert!(pipeline.bitmap_store().get("tag:temporary").unwrap().contains(1));

    fs::write(&file, "# A\nno tags now\n").unwrap();
    let r = pipeline.bulk_index(dir.path()).unwrap();
    assert_eq!(r.docs_updated, 1);

    let bs = pipeline.bitmap_store();
    assert!(
        !bs.get("tag:temporary").unwrap().contains(1),
        "stale key scrubbed even though no other doc holds it"
    );
}

#[test]
fn bulk_index_tombstone_is_lazy() {
    let mut pipeline = make_pipeline();
    let dir = tempfile::tempdir().unwrap();
    let keep = dir.path().join("keep.md");
    let gone = dir.path().join("gone.md");
    fs::write(&keep, "# Keep\n#alpha\n").unwrap();
    fs::write(&gone, "# Gone\n#beta\n").unwrap();
    pipeline.bulk_index(dir.path()).unwrap();

    fs::remove_file(&gone).unwrap();
    let r = pipeline.bulk_index(dir.path()).unwrap();
    assert_eq!(r.docs_tombstoned, 1);

    // Walk order (and thus doc-id assignment) is not deterministic --
    // identify the tombstoned doc from the tombstone bitmap itself.
    let bs = pipeline.bitmap_store();
    let tombstones = bs.get_tombstone().unwrap();
    assert_eq!(tombstones.len(), 1);
    let gone_id = tombstones.iter().next().unwrap();
    assert!(
        bs.get("tag:beta").unwrap().contains(gone_id),
        "scrub deferred to compact"
    );
}
