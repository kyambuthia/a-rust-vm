//! The provider-neutral model trait, its request and response types, and a scripted model.

use std::collections::VecDeque;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::{ConversationMessage, ToolCall, ToolResult, ToolSpec};

/// The input supplied to a model for each decision in an agent turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRequest {
    #[serde(default)]
    pub system_prompt: String,
    pub prompt: String,
    pub tool_results: Vec<ToolResult>,
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    pub conversation: Vec<ConversationMessage>,
    #[serde(default)]
    pub current_turn: Vec<ConversationMessage>,
    #[serde(default)]
    pub route: RouteRequest,
}

/// A model response can either complete the turn or request a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelResponse {
    Text(String),
    ToolCall(ToolCall),
}

/// The routing profile requested by an agent turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelMode {
    #[default]
    Default,
    Fast,
    Strong,
}

/// Capabilities advertised by a registered model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub streaming: bool,
    pub tools: bool,
}

impl ModelCapabilities {
    pub const STREAMING_TOOLS: Self = Self {
        streaming: true,
        tools: true,
    };
}

/// The host-side routing request for one model decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRequest {
    pub requested_model: Option<String>,
    pub mode: ModelMode,
    pub requires_tools: bool,
    pub requires_streaming: bool,
}

impl Default for RouteRequest {
    fn default() -> Self {
        Self {
            requested_model: None,
            mode: ModelMode::Default,
            requires_tools: false,
            requires_streaming: false,
        }
    }
}

/// The route selected by the router before a provider is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelection {
    pub model: String,
    pub capabilities: ModelCapabilities,
}

/// A partial model output received while a response is still being produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStreamEvent {
    TextDelta {
        content: String,
    },
    /// Token and cost figures reported at the end of a response.
    Usage(super::Usage),
}

impl Model for Box<dyn Model> {
    fn respond(&mut self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        self.as_mut().respond(request)
    }

    fn respond_stream(
        &mut self,
        request: &ModelRequest,
        emit: &mut dyn FnMut(ModelStreamEvent),
    ) -> Result<ModelResponse, ModelError> {
        self.as_mut().respond_stream(request, emit)
    }
}

/// Errors produced by the model boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelError {
    pub message: String,
}

impl ModelError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ModelError {}

/// Provider-neutral model interface.
pub trait Model {
    fn respond(&mut self, request: &ModelRequest) -> Result<ModelResponse, ModelError>;

    fn respond_stream(
        &mut self,
        request: &ModelRequest,
        emit: &mut dyn FnMut(ModelStreamEvent),
    ) -> Result<ModelResponse, ModelError> {
        let response = self.respond(request)?;
        if let ModelResponse::Text(content) = &response
            && !content.is_empty()
        {
            emit(ModelStreamEvent::TextDelta {
                content: content.clone(),
            });
        }
        Ok(response)
    }
}

/// A deterministic model for tests, demos, and offline development.
#[derive(Debug, Default)]
pub struct ScriptedModel {
    responses: VecDeque<ModelResponse>,
}

impl ScriptedModel {
    pub fn new(responses: impl IntoIterator<Item = ModelResponse>) -> Self {
        Self {
            responses: responses.into_iter().collect(),
        }
    }
}

impl Model for ScriptedModel {
    fn respond(&mut self, _request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        self.responses
            .pop_front()
            .ok_or_else(|| ModelError::new("scripted model has no response left"))
    }
}
