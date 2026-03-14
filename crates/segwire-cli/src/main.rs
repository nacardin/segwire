use anyhow::Result;

fn main() -> Result<()> {
    // Initialize basic logging
    tracing_subscriber::fmt::init();

    segwire_cli::run_cli(std::env::args_os())
}
