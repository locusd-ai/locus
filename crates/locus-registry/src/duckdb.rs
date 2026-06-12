//! DuckDB-backed Registry implementation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use locus_core::registry::{
    BitmapCatalogEntry, ChunkRecord, DocRecord, GlobalState, NewChunk, NewDoc,
};
use locus_core::{
    BitmapCategory, BitmapKey, ChunkId, ChunkKind, ChunkMetadata, DocId, DocType, Registry,
    RegistryError, SourceType, Timestamp, Visibility,
};
use duckdb::{params, Connection};
use tracing::instrument;

/// DuckDB-backed [`Registry`].
///
/// Wraps `Connection` in a `Mutex` to satisfy `Send + Sync` required by the trait.
/// This is fine because Locus's core is single-threaded sync; concurrency is handled
/// at the boundary via `spawn_blocking`.
pub struct DuckDbRegistry {
    conn: Arc<Mutex<Connection>>,
}

impl DuckDbRegistry {
    /// Open or create a DuckDB registry at the given file path.
    /// Pass `":memory:"` for an in-memory database (useful for tests).
    pub fn new(path: &str) -> Result<Self, RegistryError> {
        let conn =
            Connection::open(path).map_err(|e| RegistryError::Database(e.to_string()))?;
        let registry = Self { conn: Arc::new(Mutex::new(conn)) };
        registry.init_schema()?;
        Ok(registry)
    }

    /// Return a shared handle to the underlying connection.
    /// Used by `DuckDbGraphStore` to share the same DuckDB file.
    pub fn connection(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }

    /// Acquire the connection lock. Panics if poisoned (unrecoverable).
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    fn init_schema(&self) -> Result<(), RegistryError> {
        self.conn()
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS documents (
                    doc_id      UINTEGER PRIMARY KEY,
                    file_path   VARCHAR NOT NULL UNIQUE,
                    source_type VARCHAR NOT NULL,
                    blake3_hash BLOB NOT NULL,
                    last_indexed BIGINT NOT NULL,
                    auto_type   VARCHAR
                );

                CREATE TABLE IF NOT EXISTS chunks (
                    chunk_id    UINTEGER PRIMARY KEY,
                    doc_id      UINTEGER NOT NULL,
                    kind        VARCHAR NOT NULL,
                    byte_start  UINTEGER NOT NULL,
                    byte_end    UINTEGER NOT NULL,
                    label       VARCHAR,
                    depth       UTINYINT NOT NULL,
                    metadata_json VARCHAR
                );

                CREATE TABLE IF NOT EXISTS bitmap_catalog (
                    bitmap_key   VARCHAR PRIMARY KEY,
                    category     VARCHAR NOT NULL,
                    cardinality  UINTEGER NOT NULL,
                    last_updated BIGINT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS global_state (
                    id              UTINYINT PRIMARY KEY DEFAULT 1,
                    next_doc_id     UINTEGER NOT NULL DEFAULT 1,
                    next_chunk_id   UINTEGER NOT NULL DEFAULT 1,
                    total_documents UINTEGER NOT NULL DEFAULT 0
                );

                INSERT OR IGNORE INTO global_state (id, next_doc_id, next_chunk_id, total_documents)
                VALUES (1, 1, 1, 0);

                CREATE TABLE IF NOT EXISTS doc_links (
                    from_id     UINTEGER NOT NULL,
                    to_id       UINTEGER NOT NULL,
                    category    VARCHAR  NOT NULL,
                    kind        VARCHAR  NOT NULL,
                    weight      REAL     NOT NULL DEFAULT 1.0,
                    byte_offset UINTEGER,
                    created_at  BIGINT   NOT NULL,
                    PRIMARY KEY (from_id, to_id, kind)
                );

                CREATE INDEX IF NOT EXISTS idx_doc_links_from ON doc_links(from_id);
                CREATE INDEX IF NOT EXISTS idx_doc_links_to   ON doc_links(to_id);
                CREATE INDEX IF NOT EXISTS idx_doc_links_cat  ON doc_links(category);

                CREATE TABLE IF NOT EXISTS doc_links_pending (
                    from_id     UINTEGER NOT NULL,
                    target_ref  VARCHAR  NOT NULL,
                    category    VARCHAR  NOT NULL,
                    kind        VARCHAR  NOT NULL,
                    byte_offset UINTEGER,
                    created_at  BIGINT   NOT NULL,
                    PRIMARY KEY (from_id, target_ref, kind)
                );

                CREATE INDEX IF NOT EXISTS idx_doc_links_pending_target ON doc_links_pending(target_ref);

                CREATE TABLE IF NOT EXISTS doc_chunk_ranges (
                    doc_id      UINTEGER PRIMARY KEY,
                    chunk_start UINTEGER NOT NULL,
                    chunk_end   UINTEGER NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_doc_chunk_ranges_range
                    ON doc_chunk_ranges(chunk_start, chunk_end);

                CREATE TABLE IF NOT EXISTS doc_bitmap_keys (
                    doc_id     UINTEGER NOT NULL,
                    bitmap_key TEXT     NOT NULL,
                    PRIMARY KEY (doc_id, bitmap_key)
                );
                ",
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        // Migration backfill: populate doc_chunk_ranges for any docs whose range
        // row is missing (handles databases created before B1 was deployed).
        self.conn()
            .execute_batch(
                "INSERT OR IGNORE INTO doc_chunk_ranges (doc_id, chunk_start, chunk_end)
                 SELECT doc_id, MIN(chunk_id), MAX(chunk_id)
                 FROM chunks
                 GROUP BY doc_id;",
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        Ok(())
    }

    fn now() -> Timestamp {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as Timestamp
    }

    fn next_doc_id_from(conn: &Connection) -> Result<DocId, RegistryError> {
        let mut stmt = conn
            .prepare("SELECT next_doc_id FROM global_state WHERE id = 1")
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        let id: u32 = stmt
            .query_row([], |row| row.get(0))
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        Ok(id)
    }

    fn next_chunk_id_from(conn: &Connection) -> Result<ChunkId, RegistryError> {
        let mut stmt = conn
            .prepare("SELECT next_chunk_id FROM global_state WHERE id = 1")
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        let id: u32 = stmt
            .query_row([], |row| row.get(0))
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        Ok(id)
    }

    fn source_type_to_str(st: &SourceType) -> String {
        match st {
            SourceType::Obsidian => "obsidian".to_string(),
            SourceType::Code => "code".to_string(),
            SourceType::Custom(s) => s.clone(),
        }
    }

    fn str_to_source_type(s: &str) -> SourceType {
        match s {
            "obsidian" => SourceType::Obsidian,
            "code" => SourceType::Code,
            other => SourceType::Custom(other.to_string()),
        }
    }

    fn doc_type_to_str(nt: &DocType) -> String {
        match nt {
            DocType::Note => "note".to_string(),
            DocType::Task => "task".to_string(),
            DocType::Moc => "moc".to_string(),
            DocType::Reference => "reference".to_string(),
            DocType::SourceFile => "source_file".to_string(),
            DocType::TestFile => "test_file".to_string(),
            DocType::ConfigFile => "config_file".to_string(),
            DocType::Custom(s) => s.clone(),
        }
    }

    fn str_to_doc_type(s: &str) -> Option<DocType> {
        match s {
            "note" => Some(DocType::Note),
            "task" => Some(DocType::Task),
            "moc" => Some(DocType::Moc),
            "reference" => Some(DocType::Reference),
            "source_file" => Some(DocType::SourceFile),
            "test_file" => Some(DocType::TestFile),
            "config_file" => Some(DocType::ConfigFile),
            other => Some(DocType::Custom(other.to_string())),
        }
    }

    fn chunk_kind_to_str(k: &ChunkKind) -> &'static str {
        match k {
            ChunkKind::Section => "section",
            ChunkKind::Frontmatter => "frontmatter",
            ChunkKind::Body => "body",
            ChunkKind::Function => "function",
            ChunkKind::Method => "method",
            ChunkKind::Class => "class",
            ChunkKind::Module => "module",
            ChunkKind::Import => "import",
            ChunkKind::Constant => "constant",
        }
    }

    fn str_to_chunk_kind(s: &str) -> ChunkKind {
        match s {
            "section" => ChunkKind::Section,
            "frontmatter" => ChunkKind::Frontmatter,
            "body" => ChunkKind::Body,
            "function" => ChunkKind::Function,
            "method" => ChunkKind::Method,
            "class" => ChunkKind::Class,
            "module" => ChunkKind::Module,
            "import" => ChunkKind::Import,
            "constant" => ChunkKind::Constant,
            _ => ChunkKind::Body,
        }
    }

    fn category_to_str(c: &BitmapCategory) -> &'static str {
        match c {
            BitmapCategory::Tag => "tag",
            BitmapCategory::Folder => "folder",
            BitmapCategory::Link => "link",
            BitmapCategory::Type => "type",
            BitmapCategory::Source => "source",
            BitmapCategory::Enrichment => "enrichment",
            BitmapCategory::Code => "code",
            BitmapCategory::Custom => "custom",
        }
    }

    fn str_to_category(s: &str) -> BitmapCategory {
        match s {
            "tag" => BitmapCategory::Tag,
            "folder" => BitmapCategory::Folder,
            "link" => BitmapCategory::Link,
            "type" => BitmapCategory::Type,
            "source" => BitmapCategory::Source,
            "enrichment" => BitmapCategory::Enrichment,
            "code" => BitmapCategory::Code,
            "custom" => BitmapCategory::Custom,
            _ => BitmapCategory::Tag,
        }
    }

    fn metadata_to_json(m: &ChunkMetadata) -> String {
        serde_json::json!({
            "signature": m.signature,
            "language": m.language,
            "visibility": m.visibility.as_ref().map(|v| match v {
                Visibility::Public => "public",
                Visibility::Private => "private",
                Visibility::Internal => "internal",
            }),
        })
        .to_string()
    }

    fn json_to_metadata(s: &str) -> ChunkMetadata {
        let v: serde_json::Value = serde_json::from_str(s).unwrap_or_default();
        ChunkMetadata {
            signature: v["signature"].as_str().map(|s| s.to_string()),
            language: v["language"].as_str().map(|s| s.to_string()),
            visibility: v["visibility"].as_str().and_then(|s| match s {
                "public" => Some(Visibility::Public),
                "private" => Some(Visibility::Private),
                "internal" => Some(Visibility::Internal),
                _ => None,
            }),
        }
    }

    fn row_to_doc_record(row: &duckdb::Row<'_>) -> Result<DocRecord, duckdb::Error> {
        let file_path_str: String = row.get(1)?;
        let source_type_str: String = row.get(2)?;
        let hash_bytes: Vec<u8> = row.get(3)?;
        let auto_type_str: Option<String> = row.get(5)?;

        let mut blake3_hash = [0u8; 32];
        if hash_bytes.len() == 32 {
            blake3_hash.copy_from_slice(&hash_bytes);
        }

        Ok(DocRecord {
            doc_id: row.get(0)?,
            file_path: PathBuf::from(file_path_str),
            source_type: DuckDbRegistry::str_to_source_type(&source_type_str),
            blake3_hash,
            last_indexed: row.get(4)?,
            auto_type: auto_type_str.and_then(|s| DuckDbRegistry::str_to_doc_type(&s)),
        })
    }
}

impl Registry for DuckDbRegistry {
    #[instrument(skip(self, doc))]
    fn insert_doc(&mut self, doc: NewDoc) -> Result<DocId, RegistryError> {
        let conn = self.conn();
        let id = Self::next_doc_id_from(&conn)?;
        let now = Self::now();

        conn.execute(
            "INSERT INTO documents (doc_id, file_path, source_type, blake3_hash, last_indexed, auto_type)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                id,
                doc.file_path.to_string_lossy().to_string(),
                Self::source_type_to_str(&doc.source_type),
                doc.blake3_hash.to_vec(),
                now,
                doc.auto_type.as_ref().map(Self::doc_type_to_str),
            ],
        )
        .map_err(|e| {
            if e.to_string().contains("Duplicate") || e.to_string().contains("UNIQUE") {
                RegistryError::DuplicatePath(doc.file_path.clone())
            } else {
                RegistryError::Database(e.to_string())
            }
        })?;

        conn.execute(
            "UPDATE global_state SET next_doc_id = ?, total_documents = total_documents + 1 WHERE id = 1",
            params![id + 1],
        )
        .map_err(|e| RegistryError::Database(e.to_string()))?;

        Ok(id)
    }

    #[instrument(skip(self, docs))]
    fn bulk_insert_docs(&mut self, docs: Vec<NewDoc>) -> Result<Vec<DocId>, RegistryError> {
        let conn = self.conn();
        // Check for duplicates first
        for doc in &docs {
            let mut stmt = conn
                .prepare("SELECT COUNT(*) FROM documents WHERE file_path = ?")
                .map_err(|e| RegistryError::Database(e.to_string()))?;
            let count: u32 = stmt
                .query_row(params![doc.file_path.to_string_lossy().to_string()], |row| row.get(0))
                .map_err(|e| RegistryError::Database(e.to_string()))?;
            if count > 0 {
                return Err(RegistryError::DuplicatePath(doc.file_path.clone()));
            }
        }
        drop(conn); // release lock before calling insert_doc which re-acquires

        let mut ids = Vec::with_capacity(docs.len());
        for doc in docs {
            ids.push(self.insert_doc(doc)?);
        }
        Ok(ids)
    }

    fn lookup_by_path(&self, path: &Path) -> Result<Option<DocRecord>, RegistryError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT doc_id, file_path, source_type, blake3_hash, last_indexed, auto_type
                 FROM documents WHERE file_path = ?",
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        stmt.query_row(
            params![path.to_string_lossy().to_string()],
            Self::row_to_doc_record,
        )
        .map(Some)
        .or_else(|e| {
            if e == duckdb::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(RegistryError::Database(e.to_string()))
            }
        })
    }

    fn lookup_by_id(&self, doc_id: DocId) -> Result<Option<DocRecord>, RegistryError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT doc_id, file_path, source_type, blake3_hash, last_indexed, auto_type
                 FROM documents WHERE doc_id = ?",
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        stmt.query_row(params![doc_id], Self::row_to_doc_record)
            .map(Some)
            .or_else(|e| {
                if e == duckdb::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(RegistryError::Database(e.to_string()))
                }
            })
    }

    fn lookup_by_ids(&self, doc_ids: &[DocId]) -> Result<Vec<DocRecord>, RegistryError> {
        if doc_ids.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn();
        let placeholders: Vec<String> = doc_ids.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT doc_id, file_path, source_type, blake3_hash, last_indexed, auto_type
             FROM documents WHERE doc_id IN ({})",
            placeholders.join(", ")
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let params: Vec<Box<dyn duckdb::ToSql>> =
            doc_ids.iter().map(|id| Box::new(*id) as Box<dyn duckdb::ToSql>).collect();
        let param_refs: Vec<&dyn duckdb::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), Self::row_to_doc_record)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| RegistryError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    #[instrument(skip(self))]
    fn update_doc(
        &mut self,
        doc_id: DocId,
        hash: [u8; 32],
        auto_type: Option<DocType>,
    ) -> Result<(), RegistryError> {
        let conn = self.conn();
        let now = Self::now();
        let affected = conn
            .execute(
                "UPDATE documents SET blake3_hash = ?, last_indexed = ?, auto_type = ? WHERE doc_id = ?",
                params![hash.to_vec(), now, auto_type.as_ref().map(Self::doc_type_to_str), doc_id],
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        if affected == 0 {
            return Err(RegistryError::NotFound(doc_id));
        }
        Ok(())
    }

    #[instrument(skip(self))]
    fn update_path(&mut self, doc_id: DocId, new_path: PathBuf) -> Result<(), RegistryError> {
        let conn = self.conn();
        let affected = conn
            .execute(
                "UPDATE documents SET file_path = ? WHERE doc_id = ?",
                params![new_path.to_string_lossy().to_string(), doc_id],
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        if affected == 0 {
            return Err(RegistryError::NotFound(doc_id));
        }
        Ok(())
    }

    fn delete_doc(&mut self, doc_id: DocId) -> Result<(), RegistryError> {
        let conn = self.conn();
        // Delete chunks and range index first
        conn.execute("DELETE FROM chunks WHERE doc_id = ?", params![doc_id])
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        conn.execute("DELETE FROM doc_chunk_ranges WHERE doc_id = ?", params![doc_id])
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        conn.execute("DELETE FROM doc_bitmap_keys WHERE doc_id = ?", params![doc_id])
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        let affected = conn
            .execute("DELETE FROM documents WHERE doc_id = ?", params![doc_id])
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        if affected == 0 {
            return Err(RegistryError::NotFound(doc_id));
        }
        Ok(())
    }

    #[instrument(skip(self, chunks))]
    fn replace_chunks(
        &mut self,
        doc_id: DocId,
        chunks: Vec<NewChunk>,
    ) -> Result<Vec<ChunkId>, RegistryError> {
        let conn = self.conn();

        // Verify doc exists
        let mut check = conn
            .prepare("SELECT COUNT(*) FROM documents WHERE doc_id = ?")
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        let count: u32 = check
            .query_row(params![doc_id], |row| row.get(0))
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        if count == 0 {
            return Err(RegistryError::NotFound(doc_id));
        }

        // Delete old chunks and their range row
        conn.execute("DELETE FROM chunks WHERE doc_id = ?", params![doc_id])
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        conn.execute("DELETE FROM doc_chunk_ranges WHERE doc_id = ?", params![doc_id])
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let chunk_start = Self::next_chunk_id_from(&conn)?;
        let mut chunk_id = chunk_start;
        let mut ids = Vec::with_capacity(chunks.len());

        for c in &chunks {
            let meta_json = Self::metadata_to_json(&c.metadata);
            conn.execute(
                "INSERT INTO chunks (chunk_id, doc_id, kind, byte_start, byte_end, label, depth, metadata_json)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    chunk_id, doc_id, Self::chunk_kind_to_str(&c.kind),
                    c.byte_start, c.byte_end, c.label, c.depth, meta_json,
                ],
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;
            ids.push(chunk_id);
            chunk_id += 1;
        }

        conn.execute(
            "UPDATE global_state SET next_chunk_id = ? WHERE id = 1",
            params![chunk_id],
        )
        .map_err(|e| RegistryError::Database(e.to_string()))?;

        // Maintain doc_chunk_ranges — only if at least one chunk was inserted
        if !ids.is_empty() {
            let chunk_end = chunk_id - 1; // inclusive end of the new run
            conn.execute(
                "INSERT OR REPLACE INTO doc_chunk_ranges (doc_id, chunk_start, chunk_end)
                 VALUES (?, ?, ?)",
                params![doc_id, chunk_start, chunk_end],
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        }

        Ok(ids)
    }

    fn get_chunks(&self, doc_id: DocId) -> Result<Vec<ChunkRecord>, RegistryError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT chunk_id, doc_id, kind, byte_start, byte_end, label, depth, metadata_json
                 FROM chunks WHERE doc_id = ? ORDER BY chunk_id",
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![doc_id], |row| {
                let kind_str: String = row.get(2)?;
                let label: Option<String> = row.get(5)?;
                let meta_json: Option<String> = row.get(7)?;

                Ok(ChunkRecord {
                    chunk_id: row.get(0)?,
                    doc_id: row.get(1)?,
                    kind: DuckDbRegistry::str_to_chunk_kind(&kind_str),
                    byte_start: row.get(3)?,
                    byte_end: row.get(4)?,
                    label,
                    depth: row.get(6)?,
                    metadata: meta_json
                        .map(|j| DuckDbRegistry::json_to_metadata(&j))
                        .unwrap_or_default(),
                })
            })
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| RegistryError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    fn chunk_range(&self, doc_id: DocId) -> Result<Option<(ChunkId, ChunkId)>, RegistryError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT chunk_start, chunk_end FROM doc_chunk_ranges WHERE doc_id = ?")
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![doc_id], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?))
            })
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| RegistryError::Database(e.to_string()))?)),
            None => Ok(None),
        }
    }

    fn doc_for_chunk(&self, chunk_id: ChunkId) -> Result<Option<DocId>, RegistryError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT doc_id FROM doc_chunk_ranges
                 WHERE chunk_start <= ? AND chunk_end >= ?",
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![chunk_id, chunk_id], |row| row.get::<_, u32>(0))
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| RegistryError::Database(e.to_string()))?)),
            None => Ok(None),
        }
    }

    fn get_chunks_for_docs(&self, doc_ids: &[DocId]) -> Result<Vec<ChunkRecord>, RegistryError> {
        if doc_ids.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn();
        let placeholders = vec!["?"; doc_ids.len()].join(", ");
        let sql = format!(
            "SELECT chunk_id, doc_id, kind, byte_start, byte_end, label, depth, metadata_json
             FROM chunks WHERE doc_id IN ({placeholders}) ORDER BY chunk_id"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let params_vec: Vec<&dyn duckdb::ToSql> =
            doc_ids.iter().map(|id| id as &dyn duckdb::ToSql).collect();

        let rows = stmt
            .query_map(params_vec.as_slice(), |row| {
                let kind_str: String = row.get(2)?;
                let label: Option<String> = row.get(5)?;
                let meta_json: Option<String> = row.get(7)?;

                Ok(ChunkRecord {
                    chunk_id: row.get(0)?,
                    doc_id: row.get(1)?,
                    kind: DuckDbRegistry::str_to_chunk_kind(&kind_str),
                    byte_start: row.get(3)?,
                    byte_end: row.get(4)?,
                    label,
                    depth: row.get(6)?,
                    metadata: meta_json
                        .map(|j| DuckDbRegistry::json_to_metadata(&j))
                        .unwrap_or_default(),
                })
            })
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| RegistryError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    fn set_doc_keys(&mut self, doc_id: DocId, keys: &[BitmapKey]) -> Result<(), RegistryError> {
        let conn = self.conn();
        conn.execute("DELETE FROM doc_bitmap_keys WHERE doc_id = ?", params![doc_id])
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        let mut stmt = conn
            .prepare("INSERT OR IGNORE INTO doc_bitmap_keys (doc_id, bitmap_key) VALUES (?, ?)")
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        for key in keys {
            stmt.execute(params![doc_id, key])
                .map_err(|e| RegistryError::Database(e.to_string()))?;
        }
        Ok(())
    }

    fn get_doc_keys(&self, doc_id: DocId) -> Result<Vec<BitmapKey>, RegistryError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT bitmap_key FROM doc_bitmap_keys WHERE doc_id = ? ORDER BY bitmap_key")
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![doc_id], |row| row.get::<_, String>(0))
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        let mut keys = Vec::new();
        for row in rows {
            keys.push(row.map_err(|e| RegistryError::Database(e.to_string()))?);
        }
        Ok(keys)
    }

    fn delete_doc_keys(&mut self, doc_id: DocId) -> Result<(), RegistryError> {
        let conn = self.conn();
        conn.execute("DELETE FROM doc_bitmap_keys WHERE doc_id = ?", params![doc_id])
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        Ok(())
    }

    #[instrument(skip(self, entry))]
    fn upsert_catalog_entry(&mut self, entry: BitmapCatalogEntry) -> Result<(), RegistryError> {
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO bitmap_catalog (bitmap_key, category, cardinality, last_updated)
             VALUES (?, ?, ?, ?)",
            params![
                entry.bitmap_key,
                Self::category_to_str(&entry.category),
                entry.cardinality,
                entry.last_updated,
            ],
        )
        .map_err(|e| RegistryError::Database(e.to_string()))?;
        Ok(())
    }

    #[instrument(skip(self, entries))]
    fn bulk_upsert_catalog(
        &mut self,
        entries: Vec<BitmapCatalogEntry>,
    ) -> Result<(), RegistryError> {
        let conn = self.conn();
        for entry in entries {
            conn.execute(
                "INSERT OR REPLACE INTO bitmap_catalog (bitmap_key, category, cardinality, last_updated)
                 VALUES (?, ?, ?, ?)",
                params![
                    entry.bitmap_key,
                    Self::category_to_str(&entry.category),
                    entry.cardinality,
                    entry.last_updated,
                ],
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        }
        Ok(())
    }

    fn get_catalog_entry(&self, key: &str) -> Result<Option<BitmapCatalogEntry>, RegistryError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT bitmap_key, category, cardinality, last_updated
                 FROM bitmap_catalog WHERE bitmap_key = ?",
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        stmt.query_row(params![key], |row| {
            let cat_str: String = row.get(1)?;
            Ok(BitmapCatalogEntry {
                bitmap_key: row.get(0)?,
                category: DuckDbRegistry::str_to_category(&cat_str),
                cardinality: row.get(2)?,
                last_updated: row.get(3)?,
            })
        })
        .map(Some)
        .or_else(|e| {
            if e == duckdb::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(RegistryError::Database(e.to_string()))
            }
        })
    }

    fn list_catalog(
        &self,
        category: Option<BitmapCategory>,
    ) -> Result<Vec<BitmapCatalogEntry>, RegistryError> {
        let conn = self.conn();
        let (sql, cat_param) = match &category {
            Some(cat) => (
                "SELECT bitmap_key, category, cardinality, last_updated
                 FROM bitmap_catalog WHERE category = ?",
                Some(Self::category_to_str(cat).to_string()),
            ),
            None => (
                "SELECT bitmap_key, category, cardinality, last_updated
                 FROM bitmap_catalog",
                None,
            ),
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let row_mapper = |row: &duckdb::Row<'_>| {
            let cat_str: String = row.get(1)?;
            Ok(BitmapCatalogEntry {
                bitmap_key: row.get(0)?,
                category: DuckDbRegistry::str_to_category(&cat_str),
                cardinality: row.get(2)?,
                last_updated: row.get(3)?,
            })
        };

        let rows = if let Some(ref cat) = cat_param {
            stmt.query_map(params![cat], row_mapper)
        } else {
            stmt.query_map([], row_mapper)
        }
        .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| RegistryError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    fn list_all_docs(&self) -> Result<Vec<DocRecord>, RegistryError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT doc_id, file_path, source_type, blake3_hash, last_indexed, auto_type
                 FROM documents",
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let rows = stmt
            .query_map([], Self::row_to_doc_record)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| RegistryError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    fn get_global_state(&self) -> Result<GlobalState, RegistryError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT next_doc_id, next_chunk_id, total_documents FROM global_state WHERE id = 1")
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        stmt.query_row([], |row| {
            Ok(GlobalState {
                next_doc_id: row.get(0)?,
                next_chunk_id: row.get(1)?,
                total_documents: row.get(2)?,
            })
        })
        .map_err(|e| RegistryError::Database(e.to_string()))
    }
}
