//! segwire-ns-enter — Setuid helper for entering network namespaces
//!
//! This binary is installed setuid-root. It performs exactly one operation:
//! enter a network namespace and exec a command as the calling user.
//!
//! Usage: segwire-ns-enter <ns-path> -- <command> [args...]
//!
//! Security model:
//! 1. Validate that ns-path is under /run/netns/ (prevent arbitrary fd abuse)
//! 2. Open the namespace file (requires root, which setuid provides)
//! 3. setns(fd, CLONE_NEWNET) — enter the network namespace
//! 4. Permanently drop ALL privileges back to the real UID/GID
//! 5. Set PR_SET_NO_NEW_PRIVS to prevent further escalation
//! 6. execvp() the user's command — this process becomes the command
//!
//! The binary is intentionally minimal: no logging, no D-Bus, no config
//! parsing, no allocations after privilege drop. This minimizes attack surface.

use nix::fcntl::OFlag;
use nix::sched::{setns, CloneFlags};
use nix::sys::stat::Mode;
use nix::unistd::{setresgid, setresuid};
use std::ffi::CString;
use std::os::fd::BorrowedFd;

/// Allowed prefix for namespace paths. Prevents opening arbitrary files.
const ALLOWED_NS_PREFIX: &str = "/run/netns/";

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Parse: segwire-ns-enter <ns-path> -- <command> [args...]
    let (ns_path, cmd, cmd_args) = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("segwire-ns-enter: {}", msg);
            eprintln!("Usage: segwire-ns-enter <ns-path> -- <command> [args...]");
            std::process::exit(1);
        }
    };

    // Validate the namespace path
    if let Err(msg) = validate_ns_path(&ns_path) {
        eprintln!("segwire-ns-enter: {}", msg);
        std::process::exit(1);
    }

    // Open the namespace file (we have euid=0 from setuid)
    let ns_fd = match nix::fcntl::open(
        ns_path.as_str(),
        OFlag::O_RDONLY | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("segwire-ns-enter: failed to open '{}': {}", ns_path, e);
            std::process::exit(1);
        }
    };

    // Enter the network namespace
    // SAFETY: ns_fd was just opened and is valid for this scope
    if let Err(e) = setns(unsafe { BorrowedFd::borrow_raw(ns_fd) }, CloneFlags::CLONE_NEWNET) {
        eprintln!("segwire-ns-enter: setns failed: {}", e);
        std::process::exit(1);
    }
    let _ = nix::unistd::close(ns_fd);

    // Permanently drop ALL privileges back to the real UID/GID.
    // setresuid/setresgid set real, effective, AND saved-set IDs,
    // making the privilege drop irrecoverable.
    let real_gid = nix::unistd::getgid();
    let real_uid = nix::unistd::getuid();

    if let Err(e) = setresgid(real_gid, real_gid, real_gid) {
        eprintln!("segwire-ns-enter: setresgid failed: {}", e);
        std::process::exit(1);
    }
    if let Err(e) = setresuid(real_uid, real_uid, real_uid) {
        eprintln!("segwire-ns-enter: setresuid failed: {}", e);
        std::process::exit(1);
    }

    // Prevent the exec'd process from gaining any new privileges
    // (e.g., via setuid binaries or file capabilities).
    #[cfg(target_os = "linux")]
    {
        // prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)
        let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if ret != 0 {
            eprintln!("segwire-ns-enter: PR_SET_NO_NEW_PRIVS failed");
            std::process::exit(1);
        }
    }

    // Become the target command.
    // After this point, this process IS the user's command.
    let c_cmd = CString::new(cmd.as_str()).unwrap_or_else(|_| {
        eprintln!("segwire-ns-enter: invalid command name");
        std::process::exit(1);
    });

    let c_args: Vec<CString> = std::iter::once(c_cmd.clone())
        .chain(cmd_args.iter().map(|a| {
            CString::new(a.as_str()).unwrap_or_else(|_| {
                eprintln!("segwire-ns-enter: invalid argument");
                std::process::exit(1);
            })
        }))
        .collect();

    // execvp replaces this process — it never returns on success
    let err = nix::unistd::execvp(&c_cmd, &c_args);
    eprintln!("segwire-ns-enter: execvp '{}' failed: {}", cmd, err.unwrap_err());
    std::process::exit(127);
}

/// Parse command-line arguments.
/// Expected: segwire-ns-enter <ns-path> -- <command> [args...]
fn parse_args(args: &[String]) -> Result<(String, String, Vec<String>), String> {
    if args.len() < 4 {
        return Err("not enough arguments".to_string());
    }

    let ns_path = &args[1];

    // Find the "--" separator
    let separator_pos = args.iter().position(|a| a == "--")
        .ok_or_else(|| "missing '--' separator between namespace path and command".to_string())?;

    if separator_pos != 2 {
        return Err("expected: <ns-path> -- <command> [args...]".to_string());
    }

    if separator_pos + 1 >= args.len() {
        return Err("no command specified after '--'".to_string());
    }

    let cmd = args[separator_pos + 1].clone();
    let cmd_args = args[separator_pos + 2..].to_vec();

    Ok((ns_path.clone(), cmd, cmd_args))
}

/// Validate that the namespace path is safe to open.
fn validate_ns_path(path: &str) -> Result<(), String> {
    // Must be an absolute path under /run/netns/
    if !path.starts_with(ALLOWED_NS_PREFIX) {
        return Err(format!(
            "namespace path must start with '{}', got '{}'",
            ALLOWED_NS_PREFIX, path
        ));
    }

    // The namespace name portion must not contain path separators
    // (prevents /run/netns/../../etc/shadow)
    let ns_name = &path[ALLOWED_NS_PREFIX.len()..];
    if ns_name.is_empty() {
        return Err("namespace name is empty".to_string());
    }
    if ns_name.contains('/') || ns_name.contains("..") {
        return Err(format!("invalid namespace name: '{}'", ns_name));
    }

    // Must not contain null bytes
    if path.contains('\0') {
        return Err("path contains null byte".to_string());
    }

    // Verify the path exists and is not a symlink
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("cannot stat '{}': {}", path, e))?;

    if metadata.file_type().is_symlink() {
        return Err(format!("'{}' is a symlink, which is not allowed", path));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_args_valid() {
        let args = vec![
            "segwire-ns-enter".to_string(),
            "/run/netns/test".to_string(),
            "--".to_string(),
            "ip".to_string(),
            "link".to_string(),
            "show".to_string(),
        ];
        let (ns, cmd, cmd_args) = parse_args(&args).unwrap();
        assert_eq!(ns, "/run/netns/test");
        assert_eq!(cmd, "ip");
        assert_eq!(cmd_args, vec!["link", "show"]);
    }

    #[test]
    fn test_parse_args_no_separator() {
        let args = vec![
            "segwire-ns-enter".to_string(),
            "/run/netns/test".to_string(),
            "ip".to_string(),
        ];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn test_parse_args_no_command() {
        let args = vec![
            "segwire-ns-enter".to_string(),
            "/run/netns/test".to_string(),
            "--".to_string(),
        ];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn test_parse_args_too_few() {
        let args = vec!["segwire-ns-enter".to_string()];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn test_validate_ns_path_valid_prefix() {
        // Can't fully test without a real namespace, but we can test prefix validation
        // by checking that invalid prefixes are rejected
        assert!(validate_ns_path("/etc/passwd").is_err());
        assert!(validate_ns_path("/tmp/test").is_err());
        assert!(validate_ns_path("relative/path").is_err());
    }

    #[test]
    fn test_validate_ns_path_traversal() {
        assert!(validate_ns_path("/run/netns/../../etc/shadow").is_err());
        assert!(validate_ns_path("/run/netns/foo/bar").is_err());
    }

    #[test]
    fn test_validate_ns_path_empty_name() {
        assert!(validate_ns_path("/run/netns/").is_err());
    }
}
