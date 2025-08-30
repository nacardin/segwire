use crate::dbus_client::DbusClient;
use crate::output::{
    self, DaemonStatusInfo, InterfaceData, NamespaceStatusData, OutputFormat, RouteData,
};
use anyhow::Result;
use clap::Args;

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

async fn show_namespace_status(
    client: &DbusClient,
    namespace: &str,
    args: &StatusArgs,
) -> Result<()> {
    // Validate namespace name
    if namespace.is_empty() {
        return Err(anyhow::anyhow!("Namespace name cannot be empty"));
    }

    if !is_valid_namespace_name(namespace) {
        return Err(anyhow::anyhow!("Invalid namespace name: {}", namespace));
    }

    // Fetch detailed namespace state from daemon via D-Bus
    let state = client.get_namespace_status(namespace).await?;

    let data = NamespaceStatusData {
        name: state.name,
        full_name: state.full_name,
        status: state.status,
        config_path: state.config_path,
        interfaces: state
            .interfaces
            .into_iter()
            .map(|i| InterfaceData {
                name: i.name,
                iface_type: i.interface_type,
                status: i.status,
                addresses: i.addresses,
            })
            .collect(),
        routes: state
            .routes
            .into_iter()
            .map(|r| RouteData {
                destination: r.destination,
                gateway: r.gateway,
                metric: r.metric,
                interface: r.interface,
            })
            .collect(),
        dns_servers: state.dns_config.servers,
        dns_search_domains: state.dns_config.search_domains,
        created_at: state.created_at,
        last_updated: state.last_updated,
    };

    output::format_namespace_status(&data, &args.format, args.detailed)
}

async fn show_all_namespaces_status(client: &DbusClient, args: &StatusArgs) -> Result<()> {
    // Fetch namespace list and daemon status in parallel
    let namespaces = client.list_namespaces().await?;

    let daemon_status = match client.get_daemon_status().await {
        Ok((version, uptime, managed, active)) => Some(DaemonStatusInfo {
            version,
            uptime_secs: uptime,
            managed_namespaces: managed,
            active_namespaces: active,
        }),
        Err(e) => {
            tracing::warn!("Could not fetch daemon status: {}", e);
            None
        }
    };

    output::format_all_namespaces_status(&namespaces, daemon_status.as_ref(), &args.format)
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

    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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
