//! DuckDB-backed GraphStore implementation with petgraph in-memory layer.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use duckdb::{params, Connection};
use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableGraph;
use petgraph::visit::EdgeRef;
use petgraph::Directed;

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

// ── In-memory graph state ────────────────────────────────────────

struct InMemoryGraph {
    graph: StableGraph<DocId, Edge, Directed>,
    node_index: HashMap<DocId, NodeIndex>,
    last_rebuilt: Option<Timestamp>,
    bytes_estimate: u64,
}

impl InMemoryGraph {
    fn new() -> Self {
        Self {
            graph: StableGraph::new(),
            node_index: HashMap::new(),
            last_rebuilt: None,
            bytes_estimate: 0,
        }
    }

    fn get_or_add_node(&mut self, doc_id: DocId) -> NodeIndex {
        if let Some(&idx) = self.node_index.get(&doc_id) {
            return idx;
        }
        let idx = self.graph.add_node(doc_id);
        self.node_index.insert(doc_id, idx);
        idx
    }

    fn add_edge(&mut self, edge: &Edge) {
        let from_idx = self.get_or_add_node(edge.from);
        let to_idx = self.get_or_add_node(edge.to);
        self.graph.add_edge(from_idx, to_idx, edge.clone());
        // Rough per-edge byte estimate: 2 DocIds + kind string + fixed fields
        self.bytes_estimate += 64 + edge.kind.len() as u64;
    }

    fn remove_doc(&mut self, doc_id: DocId) {
        if let Some(idx) = self.node_index.remove(&doc_id) {
            // Collect edges incident to this node before removing.
            let edges: Vec<_> = self
                .graph
                .edges(idx)
                .map(|e| e.id())
                .chain(
                    self.graph
                        .edges_directed(idx, petgraph::Direction::Incoming)
                        .map(|e| e.id()),
                )
                .collect();
            for eid in edges {
                self.bytes_estimate =
                    self.bytes_estimate.saturating_sub(64);
                self.graph.remove_edge(eid);
            }
            self.graph.remove_node(idx);
        }
    }
}

// ── DuckDbGraphStore ─────────────────────────────────────────────

/// DuckDB-backed [`GraphStore`] with write-through petgraph in-memory layer.
///
/// Shares the same `Connection` as `DuckDbRegistry` via `Arc<Mutex<Connection>>`.
/// All tables (`doc_links`, `doc_links_pending`) are created by `DuckDbRegistry::init_schema`.
pub struct DuckDbGraphStore {
    conn: Arc<Mutex<Connection>>,
    mem: Arc<RwLock<Option<InMemoryGraph>>>,
}

impl DuckDbGraphStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self {
            conn,
            mem: Arc::new(RwLock::new(None)),
        }
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    fn with_mem_write<F>(&self, f: F)
    where
        F: FnOnce(&mut InMemoryGraph),
    {
        let mut guard = self.mem.write().unwrap();
        if let Some(ref mut g) = *guard {
            f(g);
        }
        // If mem is None, skip — reads fall back to DuckDB.
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
        self.with_mem_write(|g| g.add_edge(&edge));
        Ok(())
    }

    fn bulk_insert_edges(&mut self, edges: Vec<Edge>) -> Result<(), GraphError> {
        if edges.is_empty() {
            return Ok(());
        }
        let conn = self.conn();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        for edge in &edges {
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
        drop(conn);

        let mut guard = self.mem.write().unwrap();
        if let Some(ref mut g) = *guard {
            for edge in &edges {
                g.add_edge(edge);
            }
        }
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
        let conn = self.conn();

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

        let mut promoted: Vec<Edge> = Vec::with_capacity(pending.len());
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
            promoted.push(Edge {
                from: *from_id,
                to: doc_id,
                category,
                kind: kind.clone(),
                weight: 1.0,
                byte_offset: *byte_offset,
                created_at: ts,
            });
        }

        let stem_ref = stem.as_deref().unwrap_or("");
        tx.execute(
            "DELETE FROM doc_links_pending WHERE target_ref = ? OR target_ref = ?",
            params![link_target, stem_ref],
        )
        .map_err(|e| GraphError::Storage(e.to_string()))?;

        tx.commit().map_err(|e| GraphError::Storage(e.to_string()))?;
        drop(conn);

        let mut guard = self.mem.write().unwrap();
        if let Some(ref mut g) = *guard {
            for edge in &promoted {
                g.add_edge(edge);
            }
        }

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
        drop(conn);

        self.with_mem_write(|g| g.remove_doc(doc_id));
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
        for edge in &edges {
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
        drop(conn);

        // Remove all outgoing edges for this doc from the in-memory graph, then re-add.
        let mut guard = self.mem.write().unwrap();
        if let Some(ref mut g) = *guard {
            if let Some(&from_idx) = g.node_index.get(&doc_id) {
                let outgoing: Vec<_> = g
                    .graph
                    .edges(from_idx)
                    .map(|e| e.id())
                    .collect();
                for eid in outgoing {
                    g.bytes_estimate = g.bytes_estimate.saturating_sub(64);
                    g.graph.remove_edge(eid);
                }
            }
            for edge in &edges {
                g.add_edge(edge);
            }
        }
        Ok(())
    }

    fn neighbours(
        &self,
        doc_id: DocId,
        direction: Direction,
        filter: &EdgeFilter,
    ) -> Result<Vec<(DocId, Edge)>, GraphError> {
        // Fast path: use in-memory graph if available.
        {
            let guard = self.mem.read().unwrap();
            if let Some(ref g) = *guard {
                if let Some(&idx) = g.node_index.get(&doc_id) {
                    let edges: Vec<_> = match direction {
                        Direction::Outgoing => g
                            .graph
                            .edges(idx)
                            .map(|e| e.weight().clone())
                            .collect(),
                        Direction::Incoming => g
                            .graph
                            .edges_directed(idx, petgraph::Direction::Incoming)
                            .map(|e| e.weight().clone())
                            .collect(),
                        Direction::Both => g
                            .graph
                            .edges(idx)
                            .chain(
                                g.graph
                                    .edges_directed(idx, petgraph::Direction::Incoming),
                            )
                            .map(|e| e.weight().clone())
                            .collect(),
                    };
                    let result = edges
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
                        .collect();
                    return Ok(result);
                }
                // Doc has no edges — return empty without DuckDB fallback.
                return Ok(vec![]);
            }
        }

        // Slow path: query DuckDB.
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

    fn all_edges(&self) -> Result<Vec<Edge>, GraphError> {
        // Use in-memory graph if available.
        {
            let guard = self.mem.read().unwrap();
            if let Some(ref g) = *guard {
                let edges: Vec<Edge> = g
                    .graph
                    .edge_weights()
                    .cloned()
                    .collect();
                return Ok(edges);
            }
        }
        // Fallback to DuckDB.
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT from_id, to_id, category, kind, weight, byte_offset, created_at
                 FROM doc_links",
            )
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        let rows: Result<Vec<Edge>, _> = stmt
            .query_map([], row_to_edge)
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
        let start = Instant::now();
        let mut g = InMemoryGraph::new();

        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT from_id, to_id, category, kind, weight, byte_offset, created_at
                 FROM doc_links",
            )
            .map_err(|e| GraphError::Storage(e.to_string()))?;

        let rows: Result<Vec<Edge>, _> = stmt
            .query_map([], row_to_edge)
            .map_err(|e| GraphError::Storage(e.to_string()))?
            .collect();
        let edges = rows.map_err(|e| GraphError::Storage(e.to_string()))?;
        let count = edges.len() as u64;

        for edge in &edges {
            g.add_edge(edge);
        }
        g.last_rebuilt = Some(now());

        drop(conn);
        let elapsed = start.elapsed();
        tracing::info!(edges = count, elapsed_ms = elapsed.as_millis(), "graph rebuilt in memory");

        *self.mem.write().unwrap() = Some(g);
        Ok(count)
    }

    fn drop_in_memory(&mut self) {
        *self.mem.write().unwrap() = None;
    }

    fn stats(&self) -> Result<GraphStats, GraphError> {
        let (edge_count, node_count, last_rebuilt, in_memory_bytes) = {
            let guard = self.mem.read().unwrap();
            if let Some(ref g) = *guard {
                (
                    g.graph.edge_count() as u64,
                    g.graph.node_count() as u64,
                    g.last_rebuilt,
                    g.bytes_estimate,
                )
            } else {
                drop(guard);
                (self.edge_count()?, self.node_count()?, None, 0)
            }
        };

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
            in_memory_bytes,
            last_rebuilt,
        })
    }
}

fn row_to_edge(row: &duckdb::Row<'_>) -> duckdb::Result<Edge> {
    let category_str: String = row.get(2)?;
    let category = EdgeCategory::from_str(&category_str).unwrap_or(EdgeCategory::Reference);
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
        store.insert_edge(make_edge(1, 2, "ref:wikilink")).unwrap();
        store.insert_edge(make_edge(1, 3, "ref:wikilink")).unwrap();
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

        store
            .insert_unresolved(UnresolvedEdge {
                from: 1,
                category: EdgeCategory::Reference,
                kind: "ref:wikilink".into(),
                target_ref: "ProjectAlpha".into(),
                byte_offset: None,
            })
            .unwrap();

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

        let neighbours = store
            .neighbours(1, Direction::Outgoing, &EdgeFilter::Any)
            .unwrap();
        assert_eq!(neighbours.len(), 1);
        assert_eq!(neighbours[0].0, 2);
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

    #[test]
    fn rebuild_in_memory_loads_all_edges() {
        let mut store = open_store();
        let edges: Vec<Edge> = (1u32..=10).map(|i| make_edge(0, i, "ref:wikilink")).collect();
        store.bulk_insert_edges(edges).unwrap();

        // mem is None before rebuild.
        assert!(store.mem.read().unwrap().is_none());

        let count = store.rebuild_in_memory().unwrap();
        assert_eq!(count, 10);
        assert!(store.mem.read().unwrap().is_some());

        // Neighbours now served from memory.
        let neighbours = store
            .neighbours(0, Direction::Outgoing, &EdgeFilter::Any)
            .unwrap();
        assert_eq!(neighbours.len(), 10);
    }

    #[test]
    fn drop_in_memory_clears_and_duckdb_fallback_works() {
        let mut store = open_store();
        store.insert_edge(make_edge(1, 2, "ref:wikilink")).unwrap();
        store.rebuild_in_memory().unwrap();
        assert!(store.mem.read().unwrap().is_some());

        store.drop_in_memory();
        assert!(store.mem.read().unwrap().is_none());

        // DuckDB fallback still returns the edge.
        let neighbours = store
            .neighbours(1, Direction::Outgoing, &EdgeFilter::Any)
            .unwrap();
        assert_eq!(neighbours.len(), 1);
    }

    #[test]
    fn write_through_insert_visible_in_memory() {
        let mut store = open_store();
        store.rebuild_in_memory().unwrap(); // start with empty graph in memory
        store.insert_edge(make_edge(1, 2, "ref:wikilink")).unwrap();

        // Should be visible via in-memory path without another rebuild.
        let neighbours = store
            .neighbours(1, Direction::Outgoing, &EdgeFilter::Any)
            .unwrap();
        assert_eq!(neighbours.len(), 1);
    }

    #[test]
    fn replace_outgoing_write_through() {
        let mut store = open_store();
        store.rebuild_in_memory().unwrap();
        store.insert_edge(make_edge(1, 2, "ref:wikilink")).unwrap();
        store.insert_edge(make_edge(1, 3, "ref:wikilink")).unwrap();
        store
            .replace_outgoing(1, vec![make_edge(1, 99, "ref:wikilink")])
            .unwrap();

        let neighbours = store
            .neighbours(1, Direction::Outgoing, &EdgeFilter::Any)
            .unwrap();
        assert_eq!(neighbours.len(), 1);
        assert_eq!(neighbours[0].0, 99);
    }
}
