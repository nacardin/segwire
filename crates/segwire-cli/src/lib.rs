pub mod commands;
pub mod dbus_client;
pub mod output;

use anyhow::Result;
use clap::Parser;
use commands::{Cli, Commands};

/// Run the CLI with the given arguments.
///
/// This is the library entry-point so that integration tests can exercise
/// the full CLI code-path without spawning a child process.
pub fn run_cli<I, T>(args: I) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::parse_from(args);

    // Handle validate command — runs entirely locally, no D-Bus needed
    if let Commands::Validate(args) = cli.command {
        return commands::validate::execute(args);
    }

    // Handle import-wg command — purely local, no D-Bus needed
    if let Commands::ImportWg(args) = cli.command {
        return commands::import_wg::execute(args);
    }

    // Create D-Bus client for all other operations
    let client = match dbus_client::DbusClient::new() {
        Ok(client) => client,
        Err(e) => {
            if let Some(dbus_err) = e.downcast_ref::<dbus::Error>() {
                eprintln!("{}", dbus_client::utils::format_dbus_error(dbus_err));
            } else {
                eprintln!("Failed to connect to segwire daemon: {}", e);
            }
            eprintln!("Make sure segwire-daemon is running and accessible via D-Bus");
            return Err(e);
        }
    };

    // Execute the requested command
    match cli.command {
        Commands::Status(args) => commands::status::execute(client, args),
        Commands::List(args) => commands::list::execute(client, args),
        Commands::Reload(args) => commands::reload::execute(client, args),
        Commands::Restart(args) => commands::restart::execute(client, args),
        Commands::Validate(_) => unreachable!("handled above"),
        Commands::ImportWg(_) => unreachable!("handled above"),
        Commands::Exec(args) => commands::exec::execute(client, args),
    }
}
