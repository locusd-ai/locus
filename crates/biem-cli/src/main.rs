use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use biem_bitmap::memory::InMemoryBitmapStore;
use biem_core::query::{Filter, QueryEngine, QueryRequest};
use biem_ingest::IngestionPipeline;
use biem_parser::markdown::MarkdownParser;
use biem_query::BitmapQueryEngine;
use biem_registry::memory::InMemoryRegistry;

#[derive(Parser)]
#[command(name = "biem", about = "Bit-Indexed External Memory — local-first indexing for LLMs")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Index a vault directory (standalone, in-memory)
    Index {
        /// Path to the vault directory
        path: PathBuf,
    },
    /// Search the index with bitmap filters
    Search {
        /// Filter expression keys (e.g. "tag:work", "type:task"), combined with AND
        filters: Vec<String>,
        /// Maximum results
        #[arg(long, default_value = "20")]
        limit: u32,
        /// Path to the vault to search
        #[arg(long)]
        vault: PathBuf,
    },
    /// Inspect a specific file's index entry
    Inspect {
        /// Path to the file to inspect
        path: PathBuf,
        /// Path to the vault
        #[arg(long)]
        vault: PathBuf,
    },
    /// Show index status
    Status {
        /// Path to the vault
        #[arg(long)]
        vault: PathBuf,
    },
    /// List available filters (bitmap keys)
    Filters {
        /// Filter by category: tag, folder, link, type, source
        #[arg(long)]
        category: Option<String>,
        /// Path to the vault
        #[arg(long)]
        vault: PathBuf,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Index { path } => cmd_index(&path),
        Commands::Search { filters, limit, vault } => cmd_search(&vault, &filters, limit),
        Commands::Inspect { path, vault } => cmd_inspect(&vault, &path),
        Commands::Status { vault } => cmd_status(&vault),
        Commands::Filters { category, vault } => cmd_filters(&vault, category),
    }
}

/// Index and return engine only.
fn index_to_engine(vault: &PathBuf) -> Result<BitmapQueryEngine> {
    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        Box::new(InMemoryRegistry::new()),
        Box::new(InMemoryBitmapStore::new()),
    );

    pipeline.bulk_index(vault.as_path()).context("failed to index vault")?;

    let (_parsers, registry, bitmap_store) = pipeline.into_parts();
    Ok(BitmapQueryEngine::new(bitmap_store, registry))
}

// ── Commands ─────────────────────────────────────────────────────

fn cmd_index(path: &PathBuf) -> Result<()> {
    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        Box::new(InMemoryRegistry::new()),
        Box::new(InMemoryBitmapStore::new()),
    );

    let result = pipeline.bulk_index(path.as_path())
        .context("failed to index vault")?;

    println!("✓ Indexed {} documents", result.docs_indexed);
    println!("  {} bitmap keys created", result.bitmaps_created);
    println!("  {}ms elapsed", result.duration_ms);
    Ok(())
}

fn cmd_search(vault: &PathBuf, filter_keys: &[String], limit: u32) -> Result<()> {
    let engine = index_to_engine(vault)?;

    let filter = if filter_keys.len() == 1 {
        Filter::Key(filter_keys[0].clone())
    } else if filter_keys.is_empty() {
        // Match everything — use source:obsidian as universe
        Filter::Key("source:obsidian".into())
    } else {
        Filter::And(filter_keys.iter().map(|k| Filter::Key(k.clone())).collect())
    };

    let result = engine.query(QueryRequest {
        filter,
        limit: Some(limit),
        offset: None,
    }).context("query failed")?;

    println!("{} matches ({}μs)", result.total_matching, result.query_time_us);
    println!();

    for m in &result.matches {
        println!("  {} (doc_id={})", m.file_path.display(), m.doc_id);
        if let Some(ref t) = m.auto_type {
            print!("    type: {t}");
        }
        if !m.matched_filters.is_empty() {
            print!("    filters: {}", m.matched_filters.join(", "));
        }
        println!();
        for c in &m.chunks {
            println!(
                "    chunk {} ({}) bytes {}..{} {}",
                c.chunk_id,
                c.kind,
                c.byte_start,
                c.byte_end,
                c.label.as_deref().unwrap_or("")
            );
        }
    }

    Ok(())
}

fn cmd_inspect(vault: &PathBuf, path: &PathBuf) -> Result<()> {
    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        Box::new(InMemoryRegistry::new()),
        Box::new(InMemoryBitmapStore::new()),
    );

    pipeline.bulk_index(vault.as_path()).context("failed to index vault")?;
    let (_parsers, registry, bitmap_store) = pipeline.into_parts();

    // Try canonical path first, then as-is
    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
    let doc = registry.lookup_by_path(&canonical)
        .context("registry lookup failed")?
        .or(registry.lookup_by_path(path).ok().flatten());

    match doc {
        Some(d) => {
            println!("Document: {}", d.file_path.display());
            println!("  doc_id:       {}", d.doc_id);
            println!("  source:       {:?}", d.source_type);
            println!("  auto_type:    {:?}", d.auto_type);
            println!("  blake3:       {}", hex(&d.blake3_hash));
            println!("  last_indexed: {}", d.last_indexed);

            let chunks = registry.get_chunks(d.doc_id)
                .context("failed to get chunks")?;
            println!("  chunks:       {}", chunks.len());
            for c in &chunks {
                println!(
                    "    [{}] {:?} bytes {}..{} {}",
                    c.chunk_id, c.kind, c.byte_start, c.byte_end,
                    c.label.as_deref().unwrap_or("")
                );
            }

            // Show which bitmaps contain this doc
            let keys = bitmap_store.list_keys(None)
                .context("failed to list bitmap keys")?;
            let mut doc_keys = Vec::new();
            for key in &keys {
                let bm = bitmap_store.get(key).unwrap_or_default();
                if bm.contains(d.doc_id) {
                    doc_keys.push(key.clone());
                }
            }
            doc_keys.sort();
            println!("  bitmaps:      {}", doc_keys.join(", "));
        }
        None => {
            println!("Not found in index: {}", path.display());
        }
    }

    Ok(())
}

fn cmd_status(vault: &PathBuf) -> Result<()> {
    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        Box::new(InMemoryRegistry::new()),
        Box::new(InMemoryBitmapStore::new()),
    );

    pipeline.bulk_index(vault.as_path()).context("failed to index vault")?;
    let (_parsers, registry, bitmap_store) = pipeline.into_parts();

    let state = registry.get_global_state().context("failed to get state")?;
    let tombstones = bitmap_store.get_tombstone().unwrap_or_default();
    let bitmap_count = bitmap_store.list_keys(None).unwrap_or_default().len();

    println!("BIEM Index Status");
    println!("  documents:  {}", state.total_documents);
    println!("  bitmaps:    {}", bitmap_count);
    println!("  tombstoned: {}", tombstones.len());
    println!("  next_doc:   {}", state.next_doc_id);
    println!("  next_chunk: {}", state.next_chunk_id);

    Ok(())
}

fn cmd_filters(vault: &PathBuf, category: Option<String>) -> Result<()> {
    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        Box::new(InMemoryRegistry::new()),
        Box::new(InMemoryBitmapStore::new()),
    );

    pipeline.bulk_index(vault.as_path()).context("failed to index vault")?;
    let (_parsers, _registry, bitmap_store) = pipeline.into_parts();

    let prefix = category.as_deref().map(|c| format!("{c}:"));
    let keys = bitmap_store.list_keys(prefix.as_deref())
        .context("failed to list keys")?;

    println!("{} filter keys{}:", keys.len(),
        category.as_ref().map(|c| format!(" (category: {c})")).unwrap_or_default());

    for key in &keys {
        let card = bitmap_store.cardinality(key).unwrap_or(0);
        println!("  {key}  ({card} docs)");
    }

    Ok(())
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join("")
}
