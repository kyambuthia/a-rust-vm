//! Workspace tools with path containment and explicit permission metadata.

use std::cell::RefCell;
use std::fs;
use std::path::{Component, Path, PathBuf};
#[cfg(any(unix, windows))]
use std::process::Command;
use std::rc::Rc;

use crate::agent::{
    PermissionRequest, Tool, ToolArguments, ToolError, ToolRegistry, ToolSpec, ToolValue,
};

const MAX_FILE_BYTES: usize = 128 * 1024;
const MAX_SEARCH_RESULTS: usize = 100;
const MAX_COMMAND_OUTPUT_BYTES: usize = 16 * 1024;

/// Errors from workspace initialization or path resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceError {
    pub message: String,
}

impl WorkspaceError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WorkspaceError {}

/// An approved root directory for workspace operations.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, WorkspaceError> {
        let root = root.into();
        let root = fs::canonicalize(&root).map_err(|error| {
            WorkspaceError::new(format!(
                "cannot resolve workspace '{}': {error}",
                root.display()
            ))
        })?;
        if !root.is_dir() {
            return Err(WorkspaceError::new(format!(
                "workspace is not a directory: {}",
                root.display()
            )));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, requested: &str, allow_missing: bool) -> Result<PathBuf, ToolError> {
        let requested_path = Path::new(requested);
        if requested_path.is_absolute() {
            return Err(ToolError::new("workspace paths must be relative"));
        }
        if requested_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(ToolError::new("workspace paths cannot contain '..'"));
        }

        let candidate = self.root.join(requested_path);
        let resolved = if candidate.exists() {
            fs::canonicalize(&candidate)
                .map_err(|error| ToolError::new(format!("cannot resolve '{requested}': {error}")))?
        } else if allow_missing {
            let parent = candidate
                .parent()
                .ok_or_else(|| ToolError::new("workspace path has no parent"))?;
            let parent = fs::canonicalize(parent).map_err(|error| {
                ToolError::new(format!("cannot resolve parent of '{requested}': {error}"))
            })?;
            parent.join(
                candidate.file_name().ok_or_else(|| {
                    ToolError::new("workspace path must name a file or directory")
                })?,
            )
        } else {
            return Err(ToolError::new(format!("path does not exist: {requested}")));
        };

        if !resolved.starts_with(&self.root) {
            return Err(ToolError::new(format!(
                "path escapes the workspace: {requested}"
            )));
        }
        Ok(resolved)
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

type SharedWorkspace = Rc<RefCell<Workspace>>;

/// Register the safe read, write, search, and command tools for a workspace.
pub fn workspace_tool_registry(root: impl Into<PathBuf>) -> Result<ToolRegistry, ToolError> {
    let workspace = Workspace::new(root).map_err(|error| ToolError::new(error.to_string()))?;
    let workspace = Rc::new(RefCell::new(workspace));
    let mut registry = ToolRegistry::default();

    registry.register(ListFilesTool::new(workspace.clone()))?;
    registry.register(ReadFileTool::new(workspace.clone()))?;
    registry.register(SearchFilesTool::new(workspace.clone()))?;
    registry.register(WriteFileTool::new(workspace.clone()))?;
    registry.register(RunCommandTool::new(workspace))?;
    Ok(registry)
}

/// Register both VM and workspace tools for the coding-agent composition root.
pub fn coding_tool_registry(root: impl Into<PathBuf>) -> Result<ToolRegistry, ToolError> {
    let mut registry = crate::agent::vm_tool_registry()?;
    registry.merge(workspace_tool_registry(root)?)?;
    Ok(registry)
}

struct ListFilesTool {
    workspace: SharedWorkspace,
}

impl ListFilesTool {
    fn new(workspace: SharedWorkspace) -> Self {
        Self { workspace }
    }
}

impl Tool for ListFilesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "list_files",
            "List entries in a workspace directory.",
            r#"{"type":"object","properties":{"path":{"type":"string"}}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["path"])?;
        let requested = optional_text(arguments, "path").unwrap_or(".");
        let workspace = self.workspace.borrow();
        let directory = workspace.resolve(requested, false)?;
        if !directory.is_dir() {
            return Err(ToolError::new(format!("not a directory: {requested}")));
        }

        let mut entries = fs::read_dir(&directory)
            .map_err(|error| ToolError::new(format!("cannot list '{requested}': {error}")))?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                if file_type.is_symlink() {
                    return None;
                }
                if is_ignored_directory_name(entry.file_name().to_str()) {
                    return None;
                }
                let kind = if file_type.is_dir() { "dir" } else { "file" };
                Some(format!("{kind} {}", workspace.relative(&entry.path())))
            })
            .collect::<Vec<_>>();
        entries.sort();
        if entries.is_empty() {
            Ok("(empty)".to_owned())
        } else {
            Ok(entries.join("\n"))
        }
    }

    fn permission(&self, _arguments: &ToolArguments) -> Option<PermissionRequest> {
        None
    }
}

struct ReadFileTool {
    workspace: SharedWorkspace,
}

impl ReadFileTool {
    fn new(workspace: SharedWorkspace) -> Self {
        Self { workspace }
    }
}

impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "read_file",
            "Read a bounded text file from the workspace.",
            r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["path"])?;
        let requested = required_text(arguments, "path")?;
        let workspace = self.workspace.borrow();
        let path = workspace.resolve(requested, false)?;
        let metadata = fs::metadata(&path)
            .map_err(|error| ToolError::new(format!("cannot inspect '{requested}': {error}")))?;
        if !metadata.is_file() {
            return Err(ToolError::new(format!("not a file: {requested}")));
        }
        if metadata.len() > MAX_FILE_BYTES as u64 {
            return Err(ToolError::new(format!(
                "file exceeds the {} byte limit: {requested}",
                MAX_FILE_BYTES
            )));
        }
        let bytes = fs::read(&path)
            .map_err(|error| ToolError::new(format!("cannot read '{requested}': {error}")))?;
        String::from_utf8(bytes)
            .map_err(|_| ToolError::new(format!("file is not valid UTF-8: {requested}")))
    }
}

struct SearchFilesTool {
    workspace: SharedWorkspace,
}

impl SearchFilesTool {
    fn new(workspace: SharedWorkspace) -> Self {
        Self { workspace }
    }
}

impl Tool for SearchFilesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "search_files",
            "Search text files below a workspace path.",
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["pattern", "path"])?;
        let pattern = required_text(arguments, "pattern")?;
        let requested = optional_text(arguments, "path").unwrap_or(".");
        let workspace = self.workspace.borrow();
        let root = workspace.resolve(requested, false)?;
        let mut matches = Vec::new();
        search_directory(&workspace, &root, pattern, &mut matches)?;

        if matches.is_empty() {
            Ok("(no matches)".to_owned())
        } else {
            Ok(matches.join("\n"))
        }
    }
}

struct WriteFileTool {
    workspace: SharedWorkspace,
}

impl WriteFileTool {
    fn new(workspace: SharedWorkspace) -> Self {
        Self { workspace }
    }
}

impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "write_file",
            "Create or replace a UTF-8 text file in the workspace.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#,
        )
    }

    fn permission(&self, arguments: &ToolArguments) -> Option<PermissionRequest> {
        let path = optional_text(arguments, "path").unwrap_or("<invalid path>");
        Some(PermissionRequest {
            id: format!("write_file:{path}"),
            tool: "write_file".to_owned(),
            description: format!("write workspace file '{path}'"),
        })
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["path", "content"])?;
        let requested = required_text(arguments, "path")?;
        let content = required_text_allow_empty(arguments, "content")?;
        if content.len() > MAX_FILE_BYTES {
            return Err(ToolError::new(format!(
                "content exceeds the {} byte limit",
                MAX_FILE_BYTES
            )));
        }

        let workspace = self.workspace.borrow();
        let path = workspace.resolve(requested, true)?;
        if path.exists() && path.is_dir() {
            return Err(ToolError::new(format!(
                "cannot write a directory: {requested}"
            )));
        }
        fs::write(&path, content)
            .map_err(|error| ToolError::new(format!("cannot write '{requested}': {error}")))?;
        Ok(format!("wrote {} bytes to {requested}", content.len()))
    }
}

struct RunCommandTool {
    workspace: SharedWorkspace,
}

impl RunCommandTool {
    fn new(workspace: SharedWorkspace) -> Self {
        Self { workspace }
    }
}

impl Tool for RunCommandTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "run_command",
            "Run a shell command with the workspace as its working directory.",
            r#"{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}"#,
        )
    }

    fn permission(&self, arguments: &ToolArguments) -> Option<PermissionRequest> {
        let command = optional_text(arguments, "command").unwrap_or("<invalid command>");
        Some(PermissionRequest {
            id: format!("run_command:{command}"),
            tool: "run_command".to_owned(),
            description: format!("run command in workspace: {command}"),
        })
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["command"])?;
        let command = required_text(arguments, "command")?;
        reject_dangerous_command(command)?;
        let workspace = self.workspace.borrow();
        let output = shell_command(command, workspace.root())?;
        Ok(format_command_output(&output))
    }
}

#[cfg(any(unix, windows))]
fn shell_command(command: &str, directory: &Path) -> Result<std::process::Output, ToolError> {
    #[cfg(unix)]
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(directory)
        .output();
    #[cfg(windows)]
    let output = Command::new("cmd")
        .args(["/C", command])
        .current_dir(directory)
        .output();

    output.map_err(|error| ToolError::new(format!("failed to run command: {error}")))
}

#[cfg(not(any(unix, windows)))]
fn shell_command(_command: &str, _directory: &Path) -> Result<std::process::Output, ToolError> {
    Err(ToolError::new(
        "command execution is unavailable on this target",
    ))
}

fn reject_dangerous_command(command: &str) -> Result<(), ToolError> {
    if command
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<String>()
        .contains(":(){:|:&};:")
    {
        return Err(ToolError::new(format!(
            "refusing dangerous command '{command}'"
        )));
    }
    let normalized = command.to_ascii_lowercase().replace([';', '&', '|'], " ");
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return Ok(());
    }
    let joined = format!(" {} ", tokens.join(" "));
    for binary in ["shutdown", "reboot", "halt", "poweroff", "mkfs"] {
        if tokens.iter().any(|token| {
            token == &binary || token.rsplit('/').next().is_some_and(|name| name == binary)
        }) {
            return Err(ToolError::new(format!(
                "refusing dangerous command '{command}'"
            )));
        }
    }
    if tokens.iter().any(|token| *token == "dd") && joined.contains(" of=/dev/") {
        return Err(ToolError::new(format!(
            "refusing dangerous command '{command}'"
        )));
    }
    let recursive = tokens.iter().any(|token| {
        token.starts_with("-")
            && token.contains('r')
            && token.chars().skip(1).all(|flag| "rfRxivfp".contains(flag))
    });
    let removes_root = tokens.iter().any(|token| *token == "rm")
        && recursive
        && ["/", "/*", "/.", "~", "~/*", "$home", "$home/*"]
            .iter()
            .any(|target| joined.contains(&format!(" {target} ")));
    if removes_root {
        return Err(ToolError::new(format!(
            "refusing dangerous command '{command}'"
        )));
    }
    Ok(())
}

fn format_command_output(output: &std::process::Output) -> String {
    let mut content = String::new();
    if !output.stdout.is_empty() {
        content.push_str("stdout:\n");
        content.push_str(&bounded_text(&output.stdout));
    }
    if !output.stderr.is_empty() {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str("stderr:\n");
        content.push_str(&bounded_text(&output.stderr));
    }
    if content.is_empty() {
        content.push_str("(no output)\n");
    }
    content.push_str(&format!("exit_code={}", output.status.code().unwrap_or(-1)));
    content
}

fn bounded_text(bytes: &[u8]) -> String {
    let truncated = bytes.len() > MAX_COMMAND_OUTPUT_BYTES;
    let end = bytes.len().min(MAX_COMMAND_OUTPUT_BYTES);
    let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if truncated {
        text.push_str("\n[output truncated]");
    }
    text
}

fn search_directory(
    workspace: &Workspace,
    directory: &Path,
    pattern: &str,
    matches: &mut Vec<String>,
) -> Result<(), ToolError> {
    if matches.len() >= MAX_SEARCH_RESULTS {
        return Ok(());
    }

    let entries = fs::read_dir(directory).map_err(|error| {
        ToolError::new(format!("cannot search '{}': {error}", directory.display()))
    })?;
    for entry in entries.filter_map(Result::ok) {
        if matches.len() >= MAX_SEARCH_RESULTS {
            break;
        }
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if is_ignored_directory_name(entry.file_name().to_str()) {
                continue;
            }
            search_directory(workspace, &path, pattern, matches)?;
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) if metadata.len() <= MAX_FILE_BYTES as u64 => metadata,
            _ => continue,
        };
        if !metadata.is_file() {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let Ok(contents) = String::from_utf8(bytes) else {
            continue;
        };
        for (line_number, line) in contents.lines().enumerate() {
            if line.contains(pattern) {
                matches.push(format!(
                    "{}:{}:{}",
                    workspace.relative(&path),
                    line_number + 1,
                    line
                ));
                if matches.len() >= MAX_SEARCH_RESULTS {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn required_text<'a>(arguments: &'a ToolArguments, name: &str) -> Result<&'a str, ToolError> {
    match arguments.get(name) {
        Some(ToolValue::Text(value)) if !value.trim().is_empty() => Ok(value),
        Some(_) => Err(ToolError::new(format!(
            "argument '{name}' must be non-empty text"
        ))),
        None => Err(ToolError::new(format!("missing required argument: {name}"))),
    }
}

fn required_text_allow_empty<'a>(
    arguments: &'a ToolArguments,
    name: &str,
) -> Result<&'a str, ToolError> {
    match arguments.get(name) {
        Some(ToolValue::Text(value)) => Ok(value),
        Some(_) => Err(ToolError::new(format!("argument '{name}' must be text"))),
        None => Err(ToolError::new(format!("missing required argument: {name}"))),
    }
}

fn optional_text<'a>(arguments: &'a ToolArguments, name: &str) -> Option<&'a str> {
    match arguments.get(name) {
        Some(ToolValue::Text(value)) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

fn ensure_arguments(arguments: &ToolArguments, allowed: &[&str]) -> Result<(), ToolError> {
    if let Some(name) = arguments
        .keys()
        .find(|name| !allowed.iter().any(|allowed_name| allowed_name == name))
    {
        return Err(ToolError::new(format!("unexpected argument: {name}")));
    }
    Ok(())
}

fn is_ignored_directory_name(name: Option<&str>) -> bool {
    matches!(name, Some(".git" | "target" | ".arvm"))
}

#[cfg(test)]
mod tests {
    use super::{Workspace, coding_tool_registry, workspace_tool_registry};
    use crate::agent::{ToolArguments, ToolCall, ToolValue};

    fn test_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("a-rust-vm-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn rejects_paths_outside_the_workspace() {
        let root = test_root("path");
        let workspace = Workspace::new(&root).unwrap();

        let error = workspace.resolve("../outside", false).unwrap_err();

        assert_eq!(error.to_string(), "workspace paths cannot contain '..'");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_tools_read_search_and_write_files() {
        let root = test_root("tools");
        std::fs::write(root.join("notes.txt"), "hello\nneedle here\n").unwrap();
        let mut registry = workspace_tool_registry(&root).unwrap();

        let read = registry.execute(&ToolCall::new(
            "read-1",
            "read_file",
            [("path".to_owned(), ToolValue::Text("notes.txt".to_owned()))]
                .into_iter()
                .collect(),
        ));
        assert_eq!(read.content, "hello\nneedle here\n");
        assert!(!read.is_error);

        let search = registry.execute(&ToolCall::new(
            "search-1",
            "search_files",
            [("pattern".to_owned(), ToolValue::Text("needle".to_owned()))]
                .into_iter()
                .collect(),
        ));
        assert_eq!(search.content, "notes.txt:2:needle here");

        let write = registry.execute(&ToolCall::new(
            "write-1",
            "write_file",
            [
                ("path".to_owned(), ToolValue::Text("new.txt".to_owned())),
                ("content".to_owned(), ToolValue::Text("created".to_owned())),
            ]
            .into_iter()
            .collect::<ToolArguments>(),
        ));
        assert_eq!(write.content, "wrote 7 bytes to new.txt");
        assert!(!write.is_error);
        assert_eq!(
            std::fs::read_to_string(root.join("new.txt")).unwrap(),
            "created"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_and_command_tools_require_approval_metadata() {
        let root = test_root("permission");
        let registry = workspace_tool_registry(&root).unwrap();
        let call = ToolCall::new(
            "write-1",
            "write_file",
            [
                ("path".to_owned(), ToolValue::Text("new.txt".to_owned())),
                ("content".to_owned(), ToolValue::Text("created".to_owned())),
            ]
            .into_iter()
            .collect(),
        );
        let request = registry.permission_request(&call).unwrap();
        assert_eq!(request.id, "write-1:write_file:new.txt");
        assert_eq!(request.tool, "write_file");
        assert!(request.description.contains("new.txt"));

        let coding = coding_tool_registry(&root).unwrap();
        assert_eq!(coding.specs().len(), 12);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn command_tool_captures_bounded_output_and_exit_code() {
        let root = test_root("command");
        let mut registry = workspace_tool_registry(&root).unwrap();
        let call = ToolCall::new(
            "command-1",
            "run_command",
            [(
                "command".to_owned(),
                ToolValue::Text("printf hello".to_owned()),
            )]
            .into_iter()
            .collect(),
        );

        let result = registry.execute(&call);

        assert!(!result.is_error);
        assert!(result.content.contains("stdout:\nhello"));
        assert!(result.content.contains("exit_code=0"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dangerous_commands_fail_closed_before_execution() {
        let root = test_root("dangerous");
        let mut registry = workspace_tool_registry(&root).unwrap();
        for command in [
            "rm -rf /",
            "sudo rm -Rf /*",
            "mkfs -t ext4 /dev/sda1",
            "dd if=/dev/zero of=/dev/sda",
            "shutdown now",
            ":(){ :|:& };:",
        ] {
            let result = registry.execute(&ToolCall::new(
                "blocked",
                "run_command",
                [("command".to_owned(), ToolValue::Text(command.to_owned()))]
                    .into_iter()
                    .collect(),
            ));
            assert!(result.is_error, "command should be refused: {command}");
            assert!(
                result.content.contains("refusing dangerous command"),
                "unexpected content: {}",
                result.content
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
