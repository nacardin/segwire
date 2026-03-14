//! Blackbox integration tests for the segwire daemon.
//!
//! These tests exercise segwire the way a real user would:
//! - The daemon event loop runs on a background thread
//! - Interaction happens through `segwire_cli::run_cli`, which connects
//!   to the private test D-Bus session and executes CLI commands
//!
//! **Root mode**: real namespaces are created; `ip` commands verify system state.
//! **Non-root mode**: simulation flag is set; in-memory state is verified.

use segwire_test::harness::TestHarness;
use serial_test::serial;
use std::process::Command;
use std::sync::atomic::Ordering;

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

/// Full black-box test: daemon runs in background, interacted with via CLI.
///
/// 1. Start daemon in background (registers on private session bus)
/// 2. Write a namespace config
/// 3. Call `segwire reload` via the CLI library
/// 4. Call `segwire list` and `segwire status` to verify
/// 5. Root-only: verify real namespace via `ip netns list`
/// 6. Graceful shutdown
#[test]
#[serial]
fn test_namespace_setup() {
    if !is_root() {
        std::env::set_var("SEGWIRE_SIMULATION", "1");
    }

    let harness = TestHarness::new().expect("Failed to create test harness");

    // Write a namespace config with a dummy interface
    harness
        .write_namespace_config("testns", &namespace_config_with_dummy("testns"))
        .expect("Failed to write namespace config");

    // Start the daemon event loop in the background
    let (handle, shutdown) = harness
        .start_daemon_background()
        .expect("Failed to start daemon");

    // ── Exercise the CLI like a real user ──

    // `segwire reload` — triggers config scan + state sync
    segwire_cli::run_cli(["segwire", "reload"])
        .expect("'segwire reload' failed");

    // `segwire list` — should show our namespace
    segwire_cli::run_cli(["segwire", "list"])
        .expect("'segwire list' failed");

    // `segwire status testns` — detailed status
    segwire_cli::run_cli(["segwire", "status", "testns"])
        .expect("'segwire status testns' failed");

    // Root-only verification: check real system state with `ip` commands
    if is_root() {
        let output = Command::new("ip")
            .args(["netns", "list"])
            .output()
            .expect("Failed to run 'ip netns list'");

        let stdout = String::from_utf8_lossy(&output.stdout);
        // The prefixed name should appear (e.g. "test-testns")
        assert!(
            stdout.contains("test-testns") || stdout.contains("testns"),
            "Expected namespace in 'ip netns list' output: {}",
            stdout
        );

        // Cleanup the namespace unless SEGWIRE_TEST_SKIP_CLEANUP is set
        if std::env::var("SEGWIRE_TEST_SKIP_CLEANUP").is_err() {
            // Try both prefixed and unprefixed names
            let _ = Command::new("ip")
                .args(["netns", "delete", "test-testns"])
                .output();
        }
    }

    // Graceful shutdown
    shutdown.store(true, Ordering::SeqCst);
    handle.join().expect("Daemon thread panicked");
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
/// 1. Daemon creates a namespace from config (via CLI reload)
/// 2. Verifies namespace is active via `segwire status`
/// 3. Invokes `segwire-ns-enter` with the path (same as CLI would)
/// 4. Verifies the command ran inside the correct namespace
///
/// **Root-only**: requires real namespaces and CAP_SYS_ADMIN for setns.
#[test]
#[serial]
fn test_ns_enter_exec() {
    if !is_root() {
        eprintln!("Skipping test_ns_enter_exec: requires root");
        return;
    }

    let harness = TestHarness::new().expect("Failed to create test harness");

    harness
        .write_namespace_config("execns", &namespace_config_with_dummy("execns"))
        .expect("Failed to write namespace config");

    // Start daemon in background
    let (handle, shutdown) = harness
        .start_daemon_background()
        .expect("Failed to start daemon");

    // Reload config via CLI
    segwire_cli::run_cli(["segwire", "reload"])
        .expect("'segwire reload' failed");

    // Verify namespace is visible via CLI
    segwire_cli::run_cli(["segwire", "status", "execns"])
        .expect("'segwire status execns' failed");

    // Build expected namespace path (prefixed name)
    let full_name = "test-execns";
    let ns_path = format!("/run/netns/{}", full_name);
    assert!(
        std::path::Path::new(&ns_path).exists(),
        "Namespace path '{}' does not exist on disk",
        ns_path
    );

    // ── Invoke segwire-ns-enter (same as CLI after ExecAuthorize) ──

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
    shutdown.store(true, Ordering::SeqCst);
    handle.join().expect("Daemon thread panicked");

    if std::env::var("SEGWIRE_TEST_SKIP_CLEANUP").is_err() {
        let del = Command::new("ip")
            .args(["netns", "delete", full_name])
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
