//! Workspace tools with path containment and explicit permission metadata.

use std::cell::RefCell;
use std::fs;
#[cfg(any(unix, windows))]
use std::io::Read;
use std::path::{Component, Path, PathBuf};
#[cfg(any(unix, windows))]
use std::process::{Command, Stdio};
use std::rc::Rc;
#[cfg(any(unix, windows))]
use std::thread;
#[cfg(any(unix, windows))]
use std::time::{Duration, Instant};

use crate::agent::{
    PermissionRequest, Tool, ToolArguments, ToolError, ToolRegistry, ToolSpec, ensure_arguments,
    optional_flag, optional_positive, optional_text, required_text, required_text_allow_empty,
};

fn sanitize_permission_component(value: &str) -> String {
    value.replace([':', ' ', '\n', '\r', '\t'], "_")
}

const MAX_FILE_BYTES: usize = 128 * 1024;
const MAX_SEARCH_RESULTS: usize = 100;
/// Command output kept from each stream: the start and the end.
#[cfg(any(unix, windows))]
const COMMAND_OUTPUT_HEAD_BYTES: usize = 4 * 1024;
#[cfg(any(unix, windows))]
const COMMAND_OUTPUT_TAIL_BYTES: usize = 12 * 1024;
#[cfg(any(unix, windows))]
const MAX_COMMAND_RUNTIME: Duration = Duration::from_secs(30);
#[cfg(any(unix, windows))]
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);

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

/// An approved root directory for workspace operations, plus explicit
/// additional directories granted alongside it.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    additional: Vec<PathBuf>,
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
        Ok(Self {
            root,
            additional: Vec::new(),
        })
    }

    pub fn with_additional_directories(
        root: impl Into<PathBuf>,
        additional: &[PathBuf],
    ) -> Result<Self, WorkspaceError> {
        let primary = Self::new(root)?;
        let mut canonical = Vec::with_capacity(additional.len());
        for directory in additional {
            let resolved = fs::canonicalize(directory).map_err(|error| {
                WorkspaceError::new(format!(
                    "cannot resolve additional workspace directory '{}': {error}",
                    directory.display()
                ))
            })?;
            if !resolved.is_dir() {
                return Err(WorkspaceError::new(format!(
                    "additional workspace directory is not a directory: {}",
                    resolved.display()
                )));
            }
            if resolved == primary.root
                || resolved.starts_with(&primary.root)
                || primary.root.starts_with(&resolved)
            {
                return Err(WorkspaceError::new(format!(
                    "additional workspace directory must not overlap the workspace root: {}",
                    resolved.display()
                )));
            }
            if canonical.iter().any(|existing: &PathBuf| {
                *existing == resolved
                    || existing.starts_with(&resolved)
                    || resolved.starts_with(existing)
            }) {
                return Err(WorkspaceError::new(format!(
                    "additional workspace directory is a duplicate or nested: {}",
                    resolved.display()
                )));
            }
            canonical.push(resolved);
        }
        Ok(Self {
            root: primary.root,
            additional: canonical,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn additional_directories(&self) -> &[PathBuf] {
        &self.additional
    }

    fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
            || self
                .additional
                .iter()
                .any(|directory| path.starts_with(directory))
    }

    fn resolve(&self, requested: &str, allow_missing: bool) -> Result<PathBuf, ToolError> {
        let requested_path = Path::new(requested);
        if requested_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(ToolError::new("workspace paths cannot contain '..'"));
        }
        let candidate = if requested_path.is_absolute() {
            requested_path.to_path_buf()
        } else {
            self.root.join(requested_path)
        };

        let resolved = if candidate.exists() {
            fs::canonicalize(&candidate)
                .map_err(|error| ToolError::new(format!("cannot resolve '{requested}': {error}")))?
        } else if allow_missing {
            // `exists` follows symlinks, so a dangling link also lands here.
            // Writing through it would create its target, which may lie
            // outside the workspace, so only genuinely absent names qualify.
            if fs::symlink_metadata(&candidate).is_ok() {
                return Err(ToolError::new(format!(
                    "refusing to write through a dangling symlink: {requested}"
                )));
            }
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

        if !self.contains(&resolved) {
            return Err(ToolError::new(format!(
                "path escapes the workspace: {requested}"
            )));
        }
        Ok(resolved)
    }

    /// Resolve a path the agent wants to write. Paths inside a `.git`
    /// directory are refused: hooks and config there run programs, so a
    /// write could become command execution on the next git command.
    fn resolve_for_write(
        &self,
        requested: &str,
        allow_missing: bool,
    ) -> Result<PathBuf, ToolError> {
        let path = self.resolve(requested, allow_missing)?;
        let base = std::iter::once(&self.root)
            .chain(&self.additional)
            .find(|base| path.starts_with(base))
            .unwrap_or(&self.root);
        let inside_git = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .components()
            .any(|component| component.as_os_str() == ".git");
        if inside_git {
            return Err(ToolError::new(format!(
                "refusing to write inside .git: {requested}; use git commands instead"
            )));
        }
        Ok(path)
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
    workspace_tool_registry_with_directories(root, &[])
}

/// Register workspace tools with explicit additional directories.
pub fn workspace_tool_registry_with_directories(
    root: impl Into<PathBuf>,
    additional: &[PathBuf],
) -> Result<ToolRegistry, ToolError> {
    workspace_tool_registry_with_options(root, additional, &[])
}

/// Register workspace tools with additional directories and extra
/// environment variable names that `run_command` passes through.
pub fn workspace_tool_registry_with_options(
    root: impl Into<PathBuf>,
    additional: &[PathBuf],
    env_passthrough: &[String],
) -> Result<ToolRegistry, ToolError> {
    let workspace = Workspace::with_additional_directories(root, additional)
        .map_err(|error| ToolError::new(error.to_string()))?;
    let workspace = Rc::new(RefCell::new(workspace));
    let mut registry = ToolRegistry::default();

    registry.register(ListFilesTool::new(workspace.clone()))?;
    registry.register(ReadFileTool::new(workspace.clone()))?;
    registry.register(SearchFilesTool::new(workspace.clone()))?;
    registry.register(FindFilesTool::new(workspace.clone()))?;
    registry.register(WriteFileTool::new(workspace.clone()))?;
    registry.register(EditFileTool::new(workspace.clone()))?;
    registry.register(RunCommandTool::new(workspace, env_passthrough.to_vec()))?;
    Ok(registry)
}

/// Register both VM and workspace tools for the coding-agent composition root.
pub fn coding_tool_registry(root: impl Into<PathBuf>) -> Result<ToolRegistry, ToolError> {
    let mut registry = crate::agent::vm_tool_registry()?;
    registry.merge(workspace_tool_registry(root)?)?;
    Ok(registry)
}

/// Register VM and workspace tools with explicit additional directories.
pub fn coding_tool_registry_with_directories(
    root: impl Into<PathBuf>,
    additional: &[PathBuf],
) -> Result<ToolRegistry, ToolError> {
    coding_tool_registry_with_options(root, additional, &[])
}

/// Register VM and workspace tools with additional directories and extra
/// environment variable names for `run_command`.
pub fn coding_tool_registry_with_options(
    root: impl Into<PathBuf>,
    additional: &[PathBuf],
    env_passthrough: &[String],
) -> Result<ToolRegistry, ToolError> {
    let mut registry = crate::agent::vm_tool_registry()?;
    registry.merge(workspace_tool_registry_with_options(
        root,
        additional,
        env_passthrough,
    )?)?;
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
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(crate::file_walk::is_skipped_directory)
                {
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
            "Read a UTF-8 text file from the workspace. Without offset or limit the whole \
             file is returned as is (up to 128 KB). With offset (first line, 1-based) and/or \
             limit (line count) the lines come numbered, which suits large files; the \
             numbers are not part of the file.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1}},"required":["path"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["path", "offset", "limit"])?;
        let requested = required_text(arguments, "path")?;
        let offset = optional_positive(arguments, "offset")?;
        let limit = optional_positive(arguments, "limit")?;
        let workspace = self.workspace.borrow();
        let path = workspace.resolve(requested, false)?;
        if offset.is_none() && limit.is_none() {
            return read_bounded_text(&path, requested).map_err(|error| {
                if error.message.contains("byte limit") {
                    ToolError::new(format!(
                        "{}; read it in parts with offset and limit",
                        error.message
                    ))
                } else {
                    error
                }
            });
        }
        read_line_range(&path, requested, offset.unwrap_or(1), limit)
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
            "Find lines containing a literal string in text files below a workspace path \
             (default: the root). Results are path:line:text in path order; .gitignore'd, \
             binary, and dependency or build directories are skipped.",
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["pattern", "path"])?;
        let pattern = required_text(arguments, "pattern")?;
        let requested = optional_text(arguments, "path").unwrap_or(".");
        let workspace = self.workspace.borrow();
        let root = workspace.resolve(requested, false)?;
        if !root.is_dir() {
            return Err(ToolError::new(format!("not a directory: {requested}")));
        }
        let (matches, truncated) = search_files(&workspace, &root, pattern)?;
        if matches.is_empty() {
            return Ok("(no matches)".to_owned());
        }
        let mut output = matches.join("\n");
        if truncated {
            output.push_str(&format!(
                "\n[stopped at {MAX_SEARCH_RESULTS} matches; narrow the pattern or path]"
            ));
        }
        Ok(output)
    }
}

struct FindFilesTool {
    workspace: SharedWorkspace,
}

impl FindFilesTool {
    fn new(workspace: SharedWorkspace) -> Self {
        Self { workspace }
    }
}

impl Tool for FindFilesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "find_files",
            "List workspace files whose path matches a glob, in path order. `*` and `?` \
             stay within one directory and `**` spans directories (`src/**/*.rs`); a \
             pattern without `/` matches file names at any depth (`*.toml`). \
             .gitignore'd and dependency or build directories are skipped.",
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["pattern", "path"])?;
        let pattern = required_text(arguments, "pattern")?.trim_start_matches("./");
        let requested = optional_text(arguments, "path").unwrap_or(".");
        let workspace = self.workspace.borrow();
        let start = workspace.resolve(requested, false)?;
        if !start.is_dir() {
            return Err(ToolError::new(format!("not a directory: {requested}")));
        }
        let rules = crate::file_walk::IgnoreRules::load(workspace.root());
        let by_name = !pattern.contains('/');
        let mut found = Vec::new();
        let mut truncated = false;
        crate::file_walk::walk_files(workspace.root(), &start, &rules, &mut |file| {
            let below_start = file
                .path
                .strip_prefix(&start)
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let subject = if by_name {
                below_start
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            } else {
                below_start
            };
            if crate::file_walk::glob_matches(pattern, &subject) {
                if found.len() >= MAX_FIND_RESULTS {
                    truncated = true;
                    return false;
                }
                found.push(workspace.relative(&file.path));
            }
            true
        })
        .map_err(|error| ToolError::new(format!("cannot list '{requested}': {error}")))?;
        if found.is_empty() {
            return Ok("(no files)".to_owned());
        }
        let mut output = found.join("\n");
        if truncated {
            output.push_str(&format!(
                "\n[stopped at {MAX_FIND_RESULTS} files; narrow the pattern or path]"
            ));
        }
        Ok(output)
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
            id: format!("write_file:{}", sanitize_permission_component(path)),
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
        let path = workspace.resolve_for_write(requested, true)?;
        if path.exists() && path.is_dir() {
            return Err(ToolError::new(format!(
                "cannot write a directory: {requested}"
            )));
        }
        write_atomically(&path, content.as_bytes())
            .map_err(|error| ToolError::new(format!("cannot write '{requested}': {error}")))?;
        Ok(format!("wrote {} bytes to {requested}", content.len()))
    }
}

/// Lines of each side shown in an edit's diff before it is shortened.
const MAX_DIFF_LINES: usize = 12;

struct EditFileTool {
    workspace: SharedWorkspace,
}

impl EditFileTool {
    fn new(workspace: SharedWorkspace) -> Self {
        Self { workspace }
    }
}

impl Tool for EditFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "edit_file",
            "Replace exact text in an existing workspace file and return a short diff. \
             old_string must match the file exactly, including whitespace, and occur once \
             unless replace_all is true. Prefer this to write_file for changes to existing files.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean"}},"required":["path","old_string","new_string"]}"#,
        )
    }

    fn permission(&self, arguments: &ToolArguments) -> Option<PermissionRequest> {
        let path = optional_text(arguments, "path").unwrap_or("<invalid path>");
        Some(PermissionRequest {
            id: format!("edit_file:{}", sanitize_permission_component(path)),
            tool: "edit_file".to_owned(),
            description: format!("edit workspace file '{path}'"),
        })
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(
            arguments,
            &["path", "old_string", "new_string", "replace_all"],
        )?;
        let requested = required_text(arguments, "path")?;
        let old = required_text_allow_empty(arguments, "old_string")?;
        let new = required_text_allow_empty(arguments, "new_string")?;
        let replace_all = optional_flag(arguments, "replace_all")?;
        if old.is_empty() {
            return Err(ToolError::new(
                "old_string must not be empty; use write_file to create a file",
            ));
        }
        if old == new {
            return Err(ToolError::new("old_string and new_string are identical"));
        }

        let workspace = self.workspace.borrow();
        let path = workspace.resolve_for_write(requested, false)?;
        let content = read_bounded_text(&path, requested)?;
        let starts = content
            .match_indices(old)
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        let Some(&first) = starts.first() else {
            return Err(ToolError::new(format!(
                "old_string was not found in {requested}; read the file and copy the text exactly"
            )));
        };
        if starts.len() > 1 && !replace_all {
            let lines = starts
                .iter()
                .take(10)
                .map(|start| line_number_at(&content, *start).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ToolError::new(format!(
                "old_string occurs {} times in {requested} (lines {lines}); \
                 add surrounding text to make it unique or set replace_all",
                starts.len()
            )));
        }
        let updated = if replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        };
        if updated.len() > MAX_FILE_BYTES {
            return Err(ToolError::new(format!(
                "edited file would exceed the {MAX_FILE_BYTES} byte limit"
            )));
        }
        write_atomically(&path, updated.as_bytes())
            .map_err(|error| ToolError::new(format!("cannot write '{requested}': {error}")))?;

        let count = if replace_all { starts.len() } else { 1 };
        let plural = if count == 1 { "" } else { "s" };
        Ok(format!(
            "edited {requested} ({count} replacement{plural})\n{}",
            short_diff(line_number_at(&content, first), old, new)
        ))
    }
}

/// Replace `path` with `bytes` through a temporary sibling and a rename, so
/// a crash or full disk never leaves a half-written file. An existing
/// file keeps its permissions.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = parent.join(format!(
        ".{name}.arvm-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let result = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()?;
            if let Some(permissions) = permissions {
                file.set_permissions(permissions)?;
            }
            Ok(())
        })
        .and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Read a workspace file that must be bounded, UTF-8 text.
fn read_bounded_text(path: &Path, requested: &str) -> Result<String, ToolError> {
    let metadata = fs::metadata(path)
        .map_err(|error| ToolError::new(format!("cannot inspect '{requested}': {error}")))?;
    if !metadata.is_file() {
        return Err(ToolError::new(format!("not a file: {requested}")));
    }
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(ToolError::new(format!(
            "file exceeds the {MAX_FILE_BYTES} byte limit: {requested}"
        )));
    }
    let bytes = fs::read(path)
        .map_err(|error| ToolError::new(format!("cannot read '{requested}': {error}")))?;
    String::from_utf8(bytes)
        .map_err(|_| ToolError::new(format!("file is not valid UTF-8: {requested}")))
}

/// Largest file `read_file` will scan for a line range.
const MAX_RANGE_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Lines returned for a range when no limit is given.
const DEFAULT_RANGE_LINES: usize = 2_000;
/// Longest line shown in a numbered range before it is cut.
const MAX_RANGE_LINE_CHARS: usize = 2_000;

/// Numbered lines `first..first + limit` of a text file, bounded in size.
fn read_line_range(
    path: &Path,
    requested: &str,
    first: usize,
    limit: Option<usize>,
) -> Result<String, ToolError> {
    use std::io::BufRead;
    let metadata = fs::metadata(path)
        .map_err(|error| ToolError::new(format!("cannot inspect '{requested}': {error}")))?;
    if !metadata.is_file() {
        return Err(ToolError::new(format!("not a file: {requested}")));
    }
    if metadata.len() > MAX_RANGE_FILE_BYTES {
        return Err(ToolError::new(format!(
            "file exceeds the {MAX_RANGE_FILE_BYTES} byte limit for ranged reads: {requested}"
        )));
    }
    let file = fs::File::open(path)
        .map_err(|error| ToolError::new(format!("cannot read '{requested}': {error}")))?;
    let limit = limit.unwrap_or(DEFAULT_RANGE_LINES);
    let mut output = String::new();
    let mut total = 0usize;
    let mut shown = 0usize;
    let mut cut_short = false;
    for line in std::io::BufReader::new(file).split(b'\n') {
        let line =
            line.map_err(|error| ToolError::new(format!("cannot read '{requested}': {error}")))?;
        total += 1;
        if total < first || shown >= limit || cut_short {
            continue;
        }
        let text = String::from_utf8(line)
            .map_err(|_| ToolError::new(format!("file is not valid UTF-8: {requested}")))?;
        let text = text.strip_suffix('\r').unwrap_or(&text);
        let entry = match text.char_indices().nth(MAX_RANGE_LINE_CHARS) {
            Some((end, _)) => format!("{total:>6}\t{}... [line cut]\n", &text[..end]),
            None => format!("{total:>6}\t{text}\n"),
        };
        if output.len() + entry.len() > MAX_FILE_BYTES {
            cut_short = true;
            continue;
        }
        output.push_str(&entry);
        shown += 1;
    }
    if first > total.max(1) {
        return Err(ToolError::new(format!(
            "offset {first} is past the end of {requested} ({total} lines)"
        )));
    }
    let last = first + shown.saturating_sub(1);
    if shown == 0 {
        output.push_str("(no lines)\n");
    }
    if last < total {
        output.push_str(&format!(
            "[lines {first}-{last} of {total}; continue with offset {}]",
            last + 1
        ));
    } else {
        output.push_str(&format!("[end of file, {total} lines]"));
    }
    Ok(output)
}

/// The 1-based line on which byte offset `index` falls.
fn line_number_at(content: &str, index: usize) -> usize {
    content[..index].matches('\n').count() + 1
}

/// A compact diff of one replacement, each side cut to `MAX_DIFF_LINES`.
fn short_diff(line: usize, old: &str, new: &str) -> String {
    fn side(text: &str, marker: char, output: &mut String) {
        let lines = text.lines().collect::<Vec<_>>();
        for line in lines.iter().take(MAX_DIFF_LINES) {
            output.push(marker);
            output.push_str(line);
            output.push('\n');
        }
        if lines.len() > MAX_DIFF_LINES {
            output.push_str(&format!(
                "{marker}... {} more lines\n",
                lines.len() - MAX_DIFF_LINES
            ));
        }
    }
    let mut output = format!("@@ line {line} @@\n");
    side(old, '-', &mut output);
    side(new, '+', &mut output);
    output.truncate(output.trim_end().len());
    output
}

struct RunCommandTool {
    workspace: SharedWorkspace,
    env_passthrough: Vec<String>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl RunCommandTool {
    fn new(workspace: SharedWorkspace, env_passthrough: Vec<String>) -> Self {
        Self {
            workspace,
            env_passthrough,
            cancel: None,
        }
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
            id: format!("run_command:{}", sanitize_permission_component(command)),
            tool: "run_command".to_owned(),
            description: format!("run command in workspace: {command}"),
        })
    }

    fn preapproved(&self, arguments: &ToolArguments) -> bool {
        let Some(command) = optional_text(arguments, "command") else {
            return false;
        };
        arguments.len() == 1
            && reject_dangerous_command(command).is_ok()
            && crate::read_only_commands::is_read_only_command(
                command,
                self.workspace.borrow().root(),
            )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["command"])?;
        let command = required_text(arguments, "command")?;
        reject_dangerous_command(command)?;
        let workspace = self.workspace.borrow();
        let output = shell_command(
            command,
            workspace.root(),
            &self.env_passthrough,
            self.cancel.as_deref(),
        )?;
        Ok(format_command_output(&output))
    }

    fn set_cancel_token(&mut self, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.cancel = Some(cancel);
    }
}

#[cfg(any(unix, windows))]
fn shell_command(
    command: &str,
    directory: &Path,
    env_passthrough: &[String],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<std::process::Output, ToolError> {
    shell_command_with_timeout(
        command,
        directory,
        env_passthrough,
        MAX_COMMAND_RUNTIME,
        cancel,
    )
}

#[cfg(any(unix, windows))]
fn shell_command_with_timeout(
    command: &str,
    directory: &Path,
    env_passthrough: &[String],
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<std::process::Output, ToolError> {
    #[cfg(unix)]
    let mut process = Command::new("sh");
    #[cfg(windows)]
    let mut process = Command::new("cmd");
    #[cfg(unix)]
    process.args(["-c", command]);
    #[cfg(windows)]
    process.args(["/C", command]);
    process.current_dir(directory);
    crate::command_env::apply_scrubbed_environment(&mut process, env_passthrough);
    crate::command_env::start_new_process_group(&mut process);
    process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = process
        .spawn()
        .map_err(|error| ToolError::new(format!("failed to run command: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::new("command stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::new("command stderr is unavailable"))?;
    let stdout_reader = thread::spawn(|| bounded_pipe_output(stdout));
    let stderr_reader = thread::spawn(|| bounded_pipe_output(stderr));
    let deadline = Instant::now() + timeout;
    let group = child.id();
    let status = loop {
        if cancel.is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::Acquire)) {
            crate::command_env::kill_process_group(group);
            let _ = child.kill();
            let _ = child.wait();
            return Err(ToolError::new("command cancelled"));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(COMMAND_POLL_INTERVAL),
            Ok(None) => {
                crate::command_env::kill_process_group(group);
                let _ = child.kill();
                let _ = child.wait();
                return Err(ToolError::new(format!(
                    "command exceeded the {} second limit",
                    timeout.as_secs()
                )));
            }
            Err(error) => {
                crate::command_env::kill_process_group(group);
                let _ = child.kill();
                let _ = child.wait();
                return Err(ToolError::new(format!("failed to finish command: {error}")));
            }
        }
    };
    // Background children (`cmd &`) would otherwise hold the output pipes
    // open and outlive the command; end them with it.
    crate::command_env::kill_process_group(group);

    let stdout = stdout_reader
        .join()
        .map_err(|_| ToolError::new("command stdout reader failed"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| ToolError::new("command stderr reader failed"))?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(any(unix, windows))]
fn bounded_pipe_output(pipe: impl Read) -> Vec<u8> {
    capture_head_and_tail(pipe, COMMAND_OUTPUT_HEAD_BYTES, COMMAND_OUTPUT_TAIL_BYTES)
}

#[cfg(any(unix, windows))]
/// Keep the first `head` and last `tail` bytes of a stream, with a note of
/// how much was skipped between them. Failures and summaries usually come
/// at the end of build and test output, so the tail matters most.
fn capture_head_and_tail(mut pipe: impl Read, head: usize, tail: usize) -> Vec<u8> {
    let mut first = Vec::new();
    let mut last = std::collections::VecDeque::with_capacity(tail);
    let mut skipped = 0usize;
    let mut buffer = [0u8; 4096];
    loop {
        let bytes = match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(bytes) => &buffer[..bytes],
        };
        let into_head = bytes.len().min(head - first.len());
        first.extend_from_slice(&bytes[..into_head]);
        for &byte in &bytes[into_head..] {
            if last.len() == tail {
                last.pop_front();
                skipped += 1;
            }
            last.push_back(byte);
        }
    }
    if skipped > 0 {
        first.extend_from_slice(
            format!("\n[... {skipped} bytes of output omitted ...]\n").as_bytes(),
        );
    }
    first.extend(last);
    first
}

#[cfg(not(any(unix, windows)))]
fn shell_command(
    _command: &str,
    _directory: &Path,
    _env_passthrough: &[String],
    _cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<std::process::Output, ToolError> {
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
    if tokens.contains(&"dd") && joined.contains(" of=/dev/") {
        return Err(ToolError::new(format!(
            "refusing dangerous command '{command}'"
        )));
    }
    let recursive = tokens.iter().any(|token| {
        token.starts_with("-")
            && token.contains('r')
            && token.chars().skip(1).all(|flag| "rfRxivfp".contains(flag))
    });
    let removes_root = tokens.contains(&"rm")
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
    String::from_utf8_lossy(bytes).into_owned()
}

/// Longest line shown in a search result before it is cut.
const MAX_SEARCH_LINE_CHARS: usize = 300;
/// Files listed by `find_files` before the list is cut.
const MAX_FIND_RESULTS: usize = 200;

/// Matching lines below `start`, in path order, as `path:line:text`.
/// Binary, oversized, ignored, and non-UTF-8 files are skipped.
fn search_files(
    workspace: &Workspace,
    start: &Path,
    pattern: &str,
) -> Result<(Vec<String>, bool), ToolError> {
    let rules = crate::file_walk::IgnoreRules::load(workspace.root());
    let mut matches = Vec::new();
    let mut truncated = false;
    crate::file_walk::walk_files(workspace.root(), start, &rules, &mut |file| {
        let Ok(metadata) = fs::metadata(&file.path) else {
            return true;
        };
        if metadata.len() > MAX_FILE_BYTES as u64 {
            return true;
        }
        let Ok(bytes) = fs::read(&file.path) else {
            return true;
        };
        if bytes.contains(&0) {
            return true;
        }
        let Ok(contents) = String::from_utf8(bytes) else {
            return true;
        };
        let relative = workspace.relative(&file.path);
        for (line_number, line) in contents.lines().enumerate() {
            if !line.contains(pattern) {
                continue;
            }
            if matches.len() >= MAX_SEARCH_RESULTS {
                truncated = true;
                return false;
            }
            let shown = match line.char_indices().nth(MAX_SEARCH_LINE_CHARS) {
                Some((end, _)) => format!("{}... [line cut]", &line[..end]),
                None => line.to_owned(),
            };
            matches.push(format!("{relative}:{}:{shown}", line_number + 1));
        }
        true
    })
    .map_err(|error| ToolError::new(format!("cannot search '{}': {error}", start.display())))?;
    Ok((matches, truncated))
}

#[cfg(test)]
mod tests {
    use super::{
        Workspace, coding_tool_registry, workspace_tool_registry,
        workspace_tool_registry_with_directories,
    };
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

    fn edit(
        registry: &mut crate::agent::ToolRegistry,
        path: &str,
        old: &str,
        new: &str,
        replace_all: Option<bool>,
    ) -> crate::agent::ToolResult {
        let mut arguments: ToolArguments = [
            ("path".to_owned(), ToolValue::Text(path.to_owned())),
            ("old_string".to_owned(), ToolValue::Text(old.to_owned())),
            ("new_string".to_owned(), ToolValue::Text(new.to_owned())),
        ]
        .into_iter()
        .collect();
        if let Some(flag) = replace_all {
            arguments.insert("replace_all".to_owned(), ToolValue::Boolean(flag));
        }
        registry.execute(&ToolCall::new("edit-1", "edit_file", arguments))
    }

    fn read(
        registry: &mut crate::agent::ToolRegistry,
        path: &str,
        offset: Option<i32>,
        limit: Option<i32>,
    ) -> crate::agent::ToolResult {
        let mut arguments: ToolArguments = [("path".to_owned(), ToolValue::Text(path.to_owned()))]
            .into_iter()
            .collect();
        if let Some(offset) = offset {
            arguments.insert("offset".to_owned(), ToolValue::Integer(offset));
        }
        if let Some(limit) = limit {
            arguments.insert("limit".to_owned(), ToolValue::Integer(limit));
        }
        registry.execute(&ToolCall::new("read-1", "read_file", arguments))
    }

    #[test]
    fn read_file_returns_numbered_line_ranges() {
        let root = test_root("read-range");
        std::fs::write(root.join("a.txt"), "one\ntwo\r\nthree\nfour\n").unwrap();
        let mut registry = workspace_tool_registry(&root).unwrap();

        let middle = read(&mut registry, "a.txt", Some(2), Some(2));
        assert!(!middle.is_error, "{}", middle.content);
        assert_eq!(
            middle.content,
            "     2\ttwo\n     3\tthree\n[lines 2-3 of 4; continue with offset 4]"
        );
        let tail = read(&mut registry, "a.txt", Some(4), None);
        assert_eq!(tail.content, "     4\tfour\n[end of file, 4 lines]");
        let past = read(&mut registry, "a.txt", Some(9), None);
        assert!(past.is_error && past.content.contains("past the end"));
        let zero = read(&mut registry, "a.txt", Some(0), None);
        assert!(zero.is_error, "{}", zero.content);
        // Without a range the file comes back unchanged.
        let whole = read(&mut registry, "a.txt", None, None);
        assert_eq!(whole.content, "one\ntwo\r\nthree\nfour\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_file_ranges_reach_into_large_files() {
        let root = test_root("read-large");
        let big = (1..=40_000)
            .map(|n| format!("line {n}\n"))
            .collect::<String>();
        assert!(big.len() > super::MAX_FILE_BYTES);
        std::fs::write(root.join("big.log"), &big).unwrap();
        let mut registry = workspace_tool_registry(&root).unwrap();

        let whole = read(&mut registry, "big.log", None, None);
        assert!(whole.is_error && whole.content.contains("offset and limit"));
        let part = read(&mut registry, "big.log", Some(39_999), Some(5));
        assert_eq!(
            part.content,
            " 39999\tline 39999\n 40000\tline 40000\n[end of file, 40000 lines]"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn call(
        registry: &mut crate::agent::ToolRegistry,
        tool: &str,
        pairs: &[(&str, &str)],
    ) -> crate::agent::ToolResult {
        let arguments = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), ToolValue::Text((*value).to_owned())))
            .collect();
        registry.execute(&ToolCall::new("call-1", tool, arguments))
    }

    fn write_tree(root: &std::path::Path, files: &[(&str, &[u8])]) {
        for (file, content) in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
    }

    #[test]
    fn find_files_matches_globs_in_path_order() {
        let root = test_root("find");
        write_tree(
            &root,
            &[
                ("Cargo.toml", b"x"),
                ("src/main.rs", b"x"),
                ("src/cli/mod.rs", b"x"),
                ("src/cli/notes.md", b"x"),
                ("gen/out.rs", b"x"),
                ("node_modules/m/index.rs", b"x"),
                (".gitignore", b"gen/\n"),
            ],
        );
        let mut registry = workspace_tool_registry(&root).unwrap();
        let by_name = call(&mut registry, "find_files", &[("pattern", "*.rs")]);
        assert_eq!(by_name.content, "src/cli/mod.rs\nsrc/main.rs");
        let by_path = call(&mut registry, "find_files", &[("pattern", "src/*.rs")]);
        assert_eq!(by_path.content, "src/main.rs");
        let deep = call(&mut registry, "find_files", &[("pattern", "**/*.md")]);
        assert_eq!(deep.content, "src/cli/notes.md");
        let scoped = call(
            &mut registry,
            "find_files",
            &[("pattern", "*.rs"), ("path", "src/cli")],
        );
        assert_eq!(scoped.content, "src/cli/mod.rs");
        let none = call(&mut registry, "find_files", &[("pattern", "*.py")]);
        assert_eq!(none.content, "(no files)");
        let escape = call(
            &mut registry,
            "find_files",
            &[("pattern", "*"), ("path", "/etc")],
        );
        assert!(escape.is_error, "{}", escape.content);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn search_skips_binary_and_ignored_files_and_reports_truncation() {
        let root = test_root("search-quality");
        let many = "hit\n".repeat(150);
        let long_line = format!("hit {}", "y".repeat(1_000));
        write_tree(
            &root,
            &[
                ("b.txt", b"hit two\n"),
                ("a.txt", b"hit one\n"),
                ("blob.bin", b"hit\0binary"),
                ("logs/x.log", b"hit log\n"),
                (".gitignore", b"*.log\n"),
                ("long.txt", long_line.as_bytes()),
            ],
        );
        let mut registry = workspace_tool_registry(&root).unwrap();
        let result = call(&mut registry, "search_files", &[("pattern", "hit")]);
        let lines = result.content.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "a.txt:1:hit one");
        assert_eq!(lines[1], "b.txt:1:hit two");
        assert!(lines[2].starts_with("long.txt:1:hit ") && lines[2].ends_with("[line cut]"));
        assert_eq!(lines.len(), 3, "{}", result.content);

        write_tree(&root, &[("c.txt", many.as_bytes())]);
        let capped = call(&mut registry, "search_files", &[("pattern", "hit")]);
        assert!(capped.content.ends_with("narrow the pattern or path]"));
        assert_eq!(
            capped.content.lines().count(),
            super::MAX_SEARCH_RESULTS + 1
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edit_file_replaces_unique_text_and_reports_a_diff() {
        let root = test_root("edit");
        std::fs::write(root.join("a.rs"), "fn a() {}\nfn b() {\n    1\n}\n").unwrap();
        let mut registry = workspace_tool_registry(&root).unwrap();

        let result = edit(&mut registry, "a.rs", "    1\n", "    2\n", None);
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(
            result.content,
            "edited a.rs (1 replacement)\n@@ line 3 @@\n-    1\n+    2"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("a.rs")).unwrap(),
            "fn a() {}\nfn b() {\n    2\n}\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edit_file_refuses_missing_ambiguous_and_empty_matches() {
        let root = test_root("edit-errors");
        std::fs::write(root.join("a.txt"), "x\ny\nx\n").unwrap();
        let mut registry = workspace_tool_registry(&root).unwrap();

        let missing = edit(&mut registry, "a.txt", "z", "w", None);
        assert!(missing.is_error && missing.content.contains("not found"));

        let ambiguous = edit(&mut registry, "a.txt", "x", "w", None);
        assert!(ambiguous.is_error, "{}", ambiguous.content);
        assert!(ambiguous.content.contains("2 times") && ambiguous.content.contains("lines 1, 3"));

        let empty = edit(&mut registry, "a.txt", "", "w", None);
        assert!(empty.is_error);
        let same = edit(&mut registry, "a.txt", "y", "y", None);
        assert!(same.is_error);
        let absent_file = edit(&mut registry, "new.txt", "a", "b", None);
        assert!(absent_file.is_error);
        assert!(!root.join("new.txt").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "x\ny\nx\n"
        );

        let all = edit(&mut registry, "a.txt", "x", "w", Some(true));
        assert!(!all.is_error, "{}", all.content);
        assert!(all.content.starts_with("edited a.txt (2 replacements)"));
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "w\ny\nw\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_stops_a_running_command_and_its_children() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let root = test_root("cancel-command");
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let setter = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            flag.store(true, Ordering::Release);
        });
        let started = std::time::Instant::now();
        let error = super::shell_command_with_timeout(
            "sleep 20 & sleep 20",
            &root,
            &[],
            std::time::Duration::from_secs(20),
            Some(&cancel),
        )
        .unwrap_err();
        setter.join().unwrap();
        assert_eq!(error.message, "command cancelled");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn writes_and_edits_keep_permissions_and_leave_no_temporary_files() {
        use std::os::unix::fs::PermissionsExt;
        let root = test_root("atomic");
        let script = root.join("run.sh");
        std::fs::write(&script, "echo one\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut registry = workspace_tool_registry(&root).unwrap();
        let edited = edit(&mut registry, "run.sh", "one", "two", None);
        assert!(!edited.is_error, "{}", edited.content);
        let written = call(
            &mut registry,
            "write_file",
            &[("path", "run.sh"), ("content", "echo three\n")],
        );
        assert!(!written.is_error, "{}", written.content);
        let mode = std::fs::metadata(&script).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        assert_eq!(std::fs::read_to_string(&script).unwrap(), "echo three\n");
        let leftovers = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writes_and_edits_inside_git_are_refused() {
        let root = test_root("git-write");
        std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
        std::fs::write(root.join(".git/config"), "[core]\n").unwrap();
        std::fs::create_dir_all(root.join("sub/.git")).unwrap();
        let mut registry = workspace_tool_registry(&root).unwrap();
        for path in [".git/hooks/pre-commit", "./.git/config", "sub/.git/config"] {
            let write = call(
                &mut registry,
                "write_file",
                &[("path", path), ("content", "#!/bin/sh\nrun-me\n")],
            );
            assert!(
                write.is_error && write.content.contains(".git"),
                "{path}: {}",
                write.content
            );
        }
        assert!(!root.join(".git/hooks/pre-commit").exists());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join(".git/hooks"), root.join("hooks")).unwrap();
            let through_link = call(
                &mut registry,
                "write_file",
                &[("path", "hooks/pre-commit"), ("content", "run-me")],
            );
            assert!(through_link.is_error, "{}", through_link.content);
            assert!(!root.join(".git/hooks/pre-commit").exists());
        }
        let edited = edit(
            &mut registry,
            ".git/config",
            "[core]",
            "[core]\n\tfsmonitor = x",
            None,
        );
        assert!(edited.is_error, "{}", edited.content);
        assert_eq!(
            std::fs::read_to_string(root.join(".git/config")).unwrap(),
            "[core]\n"
        );
        // Similar names outside .git are fine.
        let ignore = call(
            &mut registry,
            "write_file",
            &[("path", ".gitignore"), ("content", "target/\n")],
        );
        assert!(!ignore.is_error, "{}", ignore.content);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edit_file_stays_inside_the_workspace_and_needs_approval() {
        let root = test_root("edit-scope");
        let outside = test_root("edit-scope-outside");
        std::fs::write(outside.join("x.txt"), "keep").unwrap();
        let mut registry = workspace_tool_registry(&root).unwrap();
        let escaped = outside.join("x.txt").display().to_string();
        let result = edit(&mut registry, &escaped, "keep", "lost", None);
        assert!(result.is_error, "{}", result.content);
        assert_eq!(
            std::fs::read_to_string(outside.join("x.txt")).unwrap(),
            "keep"
        );

        let call = ToolCall::new(
            "edit-2",
            "edit_file",
            [("path".to_owned(), ToolValue::Text("a.txt".to_owned()))]
                .into_iter()
                .collect(),
        );
        let request = registry.permission_request(&call).unwrap();
        assert_eq!(request.tool, "edit_file");
        assert!(request.description.contains("a.txt"));
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_file_refuses_dangling_symlinks_that_point_outside() {
        let root = test_root("dangling-link");
        let outside = test_root("dangling-link-outside");
        let target = outside.join("escaped.txt");
        std::os::unix::fs::symlink(&target, root.join("link.txt")).unwrap();
        let mut registry = workspace_tool_registry(&root).unwrap();

        let write = registry.execute(&ToolCall::new(
            "write-1",
            "write_file",
            [
                ("path".to_owned(), ToolValue::Text("link.txt".to_owned())),
                ("content".to_owned(), ToolValue::Text("pwned".to_owned())),
            ]
            .into_iter()
            .collect::<ToolArguments>(),
        ));

        assert!(write.is_error, "{}", write.content);
        assert!(!target.exists(), "write escaped the workspace");
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
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
        assert_eq!(coding.specs().len(), 14);
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
    fn long_output_keeps_its_start_and_end_with_a_note() {
        let input = (0..10_000).map(|n| format!("{n}\n")).collect::<String>();
        let kept = super::capture_head_and_tail(input.as_bytes(), 10, 20);
        let text = String::from_utf8(kept).unwrap();
        assert!(text.starts_with("0\n1\n2\n3\n4\n"), "{text}");
        assert!(text.ends_with("9997\n9998\n9999\n"), "{text}");
        let skipped = input.len() - 30;
        assert!(text.contains(&format!("[... {skipped} bytes of output omitted ...]")));
        // Short output passes through unchanged.
        assert_eq!(super::capture_head_and_tail(&b"hi\n"[..], 10, 20), b"hi\n");
        assert_eq!(
            super::capture_head_and_tail(&b"0123456789abc"[..], 10, 20),
            b"0123456789abc"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failing_test_output_shows_the_end() {
        let root = test_root("command-tail");
        let mut registry = workspace_tool_registry(&root).unwrap();
        let result = call(
            &mut registry,
            "run_command",
            &[("command", "seq 1 20000; echo FINAL-FAILURE >&2; exit 3")],
        );
        assert!(
            result.content.contains("bytes of output omitted"),
            "{}",
            result.content
        );
        assert!(result.content.contains("20000\n"));
        assert!(result.content.contains("FINAL-FAILURE"));
        assert!(result.content.ends_with("exit_code=3"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn command_tool_terminates_a_command_that_exceeds_its_deadline() {
        let root = test_root("command-timeout");

        let error = super::shell_command_with_timeout(
            "sleep 5",
            &root,
            &[],
            std::time::Duration::from_millis(100),
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("exceeded"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn command_tool_ends_background_children_with_the_command() {
        let root = test_root("command-background");
        let started = std::time::Instant::now();
        let output = super::shell_command_with_timeout(
            "sleep 30 & echo started",
            &root,
            &[],
            std::time::Duration::from_secs(10),
            None,
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("started"));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));

        let started = std::time::Instant::now();
        let error = super::shell_command_with_timeout(
            "sleep 30 & sleep 30",
            &root,
            &[],
            std::time::Duration::from_millis(200),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeded"));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn command_tool_hides_host_secrets() {
        let root = test_root("command-env");
        // SAFETY: tests in this crate do not read this variable concurrently.
        unsafe { std::env::set_var("A_RVM_TEST_SECRET_TOKEN", "hunter2") };
        let output = super::shell_command_with_timeout(
            "env",
            &root,
            &[],
            std::time::Duration::from_secs(10),
            None,
        )
        .unwrap();
        let printed = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(!printed.contains("hunter2"));
        assert!(printed.contains("PATH="));
        let output = super::shell_command_with_timeout(
            "env",
            &root,
            &["A_RVM_TEST_SECRET_TOKEN".to_owned()],
            std::time::Duration::from_secs(10),
            None,
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("hunter2"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn additional_directories_allow_absolute_access_and_reject_outsiders() {
        let root = test_root("extra-primary");
        let extra = test_root("extra-secondary");
        let outside = test_root("extra-outside");
        std::fs::write(extra.join("shared.txt"), "shared\n").unwrap();
        let mut registry =
            workspace_tool_registry_with_directories(&root, std::slice::from_ref(&extra)).unwrap();
        let absolute = extra.join("shared.txt").display().to_string();

        let read = registry.execute(&ToolCall::new(
            "read-1",
            "read_file",
            [("path".to_owned(), ToolValue::Text(absolute.clone()))]
                .into_iter()
                .collect(),
        ));
        assert_eq!(read.content, "shared\n");
        assert!(!read.is_error);

        let write = registry.execute(&ToolCall::new(
            "write-1",
            "write_file",
            [
                (
                    "path".to_owned(),
                    ToolValue::Text(extra.join("created.txt").display().to_string()),
                ),
                ("content".to_owned(), ToolValue::Text("created".to_owned())),
            ]
            .into_iter()
            .collect::<ToolArguments>(),
        ));
        assert!(!write.is_error);
        assert_eq!(
            std::fs::read_to_string(extra.join("created.txt")).unwrap(),
            "created"
        );

        let outside_path = outside.join("other.txt").display().to_string();
        std::fs::write(outside.join("other.txt"), "outside\n").unwrap();
        let rejected = registry.execute(&ToolCall::new(
            "read-2",
            "read_file",
            [("path".to_owned(), ToolValue::Text(outside_path))]
                .into_iter()
                .collect(),
        ));
        assert!(rejected.is_error);
        assert!(rejected.content.contains("path escapes the workspace"));

        assert!(
            Workspace::with_additional_directories(&root, std::slice::from_ref(&root)).is_err()
        );
        std::fs::create_dir_all(root.join("nested")).unwrap();
        assert!(Workspace::with_additional_directories(&root, &[root.join("nested")]).is_err());

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(extra).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
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
