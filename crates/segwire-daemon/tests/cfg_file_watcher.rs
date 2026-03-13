//! Integration tests for `ConfigFileWatcher`.
//!
//! These tests exercise the real `ConfigFileWatcher` pipeline, verifying that
//! creating, modifying, and deleting `.toml` files on disk produces the
//! expected `ConfigFileEvent` variants on the channel.

use segwire_daemon::config::{ConfigFileEvent, ConfigFileWatcher};
use std::fs;
use std::sync::mpsc;
use std::time::Duration;
use tempfile::TempDir;

/// Helper: drain all available events from the channel, retrying with short
/// sleeps up to `timeout`. Returns the collected events.
fn collect_events(rx: &mpsc::Receiver<ConfigFileEvent>, timeout: Duration) -> Vec<ConfigFileEvent> {
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + timeout;

    while std::time::Instant::now() < deadline {
        match rx.try_recv() {
            Ok(event) => events.push(event),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    // Final drain in case events arrived right at the deadline.
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    events
}

#[test]
fn test_cfg_file_watcher_detects_creation() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let watch_dir = temp_dir.path().to_path_buf();

    let mut monitor = ConfigFileWatcher::new(watch_dir.clone(), Duration::from_millis(100));
    let rx = monitor
        .start_monitoring()
        .expect("Failed to start monitoring");

    // Give the watcher a moment to initialise.
    std::thread::sleep(Duration::from_millis(200));

    // Create a .toml file — should trigger a Created event.
    let file_path = watch_dir.join("test.toml");
    fs::write(&file_path, "[namespace]\nname = \"test\"\n").expect("Failed to write file");

    let events = collect_events(&rx, Duration::from_secs(2));

    assert!(
        !events.is_empty(),
        "Expected at least one event after file creation"
    );
    assert!(
        events.iter().any(
            |e| matches!(e, ConfigFileEvent::Created(p) if p == &file_path)
                || matches!(e, ConfigFileEvent::Modified(p) if p == &file_path)
        ),
        "Expected a Created or Modified event for {:?}, got: {:?}",
        file_path,
        events
    );
}

#[test]
fn test_cfg_file_watcher_detects_modification() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let watch_dir = temp_dir.path().to_path_buf();

    // Pre-create the file *before* the watcher starts so we only see
    // modification events.
    let file_path = watch_dir.join("modify.toml");
    fs::write(&file_path, "[namespace]\nname = \"v1\"\n").expect("Failed to write initial file");

    let mut monitor = ConfigFileWatcher::new(watch_dir.clone(), Duration::from_millis(100));
    let rx = monitor
        .start_monitoring()
        .expect("Failed to start monitoring");

    std::thread::sleep(Duration::from_millis(200));

    // Modify the file.
    fs::write(&file_path, "[namespace]\nname = \"v2\"\n").expect("Failed to modify file");

    let events = collect_events(&rx, Duration::from_secs(2));

    assert!(
        !events.is_empty(),
        "Expected at least one event after file modification"
    );
    // A write to an existing file can surface as MODIFY or CLOSE_WRITE.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ConfigFileEvent::Modified(p) if p == &file_path)),
        "Expected a Modified event for {:?}, got: {:?}",
        file_path,
        events
    );
}

#[test]
fn test_cfg_file_watcher_detects_deletion() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let watch_dir = temp_dir.path().to_path_buf();

    // Pre-create the file.
    let file_path = watch_dir.join("delete.toml");
    fs::write(&file_path, "[namespace]\nname = \"gone\"\n").expect("Failed to write file");

    let mut monitor = ConfigFileWatcher::new(watch_dir.clone(), Duration::from_millis(100));
    let rx = monitor
        .start_monitoring()
        .expect("Failed to start monitoring");

    std::thread::sleep(Duration::from_millis(200));

    // Delete the file.
    fs::remove_file(&file_path).expect("Failed to delete file");

    let events = collect_events(&rx, Duration::from_secs(2));

    assert!(
        !events.is_empty(),
        "Expected at least one event after file deletion"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ConfigFileEvent::Deleted(p) if p == &file_path)),
        "Expected a Deleted event for {:?}, got: {:?}",
        file_path,
        events
    );
}

#[test]
fn test_cfg_file_watcher_ignores_non_toml() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let watch_dir = temp_dir.path().to_path_buf();

    let mut monitor = ConfigFileWatcher::new(watch_dir.clone(), Duration::from_millis(100));
    let rx = monitor
        .start_monitoring()
        .expect("Failed to start monitoring");

    std::thread::sleep(Duration::from_millis(200));

    // Create a non-.toml file — should be silently ignored.
    fs::write(watch_dir.join("readme.txt"), "hello").expect("Failed to write file");
    fs::write(watch_dir.join("config.json"), "{}").expect("Failed to write file");

    let events = collect_events(&rx, Duration::from_secs(1));

    assert!(
        events.is_empty(),
        "Expected no events for non-.toml files, got: {:?}",
        events
    );
}

#[test]
fn test_cfg_file_watcher_debounces_rapid_writes() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let watch_dir = temp_dir.path().to_path_buf();

    // Use a 500ms debounce so rapid writes within that window are collapsed.
    let mut monitor = ConfigFileWatcher::new(watch_dir.clone(), Duration::from_millis(500));
    let rx = monitor
        .start_monitoring()
        .expect("Failed to start monitoring");

    std::thread::sleep(Duration::from_millis(200));

    let file_path = watch_dir.join("rapid.toml");

    // Create the file, then rapidly modify it several times.
    fs::write(&file_path, "v1").expect("write");
    std::thread::sleep(Duration::from_millis(10));
    fs::write(&file_path, "v2").expect("write");
    std::thread::sleep(Duration::from_millis(10));
    fs::write(&file_path, "v3").expect("write");

    let events = collect_events(&rx, Duration::from_secs(2));

    // With a 500ms debounce the first write should produce an event,
    // but the rapid follow-ups within the window should be suppressed.
    // Depending on kernel coalescing we may see 1-2 events, but
    // certainly not one per write.
    assert!(
        events.len() <= 2,
        "Expected at most 2 events due to debouncing, got {}: {:?}",
        events.len(),
        events
    );
}
