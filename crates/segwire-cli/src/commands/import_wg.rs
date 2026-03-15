use anyhow::Result;
use clap::Args;
use segwire_common::wireguard::parse_wg_quick;
use std::path::PathBuf;

/// Arguments for the import-wg command
#[derive(Args, Clone)]
pub struct ImportWgArgs {
    /// Name for the new namespace
    #[arg(value_name = "NAMESPACE")]
    pub namespace: String,

    /// Path to the wg-quick configuration file (e.g., wg0.conf)
    #[arg(value_name = "WG_CONF")]
    pub config_path: PathBuf,

    /// Custom WireGuard interface name (default: wg0)
    #[arg(short = 'i', long = "interface", value_name = "IFNAME")]
    pub interface: Option<String>,

    /// Output directory for the generated TOML file
    /// (default: /etc/segwire/namespaces or stdout with --stdout)
    #[arg(short, long, value_name = "DIR")]
    pub output: Option<PathBuf>,

    /// Print the generated TOML to stdout instead of writing a file
    #[arg(long)]
    pub stdout: bool,
}

/// Execute the import-wg command (local — no D-Bus needed)
pub fn execute(args: ImportWgArgs) -> Result<()> {
    // Read the wg-quick config file
    let content = std::fs::read_to_string(&args.config_path).map_err(|e| {
        anyhow::anyhow!(
            "Cannot read WireGuard config '{}': {}",
            args.config_path.display(),
            e
        )
    })?;

    // Parse into NamespaceConfig
    let ns_config = parse_wg_quick(
        &content,
        &args.namespace,
        args.interface.as_deref(),
    )
    .map_err(|e| anyhow::anyhow!("Failed to parse WireGuard config: {}", e))?;

    // Validate the generated config
    ns_config
        .validate()
        .map_err(|e| anyhow::anyhow!("Generated config fails validation: {}", e))?;

    // Serialize to TOML
    let toml_output = toml::to_string_pretty(&ns_config)
        .map_err(|e| anyhow::anyhow!("Failed to serialize config to TOML: {}", e))?;

    if args.stdout {
        println!("{}", toml_output);
        return Ok(());
    }

    // Determine output path
    let output_dir = args.output.unwrap_or_else(|| {
        if std::path::Path::new("/etc/segwire/namespaces").exists() {
            PathBuf::from("/etc/segwire/namespaces")
        } else {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(format!("{}/.config/segwire/namespaces", home))
        }
    });

    // Create output directory if needed
    std::fs::create_dir_all(&output_dir).map_err(|e| {
        anyhow::anyhow!(
            "Cannot create output directory '{}': {}",
            output_dir.display(),
            e
        )
    })?;

    let output_file = output_dir.join(format!("{}.toml", args.namespace));

    if output_file.exists() {
        return Err(anyhow::anyhow!(
            "Output file already exists: {}. Use --stdout to preview or remove the existing file.",
            output_file.display()
        ));
    }

    std::fs::write(&output_file, &toml_output).map_err(|e| {
        anyhow::anyhow!(
            "Cannot write output file '{}': {}",
            output_file.display(),
            e
        )
    })?;

    println!("✅ Imported WireGuard config into: {}", output_file.display());
    println!("   Namespace: {}", args.namespace);
    println!(
        "   Interface: {}",
        args.interface.as_deref().unwrap_or("wg0")
    );
    println!(
        "   Peers: {}",
        ns_config.wireguard.as_ref().map_or(0, |wg| wg.peers.len())
    );
    println!();
    println!("Run 'segwire reload' to apply the new configuration.");

    Ok(())
}
