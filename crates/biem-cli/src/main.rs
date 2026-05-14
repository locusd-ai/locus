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
use biem_core::semantic::Embedder;
use biem_embed::{FastEmbedEmbedder, UsearchVectorStore};
use biem_enrich::builtin::{ComplexityTagger, ConventionTagger, SizeTagger, TopicTagger};
use biem_enrich::{InMemoryTaggerCache, TagPipeline, FsTaggerCache, load_yaml_taggers};
use biem_ingest::IngestionPipeline;
use biem_parser::markdown::MarkdownParser;
use biem_query::BitmapQueryEngine;
use biem_query::SemanticQueryRequest;
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

    /// Output results as JSON
    #[arg(long, global = true, default_value = "false")]
    json: bool,
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
    /// Compact: remove tombstoned documents from bitmaps and registry
    Compact {
        /// Path to the vault (only needed with --memory)
        #[arg(long)]
        vault: Option<PathBuf>,
    },
    /// List active taggers (builtin + custom)
    Taggers,
    /// Re-run enrichment taggers on an indexed vault
    Enrich {
        /// Path to the vault directory
        path: PathBuf,
        /// Force re-run, ignoring tagger cache
        #[arg(long)]
        force: bool,
    },
    /// Semantic search: bitmap pre-filter + vector similarity
    Semantic {
        /// Natural language query text
        query: String,
        /// Bitmap filter keys for pre-filtering (e.g. "tag:work")
        #[arg(long, short = 'f')]
        filter: Vec<String>,
        /// Maximum results
        #[arg(long, default_value = "5")]
        top_k: usize,
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
        Commands::Compact { ref vault } => cmd_compact(vault.as_ref(), &cli),
        Commands::Taggers => cmd_taggers(),
        Commands::Enrich { ref path, force } => cmd_enrich(path, force, &cli),
        Commands::Semantic { ref query, ref filter, top_k } => cmd_semantic(query, filter, top_k, &cli),
    }
}

// ── Storage helpers ──────────────────────────────────────────────

/// Resolve the data directory: --data-dir override > config lookup > error.
fn resolve_data_dir(cli: &Cli) -> Result<PathBuf> {
    if let Some(ref dir) = cli.data_dir {
        return Ok(dir.clone());
    }
    // Try config: if exactly one vault registered, use it
    let cfg = config::load_config().unwrap_or_default();
    if cfg.vaults.len() == 1 {
        let entry = cfg.vaults.values().next().unwrap();
        return Ok(entry.data_dir.clone());
    }
    // Fall back to legacy default ~/.biem/
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
    let tag_pipeline = build_tag_pipeline(Some(&entry.data_dir));
    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        registry,
        bitmap_store,
    ).with_tag_pipeline(tag_pipeline);

    let result = pipeline.bulk_index(&vault_path)
        .context("initial index failed")?;

    println!("✓ Indexed {} documents ({} updated, {} skipped, {} tombstoned, {} bitmap keys, {}ms)",
        result.docs_indexed, result.docs_updated, result.docs_skipped, result.docs_tombstoned,
        result.bitmaps_created, result.duration_ms);

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
    let data_dir_resolved = if !cli.memory {
        let dd = resolve_data_dir_for_vault(path, cli)?;
        std::fs::create_dir_all(&dd)
            .with_context(|| format!("failed to create data dir: {}", dd.display()))?;
        eprintln!("Data dir: {}", dd.display());
        Some(dd)
    } else {
        None
    };

    let (registry, bitmap_store) = if cli.memory {
        open_memory_stores()
    } else {
        open_persistent_stores(data_dir_resolved.as_ref().unwrap())?
    };

    let tag_pipeline = build_tag_pipeline(data_dir_resolved.as_ref());
    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        registry,
        bitmap_store,
    ).with_tag_pipeline(tag_pipeline);

    let result = pipeline.bulk_index(path.as_path())
        .context("failed to index vault")?;

    println!("✓ Indexed {} documents", result.docs_indexed);
    println!("  {} updated, {} skipped, {} tombstoned", result.docs_updated, result.docs_skipped, result.docs_tombstoned);
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

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

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
    let engine = BitmapQueryEngine::new(bitmap_store, registry);

    let result = engine.inspect(path).context("inspect failed")?;

    match result {
        Some(r) => {
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("Document: {}", r.file_path.display());
                println!("  doc_id:       {}", r.doc_id);
                println!("  source:       {}", r.source_type);
                println!("  auto_type:    {}", r.auto_type.as_deref().unwrap_or("none"));
                println!("  blake3:       {}", r.blake3_hash);
                println!("  last_indexed: {}", r.last_indexed);
                println!("  chunks:       {}", r.chunks.len());
                for c in &r.chunks {
                    println!(
                        "    [{}] {} bytes {}..{} {}",
                        c.chunk_id, c.kind, c.byte_start, c.byte_end,
                        c.label.as_deref().unwrap_or("")
                    );
                }
                println!("  bitmaps:      {}", r.bitmap_keys.join(", "));
            }
        }
        None => {
            if cli.json {
                println!("{}", serde_json::json!({"error": "not found", "path": path.display().to_string()}));
            } else {
                println!("Not found in index: {}", path.display());
            }
        }
    }

    Ok(())
}

fn cmd_status(vault: Option<&PathBuf>, cli: &Cli) -> Result<()> {
    let (registry, bitmap_store) = build_pipeline_and_index(vault, cli)?;
    let engine = BitmapQueryEngine::new(bitmap_store, registry);
    let status = engine.status().context("failed to get status")?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("BIEM Index Status");
        println!("  documents:  {}", status.total_documents);
        println!("  bitmaps:    {}", status.total_bitmaps);
        println!("  tombstoned: {}", status.tombstoned);
        println!("  next_doc:   {}", status.next_doc_id);
        println!("  next_chunk: {}", status.next_chunk_id);
    }

    Ok(())
}

fn cmd_filters(vault: Option<&PathBuf>, category: Option<String>, cli: &Cli) -> Result<()> {
    let (_registry, bitmap_store) = build_pipeline_and_index(vault, cli)?;

    let prefix = category.as_deref().map(|c| format!("{c}:"));
    let keys = bitmap_store.list_keys(prefix.as_deref())
        .context("failed to list keys")?;

    if cli.json {
        let entries: Vec<serde_json::Value> = keys.iter().map(|key| {
            let card = bitmap_store.cardinality(key).unwrap_or(0);
            serde_json::json!({ "key": key, "cardinality": card })
        }).collect();
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    println!("{} filter keys{}:", keys.len(),
        category.as_ref().map(|c| format!(" (category: {c})")).unwrap_or_default());

    for key in &keys {
        let card = bitmap_store.cardinality(key).unwrap_or(0);
        println!("  {key}  ({card} docs)");
    }

    Ok(())
}

fn cmd_compact(vault: Option<&PathBuf>, cli: &Cli) -> Result<()> {
    let (registry, bitmap_store) = if cli.memory {
        let (r, b) = open_memory_stores();
        if let Some(v) = vault {
            let mut pipeline = IngestionPipeline::new(
                vec![Box::new(MarkdownParser)],
                r, b,
            );
            pipeline.bulk_index(v.as_path()).context("failed to index vault")?;
            let (_, r, b) = pipeline.into_parts();
            (r, b)
        } else {
            (r, b)
        }
    } else {
        let data_dir = resolve_data_dir(cli)?;
        if !data_dir.exists() {
            anyhow::bail!("data dir {} does not exist — nothing to compact", data_dir.display());
        }
        open_persistent_stores(&data_dir)?
    };

    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        registry,
        bitmap_store,
    );

    let result = pipeline.compact().context("compaction failed")?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("✓ Compaction complete");
        println!("  {} documents removed", result.docs_removed);
        println!("  {} bitmaps cleaned", result.bitmaps_cleaned);
        println!("  {}ms elapsed", result.duration_ms);
    }

    Ok(())
}

fn cmd_semantic(query: &str, filter_keys: &[String], top_k: usize, cli: &Cli) -> Result<()> {
    let data_dir = resolve_data_dir(cli)?;
    if !data_dir.exists() {
        anyhow::bail!(
            "data dir {} does not exist — run `biem init <vault>` first",
            data_dir.display()
        );
    }

    let (registry, bitmap_store) = open_persistent_stores(&data_dir)?;
    let engine = BitmapQueryEngine::new(bitmap_store, registry);

    // Build filter from keys (default to source:obsidian if no filters)
    let filter = if filter_keys.len() == 1 {
        Filter::Key(filter_keys[0].clone())
    } else if filter_keys.is_empty() {
        Filter::Key("source:obsidian".into())
    } else {
        Filter::And(filter_keys.iter().map(|k| Filter::Key(k.clone())).collect())
    };

    let request = SemanticQueryRequest {
        filter,
        query_text: query.to_string(),
        top_k,
        rerank: false,
    };

    // Initialize embedder and vector store
    eprintln!("Loading embedding model...");
    let embedder = FastEmbedEmbedder::new()
        .context("failed to initialize embedder")?;
    let vector_path = data_dir.join("vectors.usearch");
    let vector_store = UsearchVectorStore::new(&vector_path, embedder.dimension())
        .map_err(|e| anyhow::anyhow!("failed to open vector store: {e}"))?;

    let result = engine
        .semantic_query(&request, &embedder, &vector_store)
        .context("semantic query failed")?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    println!(
        "{} results (bitmap: {} candidates, vector: {} searched, {}μs bitmap + {}μs vector)",
        result.pointers.len(),
        result.bitmap_candidates,
        result.vector_searched,
        result.elapsed_bitmap_us,
        result.elapsed_vector_us,
    );
    println!();

    for sp in &result.pointers {
        println!(
            "  {:.3}  {} (doc_id={}, chunk_id={})",
            sp.score,
            sp.pointer.file_path.display(),
            sp.pointer.doc_id,
            sp.chunk_id,
        );
    }

    Ok(())
}

fn cmd_enrich(path: &PathBuf, force: bool, cli: &Cli) -> Result<()> {
    let data_dir = resolve_data_dir_for_vault(path, cli)?;
    if !data_dir.exists() {
        anyhow::bail!(
            "data dir {} does not exist — run `biem init <vault>` first",
            data_dir.display()
        );
    }

    let (registry, bitmap_store) = open_persistent_stores(&data_dir)?;
    let tag_pipeline = build_tag_pipeline(Some(&data_dir)).with_force(force);
    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        registry,
        bitmap_store,
    ).with_tag_pipeline(tag_pipeline);

    let result = pipeline.bulk_index(path.as_path())
        .context("enrichment re-index failed")?;

    println!("✓ Enrichment complete{}", if force { " (forced)" } else { "" });
    println!("  {} documents processed ({} updated, {} skipped)",
        result.docs_indexed, result.docs_updated, result.docs_skipped);
    println!("  {} bitmap keys, {}ms elapsed", result.bitmaps_created, result.duration_ms);

    Ok(())
}

/// Build the default tag pipeline with builtin taggers + any custom YAML taggers.
fn build_tag_pipeline(data_dir: Option<&PathBuf>) -> TagPipeline {
    let mut taggers: Vec<Box<dyn biem_core::enrich::Tagger>> = vec![
        Box::new(SizeTagger),
        Box::new(ConventionTagger),
        Box::new(TopicTagger),
        Box::new(ComplexityTagger),
    ];

    // Load custom YAML taggers from global and local dirs
    let home = std::env::var("HOME").unwrap_or_default();
    let global_taggers_dir = PathBuf::from(&home).join(".biem").join("taggers");
    if let Ok(custom) = load_yaml_taggers(&global_taggers_dir) {
        taggers.extend(custom);
    }

    // Local taggers (relative to data_dir's parent, i.e. vault root)
    // For now, just check .biem/taggers/ relative to data_dir
    if let Some(dd) = data_dir {
        let local_taggers_dir = dd.join("taggers");
        if let Ok(custom) = load_yaml_taggers(&local_taggers_dir) {
            taggers.extend(custom);
        }
    }

    let cache: Box<dyn biem_core::enrich::TaggerCache> = match data_dir {
        Some(dd) => match FsTaggerCache::new(dd) {
            Ok(c) => Box::new(c),
            Err(_) => Box::new(InMemoryTaggerCache::new()),
        },
        None => Box::new(InMemoryTaggerCache::new()),
    };

    TagPipeline::new(taggers, cache)
}

fn cmd_taggers() -> Result<()> {
    let pipeline = build_tag_pipeline(None);
    let names = pipeline.tagger_names();

    println!("Active taggers ({}):", names.len());
    for name in names {
        println!("  • {name}");
    }

    Ok(())
}

