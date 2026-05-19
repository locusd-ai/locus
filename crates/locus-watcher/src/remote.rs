//! RemoteSource trait — polling integration for non-filesystem sources.
//!
//! Implementations poll remote APIs (Confluence, Jira, Slack, etc.) for
//! changes and feed raw bytes to `IngestionPipeline::upsert_document()`.
//! The pipeline handles parsing, hashing, and bitmap updates.

use std::path::PathBuf;
use std::time::Duration;

use locus_core::types::Timestamp;

// ── Core types ────────────────────────────────────────────────────

/// An item retrieved from a remote source.
#[derive(Debug, Clone)]
pub struct RemoteItem {
    /// Source-scoped unique identifier (e.g. Confluence page ID, Jira issue key).
    pub id: String,
    /// Stable synthetic path used as the document's registry key.
    /// Convention: `<source_id>://<scope>/<id>.<ext>`
    /// Examples: `confluence://SPACE/12345.confluence`, `jira://PROJ/PROJ-123.jira`
    pub synthetic_path: PathBuf,
    /// Human-readable title.
    pub title: String,
    /// Last modification timestamp (seconds since Unix epoch).
    pub updated_at: Timestamp,
    /// True when the item has been deleted in the remote system.
    pub deleted: bool,
}

/// Errors that can occur during remote source operations.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("HTTP error {status}: {body}")]
    Http { status: u16, body: String },
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("rate limited — retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("remote error: {0}")]
    Remote(String),
}

// ── Trait ─────────────────────────────────────────────────────────

/// Polling-based integration with a remote content source.
///
/// Implementations discover changed items via `poll()` and return their
/// raw bytes via `fetch_content()`. The bytes are passed directly to
/// `IngestionPipeline::upsert_document()` which invokes the matching
/// `Parser` implementation.
///
/// Parser selection is done by `can_parse(synthetic_path)`, so parsers
/// should check the path's "extension" or scheme convention:
/// - `.confluence` → ConfluenceParser
/// - `.jira`       → JiraParser
/// - `.slack`      → SlackParser
pub trait RemoteSource: Send {
    /// Bitmap key suffix used for `source:*` keys (e.g. `"confluence"`, `"jira"`).
    fn source_id(&self) -> &str;

    /// Poll for items created or modified since `since`.
    /// `None` requests a full sync (all items).
    fn poll(&mut self, since: Option<Timestamp>) -> Result<Vec<RemoteItem>, RemoteError>;

    /// Fetch the serialised content for a specific item.
    /// The returned bytes are passed verbatim to the parser's `parse()` method.
    /// Convention: return JSON containing both metadata and content body.
    fn fetch_content(&self, item: &RemoteItem) -> Result<Vec<u8>, RemoteError>;
}

// ── Polling loop ──────────────────────────────────────────────────

/// Drives a `RemoteSource` in a background loop, calling an ingest callback
/// for each changed item.
///
/// # Usage
///
/// ```ignore
/// let mut pipeline = IngestionPipeline::new(parsers, registry, bitmap_store);
/// let mut loop_ = RemoteIngestionLoop::new(
///     Box::new(ConfluenceSource::new(config)),
///     Duration::from_secs(300),
/// );
/// std::thread::spawn(move || {
///     loop_.run(|path, bytes| {
///         let _ = pipeline.upsert_document(path, bytes);
///     });
/// });
/// ```
pub struct RemoteIngestionLoop {
    source: Box<dyn RemoteSource>,
    poll_interval: Duration,
}

impl RemoteIngestionLoop {
    pub fn new(source: Box<dyn RemoteSource>, poll_interval: Duration) -> Self {
        Self { source, poll_interval }
    }

    /// Execute one poll cycle. Returns the number of items successfully ingested.
    /// `ingest_fn` is called with `(synthetic_path, content_bytes)` for each item.
    pub fn poll_once<F>(
        &mut self,
        since: Option<Timestamp>,
        mut ingest_fn: F,
    ) -> (u32, Option<Timestamp>)
    where
        F: FnMut(PathBuf, Vec<u8>),
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as Timestamp;

        let items = match self.source.poll(since) {
            Ok(items) => items,
            Err(e) => {
                tracing::warn!(source = self.source.source_id(), error = %e, "poll failed");
                return (0, since);
            }
        };

        let mut count = 0u32;
        for item in items {
            if item.deleted {
                continue;
            }
            match self.source.fetch_content(&item) {
                Ok(bytes) => {
                    ingest_fn(item.synthetic_path, bytes);
                    count += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        source = self.source.source_id(),
                        id = %item.id,
                        error = %e,
                        "failed to fetch item content"
                    );
                }
            }
        }

        (count, Some(now))
    }

    /// Run indefinitely. Blocks the calling thread.
    ///
    /// Call from a dedicated `std::thread::spawn` — do NOT call from a tokio task.
    pub fn run<F>(&mut self, mut ingest_fn: F)
    where
        F: FnMut(PathBuf, Vec<u8>),
    {
        let mut last_poll: Option<Timestamp> = None;

        loop {
            let (n, updated) = self.poll_once(last_poll, &mut ingest_fn);
            if n > 0 {
                tracing::info!(
                    source = self.source.source_id(),
                    n,
                    "ingested remote items"
                );
            }
            last_poll = updated;
            std::thread::sleep(self.poll_interval);
        }
    }
}
