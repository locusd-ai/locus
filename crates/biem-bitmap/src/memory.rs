//! In-memory BitmapStore implementation for testing.

use std::collections::HashMap;

use biem_core::{BitmapError, BitmapKey, BitmapStore, DocId};
use roaring::RoaringBitmap;

const TOMBSTONE_KEY: &str = "__tombstone__";

/// In-memory implementation of [`BitmapStore`] backed by a `HashMap`.
/// Intended for unit tests — not for production use.
pub struct InMemoryBitmapStore {
    bitmaps: HashMap<String, RoaringBitmap>,
}

impl InMemoryBitmapStore {
    pub fn new() -> Self {
        Self {
            bitmaps: HashMap::new(),
        }
    }
}

impl Default for InMemoryBitmapStore {
    fn default() -> Self {
        Self::new()
    }
}

impl BitmapStore for InMemoryBitmapStore {
    fn get(&self, key: &str) -> Result<RoaringBitmap, BitmapError> {
        Ok(self.bitmaps.get(key).cloned().unwrap_or_default())
    }

    fn put(&mut self, key: &str, bitmap: &RoaringBitmap) -> Result<(), BitmapError> {
        self.bitmaps.insert(key.to_string(), bitmap.clone());
        Ok(())
    }

    fn insert_id(&mut self, key: &str, doc_id: DocId) -> Result<(), BitmapError> {
        self.bitmaps
            .entry(key.to_string())
            .or_default()
            .insert(doc_id);
        Ok(())
    }

    fn remove_id(&mut self, key: &str, doc_id: DocId) -> Result<(), BitmapError> {
        if let Some(bm) = self.bitmaps.get_mut(key) {
            bm.remove(doc_id);
        }
        Ok(())
    }

    fn delete(&mut self, key: &str) -> Result<(), BitmapError> {
        self.bitmaps.remove(key);
        Ok(())
    }

    fn exists(&self, key: &str) -> bool {
        self.bitmaps.contains_key(key)
    }

    fn bulk_put(
        &mut self,
        entries: Vec<(BitmapKey, RoaringBitmap)>,
    ) -> Result<(), BitmapError> {
        for (key, bitmap) in entries {
            self.bitmaps.insert(key, bitmap);
        }
        Ok(())
    }

    fn tombstone(&mut self, doc_id: DocId) -> Result<(), BitmapError> {
        self.bitmaps
            .entry(TOMBSTONE_KEY.to_string())
            .or_default()
            .insert(doc_id);
        Ok(())
    }

    fn get_tombstone(&self) -> Result<RoaringBitmap, BitmapError> {
        Ok(self.bitmaps.get(TOMBSTONE_KEY).cloned().unwrap_or_default())
    }

    fn list_keys(&self, prefix: Option<&str>) -> Result<Vec<BitmapKey>, BitmapError> {
        let keys = self
            .bitmaps
            .keys()
            .filter(|k| *k != TOMBSTONE_KEY)
            .filter(|k| match prefix {
                Some(p) => k.starts_with(p),
                None => true,
            })
            .cloned()
            .collect();
        Ok(keys)
    }

    fn cardinality(&self, key: &str) -> Result<u32, BitmapError> {
        Ok(self
            .bitmaps
            .get(key)
            .map(|bm| bm.len() as u32)
            .unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> InMemoryBitmapStore {
        InMemoryBitmapStore::new()
    }

    #[test]
    fn put_get_round_trip() {
        let mut s = store();
        let mut bm = RoaringBitmap::new();
        bm.insert(1);
        bm.insert(42);
        s.put("tag:work", &bm).unwrap();

        let got = s.get("tag:work").unwrap();
        assert_eq!(got, bm);
    }

    #[test]
    fn get_missing_key_returns_empty() {
        let s = store();
        let got = s.get("nonexistent").unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn insert_id_into_existing_bitmap() {
        let mut s = store();
        let mut bm = RoaringBitmap::new();
        bm.insert(1);
        s.put("tag:work", &bm).unwrap();

        s.insert_id("tag:work", 2).unwrap();
        let got = s.get("tag:work").unwrap();
        assert!(got.contains(1));
        assert!(got.contains(2));
    }

    #[test]
    fn insert_id_creates_bitmap_if_missing() {
        let mut s = store();
        s.insert_id("tag:new", 5).unwrap();
        let got = s.get("tag:new").unwrap();
        assert!(got.contains(5));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn remove_id_removes_doc() {
        let mut s = store();
        s.insert_id("tag:work", 1).unwrap();
        s.insert_id("tag:work", 2).unwrap();
        s.remove_id("tag:work", 1).unwrap();

        let got = s.get("tag:work").unwrap();
        assert!(!got.contains(1));
        assert!(got.contains(2));
    }

    #[test]
    fn remove_id_on_missing_key_is_noop() {
        let mut s = store();
        s.remove_id("nonexistent", 1).unwrap();
    }

    #[test]
    fn delete_removes_key() {
        let mut s = store();
        s.insert_id("tag:work", 1).unwrap();
        assert!(s.exists("tag:work"));

        s.delete("tag:work").unwrap();
        assert!(!s.exists("tag:work"));
        assert!(s.get("tag:work").unwrap().is_empty());
    }

    #[test]
    fn exists_returns_correct_value() {
        let mut s = store();
        assert!(!s.exists("tag:work"));
        s.insert_id("tag:work", 1).unwrap();
        assert!(s.exists("tag:work"));
    }

    #[test]
    fn bulk_put_writes_multiple() {
        let mut s = store();
        let mut bm1 = RoaringBitmap::new();
        bm1.insert(1);
        let mut bm2 = RoaringBitmap::new();
        bm2.insert(2);

        s.bulk_put(vec![
            ("tag:a".to_string(), bm1.clone()),
            ("tag:b".to_string(), bm2.clone()),
        ])
        .unwrap();

        assert_eq!(s.get("tag:a").unwrap(), bm1);
        assert_eq!(s.get("tag:b").unwrap(), bm2);
    }

    #[test]
    fn tombstone_round_trip() {
        let mut s = store();
        assert!(s.get_tombstone().unwrap().is_empty());

        s.tombstone(10).unwrap();
        s.tombstone(20).unwrap();

        let ts = s.get_tombstone().unwrap();
        assert!(ts.contains(10));
        assert!(ts.contains(20));
        assert_eq!(ts.len(), 2);
    }

    #[test]
    fn list_keys_excludes_tombstone() {
        let mut s = store();
        s.insert_id("tag:a", 1).unwrap();
        s.tombstone(1).unwrap();

        let keys = s.list_keys(None).unwrap();
        assert_eq!(keys, vec!["tag:a".to_string()]);
    }

    #[test]
    fn list_keys_with_prefix() {
        let mut s = store();
        s.insert_id("tag:a", 1).unwrap();
        s.insert_id("tag:b", 2).unwrap();
        s.insert_id("folder:/projects", 3).unwrap();

        let mut keys = s.list_keys(Some("tag:")).unwrap();
        keys.sort();
        assert_eq!(keys, vec!["tag:a".to_string(), "tag:b".to_string()]);
    }

    #[test]
    fn cardinality_returns_count() {
        let mut s = store();
        assert_eq!(s.cardinality("missing").unwrap(), 0);

        s.insert_id("tag:a", 1).unwrap();
        s.insert_id("tag:a", 2).unwrap();
        s.insert_id("tag:a", 3).unwrap();
        assert_eq!(s.cardinality("tag:a").unwrap(), 3);
    }
}
