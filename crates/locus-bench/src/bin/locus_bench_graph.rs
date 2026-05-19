//! Phase 4 benchmark — graph layer: rebuild, BFS expansion, centrality, shortest path.
//!
//! Generates synthetic in-memory DuckDB graphs at increasing scales and measures:
//! - Graph rebuild (DuckDB row scan → petgraph population)
//! - BFS expansion (1-hop, 2-hop, 3-hop) per query
//! - Centrality (in-degree, out-degree, PageRank)
//! - Shortest path search
//!
//! Usage: cargo run --release --bin locus-bench-graph

use std::sync::Arc;
use std::time::Instant;

use locus_core::graph::{
    CentralityAlgorithm, Direction, Edge, EdgeCategory, EdgeFilter, ExpandSpec, GraphOp,
    GraphQueryEngine, GraphQueryRequest, GraphStore,
};
use locus_core::types::DocId;
use locus_query::PetgraphQueryEngine;
use locus_registry::duckdb::DuckDbRegistry;
use locus_registry::graph::DuckDbGraphStore;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

// ── Helpers ──────────────────────────────────────────────────────

fn median_us(times: &mut Vec<u64>) -> u64 {
    times.sort();
    times[times.len() / 2]
}

fn make_edge(from: DocId, to: DocId, category: EdgeCategory) -> Edge {
    Edge {
        from,
        to,
        category,
        kind: "ref:wikilink".into(),
        weight: 1.0,
        byte_offset: None,
        created_at: 1_700_000_000,
    }
}

/// Build an in-memory DuckDB graph with `n_docs` nodes and ~`n_docs * avg_degree` edges.
fn generate_graph(n_docs: u32, avg_degree: u32, seed: u64) -> (DuckDbRegistry, DuckDbGraphStore) {
    let registry = DuckDbRegistry::new(":memory:").unwrap();
    let mut gs = DuckDbGraphStore::new(registry.connection());

    let mut rng = StdRng::seed_from_u64(seed);
    let mut edges = Vec::new();

    for from in 1..=n_docs {
        let degree = rng.gen_range(1..=(avg_degree * 2));
        for _ in 0..degree {
            let to = rng.gen_range(1..=n_docs);
            if to == from {
                continue;
            }
            let cat = match rng.gen_range(0u8..3) {
                0 => EdgeCategory::Reference,
                1 => EdgeCategory::Dependency,
                _ => EdgeCategory::Hierarchy,
            };
            edges.push(make_edge(from, to, cat));
        }
    }

    gs.bulk_insert_edges(edges).unwrap();
    (registry, gs)
}

// ── Individual benchmarks ─────────────────────────────────────────

fn bench_rebuild(n_docs: u32, avg_degree: u32) {
    let (_reg, mut gs) = generate_graph(n_docs, avg_degree, 42);
    let mut times = Vec::new();
    for _ in 0..5 {
        gs.drop_in_memory();
        let t = Instant::now();
        gs.rebuild_in_memory().unwrap();
        times.push(t.elapsed().as_micros() as u64);
    }
    println!(
        "  rebuild  {:>6} docs / ~{:>6} edges: {:>6}µs median",
        n_docs,
        n_docs * avg_degree,
        median_us(&mut times),
    );
}

fn bench_expansion(engine: &PetgraphQueryEngine, n_docs: u32, hops: u8, n_iter: usize) {
    let mut rng = StdRng::seed_from_u64(99);
    let mut times = Vec::new();

    for _ in 0..n_iter {
        let seed_id = rng.gen_range(1..=n_docs);
        let spec = ExpandSpec {
            hops,
            direction: Direction::Outgoing,
            edge_filter: EdgeFilter::Any,
            max_nodes: Some(1000),
            include_seeds: false,
        };
        let t = Instant::now();
        let _ = engine.query(GraphQueryRequest {
            op: GraphOp::Expand { seeds: vec![seed_id], spec },
            edge_filter: EdgeFilter::Any,
            limit: None,
        });
        times.push(t.elapsed().as_micros() as u64);
    }

    println!(
        "  expand  {hops}-hop  {n_iter:>4} iters: {:>6}µs median",
        median_us(&mut times),
    );
}

fn bench_centrality(engine: &PetgraphQueryEngine, algo_name: &str, algo: CentralityAlgorithm) {
    let mut times = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let _ = engine.centrality(algo, None);
        times.push(t.elapsed().as_micros() as u64);
    }
    println!(
        "  centrality  {:>10}: {:>6}µs median",
        algo_name,
        median_us(&mut times),
    );
}

fn bench_shortest_path(engine: &PetgraphQueryEngine, n_docs: u32, n_iter: usize) {
    let mut rng = StdRng::seed_from_u64(7);
    let mut times = Vec::new();
    let mut found = 0usize;

    for _ in 0..n_iter {
        let from = rng.gen_range(1..=n_docs);
        let to = rng.gen_range(1..=n_docs);
        let t = Instant::now();
        let result = engine.shortest_path(from, to, &EdgeFilter::Any);
        times.push(t.elapsed().as_micros() as u64);
        if result.is_ok() {
            found += 1;
        }
    }

    println!(
        "  shortest_path  {n_iter:>4} iters ({found} found): {:>6}µs median",
        median_us(&mut times),
    );
}

fn run_suite(label: &str, n_docs: u32, avg_degree: u32) {
    println!("{label} ({n_docs} docs, ~{} edges):", n_docs * avg_degree);

    let (_reg, mut gs) = generate_graph(n_docs, avg_degree, 42);
    gs.rebuild_in_memory().unwrap();
    let gs_arc: Arc<dyn GraphStore> = Arc::new(gs);
    let engine = PetgraphQueryEngine::new(gs_arc);

    bench_expansion(&engine, n_docs, 1, 200);
    bench_expansion(&engine, n_docs, 2, 100);
    bench_expansion(&engine, n_docs, 3, 50);
    bench_centrality(&engine, "in-degree", CentralityAlgorithm::InDegree);
    bench_centrality(&engine, "out-degree", CentralityAlgorithm::OutDegree);
    bench_centrality(
        &engine,
        "pagerank",
        CentralityAlgorithm::PageRank { iterations: 100, damping: 0.85 },
    );
    bench_shortest_path(&engine, n_docs, 200);
    println!();
}

fn main() {
    println!("=== Locus Phase 4 — Graph Layer Benchmarks ===\n");

    // ── Rebuild scaling ───────────────────────────────────────────
    println!("Graph rebuild (DuckDB rows → petgraph):");
    bench_rebuild(1_000, 3);
    bench_rebuild(5_000, 3);
    bench_rebuild(10_000, 3);
    bench_rebuild(50_000, 3);
    println!();

    // ── Operation latency at increasing scale ─────────────────────
    run_suite("Small graph", 1_000, 3);
    run_suite("Medium graph", 5_000, 3);
    run_suite("Large graph", 10_000, 3);

    println!("Done.");
}
