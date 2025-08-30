use crate::dbus_client::DbusClient;
use crate::output::{self, OutputFormat, ProgressDisplay};
use anyhow::Result;
use clap::Args;

/// Arguments for the reload command
#[derive(Args)]
pub struct ReloadArgs {
    /// Wait for reload to complete before returning
    #[arg(short, long)]
    pub wait: bool,

    /// Timeout in seconds when waiting for reload
    #[arg(long, default_value = "60")]
    pub timeout: u64,

    /// Show detailed progress during reload
    #[arg(short, long)]
    pub verbose: bool,

    /// Validate configurations before reloading
    #[arg(long)]
    pub validate: bool,

    /// Output format
    #[arg(short, long, value_enum, default_value = "table")]
    pub format: OutputFormat,
}

/// Execute the reload command
pub async fn execute(client: DbusClient, args: ReloadArgs) -> Result<()> {
    // Check if daemon is available
    if !client.is_service_available().await {
        eprintln!("Error: segwire daemon is not running or not accessible");
        eprintln!("Please ensure segwire-daemon is started and running");
        std::process::exit(1);
    }

    reload_configuration(&client, &args).await
}

async fn reload_configuration(client: &DbusClient, args: &ReloadArgs) -> Result<()> {
    if args.validate {
        if args.verbose {
            println!("Validating configurations before reload...");
        }
        // TODO: call client.validate_configuration() for each file
    }

    let progress = if args.verbose {
        let p = ProgressDisplay::new("Reload");
        p.update(0.0, "Initiating configuration reload...");
        Some(p)
    } else {
        None
    };

    // Issue the D-Bus reload call
    let result = client.reload_configuration().await?;

    if let Some(ref p) = progress {
        if result.success {
            p.finish("Configuration reloaded");
        } else {
            p.fail(&result.message);
        }
    }

    output::format_operation_result(
        result.success,
        &result.message,
        &result.details,
        &args.format,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reload_args_validation() {
        let args = ReloadArgs {
            wait: true,
            timeout: 120,
            verbose: true,
            validate: false,
            format: OutputFormat::Table,
        };

        assert!(args.wait);
        assert_eq!(args.timeout, 120);
        assert!(args.verbose);
        assert!(!args.validate);
    }
}
