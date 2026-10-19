//! Private local persistence for replay-validated task records.
//!
//! This store is a single-process stepping stone. A shared task service will
//! need a transactional compare-and-swap before multiple hosts can update one
//! task concurrently.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::task::{MAX_TASK_RECORD_BYTES, TaskError, TaskRecord};

#[derive(Debug, Clone)]
pub struct TaskStore {
    directory: PathBuf,
}

/// Holds an exclusive local process lock until dropped. The lock file remains
/// so every process locks the same inode; the OS releases the lock on exit.
pub struct TaskLease {
    _file: File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStoreError {
    Task(TaskError),
    NotFound { id: String },
    Conflict { id: String },
    Busy { id: String },
    Io(String),
}

impl fmt::Display for TaskStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Task(error) => write!(formatter, "{error}"),
            Self::NotFound { id } => write!(formatter, "task '{id}' does not exist"),
            Self::Conflict { id } => write!(formatter, "task '{id}' changed since it was read"),
            Self::Busy { id } => write!(formatter, "task '{id}' is already running"),
            Self::Io(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for TaskStoreError {}

impl From<TaskError> for TaskStoreError {
    fn from(error: TaskError) -> Self {
        Self::Task(error)
    }
}

impl TaskStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Exclude another local CLI process from running the same task.
    pub fn lease(&self, id: &str) -> Result<TaskLease, TaskStoreError> {
        TaskRecord::validate_id(id)?;
        crate::private_fs::create_private_dir_all(&self.directory).map_err(|error| {
            TaskStoreError::Io(format!("cannot create task directory: {error}"))
        })?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(self.directory.join(format!("{id}.lock")))
            .map_err(|error| {
                TaskStoreError::Io(format!("cannot open task lock '{id}': {error}"))
            })?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => TaskStoreError::Busy { id: id.to_owned() },
            std::fs::TryLockError::Error(error) => {
                TaskStoreError::Io(format!("cannot lock task '{id}': {error}"))
            }
        })?;
        Ok(TaskLease { _file: file })
    }

    /// Save atomically, rejecting a different spec or a shorter event history.
    pub fn save(&self, task: &TaskRecord) -> Result<(), TaskStoreError> {
        let id = &task.spec().id;
        TaskRecord::validate_id(id)?;
        let bytes = serde_json::to_vec(task)
            .map_err(|error| TaskStoreError::Io(format!("cannot encode task '{id}': {error}")))?;
        TaskRecord::restore(&bytes)?;
        match self.load(id) {
            Ok(existing) => {
                if existing.spec() != task.spec() || !task.events().starts_with(existing.events()) {
                    return Err(TaskStoreError::Conflict { id: id.clone() });
                }
            }
            Err(TaskStoreError::NotFound { .. }) => {}
            Err(error) => return Err(error),
        }
        crate::private_fs::write_private(&self.path_for(id), &bytes)
            .map_err(|error| TaskStoreError::Io(format!("cannot save task '{id}': {error}")))
    }

    pub fn load(&self, id: &str) -> Result<TaskRecord, TaskStoreError> {
        TaskRecord::validate_id(id)?;
        let path = self.path_for(id);
        let file = File::open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                TaskStoreError::NotFound { id: id.to_owned() }
            } else {
                TaskStoreError::Io(format!("cannot open task '{id}': {error}"))
            }
        })?;
        let mut bytes = Vec::new();
        file.take((MAX_TASK_RECORD_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| TaskStoreError::Io(format!("cannot read task '{id}': {error}")))?;
        let task = TaskRecord::restore(&bytes)?;
        if task.spec().id != id {
            return Err(TaskStoreError::Conflict { id: id.to_owned() });
        }
        Ok(task)
    }

    pub fn list(&self) -> Result<Vec<String>, TaskStoreError> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(TaskStoreError::Io(format!("cannot list tasks: {error}")));
            }
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(|error| TaskStoreError::Io(format!("cannot list task entry: {error}")))?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if TaskRecord::validate_id(id).is_ok() {
                ids.push(id.to_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{TaskBudget, TaskEventKind, TaskSpec, TaskStatus};

    fn record() -> TaskRecord {
        TaskRecord::new(TaskSpec {
            id: "task-1".to_owned(),
            owner: "alice".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            prompt: "fix the bug".to_owned(),
            input_version_ids: Vec::new(),
            budget: TaskBudget {
                max_model_steps: 20,
                max_tool_calls: 40,
                timeout_seconds: 300,
            },
        })
        .unwrap()
    }

    #[test]
    fn lease_excludes_concurrent_local_runs_and_releases_on_drop() {
        let root = std::env::temp_dir().join(format!("arvm-task-lease-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = TaskStore::new(root.join("tasks"));
        let lease = store.lease("task-1").unwrap();
        assert!(matches!(
            store.lease("task-1"),
            Err(TaskStoreError::Busy { id }) if id == "task-1"
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.directory().join("task-1.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        drop(lease);
        store.lease("task-1").unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn saves_and_loads_a_private_task_without_losing_history() {
        let root = std::env::temp_dir().join(format!("arvm-task-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = TaskStore::new(root.join("tasks"));
        let mut task = record();
        store.save(&task).unwrap();
        task.append(TaskEventKind::Started).unwrap();
        store.save(&task).unwrap();
        assert_eq!(store.load("task-1").unwrap().status(), TaskStatus::Running);
        assert_eq!(store.list().unwrap(), ["task-1"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.directory().join("task-1.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_rollback_and_corrupt_state() {
        let root = std::env::temp_dir().join(format!("arvm-task-rollback-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = TaskStore::new(root.join("tasks"));
        let old = record();
        let mut current = old.clone();
        current.append(TaskEventKind::Started).unwrap();
        store.save(&current).unwrap();
        assert_eq!(
            store.save(&old),
            Err(TaskStoreError::Conflict {
                id: "task-1".to_owned()
            })
        );
        fs::write(store.directory().join("task-1.json"), b"not json").unwrap();
        assert!(matches!(store.load("task-1"), Err(TaskStoreError::Task(_))));
        let _ = fs::remove_dir_all(root);
    }
}
