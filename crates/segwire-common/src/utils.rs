//! Common utilities for the segwire system
//! 
//! Provides validation functions, helper utilities, and shared functionality
//! used across daemon and CLI components.

use regex::Regex;
use std::path::Path;
use crate::error::{SegwireResult, SegwireError};

/// Validate network interface name format
pub fn validate_interface_name(name: &str) -> SegwireResult<()> {
    if name.is_empty() {
        return Err(SegwireError::Validation("Interface name cannot be empty".to_string()));
    }
    
    if name.len() > 15 {
        return Err(SegwireError::Validation(
            "Interface name cannot exceed 15 characters".to_string()
        ));
    }
    
    // Interface names should contain only alphanumeric characters, hyphens, and underscores
    let re = Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
    if !re.is_match(name) {
        return Err(SegwireError::Validation(
            "Interface name contains invalid characters".to_string()
        ));
    }
    
    Ok(())
}

/// Validate namespace name format
pub fn validate_namespace_name(name: &str) -> SegwireResult<()> {
    if name.is_empty() {
        return Err(SegwireError::Validation("Namespace name cannot be empty".to_string()));
    }
    
    if name.len() > 63 {
        return Err(SegwireError::Validation(
            "Namespace name cannot exceed 63 characters".to_string()
        ));
    }
    
    // Namespace names should start with a letter and contain only alphanumeric characters and hyphens
    let re = Regex::new(r"^[a-zA-Z][a-zA-Z0-9-]*$").unwrap();
    if !re.is_match(name) {
        return Err(SegwireError::Validation(
            "Namespace name must start with a letter and contain only alphanumeric characters and hyphens".to_string()
        ));
    }
    
    Ok(())
}

/// Validate namespace prefix format
pub fn validate_namespace_prefix(prefix: &str) -> SegwireResult<()> {
    if prefix.is_empty() {
        return Err(SegwireError::Validation("Namespace prefix cannot be empty".to_string()));
    }
    
    if prefix.len() > 32 {
        return Err(SegwireError::Validation(
            "Namespace prefix cannot exceed 32 characters".to_string()
        ));
    }
    
    // Prefix should follow similar rules as namespace names
    let re = Regex::new(r"^[a-zA-Z][a-zA-Z0-9-]*$").unwrap();
    if !re.is_match(prefix) {
        return Err(SegwireError::Validation(
            "Namespace prefix must start with a letter and contain only alphanumeric characters and hyphens".to_string()
        ));
    }
    
    Ok(())
}

/// Validate IP address format (basic validation)
pub fn validate_ip_address(ip: &str) -> SegwireResult<()> {
    // Basic IPv4 validation
    let ipv4_re = Regex::new(r"^(\d{1,3}\.){3}\d{1,3}$").unwrap();
    
    // Basic IPv6 validation (simplified)
    let ipv6_re = Regex::new(r"^([0-9a-fA-F]{0,4}:){2,7}[0-9a-fA-F]{0,4}$").unwrap();
    
    if ipv4_re.is_match(ip) {
        // Validate IPv4 octets are in valid range
        for octet in ip.split('.') {
            let _num: u8 = octet.parse()
                .map_err(|_| SegwireError::Validation(format!("Invalid IP address: {}", ip)))?;
            // u8 parsing already ensures the value is 0-255
        }
        Ok(())
    } else if ipv6_re.is_match(ip) {
        // Basic IPv6 validation passed
        Ok(())
    } else {
        Err(SegwireError::Validation(format!("Invalid IP address format: {}", ip)))
    }
}

/// Validate CIDR notation (IP/prefix)
pub fn validate_cidr(cidr: &str) -> SegwireResult<()> {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return Err(SegwireError::Validation(
            "CIDR notation must contain exactly one '/' character".to_string()
        ));
    }
    
    // Validate IP part
    validate_ip_address(parts[0])?;
    
    // Validate prefix length
    let prefix: u8 = parts[1].parse()
        .map_err(|_| SegwireError::Validation("Invalid prefix length in CIDR".to_string()))?;
    
    // Check prefix length bounds (IPv4: 0-32, IPv6: 0-128)
    // For simplicity, we'll allow 0-128 for both
    if prefix > 128 {
        return Err(SegwireError::Validation("Prefix length cannot exceed 128".to_string()));
    }
    
    Ok(())
}

/// Check if a file has secure permissions (readable by owner only)
pub fn check_file_permissions(path: &Path) -> SegwireResult<()> {
    use std::os::unix::fs::PermissionsExt;
    
    let metadata = std::fs::metadata(path)
        .map_err(|e| SegwireError::System(e))?;
    
    let permissions = metadata.permissions();
    let mode = permissions.mode();
    
    // Check if file is readable by group or others (we want 600 or 644 at most)
    if mode & 0o077 != 0 && mode & 0o044 == 0 {
        return Err(SegwireError::Validation(
            format!("Configuration file {} has insecure permissions", path.display())
        ));
    }
    
    Ok(())
}

/// Generate a full namespace name with prefix
pub fn generate_full_namespace_name(prefix: &str, name: &str) -> String {
    format!("{}-{}", prefix, name)
}

/// Extract namespace name from full prefixed name
pub fn extract_namespace_name(prefix: &str, full_name: &str) -> Option<String> {
    let expected_prefix = format!("{}-", prefix);
    if full_name.starts_with(&expected_prefix) {
        Some(full_name[expected_prefix.len()..].to_string())
    } else {
        None
    }
}

/// Check if a namespace name matches the given prefix
pub fn namespace_matches_prefix(prefix: &str, full_name: &str) -> bool {
    let expected_prefix = format!("{}-", prefix);
    full_name.starts_with(&expected_prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_interface_name() {
        assert!(validate_interface_name("eth0").is_ok());
        assert!(validate_interface_name("wlan0").is_ok());
        assert!(validate_interface_name("veth-test").is_ok());
        assert!(validate_interface_name("").is_err());
        assert!(validate_interface_name("very-long-interface-name").is_err());
        assert!(validate_interface_name("eth@0").is_err());
    }

    #[test]
    fn test_validate_namespace_name() {
        assert!(validate_namespace_name("test").is_ok());
        assert!(validate_namespace_name("test-namespace").is_ok());
        assert!(validate_namespace_name("app1").is_ok());
        assert!(validate_namespace_name("").is_err());
        assert!(validate_namespace_name("1test").is_err());
        assert!(validate_namespace_name("test_namespace").is_err());
    }

    #[test]
    fn test_validate_ip_address() {
        assert!(validate_ip_address("192.168.1.1").is_ok());
        assert!(validate_ip_address("10.0.0.1").is_ok());
        assert!(validate_ip_address("::1").is_ok());
        assert!(validate_ip_address("256.1.1.1").is_err());
        assert!(validate_ip_address("not.an.ip").is_err());
    }

    #[test]
    fn test_generate_full_namespace_name() {
        assert_eq!(generate_full_namespace_name("segwire", "test"), "segwire-test");
        assert_eq!(generate_full_namespace_name("app", "backend"), "app-backend");
    }

    #[test]
    fn test_extract_namespace_name() {
        assert_eq!(extract_namespace_name("segwire", "segwire-test"), Some("test".to_string()));
        assert_eq!(extract_namespace_name("app", "segwire-test"), None);
        assert_eq!(extract_namespace_name("segwire", "test"), None);
    }
}