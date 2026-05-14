use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tracing::{info, instrument, warn};

use biem_core::bitmap::{BitmapError, BitmapStore};
use biem_core::parser::{ParseError, Parser};
use biem_core::registry::{
    NewChunk, NewDoc, Registry, RegistryError,
};
use biem_core::types::{
    BitmapKey, ChangeEvent, ChangeKind, DocId, NoteType,
    ParseResult, SourceType,
};
use biem_enrich::TagPipeline;

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
        }
    }

    /// Set the enrichment pipeline. If set, inferred tags are added to bitmap keys.
    pub fn with_tag_pipeline(mut self, pipeline: TagPipeline) -> Self {
        self.tag_pipeline = Some(pipeline);
        self
    }

    /// Decompose the pipeline into its owned parts for handoff (e.g. to a query engine).
    pub fn into_parts(self) -> (Vec<Box<dyn Parser>>, Box<dyn Registry>, Box<dyn BitmapStore>) {
        (self.parsers, self.registry, self.bitmap_store)
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

    fn type_key(note_type: &NoteType) -> BitmapKey {
        let label = match note_type {
            NoteType::Note => "note",
            NoteType::Task => "task",
            NoteType::Moc => "moc",
            NoteType::Reference => "reference",
        };
        format!("type:{label}")
    }

    fn source_key(source_type: &SourceType) -> BitmapKey {
        let label = match source_type {
            SourceType::Obsidian => "obsidian",
        };
        format!("source:{label}")
    }

    /// Collect all bitmap keys for a parse result + metadata.
    fn bitmap_keys_for(
        path: &Path,
        source: &SourceType,
        result: &ParseResult,
    ) -> Vec<BitmapKey> {
        let mut keys = Vec::new();
        for tag in &result.tags {
            keys.push(Self::tag_key(tag));
        }
        for link in &result.links {
            keys.push(Self::link_key(&link.target));
        }
        keys.push(Self::folder_key(path));
        keys.push(Self::source_key(source));
        if let Some(ref nt) = result.auto_type {
            keys.push(Self::type_key(nt));
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

        let source = SourceType::Obsidian; // Phase 1: only Obsidian
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
        let source = SourceType::Obsidian;

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
        let source = SourceType::Obsidian;

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

                    self.registry.update_doc(existing.doc_id, hash_bytes, result.auto_type.clone())?;
                    let chunks = Self::chunks_to_new(existing.doc_id, &result);
                    self.registry.replace_chunks(existing.doc_id, chunks)?;

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

                let doc_id = self.registry.insert_doc(NewDoc {
                    file_path: path.clone(),
                    source_type: source.clone(),
                    blake3_hash: hash_bytes,
                    auto_type: result.auto_type.clone(),
                })?;

                let chunks = Self::chunks_to_new(doc_id, &result);
                self.registry.replace_chunks(doc_id, chunks)?;

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
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                Self::walk_dir(&path, out)?;
            } else {
                out.push(path);
            }
        }
        Ok(())
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
