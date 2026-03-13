//! Test harness for integration tests.
//!
//! Creates a temporary configuration directory, optionally launches a private
//! D-Bus session, starts the daemon event loop in-process, and provides
//! helpers for writing namespace configs.

use anyhow::{Context, Result};
use segwire_common::DaemonConfig;
use segwire_daemon::event_loop::DaemonEventLoop;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

/// Global test counter for unique D-Bus service names.
static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Test harness that manages a daemon instance.
pub struct TestHarness {
    /// Temporary directory containing daemon and namespace configs.
    pub config_dir: TempDir,
    /// Path to the master daemon.toml config file.
    pub config_path: PathBuf,
    /// Shutdown flag for the daemon.
    shutdown_flag: Arc<AtomicBool>,
    /// PID of the private dbus-daemon, if we launched one.
    dbus_pid: Option<u32>,
}

impl TestHarness {
    /// Create a new test harness.
    ///
    /// Sets `SEGWIRE_TEST_SESSION_BUS=1` so the daemon uses session D-Bus.
    /// If `DBUS_SESSION_BUS_ADDRESS` is not set (e.g. under `sudo`), a
    /// private `dbus-daemon` is launched automatically.
    ///
    /// **Note**: this does NOT set `SEGWIRE_SIMULATION`. The test must set
    /// that itself if it wants simulation mode.
    pub fn new() -> Result<Self> {
        std::env::set_var("SEGWIRE_TEST_SESSION_BUS", "1");

        // Launch a private dbus-daemon for test isolation.
        // Use --fork so the daemon backgrounds itself after printing the address.
        // We record the PID for cleanup in Drop.
        let output = Command::new("dbus-daemon")
            .args(["--session", "--fork", "--print-address", "--print-pid"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .context("Failed to launch dbus-daemon (is dbus installed?)")?;

        let output_str = String::from_utf8_lossy(&output.stdout);
        let mut lines = output_str.lines();
        let address = lines
            .next()
            .context("dbus-daemon did not print address")?
            .trim()
            .to_string();
        let pid: u32 = lines
            .next()
            .context("dbus-daemon did not print PID")?
            .trim()
            .parse()
            .context("dbus-daemon printed invalid PID")?;

        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address);
        let dbus_pid = Some(pid);

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
namespace_prefix = "test"
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
            dbus_pid,
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

    /// Start the daemon event loop on a background thread.
    ///
    /// Returns a `DaemonEventLoop` that can be used to interact with the daemon.
    pub fn start_daemon(&self) -> Result<DaemonEventLoop> {
        let config_content = std::fs::read_to_string(&self.config_path)?;
        let daemon_config: DaemonConfig = toml::from_str(&config_content)?;
        let event_loop = DaemonEventLoop::new(daemon_config, self.config_path.clone())?;
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

impl Drop for TestHarness {
    fn drop(&mut self) {
        if let Some(pid) = self.dbus_pid {
            let nix_pid = nix::unistd::Pid::from_raw(pid as i32);
            let _ = nix::sys::signal::kill(nix_pid, nix::sys::signal::Signal::SIGTERM);

            // Wait for the dbus-daemon to actually exit so the next test
            // doesn't try to connect to the now-dead socket.
            for _ in 0..50 {
                match nix::sys::signal::kill(nix_pid, None) {
                    Ok(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                    Err(_) => break, // Process is gone
                }
            }

            // Clear the env var so the next harness starts clean
            std::env::remove_var("DBUS_SESSION_BUS_ADDRESS");
        }
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
