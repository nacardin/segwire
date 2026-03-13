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

/// End-to-end test for the `segwire exec` flow.
///
/// Exercises the full lifecycle:
/// 1. Daemon creates a namespace from config
/// 2. Resolves the namespace name → `/run/netns/` path (same logic as ExecAuthorize)
/// 3. Invokes `segwire-ns-enter` with the path (same as CLI would)
/// 4. Verifies the command ran inside the correct namespace
///
/// **Root-only**: requires real namespaces and CAP_SYS_ADMIN for setns.
///
/// Note: This test creates ConfigManager and NamespaceStateManager directly
/// instead of using start_daemon(), avoiding D-Bus connection caching issues
/// that affect serial tests in the `dbus` crate.
#[test]
#[serial]
fn test_ns_enter_exec() {
    use segwire_common::DaemonConfig;
    use segwire_daemon::config::ConfigManager;
    use segwire_daemon::namespace_state::NamespaceStateManager;

    if !is_root() {
        eprintln!("Skipping test_ns_enter_exec: requires root");
        return;
    }

    // ── Step 1: Create namespace via config + state managers directly ──

    let harness = TestHarness::new().expect("Failed to create test harness");

    harness
        .write_namespace_config("execns", &namespace_config_with_dummy("execns"))
        .expect("Failed to write namespace config");

    // Create managers directly (no D-Bus needed)
    let config_content =
        std::fs::read_to_string(&harness.config_path).expect("Failed to read config");
    let daemon_config: DaemonConfig =
        toml::from_str(&config_content).expect("Failed to parse config");
    let mut config_mgr = ConfigManager::from_config(daemon_config, harness.config_path.clone());
    let mut state_mgr = NamespaceStateManager::new_auto().expect("Failed to create state manager");

    let scan_result = config_mgr
        .scan_namespace_configs()
        .expect("Config scan failed");

    assert!(
        !scan_result.is_empty(),
        "Config scan found no namespace configs"
    );

    let sync_result = state_mgr
        .force_sync(&config_mgr)
        .expect("State sync failed");

    // The namespace is either freshly 'created' or 'updated' (already existed
    // from a previous test run). Both are fine — either way it's live.
    assert!(
        !sync_result.created.is_empty() || !sync_result.updated.is_empty(),
        "Expected namespace to be created or updated, got: created={:?}, updated={:?}, errors={:?}",
        sync_result.created,
        sync_result.updated,
        sync_result.errors
    );

    // Resolve namespace name → path (same logic as ExecAuthorize handler)
    let full_name = config_mgr.generate_full_namespace_name("execns");
    let ns = state_mgr
        .get_namespace_state(&full_name)
        .expect("Namespace state not found");
    assert!(ns.is_active(), "Namespace should be active");

    let ns_path = format!("/run/netns/{}", full_name);
    assert!(
        std::path::Path::new(&ns_path).exists(),
        "Namespace path '{}' does not exist on disk",
        ns_path
    );

    // ── Step 2: Invoke segwire-ns-enter (same as CLI after ExecAuthorize) ──

    let ns_enter_bin = std::env::current_exe()
        .expect("Failed to get current exe path")
        .parent()
        .expect("No parent dir")
        .parent()
        .expect("No grandparent dir")
        .join("segwire-ns-enter");

    assert!(
        ns_enter_bin.exists(),
        "segwire-ns-enter not found at {}. Run `cargo build --workspace` first.",
        ns_enter_bin.display()
    );

    // Test: echo inside the namespace
    let output = Command::new(&ns_enter_bin)
        .args([&ns_path, "--", "echo", "hello-from-namespace"])
        .output()
        .expect("Failed to run segwire-ns-enter");

    assert!(
        output.status.success(),
        "segwire-ns-enter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "hello-from-namespace"
    );

    // Test: ip link show to verify network isolation
    let ip_output = Command::new(&ns_enter_bin)
        .args([&ns_path, "--", "ip", "link", "show"])
        .output()
        .expect("Failed to run ip link show via segwire-ns-enter");

    assert!(ip_output.status.success());
    let ip_stdout = String::from_utf8_lossy(&ip_output.stdout);
    assert!(
        ip_stdout.contains("lo"),
        "Expected loopback in namespace, got: {}",
        ip_stdout
    );

    // Test: path validation rejects bad paths
    let bad_output = Command::new(&ns_enter_bin)
        .args(["/tmp/not-a-namespace", "--", "echo", "nope"])
        .output()
        .expect("Failed to run segwire-ns-enter with bad path");

    assert!(
        !bad_output.status.success(),
        "segwire-ns-enter should reject paths not under /run/netns/"
    );

    // ── Cleanup ──

    if std::env::var("SEGWIRE_TEST_SKIP_CLEANUP").is_err() {
        let del = Command::new("ip")
            .args(["netns", "delete", &full_name])
            .output()
            .expect("Failed to delete namespace");
        if !del.status.success() {
            eprintln!(
                "Warning: failed to delete namespace '{}': {}",
                full_name,
                String::from_utf8_lossy(&del.stderr)
            );
        }
    }
}
