use std::time::Instant;

use roaring::RoaringBitmap;
use tracing::instrument;

use biem_core::bitmap::BitmapStore;
use biem_core::query::{
    ChunkPointer, Filter, MatchPointer, QueryEngine, QueryError, QueryRequest,
    QueryResult,
};
use biem_core::registry::{BitmapCatalogEntry, Registry};
use biem_core::types::BitmapCategory;

/// Concrete query engine backed by bitmap store + registry.
pub struct BitmapQueryEngine {
    bitmap_store: Box<dyn BitmapStore>,
    registry: Box<dyn Registry>,
}

impl BitmapQueryEngine {
    pub fn new(bitmap_store: Box<dyn BitmapStore>, registry: Box<dyn Registry>) -> Self {
        Self {
            bitmap_store,
            registry,
        }
    }

    /// Recursively resolve a filter expression to a RoaringBitmap of matching DocIds.
    fn resolve_filter(&self, filter: &Filter) -> Result<RoaringBitmap, QueryError> {
        match filter {
            Filter::Key(key) => {
                let bm = self.bitmap_store.get(key)?;
                // Subtract tombstones
                let tombstones = self.bitmap_store.get_tombstone()?;
                Ok(bm - tombstones)
            }
            Filter::Not(inner) => {
                // NOT is relative to the universe of all non-tombstoned docs.
                // Universe = union of all bitmaps minus tombstones.
                // For efficiency, we use all keys to build the universe.
                let all_keys = self.bitmap_store.list_keys(None)?;
                let mut universe = RoaringBitmap::new();
                for key in &all_keys {
                    universe |= self.bitmap_store.get(key)?;
                }
                let tombstones = self.bitmap_store.get_tombstone()?;
                universe -= tombstones;

                let resolved = self.resolve_filter(inner)?;
                Ok(universe - resolved)
            }
            Filter::And(children) => {
                let mut result: Option<RoaringBitmap> = None;
                for child in children {
                    let bm = self.resolve_filter(child)?;
                    result = Some(match result {
                        Some(acc) => acc & bm,
                        None => bm,
                    });
                }
                Ok(result.unwrap_or_default())
            }
            Filter::Or(children) => {
                let mut result = RoaringBitmap::new();
                for child in children {
                    result |= self.resolve_filter(child)?;
                }
                Ok(result)
            }
        }
    }

    /// Build a MatchPointer for a given doc_id.
    fn hydrate(&self, doc_id: u32, matched_filters: Vec<String>) -> Result<Option<MatchPointer>, QueryError> {
        let doc = self.registry.lookup_by_id(doc_id)?;
        let doc = match doc {
            Some(d) => d,
            None => return Ok(None),
        };

        let chunks = self.registry.get_chunks(doc_id)?;
        let chunk_ptrs: Vec<ChunkPointer> = chunks
            .into_iter()
            .map(|c| ChunkPointer {
                chunk_id: c.chunk_id,
                kind: format!("{:?}", c.kind),
                byte_start: c.byte_start,
                byte_end: c.byte_end,
                label: c.label,
            })
            .collect();

        let auto_type = doc.auto_type.map(|t| format!("{:?}", t));
        let source_type = format!("{:?}", doc.source_type);

        Ok(Some(MatchPointer {
            doc_id,
            file_path: doc.file_path,
            source_type,
            chunks: chunk_ptrs,
            matched_filters,
            auto_type,
            score: None,
            last_modified: doc.last_indexed,
        }))
    }

    /// Collect the leaf bitmap keys from a filter for matched_filters reporting.
    fn collect_leaf_keys(filter: &Filter) -> Vec<String> {
        match filter {
            Filter::Key(k) => vec![k.clone()],
            Filter::Not(inner) => Self::collect_leaf_keys(inner),
            Filter::And(children) | Filter::Or(children) => {
                children.iter().flat_map(Self::collect_leaf_keys).collect()
            }
        }
    }
}

impl QueryEngine for BitmapQueryEngine {
    #[instrument(skip(self))]
    fn query(&self, request: QueryRequest) -> Result<QueryResult, QueryError> {
        let start = Instant::now();

        let matching = self.resolve_filter(&request.filter)?;
        let total_matching = matching.len() as u32;

        let leaf_keys = Self::collect_leaf_keys(&request.filter);

        // Apply offset and limit
        let offset = request.offset.unwrap_or(0) as usize;
        let limit = request.limit.unwrap_or(u32::MAX) as usize;

        let doc_ids: Vec<u32> = matching
            .iter()
            .skip(offset)
            .take(limit)
            .collect();

        let mut matches = Vec::with_capacity(doc_ids.len());
        for doc_id in doc_ids {
            if let Some(ptr) = self.hydrate(doc_id, leaf_keys.clone())? {
                matches.push(ptr);
            }
        }

        let query_time_us = start.elapsed().as_micros() as u64;

        Ok(QueryResult {
            matches,
            total_matching,
            query_time_us,
        })
    }

    fn list_filters(
        &self,
        category: Option<BitmapCategory>,
    ) -> Result<Vec<BitmapCatalogEntry>, QueryError> {
        Ok(self.registry.list_catalog(category)?)
    }
}
