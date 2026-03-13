//! Blackbox integration tests for the segwire daemon.
//!
//! These tests exercise segwire through its public daemon API:
//! - Write namespace configs (with virtual interface definitions)
//! - Start the daemon, which syncs config → creates namespaces
//! - Verify the resulting state
//!
//! **Root mode**: real namespaces are created; `ip` commands verify system state.
//! **Non-root mode**: simulation flag is set; in-memory state is verified.

use segwire_test::harness::TestHarness;
use serial_test::serial;
use std::process::Command;

/// Helper: check if running as root.
fn is_root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

/// A namespace config that includes a dummy virtual interface.
fn namespace_config_with_dummy(name: &str) -> String {
    format!(
        r#"[namespace]
name = "{name}"
description = "Namespace with dummy interface"

[interfaces]
move_interfaces = []

[[interfaces.virtual_interfaces]]
name = "dummy0"
interface_type = "dummy"

[routing]

[dns]
servers = ["8.8.8.8"]
"#
    )
}

/// Test that the daemon creates a namespace from config and that the
/// resulting state is observable.
///
/// - Non-root: verifies simulation state (namespace created, status active,
///   interface info tracked).
/// - Root: additionally verifies the real namespace exists via `ip netns list`.
#[test]
#[serial]
fn test_namespace_setup() {
    // Set simulation mode when not root so no real changes are made
    if !is_root() {
        std::env::set_var("SEGWIRE_SIMULATION", "1");
    }

    let harness = TestHarness::new().expect("Failed to create test harness");

    // Write a namespace config with a dummy interface
    harness
        .write_namespace_config("testns", &namespace_config_with_dummy("testns"))
        .expect("Failed to write namespace config");

    // Start the daemon — this triggers config scan + state sync
    let event_loop = harness.start_daemon().expect("Daemon failed to start");

    // Trigger config scan so the namespace config is loaded
    {
        let mut config_mgr = event_loop.config_manager().lock().unwrap();
        config_mgr
            .scan_namespace_configs()
            .expect("Config scan failed");

        assert!(
            !config_mgr.namespace_configs().is_empty(),
            "Expected at least one namespace config to be loaded"
        );
    }

    // Trigger state sync so the namespace is created
    {
        let config_mgr = event_loop.config_manager().lock().unwrap();
        let mut state_mgr = event_loop.state_manager().lock().unwrap();
        let sync_result = state_mgr
            .force_sync(&config_mgr)
            .expect("State sync failed");

        assert!(
            !sync_result.created.is_empty() || !sync_result.updated.is_empty(),
            "Expected namespace to be created or updated during sync, got: created={:?}, updated={:?}, errors={:?}",
            sync_result.created, sync_result.updated, sync_result.errors
        );
    }

    // Verify observable state: namespace should be active with the WireGuard interface
    {
        let state_mgr = event_loop.state_manager().lock().unwrap();
        let all_states = state_mgr.get_all_states();

        assert!(
            !all_states.is_empty(),
            "Expected at least one namespace in state"
        );

        // Find our namespace (it will have the prefix applied)
        let ns_state = all_states
            .values()
            .find(|ns| ns.name == "testns")
            .expect("Namespace 'testns' not found in state");

        assert!(
            ns_state.is_active(),
            "Expected namespace to be active, got status: {:?}",
            ns_state.status
        );

        // The dummy interface should be tracked in the namespace state
        assert!(
            ns_state
                .interfaces
                .iter()
                .any(|iface| iface.name == "dummy0"),
            "Expected 'dummy0' interface in namespace state, got: {:?}",
            ns_state.interfaces
        );

        // DNS config should be present
        assert!(
            ns_state.dns_config.servers.contains(&"8.8.8.8".to_string()),
            "Expected DNS server 8.8.8.8 in namespace state"
        );
    }

    // Root-only verification: check real system state with `ip` commands
    if is_root() {
        // Verify the namespace exists on the system
        let output = Command::new("ip")
            .args(["netns", "list"])
            .output()
            .expect("Failed to run 'ip netns list'");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let full_name = {
            let state_mgr = event_loop.state_manager().lock().unwrap();
            state_mgr
                .get_all_states()
                .values()
                .find(|ns| ns.name == "testns")
                .map(|ns| ns.full_name.clone())
                .expect("Namespace not found")
        };

        assert!(
            stdout.contains(&full_name),
            "Expected namespace '{}' in 'ip netns list' output: {}",
            full_name,
            stdout
        );

        // Cleanup: delete the namespace unless SEGWIRE_TEST_SKIP_CLEANUP is set
        if std::env::var("SEGWIRE_TEST_SKIP_CLEANUP").is_err() {
            let del = Command::new("ip")
                .args(["netns", "delete", &full_name])
                .output()
                .expect("Failed to run 'ip netns delete'");
            assert!(
                del.status.success(),
                "Failed to delete namespace '{}': {}",
                full_name,
                String::from_utf8_lossy(&del.stderr)
            );
        }
    }

    // Cleanup
    event_loop.request_shutdown();
}

/// Test that writing and then removing a config file works correctly
/// through the harness (does not start the daemon).
#[test]
#[serial]
fn test_config_file_lifecycle() {
    let harness = TestHarness::new().expect("Failed to create test harness");

    let path = harness
        .write_namespace_config("ephemeral", &namespace_config_with_dummy("ephemeral"))
        .expect("Failed to write config");

    assert!(path.exists(), "Config file should exist after writing");

    harness
        .remove_namespace_config("ephemeral")
        .expect("Failed to remove config");

    assert!(!path.exists(), "Config file should not exist after removal");
}
