use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read as _, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const MAX_MCP_LINE_BYTES: usize = 64 * 1024;
pub const MAX_MCP_PAGES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub working_directory: Option<PathBuf>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpError {
    pub message: String,
}

impl McpError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for McpError {}

#[derive(Debug, Clone)]
pub struct McpClient {
    config: McpConfig,
}

impl McpClient {
    pub fn new(config: McpConfig) -> Result<Self, McpError> {
        validate_config(&config)?;
        Ok(Self { config })
    }

    pub fn config(&self) -> &McpConfig {
        &self.config
    }

    pub fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_MCP_PAGES {
            let page = self.list_page(cursor.as_deref())?;
            tools.extend(page.tools);
            match page.next_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => return Ok(tools),
            }
        }
        Err(McpError::new("mcp tools/list returned too many pages"))
    }

    pub fn call_tool(&self, name: &str, arguments: serde_json::Value) -> Result<String, McpError> {
        if name.trim().is_empty() {
            return Err(McpError::new("mcp tool name must not be empty"));
        }
        let arguments = match arguments {
            serde_json::Value::Null => serde_json::Value::Object(Default::default()),
            other => other,
        };
        let mut params = BTreeMap::new();
        params.insert(
            "name".to_owned(),
            serde_json::Value::String(name.to_owned()),
        );
        params.insert("arguments".to_owned(), arguments);
        let response = self.round_trip("tools/call", params)?;
        parse_call_result(&response)
    }

    fn list_page(&self, cursor: Option<&str>) -> Result<McpPage, McpError> {
        let mut params = BTreeMap::new();
        if let Some(cursor) = cursor {
            params.insert(
                "cursor".to_owned(),
                serde_json::Value::String(cursor.to_owned()),
            );
        }
        let response = self.round_trip("tools/list", params)?;
        parse_list_result(&response)
    }

    fn round_trip(
        &self,
        method: &str,
        params: BTreeMap<String, serde_json::Value>,
    ) -> Result<serde_json::Value, McpError> {
        let mut child = spawn_server(&self.config)?;
        let mut reader = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| McpError::new("mcp server stdout is unavailable"))?,
        );
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::new("mcp server stdin is unavailable"))?;
        let timeout = Duration::from_secs(self.config.timeout.unwrap_or(30).max(1));
        let deadline = std::time::Instant::now() + timeout;
        let mut next_id = 1;
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": next_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "a-rust-vm", "version": env!("CARGO_PKG_VERSION")},
            },
        });
        next_id += 1;
        write_line(&mut stdin, &initialize)?;
        let first = read_bounded_line(&mut reader)?;
        let first_value: serde_json::Value = serde_json::from_str(&first).map_err(|error| {
            McpError::new(format!("mcp initialize response is invalid: {error}"))
        })?;
        require_result(&first_value, "initialize")?;
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            return Err(McpError::new(format!("mcp {method} timed out")));
        }
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": next_id,
            "method": method,
            "params": params,
        });
        write_line(&mut stdin, &request)?;
        drop(stdin);
        let line = read_bounded_line(&mut reader)?;
        let value: serde_json::Value = serde_json::from_str(&line)
            .map_err(|error| McpError::new(format!("mcp {method} response is invalid: {error}")))?;
        if let Some(error) = value.get("error") {
            let _ = child.wait();
            return Err(McpError::new(format!(
                "mcp {method} failed: {}",
                compact_json(error)
            )));
        }
        let status = child
            .wait()
            .map_err(|error| McpError::new(format!("failed to finish mcp server: {error}")))?;
        if !status.success() {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let mut limited = pipe.by_ref().take(1024);
                let _ = limited.read_to_string(&mut stderr);
            }
            let detail = stderr.trim();
            if detail.is_empty() {
                return Err(McpError::new(format!("mcp server exited with {status}")));
            }
            return Err(McpError::new(format!(
                "mcp server exited with {status}: {detail}"
            )));
        }
        require_result(&value, method)?;
        Ok(value)
    }
}

struct McpPage {
    tools: Vec<McpTool>,
    next_cursor: Option<String>,
}

fn validate_config(config: &McpConfig) -> Result<(), McpError> {
    if config.command.trim().is_empty() {
        return Err(McpError::new("mcp command must not be empty"));
    }
    for entry in &config.env {
        match entry.split_once('=') {
            Some((name, _)) if !name.trim().is_empty() => {}
            _ => {
                return Err(McpError::new(format!(
                    "mcp env entry must be NAME=value: '{entry}'"
                )));
            }
        }
    }
    Ok(())
}

fn spawn_server(config: &McpConfig) -> Result<std::process::Child, McpError> {
    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = &config.working_directory {
        command.current_dir(directory);
    }
    for entry in &config.env {
        if let Some((name, value)) = entry.split_once('=') {
            command.env(name.trim(), value);
        }
    }
    command.spawn().map_err(|error| {
        McpError::new(format!(
            "failed to start mcp server '{}': {error}",
            config.command
        ))
    })
}

fn write_line(
    stdin: &mut std::process::ChildStdin,
    value: &serde_json::Value,
) -> Result<(), McpError> {
    let mut encoded = serde_json::to_vec(value)
        .map_err(|error| McpError::new(format!("failed to encode mcp request: {error}")))?;
    encoded.push(b'\n');
    stdin
        .write_all(&encoded)
        .map_err(|error| McpError::new(format!("failed to send mcp request: {error}")))
}

fn read_bounded_line(
    reader: &mut BufReader<std::process::ChildStdout>,
) -> Result<String, McpError> {
    let mut buffer = Vec::new();
    let limit = (MAX_MCP_LINE_BYTES + 2) as u64;
    let taken = reader
        .by_ref()
        .take(limit)
        .read_until(b'\n', &mut buffer)
        .map_err(|error| McpError::new(format!("failed to read mcp response: {error}")))?;
    if taken == 0 && buffer.is_empty() {
        return Err(McpError::new("mcp server emitted no response"));
    }
    if buffer.len() > MAX_MCP_LINE_BYTES + 1 {
        return Err(McpError::new("mcp response exceeds 64 KiB line limit"));
    }
    let text =
        String::from_utf8(buffer).map_err(|_| McpError::new("mcp response is not valid UTF-8"))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(McpError::new("mcp server emitted no response"));
    }
    if trimmed.len() > MAX_MCP_LINE_BYTES {
        return Err(McpError::new("mcp response exceeds 64 KiB line limit"));
    }
    Ok(trimmed.to_owned())
}

fn require_result(value: &serde_json::Value, method: &str) -> Result<(), McpError> {
    if value.get("error").is_some() {
        return Err(McpError::new(format!(
            "mcp {method} failed: {}",
            compact_json(&value["error"])
        )));
    }
    if value.get("result").is_none() {
        return Err(McpError::new(format!(
            "mcp {method} response is missing a result"
        )));
    }
    Ok(())
}

fn parse_list_result(value: &serde_json::Value) -> Result<McpPage, McpError> {
    let result = value
        .get("result")
        .ok_or_else(|| McpError::new("mcp tools/list response is missing a result"))?;
    let entries = result
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| McpError::new("mcp tools/list result must carry a tools array"))?;
    let mut tools = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if name.trim().is_empty() {
            return Err(McpError::new("mcp tool entry must have a non-empty name"));
        }
        tools.push(McpTool {
            name: name.to_owned(),
            description: entry
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            input_schema: entry
                .get("inputSchema")
                .cloned()
                .unwrap_or(serde_json::Value::Object(Default::default())),
        });
    }
    let next_cursor = result
        .get("nextCursor")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Ok(McpPage { tools, next_cursor })
}

fn parse_call_result(value: &serde_json::Value) -> Result<String, McpError> {
    let result = value
        .get("result")
        .ok_or_else(|| McpError::new("mcp tools/call response is missing a result"))?;
    if result.get("isError").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err(McpError::new(format!(
            "mcp tool call failed: {}",
            compact_json(result.get("content").unwrap_or(&serde_json::Value::Null))
        )));
    }
    let content = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| McpError::new("mcp tools/call result must carry a content array"))?;
    let mut parts = Vec::with_capacity(content.len());
    for item in content {
        let text = item
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| compact_json(item));
        parts.push(text);
    }
    Ok(parts.join("\n"))
}

fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{McpClient, McpConfig};

    fn shell_config(script: &str) -> McpConfig {
        McpConfig {
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            env: Vec::new(),
            working_directory: None,
            timeout: Some(10),
        }
    }

    fn client(script: &str) -> McpClient {
        McpClient::new(shell_config(script)).expect("config should validate")
    }

    #[test]
    fn rejects_empty_command_and_malformed_env() {
        let empty = McpConfig {
            command: "   ".to_owned(),
            args: Vec::new(),
            env: Vec::new(),
            working_directory: None,
            timeout: None,
        };
        assert!(McpClient::new(empty).is_err());
        let bad_env = McpConfig {
            command: "sh".to_owned(),
            args: Vec::new(),
            env: vec!["NO_EQUALS".to_owned()],
            working_directory: None,
            timeout: None,
        };
        assert!(McpClient::new(bad_env).is_err());
    }

    #[test]
    fn lists_tools_from_a_stdio_server() {
        let script = r#"read l1; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05"}}'; read l2; echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object"}}]}}'"#;
        let tools = client(script).list_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].description, "Echo input");
    }

    #[test]
    fn follows_cursor_pagination_across_spawned_servers() {
        let script = r#"read l1; echo '{"jsonrpc":"2.0","id":1,"result":{}}'; read l2; case "$l2" in *cursor-1*) echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"second"}]}}';; *) echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"first"}],"nextCursor":"cursor-1"}}';; esac"#;
        let tools = client(script).list_tools().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "first");
        assert_eq!(tools[1].name, "second");
    }

    #[test]
    fn calls_a_tool_and_joins_text_content() {
        let script = r#"read l1; echo '{"jsonrpc":"2.0","id":1,"result":{}}'; read l2; echo '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"hello"},{"type":"text","text":"world"}]}}'"#;
        let output = client(script)
            .call_tool("echo", serde_json::json!({"text": "hi"}))
            .unwrap();
        assert_eq!(output, "hello\nworld");
    }

    #[test]
    fn surfaces_tool_errors_and_rejects_empty_names() {
        let script = r#"read l1; echo '{"jsonrpc":"2.0","id":1,"result":{}}'; read l2; echo '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"bad input"}],"isError":true}}'"#;
        let client = client(script);
        assert!(client.call_tool("", serde_json::Value::Null).is_err());
        assert!(
            client
                .call_tool("echo", serde_json::Value::Null)
                .unwrap_err()
                .to_string()
                .contains("failed")
        );
    }

    #[test]
    fn fails_closed_on_malformed_and_oversized_output() {
        let malformed =
            r#"read l1; echo '{"jsonrpc":"2.0","id":1,"result":{}}'; read l2; echo 'not json'"#;
        assert!(
            client(malformed)
                .list_tools()
                .unwrap_err()
                .to_string()
                .contains("invalid")
        );
        let missing = McpClient::new(McpConfig {
            command: "definitely-not-an-mcp-server-binary".to_owned(),
            args: Vec::new(),
            env: Vec::new(),
            working_directory: None,
            timeout: Some(5),
        })
        .unwrap();
        assert!(missing.list_tools().is_err());
        let huge = r#"read l1; echo '{"jsonrpc":"2.0","id":1,"result":{}}'; read l2; head -c 70000 /dev/zero | tr '\0' 'a'; echo"#;
        assert!(
            client(huge)
                .list_tools()
                .unwrap_err()
                .to_string()
                .contains("64 KiB")
        );
    }
}
