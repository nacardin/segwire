use anyhow::Result;
use clap::Args;
use segwire_common::config::NamespaceConfig;
use serde::Serialize;
use std::path::PathBuf;

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

    /// Continue validation even after finding errors
    #[arg(long)]
    pub continue_on_error: bool,
}

#[derive(clap::ValueEnum, Clone)]
pub enum OutputFormat {
    Human,
    Toml,
}

/// Execute the validate command (no D-Bus client needed — all validation is local)
pub fn execute(args: ValidateArgs) -> Result<()> {
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
        total_files = 1;
        let result = validate_single_file(&path, &args);
        if result.has_errors {
            error_count += 1;
        }
        if result.has_warnings {
            warning_count += 1;
        }
        validation_results.push(result);
    } else if path.is_dir() {
        let files = collect_config_files(&path, args.recursive)?;
        total_files = files.len();

        for file in files {
            let result = validate_single_file(&file, &args);
            let has_errors = result.has_errors;
            if result.has_errors {
                error_count += 1;
            }
            if result.has_warnings {
                warning_count += 1;
            }
            validation_results.push(result);

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

    output_validation_results(
        &validation_results,
        &args,
        total_files,
        error_count,
        warning_count,
    )?;

    if error_count > 0 {
        std::process::exit(1);
    }

    Ok(())
}

fn validate_single_file(path: &PathBuf, args: &ValidateArgs) -> ValidationResult {
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

    // Read file content
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            result.errors.push(format!("Cannot read file: {}", e));
            result.has_errors = true;
            return result;
        }
    };

    // Parse TOML syntax
    match toml::from_str::<toml::Value>(&content) {
        Ok(_) => {}
        Err(e) => {
            result.errors.push(format!("TOML syntax error: {}", e));
            result.has_errors = true;
            return result;
        }
    }

    // Semantic validation: parse into NamespaceConfig and call .validate()
    match toml::from_str::<NamespaceConfig>(&content) {
        Ok(config) => {
            if let Err(e) = config.validate() {
                result
                    .errors
                    .push(format!("Semantic validation error: {}", e));
                result.has_errors = true;
            }
        }
        Err(e) => {
            result
                .errors
                .push(format!("Cannot parse as NamespaceConfig: {}", e));
            result.has_errors = true;
        }
    }

    // Additional file-level validations
    if args.warnings || result.has_errors {
        if let Err(e) = validate_file_properties(path, &mut result) {
            result
                .warnings
                .push(format!("Could not check file properties: {}", e));
            result.has_warnings = true;
        }
    }

    result
}

fn validate_file_properties(path: &PathBuf, result: &mut ValidationResult) -> Result<()> {
    let metadata = std::fs::metadata(path)?;

    const MAX_CONFIG_SIZE: u64 = 1024 * 1024; // 1MB
    if metadata.len() > MAX_CONFIG_SIZE {
        result.warnings.push(format!(
            "File is unusually large ({} bytes), consider splitting configuration",
            metadata.len()
        ));
        result.has_warnings = true;
    }

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
        OutputFormat::Toml => output_toml_format(results, total_files, error_count, warning_count),
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

/// Serializable validation output for TOML format
#[derive(Serialize)]
struct TomlValidationOutput {
    summary: TomlValidationSummary,
    results: Vec<TomlValidationResult>,
}

#[derive(Serialize)]
struct TomlValidationSummary {
    total_files: usize,
    error_count: usize,
    warning_count: usize,
    valid: bool,
}

#[derive(Serialize)]
struct TomlValidationResult {
    file: String,
    valid: bool,
    errors: Vec<String>,
    warnings: Vec<String>,
}

fn output_toml_format(
    results: &[ValidationResult],
    total_files: usize,
    error_count: usize,
    warning_count: usize,
) -> Result<()> {
    let output = TomlValidationOutput {
        summary: TomlValidationSummary {
            total_files,
            error_count,
            warning_count,
            valid: error_count == 0,
        },
        results: results
            .iter()
            .map(|r| TomlValidationResult {
                file: r.file_path.to_string_lossy().to_string(),
                valid: !r.has_errors,
                errors: r.errors.clone(),
                warnings: r.warnings.clone(),
            })
            .collect(),
    };

    println!("{}", toml::to_string_pretty(&output)?);
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
            format: OutputFormat::Toml,
            warnings: true,
            continue_on_error: true,
        };

        assert_eq!(args.path, Some(PathBuf::from("/etc/segwire/namespaces")));
        assert!(args.recursive);
        assert!(matches!(args.format, OutputFormat::Toml));
        assert!(args.warnings);
        assert!(args.continue_on_error);
    }
}
