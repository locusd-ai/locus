//! Graph layer types and traits — typed adjacency, traversal, centrality.
//!
//! The graph layer is the third stage of the retrieval pipeline:
//!   bitmap pre-filter → graph expand → vector rerank
//!
//! Every stage passes `RoaringBitmap<DocId>` as the carrier so the stages
//! compose uniformly. The graph stage is opt-in everywhere.

use std::collections::HashMap;
use std::path::PathBuf;

use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

use crate::bitmap::BitmapError;
use crate::registry::RegistryError;
use crate::types::{DocId, Timestamp};

// ── Edge model ───────────────────────────────────────────────────

/// Coarse-grained edge category — source-agnostic.
/// New edge subtypes get a new `kind` string, never a new category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeCategory {
    /// Soft "mentions / refers to". Default for narrative links.
    Reference,
    /// Hard "requires / imports / uses". Removing target breaks source.
    Dependency,
    /// Containment or parent-child structure.
    Hierarchy,
    /// Process / status relationship between work items.
    Workflow,
    /// Lineage — "X was generated from / derived from Y".
    Provenance,
}

impl EdgeCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Dependency => "dependency",
            Self::Hierarchy => "hierarchy",
            Self::Workflow => "workflow",
            Self::Provenance => "provenance",
        }
    }
}

impl std::fmt::Display for EdgeCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for EdgeCategory {
    type Err = GraphError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "reference" => Ok(Self::Reference),
            "dependency" => Ok(Self::Dependency),
            "hierarchy" => Ok(Self::Hierarchy),
            "workflow" => Ok(Self::Workflow),
            "provenance" => Ok(Self::Provenance),
            other => Err(GraphError::Storage(format!("unknown edge category: {other}"))),
        }
    }
}

/// A typed directed edge between two documents.
///
/// `kind` is `"<category>:<subtype>"`, e.g. `"ref:wikilink"`, `"dep:import"`,
/// `"flow:blocks"`. The category prefix enables category-scoped traversal
/// without parsing the full kind string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: DocId,
    pub to: DocId,
    pub category: EdgeCategory,
    /// Short string e.g. `"ref:wikilink"`, `"dep:import"`, `"flow:blocks"`.
    pub kind: String,
    /// Default 1.0. Used by weighted centrality (PageRank).
    pub weight: f32,
    /// Byte offset of the link in the source file (for explainability).
    pub byte_offset: Option<u32>,
    pub created_at: Timestamp,
}

/// An edge whose target was not resolvable at ingestion time.
/// Stored until the target doc is indexed (possibly from a different source).
#[derive(Debug, Clone)]
pub struct UnresolvedEdge {
    pub from: DocId,
    pub category: EdgeCategory,
    pub kind: String,
    /// The unresolved target string — wikilink title, import path, page ID, etc.
    pub target_ref: String,
    pub byte_offset: Option<u32>,
}

// ── Traversal specs ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Follow edges from → to.
    Outgoing,
    /// Follow edges to → from (backlinks).
    Incoming,
    /// Both directions.
    Both,
}

/// Filter applied to edges during traversal.
#[derive(Debug, Clone)]
pub enum EdgeFilter {
    /// All edges.
    Any,
    /// Edges of a specific category.
    Category(EdgeCategory),
    /// Edges whose `kind` matches one of these strings exactly.
    Kinds(Vec<String>),
    /// Boolean AND of multiple filters.
    And(Vec<EdgeFilter>),
    /// Boolean OR of multiple filters.
    Or(Vec<EdgeFilter>),
}

impl EdgeFilter {
    /// Returns true if the edge passes this filter.
    pub fn matches(&self, edge: &Edge) -> bool {
        match self {
            Self::Any => true,
            Self::Category(cat) => edge.category == *cat,
            Self::Kinds(kinds) => kinds.iter().any(|k| k == &edge.kind),
            Self::And(filters) => filters.iter().all(|f| f.matches(edge)),
            Self::Or(filters) => filters.iter().any(|f| f.matches(edge)),
        }
    }
}

/// Specification for a multi-hop expansion from a seed set.
#[derive(Debug, Clone)]
pub struct ExpandSpec {
    pub hops: u8,
    pub direction: Direction,
    pub edge_filter: EdgeFilter,
    /// Hard cap on nodes added. Prevents blow-ups from hub nodes.
    pub max_nodes: Option<u32>,
    /// If true, the seed set is included in the output.
    pub include_seeds: bool,
}

impl Default for ExpandSpec {
    fn default() -> Self {
        Self {
            hops: 1,
            direction: Direction::Outgoing,
            edge_filter: EdgeFilter::Any,
            max_nodes: Some(500),
            include_seeds: true,
        }
    }
}

// ── Query request / response ─────────────────────────────────────

/// A pure graph query — no bitmap filter, no vector rerank.
#[derive(Debug, Clone)]
pub struct GraphQueryRequest {
    pub op: GraphOp,
    pub edge_filter: EdgeFilter,
    pub limit: Option<u32>,
}

/// The operation to execute in a `GraphQueryRequest`.
#[derive(Debug, Clone)]
pub enum GraphOp {
    /// Direct neighbours one hop away.
    Neighbours { from: DocId, direction: Direction },
    /// Multi-hop expansion from a seed set.
    Expand { seeds: Vec<DocId>, spec: ExpandSpec },
    /// Shortest path between two docs.
    ShortestPath { from: DocId, to: DocId },
    /// Top-k docs by centrality, optionally restricted to a candidate set.
    TopCentral {
        algorithm: CentralityAlgorithm,
        restrict_to: Option<Vec<DocId>>,
        k: u32,
    },
    /// All docs reachable from a seed (used by provenance / impact analysis).
    Reachable { from: DocId, direction: Direction },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CentralityAlgorithm {
    InDegree,
    OutDegree,
    PageRank { iterations: u16, damping: f32 },
}

impl Default for CentralityAlgorithm {
    fn default() -> Self {
        Self::PageRank {
            iterations: 100,
            damping: 0.85,
        }
    }
}

/// Result of a graph query. Mirrors MatchPointer shape so MCP consumers
/// handle it uniformly — returns pointers, not content.
#[derive(Debug, Serialize)]
pub struct GraphQueryResult {
    pub nodes: Vec<GraphNodeRef>,
    pub edges: Vec<EdgeRef>,
    pub elapsed_us: u64,
}

/// A node in a graph result.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNodeRef {
    pub doc_id: DocId,
    pub file_path: PathBuf,
    /// Source type string (e.g. "obsidian", "code", "confluence").
    pub source_type: String,
    pub label: Option<String>,
    /// Centrality score (for TopCentral queries).
    pub score: Option<f32>,
    /// Hop distance from the seed set (for Expand queries).
    pub hop_distance: Option<u8>,
}

/// An edge in a graph result.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeRef {
    pub from: DocId,
    pub to: DocId,
    pub kind: String,
    pub weight: f32,
}

/// Snapshot of in-memory graph statistics.
#[derive(Debug, Clone, Serialize)]
pub struct GraphStats {
    pub node_count: u64,
    pub edge_count: u64,
    /// Edge counts per category name.
    pub edges_by_category: HashMap<String, u64>,
    pub in_memory_bytes: u64,
    pub last_rebuilt: Option<Timestamp>,
}

// ── Errors ───────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("graph storage error: {0}")]
    Storage(String),
    #[error("doc not in graph: {0}")]
    NotFound(DocId),
    #[error("expansion exceeded max_nodes cap of {0}")]
    ExpansionLimit(u32),
    #[error("no path found between {0} and {1}")]
    NoPath(DocId, DocId),
    #[error(transparent)]
    Bitmap(#[from] BitmapError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

// ── GraphStore trait ─────────────────────────────────────────────

/// Persistent storage for the document link graph.
///
/// Backed by DuckDB (source of truth) + in-memory `petgraph::StableGraph`
/// rebuilt at startup. Mutations write through to both.
pub trait GraphStore: Send + Sync {
    // ── Write path (called by ingestion) ────────────────────────

    /// Insert a fully-resolved edge.
    fn insert_edge(&mut self, edge: Edge) -> Result<(), GraphError>;

    /// Bulk insert edges in a single transaction. Used by initial indexing.
    fn bulk_insert_edges(&mut self, edges: Vec<Edge>) -> Result<(), GraphError>;

    /// Insert an unresolved edge (target not yet in registry).
    fn insert_unresolved(&mut self, edge: UnresolvedEdge) -> Result<(), GraphError>;

    /// Try to resolve pending edges that point at `doc_id` via `link_target`.
    /// Tries exact path match then title/stem match.
    /// Returns the number of edges promoted from pending to resolved.
    fn resolve_pending(&mut self, doc_id: DocId, link_target: &str) -> Result<u32, GraphError>;

    /// Remove all edges where `from` or `to` equals `doc_id`.
    /// Called when a doc is tombstoned.
    fn remove_doc_edges(&mut self, doc_id: DocId) -> Result<u32, GraphError>;

    /// Replace all outgoing edges for a doc atomically (used on re-index).
    fn replace_outgoing(&mut self, doc_id: DocId, edges: Vec<Edge>) -> Result<(), GraphError>;

    // ── Read path ───────────────────────────────────────────────

    /// Direct neighbours of a doc, one hop, filtered by direction and edge kind.
    fn neighbours(
        &self,
        doc_id: DocId,
        direction: Direction,
        filter: &EdgeFilter,
    ) -> Result<Vec<(DocId, Edge)>, GraphError>;

    /// All edges incident to a doc (both directions, all kinds).
    fn doc_edges(&self, doc_id: DocId) -> Result<Vec<Edge>, GraphError>;

    /// Total resolved edge count.
    fn edge_count(&self) -> Result<u64, GraphError>;

    /// Total node count (docs with at least one edge).
    fn node_count(&self) -> Result<u64, GraphError>;

    // ── Lifecycle ────────────────────────────────────────────────

    /// Rebuild the in-memory petgraph from persistent DuckDB rows.
    /// Called at daemon startup. Returns the number of edges loaded.
    fn rebuild_in_memory(&mut self) -> Result<u64, GraphError>;

    /// Drop the in-memory graph (for memory pressure or shutdown).
    /// Subsequent reads fall back to DuckDB (slower path).
    fn drop_in_memory(&mut self);

    /// Graph statistics for status reporting.
    fn stats(&self) -> Result<GraphStats, GraphError>;
}

// ── GraphQueryEngine trait ───────────────────────────────────────

/// Algorithmic graph queries — traversal, paths, centrality.
/// Operates on the in-memory graph maintained by a `GraphStore`.
pub trait GraphQueryEngine: Send + Sync {
    /// Execute a graph query (dispatches on `GraphOp`).
    fn query(&self, request: GraphQueryRequest) -> Result<GraphQueryResult, GraphError>;

    /// Multi-hop expansion of a bitmap of seeds.
    ///
    /// This is the composition primitive for the three-stage pipeline.
    /// Returns a bitmap (same `DocId` space) so it composes with bitmap
    /// intersection and vector scoping.
    fn expand(
        &self,
        seeds: &RoaringBitmap,
        spec: &ExpandSpec,
    ) -> Result<RoaringBitmap, GraphError>;

    /// Compute centrality scores, optionally restricted to a candidate set.
    /// Returns `(DocId, score)` pairs sorted by descending score.
    fn centrality(
        &self,
        algorithm: CentralityAlgorithm,
        restrict_to: Option<&RoaringBitmap>,
    ) -> Result<Vec<(DocId, f32)>, GraphError>;

    /// Shortest path between two docs respecting an edge filter.
    /// Returns the sequence of DocIds from `from` to `to`, inclusive.
    fn shortest_path(
        &self,
        from: DocId,
        to: DocId,
        filter: &EdgeFilter,
    ) -> Result<Vec<DocId>, GraphError>;

    /// All nodes reachable from a seed in the given direction.
    /// Used by provenance impact analysis ("what depends on doc 42?").
    fn reachable(
        &self,
        from: DocId,
        direction: Direction,
        filter: &EdgeFilter,
    ) -> Result<RoaringBitmap, GraphError>;

    /// In-memory graph statistics for diagnostics.
    fn stats(&self) -> Result<GraphStats, GraphError>;
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_edge(from: DocId, to: DocId, category: EdgeCategory, kind: &str) -> Edge {
        Edge {
            from,
            to,
            category,
            kind: kind.to_string(),
            weight: 1.0,
            byte_offset: None,
            created_at: 0,
        }
    }

    #[test]
    fn edge_filter_any_matches_all() {
        let e = make_edge(1, 2, EdgeCategory::Reference, "ref:wikilink");
        assert!(EdgeFilter::Any.matches(&e));
    }

    #[test]
    fn edge_filter_category_matches_correct() {
        let e = make_edge(1, 2, EdgeCategory::Reference, "ref:wikilink");
        assert!(EdgeFilter::Category(EdgeCategory::Reference).matches(&e));
        assert!(!EdgeFilter::Category(EdgeCategory::Dependency).matches(&e));
    }

    #[test]
    fn edge_filter_kinds_matches_exact() {
        let e = make_edge(1, 2, EdgeCategory::Dependency, "dep:import");
        assert!(EdgeFilter::Kinds(vec!["dep:import".into()]).matches(&e));
        assert!(!EdgeFilter::Kinds(vec!["ref:wikilink".into()]).matches(&e));
    }

    #[test]
    fn edge_filter_and_requires_all() {
        let e = make_edge(1, 2, EdgeCategory::Reference, "ref:wikilink");
        let filter = EdgeFilter::And(vec![
            EdgeFilter::Category(EdgeCategory::Reference),
            EdgeFilter::Kinds(vec!["ref:wikilink".into()]),
        ]);
        assert!(filter.matches(&e));

        let filter_fail = EdgeFilter::And(vec![
            EdgeFilter::Category(EdgeCategory::Reference),
            EdgeFilter::Kinds(vec!["dep:import".into()]),
        ]);
        assert!(!filter_fail.matches(&e));
    }

    #[test]
    fn edge_filter_or_requires_any() {
        let e = make_edge(1, 2, EdgeCategory::Dependency, "dep:import");
        let filter = EdgeFilter::Or(vec![
            EdgeFilter::Category(EdgeCategory::Reference),
            EdgeFilter::Category(EdgeCategory::Dependency),
        ]);
        assert!(filter.matches(&e));
    }

    #[test]
    fn edge_category_round_trips_str() {
        for cat in [
            EdgeCategory::Reference,
            EdgeCategory::Dependency,
            EdgeCategory::Hierarchy,
            EdgeCategory::Workflow,
            EdgeCategory::Provenance,
        ] {
            let s = cat.as_str();
            let parsed: EdgeCategory = s.parse().unwrap();
            assert_eq!(parsed, cat);
        }
    }

    #[test]
    fn expand_spec_default_sensible() {
        let spec = ExpandSpec::default();
        assert_eq!(spec.hops, 1);
        assert!(spec.include_seeds);
        assert_eq!(spec.max_nodes, Some(500));
    }
}
