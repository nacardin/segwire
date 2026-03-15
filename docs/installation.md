# Installation

## Prerequisites

- **Rust toolchain** (stable) — [rustup.rs](https://rustup.rs)
- **D-Bus development libraries** — `libdbus-1-dev` (Debian/Ubuntu) or `dbus-devel` (Fedora/RHEL)
- **PolicyKit** — `policykit-1` (optional, for fine-grained authorization)

## Build

```bash
cargo build --release --workspace
```

This produces three binaries in `target/release/`:

| Binary | Purpose |
|---|---|
| `segwire-daemon` | Root daemon |
| `segwire` | CLI tool |
| `segwire-ns-enter` | Setuid namespace-entry helper |

## Install Binaries

```bash
# Daemon and CLI — standard permissions
sudo install -m 0755 target/release/segwire-daemon /usr/bin/segwire-daemon
sudo install -m 0755 target/release/segwire        /usr/bin/segwire

# Setuid helper — group-restricted
sudo install -m 4750 -o root -g segwire target/release/segwire-ns-enter /usr/libexec/segwire-ns-enter
```

> **Note**: The `segwire-ns-enter` binary is installed **setuid root** (`4750`) and owned by `root:segwire`. Only users in the `segwire` group can execute it. See [Security.md](../Security.md) for the full rationale.

## System Group

```bash
# Create the segwire system group (run once, typically in package post-install)
sudo groupadd --system segwire

# Add users who should be able to exec into namespaces
sudo usermod -aG segwire <username>
```

Users must log out and back in for group membership to take effect.

## Configuration Directories

```bash
sudo mkdir -p /etc/segwire/namespaces
```

Create a daemon config at `/etc/segwire/daemon.toml` — see [Configuration](configuration.md) for the full reference.

## PolicyKit Policy

Install the PolicyKit action definitions so that D-Bus operations are authorized through polkit rather than falling back to root-only:

```bash
sudo install -m 0644 /path/to/org.segwire.policy /usr/share/polkit-1/actions/org.segwire.policy
```

The full policy XML is documented in [Security.md](../Security.md#policykit-policy-file). It defines five actions:

| Action ID | Covers |
|---|---|
| `org.segwire.namespace.status` | list, status, validate |
| `org.segwire.namespace.create` | create |
| `org.segwire.namespace.delete` | delete, restart |
| `org.segwire.namespace.manage` | reload |
| `org.segwire.namespace.exec` | exec (enter namespace) |

If PolicyKit is not available (e.g. in a container), the daemon falls back to UID-based checks — only root is authorized.

## Systemd Service

Create `/etc/systemd/system/segwire-daemon.service`:

```ini
[Unit]
Description=Segwire Network Namespace Manager
After=network.target dbus.service
Requires=dbus.service

[Service]
Type=simple
ExecStart=/usr/bin/segwire-daemon
Restart=on-failure
RestartSec=5

# Security hardening
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/run/netns /etc/segwire
NoNewPrivileges=false

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now segwire-daemon
```

> **Note**: `NoNewPrivileges=false` is required because the daemon needs to create network namespaces (`CAP_SYS_ADMIN`). The daemon verifies it has sufficient privileges at startup and exits with a clear error if not.

## Verify Installation

```bash
# Check the daemon is running
systemctl status segwire-daemon

# List managed namespaces
segwire list

# Validate a config file without applying it
segwire validate /etc/segwire/namespaces/my-namespace.toml
```
