use clap::{Parser, Subcommand};

pub mod exec;
pub mod import_wg;
pub mod list;
pub mod reload;
pub mod restart;
pub mod status;
pub mod validate;

/// Segwire CLI - Network namespace management tool
#[derive(Parser)]
#[command(name = "segwire")]
#[command(about = "A command-line interface for managing Linux network namespaces")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(author = "Segwire Project")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Display detailed status information about namespaces
    Status(status::StatusArgs),

    /// List all managed namespaces with summary information
    List(list::ListArgs),

    /// Reload daemon configuration files
    Reload(reload::ReloadArgs),

    /// Restart (recreate) an existing namespace
    Restart(restart::RestartArgs),

    /// Validate configuration files without applying them
    Validate(validate::ValidateArgs),

    /// Execute a command inside a network namespace
    Exec(exec::ExecArgs),

    /// Import a WireGuard (wg-quick) config into a segwire namespace
    ImportWg(import_wg::ImportWgArgs),
}
