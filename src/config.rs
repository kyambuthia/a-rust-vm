//! `arvm` configuration files: permission rules and MCP servers.
//!
//! Two scopes exist. The user config belongs to the person running `arvm` and
//! may declare any rule. The workspace config ships with a project, so it may
//! only tighten permissions: `allow` rules there are rejected.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::mcp::{McpClient, McpConfig};
use crate::permissions::{PermissionEffect, PermissionRule, parse_permission_rule};

/// File name of both the user and the workspace configuration.
pub const CONFIG_FILE_NAME: &str = "config.json";
/// Directory under the workspace root that holds the workspace configuration.
pub const WORKSPACE_CONFIG_DIRECTORY: &str = ".arvm";
/// Largest configuration file read, so a stray large file fails fast.
pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;

/// Where a configuration file came from, which decides what it may declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigScope {
    User,
    Workspace,
}

impl ConfigScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Workspace => "workspace",
        }
    }
}

/// The contents of one configuration file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ArvmConfig {
    /// Permission rules in `arvm permission add` syntax, first match wins.
    #[serde(default)]
    pub rules: Vec<String>,
    /// MCP servers by name.
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub message: String,
}

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

impl ArvmConfig {
    /// Parse and validate one file's contents for the given scope.
    pub fn parse(bytes: &[u8], scope: ConfigScope) -> Result<Self, ConfigError> {
        let config: Self = serde_json::from_slice(bytes)
            .map_err(|error| ConfigError::new(format!("invalid configuration: {error}")))?;
        config.validate(scope)?;
        Ok(config)
    }

    /// Parsed permission rules, in file order.
    pub fn permission_rules(&self) -> Result<Vec<PermissionRule>, ConfigError> {
        self.rules
            .iter()
            .map(|text| {
                parse_permission_rule(text)
                    .map_err(|error| ConfigError::new(format!("invalid rule '{text}': {error}")))
            })
            .collect()
    }

    fn validate(&self, scope: ConfigScope) -> Result<(), ConfigError> {
        for (text, rule) in self.rules.iter().zip(self.permission_rules()?) {
            if scope == ConfigScope::Workspace && rule.effect == PermissionEffect::Allow {
                return Err(ConfigError::new(format!(
                    "cannot grant '{text}' from a workspace; \
                     put allow rules in the user configuration or pass --rule"
                )));
            }
        }
        for (name, server) in &self.mcp_servers {
            if !valid_server_name(name) {
                return Err(ConfigError::new(format!(
                    "invalid MCP server name '{name}': use letters, digits, '-' or '_'"
                )));
            }
            McpClient::new(server.clone()).map_err(|error| {
                ConfigError::new(format!("invalid MCP server '{name}': {error}"))
            })?;
        }
        Ok(())
    }
}

fn valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

/// One configuration file location and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSource {
    pub scope: ConfigScope,
    pub path: PathBuf,
    /// `None` when the file does not exist.
    pub config: Option<ArvmConfig>,
}

impl ConfigSource {
    /// Read the file at `path`; a missing file is not an error.
    pub fn load(scope: ConfigScope, path: PathBuf) -> Result<Self, ConfigError> {
        let config = read_config_file(&path, scope)?;
        Ok(Self {
            scope,
            path,
            config,
        })
    }
}

fn read_config_file(path: &Path, scope: ConfigScope) -> Result<Option<ArvmConfig>, ConfigError> {
    let describe = |detail: String| {
        ConfigError::new(format!(
            "{} configuration {}: {detail}",
            scope.label(),
            path.display()
        ))
    };
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(describe(error.to_string())),
    };
    if !metadata.is_file() {
        return Err(describe("not a regular file".to_owned()));
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(describe(format!("larger than {MAX_CONFIG_BYTES} bytes")));
    }
    let bytes = std::fs::read(path).map_err(|error| describe(error.to_string()))?;
    ArvmConfig::parse(&bytes, scope)
        .map(Some)
        .map_err(|error| describe(error.message))
}

/// The workspace configuration path for a workspace root.
pub fn workspace_config_path(root: &Path) -> PathBuf {
    root.join(WORKSPACE_CONFIG_DIRECTORY).join(CONFIG_FILE_NAME)
}

/// Both configuration scopes for one `arvm` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    pub workspace: ConfigSource,
    pub user: ConfigSource,
}

impl LoadedConfig {
    /// Load `<user_config_dir>/config.json` and `<workspace>/.arvm/config.json`.
    pub fn load(user_config_dir: &Path, workspace_root: &Path) -> Result<Self, ConfigError> {
        Ok(Self {
            workspace: ConfigSource::load(
                ConfigScope::Workspace,
                workspace_config_path(workspace_root),
            )?,
            user: ConfigSource::load(ConfigScope::User, user_config_dir.join(CONFIG_FILE_NAME))?,
        })
    }

    /// Sources in precedence order: workspace before user, so a project can
    /// tighten a user's allow rule but never loosen it.
    pub fn sources(&self) -> [&ConfigSource; 2] {
        [&self.workspace, &self.user]
    }

    /// Rules from both scopes in precedence order.
    pub fn permission_rules(&self) -> Vec<PermissionRule> {
        self.sources()
            .into_iter()
            .filter_map(|source| source.config.as_ref())
            .flat_map(|config| {
                config
                    .permission_rules()
                    .expect("rules were validated when the file was loaded")
            })
            .collect()
    }
}

/// One MCP server declared in a configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfiguredServer {
    pub name: String,
    pub scope: ConfigScope,
    pub config: McpConfig,
}

/// Which configured MCP servers a run may start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpServerPlan {
    /// Servers to start, sorted by name.
    pub enabled: Vec<ConfiguredServer>,
    /// Workspace servers the user has not trusted; they are not started.
    pub untrusted: Vec<ConfiguredServer>,
}

impl LoadedConfig {
    /// Every declared server, workspace entries first.
    pub fn declared_servers(&self) -> Vec<ConfiguredServer> {
        self.sources()
            .into_iter()
            .filter_map(|source| source.config.as_ref().map(|config| (source.scope, config)))
            .flat_map(|(scope, config)| {
                config
                    .mcp_servers
                    .iter()
                    .map(move |(name, server)| ConfiguredServer {
                        name: name.clone(),
                        scope,
                        config: server.clone(),
                    })
            })
            .collect()
    }

    /// The server a name refers to: the workspace entry if there is one,
    /// otherwise the user entry.
    pub fn server(&self, name: &str) -> Option<ConfiguredServer> {
        self.declared_servers()
            .into_iter()
            .find(|server| server.name == name)
    }

    /// User servers are enabled. A workspace server ships with the project,
    /// so it is enabled only when `trusted` approves that exact entry; it
    /// then replaces a user server of the same name.
    pub fn mcp_server_plan(&self, trusted: impl Fn(&str, &McpConfig) -> bool) -> McpServerPlan {
        let mut enabled = BTreeMap::new();
        let mut untrusted = Vec::new();
        let declared = self.declared_servers();
        for server in declared.iter().filter(|s| s.scope == ConfigScope::User) {
            enabled.insert(server.name.clone(), server.clone());
        }
        for server in declared
            .into_iter()
            .filter(|s| s.scope == ConfigScope::Workspace)
        {
            if trusted(&server.name, &server.config) {
                enabled.insert(server.name.clone(), server);
            } else {
                untrusted.push(server);
            }
        }
        McpServerPlan {
            enabled: enabled.into_values().collect(),
            untrusted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("a-rvm-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn parses_rules_and_mcp_servers() {
        let config = ArvmConfig::parse(
            br#"{"rules":["deny run_command:rm"],
                 "mcpServers":{"files":{"command":"mcp-files","args":["--root","."]}}}"#,
            ConfigScope::User,
        )
        .unwrap();
        assert_eq!(config.rules, vec!["deny run_command:rm".to_owned()]);
        assert_eq!(config.mcp_servers["files"].command, "mcp-files");
        assert_eq!(config.mcp_servers["files"].args, vec!["--root", "."]);
        assert_eq!(config.permission_rules().unwrap().len(), 1);
        assert_eq!(
            ArvmConfig::parse(b"{}", ConfigScope::Workspace).unwrap(),
            ArvmConfig::default()
        );
    }

    #[test]
    fn rejects_unknown_fields_and_invalid_entries() {
        for (bytes, expected) in [
            (&br#"{"rule":["deny x"]}"#[..], "unknown field"),
            (br#"{"rules":["permit x"]}"#, "invalid rule 'permit x'"),
            (
                br#"{"mcpServers":{"a":{"command":" "}}}"#,
                "invalid MCP server 'a'",
            ),
            (
                br#"{"mcpServers":{"a":{"command":"x","env":["NOVALUE"]}}}"#,
                "NAME=value",
            ),
            (
                br#"{"mcpServers":{"a":{"command":"x","cmd":"y"}}}"#,
                "unknown field",
            ),
            (
                br#"{"mcpServers":{"a b":{"command":"x"}}}"#,
                "invalid MCP server name",
            ),
            (b"not json", "invalid configuration"),
        ] {
            let error = ArvmConfig::parse(bytes, ConfigScope::User).unwrap_err();
            assert!(
                error.message.contains(expected),
                "{} should mention {expected}",
                error.message
            );
        }
    }

    #[test]
    fn workspace_scope_cannot_grant_allow_rules() {
        let bytes = br#"{"rules":["deny run_command:rm","allow run_command"]}"#;
        assert!(ArvmConfig::parse(bytes, ConfigScope::User).is_ok());
        let error = ArvmConfig::parse(bytes, ConfigScope::Workspace).unwrap_err();
        assert!(
            error
                .message
                .contains("cannot grant 'allow run_command' from a workspace")
        );
        assert!(
            ArvmConfig::parse(br#"{"rules":["ask write_file"]}"#, ConfigScope::Workspace).is_ok()
        );
    }

    #[test]
    fn missing_files_are_empty_and_bad_files_name_their_path() {
        let user = temporary_directory("missing-user");
        let workspace = temporary_directory("missing-workspace");
        let loaded = LoadedConfig::load(&user, &workspace).unwrap();
        assert_eq!(loaded.user.config, None);
        assert_eq!(loaded.workspace.config, None);
        assert!(loaded.permission_rules().is_empty());

        std::fs::create_dir_all(workspace.join(".arvm")).unwrap();
        std::fs::write(workspace_config_path(&workspace), b"{\"rules\":[1]}").unwrap();
        let error = LoadedConfig::load(&user, &workspace).unwrap_err();
        assert!(error.message.starts_with("workspace configuration "));
        assert!(error.message.contains(".arvm"));

        std::fs::remove_file(workspace_config_path(&workspace)).unwrap();
        std::fs::create_dir(workspace_config_path(&workspace)).unwrap();
        let error = LoadedConfig::load(&user, &workspace).unwrap_err();
        assert!(error.message.contains("not a regular file"));

        let _ = std::fs::remove_dir_all(&user);
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn workspace_rules_take_precedence_over_user_rules() {
        let user = temporary_directory("precedence-user");
        let workspace = temporary_directory("precedence-workspace");
        std::fs::write(
            user.join(CONFIG_FILE_NAME),
            br#"{"rules":["allow run_command"]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(workspace.join(".arvm")).unwrap();
        std::fs::write(
            workspace_config_path(&workspace),
            br#"{"rules":["deny run_command:rm"]}"#,
        )
        .unwrap();
        let loaded = LoadedConfig::load(&user, &workspace).unwrap();
        let policy = crate::permissions::PermissionPolicy::with_rules(loaded.permission_rules());
        assert_eq!(
            policy.decide("run_command", "rm -rf build"),
            PermissionEffect::Deny
        );
        assert_eq!(
            policy.decide("run_command", "cargo test"),
            PermissionEffect::Allow
        );
        let _ = std::fs::remove_dir_all(&user);
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn workspace_servers_need_trust_and_then_override_user_servers() {
        let user = temporary_directory("plan-user");
        let workspace = temporary_directory("plan-workspace");
        std::fs::write(
            user.join(CONFIG_FILE_NAME),
            br#"{"mcpServers":{"files":{"command":"user-files"},"web":{"command":"web"}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(workspace.join(".arvm")).unwrap();
        std::fs::write(
            workspace_config_path(&workspace),
            br#"{"mcpServers":{"files":{"command":"repo-files"},"db":{"command":"db"}}}"#,
        )
        .unwrap();
        let loaded = LoadedConfig::load(&user, &workspace).unwrap();
        assert_eq!(loaded.server("files").unwrap().config.command, "repo-files");
        assert_eq!(loaded.server("web").unwrap().scope, ConfigScope::User);
        assert!(loaded.server("missing").is_none());

        let names = |servers: &[ConfiguredServer]| {
            servers
                .iter()
                .map(|server| format!("{}={}", server.name, server.config.command))
                .collect::<Vec<_>>()
        };
        let plan = loaded.mcp_server_plan(|_, _| false);
        assert_eq!(names(&plan.enabled), ["files=user-files", "web=web"]);
        assert_eq!(names(&plan.untrusted), ["db=db", "files=repo-files"]);

        let plan = loaded
            .mcp_server_plan(|name, config| name == "files" && config.command == "repo-files");
        assert_eq!(names(&plan.enabled), ["files=repo-files", "web=web"]);
        assert_eq!(names(&plan.untrusted), ["db=db"]);

        let _ = std::fs::remove_dir_all(&user);
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
