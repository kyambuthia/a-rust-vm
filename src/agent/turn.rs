//! The bounded model-tool loop that runs one agent turn.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{
    AgentEvent, ConversationMessage, DEFAULT_SYSTEM_PROMPT, Model, ModelError, ModelRequest,
    ModelResponse, ModelStreamEvent, PermissionDecision, PermissionRequest, RouteRequest,
    ToolArguments, ToolCall, ToolRegistry, ToolResult, ToolValue,
};
use crate::permissions::{PermissionEffect, PermissionPolicy, SessionApprovals, TargetKind};

pub(super) const DEFAULT_CONTEXT_MESSAGES: usize = 24;
pub(super) const DEFAULT_CONTEXT_CHARS: usize = 32 * 1024;

/// How a turn that did not fail came to an end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnEnd {
    Answered,
    Cancelled,
    RepeatedToolCall,
}

/// Errors raised while coordinating one agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    Model(ModelError),
    StepLimitExceeded { limit: usize },
    ToolCallLimitExceeded { limit: usize },
    TurnTimeoutExceeded,
    PatternLoopDetected { period: usize },
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(error) => write!(formatter, "model error: {error}"),
            Self::StepLimitExceeded { limit } => {
                write!(formatter, "agent step limit exceeded: {limit}")
            }
            Self::ToolCallLimitExceeded { limit } => {
                write!(formatter, "agent tool call limit exceeded: {limit}")
            }
            Self::TurnTimeoutExceeded => write!(formatter, "agent turn timeout exceeded"),
            Self::PatternLoopDetected { period } => {
                write!(
                    formatter,
                    "agent tool pattern loop detected (period {period})"
                )
            }
        }
    }
}

impl std::error::Error for AgentError {}

/// Per-turn bookkeeping shared by the tool calls of every step.
struct BatchState<'a> {
    tool_results: &'a mut Vec<ToolResult>,
    turn_context: &'a mut Vec<ConversationMessage>,
    last_call: &'a mut Option<(String, String)>,
    executed: &'a mut Vec<String>,
    tool_calls: &'a mut usize,
}

/// A bounded model-tool execution loop.
pub struct Agent<M> {
    model: M,
    tools: ToolRegistry,
    max_steps: usize,
    max_tool_calls: usize,
    turn_timeout: Duration,
    cancel: Arc<AtomicBool>,
    route_request: RouteRequest,
    system_prompt: String,
    conversation: Vec<ConversationMessage>,
    max_context_messages: usize,
    max_context_chars: usize,
    permission_policy: PermissionPolicy,
    session_approvals: SessionApprovals,
    redactor: crate::redact::Redactor,
}

impl<M> Agent<M>
where
    M: Model,
{
    pub fn new(model: M, tools: ToolRegistry) -> Self {
        Self {
            model,
            tools,
            max_steps: 8,
            max_tool_calls: 24,
            turn_timeout: Duration::from_secs(300),
            cancel: Arc::new(AtomicBool::new(false)),
            route_request: RouteRequest::default(),
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_owned(),
            conversation: Vec::new(),
            max_context_messages: DEFAULT_CONTEXT_MESSAGES,
            max_context_chars: DEFAULT_CONTEXT_CHARS,
            permission_policy: PermissionPolicy::new(),
            session_approvals: SessionApprovals::new(),
            redactor: crate::redact::Redactor::from_environment(),
        }
    }

    /// Replace the redactor applied to every tool result before it reaches
    /// the model, the event stream, or a transcript.
    pub fn with_redactor(mut self, redactor: crate::redact::Redactor) -> Self {
        self.redactor = redactor;
        self
    }

    pub fn with_max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    pub fn with_max_tool_calls(mut self, max_tool_calls: usize) -> Self {
        self.max_tool_calls = max_tool_calls;
        self
    }

    pub fn with_turn_timeout(mut self, turn_timeout: Duration) -> Self {
        self.turn_timeout = turn_timeout;
        self
    }

    pub fn with_cancel_token(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn cancel_token(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn set_conversation(&mut self, conversation: Vec<ConversationMessage>) {
        self.conversation = conversation;
    }

    pub fn with_route_request(mut self, route_request: RouteRequest) -> Self {
        self.route_request = route_request;
        self
    }

    pub fn set_route_request(&mut self, route_request: RouteRequest) {
        self.route_request = route_request;
    }

    pub fn route_request(&self) -> &RouteRequest {
        &self.route_request
    }

    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = system_prompt.into();
        self
    }

    pub fn set_system_prompt(&mut self, system_prompt: impl Into<String>) {
        self.system_prompt = system_prompt.into();
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn with_context_limits(mut self, max_messages: usize, max_chars: usize) -> Self {
        self.max_context_messages = max_messages;
        self.max_context_chars = max_chars;
        self
    }

    pub fn conversation(&self) -> &[ConversationMessage] {
        &self.conversation
    }

    pub fn clear_conversation(&mut self) {
        self.conversation.clear();
    }

    pub fn with_permission_policy(mut self, policy: PermissionPolicy) -> Self {
        self.permission_policy = policy;
        self
    }

    pub fn set_permission_policy(&mut self, policy: PermissionPolicy) {
        self.permission_policy = policy;
        self.session_approvals.clear();
    }

    pub fn permission_policy(&self) -> &PermissionPolicy {
        &self.permission_policy
    }

    pub fn clear_session_approvals(&mut self) {
        self.session_approvals.clear();
    }

    pub fn run(&mut self, prompt: impl Into<String>) -> Result<Vec<AgentEvent>, AgentError> {
        self.run_with_approval(prompt, |_| PermissionDecision::Deny {
            reason: "tool requires host approval".to_owned(),
        })
    }

    /// Run one turn with an explicit approval callback for guarded tools.
    pub fn run_with_approval<F>(
        &mut self,
        prompt: impl Into<String>,
        approve: F,
    ) -> Result<Vec<AgentEvent>, AgentError>
    where
        F: FnMut(&PermissionRequest) -> PermissionDecision,
    {
        let mut events = Vec::new();
        self.run_turn(prompt.into(), approve, |event| events.push(event), false)?;
        Ok(events)
    }

    /// Run one turn and emit model text as it arrives.
    pub fn run_streaming<F>(&mut self, prompt: impl Into<String>, emit: F) -> Result<(), AgentError>
    where
        F: FnMut(AgentEvent),
    {
        self.run_streaming_with_approval(
            prompt,
            |_| PermissionDecision::Deny {
                reason: "tool requires host approval".to_owned(),
            },
            emit,
        )
    }

    /// Run one streaming turn with an explicit approval callback.
    pub fn run_streaming_with_approval<F, A>(
        &mut self,
        prompt: impl Into<String>,
        approve: F,
        emit: A,
    ) -> Result<(), AgentError>
    where
        F: FnMut(&PermissionRequest) -> PermissionDecision,
        A: FnMut(AgentEvent),
    {
        self.run_turn(prompt.into(), approve, emit, true)
    }

    /// The shared turn loop. With `stream`, model text is emitted as
    /// deltas while it arrives; otherwise the model answers in one piece.
    fn run_turn<F, A>(
        &mut self,
        prompt: String,
        mut approve: F,
        mut emit: A,
        stream: bool,
    ) -> Result<(), AgentError>
    where
        F: FnMut(&PermissionRequest) -> PermissionDecision,
        A: FnMut(AgentEvent),
    {
        emit(AgentEvent::UserMessage {
            content: prompt.clone(),
        });
        let mut turn_context = vec![ConversationMessage::new("user", prompt.clone())];
        let outcome = self.drive_turn(&prompt, &mut turn_context, &mut approve, &mut emit, stream);
        // A turn cancelled before any model or tool activity left no trace.
        if matches!(outcome, Ok(TurnEnd::Cancelled)) && turn_context.len() == 1 {
            return Ok(());
        }
        // Commit every other turn, not only answered ones, so the next turn
        // still sees the prompt, the tool work already done, and why it
        // stopped.
        let stopped = match &outcome {
            Ok(TurnEnd::Answered) => None,
            Ok(TurnEnd::Cancelled) => Some("cancelled".to_owned()),
            Ok(TurnEnd::RepeatedToolCall) => Some("repeated identical tool call".to_owned()),
            Err(error) => Some(error.to_string()),
        };
        if let Some(reason) = stopped {
            turn_context.push(ConversationMessage::new(
                "assistant",
                format!("[turn ended without an answer: {reason}]"),
            ));
        }
        self.commit_turn(turn_context);
        outcome.map(|_| ())
    }

    /// Drive the model/tool loop for one turn, recording the conversation
    /// into `turn_context` as it goes.
    fn drive_turn<F, A>(
        &mut self,
        prompt: &str,
        turn_context: &mut Vec<ConversationMessage>,
        approve: &mut F,
        emit: &mut A,
        stream: bool,
    ) -> Result<TurnEnd, AgentError>
    where
        F: FnMut(&PermissionRequest) -> PermissionDecision,
        A: FnMut(AgentEvent),
    {
        let mut tool_results = Vec::new();
        let mut last_call: Option<(String, String)> = None;
        let mut executed: Vec<String> = Vec::new();
        let mut tool_calls: usize = 0;
        let deadline = Instant::now() + self.turn_timeout;

        for _ in 0..self.max_steps {
            if self.cancel.load(Ordering::Acquire) {
                emit(AgentEvent::Cancelled);
                emit(AgentEvent::Done);
                return Ok(TurnEnd::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(AgentError::TurnTimeoutExceeded);
            }
            let mut emitted_text = false;
            let mut step_usage: Option<super::Usage> = None;
            let mut first_token_ms: Option<u64> = None;
            let started = Instant::now();
            let request = ModelRequest {
                prompt: prompt.to_owned(),
                tool_results: tool_results.clone(),
                tools: self.tools.specs(),
                system_prompt: self.system_prompt.clone(),
                conversation: self.bounded_context(),
                current_turn: turn_context.clone(),
                route: self.route_request.clone(),
            };
            let response = if stream {
                self.model
                    .respond_stream(&request, &mut |event| match event {
                        ModelStreamEvent::TextDelta { content } => {
                            emitted_text = true;
                            first_token_ms
                                .get_or_insert_with(|| started.elapsed().as_millis() as u64);
                            emit(AgentEvent::AssistantDelta { content });
                        }
                        ModelStreamEvent::Usage(usage) => step_usage = Some(usage),
                    })
            } else {
                self.model.respond(&request)
            }
            .map_err(AgentError::Model)?;
            if let Some(mut usage) = step_usage {
                usage.latency_ms = started.elapsed().as_millis() as u64;
                usage.first_token_ms = first_token_ms;
                emit(AgentEvent::Usage(usage));
            }

            let calls = match response {
                ModelResponse::Text(content) => {
                    turn_context.push(ConversationMessage::new("assistant", content.clone()));
                    // Streamed text already went out as deltas; an empty
                    // streamed answer is not repeated as a final message.
                    if !emitted_text && (!stream || !content.is_empty()) {
                        emit(AgentEvent::AssistantText { content });
                    }
                    emit(AgentEvent::Done);
                    return Ok(TurnEnd::Answered);
                }
                ModelResponse::ToolCall(call) => vec![call],
                ModelResponse::ToolCalls(calls) => calls,
            };
            let mut state = BatchState {
                tool_results: &mut tool_results,
                turn_context: &mut *turn_context,
                last_call: &mut last_call,
                executed: &mut executed,
                tool_calls: &mut tool_calls,
            };
            if let Some(end) = self.run_tool_batch(calls, &mut state, approve, &mut *emit)? {
                return Ok(end);
            }
        }

        Err(AgentError::StepLimitExceeded {
            limit: self.max_steps,
        })
    }

    /// Run the tool calls of one model step in order. Every call that ran is
    /// recorded as one assistant message per call followed by the results,
    /// so the history never holds a call without its result, even when a
    /// guard stops the batch part way. Returns how the turn ended, if it did.
    fn run_tool_batch<F, A>(
        &mut self,
        calls: Vec<ToolCall>,
        state: &mut BatchState<'_>,
        approve: &mut F,
        emit: &mut A,
    ) -> Result<Option<TurnEnd>, AgentError>
    where
        F: FnMut(&PermissionRequest) -> PermissionDecision,
        A: FnMut(AgentEvent),
    {
        let mut ran: Vec<(ToolCall, ToolResult)> = Vec::new();
        let outcome = (|| {
            if calls.is_empty() {
                return Err(AgentError::Model(super::ModelError::new(
                    "model returned an empty tool call list",
                )));
            }
            for call in calls {
                if *state.tool_calls >= self.max_tool_calls {
                    return Err(AgentError::ToolCallLimitExceeded {
                        limit: self.max_tool_calls,
                    });
                }
                let canonical = canonical_arguments(&call.arguments);
                if let Some((last_name, last_args)) = state.last_call.as_ref()
                    && last_name == &call.name
                    && last_args == &canonical
                {
                    emit(AgentEvent::RepeatedToolCall {
                        tool: call.name.clone(),
                        count: 2,
                    });
                    emit(AgentEvent::Done);
                    return Ok(Some(TurnEnd::RepeatedToolCall));
                }
                emit(AgentEvent::ToolCall(call.clone()));
                let mut result = self.execute_with_approval(&call, approve, &mut *emit);
                if let std::borrow::Cow::Owned(redacted) = self.redactor.redact(&result.content) {
                    result.content = redacted;
                }
                state.tool_results.push(result.clone());
                emit(AgentEvent::ToolResult(result.clone()));
                ran.push((call.clone(), result));
                *state.tool_calls += 1;
                state.executed.push(format!("{} {canonical}", call.name));
                *state.last_call = Some((call.name.clone(), canonical));
                for period in 2..=3 {
                    let executed = &state.executed;
                    let len = executed.len();
                    if len >= period * 2
                        && executed[len - period..] == executed[len - period * 2..len - period]
                    {
                        return Err(AgentError::PatternLoopDetected { period });
                    }
                }
            }
            Ok(None)
        })();
        for (call, _) in &ran {
            state
                .turn_context
                .push(ConversationMessage::assistant_tool_call(call));
        }
        for (_, result) in &ran {
            state
                .turn_context
                .push(ConversationMessage::tool_result(result));
        }
        outcome
    }

    fn execute_with_approval<F, A>(
        &mut self,
        call: &ToolCall,
        approve: &mut F,
        mut emit: A,
    ) -> ToolResult
    where
        F: FnMut(&PermissionRequest) -> PermissionDecision,
        A: FnMut(AgentEvent),
    {
        self.tools.set_cancel_token(self.cancel.clone());
        let Some(request) = self.tools.permission_request(call) else {
            return self.tools.execute(call);
        };

        let (kind, target) = rule_target(call);
        let rule_effect = self
            .permission_policy
            .matching_effect(&call.name, kind, &target);
        let effect = rule_effect.unwrap_or(PermissionEffect::Ask);
        // Deny rules win over anything remembered earlier in the session.
        if effect == PermissionEffect::Deny {
            return ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                content: format!("permission rule denies {} '{target}'", call.name),
                is_error: true,
            };
        }
        // A preapproved call (such as a read-only command) skips the prompt
        // only when no rule spoke, so an explicit `ask` rule still asks.
        if effect == PermissionEffect::Allow
            || self.session_approvals.is_allowed(&call.name, &target)
            || (rule_effect.is_none() && self.tools.is_preapproved(call))
        {
            return self.tools.execute(call);
        }

        emit(AgentEvent::PermissionRequested(request.clone()));
        match approve(&request) {
            PermissionDecision::Allow => {
                self.session_approvals.remember(&call.name, &target);
                self.tools.execute(call)
            }
            PermissionDecision::Deny { reason } => ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                content: reason,
                is_error: true,
            },
        }
    }

    fn bounded_context(&self) -> Vec<ConversationMessage> {
        newest_within(
            &self.conversation,
            self.max_context_messages,
            self.max_context_chars,
        )
    }

    /// Keep the conversation within its budget. When it overflows, trim to
    /// half the budget rather than just under it: the retained history then
    /// starts with the same message for several turns, so providers can
    /// reuse their prompt cache instead of reprocessing a shifted prefix.
    fn commit_turn(&mut self, messages: Vec<ConversationMessage>) {
        self.conversation.extend(messages);
        let fits = self.conversation.len() <= self.max_context_messages
            && self.conversation.iter().map(message_chars).sum::<usize>() <= self.max_context_chars;
        if fits {
            return;
        }
        let trimmed = newest_within(
            &self.conversation,
            (self.max_context_messages / 2).max(1),
            self.max_context_chars / 2,
        );
        self.conversation = if trimmed.is_empty() {
            self.bounded_context()
        } else {
            trimmed
        };
    }
}

/// Size of a message as sent: its text plus any tool-call arguments, which
/// can carry whole files.
fn message_chars(message: &ConversationMessage) -> usize {
    message.role.len()
        + message.content.len()
        + message
            .tool_call
            .as_ref()
            .map(|call| canonical_arguments(&call.arguments).len() + call.name.len())
            .unwrap_or(0)
}

/// The newest messages that fit both limits, starting at a user message so
/// the history never opens with a tool result whose call was trimmed away.
pub(super) fn newest_within(
    conversation: &[ConversationMessage],
    max_messages: usize,
    max_chars: usize,
) -> Vec<ConversationMessage> {
    if max_messages == 0 || max_chars == 0 {
        return Vec::new();
    }
    let mut start = conversation.len();
    let mut chars = 0;
    for (index, message) in conversation.iter().enumerate().rev() {
        let size = message_chars(message);
        if conversation.len() - index > max_messages || chars + size > max_chars {
            break;
        }
        chars += size;
        start = index;
    }
    while start < conversation.len() {
        let message = &conversation[start];
        if message.role == "user" && message.tool_call_id.is_none() {
            break;
        }
        start += 1;
    }
    conversation[start..].to_vec()
}

fn rule_target(call: &ToolCall) -> (TargetKind, String) {
    for (name, kind) in [
        ("path", TargetKind::Path),
        ("command", TargetKind::Command),
        ("program", TargetKind::Other),
    ] {
        if let Some(ToolValue::Text(value)) = call.arguments.get(name) {
            return (kind, value.clone());
        }
    }
    (TargetKind::Other, String::new())
}

fn canonical_arguments(arguments: &ToolArguments) -> String {
    serde_json::to_string(arguments).unwrap_or_default()
}
