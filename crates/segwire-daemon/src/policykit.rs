//! PolicyKit integration for D-Bus authorization
//!
//! Provides authorization checking for D-Bus method calls using PolicyKit,
//! ensuring that only authorized users can perform privileged operations.

use dbus::blocking::Connection;
use segwire_common::error::SegwireError;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, warn};

/// PolicyKit authorization checker
pub struct PolicyKitAuthorizer {
    connection: Connection,
    action_mappings: HashMap<String, String>,
}

impl PolicyKitAuthorizer {
    /// Create a new PolicyKit authorizer
    ///
    /// Clones the underlying connection handle so the authorizer
    /// can make its own D-Bus calls without borrowing the service connection.
    pub fn new(_connection: &Connection) -> Result<Self, SegwireError> {
        let mut action_mappings = HashMap::new();

        // Map D-Bus method names to PolicyKit actions
        action_mappings.insert("list".to_string(), actions::STATUS.to_string());
        action_mappings.insert("status".to_string(), actions::STATUS.to_string());
        action_mappings.insert("create".to_string(), actions::CREATE.to_string());
        action_mappings.insert("delete".to_string(), actions::DELETE.to_string());
        action_mappings.insert("restart".to_string(), actions::DELETE.to_string());
        action_mappings.insert("reload".to_string(), actions::MANAGE.to_string());
        action_mappings.insert("validate".to_string(), actions::STATUS.to_string());

        // Open a separate connection for authorization calls so we don't
        // interfere with the main service connection's message dispatch.
        let auth_conn = if std::env::var("SEGWIRE_SIMULATION").is_ok() {
            Connection::new_session()
        } else {
            Connection::new_system()
        }
        .map_err(|e| SegwireError::DBus(format!("Failed to open auth D-Bus connection: {}", e)))?;

        Ok(Self {
            connection: auth_conn,
            action_mappings,
        })
    }

    pub fn check_authorization(&self, action: &str, sender: &str) -> Result<(), SegwireError> {
        debug!("Checking PolicyKit authorization for action: {}", action);

        // Get the PolicyKit action ID for this operation
        let action_id = self
            .action_mappings
            .get(action)
            .ok_or_else(|| SegwireError::Permission(format!("Unknown action: {}", action)))?;

        debug!("Checking authorization for action '{}'", action_id);

        // Get the sender's process info (PID, UID)
        let process_info = self.get_sender_process_info(sender)?;

        // Call PolicyKit to check authorization
        match self.call_policykit_check_authorization(&process_info, action_id) {
            Ok(AuthorizationResult::Authorized) => {
                debug!("Authorization granted for action '{}'", action);
                Ok(())
            }
            Ok(AuthorizationResult::NotAuthorized) => {
                warn!("Authorization denied for action '{}'", action_id);
                Err(SegwireError::Permission(format!(
                    "Not authorized to perform action: {}",
                    action
                )))
            }
            Ok(AuthorizationResult::AuthenticationRequired) => {
                warn!("Interactive authentication required for action '{}', but not supported by daemon", action_id);
                Err(SegwireError::Permission(format!(
                    "Interactive authentication required for action: {}",
                    action
                )))
            }
            Ok(AuthorizationResult::Failed(reason)) => {
                warn!(
                    "PolicyKit authorization failed for action '{}': {}",
                    action, reason
                );
                Err(SegwireError::Permission(reason))
            }
            Err(e) => {
                warn!(
                    "PolicyKit authorization error for action '{}': {}",
                    action, e
                );
                Err(e)
            }
        }
    }

    /// Get the process information for a D-Bus sender
    fn get_sender_process_info(&self, sender: &str) -> Result<ProcessInfo, SegwireError> {
        debug!("Getting process info for D-Bus sender: {}", sender);

        let proxy = self.connection.with_proxy(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            Duration::from_secs(5),
        );

        let (pid,): (u32,) = proxy
            .method_call(
                "org.freedesktop.DBus",
                "GetConnectionUnixProcessID",
                (sender,),
            )
            .map_err(|e| SegwireError::DBus(format!("Failed to get sender PID: {}", e)))?;

        let (uid,): (u32,) = proxy
            .method_call("org.freedesktop.DBus", "GetConnectionUnixUser", (sender,))
            .map_err(|e| SegwireError::DBus(format!("Failed to get sender UID: {}", e)))?;

        debug!("Sender '{}' has PID {} and UID {}", sender, pid, uid);

        Ok(ProcessInfo { pid, uid })
    }

    /// Call PolicyKit to check authorization.
    ///
    /// Uses the `org.freedesktop.PolicyKit1.Authority.CheckAuthorization`
    /// D-Bus method with a `unix-process` subject built from the caller's
    /// PID and start-time.  If the PolicyKit daemon is not reachable we
    /// fall back to a simple UID==0 check so that the daemon still works
    /// on systems without PolicyKit installed.
    fn call_policykit_check_authorization(
        &self,
        process_info: &ProcessInfo,
        action_id: &str,
    ) -> Result<AuthorizationResult, SegwireError> {
        debug!(
            "Calling PolicyKit CheckAuthorization for PID {} (UID {}) and action '{}'",
            process_info.pid, process_info.uid, action_id
        );

        let pk_proxy = self.connection.with_proxy(
            "org.freedesktop.PolicyKit1",
            "/org/freedesktop/PolicyKit1/Authority",
            Duration::from_secs(5),
        );

        // Build the "subject" struct:
        //   (sa{sv})  →  ("unix-process", { "pid" => u32, "start-time" => u64 })
        // start-time 0 tells polkit to look it up itself.
        let subject_kind = "unix-process";
        let subject_details: HashMap<&str, dbus::arg::Variant<Box<dyn dbus::arg::RefArg>>> =
            HashMap::from([
                (
                    "pid",
                    dbus::arg::Variant(Box::new(process_info.pid) as Box<dyn dbus::arg::RefArg>),
                ),
                (
                    "start-time",
                    dbus::arg::Variant(Box::new(0u64) as Box<dyn dbus::arg::RefArg>),
                ),
            ]);

        let details: HashMap<&str, &str> = HashMap::new();
        let flags: u32 = 0; // 0 = do not allow interactive auth
        let cancellation_id = "";

        let reply: Result<(bool, bool, HashMap<String, String>), dbus::Error> = pk_proxy
            .method_call(
                "org.freedesktop.PolicyKit1.Authority",
                "CheckAuthorization",
                (
                    (subject_kind, &subject_details),
                    action_id,
                    &details,
                    flags,
                    cancellation_id,
                ),
            );

        match reply {
            Ok((is_authorized, is_challenge, _details)) => {
                if is_authorized {
                    Ok(AuthorizationResult::Authorized)
                } else if is_challenge {
                    Ok(AuthorizationResult::AuthenticationRequired)
                } else {
                    Ok(AuthorizationResult::NotAuthorized)
                }
            }
            Err(e) => {
                warn!(
                    "PolicyKit CheckAuthorization call failed ({}), falling back to UID check",
                    e
                );
                Ok(self.uid_fallback(process_info))
            }
        }
    }

    /// Simple UID-based fallback when PolicyKit is not available.
    /// Only UID 0 (root) is authorized.
    fn uid_fallback(&self, process_info: &ProcessInfo) -> AuthorizationResult {
        if process_info.uid == 0 {
            debug!("UID fallback: root user authorized");
            AuthorizationResult::Authorized
        } else {
            warn!(
                "UID fallback: non-root UID {} denied (PolicyKit unavailable)",
                process_info.uid
            );
            AuthorizationResult::NotAuthorized
        }
    }
}

/// Process information for authorization checks
#[derive(Debug, Clone)]
struct ProcessInfo {
    pid: u32,
    uid: u32,
}

/// Result of a PolicyKit authorization check
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
enum AuthorizationResult {
    /// The action is authorized
    Authorized,
    /// The action is not authorized
    NotAuthorized,
    /// Authentication is required (interactive)
    AuthenticationRequired,
    /// The authorization check failed
    Failed(String),
}

/// PolicyKit action definitions
pub mod actions {
    /// View namespace status and list namespaces
    pub const STATUS: &str = "org.segwire.namespace.status";

    /// Create new namespaces
    pub const CREATE: &str = "org.segwire.namespace.create";

    /// Delete existing namespaces
    pub const DELETE: &str = "org.segwire.namespace.delete";

    /// Manage daemon configuration and reload
    pub const MANAGE: &str = "org.segwire.namespace.manage";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_mappings() {
        // Skip if system bus is not available (CI, containers, etc.)
        let connection = match Connection::new_system() {
            Ok(c) => c,
            Err(_) => return,
        };
        let authorizer = match PolicyKitAuthorizer::new(&connection) {
            Ok(a) => a,
            Err(_) => return,
        };

        // Test that all expected actions are mapped
        assert!(authorizer.action_mappings.contains_key("list"));
        assert!(authorizer.action_mappings.contains_key("status"));
        assert!(authorizer.action_mappings.contains_key("create"));
        assert!(authorizer.action_mappings.contains_key("delete"));
        assert!(authorizer.action_mappings.contains_key("restart"));
        assert!(authorizer.action_mappings.contains_key("reload"));
        assert!(authorizer.action_mappings.contains_key("validate"));

        // Test specific mappings
        assert_eq!(
            authorizer.action_mappings.get("create").unwrap(),
            "org.segwire.namespace.create"
        );
        assert_eq!(
            authorizer.action_mappings.get("delete").unwrap(),
            "org.segwire.namespace.delete"
        );
        assert_eq!(
            authorizer.action_mappings.get("status").unwrap(),
            "org.segwire.namespace.status"
        );
    }

    #[test]
    fn test_authorization_result() {
        assert_eq!(
            AuthorizationResult::Authorized,
            AuthorizationResult::Authorized
        );
        assert_ne!(
            AuthorizationResult::Authorized,
            AuthorizationResult::NotAuthorized
        );

        match AuthorizationResult::Failed("test".to_string()) {
            AuthorizationResult::Failed(msg) => assert_eq!(msg, "test"),
            _ => panic!("Expected Failed variant"),
        }
    }
}
