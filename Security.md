# Segwire Security Model

This document describes the security architecture of segwire, with particular focus on the **namespace exec** feature that allows unprivileged users to run commands inside managed network namespaces.

## Architecture Overview

Segwire consists of three components with distinct privilege levels:

| Component | Runs As | Purpose |
|---|---|---|
| `segwire-daemon` | root (systemd service) | Creates/deletes namespaces, manages configuration, serves D-Bus API |
| `segwire` (CLI) | unprivileged user | Sends commands to the daemon via D-Bus |
| `segwire-ns-enter` | setuid root → drops to user | Enters a namespace and execs a command as the calling user |

```
┌──────────────────────────────────┐
│       User Session               │
│  ┌──────────┐  ┌──────────────┐  │
│  │ segwire  │──│ segwire-ns-  │  │
│  │  (CLI)   │  │ enter (suid) │  │
│  └────┬─────┘  └──────┬───────┘  │
│       │ D-Bus         │ execvp   │
│       │               ▼          │
│       │         ┌──────────┐     │
│       │         │ firefox  │     │
│       │         │ (as user)│     │
│       │         └──────────┘     │
└───────┼──────────────────────────┘
        │ System D-Bus
┌───────▼──────────────────────────┐
│  segwire-daemon (root)           │
│  ├── PolicyKit authorization     │
│  ├── Namespace lifecycle         │
│  └── Configuration management   │
└──────────────────────────────────┘
```

## Authorization Model

### PolicyKit (polkit)

All privileged operations are gated by PolicyKit. The daemon checks the calling process's PID and UID against polkit policies before performing any action.

| Action ID | Operations | Default Policy |
|---|---|---|
| `org.segwire.namespace.status` | list, status, validate | `auth_admin_keep` |
| `org.segwire.namespace.create` | create | `auth_admin_keep` |
| `org.segwire.namespace.delete` | delete, restart | `auth_admin_keep` |
| `org.segwire.namespace.manage` | reload | `auth_admin_keep` |
| `org.segwire.namespace.exec` | exec (enter namespace) | `auth_admin_keep` |

**Fallback behavior**: If PolicyKit is not available (e.g., in a container), the daemon falls back to a UID-based check — only root (UID 0) is authorized.

### Group Gating

The `segwire-ns-enter` binary is installed with permissions `4750` owned by `root:segwire`. Only users in the `segwire` system group can execute it. This provides a coarse-grained access control layer independent of PolicyKit.

## The setuid Helper: `segwire-ns-enter`

### Why setuid?

The Linux kernel requires `CAP_SYS_ADMIN` to call `setns(fd, CLONE_NEWNET)`, which is the syscall that enters a network namespace. There is no way for an unprivileged process to enter an existing network namespace.

The alternatives and why they don't work for our use case:

| Approach | Problem |
|---|---|
| `sudo segwire exec` | Can't run GUI apps (wrong HOME, no DISPLAY, security risk) |
| `setcap cap_sys_admin+ep` | `CAP_SYS_ADMIN` is nearly equivalent to root; rare in practice |
| Daemon `fork()+exec()` | Child inherits daemon's environment, not user's (no display, no audio) |
| User namespaces | Can't enter a netns owned by the init user namespace from a user namespace |
| D-Bus fd-passing without helper | Still need CAP_SYS_ADMIN for setns in the CLI process |

### Precedent

This is the established pattern used by widely-deployed, security-audited software:

| Software | Binary | Mechanism | Purpose |
|---|---|---|---|
| Flatpak / bubblewrap | `/usr/bin/bwrap` | setuid root | Sandbox namespace entry |
| Firejail | `/usr/bin/firejail` | setuid root | Application sandboxing |
| Chromium | `chrome-sandbox` | setuid root | Renderer sandbox |
| shadow-utils | `/usr/bin/newuidmap` | setuid root | User namespace UID mapping |

### What the Helper Does

The helper performs exactly **6 operations**, in strict order:

```
1. Validate namespace path  ← reject anything not under /run/netns/
2. Open namespace file      ← uses euid=0 from setuid
3. setns(fd, CLONE_NEWNET)  ← enters the network namespace
4. close(fd)
5. Drop ALL privileges      ← setresuid + setresgid (permanent, irrecoverable)
6. PR_SET_NO_NEW_PRIVS      ← prevent escalation via setuid/setcap binaries
7. execvp(command)           ← replace process with user's command
```

After step 5, the process is running as the calling user with no elevated privileges. After step 6, even setuid binaries executed by the child process cannot gain privileges. Step 7 replaces the helper process entirely — it ceases to exist.

### Attack Surface Analysis

The helper binary is intentionally minimal:
- **~60 lines of Rust** (no unsafe beyond what nix requires for fd borrowing)
- **No network I/O** — never opens a socket
- **No file parsing** — no config files, no data processing
- **No heap allocation after setns** — the only allocations are CString conversions before exec
- **No dependencies beyond nix and libc** — no serde, no logging framework, no D-Bus

### Security Properties

| Property | Mechanism |
|---|---|
| **Path restriction** | Only paths under `/run/netns/` are accepted |
| **Traversal protection** | Namespace name portion is checked for `/` and `..` |
| **Symlink protection** | `symlink_metadata()` rejects symlinks |
| **Null byte protection** | Path is checked for `\0` |
| **Permanent privilege drop** | `setresuid(ruid, ruid, ruid)` — sets real, effective, AND saved-set UID |
| **Escalation prevention** | `PR_SET_NO_NEW_PRIVS` prevents setuid/setcap escalation by child processes |
| **Group gating** | Binary is `4750 root:segwire` — only group members can execute |
| **Polkit pre-authorization** | The daemon must authorize the operation before the CLI invokes the helper |

### Privilege Lifecycle

```
TIME →

segwire (CLI)     [user]─────────────────────[user]───► (execvp to helper)
                     │ D-Bus                       │
                     ▼                             ▼
segwire-daemon    [root]                    [not involved]
                     │ polkit check
                     ▼
                  OK → returns ns_path

segwire-ns-enter  [euid=0,ruid=user]──setns──drop privs──[user]──► execvp(firefox)
                  ▲                                              │
                  │ setuid bit                                   │
                  │ provides euid=0                              ▼
                  │                                         firefox runs as
                  │                                         user with user's
                  process lifetime: ~1ms                    env, display, audio
```

The window where the helper runs with euid=0 is **microseconds** — just long enough to open a file and call setns.

## Threat Model

### In Scope

| Threat | Mitigation |
|---|---|
| Unprivileged user enters arbitrary namespace | Polkit authorization required; daemon validates namespace exists and is managed |
| Path traversal via ns-path argument | `/run/netns/` prefix enforced; `..` and `/` rejected in name portion |
| Symlink attack on namespace file | `symlink_metadata()` detects and rejects symlinks |
| Privilege escalation after namespace entry | `setresuid` + `PR_SET_NO_NEW_PRIVS` make escalation impossible |
| Exploitation of helper binary | Minimal code (~60 lines), no parsing, no network, no heap after setns |
| Time-of-check time-of-use (TOCTOU) | The helper validates and opens the path atomically; worst case, it enters a just-deleted namespace (harmless — the process gets an empty network stack) |

### Out of Scope

| Threat | Rationale |
|---|---|
| Root compromise | If root is compromised, all bets are off regardless |
| Kernel exploit via `setns()` | This is a kernel issue; segwire uses the same syscall as `ip netns exec` |
| D-Bus bus hijacking | Standard D-Bus trust model; mitigated by D-Bus's own authentication |
| User within namespace attacks host | Network namespaces only isolate networking; they don't provide filesystem or process isolation |

## Installation and Permissions

### File Permissions

```bash
# The daemon — standard root service binary
/usr/bin/segwire-daemon          root:root  0755

# The CLI — unprivileged
/usr/bin/segwire                 root:root  0755

# The setuid helper — group-restricted
/usr/libexec/segwire-ns-enter   root:segwire  4750
```

### System Group Setup

```bash
# Create the segwire group (during package install)
groupadd --system segwire

# Add users who should be able to exec into namespaces
usermod -aG segwire <username>
```

### Package Post-Install Script

```bash
#!/bin/sh
# debian/postinst or RPM %post

# Ensure the system group exists
getent group segwire > /dev/null || groupadd --system segwire

# Set helper permissions
chown root:segwire /usr/libexec/segwire-ns-enter
chmod 4750 /usr/libexec/segwire-ns-enter

# Ensure /run/netns exists
mkdir -p /run/netns
```

### PolicyKit Policy File

Install to `/usr/share/polkit-1/actions/org.segwire.policy`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE policyconfig PUBLIC
  "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN"
  "http://www.freedesktop.org/standards/PolicyKit/1.0/policyconfig.dtd">
<policyconfig>
  <vendor>Segwire</vendor>
  <vendor_url>https://github.com/segwire</vendor_url>

  <action id="org.segwire.namespace.status">
    <description>View segwire namespace status</description>
    <message>Authentication is required to view namespace status</message>
    <defaults>
      <allow_any>auth_admin</allow_any>
      <allow_inactive>auth_admin</allow_inactive>
      <allow_active>yes</allow_active>
    </defaults>
  </action>

  <action id="org.segwire.namespace.create">
    <description>Create a segwire network namespace</description>
    <message>Authentication is required to create a network namespace</message>
    <defaults>
      <allow_any>auth_admin</allow_any>
      <allow_inactive>auth_admin</allow_inactive>
      <allow_active>auth_admin_keep</allow_active>
    </defaults>
  </action>

  <action id="org.segwire.namespace.delete">
    <description>Delete a segwire network namespace</description>
    <message>Authentication is required to delete a network namespace</message>
    <defaults>
      <allow_any>auth_admin</allow_any>
      <allow_inactive>auth_admin</allow_inactive>
      <allow_active>auth_admin_keep</allow_active>
    </defaults>
  </action>

  <action id="org.segwire.namespace.manage">
    <description>Manage segwire daemon configuration</description>
    <message>Authentication is required to manage daemon configuration</message>
    <defaults>
      <allow_any>auth_admin</allow_any>
      <allow_inactive>auth_admin</allow_inactive>
      <allow_active>auth_admin_keep</allow_active>
    </defaults>
  </action>

  <action id="org.segwire.namespace.exec">
    <description>Execute a command in a segwire network namespace</description>
    <message>Authentication is required to run a command in a network namespace</message>
    <defaults>
      <allow_any>auth_admin</allow_any>
      <allow_inactive>auth_admin</allow_inactive>
      <allow_active>auth_admin_keep</allow_active>
    </defaults>
  </action>
</policyconfig>
```

## Development / Testing

For development, set `SEGWIRE_NS_ENTER_PATH` to point to the local build:

```bash
# Without setuid (requires sudo for the actual exec)
SEGWIRE_NS_ENTER_PATH=./target/debug/segwire-ns-enter segwire exec my-ns -- ip link show

# With setuid (full unprivileged flow)
sudo chown root:$(id -gn) target/debug/segwire-ns-enter
sudo chmod 4750 target/debug/segwire-ns-enter
SEGWIRE_NS_ENTER_PATH=./target/debug/segwire-ns-enter segwire exec my-ns -- ip link show
```
