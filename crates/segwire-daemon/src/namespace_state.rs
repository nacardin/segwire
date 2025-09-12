//! Namespace state management for segwire-daemon
//!
//! This module provides in-memory state tracking for managed namespaces,
//! state synchronization between configuration and actual namespaces,
//! and conflict resolution for configuration changes.

use crate::config::{ConfigManager, NamespaceConfigEntry};
use segwire_common::{
    dbus::{NamespaceState, NamespaceStatus},
    error::{SegwireError, SegwireResult},
    netlink::NetlinkManager,
};
use std::collections::HashMap;

use std::time::{Duration, SystemTime};
use tracing::{debug, error, info, warn};

/// In-memory state manager for network namespaces
pub struct NamespaceStateManager {
    /// Current state of all managed namespaces
    namespace_states: HashMap<String, NamespaceState>,

    /// Netlink manager for actual namespace operations
    netlink_manager: NetlinkManager,

    /// Timestamp of last state synchronization
    last_sync: SystemTime,

    /// Minimum interval between state synchronizations
    sync_interval: Duration,
}

/// Result of state synchronization operation
#[derive(Debug, Clone)]
pub struct SyncResult {
    /// Namespaces that were created
    pub created: Vec<String>,

    /// Namespaces that were updated
    pub updated: Vec<String>,

    /// Namespaces that were deleted
    pub deleted: Vec<String>,

    /// Namespaces that had conflicts
    pub conflicts: Vec<StateConflict>,

    /// Errors encountered during synchronization
    pub errors: Vec<String>,
}

/// Represents a conflict between configuration and actual state
#[derive(Debug, Clone)]
pub struct StateConflict {
    /// Name of the namespace with conflict
    pub namespace_name: String,

    /// Description of the conflict
    pub description: String,

    /// Suggested resolution action
    pub resolution: ConflictResolution,
}

/// Possible resolutions for conflicts
#[derive(Debug, Clone, PartialEq)]
pub enum ConflictResolution {
    /// Create the namespace from configuration
    CreateNamespace,

    /// Delete the namespace (no configuration)
    DeleteNamespace,
}

impl NamespaceStateManager {
    /// Create a new namespace state manager
    pub async fn new() -> SegwireResult<Self> {
        info!("Initializing namespace state manager");

        let netlink_manager = NetlinkManager::new().map_err(|e| {
            error!("Failed to initialize netlink manager: {}", e);
            e
        })?;

        Ok(Self {
            namespace_states: HashMap::new(),
            netlink_manager,
            last_sync: SystemTime::UNIX_EPOCH,
            sync_interval: Duration::from_secs(30), // Sync every 30 seconds
        })
    }

    /// Get the current state of all managed namespaces
    pub fn get_all_states(&self) -> &HashMap<String, NamespaceState> {
        &self.namespace_states
    }

    /// Get the state of a specific namespace
    pub fn get_namespace_state(&self, name: &str) -> Option<&NamespaceState> {
        self.namespace_states.get(name)
    }

    /// Get mutable reference to namespace state (for internal updates)
    fn get_namespace_state_mut(&mut self, name: &str) -> Option<&mut NamespaceState> {
        self.namespace_states.get_mut(name)
    }

    /// Add or update a namespace state
    pub fn update_namespace_state(&mut self, state: NamespaceState) {
        let name = state.name.clone();
        debug!("Updating namespace state: {}", name);
        self.namespace_states.insert(name, state);
    }

    /// Remove a namespace state
    pub fn remove_namespace_state(&mut self, name: &str) -> Option<NamespaceState> {
        debug!("Removing namespace state: {}", name);
        self.namespace_states.remove(name)
    }

    /// Check if state synchronization is needed
    pub fn needs_sync(&self) -> bool {
        let now = SystemTime::now();
        now.duration_since(self.last_sync)
            .map(|duration| duration >= self.sync_interval)
            .unwrap_or(true)
    }

    /// Force a state synchronization regardless of timing
    pub async fn force_sync(
        &mut self,
        config_manager: &ConfigManager,
    ) -> SegwireResult<SyncResult> {
        info!("Forcing namespace state synchronization");
        self.synchronize_state(config_manager).await
    }

    /// Synchronize in-memory state with configuration and actual namespaces
    pub async fn synchronize_state(
        &mut self,
        config_manager: &ConfigManager,
    ) -> SegwireResult<SyncResult> {
        info!("Starting namespace state synchronization");

        let mut result = SyncResult {
            created: Vec::new(),
            updated: Vec::new(),
            deleted: Vec::new(),
            conflicts: Vec::new(),
            errors: Vec::new(),
        };

        // Get current configuration
        let namespace_configs = config_manager.namespace_configs();

        // Get actual namespaces from the system
        let actual_namespaces = match self.netlink_manager.list_namespaces() {
            Ok(namespaces) => namespaces,
            Err(e) => {
                error!("Failed to list actual namespaces: {}", e);
                result
                    .errors
                    .push(format!("Failed to list system namespaces: {}", e));
                return Ok(result);
            }
        };

        // Filter actual namespaces to only those managed by this daemon
        let managed_actual_namespaces: HashMap<String, _> = actual_namespaces
            .into_iter()
            .filter(|(name, _)| config_manager.matches_namespace_prefix(name))
            .collect();

        debug!(
            "Found {} configured namespaces and {} actual managed namespaces",
            namespace_configs.len(),
            managed_actual_namespaces.len()
        );

        // Process each configured namespace
        for (config_name, config_entry) in namespace_configs {
            match self
                .process_configured_namespace(config_name, config_entry, &managed_actual_namespaces)
                .await
            {
                Ok(action) => match action {
                    SyncAction::Created => result.created.push(config_name.clone()),
                    SyncAction::Updated => result.updated.push(config_name.clone()),
                    SyncAction::Conflict(conflict) => result.conflicts.push(conflict),
                    SyncAction::NoChange => {}
                },
                Err(e) => {
                    error!(
                        "Failed to process configured namespace {}: {}",
                        config_name, e
                    );
                    result
                        .errors
                        .push(format!("Failed to process {}: {}", config_name, e));
                }
            }
        }

        // Process actual namespaces that don't have configuration
        for actual_name in managed_actual_namespaces.keys() {
            if !namespace_configs.contains_key(actual_name) {
                debug!(
                    "Found actual namespace without configuration: {}",
                    actual_name
                );

                let conflict = StateConflict {
                    namespace_name: actual_name.clone(),
                    description: format!(
                        "Namespace '{}' exists in system but has no configuration file",
                        actual_name
                    ),
                    resolution: ConflictResolution::DeleteNamespace,
                };

                result.conflicts.push(conflict);
            }
        }

        // Clean up state entries for namespaces that no longer exist in config or system
        let mut to_remove = Vec::new();
        for state_name in self.namespace_states.keys() {
            if !namespace_configs.contains_key(state_name)
                && !managed_actual_namespaces.contains_key(state_name)
            {
                to_remove.push(state_name.clone());
            }
        }

        for name in to_remove {
            self.remove_namespace_state(&name);
            result.deleted.push(name);
        }

        // Update last sync time
        self.last_sync = SystemTime::now();

        info!("State synchronization complete: {} created, {} updated, {} deleted, {} conflicts, {} errors",
              result.created.len(), result.updated.len(), result.deleted.len(),
              result.conflicts.len(), result.errors.len());

        Ok(result)
    }

    /// Process a single configured namespace during synchronization
    async fn process_configured_namespace(
        &mut self,
        _config_name: &str,
        config_entry: &NamespaceConfigEntry,
        actual_namespaces: &HashMap<String, segwire_common::netlink::NamespaceInfo>,
    ) -> SegwireResult<SyncAction> {
        let full_name = &config_entry.full_name;

        // Check if namespace exists in the system
        let actual_info = actual_namespaces.get(full_name);

        // Get current state entry
        let current_state = self.namespace_states.get(full_name);

        match (current_state, actual_info) {
            // Namespace exists in state and system - check for updates
            (Some(state), Some(actual)) => {
                if self.needs_namespace_update(state, config_entry, actual) {
                    debug!("Namespace {} needs update", full_name);
                    self.update_existing_namespace(config_entry, actual).await
                } else {
                    // Just update the timestamp
                    if let Some(state) = self.get_namespace_state_mut(full_name) {
                        state.touch();
                    }
                    Ok(SyncAction::NoChange)
                }
            }

            // Namespace exists in state but not in system - conflict
            (Some(_state), None) => {
                warn!("Namespace {} exists in state but not in system", full_name);
                let conflict = StateConflict {
                    namespace_name: full_name.clone(),
                    description: format!(
                        "Namespace '{}' is tracked but doesn't exist in system",
                        full_name
                    ),
                    resolution: ConflictResolution::CreateNamespace,
                };
                Ok(SyncAction::Conflict(conflict))
            }

            // Namespace doesn't exist in state but exists in system - add to state
            (None, Some(actual)) => {
                debug!("Adding existing namespace {} to state tracking", full_name);
                let state = self.create_state_from_actual(config_entry, actual).await?;
                self.update_namespace_state(state);
                Ok(SyncAction::Updated)
            }

            // Namespace doesn't exist in state or system - create it
            (None, None) => {
                debug!("Creating new namespace {}", full_name);
                self.create_new_namespace(config_entry).await
            }
        }
    }

    /// Check if a namespace needs to be updated based on configuration changes
    fn needs_namespace_update(
        &self,
        current_state: &NamespaceState,
        config_entry: &NamespaceConfigEntry,
        actual_info: &segwire_common::netlink::NamespaceInfo,
    ) -> bool {
        // Check if configuration file was modified after last state update
        let config_modified = config_entry
            .last_modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        if config_modified > current_state.last_updated {
            debug!(
                "Configuration file newer than state for {}",
                current_state.name
            );
            return true;
        }

        // Check if actual namespace configuration differs from expected
        if self.has_configuration_drift(current_state, config_entry, actual_info) {
            debug!("Configuration drift detected for {}", current_state.name);
            return true;
        }

        false
    }

    /// Check if there's drift between expected and actual namespace configuration
    fn has_configuration_drift(
        &self,
        _current_state: &NamespaceState,
        _config_entry: &NamespaceConfigEntry,
        _actual_info: &segwire_common::netlink::NamespaceInfo,
    ) -> bool {
        // TODO: Implement detailed configuration drift detection
        // This would compare:
        // - Network interfaces in the namespace
        // - Routing table entries
        // - DNS configuration
        // For now, we'll assume no drift to keep implementation simple
        false
    }

    /// Create a new namespace from configuration
    async fn create_new_namespace(
        &mut self,
        config_entry: &NamespaceConfigEntry,
    ) -> SegwireResult<SyncAction> {
        let full_name = &config_entry.full_name;
        info!("Creating new namespace: {}", full_name);

        // Create initial state entry
        let mut state = NamespaceState::new(
            config_entry.config.namespace.name.clone(),
            full_name.clone(),
            config_entry.file_path.clone(),
        );
        state.set_status(NamespaceStatus::Creating);

        // Add to state tracking immediately
        self.update_namespace_state(state.clone());

        // Attempt to create the namespace
        match self.netlink_manager.create_namespace(full_name) {
            Ok(namespace_info) => {
                info!("Successfully created namespace: {}", full_name);

                // Update state with successful creation
                if let Some(state) = self.get_namespace_state_mut(full_name) {
                    state.set_status(NamespaceStatus::Active);
                    Self::update_state_from_config_and_actual(state, config_entry, &namespace_info);
                }

                Ok(SyncAction::Created)
            }
            Err(e) => {
                error!("Failed to create namespace {}: {}", full_name, e);

                // Update state with failure
                if let Some(state) = self.get_namespace_state_mut(full_name) {
                    state.set_status(NamespaceStatus::Failed);
                    state.status = format!("failed: {}", e);
                }

                Err(e)
            }
        }
    }

    /// Update an existing namespace to match configuration
    async fn update_existing_namespace(
        &mut self,
        config_entry: &NamespaceConfigEntry,
        actual_info: &segwire_common::netlink::NamespaceInfo,
    ) -> SegwireResult<SyncAction> {
        let full_name = &config_entry.full_name;
        info!("Updating existing namespace: {}", full_name);

        // TODO: Implement namespace configuration updates
        // This would involve:
        // 1. Comparing current configuration with desired configuration
        // 2. Applying necessary changes (interfaces, routes, DNS)
        // 3. Handling rollback on failure

        // For now, just update the state timestamp and mark as updated
        if let Some(state) = self.get_namespace_state_mut(full_name) {
            state.touch();
            Self::update_state_from_config_and_actual(state, config_entry, actual_info);
        }

        Ok(SyncAction::Updated)
    }

    /// Create state entry from existing namespace
    async fn create_state_from_actual(
        &self,
        config_entry: &NamespaceConfigEntry,
        actual_info: &segwire_common::netlink::NamespaceInfo,
    ) -> SegwireResult<NamespaceState> {
        let mut state = NamespaceState::new(
            config_entry.config.namespace.name.clone(),
            config_entry.full_name.clone(),
            config_entry.file_path.clone(),
        );

        state.set_status(NamespaceStatus::Active);
        Self::update_state_from_config_and_actual(&mut state, config_entry, actual_info);

        Ok(state)
    }

    /// Update state information from configuration and actual namespace data
    fn update_state_from_config_and_actual(
        state: &mut NamespaceState,
        config_entry: &NamespaceConfigEntry,
        _actual_info: &segwire_common::netlink::NamespaceInfo,
    ) {
        // TODO: Query actual interfaces and routes from the namespace
        // For now, we'll populate from configuration since the NamespaceInfo
        // structure doesn't contain detailed interface/route information

        // Update interfaces from configuration
        let mut interfaces = Vec::new();

        // Add moved interfaces
        for interface_name in &config_entry.config.interfaces.move_interfaces {
            interfaces.push(segwire_common::dbus::InterfaceInfo {
                name: interface_name.clone(),
                interface_type: "physical".to_string(),
                status: "unknown".to_string(), // Would need to query actual status
                addresses: Vec::new(),         // Would need to query actual addresses
            });
        }

        // Add virtual interfaces
        for vif in &config_entry.config.interfaces.virtual_interfaces {
            interfaces.push(segwire_common::dbus::InterfaceInfo {
                name: vif.name.clone(),
                interface_type: vif.interface_type.clone(),
                status: "unknown".to_string(), // Would need to query actual status
                addresses: Vec::new(),         // Would need to query actual addresses
            });
        }

        state.interfaces = interfaces;

        // Update routes from configuration
        state.routes = config_entry
            .config
            .routing
            .routes
            .iter()
            .map(|route| {
                segwire_common::dbus::RouteInfo {
                    destination: route.destination.clone(),
                    gateway: route.gateway.clone(),
                    metric: route.metric.unwrap_or(0),
                    interface: "unknown".to_string(), // Would need to determine from routing table
                }
            })
            .collect();

        // Add default gateway route if configured
        if let Some(ref gateway) = config_entry.config.routing.default_gateway {
            state.routes.push(segwire_common::dbus::RouteInfo {
                destination: "default".to_string(),
                gateway: gateway.clone(),
                metric: 0,
                interface: "unknown".to_string(),
            });
        }

        // Update DNS configuration from config
        state.dns_config = segwire_common::dbus::DnsInfo {
            servers: config_entry.config.dns.servers.clone(),
            search_domains: config_entry.config.dns.search.clone(),
        };

        state.touch();
    }

    /// Resolve conflicts by applying the suggested resolution
    pub async fn resolve_conflict(
        &mut self,
        conflict: &StateConflict,
        config_manager: &ConfigManager,
    ) -> SegwireResult<()> {
        info!(
            "Resolving conflict for namespace {}: {:?}",
            conflict.namespace_name, conflict.resolution
        );

        match conflict.resolution {
            ConflictResolution::CreateNamespace => {
                if let Some(config_entry) =
                    config_manager.get_namespace_config(&conflict.namespace_name)
                {
                    self.create_new_namespace(config_entry).await?;
                } else {
                    return Err(SegwireError::Config(
                        segwire_common::error::ConfigError::InvalidValue {
                            field: "namespace_name".to_string(),
                            value: format!(
                                "No configuration found for {}",
                                conflict.namespace_name
                            ),
                        },
                    ));
                }
            }

            ConflictResolution::DeleteNamespace => {
                info!(
                    "Deleting namespace without configuration: {}",
                    conflict.namespace_name
                );
                match self
                    .netlink_manager
                    .delete_namespace(&conflict.namespace_name)
                {
                    Ok(_) => {
                        self.remove_namespace_state(&conflict.namespace_name);
                        info!(
                            "Successfully deleted namespace: {}",
                            conflict.namespace_name
                        );
                    }
                    Err(e) => {
                        error!(
                            "Failed to delete namespace {}: {}",
                            conflict.namespace_name, e
                        );
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }

    /// Get statistics about current state
    pub fn get_state_stats(&self) -> StateStats {
        let mut stats = StateStats {
            total_namespaces: self.namespace_states.len(),
            active_namespaces: 0,
            creating_namespaces: 0,
            failed_namespaces: 0,
            deleting_namespaces: 0,
            _last_sync: self.last_sync,
        };

        for state in self.namespace_states.values() {
            match state.status.as_str() {
                "active" => stats.active_namespaces += 1,
                "creating" => stats.creating_namespaces += 1,
                "deleting" => stats.deleting_namespaces += 1,
                s if s.starts_with("failed") => stats.failed_namespaces += 1,
                _ => {}
            }
        }

        stats
    }

    /// Perform periodic maintenance tasks
    pub async fn perform_maintenance(&mut self) -> SegwireResult<()> {
        debug!("Performing namespace state maintenance");

        // Clean up old failed states (older than 1 hour)
        let cutoff_time = SystemTime::now() - Duration::from_secs(3600);
        let mut to_remove = Vec::new();

        for (name, state) in &self.namespace_states {
            let state_time = SystemTime::UNIX_EPOCH + Duration::from_secs(state.last_updated);
            if state.is_failed() && state_time < cutoff_time {
                to_remove.push(name.clone());
            }
        }

        for name in to_remove {
            info!("Cleaning up old failed state: {}", name);
            self.remove_namespace_state(&name);
        }

        Ok(())
    }
}

/// Action taken during state synchronization
#[derive(Debug, Clone)]
enum SyncAction {
    Created,
    Updated,
    Conflict(StateConflict),
    NoChange,
}

/// Statistics about namespace state
#[derive(Debug, Clone)]
pub struct StateStats {
    pub total_namespaces: usize,
    pub active_namespaces: usize,
    pub creating_namespaces: usize,
    pub failed_namespaces: usize,
    pub deleting_namespaces: usize,
    pub _last_sync: SystemTime,
}

// Note: Default implementation removed since new() is now async

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NamespaceConfigEntry;
    use tempfile::TempDir;
    #[allow(dead_code)]
    fn create_test_config_entry(name: &str, temp_dir: &TempDir) -> NamespaceConfigEntry {
        let config_content = format!(
            r#"
[namespace]
name = "{}"
description = "Test namespace"

[interfaces]
move_interfaces = ["eth0"]

[routing]
default_gateway = "192.168.1.1"

[dns]
servers = ["8.8.8.8"]
"#,
            name
        );

        let config_path = temp_dir.path().join(format!("{}.toml", name));
        std::fs::write(&config_path, config_content).expect("Failed to write test config");

        let config = segwire_common::config::NamespaceConfig::from_file(&config_path)
            .expect("Failed to parse test config");

        NamespaceConfigEntry {
            config,
            file_path: config_path,
            full_name: format!("test-{}", name),
            last_modified: SystemTime::now(),
        }
    }
    #[monoio::test]
    async fn test_sync_timing() {
        // Test sync timing logic without creating actual netlink manager
        let old_time = SystemTime::now() - Duration::from_secs(60); // 1 minute ago
        let recent_time = SystemTime::now() - Duration::from_secs(10); // 10 seconds ago
        let sync_interval = Duration::from_secs(30); // 30 second interval

        // Should need sync since last sync was 60 seconds ago and interval is 30 seconds
        let needs_sync_old = SystemTime::now()
            .duration_since(old_time)
            .map(|duration| duration >= sync_interval)
            .unwrap_or(true);
        assert!(needs_sync_old);

        // Should not need sync since last sync was only 10 seconds ago
        let needs_sync_recent = SystemTime::now()
            .duration_since(recent_time)
            .map(|duration| duration >= sync_interval)
            .unwrap_or(true);
        assert!(!needs_sync_recent);
    }

    #[monoio::test]
    async fn test_state_stats() {
        // Create a mock manager for testing
        let netlink_manager = match NetlinkManager::new() {
            Ok(manager) => manager,
            Err(_) => return, // Skip test if netlink is not available
        };

        let mut manager = NamespaceStateManager {
            namespace_states: HashMap::new(),
            netlink_manager,
            last_sync: SystemTime::now(),
            sync_interval: Duration::from_secs(30),
        };

        // Add some test states
        let mut state1 = NamespaceState::new(
            "test1".to_string(),
            "test-test1".to_string(),
            std::path::PathBuf::from("/tmp/test1.toml"),
        );
        state1.set_status(NamespaceStatus::Active);
        manager.update_namespace_state(state1);

        let mut state2 = NamespaceState::new(
            "test2".to_string(),
            "test-test2".to_string(),
            std::path::PathBuf::from("/tmp/test2.toml"),
        );
        state2.set_status(NamespaceStatus::Failed);
        manager.update_namespace_state(state2);

        let stats = manager.get_state_stats();

        assert_eq!(stats.active_namespaces, 1);
        assert_eq!(stats.failed_namespaces, 1);
        assert_eq!(stats.creating_namespaces, 0);
        assert_eq!(stats.deleting_namespaces, 0);
    }

    #[monoio::test]
    async fn test_needs_namespace_update() {
        let netlink_manager = match NetlinkManager::new() {
            Ok(manager) => manager,
            Err(_) => return, // Skip test if netlink is not available
        };

        let manager = NamespaceStateManager {
            namespace_states: HashMap::new(),
            netlink_manager,
            last_sync: SystemTime::now(),
            sync_interval: Duration::from_secs(30),
        };

        let temp_dir = TempDir::new().unwrap();
        let config_entry = create_test_config_entry("app1", &temp_dir);

        let mut current_state = NamespaceState::new(
            "app1".to_string(),
            "test-app1".to_string(),
            std::path::PathBuf::from("/tmp/app1.toml"),
        );

        // State is newer than config
        current_state.last_updated = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 10;

        let actual_info = segwire_common::netlink::NamespaceInfo {
            name: "test-app1".to_string(),
            id: 1234,
            path: std::path::PathBuf::from("/var/run/netns/test-app1"),
            active: true,
        };

        // Should be false because state is newer and no drift is implemented
        assert!(!manager.needs_namespace_update(&current_state, &config_entry, &actual_info));

        // State is older than config
        current_state.last_updated = config_entry
            .last_modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            - 60;

        // Should be true because config is newer
        assert!(manager.needs_namespace_update(&current_state, &config_entry, &actual_info));
    }

    #[monoio::test]
    async fn test_resolve_conflict_delete() {
        let netlink_manager = match NetlinkManager::new() {
            Ok(manager) => manager,
            Err(_) => return, // Skip test if netlink is not available
        };

        let mut state_manager = NamespaceStateManager {
            namespace_states: HashMap::new(),
            netlink_manager,
            last_sync: SystemTime::now(),
            sync_interval: Duration::from_secs(30),
        };

        let temp_dir = TempDir::new().unwrap();
        let config_manager = ConfigManager::new(temp_dir.path().to_path_buf()).unwrap();

        // We'll test the delete resolution which doesn't need a valid config entry
        let conflict = StateConflict {
            namespace_name: "test-nonexistent".to_string(),
            description: "Test conflict delete".to_string(),
            resolution: ConflictResolution::DeleteNamespace,
        };

        // Add a dummy state that should be removed
        let state = NamespaceState::new(
            "nonexistent".to_string(),
            "test-nonexistent".to_string(),
            std::path::PathBuf::from("/tmp/nonexistent.toml"),
        );
        state_manager.update_namespace_state(state.clone());
        assert!(state_manager
            .get_namespace_state("test-nonexistent")
            .is_some());

        // This might fail at the netlink level if test-nonexistent doesn't exist,
        // but let's assert what happens. NetlinkManager returns an error for non-existent namespace.
        let result = state_manager
            .resolve_conflict(&conflict, &config_manager)
            .await;

        // It returns an IO error from netlink delete.
        assert!(result.is_err());

        // State should still be there because delete failed
        assert!(state_manager
            .get_namespace_state("test-nonexistent")
            .is_some());
    }
    #[monoio::test]
    async fn test_state_operations() {
        // Create a mock manager for testing
        let netlink_manager = match NetlinkManager::new() {
            Ok(manager) => manager,
            Err(_) => {
                // In test environment, create a minimal mock
                return; // Skip test if netlink is not available
            }
        };

        let mut manager = NamespaceStateManager {
            namespace_states: HashMap::new(),
            netlink_manager,
            last_sync: SystemTime::UNIX_EPOCH,
            sync_interval: Duration::from_secs(30),
        };

        // Test adding state
        let state = NamespaceState::new(
            "test".to_string(),
            "test-namespace".to_string(),
            std::path::PathBuf::from("/tmp/test.toml"),
        );

        manager.update_namespace_state(state.clone());
        assert_eq!(manager.namespace_states.len(), 1);
        assert!(manager.get_namespace_state("test-namespace").is_some());

        // Test removing state
        let removed = manager.remove_namespace_state("test-namespace");
        assert!(removed.is_some());
        assert_eq!(manager.namespace_states.len(), 0);
    }
}
