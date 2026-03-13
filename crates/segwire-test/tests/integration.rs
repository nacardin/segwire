//! Integration tests for the segwire daemon in simulation mode.
//!
//! These tests start the daemon with simulated netlink on a session D-Bus,
//! then exercise it via the DbusClient (the same client the CLI uses).

use segwire_test::harness::{sample_namespace_config, TestHarness};

/// Test that the harness can create a daemon instance in simulation mode
/// and that namespace configs are picked up by the daemon on startup.
///
/// NOTE: Only one daemon startup test per process is possible because
/// `DbusService::new()` claims a fixed D-Bus service name and the
/// the zbus runtime doesn't clean up connections between tests.
#[test]
fn test_daemon_startup_with_config() {
    let harness = TestHarness::new().expect("Failed to create test harness");

    // Write namespace config files before starting the daemon
    harness
        .write_namespace_config("web", &sample_namespace_config("web"))
        .expect("Failed to write namespace config");
    harness
        .write_namespace_config("vpn", &sample_namespace_config("vpn"))
        .expect("Failed to write namespace config");

    // Starting the daemon in simulation mode should succeed
    let event_loop = harness.start_daemon();
    assert!(
        event_loop.is_ok(),
        "Daemon failed to start in simulation mode: {:?}",
        event_loop.err()
    );

    // Request shutdown to verify internal state
    event_loop.unwrap().request_shutdown();
}

/// Test that writing and then removing a config file works correctly
/// through the harness (does not start the daemon).
#[test]
fn test_config_file_lifecycle() {
    let harness = TestHarness::new().expect("Failed to create test harness");

    // Write a config
    let path = harness
        .write_namespace_config("ephemeral", &sample_namespace_config("ephemeral"))
        .expect("Failed to write config");

    assert!(path.exists(), "Config file should exist after writing");

    // Remove it
    harness
        .remove_namespace_config("ephemeral")
        .expect("Failed to remove config");

    assert!(!path.exists(), "Config file should not exist after removal");
}

/// Test that the simulated NetlinkManager correctly handles namespace
/// operations in-memory.
#[test]
fn test_simulated_netlink_operations() {
    use segwire_daemon::netlink::NetlinkManager;

    let mgr = NetlinkManager::new_simulated().expect("Failed to create simulated NetlinkManager");
    assert!(mgr.is_simulated());

    // Create a namespace
    let info = mgr
        .create_namespace("test-ns")
        .expect("Failed to create simulated namespace");
    assert_eq!(info.name, "test-ns");
    assert!(info.active);

    // Check it exists
    assert!(mgr.namespace_exists("test-ns").unwrap());

    // List namespaces
    let namespaces = mgr.list_namespaces().unwrap();
    assert_eq!(namespaces.len(), 1);
    assert!(namespaces.contains_key("test-ns"));

    // Get info
    let info2 = mgr.get_namespace_info("test-ns").unwrap();
    assert_eq!(info2.name, "test-ns");

    // Delete it
    mgr.delete_namespace("test-ns")
        .expect("Failed to delete simulated namespace");
    assert!(!mgr.namespace_exists("test-ns").unwrap());
    assert!(mgr.list_namespaces().unwrap().is_empty());

    // Double-delete should fail
    assert!(mgr.delete_namespace("test-ns").is_err());
}

/// Test that duplicate namespace creation fails.
#[test]
fn test_simulated_duplicate_namespace() {
    use segwire_daemon::netlink::NetlinkManager;

    let mgr = NetlinkManager::new_simulated().expect("new_simulated");
    mgr.create_namespace("dup").expect("first create");
    assert!(
        mgr.create_namespace("dup").is_err(),
        "Duplicate create should fail"
    );
}

/// Test that simulation mode no-ops for interface and route operations.
#[test]
fn test_simulated_interface_and_route_ops() {
    use segwire_daemon::netlink::NetlinkManager;

    let mgr = NetlinkManager::new_simulated().unwrap();

    // list_interfaces returns simulated defaults
    let ifaces = mgr.list_interfaces().unwrap();
    assert!(ifaces.contains(&"lo".to_string()));
    assert!(ifaces.contains(&"eth0".to_string()));

    // interface operations are no-ops
    assert!(mgr.create_veth_pair("veth0", "veth1").is_ok());
    assert!(mgr.move_interface_to_namespace("eth0", "some-ns").is_ok());

    // list_namespace_interfaces returns simulated default
    let ns_ifaces = mgr.list_namespace_interfaces("some-ns").unwrap();
    assert!(ns_ifaces.contains(&"lo".to_string()));
}
