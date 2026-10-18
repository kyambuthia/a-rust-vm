//! Values and events that cross the model, tool, and host boundaries.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A value that can cross the model-to-tool boundary without requiring a JSON
/// dependency in the VM crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolValue {
    Text(String),
    Integer(i32),
    List(Vec<ToolValue>),
    Boolean(bool),
}

/// Arguments supplied to a tool call.
pub type ToolArguments = BTreeMap<String, ToolValue>;

/// A model-requested tool invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: ToolArguments,
}

impl ToolCall {
    /// Create a tool call with a stable caller-provided identifier.
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: ToolArguments) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// The result returned by a tool execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

/// A request that must be approved before a future guarded tool executes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub tool: String,
    pub description: String,
}

/// The decision returned by the host for a guarded tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny { reason: String },
}

/// Events emitted by one agent turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    UserMessage { content: String },
    AssistantText { content: String },
    AssistantDelta { content: String },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    PermissionRequested(PermissionRequest),
    RepeatedToolCall { tool: String, count: usize },
    Cancelled,
    Error { message: String },
    Done,
}

/// A model-facing description of a registered tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: String,
}

/// A message retained between agent turns and supplied as bounded context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ConversationMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_call: None,
            tool_call_id: None,
        }
    }

    pub(super) fn assistant_tool_call(call: &ToolCall) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: String::new(),
            tool_call: Some(call.clone()),
            tool_call_id: None,
        }
    }

    pub(super) fn tool_result(result: &ToolResult) -> Self {
        Self {
            role: "tool".to_owned(),
            content: format_tool_result(result),
            tool_call: None,
            tool_call_id: Some(result.id.clone()),
        }
    }
}

impl ToolSpec {
    pub(crate) fn new(name: &str, description: &str, input_schema: &str) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            input_schema: input_schema.to_owned(),
        }
    }
}

pub(super) fn format_tool_result(result: &ToolResult) -> String {
    let status = if result.is_error { "error" } else { "ok" };
    format!(
        "tool_result id={} name={} status={status}\n{}",
        result.id, result.name, result.content
    )
}
