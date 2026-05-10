//! SourceFeed trait and filesystem watcher implementation.

mod fs_watcher;

pub use fs_watcher::{FsWatcher, FsWatcherConfig, SourceFeed, StopHandle, WatchError};
