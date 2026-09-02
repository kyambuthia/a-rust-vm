//! Crash-safe local persistence for control-plane metadata.

use std::fs;
use std::path::{Path, PathBuf};

use crate::control_plane::{ControlPlane, ControlPlaneError, ControlPlaneSnapshot};
use crate::runtime::ResourceLimits;

#[derive(Debug, Clone)]
pub struct ControlPlaneStore {
    path: PathBuf,
    max_workspaces: usize,
    runtime_limits: ResourceLimits,
}

impl ControlPlaneStore {
    pub fn new(
        path: impl Into<PathBuf>,
        max_workspaces: usize,
        runtime_limits: ResourceLimits,
    ) -> Self {
        Self {
            path: path.into(),
            max_workspaces,
            runtime_limits,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<ControlPlane, StoreError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ControlPlane::new(self.max_workspaces, self.runtime_limits));
            }
            Err(error) => {
                return Err(StoreError::new(format!(
                    "cannot read '{}': {error}",
                    self.path.display()
                )));
            }
        };
        let snapshot: ControlPlaneSnapshot = serde_json::from_slice(&bytes).map_err(|error| {
            StoreError::new(format!("cannot decode '{}': {error}", self.path.display()))
        })?;
        ControlPlane::restore(snapshot, self.max_workspaces, self.runtime_limits)
            .map_err(StoreError::from)
    }

    pub fn save(&self, plane: &ControlPlane) -> Result<(), StoreError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| StoreError::new("control-plane path has no parent"))?;
        fs::create_dir_all(parent).map_err(|error| {
            StoreError::new(format!("cannot create '{}': {error}", parent.display()))
        })?;
        let bytes = serde_json::to_vec_pretty(&plane.snapshot()).map_err(|error| {
            StoreError::new(format!("cannot encode control-plane state: {error}"))
        })?;
        let temporary = parent.join(format!(".control-plane.{}.tmp", std::process::id()));
        fs::write(&temporary, bytes).map_err(|error| {
            StoreError::new(format!("cannot write '{}': {error}", temporary.display()))
        })?;
        if let Err(error) = fs::rename(&temporary, &self.path) {
            let _ = fs::remove_file(&temporary);
            return Err(StoreError::new(format!(
                "cannot commit '{}': {error}",
                self.path.display()
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    message: String,
}

impl StoreError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for StoreError {}
impl From<ControlPlaneError> for StoreError {
    fn from(error: ControlPlaneError) -> Self {
        Self::new(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::ControlPlaneStore;
    use crate::control_plane::CapabilityGrant;
    use crate::runtime::ResourceLimits;

    #[test]
    fn store_round_trips_workspace_policy_atomically() {
        let directory =
            std::env::temp_dir().join(format!("arvm-control-plane-{}", std::process::id()));
        let path = directory.join("state.json");
        let store = ControlPlaneStore::new(&path, 8, ResourceLimits::default());
        let mut plane = store.load().unwrap();
        plane.create_workspace("alice", "research").unwrap();
        plane
            .grant_capability(
                "alice",
                "research",
                CapabilityGrant::new("issues", "github", "repo:a/rvm", ["read"]).unwrap(),
            )
            .unwrap();
        store.save(&plane).unwrap();
        let restored = store.load().unwrap();
        assert_eq!(
            restored
                .inspect("alice", "research")
                .unwrap()
                .capabilities
                .len(),
            1
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
