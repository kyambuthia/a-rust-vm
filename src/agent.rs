//! Contracts and a deterministic execution loop for the terminal agent.
//!
//! The model boundary is intentionally provider-neutral. A live model can be
//! added later without changing VM ownership or tool policy.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::{Instruction, StepResult, Vm, VmError};

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
    Error { message: String },
    Done,
}

/// A model-facing description of a registered tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: String,
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

/// The input supplied to a model for each decision in an agent turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelRequest {
    pub prompt: String,
    pub tool_results: Vec<ToolResult>,
    pub tools: Vec<ToolSpec>,
}

/// A model response can either complete the turn or request a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelResponse {
    Text(String),
    ToolCall(ToolCall),
}

/// A partial model output received while a response is still being produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStreamEvent {
    TextDelta { content: String },
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

/// A model adapter for a process that speaks A/RVM's newline-delimited JSON
/// protocol over stdin and stdout.
///
/// The executable is launched directly, without a shell. The child receives a
/// serialized [`ModelRequest`] and can emit text, tool calls, errors, and a
/// final `done` event one line at a time.
pub struct ProcessModel {
    program: PathBuf,
    arguments: Vec<String>,
    working_directory: Option<PathBuf>,
}

impl ProcessModel {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            working_directory: None,
        }
    }

    pub fn with_arguments<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.arguments = arguments.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_working_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.working_directory = Some(directory.into());
        self
    }

    fn run_stream(
        &self,
        request: &ModelRequest,
        emit: &mut dyn FnMut(ModelStreamEvent),
    ) -> Result<ModelResponse, ModelError> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(directory) = &self.working_directory {
            command.current_dir(directory);
        }

        let mut child = command.spawn().map_err(|error| {
            ModelError::new(format!(
                "failed to start model process '{}': {error}",
                self.program.display()
            ))
        })?;

        let request_json = serde_json::to_vec(request)
            .map_err(|error| ModelError::new(format!("failed to encode model request: {error}")))?;
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| ModelError::new("model process stdin is unavailable"))?;
        stdin
            .write_all(&request_json)
            .and_then(|_| stdin.write_all(b"\n"))
            .map_err(|error| ModelError::new(format!("failed to send model request: {error}")))?;
        child.stdin.take();

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ModelError::new("model process stdout is unavailable"))?;
        let mut response = None;
        let mut text = String::new();

        for line in BufReader::new(stdout).lines() {
            let line = line.map_err(|error| {
                ModelError::new(format!("failed to read model output: {error}"))
            })?;
            if line.trim().is_empty() {
                continue;
            }

            let event = serde_json::from_str::<ProcessModelEvent>(&line).map_err(|error| {
                ModelError::new(format!("invalid model event: {error}; line={line}"))
            })?;
            match event {
                ProcessModelEvent::Text { text: chunk, part } => {
                    let chunk = chunk.or_else(|| part.and_then(|part| part.text));
                    if let Some(chunk) = chunk
                        && !chunk.is_empty()
                    {
                        text.push_str(&chunk);
                        emit(ModelStreamEvent::TextDelta { content: chunk });
                    }
                }
                ProcessModelEvent::TextDelta { text: chunk } => {
                    if !chunk.is_empty() {
                        text.push_str(&chunk);
                        emit(ModelStreamEvent::TextDelta { content: chunk });
                    }
                }
                ProcessModelEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    if response.is_some() {
                        return Err(ModelError::new("model emitted more than one response"));
                    }
                    response = Some(ModelResponse::ToolCall(ToolCall::new(id, name, arguments)));
                }
                ProcessModelEvent::Error { message } => {
                    return Err(ModelError::new(message));
                }
                ProcessModelEvent::Done => break,
                ProcessModelEvent::Ignored => {}
            }
        }

        let status = child
            .wait()
            .map_err(|error| ModelError::new(format!("failed to finish model process: {error}")))?;
        if !status.success() {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                pipe.read_to_string(&mut stderr).map_err(|error| {
                    ModelError::new(format!(
                        "model process failed and stderr was unreadable: {error}"
                    ))
                })?;
            }
            let detail = stderr.trim();
            return Err(ModelError::new(if detail.is_empty() {
                format!("model process exited with {status}")
            } else {
                format!("model process exited with {status}: {detail}")
            }));
        }

        response
            .or_else(|| (!text.is_empty()).then_some(ModelResponse::Text(text)))
            .ok_or_else(|| ModelError::new("model process emitted no response"))
    }
}

impl Model for ProcessModel {
    fn respond(&mut self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let mut ignored = |_event: ModelStreamEvent| {};
        self.run_stream(request, &mut ignored)
    }

    fn respond_stream(
        &mut self,
        request: &ModelRequest,
        emit: &mut dyn FnMut(ModelStreamEvent),
    ) -> Result<ModelResponse, ModelError> {
        self.run_stream(request, emit)
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ProcessModelEvent {
    Text {
        text: Option<String>,
        part: Option<ProcessTextPart>,
    },
    TextDelta {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: ToolArguments,
    },
    Error {
        message: String,
    },
    Done,
    #[serde(other)]
    Ignored,
}

#[derive(Debug, Deserialize)]
struct ProcessTextPart {
    text: Option<String>,
}

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
    }
}

/// Errors raised while coordinating one agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    Model(ModelError),
    StepLimitExceeded { limit: usize },
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(error) => write!(formatter, "model error: {error}"),
            Self::StepLimitExceeded { limit } => {
                write!(formatter, "agent step limit exceeded: {limit}")
            }
        }
    }
}

impl std::error::Error for AgentError {}

/// A bounded model-tool execution loop.
pub struct Agent<M> {
    model: M,
    tools: ToolRegistry,
    max_steps: usize,
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
        }
    }

    pub fn with_max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
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
        mut approve: F,
    ) -> Result<Vec<AgentEvent>, AgentError>
    where
        F: FnMut(&PermissionRequest) -> PermissionDecision,
    {
        let prompt = prompt.into();
        let mut events = vec![AgentEvent::UserMessage {
            content: prompt.clone(),
        }];
        let mut tool_results = Vec::new();

        for _ in 0..self.max_steps {
            let response = self
                .model
                .respond(&ModelRequest {
                    prompt: prompt.clone(),
                    tool_results: tool_results.clone(),
                    tools: self.tools.specs(),
                })
                .map_err(AgentError::Model)?;

            match response {
                ModelResponse::Text(content) => {
                    events.push(AgentEvent::AssistantText { content });
                    events.push(AgentEvent::Done);
                    return Ok(events);
                }
                ModelResponse::ToolCall(call) => {
                    events.push(AgentEvent::ToolCall(call.clone()));
                    let result =
                        self.execute_with_approval(&call, &mut approve, |event| events.push(event));
                    tool_results.push(result.clone());
                    events.push(AgentEvent::ToolResult(result));
                }
            }
        }

        Err(AgentError::StepLimitExceeded {
            limit: self.max_steps,
        })
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
        mut approve: F,
        mut emit: A,
    ) -> Result<(), AgentError>
    where
        F: FnMut(&PermissionRequest) -> PermissionDecision,
        A: FnMut(AgentEvent),
    {
        let prompt = prompt.into();
        emit(AgentEvent::UserMessage {
            content: prompt.clone(),
        });
        let mut tool_results = Vec::new();

        for _ in 0..self.max_steps {
            let mut emitted_text = false;
            let response = self
                .model
                .respond_stream(
                    &ModelRequest {
                        prompt: prompt.clone(),
                        tool_results: tool_results.clone(),
                        tools: self.tools.specs(),
                    },
                    &mut |event| match event {
                        ModelStreamEvent::TextDelta { content } => {
                            emitted_text = true;
                            emit(AgentEvent::AssistantDelta { content });
                        }
                    },
                )
                .map_err(AgentError::Model)?;

            match response {
                ModelResponse::Text(content) => {
                    if !emitted_text && !content.is_empty() {
                        emit(AgentEvent::AssistantText { content });
                    }
                    emit(AgentEvent::Done);
                    return Ok(());
                }
                ModelResponse::ToolCall(call) => {
                    emit(AgentEvent::ToolCall(call.clone()));
                    let result = self.execute_with_approval(&call, &mut approve, &mut emit);
                    tool_results.push(result.clone());
                    emit(AgentEvent::ToolResult(result));
                }
            }
        }

        Err(AgentError::StepLimitExceeded {
            limit: self.max_steps,
        })
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
        let Some(request) = self.tools.permission_request(call) else {
            return self.tools.execute(call);
        };

        emit(AgentEvent::PermissionRequested(request.clone()));
        match approve(&request) {
            PermissionDecision::Allow => self.tools.execute(call),
            PermissionDecision::Deny { reason } => ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                content: reason,
                is_error: true,
            },
        }
    }
}

#[derive(Debug, Default)]
struct VmToolState {
    program: Vec<Instruction>,
    vm: Vm,
}

/// Create the first set of tools that expose the VM to an agent.
pub fn vm_tool_registry() -> Result<ToolRegistry, ToolError> {
    let state = std::rc::Rc::new(std::cell::RefCell::new(VmToolState::default()));
    let mut registry = ToolRegistry::default();

    registry.register(RunProgramTool::new(state.clone()))?;
    registry.register(StepVmTool::new(state.clone()))?;
    registry.register(InspectVmTool::new(state.clone()))?;
    registry.register(DisassembleProgramTool::new(state.clone()))?;
    registry.register(ResetVmTool::new(state))?;

    Ok(registry)
}

type SharedVmToolState = std::rc::Rc<std::cell::RefCell<VmToolState>>;

struct RunProgramTool {
    state: SharedVmToolState,
}

impl RunProgramTool {
    fn new(state: SharedVmToolState) -> Self {
        Self { state }
    }
}

impl Tool for RunProgramTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "run_program",
            "Load and execute a newline-delimited A/RVM program.",
            r#"{"type":"object","properties":{"program":{"type":"string"}},"required":["program"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["program"])?;
        let source = required_text(arguments, "program")?;
        let program = parse_program(source)?;
        let mut state = self.state.borrow_mut();
        state.program = program;
        let program = state.program.clone();
        let result = state.vm.run(&program).map_err(vm_tool_error)?;
        Ok(format!(
            "result={result} ip={} stack={:?}",
            state.vm.instruction_pointer(),
            state.vm.stack()
        ))
    }
}

struct StepVmTool {
    state: SharedVmToolState,
}

impl StepVmTool {
    fn new(state: SharedVmToolState) -> Self {
        Self { state }
    }
}

impl Tool for StepVmTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "step_vm",
            "Execute one instruction from the loaded program.",
            r#"{"type":"object","properties":{}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &[])?;
        let mut state = self.state.borrow_mut();
        let program = state.program.clone();
        let result = state.vm.step(&program).map_err(vm_tool_error)?;
        match result {
            StepResult::Executed { instruction } => Ok(format!(
                "executed={} ip={} stack={:?}",
                format_instruction(instruction),
                state.vm.instruction_pointer(),
                state.vm.stack()
            )),
            StepResult::Halted { result } => Ok(format!(
                "halted result={result} ip={} stack={:?}",
                state.vm.instruction_pointer(),
                state.vm.stack()
            )),
        }
    }
}

struct InspectVmTool {
    state: SharedVmToolState,
}

impl InspectVmTool {
    fn new(state: SharedVmToolState) -> Self {
        Self { state }
    }
}

impl Tool for InspectVmTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "inspect_vm",
            "Inspect the loaded program's instruction pointer and value stack.",
            r#"{"type":"object","properties":{}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &[])?;
        let state = self.state.borrow();
        Ok(format!(
            "loaded={} ip={} halted={} stack={:?}",
            !state.program.is_empty(),
            state.vm.instruction_pointer(),
            state.vm.is_halted(),
            state.vm.stack()
        ))
    }
}

struct DisassembleProgramTool {
    state: SharedVmToolState,
}

impl DisassembleProgramTool {
    fn new(state: SharedVmToolState) -> Self {
        Self { state }
    }
}

impl Tool for DisassembleProgramTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "disassemble_program",
            "Show the loaded program with instruction offsets.",
            r#"{"type":"object","properties":{}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &[])?;
        let state = self.state.borrow();
        if state.program.is_empty() {
            return Err(ToolError::new("no program is loaded"));
        }

        Ok(state
            .program
            .iter()
            .enumerate()
            .map(|(index, instruction)| format!("{index:02} {}", format_instruction(*instruction)))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

struct ResetVmTool {
    state: SharedVmToolState,
}

impl ResetVmTool {
    fn new(state: SharedVmToolState) -> Self {
        Self { state }
    }
}

impl Tool for ResetVmTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "reset_vm",
            "Reset the loaded VM to its initial state.",
            r#"{"type":"object","properties":{}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &[])?;
        let mut state = self.state.borrow_mut();
        state.vm.reset();
        Ok("vm reset".to_owned())
    }
}

fn required_text<'a>(arguments: &'a ToolArguments, name: &str) -> Result<&'a str, ToolError> {
    match arguments.get(name) {
        Some(ToolValue::Text(value)) if !value.trim().is_empty() => Ok(value),
        Some(_) => Err(ToolError::new(format!(
            "argument '{name}' must be non-empty text"
        ))),
        None => Err(ToolError::new(format!("missing required argument: {name}"))),
    }
}

fn ensure_arguments(arguments: &ToolArguments, allowed: &[&str]) -> Result<(), ToolError> {
    if let Some(name) = arguments
        .keys()
        .find(|name| !allowed.iter().any(|allowed_name| allowed_name == name))
    {
        return Err(ToolError::new(format!("unexpected argument: {name}")));
    }

    Ok(())
}

fn parse_program(source: &str) -> Result<Vec<Instruction>, ToolError> {
    let mut program = Vec::new();

    for (line_number, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut parts = line.split_whitespace();
        let opcode = parts
            .next()
            .ok_or_else(|| ToolError::new(format!("line {} is empty", line_number + 1)))?;
        let instruction = match opcode.to_ascii_uppercase().as_str() {
            "PUSH" => {
                let value = parts
                    .next()
                    .ok_or_else(|| {
                        ToolError::new(format!("line {}: PUSH needs a value", line_number + 1))
                    })?
                    .parse::<i32>()
                    .map_err(|_| {
                        ToolError::new(format!("line {}: invalid PUSH value", line_number + 1))
                    })?;
                Instruction::Push(value)
            }
            "ADD" => Instruction::Add,
            "SUB" => Instruction::Sub,
            "MUL" => Instruction::Mul,
            "DIV" => Instruction::Div,
            "HALT" => Instruction::Halt,
            _ => {
                return Err(ToolError::new(format!(
                    "line {}: unknown instruction '{opcode}'",
                    line_number + 1
                )));
            }
        };

        if parts.next().is_some() {
            return Err(ToolError::new(format!(
                "line {}: unexpected arguments after {opcode}",
                line_number + 1
            )));
        }
        program.push(instruction);
    }

    if program.is_empty() {
        return Err(ToolError::new("program cannot be empty"));
    }

    Ok(program)
}

fn format_instruction(instruction: Instruction) -> String {
    match instruction {
        Instruction::Push(value) => format!("PUSH {value}"),
        Instruction::Add => "ADD".to_owned(),
        Instruction::Sub => "SUB".to_owned(),
        Instruction::Mul => "MUL".to_owned(),
        Instruction::Div => "DIV".to_owned(),
        Instruction::Halt => "HALT".to_owned(),
    }
}

fn vm_tool_error(error: VmError) -> ToolError {
    ToolError::new(format!("vm error: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::{
        Agent, AgentEvent, Model, ModelResponse, PermissionDecision, ScriptedModel, ToolArguments,
        ToolCall, ToolRegistry, ToolValue, vm_tool_registry,
    };

    fn program_arguments(source: &str) -> ToolArguments {
        [("program".to_owned(), ToolValue::Text(source.to_owned()))]
            .into_iter()
            .collect()
    }

    #[test]
    fn registry_exposes_vm_tools_in_stable_order() {
        let registry = vm_tool_registry().unwrap();
        let names = registry
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "disassemble_program",
                "inspect_vm",
                "reset_vm",
                "run_program",
                "step_vm"
            ]
        );
    }

    #[test]
    fn vm_tools_share_state() {
        let mut registry = vm_tool_registry().unwrap();
        let run = ToolCall::new(
            "run-1",
            "run_program",
            program_arguments("PUSH 2\nPUSH 3\nADD\nHALT"),
        );
        let result = registry.execute(&run);
        assert_eq!(result.content, "result=5 ip=4 stack=[5]");
        assert!(!result.is_error);

        let inspect = ToolCall::new("inspect-1", "inspect_vm", ToolArguments::new());
        let result = registry.execute(&inspect);
        assert_eq!(result.content, "loaded=true ip=4 halted=true stack=[5]");
    }

    #[test]
    fn malformed_program_is_reported_as_a_tool_error() {
        let mut registry = vm_tool_registry().unwrap();
        let call = ToolCall::new(
            "run-1",
            "run_program",
            program_arguments("PUSH 2\nWHAT\nHALT"),
        );

        let result = registry.execute(&call);
        assert!(result.is_error);
        assert!(result.content.contains("unknown instruction 'WHAT'"));
    }

    #[test]
    fn unexpected_tool_arguments_are_rejected() {
        let mut registry = vm_tool_registry().unwrap();
        let call = ToolCall::new(
            "inspect-1",
            "inspect_vm",
            [("unexpected".to_owned(), ToolValue::Boolean(true))]
                .into_iter()
                .collect(),
        );

        let result = registry.execute(&call);
        assert!(result.is_error);
        assert_eq!(result.content, "unexpected argument: unexpected");
    }

    #[test]
    fn scripted_agent_emits_tool_and_completion_events() {
        let model = ScriptedModel::new([
            ModelResponse::ToolCall(ToolCall::new(
                "run-1",
                "run_program",
                program_arguments("PUSH 8\nPUSH 5\nMUL\nHALT"),
            )),
            ModelResponse::Text("The VM result is 40.".to_owned()),
        ]);
        let mut agent = Agent::new(model, vm_tool_registry().unwrap());

        let events = agent.run("multiply 8 by 5").unwrap();

        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::ToolCall(_)));
        assert!(matches!(events[2], AgentEvent::ToolResult(ref result) if !result.is_error));
        assert_eq!(
            events[3],
            AgentEvent::AssistantText {
                content: "The VM result is 40.".to_owned()
            }
        );
        assert_eq!(events[4], AgentEvent::Done);
    }

    #[test]
    fn streaming_agent_emits_text_deltas_and_completion() {
        let model = ScriptedModel::new([ModelResponse::Text("ready".to_owned())]);
        let mut agent = Agent::new(model, vm_tool_registry().unwrap());
        let mut events = Vec::new();

        agent
            .run_streaming("say ready", |event| events.push(event))
            .unwrap();

        assert_eq!(
            events,
            vec![
                AgentEvent::UserMessage {
                    content: "say ready".to_owned()
                },
                AgentEvent::AssistantDelta {
                    content: "ready".to_owned()
                },
                AgentEvent::Done,
            ]
        );
    }

    #[test]
    fn model_boundary_serializes_tool_arguments_as_json_objects() {
        let call = ToolCall::new("run-1", "run_program", program_arguments("PUSH 2\nHALT"));

        let json = serde_json::to_string(&call).unwrap();

        assert_eq!(
            json,
            r#"{"id":"run-1","name":"run_program","arguments":{"program":"PUSH 2\nHALT"}}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_model_streams_json_events_without_a_shell_wrapper() {
        let mut model = super::ProcessModel::new("/bin/sh").with_arguments([
            "-c",
            "read request; printf '%s\\n' '{\"type\":\"text_delta\",\"text\":\"hello \"}' '{\"type\":\"text_delta\",\"text\":\"world\"}' '{\"type\":\"done\"}'",
        ]);
        let request = super::ModelRequest {
            prompt: "say hello".to_owned(),
            tool_results: Vec::new(),
            tools: Vec::new(),
        };
        let mut chunks = Vec::new();

        let response = model
            .respond_stream(&request, &mut |event| match event {
                super::ModelStreamEvent::TextDelta { content } => chunks.push(content),
            })
            .unwrap();

        assert_eq!(response, ModelResponse::Text("hello world".to_owned()));
        assert_eq!(chunks, vec!["hello ".to_owned(), "world".to_owned()]);
    }

    #[test]
    fn agent_stops_after_the_configured_step_limit() {
        let model = ScriptedModel::new([ModelResponse::ToolCall(ToolCall::new(
            "inspect-1",
            "inspect_vm",
            ToolArguments::new(),
        ))]);
        let mut agent = Agent::new(model, vm_tool_registry().unwrap()).with_max_steps(1);

        let error = agent.run("inspect").unwrap_err();

        assert_eq!(error, super::AgentError::StepLimitExceeded { limit: 1 });
    }

    #[test]
    fn unknown_tool_calls_return_explicit_errors() {
        let mut registry = ToolRegistry::default();
        let result = registry.execute(&ToolCall::new(
            "missing-1",
            "missing_tool",
            ToolArguments::new(),
        ));

        assert!(result.is_error);
        assert_eq!(result.content, "unknown tool: missing_tool");
    }

    #[test]
    fn denied_workspace_write_emits_permission_and_does_not_write() {
        let root =
            std::env::temp_dir().join(format!("a-rust-vm-agent-permission-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let model = ScriptedModel::new([
            ModelResponse::ToolCall(ToolCall::new(
                "write-1",
                "write_file",
                [
                    ("path".to_owned(), ToolValue::Text("blocked.txt".to_owned())),
                    ("content".to_owned(), ToolValue::Text("nope".to_owned())),
                ]
                .into_iter()
                .collect(),
            )),
            ModelResponse::Text("The write was not approved.".to_owned()),
        ]);
        let mut agent = Agent::new(
            model,
            crate::workspace::workspace_tool_registry(&root).unwrap(),
        );

        let events = agent
            .run_with_approval("write a file", |_| PermissionDecision::Deny {
                reason: "test denial".to_owned(),
            })
            .unwrap();

        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::PermissionRequested(_)))
        );
        assert!(events.iter().any(|event| {
            matches!(event, AgentEvent::ToolResult(result) if result.is_error && result.content == "test denial")
        }));
        assert!(!root.join("blocked.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
