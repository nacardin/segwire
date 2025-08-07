# Project Structure

## Workspace Organization

```
segwire/
├── Cargo.toml              # Workspace root with member crates
├── Cargo.lock              # Dependency lock file
├── crates/                 # All project crates
│   ├── segwire-daemon/     # System daemon crate
│   ├── segwire-cli/        # Command-line interface crate
│   └── segwire-common/     # Shared library crate (to be created)
└── target/                 # Build artifacts
```

## Crate Structure

### segwire-daemon
- **Purpose**: System daemon that manages network namespaces
- **Key Modules**: Configuration manager, namespace manager, D-Bus service, event loop
- **Dependencies**: Network management, D-Bus, file monitoring, logging

### segwire-cli  
- **Purpose**: Command-line interface for daemon interaction
- **Key Modules**: Command parser, D-Bus client, output formatter
- **Dependencies**: CLI parsing, D-Bus client, output formatting

### segwire-common (shared library)
- **Purpose**: Shared types, utilities, and D-Bus interfaces
- **Key Modules**: D-Bus types, configuration structures, error types, common utilities
- **Usage**: Imported by both daemon and CLI crates

## Naming Conventions

- **Crate Names**: `segwire-{component}` format
- **Module Names**: Snake_case following Rust conventions
- **Configuration**: TOML files with descriptive names
- **Namespace Prefixes**: Configurable prefix system for namespace isolation

## Dependencies Management

- **Workspace Dependencies**: Shared dependencies defined at workspace level
- **Crate-Specific**: Additional dependencies in individual Cargo.toml files
- **Version Consistency**: All crates use same dependency versions via workspace inheritance

## File Organization Principles

- **Separation of Concerns**: Each crate has distinct responsibilities
- **Shared Code**: Common functionality centralized in segwire-common
- **Configuration**: Declarative TOML-based configuration files
- **Build Artifacts**: Isolated in target/ directory