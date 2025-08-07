mod config;

use segwire_common::SegwireResult;
use config::ConfigManager;
use std::path::PathBuf;

fn main() -> SegwireResult<()> {
    println!("Segwire daemon starting...");
    
    // Default configuration path
    let config_path = PathBuf::from("/etc/segwire/daemon.toml");
    
    // Initialize configuration manager
    let _config_manager = ConfigManager::new(config_path)?;
    
    println!("Configuration loaded successfully!");
    println!("Shared library integration working!");
    
    Ok(())
}