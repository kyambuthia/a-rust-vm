use std::env;
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use a_rust_vm::agent::{
    Agent, AgentEvent, ConversationMessage, ModelCapabilities, ModelMode, ModelResponse,
    ModelRouter, ProcessModel, RouteRequest, ScriptedModel, ToolArguments, ToolCall, ToolValue,
    load_project_instructions, system_prompt_with_instructions, system_prompt_with_skills,
};
use a_rust_vm::program::Program;
use a_rust_vm::session::{Session, SessionStore};
use a_rust_vm::workspace::{coding_tool_registry, coding_tool_registry_with_directories};
use a_rust_vm::{Instruction, Vm};

const MODEL_ATTEMPTS: usize = 2;
const DEFAULT_MODEL: &str = "openrouter/deepseek/deepseek-v4-flash";

fn main() {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let command = arguments.first().map(String::as_str);
    if matches!(command, None | Some("help" | "--help" | "-h")) {
        print_help();
        return;
    }
    if matches!(command, Some("version" | "--version" | "-V")) {
        println!("arvm {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if command == Some("doctor") {
        run_doctor(&arguments[1..]);
        return;
    }
    if command == Some("workspace") {
        run_workspace_command(&arguments[1..]);
        return;
    }
    if command == Some("session") {
        run_session_command(&arguments[1..]);
        return;
    }
    if command == Some("permission") {
        run_permission_command(&arguments[1..]);
        return;
    }
    if command == Some("skill") {
        run_skill_command(&arguments[1..]);
        return;
    }
    if matches!(command, Some("run" | "check" | "disassemble" | "trace")) {
        run_program_command(command.expect("matched above"), &arguments[1..]);
        return;
    }
    if command == Some("demo") {
        run_vm_demo();
        return;
    }
    if command == Some("agent-demo") {
        run_agent_demo();
        return;
    }
    if command == Some("runtime-demo") {
        run_runtime_demo();
        return;
    }
    if command == Some("guest-demo") {
        run_guest_demo();
        return;
    }
    if command == Some("coding-demo") {
        run_coding_demo();
        return;
    }
    if matches!(command, Some("agent" | "ask")) {
        run_live_agent(&arguments[1..]);
        return;
    }
    if command == Some("model-bridge") {
        run_model_bridge();
        return;
    }
    if command == Some("serve") {
        a_rust_vm::server::run(working_directory());
        return;
    }

    eprintln!("unknown command: {}\n", command.unwrap_or_default());
    print_help();
    std::process::exit(2);
}

fn print_help() {
    println!(
        "A/RVM - a deterministic stack VM and agent runtime\n\n\
Usage: arvm <command> [options]\n\n\
Core commands:\n  run <file|-> [--json]  Validate and execute assembly\n  check <file|->          Validate without executing\n  disassemble <file|->    Print stable instruction offsets\n  trace <file|->          Execute and print deterministic stack trace\n  demo                    Run the built-in VM example\n\n\
Product commands:\n  agent | ask              Interactive coding agent REPL\n  ask [--json] [--auto] [--rule <rule>]... [--skill <name>]... [--workspace-dir <path>]... <prompt..>  One-shot prompt (exit after one turn)\n  serve                   Host the browser and agent API\n  session <command>       Manage local agent sessions (list/show/save/export)\n  skill <command>         Discover and load project skills (list/show/load)\n  permission <command>    Manage durable permission rules (list/add/clear)\n  workspace <command>     Manage durable owned workspaces\n  doctor [--json]         Report platform capabilities and readiness\n  version                 Print version information\n  help                    Show this help\n\n\
Assembly is line-oriented. Instructions: PUSH <i32>, ADD, SUB, MUL, DIV, HALT.\n\
Use '-' to read a program from standard input; '#' starts a comment."
    );
}

const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

fn parse_artifact_kind(value: &str) -> Result<a_rust_vm::artifacts::ArtifactKind, String> {
    match value {
        "source" => Ok(a_rust_vm::artifacts::ArtifactKind::Source),
        "generated" => Ok(a_rust_vm::artifacts::ArtifactKind::Generated),
        _ => Err(format!(
            "invalid artifact kind '{value}': expected source|generated"
        )),
    }
}

fn check_artifact_byte_len(byte_len: u64) -> Result<(), String> {
    if byte_len > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "artifact file too large: {byte_len} bytes exceeds 64 MiB limit"
        ));
    }
    Ok(())
}

struct ArtifactImportArguments<'a> {
    workspace: &'a str,
    artifact_id: &'a str,
    version_id: &'a str,
    file: &'a str,
    media_type: &'a str,
    parent_version_id: Option<&'a str>,
}

fn parse_artifact_import_arguments(
    arguments: &[String],
) -> Result<ArtifactImportArguments<'_>, String> {
    if arguments.len() < 5 || arguments.len() > 6 {
        return Err(
            "usage: arvm workspace artifact import <workspace> <artifact-id> <version-id> <file> <media-type> [parent-version-id]".to_owned(),
        );
    }
    Ok(ArtifactImportArguments {
        workspace: &arguments[0],
        artifact_id: &arguments[1],
        version_id: &arguments[2],
        file: &arguments[3],
        media_type: &arguments[4],
        parent_version_id: arguments.get(5).map(String::as_str),
    })
}

fn run_workspace_command(arguments: &[String]) {
    use a_rust_vm::control_plane::CapabilityGrant;
    use a_rust_vm::control_plane_store::ControlPlaneStore;
    use a_rust_vm::runtime::ResourceLimits;

    let owner = env::var("A_RVM_OWNER")
        .or_else(|_| env::var("USER"))
        .unwrap_or_else(|_| "local".to_owned());
    let store = ControlPlaneStore::new(control_plane_path(), 64, ResourceLimits::default());
    let mut plane = store.load().unwrap_or_else(|error| {
        eprintln!("cannot load workspace state: {error}");
        std::process::exit(1);
    });
    let command = arguments.first().map(String::as_str);
    if command == Some("artifact") {
        if let Err(error) =
            run_workspace_artifact_command(&arguments[1..], &owner, &store, &mut plane)
        {
            eprintln!("workspace error: {error}");
            std::process::exit(1);
        }
        return;
    }
    let result = match command {
        Some("create") if arguments.len() == 2 => {
            plane.create_workspace(&owner, &arguments[1]).map(|()| {
                store.save(&plane).unwrap_or_else(|error| {
                    eprintln!("cannot save workspace state: {error}");
                    std::process::exit(1);
                });
                println!("created workspace {owner}/{}", arguments[1]);
            })
        }
        Some("list") if arguments.len() == 1 => {
            let workspaces = plane.list(&owner);
            if workspaces.is_empty() {
                println!("no workspaces for {owner}");
            } else {
                for workspace in workspaces {
                    println!(
                        "{}\t{:?}\t{} capabilities\t{} observations",
                        workspace.id,
                        workspace.state,
                        workspace.capabilities.len(),
                        workspace.observations.len()
                    );
                }
            }
            Ok(())
        }
        Some("show") if arguments.len() == 2 => {
            plane.inspect(&owner, &arguments[1]).map(|workspace| {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&workspace)
                        .expect("workspace view is serializable")
                );
            })
        }
        Some("grant") if arguments.len() >= 6 => {
            let grant = CapabilityGrant::new(
                &arguments[2],
                &arguments[3],
                &arguments[4],
                arguments[5..].iter().cloned(),
            );
            grant
                .and_then(|grant| plane.grant_capability(&owner, &arguments[1], grant))
                .map(|()| {
                    store.save(&plane).unwrap_or_else(|error| {
                        eprintln!("cannot save workspace state: {error}");
                        std::process::exit(1);
                    });
                    println!(
                        "granted capability {} to {owner}/{}",
                        arguments[2], arguments[1]
                    );
                })
        }
        Some("revoke") if arguments.len() == 3 => plane
            .revoke_capability(&owner, &arguments[1], &arguments[2])
            .map(|()| {
                store.save(&plane).unwrap_or_else(|error| {
                    eprintln!("cannot save workspace state: {error}");
                    std::process::exit(1);
                });
                println!(
                    "revoked capability {} from {owner}/{}",
                    arguments[2], arguments[1]
                );
            }),
        _ => {
            eprintln!(
                "usage:\n  arvm workspace create <id>\n  arvm workspace list\n  arvm workspace show <id>\n  arvm workspace grant <workspace> <capability> <kind> <resource> <action>...\n  arvm workspace revoke <workspace> <capability>\n  arvm workspace artifact create <workspace> <artifact-id> <name> <source|generated>\n  arvm workspace artifact import <workspace> <artifact-id> <version-id> <file> <media-type> [parent-version-id]\n  arvm workspace artifact list <workspace>\n  arvm workspace artifact history <workspace> <artifact-id>\n\nSet A_RVM_OWNER to select the local principal."
            );
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("workspace error: {error}");
        std::process::exit(1);
    }
}

fn save_workspace_state(
    store: &a_rust_vm::control_plane_store::ControlPlaneStore,
    plane: &a_rust_vm::control_plane::ControlPlane,
) {
    store.save(plane).unwrap_or_else(|error| {
        eprintln!("cannot save workspace state: {error}");
        std::process::exit(1);
    });
}

fn run_workspace_artifact_command(
    arguments: &[String],
    owner: &str,
    store: &a_rust_vm::control_plane_store::ControlPlaneStore,
    plane: &mut a_rust_vm::control_plane::ControlPlane,
) -> Result<(), String> {
    let command = arguments.first().map(String::as_str);
    match command {
        Some("create") if arguments.len() == 5 => {
            let kind = parse_artifact_kind(&arguments[4])?;
            plane
                .create_artifact(
                    owner,
                    &arguments[1],
                    a_rust_vm::artifacts::NewArtifact::new(
                        &arguments[2],
                        &arguments[1],
                        &arguments[3],
                        kind,
                        owner,
                    ),
                )
                .map(|artifact| {
                    save_workspace_state(store, plane);
                    println!("created artifact {} in {owner}/{}", artifact.id, arguments[1]);
                })
                .map_err(|error| error.to_string())
        }
        Some("import") => {
            let request = parse_artifact_import_arguments(&arguments[1..])?;
            let bytes = std::fs::read(request.file)
                .map_err(|error| format!("cannot read '{}': {error}", request.file))?;
            check_artifact_byte_len(bytes.len() as u64)?;
            let stored = store
                .content_store()
                .put(&bytes)
                .map_err(|error| error.to_string())?;
            let mut version = a_rust_vm::artifacts::NewArtifactVersion::new(
                request.version_id,
                request.artifact_id,
                &stored.reference,
                request.media_type,
                stored.byte_len,
                owner,
            );
            if let Some(parent) = request.parent_version_id {
                version = version.with_parent(parent);
            }
            plane
                .append_artifact_version(owner, request.workspace, version)
                .map(|created| {
                    save_workspace_state(store, plane);
                    println!(
                        "imported version {} for {} ({})",
                        created.id, created.artifact_id, created.content_reference
                    );
                })
                .map_err(|error| error.to_string())
        }
        Some("list") if arguments.len() == 2 => plane
            .list_artifacts(owner, &arguments[1])
            .map(|artifacts| {
                if artifacts.is_empty() {
                    println!("no artifacts in {owner}/{}", arguments[1]);
                } else {
                    for artifact in artifacts {
                        println!(
                            "{}\t{}\t{:?}\t{}",
                            artifact.id,
                            artifact.name,
                            artifact.kind,
                            artifact.current_version_id.as_deref().unwrap_or("-")
                        );
                    }
                }
            })
            .map_err(|error| error.to_string()),
        Some("history") if arguments.len() == 3 => plane
            .artifact_version_history(owner, &arguments[1], &arguments[2])
            .map(|versions| {
                for version in versions {
                    println!(
                        "{}\t{}\t{}\t{}",
                        version.id,
                        version.content_reference,
                        version.media_type,
                        version
                            .parent_version_id
                            .as_deref()
                            .unwrap_or("-")
                    );
                }
            })
            .map_err(|error| error.to_string()),
        _ => Err("usage:\n  arvm workspace artifact create <workspace> <artifact-id> <name> <source|generated>\n  arvm workspace artifact import <workspace> <artifact-id> <version-id> <file> <media-type> [parent-version-id]\n  arvm workspace artifact list <workspace>\n  arvm workspace artifact history <workspace> <artifact-id>".to_owned()),
    }
}

fn control_plane_path() -> std::path::PathBuf {
    if let Some(directory) = env::var_os("A_RVM_STATE_DIR") {
        return std::path::PathBuf::from(directory).join("control-plane.json");
    }
    if let Some(directory) = env::var_os("XDG_STATE_HOME") {
        return std::path::PathBuf::from(directory).join("a-rust-vm/control-plane.json");
    }
    env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".local/state/a-rust-vm/control-plane.json")
}

fn session_directory() -> std::path::PathBuf {
    if let Some(directory) = env::var_os("A_RVM_STATE_DIR") {
        return std::path::PathBuf::from(directory).join("sessions");
    }
    if let Some(directory) = env::var_os("XDG_STATE_HOME") {
        return std::path::PathBuf::from(directory).join("a-rust-vm/sessions");
    }
    env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".local/state/a-rust-vm/sessions")
}

fn session_usage() {
    eprintln!("usage: arvm session list|show <id>|save <id> <role> <content>|export <id> [--json]");
}

fn format_session_text(session: &Session) -> String {
    if session.messages.is_empty() {
        return format!("(empty session {})", session.id);
    }
    session
        .messages
        .iter()
        .map(|message| format!("[{}] {}", message.role, message.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_session_json(session: &Session) -> Result<String, String> {
    serde_json::to_string_pretty(session)
        .map_err(|error| format!("cannot encode session '{}': {error}", session.id))
}

fn export_session(store: &SessionStore, id: &str, json: bool) -> Result<String, String> {
    let session = store.load(id).map_err(|error| error.to_string())?;
    if json {
        format_session_json(&session)
    } else {
        Ok(format_session_text(&session))
    }
}

fn parse_session_export_args(arguments: &[String]) -> Result<(String, bool), String> {
    match arguments {
        [id] => Ok((id.clone(), false)),
        [id, flag] if flag == "--json" => Ok((id.clone(), true)),
        _ => Err("usage: arvm session export <id> [--json]".to_owned()),
    }
}

fn permission_rules_path() -> std::path::PathBuf {
    if let Some(directory) = env::var_os("A_RVM_STATE_DIR") {
        return std::path::PathBuf::from(directory).join("rules.json");
    }
    if let Some(directory) = env::var_os("XDG_STATE_HOME") {
        return std::path::PathBuf::from(directory).join("a-rust-vm/rules.json");
    }
    env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".local/state/a-rust-vm/rules.json")
}

fn load_stored_permission_policy(
    path: &std::path::Path,
) -> Result<a_rust_vm::permissions::PermissionPolicy, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(a_rust_vm::permissions::PermissionPolicy::new());
        }
        Err(error) => return Err(format!("cannot read permission rules: {error}")),
    };
    if bytes.is_empty() {
        return Ok(a_rust_vm::permissions::PermissionPolicy::new());
    }
    let stored: Vec<String> = serde_json::from_slice(&bytes)
        .map_err(|error| format!("cannot decode permission rules: {error}"))?;
    let mut policy = a_rust_vm::permissions::PermissionPolicy::new();
    for text in stored {
        let rule = a_rust_vm::permissions::parse_permission_rule(&text)
            .map_err(|error| format!("invalid stored rule '{text}': {error}"))?;
        policy.add_rule(rule);
    }
    Ok(policy)
}

fn save_stored_permission_rule_at(path: &std::path::Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create permission directory: {error}"))?;
    }
    let trimmed = text.trim().to_owned();
    a_rust_vm::permissions::parse_permission_rule(&trimmed)
        .map_err(|error| format!("invalid rule '{trimmed}': {error}"))?;
    let mut stored: Vec<String> = match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Vec::new(),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("cannot decode permission rules: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(format!("cannot read permission rules: {error}")),
    };
    for existing in &stored {
        a_rust_vm::permissions::parse_permission_rule(existing)
            .map_err(|error| format!("invalid stored rule '{existing}': {error}"))?;
    }
    stored.push(trimmed);
    let content = serde_json::to_vec_pretty(&stored)
        .map_err(|error| format!("cannot encode permission rules: {error}"))?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temporary, content)
        .map_err(|error| format!("cannot write permission rules: {error}"))?;
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("cannot save permission rules: {error}")
    })
}

fn save_stored_permission_rule(text: &str) -> Result<(), String> {
    let path = permission_rules_path();
    save_stored_permission_rule_at(&path, text)
}

fn merged_permission_policy_at(
    path: &std::path::Path,
    arguments: &[String],
) -> Result<a_rust_vm::permissions::PermissionPolicy, String> {
    let mut policy = build_permission_policy(arguments)?;
    for rule in load_stored_permission_policy(path)?.rules().to_vec() {
        policy.add_rule(rule);
    }
    Ok(policy)
}

fn merged_permission_policy(
    arguments: &[String],
) -> Result<a_rust_vm::permissions::PermissionPolicy, String> {
    merged_permission_policy_at(&permission_rules_path(), arguments)
}

fn permission_usage() {
    eprintln!("usage: arvm permission list|add <rule>|clear");
}

fn run_permission_command(arguments: &[String]) {
    let path = permission_rules_path();
    let Some(command) = arguments.first().map(String::as_str) else {
        permission_usage();
        std::process::exit(2);
    };
    match command {
        "list" => match load_stored_permission_policy(&path) {
            Ok(policy) => {
                if policy.rules().is_empty() {
                    println!("(no rules)");
                } else {
                    for rule in policy.rules() {
                        let effect = match rule.effect {
                            a_rust_vm::permissions::PermissionEffect::Allow => "allow",
                            a_rust_vm::permissions::PermissionEffect::Ask => "ask",
                            a_rust_vm::permissions::PermissionEffect::Deny => "deny",
                        };
                        if rule.target_prefix.is_empty() {
                            println!("{effect} {}", rule.tool);
                        } else {
                            println!("{effect} {}:{}", rule.tool, rule.target_prefix);
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!("[permission] {error}");
                std::process::exit(1);
            }
        },
        "add" => {
            if arguments.len() != 2 {
                permission_usage();
                std::process::exit(2);
            }
            if let Err(error) = save_stored_permission_rule(&arguments[1]) {
                eprintln!("[permission] {error}");
                std::process::exit(1);
            }
            println!("saved rule");
        }
        "clear" => {
            if arguments.len() != 1 {
                permission_usage();
                std::process::exit(2);
            }
            match std::fs::remove_file(&path) {
                Ok(()) => println!("cleared rules"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    println!("(no rules)")
                }
                Err(error) => {
                    eprintln!("[permission] cannot clear rules: {error}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            permission_usage();
            std::process::exit(2);
        }
    }
}

fn skill_usage() {
    eprintln!("usage: arvm skill list|show <name>|load <name>");
}

fn skill_roots_for(
    project_root: &std::path::Path,
    user_root: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    vec![project_root.join("skills"), user_root.join("skills")]
}

fn skill_roots() -> Vec<std::path::PathBuf> {
    skill_roots_for(&working_directory(), &user_config_root())
}

fn run_skill_command(arguments: &[String]) {
    let roots = skill_roots();
    let Some(command) = arguments.first().map(String::as_str) else {
        skill_usage();
        std::process::exit(2);
    };
    match command {
        "list" => {
            if arguments.len() != 1 {
                skill_usage();
                std::process::exit(2);
            }
            match a_rust_vm::skills::discover_skills(&roots) {
                Ok(skills) => {
                    if skills.is_empty() {
                        println!("(no skills)");
                    } else {
                        for skill in skills {
                            println!("{}\t{}", skill.name, skill.description);
                        }
                    }
                }
                Err(error) => {
                    eprintln!("[skill] {error}");
                    std::process::exit(1);
                }
            }
        }
        "show" | "load" => {
            if arguments.len() != 2 {
                skill_usage();
                std::process::exit(2);
            }
            match a_rust_vm::skills::load_skill(&roots, &arguments[1]) {
                Ok(skill) => {
                    if command == "show" {
                        println!("{}\t{}", skill.name, skill.description);
                    } else {
                        println!("{}", skill.scoped_instructions());
                    }
                }
                Err(error) => {
                    eprintln!("[skill] {error}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            skill_usage();
            std::process::exit(2);
        }
    }
}

fn run_session_command(arguments: &[String]) {
    let store = SessionStore::new(session_directory());
    let Some(command) = arguments.first().map(String::as_str) else {
        session_usage();
        std::process::exit(2);
    };
    match command {
        "list" => match store.list() {
            Ok(ids) => {
                if ids.is_empty() {
                    println!("(no sessions)");
                } else {
                    for id in ids {
                        println!("{id}");
                    }
                }
            }
            Err(error) => {
                eprintln!("[session] cannot list: {error}");
                std::process::exit(1);
            }
        },
        "show" => {
            let Some(id) = arguments.get(1) else {
                session_usage();
                std::process::exit(2);
            };
            match store.load(id) {
                Ok(session) => {
                    if session.messages.is_empty() {
                        println!("(empty session {id})");
                    }
                    for message in &session.messages {
                        println!("[{}] {}", message.role, message.content);
                    }
                }
                Err(error) => {
                    eprintln!("[session] {error}");
                    std::process::exit(1);
                }
            }
        }
        "save" => {
            if arguments.len() < 4 {
                session_usage();
                std::process::exit(2);
            }
            let id = &arguments[1];
            let role = &arguments[2];
            let content = &arguments[3];
            let mut session = match store.load(id) {
                Ok(existing) => existing,
                Err(error) if error.is_not_found() => match Session::new(id.clone()) {
                    Ok(created) => created,
                    Err(error) => {
                        eprintln!("[session] {error}");
                        std::process::exit(1);
                    }
                },
                Err(error) => {
                    eprintln!("[session] cannot resume {id}: {error}");
                    std::process::exit(1);
                }
            };
            session.push(role.clone(), content.clone());
            if let Err(error) = store.save(&session) {
                eprintln!("[session] cannot save: {error}");
                std::process::exit(1);
            }
            println!("saved {id} ({} messages)", session.messages.len());
        }
        "export" => {
            let (id, json) = match parse_session_export_args(&arguments[1..]) {
                Ok(parsed) => parsed,
                Err(error) => {
                    eprintln!("[session] {error}");
                    std::process::exit(2);
                }
            };
            match export_session(&store, &id, json) {
                Ok(output) => println!("{output}"),
                Err(error) => {
                    eprintln!("[session] {error}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            session_usage();
            std::process::exit(2);
        }
    }
}

fn run_doctor(arguments: &[String]) {
    if arguments.iter().any(|argument| argument != "--json") || arguments.len() > 1 {
        eprintln!("usage: arvm doctor [--json]");
        std::process::exit(2);
    }
    let info = a_rust_vm::protocol::SystemInfo::current();
    if arguments
        .first()
        .is_some_and(|argument| argument == "--json")
    {
        println!(
            "{}",
            serde_json::to_string_pretty(&info).expect("system info is serializable")
        );
        return;
    }
    println!(
        "{} {} · API {}",
        info.product, info.version, info.api_version
    );
    println!("workspace control plane: available (in-memory)");
    println!("bytecode executor: available");
    println!("Wasm/WASI executor: not installed");
    println!("native sandbox executor: not installed");
}

fn run_program_command(command: &str, arguments: &[String]) {
    let json = arguments.iter().any(|argument| argument == "--json");
    let paths = arguments
        .iter()
        .filter(|argument| argument.as_str() != "--json")
        .collect::<Vec<_>>();
    if paths.len() != 1 || (command != "run" && json) {
        eprintln!(
            "usage: arvm {command} <file|->{}",
            if command == "run" { " [--json]" } else { "" }
        );
        std::process::exit(2);
    }
    let path = paths[0];
    let source = if path.as_str() == "-" {
        let mut source = String::new();
        io::stdin()
            .read_to_string(&mut source)
            .unwrap_or_else(|error| {
                eprintln!("cannot read program from stdin: {error}");
                std::process::exit(2);
            });
        source
    } else {
        std::fs::read_to_string(path).unwrap_or_else(|error| {
            eprintln!("cannot read program '{path}': {error}");
            std::process::exit(2);
        })
    };
    let program = source.parse::<Program>().unwrap_or_else(|error| {
        eprintln!("invalid program: {error}");
        std::process::exit(2);
    });
    match command {
        "check" => println!(
            "valid: {} instructions, max stack depth {}",
            program.instructions().len(),
            program.max_stack_depth()
        ),
        "disassemble" => println!("{}", program.disassemble()),
        "trace" => {
            let entries = Vm::new().trace(&program).unwrap_or_else(|error| {
                eprintln!("VM error: {error:?}");
                std::process::exit(1);
            });
            for entry in entries {
                match entry.result {
                    Some(result) => println!(
                        "ip={} instruction=HALT result={result} stack={:?}",
                        entry.instruction_pointer, entry.stack
                    ),
                    None => println!(
                        "ip={} instruction={} stack={:?}",
                        entry.instruction_pointer, entry.instruction, entry.stack
                    ),
                }
            }
        }
        "run" => {
            let result = Vm::new().run_program(&program).unwrap_or_else(|error| {
                eprintln!("VM error: {error:?}");
                std::process::exit(1);
            });
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "result": result,
                        "instructions": program.instructions().len(),
                        "max_stack_depth": program.max_stack_depth()
                    })
                );
            } else {
                println!("{result}");
            }
        }
        _ => unreachable!("command validated by caller"),
    }
}

fn run_vm_demo() {
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

fn run_live_agent(user_arguments: &[String]) {
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
    let registry =
        coding_tool_registry_with_directories(&working_directory, &additional_directories)
            .unwrap_or_else(|error| {
                eprintln!("[workspace] {error}");
                std::process::exit(2);
            });
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
    let policy = match merged_permission_policy(user_arguments) {
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

fn extract_permission_rule_texts(arguments: &[String]) -> Vec<String> {
    arguments
        .windows(2)
        .filter(|window| window[0] == "--rule")
        .map(|window| window[1].clone())
        .collect()
}

fn build_permission_policy(
    arguments: &[String],
) -> Result<a_rust_vm::permissions::PermissionPolicy, String> {
    let mut policy = a_rust_vm::permissions::PermissionPolicy::new();
    for text in extract_permission_rule_texts(arguments) {
        let rule = a_rust_vm::permissions::parse_permission_rule(&text)
            .map_err(|error| format!("invalid --rule '{text}': {error}"))?;
        policy.add_rule(rule);
    }
    Ok(policy)
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

fn extract_skill_names(arguments: &[String]) -> Vec<String> {
    arguments
        .windows(2)
        .filter(|window| window[0] == "--skill")
        .map(|window| window[1].clone())
        .collect()
}

fn load_requested_skills(root: &std::path::Path, arguments: &[String]) -> Vec<String> {
    let roots = skill_roots_for(root, &user_config_root());
    extract_skill_names(arguments)
        .iter()
        .map(|name| {
            a_rust_vm::skills::load_skill(&roots, name)
                .map(|skill| skill.scoped_instructions())
                .map_err(|error| format!("invalid --skill '{name}': {error}"))
        })
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| {
            eprintln!("[skill] {error}");
            std::process::exit(2);
        })
}

fn user_config_root() -> std::path::PathBuf {
    env::var_os("A_RVM_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from))
        .or_else(|| {
            env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".config/a-rust-vm"))
        })
        .unwrap_or_else(std::env::temp_dir)
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
    let working_directory = env::current_dir().unwrap_or_else(|error| {
        eprintln!("failed to resolve model runner directory: {error}");
        std::process::exit(1);
    });
    let response = (0..MODEL_ATTEMPTS)
        .find_map(|attempt| {
            let mut command = Command::new(&binary);
            command
                .args(["run", "--pure", "--format", "json", "--dir"])
                .arg(&working_directory)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command.args(["--model", DEFAULT_MODEL]);
            command.arg(&instruction);

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

            match parse_bridge_response(&collect_runner_text(&output.stdout)) {
                Ok(response) => Some(response),
                Err(error)
                    if error == "model response was empty" && attempt + 1 < MODEL_ATTEMPTS =>
                {
                    eprintln!("local model returned no content; retrying");
                    None
                }
                Err(error) => {
                    eprintln!("local model returned an invalid A/RVM response: {error}");
                    std::process::exit(1);
                }
            }
        })
        .unwrap_or_else(|| {
            eprintln!("local model returned no content after {MODEL_ATTEMPTS} attempts");
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

fn collect_runner_text(output: &[u8]) -> String {
    let mut text = String::new();
    for line in String::from_utf8_lossy(output).lines() {
        let Ok(event) = serde_json::from_str::<RunnerEvent>(line) else {
            continue;
        };
        if matches!(event.event_type.as_str(), "text" | "text_delta")
            && let Some(chunk) = event.text.or_else(|| event.part.and_then(|part| part.text))
        {
            text.push_str(&chunk);
        }
    }
    text
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

    if !trimmed.is_empty() {
        return Ok(BridgeResponse::Text {
            text: trimmed.to_owned(),
        });
    }

    Err("model response was empty".to_owned())
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

fn working_directory() -> std::path::PathBuf {
    env::current_dir().unwrap_or_else(|error| {
        eprintln!("failed to resolve the working directory: {error}");
        std::process::exit(2);
    })
}

#[cfg(test)]
mod tests {
    use super::{
        BridgeResponse, check_artifact_byte_len, collect_runner_text, export_session,
        extract_skill_names, extract_workspace_directories, format_session_json,
        format_session_text, parse_artifact_import_arguments, parse_artifact_kind,
        parse_bridge_response, parse_session_export_args, skill_roots_for,
        workspace_system_prompt_with_skills,
    };

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
            super::extract_permission_rule_texts(&arguments),
            vec![
                "allow write_file:notes.txt".to_owned(),
                "deny run_command:rm".to_owned(),
            ]
        );
        assert_eq!(
            super::parse_one_shot_args(&arguments),
            Some(("write notes".to_owned(), false))
        );
        assert!(super::extract_permission_rule_texts(&["hi".to_owned()]).is_empty());
    }

    #[test]
    fn permission_rule_flags_build_a_policy_or_fail_closed() {
        let arguments = [
            "--rule".to_owned(),
            "deny run_command:rm".to_owned(),
            "hi".to_owned(),
        ];
        let policy = super::build_permission_policy(&arguments).unwrap();
        assert_eq!(
            policy.decide("run_command", "rm -rf /tmp"),
            a_rust_vm::permissions::PermissionEffect::Deny
        );
        assert!(
            super::build_permission_policy(&["--rule".to_owned(), "permit x".to_owned()]).is_err()
        );
    }

    #[test]
    fn durable_permission_rules_round_trip_missing_and_invalid() {
        let path = std::env::temp_dir().join(format!(
            "a-rvm-rules-{}-{}.json",
            std::process::id(),
            "round-trip"
        ));
        let _ = std::fs::remove_file(&path);
        let empty = super::load_stored_permission_policy(&path).unwrap();
        assert!(empty.rules().is_empty());
        super::save_stored_permission_rule_at(&path, "deny run_command:rm").unwrap();
        let stored = super::load_stored_permission_policy(&path).unwrap();
        assert_eq!(
            stored.decide("run_command", "rm -rf /tmp"),
            a_rust_vm::permissions::PermissionEffect::Deny
        );
        assert!(super::save_stored_permission_rule_at(&path, "permit write_file").is_err());
        std::fs::write(&path, b"not json").unwrap();
        assert!(super::load_stored_permission_policy(&path).is_err());
        std::fs::write(&path, b"[\"permit x\"]").unwrap();
        assert!(super::load_stored_permission_policy(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cli_rules_take_precedence_over_stored_rules() {
        let path = std::env::temp_dir().join(format!(
            "a-rvm-rules-{}-{}.json",
            std::process::id(),
            "precedence"
        ));
        let _ = std::fs::remove_file(&path);
        super::save_stored_permission_rule_at(&path, "deny write_file:notes.txt").unwrap();
        let arguments = ["--rule".to_owned(), "allow write_file:notes.txt".to_owned()];
        let policy = super::merged_permission_policy_at(&path, &arguments).unwrap();
        assert_eq!(
            policy.decide("write_file", "notes.txt"),
            a_rust_vm::permissions::PermissionEffect::Allow
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn session_export_formats_text_json_and_usage() {
        let mut session = a_rust_vm::session::Session::new("export-1").unwrap();
        session.push("user", "inspect this workspace");
        session.push("assistant", "I will inspect it.");

        assert_eq!(
            format_session_text(&session),
            "[user] inspect this workspace\n[assistant] I will inspect it."
        );
        let json = format_session_json(&session).unwrap();
        let decoded: a_rust_vm::session::Session = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, session);

        let empty = a_rust_vm::session::Session::new("empty-export").unwrap();
        assert_eq!(format_session_text(&empty), "(empty session empty-export)");

        let directory = std::env::temp_dir().join(format!(
            "a-rvm-session-export-{}-{}.json",
            std::process::id(),
            "helpers"
        ));
        let directory = directory.with_extension("");
        let _ = std::fs::remove_dir_all(&directory);
        let store = a_rust_vm::session::SessionStore::new(&directory);
        store.save(&session).unwrap();
        assert_eq!(
            export_session(&store, "export-1", false).unwrap(),
            format_session_text(&session)
        );
        assert_eq!(
            export_session(&store, "export-1", true).unwrap(),
            format_session_json(&session).unwrap()
        );
        assert!(export_session(&store, "missing", false).is_err());
        let _ = std::fs::remove_dir_all(&directory);

        assert_eq!(
            parse_session_export_args(&["export-1".to_owned()]).unwrap(),
            ("export-1".to_owned(), false)
        );
        assert_eq!(
            parse_session_export_args(&["export-1".to_owned(), "--json".to_owned()]).unwrap(),
            ("export-1".to_owned(), true)
        );
        assert!(parse_session_export_args(&[]).is_err());
        assert!(parse_session_export_args(&["a".to_owned(), "b".to_owned()]).is_err());
        assert!(parse_session_export_args(&["a".to_owned(), "--text".to_owned()]).is_err());
    }

    #[test]
    fn skill_roots_prefer_project_then_user_and_cli_loads() {
        let project =
            std::env::temp_dir().join(format!("a-rvm-skill-cli-project-{}", std::process::id()));
        let user =
            std::env::temp_dir().join(format!("a-rvm-skill-cli-user-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(project.join("skills").join("review")).unwrap();
        std::fs::write(
            project.join("skills").join("review").join("SKILL.md"),
            "---\nname: review\ndescription: Review changes\n---\nCheck diffs.\n",
        )
        .unwrap();

        let roots = skill_roots_for(&project, &user);
        assert_eq!(roots, vec![project.join("skills"), user.join("skills")]);
        let discovered = a_rust_vm::skills::discover_skills(&roots).unwrap();
        assert_eq!(discovered.len(), 1);
        let loaded = a_rust_vm::skills::load_skill(&roots, "review").unwrap();
        assert!(loaded.scoped_instructions().contains("Check diffs."));
        assert!(a_rust_vm::skills::load_skill(&roots, "missing").is_err());
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&user);
    }

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

    #[test]
    fn collects_text_and_text_delta_runner_events() {
        let output = br#"
            {"type":"text_delta","text":"{\"kind\":\"text\",\"text\":"}
            {"type":"text","part":{"text":"hello"}}
            {"type":"done"}
        "#;

        assert_eq!(
            collect_runner_text(output),
            "{\"kind\":\"text\",\"text\":hello"
        );
    }

    #[test]
    fn accepts_plain_runner_text_when_the_model_skips_the_json_envelope() {
        assert!(matches!(
            parse_bridge_response("4"),
            Ok(BridgeResponse::Text { text }) if text == "4"
        ));
    }

    #[test]
    fn parses_artifact_kinds() {
        assert!(matches!(
            parse_artifact_kind("source"),
            Ok(a_rust_vm::artifacts::ArtifactKind::Source)
        ));
        assert!(matches!(
            parse_artifact_kind("generated"),
            Ok(a_rust_vm::artifacts::ArtifactKind::Generated)
        ));
        assert!(parse_artifact_kind("blob").is_err());
    }

    #[test]
    fn enforces_artifact_size_limit() {
        assert!(check_artifact_byte_len(64 * 1024 * 1024).is_ok());
        assert!(check_artifact_byte_len(64 * 1024 * 1024 + 1).is_err());
    }

    #[test]
    fn parses_artifact_import_arguments_with_optional_parent() {
        let arguments = [
            "research".to_owned(),
            "budget".to_owned(),
            "budget-v1".to_owned(),
            "budget.xlsx".to_owned(),
            "application/pdf".to_owned(),
            "budget-v0".to_owned(),
        ];
        let parsed = parse_artifact_import_arguments(&arguments).unwrap();
        assert_eq!(parsed.workspace, "research");
        assert_eq!(parsed.parent_version_id, Some("budget-v0"));
        assert!(parse_artifact_import_arguments(&arguments[..2]).is_err());
    }
}
