//! Agent tests, grouped by the module they exercise. Shared fakes live here.

mod process_model;
mod prompt;
mod router;
mod subagent;
mod turn;
mod vm_tools;

use super::*;

use std::cell::RefCell;
use std::rc::Rc;

fn model_request(route: RouteRequest) -> super::ModelRequest {
    super::ModelRequest {
        system_prompt: String::new(),
        prompt: "test".to_owned(),
        tool_results: Vec::new(),
        tools: Vec::new(),
        conversation: Vec::new(),
        current_turn: Vec::new(),
        route,
    }
}

struct FailingModel;

impl Model for FailingModel {
    fn respond(&mut self, _request: &super::ModelRequest) -> Result<ModelResponse, ModelError> {
        Err(ModelError::new("provider unavailable"))
    }
}

struct PartialFailureModel;

impl Model for PartialFailureModel {
    fn respond(&mut self, _request: &super::ModelRequest) -> Result<ModelResponse, ModelError> {
        Err(ModelError::new("provider unavailable"))
    }

    fn respond_stream(
        &mut self,
        _request: &super::ModelRequest,
        emit: &mut dyn FnMut(ModelStreamEvent),
    ) -> Result<ModelResponse, ModelError> {
        emit(ModelStreamEvent::TextDelta {
            content: "partial".to_owned(),
        });
        Err(ModelError::new("stream interrupted"))
    }
}

struct RecordingModel {
    requests: Rc<RefCell<Vec<ModelRequest>>>,
    responses: std::collections::VecDeque<ModelResponse>,
}

impl Model for RecordingModel {
    fn respond(&mut self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        self.requests.borrow_mut().push(request.clone());
        self.responses
            .pop_front()
            .ok_or_else(|| ModelError::new("recording model has no response left"))
    }
}

fn program_arguments(source: &str) -> ToolArguments {
    [("program".to_owned(), ToolValue::Text(source.to_owned()))]
        .into_iter()
        .collect()
}

struct EchoAfterOneToolCall {
    call: ToolCall,
    used: bool,
}

impl EchoAfterOneToolCall {
    fn new(call: ToolCall) -> Self {
        Self { call, used: false }
    }
}

impl Default for EchoAfterOneToolCall {
    fn default() -> Self {
        Self::new(nested_call())
    }
}

impl Model for EchoAfterOneToolCall {
    fn respond(&mut self, request: &super::ModelRequest) -> Result<ModelResponse, ModelError> {
        if self.used {
            let echoed = request
                .tool_results
                .last()
                .map(|result| result.content.clone())
                .filter(|content| !content.is_empty())
                .unwrap_or_else(|| "child finished".to_owned());
            return Ok(ModelResponse::Text(echoed));
        }
        self.used = true;
        Ok(ModelResponse::ToolCall(self.call.clone()))
    }
}

fn parent_call() -> ToolCall {
    ToolCall::new(
        "delegate-1",
        "delegate_subagent",
        [("task".to_owned(), ToolValue::Text("outer".to_owned()))]
            .into_iter()
            .collect(),
    )
}

fn nested_call() -> ToolCall {
    ToolCall::new(
        "nested-2",
        "delegate_subagent",
        [("task".to_owned(), ToolValue::Text("nested".to_owned()))]
            .into_iter()
            .collect(),
    )
}

fn nested_child_tools() -> ToolRegistry {
    let mut tools = vm_tool_registry().unwrap();
    tools
        .register(SubagentTool::new(
            ScriptedModel::default,
            || vm_tool_registry().unwrap(),
            SubagentConfig::default(),
        ))
        .unwrap();
    tools
}
