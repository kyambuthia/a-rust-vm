use std::env;

mod cli;

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
        cli::doctor::run_doctor(&arguments[1..]);
        return;
    }
    if command == Some("workspace") {
        cli::workspace::run_workspace_command(&arguments[1..]);
        return;
    }
    if command == Some("session") {
        cli::session::run_session_command(&arguments[1..]);
        return;
    }
    if command == Some("permission") {
        cli::permission::run_permission_command(&arguments[1..]);
        return;
    }
    if command == Some("mcp") {
        cli::mcp::run_mcp_command(&arguments[1..]);
        return;
    }
    if command == Some("config") {
        cli::config::run_config_command(&arguments[1..]);
        return;
    }
    if command == Some("skill") {
        cli::skill::run_skill_command(&arguments[1..]);
        return;
    }
    if matches!(command, Some("run" | "check" | "disassemble" | "trace")) {
        cli::program::run_program_command(command.expect("matched above"), &arguments[1..]);
        return;
    }
    if command == Some("demo") {
        cli::demo::run_vm_demo();
        return;
    }
    if command == Some("agent-demo") {
        cli::demo::run_agent_demo();
        return;
    }
    if command == Some("runtime-demo") {
        cli::demo::run_runtime_demo();
        return;
    }
    if command == Some("guest-demo") {
        cli::demo::run_guest_demo();
        return;
    }
    if command == Some("coding-demo") {
        cli::demo::run_coding_demo();
        return;
    }
    if matches!(command, Some("agent" | "ask")) {
        cli::agent::run_live_agent(&arguments[1..]);
        return;
    }
    if command == Some("model-bridge") {
        cli::bridge::run_model_bridge();
        return;
    }
    if command == Some("serve") {
        a_rust_vm::server::run(cli::paths::working_directory());
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
Product commands:\n  agent | ask              Interactive coding agent REPL\n  ask [--json] [--auto] [--rule <rule>]... [--skill <name>]... [--workspace-dir <path>]... <prompt..>  One-shot prompt (exit after one turn)\n  serve                   Host the browser and agent API\n  session <command>       Manage local agent sessions (list/show/save/export)\n  skill <command>         Discover and load project skills (list/show/load)\n  permission <command>    Manage durable permission rules (list/add/clear)\n  config show [--json]    Show user and workspace configuration (.arvm/config.json)\n  mcp <command>           Configured MCP servers (list/tools/trust/untrust)\n  workspace <command>     Manage durable owned workspaces\n  doctor [--json]         Report platform capabilities and readiness\n  version                 Print version information\n  help                    Show this help\n\n\
Assembly is line-oriented. Instructions: PUSH <i32>, ADD, SUB, MUL, DIV, DUP, POP, SWAP, OVER, NEG, NOT, EQ, LT, JMP <index>, JZ <index>, HALT.\n\
Use '-' to read a program from standard input; '#' starts a comment."
    );
}
