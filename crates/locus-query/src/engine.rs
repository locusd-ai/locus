use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use roaring::RoaringBitmap;
use tracing::instrument;

use locus_core::bitmap::BitmapStore;
use locus_core::graph::{GraphQueryEngine, ExpandSpec};
use locus_core::query::{
    ChunkPointer, Filter, FilterEntry, IndexStatus, InspectResult as QueryInspectResult,
    MatchPointer, QueryEngine, QueryError, QueryRequest, QueryResult,
};
use locus_core::registry::{BitmapCatalogEntry, Registry};
use locus_core::semantic::{
    ScoreSource, ScoredPointer, SemanticQueryRequest, SemanticQueryResult,
    Embedder, Reranker, VectorStore,
};
use locus_core::types::BitmapCategory;

/// Concrete query engine backed by bitmap store + registry.
pub struct BitmapQueryEngine {
    bitmap_store: Box<dyn BitmapStore>,
    registry: Box<dyn Registry>,
    graph: Option<Arc<dyn GraphQueryEngine>>,
}

impl BitmapQueryEngine {
    pub fn new(bitmap_store: Box<dyn BitmapStore>, registry: Box<dyn Registry>) -> Self {
        Self {
            bitmap_store,
            registry,
            graph: None,
        }
    }

    /// Attach a graph query engine for the three-stage pipeline.
    pub fn with_graph(mut self, g: Arc<dyn GraphQueryEngine>) -> Self {
        self.graph = Some(g);
        self
    }

    /// Expand a seed bitmap via the graph layer (if wired); returns seeds unchanged if not.
    pub fn graph_expand(
        &self,
        seeds: &RoaringBitmap,
        spec: &ExpandSpec,
    ) -> Result<RoaringBitmap, QueryError> {
        match &self.graph {
            Some(g) => g
                .expand(seeds, spec)
                .map_err(|e| QueryError::Semantic(e.to_string())),
            None => Ok(seeds.clone()),
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

    /// Resolve the text content of a chunk by reading its byte range from the source file.
    /// Returns `None` if the chunk or document can't be found, or the file can't be read.
    fn resolve_chunk_text(&self, chunk_id: u32, matching_docs: &RoaringBitmap) -> Option<String> {
        for doc_id in matching_docs.iter() {
            if let Ok(chunks) = self.registry.get_chunks(doc_id) {
                if let Some(chunk) = chunks.iter().find(|c| c.chunk_id == chunk_id) {
                    if let Some(doc) = self.registry.lookup_by_id(doc_id).ok().flatten() {
                        if let Ok(content) = std::fs::read_to_string(&doc.file_path) {
                            let start = chunk.byte_start as usize;
                            let end = chunk.byte_end as usize;
                            if end <= content.len() {
                                return Some(content[start..end].to_string());
                            }
                        }
                    }
                    return None;
                }
            }
        }
        None
    }

    /// Perform a semantic query: bitmap pre-filter → vector search → optional rerank.
    ///
    /// The bitmap filter is mandatory — there is no full-space vector search.
    /// If `request.rerank` is true and a reranker is provided, the top-k vector results
    /// are re-scored using the cross-encoder for higher-quality ranking.
    #[instrument(skip(self, embedder, vector_store, reranker))]
    pub fn semantic_query(
        &self,
        request: &SemanticQueryRequest,
        embedder: &dyn Embedder,
        vector_store: &dyn VectorStore,
        reranker: Option<&dyn Reranker>,
    ) -> Result<SemanticQueryResult, QueryError> {
        // Phase 1: bitmap pre-filter
        let bitmap_start = Instant::now();
        let matching = self.resolve_filter(&request.filter)?;
        let bitmap_candidates = matching.len() as u32;
        let elapsed_bitmap_us = bitmap_start.elapsed().as_micros() as u64;

        if matching.is_empty() {
            return Ok(SemanticQueryResult {
                pointers: vec![],
                bitmap_candidates: 0,
                graph_expanded_to: request.graph_expand.as_ref().map(|_| 0u32),
                vector_searched: 0,
                elapsed_bitmap_us,
                elapsed_graph_us: 0,
                elapsed_vector_us: 0,
                elapsed_rerank_us: 0,
            });
        }

        // Phase 1b: optional graph expand (bitmap pre-filter → graph expand)
        let graph_start = Instant::now();
        let (candidates, graph_expanded_to) = if let Some(ref spec) = request.graph_expand {
            let expanded = self.graph_expand(&matching, spec)?;
            let count = expanded.len() as u32;
            (expanded, Some(count))
        } else {
            (matching, None)
        };
        let elapsed_graph_us = graph_start.elapsed().as_micros() as u64;

        // Phase 2: collect all chunk IDs for matching docs
        let mut candidate_chunk_ids: Vec<u32> = Vec::new();
        for doc_id in candidates.iter() {
            if let Ok(chunks) = self.registry.get_chunks(doc_id) {
                for c in chunks {
                    candidate_chunk_ids.push(c.chunk_id);
                }
            }
        }

        let vector_searched = candidate_chunk_ids.len() as u32;

        // Phase 3: embed query and search
        let vector_start = Instant::now();
        let query_embedding = embedder
            .embed(&[&request.query_text])
            .map_err(|e| QueryError::Semantic(e.to_string()))?;

        let query_vec = query_embedding
            .into_iter()
            .next()
            .ok_or_else(|| QueryError::Semantic("embedder returned no vector".into()))?;

        let scored_chunks = vector_store
            .search_within(&query_vec, &candidate_chunk_ids, request.top_k)
            .map_err(|e| QueryError::Semantic(e.to_string()))?;
        let elapsed_vector_us = vector_start.elapsed().as_micros() as u64;

        // Phase 4: optional reranking
        let rerank_start = Instant::now();
        let (final_scored, score_source) = if request.rerank {
            if let Some(reranker) = reranker {
                // Build (chunk_id, text) pairs for reranking by reading chunk
                // content from source files using registry metadata.
                let mut rerank_candidates: Vec<(u32, String)> = Vec::new();
                for &(chunk_id, _) in &scored_chunks {
                    if let Some(text) = self.resolve_chunk_text(chunk_id, &candidates) {
                        rerank_candidates.push((chunk_id, text));
                    }
                }

                if rerank_candidates.is_empty() {
                    (scored_chunks, ScoreSource::CosineSimilarity)
                } else {
                    let refs: Vec<(u32, &str)> = rerank_candidates
                        .iter()
                        .map(|(id, text)| (*id, text.as_str()))
                        .collect();
                    let reranked = reranker
                        .rerank(&request.query_text, &refs)
                        .map_err(|e| QueryError::Semantic(e.to_string()))?;
                    (reranked, ScoreSource::Reranker("fastembed".into()))
                }
            } else {
                (scored_chunks, ScoreSource::CosineSimilarity)
            }
        } else {
            (scored_chunks, ScoreSource::CosineSimilarity)
        };
        let elapsed_rerank_us = rerank_start.elapsed().as_micros() as u64;

        // Phase 5: hydrate results
        let leaf_keys = Self::collect_leaf_keys(&request.filter);
        let mut pointers = Vec::new();

        for (chunk_id, score) in final_scored {
            // Find the doc that owns this chunk
            let mut owner_doc_id = None;
            for doc_id in candidates.iter() {
                if let Ok(chunks) = self.registry.get_chunks(doc_id) {
                    if chunks.iter().any(|c| c.chunk_id == chunk_id) {
                        owner_doc_id = Some(doc_id);
                        break;
                    }
                }
            }

            if let Some(doc_id) = owner_doc_id {
                if let Some(ptr) = self.hydrate(doc_id, leaf_keys.clone())? {
                    pointers.push(ScoredPointer {
                        pointer: ptr,
                        chunk_id,
                        score,
                        score_source: score_source.clone(),
                    });
                }
            }
        }

        Ok(SemanticQueryResult {
            pointers,
            bitmap_candidates,
            graph_expanded_to,
            vector_searched,
            elapsed_bitmap_us,
            elapsed_graph_us,
            elapsed_vector_us,
            elapsed_rerank_us,
        })
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
