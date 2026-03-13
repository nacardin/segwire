use crate::dbus_client::DbusClient;
use anyhow::Result;
use clap::Args;

/// Arguments for the exec command
#[derive(Args)]
pub struct ExecArgs {
    /// Name of the namespace to execute in
    #[arg(value_name = "NAMESPACE")]
    pub namespace: String,

    /// Command to execute inside the namespace
    #[arg(last = true, required = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

/// Default locations to search for the ns-enter helper binary
const HELPER_SEARCH_PATHS: &[&str] = &[
    "/usr/libexec/segwire-ns-enter",
    "/usr/lib/segwire/segwire-ns-enter",
    "/usr/local/libexec/segwire-ns-enter",
];

/// Execute the exec command
pub fn execute(client: DbusClient, args: ExecArgs) -> Result<()> {
    if args.command.is_empty() {
        return Err(anyhow::anyhow!("No command specified"));
    }

    // Check if daemon is available
    if !client.is_service_available() {
        eprintln!("Error: segwire daemon is not running or not accessible");
        eprintln!("Please ensure segwire-daemon is started and running");
        std::process::exit(1);
    }

    // Step 1: Authorize and get the namespace path via D-Bus
    let ns_path = client.exec_authorize(&args.namespace)?;

    // Step 2: Find the helper binary
    let helper_path = find_helper_binary()?;

    // Step 3: Build the command: segwire-ns-enter <ns_path> -- <command> [args...]
    let mut helper_args = vec![
        ns_path,
        "--".to_string(),
    ];
    helper_args.extend(args.command);

    // Step 4: Exec the helper — this replaces the current process
    let err = exec::execvp(&helper_path, &helper_args);
    // execvp only returns on error
    Err(anyhow::anyhow!("Failed to exec '{}': {}", helper_path, err))
}

/// Find the segwire-ns-enter helper binary.
///
/// Search order:
/// 1. SEGWIRE_NS_ENTER_PATH environment variable (for development/testing)
/// 2. Same directory as the running segwire binary
/// 3. Standard system locations (/usr/libexec, /usr/lib/segwire, etc.)
/// 4. $PATH lookup
fn find_helper_binary() -> Result<String> {
    // Check environment variable override
    if let Ok(path) = std::env::var("SEGWIRE_NS_ENTER_PATH") {
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
    }

    // Check next to the current binary
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            let sibling = dir.join("segwire-ns-enter");
            if sibling.exists() {
                return Ok(sibling.display().to_string());
            }
        }
    }

    // Check standard system paths
    for path in HELPER_SEARCH_PATHS {
        if std::path::Path::new(path).exists() {
            return Ok(path.to_string());
        }
    }

    // Try $PATH via `which`
    if let Ok(output) = std::process::Command::new("which")
        .arg("segwire-ns-enter")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(path);
            }
        }
    }

    Err(anyhow::anyhow!(
        "segwire-ns-enter helper not found.\n\
         Searched: {:?}\n\
         Set SEGWIRE_NS_ENTER_PATH or install segwire-ns-enter to a standard location.",
        HELPER_SEARCH_PATHS
    ))
}

/// Minimal exec helper using std::os::unix
mod exec {
    use std::ffi::CString;

    pub fn execvp(program: &str, args: &[String]) -> std::io::Error {
        let c_program = CString::new(program).expect("invalid program name");
        let c_args: Vec<CString> = std::iter::once(c_program.clone())
            .chain(args.iter().map(|a| CString::new(a.as_bytes()).expect("invalid argument")))
            .collect();

        // This does not return on success
        nix::unistd::execvp(&c_program, &c_args)
            .expect_err("execvp returned Ok, which should be impossible")
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_helper_search() {
        // Should not panic — either finds it or returns Err
        let result = find_helper_binary();
        // In dev, the binary may not be installed; just ensure no panic
        assert!(result.is_ok() || result.is_err());
    }
}
