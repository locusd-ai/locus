//! Phase 2/3 benchmark — measures enrichment, code parsing, semantic query, and on-disk query.
//!
//! Usage: cargo run --release --bin locus-bench-phase2

use std::path::Path;
use std::time::Instant;

use locus_bitmap::lmdb::LmdbBitmapStore;
use locus_bitmap::memory::InMemoryBitmapStore;
use locus_code::CodeParser;
use locus_core::parser::Parser;
use locus_core::query::{Filter, QueryEngine, QueryRequest};
use locus_core::semantic::{EmbeddingVector, VectorStore};
use locus_core::types::SourceType;
use locus_embed::InMemoryVectorStore;
use locus_enrich::builtin::{ComplexityTagger, ConventionTagger, SizeTagger, TopicTagger};
use locus_enrich::{InMemoryTaggerCache, TagPipeline};
use locus_bench::vault_gen::{generate_vault, VaultConfig};
use locus_ingest::IngestionPipeline;
use locus_parser::markdown::MarkdownParser;
use locus_query::{BitmapQueryEngine, SemanticQueryRequest};
use locus_registry::duckdb::DuckDbRegistry;
use locus_registry::memory::InMemoryRegistry;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn median_us(times: &mut Vec<u64>) -> u64 {
    times.sort();
    times[times.len() / 2]
}

fn random_vector(dim: usize, rng: &mut StdRng) -> EmbeddingVector {
    let v: Vec<f32> = (0..dim).map(|_| rng.gen::<f32>() * 2.0 - 1.0).collect();
    // L2-normalise so cosine similarity is meaningful
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.into_iter().map(|x| x / norm.max(1e-8)).collect()
}

/// Generate synthetic Rust source files in a temp dir.
fn generate_rust_files(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    for i in 0..n {
        let content = format!(
            r#"//! Module {i}

use std::collections::HashMap;

pub struct Config{i} {{
    pub name: String,
    pub value: u64,
    pub enabled: bool,
}}

impl Config{i} {{
    pub fn new(name: &str, value: u64) -> Self {{
        Self {{ name: name.to_string(), value, enabled: true }}
    }}

    pub fn is_valid(&self) -> bool {{
        !self.name.is_empty() && self.value > 0
    }}

    pub fn to_map(&self) -> HashMap<String, String> {{
        let mut m = HashMap::new();
        m.insert("name".into(), self.name.clone());
        m.insert("value".into(), self.value.to_string());
        m
    }}
}}

pub fn process_{i}(input: &str) -> Option<String> {{
    if input.is_empty() {{
        return None;
    }}
    Some(input.to_uppercase())
}}

#[cfg(test)]
mod tests {{
    use super::*;

    #[test]
    fn test_config_{i}_valid() {{
        let c = Config{i}::new("test", 42);
        assert!(c.is_valid());
    }}

    #[test]
    fn test_process_{i}() {{
        assert_eq!(process_{i}("hello"), Some("HELLO".to_string()));
        assert_eq!(process_{i}(""), None);
    }}
}}
"#
        );
        std::fs::write(dir.join("src").join(format!("module_{i}.rs")), content).unwrap();
    }
}

// ── Benchmark 1: Code parsing throughput ─────────────────────────────────────

fn bench_code_parsing(n: usize) -> (f64, f64) {
    eprintln!("  [code parsing] generating {n} Rust files...");
    let dir = tempfile::tempdir().unwrap();
    generate_rust_files(dir.path(), n);

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() { collect_rs(&p, out); }
                else if p.extension().map(|e| e == "rs").unwrap_or(false) { out.push(p); }
            }
        }
    }
    collect_rs(dir.path(), &mut files);

    // Pre-read into memory
    let contents: Vec<(std::path::PathBuf, Vec<u8>)> = files
        .iter()
        .map(|f| (f.clone(), std::fs::read(f).unwrap()))
        .collect();

    let parser = CodeParser::new();

    // Warm up
    for (path, content) in &contents[..contents.len().min(5)] {
        let _ = parser.parse(path, content);
    }

    // Measure
    let start = Instant::now();
    let mut total_chunks = 0usize;
    for (path, content) in &contents {
        if let Ok(result) = parser.parse(path, content) {
            total_chunks += result.chunks.len();
        }
    }
    let elapsed = start.elapsed();

    let files_per_sec = n as f64 / elapsed.as_secs_f64();
    let total_bytes: usize = contents.iter().map(|(_, c)| c.len()).sum();
    let mb_per_sec = (total_bytes as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64();

    eprintln!("  [code parsing] {n} files → {total_chunks} chunks in {:.0}ms → {files_per_sec:.0} files/s, {mb_per_sec:.1} MB/s",
        elapsed.as_millis());

    (files_per_sec, mb_per_sec)
}

// ── Benchmark 2: Enrichment throughput ───────────────────────────────────────

fn bench_enrichment(n: usize) -> (f64, f64) {
    eprintln!("  [enrichment] generating {n} markdown files...");
    let dir = tempfile::tempdir().unwrap();
    generate_vault(dir.path(), &VaultConfig { num_files: n, ..Default::default() });

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    fn collect_md(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() { collect_md(&p, out); }
                else if p.extension().map(|e| e == "md").unwrap_or(false) { out.push(p); }
            }
        }
    }
    collect_md(dir.path(), &mut files);

    let parser = MarkdownParser;
    let contents: Vec<(std::path::PathBuf, Vec<u8>)> = files
        .iter()
        .map(|f| (f.clone(), std::fs::read(f).unwrap()))
        .collect();

    // Parse all files first (benchmark is enrichment only, not parsing)
    let parsed: Vec<(std::path::PathBuf, Vec<u8>, locus_core::types::ParseResult)> = contents
        .into_iter()
        .filter_map(|(p, c)| {
            parser.parse(&p, &c).ok().map(|r| (p, c, r))
        })
        .collect();

    let make_pipeline = || {
        TagPipeline::new(
            vec![
                Box::new(SizeTagger),
                Box::new(ConventionTagger),
                Box::new(TopicTagger),
                Box::new(ComplexityTagger),
            ],
            Box::new(InMemoryTaggerCache::new()),
        )
    };

    // Cold run (cache miss — every file is new)
    let pipeline_cold = make_pipeline();
    let cold_start = Instant::now();
    let mut total_tags = 0usize;
    for (path, content, parse_result) in &parsed {
        if let Ok(tags) = pipeline_cold.enrich(path, content, parse_result, &SourceType::Obsidian) {
            total_tags += tags.len();
        }
    }
    let cold_elapsed = cold_start.elapsed();
    let cold_files_per_sec = parsed.len() as f64 / cold_elapsed.as_secs_f64();

    // Warm run (cache hit — same content, same pipeline config)
    let warm_start = Instant::now();
    for (path, content, parse_result) in &parsed {
        let _ = pipeline_cold.enrich(path, content, parse_result, &SourceType::Obsidian);
    }
    let warm_elapsed = warm_start.elapsed();
    let warm_files_per_sec = parsed.len() as f64 / warm_elapsed.as_secs_f64();

    eprintln!("  [enrichment] {n} files, {total_tags} total tags");
    eprintln!("    cold (cache miss): {:.0}ms → {cold_files_per_sec:.0} files/s", cold_elapsed.as_millis());
    eprintln!("    warm (cache hit):  {:.0}ms → {warm_files_per_sec:.0} files/s", warm_elapsed.as_millis());

    (cold_files_per_sec, warm_files_per_sec)
}

// ── Benchmark 3: Semantic query latency ──────────────────────────────────────

fn bench_semantic_query(n: usize) -> (u64, u64, u64) {
    eprintln!("  [semantic query] indexing {n} docs with random 384-dim vectors...");
    let dir = tempfile::tempdir().unwrap();
    generate_vault(dir.path(), &VaultConfig { num_files: n, ..Default::default() });

    // Build bitmap index
    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        Box::new(InMemoryRegistry::new()),
        Box::new(InMemoryBitmapStore::new()),
    );
    let bulk = pipeline.bulk_index(dir.path()).unwrap();
    eprintln!("  [semantic query] indexed {} docs, {} bitmaps", bulk.docs_indexed, bulk.bitmaps_created);

    let (_, registry, bitmap_store) = pipeline.into_parts();

    // Populate vector store with random vectors (384-dim = BGE-small-en-v1.5)
    const DIM: usize = 384;
    let vector_store = InMemoryVectorStore::new();
    let mut rng = StdRng::seed_from_u64(42);

    // Assign one vector per chunk (use chunk_id as key)
    // We need to enumerate all chunk IDs from the registry
    // Use doc IDs 0..n and assign chunk_id = doc_id for simplicity
    for chunk_id in 0..(n as u32) {
        let v = random_vector(DIM, &mut rng);
        vector_store.upsert(chunk_id, &v).unwrap();
    }

    let engine = BitmapQueryEngine::new(bitmap_store, registry);

    // Dummy embedder that returns a random vector
    struct RandomEmbedder { dim: usize }
    impl locus_core::semantic::Embedder for RandomEmbedder {
        fn embed(&self, _texts: &[&str]) -> Result<Vec<EmbeddingVector>, locus_core::semantic::EmbedError> {
            let mut rng = StdRng::seed_from_u64(99);
            Ok(vec![random_vector(self.dim, &mut rng)])
        }
        fn dimension(&self) -> usize { self.dim }
    }
    let embedder = RandomEmbedder { dim: DIM };

    let filter = Filter::Key("tag:work".into());
    let request = SemanticQueryRequest {
        filter: filter.clone(),
        query_text: "find relevant notes about work projects".into(),
        top_k: 10,
        rerank: false,
    };

    // Warm up
    for _ in 0..5 {
        let _ = engine.semantic_query(&request, &embedder, &vector_store, None);
    }

    // Measure bitmap-only baseline
    let mut bitmap_times = Vec::with_capacity(50);
    for _ in 0..50 {
        let t = Instant::now();
        let _ = engine.query(QueryRequest { filter: filter.clone(), limit: Some(10), offset: None });
        bitmap_times.push(t.elapsed().as_micros() as u64);
    }
    let bitmap_us = median_us(&mut bitmap_times);

    // Measure full semantic (bitmap → vector search)
    let mut semantic_times = Vec::with_capacity(50);
    for _ in 0..50 {
        let t = Instant::now();
        let _ = engine.semantic_query(&request, &embedder, &vector_store, None);
        semantic_times.push(t.elapsed().as_micros() as u64);
    }
    let semantic_us = median_us(&mut semantic_times);

    // Measure vector search portion only (sequential scan over candidate IDs)
    // Estimate: full - bitmap
    let vector_only_us = semantic_us.saturating_sub(bitmap_us);

    eprintln!("  [semantic query] bitmap={bitmap_us}µs, full_semantic={semantic_us}µs, vector_scan≈{vector_only_us}µs");

    (bitmap_us, semantic_us, vector_only_us)
}

// ── Benchmark 4: On-disk LMDB query ──────────────────────────────────────────

fn bench_ondisk_query(n: usize) -> (u64, u64) {
    eprintln!("  [on-disk query] indexing {n} docs to LMDB + DuckDB...");
    let vault_dir = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();

    generate_vault(vault_dir.path(), &VaultConfig { num_files: n, ..Default::default() });

    let registry = DuckDbRegistry::new(&data_dir.path().join("registry.db").to_string_lossy()).unwrap();
    let bitmap_store = LmdbBitmapStore::new(&data_dir.path().join("bitmaps")).unwrap();

    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        Box::new(registry),
        Box::new(bitmap_store),
    );
    let bulk = pipeline.bulk_index(vault_dir.path()).unwrap();
    eprintln!("  [on-disk query] indexed {} docs, {} bitmaps", bulk.docs_indexed, bulk.bitmaps_created);

    let (_, registry, bitmap_store) = pipeline.into_parts();
    let engine = BitmapQueryEngine::new(bitmap_store, registry);

    // Warm up
    for _ in 0..10 {
        let _ = engine.query(QueryRequest {
            filter: Filter::Key("tag:work".into()),
            limit: Some(20),
            offset: None,
        });
    }

    // Single key
    let mut single_times = Vec::with_capacity(100);
    for _ in 0..100 {
        let t = Instant::now();
        let _ = engine.query(QueryRequest {
            filter: Filter::Key("tag:work".into()),
            limit: Some(20),
            offset: None,
        });
        single_times.push(t.elapsed().as_micros() as u64);
    }
    let single_us = median_us(&mut single_times);

    // AND query
    let mut and_times = Vec::with_capacity(100);
    for _ in 0..100 {
        let t = Instant::now();
        let _ = engine.query(QueryRequest {
            filter: Filter::And(vec![
                Filter::Key("tag:project".into()),
                Filter::Key("source:obsidian".into()),
            ]),
            limit: Some(20),
            offset: None,
        });
        and_times.push(t.elapsed().as_micros() as u64);
    }
    let and_us = median_us(&mut and_times);

    eprintln!("  [on-disk / LMDB] single={single_us}µs, AND={and_us}µs");

    (single_us, and_us)
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    eprintln!("Locus Phase 2/3 Benchmarks");
    eprintln!("==========================");
    eprintln!("Note: semantic query uses random 384-dim vectors (no model inference).");
    eprintln!("      This isolates pipeline latency from embedding generation time.");
    eprintln!();

    // ── 1. Code parsing throughput ──────────────────────────────────────────
    eprintln!("─── 1. Code Parsing Throughput (target: >10K files/s) ───");
    let sizes = [100, 500, 1_000, 5_000, 10_000];
    let mut code_results: Vec<(usize, f64, f64)> = Vec::new();
    for &n in &sizes {
        let (fps, mbps) = bench_code_parsing(n);
        code_results.push((n, fps, mbps));
    }
    eprintln!();

    // ── 2. Enrichment throughput ────────────────────────────────────────────
    eprintln!("─── 2. Enrichment Throughput (target: >15K files/s cold) ───");
    let enrich_sizes = [500, 1_000, 5_000, 10_000];
    let mut enrich_results: Vec<(usize, f64, f64)> = Vec::new();
    for &n in &enrich_sizes {
        let (cold, warm) = bench_enrichment(n);
        enrich_results.push((n, cold, warm));
    }
    eprintln!();

    // ── 3. Semantic query latency ───────────────────────────────────────────
    eprintln!("─── 3. Semantic Query Latency (target: <50ms at 100K docs) ───");
    let semantic_sizes = [1_000, 5_000, 10_000, 50_000];
    let mut semantic_results: Vec<(usize, u64, u64, u64)> = Vec::new();
    for &n in &semantic_sizes {
        let (bitmap_us, semantic_us, vector_us) = bench_semantic_query(n);
        semantic_results.push((n, bitmap_us, semantic_us, vector_us));
    }
    eprintln!();

    // ── 4. On-disk LMDB query ───────────────────────────────────────────────
    eprintln!("─── 4. On-Disk Query Speed (LMDB, persistent) ───");
    let ondisk_sizes = [1_000, 5_000, 10_000];
    let mut ondisk_results: Vec<(usize, u64, u64)> = Vec::new();
    for &n in &ondisk_sizes {
        let (single, and) = bench_ondisk_query(n);
        ondisk_results.push((n, single, and));
    }
    eprintln!();

    // ── Summary ─────────────────────────────────────────────────────────────
    println!("\n========================================");
    println!("RESULTS SUMMARY");
    println!("========================================\n");

    println!("1. Code Parsing Throughput (CodeParser / Tree-sitter)");
    println!("   Target: >10,000 files/s");
    println!("   {:>10}  {:>14}  {:>12}", "Files", "files/s", "MB/s");
    for (n, fps, mbps) in &code_results {
        let flag = if *fps >= 10_000.0 { "✓" } else { "✗" };
        println!("   {:>10}  {:>13.0}  {:>11.1}  {flag}", n, fps, mbps);
    }

    println!("\n2. Enrichment Throughput (all 4 builtin taggers)");
    println!("   Target: >15,000 files/s cold (cache miss)");
    println!("   {:>10}  {:>16}  {:>16}", "Files", "cold files/s", "warm files/s");
    for (n, cold, warm) in &enrich_results {
        let flag = if *cold >= 15_000.0 { "✓" } else { "✗" };
        println!("   {:>10}  {:>15.0}  {:>15.0}  {flag}", n, cold, warm);
    }

    println!("\n3. Semantic Query Latency (bitmap pre-filter + vector scan, random vectors)");
    println!("   Target: <50,000µs (<50ms) full pipeline");
    println!("   {:>10}  {:>12}  {:>16}  {:>13}", "Docs", "bitmap (µs)", "full semantic (µs)", "vector scan (µs)");
    for (n, bm, sem, vec) in &semantic_results {
        let flag = if *sem < 50_000 { "✓" } else { "✗" };
        println!("   {:>10}  {:>12}  {:>18}  {:>13}  {flag}", n, bm, sem, vec);
    }

    println!("\n4. On-Disk Query Speed (LMDB BitmapStore, DuckDB Registry)");
    println!("   (comparison: in-memory was ~16µs single, ~19µs AND at 100K)");
    println!("   {:>10}  {:>14}  {:>14}", "Docs", "single key (µs)", "AND query (µs)");
    for (n, single, and) in &ondisk_results {
        println!("   {:>10}  {:>14}  {:>14}", n, single, and);
    }

    println!("\n========================================");
}
