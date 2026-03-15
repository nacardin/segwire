# Development Guide

## Building

```bash
# Full workspace build
cargo build --workspace

# Run clippy lints
cargo clippy --workspace --all-targets

# Check dependency sorting (requires cargo-sort)
cargo sort --workspace --check
```

## Crate Layout

```
crates/
├── segwire-cli/          CLI binary — clap parser, D-Bus client, output formatting
│   └── src/commands/     One module per subcommand (exec, list, reload, restart, status, validate)
├── segwire-common/       Shared library — config structs, validation, error types, D-Bus constants
├── segwire-daemon/       Daemon binary — event loop, D-Bus service, netlink, inotify, PolicyKit
├── segwire-ns-enter/     Setuid helper — ~60 lines, minimal dependencies
└── segwire-test/         Integration test harness + tests
```

## Testing

### Unit Tests

```bash
# Run all unit tests (no root required)
cargo test --workspace
```

### Integration Tests

Integration tests live in `segwire-test` and exercise the full daemon + CLI flow as a black-box:

1. A private `dbus-daemon` is launched for test isolation
2. The daemon event loop runs on a background thread
3. CLI commands are invoked through `segwire_cli::run_cli()` (in-process, no child process)
4. Assertions verify both D-Bus responses and (when root) real system state

```bash
# Root-required tests: create real namespaces, test netlink, verify ping connectivity
sudo cargo test -p segwire-test -- --test-threads=1

# Non-root: tests run in simulation mode (in-memory state only)
cargo test -p segwire-test -- --test-threads=1
```

> **Note**: Integration tests use `serial_test` and must run single-threaded (`--test-threads=1`) because they share process-global state (`DBUS_SESSION_BUS_ADDRESS` env var).

### Test Harness

The `TestHarness` struct (`segwire-test/src/harness.rs`) provides:

- **Private D-Bus session**: launches `dbus-daemon --session --fork` and sets `DBUS_SESSION_BUS_ADDRESS`
- **Temp config directory**: creates a `TempDir` with a daemon config pre-written
- **Daemon lifecycle**: `start_daemon_background()` returns a `JoinHandle` and `AtomicBool` shutdown flag
- **Config helpers**: `write_namespace_config()` and `remove_namespace_config()` for test setup
- **Automatic cleanup**: `Drop` impl kills the private dbus-daemon and clears env vars

## Environment Variables

| Variable | Used by | Description |
|---|---|---|
| `DBUS_SESSION_BUS_ADDRESS` | daemon + CLI | When set, both connect to a session bus instead of the system bus. Used automatically by the test harness. |
| `SEGWIRE_SIMULATION` | daemon | Set to `1` to skip real netlink operations (in-memory namespace state only). Allows non-root testing. |
| `SEGWIRE_NS_ENTER_PATH` | CLI | Override the path to the `segwire-ns-enter` binary. Useful during development to point at `target/debug/segwire-ns-enter`. |
| `SEGWIRE_TEST_SKIP_CLEANUP` | tests | When set, integration tests skip namespace and veth cleanup. Useful for post-mortem inspection. |

## Running Locally (Development)

```bash
# 1. Build everything
cargo build --workspace

# 2. Create temp config directory
mkdir -p /tmp/segwire-dev

# 3. Write a daemon config
cat > /tmp/segwire-dev/daemon.toml << 'EOF'
[daemon]
namespace_prefix = "dev"
config_dir = "/tmp/segwire-dev"

[dbus]
EOF

# 4. Run the daemon (requires root for real netlink operations)
sudo target/debug/segwire-daemon

# 5. In another terminal, use the CLI
target/debug/segwire list
```

### Testing the Exec Flow

The `segwire exec` flow requires the `segwire-ns-enter` binary to be setuid. For development:

```bash
# Option A: Run without setuid (requires sudo for the exec itself)
SEGWIRE_NS_ENTER_PATH=./target/debug/segwire-ns-enter segwire exec my-ns -- ip link show

# Option B: Set up setuid on the debug binary (full unprivileged flow)
sudo chown root:$(id -gn) target/debug/segwire-ns-enter
sudo chmod 4750 target/debug/segwire-ns-enter
SEGWIRE_NS_ENTER_PATH=./target/debug/segwire-ns-enter segwire exec my-ns -- ip link show
```
