mod config;
mod dbus_service;
mod policykit;

use config::ConfigManager;
use dbus_service::DbusService;
use segwire_common::SegwireResult;
use std::path::PathBuf;
use tracing::{error, info, warn};

#[monoio::main]
async fn main() -> SegwireResult<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    info!("Segwire daemon starting...");

    // Check for required capabilities
    check_capabilities()?;

    // Default configuration path
    let config_path = PathBuf::from("/etc/segwire/daemon.toml");

    // Initialize configuration manager
    let config_manager = ConfigManager::new(config_path)?;
    info!("Configuration loaded successfully!");

    // Get configuration values
    let config_dir = config_manager.get_config_dir();
    let namespace_prefix = config_manager.get_namespace_prefix();

    // Initialize D-Bus service
    let dbus_service = DbusService::new(config_dir.clone(), namespace_prefix.clone()).await?;
    info!("D-Bus service initialized successfully!");

    // Start the main event loop
    info!("Starting daemon event loop...");
    
    // Spawn D-Bus service task
    let _dbus_task = monoio::spawn(async move {
        if let Err(e) = dbus_service.run().await {
            error!("D-Bus service error: {}", e);
        }
    });

    // Wait for shutdown signal (simplified for monoio)
    // In a full implementation, this would use proper signal handling
    info!("Daemon running - press Ctrl+C to stop");
    
    // For now, we'll use a simple loop that can be interrupted
    // In production, this should be replaced with proper signal handling
    loop {
        monoio::time::sleep(std::time::Duration::from_secs(1)).await;
        // The daemon will be stopped by external signals (systemd, etc.)
        // This is a placeholder - in production we'd handle SIGTERM/SIGINT
    }

    // This line is unreachable due to the infinite loop above
    // In production, proper signal handling would allow clean shutdown
    // info!("Segwire daemon stopped");
    Ok(())
}

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