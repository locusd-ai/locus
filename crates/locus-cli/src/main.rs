use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use locus_bitmap::lmdb::LmdbBitmapStore;
use locus_bitmap::memory::InMemoryBitmapStore;
use locus_core::bitmap::BitmapStore;
use locus_core::config::{self, RemoteSourceEntry, StorageMode};
use locus_core::graph::{
    CentralityAlgorithm, Direction, EdgeCategory, EdgeFilter, ExpandSpec, GraphOp,
    GraphQueryEngine, GraphQueryRequest, GraphStore,
};
use locus_core::query::{Filter, QueryEngine, QueryRequest};
use locus_core::registry::Registry;
use locus_core::semantic::Embedder;
use locus_core::types::SourceType;
use locus_confluence::{ConfluenceConfig, ConfluenceParser, ConfluenceSource};
use locus_embed::{FastEmbedEmbedder, UsearchVectorStore};
use locus_enrich::builtin::{ComplexityTagger, ConventionTagger, SizeTagger, TopicTagger};
use locus_enrich::{InMemoryTaggerCache, TagPipeline, FsTaggerCache, load_yaml_taggers};
use locus_ingest::IngestionPipeline;
use locus_jira::{JiraConfig, JiraParser, JiraSource};
use locus_parser::markdown::MarkdownParser;
use locus_code::CodeParser;
use locus_query::BitmapQueryEngine;
use locus_query::PetgraphQueryEngine;
use locus_query::SemanticQueryRequest;
use locus_registry::duckdb::DuckDbRegistry;
use locus_registry::graph::DuckDbGraphStore;
use locus_registry::memory::InMemoryRegistry;
use locus_slack::{SlackConfig, SlackParser, SlackSource};
use locus_watcher::remote::RemoteIngestionLoop;

#[derive(Parser)]
#[command(name = "locus", about = "Locus — local-first indexing for LLMs")]
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
    /// Register a source and run initial index
    Init {
        /// Path to the source directory
        path: PathBuf,
        /// Store state inside the vault (.locus/) instead of globally
        #[arg(long)]
        local: bool,
        /// Source type: obsidian (default) or code
        #[arg(long, value_name = "TYPE", default_value = "obsidian")]
        r#type: String,
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
    /// Graph traversal and analysis
    Graph {
        #[command(subcommand)]
        command: GraphCommands,
    },
    /// Manage remote sources (Confluence, Jira, Slack)
    Remote {
        #[command(subcommand)]
        command: RemoteCommands,
    },
    /// Set up Locus as an MCP server for AI agents (Claude Code, Cursor)
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },
}

#[derive(Subcommand)]
enum McpCommands {
    /// Register locusd as a stdio MCP server by writing .mcp.json
    Install {
        /// Vault or repo to serve (default: current directory)
        #[arg(default_value = ".")]
        vault: PathBuf,
        /// Path to the locusd binary (default: next to this binary, then $PATH)
        #[arg(long)]
        locusd: Option<PathBuf>,
        /// Directory whose .mcp.json to write (default: the vault directory)
        #[arg(long)]
        target: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum GraphCommands {
    /// Show direct neighbours of a file in the link graph
    Neighbours {
        /// File to query neighbours for
        path: PathBuf,
        /// Show incoming edges (backlinks) instead of outgoing
        #[arg(long)]
        incoming: bool,
        /// Show both incoming and outgoing edges
        #[arg(long)]
        both: bool,
        /// Filter by edge category: reference, dependency, hierarchy, workflow, provenance
        #[arg(long)]
        category: Option<String>,
        /// Maximum number of neighbours to show
        #[arg(long, default_value = "20")]
        limit: u32,
    },
    /// Multi-hop expansion from a file
    Expand {
        /// Starting file
        path: PathBuf,
        /// Number of hops to traverse
        #[arg(long, default_value = "1")]
        hops: u8,
        /// Traversal direction: outgoing, incoming, both
        #[arg(long, default_value = "outgoing")]
        direction: String,
        /// Filter by edge category
        #[arg(long)]
        category: Option<String>,
        /// Maximum nodes to include
        #[arg(long)]
        max_nodes: Option<u32>,
    },
    /// Shortest path between two files
    Path {
        /// Source file
        from: PathBuf,
        /// Target file
        to: PathBuf,
        /// Filter by edge category
        #[arg(long)]
        category: Option<String>,
    },
    /// Top documents by centrality score
    Central {
        /// Centrality algorithm: pagerank, indegree, outdegree
        #[arg(long, default_value = "pagerank")]
        algorithm: String,
        /// Number of top documents to return
        #[arg(long, default_value = "10")]
        limit: u32,
    },
    /// Graph statistics
    Stats,
}

#[derive(Subcommand)]
enum RemoteCommands {
    /// Register a new remote source
    Add {
        #[command(subcommand)]
        kind: RemoteAddCommands,
    },
    /// List registered remote sources and their poll status
    List,
    /// Run a one-shot poll for one or all remote sources
    Poll {
        /// Name of the remote source to poll (polls all if omitted)
        #[arg(long, short = 'n')]
        name: Option<String>,
        /// Force a full sync (ignore last_poll, fetch all items)
        #[arg(long)]
        full: bool,
    },
    /// Remove a remote source from the config
    Remove {
        /// Name of the remote source to remove
        name: String,
    },
}

#[derive(Subcommand)]
enum RemoteAddCommands {
    /// Register a Confluence Cloud space
    /// API token: set CONFLUENCE_API_TOKEN env var
    Confluence {
        /// Friendly name for this source (used as config key)
        #[arg(long)]
        name: String,
        /// Atlassian base URL, e.g. https://org.atlassian.net
        #[arg(long)]
        base_url: String,
        /// Atlassian account email
        #[arg(long)]
        username: String,
        /// Comma-separated space keys to index, e.g. ENG,OPS
        #[arg(long, value_delimiter = ',')]
        spaces: Vec<String>,
        /// Poll interval in seconds (default 300)
        #[arg(long, default_value = "300")]
        poll_interval_secs: u64,
    },
    /// Register a Jira Cloud project
    /// API token: set JIRA_API_TOKEN env var
    Jira {
        /// Friendly name for this source
        #[arg(long)]
        name: String,
        /// Atlassian base URL, e.g. https://org.atlassian.net
        #[arg(long)]
        base_url: String,
        /// Atlassian account email
        #[arg(long)]
        username: String,
        /// Comma-separated project keys to index, e.g. PROJ,INFRA
        #[arg(long, value_delimiter = ',')]
        projects: Vec<String>,
        /// Poll interval in seconds (default 300)
        #[arg(long, default_value = "300")]
        poll_interval_secs: u64,
    },
    /// Register Slack channels
    /// Bot token: set SLACK_BOT_TOKEN env var
    Slack {
        /// Friendly name for this source
        #[arg(long)]
        name: String,
        /// Comma-separated Slack channel IDs, e.g. C01234567,C09876543
        #[arg(long, value_delimiter = ',')]
        channel_ids: Vec<String>,
        /// Comma-separated human-readable channel names (same order as channel-ids)
        #[arg(long, value_delimiter = ',')]
        channel_names: Vec<String>,
        /// Poll interval in seconds (default 300)
        #[arg(long, default_value = "300")]
        poll_interval_secs: u64,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { ref path, local, ref r#type } => cmd_init(path, local, r#type),
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
        Commands::Graph { ref command } => cmd_graph(command, &cli),
        Commands::Remote { ref command } => cmd_remote(command),
        Commands::Mcp { ref command } => cmd_mcp(command),
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
    if cfg.sources.len() == 1 {
        let entry = cfg.sources.values().next().unwrap();
        return Ok(entry.data_dir.clone());
    }
    // Multiple sources: use the one containing the current directory
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(entry) = cfg.sources.values().find(|e| cwd.starts_with(&e.path)) {
            return Ok(entry.data_dir.clone());
        }
    }
    // Fall back to legacy default ~/.locus/
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".locus"))
}

/// Resolve data dir for a known vault path, using config.
fn resolve_data_dir_for_vault(vault_path: &PathBuf, cli: &Cli) -> Result<PathBuf> {
    if let Some(ref dir) = cli.data_dir {
        return Ok(dir.clone());
    }
    // Try config resolution
    let cfg = config::load_config().unwrap_or_default();
    match config::resolve_source(vault_path, &cfg) {
        Ok(entry) => Ok(entry.data_dir),
        Err(_) => {
            // Fall back to legacy default
            let home = std::env::var("HOME").context("HOME not set")?;
            Ok(PathBuf::from(home).join(".locus"))
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
            vec![Box::new(MarkdownParser), Box::new(CodeParser::new())],
            r,
            b,
        );
        pipeline.bulk_index(vault.as_path()).context("failed to index vault")?;
        let (_parsers, registry, bitmap_store) = pipeline.into_parts();
        (registry, bitmap_store)
    } else {
        // Persistent mode: stores already have data from previous `locus index`
        let data_dir = resolve_data_dir(cli)?;
        if !data_dir.exists() {
            anyhow::bail!(
                "data dir {} does not exist — run `locus index <vault>` first, or use --memory",
                data_dir.display()
            );
        }
        open_persistent_stores(&data_dir)?
    };
    Ok((registry, bitmap_store))
}

// ── Commands ─────────────────────────────────────────────────────

/// Build the parser list for a given source type.
fn parsers_for_source(source_type: &SourceType) -> Vec<Box<dyn locus_core::parser::Parser>> {
    match source_type {
        SourceType::Obsidian => vec![Box::new(MarkdownParser)],
        SourceType::Code => vec![Box::new(CodeParser::new())],
        SourceType::Custom(kind) => parsers_for_remote_kind(kind),
    }
}

/// Build the parser list for a remote source kind string.
fn parsers_for_remote_kind(kind: &str) -> Vec<Box<dyn locus_core::parser::Parser>> {
    match kind {
        "confluence" => vec![Box::new(ConfluenceParser)],
        "jira" => vec![Box::new(JiraParser)],
        "slack" => vec![Box::new(SlackParser)],
        // Unknown custom types fall back to markdown (best-effort)
        _ => vec![Box::new(MarkdownParser)],
    }
}

fn cmd_init(path: &PathBuf, local: bool, source_type_str: &str) -> Result<()> {
    let vault_path = path.canonicalize()
        .with_context(|| format!("path does not exist: {}", path.display()))?;

    let source_type = match source_type_str {
        "obsidian" => SourceType::Obsidian,
        "code" => SourceType::Code,
        other => SourceType::Custom(other.to_string()),
    };

    let locus_dir = config::default_config_dir()
        .context("could not determine Locus config directory")?;
    let mut cfg = config::load_config().unwrap_or_default();

    let storage = if local { StorageMode::Local } else { StorageMode::Global };

    let (name, entry) = config::register_source(&vault_path, source_type.clone(), storage, &locus_dir, &mut cfg)
        .with_context(|| format!("failed to register source: {}", vault_path.display()))?;

    config::save_config(&cfg)
        .context("failed to save config")?;

    println!("✓ Registered source '{}' ({:?}) at {}", name, source_type, vault_path.display());
    println!("  storage: {:?}", entry.storage);
    println!("  data_dir: {}", entry.data_dir.display());

    // Run initial bulk index
    let (registry, bitmap_store) = open_persistent_stores(&entry.data_dir)?;
    let tag_pipeline = build_tag_pipeline(Some(&entry.data_dir));
    let parsers = parsers_for_source(&source_type);
    let mut pipeline = IngestionPipeline::new(
        parsers,
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

    if cfg.sources.is_empty() {
        println!("No sources registered. Run `locus init <path>` to get started.");
        return Ok(());
    }

    println!("Registered sources:");
    for (name, entry) in &cfg.sources {
        println!();
        println!("  [{}]", name);
        println!("    path:     {}", entry.path.display());
        println!("    type:     {:?}", entry.source_type);
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

    // Determine source type from config
    let cfg = config::load_config().unwrap_or_default();
    let source_type: SourceType = config::resolve_source(path, &cfg)
        .map(|e| e.source_type.clone().into())
        .unwrap_or(SourceType::Obsidian);
    let parsers = parsers_for_source(&source_type);

    let tag_pipeline = build_tag_pipeline(data_dir_resolved.as_ref());
    let mut pipeline = IngestionPipeline::new(
        parsers,
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
        println!("Locus Index Status");
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
            "data dir {} does not exist — run `locus init <vault>` first",
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
        graph_expand: None,
    };

    // Initialize embedder and vector store
    eprintln!("Loading embedding model...");
    let embedder = FastEmbedEmbedder::new()
        .context("failed to initialize embedder")?;
    let vector_path = data_dir.join("vectors.usearch");
    let vector_store = UsearchVectorStore::new(&vector_path, embedder.dimension())
        .map_err(|e| anyhow::anyhow!("failed to open vector store: {e}"))?;

    let result = engine
        .semantic_query(&request, &embedder, &vector_store, None)
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
            "data dir {} does not exist — run `locus init <vault>` first",
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
    let mut taggers: Vec<Box<dyn locus_core::enrich::Tagger>> = vec![
        Box::new(SizeTagger),
        Box::new(ConventionTagger),
        Box::new(TopicTagger),
        Box::new(ComplexityTagger),
    ];

    // Load custom YAML taggers from global and local dirs
    let home = std::env::var("HOME").unwrap_or_default();
    let global_taggers_dir = PathBuf::from(&home).join(".locus").join("taggers");
    if let Ok(custom) = load_yaml_taggers(&global_taggers_dir) {
        taggers.extend(custom);
    }

    // Local taggers (relative to data_dir's parent, i.e. vault root)
    // For now, just check .locus/taggers/ relative to data_dir
    if let Some(dd) = data_dir {
        let local_taggers_dir = dd.join("taggers");
        if let Ok(custom) = load_yaml_taggers(&local_taggers_dir) {
            taggers.extend(custom);
        }
    }

    let cache: Box<dyn locus_core::enrich::TaggerCache> = match data_dir {
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

// ── Graph CLI ─────────────────────────────────────────────────────

fn open_graph_store(data_dir: &PathBuf) -> Result<(DuckDbRegistry, DuckDbGraphStore)> {
    let db_path = data_dir.join("registry.duckdb");
    let registry = DuckDbRegistry::new(db_path.to_str().unwrap())
        .context("failed to open DuckDB registry")?;
    let gs = DuckDbGraphStore::new(registry.connection());
    Ok((registry, gs))
}

fn lookup_doc_id(registry: &DuckDbRegistry, path: &PathBuf) -> Result<u32> {
    let canonical = path.canonicalize()
        .with_context(|| format!("path not found: {}", path.display()))?;
    let doc = registry.lookup_by_path(&canonical)?
        .or_else(|| registry.lookup_by_path(path).ok().flatten())
        .with_context(|| format!("not indexed: {}", path.display()))?;
    Ok(doc.doc_id)
}

fn doc_path_str(registry: &DuckDbRegistry, doc_id: u32) -> String {
    match registry.lookup_by_id(doc_id) {
        Ok(Some(doc)) => doc.file_path.display().to_string(),
        _ => format!("[doc:{doc_id}]"),
    }
}

fn parse_edge_filter(category: Option<&str>) -> Result<EdgeFilter> {
    match category {
        None => Ok(EdgeFilter::Any),
        Some(s) => {
            let cat: EdgeCategory = s.parse().map_err(|_| {
                anyhow::anyhow!(
                    "unknown category '{s}'. Valid: reference, dependency, hierarchy, workflow, provenance"
                )
            })?;
            Ok(EdgeFilter::Category(cat))
        }
    }
}

fn parse_direction(s: &str) -> Result<Direction> {
    match s {
        "outgoing" | "out" => Ok(Direction::Outgoing),
        "incoming" | "in" => Ok(Direction::Incoming),
        "both" => Ok(Direction::Both),
        other => Err(anyhow::anyhow!(
            "unknown direction '{other}'. Valid: outgoing, incoming, both"
        )),
    }
}

fn cmd_graph(command: &GraphCommands, cli: &Cli) -> Result<()> {
    let data_dir = resolve_data_dir(cli)?;
    if !data_dir.exists() {
        anyhow::bail!(
            "data dir {} does not exist — run `locus init <vault>` first",
            data_dir.display()
        );
    }

    let (registry, mut gs) = open_graph_store(&data_dir)?;
    gs.rebuild_in_memory().context("failed to rebuild graph index")?;
    let gs_arc: Arc<dyn GraphStore> = Arc::new(gs);
    let engine = PetgraphQueryEngine::new(gs_arc.clone());

    match command {
        GraphCommands::Stats => {
            let stats = gs_arc.stats().map_err(|e| anyhow::anyhow!(e))?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                println!("Graph Statistics");
                println!("  nodes:  {}", stats.node_count);
                println!("  edges:  {}", stats.edge_count);
                if !stats.edges_by_category.is_empty() {
                    println!("  by category:");
                    let mut cats: Vec<_> = stats.edges_by_category.iter().collect();
                    cats.sort_by_key(|(k, _)| k.as_str());
                    for (cat, count) in cats {
                        println!("    {cat}: {count}");
                    }
                }
            }
        }

        GraphCommands::Neighbours { path, incoming, both, category, limit } => {
            let doc_id = lookup_doc_id(&registry, path)?;
            let direction = if *both {
                Direction::Both
            } else if *incoming {
                Direction::Incoming
            } else {
                Direction::Outgoing
            };
            let edge_filter = parse_edge_filter(category.as_deref())?;

            let result = engine
                .query(GraphQueryRequest {
                    op: GraphOp::Neighbours { from: doc_id, direction },
                    edge_filter,
                    limit: Some(*limit),
                })
                .map_err(|e| anyhow::anyhow!(e))?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("{} neighbour(s) of {}:", result.nodes.len(), path.display());
                for node in &result.nodes {
                    let node_path = if node.file_path.as_os_str().is_empty() {
                        doc_path_str(&registry, node.doc_id)
                    } else {
                        node.file_path.display().to_string()
                    };
                    println!("  {} {}", node.doc_id, node_path);
                }
                for edge in &result.edges {
                    println!("  {} -> {} ({})", edge.from, edge.to, edge.kind);
                }
            }
        }

        GraphCommands::Expand { path, hops, direction, category, max_nodes } => {
            let doc_id = lookup_doc_id(&registry, path)?;
            let dir = parse_direction(direction)?;
            let edge_filter = parse_edge_filter(category.as_deref())?;
            let spec = ExpandSpec {
                hops: *hops,
                direction: dir,
                edge_filter: edge_filter.clone(),
                max_nodes: *max_nodes,
                include_seeds: false,
            };

            let result = engine
                .query(GraphQueryRequest {
                    op: GraphOp::Expand { seeds: vec![doc_id], spec },
                    edge_filter,
                    limit: None,
                })
                .map_err(|e| anyhow::anyhow!(e))?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "{} nodes reachable from {} ({} hop(s), {:?}):",
                    result.nodes.len(),
                    path.display(),
                    hops,
                    dir,
                );
                for node in &result.nodes {
                    let node_path = if node.file_path.as_os_str().is_empty() {
                        doc_path_str(&registry, node.doc_id)
                    } else {
                        node.file_path.display().to_string()
                    };
                    let hop_str = node
                        .hop_distance
                        .map(|h| format!(" (hop {h})"))
                        .unwrap_or_default();
                    println!("  {node_path}{hop_str}");
                }
            }
        }

        GraphCommands::Path { from, to, category } => {
            let from_id = lookup_doc_id(&registry, from)?;
            let to_id = lookup_doc_id(&registry, to)?;
            let edge_filter = parse_edge_filter(category.as_deref())?;

            let path_ids = engine
                .shortest_path(from_id, to_id, &edge_filter)
                .map_err(|e| anyhow::anyhow!(e))?;

            if cli.json {
                let nodes: Vec<_> = path_ids
                    .iter()
                    .map(|&id| {
                        serde_json::json!({ "doc_id": id, "path": doc_path_str(&registry, id) })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&nodes)?);
            } else {
                println!("Shortest path: {} hop(s)", path_ids.len().saturating_sub(1));
                for id in &path_ids {
                    println!("  {}", doc_path_str(&registry, *id));
                }
            }
        }

        GraphCommands::Central { algorithm, limit } => {
            let algo = match algorithm.as_str() {
                "pagerank" | "pr" => CentralityAlgorithm::PageRank { iterations: 100, damping: 0.85 },
                "indegree" | "in" => CentralityAlgorithm::InDegree,
                "outdegree" | "out" => CentralityAlgorithm::OutDegree,
                other => anyhow::bail!(
                    "unknown algorithm '{other}'. Valid: pagerank, indegree, outdegree"
                ),
            };

            let scores = engine
                .centrality(algo, None)
                .map_err(|e| anyhow::anyhow!(e))?;
            let limited: Vec<_> = scores.into_iter().take(*limit as usize).collect();

            if cli.json {
                let out: Vec<_> = limited
                    .iter()
                    .map(|(id, score)| {
                        serde_json::json!({
                            "doc_id": id,
                            "path": doc_path_str(&registry, *id),
                            "score": score,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("Top {} by {} centrality:", limited.len(), algorithm);
                for (i, (id, score)) in limited.iter().enumerate() {
                    println!("  {:2}. {:.4}  {}", i + 1, score, doc_path_str(&registry, *id));
                }
            }
        }
    }

    Ok(())
}

// ── locus remote ─────────────────────────────────────────────────────────────

fn cmd_remote(command: &RemoteCommands) -> Result<()> {
    match command {
        RemoteCommands::Add { kind } => cmd_remote_add(kind),
        RemoteCommands::List => cmd_remote_list(),
        RemoteCommands::Poll { name, full } => cmd_remote_poll(name.as_deref(), *full),
        RemoteCommands::Remove { name } => cmd_remote_remove(name),
    }
}

fn cmd_remote_add(kind: &RemoteAddCommands) -> Result<()> {
    let locus_dir = config::default_config_dir().context("could not find home directory")?;
    let mut cfg = config::load_config().unwrap_or_default();

    let (name, entry) = match kind {
        RemoteAddCommands::Confluence { name, base_url, username, spaces, poll_interval_secs } => {
            let entry = RemoteSourceEntry {
                kind: "confluence".to_string(),
                data_dir: PathBuf::new(),
                last_poll: None,
                poll_interval_secs: *poll_interval_secs,
                base_url: Some(base_url.clone()),
                username: Some(username.clone()),
                spaces: Some(spaces.clone()),
                projects: None,
                channel_ids: None,
                channel_names: None,
            };
            (name.clone(), entry)
        }
        RemoteAddCommands::Jira { name, base_url, username, projects, poll_interval_secs } => {
            let entry = RemoteSourceEntry {
                kind: "jira".to_string(),
                data_dir: PathBuf::new(),
                last_poll: None,
                poll_interval_secs: *poll_interval_secs,
                base_url: Some(base_url.clone()),
                username: Some(username.clone()),
                spaces: None,
                projects: Some(projects.clone()),
                channel_ids: None,
                channel_names: None,
            };
            (name.clone(), entry)
        }
        RemoteAddCommands::Slack { name, channel_ids, channel_names, poll_interval_secs } => {
            let entry = RemoteSourceEntry {
                kind: "slack".to_string(),
                data_dir: PathBuf::new(),
                last_poll: None,
                poll_interval_secs: *poll_interval_secs,
                base_url: None,
                username: None,
                spaces: None,
                projects: None,
                channel_ids: Some(channel_ids.clone()),
                channel_names: Some(channel_names.clone()),
            };
            (name.clone(), entry)
        }
    };

    config::register_remote_source(&name, entry, &locus_dir, &mut cfg)
        .with_context(|| format!("failed to register remote source '{name}'"))?;
    config::save_config(&cfg).context("failed to save config")?;

    println!("Remote source '{}' registered.", name);
    let tip = match kind {
        RemoteAddCommands::Confluence { name, .. } =>
            format!("Set CONFLUENCE_API_TOKEN, then run: locus remote poll --name {name}"),
        RemoteAddCommands::Jira { name, .. } =>
            format!("Set JIRA_API_TOKEN, then run: locus remote poll --name {name}"),
        RemoteAddCommands::Slack { name, .. } =>
            format!("Set SLACK_BOT_TOKEN, then run: locus remote poll --name {name}"),
    };
    println!("Next: {tip}");
    Ok(())
}

fn cmd_remote_list() -> Result<()> {
    let cfg = config::load_config().unwrap_or_default();
    if cfg.remote_sources.is_empty() {
        println!("No remote sources registered. Use `locus remote add` to register one.");
        return Ok(());
    }
    println!("{:<20} {:<12} {:<10} {}", "NAME", "KIND", "INTERVAL", "LAST POLL");
    println!("{}", "-".repeat(70));
    for (name, entry) in &cfg.remote_sources {
        let last = entry.last_poll.map(format_timestamp).unwrap_or_else(|| "never".to_string());
        let summary = match entry.kind.as_str() {
            "confluence" => format!("spaces: {}", entry.spaces.as_deref().unwrap_or_default().join(", ")),
            "jira" => format!("projects: {}", entry.projects.as_deref().unwrap_or_default().join(", ")),
            "slack" => {
                let names = entry.channel_names.as_deref().unwrap_or_default();
                format!("channels: {}", names.iter().map(|n| format!("#{n}")).collect::<Vec<_>>().join(", "))
            }
            _ => String::new(),
        };
        println!("{:<20} {:<12} {:<10} {}  ({})", name, entry.kind, entry.poll_interval_secs, last, summary);
    }
    Ok(())
}

fn cmd_remote_poll(name: Option<&str>, full: bool) -> Result<()> {
    let locus_dir = config::default_config_dir().context("could not find home directory")?;
    let mut cfg = config::load_config().unwrap_or_default();

    let names: Vec<String> = if let Some(n) = name {
        if !cfg.remote_sources.contains_key(n) {
            anyhow::bail!("remote source '{}' not found — use `locus remote list`", n);
        }
        vec![n.to_string()]
    } else {
        cfg.remote_sources.keys().cloned().collect()
    };

    if names.is_empty() {
        println!("No remote sources registered. Use `locus remote add` first.");
        return Ok(());
    }

    for source_name in &names {
        let entry = cfg.remote_sources[source_name].clone();
        let since = if full { None } else { entry.last_poll };

        println!(
            "Polling '{}' ({}) since {}...",
            source_name, entry.kind,
            since.map(format_timestamp).unwrap_or_else(|| "beginning".to_string())
        );

        let db_path = entry.data_dir.join("registry.duckdb");
        let lmdb_path = entry.data_dir.join("bitmaps.lmdb");
        let registry = DuckDbRegistry::new(db_path.to_str().unwrap())
            .with_context(|| format!("failed to open DuckDB for '{source_name}'"))?;
        let bitmap_store = LmdbBitmapStore::new(&lmdb_path)
            .with_context(|| format!("failed to open LMDB for '{source_name}'"))?;

        let parsers = parsers_for_remote_kind(&entry.kind);
        let mut pipeline = IngestionPipeline::new(parsers, Box::new(registry), Box::new(bitmap_store));

        let remote_source: Box<dyn locus_watcher::remote::RemoteSource> = match entry.kind.as_str() {
            "confluence" => {
                let api_token = std::env::var("CONFLUENCE_API_TOKEN")
                    .context("CONFLUENCE_API_TOKEN not set")?;
                Box::new(ConfluenceSource::new(ConfluenceConfig {
                    base_url: entry.base_url.unwrap_or_default(),
                    username: entry.username.unwrap_or_default(),
                    api_token,
                    spaces: entry.spaces.unwrap_or_default(),
                    poll_interval_secs: entry.poll_interval_secs,
                }))
            }
            "jira" => {
                let api_token = std::env::var("JIRA_API_TOKEN")
                    .context("JIRA_API_TOKEN not set")?;
                Box::new(JiraSource::new(JiraConfig {
                    base_url: entry.base_url.unwrap_or_default(),
                    username: entry.username.unwrap_or_default(),
                    api_token,
                    projects: entry.projects.unwrap_or_default(),
                    poll_interval_secs: entry.poll_interval_secs,
                }))
            }
            "slack" => {
                let bot_token = std::env::var("SLACK_BOT_TOKEN")
                    .context("SLACK_BOT_TOKEN not set")?;
                Box::new(SlackSource::new(SlackConfig {
                    bot_token,
                    channel_ids: entry.channel_ids.unwrap_or_default(),
                    channel_names: entry.channel_names.unwrap_or_default(),
                    poll_interval_secs: entry.poll_interval_secs,
                }))
            }
            other => anyhow::bail!("unknown remote source kind: '{other}'"),
        };

        let mut loop_ = RemoteIngestionLoop::new(
            remote_source,
            std::time::Duration::from_secs(entry.poll_interval_secs),
        );

        let (count, new_ts) = loop_.poll_once(since, |path, bytes| {
            if let Err(e) = pipeline.upsert_document(path.clone(), bytes) {
                eprintln!("  warn: failed to ingest {}: {e}", path.display());
            }
        });

        println!("  ingested {count} item(s)");

        if let Some(ts) = new_ts {
            config::update_remote_last_poll(source_name, ts, &mut cfg)
                .context("failed to update last_poll")?;
            config::save_config(&cfg).context("failed to save config")?;
        }
    }

    let _ = locus_dir;
    Ok(())
}

fn cmd_remote_remove(name: &str) -> Result<()> {
    let mut cfg = config::load_config().unwrap_or_default();
    config::remove_remote_source(name, &mut cfg)
        .with_context(|| format!("remote source '{}' not found", name))?;
    config::save_config(&cfg).context("failed to save config")?;
    println!("Remote source '{}' removed from config.", name);
    println!("Note: state directory is NOT deleted. Remove it manually if needed.");
    Ok(())
}

fn format_timestamp(ts: i64) -> String {
    let secs = ts.max(0) as u64;
    let days = secs / 86400;
    let (y, m, d) = days_to_ymd(days);
    let h = (secs % 86400) / 3600;
    let min = (secs % 3600) / 60;
    format!("{:04}-{:02}-{:02} {:02}:{:02}Z", y, m, d, h, min)
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}


// ── MCP setup ────────────────────────────────────────────────────

fn cmd_mcp(command: &McpCommands) -> Result<()> {
    match command {
        McpCommands::Install { vault, locusd, target } => cmd_mcp_install(vault, locusd.as_ref(), target.as_ref()),
    }
}

fn resolve_locusd(explicit: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return p.canonicalize().with_context(|| format!("locusd not found at {}", p.display()));
    }
    // Prefer a locusd sitting next to this locus binary (cargo install puts them together)
    if let Ok(me) = std::env::current_exe() {
        let sibling = me.with_file_name("locusd");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    // Fall back to $PATH
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("locusd");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    anyhow::bail!("locusd binary not found next to locus or on $PATH — pass --locusd /path/to/locusd")
}

fn cmd_mcp_install(vault: &PathBuf, locusd: Option<&PathBuf>, target: Option<&PathBuf>) -> Result<()> {
    let vault = vault
        .canonicalize()
        .with_context(|| format!("vault path does not exist: {}", vault.display()))?;
    let locusd = resolve_locusd(locusd)?;
    let target_dir = match target {
        Some(t) => t.canonicalize().with_context(|| format!("target dir does not exist: {}", t.display()))?,
        None => vault.clone(),
    };
    let config_path = target_dir.join(".mcp.json");

    let mut root: serde_json::Value = if config_path.is_file() {
        let raw = std::fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read {}", config_path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("{} is not valid JSON — fix or remove it first", config_path.display()))?
    } else {
        serde_json::json!({})
    };

    if !root.is_object() {
        anyhow::bail!("{} does not contain a JSON object", config_path.display());
    }
    let servers = root
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        anyhow::bail!("\"mcpServers\" in {} is not an object", config_path.display());
    }
    servers.as_object_mut().unwrap().insert(
        "locus".to_string(),
        serde_json::json!({
            "command": locusd.to_string_lossy(),
            "args": [vault.to_string_lossy(), "--mcp"],
        }),
    );

    std::fs::write(&config_path, serde_json::to_string_pretty(&root)? + "\n")
        .with_context(|| format!("failed to write {}", config_path.display()))?;

    println!("✓ Registered Locus MCP server in {}", config_path.display());
    println!("  command: {} {} --mcp", locusd.display(), vault.display());
    println!();
    println!("  Claude Code picks this up automatically next time it starts in {}", target_dir.display());
    println!("  Make sure the vault is indexed first:  locus index {}", vault.display());
    Ok(())
}
