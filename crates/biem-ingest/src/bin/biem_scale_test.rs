//! Scale test — measures BIEM performance at large vault sizes.
//!
//! Outputs JSON results to stdout for the report generator.
//! Usage: cargo run --release --bin biem-scale-test [-- --max-files 100000] [--skip-slow]

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use biem_bitmap::memory::InMemoryBitmapStore;
use biem_core::parser::Parser;
use biem_core::query::{Filter, QueryEngine, QueryRequest};
use biem_ingest::vault_gen::{generate_vault, vault_size_bytes, VaultConfig};
use biem_ingest::IngestionPipeline;
use biem_parser::markdown::MarkdownParser;
use biem_query::BitmapQueryEngine;
use biem_registry::memory::InMemoryRegistry;
use petgraph::graph::{Graph, NodeIndex};

fn make_pipeline() -> IngestionPipeline {
    IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        Box::new(InMemoryRegistry::new()),
        Box::new(InMemoryBitmapStore::new()),
    )
}

#[derive(serde::Serialize)]
struct ScaleResult {
    num_files: usize,
    vault_size_mb: f64,
    generate_ms: u64,
    index_ms: u64,
    reindex_ms: u64,
    docs_indexed: u32,
    docs_skipped: u32,
    bitmaps_created: u32,
    query_single_us: u64,
    query_and_us: u64,
    query_or_us: u64,
    index_throughput_files_per_sec: f64,
    index_throughput_mb_per_sec: f64,
    // Baseline comparisons
    baseline_grep_us: u64,
    baseline_parse_filter_us: u64,
    baseline_hashmap_us: u64,
    baseline_hashset_single_us: u64,
    baseline_hashset_and_us: u64,
    baseline_sql_single_us: u64,
    baseline_sql_and_us: u64,
    baseline_graph_single_ns: u64,
    baseline_graph_and_ns: u64,
    baseline_graph_build_ms: u64,
}

fn run_scale(n: usize, skip_slow: bool) -> ScaleResult {
    eprintln!("── {n} files ──");

    // Generate vault
    eprintln!("  generating vault...");
    let dir = tempfile::tempdir().unwrap();
    let gen_start = Instant::now();
    generate_vault(
        dir.path(),
        &VaultConfig {
            num_files: n,
            ..Default::default()
        },
    );
    let generate_ms = gen_start.elapsed().as_millis() as u64;
    let vault_bytes = vault_size_bytes(dir.path());
    let vault_size_mb = vault_bytes as f64 / (1024.0 * 1024.0);
    eprintln!("  vault: {vault_size_mb:.1} MB ({n} files) in {generate_ms}ms");

    // First index
    eprintln!("  indexing...");
    let mut pipeline = make_pipeline();
    let idx_start = Instant::now();
    let result = pipeline.bulk_index(dir.path()).unwrap();
    let index_ms = idx_start.elapsed().as_millis() as u64;
    let index_throughput_files = n as f64 / (index_ms as f64 / 1000.0);
    let index_throughput_mb = vault_size_mb / (index_ms as f64 / 1000.0);
    eprintln!(
        "  indexed: {} docs, {} bitmaps in {}ms ({:.0} files/s, {:.1} MB/s)",
        result.docs_indexed, result.bitmaps_created, index_ms,
        index_throughput_files, index_throughput_mb
    );

    // Re-index (should skip all)
    eprintln!("  re-indexing (idempotent)...");
    let reidx_start = Instant::now();
    let reidx_result = pipeline.bulk_index(dir.path()).unwrap();
    let reindex_ms = reidx_start.elapsed().as_millis() as u64;
    eprintln!(
        "  re-index: {} skipped, {} indexed in {}ms",
        reidx_result.docs_skipped, reidx_result.docs_indexed, reindex_ms
    );

    // Queries
    eprintln!("  running queries...");
    let (_, registry, bitmap_store) = pipeline.into_parts();
    let engine = BitmapQueryEngine::new(bitmap_store, registry);

    let query_single_us = bench_query_us(&engine, Filter::Key("tag:work".into()));
    let query_and_us = bench_query_us(
        &engine,
        Filter::And(vec![
            Filter::Key("tag:project".into()),
            Filter::Key("source:obsidian".into()),
        ]),
    );
    let query_or_us = bench_query_us(
        &engine,
        Filter::Or(vec![
            Filter::Key("tag:project".into()),
            Filter::Key("tag:meeting".into()),
            Filter::Key("tag:research".into()),
        ]),
    );
    eprintln!(
        "  queries: single={query_single_us}µs, AND={query_and_us}µs, OR={query_or_us}µs"
    );

    // Baseline: grep (scan every file for tag string)
    let baseline_grep_us = if skip_slow {
        eprintln!("  skipping baseline grep (--skip-slow)");
        0
    } else {
        eprintln!("  running baseline grep...");
        bench_baseline_grep(dir.path())
    };

    // Baseline: parse + filter (parse every file, check parsed tags)
    let baseline_parse_filter_us = if skip_slow {
        eprintln!("  skipping baseline parse+filter (--skip-slow)");
        0
    } else {
        eprintln!("  running baseline parse filter...");
        bench_baseline_parse_filter(dir.path())
    };

    // Baseline: hashmap inverted index (build once, then lookup)
    eprintln!("  running baseline hashmap...");
    let baseline_hashmap_us = bench_baseline_hashmap(dir.path());

    // Baseline: HashSet inverted index (single key + AND intersection)
    eprintln!("  running baseline hashset...");
    let (baseline_hashset_single_us, baseline_hashset_and_us) = bench_baseline_hashset(dir.path());

    // Baseline: SQL via DuckDB (single + AND)
    eprintln!("  running baseline SQL...");
    let (baseline_sql_single_us, baseline_sql_and_us) = bench_baseline_sql(dir.path());

    // Baseline: Graph via petgraph (single + AND)
    eprintln!("  running baseline Graph...");
    let (baseline_graph_single_ns, baseline_graph_and_ns, baseline_graph_build_ms) = bench_baseline_graph(dir.path());

    eprintln!(
        "  baselines: grep={baseline_grep_us}µs, parse+filter={baseline_parse_filter_us}µs, hashmap={baseline_hashmap_us}µs"
    );
    eprintln!(
        "  baselines: hashset_1={baseline_hashset_single_us}µs, hashset_AND={baseline_hashset_and_us}µs, sql_1={baseline_sql_single_us}µs, sql_AND={baseline_sql_and_us}µs"
    );
    eprintln!(
        "  baselines: graph_1={baseline_graph_single_ns}ns, graph_AND={baseline_graph_and_ns}ns"
    );

    ScaleResult {
        num_files: n,
        vault_size_mb,
        generate_ms,
        index_ms,
        reindex_ms,
        docs_indexed: result.docs_indexed,
        docs_skipped: reidx_result.docs_skipped,
        bitmaps_created: result.bitmaps_created,
        query_single_us,
        query_and_us,
        query_or_us,
        index_throughput_files_per_sec: index_throughput_files,
        index_throughput_mb_per_sec: index_throughput_mb,
        baseline_grep_us,
        baseline_parse_filter_us,
        baseline_hashmap_us,
        baseline_hashset_single_us,
        baseline_hashset_and_us,
        baseline_sql_single_us,
        baseline_sql_and_us,
        baseline_graph_single_ns,
        baseline_graph_and_ns,
        baseline_graph_build_ms,
    }
}

fn run_scale_old(n: usize) -> ScaleResult {
    eprintln!("── {n} files ──");

    // Generate vault
    eprintln!("  generating vault...");
    let dir = tempfile::tempdir().unwrap();
    let gen_start = Instant::now();
    generate_vault(
        dir.path(),
        &VaultConfig {
            num_files: n,
            ..Default::default()
        },
    );
    let generate_ms = gen_start.elapsed().as_millis() as u64;
    let vault_bytes = vault_size_bytes(dir.path());
    let vault_size_mb = vault_bytes as f64 / (1024.0 * 1024.0);
    eprintln!("  vault: {vault_size_mb:.1} MB ({n} files) in {generate_ms}ms");

    // First index
    eprintln!("  indexing...");
    let mut pipeline = make_pipeline();
    let idx_start = Instant::now();
    let result = pipeline.bulk_index(dir.path()).unwrap();
    let index_ms = idx_start.elapsed().as_millis() as u64;
    let index_throughput_files = n as f64 / (index_ms as f64 / 1000.0);
    let index_throughput_mb = vault_size_mb / (index_ms as f64 / 1000.0);
    eprintln!(
        "  indexed: {} docs, {} bitmaps in {}ms ({:.0} files/s, {:.1} MB/s)",
        result.docs_indexed, result.bitmaps_created, index_ms,
        index_throughput_files, index_throughput_mb
    );

    // Re-index (should skip all)
    eprintln!("  re-indexing (idempotent)...");
    let reidx_start = Instant::now();
    let reidx_result = pipeline.bulk_index(dir.path()).unwrap();
    let reindex_ms = reidx_start.elapsed().as_millis() as u64;
    eprintln!(
        "  re-index: {} skipped, {} indexed in {}ms",
        reidx_result.docs_skipped, reidx_result.docs_indexed, reindex_ms
    );

    // Queries
    eprintln!("  running queries...");
    let (_, registry, bitmap_store) = pipeline.into_parts();
    let engine = BitmapQueryEngine::new(bitmap_store, registry);

    let query_single_us = bench_query_us(&engine, Filter::Key("tag:work".into()));
    let query_and_us = bench_query_us(
        &engine,
        Filter::And(vec![
            Filter::Key("tag:project".into()),
            Filter::Key("source:obsidian".into()),
        ]),
    );
    let query_or_us = bench_query_us(
        &engine,
        Filter::Or(vec![
            Filter::Key("tag:project".into()),
            Filter::Key("tag:meeting".into()),
            Filter::Key("tag:research".into()),
        ]),
    );
    eprintln!(
        "  queries: single={query_single_us}µs, AND={query_and_us}µs, OR={query_or_us}µs"
    );

    // Baseline: grep (scan every file for tag string)
    eprintln!("  running baseline grep...");
    let baseline_grep_us = bench_baseline_grep(dir.path());

    // Baseline: parse + filter (parse every file, check parsed tags)
    eprintln!("  running baseline parse filter...");
    let baseline_parse_filter_us = bench_baseline_parse_filter(dir.path());

    // Baseline: hashmap inverted index (build once, then lookup)
    eprintln!("  running baseline hashmap...");
    let baseline_hashmap_us = bench_baseline_hashmap(dir.path());

    // Baseline: HashSet inverted index (single key + AND intersection)
    eprintln!("  running baseline hashset...");
    let (baseline_hashset_single_us, baseline_hashset_and_us) = bench_baseline_hashset(dir.path());

    // Baseline: SQL via DuckDB (single + AND)
    eprintln!("  running baseline SQL...");
    let (baseline_sql_single_us, baseline_sql_and_us) = bench_baseline_sql(dir.path());

    eprintln!(
        "  baselines: grep={baseline_grep_us}µs, parse+filter={baseline_parse_filter_us}µs, hashmap={baseline_hashmap_us}µs"
    );
    // Baseline: petgraph (property graph, neighbor intersection)
    eprintln!("  running baseline graph...");
    let (baseline_graph_single_ns, baseline_graph_and_ns, baseline_graph_build_ms) = bench_baseline_graph(dir.path());

    eprintln!(
        "  baselines: hashset_1={baseline_hashset_single_us}µs, hashset_AND={baseline_hashset_and_us}µs, sql_1={baseline_sql_single_us}µs, sql_AND={baseline_sql_and_us}µs"
    );
    eprintln!(
        "  baselines: graph_1={baseline_graph_single_ns}ns, graph_AND={baseline_graph_and_ns}ns"
    );

    ScaleResult {
        num_files: n,
        vault_size_mb,
        generate_ms,
        index_ms,
        reindex_ms,
        docs_indexed: result.docs_indexed,
        docs_skipped: reidx_result.docs_skipped,
        bitmaps_created: result.bitmaps_created,
        query_single_us,
        query_and_us,
        query_or_us,
        index_throughput_files_per_sec: index_throughput_files,
        index_throughput_mb_per_sec: index_throughput_mb,
        baseline_grep_us,
        baseline_parse_filter_us,
        baseline_hashmap_us,
        baseline_hashset_single_us,
        baseline_hashset_and_us,
        baseline_sql_single_us,
        baseline_sql_and_us,
        baseline_graph_single_ns,
        baseline_graph_and_ns,
        baseline_graph_build_ms,
    }
}

fn bench_query_us(engine: &BitmapQueryEngine, filter: Filter) -> u64 {
    // Warm up
    for _ in 0..10 {
        let _ = engine.query(QueryRequest {
            filter: filter.clone(),
            limit: Some(20),
            offset: None,
        });
    }
    // Measure median of 100 runs
    let mut times = Vec::with_capacity(100);
    for _ in 0..100 {
        let start = Instant::now();
        let _ = engine.query(QueryRequest {
            filter: filter.clone(),
            limit: Some(20),
            offset: None,
        });
        times.push(start.elapsed().as_micros() as u64);
    }
    times.sort();
    times[times.len() / 2] // median
}

fn bench_baseline_grep(vault_root: &std::path::Path) -> u64 {
    // Simulate `grep -rl "work" vault/` — read every file, check if bytes contain "work"
    let mut files = Vec::new();
    collect_md_files(vault_root, &mut files);

    // Pre-warm filesystem cache
    for f in &files {
        let _ = std::fs::read(f);
    }

    let mut times = Vec::with_capacity(20);
    for _ in 0..20 {
        let start = Instant::now();
        let mut _matches = 0u32;
        for f in &files {
            let content = std::fs::read(f).unwrap();
            if content.windows(4).any(|w| w == b"work") {
                _matches += 1;
            }
        }
        times.push(start.elapsed().as_micros() as u64);
    }
    times.sort();
    times[times.len() / 2]
}

fn bench_baseline_parse_filter(vault_root: &std::path::Path) -> u64 {
    // Read every file, parse with MarkdownParser, check if tags contain "work"
    let parser = MarkdownParser;
    let mut files = Vec::new();
    collect_md_files(vault_root, &mut files);

    // Pre-warm
    for f in &files {
        let _ = std::fs::read(f);
    }

    let mut times = Vec::with_capacity(10);
    for _ in 0..10 {
        let start = Instant::now();
        let mut _matches = 0u32;
        for f in &files {
            let content = std::fs::read(f).unwrap();
            if let Ok(result) = parser.parse(f, &content) {
                if result.tags.iter().any(|t| t == "work") {
                    _matches += 1;
                }
            }
        }
        times.push(start.elapsed().as_micros() as u64);
    }
    times.sort();
    times[times.len() / 2]
}

fn bench_baseline_hashmap(vault_root: &std::path::Path) -> u64 {
    // Build an inverted HashMap<tag, Vec<PathBuf>>, then look up "work"
    let parser = MarkdownParser;
    let mut files = Vec::new();
    collect_md_files(vault_root, &mut files);

    // Build the index (one-time cost, not measured)
    let mut index: HashMap<String, Vec<std::path::PathBuf>> = HashMap::new();
    for f in &files {
        let content = std::fs::read(f).unwrap();
        if let Ok(result) = parser.parse(f, &content) {
            for tag in &result.tags {
                index.entry(tag.clone()).or_default().push(f.clone());
            }
        }
    }

    // Measure lookup only
    let mut times = Vec::with_capacity(100);
    for _ in 0..100 {
        let start = Instant::now();
        let _matches = index.get("work").map(|v| v.len()).unwrap_or(0);
        times.push(start.elapsed().as_micros() as u64);
    }
    times.sort();
    times[times.len() / 2]
}

fn bench_baseline_hashset(vault_root: &std::path::Path) -> (u64, u64) {
    // Build an inverted index: tag → HashSet<u32> (doc IDs, like BIEM but without Roaring)
    let parser = MarkdownParser;
    let mut files = Vec::new();
    collect_md_files(vault_root, &mut files);

    let mut index: HashMap<String, HashSet<u32>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        let content = std::fs::read(f).unwrap();
        if let Ok(result) = parser.parse(f, &content) {
            for tag in &result.tags {
                index.entry(format!("tag:{tag}")).or_default().insert(i as u32);
            }
            index.entry("source:obsidian".into()).or_default().insert(i as u32);
        }
    }

    // Single key lookup
    let single_us = {
        let mut times = Vec::with_capacity(100);
        for _ in 0..100 {
            let start = Instant::now();
            let _result: Vec<u32> = index.get("tag:work")
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            times.push(start.elapsed().as_nanos() as u64);
        }
        times.sort();
        times[times.len() / 2] / 1000 // convert ns → µs
    };

    // AND intersection: tag:project ∩ source:obsidian
    let and_us = {
        let mut times = Vec::with_capacity(100);
        for _ in 0..100 {
            let start = Instant::now();
            let a = index.get("tag:project");
            let b = index.get("source:obsidian");
            let _result: Vec<u32> = match (a, b) {
                (Some(sa), Some(sb)) => sa.intersection(sb).copied().collect(),
                _ => vec![],
            };
            times.push(start.elapsed().as_nanos() as u64);
        }
        times.sort();
        times[times.len() / 2] / 1000
    };

    (single_us, and_us)
}

fn bench_baseline_sql(vault_root: &std::path::Path) -> (u64, u64) {
    // Build a DuckDB in-memory database with a normalized tag table, then query it
    let parser = MarkdownParser;
    let mut files = Vec::new();
    collect_md_files(vault_root, &mut files);

    let db = duckdb::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE documents (doc_id INTEGER PRIMARY KEY, file_path TEXT);
         CREATE TABLE doc_tags (doc_id INTEGER, tag TEXT);
         CREATE INDEX idx_doc_tags_tag ON doc_tags(tag);"
    ).unwrap();

    for (i, f) in files.iter().enumerate() {
        let content = std::fs::read(f).unwrap();
        let doc_id = i as u32;
        db.execute(
            "INSERT INTO documents VALUES (?, ?)",
            duckdb::params![doc_id, f.to_string_lossy().to_string()],
        ).unwrap();
        if let Ok(result) = parser.parse(f, &content) {
            for tag in &result.tags {
                db.execute(
                    "INSERT INTO doc_tags VALUES (?, ?)",
                    duckdb::params![doc_id, tag.as_str()],
                ).unwrap();
            }
        }
    }

    // Single key query
    let single_us = {
        let mut times = Vec::with_capacity(100);
        for _ in 0..100 {
            let start = Instant::now();
            let mut stmt = db.prepare_cached(
                "SELECT d.doc_id, d.file_path FROM documents d
                 JOIN doc_tags t ON d.doc_id = t.doc_id WHERE t.tag = 'work' LIMIT 20"
            ).unwrap();
            let _rows: Vec<(u32, String)> = stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?))
            }).unwrap().filter_map(|r| r.ok()).collect();
            times.push(start.elapsed().as_micros() as u64);
        }
        times.sort();
        times[times.len() / 2]
    };

    // AND query: tag = 'project' AND tag = 'work' (docs that have BOTH tags)
    let and_us = {
        let mut times = Vec::with_capacity(100);
        for _ in 0..100 {
            let start = Instant::now();
            let mut stmt = db.prepare_cached(
                "SELECT d.doc_id, d.file_path FROM documents d
                 WHERE d.doc_id IN (SELECT doc_id FROM doc_tags WHERE tag = 'project')
                   AND d.doc_id IN (SELECT doc_id FROM doc_tags WHERE tag = 'work')
                 LIMIT 20"
            ).unwrap();
            let _rows: Vec<(u32, String)> = stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?))
            }).unwrap().filter_map(|r| r.ok()).collect();
            times.push(start.elapsed().as_micros() as u64);
        }
        times.sort();
        times[times.len() / 2]
    };

    (single_us, and_us)
}

fn bench_baseline_graph(vault_root: &std::path::Path) -> (u64, u64, u64) {
    // Build a property graph: doc nodes + tag nodes, edges between them.
    // Query = "find all neighbors of tag node X" (single) or intersect neighbors of two tag nodes (AND).
    let parser = MarkdownParser;
    let mut files = Vec::new();
    collect_md_files(vault_root, &mut files);

    // Measure build time (parse + graph construction)
    let build_start = Instant::now();
    let mut graph = Graph::<String, (), petgraph::Undirected>::new_undirected();
    let mut tag_nodes: HashMap<String, NodeIndex> = HashMap::new();
    let mut _doc_nodes: Vec<NodeIndex> = Vec::with_capacity(files.len());

    for f in &files {
        let doc_node = graph.add_node(f.to_string_lossy().to_string());
        _doc_nodes.push(doc_node);
        let content = std::fs::read(f).unwrap();
        if let Ok(result) = parser.parse(f, &content) {
            for tag in &result.tags {
                let tag_key = format!("tag:{tag}");
                let tag_node = *tag_nodes.entry(tag_key.clone())
                    .or_insert_with(|| graph.add_node(tag_key));
                graph.add_edge(doc_node, tag_node, ());
            }
            // Add source:obsidian edge
            let src_key = "source:obsidian".to_string();
            let src_node = *tag_nodes.entry(src_key.clone())
                .or_insert_with(|| graph.add_node(src_key));
            graph.add_edge(doc_node, src_node, ());
        }
    }

    let build_ms = build_start.elapsed().as_millis() as u64;

    let work_node = tag_nodes.get("tag:work").copied();
    let project_node = tag_nodes.get("tag:project").copied();
    let obsidian_node = tag_nodes.get("source:obsidian").copied();

    eprintln!("  graph: {} nodes, {} edges, build={}ms, work_neighbors={}, obsidian_neighbors={}",
        graph.node_count(), graph.edge_count(), build_ms,
        work_node.map(|n| graph.neighbors(n).count()).unwrap_or(0),
        obsidian_node.map(|n| graph.neighbors(n).count()).unwrap_or(0),
    );

    // Single key: find all neighbors of "tag:work" node — batch 1000 iterations
    let single_ns = {
        // warm up
        for _ in 0..10 {
            let r: Vec<NodeIndex> = work_node
                .map(|n| graph.neighbors(n).collect())
                .unwrap_or_default();
            std::hint::black_box(&r);
        }
        let start = Instant::now();
        for _ in 0..1000 {
            let r: Vec<NodeIndex> = work_node
                .map(|n| graph.neighbors(n).collect())
                .unwrap_or_default();
            std::hint::black_box(&r);
        }
        start.elapsed().as_nanos() as u64 / 1000
    };

    // AND: neighbors of "tag:project" ∩ neighbors of "source:obsidian" — batch 100 iterations
    let and_ns = {
        for _ in 0..5 {
            let a: HashSet<NodeIndex> = project_node
                .map(|n| graph.neighbors(n).collect())
                .unwrap_or_default();
            let b: HashSet<NodeIndex> = obsidian_node
                .map(|n| graph.neighbors(n).collect())
                .unwrap_or_default();
            let r: Vec<NodeIndex> = a.intersection(&b).copied().collect();
            std::hint::black_box(&r);
        }
        let start = Instant::now();
        for _ in 0..100 {
            let a: HashSet<NodeIndex> = project_node
                .map(|n| graph.neighbors(n).collect())
                .unwrap_or_default();
            let b: HashSet<NodeIndex> = obsidian_node
                .map(|n| graph.neighbors(n).collect())
                .unwrap_or_default();
            let r: Vec<NodeIndex> = a.intersection(&b).copied().collect();
            std::hint::black_box(&r);
        }
        start.elapsed().as_nanos() as u64 / 100
    };

    (single_ns, and_ns, build_ms)
}

fn collect_md_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_md_files(&path, out);
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                out.push(path);
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let max_files: usize = args.iter()
        .position(|a| a == "--max-files")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    let skip_slow = args.iter().any(|a| a == "--skip-slow");

    let scales: Vec<usize> = [100, 500, 1_000, 5_000, 10_000, 50_000, 100_000, 500_000, 1_000_000]
        .into_iter()
        .filter(|&n| n <= max_files)
        .collect();

    eprintln!("BIEM Scale Test — testing {} sizes up to {max_files}{}", scales.len(),
        if skip_slow { " (skipping slow baselines)" } else { "" });
    eprintln!();

    let mut results = Vec::new();
    for &n in &scales {
        results.push(run_scale(n, skip_slow));
        eprintln!();
    }

    // Output JSON to stdout
    println!("{}", serde_json::to_string_pretty(&results).unwrap());
}
