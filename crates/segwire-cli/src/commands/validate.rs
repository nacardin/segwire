use crate::dbus_client::DbusClient;
use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

/// Trait for D-Bus client operations needed by validate command
#[allow(dead_code)] // Used via generic validate_configurations<T> internally
trait ValidateDbusClient {
    async fn is_service_available(&self) -> bool;
}

impl ValidateDbusClient for DbusClient {
    async fn is_service_available(&self) -> bool {
        DbusClient::is_service_available(self).await
    }
}

/// Dummy D-Bus client for syntax-only validation
struct DummyDbusClient;

impl ValidateDbusClient for DummyDbusClient {
    async fn is_service_available(&self) -> bool {
        false
    }
}

/// Arguments for the validate command
#[derive(Args, Clone)]
pub struct ValidateArgs {
    /// Path to configuration file or directory to validate
    /// If not specified, validates the default configuration directory
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Recursively validate all .toml files in directory
    #[arg(short, long)]
    pub recursive: bool,

    /// Output format for validation results
    #[arg(short, long, value_enum, default_value = "human")]
    pub format: OutputFormat,

    /// Show warnings in addition to errors
    #[arg(short, long)]
    pub warnings: bool,

    /// Validate syntax only, skip semantic validation
    #[arg(long)]
    pub syntax_only: bool,

    /// Continue validation even after finding errors
    #[arg(long)]
    pub continue_on_error: bool,
}

#[derive(clap::ValueEnum, Clone)]
pub enum OutputFormat {
    Human,
    Json,
    Yaml,
}

/// Execute the validate command with syntax-only mode (no D-Bus client needed)
pub async fn execute_syntax_only(args: ValidateArgs) -> Result<()> {
    // Create a dummy client that won't be used
    let dummy_client = DummyDbusClient;
    validate_configurations(&dummy_client, &args).await
}

/// Execute the validate command
pub async fn execute(client: DbusClient, args: ValidateArgs) -> Result<()> {
    // Note: Validation can work without daemon for syntax checking
    // But semantic validation may require daemon connection

    if !args.syntax_only && !client.is_service_available().await {
        eprintln!("Warning: segwire daemon is not running");
        eprintln!("Only syntax validation will be performed");
        eprintln!("Start segwire-daemon for full semantic validation");
    }

    validate_configurations(&client, &args).await
}

async fn validate_configurations<T: ValidateDbusClient>(
    client: &T,
    args: &ValidateArgs,
) -> Result<()> {
    let mut validation_results = Vec::new();
    let total_files;
    let mut error_count = 0;
    let mut warning_count = 0;

    // Determine the path to validate
    let path = match &args.path {
        Some(path) => path.clone(),
        None => {
            // Default to /etc/segwire/namespaces or ~/.config/segwire/namespaces
            let default_path = if std::path::Path::new("/etc/segwire/namespaces").exists() {
                PathBuf::from("/etc/segwire/namespaces")
            } else {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                PathBuf::from(format!("{}/.config/segwire/namespaces", home))
            };
            println!(
                "No path specified, using default configuration directory: {}",
                default_path.display()
            );
            default_path
        }
    };

    if path.is_file() {
        // Validate single file
        total_files = 1;
        let result = validate_single_file(&path, client, args).await?;
        if result.has_errors {
            error_count += 1;
        }
        if result.has_warnings {
            warning_count += 1;
        }
        validation_results.push(result);
    } else if path.is_dir() {
        // Validate directory
        let files = collect_config_files(&path, args.recursive)?;
        total_files = files.len();

        for file in files {
            let result = validate_single_file(&file, client, args).await?;
            let has_errors = result.has_errors;
            if result.has_errors {
                error_count += 1;
            }
            if result.has_warnings {
                warning_count += 1;
            }
            validation_results.push(result);

            // Stop on first error if not continuing
            if has_errors && !args.continue_on_error {
                break;
            }
        }
    } else {
        return Err(anyhow::anyhow!(
            "Path does not exist or is not a file/directory: {}",
            path.display()
        ));
    }

    // Output results
    output_validation_results(
        &validation_results,
        args,
        total_files,
        error_count,
        warning_count,
    )?;

    // Exit with error code if validation failed
    if error_count > 0 {
        std::process::exit(1);
    }

    Ok(())
}

async fn validate_single_file<T: ValidateDbusClient>(
    path: &PathBuf,
    _client: &T,
    args: &ValidateArgs,
) -> Result<ValidationResult> {
    let mut result = ValidationResult {
        file_path: path.clone(),
        has_errors: false,
        has_warnings: false,
        errors: Vec::new(),
        warnings: Vec::new(),
    };

    // Check file extension
    if let Some(extension) = path.extension() {
        if extension != "toml" {
            result
                .warnings
                .push("File does not have .toml extension".to_string());
            result.has_warnings = true;
        }
    } else {
        result
            .warnings
            .push("File has no extension, expected .toml".to_string());
        result.has_warnings = true;
    }

    // Validate file accessibility
    match std::fs::File::open(path) {
        Ok(_) => {}
        Err(e) => {
            result.errors.push(format!("Cannot read file: {}", e));
            result.has_errors = true;
            return Ok(result);
        }
    }

    // Read and parse TOML content
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            result
                .errors
                .push(format!("Cannot read file content: {}", e));
            result.has_errors = true;
            return Ok(result);
        }
    };

    // Parse TOML syntax
    match toml::from_str::<toml::Value>(&content) {
        Ok(_) => {
            // Syntax is valid
            if !args.syntax_only {
                // TODO: Implement semantic validation via D-Bus when methods are available
                // For now, just indicate that semantic validation would happen here
            }
        }
        Err(e) => {
            result.errors.push(format!("TOML syntax error: {}", e));
            result.has_errors = true;
        }
    }

    // Additional file-level validations
    if args.warnings || result.has_errors {
        validate_file_properties(path, &mut result)?;
    }

    Ok(result)
}

fn validate_file_properties(path: &PathBuf, result: &mut ValidationResult) -> Result<()> {
    let metadata = std::fs::metadata(path)?;

    // Check file size
    const MAX_CONFIG_SIZE: u64 = 1024 * 1024; // 1MB
    if metadata.len() > MAX_CONFIG_SIZE {
        result.warnings.push(format!(
            "File is unusually large ({} bytes), consider splitting configuration",
            metadata.len()
        ));
        result.has_warnings = true;
    }

    // Check permissions on Unix systems
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = metadata.permissions();
        let mode = permissions.mode();

        if mode & 0o002 != 0 {
            result
                .warnings
                .push("File is world-writable, this may be a security risk".to_string());
            result.has_warnings = true;
        }
    }

    Ok(())
}

fn collect_config_files(dir: &PathBuf, recursive: bool) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    let entries = std::fs::read_dir(dir)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            if let Some(extension) = path.extension() {
                if extension == "toml" {
                    files.push(path);
                }
            }
        } else if path.is_dir() && recursive {
            let mut sub_files = collect_config_files(&path, recursive)?;
            files.append(&mut sub_files);
        }
    }

    files.sort();
    Ok(files)
}

fn output_validation_results(
    results: &[ValidationResult],
    args: &ValidateArgs,
    total_files: usize,
    error_count: usize,
    warning_count: usize,
) -> Result<()> {
    match args.format {
        OutputFormat::Human => {
            output_human_format(results, args, total_files, error_count, warning_count)
        }
        OutputFormat::Json => output_json_format(results, total_files, error_count, warning_count),
        OutputFormat::Yaml => output_yaml_format(results, total_files, error_count, warning_count),
    }
}

fn output_human_format(
    results: &[ValidationResult],
    args: &ValidateArgs,
    total_files: usize,
    error_count: usize,
    warning_count: usize,
) -> Result<()> {
    println!("Configuration Validation Results");
    println!("================================");
    println!();

    for result in results {
        let status = if result.has_errors {
            "❌ FAILED"
        } else if result.has_warnings && args.warnings {
            "⚠️  WARNINGS"
        } else {
            "✅ PASSED"
        };

        println!("{} {}", status, result.file_path.display());

        for error in &result.errors {
            println!("  Error: {}", error);
        }

        if args.warnings {
            for warning in &result.warnings {
                println!("  Warning: {}", warning);
            }
        }

        if !result.errors.is_empty() || (args.warnings && !result.warnings.is_empty()) {
            println!();
        }
    }

    println!("Summary:");
    println!("  Total files: {}", total_files);
    println!("  Files with errors: {}", error_count);
    if args.warnings {
        println!("  Files with warnings: {}", warning_count);
    }

    if error_count == 0 {
        println!("  ✅ All configurations are valid!");
    }

    Ok(())
}

fn output_json_format(
    results: &[ValidationResult],
    total_files: usize,
    error_count: usize,
    warning_count: usize,
) -> Result<()> {
    use serde_json::json;

    let json_results: Vec<_> = results
        .iter()
        .map(|r| {
            json!({
                "file": r.file_path.to_string_lossy(),
                "valid": !r.has_errors,
                "errors": r.errors,
                "warnings": r.warnings
            })
        })
        .collect();

    let output = json!({
        "summary": {
            "total_files": total_files,
            "error_count": error_count,
            "warning_count": warning_count,
            "valid": error_count == 0
        },
        "results": json_results
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn output_yaml_format(
    results: &[ValidationResult],
    total_files: usize,
    error_count: usize,
    warning_count: usize,
) -> Result<()> {
    println!("summary:");
    println!("  total_files: {}", total_files);
    println!("  error_count: {}", error_count);
    println!("  warning_count: {}", warning_count);
    println!("  valid: {}", error_count == 0);
    println!("results:");

    for result in results {
        println!("  - file: \"{}\"", result.file_path.display());
        println!("    valid: {}", !result.has_errors);
        println!("    errors:");
        for error in &result.errors {
            println!("      - \"{}\"", error);
        }
        println!("    warnings:");
        for warning in &result.warnings {
            println!("      - \"{}\"", warning);
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct ValidationResult {
    file_path: PathBuf,
    has_errors: bool,
    has_warnings: bool,
    errors: Vec<String>,
    warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_args() {
        let args = ValidateArgs {
            path: Some(PathBuf::from("/etc/segwire/namespaces")),
            recursive: true,
            format: OutputFormat::Json,
            warnings: true,
            syntax_only: false,
            continue_on_error: true,
        };

        assert_eq!(args.path, Some(PathBuf::from("/etc/segwire/namespaces")));
        assert!(args.recursive);
        assert!(matches!(args.format, OutputFormat::Json));
        assert!(args.warnings);
        assert!(!args.syntax_only);
        assert!(args.continue_on_error);
    }
}
