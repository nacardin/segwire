# Implementation Plan

- [x] 1. Set up project structure and shared crate foundation
  - Create segwire-common crate with basic module structure
  - Define workspace dependencies and shared configuration
  - Set up basic error types and common utilities
  - _Requirements: 2.1, 2.2, 6.1_

- [ ] 2. Implement core configuration types and parsing
  - [x] 2.1 Create TOML configuration structures in segwire-common
    - Define master daemon configuration structure
    - Define namespace configuration structure with all fields
    - Implement serde serialization/deserialization
    - _Requirements: 2.1, 2.2, 2.7_

  - [x] 2.2 Implement configuration validation logic
    - Add validation functions for network interface names
    - Add validation for routing rules and DNS configuration
    - Implement namespace name prefix validation
    - _Requirements: 2.3, 2.4, 2.5, 2.6_

  - [x] 2.3 Add environment variable substitution support
    - Implement environment variable parsing in TOML values
    - Add runtime variable resolution functionality
    - Write unit tests for variable substitution
    - _Requirements: 2.8_

- [x] 3. Create D-Bus interface definitions and types
  - [x] 3.1 Define D-Bus interface structures in segwire-common
    - Create method signatures for all daemon operations
    - Define signal types for status updates and progress
    - Implement D-Bus error types with descriptive messages
    - _Requirements: 6.1, 6.2, 6.6_

  - [x] 3.2 Implement D-Bus introspection support
    - Generate introspection XML from interface definitions
    - Add method discovery and enumeration support
    - Write tests for introspection functionality
    - _Requirements: 6.7_

- [x] 4. Build daemon configuration management system
  - [x] 4.1 Implement master configuration loading
    - Create configuration file reader with error handling
    - Add namespace prefix parsing and validation
    - Implement configuration directory path resolution
    - _Requirements: 1.1, 2.2, 2.3_

  - [x] 4.2 Create namespace configuration scanner
    - Implement directory scanning for .toml files
    - Add configuration file parsing with detailed error reporting
    - Filter configurations by daemon namespace prefix
    - _Requirements: 1.1, 1.2, 2.3, 2.9_

  - [x] 4.3 Add file system monitoring with monoio
    - Implement io_uring-based file watching for configuration changes
    - Handle file creation, modification, and deletion events
    - Add debouncing for rapid file changes
    - _Requirements: 1.3, 1.4_

- [x] 5. Implement network namespace management core
  - [x] 5.1 Create netlink interface wrapper
    - Implement netlink socket communication for namespace operations
    - Add namespace creation and deletion functions
    - Create error handling for netlink operations
    - _Requirements: 1.2, 1.5, 7.5_

  - [x] 5.2 Implement network interface management
    - Add functions to move interfaces between namespaces
    - Implement interface validation and availability checking
    - Create virtual interface creation (veth pairs)
    - _Requirements: 2.4, 7.5_

  - [x] 5.3 Add routing and DNS configuration
    - Implement routing table configuration within namespaces
    - Add DNS resolver configuration for namespaces
    - Create route validation and conflict detection
    - _Requirements: 2.5, 2.6_

- [x] 6. Build daemon D-Bus service implementation
  - [x] 6.1 Create D-Bus service registration and setup
    - Implement service registration on system bus
    - Add PolicyKit integration for authorization
    - Create method call dispatcher and error handling
    - _Requirements: 6.1, 7.3, 7.4_

  - [x] 6.2 Implement namespace management D-Bus methods
    - Add ListNamespaces method with status information
    - Implement GetNamespaceStatus with detailed information
    - Create CreateNamespace and DeleteNamespace methods
    - _Requirements: 3.1, 3.2, 4.1, 4.2_

  - [x] 6.3 Add configuration management D-Bus methods
    - Implement ReloadConfiguration method
    - Add ValidateConfiguration method with error reporting
    - Create progress signal emission for long operations
    - _Requirements: 4.3, 4.6, 6.4_

- [ ] 7. Create daemon main event loop and coordination
  - [x] 7.1 Implement monoio-based event loop
    - Set up monoio runtime with io_uring support
    - Create task coordination between configuration monitoring and D-Bus service
    - Add graceful shutdown handling with cleanup
    - _Requirements: 1.6, 5.1, 5.2_

  - [x] 7.2 Add namespace state management
    - Implement in-memory state tracking for managed namespaces
    - Create state synchronization between configuration and actual namespaces
    - Add conflict resolution for configuration changes
    - _Requirements: 1.3, 1.4, 2.9_

  - [x] 7.3 Implement logging and error reporting
    - Add structured logging with tracing crate
    - Implement detailed error reporting with context
    - _Requirements: 5.1, 5.2, 5.3, 5.4, 5.6_

- [ ] 8. Build CLI D-Bus client implementation
  - [x] 8.1 Create D-Bus client connection and discovery
    - Implement connection to daemon's D-Bus service
    - Add service discovery and availability checking
    - Create connection error handling and retry logic
    - _Requirements: 6.2, 6.5_

  - [-] 8.2 Implement CLI command parsing and validation
    - Create clap-based command structure for all operations
    - Add input validation and help text generation
    - Implement command-specific argument parsing
    - _Requirements: 3.1, 3.2, 4.1, 4.2, 4.3, 4.4, 4.6_

  - [ ] 8.3 Add output formatting and display
    - Implement table-based output for status and list commands
    - Add JSON and YAML output format options
    - Create progress display for long-running operations
    - _Requirements: 3.1, 3.2, 3.3, 3.6_

- [ ] 9. Implement privilege checking and security
  - [ ] 9.1 Add capability checking in daemon
    - Implement CAP_SYS_ADMIN capability verification
    - Add runtime privilege checking with clear error messages
    - Create container environment detection and adaptation
    - _Requirements: 7.1, 7.5, 7.6, 7.7_

  - [ ] 9.2 Create PolicyKit integration
    - Implement PolicyKit authorization for D-Bus methods
    - Add fine-grained permission checking for different operations
    - Create clear error messages for authorization failures
    - _Requirements: 7.3, 7.4, 7.6, 7.8_

  - [ ] 9.3 Add configuration file security validation
    - Implement file permission and ownership checking
    - Add input sanitization for configuration values
    - Create path traversal protection for configuration paths
    - _Requirements: 7.2_

- [ ] 10. Create comprehensive test suite
  - [ ] 10.1 Write unit tests for shared crate
    - Test configuration parsing and validation logic
    - Test D-Bus type serialization and deserialization
    - Test error handling and utility functions
    - _Requirements: 2.1, 2.7, 6.6_

  - [ ] 10.2 Write daemon component tests
    - Test configuration monitoring and file watching
    - Test namespace management operations with mocks
    - Test D-Bus service methods and signal emission
    - _Requirements: 1.1, 1.2, 1.3, 6.1, 6.2_

  - [ ] 10.3 Write CLI component tests
    - Test command parsing and validation
    - Test D-Bus client communication with mock daemon
    - Test output formatting for different data types
    - _Requirements: 3.1, 3.2, 6.2, 6.5_

- [ ] 11. Add integration testing and system validation
  - [ ] 11.1 Create end-to-end workflow tests
    - Test complete daemon startup and configuration loading
    - Test CLI commands against running daemon instance
    - Test configuration file changes and automatic updates
    - _Requirements: 1.1, 1.3, 4.3_

  - [ ] 11.2 Add system integration tests
    - Test actual network namespace creation (requires privileges)
    - Test real network interface manipulation
    - Test D-Bus system bus integration and PolicyKit
    - _Requirements: 1.2, 2.4, 6.1, 7.3_

- [ ] 12. Create deployment and packaging
  - [ ] 12.1 Create systemd service configuration
    - Write systemd service file with proper security settings
    - Add capability configuration and privilege dropping
    - Implement service dependencies and startup ordering
    - _Requirements: 7.1, 7.5_

  - [ ] 12.2 Add PolicyKit rules and D-Bus service files
    - Create PolicyKit policy files for fine-grained permissions
    - Write D-Bus service activation files
    - Add installation scripts for system integration
    - _Requirements: 7.3, 7.4, 7.8_

  - [ ] 12.3 Create package structure and installation
    - Build release binaries with optimizations
    - Create installation script with proper file permissions
    - Add configuration file templates and documentation
    - _Requirements: 2.1, 2.2_