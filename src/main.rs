use std::env;
use std::io::{self, BufRead, Write};

use a_rust_vm::agent::{
    Agent, AgentEvent, ModelResponse, ProcessModel, ScriptedModel, ToolArguments, ToolCall,
    ToolValue,
};
use a_rust_vm::workspace::coding_tool_registry;
use a_rust_vm::{Instruction, Vm};

fn main() {
    if env::args().nth(1).as_deref() == Some("agent-demo") {
        run_agent_demo();
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

fn run_live_agent() {
    let Some(program) = env::var_os("A_RVM_MODEL_PROGRAM") else {
        eprintln!("set A_RVM_MODEL_PROGRAM to a model process before running `agent`");
        std::process::exit(2);
    };
    let arguments = env::var("A_RVM_MODEL_ARGS")
        .map(|value| {
            value
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let working_directory = working_directory();
    let model = ProcessModel::new(program)
        .with_arguments(arguments)
        .with_working_directory(&working_directory);
    let mut agent = Agent::new(
        model,
        coding_tool_registry(&working_directory).expect("agent tools should register"),
    );
    let stdin = io::stdin();

    println!("A/RVM agent. Type /exit to quit.");
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
    let mut agent = Agent::new(
        model,
        coding_tool_registry(working_directory()).expect("agent tools should register"),
    );

    println!("Agent prompt: inspect this workspace");
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
