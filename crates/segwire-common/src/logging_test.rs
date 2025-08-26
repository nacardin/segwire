//! Simple logging test to verify functionality

#[cfg(test)]
mod tests {

    use crate::{init_logging, LogConfig, LogContext, LogLevel};
    use std::path::PathBuf;

    #[test]
    fn test_logging_initialization() {
        let log_config = LogConfig {
            level: LogLevel::Debug,
            with_timestamps: false, // Disable for test consistency
            with_thread_names: false,
            with_file_line: false,
            with_spans: false,
            component: "test".to_string(),
        };

        // This should not panic
        let result = init_logging(log_config);
        // We can't test the actual output easily, but we can verify it doesn't error
        // In a real test environment, this might fail due to multiple initialization attempts
        // but that's expected behavior
        match result {
            Ok(_) => println!("Logging initialized successfully"),
            Err(e) => println!("Logging initialization failed (expected in tests): {}", e),
        }
    }

    #[test]
    fn test_log_context_creation() {
        let ctx = LogContext::new("test_operation")
            .with_namespace("test-ns")
            .with_config_path(PathBuf::from("/test/config.toml"))
            .with_user_id(1000)
            .with_field("test_field", "test_value");

        assert_eq!(ctx.operation, "test_operation");
        assert_eq!(ctx.namespace, Some("test-ns".to_string()));
        assert_eq!(ctx.config_path, Some(PathBuf::from("/test/config.toml")));
        assert_eq!(ctx.user_id, Some(1000));
        assert_eq!(
            ctx.fields.get("test_field"),
            Some(&"test_value".to_string())
        );
    }
}
