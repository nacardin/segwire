use segwire_common::{init_logging, DaemonConfig, LogConfig, LogLevel, SegwireResult};
use segwire_daemon::capabilities;
use segwire_daemon::event_loop::DaemonEventLoop;
use std::path::PathBuf;
use tracing::info;

#[monoio::main]
async fn main() -> SegwireResult<()> {
    // Default configuration path
    let config_path = PathBuf::from("/etc/segwire/daemon.toml");

    // Load daemon configuration
    let daemon_config = DaemonConfig::from_file(&config_path)?;

    // Initialize logging based on configuration
    let log_config = LogConfig {
        level: daemon_config
            .daemon
            .logging
            .level
            .parse()
            .unwrap_or(LogLevel::Info),
        with_timestamps: daemon_config.daemon.logging.with_timestamps,
        with_thread_names: daemon_config.daemon.logging.with_thread_names,
        with_file_line: daemon_config.daemon.logging.with_file_line,
        with_spans: daemon_config.daemon.logging.with_spans,
        component: "segwire-daemon".to_string(),
    };

    init_logging(log_config)
        .map_err(|e| {
            eprintln!("Failed to initialize logging: {}", e);
            std::process::exit(1);
        })
        .unwrap();

    info!("Segwire daemon starting...");
    info!("Configuration loaded from: {}", config_path.display());

    // Verify that the daemon has the required privileges
    match capabilities::verify_privileges() {
        Ok(cap_result) => {
            info!("Privilege check: {}", cap_result);
        }
        Err(e) => {
            tracing::error!("{}", e);
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }

    info!(
        "Namespace prefix: {}",
        daemon_config.daemon.namespace_prefix
    );
    info!(
        "Config directory: {}",
        daemon_config.daemon.config_dir.display()
    );

    // Create and run the daemon event loop
    let daemon = DaemonEventLoop::new(daemon_config, config_path).await?;
    daemon.run().await?;

    info!("Segwire daemon stopped");
    Ok(())
}
