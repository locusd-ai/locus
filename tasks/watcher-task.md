# Task: Implement biem-watcher (Filesystem Watcher) ✅

## Goal
Implement the `SourceFeed` trait with an `FsWatcher` that monitors an Obsidian vault directory for file changes (create, modify, delete, rename) using the `notify` crate, with debouncing and glob-based ignore patterns.

## Steps

### Step 1: Core types in biem-core
- [x] Add `ChangeEvent`, `ChangeKind` to biem-core/types.rs
- [x] Add `SourceFeed` trait to biem-watcher
- **Validate**: `cargo check -p biem-core`

### Step 2: FsWatcher struct and configuration
- [x] Define `FsWatcherConfig` (root, debounce_ms, ignore patterns, extensions)
- [x] Define `FsWatcher` holding config + notify watcher
- [x] Implement `FsWatcher::new(config)` — create debounced watcher
- [x] Implement `StopHandle` for graceful shutdown
- **Validate**: `cargo check -p biem-watcher`

### Step 3: Event translation
- [x] Map notify events to `ChangeEvent` / `ChangeKind`
- [x] Debounce via `notify-debouncer-mini`
- [x] Filter by file extension (.md)
- [x] Filter by glob ignore patterns (globset)
- **Validate**: `cargo check -p biem-watcher`

### Step 4: Start/stop lifecycle
- [x] `fn start(&mut self, tx: Sender<ChangeEvent>)` — blocking watcher loop
- [x] `fn stop_handle(&self) -> StopHandle` — signals shutdown
- **Validate**: `cargo check -p biem-watcher`

### Step 5: Unit tests
- [x] Default config values
- [x] Watcher creation
- [x] Stop handle
- [x] Detects file creation
- [x] Detects file deletion
- [x] Ignores glob patterns
- **Validate**: `cargo test -p biem-watcher`

### Step 6: Commit
- [x] `feat(watcher): implement FsWatcher with notify, debouncing, and glob ignore`
