//! Common utilities for the segwire system
//!
//! Provides validation functions, helper utilities, and shared functionality
//! used across daemon and CLI components.

use crate::error::{SegwireError, SegwireResult};
use regex::Regex;
use std::path::Path;

/// Validate network interface name format
pub fn validate_interface_name(name: &str) -> SegwireResult<()> {
    if name.is_empty() {
        return Err(SegwireError::Validation(
            "Interface name cannot be empty".to_string(),
        ));
    }

    if name.len() > 15 {
        return Err(SegwireError::Validation(
            "Interface name cannot exceed 15 characters".to_string(),
        ));
    }

    // Interface names should contain only alphanumeric characters, hyphens, and underscores
    let re = Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
    if !re.is_match(name) {
        return Err(SegwireError::Validation(
            "Interface name contains invalid characters".to_string(),
        ));
    }

    Ok(())
}

/// Validate namespace name format
pub fn validate_namespace_name(name: &str) -> SegwireResult<()> {
    if name.is_empty() {
        return Err(SegwireError::Validation(
            "Namespace name cannot be empty".to_string(),
        ));
    }

    if name.len() > 63 {
        return Err(SegwireError::Validation(
            "Namespace name cannot exceed 63 characters".to_string(),
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
        return Err(SegwireError::Validation(
            "Namespace prefix cannot be empty".to_string(),
        ));
    }

    if prefix.len() > 32 {
        return Err(SegwireError::Validation(
            "Namespace prefix cannot exceed 32 characters".to_string(),
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
            let _num: u8 = octet
                .parse()
                .map_err(|_| SegwireError::Validation(format!("Invalid IP address: {}", ip)))?;
            // u8 parsing already ensures the value is 0-255
        }
        Ok(())
    } else if ipv6_re.is_match(ip) {
        // Basic IPv6 validation passed
        Ok(())
    } else {
        Err(SegwireError::Validation(format!(
            "Invalid IP address format: {}",
            ip
        )))
    }
}

/// Validate CIDR notation (IP/prefix)
pub fn validate_cidr(cidr: &str) -> SegwireResult<()> {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return Err(SegwireError::Validation(
            "CIDR notation must contain exactly one '/' character".to_string(),
        ));
    }

    // Validate IP part
    validate_ip_address(parts[0])?;

    // Validate prefix length
    let prefix: u8 = parts[1]
        .parse()
        .map_err(|_| SegwireError::Validation("Invalid prefix length in CIDR".to_string()))?;

    // Check prefix length bounds based on IP version
    let ipv4_re = Regex::new(r"^(\d{1,3}\.){3}\d{1,3}$").unwrap();
    let max_prefix = if ipv4_re.is_match(parts[0]) {
        32 // IPv4
    } else {
        128 // IPv6
    };

    if prefix > max_prefix {
        return Err(SegwireError::Validation(format!(
            "Prefix length cannot exceed {} for this IP version",
            max_prefix
        )));
    }

    Ok(())
}

/// Check if a file has secure permissions (readable by owner only)
pub fn check_file_permissions(path: &Path) -> SegwireResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path).map_err(SegwireError::System)?;

    let permissions = metadata.permissions();
    let mode = permissions.mode();

    // Check if file is readable by group or others (we want 600 or 644 at most)
    if mode & 0o077 != 0 && mode & 0o044 == 0 {
        return Err(SegwireError::Validation(format!(
            "Configuration file {} has insecure permissions",
            path.display()
        )));
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

/// Substitute environment variables in a string value
///
/// Supports the following formats:
/// - ${VAR_NAME} - substitutes with environment variable or config environment value
/// - ${VAR_NAME:-default} - substitutes with default if variable is not set
/// - ${VAR_NAME:+alternate} - substitutes with alternate if variable is set
pub fn substitute_env_vars(
    input: &str,
    config_env: &std::collections::HashMap<String, String>,
) -> SegwireResult<String> {
    use crate::error::ConfigError;

    let mut result = input.to_string();
    let var_pattern = Regex::new(r"\$\{([^}]+)\}").unwrap();

    // Keep substituting until no more variables are found (handles nested substitution)
    let mut changed = true;
    let mut iteration_count = 0;
    const MAX_ITERATIONS: usize = 10; // Prevent infinite loops

    while changed && iteration_count < MAX_ITERATIONS {
        changed = false;
        iteration_count += 1;

        // Find all variable references in the current result
        let matches: Vec<_> = var_pattern
            .find_iter(&result)
            .map(|m| (m.start(), m.end(), m.as_str().to_string()))
            .collect();

        // Process matches in reverse order to avoid offset issues
        for (start, end, full_match) in matches.iter().rev() {
            let var_spec = &full_match[2..full_match.len() - 1]; // Remove ${ and }

            let substitution = resolve_variable_spec(var_spec, config_env)?;

            // Replace this occurrence
            result.replace_range(*start..*end, &substitution);
            changed = true;
        }
    }

    if iteration_count >= MAX_ITERATIONS {
        return Err(SegwireError::Config(ConfigError::EnvSubstitution(
            "Maximum substitution iterations exceeded - possible circular reference".to_string(),
        )));
    }

    Ok(result)
}

/// Resolve a variable specification (the part inside ${})
fn resolve_variable_spec(
    spec: &str,
    config_env: &std::collections::HashMap<String, String>,
) -> SegwireResult<String> {
    use crate::error::ConfigError;

    // Handle default value syntax: VAR_NAME:-default
    if let Some(colon_pos) = spec.find(":-") {
        let var_name = &spec[..colon_pos];
        let default_value = &spec[colon_pos + 2..];

        if let Some(value) = get_variable_value(var_name, config_env) {
            if value.is_empty() {
                Ok(default_value.to_string())
            } else {
                Ok(value)
            }
        } else {
            Ok(default_value.to_string())
        }
    }
    // Handle alternate value syntax: VAR_NAME:+alternate
    else if let Some(colon_pos) = spec.find(":+") {
        let var_name = &spec[..colon_pos];
        let alternate_value = &spec[colon_pos + 2..];

        if let Some(value) = get_variable_value(var_name, config_env) {
            if !value.is_empty() {
                Ok(alternate_value.to_string())
            } else {
                Ok(String::new())
            }
        } else {
            Ok(String::new())
        }
    }
    // Simple variable reference: VAR_NAME
    else {
        let var_name = spec;

        if let Some(value) = get_variable_value(var_name, config_env) {
            Ok(value)
        } else {
            Err(SegwireError::Config(ConfigError::EnvSubstitution(format!(
                "Environment variable '{}' not found",
                var_name
            ))))
        }
    }
}

/// Get variable value from config environment or system environment
fn get_variable_value(
    var_name: &str,
    config_env: &std::collections::HashMap<String, String>,
) -> Option<String> {
    // First check config environment variables
    if let Some(value) = config_env.get(var_name) {
        return Some(value.clone());
    }

    // Then check system environment variables
    std::env::var(var_name).ok()
}

/// Validate domain name format
pub fn validate_domain_name(domain: &str) -> SegwireResult<()> {
    if domain.is_empty() {
        return Err(SegwireError::Validation(
            "Domain name cannot be empty".to_string(),
        ));
    }

    if domain.len() > 253 {
        return Err(SegwireError::Validation(
            "Domain name cannot exceed 253 characters".to_string(),
        ));
    }

    // Handle trailing dot (FQDN)
    let domain_to_check = if let Some(stripped) = domain.strip_suffix('.') {
        stripped
    } else {
        domain
    };

    // Basic domain name validation
    let re = Regex::new(
        r"^[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?)*$",
    )
    .unwrap();
    if !re.is_match(domain_to_check) {
        return Err(SegwireError::Validation(format!(
            "Invalid domain name format: {}",
            domain
        )));
    }

    // Check that no label exceeds 63 characters
    for label in domain_to_check.split('.') {
        if label.len() > 63 {
            return Err(SegwireError::Validation(
                "Domain label cannot exceed 63 characters".to_string(),
            ));
        }
    }

    Ok(())
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
        assert_eq!(
            generate_full_namespace_name("segwire", "test"),
            "segwire-test"
        );
        assert_eq!(
            generate_full_namespace_name("app", "backend"),
            "app-backend"
        );
    }

    #[test]
    fn test_extract_namespace_name() {
        assert_eq!(
            extract_namespace_name("segwire", "segwire-test"),
            Some("test".to_string())
        );
        assert_eq!(extract_namespace_name("app", "segwire-test"), None);
        assert_eq!(extract_namespace_name("segwire", "test"), None);
    }

    #[test]
    fn test_validate_cidr() {
        assert!(validate_cidr("192.168.1.0/24").is_ok());
        assert!(validate_cidr("10.0.0.0/8").is_ok());
        assert!(validate_cidr("::1/128").is_ok());
        assert!(validate_cidr("192.168.1.0").is_err()); // Missing prefix
        assert!(validate_cidr("192.168.1.0/33").is_err()); // Invalid prefix for IPv4
        assert!(validate_cidr("invalid/24").is_err()); // Invalid IP
    }

    #[test]
    fn test_validate_domain_name() {
        assert!(validate_domain_name("example.com").is_ok());
        assert!(validate_domain_name("sub.example.com").is_ok());
        assert!(validate_domain_name("test-domain.org").is_ok());
        assert!(validate_domain_name("").is_err()); // Empty domain
        assert!(validate_domain_name("invalid..domain").is_err()); // Double dot
        assert!(validate_domain_name(".example.com").is_err()); // Starting with dot
        assert!(validate_domain_name("example.com.").is_ok()); // Trailing dot is valid

        // Test very long domain name
        let long_domain = "a".repeat(254);
        assert!(validate_domain_name(&long_domain).is_err());

        // Test long label
        let long_label = format!("{}.com", "a".repeat(64));
        assert!(validate_domain_name(&long_label).is_err());
    }

    #[test]
    fn test_validate_namespace_prefix() {
        assert!(validate_namespace_prefix("segwire").is_ok());
        assert!(validate_namespace_prefix("app-1").is_ok());
        assert!(validate_namespace_prefix("").is_err()); // Empty prefix
        assert!(validate_namespace_prefix("1invalid").is_err()); // Starting with number
        assert!(validate_namespace_prefix("invalid_prefix").is_err()); // Underscore not allowed

        // Test very long prefix
        let long_prefix = "a".repeat(33);
        assert!(validate_namespace_prefix(&long_prefix).is_err());
    }

    #[test]
    fn test_substitute_env_vars_simple() {
        let mut config_env = std::collections::HashMap::new();
        config_env.insert("TEST_VAR".to_string(), "test_value".to_string());

        // Simple substitution
        let result = substitute_env_vars("${TEST_VAR}", &config_env).unwrap();
        assert_eq!(result, "test_value");

        // Substitution within text
        let result = substitute_env_vars("prefix-${TEST_VAR}-suffix", &config_env).unwrap();
        assert_eq!(result, "prefix-test_value-suffix");

        // Multiple substitutions
        config_env.insert("VAR2".to_string(), "value2".to_string());
        let result = substitute_env_vars("${TEST_VAR}-${VAR2}", &config_env).unwrap();
        assert_eq!(result, "test_value-value2");

        // No substitution needed
        let result = substitute_env_vars("no_variables_here", &config_env).unwrap();
        assert_eq!(result, "no_variables_here");
    }

    #[test]
    fn test_substitute_env_vars_default_values() {
        let config_env = std::collections::HashMap::new();

        // Default value when variable not set
        let result = substitute_env_vars("${MISSING_VAR:-default_value}", &config_env).unwrap();
        assert_eq!(result, "default_value");

        // Default value when variable is empty
        let mut config_env = std::collections::HashMap::new();
        config_env.insert("EMPTY_VAR".to_string(), "".to_string());
        let result = substitute_env_vars("${EMPTY_VAR:-default_value}", &config_env).unwrap();
        assert_eq!(result, "default_value");

        // No default used when variable has value
        config_env.insert("SET_VAR".to_string(), "actual_value".to_string());
        let result = substitute_env_vars("${SET_VAR:-default_value}", &config_env).unwrap();
        assert_eq!(result, "actual_value");

        // Complex default value
        let result =
            substitute_env_vars("${MISSING_VAR:-complex-default-123}", &config_env).unwrap();
        assert_eq!(result, "complex-default-123");
    }

    #[test]
    fn test_substitute_env_vars_alternate_values() {
        let mut config_env = std::collections::HashMap::new();

        // Alternate value when variable is set
        config_env.insert("SET_VAR".to_string(), "original_value".to_string());
        let result = substitute_env_vars("${SET_VAR:+alternate_value}", &config_env).unwrap();
        assert_eq!(result, "alternate_value");

        // No alternate when variable not set
        let result = substitute_env_vars("${MISSING_VAR:+alternate_value}", &config_env).unwrap();
        assert_eq!(result, "");

        // No alternate when variable is empty
        config_env.insert("EMPTY_VAR".to_string(), "".to_string());
        let result = substitute_env_vars("${EMPTY_VAR:+alternate_value}", &config_env).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_substitute_env_vars_system_env() {
        let config_env = std::collections::HashMap::new();

        // Set a system environment variable for testing
        std::env::set_var("SEGWIRE_TEST_VAR", "system_value");

        let result = substitute_env_vars("${SEGWIRE_TEST_VAR}", &config_env).unwrap();
        assert_eq!(result, "system_value");

        // Config environment takes precedence over system environment
        let mut config_env = std::collections::HashMap::new();
        config_env.insert("SEGWIRE_TEST_VAR".to_string(), "config_value".to_string());
        let result = substitute_env_vars("${SEGWIRE_TEST_VAR}", &config_env).unwrap();
        assert_eq!(result, "config_value");

        // Clean up
        std::env::remove_var("SEGWIRE_TEST_VAR");
    }

    #[test]
    fn test_substitute_env_vars_nested() {
        let mut config_env = std::collections::HashMap::new();
        config_env.insert("INNER_VAR".to_string(), "inner_value".to_string());
        config_env.insert("OUTER_VAR".to_string(), "${INNER_VAR}".to_string());

        // Nested substitution
        let result = substitute_env_vars("${OUTER_VAR}", &config_env).unwrap();
        assert_eq!(result, "inner_value");

        // More complex nesting
        config_env.insert("PREFIX".to_string(), "test".to_string());
        config_env.insert("SUFFIX".to_string(), "app".to_string());
        config_env.insert("FULL_NAME".to_string(), "${PREFIX}-${SUFFIX}".to_string());
        let result = substitute_env_vars("namespace-${FULL_NAME}", &config_env).unwrap();
        assert_eq!(result, "namespace-test-app");
    }

    #[test]
    fn test_substitute_env_vars_errors() {
        let config_env = std::collections::HashMap::new();

        // Missing variable without default
        let result = substitute_env_vars("${MISSING_VAR}", &config_env);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Environment variable 'MISSING_VAR' not found"));

        // Test circular reference protection
        let mut config_env = std::collections::HashMap::new();
        config_env.insert("VAR1".to_string(), "${VAR2}".to_string());
        config_env.insert("VAR2".to_string(), "${VAR1}".to_string());
        let result = substitute_env_vars("${VAR1}", &config_env);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Maximum substitution iterations exceeded"));
    }

    #[test]
    fn test_substitute_env_vars_edge_cases() {
        let mut config_env = std::collections::HashMap::new();
        config_env.insert("EMPTY".to_string(), "".to_string());
        config_env.insert("SPACES".to_string(), "  value with spaces  ".to_string());
        config_env.insert(
            "SPECIAL_CHARS".to_string(),
            "value-with_special.chars123".to_string(),
        );

        // Empty variable
        let result = substitute_env_vars("${EMPTY}", &config_env).unwrap();
        assert_eq!(result, "");

        // Variable with spaces
        let result = substitute_env_vars("${SPACES}", &config_env).unwrap();
        assert_eq!(result, "  value with spaces  ");

        // Variable with special characters
        let result = substitute_env_vars("${SPECIAL_CHARS}", &config_env).unwrap();
        assert_eq!(result, "value-with_special.chars123");

        // Multiple variables with different formats
        let result = substitute_env_vars(
            "${EMPTY:-default}-${SPACES}-${SPECIAL_CHARS:+alt}",
            &config_env,
        )
        .unwrap();
        assert_eq!(result, "default-  value with spaces  -alt");
    }
}
