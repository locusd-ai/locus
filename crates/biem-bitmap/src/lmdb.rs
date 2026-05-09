//! LMDB-backed BitmapStore implementation using heed.

use std::path::Path;

use biem_core::{BitmapError, BitmapKey, BitmapStore, DocId};
use heed::types::{Bytes, Str};
use heed::{Database, Env, EnvOpenOptions};
use roaring::RoaringBitmap;
use tracing::instrument;

const TOMBSTONE_KEY: &str = "__tombstone__";

/// LMDB-backed [`BitmapStore`] using the `heed` crate.
///
/// Each bitmap is stored as a serialized `RoaringBitmap` (portable format),
/// keyed by its string `BitmapKey`.
pub struct LmdbBitmapStore {
    env: Env,
    db: Database<Str, Bytes>,
}

impl LmdbBitmapStore {
    /// Open or create an LMDB bitmap store at the given directory path.
    ///
    /// The directory will be created if it doesn't exist.
    pub fn new(path: &Path) -> Result<Self, BitmapError> {
        std::fs::create_dir_all(path).map_err(|e| BitmapError::Storage(e.to_string()))?;

        let env = unsafe {
            EnvOpenOptions::new()
                .map_size(1024 * 1024 * 1024) // 1 GB
                .max_dbs(1)
                .open(path)
                .map_err(|e| BitmapError::Storage(e.to_string()))?
        };

        let mut wtxn = env
            .write_txn()
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        let db = env
            .create_database(&mut wtxn, Some("bitmaps"))
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        wtxn.commit()
            .map_err(|e| BitmapError::Storage(e.to_string()))?;

        Ok(Self { env, db })
    }

    fn serialize(bitmap: &RoaringBitmap) -> Result<Vec<u8>, BitmapError> {
        let mut buf = Vec::with_capacity(bitmap.serialized_size());
        bitmap
            .serialize_into(&mut buf)
            .map_err(|e| BitmapError::Serialization(e.to_string()))?;
        Ok(buf)
    }

    fn deserialize(bytes: &[u8]) -> Result<RoaringBitmap, BitmapError> {
        RoaringBitmap::deserialize_from(bytes)
            .map_err(|e| BitmapError::Serialization(e.to_string()))
    }
}

impl BitmapStore for LmdbBitmapStore {
    #[instrument(skip(self))]
    fn get(&self, key: &str) -> Result<RoaringBitmap, BitmapError> {
        let rtxn = self
            .env
            .read_txn()
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        match self
            .db
            .get(&rtxn, key)
            .map_err(|e| BitmapError::Storage(e.to_string()))?
        {
            Some(bytes) => Self::deserialize(bytes),
            None => Ok(RoaringBitmap::new()),
        }
    }

    #[instrument(skip(self, bitmap))]
    fn put(&mut self, key: &str, bitmap: &RoaringBitmap) -> Result<(), BitmapError> {
        let buf = Self::serialize(bitmap)?;
        let mut wtxn = self
            .env
            .write_txn()
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        self.db
            .put(&mut wtxn, key, &buf)
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        wtxn.commit()
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        Ok(())
    }

    #[instrument(skip(self))]
    fn insert_id(&mut self, key: &str, doc_id: DocId) -> Result<(), BitmapError> {
        let mut bitmap = self.get(key)?;
        bitmap.insert(doc_id);
        self.put(key, &bitmap)
    }

    #[instrument(skip(self))]
    fn remove_id(&mut self, key: &str, doc_id: DocId) -> Result<(), BitmapError> {
        let mut bitmap = self.get(key)?;
        if bitmap.remove(doc_id) {
            self.put(key, &bitmap)?;
        }
        Ok(())
    }

    #[instrument(skip(self))]
    fn delete(&mut self, key: &str) -> Result<(), BitmapError> {
        let mut wtxn = self
            .env
            .write_txn()
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        self.db
            .delete(&mut wtxn, key)
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        wtxn.commit()
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        Ok(())
    }

    fn exists(&self, key: &str) -> bool {
        let Ok(rtxn) = self.env.read_txn() else {
            return false;
        };
        self.db.get(&rtxn, key).ok().flatten().is_some()
    }

    #[instrument(skip(self, entries))]
    fn bulk_put(
        &mut self,
        entries: Vec<(BitmapKey, RoaringBitmap)>,
    ) -> Result<(), BitmapError> {
        let mut wtxn = self
            .env
            .write_txn()
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        for (key, bitmap) in &entries {
            let buf = Self::serialize(bitmap)?;
            self.db
                .put(&mut wtxn, key, &buf)
                .map_err(|e| BitmapError::Storage(e.to_string()))?;
        }
        wtxn.commit()
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        Ok(())
    }

    #[instrument(skip(self))]
    fn tombstone(&mut self, doc_id: DocId) -> Result<(), BitmapError> {
        self.insert_id(TOMBSTONE_KEY, doc_id)
    }

    fn get_tombstone(&self) -> Result<RoaringBitmap, BitmapError> {
        self.get(TOMBSTONE_KEY)
    }

    #[instrument(skip(self))]
    fn list_keys(&self, prefix: Option<&str>) -> Result<Vec<BitmapKey>, BitmapError> {
        let rtxn = self
            .env
            .read_txn()
            .map_err(|e| BitmapError::Storage(e.to_string()))?;
        let iter = self
            .db
            .iter(&rtxn)
            .map_err(|e| BitmapError::Storage(e.to_string()))?;

        let mut keys = Vec::new();
        for result in iter {
            let (key, _) = result.map_err(|e| BitmapError::Storage(e.to_string()))?;
            if key == TOMBSTONE_KEY {
                continue;
            }
            match prefix {
                Some(p) if !key.starts_with(p) => continue,
                _ => keys.push(key.to_string()),
            }
        }
        Ok(keys)
    }

    #[instrument(skip(self))]
    fn cardinality(&self, key: &str) -> Result<u32, BitmapError> {
        let bitmap = self.get(key)?;
        Ok(bitmap.len() as u32)
    }
}
