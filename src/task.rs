//! Versioned, replay-validated task state for long-running agent work.
//!
//! This domain contract is independent of CLI sessions and guest runners. A
//! checkpoint names the existing session position until one task service owns
//! both records. Deserializing bytes is followed by replay validation so a
//! corrupt status or event order cannot silently resume an invalid task.

use std::fmt;

use serde::{Deserialize, Serialize};

pub const TASK_SCHEMA_VERSION: u32 = 1;
const MAX_EVENTS: usize = 512;
const MAX_PLAN_STEPS: usize = 128;
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_EVENT_BYTES: usize = 8 * 1024;
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBudget {
    pub max_model_steps: u32,
    pub max_tool_calls: u32,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub id: String,
    pub owner: String,
    pub workspace_id: String,
    pub prompt: String,
    pub input_version_ids: Vec<String>,
    pub budget: TaskBudget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPlan {
    pub steps: Vec<String>,
    pub intended_outputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCheckpoint {
    pub session_id: String,
    pub completed_turns: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskEventKind {
    Started,
    PlanUpdated { plan: TaskPlan },
    Checkpointed { checkpoint: TaskCheckpoint },
    ApprovalRequested { request_id: String },
    ApprovalResolved { request_id: String, allowed: bool },
    Completed { output_version_ids: Vec<String> },
    Failed { reason: String },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEvent {
    pub sequence: u64,
    pub kind: TaskEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRecord {
    schema_version: u32,
    spec: TaskSpec,
    status: TaskStatus,
    plan: Option<TaskPlan>,
    checkpoint: Option<TaskCheckpoint>,
    pending_approval: Option<String>,
    events: Vec<TaskEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskError {
    InvalidField(&'static str),
    InvalidTransition {
        status: TaskStatus,
        event: &'static str,
    },
    ApprovalMismatch,
    EventLimitExceeded,
    SequenceExhausted,
    UnsupportedSchema {
        actual: u32,
    },
    InvalidRecord,
    Decode(String),
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField(field) => write!(formatter, "invalid task {field}"),
            Self::InvalidTransition { status, event } => {
                write!(formatter, "cannot apply {event} while task is {status:?}")
            }
            Self::ApprovalMismatch => {
                formatter.write_str("approval response does not match the pending request")
            }
            Self::EventLimitExceeded => formatter.write_str("task event limit exceeded"),
            Self::SequenceExhausted => formatter.write_str("task event sequence exhausted"),
            Self::UnsupportedSchema { actual } => write!(
                formatter,
                "unsupported task schema {actual}; expected {TASK_SCHEMA_VERSION}"
            ),
            Self::InvalidRecord => {
                formatter.write_str("task record does not match its event history")
            }
            Self::Decode(message) => write!(formatter, "cannot decode task: {message}"),
        }
    }
}

impl std::error::Error for TaskError {}

impl TaskRecord {
    pub fn new(spec: TaskSpec) -> Result<Self, TaskError> {
        validate_file_id(&spec.id, "id")?;
        validate_resource_id(&spec.owner, "owner")?;
        validate_resource_id(&spec.workspace_id, "workspace id")?;
        validate_text(&spec.prompt, "prompt")?;
        validate_ids(&spec.input_version_ids, "input version id")?;
        if spec.budget.max_model_steps == 0
            || spec.budget.max_tool_calls == 0
            || spec.budget.timeout_seconds == 0
        {
            return Err(TaskError::InvalidField("budget"));
        }
        Ok(Self {
            schema_version: TASK_SCHEMA_VERSION,
            spec,
            status: TaskStatus::Pending,
            plan: None,
            checkpoint: None,
            pending_approval: None,
            events: Vec::new(),
        })
    }

    pub fn spec(&self) -> &TaskSpec {
        &self.spec
    }

    pub fn status(&self) -> TaskStatus {
        self.status
    }

    pub fn plan(&self) -> Option<&TaskPlan> {
        self.plan.as_ref()
    }

    pub fn checkpoint(&self) -> Option<&TaskCheckpoint> {
        self.checkpoint.as_ref()
    }

    pub fn events(&self) -> &[TaskEvent] {
        &self.events
    }

    /// Apply a validated state transition without mutating on failure.
    pub fn append(&mut self, kind: TaskEventKind) -> Result<&TaskEvent, TaskError> {
        if self.events.len() >= MAX_EVENTS {
            return Err(TaskError::EventLimitExceeded);
        }
        let sequence = self
            .events
            .last()
            .map_or(Some(1), |event| event.sequence.checked_add(1))
            .ok_or(TaskError::SequenceExhausted)?;
        let next = self.transition(&kind)?;
        if serde_json::to_vec(&kind)
            .map_err(|error| TaskError::Decode(error.to_string()))?
            .len()
            > MAX_EVENT_BYTES
        {
            return Err(TaskError::InvalidField("event size"));
        }
        self.status = next.status;
        self.plan = next.plan;
        self.checkpoint = next.checkpoint;
        self.pending_approval = next.pending_approval;
        self.events.push(TaskEvent { sequence, kind });
        Ok(self.events.last().expect("event was pushed"))
    }

    /// Decode and replay all events, rejecting changed state or missing events.
    pub fn restore(bytes: &[u8]) -> Result<Self, TaskError> {
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(TaskError::InvalidField("record size"));
        }
        let stored: Self =
            serde_json::from_slice(bytes).map_err(|error| TaskError::Decode(error.to_string()))?;
        if stored.schema_version != TASK_SCHEMA_VERSION {
            return Err(TaskError::UnsupportedSchema {
                actual: stored.schema_version,
            });
        }
        let mut replayed = Self::new(stored.spec.clone())?;
        for event in &stored.events {
            let applied = replayed.append(event.kind.clone())?;
            if applied.sequence != event.sequence {
                return Err(TaskError::InvalidRecord);
            }
        }
        if replayed != stored {
            return Err(TaskError::InvalidRecord);
        }
        Ok(stored)
    }

    fn transition(&self, kind: &TaskEventKind) -> Result<NextState, TaskError> {
        let mut next = NextState {
            status: self.status,
            plan: self.plan.clone(),
            checkpoint: self.checkpoint.clone(),
            pending_approval: self.pending_approval.clone(),
        };
        match (self.status, kind) {
            (TaskStatus::Pending, TaskEventKind::Started) => next.status = TaskStatus::Running,
            (TaskStatus::Running, TaskEventKind::PlanUpdated { plan }) => {
                if plan.steps.is_empty() || plan.steps.len() > MAX_PLAN_STEPS {
                    return Err(TaskError::InvalidField("plan steps"));
                }
                for text in plan.steps.iter().chain(&plan.intended_outputs) {
                    validate_text(text, "plan text")?;
                }
                next.plan = Some(plan.clone());
            }
            (TaskStatus::Running, TaskEventKind::Checkpointed { checkpoint }) => {
                validate_file_id(&checkpoint.session_id, "checkpoint session id")?;
                if self
                    .checkpoint
                    .as_ref()
                    .is_some_and(|old| checkpoint.completed_turns < old.completed_turns)
                {
                    return Err(TaskError::InvalidField("checkpoint turn count"));
                }
                next.checkpoint = Some(checkpoint.clone());
            }
            (TaskStatus::Running, TaskEventKind::ApprovalRequested { request_id }) => {
                validate_text(request_id, "approval request id")?;
                next.status = TaskStatus::WaitingApproval;
                next.pending_approval = Some(request_id.clone());
            }
            (TaskStatus::WaitingApproval, TaskEventKind::ApprovalResolved { request_id, .. }) => {
                if self.pending_approval.as_deref() != Some(request_id) {
                    return Err(TaskError::ApprovalMismatch);
                }
                next.status = TaskStatus::Running;
                next.pending_approval = None;
            }
            (TaskStatus::Running, TaskEventKind::Completed { output_version_ids }) => {
                validate_ids(output_version_ids, "output version id")?;
                next.status = TaskStatus::Completed;
            }
            (
                TaskStatus::Running | TaskStatus::WaitingApproval,
                TaskEventKind::Failed { reason },
            ) => {
                validate_text(reason, "failure reason")?;
                next.status = TaskStatus::Failed;
                next.pending_approval = None;
            }
            (
                TaskStatus::Pending | TaskStatus::Running | TaskStatus::WaitingApproval,
                TaskEventKind::Cancelled,
            ) => {
                next.status = TaskStatus::Cancelled;
                next.pending_approval = None;
            }
            (status, event) => {
                return Err(TaskError::InvalidTransition {
                    status,
                    event: event.name(),
                });
            }
        }
        Ok(next)
    }
}

struct NextState {
    status: TaskStatus,
    plan: Option<TaskPlan>,
    checkpoint: Option<TaskCheckpoint>,
    pending_approval: Option<String>,
}

impl TaskEventKind {
    fn name(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::PlanUpdated { .. } => "plan_updated",
            Self::Checkpointed { .. } => "checkpointed",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::ApprovalResolved { .. } => "approval_resolved",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

fn validate_file_id(value: &str, field: &'static str) -> Result<(), TaskError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(TaskError::InvalidField(field));
    }
    Ok(())
}

fn validate_resource_id(value: &str, field: &'static str) -> Result<(), TaskError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(TaskError::InvalidField(field));
    }
    Ok(())
}

fn validate_ids(values: &[String], field: &'static str) -> Result<(), TaskError> {
    if values.len() > MAX_PLAN_STEPS {
        return Err(TaskError::InvalidField(field));
    }
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        validate_resource_id(value, field)?;
        if !seen.insert(value) {
            return Err(TaskError::InvalidField(field));
        }
    }
    Ok(())
}

fn validate_text(value: &str, field: &'static str) -> Result<(), TaskError> {
    if value.trim().is_empty() || value.len() > MAX_TEXT_BYTES {
        return Err(TaskError::InvalidField(field));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> TaskSpec {
        TaskSpec {
            id: "task-1".to_owned(),
            owner: "alice".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            prompt: "fix the bug".to_owned(),
            input_version_ids: vec!["version-1".to_owned()],
            budget: TaskBudget {
                max_model_steps: 20,
                max_tool_calls: 40,
                timeout_seconds: 300,
            },
        }
    }

    #[test]
    fn task_replays_a_plan_checkpoint_and_completion() {
        let mut task = TaskRecord::new(spec()).unwrap();
        task.append(TaskEventKind::Started).unwrap();
        task.append(TaskEventKind::PlanUpdated {
            plan: TaskPlan {
                steps: vec!["edit file".to_owned(), "run tests".to_owned()],
                intended_outputs: vec!["patch".to_owned()],
            },
        })
        .unwrap();
        task.append(TaskEventKind::Checkpointed {
            checkpoint: TaskCheckpoint {
                session_id: "session-1".to_owned(),
                completed_turns: 1,
            },
        })
        .unwrap();
        task.append(TaskEventKind::Completed {
            output_version_ids: vec!["version-2".to_owned()],
        })
        .unwrap();

        let encoded = serde_json::to_vec(&task).unwrap();
        assert_eq!(TaskRecord::restore(&encoded).unwrap(), task);
        assert_eq!(task.status(), TaskStatus::Completed);
        assert_eq!(task.events().len(), 4);
    }

    #[test]
    fn invalid_transition_does_not_mutate_task() {
        let mut task = TaskRecord::new(spec()).unwrap();
        let original = task.clone();
        assert!(matches!(
            task.append(TaskEventKind::Completed {
                output_version_ids: vec![],
            }),
            Err(TaskError::InvalidTransition { .. })
        ));
        assert_eq!(task, original);
    }

    #[test]
    fn approval_must_match_before_task_resumes() {
        let mut task = TaskRecord::new(spec()).unwrap();
        task.append(TaskEventKind::Started).unwrap();
        task.append(TaskEventKind::ApprovalRequested {
            request_id: "approval-1".to_owned(),
        })
        .unwrap();
        assert_eq!(task.status(), TaskStatus::WaitingApproval);
        assert_eq!(
            task.append(TaskEventKind::ApprovalResolved {
                request_id: "other".to_owned(),
                allowed: true,
            }),
            Err(TaskError::ApprovalMismatch)
        );
        task.append(TaskEventKind::ApprovalResolved {
            request_id: "approval-1".to_owned(),
            allowed: false,
        })
        .unwrap();
        assert_eq!(task.status(), TaskStatus::Running);
    }

    #[test]
    fn restore_rejects_changed_status_and_event_sequence() {
        let mut task = TaskRecord::new(spec()).unwrap();
        task.append(TaskEventKind::Started).unwrap();
        let mut encoded = serde_json::to_value(&task).unwrap();
        encoded["status"] = serde_json::json!("completed");
        assert_eq!(
            TaskRecord::restore(&serde_json::to_vec(&encoded).unwrap()),
            Err(TaskError::InvalidRecord)
        );
        encoded["status"] = serde_json::json!("running");
        encoded["events"][0]["sequence"] = serde_json::json!(9);
        assert_eq!(
            TaskRecord::restore(&serde_json::to_vec(&encoded).unwrap()),
            Err(TaskError::InvalidRecord)
        );
    }

    #[test]
    fn restore_rejects_future_schema() {
        let task = TaskRecord::new(spec()).unwrap();
        let mut encoded = serde_json::to_value(&task).unwrap();
        encoded["schema_version"] = serde_json::json!(TASK_SCHEMA_VERSION + 1);
        assert_eq!(
            TaskRecord::restore(&serde_json::to_vec(&encoded).unwrap()),
            Err(TaskError::UnsupportedSchema {
                actual: TASK_SCHEMA_VERSION + 1,
            })
        );
    }

    #[test]
    fn task_ids_cannot_name_parent_directories() {
        let mut unsafe_spec = spec();
        unsafe_spec.id = "..".to_owned();
        assert_eq!(
            TaskRecord::new(unsafe_spec),
            Err(TaskError::InvalidField("id"))
        );
    }

    #[test]
    fn oversized_event_fails_without_mutating_task() {
        let mut task = TaskRecord::new(spec()).unwrap();
        task.append(TaskEventKind::Started).unwrap();
        let before = task.clone();
        assert_eq!(
            task.append(TaskEventKind::Failed {
                reason: "x".repeat(MAX_EVENT_BYTES),
            }),
            Err(TaskError::InvalidField("event size"))
        );
        assert_eq!(task, before);
    }
}
