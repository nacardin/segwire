use anyhow::Result;
use clap::Parser;

mod commands;
mod dbus_client;
pub mod output;

use commands::{Cli, Commands};
use dbus_client::DbusClient;

#[monoio::main]
async fn main() -> Result<()> {
    // Initialize basic logging
    tracing_subscriber::fmt::init();

    // Parse command line arguments
    let cli = Cli::parse();

    // Handle validate command with syntax-only flag specially
    if let Commands::Validate(ref args) = cli.command {
        if args.syntax_only {
            // For syntax-only validation, we don't need a D-Bus connection
            return commands::validate::execute_syntax_only((*args).clone()).await;
        }
    }

    // Create D-Bus client for all other operations
    let client = match DbusClient::new().await {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Failed to connect to segwire daemon: {}", e);
            eprintln!("Make sure segwire-daemon is running and accessible via D-Bus");
            std::process::exit(1);
        }
    };

    // Execute the requested command
    match cli.command {
        Commands::Status(args) => commands::status::execute(client, args).await,
        Commands::List(args) => commands::list::execute(client, args).await,
        Commands::Reload(args) => commands::reload::execute(client, args).await,
        Commands::Restart(args) => commands::restart::execute(client, args).await,
        Commands::Validate(args) => commands::validate::execute(client, args).await,
    }
}
