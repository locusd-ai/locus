use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use biem_bitmap::lmdb::LmdbBitmapStore;
use biem_bitmap::memory::InMemoryBitmapStore;
use biem_core::bitmap::BitmapStore;
use biem_core::config::{self, StorageMode};
use biem_core::query::{Filter, QueryEngine, QueryRequest};
use biem_core::registry::Registry;
use biem_ingest::IngestionPipeline;
use biem_parser::markdown::MarkdownParser;
use biem_query::BitmapQueryEngine;
use biem_registry::duckdb::DuckDbRegistry;
use biem_registry::memory::InMemoryRegistry;

#[derive(Parser)]
#[command(name = "biem", about = "Bit-Indexed External Memory — local-first indexing for LLMs")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Data directory for persistent storage (overrides config resolution)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// Use in-memory storage instead of persistent (no data dir needed)
    #[arg(long, global = true, default_value = "false")]
    memory: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Register a vault and run initial index
    Init {
        /// Path to the vault directory
        path: PathBuf,
        /// Store state inside the vault (.biem/) instead of globally
        #[arg(long)]
        local: bool,
    },
    /// Show configuration (registered vaults)
    Config,
    /// Index a vault directory
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
        /// Path to the vault to search (only needed with --memory)
        #[arg(long)]
        vault: Option<PathBuf>,
    },
    /// Inspect a specific file's index entry
    Inspect {
        /// Path to the file to inspect
        path: PathBuf,
        /// Path to the vault (only needed with --memory)
        #[arg(long)]
        vault: Option<PathBuf>,
    },
    /// Show index status
    Status {
        /// Path to the vault (only needed with --memory)
        #[arg(long)]
        vault: Option<PathBuf>,
    },
    /// List available filters (bitmap keys)
    Filters {
        /// Filter by category: tag, folder, link, type, source
        #[arg(long)]
        category: Option<String>,
        /// Path to the vault (only needed with --memory)
        #[arg(long)]
        vault: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { ref path, local } => cmd_init(path, local),
        Commands::Config => cmd_config(),
        Commands::Index { ref path } => cmd_index(path, &cli),
        Commands::Search { ref filters, limit, ref vault } => cmd_search(vault.as_ref(), filters, limit, &cli),
        Commands::Inspect { ref path, ref vault } => cmd_inspect(vault.as_ref(), path, &cli),
        Commands::Status { ref vault } => cmd_status(vault.as_ref(), &cli),
        Commands::Filters { ref category, ref vault } => cmd_filters(vault.as_ref(), category.clone(), &cli),
    }
}

// ── Storage helpers ──────────────────────────────────────────────

/// Resolve the data directory: --data-dir override > config lookup > error.
fn resolve_data_dir(cli: &Cli) -> Result<PathBuf> {
    if let Some(ref dir) = cli.data_dir {
        return Ok(dir.clone());
    }
    // No override — fall back to legacy default ~/.biem/
    // (will be replaced by config-based resolution once vaults are registered)
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".biem"))
}

/// Resolve data dir for a known vault path, using config.
fn resolve_data_dir_for_vault(vault_path: &PathBuf, cli: &Cli) -> Result<PathBuf> {
    if let Some(ref dir) = cli.data_dir {
        return Ok(dir.clone());
    }
    // Try config resolution
    let cfg = config::load_config().unwrap_or_default();
    match config::resolve_vault(vault_path, &cfg) {
        Ok(entry) => Ok(entry.data_dir),
        Err(_) => {
            // Fall back to legacy default
            let home = std::env::var("HOME").context("HOME not set")?;
            Ok(PathBuf::from(home).join(".biem"))
        }
    }
}

/// Create persistent stores (DuckDB + LMDB).
fn open_persistent_stores(data_dir: &PathBuf) -> Result<(Box<dyn Registry>, Box<dyn BitmapStore>)> {
    let db_path = data_dir.join("registry.duckdb");
    let registry = DuckDbRegistry::new(db_path.to_str().unwrap())
        .context("failed to open DuckDB registry")?;

    let lmdb_path = data_dir.join("bitmaps.lmdb");
    let bitmap_store = LmdbBitmapStore::new(&lmdb_path)
        .context("failed to open LMDB bitmap store")?;

    Ok((Box::new(registry), Box::new(bitmap_store)))
}

/// Create in-memory stores.
fn open_memory_stores() -> (Box<dyn Registry>, Box<dyn BitmapStore>) {
    (Box::new(InMemoryRegistry::new()), Box::new(InMemoryBitmapStore::new()))
}

/// Build a pipeline with the appropriate backend, optionally indexing a vault.
fn build_pipeline_and_index(
    vault: Option<&PathBuf>,
    cli: &Cli,
) -> Result<(Box<dyn Registry>, Box<dyn BitmapStore>)> {
    let (registry, bitmap_store) = if cli.memory {
        let (r, b) = open_memory_stores();
        // In-memory mode: must index on the fly
        let vault = vault.context("--vault is required with --memory mode")?;
        let mut pipeline = IngestionPipeline::new(
            vec![Box::new(MarkdownParser)],
            r,
            b,
        );
        pipeline.bulk_index(vault.as_path()).context("failed to index vault")?;
        let (_parsers, registry, bitmap_store) = pipeline.into_parts();
        (registry, bitmap_store)
    } else {
        // Persistent mode: stores already have data from previous `biem index`
        let data_dir = resolve_data_dir(cli)?;
        if !data_dir.exists() {
            anyhow::bail!(
                "data dir {} does not exist — run `biem index <vault>` first, or use --memory",
                data_dir.display()
            );
        }
        open_persistent_stores(&data_dir)?
    };
    Ok((registry, bitmap_store))
}

// ── Commands ─────────────────────────────────────────────────────

fn cmd_init(path: &PathBuf, local: bool) -> Result<()> {
    let vault_path = path.canonicalize()
        .with_context(|| format!("vault path does not exist: {}", path.display()))?;

    let biem_dir = config::default_config_dir()
        .context("could not determine BIEM config directory")?;
    let mut cfg = config::load_config().unwrap_or_default();

    let storage = if local { StorageMode::Local } else { StorageMode::Global };

    let (name, entry) = config::register_vault(&vault_path, storage, &biem_dir, &mut cfg)
        .with_context(|| format!("failed to register vault: {}", vault_path.display()))?;

    config::save_config(&cfg)
        .context("failed to save config")?;

    println!("✓ Registered vault '{}' at {}", name, vault_path.display());
    println!("  storage: {:?}", entry.storage);
    println!("  data_dir: {}", entry.data_dir.display());

    // Run initial bulk index
    let (registry, bitmap_store) = open_persistent_stores(&entry.data_dir)?;
    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        registry,
        bitmap_store,
    );

    let result = pipeline.bulk_index(&vault_path)
        .context("initial index failed")?;

    println!("✓ Indexed {} documents ({} bitmap keys, {}ms)",
        result.docs_indexed, result.bitmaps_created, result.duration_ms);

    Ok(())
}

fn cmd_config() -> Result<()> {
    let cfg = config::load_config().unwrap_or_default();

    if cfg.vaults.is_empty() {
        println!("No vaults registered. Run `biem init <vault>` to get started.");
        return Ok(());
    }

    println!("Registered vaults:");
    for (name, entry) in &cfg.vaults {
        println!();
        println!("  [{}]", name);
        println!("    path:     {}", entry.path.display());
        println!("    storage:  {:?}", entry.storage);
        println!("    data_dir: {}", entry.data_dir.display());
    }

    Ok(())
}

fn cmd_index(path: &PathBuf, cli: &Cli) -> Result<()> {
    let (registry, bitmap_store) = if cli.memory {
        open_memory_stores()
    } else {
        let data_dir = resolve_data_dir_for_vault(path, cli)?;
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("failed to create data dir: {}", data_dir.display()))?;
        eprintln!("Data dir: {}", data_dir.display());
        open_persistent_stores(&data_dir)?
    };

    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        registry,
        bitmap_store,
    );

    let result = pipeline.bulk_index(path.as_path())
        .context("failed to index vault")?;

    println!("✓ Indexed {} documents", result.docs_indexed);
    println!("  {} bitmap keys created", result.bitmaps_created);
    println!("  {}ms elapsed", result.duration_ms);

    if !cli.memory {
        let data_dir = resolve_data_dir(cli)?;
        println!("  stored in {}", data_dir.display());
    }

    Ok(())
}

fn cmd_search(vault: Option<&PathBuf>, filter_keys: &[String], limit: u32, cli: &Cli) -> Result<()> {
    let (registry, bitmap_store) = build_pipeline_and_index(vault, cli)?;
    let engine = BitmapQueryEngine::new(bitmap_store, registry);

    let filter = if filter_keys.len() == 1 {
        Filter::Key(filter_keys[0].clone())
    } else if filter_keys.is_empty() {
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
                c.chunk_id, c.kind, c.byte_start, c.byte_end,
                c.label.as_deref().unwrap_or("")
            );
        }
    }

    Ok(())
}

fn cmd_inspect(vault: Option<&PathBuf>, path: &PathBuf, cli: &Cli) -> Result<()> {
    let (registry, bitmap_store) = build_pipeline_and_index(vault, cli)?;

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

fn cmd_status(vault: Option<&PathBuf>, cli: &Cli) -> Result<()> {
    let (registry, bitmap_store) = build_pipeline_and_index(vault, cli)?;

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

fn cmd_filters(vault: Option<&PathBuf>, category: Option<String>, cli: &Cli) -> Result<()> {
    let (_registry, bitmap_store) = build_pipeline_and_index(vault, cli)?;

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
