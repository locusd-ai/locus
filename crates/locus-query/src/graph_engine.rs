//! PetgraphQueryEngine — algorithmic graph queries over a GraphStore.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use roaring::RoaringBitmap;

use locus_core::graph::{
    CentralityAlgorithm, Direction, Edge, EdgeFilter, ExpandSpec, GraphError, GraphNodeRef,
    GraphOp, GraphQueryEngine, GraphQueryRequest, GraphQueryResult, GraphStats, GraphStore,
};
use locus_core::types::DocId;

// ── PetgraphQueryEngine ──────────────────────────────────────────

/// Graph query engine that delegates traversal to the [`GraphStore`]'s
/// `neighbours()` interface (backed by petgraph in-memory when available).
///
/// Centrality algorithms build a local `petgraph::Graph` from `all_edges()`.
pub struct PetgraphQueryEngine {
    store: Arc<dyn GraphStore>,
}

impl PetgraphQueryEngine {
    pub fn new(store: Arc<dyn GraphStore>) -> Self {
        Self { store }
    }
}

// ── Internal helpers ─────────────────────────────────────────────

/// Build a dense `petgraph::Graph<DocId, f32>` from a flat edge list.
/// Returns (graph, node_index_to_doc_id, doc_id_to_node_index).
fn build_petgraph(
    edges: &[Edge],
) -> (
    petgraph::Graph<DocId, f32>,
    Vec<DocId>,              // node_index → DocId
    HashMap<DocId, u32>,     // DocId → node_index (as u32 for RoaringBitmap)
) {
    use petgraph::Graph;

    let mut doc_to_idx: HashMap<DocId, petgraph::graph::NodeIndex> = HashMap::new();
    let mut idx_to_doc: Vec<DocId> = Vec::new();
    let mut graph: Graph<DocId, f32> = Graph::new();

    let mut get_or_add = |g: &mut Graph<DocId, f32>,
                          id_to_doc: &mut Vec<DocId>,
                          doc_to_id: &mut HashMap<DocId, petgraph::graph::NodeIndex>,
                          doc_id: DocId|
     -> petgraph::graph::NodeIndex {
        if let Some(&idx) = doc_to_id.get(&doc_id) {
            return idx;
        }
        let idx = g.add_node(doc_id);
        id_to_doc.push(doc_id);
        doc_to_id.insert(doc_id, idx);
        idx
    };

    for edge in edges {
        let from_idx = get_or_add(&mut graph, &mut idx_to_doc, &mut doc_to_idx, edge.from);
        let to_idx = get_or_add(&mut graph, &mut idx_to_doc, &mut doc_to_idx, edge.to);
        graph.add_edge(from_idx, to_idx, edge.weight);
    }

    let idx_to_doc_u32: HashMap<DocId, u32> = idx_to_doc
        .iter()
        .enumerate()
        .map(|(i, &doc)| (doc, i as u32))
        .collect();

    (graph, idx_to_doc, idx_to_doc_u32)
}

// ── GraphQueryEngine impl ─────────────────────────────────────────

impl GraphQueryEngine for PetgraphQueryEngine {
    fn query(&self, request: GraphQueryRequest) -> Result<GraphQueryResult, GraphError> {
        let start = Instant::now();
        let (nodes, edges) = match request.op {
            GraphOp::Neighbours { from, direction } => {
                let neighbours =
                    self.store.neighbours(from, direction, &request.edge_filter)?;
                let limit = request.limit.unwrap_or(u32::MAX) as usize;
                let edge_refs: Vec<_> = neighbours
                    .iter()
                    .take(limit)
                    .map(|(_, e)| locus_core::graph::EdgeRef {
                        from: e.from,
                        to: e.to,
                        kind: e.kind.clone(),
                        weight: e.weight,
                    })
                    .collect();
                let node_refs: Vec<_> = neighbours
                    .iter()
                    .take(limit)
                    .map(|(id, _)| GraphNodeRef {
                        doc_id: *id,
                        file_path: PathBuf::new(),
                        source_type: String::new(),
                        label: None,
                        score: None,
                        hop_distance: Some(1),
                    })
                    .collect();
                (node_refs, edge_refs)
            }
            GraphOp::Expand { seeds, spec } => {
                let seed_bm: RoaringBitmap = seeds.iter().collect();
                let result = self.expand(&seed_bm, &spec)?;
                let limit = request.limit.unwrap_or(u32::MAX) as usize;
                let node_refs: Vec<_> = result
                    .iter()
                    .take(limit)
                    .map(|doc_id| GraphNodeRef {
                        doc_id,
                        file_path: PathBuf::new(),
                        source_type: String::new(),
                        label: None,
                        score: None,
                        hop_distance: None,
                    })
                    .collect();
                (node_refs, vec![])
            }
            GraphOp::ShortestPath { from, to } => {
                let path = self.shortest_path(from, to, &request.edge_filter)?;
                let node_refs: Vec<_> = path
                    .iter()
                    .map(|&doc_id| GraphNodeRef {
                        doc_id,
                        file_path: PathBuf::new(),
                        source_type: String::new(),
                        label: None,
                        score: None,
                        hop_distance: None,
                    })
                    .collect();
                (node_refs, vec![])
            }
            GraphOp::TopCentral { algorithm, restrict_to, k } => {
                let restrict_bm = restrict_to.map(|ids| {
                    let mut bm = RoaringBitmap::new();
                    for id in ids {
                        bm.insert(id);
                    }
                    bm
                });
                let scores =
                    self.centrality(algorithm, restrict_bm.as_ref())?;
                let limit = k.min(request.limit.unwrap_or(k)) as usize;
                let node_refs: Vec<_> = scores
                    .iter()
                    .take(limit)
                    .map(|&(doc_id, score)| GraphNodeRef {
                        doc_id,
                        file_path: PathBuf::new(),
                        source_type: String::new(),
                        label: None,
                        score: Some(score),
                        hop_distance: None,
                    })
                    .collect();
                (node_refs, vec![])
            }
            GraphOp::Reachable { from, direction } => {
                let result = self.reachable(from, direction, &request.edge_filter)?;
                let limit = request.limit.unwrap_or(u32::MAX) as usize;
                let node_refs: Vec<_> = result
                    .iter()
                    .take(limit)
                    .map(|doc_id| GraphNodeRef {
                        doc_id,
                        file_path: PathBuf::new(),
                        source_type: String::new(),
                        label: None,
                        score: None,
                        hop_distance: None,
                    })
                    .collect();
                (node_refs, vec![])
            }
        };

        Ok(GraphQueryResult {
            nodes,
            edges,
            elapsed_us: start.elapsed().as_micros() as u64,
        })
    }

    fn expand(
        &self,
        seeds: &RoaringBitmap,
        spec: &ExpandSpec,
    ) -> Result<RoaringBitmap, GraphError> {
        // BFS up to `spec.hops` hops from the seed set.
        let mut visited = seeds.clone();
        let mut result = if spec.include_seeds {
            seeds.clone()
        } else {
            RoaringBitmap::new()
        };

        let mut frontier: Vec<DocId> = seeds.iter().collect();

        for _ in 0..spec.hops {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier = Vec::new();
            for &node in &frontier {
                let neighbour_dir = match spec.direction {
                    Direction::Both => Direction::Both,
                    d => d,
                };
                let neighbours =
                    self.store.neighbours(node, neighbour_dir, &spec.edge_filter)?;
                for (neighbour_id, _) in neighbours {
                    if !visited.contains(neighbour_id) {
                        visited.insert(neighbour_id);
                        result.insert(neighbour_id);
                        next_frontier.push(neighbour_id);
                        if let Some(max) = spec.max_nodes {
                            if result.len() > max as u64 {
                                return Err(GraphError::ExpansionLimit(max));
                            }
                        }
                    }
                }
            }
            frontier = next_frontier;
        }

        Ok(result)
    }

    fn centrality(
        &self,
        algorithm: CentralityAlgorithm,
        restrict_to: Option<&RoaringBitmap>,
    ) -> Result<Vec<(DocId, f32)>, GraphError> {
        let all_edges = self.store.all_edges()?;

        // Filter edges to restrict_to if provided.
        let edges: Vec<&Edge> = if let Some(bm) = restrict_to {
            all_edges
                .iter()
                .filter(|e| bm.contains(e.from) && bm.contains(e.to))
                .collect()
        } else {
            all_edges.iter().collect()
        };

        match algorithm {
            CentralityAlgorithm::InDegree => {
                let mut counts: HashMap<DocId, f32> = HashMap::new();
                for edge in &edges {
                    *counts.entry(edge.to).or_insert(0.0) += 1.0;
                }
                let mut result: Vec<(DocId, f32)> = counts.into_iter().collect();
                result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                Ok(result)
            }
            CentralityAlgorithm::OutDegree => {
                let mut counts: HashMap<DocId, f32> = HashMap::new();
                for edge in &edges {
                    *counts.entry(edge.from).or_insert(0.0) += 1.0;
                }
                let mut result: Vec<(DocId, f32)> = counts.into_iter().collect();
                result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                Ok(result)
            }
            CentralityAlgorithm::PageRank { iterations, damping } => {
                let owned_edges: Vec<Edge>;
                let edge_slice: &[Edge] = if restrict_to.is_some() {
                    owned_edges = edges.iter().map(|e| (*e).clone()).collect();
                    &owned_edges
                } else {
                    &all_edges
                };

                let (graph, idx_to_doc, _) = build_petgraph(edge_slice);
                if graph.node_count() == 0 {
                    return Ok(vec![]);
                }

                let scores = petgraph::algo::page_rank(
                    &graph,
                    damping,
                    iterations as usize,
                );

                let mut result: Vec<(DocId, f32)> = idx_to_doc
                    .iter()
                    .enumerate()
                    .map(|(i, &doc_id)| (doc_id, scores.get(i).copied().unwrap_or(0.0)))
                    .collect();
                result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                Ok(result)
            }
        }
    }

    fn shortest_path(
        &self,
        from: DocId,
        to: DocId,
        filter: &EdgeFilter,
    ) -> Result<Vec<DocId>, GraphError> {
        // BFS from `from`, tracking parents to reconstruct path.
        let mut parent: HashMap<DocId, DocId> = HashMap::new();
        let mut queue = std::collections::VecDeque::new();
        parent.insert(from, from); // sentinel: from's parent is itself
        queue.push_back(from);

        while let Some(current) = queue.pop_front() {
            if current == to {
                // Reconstruct path.
                let mut path = vec![to];
                let mut node = to;
                while node != from {
                    node = parent[&node];
                    path.push(node);
                }
                path.reverse();
                return Ok(path);
            }
            let neighbours =
                self.store.neighbours(current, Direction::Outgoing, filter)?;
            for (neighbour, _) in neighbours {
                if !parent.contains_key(&neighbour) {
                    parent.insert(neighbour, current);
                    queue.push_back(neighbour);
                }
            }
        }

        Err(GraphError::NoPath(from, to))
    }

    fn reachable(
        &self,
        from: DocId,
        direction: Direction,
        filter: &EdgeFilter,
    ) -> Result<RoaringBitmap, GraphError> {
        let mut visited = RoaringBitmap::new();
        let mut stack = vec![from];
        visited.insert(from);

        while let Some(current) = stack.pop() {
            let neighbours = match direction {
                Direction::Both => {
                    let mut n = self.store.neighbours(current, Direction::Outgoing, filter)?;
                    n.extend(self.store.neighbours(current, Direction::Incoming, filter)?);
                    n
                }
                d => self.store.neighbours(current, d, filter)?,
            };
            for (neighbour, _) in neighbours {
                if !visited.contains(neighbour) {
                    visited.insert(neighbour);
                    stack.push(neighbour);
                }
            }
        }

        // Exclude the seed itself from the result (reachable = nodes you can reach FROM `from`).
        visited.remove(from);
        Ok(visited)
    }

    fn stats(&self) -> Result<GraphStats, GraphError> {
        self.store.stats()
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use locus_core::graph::{Edge, EdgeCategory, EdgeFilter, ExpandSpec};
    use locus_registry::duckdb::DuckDbRegistry;
    use locus_registry::graph::DuckDbGraphStore;

    #[test]
    fn expand_linear_chain_two_hops() {
        // 0 → 1 → 2 → 3
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        let mut gs = DuckDbGraphStore::new(registry.connection());
        for (a, b) in [(0, 1), (1, 2), (2, 3)] {
            gs.insert_edge(Edge {
                from: a,
                to: b,
                category: EdgeCategory::Reference,
                kind: "ref:wikilink".into(),
                weight: 1.0,
                byte_offset: None,
                created_at: 0,
            })
            .unwrap();
        }
        let gs: Arc<dyn GraphStore> = Arc::new(gs);
        let engine = PetgraphQueryEngine::new(gs);

        let seeds: RoaringBitmap = [0u32].iter().cloned().collect();
        let spec = ExpandSpec {
            hops: 2,
            direction: Direction::Outgoing,
            edge_filter: EdgeFilter::Any,
            max_nodes: None,
            include_seeds: true,
        };
        let result = engine.expand(&seeds, &spec).unwrap();
        // Seed=0, hop1=1, hop2=2 → {0, 1, 2}
        assert!(result.contains(0));
        assert!(result.contains(1));
        assert!(result.contains(2));
        assert!(!result.contains(3)); // 3 is 3 hops away
    }

    #[test]
    fn expand_max_nodes_cap_fires() {
        // Hub: 0 → 1..=10
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        let mut gs = DuckDbGraphStore::new(registry.connection());
        for i in 1u32..=10 {
            gs.insert_edge(Edge {
                from: 0,
                to: i,
                category: EdgeCategory::Reference,
                kind: "ref:wikilink".into(),
                weight: 1.0,
                byte_offset: None,
                created_at: 0,
            })
            .unwrap();
        }
        let gs: Arc<dyn GraphStore> = Arc::new(gs);
        let engine = PetgraphQueryEngine::new(gs);

        let seeds: RoaringBitmap = [0u32].iter().cloned().collect();
        let spec = ExpandSpec {
            hops: 1,
            direction: Direction::Outgoing,
            edge_filter: EdgeFilter::Any,
            max_nodes: Some(3), // cap at 3 nodes total
            include_seeds: true,
        };
        let err = engine.expand(&seeds, &spec).unwrap_err();
        assert!(matches!(err, GraphError::ExpansionLimit(3)));
    }

    #[test]
    fn shortest_path_finds_correct_path() {
        // 0 → 1 → 2 → 3; also 0 → 3 (shortcut)
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        let mut gs = DuckDbGraphStore::new(registry.connection());
        for (a, b) in [(0, 1), (1, 2), (2, 3), (0, 3)] {
            gs.insert_edge(Edge {
                from: a,
                to: b,
                category: EdgeCategory::Reference,
                kind: "ref:wikilink".into(),
                weight: 1.0,
                byte_offset: None,
                created_at: 0,
            })
            .unwrap();
        }
        let gs: Arc<dyn GraphStore> = Arc::new(gs);
        let engine = PetgraphQueryEngine::new(gs);

        let path = engine
            .shortest_path(0, 3, &EdgeFilter::Any)
            .unwrap();
        // BFS finds shortest: 0 → 3 (1 hop)
        assert_eq!(path, vec![0, 3]);
    }

    #[test]
    fn shortest_path_no_path_returns_error() {
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        let gs: Arc<dyn GraphStore> = Arc::new(DuckDbGraphStore::new(registry.connection()));
        let engine = PetgraphQueryEngine::new(gs);

        let err = engine.shortest_path(0, 99, &EdgeFilter::Any).unwrap_err();
        assert!(matches!(err, GraphError::NoPath(0, 99)));
    }

    #[test]
    fn reachable_finds_transitive_closure() {
        // 0 → 1 → 2, 0 → 3
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        let mut gs = DuckDbGraphStore::new(registry.connection());
        for (a, b) in [(0, 1), (1, 2), (0, 3)] {
            gs.insert_edge(Edge {
                from: a,
                to: b,
                category: EdgeCategory::Reference,
                kind: "ref:wikilink".into(),
                weight: 1.0,
                byte_offset: None,
                created_at: 0,
            })
            .unwrap();
        }
        let gs: Arc<dyn GraphStore> = Arc::new(gs);
        let engine = PetgraphQueryEngine::new(gs);

        let reachable = engine
            .reachable(0, Direction::Outgoing, &EdgeFilter::Any)
            .unwrap();
        assert!(reachable.contains(1));
        assert!(reachable.contains(2));
        assert!(reachable.contains(3));
        assert!(!reachable.contains(0)); // seed excluded
    }

    #[test]
    fn centrality_pagerank_hub_node_scores_highest() {
        // Three nodes all pointing to node 0 (hub).
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        let mut gs = DuckDbGraphStore::new(registry.connection());
        for src in [1u32, 2, 3] {
            gs.insert_edge(Edge {
                from: src,
                to: 0,
                category: EdgeCategory::Reference,
                kind: "ref:wikilink".into(),
                weight: 1.0,
                byte_offset: None,
                created_at: 0,
            })
            .unwrap();
        }
        let gs: Arc<dyn GraphStore> = Arc::new(gs);
        let engine = PetgraphQueryEngine::new(gs);

        let scores = engine
            .centrality(CentralityAlgorithm::PageRank { iterations: 100, damping: 0.85 }, None)
            .unwrap();

        // Node 0 should have the highest score.
        assert!(!scores.is_empty());
        assert_eq!(scores[0].0, 0, "node 0 should rank first");
    }

    #[test]
    fn centrality_indegree_counts_correctly() {
        // 1 → 0, 2 → 0, 3 → 1
        let registry = DuckDbRegistry::new(":memory:").unwrap();
        let mut gs = DuckDbGraphStore::new(registry.connection());
        for (a, b) in [(1u32, 0), (2, 0), (3, 1)] {
            gs.insert_edge(Edge {
                from: a,
                to: b,
                category: EdgeCategory::Reference,
                kind: "ref:wikilink".into(),
                weight: 1.0,
                byte_offset: None,
                created_at: 0,
            })
            .unwrap();
        }
        let gs: Arc<dyn GraphStore> = Arc::new(gs);
        let engine = PetgraphQueryEngine::new(gs);

        let scores = engine
            .centrality(CentralityAlgorithm::InDegree, None)
            .unwrap();
        // Node 0 has in-degree 2, node 1 has in-degree 1.
        assert_eq!(scores[0].0, 0);
        assert!((scores[0].1 - 2.0).abs() < 0.01);
    }
}
