//! Integration tests for DuckDbRegistry.

use std::path::{Path, PathBuf};

use biem_core::registry::{BitmapCatalogEntry, NewChunk, NewDoc};
use biem_core::{
    BitmapCategory, ChunkKind, ChunkMetadata, DocType, Registry, RegistryError, SourceType,
};
use biem_registry::duckdb::DuckDbRegistry;

fn open_registry() -> DuckDbRegistry {
    DuckDbRegistry::new(":memory:").unwrap()
}

fn make_doc(path: &str) -> NewDoc {
    NewDoc {
        file_path: PathBuf::from(path),
        source_type: SourceType::Obsidian,
        blake3_hash: [0u8; 32],
        auto_type: None,
    }
}

fn make_chunk(doc_id: u32) -> NewChunk {
    NewChunk {
        doc_id,
        kind: ChunkKind::Section,
        byte_start: 0,
        byte_end: 100,
        label: Some("Intro".into()),
        depth: 1,
        metadata: ChunkMetadata::default(),
    }
}

#[test]
fn insert_and_lookup_by_path() {
    let mut r = open_registry();
    let id = r.insert_doc(make_doc("a.md")).unwrap();
    let record = r.lookup_by_path(Path::new("a.md")).unwrap().unwrap();
    assert_eq!(record.doc_id, id);
}

#[test]
fn insert_and_lookup_by_id() {
    let mut r = open_registry();
    let id = r.insert_doc(make_doc("a.md")).unwrap();
    let record = r.lookup_by_id(id).unwrap().unwrap();
    assert_eq!(record.file_path, PathBuf::from("a.md"));
}

#[test]
fn monotonic_ids() {
    let mut r = open_registry();
    let id1 = r.insert_doc(make_doc("a.md")).unwrap();
    let id2 = r.insert_doc(make_doc("b.md")).unwrap();
    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
}

#[test]
fn duplicate_path_error() {
    let mut r = open_registry();
    r.insert_doc(make_doc("a.md")).unwrap();
    let result = r.insert_doc(make_doc("a.md"));
    assert!(matches!(result, Err(RegistryError::DuplicatePath(_))));
}

#[test]
fn bulk_insert() {
    let mut r = open_registry();
    let ids = r
        .bulk_insert_docs(vec![make_doc("a.md"), make_doc("b.md")])
        .unwrap();
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn update_doc_fields() {
    let mut r = open_registry();
    let id = r.insert_doc(make_doc("a.md")).unwrap();
    let new_hash = [1u8; 32];
    r.update_doc(id, new_hash, Some(DocType::Task)).unwrap();

    let record = r.lookup_by_id(id).unwrap().unwrap();
    assert_eq!(record.blake3_hash, new_hash);
    assert_eq!(record.auto_type, Some(DocType::Task));
}

#[test]
fn update_doc_not_found() {
    let mut r = open_registry();
    assert!(matches!(
        r.update_doc(999, [0u8; 32], None),
        Err(RegistryError::NotFound(999))
    ));
}

#[test]
fn update_path_rename() {
    let mut r = open_registry();
    let id = r.insert_doc(make_doc("old.md")).unwrap();
    r.update_path(id, PathBuf::from("new.md")).unwrap();

    assert!(r.lookup_by_path(Path::new("old.md")).unwrap().is_none());
    assert!(r.lookup_by_path(Path::new("new.md")).unwrap().is_some());
}

#[test]
fn replace_and_get_chunks() {
    let mut r = open_registry();
    let id = r.insert_doc(make_doc("a.md")).unwrap();

    let ids1 = r.replace_chunks(id, vec![make_chunk(id)]).unwrap();
    assert_eq!(ids1.len(), 1);

    let ids2 = r
        .replace_chunks(id, vec![make_chunk(id), make_chunk(id)])
        .unwrap();
    assert_eq!(ids2.len(), 2);

    let chunks = r.get_chunks(id).unwrap();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].chunk_id, ids2[0]);
}

#[test]
fn catalog_upsert_and_get() {
    let mut r = open_registry();
    r.upsert_catalog_entry(BitmapCatalogEntry {
        bitmap_key: "tag:work".into(),
        category: BitmapCategory::Tag,
        cardinality: 5,
        last_updated: 1000,
    })
    .unwrap();

    let entry = r.get_catalog_entry("tag:work").unwrap().unwrap();
    assert_eq!(entry.cardinality, 5);
}

#[test]
fn list_catalog_with_filter() {
    let mut r = open_registry();
    r.upsert_catalog_entry(BitmapCatalogEntry {
        bitmap_key: "tag:a".into(),
        category: BitmapCategory::Tag,
        cardinality: 1,
        last_updated: 0,
    })
    .unwrap();
    r.upsert_catalog_entry(BitmapCatalogEntry {
        bitmap_key: "folder:/x".into(),
        category: BitmapCategory::Folder,
        cardinality: 2,
        last_updated: 0,
    })
    .unwrap();

    let tags = r.list_catalog(Some(BitmapCategory::Tag)).unwrap();
    assert_eq!(tags.len(), 1);

    let all = r.list_catalog(None).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn global_state() {
    let mut r = open_registry();
    let state = r.get_global_state().unwrap();
    assert_eq!(state.next_doc_id, 1);
    assert_eq!(state.total_documents, 0);

    r.insert_doc(make_doc("a.md")).unwrap();
    let state = r.get_global_state().unwrap();
    assert_eq!(state.next_doc_id, 2);
    assert_eq!(state.total_documents, 1);
}

#[test]
fn lookup_by_ids() {
    let mut r = open_registry();
    let id1 = r.insert_doc(make_doc("a.md")).unwrap();
    let _id2 = r.insert_doc(make_doc("b.md")).unwrap();
    let id3 = r.insert_doc(make_doc("c.md")).unwrap();

    let results = r.lookup_by_ids(&[id1, id3, 999]).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn schema_idempotent() {
    // Opening twice on same :memory: is different DBs, but we can
    // test that init_schema doesn't fail on a fresh DB
    let _r1 = open_registry();
    let _r2 = open_registry();
}
