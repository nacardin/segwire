# Segwire

**Declarative Linux network namespace management, without the `sudo` dance.**

Segwire is a daemon + CLI tool that lets you define network namespaces in TOML config files and manages their full lifecycle — creation, interface setup, routing, DNS, live-reload, and unprivileged command execution inside namespaces.

## Features

- **Declarative configuration** — define namespaces, interfaces, routes, and DNS in TOML files
- **Hot-reload** — inotify-based config monitoring; add or remove a `.toml` file and the daemon reacts automatically
- **Unprivileged `exec`** — run commands (including GUI apps) inside namespaces without `sudo`, via a minimal setuid helper
- **D-Bus API** — all operations go through a well-defined D-Bus interface with PolicyKit authorization
- **Dual-stack** — full IPv4 and IPv6 support for addresses, routes, and DNS
- **Virtual interfaces** — veth pairs, bridges, dummy, macvlan, and ipvlan
- **Interface migration** — move physical interfaces into namespaces and restore them on shutdown
- **Graceful lifecycle** — optional cleanup-on-shutdown returns interfaces and deletes namespaces

## Quick Start

```bash
# Build
cargo build --release --workspace

# Install (see docs/installation.md for full setup including setuid helper)
sudo cp target/release/segwire-daemon /usr/bin/
sudo cp target/release/segwire /usr/bin/

# Create config directory
sudo mkdir -p /etc/segwire/namespaces

# Write a daemon config
sudo tee /etc/segwire/daemon.toml << 'EOF'
[daemon]
namespace_prefix = "sw"
config_dir = "/etc/segwire/namespaces"

[dbus]
EOF

# Write a namespace config
sudo tee /etc/segwire/namespaces/vpn.toml << 'EOF'
[namespace]
name = "vpn"
description = "VPN isolation namespace"

[interfaces]
move_interfaces = []

[[interfaces.virtual_interfaces]]
name = "veth-vpn"
interface_type = "veth"
peer = "veth-host"
addresses = ["10.200.0.2/24"]

[routing]
default_gateway = "10.200.0.1"

[dns]
servers = ["1.1.1.1", "9.9.9.9"]
EOF

# Start the daemon, then use the CLI
sudo systemctl start segwire-daemon   # or run directly: sudo segwire-daemon
segwire list
segwire status vpn
segwire exec vpn -- curl ifconfig.me
```

## CLI Commands

| Command | Description |
|---|---|
| `segwire list` | List all managed namespaces |
| `segwire status <name>` | Show detailed status for a namespace |
| `segwire reload` | Reload configuration files and sync state |
| `segwire restart <name>` | Delete and recreate a namespace from its config |
| `segwire validate [path]` | Validate configuration files without applying |
| `segwire exec <name> -- <cmd>` | Run a command inside a namespace (no sudo needed) |

## Architecture

```
┌──────────────────────────────────┐
│       User Session               │
│  ┌──────────┐  ┌──────────────┐  │
│  │ segwire  │  │ segwire-ns-  │  │
│  │  (CLI)   │  │ enter (suid) │  │
│  └────┬─────┘  └──────┬───────┘  │
│       │ D-Bus         │ execvp   │
│       │               ▼          │
│       │         ┌──────────┐     │
│       │         │ command  │     │
│       │         │ (as user)│     │
│       │         └──────────┘     │
└───────┼──────────────────────────┘
        │ System D-Bus
┌───────▼──────────────────────────┐
│  segwire-daemon (root)           │
│  ├── PolicyKit authorization     │
│  ├── Namespace lifecycle         │
│  └── Configuration management    │
└──────────────────────────────────┘
```

## Documentation

| Document | Description |
|---|---|
| [Architecture](docs/architecture.md) | Crate layout, event loop, D-Bus interface, namespace lifecycle |
| [Configuration](docs/configuration.md) | Daemon and namespace TOML reference with examples |
| [Installation](docs/installation.md) | Build, permissions, systemd, PolicyKit setup |
| [Development](docs/development.md) | Building, testing, environment variables, contributing |
| [Security](Security.md) | Security model, setuid helper, threat model, attack surface |

## License

<!-- TODO: Add license -->
