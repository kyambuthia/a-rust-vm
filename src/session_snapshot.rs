//! Versioned durable state for one owner-scoped guest session.
//!
//! A snapshot contains only state that can be resumed safely. Guest process
//! queues and pending approval waits are live execution state and are not
//! serialized; in-flight jobs are closed as failed during restoration.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::jobs::{JobStore, JobStoreSnapshot};
use crate::runtime::{RuntimeError, VmInstance, VmSnapshot};

pub const SESSION_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub schema_version: u32,
    pub owner: String,
    pub session_id: String,
    pub vm: VmSnapshot,
    pub jobs: JobStoreSnapshot,
}

#[derive(Debug)]
pub struct RestoredSession {
    pub owner: String,
    pub session_id: String,
    pub vm: VmInstance,
    pub jobs: JobStore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSnapshotError {
    UnsupportedSchema { expected: u32, actual: u32 },
    InvalidIdentity { kind: &'static str },
    MismatchedVmId,
    ForeignJob { job_id: String },
    Runtime(RuntimeError),
    Jobs(String),
}

impl fmt::Display for SessionSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema { expected, actual } => write!(
                formatter,
                "unsupported session snapshot schema: expected {expected} got {actual}"
            ),
            Self::InvalidIdentity { kind } => write!(formatter, "invalid session {kind}"),
            Self::MismatchedVmId => {
                formatter.write_str("session snapshot VM id does not match session id")
            }
            Self::ForeignJob { job_id } => {
                write!(
                    formatter,
                    "session snapshot contains a job owned by another identity: {job_id}"
                )
            }
            Self::Runtime(error) => write!(formatter, "cannot restore VM snapshot: {error}"),
            Self::Jobs(error) => write!(formatter, "cannot restore job snapshot: {error}"),
        }
    }
}

impl std::error::Error for SessionSnapshotError {}

impl SessionSnapshot {
    pub fn capture(
        owner: impl Into<String>,
        session_id: impl Into<String>,
        vm: &VmInstance,
        jobs: &JobStore,
    ) -> Result<Self, SessionSnapshotError> {
        let owner = owner.into();
        let session_id = session_id.into();
        validate_identity(&owner, "owner")?;
        validate_identity(&session_id, "id")?;
        if vm.id() != session_id {
            return Err(SessionSnapshotError::MismatchedVmId);
        }
        Ok(Self {
            schema_version: SESSION_SNAPSHOT_SCHEMA_VERSION,
            owner,
            session_id,
            vm: vm.snapshot(),
            jobs: jobs.snapshot(),
        })
    }

    pub fn restore(self) -> Result<RestoredSession, SessionSnapshotError> {
        if self.schema_version != SESSION_SNAPSHOT_SCHEMA_VERSION {
            return Err(SessionSnapshotError::UnsupportedSchema {
                expected: SESSION_SNAPSHOT_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        validate_identity(&self.owner, "owner")?;
        validate_identity(&self.session_id, "id")?;
        if self.vm.id != self.session_id {
            return Err(SessionSnapshotError::MismatchedVmId);
        }
        if let Some(job) = self.jobs.jobs.iter().find(|job| job.owner != self.owner) {
            return Err(SessionSnapshotError::ForeignJob {
                job_id: job.id.clone(),
            });
        }
        let vm = VmInstance::from_snapshot(self.vm).map_err(SessionSnapshotError::Runtime)?;
        let jobs = JobStore::from_snapshot(self.jobs).map_err(SessionSnapshotError::Jobs)?;
        Ok(RestoredSession {
            owner: self.owner,
            session_id: self.session_id,
            vm,
            jobs,
        })
    }
}

fn validate_identity(value: &str, kind: &'static str) -> Result<(), SessionSnapshotError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'@' | b'-')
        })
    {
        return Err(SessionSnapshotError::InvalidIdentity { kind });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{SESSION_SNAPSHOT_SCHEMA_VERSION, SessionSnapshot, SessionSnapshotError};
    use crate::jobs::JobStore;
    use crate::runtime::VmInstance;

    #[test]
    fn session_snapshots_round_trip_resumable_state() {
        let mut vm = VmInstance::new("session-a");
        vm.write_file("/workspace/notes.txt", "hello").unwrap();
        let mut jobs = JobStore::default();
        jobs.try_start_tabulation("alice", "/workspace/notes.txt", 4);

        let snapshot = SessionSnapshot::capture("alice", "session-a", &vm, &jobs).unwrap();
        let encoded = serde_json::to_vec(&snapshot).unwrap();
        let restored = SessionSnapshot::restore(serde_json::from_slice(&encoded).unwrap()).unwrap();

        assert_eq!(restored.owner, "alice");
        assert_eq!(restored.session_id, "session-a");
        assert_eq!(
            restored.vm.read_text("/workspace/notes.txt").unwrap(),
            "hello"
        );
        assert_eq!(restored.jobs.list("alice").len(), 1);
        assert!(matches!(
            restored.jobs.list("alice")[0].state,
            crate::jobs::JobState::Failed
        ));
    }

    #[test]
    fn session_snapshots_reject_unknown_schema_and_foreign_jobs() {
        let vm = VmInstance::new("session-a");
        let jobs = JobStore::default();
        let mut snapshot = SessionSnapshot::capture("alice", "session-a", &vm, &jobs).unwrap();
        snapshot.schema_version = SESSION_SNAPSHOT_SCHEMA_VERSION + 1;
        assert!(matches!(
            snapshot.restore(),
            Err(SessionSnapshotError::UnsupportedSchema { .. })
        ));

        let mut snapshot = SessionSnapshot::capture("alice", "session-a", &vm, &jobs).unwrap();
        snapshot.jobs.jobs.push(crate::jobs::JobRecord {
            id: "job-1".to_owned(),
            owner: "bob".to_owned(),
            executor: "builtin.tabulate.v1".to_owned(),
            input_path: "/workspace/uploads/file.csv".to_owned(),
            output_path: None,
            state: crate::jobs::JobState::Failed,
            error: Some("test".to_owned()),
        });
        assert!(matches!(
            snapshot.restore(),
            Err(SessionSnapshotError::ForeignJob { .. })
        ));
    }
}
