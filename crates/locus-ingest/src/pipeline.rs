use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tracing::{info, instrument, warn};

use locus_core::bitmap::{BitmapError, BitmapStore};
use locus_core::graph::{Edge, EdgeCategory, GraphStore, UnresolvedEdge};
use locus_core::parser::{ParseError, Parser};
use locus_core::registry::{
    NewChunk, NewDoc, Registry, RegistryError,
};
use locus_core::semantic::{Embedder, VectorStore};
use locus_core::types::{
    BitmapKey, ChangeEvent, ChangeKind, DocId, DocType,
    ParseResult, SourceType, Timestamp, Visibility,
};
use locus_enrich::TagPipeline;

// ── Error ────────────────────────────────────────────────────────

/// Errors that can occur during ingestion.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Bitmap(#[from] BitmapError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("no parser found for: {0}")]
    NoParser(PathBuf),
}

// ── Result types ─────────────────────────────────────────────────

/// What action was taken for a single file event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestAction {
    Indexed,
    Updated,
    Skipped,
    Tombstoned,
    Moved,
}

/// Result of processing a single change event.
#[derive(Debug, Clone)]
pub struct IngestResult {
    pub action: IngestAction,
    pub bitmaps_updated: u32,
}

/// Result of a bulk indexing run.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BulkIndexResult {
    pub docs_indexed: u32,
    pub docs_updated: u32,
    pub docs_skipped: u32,
    pub docs_tombstoned: u32,
    pub bitmaps_created: u32,
    pub duration_ms: u64,
}

/// Result of a compaction run.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompactResult {
    pub docs_removed: u32,
    pub bitmaps_cleaned: u32,
    pub duration_ms: u64,
}

// ── Pipeline ─────────────────────────────────────────────────────

/// Orchestrates parsers, registry, and bitmap store for ingestion.
pub struct IngestionPipeline {
    parsers: Vec<Box<dyn Parser>>,
    registry: Box<dyn Registry>,
    bitmap_store: Box<dyn BitmapStore>,
    tag_pipeline: Option<TagPipeline>,
    embedder: Option<Box<dyn Embedder>>,
    vector_store: Option<Box<dyn VectorStore>>,
    graph_store: Option<Box<dyn GraphStore>>,
}

impl IngestionPipeline {
    pub fn new(
        parsers: Vec<Box<dyn Parser>>,
        registry: Box<dyn Registry>,
        bitmap_store: Box<dyn BitmapStore>,
    ) -> Self {
        Self {
            parsers,
            registry,
            bitmap_store,
            tag_pipeline: None,
            embedder: None,
            vector_store: None,
            graph_store: None,
        }
    }

    /// Set the enrichment pipeline. If set, inferred tags are added to bitmap keys.
    pub fn with_tag_pipeline(mut self, pipeline: TagPipeline) -> Self {
        self.tag_pipeline = Some(pipeline);
        self
    }

    /// Set the embedder and vector store. If set, chunk embeddings are generated during ingestion.
    pub fn with_embedder(mut self, embedder: Box<dyn Embedder>, vector_store: Box<dyn VectorStore>) -> Self {
        self.embedder = Some(embedder);
        self.vector_store = Some(vector_store);
        self
    }

    /// Set the graph store. If set, link edges are written during ingestion.
    pub fn with_graph_store(mut self, gs: Box<dyn GraphStore>) -> Self {
        self.graph_store = Some(gs);
        self
    }

    /// Decompose the pipeline into its owned parts for handoff (e.g. to a query engine).
    pub fn into_parts(self) -> (Vec<Box<dyn Parser>>, Box<dyn Registry>, Box<dyn BitmapStore>) {
        (self.parsers, self.registry, self.bitmap_store)
    }

    /// Decompose including the optional vector store.
    pub fn into_parts_with_vectors(
        self,
    ) -> (
        Vec<Box<dyn Parser>>,
        Box<dyn Registry>,
        Box<dyn BitmapStore>,
        Option<Box<dyn VectorStore>>,
    ) {
        (self.parsers, self.registry, self.bitmap_store, self.vector_store)
    }

    /// Decompose including the optional graph store.
    pub fn into_parts_with_graph(
        self,
    ) -> (
        Vec<Box<dyn Parser>>,
        Box<dyn Registry>,
        Box<dyn BitmapStore>,
        Option<Box<dyn GraphStore>>,
    ) {
        (self.parsers, self.registry, self.bitmap_store, self.graph_store)
    }

    // ── Graph helpers ────────────────────────────────────────────

    fn now_secs() -> Timestamp {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as Timestamp
    }

    /// Map source type + link to an edge (category, kind) pair.
    fn edge_kind_for_link(source: &SourceType) -> (EdgeCategory, &'static str) {
        match source {
            SourceType::Code => (EdgeCategory::Dependency, "dep:import"),
            SourceType::Obsidian => (EdgeCategory::Reference, "ref:wikilink"),
            SourceType::Custom(_) => (EdgeCategory::Reference, "ref:link"),
        }
    }

    /// Build resolved and unresolved edge lists from a parse result.
    ///
    /// Tries an exact path lookup first; unmatched targets become unresolved edges
    /// and are promoted by `resolve_pending` when the target doc is later indexed.
    fn build_edge_lists(
        registry: &dyn Registry,
        doc_id: DocId,
        source: &SourceType,
        result: &ParseResult,
        ts: Timestamp,
    ) -> (Vec<Edge>, Vec<UnresolvedEdge>) {
        let (category, kind_str) = Self::edge_kind_for_link(source);
        let mut resolved = Vec::new();
        let mut unresolved = Vec::new();

        for link in &result.links {
            let target_path = std::path::Path::new(&link.target);
            let target_doc = registry.lookup_by_path(target_path).ok().flatten();
            if let Some(target) = target_doc {
                resolved.push(Edge {
                    from: doc_id,
                    to: target.doc_id,
                    category,
                    kind: kind_str.to_string(),
                    weight: 1.0,
                    byte_offset: Some(link.byte_offset as u32),
                    created_at: ts,
                });
            } else {
                unresolved.push(UnresolvedEdge {
                    from: doc_id,
                    category,
                    kind: kind_str.to_string(),
                    target_ref: link.target.clone(),
                    byte_offset: Some(link.byte_offset as u32),
                });
            }
        }
        (resolved, unresolved)
    }

    /// Write edges to the graph store for a newly indexed doc.
    /// Uses bulk_insert for resolved edges; inserts each unresolved edge individually.
    /// Then calls resolve_pending so any prior unresolved edges pointing TO this doc are promoted.
    fn graph_write_new(
        gs: &mut dyn GraphStore,
        doc_id: DocId,
        path: &Path,
        resolved: Vec<Edge>,
        unresolved: Vec<UnresolvedEdge>,
    ) {
        if let Err(e) = gs.bulk_insert_edges(resolved) {
            warn!(doc_id, error = %e, "graph bulk_insert_edges failed");
        }
        for ue in unresolved {
            if let Err(e) = gs.insert_unresolved(ue) {
                warn!(doc_id, error = %e, "graph insert_unresolved failed");
            }
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Err(e) = gs.resolve_pending(doc_id, &stem) {
            warn!(doc_id, error = %e, "graph resolve_pending failed");
        }
    }

    /// Atomically replace outgoing edges for a re-indexed doc.
    fn graph_replace_outgoing(
        gs: &mut dyn GraphStore,
        doc_id: DocId,
        path: &Path,
        resolved: Vec<Edge>,
        unresolved: Vec<UnresolvedEdge>,
    ) {
        if let Err(e) = gs.replace_outgoing(doc_id, resolved) {
            warn!(doc_id, error = %e, "graph replace_outgoing failed");
        }
        for ue in unresolved {
            if let Err(e) = gs.insert_unresolved(ue) {
                warn!(doc_id, error = %e, "graph insert_unresolved failed");
            }
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Err(e) = gs.resolve_pending(doc_id, &stem) {
            warn!(doc_id, error = %e, "graph resolve_pending failed");
        }
    }

    /// Upsert a document from raw bytes — insert if new, update if changed, skip if unchanged.
    ///
    /// This is the entry point for remote sources (Confluence, Jira, Slack) that
    /// supply content directly rather than via filesystem paths.
    /// The `path` is a synthetic stable identifier (e.g. `confluence://SPACE/123.confluence`).
    #[instrument(skip(self, content), fields(path = %path.display()))]
    pub fn upsert_document(&mut self, path: PathBuf, content: Vec<u8>) -> Result<IngestResult, IngestError> {
        let parser = self
            .find_parser(&path)
            .ok_or_else(|| IngestError::NoParser(path.clone()))?;

        let hash = blake3::hash(&content);

        // Check for existing doc at this path
        let existing = self.registry.lookup_by_path(&path)?;

        match existing {
            None => {
                // New document — insert
                let result = parser.parse(&path, &content)?;
                let source = Self::infer_source_type(&result);

                let doc_id = self.registry.insert_doc(NewDoc {
                    file_path: path.clone(),
                    source_type: source.clone(),
                    blake3_hash: *hash.as_bytes(),
                    auto_type: result.auto_type.clone(),
                })?;

                let chunks = Self::chunks_to_new(doc_id, &result);
                self.registry.replace_chunks(doc_id, chunks)?;

                let keys = self.enriched_bitmap_keys(&path, &content, &source, &result);
                let mut updated = 0u32;
                for key in &keys {
                    self.bitmap_store.insert_id(key, doc_id)?;
                    updated += 1;
                }

                let chunk_ids: Vec<u32> = self.registry.get_chunks(doc_id)?
                    .iter().map(|c| c.chunk_id).collect();
                self.embed_chunks(&content, &result, &chunk_ids);

                if self.graph_store.is_some() {
                    let ts = Self::now_secs();
                    let (resolved, unresolved) =
                        Self::build_edge_lists(&*self.registry, doc_id, &source, &result, ts);
                    Self::graph_write_new(
                        self.graph_store.as_deref_mut().unwrap(),
                        doc_id,
                        &path,
                        resolved,
                        unresolved,
                    );
                }

                info!(doc_id, bitmaps = updated, "upserted new remote document");
                Ok(IngestResult { action: IngestAction::Indexed, bitmaps_updated: updated })
            }

            Some(doc_record) => {
                // Existing document — skip if hash unchanged
                if doc_record.blake3_hash == *hash.as_bytes() {
                    return Ok(IngestResult { action: IngestAction::Skipped, bitmaps_updated: 0 });
                }

                let result = parser.parse(&path, &content)?;
                let source = Self::infer_source_type(&result);

                let new_keys: HashSet<BitmapKey> =
                    self.enriched_bitmap_keys(&path, &content, &source, &result).into_iter().collect();

                let all_existing_keys = self.bitmap_store.list_keys(None)?;
                let old_keys: HashSet<BitmapKey> = all_existing_keys
                    .into_iter()
                    .filter(|k| {
                        self.bitmap_store
                            .get(k)
                            .map(|bm| bm.contains(doc_record.doc_id))
                            .unwrap_or(false)
                    })
                    .collect();

                let removed: Vec<_> = old_keys.difference(&new_keys).cloned().collect();
                for key in &removed {
                    self.bitmap_store.remove_id(key, doc_record.doc_id)?;
                }
                let added: Vec<_> = new_keys.difference(&old_keys).cloned().collect();
                for key in &added {
                    self.bitmap_store.insert_id(key, doc_record.doc_id)?;
                }

                self.registry.update_doc(
                    doc_record.doc_id,
                    *hash.as_bytes(),
                    result.auto_type.clone(),
                )?;
                let chunks = Self::chunks_to_new(doc_record.doc_id, &result);
                self.registry.replace_chunks(doc_record.doc_id, chunks)?;

                let chunk_ids: Vec<u32> = self.registry.get_chunks(doc_record.doc_id)?
                    .iter().map(|c| c.chunk_id).collect();
                self.embed_chunks(&content, &result, &chunk_ids);

                if self.graph_store.is_some() {
                    let ts = Self::now_secs();
                    let (resolved, unresolved) = Self::build_edge_lists(
                        &*self.registry, doc_record.doc_id, &source, &result, ts,
                    );
                    Self::graph_replace_outgoing(
                        self.graph_store.as_deref_mut().unwrap(),
                        doc_record.doc_id,
                        &path,
                        resolved,
                        unresolved,
                    );
                }

                let delta = (removed.len() + added.len()) as u32;
                info!(doc_id = doc_record.doc_id, bitmaps = delta, "upserted updated remote document");
                Ok(IngestResult { action: IngestAction::Updated, bitmaps_updated: delta })
            }
        }
    }

    /// Find the first parser that can handle the given path.
    fn find_parser(&self, path: &Path) -> Option<&dyn Parser> {
        self.parsers.iter().find_map(|p| {
            if p.can_parse(path) {
                Some(p.as_ref())
            } else {
                None
            }
        })
    }

    // ── Bitmap key helpers ───────────────────────────────────────

    fn tag_key(tag: &str) -> BitmapKey {
        format!("tag:{tag}")
    }

    fn folder_key(path: &Path) -> BitmapKey {
        let folder = path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("folder:{folder}")
    }

    fn link_key(target: &str) -> BitmapKey {
        format!("link:{target}")
    }

    fn type_key(doc_type: &DocType) -> BitmapKey {
        let label = match doc_type {
            DocType::Note => "note",
            DocType::Task => "task",
            DocType::Moc => "moc",
            DocType::Reference => "reference",
            DocType::SourceFile => "source_file",
            DocType::TestFile => "test_file",
            DocType::ConfigFile => "config_file",
            DocType::Custom(s) => return format!("type:{s}"),
        };
        format!("type:{label}")
    }

    fn source_key(source_type: &SourceType) -> BitmapKey {
        let label = match source_type {
            SourceType::Obsidian => "obsidian",
            SourceType::Code => "code",
            SourceType::Custom(s) => return format!("source:{s}"),
        };
        format!("source:{label}")
    }

    /// Infer the source type from parse result tags.
    ///
    /// Parsers signal their source type by emitting a `source:<name>` tag in `ParseResult.tags`.
    /// Falls back to a `lang:` heuristic for code, then defaults to Obsidian.
    fn infer_source_type(result: &ParseResult) -> SourceType {
        if let Some(tag) = result.tags.iter().find(|t| t.starts_with("source:")) {
            let name = tag.strip_prefix("source:").unwrap_or("unknown");
            return match name {
                "obsidian" => SourceType::Obsidian,
                "code" => SourceType::Code,
                other => SourceType::Custom(other.to_string()),
            };
        }
        if result.tags.iter().any(|t| t.starts_with("lang:")) {
            SourceType::Code
        } else {
            SourceType::Obsidian
        }
    }

    /// Collect all bitmap keys for a parse result + metadata.
    fn bitmap_keys_for(
        path: &Path,
        source: &SourceType,
        result: &ParseResult,
    ) -> Vec<BitmapKey> {
        let mut keys = Vec::new();
        for tag in &result.tags {
            // Tags that already contain a namespace prefix (e.g. "lang:rust", "kind:function")
            // are used as-is. Tags without a prefix get the "tag:" namespace.
            if tag.contains(':') {
                keys.push(tag.clone());
            } else {
                keys.push(Self::tag_key(tag));
            }
        }
        for link in &result.links {
            keys.push(Self::link_key(&link.target));
        }
        keys.push(Self::folder_key(path));
        keys.push(Self::source_key(source));
        if let Some(ref nt) = result.auto_type {
            keys.push(Self::type_key(nt));
        }

        // Code-specific: generate import: and visibility: keys from chunks
        if *source == SourceType::Code {
            // Import keys from links (code uses LinkRef for imports)
            for link in &result.links {
                let import_key = format!("import:{}", link.target);
                if !keys.contains(&import_key) {
                    keys.push(import_key);
                }
            }
            // Visibility keys from chunk metadata
            for chunk in &result.chunks {
                if let Some(ref vis) = chunk.metadata.visibility {
                    let vis_key = match vis {
                        Visibility::Public => "visibility:public",
                        Visibility::Private => "visibility:private",
                        Visibility::Internal => "visibility:internal",
                    };
                    if !keys.contains(&vis_key.to_string()) {
                        keys.push(vis_key.to_string());
                    }
                }
            }
        }

        keys
    }

    /// Collect bitmap keys including enrichment tags (if pipeline is configured).
    fn enriched_bitmap_keys(
        &self,
        path: &Path,
        content: &[u8],
        source: &SourceType,
        result: &ParseResult,
    ) -> Vec<BitmapKey> {
        let mut keys = Self::bitmap_keys_for(path, source, result);
        if let Some(ref tp) = self.tag_pipeline {
            match tp.enrich(path, content, result, source) {
                Ok(inferred) => {
                    for tag in inferred {
                        // Inferred tags are already namespaced (e.g. "topic:auth", "size:small")
                        keys.push(tag);
                    }
                }
                Err(e) => {
                    warn!(error = %e, "enrichment failed, continuing without inferred tags");
                }
            }
        }
        keys
    }

    // ── Chunk conversion helper ──────────────────────────────────

    /// Embed chunks for a document if embedder is configured.
    /// chunk_ids must be in the same order as result.chunks.
    fn embed_chunks(
        &self,
        content: &[u8],
        result: &ParseResult,
        chunk_ids: &[u32],
    ) {
        let (Some(embedder), Some(vs)) = (&self.embedder, &self.vector_store) else {
            return;
        };

        // Extract text for each chunk from byte ranges
        let texts: Vec<String> = result
            .chunks
            .iter()
            .map(|c| {
                let start = c.byte_range.start.min(content.len());
                let end = c.byte_range.end.min(content.len());
                String::from_utf8_lossy(&content[start..end]).to_string()
            })
            .collect();

        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

        match embedder.embed(&text_refs) {
            Ok(vectors) => {
                for (i, vec) in vectors.iter().enumerate() {
                    if i < chunk_ids.len() {
                        if let Err(e) = vs.upsert(chunk_ids[i], vec) {
                            warn!(chunk_id = chunk_ids[i], error = %e, "failed to upsert embedding");
                        }
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to embed chunks");
            }
        }
    }

    fn chunks_to_new(doc_id: DocId, result: &ParseResult) -> Vec<NewChunk> {
        result
            .chunks
            .iter()
            .map(|c| NewChunk {
                doc_id,
                kind: c.kind.clone(),
                byte_start: c.byte_range.start as u32,
                byte_end: c.byte_range.end as u32,
                label: c.label.clone(),
                depth: c.depth,
                metadata: c.metadata.clone(),
            })
            .collect()
    }

    // ── Event processing ─────────────────────────────────────────

    /// Process a single filesystem change event.
    #[instrument(skip(self), fields(path = %event.path.display()))]
    pub fn process_event(&mut self, event: &ChangeEvent) -> Result<IngestResult, IngestError> {
        // Skip ignored paths
        if Self::is_ignored(&event.path) {
            return Ok(IngestResult {
                action: IngestAction::Skipped,
                bitmaps_updated: 0,
            });
        }
        match &event.kind {
            ChangeKind::Created => self.handle_created(&event.path),
            ChangeKind::Modified => self.handle_modified(&event.path),
            ChangeKind::Deleted => self.handle_deleted(&event.path),
            ChangeKind::Renamed { from } => self.handle_renamed(from, &event.path),
        }
    }

    fn handle_created(&mut self, path: &Path) -> Result<IngestResult, IngestError> {
        let parser = self
            .find_parser(path)
            .ok_or_else(|| IngestError::NoParser(path.to_path_buf()))?;

        let content = fs::read(path)?;
        let hash = blake3::hash(&content);
        let result = parser.parse(path, &content)?;

        let source = Self::infer_source_type(&result);
        let doc_id = self.registry.insert_doc(NewDoc {
            file_path: path.to_path_buf(),
            source_type: source.clone(),
            blake3_hash: *hash.as_bytes(),
            auto_type: result.auto_type.clone(),
        })?;

        let chunks = Self::chunks_to_new(doc_id, &result);
        self.registry.replace_chunks(doc_id, chunks)?;

        let keys = self.enriched_bitmap_keys(path, &content, &source, &result);
        let mut updated = 0u32;
        for key in &keys {
            self.bitmap_store.insert_id(key, doc_id)?;
            updated += 1;
        }

        // Embed chunks if embedder configured
        let chunk_ids: Vec<u32> = self.registry.get_chunks(doc_id)?
            .iter().map(|c| c.chunk_id).collect();
        self.embed_chunks(&content, &result, &chunk_ids);

        // Write graph edges if graph store configured
        if self.graph_store.is_some() {
            let ts = Self::now_secs();
            let (resolved, unresolved) =
                Self::build_edge_lists(&*self.registry, doc_id, &source, &result, ts);
            Self::graph_write_new(
                self.graph_store.as_deref_mut().unwrap(),
                doc_id,
                path,
                resolved,
                unresolved,
            );
        }

        info!(doc_id, bitmaps = updated, "indexed new document");
        Ok(IngestResult {
            action: IngestAction::Indexed,
            bitmaps_updated: updated,
        })
    }

    fn handle_modified(&mut self, path: &Path) -> Result<IngestResult, IngestError> {
        let parser = self
            .find_parser(path)
            .ok_or_else(|| IngestError::NoParser(path.to_path_buf()))?;

        let content = fs::read(path)?;
        let hash = blake3::hash(&content);

        // Look up existing doc
        let existing = self.registry.lookup_by_path(path)?;
        let doc_record = match existing {
            Some(r) => r,
            None => {
                // Not in registry yet — treat as Created
                warn!("modified event for unknown file, treating as created");
                return self.handle_created(path);
            }
        };

        // Skip if hash unchanged
        if doc_record.blake3_hash == *hash.as_bytes() {
            return Ok(IngestResult {
                action: IngestAction::Skipped,
                bitmaps_updated: 0,
            });
        }

        let result = parser.parse(path, &content)?;
        let source = Self::infer_source_type(&result);

        // Compute old bitmap keys from the stored record
        // We need to reconstruct what the old keys were; we re-parse isn't possible
        // since we don't store old content. Instead, list keys containing this doc_id
        // and diff against new keys.
        let new_keys: HashSet<BitmapKey> =
            self.enriched_bitmap_keys(path, &content, &source, &result).into_iter().collect();

        // Find old keys by scanning existing bitmaps that contain this doc_id
        let all_existing_keys = self.bitmap_store.list_keys(None)?;
        let old_keys: HashSet<BitmapKey> = all_existing_keys
            .into_iter()
            .filter(|k| {
                self.bitmap_store
                    .get(k)
                    .map(|bm| bm.contains(doc_record.doc_id))
                    .unwrap_or(false)
            })
            .collect();

        // Remove doc from keys no longer present
        let removed: Vec<_> = old_keys.difference(&new_keys).cloned().collect();
        for key in &removed {
            self.bitmap_store.remove_id(key, doc_record.doc_id)?;
        }

        // Add doc to new keys
        let added: Vec<_> = new_keys.difference(&old_keys).cloned().collect();
        for key in &added {
            self.bitmap_store.insert_id(key, doc_record.doc_id)?;
        }

        // Update registry
        self.registry.update_doc(
            doc_record.doc_id,
            *hash.as_bytes(),
            result.auto_type.clone(),
        )?;

        let chunks = Self::chunks_to_new(doc_record.doc_id, &result);
        self.registry.replace_chunks(doc_record.doc_id, chunks)?;

        // Re-embed chunks
        let chunk_ids: Vec<u32> = self.registry.get_chunks(doc_record.doc_id)?
            .iter().map(|c| c.chunk_id).collect();
        self.embed_chunks(&content, &result, &chunk_ids);

        // Replace graph edges atomically if graph store configured
        if self.graph_store.is_some() {
            let ts = Self::now_secs();
            let (resolved, unresolved) = Self::build_edge_lists(
                &*self.registry,
                doc_record.doc_id,
                &source,
                &result,
                ts,
            );
            Self::graph_replace_outgoing(
                self.graph_store.as_deref_mut().unwrap(),
                doc_record.doc_id,
                path,
                resolved,
                unresolved,
            );
        }

        let updated = (removed.len() + added.len()) as u32;
        info!(doc_id = doc_record.doc_id, bitmaps = updated, "updated document");
        Ok(IngestResult {
            action: IngestAction::Updated,
            bitmaps_updated: updated,
        })
    }

    fn handle_deleted(&mut self, path: &Path) -> Result<IngestResult, IngestError> {
        let doc_record = self
            .registry
            .lookup_by_path(path)?
            .ok_or_else(|| RegistryError::NotFound(0))?;

        let doc_id = doc_record.doc_id;

        // Tombstone the doc
        self.bitmap_store.tombstone(doc_id)?;

        // Delete chunk vectors if vector store configured
        if let Some(ref vs) = self.vector_store {
            if let Ok(chunks) = self.registry.get_chunks(doc_id) {
                for c in &chunks {
                    let _ = vs.delete(c.chunk_id);
                }
            }
        }

        // Remove from all bitmaps that contain this doc_id
        let all_keys = self.bitmap_store.list_keys(None)?;
        let mut removed = 0u32;
        for key in &all_keys {
            let bm = self.bitmap_store.get(key)?;
            if bm.contains(doc_id) {
                self.bitmap_store.remove_id(key, doc_id)?;
                removed += 1;
            }
        }

        // Remove graph edges in both directions
        if let Some(gs) = &mut self.graph_store {
            if let Err(e) = gs.remove_doc_edges(doc_id) {
                warn!(doc_id, error = %e, "graph remove_doc_edges failed");
            }
        }

        info!(doc_id, bitmaps = removed, "tombstoned document");
        Ok(IngestResult {
            action: IngestAction::Tombstoned,
            bitmaps_updated: removed,
        })
    }

    fn handle_renamed(
        &mut self,
        from: &Path,
        to: &Path,
    ) -> Result<IngestResult, IngestError> {
        let doc_record = self
            .registry
            .lookup_by_path(from)?
            .ok_or_else(|| RegistryError::NotFound(0))?;

        let doc_id = doc_record.doc_id;
        self.registry.update_path(doc_id, to.to_path_buf())?;

        // Update folder bitmap if parent changed
        let mut updated = 0u32;
        let old_folder = Self::folder_key(from);
        let new_folder = Self::folder_key(to);
        if old_folder != new_folder {
            self.bitmap_store.remove_id(&old_folder, doc_id)?;
            self.bitmap_store.insert_id(&new_folder, doc_id)?;
            updated = 2;
        }

        info!(doc_id, "renamed document");
        Ok(IngestResult {
            action: IngestAction::Moved,
            bitmaps_updated: updated,
        })
    }

    // ── Bulk indexing ────────────────────────────────────────────

    /// Index all parseable files in a directory tree (idempotent).
    ///
    /// - Files already indexed with the same hash are skipped.
    /// - Files with a changed hash are updated (re-parsed, bitmaps diffed).
    /// - New files are inserted.
    /// - Files in the registry but no longer on disk are tombstoned.
    #[instrument(skip(self))]
    pub fn bulk_index(&mut self, root: &Path) -> Result<BulkIndexResult, IngestError> {
        let start = Instant::now();

        // Collect all parseable files on disk
        let mut files: Vec<PathBuf> = Vec::new();
        Self::walk_dir(root, &mut files)?;
        let parseable: Vec<PathBuf> = files
            .into_iter()
            .filter(|p| self.find_parser(p).is_some())
            .collect();

        // Build a set of on-disk paths for tombstone detection
        let on_disk: HashSet<PathBuf> = parseable.iter().cloned().collect();

        // Build a lookup of existing registry docs by path
        let existing_docs = self.registry.list_all_docs()?;
        let mut existing_by_path: std::collections::HashMap<PathBuf, _> = existing_docs
            .into_iter()
            .map(|d| (d.file_path.clone(), d))
            .collect();

        let mut docs_indexed = 0u32;
        let mut docs_updated = 0u32;
        let mut docs_skipped = 0u32;
        let mut docs_tombstoned = 0u32;
        let mut all_bitmap_entries: std::collections::HashMap<BitmapKey, roaring::RoaringBitmap> =
            std::collections::HashMap::new();

        for path in &parseable {
            let content = fs::read(path)?;
            let hash = blake3::hash(&content);
            let hash_bytes = *hash.as_bytes();

            if let Some(existing) = existing_by_path.remove(path) {
                // Already registered
                if existing.blake3_hash == hash_bytes {
                    // Unchanged — skip, but still collect bitmap keys so bulk_put is correct
                    let parser = self.find_parser(path).expect("already filtered");
                    let result = parser.parse(path, &content)?;
                    let source = Self::infer_source_type(&result);
                    let keys = self.enriched_bitmap_keys(path, &content, &source, &result);
                    for key in keys {
                        all_bitmap_entries
                            .entry(key)
                            .or_insert_with(roaring::RoaringBitmap::new)
                            .insert(existing.doc_id);
                    }
                    docs_skipped += 1;
                } else {
                    // Changed — update
                    let parser = self.find_parser(path).expect("already filtered");
                    let result = parser.parse(path, &content)?;
                    let source = Self::infer_source_type(&result);

                    self.registry.update_doc(existing.doc_id, hash_bytes, result.auto_type.clone())?;
                    let chunks = Self::chunks_to_new(existing.doc_id, &result);
                    self.registry.replace_chunks(existing.doc_id, chunks)?;

                    // Re-embed changed chunks
                    let chunk_ids: Vec<u32> = self.registry.get_chunks(existing.doc_id)?
                        .iter().map(|c| c.chunk_id).collect();
                    self.embed_chunks(&content, &result, &chunk_ids);

                    // Replace graph edges atomically for changed doc
                    if self.graph_store.is_some() {
                        let ts = Self::now_secs();
                        let (resolved, unresolved) = Self::build_edge_lists(
                            &*self.registry, existing.doc_id, &source, &result, ts,
                        );
                        Self::graph_replace_outgoing(
                            self.graph_store.as_deref_mut().unwrap(),
                            existing.doc_id, path, resolved, unresolved,
                        );
                    }

                    let keys = self.enriched_bitmap_keys(path, &content, &source, &result);
                    for key in keys {
                        all_bitmap_entries
                            .entry(key)
                            .or_insert_with(roaring::RoaringBitmap::new)
                            .insert(existing.doc_id);
                    }
                    docs_updated += 1;
                }
            } else {
                // New file — insert
                let parser = self.find_parser(path).expect("already filtered");
                let result = parser.parse(path, &content)?;
                let source = Self::infer_source_type(&result);

                let doc_id = self.registry.insert_doc(NewDoc {
                    file_path: path.clone(),
                    source_type: source.clone(),
                    blake3_hash: hash_bytes,
                    auto_type: result.auto_type.clone(),
                })?;

                let chunks = Self::chunks_to_new(doc_id, &result);
                self.registry.replace_chunks(doc_id, chunks)?;

                // Embed new chunks
                let chunk_ids: Vec<u32> = self.registry.get_chunks(doc_id)?
                    .iter().map(|c| c.chunk_id).collect();
                self.embed_chunks(&content, &result, &chunk_ids);

                // Write graph edges for new doc
                if self.graph_store.is_some() {
                    let ts = Self::now_secs();
                    let (resolved, unresolved) = Self::build_edge_lists(
                        &*self.registry, doc_id, &source, &result, ts,
                    );
                    Self::graph_write_new(
                        self.graph_store.as_deref_mut().unwrap(),
                        doc_id, path, resolved, unresolved,
                    );
                }

                let keys = self.enriched_bitmap_keys(path, &content, &source, &result);
                for key in keys {
                    all_bitmap_entries
                        .entry(key)
                        .or_insert_with(roaring::RoaringBitmap::new)
                        .insert(doc_id);
                }
                docs_indexed += 1;
            }
        }

        // Tombstone files that are in the registry but no longer on disk
        // Only consider docs whose paths are under root (don't tombstone docs from other vaults)
        for (path, doc) in &existing_by_path {
            if path.starts_with(root) && !on_disk.contains(path) {
                self.bitmap_store.tombstone(doc.doc_id)?;
                // Remove from all bitmaps
                let all_keys = self.bitmap_store.list_keys(None)?;
                for key in &all_keys {
                    let bm = self.bitmap_store.get(key)?;
                    if bm.contains(doc.doc_id) {
                        self.bitmap_store.remove_id(key, doc.doc_id)?;
                    }
                }
                // Remove graph edges in both directions
                if let Some(gs) = &mut self.graph_store {
                    if let Err(e) = gs.remove_doc_edges(doc.doc_id) {
                        warn!(doc_id = doc.doc_id, error = %e, "graph remove_doc_edges failed");
                    }
                }
                docs_tombstoned += 1;
            }
        }

        // Bulk put bitmaps (merge with existing)
        let bitmaps_created = all_bitmap_entries.len() as u32;
        let entries: Vec<(BitmapKey, roaring::RoaringBitmap)> =
            all_bitmap_entries.into_iter().collect();
        self.bitmap_store.bulk_put(entries)?;

        let duration_ms = start.elapsed().as_millis() as u64;
        info!(docs_indexed, docs_updated, docs_skipped, docs_tombstoned, bitmaps_created, duration_ms, "bulk index complete");

        Ok(BulkIndexResult {
            docs_indexed,
            docs_updated,
            docs_skipped,
            docs_tombstoned,
            bitmaps_created,
            duration_ms,
        })
    }

    fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
        use ignore::WalkBuilder;

        let walker = WalkBuilder::new(dir)
            // Respect .gitignore
            .git_ignore(true)
            .git_global(false)
            .git_exclude(false)
            // Add custom .locusignore
            .add_custom_ignore_filename(".locusignore")
            // Add default ignores
            .filter_entry(|entry| {
                let name = entry.file_name().to_string_lossy();
                // Skip common build/dependency directories even without ignore files
                !matches!(
                    name.as_ref(),
                    "target" | "node_modules" | "__pycache__" | ".git" | ".hg" | ".svn"
                        | "dist" | "build" | ".tox" | ".mypy_cache" | ".pytest_cache"
                )
            })
            .build();

        for result in walker {
            match result {
                Ok(entry) => {
                    let path = entry.into_path();
                    if path.is_file() {
                        out.push(path);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "walk error, skipping entry");
                }
            }
        }
        Ok(())
    }

    /// Check if a path should be ignored based on default patterns and .locusignore.
    fn is_ignored(path: &Path) -> bool {
        // Check against default ignore directory names
        for component in path.components() {
            let name = component.as_os_str().to_string_lossy();
            if matches!(
                name.as_ref(),
                "target" | "node_modules" | "__pycache__" | ".git" | ".hg" | ".svn"
                    | "dist" | "build" | ".tox" | ".mypy_cache" | ".pytest_cache"
            ) {
                return true;
            }
        }

        // Check for .locusignore in parent directories
        if let Some(parent) = path.parent() {
            let ignore_file = parent.join(".locusignore");
            if ignore_file.exists() {
                if let Ok(contents) = fs::read_to_string(&ignore_file) {
                    let mut builder = ignore::gitignore::GitignoreBuilder::new(parent);
                    for line in contents.lines() {
                        let _ = builder.add_line(None, line);
                    }
                    if let Ok(gitignore) = builder.build() {
                        if gitignore.matched(path, path.is_dir()).is_ignore() {
                            return true;
                        }
                    }
                }
            }
        }

        false
    }

    // ── Compaction ───────────────────────────────────────────────

    /// Remove tombstoned doc IDs from all bitmaps and delete them from the registry.
    /// Clears the tombstone bitmap and returns a summary.
    #[instrument(skip(self))]
    pub fn compact(&mut self) -> Result<CompactResult, IngestError> {
        let start = Instant::now();
        let tombstones = self.bitmap_store.get_tombstone()?;

        if tombstones.is_empty() {
            return Ok(CompactResult {
                docs_removed: 0,
                bitmaps_cleaned: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let doc_ids: Vec<DocId> = tombstones.iter().collect();
        let all_keys = self.bitmap_store.list_keys(None)?;

        // Remove tombstoned IDs from every bitmap
        let mut bitmaps_cleaned = 0u32;
        for key in &all_keys {
            let bm = self.bitmap_store.get(key)?;
            let mut dirty = false;
            for &doc_id in &doc_ids {
                if bm.contains(doc_id) {
                    dirty = true;
                }
            }
            if dirty {
                let mut bm = bm;
                for &doc_id in &doc_ids {
                    bm.remove(doc_id);
                }
                if bm.is_empty() {
                    self.bitmap_store.delete(key)?;
                } else {
                    self.bitmap_store.put(key, &bm)?;
                }
                bitmaps_cleaned += 1;
            }
        }

        // Delete docs from registry (ignore NotFound — may already be gone)
        for &doc_id in &doc_ids {
            match self.registry.delete_doc(doc_id) {
                Ok(()) => {}
                Err(RegistryError::NotFound(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }

        // Clear the tombstone bitmap
        self.bitmap_store.clear_tombstone()?;

        let docs_removed = doc_ids.len() as u32;
        info!(docs_removed, bitmaps_cleaned, "compaction complete");

        Ok(CompactResult {
            docs_removed,
            bitmaps_cleaned,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}
