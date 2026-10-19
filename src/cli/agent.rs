//! `arvm agent` and `arvm ask`: the interactive REPL and one-shot prompts.

use std::env;
use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use a_rust_vm::agent::{
    Agent, AgentEvent, ModelCapabilities, ModelMode, ModelRouter, ProcessModel, RouteRequest,
    load_project_instructions, system_prompt_with_instructions, system_prompt_with_skills,
};
use a_rust_vm::session::{Session, SessionStore, TurnRecord};
use a_rust_vm::task::{TaskBudget, TaskCheckpoint, TaskEventKind, TaskRecord, TaskSpec};
use a_rust_vm::task_store::{TaskStore, TaskStoreError};
use a_rust_vm::workspace::coding_tool_registry_with_options;

use super::config::load_config_or_exit;
use super::mcp::register_mcp_tools;
use super::paths::{session_directory, task_directory, working_directory};
use super::permission::merged_permission_policy;
use super::skill::{extract_skill_names, load_requested_skills};

/// Which model implementation a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelBackend {
    /// `A_RVM_MODEL_PROGRAM`: an external process speaking the JSON protocol.
    Program,
    /// `OPENROUTER_API_KEY`: the provider API, called in process.
    OpenRouter,
    /// Neither: the `opencode` bridge, slower and kept as a fallback.
    OpencodeBridge,
}

fn select_model_backend(has_program: bool, has_openrouter_key: bool) -> ModelBackend {
    if has_program {
        ModelBackend::Program
    } else if has_openrouter_key {
        ModelBackend::OpenRouter
    } else {
        ModelBackend::OpencodeBridge
    }
}

/// The default, fast, and strong model names for an OpenRouter run:
/// environment first, then the user configuration, then the default model.
fn route_models(
    default: &str,
    settings: &a_rust_vm::openrouter::ModelSettings,
    var: impl Fn(&str) -> Option<String>,
) -> (String, String) {
    let pick = |name: &str, configured: &Option<String>| {
        var(name)
            .filter(|value| !value.trim().is_empty())
            .or_else(|| configured.clone())
            .unwrap_or_else(|| default.to_owned())
    };
    (
        pick("OPENROUTER_FAST_MODEL", &settings.fast),
        pick("OPENROUTER_STRONG_MODEL", &settings.strong),
    )
}

/// Build a router with default, fast, and strong routes. Returns the router
/// and the default model's name.
fn build_router(
    backend: ModelBackend,
    working_directory: &std::path::Path,
    settings: &a_rust_vm::openrouter::ModelSettings,
) -> (ModelRouter, String) {
    let mut router = ModelRouter::new();
    let (default, fast, strong) = if backend == ModelBackend::OpenRouter {
        let base = a_rust_vm::openrouter::OpenRouterConfig::from_env_and(settings).unwrap_or_else(
            |error| {
                eprintln!("[model] {error}");
                std::process::exit(2);
            },
        );
        let (fast, strong) = route_models(&base.model, settings, |name| env::var(name).ok());
        let mut registered = Vec::<String>::new();
        for name in [&base.model, &fast, &strong] {
            if registered.contains(name) {
                continue;
            }
            let model = a_rust_vm::openrouter::OpenRouterModel::from_config(base.for_model(name))
                .unwrap_or_else(|error| {
                    eprintln!("[model] {error}");
                    std::process::exit(2);
                });
            router
                .register_model(name, ModelCapabilities::STREAMING_TOOLS, model)
                .expect("model route should register");
            registered.push(name.clone());
        }
        (base.model.clone(), fast, strong)
    } else {
        let (name, model) = build_process_model(backend, working_directory);
        router
            .register_model(&name, ModelCapabilities::STREAMING_TOOLS, model)
            .expect("model route should register");
        (name.clone(), name.clone(), name)
    };
    router
        .set_default(&default)
        .expect("default model route should exist");
    router
        .set_fast(&fast)
        .expect("fast model route should exist");
    router
        .set_strong(&strong)
        .expect("strong model route should exist");
    (router, default)
}

/// Build a process-backed model and the name it is routed under.
fn build_process_model(
    backend: ModelBackend,
    working_directory: &std::path::Path,
) -> (String, Box<dyn a_rust_vm::agent::Model>) {
    let program = match backend {
        ModelBackend::Program => env::var_os("A_RVM_MODEL_PROGRAM").unwrap_or_default(),
        _ => env::current_exe()
            .unwrap_or_else(|error| {
                eprintln!("failed to resolve the built-in model bridge: {error}");
                std::process::exit(2);
            })
            .into(),
    };
    let arguments = match backend {
        ModelBackend::Program => env::var("A_RVM_MODEL_ARGS")
            .map(|value| {
                value
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        _ => vec!["model-bridge".to_owned()],
    };
    let model = ProcessModel::new(program)
        .with_arguments(arguments)
        .with_working_directory(working_directory);
    let name = env::var("A_RVM_MODEL_NAME").unwrap_or_else(|_| "configured".to_owned());
    (name, Box::new(model))
}

pub(crate) fn run_live_agent(user_arguments: &[String]) {
    let backend = select_model_backend(
        env::var_os("A_RVM_MODEL_PROGRAM").is_some_and(|value| !value.is_empty()),
        env::var("OPENROUTER_API_KEY").is_ok_and(|value| !value.trim().is_empty()),
    );
    let working_directory = working_directory();
    let config = load_config_or_exit();
    let additional_directories = extract_workspace_directories(user_arguments);
    let (router, model_name) = build_router(backend, &working_directory, &config.model_settings());
    let mut registry = coding_tool_registry_with_options(
        &working_directory,
        &additional_directories,
        &config.env_passthrough(),
    )
    .unwrap_or_else(|error| {
        eprintln!("[workspace] {error}");
        std::process::exit(2);
    });
    let mcp_summary = register_mcp_tools(&config, &mut registry);
    let limits = config.turn_limits();
    let mut agent = Agent::new(router, registry)
        .with_max_steps(limits.max_steps)
        .with_max_tool_calls(limits.max_tool_calls)
        .with_turn_timeout(std::time::Duration::from_secs(limits.turn_timeout_secs))
        .with_context_limits(limits.context_messages, limits.context_chars)
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
    let status = RunStatus {
        instructions: working_directory.join("AGENTS.md").is_file(),
        skills: extract_skill_names(user_arguments),
        config_files: config
            .sources()
            .into_iter()
            .filter(|source| source.config.is_some())
            .map(|source| source.path.display().to_string())
            .collect(),
        mcp: mcp_summary,
        rules: policy.rules().len(),
        limits,
    };
    agent.set_permission_policy(policy);
    super::interrupt::install(agent.cancel_token());
    let mut route = agent.route_request().clone();
    let session_id = extract_session_id(user_arguments);
    let task_id = extract_task_id(user_arguments);
    if user_arguments.iter().any(|argument| argument == "--task") && task_id.is_none() {
        eprintln!("[task] --task requires an id");
        std::process::exit(2);
    }
    let store = SessionStore::new(session_directory());
    if let Some((prompt, json)) = parse_one_shot_args(user_arguments) {
        let permission = extract_one_shot_permission(user_arguments);
        if task_id.is_some() && session_id.is_some() {
            eprintln!("[task] --task uses its id as the session id; omit --session");
            std::process::exit(2);
        }
        let task_store = TaskStore::new(task_directory());
        let task = task_id.as_ref().map(|id| {
            new_one_shot_task(&task_store, &store, id, &working_directory, &prompt, limits)
                .unwrap_or_else(|error| {
                    eprintln!("[task] {error}");
                    std::process::exit(2);
                })
        });
        // `ask --session <id>` continues (or starts) a saved session.
        let session = session_id.as_ref().or(task_id.as_ref()).map(|id| {
            let session = match store.load(id) {
                Ok(existing) => existing,
                Err(error) if error.is_not_found() => {
                    Session::new(id.clone()).unwrap_or_else(|error| {
                        eprintln!("[session] {error}");
                        std::process::exit(2);
                    })
                }
                Err(error) => {
                    eprintln!("[session] cannot resume {id}: {error}");
                    std::process::exit(2);
                }
            };
            agent.set_conversation(session.resume_conversation());
            session
        });
        run_one_shot_agent(
            &mut agent,
            &prompt,
            json,
            permission,
            session.map(|s| (&store, s)),
            task.map(|record| (&task_store, record)),
        );
        return;
    }
    if task_id.is_some() {
        eprintln!("[task] --task requires a one-shot prompt");
        std::process::exit(2);
    }
    let mut session = match &session_id {
        Some(id) => match store.load(id) {
            Ok(existing) => {
                println!(
                    "[session] resumed {id} ({} messages, {} recorded turns)",
                    existing.messages.len(),
                    existing.turns.len()
                );
                agent.set_conversation(existing.resume_conversation());
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
    println!("Routes: /model <name>, /fast, /strong, /default, /status, /usage");
    let mut session_usage = a_rust_vm::agent::Usage::default();
    let mut session_steps = 0usize;
    match backend {
        ModelBackend::OpenRouter => println!("Model: {model_name} (OpenRouter)"),
        ModelBackend::Program => println!("Model: {model_name} (external program)"),
        ModelBackend::OpencodeBridge => {
            println!("Model bridge: opencode (set OPENROUTER_API_KEY for the faster native path)")
        }
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
        if prompt == "/usage" {
            if session_steps == 0 {
                println!("[usage] no model usage reported yet");
            } else {
                println!(
                    "[usage] session: {}",
                    usage_line(session_steps, &session_usage)
                );
            }
            continue;
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
            for line in status.lines() {
                println!("[status] {line}");
            }
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
        let mut turn_usage = a_rust_vm::agent::Usage::default();
        let mut turn_steps = 0usize;
        let mut record = TurnRecord::new(prompt);
        let turn = super::interrupt::TurnGuard::begin(&agent.cancel_token());
        if let Err(error) = agent.run_streaming_with_approval(
            prompt,
            |_request| {
                print!("[approval] allow? [y/N, or n <reason to tell the agent>] ");
                io::stdout().flush().expect("stdout should be writable");
                let mut answer = String::new();
                if io::stdin().read_line(&mut answer).is_err() {
                    answer.clear();
                }
                approval_decision(&answer)
            },
            |event| {
                record.record(&event);
                match event {
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
                        println!("\n[permission] {}", request.description);
                        if let Some(detail) = &request.detail {
                            for line in detail.lines() {
                                println!("  {line}");
                            }
                        }
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
                    AgentEvent::Usage(usage) => {
                        turn_usage.accumulate(&usage);
                        turn_steps += 1;
                    }
                    AgentEvent::UserMessage { .. } => {}
                }
            },
        ) {
            eprintln!("\n[agent error] {error}");
        }
        drop(turn);
        session.finish_turn(record, agent.conversation());
        if turn_steps > 0 {
            println!("\n[usage] {}", usage_line(turn_steps, &turn_usage));
            session_usage.accumulate(&turn_usage);
            session_steps += turn_steps;
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

/// What a run loaded, for `/status`: everything that shapes the agent's
/// instructions and permissions.
struct RunStatus {
    instructions: bool,
    skills: Vec<String>,
    config_files: Vec<String>,
    mcp: Vec<String>,
    rules: usize,
    limits: a_rust_vm::config::ResolvedLimits,
}

impl RunStatus {
    fn lines(&self) -> Vec<String> {
        let list = |items: &[String]| {
            if items.is_empty() {
                "none".to_owned()
            } else {
                items.join(", ")
            }
        };
        vec![
            format!(
                "project instructions: {}",
                if self.instructions {
                    "AGENTS.md"
                } else {
                    "none"
                }
            ),
            format!("skills: {}", list(&self.skills)),
            format!("config: {}", list(&self.config_files)),
            format!("mcp: {}", list(&self.mcp)),
            format!("permission rules: {}", self.rules),
            format!(
                "turn limits: {} steps, {} tool calls, {} s, context {} messages / {} KB",
                self.limits.max_steps,
                self.limits.max_tool_calls,
                self.limits.turn_timeout_secs,
                self.limits.context_messages,
                self.limits.context_chars / 1024
            ),
        ]
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
            || argument == "--task"
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

fn extract_task_id(arguments: &[String]) -> Option<String> {
    arguments
        .windows(2)
        .find(|window| window[0] == "--task")
        .map(|window| window[1].clone())
}

fn new_one_shot_task(
    task_store: &TaskStore,
    session_store: &SessionStore,
    id: &str,
    working_directory: &std::path::Path,
    prompt: &str,
    limits: a_rust_vm::config::ResolvedLimits,
) -> Result<TaskRecord, String> {
    TaskRecord::validate_id(id).map_err(|error| error.to_string())?;
    Session::new(id).map_err(|error| error.to_string())?;
    match task_store.load(id) {
        Ok(_) => return Err(format!("task '{id}' already exists")),
        Err(TaskStoreError::NotFound { .. }) => {}
        Err(error) => return Err(error.to_string()),
    }
    match session_store.load(id) {
        Ok(_) => return Err(format!("session '{id}' already exists")),
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.to_string()),
    }
    let canonical = working_directory
        .canonicalize()
        .map_err(|error| format!("cannot resolve workspace: {error}"))?;
    let path = canonical
        .to_str()
        .ok_or("workspace path is not valid UTF-8")?;
    let workspace_id = format!("sha256:{:x}", Sha256::digest(path.as_bytes()));
    let mut task = TaskRecord::new(TaskSpec {
        id: id.to_owned(),
        owner: "local".to_owned(),
        workspace_id,
        prompt: prompt.to_owned(),
        input_version_ids: Vec::new(),
        budget: TaskBudget {
            max_model_steps: u32::try_from(limits.max_steps)
                .map_err(|_| "model step budget exceeds task limit")?,
            max_tool_calls: u32::try_from(limits.max_tool_calls)
                .map_err(|_| "tool call budget exceeds task limit")?,
            timeout_seconds: limits.turn_timeout_secs,
        },
    })
    .map_err(|error| error.to_string())?;
    task.append(TaskEventKind::Started)
        .map_err(|error| error.to_string())?;
    task_store.save(&task).map_err(|error| error.to_string())?;
    Ok(task)
}

/// Read an approval answer: `y`/`yes` allows; anything else denies, and
/// text after `n` or `no` is passed to the agent as the reason.
fn approval_decision(answer: &str) -> a_rust_vm::agent::PermissionDecision {
    let answer = answer.trim();
    let lower = answer.to_ascii_lowercase();
    if matches!(lower.as_str(), "y" | "yes") {
        return a_rust_vm::agent::PermissionDecision::Allow;
    }
    let reason = ["no ", "n "]
        .iter()
        .find_map(|prefix| {
            lower
                .starts_with(prefix)
                .then(|| answer[prefix.len()..].trim())
        })
        .filter(|reason| !reason.is_empty());
    a_rust_vm::agent::PermissionDecision::Deny {
        reason: match reason {
            Some(reason) => format!("approval denied by user: {reason}"),
            None => "approval denied by user".to_owned(),
        },
    }
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
    mut session: Option<(&SessionStore, Session)>,
    mut task: Option<(&TaskStore, TaskRecord)>,
) {
    let mut record = TurnRecord::new(prompt);
    let mut total = a_rust_vm::agent::Usage::default();
    let mut steps = 0usize;
    let turn = super::interrupt::TurnGuard::begin(&agent.cancel_token());
    let result = agent.run_streaming_with_approval(
        prompt,
        |request| match permission {
            OneShotPermission::Auto => a_rust_vm::agent::PermissionDecision::Allow,
            OneShotPermission::Deny => a_rust_vm::agent::PermissionDecision::Deny {
                reason: format!("one-shot mode denies guarded tool: {}", request.description),
            },
        },
        |event| {
            record.record(&event);
            if let AgentEvent::Usage(usage) = &event {
                total.accumulate(usage);
                steps += 1;
            }
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
                    let decision = match permission {
                        OneShotPermission::Auto => "approved (--auto)",
                        OneShotPermission::Deny => "denied (use --auto or a rule to allow)",
                    };
                    println!("\n[permission {decision}] {}", request.description);
                    if let Some(detail) = &request.detail {
                        for line in detail.lines() {
                            println!("  {line}");
                        }
                    }
                    println!();
                }
                AgentEvent::RepeatedToolCall { tool, count } => {
                    println!("\n[agent] repeated tool call: {tool} (x{count})\n")
                }
                AgentEvent::Error { message } => println!("\n[error] {message}\n"),
                AgentEvent::Cancelled => println!("\n[agent] cancelled\n"),
                AgentEvent::UserMessage { .. } | AgentEvent::Done | AgentEvent::Usage(_) => {}
            }
        },
    );
    drop(turn);
    if !json && steps > 0 {
        eprintln!("\n[usage] {}", usage_line(steps, &total));
    }
    if let Err(error) = &result {
        record.record(&AgentEvent::Error {
            message: error.to_string(),
        });
    }
    let cancelled = record
        .events
        .iter()
        .any(|event| matches!(event, AgentEvent::Cancelled));
    let repeated_call = record
        .events
        .iter()
        .any(|event| matches!(event, AgentEvent::RepeatedToolCall { .. }));
    let mut checkpoint = None;
    let mut session_save_error = None;
    if let Some((store, session)) = session.as_mut() {
        session.push("user", prompt);
        let answer = record
            .events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::AssistantText { content } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !answer.is_empty() {
            session.push("assistant", answer);
        }
        session.finish_turn(record, agent.conversation());
        match store.save(session) {
            Ok(()) => match u32::try_from(session.turns.len()) {
                Ok(completed_turns) => {
                    checkpoint = Some(TaskCheckpoint {
                        session_id: session.id.clone(),
                        completed_turns,
                    })
                }
                Err(_) => {
                    session_save_error =
                        Some("session turn count exceeds task checkpoint limit".to_owned());
                }
            },
            Err(error) => {
                eprintln!("[session] cannot save: {error}");
                session_save_error = Some(error.to_string());
            }
        }
    }
    if let Some((store, task)) = task.as_mut() {
        if let Some(checkpoint) = checkpoint {
            task.append(TaskEventKind::Checkpointed { checkpoint })
                .unwrap_or_else(|error| {
                    eprintln!("[task] cannot checkpoint: {error}");
                    std::process::exit(1);
                });
        }
        let outcome = if let Some(error) = &session_save_error {
            TaskEventKind::Failed {
                reason: format!("cannot save session: {error}"),
            }
        } else if let Err(error) = &result {
            TaskEventKind::Failed {
                reason: error.to_string(),
            }
        } else if cancelled {
            TaskEventKind::Cancelled
        } else if repeated_call {
            TaskEventKind::Failed {
                reason: "repeated tool call without an answer".to_owned(),
            }
        } else {
            TaskEventKind::Completed {
                output_version_ids: Vec::new(),
            }
        };
        task.append(outcome).unwrap_or_else(|error| {
            eprintln!("[task] cannot record outcome: {error}");
            std::process::exit(1);
        });
        store.save(task).unwrap_or_else(|error| {
            eprintln!("[task] cannot save outcome: {error}");
            std::process::exit(1);
        });
    }
    if let Err(error) = result {
        if json {
            let event = serde_json::json!({"type": "error", "message": error.to_string()});
            println!("{event}");
        } else {
            eprintln!("\n[agent error] {error}\n");
        }
        std::process::exit(1);
    }
    if session_save_error.is_some() && task.is_some() {
        std::process::exit(1);
    }
    if !json {
        println!();
    }
}

fn usage_line(steps: usize, total: &a_rust_vm::agent::Usage) -> String {
    let noun = if steps == 1 { "step" } else { "steps" };
    format!("{steps} model {noun} · {}", total.summary())
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
    fn fast_and_strong_routes_fall_back_to_the_default_model() {
        let settings = a_rust_vm::openrouter::ModelSettings {
            fast: Some("config/fast".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            route_models("main", &settings, |_| None),
            ("config/fast".to_owned(), "main".to_owned())
        );
        let env = |name: &str| (name == "OPENROUTER_STRONG_MODEL").then(|| "env/strong".to_owned());
        assert_eq!(
            route_models("main", &settings, env),
            ("config/fast".to_owned(), "env/strong".to_owned())
        );
    }

    #[test]
    fn model_backend_prefers_program_then_native_then_bridge() {
        assert_eq!(select_model_backend(true, true), ModelBackend::Program);
        assert_eq!(select_model_backend(false, true), ModelBackend::OpenRouter);
        assert_eq!(
            select_model_backend(false, false),
            ModelBackend::OpencodeBridge
        );
    }

    #[test]
    fn approval_answers_allow_deny_and_carry_reasons() {
        use a_rust_vm::agent::PermissionDecision;
        assert_eq!(super::approval_decision("y\n"), PermissionDecision::Allow);
        assert_eq!(super::approval_decision(" YES "), PermissionDecision::Allow);
        let denied = |reason: &str| PermissionDecision::Deny {
            reason: reason.to_owned(),
        };
        assert_eq!(
            super::approval_decision(""),
            denied("approval denied by user")
        );
        assert_eq!(
            super::approval_decision("n"),
            denied("approval denied by user")
        );
        assert_eq!(
            super::approval_decision("yep"),
            denied("approval denied by user")
        );
        assert_eq!(
            super::approval_decision("n Use tests/ instead\n"),
            denied("approval denied by user: Use tests/ instead")
        );
        assert_eq!(
            super::approval_decision("No   too risky"),
            denied("approval denied by user: too risky")
        );
    }

    #[test]
    fn status_lists_what_shapes_the_run() {
        let status = RunStatus {
            instructions: true,
            skills: vec!["review".to_owned()],
            config_files: Vec::new(),
            mcp: vec!["files (3 tools)".to_owned()],
            rules: 2,
            limits: a_rust_vm::config::TurnLimits::DEFAULTS,
        };
        assert_eq!(
            status.lines(),
            [
                "project instructions: AGENTS.md",
                "skills: review",
                "config: none",
                "mcp: files (3 tools)",
                "permission rules: 2",
                "turn limits: 50 steps, 150 tool calls, 900 s, context 400 messages / 256 KB",
            ]
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
