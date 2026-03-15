# Configuration Reference

Segwire uses TOML configuration files for both the daemon and individual namespaces.

## Daemon Configuration

**Location**: `/etc/segwire/daemon.toml`

```toml
[daemon]
# Prefix prepended to all namespace names (e.g. "sw" → "sw-myns")
namespace_prefix = "sw"

# Directory containing namespace .toml files (monitored by inotify)
config_dir = "/etc/segwire/namespaces"

# Delete managed namespaces and restore interfaces on daemon shutdown
# Default: true
cleanup_on_shutdown = true

# Logging settings
[daemon.logging]
level = "info"              # error | warn | info | debug | trace
with_timestamps = true      # Include timestamps in log output
with_thread_names = true    # Include thread names (config-monitor, state-sync)
with_file_line = false      # Include source file:line for each log entry
with_spans = false          # Include tracing span info

[dbus]
# D-Bus well-known name and object path (defaults shown)
service_name = "org.segwire.NamespaceManager"
object_path = "/org/segwire/NamespaceManager"
```

## Namespace Configuration

**Location**: `/etc/segwire/namespaces/<name>.toml`

Each file defines one namespace. The daemon watches the `config_dir` for changes via inotify — adding, modifying, or removing a `.toml` file triggers automatic reconciliation.

### Full Example

```toml
[namespace]
name = "vpn-tunnel"
description = "Isolated namespace for VPN traffic"

# Environment variables available for ${VAR} substitution below
[environment]
SUBNET = "10.200.0"
DNS_PRIMARY = "1.1.1.1"

[interfaces]
# Physical interfaces to move into this namespace
# They are returned to the default namespace on shutdown (if cleanup_on_shutdown = true)
move_interfaces = []

# Virtual interfaces to create
[[interfaces.virtual_interfaces]]
name = "veth-vpn"               # Interface inside the namespace
interface_type = "veth"         # veth | bridge | dummy | macvlan | ipvlan
peer = "veth-host"              # Host-side peer (required for veth)
addresses = [                   # IP addresses assigned inside the namespace (CIDR)
    "${SUBNET}.2/24",
    "fd00:vpn::2/64",
]

[routing]
# Default gateway within the namespace
default_gateway = "${SUBNET}.1"

# Additional static routes
[[routing.routes]]
destination = "192.168.0.0/16"
gateway = "${SUBNET}.1"
metric = 100                    # Optional, must be > 0

[dns]
servers = ["${DNS_PRIMARY}", "9.9.9.9"]
search = ["internal.example.com"]
```

### Section Reference

#### `[namespace]`

| Field | Required | Description |
|---|---|---|
| `name` | ✅ | Namespace name. Must start with a letter, contain only `a-z`, `0-9`, `-`. Combined with the daemon prefix to form the full name. |
| `description` | — | Human-readable description |

#### `[interfaces]`

| Field | Default | Description |
|---|---|---|
| `move_interfaces` | `[]` | List of physical interface names to move into the namespace |

#### `[[interfaces.virtual_interfaces]]`

| Field | Required | Description |
|---|---|---|
| `name` | ✅ | Interface name (max 15 chars per Linux IFNAMSIZ) |
| `interface_type` | ✅ | One of: `veth`, `bridge`, `dummy`, `macvlan`, `ipvlan` |
| `peer` | veth only | Peer interface name (required for `veth`, rejected for others) |
| `addresses` | — | List of IP addresses in CIDR notation (e.g. `"10.0.0.2/24"`, `"fd00::2/64"`) |

#### `[routing]`

| Field | Default | Description |
|---|---|---|
| `default_gateway` | — | Default gateway IP address inside the namespace |

#### `[[routing.routes]]`

| Field | Required | Description |
|---|---|---|
| `destination` | ✅ | Destination network in CIDR notation |
| `gateway` | ✅ | Gateway IP address |
| `metric` | — | Route metric (must be > 0 if set) |

#### `[dns]`

| Field | Default | Description |
|---|---|---|
| `servers` | `[]` | DNS server IP addresses |
| `search` | `[]` | DNS search domains |

#### `[environment]`

A flat key-value map. Values are available for `${KEY}` substitution in all other fields. Environment variables from the system are **not** inherited — only variables defined in this section.

### Validation Rules

- **Namespace names**: must match `^[a-z][a-z0-9-]*$`
- **Interface names**: must be valid Linux interface names (≤ 15 chars, no `/` or whitespace)
- **Addresses**: must be valid CIDR notation (`ip/prefix`)
- **Gateways**: must be valid IP addresses
- **Route metrics**: must be > 0
- **No duplicates**: interface names, DNS servers, search domains, and route destinations must be unique within their respective lists
