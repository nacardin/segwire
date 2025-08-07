# Technology Stack

## Build System
- **Cargo Workspace**: Multi-crate Rust project using workspace resolver "2"
- **Edition**: Rust 2021 edition across all crates

## Core Technologies
- **Language**: Rust
- **Async Runtime**: monoio with io_uring for high-performance I/O
- **IPC**: D-Bus for daemon-CLI communication
- **Configuration**: TOML for declarative configuration files
- **Authorization**: PolicyKit integration for fine-grained permissions

## Key Dependencies

### Shared Dependencies (workspace-level)
- `zbus` - D-Bus communication library
- `clap` - Command-line argument parsing
- `tabled` - Table formatting for CLI output
- `serde_json` - JSON serialization
- `monoio` - io_uring-based async runtime
- `anyhow` - Error handling

### Daemon-Specific
- `netlink-packet-route` - Network namespace management
- `rtnetlink` - Netlink socket communication
- `nix` - Unix system calls
- `toml` - Configuration file parsing
- `tracing` - Structured logging

### CLI-Specific
- Output formatting and D-Bus client functionality

## Common Commands

### Building
```bash
# Build entire workspace
cargo build

# Build specific crate
cargo build -p segwire-daemon
cargo build -p segwire-cli

# Release build
cargo build --release
```

### Testing
```bash
# Run all tests
cargo test

# Test specific crate
cargo test -p segwire-common
```

### Running
```bash
# Run specific crate
cargo run --bin segwire-cli
```

### Development
```bash
# Check code without building
cargo check

# Format code
cargo fmt

# Lint code
cargo clippy
```

### Environment Varialbes
```bash
# Enable backtraces
export RUST_BACKTRACE=full
```

## Architecture Patterns
- **Shared Library Pattern**: Common types and utilities in `segwire-common`
- **Async/Await**: All I/O operations use async patterns with monoio runtime
- **Error Propagation**: Consistent error handling with `anyhow::Result` and custom error types
- **Configuration-Driven**: Declarative TOML-based configuration approach