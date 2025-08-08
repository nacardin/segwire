mod config;
mod dbus_service;
mod event_loop;
mod policykit;

use event_loop::DaemonEventLoop;
use segwire_common::SegwireResult;
use std::path::PathBuf;
use tracing::info;

#[monoio::main]
async fn main() -> SegwireResult<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    info!("Segwire daemon starting...");

    // Default configuration path
    let config_path = PathBuf::from("/etc/segwire/daemon.toml");

    // Create and run the daemon event loop
    let daemon = DaemonEventLoop::new(config_path).await?;
    daemon.run().await?;

    info!("Segwire daemon stopped");
    Ok(())
}