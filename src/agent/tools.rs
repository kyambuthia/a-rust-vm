//! The tool trait and the registry that validates and dispatches tool calls.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::{PermissionRequest, ToolArguments, ToolCall, ToolResult, ToolSpec, ToolValue};

/// Errors from tool registration, validation, or execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    pub message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

/// The executable contract implemented by every agent tool.
pub trait Tool {
    fn spec(&self) -> ToolSpec;
    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError>;

    fn permission(&self, _arguments: &ToolArguments) -> Option<PermissionRequest> {
        None
    }

    fn set_cancel_token(&mut self, _cancel: Arc<AtomicBool>) {}
}

/// Mutable collection of named tools with deterministic dispatch.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn register<T>(&mut self, tool: T) -> Result<(), ToolError>
    where
        T: Tool + 'static,
    {
        let spec = tool.spec();
        if self.tools.contains_key(&spec.name) {
            return Err(ToolError::new(format!(
                "tool is already registered: {}",
                spec.name
            )));
        }

        self.tools.insert(spec.name, Box::new(tool));
        Ok(())
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    pub fn merge(&mut self, other: ToolRegistry) -> Result<(), ToolError> {
        if let Some(name) = other
            .tools
            .keys()
            .find(|name| self.tools.contains_key(*name))
        {
            return Err(ToolError::new(format!(
                "tool is already registered: {name}"
            )));
        }
        self.tools.extend(other.tools);
        Ok(())
    }

    pub fn set_cancel_token(&mut self, cancel: Arc<AtomicBool>) {
        for tool in self.tools.values_mut() {
            tool.set_cancel_token(cancel.clone());
        }
    }

    pub fn execute(&mut self, call: &ToolCall) -> ToolResult {
        let Some(tool) = self.tools.get_mut(&call.name) else {
            return ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                content: format!("unknown tool: {}", call.name),
                is_error: true,
            };
        };

        match tool.execute(&call.arguments) {
            Ok(content) => ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                content,
                is_error: false,
            },
            Err(error) => ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                content: error.to_string(),
                is_error: true,
            },
        }
    }

    pub fn permission_request(&self, call: &ToolCall) -> Option<PermissionRequest> {
        self.tools
            .get(&call.name)
            .and_then(|tool| tool.permission(&call.arguments))
            .map(|mut request| {
                request.id = format!("{}:{}", call.id, request.id);
                request
            })
    }
}

/// Reads a required, non-blank text argument.
pub(crate) fn required_text<'a>(
    arguments: &'a ToolArguments,
    name: &str,
) -> Result<&'a str, ToolError> {
    match arguments.get(name) {
        Some(ToolValue::Text(value)) if !value.trim().is_empty() => Ok(value),
        Some(_) => Err(ToolError::new(format!(
            "argument '{name}' must be non-empty text"
        ))),
        None => Err(ToolError::new(format!("missing required argument: {name}"))),
    }
}

/// Reads a required text argument that may be empty.
pub(crate) fn required_text_allow_empty<'a>(
    arguments: &'a ToolArguments,
    name: &str,
) -> Result<&'a str, ToolError> {
    match arguments.get(name) {
        Some(ToolValue::Text(value)) => Ok(value),
        Some(_) => Err(ToolError::new(format!("argument '{name}' must be text"))),
        None => Err(ToolError::new(format!("missing required argument: {name}"))),
    }
}

/// Reads an optional text argument, treating blank text as absent.
pub(crate) fn optional_text<'a>(arguments: &'a ToolArguments, name: &str) -> Option<&'a str> {
    match arguments.get(name) {
        Some(ToolValue::Text(value)) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

/// Reads an optional boolean argument; absent means `false`. Any other type
/// is an error, so `"true"` as text is not silently ignored.
pub(crate) fn optional_flag(arguments: &ToolArguments, name: &str) -> Result<bool, ToolError> {
    match arguments.get(name) {
        None => Ok(false),
        Some(ToolValue::Boolean(value)) => Ok(*value),
        Some(_) => Err(ToolError::new(format!(
            "argument '{name}' must be true or false"
        ))),
    }
}

/// Rejects arguments outside `allowed` so typos fail instead of being ignored.
pub(crate) fn ensure_arguments(
    arguments: &ToolArguments,
    allowed: &[&str],
) -> Result<(), ToolError> {
    if let Some(name) = arguments
        .keys()
        .find(|name| !allowed.iter().any(|allowed_name| allowed_name == name))
    {
        return Err(ToolError::new(format!("unexpected argument: {name}")));
    }

    Ok(())
}
