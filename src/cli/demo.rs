//! Offline demos of the VM, the agent loop, the guest runtime, and the coding tools.

use a_rust_vm::agent::{
    Agent, AgentEvent, ModelCapabilities, ModelResponse, ModelRouter, RouteRequest, ScriptedModel,
    ToolArguments, ToolCall, ToolValue,
};
use a_rust_vm::workspace::coding_tool_registry;
use a_rust_vm::{Instruction, Vm};

use super::paths::working_directory;

pub(crate) fn run_vm_demo() {
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

pub(crate) fn run_agent_demo() {
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
                    AgentEvent::Usage(_) => {}
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
                    AgentEvent::RepeatedToolCall { tool, count } => {
                        println!("repeated tool call: {tool} (x{count})")
                    }
                    AgentEvent::Error { message } => println!("error: {message}"),
                    AgentEvent::Cancelled => println!("cancelled"),
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

pub(crate) fn run_runtime_demo() {
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

pub(crate) fn run_guest_demo() {
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

pub(crate) fn run_coding_demo() {
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
                    AgentEvent::Usage(_) => {}
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
                    AgentEvent::RepeatedToolCall { tool, count } => {
                        println!("repeated tool call: {tool} (x{count})")
                    }
                    AgentEvent::Error { message } => println!("error: {message}"),
                    AgentEvent::Cancelled => println!("cancelled"),
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
