use anyhow::Result;
use clap::Args;
use crate::dbus_client::DbusClient;

/// Arguments for the status command
#[derive(Args)]
pub struct StatusArgs {
    /// Specific namespace to show status for
    #[arg(value_name = "NAMESPACE")]
    pub namespace: Option<String>,
    
    /// Output format
    #[arg(short, long, value_enum, default_value = "table")]
    pub format: OutputFormat,
    
    /// Show detailed information including interfaces and routes
    #[arg(short, long)]
    pub detailed: bool,
    
    /// Show network statistics and performance metrics
    #[arg(short, long)]
    pub stats: bool,
    
    /// Show recent log entries for the namespace
    #[arg(short, long)]
    pub logs: bool,
}

#[derive(clap::ValueEnum, Clone)]
pub enum OutputFormat {
    Table,
    Json,
    Yaml,
}

/// Execute the status command
pub async fn execute(client: DbusClient, args: StatusArgs) -> Result<()> {
    // Check if daemon is available
    if !client.is_service_available().await {
        eprintln!("Error: segwire daemon is not running or not accessible");
        eprintln!("Please ensure segwire-daemon is started and running");
        std::process::exit(1);
    }
    
    match &args.namespace {
        Some(namespace) => {
            // Show detailed status for specific namespace
            show_namespace_status(&client, namespace, &args).await
        }
        None => {
            // Show status for all namespaces
            show_all_namespaces_status(&client, &args).await
        }
    }
}

async fn show_namespace_status(_client: &DbusClient, namespace: &str, args: &StatusArgs) -> Result<()> {
    // Validate namespace name
    if namespace.is_empty() {
        return Err(anyhow::anyhow!("Namespace name cannot be empty"));
    }
    
    if !is_valid_namespace_name(namespace) {
        return Err(anyhow::anyhow!("Invalid namespace name: {}", namespace));
    }
    
    println!("Getting status for namespace: {}", namespace);
    
    // TODO: Implement actual D-Bus call to get namespace status
    // This will be implemented when the D-Bus methods are available
    match args.format {
        OutputFormat::Table => {
            println!("Status information for namespace '{}' (table format)", namespace);
            if args.detailed {
                println!("  - Detailed information requested");
            }
            if args.stats {
                println!("  - Statistics requested");
            }
            if args.logs {
                println!("  - Logs requested");
            }
        }
        OutputFormat::Json => {
            println!("{{\"namespace\": \"{}\", \"status\": \"placeholder\"}}", namespace);
        }
        OutputFormat::Yaml => {
            println!("namespace: {}", namespace);
            println!("status: placeholder");
        }
    }
    
    Ok(())
}

async fn show_all_namespaces_status(_client: &DbusClient, args: &StatusArgs) -> Result<()> {
    println!("Getting status for all managed namespaces");
    
    // TODO: Implement actual D-Bus call to list all namespaces
    // This will be implemented when the D-Bus methods are available
    match args.format {
        OutputFormat::Table => {
            println!("All namespaces status (table format)");
            if args.detailed {
                println!("  - Detailed information requested");
            }
            if args.stats {
                println!("  - Statistics requested");
            }
        }
        OutputFormat::Json => {
            println!("{{\"namespaces\": [], \"status\": \"placeholder\"}}");
        }
        OutputFormat::Yaml => {
            println!("namespaces: []");
            println!("status: placeholder");
        }
    }
    
    Ok(())
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
}