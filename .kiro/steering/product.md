# Product Overview

Segwire is a Linux network namespace management system that provides declarative configuration and management of network namespaces through a daemon-CLI architecture.

## Core Purpose

- **Declarative Configuration**: Administrators define desired namespace states in TOML files, similar to systemd-networkd
- **System Integration**: Uses D-Bus for IPC and PolicyKit for authorization
- **High Performance**: Built with Rust and monoio runtime using io_uring for efficient I/O operations

## Key Components

1. **segwire-daemon** - System daemon that manages network namespaces based on TOML configuration
2. **segwire-cli** - Command-line interface for interacting with the daemon via D-Bus
3. **segwire-common** - Shared library containing D-Bus types, configuration structures, and utilities

## Target Use Cases

- Isolated network environments for applications
- Network segmentation and security
- Development and testing environments
- Container-like network isolation without full containerization