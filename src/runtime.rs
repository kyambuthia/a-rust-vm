//! A deterministic guest runtime with an isolated virtual filesystem and
//! bytecode processes.
//!
//! `VmInstance` is deliberately independent from the host filesystem and
//! process table. It models the guest environment that agent tools can target;
//! a later host adapter can place each instance behind an OS sandbox.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Instruction, StepResult, Vm, VmError};

const DEFAULT_MAX_INODES: usize = 4_096;
const DEFAULT_MAX_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_PROCESSES: usize = 128;
const DEFAULT_MAX_STEPS: usize = 100_000;
const MAX_SNAPSHOT_ENTRIES: usize = 4_096;
const MAX_SNAPSHOT_ENTRY_PATH_BYTES: usize = 4_096;
const MAX_SNAPSHOT_PATH_COMPONENTS: usize = 128;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
const MAX_SNAPSHOT_INODES: usize = 32_768;
const MAX_SNAPSHOT_PROCESSES: usize = 512;
const MAX_SNAPSHOT_STEPS: usize = 10_000_000;

/// A guest process identifier. PIDs are scoped to one [`VmInstance`].
pub type Pid = u32;

/// Limits applied independently to one guest VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct ResourceLimits {
    pub max_inodes: usize,
    pub max_bytes: usize,
    pub max_processes: usize,
    pub max_steps: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_inodes: DEFAULT_MAX_INODES,
            max_bytes: DEFAULT_MAX_BYTES,
            max_processes: DEFAULT_MAX_PROCESSES,
            max_steps: DEFAULT_MAX_STEPS,
        }
    }
}

/// Errors raised by the guest runtime or virtual filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    InvalidPath {
        path: String,
        reason: String,
    },
    NotFound {
        path: String,
    },
    AlreadyExists {
        path: String,
    },
    NotDirectory {
        path: String,
    },
    IsDirectory {
        path: String,
    },
    ParentMissing {
        path: String,
    },
    InvalidUtf8 {
        path: String,
    },
    InvalidLimits {
        resource: &'static str,
        minimum: usize,
        actual: usize,
    },
    InvalidProgram {
        reason: String,
    },
    InvalidSnapshot {
        reason: String,
    },
    QuotaExceeded {
        resource: &'static str,
        limit: usize,
    },
    ProcessNotFound {
        pid: Pid,
    },
    ProcessLimitReached {
        limit: usize,
    },
    InvalidParent {
        pid: Pid,
    },
    ExecutionLimitExceeded {
        limit: usize,
    },
    Vm(VmError),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path, reason } => {
                write!(formatter, "invalid path '{path}': {reason}")
            }
            Self::NotFound { path } => write!(formatter, "path does not exist: {path}"),
            Self::AlreadyExists { path } => write!(formatter, "path already exists: {path}"),
            Self::NotDirectory { path } => write!(formatter, "not a directory: {path}"),
            Self::IsDirectory { path } => write!(formatter, "is a directory: {path}"),
            Self::ParentMissing { path } => {
                write!(formatter, "parent directory is missing: {path}")
            }
            Self::InvalidUtf8 { path } => write!(formatter, "file is not valid UTF-8: {path}"),
            Self::InvalidLimits {
                resource,
                minimum,
                actual,
            } => write!(
                formatter,
                "invalid {resource} limit: minimum={minimum} actual={actual}"
            ),
            Self::InvalidProgram { reason } => write!(formatter, "invalid guest program: {reason}"),
            Self::InvalidSnapshot { reason } => write!(formatter, "invalid VM snapshot: {reason}"),
            Self::QuotaExceeded { resource, limit } => {
                write!(formatter, "guest {resource} quota exceeded: limit={limit}")
            }
            Self::ProcessNotFound { pid } => write!(formatter, "process does not exist: {pid}"),
            Self::ProcessLimitReached { limit } => {
                write!(formatter, "guest process quota exceeded: limit={limit}")
            }
            Self::InvalidParent { pid } => {
                write!(formatter, "parent process does not exist: {pid}")
            }
            Self::ExecutionLimitExceeded { limit } => {
                write!(formatter, "guest execution limit exceeded: limit={limit}")
            }
            Self::Vm(error) => write!(formatter, "guest VM error: {error:?}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<VmError> for RuntimeError {
    fn from(error: VmError) -> Self {
        Self::Vm(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Directory(BTreeMap<String, Node>),
    File(Vec<u8>),
}

pub const VM_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotEntryKind {
    Directory,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub path: String,
    pub kind: SnapshotEntryKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmSnapshot {
    pub schema_version: u32,
    pub id: String,
    pub limits: ResourceLimits,
    pub entries: Vec<SnapshotEntry>,
}

/// The kind of a guest filesystem entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
}

/// Metadata returned when listing a guest directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: usize,
}

/// A filesystem that exists entirely inside one guest VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualFileSystem {
    root: Node,
    inode_count: usize,
    byte_count: usize,
    limits: ResourceLimits,
}

impl VirtualFileSystem {
    pub fn new(limits: ResourceLimits) -> Result<Self, RuntimeError> {
        if limits.max_inodes < 4 {
            return Err(RuntimeError::InvalidLimits {
                resource: "inodes",
                minimum: 4,
                actual: limits.max_inodes,
            });
        }
        let mut filesystem = Self {
            root: Node::Directory(BTreeMap::new()),
            inode_count: 1,
            byte_count: 0,
            limits,
        };
        filesystem
            .mkdir("/workspace", true)
            .expect("default path is valid");
        filesystem
            .mkdir("/tmp", true)
            .expect("default path is valid");
        filesystem
            .mkdir("/home", true)
            .expect("default path is valid");
        Ok(filesystem)
    }

    pub fn inode_count(&self) -> usize {
        self.inode_count
    }

    pub fn byte_count(&self) -> usize {
        self.byte_count
    }

    pub fn limits(&self) -> ResourceLimits {
        self.limits
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, RuntimeError> {
        let components = normalize_path(path, "/")?;
        match node_at(&self.root, &components) {
            Some(Node::File(content)) => Ok(content.clone()),
            Some(Node::Directory(_)) => Err(RuntimeError::IsDirectory {
                path: display_path(&components),
            }),
            None => Err(RuntimeError::NotFound {
                path: display_path(&components),
            }),
        }
    }

    pub fn read_text(&self, path: &str) -> Result<String, RuntimeError> {
        let bytes = self.read_file(path)?;
        String::from_utf8(bytes).map_err(|_| RuntimeError::InvalidUtf8 {
            path: path.to_owned(),
        })
    }

    pub fn write_file(
        &mut self,
        path: &str,
        content: impl AsRef<[u8]>,
    ) -> Result<(), RuntimeError> {
        let components = normalize_path(path, "/")?;
        if components.is_empty() {
            return Err(RuntimeError::IsDirectory {
                path: "/".to_owned(),
            });
        }
        let parent_components = &components[..components.len() - 1];
        let name = components.last().expect("non-empty path");
        let content = content.as_ref();
        let old_size = match node_at(&self.root, &components) {
            Some(Node::File(existing)) => existing.len(),
            Some(Node::Directory(_)) => {
                return Err(RuntimeError::IsDirectory {
                    path: display_path(&components),
                });
            }
            None => 0,
        };
        let new_bytes = self.byte_count - old_size + content.len();
        if new_bytes > self.limits.max_bytes {
            return Err(RuntimeError::QuotaExceeded {
                resource: "file bytes",
                limit: self.limits.max_bytes,
            });
        }
        let new_inode = node_at(&self.root, &components).is_none();
        if new_inode {
            self.ensure_inode_capacity()?;
        }
        let parent = node_at_mut(&mut self.root, parent_components).ok_or_else(|| {
            RuntimeError::ParentMissing {
                path: display_path(parent_components),
            }
        })?;
        let Node::Directory(entries) = parent else {
            return Err(RuntimeError::NotDirectory {
                path: display_path(parent_components),
            });
        };
        entries.insert(name.clone(), Node::File(content.to_vec()));
        if new_inode {
            self.inode_count += 1;
        }
        self.byte_count = new_bytes;
        Ok(())
    }

    pub fn mkdir(&mut self, path: &str, recursive: bool) -> Result<(), RuntimeError> {
        let components = normalize_path(path, "/")?;
        if components.is_empty() {
            return Ok(());
        }

        let mut current = &self.root;
        let mut first_missing = None;
        for (index, component) in components.iter().enumerate() {
            let Node::Directory(entries) = current else {
                return Err(RuntimeError::NotDirectory {
                    path: display_path(&components[..index]),
                });
            };
            match entries.get(component) {
                Some(node) => current = node,
                None => {
                    first_missing = Some(index);
                    break;
                }
            }
        }
        let Some(first_missing) = first_missing else {
            if matches!(current, Node::Directory(_)) {
                return Ok(());
            }
            return Err(RuntimeError::AlreadyExists {
                path: display_path(&components),
            });
        };
        if !recursive && first_missing + 1 != components.len() {
            return Err(RuntimeError::ParentMissing {
                path: display_path(&components),
            });
        }
        let missing = components.len() - first_missing;
        if self.inode_count + missing > self.limits.max_inodes {
            return Err(RuntimeError::QuotaExceeded {
                resource: "inodes",
                limit: self.limits.max_inodes,
            });
        }

        let mut current = &mut self.root;
        for component in &components[..first_missing] {
            let Node::Directory(entries) = current else {
                return Err(RuntimeError::NotDirectory {
                    path: display_path(&components),
                });
            };
            current = entries
                .get_mut(component)
                .expect("existing parent was validated");
        }
        for component in &components[first_missing..] {
            let Node::Directory(entries) = current else {
                return Err(RuntimeError::NotDirectory {
                    path: display_path(&components),
                });
            };
            entries.insert(component.clone(), Node::Directory(BTreeMap::new()));
            current = entries.get_mut(component).expect("entry was inserted");
        }
        self.inode_count += missing;
        Ok(())
    }

    pub fn list_dir(&self, path: &str) -> Result<Vec<DirectoryEntry>, RuntimeError> {
        let components = normalize_path(path, "/")?;
        let node = node_at(&self.root, &components).ok_or_else(|| RuntimeError::NotFound {
            path: display_path(&components),
        })?;
        let Node::Directory(entries) = node else {
            return Err(RuntimeError::NotDirectory {
                path: display_path(&components),
            });
        };
        Ok(entries
            .iter()
            .map(|(name, node)| match node {
                Node::Directory(_) => DirectoryEntry {
                    name: name.clone(),
                    kind: EntryKind::Directory,
                    size: 0,
                },
                Node::File(content) => DirectoryEntry {
                    name: name.clone(),
                    kind: EntryKind::File,
                    size: content.len(),
                },
            })
            .collect())
    }

    fn ensure_inode_capacity(&self) -> Result<(), RuntimeError> {
        if self.inode_count >= self.limits.max_inodes {
            return Err(RuntimeError::QuotaExceeded {
                resource: "inodes",
                limit: self.limits.max_inodes,
            });
        }
        Ok(())
    }
}

fn node_at<'a>(root: &'a Node, components: &[String]) -> Option<&'a Node> {
    components
        .iter()
        .try_fold(root, |node, component| match node {
            Node::Directory(entries) => entries.get(component),
            Node::File(_) => None,
        })
}

fn node_at_mut<'a>(root: &'a mut Node, components: &[String]) -> Option<&'a mut Node> {
    components
        .iter()
        .try_fold(root, |node, component| match node {
            Node::Directory(entries) => entries.get_mut(component),
            Node::File(_) => None,
        })
}

fn collect_snapshot_entries(node: &Node, prefix: &str, entries: &mut Vec<SnapshotEntry>) {
    let Node::Directory(children) = node else {
        return;
    };
    for (name, child) in children {
        let path = if prefix.is_empty() {
            format!("/{name}")
        } else {
            format!("{prefix}/{name}")
        };
        match child {
            Node::Directory(_) => {
                entries.push(SnapshotEntry {
                    path: path.clone(),
                    kind: SnapshotEntryKind::Directory,
                    bytes: Vec::new(),
                });
                collect_snapshot_entries(child, &path, entries);
            }
            Node::File(bytes) => entries.push(SnapshotEntry {
                path,
                kind: SnapshotEntryKind::File,
                bytes: bytes.clone(),
            }),
        }
    }
}

fn normalize_path(path: &str, cwd: &str) -> Result<Vec<String>, RuntimeError> {
    if path.is_empty() || path.contains('\0') {
        return Err(RuntimeError::InvalidPath {
            path: path.to_owned(),
            reason: "path is empty or contains NUL".to_owned(),
        });
    }
    let combined = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{cwd}/{path}")
    };
    let mut components = Vec::new();
    for component in combined.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(RuntimeError::InvalidPath {
                        path: path.to_owned(),
                        reason: "path escapes the guest root".to_owned(),
                    });
                }
            }
            value => components.push(value.to_owned()),
        }
    }
    Ok(components)
}

fn display_path(components: &[String]) -> String {
    if components.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", components.join("/"))
    }
}

/// Lifecycle state for a guest process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessState {
    Ready,
    Running,
    Exited { code: i32 },
    Failed { error: String },
}

/// Inspectable process metadata without exposing mutable guest state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: Pid,
    pub parent: Option<Pid>,
    pub argv: Vec<String>,
    pub cwd: String,
    pub state: ProcessState,
    pub instruction_pointer: usize,
    pub stack: Vec<i32>,
}

#[derive(Debug)]
struct GuestProcess {
    pid: Pid,
    parent: Option<Pid>,
    argv: Vec<String>,
    cwd: String,
    program: Vec<Instruction>,
    vm: Vm,
    state: ProcessState,
}

/// The result of one deterministic scheduler tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEvent {
    InstructionExecuted {
        pid: Pid,
        instruction_pointer: usize,
        instruction: Instruction,
        stack: Vec<i32>,
    },
    ProcessExited {
        pid: Pid,
        code: i32,
    },
    ProcessFailed {
        pid: Pid,
        error: String,
    },
}

/// Result of polling a guest process with `wait`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitStatus {
    Running,
    Exited { code: i32 },
    Failed { error: String },
}

/// Guest operations exposed as an explicit syscall-like API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Syscall {
    ReadFile {
        path: String,
    },
    WriteFile {
        path: String,
        content: Vec<u8>,
    },
    MakeDirectory {
        path: String,
        recursive: bool,
    },
    ListDirectory {
        path: String,
    },
    Spawn {
        program: Vec<Instruction>,
        argv: Vec<String>,
    },
    Wait {
        pid: Pid,
    },
    Exit {
        code: i32,
    },
}

/// Values returned by a guest syscall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyscallResult {
    File(Vec<u8>),
    Directory(Vec<DirectoryEntry>),
    Process(Pid),
    Wait(WaitStatus),
    Unit,
}

/// One isolated guest environment.
#[derive(Debug)]
pub struct VmInstance {
    id: String,
    limits: ResourceLimits,
    filesystem: VirtualFileSystem,
    processes: BTreeMap<Pid, GuestProcess>,
    runnable: VecDeque<Pid>,
    next_pid: Pid,
    ticks: usize,
}

impl VmInstance {
    pub fn new(id: impl Into<String>) -> Self {
        Self::with_limits(id, ResourceLimits::default()).expect("default limits are valid")
    }

    pub fn with_limits(
        id: impl Into<String>,
        limits: ResourceLimits,
    ) -> Result<Self, RuntimeError> {
        Ok(Self {
            id: id.into(),
            limits,
            filesystem: VirtualFileSystem::new(limits)?,
            processes: BTreeMap::new(),
            runnable: VecDeque::new(),
            next_pid: 1,
            ticks: 0,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn limits(&self) -> ResourceLimits {
        self.limits
    }

    pub fn ticks(&self) -> usize {
        self.ticks
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, RuntimeError> {
        self.filesystem.read_file(path)
    }

    pub fn read_text(&self, path: &str) -> Result<String, RuntimeError> {
        self.filesystem.read_text(path)
    }

    pub fn write_file(
        &mut self,
        path: &str,
        content: impl AsRef<[u8]>,
    ) -> Result<(), RuntimeError> {
        self.filesystem.write_file(path, content)
    }

    pub fn mkdir(&mut self, path: &str, recursive: bool) -> Result<(), RuntimeError> {
        self.filesystem.mkdir(path, recursive)
    }

    pub fn list_dir(&self, path: &str) -> Result<Vec<DirectoryEntry>, RuntimeError> {
        self.filesystem.list_dir(path)
    }

    pub fn filesystem(&self) -> &VirtualFileSystem {
        &self.filesystem
    }

    /// Capture durable guest state. Live process queues and instruction
    /// counters are intentionally excluded because they cannot be resumed
    /// safely across a service restart.
    pub fn snapshot(&self) -> VmSnapshot {
        let mut entries = Vec::new();
        collect_snapshot_entries(&self.filesystem.root, "", &mut entries);
        VmSnapshot {
            schema_version: VM_SNAPSHOT_SCHEMA_VERSION,
            id: self.id.clone(),
            limits: self.limits,
            entries,
        }
    }

    pub fn from_snapshot(snapshot: VmSnapshot) -> Result<Self, RuntimeError> {
        if snapshot.schema_version != VM_SNAPSHOT_SCHEMA_VERSION {
            return Err(RuntimeError::InvalidSnapshot {
                reason: format!(
                    "unsupported VM snapshot schema: expected {} got {}",
                    VM_SNAPSHOT_SCHEMA_VERSION, snapshot.schema_version
                ),
            });
        }
        validate_snapshot_bounds(&snapshot)?;
        let mut vm = Self::with_limits(snapshot.id, snapshot.limits)?;
        let mut paths = BTreeSet::new();
        for entry in snapshot.entries {
            let components = normalize_path(&entry.path, "/")?;
            if components.is_empty() || !paths.insert(display_path(&components)) {
                return Err(RuntimeError::InvalidSnapshot {
                    reason: format!("duplicate or root snapshot entry: {}", entry.path),
                });
            }
            match entry.kind {
                SnapshotEntryKind::Directory => {
                    if !entry.bytes.is_empty() {
                        return Err(RuntimeError::InvalidSnapshot {
                            reason: format!("directory entry contains bytes: {}", entry.path),
                        });
                    }
                    vm.mkdir(&entry.path, true)?;
                }
                SnapshotEntryKind::File => {
                    let parent = display_path(&components[..components.len() - 1]);
                    vm.mkdir(&parent, true)?;
                    vm.write_file(&entry.path, entry.bytes)?;
                }
            }
        }
        Ok(vm)
    }

    pub fn process_info(&self, pid: Pid) -> Result<ProcessInfo, RuntimeError> {
        let process = self
            .processes
            .get(&pid)
            .ok_or(RuntimeError::ProcessNotFound { pid })?;
        Ok(ProcessInfo {
            pid: process.pid,
            parent: process.parent,
            argv: process.argv.clone(),
            cwd: process.cwd.clone(),
            state: process.state.clone(),
            instruction_pointer: process.vm.instruction_pointer(),
            stack: process.vm.stack().to_vec(),
        })
    }

    pub fn processes(&self) -> Vec<ProcessInfo> {
        self.processes
            .keys()
            .filter_map(|pid| self.process_info(*pid).ok())
            .collect()
    }

    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    pub fn spawn(
        &mut self,
        parent: Option<Pid>,
        program: Vec<Instruction>,
        argv: Vec<String>,
    ) -> Result<Pid, RuntimeError> {
        let program = crate::program::Program::new(program)
            .map_err(|error| RuntimeError::InvalidProgram {
                reason: error.to_string(),
            })?
            .instructions()
            .to_vec();
        if self.processes.len() >= self.limits.max_processes {
            return Err(RuntimeError::ProcessLimitReached {
                limit: self.limits.max_processes,
            });
        }
        let cwd = match parent {
            Some(pid) => self
                .processes
                .get(&pid)
                .ok_or(RuntimeError::InvalidParent { pid })?
                .cwd
                .clone(),
            None => "/".to_owned(),
        };
        let pid = self.next_pid;
        self.next_pid = self
            .next_pid
            .checked_add(1)
            .ok_or(RuntimeError::QuotaExceeded {
                resource: "process IDs",
                limit: u32::MAX as usize,
            })?;
        self.processes.insert(
            pid,
            GuestProcess {
                pid,
                parent,
                argv,
                cwd,
                program,
                vm: Vm::new(),
                state: ProcessState::Ready,
            },
        );
        self.runnable.push_back(pid);
        Ok(pid)
    }

    pub fn wait(&self, pid: Pid) -> Result<WaitStatus, RuntimeError> {
        let process = self
            .processes
            .get(&pid)
            .ok_or(RuntimeError::ProcessNotFound { pid })?;
        Ok(match &process.state {
            ProcessState::Ready | ProcessState::Running => WaitStatus::Running,
            ProcessState::Exited { code } => WaitStatus::Exited { code: *code },
            ProcessState::Failed { error } => WaitStatus::Failed {
                error: error.clone(),
            },
        })
    }

    /// Execute one instruction from the next ready process.
    pub fn tick(&mut self) -> Option<ProcessEvent> {
        if self.ticks >= self.limits.max_steps {
            return None;
        }
        while let Some(pid) = self.runnable.pop_front() {
            let Some(process) = self.processes.get_mut(&pid) else {
                continue;
            };
            if !matches!(process.state, ProcessState::Ready) {
                continue;
            }
            process.state = ProcessState::Running;
            let instruction_pointer = process.vm.instruction_pointer();
            let result = process.vm.step(&process.program);
            self.ticks = self.ticks.saturating_add(1);
            return match result {
                Ok(StepResult::Executed { instruction }) => {
                    let stack = process.vm.stack().to_vec();
                    process.state = ProcessState::Ready;
                    self.runnable.push_back(pid);
                    Some(ProcessEvent::InstructionExecuted {
                        pid,
                        instruction_pointer,
                        instruction,
                        stack,
                    })
                }
                Ok(StepResult::Halted { result }) => {
                    process.state = ProcessState::Exited { code: result };
                    Some(ProcessEvent::ProcessExited { pid, code: result })
                }
                Err(error) => {
                    let error = format!("{error:?}");
                    process.state = ProcessState::Failed {
                        error: error.clone(),
                    };
                    Some(ProcessEvent::ProcessFailed { pid, error })
                }
            };
        }
        None
    }

    pub fn run_until_idle(&mut self, max_ticks: usize) -> Result<Vec<ProcessEvent>, RuntimeError> {
        let mut events = Vec::new();
        for _ in 0..max_ticks {
            if self.runnable.is_empty() {
                return Ok(events);
            }
            if self.ticks >= self.limits.max_steps {
                return Err(RuntimeError::ExecutionLimitExceeded {
                    limit: self.limits.max_steps,
                });
            }
            match self.tick() {
                Some(event) => events.push(event),
                None if self.runnable.is_empty() => return Ok(events),
                None => {
                    return Err(RuntimeError::ExecutionLimitExceeded {
                        limit: self.limits.max_steps,
                    });
                }
            }
        }
        if self.runnable.is_empty() {
            Ok(events)
        } else {
            Err(RuntimeError::ExecutionLimitExceeded { limit: max_ticks })
        }
    }

    pub fn syscall(&mut self, pid: Pid, syscall: Syscall) -> Result<SyscallResult, RuntimeError> {
        let cwd = self
            .processes
            .get(&pid)
            .ok_or(RuntimeError::ProcessNotFound { pid })?
            .cwd
            .clone();
        match syscall {
            Syscall::ReadFile { path } => {
                let path = guest_path(&cwd, &path)?;
                Ok(SyscallResult::File(self.filesystem.read_file(&path)?))
            }
            Syscall::WriteFile { path, content } => {
                let path = guest_path(&cwd, &path)?;
                self.filesystem.write_file(&path, content)?;
                Ok(SyscallResult::Unit)
            }
            Syscall::MakeDirectory { path, recursive } => {
                let path = guest_path(&cwd, &path)?;
                self.filesystem.mkdir(&path, recursive)?;
                Ok(SyscallResult::Unit)
            }
            Syscall::ListDirectory { path } => {
                let path = guest_path(&cwd, &path)?;
                Ok(SyscallResult::Directory(self.filesystem.list_dir(&path)?))
            }
            Syscall::Spawn { program, argv } => {
                let child = self.spawn(Some(pid), program, argv)?;
                Ok(SyscallResult::Process(child))
            }
            Syscall::Wait { pid: child } => Ok(SyscallResult::Wait(self.wait(child)?)),
            Syscall::Exit { code } => {
                let process = self
                    .processes
                    .get_mut(&pid)
                    .ok_or(RuntimeError::ProcessNotFound { pid })?;
                process.state = ProcessState::Exited { code };
                Ok(SyscallResult::Unit)
            }
        }
    }
}

fn validate_snapshot_bounds(snapshot: &VmSnapshot) -> Result<(), RuntimeError> {
    if snapshot.id.is_empty()
        || snapshot.id.len() > 128
        || !snapshot.id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'@' | b'-')
        })
    {
        return Err(RuntimeError::InvalidSnapshot {
            reason: "VM id is invalid".to_owned(),
        });
    }
    if snapshot.entries.len() > MAX_SNAPSHOT_ENTRIES {
        return Err(RuntimeError::InvalidSnapshot {
            reason: format!("too many filesystem entries: maximum is {MAX_SNAPSHOT_ENTRIES}"),
        });
    }
    if snapshot.limits.max_inodes > MAX_SNAPSHOT_INODES {
        return Err(RuntimeError::InvalidSnapshot {
            reason: format!(
                "inode limit exceeds snapshot maximum: maximum is {MAX_SNAPSHOT_INODES}"
            ),
        });
    }
    if snapshot.limits.max_bytes > MAX_SNAPSHOT_BYTES {
        return Err(RuntimeError::InvalidSnapshot {
            reason: format!("byte limit exceeds snapshot maximum: maximum is {MAX_SNAPSHOT_BYTES}"),
        });
    }
    if snapshot.limits.max_processes > MAX_SNAPSHOT_PROCESSES {
        return Err(RuntimeError::InvalidSnapshot {
            reason: format!(
                "process limit exceeds snapshot maximum: maximum is {MAX_SNAPSHOT_PROCESSES}"
            ),
        });
    }
    if snapshot.limits.max_steps > MAX_SNAPSHOT_STEPS {
        return Err(RuntimeError::InvalidSnapshot {
            reason: format!("step limit exceeds snapshot maximum: maximum is {MAX_SNAPSHOT_STEPS}"),
        });
    }

    let mut total_bytes = 0usize;
    for entry in &snapshot.entries {
        if entry.path.len() > MAX_SNAPSHOT_ENTRY_PATH_BYTES {
            return Err(RuntimeError::InvalidSnapshot {
                reason: format!("snapshot path exceeds {MAX_SNAPSHOT_ENTRY_PATH_BYTES} bytes"),
            });
        }
        let components = normalize_path(&entry.path, "/")?;
        if components.len() > MAX_SNAPSHOT_PATH_COMPONENTS {
            return Err(RuntimeError::InvalidSnapshot {
                reason: format!("snapshot path exceeds {MAX_SNAPSHOT_PATH_COMPONENTS} components"),
            });
        }
        total_bytes = total_bytes.checked_add(entry.bytes.len()).ok_or_else(|| {
            RuntimeError::InvalidSnapshot {
                reason: "snapshot byte count overflowed".to_owned(),
            }
        })?;
        if total_bytes > MAX_SNAPSHOT_BYTES || total_bytes > snapshot.limits.max_bytes {
            return Err(RuntimeError::InvalidSnapshot {
                reason: "snapshot file bytes exceed the configured quota".to_owned(),
            });
        }
    }
    Ok(())
}

/// Errors from the ownership-aware VM manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmManagerError {
    InvalidIdentity { kind: &'static str },
    AlreadyExists { owner: String, id: String },
    NotFound { owner: String, id: String },
    InstanceLimitReached { limit: usize },
    Runtime(RuntimeError),
}

impl fmt::Display for VmManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentity { kind } => write!(formatter, "{kind} must be non-empty"),
            Self::AlreadyExists { owner, id } => {
                write!(formatter, "VM already exists for owner '{owner}': {id}")
            }
            Self::NotFound { owner, id } => {
                write!(formatter, "VM does not exist for owner '{owner}': {id}")
            }
            Self::InstanceLimitReached { limit } => {
                write!(formatter, "VM instance quota exceeded: limit={limit}")
            }
            Self::Runtime(error) => write!(formatter, "VM runtime error: {error}"),
        }
    }
}

impl std::error::Error for VmManagerError {}

impl From<RuntimeError> for VmManagerError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

/// A read-only summary used by a control plane to list a user's VMs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VmSummary {
    pub owner: String,
    pub id: String,
    pub ticks: usize,
    pub process_count: usize,
    pub inode_count: usize,
    pub byte_count: usize,
}

/// Owns multiple isolated guest VMs and scopes every lookup by owner.
///
/// The owner value must come from an authenticated control plane. It is not an
/// authorization mechanism by itself and must never be accepted as proof of
/// identity from an untrusted browser request.
#[derive(Debug)]
pub struct VmManager {
    instances: BTreeMap<(String, String), VmInstance>,
    limits: ResourceLimits,
    max_instances: usize,
}

impl VmManager {
    pub fn new(limits: ResourceLimits) -> Self {
        Self::with_max_instances(64, limits)
    }

    pub fn with_max_instances(max_instances: usize, limits: ResourceLimits) -> Self {
        Self {
            instances: BTreeMap::new(),
            limits,
            max_instances,
        }
    }

    pub fn create(
        &mut self,
        owner: impl Into<String>,
        id: impl Into<String>,
    ) -> Result<(), VmManagerError> {
        let owner = owner.into();
        let id = id.into();
        validate_identity(&owner, "owner")?;
        validate_identity(&id, "VM id")?;
        if self.instances.len() >= self.max_instances {
            return Err(VmManagerError::InstanceLimitReached {
                limit: self.max_instances,
            });
        }
        let key = (owner.clone(), id.clone());
        if self.instances.contains_key(&key) {
            return Err(VmManagerError::AlreadyExists { owner, id });
        }
        let vm = VmInstance::with_limits(id, self.limits)?;
        self.instances.insert(key, vm);
        Ok(())
    }

    pub fn get(&self, owner: &str, id: &str) -> Result<&VmInstance, VmManagerError> {
        self.instances
            .get(&(owner.to_owned(), id.to_owned()))
            .ok_or_else(|| VmManagerError::NotFound {
                owner: owner.to_owned(),
                id: id.to_owned(),
            })
    }

    pub fn get_mut(&mut self, owner: &str, id: &str) -> Result<&mut VmInstance, VmManagerError> {
        self.instances
            .get_mut(&(owner.to_owned(), id.to_owned()))
            .ok_or_else(|| VmManagerError::NotFound {
                owner: owner.to_owned(),
                id: id.to_owned(),
            })
    }

    pub fn destroy(&mut self, owner: &str, id: &str) -> Result<VmInstance, VmManagerError> {
        self.instances
            .remove(&(owner.to_owned(), id.to_owned()))
            .ok_or_else(|| VmManagerError::NotFound {
                owner: owner.to_owned(),
                id: id.to_owned(),
            })
    }

    pub fn list(&self, owner: &str) -> Vec<VmSummary> {
        self.instances
            .iter()
            .filter(|((instance_owner, _), _)| instance_owner == owner)
            .map(|((instance_owner, id), vm)| VmSummary {
                owner: instance_owner.clone(),
                id: id.clone(),
                ticks: vm.ticks(),
                process_count: vm.process_count(),
                inode_count: vm.filesystem().inode_count(),
                byte_count: vm.filesystem().byte_count(),
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.instances.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }
}

fn validate_identity(value: &str, kind: &'static str) -> Result<(), VmManagerError> {
    if value.trim().is_empty() {
        Err(VmManagerError::InvalidIdentity { kind })
    } else {
        Ok(())
    }
}

fn guest_path(cwd: &str, path: &str) -> Result<String, RuntimeError> {
    Ok(display_path(&normalize_path(path, cwd)?))
}

#[cfg(test)]
mod tests {
    use super::{
        EntryKind, ProcessEvent, ProcessState, ResourceLimits, RuntimeError, Syscall,
        SyscallResult, VmInstance, VmManager, VmManagerError, WaitStatus,
    };
    use crate::Instruction;

    fn add_program() -> Vec<Instruction> {
        vec![
            Instruction::Push(2),
            Instruction::Push(3),
            Instruction::Add,
            Instruction::Halt,
        ]
    }

    #[test]
    fn instances_have_independent_filesystems_and_pid_spaces() {
        let mut first = VmInstance::new("user-a");
        let mut second = VmInstance::new("user-b");

        first.write_file("/workspace/secret.txt", "only-a").unwrap();
        let pid_a = first
            .spawn(None, add_program(), vec!["calc".to_owned()])
            .unwrap();
        let pid_b = second
            .spawn(None, add_program(), vec!["calc".to_owned()])
            .unwrap();

        assert_eq!(first.read_text("/workspace/secret.txt").unwrap(), "only-a");
        assert!(matches!(
            second.read_file("/workspace/secret.txt"),
            Err(RuntimeError::NotFound { .. })
        ));
        assert_eq!(pid_a, 1);
        assert_eq!(pid_b, 1);
        assert_eq!(first.id(), "user-a");
    }

    #[test]
    fn filesystem_normalizes_guest_paths_without_host_access() {
        let mut vm = VmInstance::new("paths");
        vm.write_file("/workspace/notes.txt", "hello").unwrap();

        assert_eq!(vm.read_text("/workspace/./notes.txt").unwrap(), "hello");
        assert_eq!(
            vm.read_text("/workspace/../workspace/notes.txt").unwrap(),
            "hello"
        );
        assert!(matches!(
            vm.read_file("../../etc/passwd"),
            Err(RuntimeError::InvalidPath { .. })
        ));
    }

    #[test]
    fn filesystem_tracks_entries_and_enforces_quotas() {
        let limits = ResourceLimits {
            max_inodes: 5,
            max_bytes: 10,
            ..ResourceLimits::default()
        };
        let mut vm = VmInstance::with_limits("quota", limits).unwrap();
        vm.write_file("/workspace/a", "1234").unwrap();
        assert_eq!(vm.filesystem().inode_count(), 5);
        assert!(matches!(
            vm.write_file("/workspace/b", "x"),
            Err(RuntimeError::QuotaExceeded {
                resource: "inodes",
                ..
            })
        ));
        assert!(matches!(
            vm.write_file("/workspace/a", "12345678901"),
            Err(RuntimeError::QuotaExceeded {
                resource: "file bytes",
                ..
            })
        ));
    }

    #[test]
    fn vm_snapshots_round_trip_files_and_empty_directories() {
        let mut vm = VmInstance::new("snapshot");
        vm.mkdir("/workspace/empty", true).unwrap();
        vm.write_file("/workspace/notes.txt", b"hello").unwrap();
        vm.spawn(
            None,
            add_program(),
            vec!["ignored-after-restart".to_owned()],
        )
        .unwrap();

        let encoded = serde_json::to_vec(&vm.snapshot()).unwrap();
        let restored =
            VmInstance::from_snapshot(serde_json::from_slice(&encoded).unwrap()).unwrap();

        assert_eq!(restored.id(), "snapshot");
        assert_eq!(restored.read_text("/workspace/notes.txt").unwrap(), "hello");
        assert!(restored.list_dir("/workspace/empty").unwrap().is_empty());
        assert_eq!(restored.process_count(), 0);
        assert_eq!(restored.snapshot(), vm.snapshot());
    }

    #[test]
    fn vm_snapshot_rejects_unknown_schema() {
        let mut snapshot = VmInstance::new("snapshot").snapshot();
        snapshot.schema_version += 1;

        assert!(matches!(
            VmInstance::from_snapshot(snapshot),
            Err(RuntimeError::InvalidSnapshot { .. })
        ));
    }

    #[test]
    fn vm_snapshot_rejects_untrusted_bounds() {
        let mut snapshot = VmInstance::new("snapshot").snapshot();
        snapshot.limits.max_bytes = super::MAX_SNAPSHOT_BYTES + 1;
        assert!(matches!(
            VmInstance::from_snapshot(snapshot),
            Err(RuntimeError::InvalidSnapshot { .. })
        ));

        let mut snapshot = VmInstance::new("snapshot").snapshot();
        snapshot.entries = (0..=super::MAX_SNAPSHOT_ENTRIES)
            .map(|index| super::SnapshotEntry {
                path: format!("/workspace/file-{index}"),
                kind: super::SnapshotEntryKind::File,
                bytes: Vec::new(),
            })
            .collect();
        assert!(matches!(
            VmInstance::from_snapshot(snapshot),
            Err(RuntimeError::InvalidSnapshot { .. })
        ));
    }

    #[test]
    fn scheduler_runs_processes_round_robin_and_reports_results() {
        let mut vm = VmInstance::new("scheduler");
        let first = vm
            .spawn(None, add_program(), vec!["first".to_owned()])
            .unwrap();
        let second = vm
            .spawn(None, add_program(), vec!["second".to_owned()])
            .unwrap();

        let events = vm.run_until_idle(8).unwrap();
        let pids = events
            .iter()
            .map(|event| match event {
                ProcessEvent::InstructionExecuted { pid, .. }
                | ProcessEvent::ProcessExited { pid, .. }
                | ProcessEvent::ProcessFailed { pid, .. } => *pid,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            pids,
            vec![first, second, first, second, first, second, first, second]
        );
        assert_eq!(vm.wait(first), Ok(WaitStatus::Exited { code: 5 }));
        assert_eq!(vm.wait(second), Ok(WaitStatus::Exited { code: 5 }));
        assert_eq!(
            vm.process_info(first).unwrap().state,
            ProcessState::Exited { code: 5 }
        );
    }

    #[test]
    fn spawn_rejects_programs_that_fail_structural_validation() {
        let mut vm = VmInstance::new("validation");

        assert!(matches!(
            vm.spawn(None, vec![Instruction::Push(1)], vec![]),
            Err(RuntimeError::InvalidProgram { .. })
        ));
        assert_eq!(vm.process_count(), 0);
    }

    #[test]
    fn scheduler_discards_processes_exited_by_syscall() {
        let mut vm = VmInstance::new("stale-queue");
        let pid = vm
            .spawn(
                None,
                vec![Instruction::Push(0), Instruction::Halt],
                vec!["exit".to_owned()],
            )
            .unwrap();

        vm.syscall(pid, Syscall::Exit { code: 0 }).unwrap();

        assert_eq!(vm.run_until_idle(1), Ok(Vec::new()));
        assert_eq!(vm.wait(pid), Ok(WaitStatus::Exited { code: 0 }));
    }

    #[test]
    fn syscalls_use_the_callers_guest_working_directory() {
        let mut vm = VmInstance::new("syscalls");
        let pid = vm
            .spawn(
                None,
                vec![Instruction::Push(0), Instruction::Halt],
                vec!["init".to_owned()],
            )
            .unwrap();
        vm.syscall(
            pid,
            Syscall::MakeDirectory {
                path: "/workspace/app".to_owned(),
                recursive: false,
            },
        )
        .unwrap();
        vm.syscall(
            pid,
            Syscall::WriteFile {
                path: "/workspace/app/main.arvm".to_owned(),
                content: b"PUSH 2".to_vec(),
            },
        )
        .unwrap();
        let listing = vm
            .syscall(
                pid,
                Syscall::ListDirectory {
                    path: "/workspace/app".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(
            listing,
            SyscallResult::Directory(vec![super::DirectoryEntry {
                name: "main.arvm".to_owned(),
                kind: EntryKind::File,
                size: 6,
            }])
        );
    }

    #[test]
    fn process_can_spawn_and_poll_a_child() {
        let mut vm = VmInstance::new("processes");
        let parent = vm
            .spawn(
                None,
                vec![Instruction::Push(0), Instruction::Halt],
                vec!["parent".to_owned()],
            )
            .unwrap();
        let child = match vm
            .syscall(
                parent,
                Syscall::Spawn {
                    program: add_program(),
                    argv: vec!["child".to_owned()],
                },
            )
            .unwrap()
        {
            SyscallResult::Process(pid) => pid,
            result => panic!("unexpected syscall result: {result:?}"),
        };
        assert_eq!(vm.wait(child), Ok(WaitStatus::Running));
        vm.run_until_idle(8).unwrap();
        assert_eq!(vm.wait(child), Ok(WaitStatus::Exited { code: 5 }));
    }

    #[test]
    fn manager_scopes_same_vm_id_to_each_owner() {
        let mut manager = VmManager::with_max_instances(4, ResourceLimits::default());
        manager.create("alice", "dev").unwrap();
        manager.create("bob", "dev").unwrap();

        manager
            .get_mut("alice", "dev")
            .unwrap()
            .write_file("/workspace/secret", "alice-only")
            .unwrap();
        assert_eq!(
            manager
                .get("alice", "dev")
                .unwrap()
                .read_text("/workspace/secret")
                .unwrap(),
            "alice-only"
        );
        assert!(matches!(
            manager
                .get("bob", "dev")
                .unwrap()
                .read_file("/workspace/secret"),
            Err(RuntimeError::NotFound { .. })
        ));
        assert!(matches!(
            manager.get("mallory", "dev"),
            Err(VmManagerError::NotFound { .. })
        ));
        assert_eq!(manager.list("alice").len(), 1);
        assert_eq!(manager.list("bob").len(), 1);
    }

    #[test]
    fn manager_enforces_instance_quota_and_duplicate_ids() {
        let mut manager = VmManager::with_max_instances(2, ResourceLimits::default());
        manager.create("alice", "dev").unwrap();
        assert!(matches!(
            manager.create("alice", "dev"),
            Err(VmManagerError::AlreadyExists { .. })
        ));
        manager.create("bob", "other").unwrap();
        assert!(matches!(
            manager.create("bob", "second"),
            Err(VmManagerError::InstanceLimitReached { limit: 2 })
        ));
        assert!(matches!(
            VmInstance::with_limits(
                "invalid",
                ResourceLimits {
                    max_inodes: 3,
                    ..ResourceLimits::default()
                }
            ),
            Err(RuntimeError::InvalidLimits { .. })
        ));
    }

    #[test]
    fn scheduler_honors_the_per_instance_step_limit() {
        let mut vm = VmInstance::with_limits(
            "limited",
            ResourceLimits {
                max_steps: 2,
                ..ResourceLimits::default()
            },
        )
        .unwrap();
        vm.spawn(None, add_program(), vec!["limited".to_owned()])
            .unwrap();

        assert_eq!(
            vm.run_until_idle(8),
            Err(RuntimeError::ExecutionLimitExceeded { limit: 2 })
        );
        assert_eq!(vm.ticks(), 2);
    }
}
