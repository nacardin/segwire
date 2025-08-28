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

### Environment Variables
```bash
# Enable backtraces
export RUST_BACKTRACE=full
```

## Architecture Patterns
- **Shared Library Pattern**: Common types and utilities in `segwire-common`
- **Async/Await**: File monitoring and API operations (like TOML reading) use async patterns with monoio runtime. System calls (like `nix` namespace operations) and Netlink socket communication are synchronous.
- **Error Propagation**: Consistent error handling with `anyhow::Result` and custom error types
- **Configuration-Driven**: Declarative TOML-based configuration approach

## Async Runtime Guidelines
- **MUST USE**: `monoio` as the async runtime for all async operations
- **DO NOT USE**: `tokio` - monoio provides better performance with io_uring
- **Spawning Tasks**: Use `monoio::spawn()` instead of `tokio::spawn()`
- **Async Functions**: All async operations must be compatible with monoio runtime
- **Dependencies**: Ensure all async dependencies are compatible with monoio or provide monoio-compatible alternatives

### Sync vs Async Workloads
To ensure performance and correctness, segwire cleanly separates control-plane tasks:
- **Use Async (`monoio`) for**: File system operations (e.g., reading/writing TOML configurations natively supported by `io_uring`), D-Bus API communications, and general daemon coordination.
- **Use Sync (`std`/`nix`) for**: Syscalls that manipulate the kernel namespace state (e.g., `setns()`, `unshare()`). These must run synchronously on the bound OS thread to avoid leaking namespace context across async workers. Direct netlink communications (`netlink-packet-route`) should also be performed synchronously within these bound contexts to avoid async protocol overhead for infrequent control-plane packets.

### Mutexes and Locking
- **`std::sync::Mutex`**: It is perfectly fine to use standard, non-async synchronous mutexes (`std::sync::Mutex`) as long as they are **only used within synchronous functions and never held across `await` points**. Holding a synchronous mutex across an `await` point will cause compilation errors in Rust or deadlocks/Clippy warnings.
- **`async_lock::Mutex`**: If shared state needs to be locked and held while performing asynchronous `io_uring` file operations, DBus operations, or `monoio` tasks, you must use an asynchronous mutex like `async_lock::Mutex` (since `tokio::sync::Mutex` is forbidden).