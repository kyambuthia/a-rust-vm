//! Workspace ownership, capabilities, provenance, and audit control plane.
//!
//! The control plane never stores credentials. A capability identifies an
//! allowed resource and action set; a host-side gatekeeper resolves it to a
//! concrete service credential outside the guest runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::runtime::{ResourceLimits, VmManager, VmManagerError, VmSummary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceState {
    Active,
    Suspended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityGrant {
    pub id: String,
    pub kind: String,
    pub resource: String,
    pub actions: BTreeSet<String>,
}

impl CapabilityGrant {
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        resource: impl Into<String>,
        actions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, ControlPlaneError> {
        let grant = Self {
            id: id.into(),
            kind: kind.into(),
            resource: resource.into(),
            actions: actions.into_iter().map(Into::into).collect(),
        };
        validate_name(&grant.id, "capability id")?;
        validate_non_empty(&grant.kind, "capability kind")?;
        validate_non_empty(&grant.resource, "capability resource")?;
        if grant.actions.is_empty() || grant.actions.iter().any(|action| action.trim().is_empty()) {
            return Err(ControlPlaneError::InvalidInput {
                field: "capability actions",
            });
        }
        Ok(grant)
    }

    pub fn allows(&self, action: &str) -> bool {
        self.actions.contains(action)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub sequence: u64,
    pub capability_id: String,
    pub resource: String,
    pub action: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    WorkspaceCreated,
    WorkspaceSuspended,
    WorkspaceResumed,
    CapabilityGranted,
    CapabilityRevoked,
    ResourceObserved,
    AccessDenied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub sequence: u64,
    pub workspace_id: String,
    pub actor: String,
    pub kind: AuditKind,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceView {
    pub id: String,
    pub owner: String,
    pub state: WorkspaceState,
    pub runtime: VmSummary,
    pub capabilities: Vec<CapabilityGrant>,
    pub observations: Vec<Observation>,
    pub audit: Vec<AuditEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceRecord {
    id: String,
    owner: String,
    state: WorkspaceState,
    capabilities: BTreeMap<String, CapabilityGrant>,
    observations: Vec<Observation>,
    audit: Vec<AuditEvent>,
}

#[derive(Debug)]
pub struct ControlPlane {
    workspaces: BTreeMap<(String, String), WorkspaceRecord>,
    runtimes: VmManager,
    next_sequence: u64,
}

pub const CONTROL_PLANE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlPlaneSnapshot {
    pub schema_version: u32,
    next_sequence: u64,
    workspaces: Vec<WorkspaceRecord>,
}

impl ControlPlane {
    pub fn new(max_workspaces: usize, runtime_limits: ResourceLimits) -> Self {
        Self {
            workspaces: BTreeMap::new(),
            runtimes: VmManager::with_max_instances(max_workspaces, runtime_limits),
            next_sequence: 0,
        }
    }

    pub fn snapshot(&self) -> ControlPlaneSnapshot {
        ControlPlaneSnapshot {
            schema_version: CONTROL_PLANE_SCHEMA_VERSION,
            next_sequence: self.next_sequence,
            workspaces: self.workspaces.values().cloned().collect(),
        }
    }

    pub fn restore(
        snapshot: ControlPlaneSnapshot,
        max_workspaces: usize,
        runtime_limits: ResourceLimits,
    ) -> Result<Self, ControlPlaneError> {
        if snapshot.schema_version != CONTROL_PLANE_SCHEMA_VERSION {
            return Err(ControlPlaneError::UnsupportedSchema {
                expected: CONTROL_PLANE_SCHEMA_VERSION,
                actual: snapshot.schema_version,
            });
        }
        let mut plane = Self::new(max_workspaces, runtime_limits);
        plane.next_sequence = snapshot.next_sequence;
        for record in snapshot.workspaces {
            let key = (record.owner.clone(), record.id.clone());
            if plane.workspaces.contains_key(&key) {
                return Err(ControlPlaneError::AlreadyExists {
                    owner: record.owner,
                    workspace_id: record.id,
                });
            }
            plane.runtimes.create(&record.owner, &record.id)?;
            plane.workspaces.insert(key, record);
        }
        Ok(plane)
    }

    pub fn create_workspace(&mut self, owner: &str, id: &str) -> Result<(), ControlPlaneError> {
        validate_name(owner, "owner")?;
        validate_name(id, "workspace id")?;
        let key = (owner.to_owned(), id.to_owned());
        if self.workspaces.contains_key(&key) {
            return Err(ControlPlaneError::AlreadyExists {
                owner: owner.to_owned(),
                workspace_id: id.to_owned(),
            });
        }
        self.runtimes.create(owner, id)?;
        let mut record = WorkspaceRecord {
            id: id.to_owned(),
            owner: owner.to_owned(),
            state: WorkspaceState::Active,
            capabilities: BTreeMap::new(),
            observations: Vec::new(),
            audit: Vec::new(),
        };
        let event = self.event(id, owner, AuditKind::WorkspaceCreated, "workspace created");
        record.audit.push(event);
        self.workspaces.insert(key, record);
        Ok(())
    }

    pub fn grant_capability(
        &mut self,
        owner: &str,
        workspace_id: &str,
        grant: CapabilityGrant,
    ) -> Result<(), ControlPlaneError> {
        let sequence = self.next_sequence();
        let record = self.record_mut(owner, workspace_id)?;
        if record.capabilities.contains_key(&grant.id) {
            return Err(ControlPlaneError::CapabilityAlreadyExists(grant.id));
        }
        let detail = format!("granted {} for {}", grant.id, grant.resource);
        record.capabilities.insert(grant.id.clone(), grant);
        record.audit.push(AuditEvent {
            sequence,
            workspace_id: workspace_id.to_owned(),
            actor: owner.to_owned(),
            kind: AuditKind::CapabilityGranted,
            detail,
        });
        Ok(())
    }

    pub fn revoke_capability(
        &mut self,
        owner: &str,
        workspace_id: &str,
        capability_id: &str,
    ) -> Result<(), ControlPlaneError> {
        let sequence = self.next_sequence();
        let record = self.record_mut(owner, workspace_id)?;
        if record.capabilities.remove(capability_id).is_none() {
            return Err(ControlPlaneError::CapabilityNotFound(
                capability_id.to_owned(),
            ));
        }
        record.audit.push(AuditEvent {
            sequence,
            workspace_id: workspace_id.to_owned(),
            actor: owner.to_owned(),
            kind: AuditKind::CapabilityRevoked,
            detail: format!("revoked {capability_id}"),
        });
        Ok(())
    }

    pub fn record_observation(
        &mut self,
        owner: &str,
        workspace_id: &str,
        capability_id: &str,
        resource: &str,
        action: &str,
    ) -> Result<Observation, ControlPlaneError> {
        validate_non_empty(resource, "observed resource")?;
        validate_non_empty(action, "capability action")?;
        let sequence = self.next_sequence();
        let record = self.record_mut(owner, workspace_id)?;
        if record.state != WorkspaceState::Active {
            return Err(ControlPlaneError::WorkspaceSuspended);
        }
        let Some(capability) = record.capabilities.get(capability_id) else {
            record.audit.push(denial_event(
                sequence,
                workspace_id,
                owner,
                capability_id,
                action,
            ));
            return Err(ControlPlaneError::CapabilityNotFound(
                capability_id.to_owned(),
            ));
        };
        if !capability.allows(action) || capability.resource != resource {
            record.audit.push(denial_event(
                sequence,
                workspace_id,
                owner,
                capability_id,
                action,
            ));
            return Err(ControlPlaneError::AccessDenied {
                capability_id: capability_id.to_owned(),
                resource: resource.to_owned(),
                action: action.to_owned(),
            });
        }
        let observation = Observation {
            sequence,
            capability_id: capability_id.to_owned(),
            resource: resource.to_owned(),
            action: action.to_owned(),
        };
        record.observations.push(observation.clone());
        record.audit.push(AuditEvent {
            sequence,
            workspace_id: workspace_id.to_owned(),
            actor: owner.to_owned(),
            kind: AuditKind::ResourceObserved,
            detail: format!("{action} {resource} via {capability_id}"),
        });
        Ok(observation)
    }

    pub fn inspect(
        &self,
        owner: &str,
        workspace_id: &str,
    ) -> Result<WorkspaceView, ControlPlaneError> {
        let record = self.record(owner, workspace_id)?;
        let runtime = self
            .runtimes
            .list(owner)
            .into_iter()
            .find(|vm| vm.id == workspace_id)
            .ok_or_else(|| ControlPlaneError::NotFound {
                owner: owner.to_owned(),
                workspace_id: workspace_id.to_owned(),
            })?;
        Ok(WorkspaceView {
            id: record.id.clone(),
            owner: record.owner.clone(),
            state: record.state,
            runtime,
            capabilities: record.capabilities.values().cloned().collect(),
            observations: record.observations.clone(),
            audit: record.audit.clone(),
        })
    }

    pub fn list(&self, owner: &str) -> Vec<WorkspaceView> {
        self.workspaces
            .keys()
            .filter(|(record_owner, _)| record_owner == owner)
            .filter_map(|(_, id)| self.inspect(owner, id).ok())
            .collect()
    }

    fn record(&self, owner: &str, id: &str) -> Result<&WorkspaceRecord, ControlPlaneError> {
        self.workspaces
            .get(&(owner.to_owned(), id.to_owned()))
            .ok_or_else(|| ControlPlaneError::NotFound {
                owner: owner.to_owned(),
                workspace_id: id.to_owned(),
            })
    }

    fn record_mut(
        &mut self,
        owner: &str,
        id: &str,
    ) -> Result<&mut WorkspaceRecord, ControlPlaneError> {
        self.workspaces
            .get_mut(&(owner.to_owned(), id.to_owned()))
            .ok_or_else(|| ControlPlaneError::NotFound {
                owner: owner.to_owned(),
                workspace_id: id.to_owned(),
            })
    }

    fn next_sequence(&mut self) -> u64 {
        self.next_sequence += 1;
        self.next_sequence
    }

    fn event(
        &mut self,
        workspace_id: &str,
        actor: &str,
        kind: AuditKind,
        detail: &str,
    ) -> AuditEvent {
        AuditEvent {
            sequence: self.next_sequence(),
            workspace_id: workspace_id.to_owned(),
            actor: actor.to_owned(),
            kind,
            detail: detail.to_owned(),
        }
    }
}

fn denial_event(
    sequence: u64,
    workspace_id: &str,
    actor: &str,
    capability_id: &str,
    action: &str,
) -> AuditEvent {
    AuditEvent {
        sequence,
        workspace_id: workspace_id.to_owned(),
        actor: actor.to_owned(),
        kind: AuditKind::AccessDenied,
        detail: format!("denied {action} via {capability_id}"),
    }
}

fn validate_name(value: &str, field: &'static str) -> Result<(), ControlPlaneError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(ControlPlaneError::InvalidInput { field });
    }
    Ok(())
}

fn validate_non_empty(value: &str, field: &'static str) -> Result<(), ControlPlaneError> {
    if value.trim().is_empty() {
        Err(ControlPlaneError::InvalidInput { field })
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlPlaneError {
    InvalidInput {
        field: &'static str,
    },
    AlreadyExists {
        owner: String,
        workspace_id: String,
    },
    NotFound {
        owner: String,
        workspace_id: String,
    },
    CapabilityAlreadyExists(String),
    CapabilityNotFound(String),
    AccessDenied {
        capability_id: String,
        resource: String,
        action: String,
    },
    WorkspaceSuspended,
    Runtime(String),
    UnsupportedSchema {
        expected: u32,
        actual: u32,
    },
}

impl fmt::Display for ControlPlaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { field } => write!(f, "invalid {field}"),
            Self::AlreadyExists {
                owner,
                workspace_id,
            } => write!(f, "workspace already exists: {owner}/{workspace_id}"),
            Self::NotFound {
                owner,
                workspace_id,
            } => write!(f, "workspace not found: {owner}/{workspace_id}"),
            Self::CapabilityAlreadyExists(id) => write!(f, "capability already exists: {id}"),
            Self::CapabilityNotFound(id) => write!(f, "capability not found: {id}"),
            Self::AccessDenied {
                capability_id,
                resource,
                action,
            } => write!(
                f,
                "capability {capability_id} does not allow {action} on {resource}"
            ),
            Self::WorkspaceSuspended => f.write_str("workspace is suspended"),
            Self::Runtime(message) => f.write_str(message),
            Self::UnsupportedSchema { expected, actual } => write!(
                f,
                "unsupported control-plane schema: expected {expected}, got {actual}"
            ),
        }
    }
}

impl std::error::Error for ControlPlaneError {}

impl From<VmManagerError> for ControlPlaneError {
    fn from(error: VmManagerError) -> Self {
        Self::Runtime(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{CapabilityGrant, ControlPlane};
    use crate::runtime::ResourceLimits;

    #[test]
    fn workspaces_start_with_no_external_capabilities() {
        let mut plane = ControlPlane::new(4, ResourceLimits::default());
        plane.create_workspace("alice", "research").unwrap();
        let workspace = plane.inspect("alice", "research").unwrap();
        assert!(workspace.capabilities.is_empty());
        assert_eq!(workspace.audit.len(), 1);
    }

    #[test]
    fn capabilities_scope_resources_and_actions_and_audit_denials() {
        let mut plane = ControlPlane::new(4, ResourceLimits::default());
        plane.create_workspace("alice", "research").unwrap();
        let grant =
            CapabilityGrant::new("issues", "github", "repo:a/rvm", ["read_issues"]).unwrap();
        plane.grant_capability("alice", "research", grant).unwrap();
        plane
            .record_observation("alice", "research", "issues", "repo:a/rvm", "read_issues")
            .unwrap();
        assert!(
            plane
                .record_observation("alice", "research", "issues", "repo:a/rvm", "merge")
                .is_err()
        );
        let workspace = plane.inspect("alice", "research").unwrap();
        assert_eq!(workspace.observations.len(), 1);
        assert!(
            workspace
                .audit
                .iter()
                .any(|event| format!("{:?}", event.kind) == "AccessDenied")
        );
    }

    #[test]
    fn ownership_scopes_workspace_lookup() {
        let mut plane = ControlPlane::new(4, ResourceLimits::default());
        plane.create_workspace("alice", "shared-name").unwrap();
        plane.create_workspace("bob", "shared-name").unwrap();
        assert_eq!(plane.list("alice").len(), 1);
        assert_eq!(plane.list("bob").len(), 1);
        assert!(plane.inspect("mallory", "shared-name").is_err());
    }

    #[test]
    fn snapshots_restore_policy_and_audit_with_fresh_runtimes() {
        let limits = ResourceLimits::default();
        let mut plane = ControlPlane::new(4, limits);
        plane.create_workspace("alice", "research").unwrap();
        let grant =
            CapabilityGrant::new("issues", "github", "repo:a/rvm", ["read_issues"]).unwrap();
        plane.grant_capability("alice", "research", grant).unwrap();
        let restored = ControlPlane::restore(plane.snapshot(), 4, limits).unwrap();
        let workspace = restored.inspect("alice", "research").unwrap();
        assert_eq!(workspace.capabilities.len(), 1);
        assert_eq!(workspace.runtime.process_count, 0);
    }
}
