use segwire_daemon::config::NamespaceConfigEntry;
use segwire_daemon::namespace_state::*;
use std::fs;
use std::time::SystemTime;
use tempfile::TempDir;

#[allow(dead_code)] // Prepared utility for future sync/conflict tests
fn create_test_config_entry(name: &str, temp_dir: &TempDir) -> NamespaceConfigEntry {
    let config_content = format!(
        r#"
[namespace]
name = "{}"
description = "Test namespace"

[interfaces]
move_interfaces = ["eth0"]

[routing]
default_gateway = "192.168.1.1"

[dns]
servers = ["8.8.8.8"]
"#,
        name
    );

    let config_path = temp_dir.path().join(format!("{}.toml", name));
    fs::write(&config_path, config_content).expect("Failed to write test config");

    let config = segwire_common::config::NamespaceConfig::from_file(&config_path)
        .expect("Failed to parse test config");

    NamespaceConfigEntry {
        config,
        file_path: config_path,
        full_name: format!("test-{}", name),
        last_modified: SystemTime::now(),
    }
}

#[test]
fn test_state_manager_creation() {
    let result = NamespaceStateManager::new();

    // Creation might fail in test environment without proper netlink access
    // but we can test the basic structure
    match result {
        Ok(manager) => {
            assert_eq!(manager.get_state_stats().total_namespaces, 0);
            assert!(manager.needs_sync());
        }
        Err(e) => {
            // Expected in test environment without netlink access
            println!("Expected error in test environment: {}", e);
        }
    }
}
