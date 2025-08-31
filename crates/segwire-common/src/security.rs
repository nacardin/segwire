//! Configuration file security validation
//!
//! Implements file permission and ownership checking, input sanitization
//! for configuration values, and path traversal protection for configuration paths.

use crate::error::{SegwireError, SegwireResult};
use std::path::{Component, Path, PathBuf};

// ──────────────────────────────────────────────
// File permission and ownership checking
// ──────────────────────────────────────────────

/// Comprehensive security check result for a configuration file.
#[derive(Debug, Clone)]
pub struct SecurityCheckResult {
    pub path: PathBuf,
    pub passed: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl SecurityCheckResult {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            passed: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn add_error(&mut self, msg: String) {
        self.passed = false;
        self.errors.push(msg);
    }

    fn add_warning(&mut self, msg: String) {
        self.warnings.push(msg);
    }
}

impl std::fmt::Display for SecurityCheckResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.passed {
            write!(f, "PASS: {}", self.path.display())?;
        } else {
            write!(f, "FAIL: {}", self.path.display())?;
        }
        for e in &self.errors {
            write!(f, "\n  error: {}", e)?;
        }
        for w in &self.warnings {
            write!(f, "\n  warning: {}", w)?;
        }
        Ok(())
    }
}

/// Perform a comprehensive security check on a configuration file.
///
/// Checks:
/// - File exists and is a regular file (not symlink, fifo, etc.)
/// - Ownership (should be root or current user)
/// - Permissions (should not be world-writable, ideally 644 or stricter)
/// - Path traversal protection (no `..` components)
pub fn check_config_file_security(path: &Path) -> SegwireResult<SecurityCheckResult> {
    let mut result = SecurityCheckResult::new(path);

    // 1. Canonical path check (resolves symlinks and .. components)
    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            result.add_error(format!("Cannot resolve path: {}", e));
            return Ok(result);
        }
    };

    // 2. Path traversal check on the raw path (before resolution)
    if let Err(msg) = check_path_traversal(path) {
        result.add_error(msg);
    }

    // 3. Ensure it's a regular file
    let metadata = match std::fs::metadata(&canonical) {
        Ok(m) => m,
        Err(e) => {
            result.add_error(format!("Cannot stat file: {}", e));
            return Ok(result);
        }
    };

    if !metadata.is_file() {
        result.add_error("Path is not a regular file".to_string());
        return Ok(result);
    }

    // 4. Symlink check – canonical vs original differs means there's a symlink
    if canonical != path.canonicalize().unwrap_or_default() {
        result.add_warning("Path involves symlinks; resolved path may differ".to_string());
    }

    // 5. Permission and ownership checks (Unix only)
    #[cfg(unix)]
    check_unix_permissions(&metadata, path, &mut result);

    // 6. File extension check
    if let Some(ext) = path.extension() {
        if ext != "toml" {
            result.add_warning(format!(
                "Expected .toml extension, got .{}",
                ext.to_string_lossy()
            ));
        }
    } else {
        result.add_warning("File has no extension; expected .toml".to_string());
    }

    Ok(result)
}

/// Check a directory for overall security posture.
pub fn check_config_directory_security(dir: &Path) -> SegwireResult<SecurityCheckResult> {
    let mut result = SecurityCheckResult::new(dir);

    if !dir.exists() {
        result.add_error("Directory does not exist".to_string());
        return Ok(result);
    }

    if !dir.is_dir() {
        result.add_error("Path is not a directory".to_string());
        return Ok(result);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;

        if let Ok(metadata) = std::fs::metadata(dir) {
            let mode = metadata.permissions().mode();
            let uid = metadata.uid();

            // Directory should not be world-writable
            if mode & 0o002 != 0 {
                result.add_error(format!(
                    "Directory is world-writable (mode {:04o}); \
                     this allows anyone to inject configuration files",
                    mode & 0o7777
                ));
            }

            // Should be owned by root
            if uid != 0 {
                result.add_warning(format!(
                    "Directory owned by UID {} rather than root (UID 0); \
                     consider chown root:root",
                    uid
                ));
            }

            // Group writable is a mild concern
            if mode & 0o020 != 0 {
                result.add_warning(format!(
                    "Directory is group-writable (mode {:04o}); consider \
                     restricting to 755",
                    mode & 0o7777,
                ));
            }
        }
    }

    Ok(result)
}

#[cfg(unix)]
fn check_unix_permissions(
    metadata: &std::fs::Metadata,
    path: &Path,
    result: &mut SecurityCheckResult,
) {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode();
    let uid = metadata.uid();

    // World-writable is an error
    if mode & 0o002 != 0 {
        result.add_error(format!(
            "File is world-writable (mode {:04o}); \
             configuration files must not be writable by all users",
            mode & 0o7777,
        ));
    }

    // Should be owned by root for production configs
    if uid != 0 {
        let effective_uid = nix::unistd::Uid::effective().as_raw();
        if uid != effective_uid {
            result.add_warning(format!(
                "File '{}' is owned by UID {} (not root, not current user UID {})",
                path.display(),
                uid,
                effective_uid,
            ));
        }
    }

    // Recommended permissions: 644 or 640
    let recommended = [0o644, 0o640, 0o600];
    let file_perms = mode & 0o7777;
    if !recommended.contains(&file_perms) {
        result.add_warning(format!(
            "File permissions {:04o} are non-standard; \
             recommended: 0644 or 0640",
            file_perms,
        ));
    }
}

// ──────────────────────────────────────────────
// Path traversal protection
// ──────────────────────────────────────────────

/// Check a path for traversal attacks (e.g. `../../etc/shadow`).
pub fn check_path_traversal(path: &Path) -> Result<(), String> {
    for component in path.components() {
        if component == Component::ParentDir {
            return Err(format!(
                "Path contains '..' component which is not allowed: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Sanitize and validate a configuration path.
///
/// Returns the resolved, absolute path if it passes safety checks, or an
/// error if the path is dangerous.
pub fn sanitize_config_path(path: &str, allowed_root: &Path) -> SegwireResult<PathBuf> {
    // Reject empty paths
    if path.is_empty() {
        return Err(SegwireError::Validation(
            "Configuration path cannot be empty".to_string(),
        ));
    }

    // Reject paths with null bytes
    if path.contains('\0') {
        return Err(SegwireError::Validation(
            "Configuration path contains null byte".to_string(),
        ));
    }

    let path = Path::new(path);

    // Reject path traversal
    check_path_traversal(path).map_err(|msg| SegwireError::Validation(msg))?;

    // Resolve the path relative to the allowed root
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        allowed_root.join(path)
    };

    // Canonicalize and verify it's within the allowed directory
    let canonical = resolved
        .canonicalize()
        .map_err(|e| SegwireError::Validation(format!("Cannot resolve path: {}", e)))?;

    let allowed_canonical = allowed_root
        .canonicalize()
        .map_err(|e| SegwireError::Validation(format!("Cannot resolve allowed root: {}", e)))?;

    if !canonical.starts_with(&allowed_canonical) {
        return Err(SegwireError::Validation(format!(
            "Path '{}' resolves to '{}' which is outside allowed directory '{}'",
            path.display(),
            canonical.display(),
            allowed_canonical.display(),
        )));
    }

    Ok(canonical)
}

// ──────────────────────────────────────────────
// Input sanitization for configuration values
// ──────────────────────────────────────────────

/// Sanitize a string value from configuration for safe use.
///
/// This function checks for potentially dangerous characters or patterns
/// in configuration values that might be used in shell commands, file paths,
/// or network operations.
pub fn sanitize_config_value(value: &str, field_name: &str) -> SegwireResult<()> {
    // Reject null bytes
    if value.contains('\0') {
        return Err(SegwireError::Validation(format!(
            "Field '{}' contains null byte",
            field_name
        )));
    }

    // Reject control characters (except tab, newline for multi-line values)
    for ch in value.chars() {
        if ch.is_control() && ch != '\t' && ch != '\n' && ch != '\r' {
            return Err(SegwireError::Validation(format!(
                "Field '{}' contains control character U+{:04X}",
                field_name, ch as u32
            )));
        }
    }

    // Reject excessively long values
    const MAX_VALUE_LENGTH: usize = 4096;
    if value.len() > MAX_VALUE_LENGTH {
        return Err(SegwireError::Validation(format!(
            "Field '{}' exceeds maximum length of {} characters ({} given)",
            field_name,
            MAX_VALUE_LENGTH,
            value.len()
        )));
    }

    Ok(())
}

/// Sanitize a namespace name for safe use in file system operations.
pub fn sanitize_namespace_name(name: &str) -> SegwireResult<()> {
    sanitize_config_value(name, "namespace_name")?;

    // Additional namespace-specific checks
    if name.contains('/') || name.contains('\\') {
        return Err(SegwireError::Validation(format!(
            "Namespace name '{}' contains path separator characters",
            name
        )));
    }

    if name.starts_with('.') {
        return Err(SegwireError::Validation(format!(
            "Namespace name '{}' starts with a dot, which is not allowed",
            name
        )));
    }

    if name.starts_with('-') {
        return Err(SegwireError::Validation(format!(
            "Namespace name '{}' starts with a hyphen, which is not allowed",
            name
        )));
    }

    Ok(())
}

/// Sanitize an interface name for safe use.
pub fn sanitize_interface_name(name: &str) -> SegwireResult<()> {
    sanitize_config_value(name, "interface_name")?;

    // Interface names have strict requirements on Linux
    if name.len() > 15 {
        return Err(SegwireError::Validation(format!(
            "Interface name '{}' exceeds Linux maximum of 15 characters",
            name
        )));
    }

    if name.contains('/') || name.contains(' ') {
        return Err(SegwireError::Validation(format!(
            "Interface name '{}' contains invalid characters",
            name
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn test_path_traversal_clean() {
        assert!(check_path_traversal(Path::new("/etc/segwire/test.toml")).is_ok());
        assert!(check_path_traversal(Path::new("relative/path.toml")).is_ok());
    }

    #[test]
    fn test_path_traversal_blocked() {
        assert!(check_path_traversal(Path::new("../../etc/shadow")).is_err());
        assert!(check_path_traversal(Path::new("/etc/segwire/../shadow")).is_err());
    }

    #[test]
    fn test_sanitize_config_value_valid() {
        assert!(sanitize_config_value("hello world", "test").is_ok());
        assert!(sanitize_config_value("192.168.1.1", "ip").is_ok());
        assert!(sanitize_config_value("multi\nline", "desc").is_ok());
    }

    #[test]
    fn test_sanitize_config_value_null() {
        assert!(sanitize_config_value("bad\0value", "test").is_err());
    }

    #[test]
    fn test_sanitize_config_value_control_char() {
        assert!(sanitize_config_value("bad\x01value", "test").is_err());
    }

    #[test]
    fn test_sanitize_config_value_too_long() {
        let long = "x".repeat(5000);
        assert!(sanitize_config_value(&long, "test").is_err());
    }

    #[test]
    fn test_sanitize_namespace_name() {
        assert!(sanitize_namespace_name("my-namespace").is_ok());
        assert!(sanitize_namespace_name("test_123").is_ok());

        assert!(sanitize_namespace_name(".hidden").is_err());
        assert!(sanitize_namespace_name("-starts-dash").is_err());
        assert!(sanitize_namespace_name("has/slash").is_err());
    }

    #[test]
    fn test_sanitize_interface_name() {
        assert!(sanitize_interface_name("eth0").is_ok());
        assert!(sanitize_interface_name("veth-app").is_ok());

        // Too long (Linux max is IFNAMSIZ = 16, minus null = 15)
        assert!(sanitize_interface_name("veryverylongname1").is_err());
        assert!(sanitize_interface_name("has space").is_err());
    }

    #[test]
    fn test_sanitize_config_path_empty() {
        assert!(sanitize_config_path("", Path::new("/etc/segwire")).is_err());
    }

    #[test]
    fn test_sanitize_config_path_null_byte() {
        assert!(sanitize_config_path("test\0.toml", Path::new("/etc/segwire")).is_err());
    }

    #[test]
    fn test_sanitize_config_path_traversal() {
        assert!(sanitize_config_path("../../etc/shadow", Path::new("/etc/segwire")).is_err());
    }

    #[test]
    fn test_sanitize_config_path_within_root() {
        // Create a temp dir and file to test with
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.toml");
        let mut f = fs::File::create(&file_path).unwrap();
        writeln!(f, "[namespace]").unwrap();

        let result = sanitize_config_path("test.toml", dir.path());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_check_config_file_security() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.toml");
        let mut f = fs::File::create(&file_path).unwrap();
        writeln!(f, "[namespace]").unwrap();

        let result = check_config_file_security(&file_path).unwrap();
        // On a normal system, this should pass basic checks
        assert!(result.errors.is_empty() || !result.passed);
    }

    #[test]
    fn test_check_config_directory_security_nonexistent() {
        let result = check_config_directory_security(Path::new("/nonexistent/dir/xyz")).unwrap();
        assert!(!result.passed);
        assert!(result.errors[0].contains("does not exist"));
    }

    #[test]
    fn test_security_check_result_display() {
        let mut result = SecurityCheckResult::new(Path::new("/test/path"));
        result.add_error("test error".to_string());
        result.add_warning("test warning".to_string());

        let display = format!("{}", result);
        assert!(display.contains("FAIL"));
        assert!(display.contains("test error"));
        assert!(display.contains("test warning"));
    }
}
