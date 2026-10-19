//! Delegated child turns, persistent subagent sessions, and the delegation tool.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::{
    Agent, AgentError, AgentEvent, ConversationMessage, DEFAULT_CONTEXT_CHARS,
    DEFAULT_CONTEXT_MESSAGES, Model, Tool, ToolArguments, ToolError, ToolRegistry, ToolSpec,
    ensure_arguments, required_text,
};
use crate::session::{Session, SessionStore};

/// Bounds for one delegated child turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentConfig {
    pub max_steps: usize,
    pub max_tool_calls: usize,
    pub max_context_messages: usize,
    pub max_context_chars: usize,
    pub system_prompt: Option<String>,
    pub max_transcript_chars: usize,
    pub max_depth: usize,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            max_steps: 4,
            max_tool_calls: 8,
            max_context_messages: DEFAULT_CONTEXT_MESSAGES,
            max_context_chars: DEFAULT_CONTEXT_CHARS,
            system_prompt: None,
            max_transcript_chars: 4 * 1024,
            max_depth: 1,
        }
    }
}

/// Bounded transcript returned by one delegated child turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentResult {
    pub events: Vec<AgentEvent>,
    pub assistant_text: String,
    pub tool_calls: usize,
}

/// Errors from one-off subagent delegation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentError {
    EmptyPrompt,
    InvalidId,
    DepthLimitExceeded { limit: usize },
    Storage(String),
    Agent(AgentError),
}

impl fmt::Display for SubagentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPrompt => formatter.write_str("subagent prompt must not be empty"),
            Self::InvalidId => formatter.write_str("subagent id must not be empty"),
            Self::DepthLimitExceeded { limit } => {
                write!(formatter, "subagent delegation depth exceeded: {limit}")
            }
            Self::Storage(message) => {
                write!(formatter, "subagent transcript store failed: {message}")
            }
            Self::Agent(error) => write!(formatter, "subagent turn failed: {error}"),
        }
    }
}

impl std::error::Error for SubagentError {}

/// Run one bounded child turn with its own session and default-deny approvals.
///
/// The child starts with an empty conversation, so parent context never leaks
/// in. The returned transcript is truncated to the configured character bound.
pub fn run_subagent<M>(
    model: M,
    tools: ToolRegistry,
    prompt: impl Into<String>,
    config: SubagentConfig,
) -> Result<SubagentResult, SubagentError>
where
    M: Model,
{
    run_subagent_with_cancel(
        model,
        tools,
        prompt,
        config,
        Arc::new(AtomicBool::new(false)),
    )
}

/// Run one bounded child turn sharing the caller's cancellation token.
///
/// A pre-cancelled token fails closed before the model is consulted, so a
/// parent/host cancel stops delegated tool turns through the same primitive
/// used by [`Agent`] and [`SubagentSession::send`].
pub fn run_subagent_with_cancel<M>(
    model: M,
    tools: ToolRegistry,
    prompt: impl Into<String>,
    config: SubagentConfig,
    cancel: Arc<AtomicBool>,
) -> Result<SubagentResult, SubagentError>
where
    M: Model,
{
    let prompt = prompt.into();
    if prompt.trim().is_empty() {
        return Err(SubagentError::EmptyPrompt);
    }
    let mut agent = Agent::new(model, tools)
        .with_max_steps(config.max_steps)
        .with_max_tool_calls(config.max_tool_calls)
        .with_cancel_token(cancel)
        .with_context_limits(config.max_context_messages, config.max_context_chars);
    if let Some(system_prompt) = config.system_prompt.clone() {
        agent.set_system_prompt(system_prompt);
    }
    let events = agent.run(prompt).map_err(SubagentError::Agent)?;
    let tool_calls = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolCall(_)))
        .count();
    let mut assistant_text = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::AssistantText { content } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if assistant_text.len() > config.max_transcript_chars {
        truncate_at_char_boundary(&mut assistant_text, config.max_transcript_chars);
    }
    Ok(SubagentResult {
        events,
        assistant_text,
        tool_calls,
    })
}

/// A persistent child session with its own independent conversation history.
///
/// Each `send` rehydrates a fresh [`Agent`] from the stored history, so a
/// multi-step delegated task accumulates context while the parent-visible
/// one-off `run_subagent` entry point keeps its stateless fail-closed shape.
/// `save`/`load` persist the config plus history through [`SessionStore`].
#[derive(Debug, Clone)]
pub struct SubagentSession {
    id: String,
    config: SubagentConfig,
    history: Vec<ConversationMessage>,
    cancel: Arc<AtomicBool>,
}

impl PartialEq for SubagentSession {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.config == other.config && self.history == other.history
    }
}

impl Eq for SubagentSession {}

const SUBAGENT_CONFIG_ROLE: &str = "subagent-config";

impl SubagentSession {
    pub fn new(id: impl Into<String>, config: SubagentConfig) -> Result<Self, SubagentError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(SubagentError::InvalidId);
        }
        Ok(Self {
            id,
            config,
            history: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn with_cancel_token(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn cancel_token(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn history(&self) -> &[ConversationMessage] {
        &self.history
    }

    pub fn save(&self, store: &SessionStore) -> Result<(), SubagentError> {
        let mut session = Session::new(self.id.clone())
            .map_err(|error| SubagentError::Storage(error.to_string()))?;
        let config = serde_json::to_string(&self.config)
            .map_err(|error| SubagentError::Storage(error.to_string()))?;
        session.push(SUBAGENT_CONFIG_ROLE, config);
        for message in &self.history {
            session.push(message.role.clone(), message.content.clone());
        }
        store
            .save(&session)
            .map_err(|error| SubagentError::Storage(error.to_string()))
    }

    pub fn load(store: &SessionStore, id: &str) -> Result<Self, SubagentError> {
        if id.trim().is_empty() {
            return Err(SubagentError::InvalidId);
        }
        let session = store
            .load(id)
            .map_err(|error| SubagentError::Storage(error.to_string()))?;
        let (first, rest) = session.messages.split_first().ok_or_else(|| {
            SubagentError::Storage(format!(
                "subagent transcript '{}' has no config",
                session.id
            ))
        })?;
        if first.role != SUBAGENT_CONFIG_ROLE {
            return Err(SubagentError::Storage(format!(
                "subagent transcript '{}' has invalid config entry",
                session.id
            )));
        }
        let config: SubagentConfig = serde_json::from_str(&first.content)
            .map_err(|error| SubagentError::Storage(error.to_string()))?;
        Ok(Self {
            id: session.id,
            config,
            history: rest
                .iter()
                .map(|message| {
                    ConversationMessage::new(message.role.clone(), message.content.clone())
                })
                .collect(),
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn send<M>(
        &mut self,
        model: M,
        tools: ToolRegistry,
        prompt: impl Into<String>,
    ) -> Result<SubagentResult, SubagentError>
    where
        M: Model,
    {
        let prompt = prompt.into();
        if prompt.trim().is_empty() {
            return Err(SubagentError::EmptyPrompt);
        }
        let mut agent = Agent::new(model, tools)
            .with_max_steps(self.config.max_steps)
            .with_max_tool_calls(self.config.max_tool_calls)
            .with_cancel_token(self.cancel.clone())
            .with_context_limits(
                self.config.max_context_messages,
                self.config.max_context_chars,
            );
        if let Some(system_prompt) = self.config.system_prompt.clone() {
            agent.set_system_prompt(system_prompt);
        }
        agent.set_conversation(self.history.clone());
        let events = agent.run(prompt).map_err(SubagentError::Agent)?;
        self.history = agent.conversation().to_vec();
        let tool_calls = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCall(_)))
            .count();
        let mut assistant_text = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::AssistantText { content } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if assistant_text.len() > self.config.max_transcript_chars {
            truncate_at_char_boundary(&mut assistant_text, self.config.max_transcript_chars);
        }
        Ok(SubagentResult {
            events,
            assistant_text,
            tool_calls,
        })
    }
}

// A parent-visible tool that delegates one bounded subtask to an isolated child.
std::thread_local! {
    static SUBAGENT_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct DepthGuard {
    previous: usize,
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        SUBAGENT_DEPTH.with(|depth| depth.set(self.previous));
    }
}

#[cfg(test)]
pub(crate) fn subagent_depth() -> usize {
    SUBAGENT_DEPTH.with(|depth| depth.get())
}

/// A parent-visible tool that delegates one bounded subtask to an isolated child.
pub struct SubagentTool {
    make_model: Box<dyn FnMut() -> Box<dyn Model>>,
    make_tools: Box<dyn FnMut() -> ToolRegistry>,
    config: SubagentConfig,
    cancel: Arc<AtomicBool>,
}

impl SubagentTool {
    /// Create a delegation tool with factories for fresh child models and tools.
    pub fn new<M, MF, TF>(make_model: MF, make_tools: TF, config: SubagentConfig) -> Self
    where
        M: Model + 'static,
        MF: FnMut() -> M + 'static,
        TF: FnMut() -> ToolRegistry + 'static,
    {
        let mut make_model = make_model;
        Self {
            make_model: Box::new(move || Box::new(make_model()) as Box<dyn Model>),
            make_tools: Box::new(make_tools),
            config,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_cancel_token(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn cancel_token(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
}

impl Tool for SubagentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "delegate_subagent",
            "Delegate one bounded subtask to an isolated child agent.",
            r#"{"type":"object","properties":{"task":{"type":"string"}},"required":["task"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["task"])?;
        let task = required_text(arguments, "task")?;
        let entered = SUBAGENT_DEPTH.with(|depth| {
            if depth.get() >= self.config.max_depth {
                return None;
            }
            let guard = DepthGuard {
                previous: depth.get(),
            };
            depth.set(guard.previous + 1);
            Some(guard)
        });
        let Some(_guard) = entered else {
            return Err(ToolError::new(
                SubagentError::DepthLimitExceeded {
                    limit: self.config.max_depth,
                }
                .to_string(),
            ));
        };
        let model = (self.make_model)();
        let tools = (self.make_tools)();
        let cancel = self.cancel.clone();
        run_subagent_with_cancel(model, tools, task, self.config.clone(), cancel)
            .map(|result| {
                let mut output = if result.assistant_text.is_empty() {
                    format!("tool_calls={}", result.tool_calls)
                } else {
                    format!(
                        "{}\ntool_calls={}",
                        result.assistant_text, result.tool_calls
                    )
                };
                let errors = result
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        AgentEvent::ToolResult(tool_result) if tool_result.is_error => {
                            Some(tool_result.content.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !errors.is_empty() {
                    output.push_str(&format!("\ntool_errors:\n{errors}"));
                }
                if result
                    .events
                    .iter()
                    .any(|event| matches!(event, AgentEvent::Cancelled))
                {
                    output.push_str("\ncancelled");
                }
                output
            })
            .map_err(|error| ToolError::new(error.to_string()))
    }

    fn set_cancel_token(&mut self, cancel: Arc<AtomicBool>) {
        self.cancel = cancel;
    }
}

/// Cut `text` to at most `max_bytes`, backing off to a character boundary so
/// multi-byte text cannot cause a panic.
fn truncate_at_char_boundary(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}
