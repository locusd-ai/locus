//! DuckDB-backed GraphStore implementation.

use std::str::FromStr;
use std::sync::{Arc, Mutex};

use duckdb::{params, Connection};

use locus_core::graph::{
    Direction, Edge, EdgeCategory, EdgeFilter, GraphError, GraphStats, GraphStore, UnresolvedEdge,
};
use locus_core::types::{DocId, Timestamp};

fn now() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as Timestamp
}

/// DuckDB-backed [`GraphStore`].
///
/// Shares the same `Connection` as `DuckDbRegistry` via `Arc<Mutex<Connection>>`.
/// All tables (`doc_links`, `doc_links_pending`) are created by `DuckDbRegistry::init_schema`.
pub struct DuckDbGraphStore {
    conn: Arc<Mutex<Connection>>,
}

impl DuckDbGraphStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }
}

impl GraphStore for DuckDbGraphStore {
    fn insert_edge(&mut self, edge: Edge) -> Result<(), GraphError> {
        self.conn()
            .execute(
                "INSERT OR REPLACE INTO doc_links
                 (from_id, to_id, category, kind, weight, byte_offset, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    edge.from,
                    edge.to,
                    edge.category.as_str(),
                    edge.kind,
                    edge.weight,
                    edge.byte_offset,
                    edge.created_at,
                ],
            )
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        Ok(())
    }

    fn bulk_insert_edges(&mut self, edges: Vec<Edge>) -> Result<(), GraphError> {
        if edges.is_empty() {
            return Ok(());
        }
        let conn = self.conn();
        let tx = conn.unchecked_transaction()
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        for edge in edges {
            tx.execute(
                "INSERT OR REPLACE INTO doc_links
                 (from_id, to_id, category, kind, weight, byte_offset, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    edge.from,
                    edge.to,
                    edge.category.as_str(),
                    edge.kind,
                    edge.weight,
                    edge.byte_offset,
                    edge.created_at,
                ],
            )
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        }
        tx.commit().map_err(|e| GraphError::Storage(e.to_string()))?;
        Ok(())
    }

    fn insert_unresolved(&mut self, edge: UnresolvedEdge) -> Result<(), GraphError> {
        self.conn()
            .execute(
                "INSERT OR IGNORE INTO doc_links_pending
                 (from_id, target_ref, category, kind, byte_offset, created_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    edge.from,
                    edge.target_ref,
                    edge.category.as_str(),
                    edge.kind,
                    edge.byte_offset,
                    now(),
                ],
            )
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        Ok(())
    }

    fn resolve_pending(&mut self, doc_id: DocId, link_target: &str) -> Result<u32, GraphError> {
        // Find pending edges that match this target by:
        //   1. Exact target_ref match (e.g. full path)
        //   2. Stem/title match — target_ref equals the file stem of the new doc's path
        //      (handles Obsidian wikilinks like [[NoteTitle]] → note-title.md)
        //
        // We look up the doc's file_path from the documents table to get its stem.
        let conn = self.conn();

        // Get the file path stem for title-based resolution.
        let file_path: Option<String> = {
            let mut stmt = conn
                .prepare("SELECT file_path FROM documents WHERE doc_id = ?")
                .map_err(|e| GraphError::Storage(e.to_string()))?;
            stmt.query_row(params![doc_id], |row| row.get(0)).ok()
        };

        let stem: Option<String> = file_path.as_deref().and_then(|p| {
            std::path::Path::new(p)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        });

        // Collect matching pending edges (include from_id so we can promote correctly).
        let mut pending: Vec<(DocId, String, String, Option<u32>)> = Vec::new();

        {
            let mut stmt = conn
                .prepare(
                    "SELECT from_id, category, kind, byte_offset FROM doc_links_pending
                     WHERE target_ref = ? OR target_ref = ?",
                )
                .map_err(|e| GraphError::Storage(e.to_string()))?;

            let stem_ref = stem.as_deref().unwrap_or("");
            let rows = stmt
                .query_map(params![link_target, stem_ref], |row| {
                    Ok((
                        row.get::<_, DocId>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<u32>>(3)?,
                    ))
                })
                .map_err(|e| GraphError::Storage(e.to_string()))?;

            for row in rows {
                pending.push(row.map_err(|e| GraphError::Storage(e.to_string()))?);
            }
        }

        if pending.is_empty() {
            return Ok(0);
        }

        let ts = now();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| GraphError::Storage(e.to_string()))?;

        for (from_id, category_str, kind, byte_offset) in &pending {
            let category = EdgeCategory::from_str(category_str)
                .map_err(|e| GraphError::Storage(e.to_string()))?;
            tx.execute(
                "INSERT OR REPLACE INTO doc_links
                 (from_id, to_id, category, kind, weight, byte_offset, created_at)
                 VALUES (?, ?, ?, ?, 1.0, ?, ?)",
                params![from_id, doc_id, category.as_str(), kind, byte_offset, ts],
            )
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        }

        // Remove promoted pending edges.
        let stem_ref = stem.as_deref().unwrap_or("");
        tx.execute(
            "DELETE FROM doc_links_pending WHERE target_ref = ? OR target_ref = ?",
            params![link_target, stem_ref],
        )
        .map_err(|e| GraphError::Storage(e.to_string()))?;

        tx.commit().map_err(|e| GraphError::Storage(e.to_string()))?;

        Ok(pending.len() as u32)
    }

    fn remove_doc_edges(&mut self, doc_id: DocId) -> Result<u32, GraphError> {
        let conn = self.conn();
        let n = conn
            .execute(
                "DELETE FROM doc_links WHERE from_id = ? OR to_id = ?",
                params![doc_id, doc_id],
            )
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        conn.execute(
            "DELETE FROM doc_links_pending WHERE from_id = ?",
            params![doc_id],
        )
        .map_err(|e| GraphError::Storage(e.to_string()))?;
        Ok(n as u32)
    }

    fn replace_outgoing(&mut self, doc_id: DocId, edges: Vec<Edge>) -> Result<(), GraphError> {
        let conn = self.conn();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        tx.execute(
            "DELETE FROM doc_links WHERE from_id = ?",
            params![doc_id],
        )
        .map_err(|e| GraphError::Storage(e.to_string()))?;
        tx.execute(
            "DELETE FROM doc_links_pending WHERE from_id = ?",
            params![doc_id],
        )
        .map_err(|e| GraphError::Storage(e.to_string()))?;
        for edge in edges {
            tx.execute(
                "INSERT OR REPLACE INTO doc_links
                 (from_id, to_id, category, kind, weight, byte_offset, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    edge.from,
                    edge.to,
                    edge.category.as_str(),
                    edge.kind,
                    edge.weight,
                    edge.byte_offset,
                    edge.created_at,
                ],
            )
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        }
        tx.commit().map_err(|e| GraphError::Storage(e.to_string()))?;
        Ok(())
    }

    fn neighbours(
        &self,
        doc_id: DocId,
        direction: Direction,
        filter: &EdgeFilter,
    ) -> Result<Vec<(DocId, Edge)>, GraphError> {
        let conn = self.conn();
        let sql = match direction {
            Direction::Outgoing => {
                "SELECT from_id, to_id, category, kind, weight, byte_offset, created_at
                 FROM doc_links WHERE from_id = ?"
            }
            Direction::Incoming => {
                "SELECT from_id, to_id, category, kind, weight, byte_offset, created_at
                 FROM doc_links WHERE to_id = ?"
            }
            Direction::Both => {
                "SELECT from_id, to_id, category, kind, weight, byte_offset, created_at
                 FROM doc_links WHERE from_id = ? OR to_id = ?"
            }
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| GraphError::Storage(e.to_string()))?;

        let rows: Result<Vec<_>, _> = if direction == Direction::Both {
            stmt.query_map(params![doc_id, doc_id], row_to_edge)
        } else {
            stmt.query_map(params![doc_id], row_to_edge)
        }
        .map_err(|e| GraphError::Storage(e.to_string()))?
        .collect();

        let edges = rows.map_err(|e| GraphError::Storage(e.to_string()))?;

        Ok(edges
            .into_iter()
            .filter(|e| filter.matches(e))
            .map(|e| {
                let neighbour = match direction {
                    Direction::Outgoing => e.to,
                    Direction::Incoming => e.from,
                    Direction::Both => {
                        if e.from == doc_id { e.to } else { e.from }
                    }
                };
                (neighbour, e)
            })
            .collect())
    }

    fn doc_edges(&self, doc_id: DocId) -> Result<Vec<Edge>, GraphError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT from_id, to_id, category, kind, weight, byte_offset, created_at
                 FROM doc_links WHERE from_id = ? OR to_id = ?",
            )
            .map_err(|e| GraphError::Storage(e.to_string()))?;

        let rows: Result<Vec<Edge>, _> = stmt
            .query_map(params![doc_id, doc_id], row_to_edge)
            .map_err(|e| GraphError::Storage(e.to_string()))?
            .collect();

        rows.map_err(|e| GraphError::Storage(e.to_string()))
    }

    fn edge_count(&self) -> Result<u64, GraphError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT COUNT(*) FROM doc_links")
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        let n: u64 = stmt
            .query_row([], |row| row.get(0))
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        Ok(n)
    }

    fn node_count(&self) -> Result<u64, GraphError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT COUNT(DISTINCT id) FROM (
                   SELECT from_id AS id FROM doc_links
                   UNION
                   SELECT to_id AS id FROM doc_links
                 )",
            )
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        let n: u64 = stmt
            .query_row([], |row| row.get(0))
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        Ok(n)
    }

    fn rebuild_in_memory(&mut self) -> Result<u64, GraphError> {
        // No-op until petgraph is added in Task 4.
        Ok(0)
    }

    fn drop_in_memory(&mut self) {
        // No-op until petgraph is added in Task 4.
    }

    fn stats(&self) -> Result<GraphStats, GraphError> {
        let edge_count = self.edge_count()?;
        let node_count = self.node_count()?;

        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT category, COUNT(*) FROM doc_links GROUP BY category")
            .map_err(|e| GraphError::Storage(e.to_string()))?;

        let mut edges_by_category = std::collections::HashMap::new();
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        for row in rows {
            let (cat, count) = row.map_err(|e| GraphError::Storage(e.to_string()))?;
            edges_by_category.insert(cat, count);
        }

        Ok(GraphStats {
            node_count,
            edge_count,
            edges_by_category,
            in_memory_bytes: 0,
            last_rebuilt: None,
        })
    }
}

fn row_to_edge(row: &duckdb::Row<'_>) -> duckdb::Result<Edge> {
    let category_str: String = row.get(2)?;
    let category = EdgeCategory::from_str(&category_str)
        .unwrap_or(EdgeCategory::Reference);
    Ok(Edge {
        from: row.get(0)?,
        to: row.get(1)?,
        category,
        kind: row.get(3)?,
        weight: row.get(4)?,
        byte_offset: row.get(5)?,
        created_at: row.get(6)?,
    })
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use locus_core::graph::{EdgeCategory, UnresolvedEdge};

    fn open_store() -> DuckDbGraphStore {
        // Use an in-memory DuckDB shared via the registry connection.
        use crate::duckdb::DuckDbRegistry;
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        DuckDbGraphStore::new(registry.connection())
    }

    fn make_edge(from: DocId, to: DocId, kind: &str) -> Edge {
        Edge {
            from,
            to,
            category: EdgeCategory::Reference,
            kind: kind.to_string(),
            weight: 1.0,
            byte_offset: None,
            created_at: 0,
        }
    }

    #[test]
    fn insert_edge_and_neighbours_round_trip() {
        let mut store = open_store();
        store.insert_edge(make_edge(1, 2, "ref:wikilink")).unwrap();
        store.insert_edge(make_edge(1, 3, "ref:wikilink")).unwrap();

        let neighbours = store
            .neighbours(1, Direction::Outgoing, &EdgeFilter::Any)
            .unwrap();
        assert_eq!(neighbours.len(), 2);
        assert!(neighbours.iter().any(|(id, _)| *id == 2));
        assert!(neighbours.iter().any(|(id, _)| *id == 3));
    }

    #[test]
    fn bulk_insert_and_edge_count() {
        let mut store = open_store();
        let edges = (1u32..=5).map(|i| make_edge(0, i, "ref:wikilink")).collect();
        store.bulk_insert_edges(edges).unwrap();
        assert_eq!(store.edge_count().unwrap(), 5);
        assert_eq!(store.node_count().unwrap(), 6); // 0 + 1..5
    }

    #[test]
    fn remove_doc_edges_clears_both_directions() {
        let mut store = open_store();
        store.insert_edge(make_edge(1, 2, "ref:wikilink")).unwrap();
        store.insert_edge(make_edge(3, 1, "ref:wikilink")).unwrap();
        store.remove_doc_edges(1).unwrap();
        assert_eq!(store.edge_count().unwrap(), 0);
    }

    #[test]
    fn replace_outgoing_is_atomic() {
        let mut store = open_store();
        // First index: 1 → 2, 1 → 3
        store.insert_edge(make_edge(1, 2, "ref:wikilink")).unwrap();
        store.insert_edge(make_edge(1, 3, "ref:wikilink")).unwrap();
        // Re-index: now only 1 → 4
        store
            .replace_outgoing(1, vec![make_edge(1, 4, "ref:wikilink")])
            .unwrap();

        let neighbours = store
            .neighbours(1, Direction::Outgoing, &EdgeFilter::Any)
            .unwrap();
        assert_eq!(neighbours.len(), 1);
        assert_eq!(neighbours[0].0, 4);
    }

    #[test]
    fn incoming_neighbours() {
        let mut store = open_store();
        store.insert_edge(make_edge(10, 20, "ref:wikilink")).unwrap();
        store.insert_edge(make_edge(11, 20, "ref:wikilink")).unwrap();

        let backlinks = store
            .neighbours(20, Direction::Incoming, &EdgeFilter::Any)
            .unwrap();
        assert_eq!(backlinks.len(), 2);
    }

    #[test]
    fn edge_filter_category_applied_in_neighbours() {
        let mut store = open_store();
        store.insert_edge(make_edge(1, 2, "ref:wikilink")).unwrap();
        store
            .insert_edge(Edge {
                from: 1,
                to: 3,
                category: EdgeCategory::Dependency,
                kind: "dep:import".into(),
                weight: 1.0,
                byte_offset: None,
                created_at: 0,
            })
            .unwrap();

        let refs = store
            .neighbours(
                1,
                Direction::Outgoing,
                &EdgeFilter::Category(EdgeCategory::Reference),
            )
            .unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, 2);
    }

    #[test]
    fn insert_unresolved_and_resolve_pending_by_title() {
        let mut store = open_store();

        // A pending edge from doc 1 to a not-yet-indexed "ProjectAlpha"
        store
            .insert_unresolved(UnresolvedEdge {
                from: 1,
                category: EdgeCategory::Reference,
                kind: "ref:wikilink".into(),
                target_ref: "ProjectAlpha".into(),
                byte_offset: None,
            })
            .unwrap();

        // Simulate inserting doc 2 whose file stem is "ProjectAlpha"
        // We need to insert into the documents table so resolve_pending can look it up.
        {
            let conn = store.conn();
            conn.execute(
                "INSERT INTO documents (doc_id, file_path, source_type, blake3_hash, last_indexed)
                 VALUES (2, '/vault/ProjectAlpha.md', 'obsidian', X'00', 0)",
                [],
            )
            .unwrap();
        }

        let resolved = store.resolve_pending(2, "ProjectAlpha").unwrap();
        assert_eq!(resolved, 1);

        // The edge should now be in doc_links.
        let neighbours = store
            .neighbours(1, Direction::Outgoing, &EdgeFilter::Any)
            .unwrap();
        assert_eq!(neighbours.len(), 1);
    }

    #[test]
    fn stats_returns_correct_counts() {
        let mut store = open_store();
        store.insert_edge(make_edge(1, 2, "ref:wikilink")).unwrap();
        store
            .insert_edge(Edge {
                from: 2,
                to: 3,
                category: EdgeCategory::Dependency,
                kind: "dep:import".into(),
                weight: 1.0,
                byte_offset: None,
                created_at: 0,
            })
            .unwrap();
        let stats = store.stats().unwrap();
        assert_eq!(stats.edge_count, 2);
        assert_eq!(stats.edges_by_category.get("reference"), Some(&1));
        assert_eq!(stats.edges_by_category.get("dependency"), Some(&1));
    }
}
