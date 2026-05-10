#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use biem_core::types::ChangeKind;
    use biem_watcher::{FsWatcher, FsWatcherConfig, SourceFeed};

    #[test]
    fn test_default_config() {
        let config = FsWatcherConfig::default();
        assert_eq!(config.debounce_ms, 500);
        assert!(!config.ignore_patterns.is_empty());
    }

    #[test]
    fn test_watcher_creation() {
        let dir = tempfile::tempdir().unwrap();
        let config = FsWatcherConfig {
            root: dir.path().to_path_buf(),
            debounce_ms: 100,
            ignore_patterns: vec![],
        };
        assert!(FsWatcher::new(config).is_ok());
    }

    #[test]
    fn test_stop_handle() {
        let dir = tempfile::tempdir().unwrap();
        let config = FsWatcherConfig {
            root: dir.path().to_path_buf(),
            debounce_ms: 100,
            ignore_patterns: vec![],
        };
        let mut watcher = FsWatcher::new(config).unwrap();
        let stop = watcher.stop_handle();
        let (tx, _rx) = mpsc::channel();

        // Start in background, stop after a short delay
        let handle = thread::spawn(move || watcher.start(tx));
        thread::sleep(Duration::from_millis(200));
        stop.stop();

        let result = handle.join().unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn test_detects_file_creation() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();
        let config = FsWatcherConfig {
            root: dir_path.clone(),
            debounce_ms: 100,
            ignore_patterns: vec![],
        };
        let mut watcher = FsWatcher::new(config).unwrap();
        let stop = watcher.stop_handle();
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || watcher.start(tx));
        thread::sleep(Duration::from_millis(200));

        fs::write(dir_path.join("test.md"), "# Hello").unwrap();

        let event = rx.recv_timeout(Duration::from_secs(3));
        assert!(event.is_ok(), "should receive a change event");
        let event = event.unwrap();
        assert!(event.path.ends_with("test.md"));
        assert_eq!(event.kind, ChangeKind::Modified);

        stop.stop();
        let _ = handle.join();
    }

    #[test]
    fn test_detects_file_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("to_delete.md");
        fs::write(&file_path, "# Delete me").unwrap();

        let config = FsWatcherConfig {
            root: dir.path().to_path_buf(),
            debounce_ms: 100,
            ignore_patterns: vec![],
        };
        let mut watcher = FsWatcher::new(config).unwrap();
        let stop = watcher.stop_handle();
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || watcher.start(tx));
        thread::sleep(Duration::from_millis(200));

        fs::remove_file(&file_path).unwrap();

        let event = rx.recv_timeout(Duration::from_secs(3));
        assert!(event.is_ok(), "should receive a deletion event");
        assert_eq!(event.unwrap().kind, ChangeKind::Deleted);

        stop.stop();
        let _ = handle.join();
    }

    #[test]
    fn test_ignores_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();
        fs::create_dir_all(dir_path.join(".obsidian")).unwrap();

        let config = FsWatcherConfig {
            root: dir_path.clone(),
            debounce_ms: 100,
            ignore_patterns: vec![".obsidian/**".into()],
        };
        let mut watcher = FsWatcher::new(config).unwrap();
        let stop = watcher.stop_handle();
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || watcher.start(tx));
        thread::sleep(Duration::from_millis(200));

        // Write to ignored path first
        fs::write(dir_path.join(".obsidian/workspace.json"), "{}").unwrap();
        // Then write to non-ignored path
        thread::sleep(Duration::from_millis(300));
        fs::write(dir_path.join("note.md"), "# Note").unwrap();

        // Collect events
        let mut events = vec![];
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(3)) {
            events.push(e);
            // Drain any additional
            thread::sleep(Duration::from_millis(500));
            while let Ok(e) = rx.recv_timeout(Duration::from_millis(100)) {
                events.push(e);
            }
            break;
        }

        assert!(!events.is_empty());
        for event in &events {
            assert!(
                !event.path.to_string_lossy().contains(".obsidian"),
                "should not get events for ignored paths: {:?}",
                event.path
            );
        }

        stop.stop();
        let _ = handle.join();
    }
}
