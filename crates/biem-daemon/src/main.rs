use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use biem_bitmap::lmdb::LmdbBitmapStore;
use biem_bitmap::memory::InMemoryBitmapStore;
use biem_core::types::ChangeEvent;
use biem_ingest::IngestionPipeline;
use biem_parser::markdown::MarkdownParser;
use biem_query::BitmapQueryEngine;
use biem_registry::duckdb::DuckDbRegistry;
use biem_registry::memory::InMemoryRegistry;
use biem_watcher::{FsWatcher, FsWatcherConfig, SourceFeed};
use rmcp::ServiceExt;

mod http;
mod mcp;

#[derive(Parser)]
#[command(name = "biemd", about = "BIEM daemon — watcher, ingestion, and query server")]
struct Cli {
    /// Path to the vault to watch and index
    vault: PathBuf,

    /// Data directory for persistent storage (default: ~/.biem)
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Use in-memory storage (no persistence)
    #[arg(long, default_value = "false")]
    memory: bool,

    /// Debounce duration in milliseconds
    #[arg(long, default_value = "500")]
    debounce_ms: u64,

    /// Perform an initial bulk index before watching (skipped if already indexed)
    #[arg(long, default_value = "true")]
    initial_index: bool,

    /// Run as MCP server over stdio (no watcher)
    #[arg(long)]
    mcp: bool,

    /// Run HTTP API server
    #[arg(long)]
    http: bool,

    /// HTTP API port (default: 3141)
    #[arg(long, default_value = "3141")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let vault = cli.vault.canonicalize()
        .context("vault path does not exist")?;

    info!(vault = %vault.display(), "starting biemd");

    // Build stores
    let data_dir_resolved = resolve_daemon_data_dir(&cli, &vault)?;
    let (mut registry, mut bitmap_store): (Box<dyn biem_core::registry::Registry>, Box<dyn biem_core::bitmap::BitmapStore>) = if cli.memory {
        info!("using in-memory storage");
        (Box::new(InMemoryRegistry::new()), Box::new(InMemoryBitmapStore::new()))
    } else {
        let data_dir = data_dir_resolved.clone();
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("failed to create data dir: {}", data_dir.display()))?;

        info!(data_dir = %data_dir.display(), "using persistent storage");

        let db_path = data_dir.join("registry.duckdb");
        let registry = DuckDbRegistry::new(db_path.to_str().unwrap())
            .context("failed to open DuckDB registry")?;

        let lmdb_path = data_dir.join("bitmaps.lmdb");
        let bitmap_store = LmdbBitmapStore::new(&lmdb_path)
            .context("failed to open LMDB bitmap store")?;

        (Box::new(registry), Box::new(bitmap_store))
    };

    if cli.mcp || cli.http {
        if cli.memory {
            anyhow::bail!("MCP/HTTP mode requires persistent storage (--memory not supported)");
        }

        // Use the first set of stores for initial indexing
        if cli.initial_index {
            info!("performing initial bulk index");
            let mut pipeline = IngestionPipeline::new(
                vec![Box::new(MarkdownParser)],
                registry,
                bitmap_store,
            );
            let result = pipeline.bulk_index(&vault)
                .context("initial bulk index failed")?;
            info!(
                indexed = result.docs_indexed,
                updated = result.docs_updated,
                skipped = result.docs_skipped,
                tombstoned = result.docs_tombstoned,
                bitmaps = result.bitmaps_created,
                ms = result.duration_ms,
                "initial index complete"
            );
        }

        // Open fresh handles for the query engine
        let data_dir = data_dir_resolved.clone();
        let db_path = data_dir.join("registry.duckdb");
        let lmdb_path = data_dir.join("bitmaps.lmdb");

        let qe_registry: Box<dyn biem_core::registry::Registry> = Box::new(
            DuckDbRegistry::new(db_path.to_str().unwrap()).context("failed to open DuckDB for query engine")?
        );
        let qe_bitmap: Box<dyn biem_core::bitmap::BitmapStore> = Box::new(
            LmdbBitmapStore::new(&lmdb_path).context("failed to open LMDB for query engine")?
        );
        let engine: Arc<dyn biem_core::query::QueryEngine> = Arc::new(
            BitmapQueryEngine::new(qe_bitmap, qe_registry),
        );

        // Launch HTTP server (background task if also running MCP)
        if cli.http {
            let addr = format!("0.0.0.0:{}", cli.port);
            let app = http::router(engine.clone());
            let listener = tokio::net::TcpListener::bind(&addr).await
                .with_context(|| format!("failed to bind HTTP on {addr}"))?;
            info!(addr = %addr, "starting HTTP API server");

            if cli.mcp {
                // Run HTTP in background, MCP in foreground
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener, app).await {
                        error!("HTTP server error: {e}");
                    }
                });
            } else {
                // HTTP-only mode: block on it
                info!("HTTP-only mode — press Ctrl+C to stop");
                axum::serve(listener, app).await
                    .context("HTTP server error")?;
                return Ok(());
            }
        }

        if cli.mcp {
            let server = mcp::BiemMcpServer::new(engine);
            info!("starting MCP server over stdio");
            let transport = rmcp::transport::io::stdio();
            let service = server.serve(transport).await
                .context("MCP server failed to start")?;
            service.waiting().await
                .map_err(|e| anyhow::anyhow!("MCP service error: {e}"))?;
            info!("MCP server stopped");
        }

        return Ok(());
    }

    // Initial bulk index (watcher mode)
    if cli.initial_index {
        let mut pipeline = IngestionPipeline::new(
            vec![Box::new(MarkdownParser)],
            registry,
            bitmap_store,
        );
        info!("performing initial bulk index");
        let result = pipeline.bulk_index(&vault)
            .context("initial bulk index failed")?;
        info!(
            indexed = result.docs_indexed,
            updated = result.docs_updated,
            skipped = result.docs_skipped,
            tombstoned = result.docs_tombstoned,
            bitmaps = result.bitmaps_created,
            ms = result.duration_ms,
            "initial index complete"
        );
        let (_, r, b) = pipeline.into_parts();
        registry = r;
        bitmap_store = b;
    }

    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        registry,
        bitmap_store,
    );

    // Set up watcher
    let config = FsWatcherConfig {
        root: vault.clone(),
        debounce_ms: cli.debounce_ms,
        ..FsWatcherConfig::default()
    };

    let mut watcher = FsWatcher::new(config)
        .context("failed to create watcher")?;
    let stop_handle = watcher.stop_handle();

    let (tx, rx) = mpsc::channel::<ChangeEvent>();

    let watcher_handle = tokio::task::spawn_blocking(move || {
        if let Err(e) = watcher.start(tx) {
            error!("watcher error: {e}");
        }
    });

    let ingest_handle = tokio::task::spawn_blocking(move || {
        run_ingestion_loop(pipeline, rx);
    });

    info!("biemd running — press Ctrl+C to stop");
    tokio::signal::ctrl_c().await?;

    info!("shutting down");
    stop_handle.stop();

    let _ = watcher_handle.await;
    let _ = ingest_handle.await;

    info!("biemd stopped");
    Ok(())
}

fn resolve_daemon_data_dir(cli: &Cli, vault: &PathBuf) -> Result<PathBuf> {
    if let Some(ref dir) = cli.data_dir {
        return Ok(dir.clone());
    }
    let cfg = biem_core::config::load_config().unwrap_or_default();
    match biem_core::config::resolve_vault(vault, &cfg) {
        Ok(entry) => Ok(entry.data_dir),
        Err(_) => {
            let home = std::env::var("HOME").context("HOME not set")?;
            Ok(PathBuf::from(home).join(".biem"))
        }
    }
}

fn run_ingestion_loop(mut pipeline: IngestionPipeline, rx: mpsc::Receiver<ChangeEvent>) {
    for event in rx {
        info!(path = %event.path.display(), kind = ?event.kind, "processing event");
        match pipeline.process_event(&event) {
            Ok(result) => {
                info!(
                    action = ?result.action,
                    bitmaps = result.bitmaps_updated,
                    "event processed"
                );
            }
            Err(e) => {
                warn!(path = %event.path.display(), error = %e, "ingestion error");
            }
        }
    }
    info!("ingestion loop exiting (channel closed)");
}
