//! `arvm agent` and `arvm ask`: the interactive REPL and one-shot prompts.

use std::env;
use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use a_rust_vm::agent::{
    Agent, AgentEvent, ConversationMessage, ModelCapabilities, ModelMode, ModelRouter,
    ProcessModel, RouteRequest, load_project_instructions, system_prompt_with_instructions,
    system_prompt_with_skills,
};
use a_rust_vm::session::{Session, SessionStore};
use a_rust_vm::workspace::coding_tool_registry_with_directories;

use super::config::load_config_or_exit;
use super::mcp::register_mcp_tools;
use super::paths::{session_directory, working_directory};
use super::permission::merged_permission_policy;
use super::skill::load_requested_skills;

pub(crate) fn run_live_agent(user_arguments: &[String]) {
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
    let config = load_config_or_exit();
    let additional_directories = extract_workspace_directories(user_arguments);
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
    let mut registry =
        coding_tool_registry_with_directories(&working_directory, &additional_directories)
            .unwrap_or_else(|error| {
                eprintln!("[workspace] {error}");
                std::process::exit(2);
            });
    register_mcp_tools(&config, &mut registry);
    let mut agent = Agent::new(router, registry)
        .with_system_prompt(workspace_system_prompt_with_skills(
            &working_directory,
            &load_requested_skills(&working_directory, user_arguments),
        ))
        .with_route_request(RouteRequest {
            requires_tools: true,
            requires_streaming: true,
            ..RouteRequest::default()
        });
    let policy = match merged_permission_policy(user_arguments, config.permission_rules()) {
        Ok(policy) => policy,
        Err(error) => {
            eprintln!("[permission] {error}");
            std::process::exit(2);
        }
    };
    agent.set_permission_policy(policy);
    let mut route = agent.route_request().clone();
    let session_id = extract_session_id(user_arguments);
    if let Some((prompt, json)) = parse_one_shot_args(user_arguments) {
        let permission = extract_one_shot_permission(user_arguments);
        run_one_shot_agent(&mut agent, &prompt, json, permission);
        return;
    }
    let store = SessionStore::new(session_directory());
    let mut session = match &session_id {
        Some(id) => match store.load(id) {
            Ok(existing) => {
                println!(
                    "[session] resumed {id} ({} messages)",
                    existing.messages.len()
                );
                let conversation = existing
                    .messages
                    .iter()
                    .map(|message| ConversationMessage::new(&message.role, &message.content))
                    .collect::<Vec<_>>();
                agent.set_conversation(conversation);
                existing
            }
            Err(error) => {
                eprintln!("[session] cannot resume {id}: {error}");
                std::process::exit(2);
            }
        },
        None => {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or_default();
            match Session::new(format!("ask-{timestamp}")) {
                Ok(created) => created,
                Err(error) => {
                    eprintln!("[session] {error}");
                    std::process::exit(2);
                }
            }
        }
    };
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

        session.push("user", prompt);
        let mut reply = String::new();
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
                    reply.push_str(&content);
                    print!("{content}");
                    io::stdout().flush().expect("stdout should be writable");
                }
                AgentEvent::AssistantText { content } => {
                    reply.push_str(&content);
                    print!("{content}");
                }
                AgentEvent::ToolCall(call) => print!("\n[tool] {}\n", call.name),
                AgentEvent::ToolResult(result) => {
                    session.push("tool", format!("{}: {}", result.name, result.content));
                    println!("[tool result] {}", result.content);
                }
                AgentEvent::PermissionRequested(request) => {
                    println!("[permission] {}", request.description)
                }
                AgentEvent::RepeatedToolCall { tool, count } => {
                    println!("[agent] repeated tool call: {tool} (x{count})")
                }
                AgentEvent::Error { message } => println!("[error] {message}"),
                AgentEvent::Cancelled => println!("\n[agent] cancelled"),
                AgentEvent::Done => {
                    if !reply.is_empty() {
                        session.push("assistant", std::mem::take(&mut reply));
                    }
                }
                AgentEvent::UserMessage { .. } => {}
            },
        ) {
            eprintln!("\n[agent error] {error}");
        }
        if let Err(error) = store.save(&session) {
            eprintln!("\n[session] cannot save: {error}");
        }
        println!();
    }
    if !session.messages.is_empty() {
        println!("\n[session] saved {}", session.id);
    }
}

fn parse_one_shot_args(arguments: &[String]) -> Option<(String, bool)> {
    if arguments.is_empty() {
        return None;
    }
    let json = arguments.iter().any(|argument| argument == "--json");
    let mut prompt_parts = Vec::new();
    let mut skip_next = false;
    for argument in arguments {
        if argument == "--session"
            || argument == "--rule"
            || argument == "--skill"
            || argument == "--workspace-dir"
        {
            skip_next = true;
            continue;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if !matches!(argument.as_str(), "--json" | "--auto" | "--deny") {
            prompt_parts.push(argument.clone());
        }
    }
    let prompt = prompt_parts.join(" ");
    if prompt.trim().is_empty() {
        None
    } else {
        Some((prompt, json))
    }
}

fn extract_workspace_directories(arguments: &[String]) -> Vec<std::path::PathBuf> {
    arguments
        .windows(2)
        .filter(|window| window[0] == "--workspace-dir")
        .map(|window| std::path::PathBuf::from(&window[1]))
        .collect()
}

fn extract_session_id(arguments: &[String]) -> Option<String> {
    arguments
        .windows(2)
        .find(|window| window[0] == "--session")
        .map(|window| window[1].clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OneShotPermission {
    Auto,
    Deny,
}

fn extract_one_shot_permission(arguments: &[String]) -> OneShotPermission {
    if arguments.iter().any(|argument| argument == "--auto") {
        OneShotPermission::Auto
    } else {
        OneShotPermission::Deny
    }
}

fn run_one_shot_agent(
    agent: &mut Agent<ModelRouter>,
    prompt: &str,
    json: bool,
    permission: OneShotPermission,
) {
    let result = agent.run_streaming_with_approval(
        prompt,
        |request| match permission {
            OneShotPermission::Auto => a_rust_vm::agent::PermissionDecision::Allow,
            OneShotPermission::Deny => a_rust_vm::agent::PermissionDecision::Deny {
                reason: format!("one-shot mode denies guarded tool: {}", request.description),
            },
        },
        |event| {
            if json {
                if let Ok(line) = serde_json::to_string(&event) {
                    println!("{line}");
                }
                return;
            }
            match event {
                AgentEvent::AssistantDelta { content } | AgentEvent::AssistantText { content } => {
                    print!("{content}");
                    io::stdout().flush().expect("stdout should be writable");
                }
                AgentEvent::ToolCall(call) => println!("\n[tool] {}\n", call.name),
                AgentEvent::ToolResult(result) => println!("\n[tool result] {}\n", result.content),
                AgentEvent::PermissionRequested(request) => {
                    println!("\n[permission denied] {}\n", request.description)
                }
                AgentEvent::RepeatedToolCall { tool, count } => {
                    println!("\n[agent] repeated tool call: {tool} (x{count})\n")
                }
                AgentEvent::Error { message } => println!("\n[error] {message}\n"),
                AgentEvent::Cancelled => println!("\n[agent] cancelled\n"),
                AgentEvent::UserMessage { .. } | AgentEvent::Done => {}
            }
        },
    );
    if let Err(error) = result {
        if json {
            let event = serde_json::json!({"type": "error", "message": error.to_string()});
            println!("{event}");
        } else {
            eprintln!("\n[agent error] {error}\n");
        }
        std::process::exit(1);
    }
    if !json {
        println!();
    }
}

fn workspace_system_prompt_with_skills(root: &std::path::Path, skills: &[String]) -> String {
    let instructions = load_project_instructions(root).unwrap_or_else(|error| {
        eprintln!("[warning] project instructions unavailable: {error}");
        None
    });
    system_prompt_with_skills(
        &system_prompt_with_instructions(instructions.as_deref()),
        skills,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::permission::extract_permission_rule_texts;
    use crate::cli::skill::{extract_skill_names, skill_roots_for};

    #[test]
    fn one_shot_args_extract_prompt_and_json_flag() {
        assert_eq!(
            super::parse_one_shot_args(&["--json".to_owned(), "do".to_owned(), "x".to_owned()]),
            Some(("do x".to_owned(), true))
        );
        assert_eq!(
            super::parse_one_shot_args(&["do".to_owned(), "x".to_owned()]),
            Some(("do x".to_owned(), false))
        );
        assert_eq!(super::parse_one_shot_args(&[]), None);
        assert_eq!(super::parse_one_shot_args(&["--json".to_owned()]), None);
        assert_eq!(
            super::parse_one_shot_args(&[
                "--session".to_owned(),
                "s-1".to_owned(),
                "--json".to_owned(),
                "hi".to_owned()
            ]),
            Some(("hi".to_owned(), true))
        );
        assert_eq!(
            super::extract_session_id(&["--session".to_owned(), "s-1".to_owned(), "hi".to_owned()]),
            Some("s-1".to_owned())
        );
        assert_eq!(super::extract_session_id(&["hi".to_owned()]), None);
        assert_eq!(
            super::parse_one_shot_args(&["--auto".to_owned(), "write".to_owned(), "x".to_owned()]),
            Some(("write x".to_owned(), false))
        );
        assert_eq!(
            super::extract_one_shot_permission(&["--auto".to_owned(), "hi".to_owned()]),
            super::OneShotPermission::Auto
        );
        assert_eq!(
            super::extract_one_shot_permission(&["hi".to_owned()]),
            super::OneShotPermission::Deny
        );
        assert_eq!(
            super::parse_one_shot_args(&[
                "--skill".to_owned(),
                "review".to_owned(),
                "do".to_owned(),
                "x".to_owned()
            ]),
            Some(("do x".to_owned(), false))
        );
        assert_eq!(
            super::parse_one_shot_args(&[
                "--workspace-dir".to_owned(),
                "/tmp/extra".to_owned(),
                "do".to_owned(),
                "x".to_owned()
            ]),
            Some(("do x".to_owned(), false))
        );
    }

    #[test]
    fn workspace_dir_flags_are_extracted_repeatably() {
        let arguments = [
            "--workspace-dir".to_owned(),
            "/tmp/first".to_owned(),
            "hi".to_owned(),
            "--workspace-dir".to_owned(),
            "/tmp/second".to_owned(),
        ];
        assert_eq!(
            extract_workspace_directories(&arguments),
            vec![
                std::path::PathBuf::from("/tmp/first"),
                std::path::PathBuf::from("/tmp/second")
            ]
        );
        assert!(extract_workspace_directories(&["hi".to_owned()]).is_empty());
    }

    #[test]
    fn skill_flags_are_extracted_and_composed_into_prompt() {
        let arguments = [
            "--skill".to_owned(),
            "review".to_owned(),
            "do".to_owned(),
            "x".to_owned(),
        ];
        assert_eq!(extract_skill_names(&arguments), vec!["review".to_owned()]);
        assert!(extract_skill_names(&["hi".to_owned()]).is_empty());

        let project =
            std::env::temp_dir().join(format!("a-rvm-skill-prompt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(project.join("skills").join("review")).unwrap();
        std::fs::write(
            project.join("skills").join("review").join("SKILL.md"),
            "---\nname: review\ndescription: Review changes\n---\nCheck diffs.\n",
        )
        .unwrap();
        let roots = skill_roots_for(
            &project,
            &std::env::temp_dir().join("a-rvm-skill-prompt-missing-user"),
        );
        let skill = a_rust_vm::skills::load_skill(&roots, "review").unwrap();
        assert!(a_rust_vm::skills::load_skill(&roots, "missing").is_err());
        let prompt = workspace_system_prompt_with_skills(&project, &[skill.scoped_instructions()]);
        assert!(prompt.contains("Check diffs."));
        assert!(prompt.contains("Loaded skills"));
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn permission_rule_flags_are_extracted_and_excluded_from_prompt() {
        let arguments = [
            "--rule".to_owned(),
            "allow write_file:notes.txt".to_owned(),
            "--rule".to_owned(),
            "deny run_command:rm".to_owned(),
            "write".to_owned(),
            "notes".to_owned(),
        ];
        assert_eq!(
            extract_permission_rule_texts(&arguments),
            vec![
                "allow write_file:notes.txt".to_owned(),
                "deny run_command:rm".to_owned(),
            ]
        );
        assert_eq!(
            super::parse_one_shot_args(&arguments),
            Some(("write notes".to_owned(), false))
        );
        assert!(extract_permission_rule_texts(&["hi".to_owned()]).is_empty());
    }
}
