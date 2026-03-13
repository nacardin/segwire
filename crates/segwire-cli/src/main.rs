use anyhow::Result;
use clap::Parser;

use segwire_cli::commands::{self, Cli, Commands};
use segwire_cli::dbus_client::{self, DbusClient};

fn main() -> Result<()> {
    // Initialize basic logging
    tracing_subscriber::fmt::init();

    // Parse command line arguments
    let cli = Cli::parse();

    // Handle validate command — runs entirely locally, no D-Bus needed
    if let Commands::Validate(args) = cli.command {
        return commands::validate::execute(args);
    }

    // Create D-Bus client for all other operations
    let client = match DbusClient::new() {
        Ok(client) => client,
        Err(e) => {
            if let Some(dbus_err) = e.downcast_ref::<dbus::Error>() {
                eprintln!("{}", dbus_client::utils::format_dbus_error(dbus_err));
            } else {
                eprintln!("Failed to connect to segwire daemon: {}", e);
            }
            eprintln!("Make sure segwire-daemon is running and accessible via D-Bus");
            std::process::exit(1);
        }
    };

    // Execute the requested command
    match cli.command {
        Commands::Status(args) => commands::status::execute(client, args),
        Commands::List(args) => commands::list::execute(client, args),
        Commands::Reload(args) => commands::reload::execute(client, args),
        Commands::Restart(args) => commands::restart::execute(client, args),
        Commands::Validate(_) => unreachable!("handled above"),
        Commands::Exec(args) => commands::exec::execute(client, args),
    }
}
