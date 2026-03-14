//! Test harness for integration tests.
//!
//! Creates a temporary configuration directory, launches a private D-Bus
//! session, starts the daemon event loop in-process, and provides helpers
//! for writing namespace configs.
//!
//! The harness registers the daemon under the **default** well-known name
//! (`org.segwire.NamespaceManager`) on a private session bus so that the
//! standard CLI code-path can connect to it without any special wiring.

use anyhow::{Context, Result};
use segwire_common::dbus::interface;
use segwire_common::DaemonConfig;
use segwire_daemon::event_loop::DaemonEventLoop;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread::JoinHandle;
use tempfile::TempDir;

/// Test harness that manages a daemon instance.
pub struct TestHarness {
    /// Temporary directory containing daemon and namespace configs.
    pub config_dir: TempDir,
    /// Path to the master daemon.toml config file.
    pub config_path: PathBuf,
    /// PID of the private dbus-daemon, if we launched one.
    dbus_pid: Option<u32>,
}

impl TestHarness {
    /// Create a new test harness.
    ///
    /// Launches a **private** `dbus-daemon` for test isolation and sets
    /// `DBUS_SESSION_BUS_ADDRESS` so that both the daemon and the CLI client
    /// connect to it automatically.
    ///
    /// The daemon configuration uses the **default** well-known D-Bus name
    /// (`org.segwire.NamespaceManager`) so the CLI needs zero customisation.
    ///
    /// **Note**: this does NOT set `SEGWIRE_SIMULATION`. The test must set
    /// that itself if it wants simulation mode.
    pub fn new() -> Result<Self> {

        // Launch a private dbus-daemon for test isolation.
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

        // Write a minimal daemon.toml using the **default** D-Bus name so
        // the unmodified CLI can discover the service.
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
            interface::SERVICE_NAME,
            interface::OBJECT_PATH,
        )?;

        Ok(Self {
            config_dir,
            config_path,
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
    /// Returns a join handle and a shutdown flag.  Set the flag to `true`
    /// and then `join()` the handle to perform a graceful shutdown.
    pub fn start_daemon_background(&self) -> Result<(JoinHandle<()>, Arc<AtomicBool>)> {
        let config_content = std::fs::read_to_string(&self.config_path)?;
        let daemon_config: DaemonConfig = toml::from_str(&config_content)?;
        let event_loop = DaemonEventLoop::new(daemon_config, self.config_path.clone())?;
        let shutdown = event_loop.shutdown_signal();

        let handle = std::thread::Builder::new()
            .name("test-daemon".to_string())
            .spawn(move || {
                if let Err(e) = event_loop.run() {
                    eprintln!("Daemon event loop error: {}", e);
                }
            })
            .context("Failed to spawn daemon thread")?;

        // Give the daemon a moment to register its D-Bus name.
        std::thread::sleep(std::time::Duration::from_millis(200));

        Ok((handle, shutdown))
    }

    /// Start the daemon event loop (foreground, non-blocking init).
    ///
    /// Returns a `DaemonEventLoop` that can be used to interact with the daemon.
    pub fn start_daemon(&self) -> Result<DaemonEventLoop> {
        let config_content = std::fs::read_to_string(&self.config_path)?;
        let daemon_config: DaemonConfig = toml::from_str(&config_content)?;
        let event_loop = DaemonEventLoop::new(daemon_config, self.config_path.clone())?;
        Ok(event_loop)
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
