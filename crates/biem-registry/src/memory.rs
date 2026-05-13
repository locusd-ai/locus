//! In-memory Registry implementation for testing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use biem_core::{
    BitmapCategory, BitmapKey, ChunkId, DocId, NoteType, Registry, RegistryError, Timestamp,
};
use biem_core::registry::{
    BitmapCatalogEntry, ChunkRecord, DocRecord, GlobalState, NewChunk, NewDoc,
};

/// In-memory implementation of [`Registry`] backed by `HashMap`/`Vec`.
/// Intended for unit tests — not for production use.
pub struct InMemoryRegistry {
    next_doc_id: DocId,
    next_chunk_id: ChunkId,
    docs: HashMap<DocId, DocRecord>,
    path_index: HashMap<PathBuf, DocId>,
    chunks: HashMap<DocId, Vec<ChunkRecord>>,
    catalog: HashMap<BitmapKey, BitmapCatalogEntry>,
}

impl InMemoryRegistry {
    pub fn new() -> Self {
        Self {
            next_doc_id: 1,
            next_chunk_id: 1,
            docs: HashMap::new(),
            path_index: HashMap::new(),
            chunks: HashMap::new(),
            catalog: HashMap::new(),
        }
    }

    fn now() -> Timestamp {
        // Simple wall-clock for tests; not critical
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as Timestamp
    }
}

impl Default for InMemoryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry for InMemoryRegistry {
    fn insert_doc(&mut self, doc: NewDoc) -> Result<DocId, RegistryError> {
        if self.path_index.contains_key(&doc.file_path) {
            return Err(RegistryError::DuplicatePath(doc.file_path));
        }
        let id = self.next_doc_id;
        self.next_doc_id += 1;

        let record = DocRecord {
            doc_id: id,
            file_path: doc.file_path.clone(),
            source_type: doc.source_type,
            blake3_hash: doc.blake3_hash,
            last_indexed: Self::now(),
            auto_type: doc.auto_type,
        };
        self.path_index.insert(doc.file_path, id);
        self.docs.insert(id, record);
        Ok(id)
    }

    fn bulk_insert_docs(&mut self, docs: Vec<NewDoc>) -> Result<Vec<DocId>, RegistryError> {
        // Check for duplicates before inserting any
        for doc in &docs {
            if self.path_index.contains_key(&doc.file_path) {
                return Err(RegistryError::DuplicatePath(doc.file_path.clone()));
            }
        }
        let mut ids = Vec::with_capacity(docs.len());
        for doc in docs {
            ids.push(self.insert_doc(doc)?);
        }
        Ok(ids)
    }

    fn lookup_by_path(&self, path: &Path) -> Result<Option<DocRecord>, RegistryError> {
        let record = self
            .path_index
            .get(path)
            .and_then(|id| self.docs.get(id));
        Ok(record.cloned())
    }

    fn lookup_by_id(&self, doc_id: DocId) -> Result<Option<DocRecord>, RegistryError> {
        Ok(self.docs.get(&doc_id).cloned())
    }

    fn lookup_by_ids(&self, doc_ids: &[DocId]) -> Result<Vec<DocRecord>, RegistryError> {
        Ok(doc_ids
            .iter()
            .filter_map(|id| self.docs.get(id).cloned())
            .collect())
    }

    fn update_doc(
        &mut self,
        doc_id: DocId,
        hash: [u8; 32],
        auto_type: Option<NoteType>,
    ) -> Result<(), RegistryError> {
        let record = self
            .docs
            .get_mut(&doc_id)
            .ok_or(RegistryError::NotFound(doc_id))?;
        record.blake3_hash = hash;
        record.last_indexed = Self::now();
        record.auto_type = auto_type;
        Ok(())
    }

    fn update_path(&mut self, doc_id: DocId, new_path: PathBuf) -> Result<(), RegistryError> {
        let record = self
            .docs
            .get_mut(&doc_id)
            .ok_or(RegistryError::NotFound(doc_id))?;
        // Remove old path index entry
        self.path_index.remove(&record.file_path);
        record.file_path = new_path.clone();
        self.path_index.insert(new_path, doc_id);
        Ok(())
    }

    fn delete_doc(&mut self, doc_id: DocId) -> Result<(), RegistryError> {
        let record = self
            .docs
            .remove(&doc_id)
            .ok_or(RegistryError::NotFound(doc_id))?;
        self.path_index.remove(&record.file_path);
        self.chunks.remove(&doc_id);
        Ok(())
    }

    fn replace_chunks(
        &mut self,
        doc_id: DocId,
        chunks: Vec<NewChunk>,
    ) -> Result<Vec<ChunkId>, RegistryError> {
        if !self.docs.contains_key(&doc_id) {
            return Err(RegistryError::NotFound(doc_id));
        }
        // Remove old chunks
        self.chunks.remove(&doc_id);

        let mut ids = Vec::with_capacity(chunks.len());
        let mut records = Vec::with_capacity(chunks.len());

        for c in chunks {
            let id = self.next_chunk_id;
            self.next_chunk_id += 1;
            ids.push(id);
            records.push(ChunkRecord {
                chunk_id: id,
                doc_id,
                kind: c.kind,
                byte_start: c.byte_start,
                byte_end: c.byte_end,
                label: c.label,
                depth: c.depth,
                metadata: c.metadata,
            });
        }
        self.chunks.insert(doc_id, records);
        Ok(ids)
    }

    fn get_chunks(&self, doc_id: DocId) -> Result<Vec<ChunkRecord>, RegistryError> {
        Ok(self.chunks.get(&doc_id).cloned().unwrap_or_default())
    }

    fn upsert_catalog_entry(&mut self, entry: BitmapCatalogEntry) -> Result<(), RegistryError> {
        self.catalog.insert(entry.bitmap_key.clone(), entry);
        Ok(())
    }

    fn bulk_upsert_catalog(
        &mut self,
        entries: Vec<BitmapCatalogEntry>,
    ) -> Result<(), RegistryError> {
        for entry in entries {
            self.catalog.insert(entry.bitmap_key.clone(), entry);
        }
        Ok(())
    }

    fn get_catalog_entry(&self, key: &str) -> Result<Option<BitmapCatalogEntry>, RegistryError> {
        Ok(self.catalog.get(key).cloned())
    }

    fn list_catalog(
        &self,
        category: Option<BitmapCategory>,
    ) -> Result<Vec<BitmapCatalogEntry>, RegistryError> {
        Ok(self
            .catalog
            .values()
            .filter(|e| match &category {
                Some(cat) => e.category == *cat,
                None => true,
            })
            .cloned()
            .collect())
    }

    fn list_all_docs(&self) -> Result<Vec<DocRecord>, RegistryError> {
        Ok(self.docs.values().cloned().collect())
    }

    fn get_global_state(&self) -> Result<GlobalState, RegistryError> {
        Ok(GlobalState {
            next_doc_id: self.next_doc_id,
            next_chunk_id: self.next_chunk_id,
            total_documents: self.docs.len() as u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use biem_core::{ChunkKind, ChunkMetadata, SourceType};

    fn make_doc(path: &str) -> NewDoc {
        NewDoc {
            file_path: PathBuf::from(path),
            source_type: SourceType::Obsidian,
            blake3_hash: [0u8; 32],
            auto_type: None,
        }
    }

    fn make_chunk(doc_id: DocId) -> NewChunk {
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
    fn insert_doc_assigns_monotonic_ids() {
        let mut r = InMemoryRegistry::new();
        let id1 = r.insert_doc(make_doc("a.md")).unwrap();
        let id2 = r.insert_doc(make_doc("b.md")).unwrap();
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }

    #[test]
    fn lookup_by_path_round_trip() {
        let mut r = InMemoryRegistry::new();
        let id = r.insert_doc(make_doc("notes/a.md")).unwrap();
        let record = r.lookup_by_path(Path::new("notes/a.md")).unwrap().unwrap();
        assert_eq!(record.doc_id, id);
        assert_eq!(record.file_path, PathBuf::from("notes/a.md"));
    }

    #[test]
    fn lookup_by_id_round_trip() {
        let mut r = InMemoryRegistry::new();
        let id = r.insert_doc(make_doc("a.md")).unwrap();
        let record = r.lookup_by_id(id).unwrap().unwrap();
        assert_eq!(record.doc_id, id);
    }

    #[test]
    fn lookup_by_id_missing_returns_none() {
        let r = InMemoryRegistry::new();
        assert!(r.lookup_by_id(999).unwrap().is_none());
    }

    #[test]
    fn bulk_insert_docs_returns_ids() {
        let mut r = InMemoryRegistry::new();
        let ids = r
            .bulk_insert_docs(vec![make_doc("a.md"), make_doc("b.md"), make_doc("c.md")])
            .unwrap();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn update_doc_modifies_fields() {
        let mut r = InMemoryRegistry::new();
        let id = r.insert_doc(make_doc("a.md")).unwrap();
        let new_hash = [1u8; 32];
        r.update_doc(id, new_hash, Some(NoteType::Task)).unwrap();

        let record = r.lookup_by_id(id).unwrap().unwrap();
        assert_eq!(record.blake3_hash, new_hash);
        assert_eq!(record.auto_type, Some(NoteType::Task));
    }

    #[test]
    fn update_doc_not_found() {
        let mut r = InMemoryRegistry::new();
        let result = r.update_doc(999, [0u8; 32], None);
        assert!(matches!(result, Err(RegistryError::NotFound(999))));
    }

    #[test]
    fn update_path_handles_rename() {
        let mut r = InMemoryRegistry::new();
        let id = r.insert_doc(make_doc("old.md")).unwrap();
        r.update_path(id, PathBuf::from("new.md")).unwrap();

        assert!(r.lookup_by_path(Path::new("old.md")).unwrap().is_none());
        let record = r.lookup_by_path(Path::new("new.md")).unwrap().unwrap();
        assert_eq!(record.doc_id, id);
    }

    #[test]
    fn replace_chunks_deletes_old_inserts_new() {
        let mut r = InMemoryRegistry::new();
        let id = r.insert_doc(make_doc("a.md")).unwrap();

        let ids1 = r.replace_chunks(id, vec![make_chunk(id)]).unwrap();
        assert_eq!(ids1.len(), 1);

        let ids2 = r
            .replace_chunks(id, vec![make_chunk(id), make_chunk(id)])
            .unwrap();
        assert_eq!(ids2.len(), 2);
        // Old chunks replaced
        let chunks = r.get_chunks(id).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chunk_id, ids2[0]);
    }

    #[test]
    fn get_chunks_empty_for_no_chunks() {
        let mut r = InMemoryRegistry::new();
        let id = r.insert_doc(make_doc("a.md")).unwrap();
        assert!(r.get_chunks(id).unwrap().is_empty());
    }

    #[test]
    fn replace_chunks_not_found() {
        let mut r = InMemoryRegistry::new();
        let result = r.replace_chunks(999, vec![]);
        assert!(matches!(result, Err(RegistryError::NotFound(999))));
    }

    #[test]
    fn catalog_upsert_and_get() {
        let mut r = InMemoryRegistry::new();
        let entry = BitmapCatalogEntry {
            bitmap_key: "tag:work".into(),
            category: BitmapCategory::Tag,
            cardinality: 5,
            last_updated: 1000,
        };
        r.upsert_catalog_entry(entry.clone()).unwrap();
        let got = r.get_catalog_entry("tag:work").unwrap().unwrap();
        assert_eq!(got.cardinality, 5);
    }

    #[test]
    fn catalog_get_missing_returns_none() {
        let r = InMemoryRegistry::new();
        assert!(r.get_catalog_entry("nope").unwrap().is_none());
    }

    #[test]
    fn list_catalog_with_category_filter() {
        let mut r = InMemoryRegistry::new();
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
        assert_eq!(tags[0].bitmap_key, "tag:a");

        let all = r.list_catalog(None).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn get_global_state_reflects_counters() {
        let mut r = InMemoryRegistry::new();
        let state = r.get_global_state().unwrap();
        assert_eq!(state.next_doc_id, 1);
        assert_eq!(state.total_documents, 0);

        r.insert_doc(make_doc("a.md")).unwrap();
        r.insert_doc(make_doc("b.md")).unwrap();
        let state = r.get_global_state().unwrap();
        assert_eq!(state.next_doc_id, 3);
        assert_eq!(state.total_documents, 2);
    }

    #[test]
    fn duplicate_path_error() {
        let mut r = InMemoryRegistry::new();
        r.insert_doc(make_doc("a.md")).unwrap();
        let result = r.insert_doc(make_doc("a.md"));
        assert!(matches!(result, Err(RegistryError::DuplicatePath(_))));
    }

    #[test]
    fn bulk_insert_duplicate_rolls_back_none() {
        let mut r = InMemoryRegistry::new();
        r.insert_doc(make_doc("a.md")).unwrap();
        // Second doc collides — whole bulk should fail
        let result = r.bulk_insert_docs(vec![make_doc("b.md"), make_doc("a.md")]);
        assert!(result.is_err());
    }

    #[test]
    fn lookup_by_ids_returns_matching() {
        let mut r = InMemoryRegistry::new();
        let id1 = r.insert_doc(make_doc("a.md")).unwrap();
        let _id2 = r.insert_doc(make_doc("b.md")).unwrap();
        let id3 = r.insert_doc(make_doc("c.md")).unwrap();

        let results = r.lookup_by_ids(&[id1, id3, 999]).unwrap();
        assert_eq!(results.len(), 2);
    }
}
