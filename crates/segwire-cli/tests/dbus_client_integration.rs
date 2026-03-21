use anyhow::Result;
use segwire_cli::dbus_client::DbusClient;
use segwire_common::dbus::{
    method_signatures::{DaemonStatusResult, ListNamespacesResult},
    NamespaceState, NamespaceStatus, OperationResult,
};
use zbus::Connection;

struct MockDaemon;

#[zbus::dbus_interface(name = "org.segwire.NamespaceManager")]
impl MockDaemon {
    async fn get_daemon_status(&self) -> DaemonStatusResult {
        ("1.0.0".to_string(), 100, 1, 1)
    }

    async fn list_namespaces(&self) -> ListNamespacesResult {
        vec![(
            "test-ns".to_string(),
            "active".to_string(),
            "/path/to/test-ns.toml".to_string(),
            "Test NS".to_string(),
        )]
    }

    async fn get_namespace_status(
        &self,
        name: &str,
    ) -> std::result::Result<NamespaceState, zbus::fdo::Error> {
        if name == "test-ns" {
            Ok(NamespaceState {
                name: "test-ns".to_string(),
                full_name: "segwire-test-ns".to_string(),
                status: NamespaceStatus::Active,
                config_path: "/path/to/test-ns.toml".to_string(),
                interfaces: vec![],
                routes: vec![],
                dns_config: segwire_common::dbus::DnsInfo {
                    servers: vec![],
                    search_domains: vec![],
                },
                created_at: 0,
                last_updated: 0,
            })
        } else {
            Err(zbus::fdo::Error::InvalidArgs("Not found".to_string()))
        }
    }

    async fn reload_configuration(&self) -> OperationResult {
        OperationResult {
            success: true,
            message: "Reloaded".to_string(),
            details: std::collections::HashMap::new(),
        }
    }

    async fn restart_namespace(&self, name: &str) -> OperationResult {
        OperationResult {
            success: true,
            message: format!("Restarted {}", name),
            details: std::collections::HashMap::new(),
        }
    }
}

#[monoio::test]
async fn test_dbus_client_mock() -> Result<()> {
    std::env::set_var("SEGWIRE_TEST_SESSION_BUS", "1");
    // Set up mock server
    let connection = Connection::session().await?;
    connection
        .object_server()
        .at("/org/segwire/NamespaceManager", MockDaemon)
        .await?;
    connection
        .request_name("org.segwire.NamespaceManager")
        .await?;

    // Test client methods
    let client = DbusClient::new().await?;

    let status = client.get_daemon_status().await?;
    assert_eq!(status.0, "1.0.0"); // version
    assert_eq!(status.1, 100); // uptime_seconds

    let list = client.list_namespaces().await?;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, "test-ns");

    let ns = client.get_namespace_status("test-ns").await?;
    assert_eq!(ns.name, "test-ns");

    let ns_err = client.get_namespace_status("nonexistent").await;
    assert!(ns_err.is_err());

    let reload = client.reload_configuration().await?;
    assert!(reload.success);

    let restart = client.restart_namespace("test-ns").await?;
    assert!(restart.success);

    Ok(())
}
