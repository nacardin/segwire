//! Test harness for integration tests.
//!
//! Creates a temporary configuration directory, sets environment variables for
//! simulation mode, starts the daemon event loop in-process, and provides a
//! `DbusClient` to run CLI operations against it.

use anyhow::Result;
use segwire_common::DaemonConfig;
use segwire_daemon::event_loop::DaemonEventLoop;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

/// Global test counter for unique D-Bus service names.
static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Test harness that manages a simulated daemon instance.
pub struct TestHarness {
    /// Temporary directory containing daemon and namespace configs.
    pub config_dir: TempDir,
    /// Path to the master daemon.toml config file.
    pub config_path: PathBuf,
    /// Shutdown flag for the daemon.
    shutdown_flag: Arc<AtomicBool>,
}

impl TestHarness {
    /// Create a new test harness with an empty configuration.
    ///
    /// Sets `SEGWIRE_SIMULATION=1` and `SEGWIRE_TEST_SESSION_BUS=1` so
    /// the daemon uses session D-Bus and simulated netlink.
    pub fn new() -> Result<Self> {
        // Ensure env vars are set BEFORE any daemon component initialises
        std::env::set_var("SEGWIRE_SIMULATION", "1");
        std::env::set_var("SEGWIRE_TEST_SESSION_BUS", "1");

        let config_dir = TempDir::new()?;
        let config_path = config_dir.path().join("daemon.toml");

        // Generate a unique D-Bus service name for this test instance
        let test_id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let service_name = format!("org.segwire.Test{}", test_id);
        let object_path = format!("/org/segwire/Test{}", test_id);

        // Write a minimal daemon.toml
        let mut f = std::fs::File::create(&config_path)?;
        writeln!(
            f,
            r#"[daemon]
namespace_prefix = "test-"
config_dir = "{}"
log_level = "debug"
sync_interval_seconds = 30
cleanup_on_shutdown = false

[dbus]
service_name = "{}"
object_path = "{}"
"#,
            config_dir.path().display(),
            service_name,
            object_path,
        )?;

        Ok(Self {
            config_dir,
            config_path,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Write a namespace TOML config file into the config directory.
    pub fn write_namespace_config(&self, name: &str, content: &str) -> Result<PathBuf> {
        let path = self.config_dir.path().join(format!("{}.toml", name));
        std::fs::write(&path, content)?;
        Ok(path)
    }

    /// Remove a namespace TOML config file from the config directory.
    pub fn remove_namespace_config(&self, name: &str) -> Result<()> {
        let path = self.config_dir.path().join(format!("{}.toml", name));
        std::fs::remove_file(path)?;
        Ok(())
    }

    /// Get the config directory path.
    pub fn config_dir_path(&self) -> &Path {
        self.config_dir.path()
    }

    /// Start the daemon event loop on a background monoio task.
    ///
    /// Returns a `DaemonEventLoop` that can be used to interact with the daemon.
    pub async fn start_daemon(&self) -> Result<DaemonEventLoop> {
        let config_content = std::fs::read_to_string(&self.config_path)?;
        let daemon_config: DaemonConfig = toml::from_str(&config_content)?;
        let event_loop = DaemonEventLoop::new(daemon_config, self.config_path.clone()).await?;
        Ok(event_loop)
    }

    /// Get the shutdown flag for graceful shutdown testing.
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        self.shutdown_flag.clone()
    }

    /// Request a shutdown.
    pub fn request_shutdown(&self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
    }
}

/// A minimal namespace config for testing.
pub fn sample_namespace_config(name: &str) -> String {
    format!(
        r#"[namespace]
name = "{name}"
description = "Test namespace"

[interfaces]
move_interfaces = []
virtual_interfaces = []

[routing]

[dns]
servers = ["8.8.8.8"]
"#
    )
}
