//! Capability checking and privilege verification for the daemon
//!
//! Implements CAP_SYS_ADMIN capability verification, runtime privilege checking,
//! and container environment detection. This ensures the daemon has appropriate
//! privileges to manage network namespaces.

use segwire_common::error::SegwireError;
use std::path::Path;
use tracing::{debug, info, warn};

/// Required Linux capabilities for namespace management
const REQUIRED_CAPABILITIES: &[&str] = &["cap_sys_admin"];

/// Result of a capability check.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CapabilityCheckResult {
    pub has_required_capabilities: bool,
    pub is_root: bool,
    pub is_container: bool,
    pub container_runtime: Option<String>,
    pub missing_capabilities: Vec<String>,
    pub warnings: Vec<String>,
}

impl std::fmt::Display for CapabilityCheckResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.has_required_capabilities {
            write!(f, "Privilege check passed")?;
        } else {
            write!(
                f,
                "Privilege check FAILED – missing: {}",
                self.missing_capabilities.join(", ")
            )?;
        }
        if self.is_container {
            write!(
                f,
                " (container: {})",
                self.container_runtime.as_deref().unwrap_or("unknown")
            )?;
        }
        Ok(())
    }
}

// ──────────────────────────────────────────────
// Public API
// ──────────────────────────────────────────────

/// Perform a full privilege check, returning an error if the daemon cannot
/// manage network namespaces.
///
/// This should be called early in daemon startup, before any privileged
/// operations are attempted.
pub fn verify_privileges() -> Result<CapabilityCheckResult, SegwireError> {
    let result = check_capabilities();

    info!("{}", result);

    for w in &result.warnings {
        warn!("{}", w);
    }

    if !result.has_required_capabilities {
        let msg = format!(
            "Daemon requires the following capabilities: {}. \
             Missing: {}. \
             Either run as root or grant capabilities via systemd \
             (AmbientCapabilities=CAP_SYS_ADMIN).",
            REQUIRED_CAPABILITIES.join(", "),
            result.missing_capabilities.join(", "),
        );
        return Err(SegwireError::Permission(msg));
    }

    Ok(result)
}

/// Lightweight check that simply returns the result without erroring.
pub fn check_capabilities() -> CapabilityCheckResult {
    let is_root = check_effective_root();
    let (is_container, container_runtime) = detect_container_environment();

    let mut missing = Vec::new();
    let mut warnings = Vec::new();

    // If running as root, all capabilities are available.
    let has_caps = if is_root {
        debug!("Running as root – all capabilities available");
        true
    } else {
        // Parse the effective capability set from /proc/self/status
        let effective = read_effective_capabilities();
        let has = has_cap_sys_admin(&effective);
        if !has {
            missing.push("CAP_SYS_ADMIN".to_string());
        }
        has
    };

    // Container-specific warnings
    if is_container {
        warnings.push(format!(
            "Running inside a container ({}). Ensure the container has been \
             started with --cap-add=SYS_ADMIN or an equivalent setting.",
            container_runtime.as_deref().unwrap_or("unknown")
        ));

        // Check if /var/run/netns is available in the container
        if !Path::new("/var/run/netns").exists() {
            warnings.push(
                "/var/run/netns does not exist – network namespace \
                 persistence may not work inside this container."
                    .to_string(),
            );
        }
    }

    // Check if seccomp might block namespace syscalls
    if let Some(seccomp_warning) = check_seccomp_status() {
        warnings.push(seccomp_warning);
    }

    CapabilityCheckResult {
        has_required_capabilities: has_caps,
        is_root,
        is_container,
        container_runtime,
        missing_capabilities: missing,
        warnings,
    }
}

// ──────────────────────────────────────────────
// Internal helpers
// ──────────────────────────────────────────────

/// Check if the process is running as the effective root user.
fn check_effective_root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

/// Read the effective capability bitmask from /proc/self/status.
/// Returns the hex string value of `CapEff`, e.g. `"0000003fffffffff"`.
fn read_effective_capabilities() -> String {
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(e) => {
            debug!("Cannot read /proc/self/status: {}", e);
            return String::new();
        }
    };

    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("CapEff:") {
            return rest.trim().to_string();
        }
    }

    String::new()
}

/// Check whether the `CapEff` hex string includes CAP_SYS_ADMIN (bit 21).
fn has_cap_sys_admin(cap_eff_hex: &str) -> bool {
    if cap_eff_hex.is_empty() {
        return false;
    }
    match u64::from_str_radix(cap_eff_hex, 16) {
        Ok(bits) => {
            let sys_admin_bit = 1u64 << 21; // CAP_SYS_ADMIN = 21
            bits & sys_admin_bit != 0
        }
        Err(e) => {
            debug!("Failed to parse CapEff '{}': {}", cap_eff_hex, e);
            false
        }
    }
}

/// Detect if we are running inside a container and, if so, which runtime.
fn detect_container_environment() -> (bool, Option<String>) {
    // 1. Check /.dockerenv
    if Path::new("/.dockerenv").exists() {
        return (true, Some("Docker".to_string()));
    }

    // 2. Check /run/.containerenv (Podman)
    if Path::new("/run/.containerenv").exists() {
        return (true, Some("Podman".to_string()));
    }

    // 3. Check cgroup for docker / lxc / kubepods
    if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
        if cgroup.contains("docker") {
            return (true, Some("Docker".to_string()));
        }
        if cgroup.contains("lxc") {
            return (true, Some("LXC".to_string()));
        }
        if cgroup.contains("kubepods") {
            return (true, Some("Kubernetes".to_string()));
        }
    }

    // 4. Check for systemd-nspawn
    if std::env::var("container").as_deref() == Ok("systemd-nspawn") {
        return (true, Some("systemd-nspawn".to_string()));
    }

    // 5. Generic $container variable
    if std::env::var("container").is_ok() {
        return (true, Some("unknown".to_string()));
    }

    (false, None)
}

/// Check if seccomp may be blocking namespace-related syscalls.
fn check_seccomp_status() -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Seccomp:") {
            let mode: u32 = rest.trim().parse().unwrap_or(0);
            // 0 = disabled, 1 = strict, 2 = filter
            if mode == 2 {
                return Some(
                    "Seccomp filter is active – if namespace syscalls fail, \
                     check that unshare(2) and setns(2) are allowed."
                        .to_string(),
                );
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_cap_sys_admin_full_caps() {
        // All capabilities set
        assert!(has_cap_sys_admin("0000003fffffffff"));
    }

    #[test]
    fn test_has_cap_sys_admin_exact() {
        // Only CAP_SYS_ADMIN set (bit 21)
        let hex = format!("{:016x}", 1u64 << 21);
        assert!(has_cap_sys_admin(&hex));
    }

    #[test]
    fn test_has_cap_sys_admin_missing() {
        // No capabilities set
        assert!(!has_cap_sys_admin("0000000000000000"));
    }

    #[test]
    fn test_has_cap_sys_admin_empty() {
        assert!(!has_cap_sys_admin(""));
    }

    #[test]
    fn test_has_cap_sys_admin_bad_hex() {
        assert!(!has_cap_sys_admin("not_hex"));
    }

    #[test]
    fn test_check_capabilities_runs() {
        // Just ensure it doesn't panic
        let result = check_capabilities();
        // We can't assert much about the result since it depends on the
        // test runner's environment, but the struct should be valid.
        assert!(!result.missing_capabilities.contains(&"".to_string()));
    }

    #[test]
    fn test_capability_check_result_display() {
        let result = CapabilityCheckResult {
            has_required_capabilities: false,
            is_root: false,
            is_container: true,
            container_runtime: Some("Docker".to_string()),
            missing_capabilities: vec!["CAP_SYS_ADMIN".to_string()],
            warnings: vec![],
        };
        let display = format!("{}", result);
        assert!(display.contains("FAILED"));
        assert!(display.contains("CAP_SYS_ADMIN"));
        assert!(display.contains("Docker"));
    }
}
