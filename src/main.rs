use std::env;
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, Stdio};

use serde::Deserialize;

use a_rust_vm::agent::{
    Agent, AgentEvent, ModelCapabilities, ModelMode, ModelResponse, ModelRouter, ProcessModel,
    RouteRequest, ScriptedModel, ToolArguments, ToolCall, ToolValue, load_project_instructions,
    system_prompt_with_instructions,
};
use a_rust_vm::workspace::coding_tool_registry;
use a_rust_vm::{Instruction, Vm};

fn main() {
    if env::args().nth(1).as_deref() == Some("agent-demo") {
        run_agent_demo();
        return;
    }
    if env::args().nth(1).as_deref() == Some("runtime-demo") {
        run_runtime_demo();
        return;
    }
    if env::args().nth(1).as_deref() == Some("guest-demo") {
        run_guest_demo();
        return;
    }
    if env::args().nth(1).as_deref() == Some("coding-demo") {
        run_coding_demo();
        return;
    }
    if env::args().nth(1).as_deref() == Some("agent") {
        run_live_agent();
        return;
    }
    if env::args().nth(1).as_deref() == Some("model-bridge") {
        run_model_bridge();
        return;
    }
    if env::args().nth(1).as_deref() == Some("serve") {
        a_rust_vm::server::run(working_directory());
        return;
    }

    let program = [
        Instruction::Push(2),
        Instruction::Push(3),
        Instruction::Push(4),
        Instruction::Mul,
        Instruction::Add,
        Instruction::Halt,
    ];

    println!("Executing bytecode: {program:?}");

    let mut vm = Vm::new();
    match vm.run(&program) {
        Ok(result) => println!("Result: {result}"),
        Err(error) => {
            eprintln!("VM error: {error:?}");
            std::process::exit(1);
        }
    }
}

fn run_agent_demo() {
    let arguments = [(
        "program".to_owned(),
        ToolValue::Text("PUSH 8\nPUSH 5\nMUL\nHALT".to_owned()),
    )]
    .into_iter()
    .collect::<ToolArguments>();
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new("call-1", "run_program", arguments)),
        ModelResponse::Text("The VM result is 40.".to_owned()),
    ]);
    let mut agent = Agent::new(
        model,
        coding_tool_registry(working_directory()).expect("agent tools should register"),
    );

    println!("Agent prompt: multiply 8 by 5");
    match agent.run("multiply 8 by 5") {
        Ok(events) => {
            for event in events {
                match event {
                    AgentEvent::UserMessage { content } => println!("user: {content}"),
                    AgentEvent::AssistantText { content } => println!("assistant: {content}"),
                    AgentEvent::AssistantDelta { content } => {
                        println!("assistant delta: {content}")
                    }
                    AgentEvent::ToolCall(call) => {
                        println!("tool call: {} ({})", call.name, call.id)
                    }
                    AgentEvent::ToolResult(result) => {
                        println!("tool result: {}", result.content)
                    }
                    AgentEvent::PermissionRequested(request) => {
                        println!("permission: {}", request.description)
                    }
                    AgentEvent::Error { message } => println!("error: {message}"),
                    AgentEvent::Done => println!("done"),
                }
            }
        }
        Err(error) => {
            eprintln!("agent error: {error}");
            std::process::exit(1);
        }
    }
}

fn run_runtime_demo() {
    let program = "PUSH 2\nPUSH 3\nADD\nHALT";
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new(
            "compile-1",
            "compile_program",
            [("program".to_owned(), ToolValue::Text(program.to_owned()))]
                .into_iter()
                .collect(),
        )),
        ModelResponse::ToolCall(ToolCall::new(
            "trace-1",
            "trace_program",
            [("program".to_owned(), ToolValue::Text(program.to_owned()))]
                .into_iter()
                .collect(),
        )),
        ModelResponse::Text("The program compiles and produces 5.".to_owned()),
    ]);
    let mut agent = Agent::new(
        model,
        coding_tool_registry(working_directory()).expect("agent tools should register"),
    );

    println!("Runtime agent prompt: compile and trace 2 + 3");
    match agent.run("compile and trace PUSH 2, PUSH 3, ADD, HALT") {
        Ok(events) => {
            for event in events {
                match event {
                    AgentEvent::ToolCall(call) => {
                        println!("tool call: {} ({})", call.name, call.id)
                    }
                    AgentEvent::ToolResult(result) => println!("tool result:\n{}", result.content),
                    AgentEvent::AssistantText { content } => println!("assistant: {content}"),
                    AgentEvent::Done => println!("done"),
                    _ => {}
                }
            }
        }
        Err(error) => {
            eprintln!("agent error: {error}");
            std::process::exit(1);
        }
    }
}

fn run_guest_demo() {
    use a_rust_vm::runtime::VmInstance;

    let program = vec![
        Instruction::Push(2),
        Instruction::Push(3),
        Instruction::Add,
        Instruction::Halt,
    ];
    let mut alice = VmInstance::new("user-alice");
    let mut bob = VmInstance::new("user-bob");
    alice
        .write_file("/workspace/notes.txt", "private to alice")
        .expect("guest file write should succeed");
    let alice_pid = alice
        .spawn(None, program.clone(), vec!["calculator".to_owned()])
        .expect("alice process should spawn");
    let bob_pid = bob
        .spawn(None, program, vec!["calculator".to_owned()])
        .expect("bob process should spawn");

    println!("Guest VM alice: id={} pid={alice_pid}", alice.id());
    println!("Guest VM bob:   id={} pid={bob_pid}", bob.id());
    println!(
        "Alice reads /workspace/notes.txt: {}",
        alice
            .read_text("/workspace/notes.txt")
            .expect("alice should see her guest file")
    );
    println!(
        "Bob reads /workspace/notes.txt: {}",
        bob.read_text("/workspace/notes.txt")
            .expect_err("bob must not see alice's guest file")
    );
    for event in alice
        .run_until_idle(8)
        .expect("alice guest scheduler should finish")
    {
        println!("alice event: {event:?}");
    }
    println!("Alice process: {:?}", alice.process_info(alice_pid));
    println!("Bob process:   {:?}", bob.process_info(bob_pid));
}

fn run_live_agent() {
    let using_builtin_bridge = env::var_os("A_RVM_MODEL_PROGRAM").is_none();
    let program = env::var_os("A_RVM_MODEL_PROGRAM").unwrap_or_else(|| {
        env::current_exe()
            .unwrap_or_else(|error| {
                eprintln!("failed to resolve the built-in model bridge: {error}");
                std::process::exit(2);
            })
            .into()
    });
    let arguments = if using_builtin_bridge {
        vec!["model-bridge".to_owned()]
    } else {
        env::var("A_RVM_MODEL_ARGS")
            .map(|value| {
                value
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let working_directory = working_directory();
    let model = ProcessModel::new(program)
        .with_arguments(arguments)
        .with_working_directory(&working_directory);
    let model_name = env::var("A_RVM_MODEL_NAME").unwrap_or_else(|_| "configured".to_owned());
    let mut router = ModelRouter::new();
    router
        .register_model(&model_name, ModelCapabilities::STREAMING_TOOLS, model)
        .expect("model route should register");
    router
        .set_default(&model_name)
        .expect("default model route should exist");
    router
        .set_fast(&model_name)
        .expect("fast model route should exist");
    router
        .set_strong(&model_name)
        .expect("strong model route should exist");
    let mut agent = Agent::new(
        router,
        coding_tool_registry(&working_directory).expect("agent tools should register"),
    )
    .with_system_prompt(workspace_system_prompt(&working_directory))
    .with_route_request(RouteRequest {
        requires_tools: true,
        requires_streaming: true,
        ..RouteRequest::default()
    });
    let mut route = agent.route_request().clone();
    let stdin = io::stdin();

    println!("A/RVM agent. Type /exit to quit.");
    println!("Routes: /model <name>, /fast, /strong, /default, /status");
    if using_builtin_bridge {
        println!("Model bridge: local runner");
    }
    loop {
        print!("\n> ");
        io::stdout().flush().expect("stdout should be writable");
        let mut prompt = String::new();
        if stdin
            .lock()
            .read_line(&mut prompt)
            .expect("stdin should be readable")
            == 0
        {
            break;
        }
        let prompt = prompt.trim();
        if prompt.is_empty() {
            continue;
        }
        if matches!(prompt, "/exit" | "/quit") {
            break;
        }
        if prompt == "/status" {
            let selected = match route.requested_model.as_deref() {
                Some(model) => format!("model={model}"),
                None => format!("mode={:?}", route.mode),
            };
            println!(
                "[status] {selected} tools={} streaming={}",
                route.requires_tools, route.requires_streaming
            );
            continue;
        }
        if let Some(model) = prompt.strip_prefix("/model ") {
            let model = model.trim();
            if model.is_empty() {
                println!("[route] usage: /model <name>");
            } else {
                route.requested_model = Some(model.to_owned());
                route.mode = ModelMode::Default;
                agent.set_route_request(route.clone());
                println!("[route] requested model={model}");
            }
            continue;
        }
        if let Some(mode) = match prompt {
            "/fast" => Some(ModelMode::Fast),
            "/strong" => Some(ModelMode::Strong),
            "/default" => Some(ModelMode::Default),
            _ => None,
        } {
            route.requested_model = None;
            route.mode = mode;
            agent.set_route_request(route.clone());
            println!("[route] mode={mode:?}");
            continue;
        }

        if let Err(error) = agent.run_streaming_with_approval(
            prompt,
            |request| {
                println!("[approval] allow {}? [y/N]", request.description);
                let mut answer = String::new();
                io::stdin()
                    .read_line(&mut answer)
                    .expect("stdin should be readable");
                if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    a_rust_vm::agent::PermissionDecision::Allow
                } else {
                    a_rust_vm::agent::PermissionDecision::Deny {
                        reason: "approval denied by user".to_owned(),
                    }
                }
            },
            |event| match event {
                AgentEvent::AssistantDelta { content } => {
                    print!("{content}");
                    io::stdout().flush().expect("stdout should be writable");
                }
                AgentEvent::AssistantText { content } => print!("{content}"),
                AgentEvent::ToolCall(call) => print!("\n[tool] {}\n", call.name),
                AgentEvent::ToolResult(result) => println!("[tool result] {}", result.content),
                AgentEvent::PermissionRequested(request) => {
                    println!("[permission] {}", request.description)
                }
                AgentEvent::Error { message } => println!("[error] {message}"),
                AgentEvent::UserMessage { .. } | AgentEvent::Done => {}
            },
        ) {
            eprintln!("\n[agent error] {error}");
        }
        println!();
    }
}

fn workspace_system_prompt(root: &std::path::Path) -> String {
    let instructions = load_project_instructions(root).unwrap_or_else(|error| {
        eprintln!("[warning] project instructions unavailable: {error}");
        None
    });
    system_prompt_with_instructions(instructions.as_deref())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BridgeResponse {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: ToolArguments,
    },
}

#[derive(Debug, Deserialize)]
struct RunnerEvent {
    #[serde(rename = "type")]
    event_type: String,
    text: Option<String>,
    part: Option<RunnerPart>,
}

#[derive(Debug, Deserialize)]
struct RunnerPart {
    text: Option<String>,
}

fn run_model_bridge() {
    let mut request_json = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut request_json) {
        eprintln!("failed to read model request: {error}");
        std::process::exit(1);
    }
    let request = match serde_json::from_str::<a_rust_vm::agent::ModelRequest>(&request_json) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("invalid model request: {error}");
            std::process::exit(1);
        }
    };

    let instruction = format!(
        "You are the language model inside A/RVM. Do not execute tools yourself. Decide whether to answer the user or request exactly one tool from the supplied list. Reply with exactly one JSON object and no markdown. Use one of these shapes: {{\"kind\":\"text\",\"text\":\"...\"}} or {{\"kind\":\"tool_call\",\"id\":\"call-1\",\"name\":\"tool_name\",\"arguments\":{{}}}}. Return valid JSON only.\n\nRequest JSON:\n{}",
        serde_json::to_string(&request).expect("model request should serialize")
    );

    let binary = env::var_os("A_RVM_OPENCODE_BIN").unwrap_or_else(|| "opencode".into());
    let mut command = Command::new(binary);
    command
        .args(["run", "--pure", "--format", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Ok(model) = env::var("A_RVM_OPENCODE_MODEL") {
        command.args(["--model", model.as_str()]);
    }
    command.arg(instruction);

    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("failed to start local model runner: {error}");
            std::process::exit(1);
        }
    };
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        eprintln!("local model runner failed: {}", error.trim());
        std::process::exit(1);
    }

    let mut text = String::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(event) = serde_json::from_str::<RunnerEvent>(line) else {
            continue;
        };
        if event.event_type == "text"
            && let Some(chunk) = event.text.or_else(|| event.part.and_then(|part| part.text))
        {
            text.push_str(&chunk);
        }
    }

    let response = parse_bridge_response(&text).unwrap_or_else(|error| {
        eprintln!("local model returned an invalid A/RVM response: {error}");
        std::process::exit(1);
    });
    match response {
        BridgeResponse::Text { text } => {
            emit_bridge_event(serde_json::json!({"type": "text_delta", "text": text}));
        }
        BridgeResponse::ToolCall {
            id,
            name,
            arguments,
        } => {
            emit_bridge_event(serde_json::json!({
                "type": "tool_call",
                "id": id,
                "name": name,
                "arguments": arguments,
            }));
        }
    }
    emit_bridge_event(serde_json::json!({"type": "done"}));
}

fn parse_bridge_response(text: &str) -> Result<BridgeResponse, String> {
    let trimmed = text.trim();
    let fenced = trimmed
        .strip_prefix("```")
        .and_then(|value| value.strip_suffix("```"))
        .map(|value| value.strip_prefix("json").unwrap_or(value).trim())
        .unwrap_or(trimmed);
    let json_candidates = [fenced, extract_json_object(fenced).unwrap_or(fenced)];

    for json in json_candidates {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            continue;
        };
        if let Ok(response) = serde_json::from_value::<BridgeResponse>(value.clone()) {
            return Ok(response);
        }
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("text") | Some("text_delta") => {
                if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
                    return Ok(BridgeResponse::Text {
                        text: text.to_owned(),
                    });
                }
            }
            Some("tool_call") => {
                let id = value.get("id").and_then(serde_json::Value::as_str);
                let name = value.get("name").and_then(serde_json::Value::as_str);
                if let (Some(id), Some(name)) = (id, name) {
                    let arguments = value
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    if let Ok(arguments) = serde_json::from_value(arguments) {
                        return Ok(BridgeResponse::ToolCall {
                            id: id.to_owned(),
                            name: name.to_owned(),
                            arguments,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    Err("expected a JSON text or tool_call response".to_owned())
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (start < end).then(|| &text[start..=end])
}

fn emit_bridge_event(event: serde_json::Value) {
    println!("{event}");
}

fn run_coding_demo() {
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new("list-1", "list_files", ToolArguments::new())),
        ModelResponse::ToolCall(ToolCall::new(
            "read-1",
            "read_file",
            [("path".to_owned(), ToolValue::Text("README.md".to_owned()))]
                .into_iter()
                .collect(),
        )),
        ModelResponse::Text("I inspected the workspace and read its README.".to_owned()),
    ]);
    let mut router = ModelRouter::new();
    router
        .register_model("scripted", ModelCapabilities::STREAMING_TOOLS, model)
        .expect("scripted route should register");
    router
        .set_default("scripted")
        .expect("scripted default route should exist");
    let mut agent = Agent::new(
        router,
        coding_tool_registry(working_directory()).expect("agent tools should register"),
    )
    .with_route_request(RouteRequest {
        requires_tools: true,
        ..RouteRequest::default()
    });

    println!("Agent prompt: inspect this workspace (route=scripted)");
    match agent.run("inspect this workspace") {
        Ok(events) => {
            for event in events {
                match event {
                    AgentEvent::UserMessage { content } => println!("user: {content}"),
                    AgentEvent::AssistantText { content } => println!("assistant: {content}"),
                    AgentEvent::ToolCall(call) => {
                        println!("tool call: {} ({})", call.name, call.id)
                    }
                    AgentEvent::ToolResult(result) => {
                        let content = if result.content.len() > 600 {
                            let preview = result.content.chars().take(600).collect::<String>();
                            format!("{preview}...")
                        } else {
                            result.content
                        };
                        println!("tool result: {content}");
                    }
                    AgentEvent::AssistantDelta { content } => {
                        println!("assistant delta: {content}")
                    }
                    AgentEvent::PermissionRequested(request) => {
                        println!("permission: {}", request.description)
                    }
                    AgentEvent::Error { message } => println!("error: {message}"),
                    AgentEvent::Done => println!("done"),
                }
            }
        }
        Err(error) => {
            eprintln!("agent error: {error}");
            std::process::exit(1);
        }
    }
}

fn working_directory() -> std::path::PathBuf {
    env::current_dir().unwrap_or_else(|error| {
        eprintln!("failed to resolve the working directory: {error}");
        std::process::exit(2);
    })
}

#[cfg(test)]
mod tests {
    use super::{BridgeResponse, parse_bridge_response};

    #[test]
    fn parses_bridge_text_response() {
        assert!(matches!(
            parse_bridge_response(r#"{"kind":"text","text":"hello"}"#),
            Ok(BridgeResponse::Text { text }) if text == "hello"
        ));
    }

    #[test]
    fn parses_runner_text_event_response() {
        assert!(matches!(
            parse_bridge_response(r#"{"type":"text_delta","text":"hello"}"#),
            Ok(BridgeResponse::Text { text }) if text == "hello"
        ));
    }
}
