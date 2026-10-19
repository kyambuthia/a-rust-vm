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

/// Token, cost, and timing figures for one model step.
///
/// Providers fill what they report; the turn loop adds the timings. Cost is
/// in millionths of a US dollar so the type stays exact and comparable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Input tokens served from the provider's prompt cache.
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micro_usd: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Time from sending the request to the end of the response.
    pub latency_ms: u64,
    /// Time from sending the request to the first streamed text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
}

impl Usage {
    /// Add another step's figures to a running total.
    pub fn accumulate(&mut self, step: &Usage) {
        self.input_tokens += step.input_tokens;
        self.output_tokens += step.output_tokens;
        self.cached_tokens += step.cached_tokens;
        self.reasoning_tokens += step.reasoning_tokens;
        self.cost_micro_usd = match (self.cost_micro_usd, step.cost_micro_usd) {
            (None, None) => None,
            (total, step) => Some(total.unwrap_or(0) + step.unwrap_or(0)),
        };
        self.latency_ms += step.latency_ms;
    }

    /// One-line summary such as `1,204 in (800 cached) · 56 out · $0.000120 · 2.1 s`.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{} in", group_digits(self.input_tokens))];
        if self.cached_tokens > 0 {
            parts[0].push_str(&format!(" ({} cached)", group_digits(self.cached_tokens)));
        }
        parts.push(format!("{} out", group_digits(self.output_tokens)));
        if let Some(cost) = self.cost_micro_usd {
            parts.push(format!("${}.{:06}", cost / 1_000_000, cost % 1_000_000));
        }
        parts.push(format!(
            "{}.{} s",
            self.latency_ms / 1000,
            (self.latency_ms % 1000) / 100
        ));
        parts.join(" · ")
    }
}

fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Events emitted by one agent turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    UserMessage {
        content: String,
    },
    AssistantText {
        content: String,
    },
    AssistantDelta {
        content: String,
    },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    PermissionRequested(PermissionRequest),
    RepeatedToolCall {
        tool: String,
        count: usize,
    },
    /// Figures for one model step, when the model reports them.
    Usage(Usage),
    Cancelled,
    Error {
        message: String,
    },
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

    pub(crate) fn assistant_tool_call(call: &ToolCall) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: String::new(),
            tool_call: Some(call.clone()),
            tool_call_id: None,
        }
    }

    pub(crate) fn tool_result(result: &ToolResult) -> Self {
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
