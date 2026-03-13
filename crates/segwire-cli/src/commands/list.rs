use crate::dbus_client::DbusClient;
use crate::output::{self, OutputFormat};
use anyhow::Result;
use clap::Args;

/// Arguments for the list command
#[derive(Args)]
pub struct ListArgs {
    /// Output format
    #[arg(short, long, value_enum, default_value = "table")]
    pub format: OutputFormat,

    /// Show only namespaces with specific status
    #[arg(long, value_enum)]
    pub status: Option<NamespaceStatusFilter>,

    /// Show additional details (full paths, etc.)
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum NamespaceStatusFilter {
    Active,
    Creating,
    Failed,
    Deleting,
}

impl NamespaceStatusFilter {
    fn matches(&self, status: &str) -> bool {
        let lower = status.to_lowercase();
        match self {
            NamespaceStatusFilter::Active => lower == "active",
            NamespaceStatusFilter::Creating => lower == "creating",
            NamespaceStatusFilter::Failed => lower.starts_with("failed"),
            NamespaceStatusFilter::Deleting => lower == "deleting",
        }
    }
}

/// Execute the list command
pub fn execute(client: DbusClient, args: ListArgs) -> Result<()> {
    // Check if daemon is available
    if !client.is_service_available() {
        eprintln!("Error: segwire daemon is not running or not accessible");
        eprintln!("Please ensure segwire-daemon is started and running");
        std::process::exit(1);
    }

    list_namespaces(&client, &args)
}

fn list_namespaces(client: &DbusClient, args: &ListArgs) -> Result<()> {
    // Fetch namespace list from daemon via D-Bus
    let mut namespaces = client.list_namespaces()?;

    // Apply status filter if specified
    if let Some(ref filter) = args.status {
        namespaces.retain(|(_name, status, _config, _desc)| filter.matches(status));
    }

    output::format_namespace_list(&namespaces, &args.format, args.verbose)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_args_parsing() {
        let args = ListArgs {
            format: OutputFormat::Table,
            status: Some(NamespaceStatusFilter::Active),
            verbose: true,
        };

        assert!(matches!(args.format, OutputFormat::Table));
        assert!(matches!(args.status, Some(NamespaceStatusFilter::Active)));
        assert!(args.verbose);
    }

    #[test]
    fn test_status_filter_matches() {
        assert!(NamespaceStatusFilter::Active.matches("active"));
        assert!(NamespaceStatusFilter::Active.matches("Active"));
        assert!(!NamespaceStatusFilter::Active.matches("creating"));

        assert!(NamespaceStatusFilter::Creating.matches("creating"));
        assert!(NamespaceStatusFilter::Failed.matches("failed"));
        assert!(NamespaceStatusFilter::Failed.matches("failed: some error"));
        assert!(NamespaceStatusFilter::Deleting.matches("deleting"));
    }
}
