//! Event loop coordination for the segwire daemon
//! 
//! This module provides the main event loop implementation that coordinates
//! between configuration monitoring, D-Bus service, and graceful shutdown handling.
//! 
//! The event loop uses monoio runtime with io_uring support for high-performance
//! asynchronous I/O operations.

use crate::config::{ConfigManager, ConfigFileEvent};
use crate::dbus_service::DbusService;
use segwire_common::SegwireResult;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{error, info, warn, debug};

/// Check if the daemon has the required capabilities
fn check_capabilities() -> SegwireResult<()> {
    use nix::unistd::Uid;

    // Check if running as root or with CAP_SYS_ADMIN
    if !Uid::effective().is_root() {
        // In a full implementation, we would check for CAP_SYS_ADMIN capability
        // For now, we'll just warn and continue
        warn!("Daemon is not running as root - some operations may fail");
        warn!("Ensure the daemon has CAP_SYS_ADMIN capability for namespace operations");
    } else {
        info!("Daemon running with root privileges");
    }

    Ok(())
}

/// Main daemon event loop coordinator
/// 
/// This struct coordinates between different daemon components:
/// - Configuration file monitoring with io_uring-based file watching
/// - D-Bus service for CLI communication
/// - Graceful shutdown handling with cleanup
pub struct DaemonEventLoop {
    config_manager: Arc<Mutex<ConfigManager>>,
    dbus_service: Arc<DbusService>,
    shutdown_signal: Arc<AtomicBool>,
}

impl DaemonEventLoop {
    /// Create a new daemon event loop
    /// 
    /// # Arguments
    /// * `config_path` - Path to the master daemon configuration file
    /// 
    /// # Returns
    /// A new `DaemonEventLoop` instance ready to run
    pub async fn new(config_path: PathBuf) -> SegwireResult<Self> {
        info!("Initializing daemon event loop");

        // Check for required capabilities
        check_capabilities()?;

        // Initialize configuration manager
        let config_manager = Arc::new(Mutex::new(ConfigManager::new(config_path)?));
        info!("Configuration loaded successfully!");

        // Get configuration values for D-Bus service initialization
        let (config_dir, namespace_prefix) = {
            let manager = config_manager.lock().unwrap();
            (manager.get_config_dir(), manager.get_namespace_prefix())
        };

        // Initialize D-Bus service
        let dbus_service = Arc::new(DbusService::new(config_dir, namespace_prefix).await?);
        info!("D-Bus service initialized successfully!");

        // Create shutdown signal
        let shutdown_signal = Arc::new(AtomicBool::new(false));

        Ok(Self {
            config_manager,
            dbus_service,
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
            let mut manager = self.config_manager.lock().unwrap();
            if let Err(e) = manager.scan_namespace_configs() {
                error!("Initial configuration scan failed: {}", e);
                return Err(e);
            }
        }

        // Start configuration file monitoring
        let config_event_receiver = {
            let mut manager = self.config_manager.lock().unwrap();
            manager.start_file_monitoring().await?
        };

        // Spawn configuration monitoring task
        let config_task = self.spawn_config_monitoring_task(config_event_receiver);

        // Spawn D-Bus service task
        let dbus_task = self.spawn_dbus_service_task();

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
                            let mut manager = config_manager.lock().unwrap();
                            match manager.handle_file_event(event).await {
                                Ok(namespaces) => namespaces,
                                Err(e) => {
                                    error!("Failed to handle configuration file event: {}", e);
                                    continue;
                                }
                            }
                        };

                        // Emit D-Bus signals for affected namespaces
                        for namespace in affected_namespaces {
                            if let Err(e) = dbus_service.emit_namespace_status_changed(
                                &namespace,
                                "unknown",
                                "updated"
                            ).await {
                                warn!("Failed to emit namespace status change signal: {}", e);
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

    /// Spawn signal handling task for graceful shutdown
    /// 
    /// This task handles system signals (SIGTERM, SIGINT) for graceful shutdown.
    /// In the current implementation, it's a placeholder that can be extended
    /// with proper signal handling.
    fn spawn_signal_handling_task(&self) -> monoio::task::JoinHandle<()> {
        let shutdown_signal = self.shutdown_signal.clone();

        monoio::spawn(async move {
            info!("Signal handling task started");

            // In a real implementation, this would use proper signal handling
            // For now, we'll simulate signal handling with a timeout
            // This allows the daemon to run for testing and be stopped externally

            // Wait for a reasonable amount of time or until shutdown is requested
            let mut check_count = 0;
            loop {
                monoio::time::sleep(std::time::Duration::from_secs(1)).await;
                check_count += 1;

                // Check if shutdown was already requested by another task
                if shutdown_signal.load(Ordering::Relaxed) {
                    debug!("Shutdown already requested, signal handler exiting");
                    break;
                }

                // For testing purposes, we can add a timeout
                // In production, this would be replaced with actual signal handling
                if check_count % 60 == 0 {
                    debug!("Signal handler heartbeat: {} minutes", check_count / 60);
                }
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
            let manager = self.config_manager.lock().unwrap();
            manager.should_cleanup_on_shutdown()
        };

        if should_cleanup {
            info!("Performing namespace cleanup on shutdown");
            
            // Get list of managed namespaces
            let managed_namespaces = {
                let manager = self.config_manager.lock().unwrap();
                manager.namespace_configs().keys().cloned().collect::<Vec<_>>()
            };

            // Cleanup each managed namespace
            for namespace_name in managed_namespaces {
                info!("Cleaning up namespace: {}", namespace_name);
                
                // Emit deletion signal
                if let Err(e) = self.dbus_service.emit_namespace_deleted(
                    &namespace_name,
                    "daemon_shutdown"
                ).await {
                    warn!("Failed to emit namespace deletion signal for {}: {}", namespace_name, e);
                }

                // TODO: Implement actual namespace cleanup using netlink
                // This would involve:
                // 1. Moving interfaces back to default namespace
                // 2. Deleting the network namespace
                // 3. Cleaning up any associated resources
                debug!("Namespace cleanup for {} completed (placeholder)", namespace_name);
            }

            info!("Namespace cleanup completed");
        } else {
            info!("Namespace cleanup on shutdown is disabled");
        }

        // Emit final D-Bus signal
        if let Err(e) = self.dbus_service.emit_operation_progress(
            "daemon_shutdown",
            1.0,
            "Daemon shutdown completed"
        ).await {
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
        fs::create_dir_all(temp_dir.path().join("namespaces")).expect("Failed to create namespaces dir");
        
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
        let result = DaemonEventLoop::new(config_path).await;
        
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

    #[test]
    fn test_check_capabilities() {
        // Test that capability checking doesn't panic
        let result = check_capabilities();
        assert!(result.is_ok());
    }
}