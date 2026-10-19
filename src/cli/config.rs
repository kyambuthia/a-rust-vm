//! `arvm config`: the user and workspace configuration files.

use a_rust_vm::config::{ConfigSource, LoadedConfig};

use super::paths::{user_config_root, working_directory};

/// Load both configuration scopes for this run, or exit with the error.
pub(crate) fn load_config_or_exit() -> LoadedConfig {
    LoadedConfig::load(&user_config_root(), &working_directory()).unwrap_or_else(|error| {
        eprintln!("[config] {error}");
        std::process::exit(2);
    })
}

fn config_usage() {
    eprintln!("usage: arvm config show [--json]");
}

pub(crate) fn run_config_command(arguments: &[String]) {
    let json = match arguments {
        [command] if command == "show" => false,
        [command, flag] if command == "show" && flag == "--json" => true,
        _ => {
            config_usage();
            std::process::exit(2);
        }
    };
    let loaded = load_config_or_exit();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&config_json(&loaded))
                .expect("configuration is serializable")
        );
    } else {
        print!("{}", describe_config(&loaded));
    }
}

fn config_json(loaded: &LoadedConfig) -> serde_json::Value {
    let sources = loaded
        .sources()
        .into_iter()
        .map(|source| {
            serde_json::json!({
                "scope": source.scope,
                "path": source.path,
                "exists": source.config.is_some(),
                "config": source.config,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "sources": sources })
}

fn describe_config(loaded: &LoadedConfig) -> String {
    loaded.sources().into_iter().map(describe_source).collect()
}

fn describe_source(source: &ConfigSource) -> String {
    let mut text = format!("{} {}", source.scope.label(), source.path.display());
    let Some(config) = &source.config else {
        text.push_str(" (not found)\n");
        return text;
    };
    text.push('\n');
    if config.rules.is_empty()
        && config.mcp_servers.is_empty()
        && config.env_passthrough.is_empty()
        && config.model.is_none()
    {
        text.push_str("  (empty)\n");
    }
    if let Some(model) = &config.model {
        let fields = serde_json::to_value(model).expect("model settings are serializable");
        let summary = fields
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(key, value)| match value.as_str() {
                        Some(text) => format!("{key}={text}"),
                        None => format!("{key}={value}"),
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        text.push_str(&format!("  model {summary}\n"));
    }
    for rule in &config.rules {
        text.push_str(&format!("  rule {rule}\n"));
    }
    if !config.env_passthrough.is_empty() {
        text.push_str(&format!(
            "  env passthrough {}\n",
            config.env_passthrough.join(" ")
        ));
    }
    for (name, server) in &config.mcp_servers {
        let mut command = server.command.clone();
        for argument in &server.args {
            command.push(' ');
            command.push_str(argument);
        }
        text.push_str(&format!("  mcp {name}: {command}\n"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use a_rust_vm::config::{ArvmConfig, ConfigScope};

    #[test]
    fn describes_missing_empty_and_populated_sources() {
        let workspace = ConfigSource {
            scope: ConfigScope::Workspace,
            path: "/w/.arvm/config.json".into(),
            config: None,
        };
        let user = ConfigSource {
            scope: ConfigScope::User,
            path: "/u/config.json".into(),
            config: Some(
                ArvmConfig::parse(
                    br#"{"rules":["deny run_command:rm"],
                         "mcpServers":{"files":{"command":"mcp-files","args":["--root","."]}}}"#,
                    ConfigScope::User,
                )
                .unwrap(),
            ),
        };
        let loaded = LoadedConfig { workspace, user };
        assert_eq!(
            describe_config(&loaded),
            "workspace /w/.arvm/config.json (not found)\n\
             user /u/config.json\n  rule deny run_command:rm\n  mcp files: mcp-files --root .\n"
        );
        let json = config_json(&loaded);
        assert_eq!(json["sources"][0]["scope"], "workspace");
        assert_eq!(json["sources"][0]["exists"], false);
        assert_eq!(
            json["sources"][1]["config"]["mcpServers"]["files"]["command"],
            "mcp-files"
        );

        let empty = ConfigSource {
            scope: ConfigScope::User,
            path: "/u/config.json".into(),
            config: Some(ArvmConfig::default()),
        };
        assert_eq!(describe_source(&empty), "user /u/config.json\n  (empty)\n");
        let with_model = ConfigSource {
            scope: ConfigScope::User,
            path: "/u/config.json".into(),
            config: Some(
                ArvmConfig::parse(
                    br#"{"model":{"name":"m/x","maxTokens":2048}}"#,
                    ConfigScope::User,
                )
                .unwrap(),
            ),
        };
        assert_eq!(
            describe_source(&with_model),
            "user /u/config.json\n  model maxTokens=2048 name=m/x\n"
        );
    }
}
