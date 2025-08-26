//! Segwire Common Library
//!
//! Shared types, utilities, and D-Bus interfaces for the segwire network namespace
//! management system. This crate provides common functionality used by both the
//! daemon and CLI components.

pub mod config;
pub mod dbus;
pub mod error;
pub mod logging;
pub mod netlink;
pub mod utils;

#[cfg(test)]
mod logging_test;

// Re-export commonly used types
pub use config::{DaemonConfig, NamespaceConfig};
pub use dbus::{NamespaceState, NamespaceStatus};
pub use error::{SegwireError, SegwireResult};
pub use logging::{init_logging, LogConfig, LogContext, LogLevel};
pub use netlink::{DnsConfig, NamespaceInfo, NetlinkError, NetlinkManager, RouteConfig};
