//! Output formatting and display utilities
//!
//! Provides table-based and TOML output formatting for CLI commands,
//! as well as progress display for long-running operations.

use anyhow::Result;
use serde::Serialize;
use std::io::{self, Write};
use tabled::settings::{object::Columns, Alignment, Modify, Style};
use tabled::{Table, Tabled};

/// Common output format enum used across commands
#[derive(clap::ValueEnum, Clone, Debug)]
pub enum OutputFormat {
    Table,
    Toml,
}

// ──────────────────────────────────────────────
// Table row types
// ──────────────────────────────────────────────

/// A row in the namespace list table
#[derive(Tabled, Serialize)]
pub struct NamespaceListRow {
    #[tabled(rename = "NAME")]
    pub name: String,
    #[tabled(rename = "STATUS")]
    pub status: String,
    #[tabled(rename = "INTERFACES")]
    pub interfaces: String,
    #[tabled(rename = "CONFIG")]
    pub config_path: String,
    #[tabled(rename = "DESCRIPTION")]
    pub description: String,
}

/// A row in the namespace status detail table
#[derive(Tabled, Serialize)]
pub struct NamespaceDetailRow {
    #[tabled(rename = "PROPERTY")]
    pub property: String,
    #[tabled(rename = "VALUE")]
    pub value: String,
}

/// A row in the interface table
#[derive(Tabled, Serialize)]
pub struct InterfaceRow {
    #[tabled(rename = "INTERFACE")]
    pub name: String,
    #[tabled(rename = "TYPE")]
    pub iface_type: String,
    #[tabled(rename = "STATUS")]
    pub status: String,
    #[tabled(rename = "ADDRESSES")]
    pub addresses: String,
}

/// A row in the route table
#[derive(Tabled, Serialize)]
pub struct RouteRow {
    #[tabled(rename = "DESTINATION")]
    pub destination: String,
    #[tabled(rename = "GATEWAY")]
    pub gateway: String,
    #[tabled(rename = "METRIC")]
    pub metric: u32,
    #[tabled(rename = "INTERFACE")]
    pub interface: String,
}

/// Daemon status information
#[derive(Serialize)]
pub struct DaemonStatusInfo {
    pub version: String,
    pub uptime_secs: u64,
    pub managed_namespaces: u32,
    pub active_namespaces: u32,
}

// ──────────────────────────────────────────────
// Table rendering
// ──────────────────────────────────────────────

/// Render a `tabled` table with a clean, modern style.
pub fn render_table<T: Tabled>(rows: &[T]) -> String {
    if rows.is_empty() {
        return "(no results)".to_string();
    }
    Table::new(rows)
        .with(Style::rounded())
        .with(Modify::new(Columns::first()).with(Alignment::left()))
        .to_string()
}

// ──────────────────────────────────────────────
// Namespace list output helpers
// ──────────────────────────────────────────────

/// Serializable namespace list for TOML output
#[derive(Serialize)]
struct NamespaceListOutput {
    total: usize,
    namespaces: Vec<NamespaceListItem>,
}

#[derive(Serialize)]
struct NamespaceListItem {
    name: String,
    status: String,
    config_path: String,
    description: String,
}

/// Format namespace list data from D-Bus tuples (name, status, config_path, description).
pub fn format_namespace_list(
    namespaces: &[(String, String, String, String)],
    format: &OutputFormat,
    verbose: bool,
) -> Result<()> {
    match format {
        OutputFormat::Table => {
            if namespaces.is_empty() {
                println!("No managed namespaces found.");
                return Ok(());
            }
            let rows: Vec<NamespaceListRow> = namespaces
                .iter()
                .map(|(name, status, config, desc)| NamespaceListRow {
                    name: name.clone(),
                    status: colorize_status(status),
                    interfaces: "-".to_string(), // Summary count not available in list tuple
                    config_path: if verbose {
                        config.clone()
                    } else {
                        short_path(config)
                    },
                    description: desc.clone(),
                })
                .collect();
            println!("{}", render_table(&rows));
            println!();
            println!("Total: {} namespace(s)", namespaces.len());
        }
        OutputFormat::Toml => {
            let output = NamespaceListOutput {
                total: namespaces.len(),
                namespaces: namespaces
                    .iter()
                    .map(|(name, status, config, desc)| NamespaceListItem {
                        name: name.clone(),
                        status: status.clone(),
                        config_path: config.clone(),
                        description: desc.clone(),
                    })
                    .collect(),
            };
            println!("{}", toml::to_string_pretty(&output)?);
        }
    }
    Ok(())
}

// ──────────────────────────────────────────────
// Namespace status output helpers
// ──────────────────────────────────────────────

/// Full namespace status information for rendering.
#[derive(Serialize)]
pub struct NamespaceStatusData {
    pub name: String,
    pub full_name: String,
    pub status: String,
    pub config_path: String,
    pub created_at: u64,
    pub last_updated: u64,
    pub interfaces: Vec<InterfaceData>,
    pub routes: Vec<RouteData>,
    pub dns_servers: Vec<String>,
    pub dns_search_domains: Vec<String>,
}

#[derive(Serialize)]
pub struct InterfaceData {
    pub name: String,
    pub iface_type: String,
    pub status: String,
    pub addresses: Vec<String>,
}

#[derive(Serialize)]
pub struct RouteData {
    pub destination: String,
    pub gateway: String,
    pub metric: u32,
    pub interface: String,
}

/// Format detailed namespace status output.
pub fn format_namespace_status(
    data: &NamespaceStatusData,
    format: &OutputFormat,
    detailed: bool,
) -> Result<()> {
    match format {
        OutputFormat::Table => {
            println!("Namespace: {}", data.name);
            println!();

            // Properties table
            let mut props = vec![
                NamespaceDetailRow {
                    property: "Full Name".to_string(),
                    value: data.full_name.clone(),
                },
                NamespaceDetailRow {
                    property: "Status".to_string(),
                    value: colorize_status(&data.status),
                },
                NamespaceDetailRow {
                    property: "Config File".to_string(),
                    value: data.config_path.clone(),
                },
                NamespaceDetailRow {
                    property: "Created".to_string(),
                    value: format_timestamp(data.created_at),
                },
                NamespaceDetailRow {
                    property: "Last Updated".to_string(),
                    value: format_timestamp(data.last_updated),
                },
            ];

            if !data.dns_servers.is_empty() {
                props.push(NamespaceDetailRow {
                    property: "DNS Servers".to_string(),
                    value: data.dns_servers.join(", "),
                });
            }
            if !data.dns_search_domains.is_empty() {
                props.push(NamespaceDetailRow {
                    property: "DNS Search".to_string(),
                    value: data.dns_search_domains.join(", "),
                });
            }

            println!("{}", render_table(&props));

            if detailed {
                // Interfaces table
                if !data.interfaces.is_empty() {
                    println!();
                    println!("Interfaces:");
                    let iface_rows: Vec<InterfaceRow> = data
                        .interfaces
                        .iter()
                        .map(|i| InterfaceRow {
                            name: i.name.clone(),
                            iface_type: i.iface_type.clone(),
                            status: colorize_status(&i.status),
                            addresses: i.addresses.join(", "),
                        })
                        .collect();
                    println!("{}", render_table(&iface_rows));
                }

                // Routes table
                if !data.routes.is_empty() {
                    println!();
                    println!("Routes:");
                    let route_rows: Vec<RouteRow> = data
                        .routes
                        .iter()
                        .map(|r| RouteRow {
                            destination: r.destination.clone(),
                            gateway: r.gateway.clone(),
                            metric: r.metric,
                            interface: r.interface.clone(),
                        })
                        .collect();
                    println!("{}", render_table(&route_rows));
                }
            }
        }
        OutputFormat::Toml => {
            println!("{}", toml::to_string_pretty(data)?);
        }
    }
    Ok(())
}

/// Serializable all-namespaces summary for TOML output
#[derive(Serialize)]
struct AllNamespacesOutput {
    total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon: Option<DaemonStatusToml>,
    namespaces: Vec<NamespaceListItem>,
}

#[derive(Serialize)]
struct DaemonStatusToml {
    version: String,
    uptime_secs: u64,
    managed_namespaces: u32,
    active_namespaces: u32,
}

/// Format the all-namespace summary (used by `segwire status` with no args).
pub fn format_all_namespaces_status(
    namespaces: &[(String, String, String, String)],
    daemon_status: Option<&DaemonStatusInfo>,
    format: &OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Table => {
            if let Some(ds) = daemon_status {
                println!("Daemon Status");
                let ds_rows = vec![
                    NamespaceDetailRow {
                        property: "Version".to_string(),
                        value: ds.version.clone(),
                    },
                    NamespaceDetailRow {
                        property: "Uptime".to_string(),
                        value: format_duration(ds.uptime_secs),
                    },
                    NamespaceDetailRow {
                        property: "Managed".to_string(),
                        value: ds.managed_namespaces.to_string(),
                    },
                    NamespaceDetailRow {
                        property: "Active".to_string(),
                        value: ds.active_namespaces.to_string(),
                    },
                ];
                println!("{}", render_table(&ds_rows));
                println!();
            }

            format_namespace_list(namespaces, format, false)?;
        }
        OutputFormat::Toml => {
            let output = AllNamespacesOutput {
                total: namespaces.len(),
                daemon: daemon_status.map(|ds| DaemonStatusToml {
                    version: ds.version.clone(),
                    uptime_secs: ds.uptime_secs,
                    managed_namespaces: ds.managed_namespaces,
                    active_namespaces: ds.active_namespaces,
                }),
                namespaces: namespaces
                    .iter()
                    .map(|(name, status, config, desc)| NamespaceListItem {
                        name: name.clone(),
                        status: status.clone(),
                        config_path: config.clone(),
                        description: desc.clone(),
                    })
                    .collect(),
            };
            println!("{}", toml::to_string_pretty(&output)?);
        }
    }
    Ok(())
}

// ──────────────────────────────────────────────
// Progress display
// ──────────────────────────────────────────────

/// Display a progress bar for long-running operations.
pub struct ProgressDisplay {
    pub operation: String,
    pub total_width: usize,
}

impl ProgressDisplay {
    pub fn new(operation: &str) -> Self {
        Self {
            operation: operation.to_string(),
            total_width: 40,
        }
    }

    /// Update the progress bar.
    /// `progress` is a value between 0.0 and 1.0.
    pub fn update(&self, progress: f64, message: &str) {
        let filled = (progress * self.total_width as f64) as usize;
        let empty = self.total_width - filled;

        let bar: String = "█".repeat(filled) + &"░".repeat(empty);
        let pct = (progress * 100.0) as u32;

        print!(
            "\r{}: [{}] {:>3}% {}",
            self.operation,
            bar,
            pct,
            truncate_string(message, 30)
        );
        let _ = io::stdout().flush();
    }

    /// Mark the operation as complete.
    pub fn finish(&self, message: &str) {
        let bar = "█".repeat(self.total_width);
        println!("\r{}: [{}] 100% {}", self.operation, bar, message);
    }

    /// Mark the operation as failed.
    pub fn fail(&self, message: &str) {
        println!("\r{}: ✗ {}", self.operation, message);
    }
}

// ──────────────────────────────────────────────
// Operation result formatting
// ──────────────────────────────────────────────

/// Serializable operation result for TOML output
#[derive(Serialize)]
struct OperationResultOutput {
    success: bool,
    message: String,
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    details: std::collections::HashMap<String, String>,
}

/// Format an operation result (success / failure) from D-Bus response.
pub fn format_operation_result(
    success: bool,
    message: &str,
    details: &std::collections::HashMap<String, String>,
    format: &OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Table => {
            if success {
                println!("✓ {}", message);
            } else {
                eprintln!("✗ {}", message);
            }
            if !details.is_empty() {
                println!();
                let rows: Vec<NamespaceDetailRow> = details
                    .iter()
                    .map(|(k, v)| NamespaceDetailRow {
                        property: k.clone(),
                        value: v.clone(),
                    })
                    .collect();
                println!("{}", render_table(&rows));
            }
        }
        OutputFormat::Toml => {
            let output = OperationResultOutput {
                success,
                message: message.to_string(),
                details: details.clone(),
            };
            println!("{}", toml::to_string_pretty(&output)?);
        }
    }
    Ok(())
}

// ──────────────────────────────────────────────
// Utility helpers
// ──────────────────────────────────────────────

/// Add ANSI color to status strings for terminal display.
pub fn colorize_status(status: &str) -> String {
    match status.to_lowercase().as_str() {
        "active" | "up" | "running" => format!("\x1b[32m{}\x1b[0m", status), // Green
        "inactive" | "down" | "stopped" => format!("\x1b[31m{}\x1b[0m", status), // Red
        "error" | "failed" => format!("\x1b[91m{}\x1b[0m", status),          // Bright red
        "creating" | "starting" | "pending" => format!("\x1b[33m{}\x1b[0m", status), // Yellow
        _ => status.to_string(),
    }
}

/// Shorten a file path for display (show only last 2 components).
fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() > 2 {
        format!("…/{}", parts[parts.len() - 2..].join("/"))
    } else {
        path.to_string()
    }
}

/// Format a Unix timestamp as a human-readable string.
fn format_timestamp(unix_secs: u64) -> String {
    if unix_secs == 0 {
        return "-".to_string();
    }

    match time::OffsetDateTime::from_unix_timestamp(unix_secs as i64) {
        Ok(dt) => {
            let format = time::macros::format_description!(
                "[year]-[month padding:zero]-[day padding:zero] [hour padding:zero]:[minute padding:zero]:[second padding:zero] UTC"
            );
            dt.format(&format).unwrap_or_else(|_| "-".to_string())
        }
        Err(_) => "-".to_string(),
    }
}

/// Format a duration in seconds as a human-friendly string.
fn format_duration(secs: u64) -> String {
    if secs < 60 {
        return format!("{}s", secs);
    }
    let minutes = secs / 60;
    let remaining_secs = secs % 60;
    if minutes < 60 {
        return format!("{}m {}s", minutes, remaining_secs);
    }
    let hours = minutes / 60;
    let remaining_mins = minutes % 60;
    if hours < 24 {
        return format!("{}h {}m", hours, remaining_mins);
    }
    let days = hours / 24;
    let remaining_hours = hours % 24;
    format!("{}d {}h", days, remaining_hours)
}

/// Truncate a string to a maximum length, appending "…" if truncated.
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len.saturating_sub(1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(65), "1m 5s");
        assert_eq!(format_duration(3661), "1h 1m");
        assert_eq!(format_duration(90061), "1d 1h");
    }

    #[test]
    fn test_format_timestamp_zero() {
        assert_eq!(format_timestamp(0), "-");
    }

    #[test]
    fn test_format_timestamp_epoch() {
        // 1970-01-01 00:00:01 UTC
        let ts = format_timestamp(1);
        assert!(ts.starts_with("1970-01-01 00:00:01"));
    }

    #[test]
    fn test_short_path() {
        assert_eq!(
            short_path("/etc/segwire/namespaces/test.toml"),
            "…/namespaces/test.toml"
        );
        assert_eq!(short_path("test.toml"), "test.toml");
    }

    #[test]
    fn test_truncate_string() {
        assert_eq!(truncate_string("hello", 10), "hello");
        assert_eq!(truncate_string("hello world", 5), "hell…");
    }

    #[test]
    fn test_colorize_status_active() {
        let colored = colorize_status("active");
        assert!(colored.contains("active"));
        // Contains ANSI escape codes for green
        assert!(colored.contains("\x1b[32m"));
    }

    #[test]
    fn test_render_empty_table() {
        let rows: Vec<NamespaceListRow> = vec![];
        assert_eq!(render_table(&rows), "(no results)");
    }

    #[test]
    fn test_render_table_with_data() {
        let rows = vec![NamespaceListRow {
            name: "test-ns".to_string(),
            status: "active".to_string(),
            interfaces: "2".to_string(),
            config_path: "/etc/test.toml".to_string(),
            description: "Test namespace".to_string(),
        }];
        let output = render_table(&rows);
        assert!(output.contains("test-ns"));
        assert!(output.contains("active"));
    }

    #[test]
    fn test_progress_display() {
        let pd = ProgressDisplay::new("Test");
        assert_eq!(pd.total_width, 40);
        assert_eq!(pd.operation, "Test");
    }
}
