//! SourceFeed trait, filesystem watcher, and remote polling source infrastructure.

mod fs_watcher;
pub mod remote;

pub use fs_watcher::{FsWatcher, FsWatcherConfig, SourceFeed, StopHandle, WatchError};
pub use remote::{RemoteError, RemoteIngestionLoop, RemoteItem, RemoteSource};
