//! Segwire Common Library
//! 
//! Shared types, utilities, and D-Bus interfaces for the segwire network namespace
//! management system. This crate provides common functionality used by both the
//! daemon and CLI components.

pub mod config;
pub mod dbus;
pub mod error;
pub mod utils;

// Re-export commonly used types
pub use error::{SegwireError, SegwireResult};
pub use config::{DaemonConfig, NamespaceConfig};
pub use dbus::{NamespaceState, NamespaceStatus};