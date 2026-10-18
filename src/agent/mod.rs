//! Contracts and a deterministic execution loop for the terminal agent.
//!
//! The model boundary is intentionally provider-neutral. A live model can be
//! added later without changing VM ownership or tool policy.

mod model;
mod process_model;
mod prompt;
mod router;
mod subagent;
mod tools;
mod turn;
mod types;
mod vm_tools;

#[cfg(test)]
mod tests;

pub use model::{
    Model, ModelCapabilities, ModelError, ModelMode, ModelRequest, ModelResponse, ModelSelection,
    ModelStreamEvent, RouteRequest, ScriptedModel,
};
pub use process_model::ProcessModel;
pub use prompt::{
    DEFAULT_SYSTEM_PROMPT, load_project_instructions, system_prompt_with_instructions,
    system_prompt_with_skills,
};
pub use router::{ModelRouter, RouteError};
#[cfg(test)]
pub(crate) use subagent::subagent_depth;
pub use subagent::{
    SubagentConfig, SubagentError, SubagentResult, SubagentSession, SubagentTool, run_subagent,
    run_subagent_with_cancel,
};
pub use tools::{Tool, ToolError, ToolRegistry};
use tools::{ensure_arguments, required_text};
pub use turn::{Agent, AgentError};
use turn::{DEFAULT_CONTEXT_CHARS, DEFAULT_CONTEXT_MESSAGES};
pub use types::{
    AgentEvent, ConversationMessage, PermissionDecision, PermissionRequest, ToolArguments,
    ToolCall, ToolResult, ToolSpec, ToolValue,
};
pub(crate) use vm_tools::parse_program;
pub use vm_tools::vm_tool_registry;
