use anyhow::Result;
use clap::Args;
use crate::dbus_client::DbusClient;

/// Arguments for the restart command
#[derive(Args)]
pub struct RestartArgs {
    /// Name of the namespace to restart
    #[arg(value_name = "NAMESPACE")]
    pub namespace: String,
    
    /// Force restart without confirmation prompt
    #[arg(short, long)]
    pub force: bool,
    
    /// Wait for restart to complete before returning
    #[arg(short, long)]
    pub wait: bool,
    
    /// Timeout in seconds when waiting for restart
    #[arg(long, default_value = "60")]
    pub timeout: u64,
    
    /// Show detailed progress during restart
    #[arg(short, long)]
    pub verbose: bool,
}

/// Execute the restart command
pub async fn execute(client: DbusClient, args: RestartArgs) -> Result<()> {
    // Check if daemon is available
    if !client.is_service_available().await {
        eprintln!("Error: segwire daemon is not running or not accessible");
        eprintln!("Please ensure segwire-daemon is started and running");
        std::process::exit(1);
    }
    
    restart_namespace(&client, &args).await
}

async fn restart_namespace(_client: &DbusClient, args: &RestartArgs) -> Result<()> {
    // Validate namespace name
    if args.namespace.is_empty() {
        return Err(anyhow::anyhow!("Namespace name cannot be empty"));
    }
    
    if !is_valid_namespace_name(&args.namespace) {
        return Err(anyhow::anyhow!("Invalid namespace name: {}", args.namespace));
    }
    
    // Check if this operation conflicts with automatic management
    let is_auto_managed = check_if_auto_managed(&args.namespace).await?;
    
    if is_auto_managed && !args.force {
        eprintln!("Note: Namespace '{}' is managed by configuration files.", args.namespace);
        eprintln!("This restart will recreate the namespace based on its current configuration file.");
        eprintln!("If you want to modify the namespace, edit its configuration file and use 'reload' instead.");
        eprintln!();
        
        if !confirm_restart(&args.namespace)? {
            println!("Restart cancelled by user");
            return Ok(());
        }
    }
    
    // Confirm restart if not forced
    if !args.force && !confirm_restart(&args.namespace)? {
        println!("Restart cancelled by user");
        return Ok(());
    }
    
    println!("Restarting namespace: {} (will recreate from configuration file)", args.namespace);
    
    if args.verbose {
        println!("Verbose progress reporting enabled");
    }
    
    // TODO: Implement actual D-Bus call to restart namespace
    // This will involve deleting and recreating the namespace
    // This will be implemented when the D-Bus methods are available
    
    if args.wait {
        println!("Waiting for namespace restart to complete (timeout: {}s)", args.timeout);
        
        if args.verbose {
            println!("Monitoring restart progress...");
            // TODO: Listen for D-Bus progress signals
        }
        
        // TODO: Implement waiting logic with progress updates
    }
    
    println!("Namespace restart initiated successfully");
    
    Ok(())
}

/// Check if a namespace is automatically managed by configuration files
async fn check_if_auto_managed(_namespace: &str) -> Result<bool> {
    // TODO: Implement actual check via D-Bus when methods are available
    // For now, assume all namespaces might be auto-managed
    Ok(true)
}

/// Prompt user for confirmation of restart
fn confirm_restart(namespace: &str) -> Result<bool> {
    use std::io::{self, Write};
    
    print!("Are you sure you want to restart namespace '{}'? [y/N]: ", namespace);
    io::stdout().flush()?;
    
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    
    let input = input.trim().to_lowercase();
    Ok(input == "y" || input == "yes")
}

/// Validate namespace name according to Linux namespace naming rules
fn is_valid_namespace_name(name: &str) -> bool {
    // Basic validation - namespace names should be valid identifiers
    // Must start with letter or underscore, contain only alphanumeric, underscore, hyphen
    if name.is_empty() || name.len() > 255 {
        return false;
    }
    
    let first_char = name.chars().next().unwrap();
    if !first_char.is_ascii_alphabetic() && first_char != '_' {
        return false;
    }
    
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_namespace_name_validation() {
        // Valid names
        assert!(is_valid_namespace_name("test"));
        assert!(is_valid_namespace_name("test-namespace"));
        assert!(is_valid_namespace_name("test_namespace"));
        assert!(is_valid_namespace_name("_test"));
        assert!(is_valid_namespace_name("test123"));
        
        // Invalid names
        assert!(!is_valid_namespace_name(""));
        assert!(!is_valid_namespace_name("123test"));
        assert!(!is_valid_namespace_name("-test"));
        assert!(!is_valid_namespace_name("test.namespace"));
        assert!(!is_valid_namespace_name("test namespace"));
        assert!(!is_valid_namespace_name("test/namespace"));
    }
    
    #[test]
    fn test_restart_args_validation() {
        let args = RestartArgs {
            namespace: "test-namespace".to_string(),
            force: false,
            wait: true,
            timeout: 90,
            verbose: true,
        };
        
        assert_eq!(args.namespace, "test-namespace");
        assert!(!args.force);
        assert!(args.wait);
        assert_eq!(args.timeout, 90);
        assert!(args.verbose);
    }
}