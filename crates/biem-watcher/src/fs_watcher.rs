use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use globset::{Glob, GlobSet, GlobSetBuilder};
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use tracing::{debug, info, warn};

use biem_core::types::{ChangeEvent, ChangeKind};

// ── Config ───────────────────────────────────────────────────────

/// Configuration for the filesystem watcher.
#[derive(Debug, Clone)]
pub struct FsWatcherConfig {
    /// Root path to watch.
    pub root: PathBuf,
    /// Debounce duration in milliseconds.
    pub debounce_ms: u64,
    /// Glob patterns to ignore (e.g. ".obsidian/**", ".biem/**").
    pub ignore_patterns: Vec<String>,
}

impl Default for FsWatcherConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            debounce_ms: 500,
            ignore_patterns: vec![
                ".obsidian/**".into(),
                ".biem/**".into(),
                ".git/**".into(),
                ".DS_Store".into(),
            ],
        }
    }
}

// ── Error ────────────────────────────────────────────────────────

/// Errors from watcher operations.
#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("watch error: {0}")]
    Watch(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// ── SourceFeed trait ─────────────────────────────────────────────

/// Trait for any source of change events.
/// Filesystem watcher is the first implementation.
pub trait SourceFeed: Send {
    /// Start watching. Sends events to the provided channel.
    /// Blocks until stop() is called or an error occurs.
    fn start(&mut self, tx: mpsc::Sender<ChangeEvent>) -> Result<(), WatchError>;

    /// Signal the feed to stop.
    fn stop(&mut self) -> Result<(), WatchError>;
}

// ── FsWatcher ────────────────────────────────────────────────────

/// Filesystem watcher using the `notify` crate with debouncing.
pub struct FsWatcher {
    config: FsWatcherConfig,
    stop_flag: Arc<AtomicBool>,
    ignore_set: GlobSet,
}

impl FsWatcher {
    pub fn new(config: FsWatcherConfig) -> Result<Self, WatchError> {
        let ignore_set = build_glob_set(&config.root, &config.ignore_patterns)
            .map_err(|e| WatchError::Watch(format!("invalid ignore pattern: {e}")))?;

        // Canonicalize root to handle symlinks (e.g. /var → /private/var on macOS)
        let mut config = config;
        if let Ok(canonical) = config.root.canonicalize() {
            config.root = canonical;
        }

        Ok(Self {
            config,
            stop_flag: Arc::new(AtomicBool::new(false)),
            ignore_set,
        })
    }

    /// Check if a path should be ignored based on config patterns.
    fn is_ignored(&self, path: &std::path::Path) -> bool {
        // Match against path relative to root, or absolute
        if let Ok(rel) = path.strip_prefix(&self.config.root) {
            self.ignore_set.is_match(rel)
        } else {
            self.ignore_set.is_match(path)
        }
    }
    /// Get a cloneable stop handle that can be used from another thread.
    pub fn stop_handle(&self) -> StopHandle {
        StopHandle(self.stop_flag.clone())
    }
}

/// A thread-safe handle to stop a running watcher.
#[derive(Clone)]
pub struct StopHandle(Arc<AtomicBool>);

impl StopHandle {
    /// Signal the watcher to stop.
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl SourceFeed for FsWatcher {
    fn start(&mut self, tx: mpsc::Sender<ChangeEvent>) -> Result<(), WatchError> {
        use notify::RecursiveMode;

        self.stop_flag.store(false, Ordering::SeqCst);
        let stop_flag = self.stop_flag.clone();

        let debounce_duration = Duration::from_millis(self.config.debounce_ms);

        // Channel for debounced events from notify
        let (notify_tx, notify_rx) = mpsc::channel();

        let mut debouncer = new_debouncer(debounce_duration, notify_tx)
            .map_err(|e| WatchError::Watch(format!("failed to create debouncer: {e}")))?;

        debouncer
            .watcher()
            .watch(&self.config.root, RecursiveMode::Recursive)
            .map_err(|e| WatchError::Watch(format!("failed to start watching: {e}")))?;

        info!(root = %self.config.root.display(), "watcher started");

        // Event loop — blocks until stop() is called
        loop {
            if stop_flag.load(Ordering::SeqCst) {
                info!("watcher stopping (stop signal received)");
                break;
            }

            // Non-blocking recv with timeout so we can check stop_flag
            match notify_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(events)) => {
                    for event in events {
                        if self.is_ignored(&event.path) {
                            debug!(path = %event.path.display(), "ignoring event");
                            continue;
                        }

                        let change_kind = match event.kind {
                            DebouncedEventKind::Any => {
                                // Determine if created, modified, or deleted
                                if event.path.exists() {
                                    // Could be created or modified — we can't distinguish
                                    // from debounced events alone. The ingestion pipeline
                                    // handles both: if doc exists in registry → Modified,
                                    // otherwise → Created.
                                    ChangeKind::Modified
                                } else {
                                    ChangeKind::Deleted
                                }
                            }
                            DebouncedEventKind::AnyContinuous => continue,
                            _ => continue,
                        };

                        let change_event = ChangeEvent {
                            path: event.path.clone(),
                            kind: change_kind,
                        };

                        debug!(?change_event, "emitting change event");
                        if tx.send(change_event).is_err() {
                            warn!("receiver dropped, stopping watcher");
                            return Ok(());
                        }
                    }
                }
                Ok(Err(errs)) => {
                    warn!("watch error: {errs}");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Normal — just loop and check stop_flag
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    warn!("notify channel disconnected");
                    break;
                }
            }
        }

        Ok(())
    }

    fn stop(&mut self) -> Result<(), WatchError> {
        self.stop_flag.store(true, Ordering::SeqCst);
        Ok(())
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn build_glob_set(
    _root: &std::path::Path,
    patterns: &[String],
) -> Result<GlobSet, globset::Error> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }
    builder.build()
}
