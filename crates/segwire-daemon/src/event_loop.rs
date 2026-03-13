//! Event loop coordination for the segwire daemon
//!
//! This module provides the main event loop implementation that coordinates
//! between configuration monitoring, D-Bus service, and graceful shutdown handling.
//!
//! The event loop uses OS threads for concurrent operations, coordinated via
//! `std::sync::Mutex` and `std::sync::mpsc` channels.
//!
//! # Architecture
//!
//! The `dbus::blocking::Connection` is `!Sync`, so the D-Bus service must stay
//! on the main thread.  Background threads communicate state changes back to
//! the main thread via an `mpsc` channel, and the main thread emits D-Bus
//! signals on their behalf.

use crate::config::{ConfigFileEvent, ConfigManager};
use crate::dbus_service::DbusService;
use crate::namespace_state::NamespaceStateManager;
use segwire_common::{log_info, DaemonConfig, LogContext, SegwireResult};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tracing::{debug, error, info, warn};

/// Events sent from background threads to the main loop for D-Bus signal emission.
#[derive(Debug)]
enum DaemonEvent {
    Created {
        name: String,
        source: String,
    },
    Deleted {
        name: String,
        source: String,
    },
    StatusChanged {
        name: String,
        old_status: String,
        new_status: String,
    },
}

/// Main daemon event loop coordinator
///
/// This struct coordinates between different daemon components:
/// - Configuration file monitoring with inotify-based file watching
/// - D-Bus service for CLI communication
/// - Namespace state management and synchronization
/// - Graceful shutdown handling with cleanup
///
/// # Lock Ordering
///
/// When acquiring multiple locks, always lock in this order to prevent deadlocks:
/// 1. `config_manager`
/// 2. `state_manager`
///
/// Never hold `state_manager` while acquiring `config_manager`.
pub struct DaemonEventLoop {
    config_manager: Arc<Mutex<ConfigManager>>,
    dbus_service: DbusService,
    state_manager: Arc<Mutex<NamespaceStateManager>>,
    shutdown_signal: Arc<AtomicBool>,
}

impl DaemonEventLoop {
    /// Create a new daemon event loop
    ///
    /// # Arguments
    /// * `daemon_config` - The already-loaded daemon configuration
    /// * `config_path` - Path to the master daemon configuration file
    ///
    /// # Returns
    /// A new `DaemonEventLoop` instance ready to run
    pub fn new(daemon_config: DaemonConfig, config_path: PathBuf) -> SegwireResult<Self> {
        let ctx = LogContext::new("daemon_initialization").with_config_path(config_path.clone());

        log_info!(ctx, "Initializing daemon event loop");

        // Note: Capability/privilege checks are done in main() before reaching here.

        // Initialize configuration manager from the already-loaded config
        let config_manager = Arc::new(Mutex::new(ConfigManager::from_config(
            daemon_config,
            config_path,
        )));
        log_info!(ctx, "Configuration manager initialized");

        // Initialize namespace state manager
        let state_manager = Arc::new(Mutex::new(NamespaceStateManager::new_auto()?));
        log_info!(ctx, "Namespace state manager initialized successfully");

        // Get configuration values for logging
        let (config_dir, namespace_prefix) = {
            let manager = config_manager.lock().unwrap();
            (
                manager.config_directory().to_path_buf(),
                manager.namespace_prefix().to_owned(),
            )
        };

        let ctx = ctx
            .with_field("namespace_prefix", namespace_prefix.clone())
            .with_field("config_dir", config_dir.display().to_string());

        // Initialize D-Bus service (not Arc — stays on the main thread)
        let dbus_service = DbusService::new(config_manager.clone(), state_manager.clone())?;
        log_info!(ctx, "D-Bus service initialized successfully");

        // Create shutdown signal
        let shutdown_signal = Arc::new(AtomicBool::new(false));

        Ok(Self {
            config_manager,
            dbus_service,
            state_manager,
            shutdown_signal,
        })
    }

    /// Run the main event loop with task coordination
    ///
    /// This method starts all daemon tasks and coordinates their execution:
    /// 1. Configuration file monitoring task
    /// 2. State synchronization task
    /// 3. Signal handling
    ///
    /// The event loop runs until a shutdown signal is received or a critical task fails.
    pub fn run(&self) -> SegwireResult<()> {
        info!("Starting daemon event loop with task coordination");

        // Perform initial configuration scan
        {
            let mut manager = self.config_manager.lock().unwrap();
            if let Err(e) = manager.scan_namespace_configs() {
                error!("Initial configuration scan failed: {}", e);
                return Err(e);
            }
        }

        // Start configuration file monitoring
        let config_event_receiver = {
            let mut manager = self.config_manager.lock().unwrap();
            manager.start_file_monitoring()?
        };

        // Channel for background threads to notify the main loop of state changes
        let (event_tx, event_rx) = std::sync::mpsc::channel::<DaemonEvent>();

        // Spawn configuration monitoring thread
        let config_thread =
            self.spawn_config_monitoring_thread(config_event_receiver, event_tx.clone());

        // Spawn state synchronization thread
        let state_thread = self.spawn_state_synchronization_thread(event_tx);

        // Set up signal handler for graceful shutdown
        {
            let shutdown_signal = self.shutdown_signal.clone();
            if let Err(e) = ctrlc::set_handler(move || {
                info!("Received termination signal (SIGINT/SIGTERM)");
                shutdown_signal.store(true, Ordering::Relaxed);
            }) {
                error!("Error setting signal handler: {}", e);
            }
        }

        info!("All daemon tasks started successfully");

        // Main coordination loop:
        //   1. Process incoming D-Bus messages (method calls from CLI)
        //   2. Drain daemon events from background threads and emit D-Bus signals
        //   3. Check shutdown flag
        loop {
            if self.shutdown_signal.load(Ordering::Relaxed) {
                info!("Shutdown signal received, initiating graceful shutdown");
                break;
            }

            // Process D-Bus messages for up to 100ms
            if let Err(e) = self
                .dbus_service
                .process(std::time::Duration::from_millis(100))
            {
                warn!("Error processing D-Bus messages: {}", e);
            }

            // Drain events from background threads and emit signals
            while let Ok(event) = event_rx.try_recv() {
                match event {
                    DaemonEvent::Created { name, source } => {
                        if let Err(e) = self.dbus_service.emit_namespace_created(&name, &source) {
                            warn!(
                                "Failed to emit namespace created signal for {}: {}",
                                name, e
                            );
                        }
                    }
                    DaemonEvent::Deleted { name, source } => {
                        if let Err(e) = self.dbus_service.emit_namespace_deleted(&name, &source) {
                            warn!(
                                "Failed to emit namespace deleted signal for {}: {}",
                                name, e
                            );
                        }
                    }
                    DaemonEvent::StatusChanged {
                        name,
                        old_status,
                        new_status,
                    } => {
                        if let Err(e) = self.dbus_service.emit_namespace_status_changed(
                            &name,
                            &old_status,
                            &new_status,
                        ) {
                            warn!(
                                "Failed to emit namespace status changed signal for {}: {}",
                                name, e
                            );
                        }
                    }
                }
            }
        }

        // Perform graceful shutdown
        self.graceful_shutdown()?;

        // Wait for threads to finish
        if let Err(e) = config_thread.join() {
            error!("Configuration monitoring thread panicked: {:?}", e);
        }
        if let Err(e) = state_thread.join() {
            error!("State synchronization thread panicked: {:?}", e);
        }

        info!("Daemon event loop completed successfully");
        Ok(())
    }

    /// Spawn configuration monitoring thread
    ///
    /// This thread monitors configuration file changes using inotify-based file watching
    /// and handles configuration updates by coordinating with the D-Bus service.
    fn spawn_config_monitoring_thread(
        &self,
        config_event_receiver: std::sync::mpsc::Receiver<ConfigFileEvent>,
        event_tx: std::sync::mpsc::Sender<DaemonEvent>,
    ) -> JoinHandle<()> {
        let config_manager = self.config_manager.clone();
        let state_manager = self.state_manager.clone();
        let shutdown_signal = self.shutdown_signal.clone();

        std::thread::Builder::new()
            .name("config-monitor".to_string())
            .spawn(move || {
                info!("Configuration monitoring thread started");

                loop {
                    // Check for shutdown signal
                    if shutdown_signal.load(Ordering::Relaxed) {
                        debug!("Configuration monitoring thread received shutdown signal");
                        break;
                    }

                    // Wait for the next configuration file event with a timeout
                    // so we can periodically check the shutdown signal.
                    let event = match config_event_receiver
                        .recv_timeout(std::time::Duration::from_secs(1))
                    {
                        Ok(event) => event,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            warn!("Configuration file monitoring channel closed");
                            break;
                        }
                    };

                    debug!("Processing configuration file event: {:?}", event);

                    // Handle the configuration file event
                    let affected_namespaces = {
                        let mut manager = config_manager.lock().unwrap();
                        match manager.handle_file_event(event) {
                            Ok(namespaces) => namespaces,
                            Err(e) => {
                                error!("Failed to handle configuration file event: {}", e);
                                continue;
                            }
                        }
                    };

                    // Trigger immediate state synchronization for affected namespaces
                    if !affected_namespaces.is_empty() {
                        debug!("Configuration change detected, triggering state synchronization");

                        let config_mgr = config_manager.lock().unwrap();
                        let mut state_mgr = state_manager.lock().unwrap();

                        match state_mgr.force_sync(&config_mgr) {
                            Ok(result) => {
                                // Send events to main loop for D-Bus signal emission
                                for namespace in &result.created {
                                    let _ = event_tx.send(DaemonEvent::Created {
                                        name: namespace.clone(),
                                        source: "config_change".to_string(),
                                    });
                                }

                                for namespace in &result.deleted {
                                    let _ = event_tx.send(DaemonEvent::Deleted {
                                        name: namespace.clone(),
                                        source: "config_change".to_string(),
                                    });
                                }

                                for namespace in &result.updated {
                                    let _ = event_tx.send(DaemonEvent::StatusChanged {
                                        name: namespace.clone(),
                                        old_status: "unknown".to_string(),
                                        new_status: "updated".to_string(),
                                    });
                                }
                            }
                            Err(e) => {
                                error!(
                                    "Failed to synchronize state after configuration change: {}",
                                    e
                                );
                            }
                        }
                    }
                }

                info!("Configuration monitoring thread completed");
            })
            .expect("Failed to spawn config-monitor thread")
    }

    /// Spawn state synchronization thread
    ///
    /// This thread periodically synchronizes the in-memory namespace state
    /// with the actual system state and configuration files.
    fn spawn_state_synchronization_thread(
        &self,
        event_tx: std::sync::mpsc::Sender<DaemonEvent>,
    ) -> JoinHandle<()> {
        let config_manager = self.config_manager.clone();
        let state_manager = self.state_manager.clone();
        let shutdown_signal = self.shutdown_signal.clone();

        std::thread::Builder::new()
            .name("state-sync".to_string())
            .spawn(move || {
                info!("State synchronization thread started");

                // Perform initial state synchronization
                {
                    let config_mgr = config_manager.lock().unwrap();
                    let mut state_mgr = state_manager.lock().unwrap();

                    match state_mgr.force_sync(&config_mgr) {
                        Ok(result) => {
                            info!("Initial state synchronization completed: {} created, {} updated, {} conflicts", 
                                  result.created.len(), result.updated.len(), result.conflicts.len());

                            // Send events to main loop for D-Bus signal emission
                            for namespace in &result.created {
                                let _ = event_tx.send(DaemonEvent::Created {
                                    name: namespace.clone(),
                                    source: "initial_sync".to_string(),
                                });
                            }

                            // Log conflicts for manual resolution
                            for conflict in &result.conflicts {
                                warn!(
                                    "State conflict detected: {} - {} (resolution: {:?})",
                                    conflict.namespace_name, conflict.description, conflict.resolution
                                );
                            }
                        }
                        Err(e) => {
                            error!("Initial state synchronization failed: {}", e);
                        }
                    }
                }

                loop {
                    // Check for shutdown signal
                    if shutdown_signal.load(Ordering::Relaxed) {
                        debug!("State synchronization thread received shutdown signal");
                        break;
                    }

                    // Check if synchronization is needed
                    let needs_sync = {
                        let state_mgr = state_manager.lock().unwrap();
                        state_mgr.needs_sync()
                    };

                    if needs_sync {
                        debug!("Performing periodic state synchronization");

                        let config_mgr = config_manager.lock().unwrap();
                        let mut state_mgr = state_manager.lock().unwrap();

                        match state_mgr.synchronize_state(&config_mgr) {
                            Ok(result) => {
                                if !result.created.is_empty()
                                    || !result.updated.is_empty()
                                    || !result.deleted.is_empty()
                                    || !result.conflicts.is_empty()
                                {
                                    info!("State synchronization completed: {} created, {} updated, {} deleted, {} conflicts", 
                                          result.created.len(), result.updated.len(), result.deleted.len(), result.conflicts.len());

                                    // Send events to main loop for signal emission
                                    for namespace in &result.created {
                                        let _ = event_tx.send(DaemonEvent::Created {
                                            name: namespace.clone(),
                                            source: "sync".to_string(),
                                        });
                                    }

                                    for namespace in &result.deleted {
                                        let _ = event_tx.send(DaemonEvent::Deleted {
                                            name: namespace.clone(),
                                            source: "sync".to_string(),
                                        });
                                    }

                                    for namespace in &result.updated {
                                        let _ = event_tx.send(DaemonEvent::StatusChanged {
                                            name: namespace.clone(),
                                            old_status: "unknown".to_string(),
                                            new_status: "updated".to_string(),
                                        });
                                    }

                                    for conflict in &result.conflicts {
                                        warn!(
                                            "State synchronization conflict for {}: {:?}",
                                            conflict.namespace_name, conflict.resolution
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Failed to synchronize namespace states: {}", e);
                            }
                        }

                        // Perform periodic maintenance
                        if let Err(e) = state_mgr.perform_maintenance() {
                            warn!("State maintenance failed: {}", e);
                        }
                    }

                    // Sleep for 30 seconds before next check
                    std::thread::sleep(std::time::Duration::from_secs(30));
                }

                info!("State synchronization thread completed");
            })
            .expect("Failed to spawn state-sync thread")
    }

    /// Perform graceful shutdown with cleanup
    ///
    /// This method handles the graceful shutdown sequence:
    /// 1. Sets shutdown signal to stop all tasks
    /// 2. Optionally cleans up managed namespaces
    /// 3. Emits final D-Bus signals
    fn graceful_shutdown(&self) -> SegwireResult<()> {
        info!("Starting graceful shutdown sequence");

        // Set shutdown signal to stop all tasks
        self.shutdown_signal.store(true, Ordering::Relaxed);

        // Check if we should cleanup namespaces on shutdown
        let should_cleanup = {
            let manager = self.config_manager.lock().unwrap();
            manager.should_cleanup_on_shutdown()
        };

        if should_cleanup {
            info!("Performing namespace cleanup on shutdown");

            // Get list of managed namespaces
            let managed_namespaces = {
                let manager = self.config_manager.lock().unwrap();
                manager
                    .namespace_configs()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            };

            // Cleanup each managed namespace
            for namespace_name in managed_namespaces {
                info!("Cleaning up namespace: {}", namespace_name);

                // Emit deletion signal
                if let Err(e) = self
                    .dbus_service
                    .emit_namespace_deleted(&namespace_name, "daemon_shutdown")
                {
                    warn!(
                        "Failed to emit namespace deletion signal for {}: {}",
                        namespace_name, e
                    );
                }

                // Actual namespace cleanup using netlink
                if let Ok(netlink) = crate::netlink::NetlinkManager::new() {
                    let config_mgr = self.config_manager.lock().unwrap();
                    if let Some(config_entry) = config_mgr.get_namespace_config(&namespace_name) {
                        // 1. Moving interfaces back to default namespace
                        for if_name in &config_entry.config.interfaces.move_interfaces {
                            if let Err(e) = netlink
                                .move_interface_from_namespace_to_default(&namespace_name, if_name)
                            {
                                warn!(
                                    "Failed to move interface {} from namespace {}: {}",
                                    if_name, namespace_name, e
                                );
                            }
                        }
                    }

                    // 2. Deleting the network namespace
                    if let Err(e) = netlink.delete_namespace(&namespace_name) {
                        warn!(
                            "Failed to delete network namespace {}: {}",
                            namespace_name, e
                        );
                    } else {
                        info!("Successfully cleaned up namespace {}", namespace_name);
                    }
                } else {
                    warn!(
                        "Failed to initialize NetlinkManager for cleanup of {}",
                        namespace_name
                    );
                }
            }

            info!("Namespace cleanup completed");
        } else {
            info!("Namespace cleanup on shutdown is disabled");
        }

        // Emit final D-Bus signal
        if let Err(e) = self.dbus_service.emit_operation_progress(
            "daemon_shutdown",
            1.0,
            "Daemon shutdown completed",
        ) {
            warn!("Failed to emit shutdown completion signal: {}", e);
        }

        info!("Graceful shutdown completed");
        Ok(())
    }

    /// Get a reference to the config manager for test inspection.
    pub fn config_manager(&self) -> &Arc<Mutex<ConfigManager>> {
        &self.config_manager
    }

    /// Get a reference to the state manager for test inspection.
    pub fn state_manager(&self) -> &Arc<Mutex<NamespaceStateManager>> {
        &self.state_manager
    }

    /// Request shutdown from external signal
    ///
    /// This method can be called from signal handlers or other external
    /// sources to request a graceful shutdown of the daemon.
    pub fn request_shutdown(&self) {
        info!("Shutdown requested");
        self.shutdown_signal.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_daemon_config(temp_dir: &TempDir, namespace_prefix: &str) -> PathBuf {
        let config_content = format!(
            r#"
[daemon]
namespace_prefix = "{}"
config_dir = "{}"
cleanup_on_shutdown = true
log_level = "info"
log_target = "stdout"

[dbus]
service_name = "org.segwire.NamespaceManager"
object_path = "/org/segwire/NamespaceManager"
"#,
            namespace_prefix,
            temp_dir.path().join("namespaces").display()
        );

        let config_path = temp_dir.path().join("daemon.toml");
        fs::write(&config_path, config_content).expect("Failed to write test config");

        // Create the namespaces directory
        fs::create_dir_all(temp_dir.path().join("namespaces"))
            .expect("Failed to create namespaces dir");

        config_path
    }

    #[test]
    fn test_daemon_event_loop_creation() {
        // Initialize logging for test
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .try_init();

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = create_test_daemon_config(&temp_dir, "test");

        // Test that we can create a daemon event loop
        let config_content = std::fs::read_to_string(&config_path).expect("Failed to read config");
        let daemon_config: DaemonConfig =
            toml::from_str(&config_content).expect("Failed to parse config");
        let result = DaemonEventLoop::new(daemon_config, config_path);

        // The creation might fail due to D-Bus system bus not being available in test environment
        // but we can at least verify the basic structure works
        match result {
            Ok(daemon) => {
                // If creation succeeds, verify the shutdown mechanism works
                daemon.request_shutdown();
                assert!(daemon.shutdown_signal.load(Ordering::Relaxed));
            }
            Err(e) => {
                // Expected in test environment without D-Bus system bus
                println!("Expected error in test environment: {}", e);
            }
        }
    }
}
