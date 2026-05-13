//! Performance benchmarks for the BIEM ingestion pipeline.
//!
//! Tests bulk_index, re-index (idempotent), single-event processing,
//! and query performance at various vault sizes.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::Duration;

use biem_bitmap::memory::InMemoryBitmapStore;
use biem_core::bitmap::BitmapStore;
use biem_core::query::{Filter, QueryEngine, QueryRequest};
use biem_core::registry::Registry;
use biem_core::types::{ChangeEvent, ChangeKind};
use biem_ingest::vault_gen::{generate_vault, VaultConfig};
use biem_ingest::IngestionPipeline;
use biem_parser::markdown::MarkdownParser;
use biem_query::BitmapQueryEngine;
use biem_registry::memory::InMemoryRegistry;

fn make_pipeline() -> IngestionPipeline {
    IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        Box::new(InMemoryRegistry::new()),
        Box::new(InMemoryBitmapStore::new()),
    )
}

// ── Bulk index at various scales ─────────────────────────────────

fn bench_bulk_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_index");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    for &n in &[100, 500, 1000, 5000] {
        let dir = tempfile::tempdir().unwrap();
        generate_vault(dir.path(), &VaultConfig { num_files: n, ..Default::default() });

        group.bench_with_input(BenchmarkId::new("files", n), &n, |b, _| {
            b.iter(|| {
                let mut pipeline = make_pipeline();
                let result = pipeline.bulk_index(black_box(dir.path())).unwrap();
                assert_eq!(result.docs_indexed as usize, n);
            });
        });
    }
    group.finish();
}

// ── Re-index (idempotent skip) ───────────────────────────────────

fn bench_reindex_skip(c: &mut Criterion) {
    let mut group = c.benchmark_group("reindex_skip");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    for &n in &[100, 500, 1000] {
        let dir = tempfile::tempdir().unwrap();
        generate_vault(dir.path(), &VaultConfig { num_files: n, ..Default::default() });

        let mut pipeline = IngestionPipeline::new(
            vec![Box::new(MarkdownParser)],
            Box::new(InMemoryRegistry::new()),
            Box::new(InMemoryBitmapStore::new()),
        );
        pipeline.bulk_index(dir.path()).unwrap();

        group.bench_with_input(BenchmarkId::new("files", n), &n, |b, _| {
            b.iter(|| {
                let result = pipeline.bulk_index(black_box(dir.path())).unwrap();
                assert_eq!(result.docs_skipped as usize, n);
            });
        });
    }
    group.finish();
}

// ── Single file event processing ─────────────────────────────────

fn bench_single_event(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_event");
    let dir = tempfile::tempdir().unwrap();
    let paths = generate_vault(dir.path(), &VaultConfig { num_files: 100, ..Default::default() });

    group.bench_function("created", |b| {
        b.iter_with_setup(|| make_pipeline(), |mut pipeline| {
            pipeline.process_event(black_box(&ChangeEvent {
                path: paths[0].clone(), kind: ChangeKind::Created,
            })).unwrap();
        });
    });

    group.bench_function("modified_skip", |b| {
        b.iter_with_setup(
            || {
                let mut p = make_pipeline();
                p.process_event(&ChangeEvent { path: paths[0].clone(), kind: ChangeKind::Created }).unwrap();
                p
            },
            |mut pipeline| {
                pipeline.process_event(black_box(&ChangeEvent {
                    path: paths[0].clone(), kind: ChangeKind::Modified,
                })).unwrap();
            },
        );
    });
    group.finish();
}

// ── Query benchmarks ─────────────────────────────────────────────

fn bench_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("query");
    group.sample_size(50);

    for &n in &[500, 1000, 5000] {
        let dir = tempfile::tempdir().unwrap();
        generate_vault(dir.path(), &VaultConfig { num_files: n, ..Default::default() });
        let mut pipeline = make_pipeline();
        pipeline.bulk_index(dir.path()).unwrap();
        let (_, registry, bitmap_store) = pipeline.into_parts();
        let engine = BitmapQueryEngine::new(bitmap_store, registry);

        group.bench_with_input(BenchmarkId::new("single_key", n), &n, |b, _| {
            b.iter(|| {
                black_box(engine.query(black_box(QueryRequest {
                    filter: Filter::Key("tag:work".into()), limit: Some(20), offset: None,
                })).unwrap());
            });
        });
        group.bench_with_input(BenchmarkId::new("and_two_keys", n), &n, |b, _| {
            b.iter(|| {
                black_box(engine.query(black_box(QueryRequest {
                    filter: Filter::And(vec![Filter::Key("tag:project".into()), Filter::Key("source:obsidian".into())]),
                    limit: Some(20), offset: None,
                })).unwrap());
            });
        });
        group.bench_with_input(BenchmarkId::new("or_three_keys", n), &n, |b, _| {
            b.iter(|| {
                black_box(engine.query(black_box(QueryRequest {
                    filter: Filter::Or(vec![
                        Filter::Key("tag:project".into()),
                        Filter::Key("tag:meeting".into()),
                        Filter::Key("tag:research".into()),
                    ]),
                    limit: Some(20), offset: None,
                })).unwrap());
            });
        });
    }
    group.finish();
}

// ── Parser throughput ────────────────────────────────────────────

fn bench_parse_throughput(c: &mut Criterion) {
    use biem_core::parser::Parser;
    let mut group = c.benchmark_group("parse_throughput");
    let dir = tempfile::tempdir().unwrap();
    let paths = generate_vault(dir.path(), &VaultConfig {
        num_files: 100, avg_paragraphs: 12, ..Default::default()
    });
    let files: Vec<(std::path::PathBuf, Vec<u8>)> = paths.iter()
        .map(|p| (p.clone(), std::fs::read(p).unwrap())).collect();
    let total_bytes: usize = files.iter().map(|(_, c)| c.len()).sum();
    let parser = MarkdownParser;
    group.throughput(criterion::Throughput::Bytes(total_bytes as u64));
    group.bench_function("100_files", |b| {
        b.iter(|| { for (path, content) in &files { black_box(parser.parse(path, content).unwrap()); } });
    });
    group.finish();
}

criterion_group!(benches, bench_bulk_index, bench_reindex_skip, bench_single_event, bench_query, bench_parse_throughput);
criterion_main!(benches);
