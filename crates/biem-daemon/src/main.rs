use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use biem_bitmap::memory::InMemoryBitmapStore;
use biem_core::types::ChangeEvent;
use biem_ingest::IngestionPipeline;
use biem_parser::markdown::MarkdownParser;
use biem_registry::memory::InMemoryRegistry;
use biem_watcher::{FsWatcher, FsWatcherConfig, SourceFeed};

#[derive(Parser)]
#[command(name = "biemd", about = "BIEM daemon — watcher, ingestion, and query server")]
struct Cli {
    /// Path to the vault to watch and index
    vault: PathBuf,

    /// Debounce duration in milliseconds
    #[arg(long, default_value = "500")]
    debounce_ms: u64,

    /// Perform an initial bulk index before watching
    #[arg(long, default_value = "true")]
    initial_index: bool,
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

    // Build the ingestion pipeline (sync, in-memory for now)
    let mut pipeline = IngestionPipeline::new(
        vec![Box::new(MarkdownParser)],
        Box::new(InMemoryRegistry::new()),
        Box::new(InMemoryBitmapStore::new()),
    );

    // Initial bulk index
    if cli.initial_index {
        info!("performing initial bulk index");
        let result = pipeline.bulk_index(&vault)
            .context("initial bulk index failed")?;
        info!(
            docs = result.docs_indexed,
            bitmaps = result.bitmaps_created,
            ms = result.duration_ms,
            "initial index complete"
        );
    }

    // Set up watcher
    let config = FsWatcherConfig {
        root: vault.clone(),
        debounce_ms: cli.debounce_ms,
        ..FsWatcherConfig::default()
    };

    let mut watcher = FsWatcher::new(config)
        .context("failed to create watcher")?;
    let stop_handle = watcher.stop_handle();

    // Channel: watcher → ingestion loop
    let (tx, rx) = mpsc::channel::<ChangeEvent>();

    // Spawn watcher in a blocking thread (it blocks on start())
    let watcher_handle = tokio::task::spawn_blocking(move || {
        if let Err(e) = watcher.start(tx) {
            error!("watcher error: {e}");
        }
    });

    // Spawn ingestion loop in a blocking thread (sync pipeline)
    let ingest_handle = tokio::task::spawn_blocking(move || {
        run_ingestion_loop(pipeline, rx);
    });

    // Wait for ctrl-c
    info!("biemd running — press Ctrl+C to stop");
    tokio::signal::ctrl_c().await?;

    info!("shutting down");
    stop_handle.stop();

    // Wait for threads to finish
    let _ = watcher_handle.await;
    let _ = ingest_handle.await;

    info!("biemd stopped");
    Ok(())
}

/// Sync ingestion loop: reads ChangeEvents from the channel and processes them.
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
