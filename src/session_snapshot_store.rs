//! Bounded filesystem storage for owner-scoped runtime snapshots.
//!
//! This store is intentionally a local durability primitive. It keeps the
//! serialized snapshot behind a caller-selected directory so the server can
//! use a mounted persistent volume without making the VM runtime depend on a
//! cloud SDK. A future object-store adapter can implement the same boundary.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::session_snapshot::SessionSnapshot;

const MAX_SERIALIZED_SNAPSHOT_BYTES: u64 = 96 * 1024 * 1024;
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSessionSnapshot {
    pub last_seen_at: u64,
    pub snapshot: SessionSnapshot,
}

#[derive(Debug, Clone)]
pub struct SessionSnapshotStore {
    directory: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSnapshotStoreError {
    InvalidIdentity {
        field: &'static str,
    },
    TooLarge {
        bytes: u64,
        maximum: u64,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        message: String,
    },
    Decode {
        path: PathBuf,
        message: String,
    },
    IdentityMismatch {
        path: PathBuf,
    },
}

impl fmt::Display for SessionSnapshotStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentity { field } => write!(formatter, "invalid snapshot {field}"),
            Self::TooLarge { bytes, maximum } => {
                write!(
                    formatter,
                    "snapshot is too large: bytes={bytes} maximum={maximum}"
                )
            }
            Self::Io {
                operation,
                path,
                message,
            } => write!(
                formatter,
                "cannot {operation} '{}': {message}",
                path.display()
            ),
            Self::Decode { path, message } => {
                write!(
                    formatter,
                    "cannot decode snapshot '{}': {message}",
                    path.display()
                )
            }
            Self::IdentityMismatch { path } => write!(
                formatter,
                "snapshot identity does not match its storage key: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SessionSnapshotStoreError {}

impl SessionSnapshotStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn save(&self, snapshot: &SessionSnapshot) -> Result<(), SessionSnapshotStoreError> {
        validate_component(&snapshot.owner, "owner")?;
        validate_component(&snapshot.session_id, "id")?;
        let stored = StoredSessionSnapshot {
            last_seen_at: unix_timestamp(),
            snapshot: snapshot.clone(),
        };
        let bytes = serde_json::to_vec(&stored).map_err(|error| SessionSnapshotStoreError::Io {
            operation: "encode snapshot",
            path: self.path_for(&snapshot.owner, &snapshot.session_id),
            message: error.to_string(),
        })?;
        ensure_size(bytes.len() as u64)?;
        fs::create_dir_all(&self.directory).map_err(|error| SessionSnapshotStoreError::Io {
            operation: "create snapshot directory",
            path: self.directory.clone(),
            message: error.to_string(),
        })?;
        set_private_directory_permissions(&self.directory)?;

        let target = self.path_for(&snapshot.owner, &snapshot.session_id);
        let owner_directory = self.directory.join(&snapshot.owner);
        fs::create_dir_all(&owner_directory).map_err(|error| SessionSnapshotStoreError::Io {
            operation: "create owner snapshot directory",
            path: owner_directory.clone(),
            message: error.to_string(),
        })?;
        set_private_directory_permissions(&owner_directory)?;
        let temporary = owner_directory.join(format!(
            ".{}.{}.{}.tmp",
            snapshot.session_id,
            std::process::id(),
            NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| SessionSnapshotStoreError::Io {
                operation: "create temporary snapshot",
                path: temporary.clone(),
                message: error.to_string(),
            })?;
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(SessionSnapshotStoreError::Io {
                operation: "write snapshot",
                path: temporary,
                message: error.to_string(),
            });
        }
        set_private_file_permissions(&temporary)?;
        if let Err(error) = fs::rename(&temporary, &target) {
            let _ = fs::remove_file(&temporary);
            return Err(SessionSnapshotStoreError::Io {
                operation: "commit snapshot",
                path: target,
                message: error.to_string(),
            });
        }
        Ok(())
    }

    pub fn load(
        &self,
        owner: &str,
        session_id: &str,
    ) -> Result<Option<StoredSessionSnapshot>, SessionSnapshotStoreError> {
        validate_component(owner, "owner")?;
        validate_component(session_id, "id")?;
        let path = self.path_for(owner, session_id);
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(SessionSnapshotStoreError::Io {
                    operation: "inspect snapshot",
                    path,
                    message: error.to_string(),
                });
            }
        };
        ensure_size(metadata.len())?;
        let bytes = fs::read(&path).map_err(|error| SessionSnapshotStoreError::Io {
            operation: "read snapshot",
            path: path.clone(),
            message: error.to_string(),
        })?;
        let stored = serde_json::from_slice::<StoredSessionSnapshot>(&bytes).map_err(|error| {
            SessionSnapshotStoreError::Decode {
                path: path.clone(),
                message: error.to_string(),
            }
        })?;
        if stored.snapshot.owner != owner || stored.snapshot.session_id != session_id {
            return Err(SessionSnapshotStoreError::IdentityMismatch { path });
        }
        Ok(Some(stored))
    }

    pub fn remove(&self, owner: &str, session_id: &str) -> Result<(), SessionSnapshotStoreError> {
        validate_component(owner, "owner")?;
        validate_component(session_id, "id")?;
        let path = self.path_for(owner, session_id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SessionSnapshotStoreError::Io {
                operation: "remove snapshot",
                path,
                message: error.to_string(),
            }),
        }
    }

    fn path_for(&self, owner: &str, session_id: &str) -> PathBuf {
        self.directory
            .join(owner)
            .join(format!("{session_id}.json"))
    }
}

fn ensure_size(bytes: u64) -> Result<(), SessionSnapshotStoreError> {
    if bytes > MAX_SERIALIZED_SNAPSHOT_BYTES {
        return Err(SessionSnapshotStoreError::TooLarge {
            bytes,
            maximum: MAX_SERIALIZED_SNAPSHOT_BYTES,
        });
    }
    Ok(())
}

fn validate_component(value: &str, field: &'static str) -> Result<(), SessionSnapshotStoreError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'@' | b'-')
        })
    {
        return Err(SessionSnapshotStoreError::InvalidIdentity { field });
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), SessionSnapshotStoreError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        SessionSnapshotStoreError::Io {
            operation: "protect snapshot directory",
            path: path.to_owned(),
            message: error.to_string(),
        }
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), SessionSnapshotStoreError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), SessionSnapshotStoreError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        SessionSnapshotStoreError::Io {
            operation: "protect snapshot",
            path: path.to_owned(),
            message: error.to_string(),
        }
    })
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), SessionSnapshotStoreError> {
    Ok(())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{SessionSnapshotStore, SessionSnapshotStoreError};
    use crate::jobs::JobStore;
    use crate::runtime::VmInstance;
    use crate::session_snapshot::SessionSnapshot;

    fn test_directory(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "a-rust-vm-runtime-snapshot-{label}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn snapshots_round_trip_with_owner_key_and_timestamp() {
        let directory = test_directory("round-trip");
        let store = SessionSnapshotStore::new(&directory);
        let mut vm = VmInstance::new("session-a");
        vm.write_file("/workspace/notes.txt", "hello").unwrap();
        let snapshot =
            SessionSnapshot::capture("alice", "session-a", &vm, &JobStore::default()).unwrap();

        store.save(&snapshot).unwrap();
        let stored = store.load("alice", "session-a").unwrap().unwrap();

        assert!(stored.last_seen_at > 0);
        assert_eq!(stored.snapshot, snapshot);
        assert!(directory.join("alice/session-a.json").is_file());
        store.remove("alice", "session-a").unwrap();
        assert!(store.load("alice", "session-a").unwrap().is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn snapshot_keys_reject_path_traversal() {
        let store = SessionSnapshotStore::new(test_directory("invalid"));

        assert!(matches!(
            store.load("../alice", "session-a"),
            Err(SessionSnapshotStoreError::InvalidIdentity { field: "owner" })
        ));
        assert!(matches!(
            store.remove("alice", "../session-a"),
            Err(SessionSnapshotStoreError::InvalidIdentity { field: "id" })
        ));
    }
}
