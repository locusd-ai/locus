use std::path::Path;
use std::time::Instant;

use roaring::RoaringBitmap;
use tracing::instrument;

use biem_core::bitmap::BitmapStore;
use biem_core::query::{
    ChunkPointer, Filter, FilterEntry, IndexStatus, InspectResult as QueryInspectResult,
    MatchPointer, QueryEngine, QueryError, QueryRequest, QueryResult,
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
                // Sort children by estimated cardinality (smallest first) for faster intersection.
                let mut indexed: Vec<(usize, u64)> = children
                    .iter()
                    .enumerate()
                    .map(|(i, child)| {
                        let card = self.estimate_cardinality(child);
                        (i, card)
                    })
                    .collect();
                indexed.sort_by_key(|&(_, card)| card);

                let mut result: Option<RoaringBitmap> = None;
                for (i, _) in indexed {
                    let bm = self.resolve_filter(&children[i])?;
                    result = Some(match result {
                        Some(acc) => {
                            let intersection = acc & bm;
                            if intersection.is_empty() {
                                return Ok(RoaringBitmap::new()); // short-circuit
                            }
                            intersection
                        }
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

    /// Estimate cardinality of a filter for sorting purposes.
    /// For Key filters, uses bitmap_store.cardinality(). For compound filters, uses a rough estimate.
    fn estimate_cardinality(&self, filter: &Filter) -> u64 {
        match filter {
            Filter::Key(key) => self.bitmap_store.cardinality(key).unwrap_or(0) as u64,
            Filter::Not(_) => u64::MAX, // NOT is expensive, process last
            Filter::And(children) => {
                // Estimate as min of children
                children.iter().map(|c| self.estimate_cardinality(c)).min().unwrap_or(0)
            }
            Filter::Or(children) => {
                // Estimate as sum of children (upper bound)
                children.iter().map(|c| self.estimate_cardinality(c)).sum()
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

    fn inspect(&self, path: &Path) -> Result<Option<QueryInspectResult>, QueryError> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let doc = self.registry.lookup_by_path(&canonical)?
            .or(self.registry.lookup_by_path(path).ok().flatten());

        let doc = match doc {
            Some(d) => d,
            None => return Ok(None),
        };

        let chunks = self.registry.get_chunks(doc.doc_id)?;
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

        // Find bitmap keys containing this doc
        let all_keys = self.bitmap_store.list_keys(None)?;
        let mut doc_keys: Vec<String> = all_keys
            .into_iter()
            .filter(|k| self.bitmap_store.get(k).map(|bm| bm.contains(doc.doc_id)).unwrap_or(false))
            .collect();
        doc_keys.sort();

        let hash = doc.blake3_hash.iter().map(|b| format!("{b:02x}")).collect::<String>();

        Ok(Some(QueryInspectResult {
            doc_id: doc.doc_id,
            file_path: doc.file_path,
            source_type: format!("{:?}", doc.source_type),
            auto_type: doc.auto_type.map(|t| format!("{:?}", t)),
            blake3_hash: hash,
            last_indexed: doc.last_indexed,
            chunks: chunk_ptrs,
            bitmap_keys: doc_keys,
        }))
    }

    fn status(&self) -> Result<IndexStatus, QueryError> {
        let state = self.registry.get_global_state()?;
        let tombstones = self.bitmap_store.get_tombstone()?;
        let bitmap_count = self.bitmap_store.list_keys(None)?.len();

        Ok(IndexStatus {
            total_documents: state.total_documents,
            total_bitmaps: bitmap_count,
            tombstoned: tombstones.len(),
            next_doc_id: state.next_doc_id,
            next_chunk_id: state.next_chunk_id,
        })
    }

    fn list_filter_keys(&self, category: Option<&str>) -> Result<Vec<FilterEntry>, QueryError> {
        let prefix = category.map(|c| format!("{c}:"));
        let keys = self.bitmap_store.list_keys(prefix.as_deref())?;
        let entries = keys
            .iter()
            .map(|k| FilterEntry {
                key: k.clone(),
                cardinality: self.bitmap_store.cardinality(k).unwrap_or(0),
            })
            .collect();
        Ok(entries)
    }
}
