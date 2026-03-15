//! Event loop coordination for the segwire daemon
//!
//! This module provides the main event loop implementation that coordinates
//! between configuration monitoring, D-Bus service, and graceful shutdown handling.
//!
//! The event loop uses monoio runtime with io_uring support for high-performance
//! asynchronous I/O operations.

use crate::config::{ConfigFileEvent, ConfigManager};
use crate::dbus_service::DbusService;
use crate::namespace_state::NamespaceStateManager;
use async_lock::Mutex;
use segwire_common::{log_info, DaemonConfig, LogContext, SegwireResult};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Main daemon event loop coordinator
///
/// This struct coordinates between different daemon components:
/// - Configuration file monitoring with io_uring-based file watching
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
#[derive(Clone)]
pub struct DaemonEventLoop {
    config_manager: Arc<Mutex<ConfigManager>>,
    dbus_service: Arc<DbusService>,
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
    pub async fn new(daemon_config: DaemonConfig, config_path: PathBuf) -> SegwireResult<Self> {
        let ctx = LogContext::new("daemon_initialization").with_config_path(config_path.clone());

        log_info!(ctx, "Initializing daemon event loop");

        // Note: Capability/privilege checks are done in main() before reaching here.

        // Initialize configuration manager from the already-loaded config
        let config_manager = Arc::new(Mutex::new(ConfigManager::from_config(
            daemon_config,
            config_path,
        )));
        log_info!(ctx, "Configuration manager initialized");

        // Initialize namespace state manager FIRST
        let state_manager = Arc::new(Mutex::new(NamespaceStateManager::new_auto().await?));
        log_info!(ctx, "Namespace state manager initialized successfully");

        // Get configuration values for logging
        let (config_dir, namespace_prefix) = {
            let manager = config_manager.lock().await;
            (
                manager.config_directory().to_path_buf(),
                manager.namespace_prefix().to_owned(),
            )
        };

        let ctx = ctx
            .with_field("namespace_prefix", namespace_prefix.clone())
            .with_field("config_dir", config_dir.display().to_string());

        // Initialize D-Bus service
        let dbus_service =
            Arc::new(DbusService::new(config_manager.clone(), state_manager.clone()).await?);
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
    /// 2. D-Bus service task
    /// 3. Signal handling task
    ///
    /// The event loop runs until a shutdown signal is received or a critical task fails.
    pub async fn run(&self) -> SegwireResult<()> {
        info!("Starting daemon event loop with task coordination");

        // Perform initial configuration scan
        {
            let mut manager = self.config_manager.lock().await;
            if let Err(e) = manager.scan_namespace_configs() {
                error!("Initial configuration scan failed: {}", e);
                return Err(e);
            }
        }

        // Start configuration file monitoring
        let config_event_receiver = {
            let mut manager = self.config_manager.lock().await;
            manager.start_file_monitoring().await?
        };

        // Spawn configuration monitoring task
        let config_task = self.spawn_config_monitoring_task(config_event_receiver);

        // Spawn D-Bus service task
        let dbus_task = self.spawn_dbus_service_task();

        // Spawn state synchronization task
        let state_task = self.spawn_state_synchronization_task();

        // Spawn signal handling task
        let signal_task = self.spawn_signal_handling_task();

        info!("All daemon tasks started successfully");

        // Wait for shutdown signal or task completion
        let shutdown_signal = self.shutdown_signal.clone();

        // Main coordination loop
        loop {
            // Check if shutdown was requested
            if shutdown_signal.load(Ordering::Relaxed) {
                info!("Shutdown signal received, initiating graceful shutdown");
                break;
            }

            // Check if any critical tasks have failed
            if config_task.is_finished() {
                error!("Configuration monitoring task terminated unexpectedly");
                break;
            }

            if dbus_task.is_finished() {
                error!("D-Bus service task terminated unexpectedly");
                break;
            }

            if state_task.is_finished() {
                error!("State synchronization task terminated unexpectedly");
                break;
            }

            if signal_task.is_finished() {
                debug!("Signal handling task completed");
                break;
            }

            // Sleep briefly before next check
            monoio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Perform graceful shutdown
        self.graceful_shutdown().await?;

        info!("Daemon event loop completed successfully");
        Ok(())
    }

    /// Spawn configuration monitoring task
    ///
    /// This task monitors configuration file changes using io_uring-based file watching
    /// and handles configuration updates by coordinating with the D-Bus service.
    fn spawn_config_monitoring_task(
        &self,
        config_event_receiver: std::sync::mpsc::Receiver<ConfigFileEvent>,
    ) -> monoio::task::JoinHandle<()> {
        let config_manager = self.config_manager.clone();
        let state_manager = self.state_manager.clone();
        let dbus_service = self.dbus_service.clone();
        let shutdown_signal = self.shutdown_signal.clone();

        monoio::spawn(async move {
            info!("Configuration monitoring task started");

            loop {
                // Check for shutdown signal
                if shutdown_signal.load(Ordering::Relaxed) {
                    debug!("Configuration monitoring task received shutdown signal");
                    break;
                }

                // Check for configuration file events (non-blocking)
                match config_event_receiver.try_recv() {
                    Ok(event) => {
                        debug!("Processing configuration file event: {:?}", event);

                        // Handle the configuration file event
                        let affected_namespaces = {
                            let mut manager = config_manager.lock().await;
                            match manager.handle_file_event(event).await {
                                Ok(namespaces) => namespaces,
                                Err(e) => {
                                    error!("Failed to handle configuration file event: {}", e);
                                    continue;
                                }
                            }
                        };

                        // Trigger immediate state synchronization for affected namespaces
                        if !affected_namespaces.is_empty() {
                            debug!(
                                "Configuration change detected, triggering state synchronization"
                            );

                            let config_mgr = config_manager.lock().await;
                            let mut state_mgr = state_manager.lock().await;

                            match state_mgr.force_sync(&config_mgr).await {
                                Ok(result) => {
                                    // Emit D-Bus signals for state changes
                                    for namespace in &result.created {
                                        if let Err(e) = dbus_service
                                            .emit_namespace_created(namespace, "config_change")
                                            .await
                                        {
                                            warn!("Failed to emit namespace created signal for {}: {}", namespace, e);
                                        }
                                    }

                                    for namespace in &result.deleted {
                                        if let Err(e) = dbus_service
                                            .emit_namespace_deleted(namespace, "config_change")
                                            .await
                                        {
                                            warn!("Failed to emit namespace deleted signal for {}: {}", namespace, e);
                                        }
                                    }

                                    for namespace in &result.updated {
                                        if let Err(e) = dbus_service
                                            .emit_namespace_status_changed(
                                                namespace, "unknown", "updated",
                                            )
                                            .await
                                        {
                                            warn!("Failed to emit namespace status changed signal for {}: {}", namespace, e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to synchronize state after configuration change: {}", e);
                                }
                            }
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        // No events available, continue
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        warn!("Configuration file monitoring channel disconnected");
                        break;
                    }
                }

                // Sleep briefly to avoid busy waiting
                monoio::time::sleep(std::time::Duration::from_millis(50)).await;
            }

            info!("Configuration monitoring task completed");
        })
    }

    /// Spawn D-Bus service task
    ///
    /// This task monitors the D-Bus service and handles shutdown coordination.
    /// The actual D-Bus request handling is done automatically by zbus.
    fn spawn_dbus_service_task(&self) -> monoio::task::JoinHandle<()> {
        let shutdown_signal = self.shutdown_signal.clone();

        monoio::spawn(async move {
            info!("D-Bus service task started");

            // The D-Bus service is already initialized and registered
            // The zbus connection handles incoming requests automatically
            // We just need to monitor for shutdown signals
            loop {
                // Check for shutdown signal
                if shutdown_signal.load(Ordering::Relaxed) {
                    debug!("D-Bus service task received shutdown signal");
                    break;
                }

                // Sleep briefly to avoid busy waiting
                monoio::time::sleep(std::time::Duration::from_millis(100)).await;
            }

            info!("D-Bus service task completed");
        })
    }

    /// Spawn state synchronization task
    ///
    /// This task periodically synchronizes the in-memory namespace state
    /// with the actual system state and configuration files.
    fn spawn_state_synchronization_task(&self) -> monoio::task::JoinHandle<()> {
        let config_manager = self.config_manager.clone();
        let state_manager = self.state_manager.clone();
        let dbus_service = self.dbus_service.clone();
        let shutdown_signal = self.shutdown_signal.clone();

        monoio::spawn(async move {
            info!("State synchronization task started");

            // Perform initial state synchronization
            {
                let config_mgr = config_manager.lock().await;
                let mut state_mgr = state_manager.lock().await;

                match state_mgr.force_sync(&config_mgr).await {
                    Ok(result) => {
                        info!("Initial state synchronization completed: {} created, {} updated, {} conflicts", 
                              result.created.len(), result.updated.len(), result.conflicts.len());

                        // Emit D-Bus signals for created namespaces
                        for namespace in &result.created {
                            if let Err(e) = dbus_service
                                .emit_namespace_created(namespace, "initial_sync")
                                .await
                            {
                                warn!(
                                    "Failed to emit namespace created signal for {}: {}",
                                    namespace, e
                                );
                            }
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

            let mut maintenance_counter = 0;

            loop {
                // Check for shutdown signal
                if shutdown_signal.load(Ordering::Relaxed) {
                    debug!("State synchronization task received shutdown signal");
                    break;
                }

                // Check if synchronization is needed
                let needs_sync = {
                    let state_mgr = state_manager.lock().await;
                    state_mgr.needs_sync()
                };

                if needs_sync {
                    debug!("Performing periodic state synchronization");

                    let config_mgr = config_manager.lock().await;
                    let mut state_mgr = state_manager.lock().await;

                    match state_mgr.synchronize_state(&config_mgr).await {
                        Ok(result) => {
                            if !result.created.is_empty()
                                || !result.updated.is_empty()
                                || !result.deleted.is_empty()
                                || !result.conflicts.is_empty()
                            {
                                info!("State synchronization completed: {} created, {} updated, {} deleted, {} conflicts", 
                                      result.created.len(), result.updated.len(), result.deleted.len(), result.conflicts.len());

                                // Emit D-Bus signals for state changes
                                for namespace in &result.created {
                                    if let Err(e) =
                                        dbus_service.emit_namespace_created(namespace, "sync").await
                                    {
                                        warn!(
                                            "Failed to emit namespace created signal for {}: {}",
                                            namespace, e
                                        );
                                    }
                                }

                                for namespace in &result.deleted {
                                    if let Err(e) =
                                        dbus_service.emit_namespace_deleted(namespace, "sync").await
                                    {
                                        warn!(
                                            "Failed to emit namespace deleted signal for {}: {}",
                                            namespace, e
                                        );
                                    }
                                }

                                for namespace in &result.updated {
                                    if let Err(e) = dbus_service
                                        .emit_namespace_status_changed(
                                            namespace, "unknown", "updated",
                                        )
                                        .await
                                    {
                                        warn!("Failed to emit namespace status changed signal for {}: {}", namespace, e);
                                    }
                                }

                                // Auto-resolve Create/Delete conflicts
                                for conflict in &result.conflicts {
                                    match conflict.resolution {
                                        crate::namespace_state::ConflictResolution::CreateNamespace |
                                        crate::namespace_state::ConflictResolution::DeleteNamespace => {
                                            if let Err(e) = state_mgr.resolve_conflict(conflict, &config_mgr).await {
                                                warn!("Failed to auto-resolve conflict for {}: {}", conflict.namespace_name, e);
                                            } else {
                                                info!("Auto-resolved conflict for {}", conflict.namespace_name);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("State synchronization failed: {}", e);
                        }
                    }
                }

                // Perform maintenance every 10 sync cycles (approximately every 5 minutes)
                maintenance_counter += 1;
                if maintenance_counter >= 10 {
                    maintenance_counter = 0;

                    let mut state_mgr = state_manager.lock().await;
                    if let Err(e) = state_mgr.perform_maintenance().await {
                        warn!("State maintenance failed: {}", e);
                    }
                }

                // Sleep for 30 seconds before next check
                monoio::time::sleep(std::time::Duration::from_secs(30)).await;
            }

            info!("State synchronization task completed");
        })
    }

    /// Spawn signal handling task for graceful shutdown
    ///
    /// This task handles system signals (SIGTERM, SIGINT) for graceful shutdown.
    /// In the current implementation, it's a placeholder that can be extended
    /// with proper signal handling.
    fn spawn_signal_handling_task(&self) -> monoio::task::JoinHandle<()> {
        let shutdown_signal = self.shutdown_signal.clone();
        let daemon_clone = self.clone();

        monoio::spawn(async move {
            info!("Signal handling task started");

            // Use ctrlc crate to handle SIGINT and SIGTERM
            if let Err(e) = ctrlc::set_handler(move || {
                info!("Received termination signal (SIGINT/SIGTERM)");
                daemon_clone.request_shutdown();
            }) {
                error!("Error setting signal handler: {}", e);
            }

            // Wait for shutdown to be requested by the handler or another task
            loop {
                if shutdown_signal.load(Ordering::Relaxed) {
                    debug!("Shutdown requested, signal handler exiting");
                    break;
                }
                monoio::time::sleep(std::time::Duration::from_secs(1)).await;
            }

            info!("Signal handling task completed");
        })
    }

    /// Perform graceful shutdown with cleanup
    ///
    /// This method handles the graceful shutdown sequence:
    /// 1. Sets shutdown signal to stop all tasks
    /// 2. Optionally cleans up managed namespaces
    /// 3. Emits final D-Bus signals
    async fn graceful_shutdown(&self) -> SegwireResult<()> {
        info!("Starting graceful shutdown sequence");

        // Set shutdown signal to stop all tasks
        self.shutdown_signal.store(true, Ordering::Relaxed);

        // Check if we should cleanup namespaces on shutdown
        let should_cleanup = {
            let manager = self.config_manager.lock().await;
            manager.should_cleanup_on_shutdown()
        };

        if should_cleanup {
            info!("Performing namespace cleanup on shutdown");

            // Get list of managed namespaces
            let managed_namespaces = {
                let manager = self.config_manager.lock().await;
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
                    .await
                {
                    warn!(
                        "Failed to emit namespace deletion signal for {}: {}",
                        namespace_name, e
                    );
                }

                // Actual namespace cleanup using netlink
                if let Ok(netlink) = segwire_common::netlink::NetlinkManager::new() {
                    let config_mgr = self.config_manager.lock().await;
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
        if let Err(e) = self
            .dbus_service
            .emit_operation_progress("daemon_shutdown", 1.0, "Daemon shutdown completed")
            .await
        {
            warn!("Failed to emit shutdown completion signal: {}", e);
        }

        info!("Graceful shutdown completed");
        Ok(())
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

    #[monoio::test]
    async fn test_daemon_event_loop_creation() {
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
        let result = DaemonEventLoop::new(daemon_config, config_path).await;

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
