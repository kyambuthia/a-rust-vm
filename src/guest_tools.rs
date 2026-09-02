//! Agent tools backed exclusively by a [`VmInstance`].
//!
//! These tools are the migration boundary away from host workspace access:
//! every path and process operation is resolved by the guest runtime. They do
//! not invoke a host shell or read the host filesystem.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::agent::{Tool, ToolArguments, ToolError, ToolRegistry, ToolSpec, ToolValue};
use crate::runtime::{
    DirectoryEntry, EntryKind, ProcessEvent, ProcessInfo, ProcessState, RuntimeError, VmInstance,
    WaitStatus,
};

/// Shared handle used when multiple server requests target one guest VM.
pub type SharedGuestVm = Arc<Mutex<VmInstance>>;

/// Register tools that operate only inside the supplied guest VM.
pub fn guest_tool_registry(vm: VmInstance) -> Result<ToolRegistry, ToolError> {
    guest_tool_registry_shared(Arc::new(Mutex::new(vm)))
}

/// Register guest tools against a VM that persists across requests.
pub fn guest_tool_registry_shared(vm: SharedGuestVm) -> Result<ToolRegistry, ToolError> {
    let mut registry = ToolRegistry::default();

    registry.register(GuestListFilesTool::new(vm.clone()))?;
    registry.register(GuestReadFileTool::new(vm.clone()))?;
    registry.register(GuestWriteFileTool::new(vm.clone()))?;
    registry.register(GuestMakeDirectoryTool::new(vm.clone()))?;
    registry.register(GuestTabulateFileTool::new(vm.clone()))?;
    registry.register(GuestInspectPdfTool::new(vm.clone()))?;
    registry.register(GuestSpawnTool::new(vm.clone()))?;
    registry.register(GuestTickTool::new(vm.clone()))?;
    registry.register(GuestRunTool::new(vm.clone()))?;
    registry.register(GuestProcessListTool::new(vm.clone()))?;
    registry.register(GuestWaitTool::new(vm))?;
    Ok(registry)
}

/// Compose the arithmetic VM tools with the isolated guest tools.
pub fn guest_coding_tool_registry(vm: VmInstance) -> Result<ToolRegistry, ToolError> {
    guest_coding_tool_registry_shared(Arc::new(Mutex::new(vm)))
}

/// Compose the arithmetic VM tools with a shared isolated guest VM.
pub fn guest_coding_tool_registry_shared(vm: SharedGuestVm) -> Result<ToolRegistry, ToolError> {
    let mut registry = crate::agent::vm_tool_registry()?;
    registry.merge(guest_tool_registry_shared(vm)?)?;
    Ok(registry)
}

struct GuestListFilesTool {
    vm: SharedGuestVm,
}

impl GuestListFilesTool {
    fn new(vm: SharedGuestVm) -> Self {
        Self { vm }
    }
}

impl Tool for GuestListFilesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "guest_list_files",
            "List entries in a directory inside the guest filesystem.",
            r#"{"type":"object","properties":{"path":{"type":"string"}}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["path"])?;
        let path = optional_text(arguments, "path").unwrap_or("/");
        let vm = lock_guest(&self.vm)?;
        let entries = vm.list_dir(path).map_err(runtime_error)?;
        Ok(format_entries(&entries))
    }
}

struct GuestReadFileTool {
    vm: SharedGuestVm,
}

impl GuestReadFileTool {
    fn new(vm: SharedGuestVm) -> Self {
        Self { vm }
    }
}

impl Tool for GuestReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "guest_read_file",
            "Read a UTF-8 file from the guest filesystem.",
            r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["path"])?;
        let path = required_text(arguments, "path")?;
        lock_guest(&self.vm)?.read_text(path).map_err(runtime_error)
    }
}

struct GuestWriteFileTool {
    vm: SharedGuestVm,
}

impl GuestWriteFileTool {
    fn new(vm: SharedGuestVm) -> Self {
        Self { vm }
    }
}

impl Tool for GuestWriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "guest_write_file",
            "Create or replace a UTF-8 file inside the guest filesystem.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["path", "content"])?;
        let path = required_text(arguments, "path")?;
        let content = required_text_allow_empty(arguments, "content")?;
        let byte_count = content.len();
        lock_guest_mut(&self.vm)?
            .write_file(path, content)
            .map_err(runtime_error)?;
        Ok(format!("wrote {byte_count} bytes to guest:{path}"))
    }
}

struct GuestMakeDirectoryTool {
    vm: SharedGuestVm,
}

impl GuestMakeDirectoryTool {
    fn new(vm: SharedGuestVm) -> Self {
        Self { vm }
    }
}

impl Tool for GuestMakeDirectoryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "guest_make_directory",
            "Create a directory inside the guest filesystem.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"recursive":{"type":"boolean"}},"required":["path"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["path", "recursive"])?;
        let path = required_text(arguments, "path")?;
        let recursive = optional_bool(arguments, "recursive").unwrap_or(false);
        lock_guest_mut(&self.vm)?
            .mkdir(path, recursive)
            .map_err(runtime_error)?;
        Ok(format!("created guest directory {path}"))
    }
}

struct GuestSpawnTool {
    vm: SharedGuestVm,
}

struct GuestTabulateFileTool {
    vm: SharedGuestVm,
}

impl GuestTabulateFileTool {
    fn new(vm: SharedGuestVm) -> Self {
        Self { vm }
    }
}

impl Tool for GuestTabulateFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "guest_tabulate_file",
            "Tabulate an uploaded CSV or TSV guest file and write a JSON summary to /workspace/output.",
            r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["path"])?;
        let path = required_text(arguments, "path")?;
        let mut vm = lock_guest_mut(&self.vm)?;
        let summary = crate::jobs::tabulate_uploaded_file(&mut vm, path).map_err(ToolError::new)?;
        serde_json::to_string(&summary)
            .map_err(|error| ToolError::new(format!("failed to encode table summary: {error}")))
    }
}

struct GuestInspectPdfTool {
    vm: SharedGuestVm,
}

impl GuestInspectPdfTool {
    fn new(vm: SharedGuestVm) -> Self {
        Self { vm }
    }
}

impl Tool for GuestInspectPdfTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "guest_inspect_pdf",
            "Validate an uploaded PDF and write safe document metadata to /workspace/output. This does not extract document text.",
            r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["path"])?;
        let path = required_text(arguments, "path")?;
        let mut vm = lock_guest_mut(&self.vm)?;
        let summary = crate::jobs::inspect_uploaded_pdf(&mut vm, path).map_err(ToolError::new)?;
        serde_json::to_string(&summary)
            .map_err(|error| ToolError::new(format!("failed to encode PDF summary: {error}")))
    }
}

impl GuestSpawnTool {
    fn new(vm: SharedGuestVm) -> Self {
        Self { vm }
    }
}

impl Tool for GuestSpawnTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "guest_spawn",
            "Spawn a validated A/RVM bytecode program as a guest process.",
            r#"{"type":"object","properties":{"program":{"type":"string"},"argv":{"type":"array","items":{"type":"string"}}},"required":["program"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["program", "argv"])?;
        let source = required_text(arguments, "program")?;
        let program = crate::agent::parse_program(source)?;
        let argv = optional_string_list(arguments, "argv")?;
        let pid = lock_guest_mut(&self.vm)?
            .spawn(None, program, argv)
            .map_err(runtime_error)?;
        Ok(format!("spawned guest process pid={pid}"))
    }
}

struct GuestTickTool {
    vm: SharedGuestVm,
}

impl GuestTickTool {
    fn new(vm: SharedGuestVm) -> Self {
        Self { vm }
    }
}

impl Tool for GuestTickTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "guest_tick",
            "Execute one instruction from the next ready guest process.",
            r#"{"type":"object","properties":{}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &[])?;
        let event = lock_guest_mut(&self.vm)?.tick();
        Ok(event
            .map(|event| format_event(&event))
            .unwrap_or_else(|| "guest scheduler idle".to_owned()))
    }
}

struct GuestRunTool {
    vm: SharedGuestVm,
}

impl GuestRunTool {
    fn new(vm: SharedGuestVm) -> Self {
        Self { vm }
    }
}

impl Tool for GuestRunTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "guest_run",
            "Run ready guest processes for a bounded number of scheduler ticks.",
            r#"{"type":"object","properties":{"max_ticks":{"type":"integer","minimum":1}}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["max_ticks"])?;
        let max_ticks = optional_integer(arguments, "max_ticks")
            .unwrap_or(1_000)
            .try_into()
            .map_err(|_| ToolError::new("argument 'max_ticks' must be a positive integer"))?;
        if max_ticks == 0 {
            return Err(ToolError::new(
                "argument 'max_ticks' must be a positive integer",
            ));
        }
        let events = lock_guest_mut(&self.vm)?
            .run_until_idle(max_ticks)
            .map_err(runtime_error)?;
        if events.is_empty() {
            Ok("guest scheduler idle".to_owned())
        } else {
            Ok(events
                .iter()
                .map(format_event)
                .collect::<Vec<_>>()
                .join("\n"))
        }
    }
}

struct GuestProcessListTool {
    vm: SharedGuestVm,
}

impl GuestProcessListTool {
    fn new(vm: SharedGuestVm) -> Self {
        Self { vm }
    }
}

impl Tool for GuestProcessListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "guest_processes",
            "List guest process IDs, states, working directories, and stacks.",
            r#"{"type":"object","properties":{}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &[])?;
        let vm = lock_guest(&self.vm)?;
        let processes = vm.processes();
        if processes.is_empty() {
            return Ok("(no guest processes)".to_owned());
        }
        Ok(processes
            .iter()
            .map(format_process)
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

struct GuestWaitTool {
    vm: SharedGuestVm,
}

impl GuestWaitTool {
    fn new(vm: SharedGuestVm) -> Self {
        Self { vm }
    }
}

impl Tool for GuestWaitTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "guest_wait",
            "Poll the exit status of a guest process.",
            r#"{"type":"object","properties":{"pid":{"type":"integer","minimum":1}},"required":["pid"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["pid"])?;
        let pid = required_pid(arguments, "pid")?;
        let status = lock_guest(&self.vm)?.wait(pid).map_err(runtime_error)?;
        Ok(format_wait_status(&status))
    }
}

fn runtime_error(error: RuntimeError) -> ToolError {
    ToolError::new(error.to_string())
}

fn lock_guest(vm: &SharedGuestVm) -> Result<MutexGuard<'_, VmInstance>, ToolError> {
    vm.lock()
        .map_err(|_| ToolError::new("guest VM lock is poisoned"))
}

fn lock_guest_mut(vm: &SharedGuestVm) -> Result<MutexGuard<'_, VmInstance>, ToolError> {
    lock_guest(vm)
}

fn format_entries(entries: &[DirectoryEntry]) -> String {
    if entries.is_empty() {
        return "(empty)".to_owned();
    }
    entries
        .iter()
        .map(|entry| {
            let kind = match entry.kind {
                EntryKind::Directory => "dir",
                EntryKind::File => "file",
            };
            format!("{kind} {} size={}", entry.name, entry.size)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_event(event: &ProcessEvent) -> String {
    match event {
        ProcessEvent::InstructionExecuted {
            pid,
            instruction_pointer,
            instruction,
            stack,
        } => format!(
            "pid={pid} ip={instruction_pointer} instruction={instruction:?} stack={stack:?}"
        ),
        ProcessEvent::ProcessExited { pid, code } => format!("pid={pid} exited code={code}"),
        ProcessEvent::ProcessFailed { pid, error } => format!("pid={pid} failed error={error}"),
    }
}

fn format_process(process: &ProcessInfo) -> String {
    let state = match &process.state {
        ProcessState::Ready => "ready".to_owned(),
        ProcessState::Running => "running".to_owned(),
        ProcessState::Exited { code } => format!("exited({code})"),
        ProcessState::Failed { error } => format!("failed({error})"),
    };
    format!(
        "pid={} state={state} cwd={} argv={:?} ip={} stack={:?}",
        process.pid, process.cwd, process.argv, process.instruction_pointer, process.stack
    )
}

fn format_wait_status(status: &WaitStatus) -> String {
    match status {
        WaitStatus::Running => "running".to_owned(),
        WaitStatus::Exited { code } => format!("exited code={code}"),
        WaitStatus::Failed { error } => format!("failed error={error}"),
    }
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

fn optional_bool(arguments: &ToolArguments, name: &str) -> Option<bool> {
    match arguments.get(name) {
        Some(ToolValue::Boolean(value)) => Some(*value),
        _ => None,
    }
}

fn optional_integer(arguments: &ToolArguments, name: &str) -> Option<i32> {
    match arguments.get(name) {
        Some(ToolValue::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn required_pid(arguments: &ToolArguments, name: &str) -> Result<u32, ToolError> {
    let value = match arguments.get(name) {
        Some(ToolValue::Integer(value)) => *value,
        Some(_) => {
            return Err(ToolError::new(format!(
                "argument '{name}' must be an integer"
            )));
        }
        None => return Err(ToolError::new(format!("missing required argument: {name}"))),
    };
    u32::try_from(value)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| ToolError::new(format!("argument '{name}' must be a positive integer")))
}

fn optional_string_list(arguments: &ToolArguments, name: &str) -> Result<Vec<String>, ToolError> {
    let Some(value) = arguments.get(name) else {
        return Ok(Vec::new());
    };
    let ToolValue::List(values) = value else {
        return Err(ToolError::new(format!(
            "argument '{name}' must be a string list"
        )));
    };
    values
        .iter()
        .map(|value| match value {
            ToolValue::Text(value) => Ok(value.clone()),
            _ => Err(ToolError::new(format!(
                "argument '{name}' must contain only strings"
            ))),
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::{guest_coding_tool_registry, guest_tool_registry};
    use crate::agent::{ToolCall, ToolValue};
    use crate::runtime::VmInstance;

    fn call(
        id: &str,
        name: &str,
        arguments: impl IntoIterator<Item = (&'static str, ToolValue)>,
    ) -> ToolCall {
        ToolCall::new(
            id,
            name,
            arguments
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }

    #[test]
    fn guest_tools_share_state_without_touching_host_paths() {
        let mut registry = guest_tool_registry(VmInstance::new("guest-tools")).unwrap();

        let write = registry.execute(&call(
            "write",
            "guest_write_file",
            [
                ("path", ToolValue::Text("/workspace/hello.txt".to_owned())),
                ("content", ToolValue::Text("hello guest".to_owned())),
            ],
        ));
        assert!(!write.is_error);

        let read = registry.execute(&call(
            "read",
            "guest_read_file",
            [("path", ToolValue::Text("/workspace/hello.txt".to_owned()))],
        ));
        assert_eq!(read.content, "hello guest");

        let listing = registry.execute(&call(
            "list",
            "guest_list_files",
            [("path", ToolValue::Text("/workspace".to_owned()))],
        ));
        assert_eq!(listing.content, "file hello.txt size=11");

        let host_path = registry.execute(&call(
            "host",
            "guest_read_file",
            [("path", ToolValue::Text("/etc/hosts".to_owned()))],
        ));
        assert!(host_path.is_error);
        assert!(host_path.content.contains("path does not exist"));
    }

    #[test]
    fn guest_process_tools_spawn_run_and_wait() {
        let mut registry = guest_tool_registry(VmInstance::new("guest-processes")).unwrap();
        let spawn = registry.execute(&call(
            "spawn",
            "guest_spawn",
            [("program", ToolValue::Text("PUSH 7\nHALT".to_owned()))],
        ));
        assert_eq!(spawn.content, "spawned guest process pid=1");

        let run = registry.execute(&call(
            "run",
            "guest_run",
            [("max_ticks", ToolValue::Integer(4))],
        ));
        assert!(run.content.contains("pid=1 exited code=7"));

        let wait = registry.execute(&call(
            "wait",
            "guest_wait",
            [("pid", ToolValue::Integer(1))],
        ));
        assert_eq!(wait.content, "exited code=7");
    }

    #[test]
    fn guest_coding_registry_keeps_arithmetic_and_guest_tools_distinct() {
        let registry = guest_coding_tool_registry(VmInstance::new("composed")).unwrap();
        let names = registry
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"run_program".to_owned()));
        assert!(names.contains(&"guest_run".to_owned()));
        assert!(names.contains(&"guest_processes".to_owned()));
    }
}
