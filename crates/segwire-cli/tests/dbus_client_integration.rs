use anyhow::Result;
use dbus::blocking::Connection;
use dbus::channel::MatchingReceiver;
use dbus_crossroads::Crossroads;
use segwire_cli::dbus_client::DbusClient;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Start a mock D-Bus service that mimics the daemon for testing.
///
/// Returns a `JoinHandle` so the caller can shut it down after the test.
fn start_mock_daemon(ready: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let connection =
            Connection::new_session().expect("Failed to connect to session bus for mock daemon");

        connection
            .request_name("org.segwire.NamespaceManager", false, true, false)
            .expect("Failed to request service name for mock daemon");

        let mut cr = Crossroads::new();

        let iface_token = cr.register("org.segwire.NamespaceManager", |b| {
            b.method(
                "GetDaemonStatus",
                (),
                ("version", "uptime", "managed_count", "active_count"),
                |_ctx, _data: &mut (), ()| -> Result<(String, u64, u32, u32), dbus_crossroads::MethodErr> {
                    Ok(("1.0.0".to_string(), 100u64, 1u32, 1u32))
                },
            );

            b.method(
                "ListNamespaces",
                (),
                ("namespaces",),
                |_ctx, _data: &mut (), ()| {
                    Ok((vec![(
                        "test-ns".to_string(),
                        "active".to_string(),
                        "/path/to/test-ns.toml".to_string(),
                        "Test NS".to_string(),
                    )],))
                },
            );

            b.method(
                "GetNamespaceStatus",
                ("name",),
                ("name", "full_name", "status", "config_path", "created_at", "last_updated"),
                |_ctx, _data: &mut (), (name,): (String,)| -> Result<(String, String, String, String, u64, u64), dbus_crossroads::MethodErr> {
                    if name == "test-ns" {
                        Ok((
                            "test-ns".to_string(),
                            "segwire-test-ns".to_string(),
                            "active".to_string(),
                            "/path/to/test-ns.toml".to_string(),
                            0u64,
                            0u64,
                        ))
                    } else {
                        Err(dbus_crossroads::MethodErr::failed(&format!(
                            "Namespace '{}' not found",
                            name
                        )))
                    }
                },
            );

            b.method(
                "ReloadConfiguration",
                (),
                ("success", "message", "details"),
                |_ctx, _data: &mut (), ()| -> Result<(bool, String, HashMap<String, String>), dbus_crossroads::MethodErr> {
                    Ok((true, "Reloaded".to_string(), HashMap::new()))
                },
            );

            b.method(
                "RestartNamespace",
                ("name",),
                ("success", "message", "details"),
                |_ctx, _data: &mut (), (name,): (String,)| -> Result<(bool, String, HashMap<String, String>), dbus_crossroads::MethodErr> {
                    Ok((true, format!("Restarted {}", name), HashMap::new()))
                },
            );
        });

        cr.insert("/org/segwire/NamespaceManager", &[iface_token], ());

        connection.start_receive(
            dbus::message::MatchRule::new_method_call(),
            Box::new(move |msg, conn| {
                cr.handle_message(msg, conn).unwrap();
                true
            }),
        );

        // Signal that we're ready
        ready.store(true, Ordering::SeqCst);

        // Process messages until the connection is closed
        loop {
            match connection.process(std::time::Duration::from_millis(100)) {
                Ok(true) => {}   // processed a message
                Ok(false) => {}  // timeout, continue
                Err(_) => break, // connection error → stop
            }
        }
    })
}

#[test]
fn test_dbus_client_mock() -> Result<()> {
    // DbusClient uses cfg!(test) and DBUS_SESSION_BUS_ADDRESS to select the session bus

    let ready = Arc::new(AtomicBool::new(false));
    let _handle = start_mock_daemon(ready.clone());

    // Wait for mock daemon to be ready
    for _ in 0..50 {
        if ready.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        ready.load(Ordering::SeqCst),
        "Mock daemon did not start in time"
    );

    // Give it a tiny bit more time to register the name
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Test client methods
    let client = DbusClient::new()?;

    let status = client.get_daemon_status()?;
    assert_eq!(status.0, "1.0.0"); // version
    assert_eq!(status.1, 100); // uptime_seconds

    let list = client.list_namespaces()?;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, "test-ns");

    let ns = client.get_namespace_status("test-ns")?;
    assert_eq!(ns.name, "test-ns");

    let ns_err = client.get_namespace_status("nonexistent");
    assert!(ns_err.is_err());

    let reload = client.reload_configuration()?;
    assert!(reload.success);

    let restart = client.restart_namespace("test-ns")?;
    assert!(restart.success);

    Ok(())
}
