//! Load and run validated module VM images from files or standard input.

use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fmt,
    fs::File,
    io::{self, Read},
};

use a_rust_vm::{
    agent::{ToolArguments, ToolCall, ToolValue},
    module::{MAX_MODULE_HEAP_BYTES, ModuleVm, OwnedValue},
    module_assembly::assemble_module,
    module_format::{MAX_MODULE_SOURCE_BYTES, parse_module},
    workspace::workspace_tool_registry,
};

const MAX_MODULE_INPUT_BYTES: usize = 8 * 1024 * 1024;
const USAGE: &str = "usage: arvm module check <file|->\n       arvm module run <file|-> [--input <file|->] [--import-replies <file|->] [--allow-import workspace.read_file --workspace <dir>] [--heap-bytes <n>]";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportReply {
    name: String,
    arguments: Vec<OwnedValue>,
    result: OwnedValue,
}

#[derive(Debug)]
pub(crate) struct ModuleCommandError {
    message: String,
    code: i32,
}

impl ModuleCommandError {
    fn input(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 2,
        }
    }

    fn runtime(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 1,
        }
    }

    pub(crate) fn code(&self) -> i32 {
        self.code
    }
}

impl fmt::Display for ModuleCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn read_limited(path: &str, limit: usize) -> Result<Vec<u8>, ModuleCommandError> {
    let reader: Box<dyn Read> =
        if path == "-" {
            Box::new(io::stdin())
        } else {
            Box::new(File::open(path).map_err(|error| {
                ModuleCommandError::input(format!("cannot open '{path}': {error}"))
            })?)
        };
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| ModuleCommandError::input(format!("cannot read '{path}': {error}")))?;
    if bytes.len() > limit {
        return Err(ModuleCommandError::input(format!(
            "'{path}' exceeds {limit} bytes"
        )));
    }
    Ok(bytes)
}

pub(crate) fn run_module_command(arguments: &[String]) -> Result<(), ModuleCommandError> {
    let command = arguments
        .first()
        .map(String::as_str)
        .ok_or_else(|| ModuleCommandError::input(USAGE))?;
    if !matches!(command, "check" | "run") {
        return Err(ModuleCommandError::input(USAGE));
    }
    let mut source_path = None;
    let mut input_path = None;
    let mut replies_path = None;
    let mut live_import = false;
    let mut workspace_path = None;
    let mut heap_limit = MAX_MODULE_HEAP_BYTES;
    let mut heap_limit_set = false;
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--input" if command == "run" && input_path.is_none() => {
                index += 1;
                input_path = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| ModuleCommandError::input(USAGE))?
                        .as_str(),
                );
            }
            "--import-replies" if command == "run" && replies_path.is_none() => {
                index += 1;
                replies_path = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| ModuleCommandError::input(USAGE))?
                        .as_str(),
                );
            }
            "--allow-import" if command == "run" && !live_import => {
                index += 1;
                if arguments.get(index).map(String::as_str) != Some("workspace.read_file") {
                    return Err(ModuleCommandError::input(
                        "only workspace.read_file is available as a live import",
                    ));
                }
                live_import = true;
            }
            "--workspace" if command == "run" && workspace_path.is_none() => {
                index += 1;
                workspace_path = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| ModuleCommandError::input(USAGE))?
                        .as_str(),
                );
            }
            "--heap-bytes" if command == "run" && !heap_limit_set => {
                index += 1;
                heap_limit = arguments
                    .get(index)
                    .ok_or_else(|| ModuleCommandError::input(USAGE))?
                    .parse::<usize>()
                    .map_err(|_| {
                        ModuleCommandError::input("--heap-bytes must be a nonnegative integer")
                    })?;
                if heap_limit > MAX_MODULE_HEAP_BYTES {
                    return Err(ModuleCommandError::input(format!(
                        "--heap-bytes exceeds {MAX_MODULE_HEAP_BYTES}"
                    )));
                }
                heap_limit_set = true;
            }
            path if !path.starts_with("--") && source_path.is_none() => source_path = Some(path),
            _ => return Err(ModuleCommandError::input(USAGE)),
        }
        index += 1;
    }
    let source_path = source_path.ok_or_else(|| ModuleCommandError::input(USAGE))?;
    if (live_import != workspace_path.is_some()) || (live_import && replies_path.is_some()) {
        return Err(ModuleCommandError::input(
            "live imports require --allow-import workspace.read_file and --workspace <dir>, without --import-replies",
        ));
    }
    if [Some(source_path), input_path, replies_path]
        .into_iter()
        .filter(|path| *path == Some("-"))
        .count()
        > 1
    {
        return Err(ModuleCommandError::input(
            "only one module, input, or import replies file can use standard input",
        ));
    }
    let source = read_limited(source_path, MAX_MODULE_SOURCE_BYTES)?;
    let module = if source
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        == Some(b'{')
    {
        parse_module(&source).map_err(|error| ModuleCommandError::input(error.to_string()))?
    } else {
        let text = std::str::from_utf8(&source).map_err(|error| {
            ModuleCommandError::input(format!("invalid UTF-8 module source: {error}"))
        })?;
        assemble_module(text).map_err(|error| ModuleCommandError::input(error.to_string()))?
    };
    if command == "check" {
        let instructions: usize = module
            .functions()
            .iter()
            .map(|function| function.code().len())
            .sum();
        println!(
            "valid: {} functions, {instructions} instructions, {} inputs",
            module.functions().len(),
            module.input_count()
        );
        return Ok(());
    }
    let inputs: Vec<OwnedValue> = match input_path {
        Some(path) => serde_json::from_slice(&read_limited(path, MAX_MODULE_INPUT_BYTES)?)
            .map_err(|error| ModuleCommandError::input(format!("invalid input JSON: {error}")))?,
        None => Vec::new(),
    };
    let replies: Vec<ImportReply> = match replies_path {
        Some(path) => serde_json::from_slice(&read_limited(path, MAX_MODULE_INPUT_BYTES)?)
            .map_err(|error| {
                ModuleCommandError::input(format!("invalid import replies JSON: {error}"))
            })?,
        None => Vec::new(),
    };
    let mut grants: Vec<String> = replies
        .iter()
        .map(|reply| reply.name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut workspace_tools = match workspace_path {
        Some(path) => {
            grants.push("workspace.read_file".to_owned());
            Some(
                workspace_tool_registry(path)
                    .map_err(|error| ModuleCommandError::input(error.to_string()))?,
            )
        }
        None => None,
    };
    let mut vm = ModuleVm::with_inputs_and_imports(&module, inputs, heap_limit, &grants)
        .map_err(|error| ModuleCommandError::input(format!("invalid module input: {error}")))?;
    let mut next_reply = 0;
    while vm.result().is_none() {
        vm.step()
            .map_err(|error| ModuleCommandError::runtime(format!("module VM error: {error}")))?;
        if let Some(request) = vm
            .pending_import()
            .map_err(|error| ModuleCommandError::runtime(format!("module VM error: {error}")))?
        {
            if let Some(tools) = workspace_tools.as_mut() {
                let [OwnedValue::Text(path)] = request.arguments.as_slice() else {
                    return Err(ModuleCommandError::runtime(
                        "workspace.read_file requires one text path",
                    ));
                };
                let arguments: ToolArguments = [("path".to_owned(), ToolValue::Text(path.clone()))]
                    .into_iter()
                    .collect();
                let response = tools.execute(&ToolCall::new(
                    next_reply.to_string(),
                    "read_file",
                    arguments,
                ));
                if response.is_error {
                    return Err(ModuleCommandError::runtime(format!(
                        "workspace.read_file: {}",
                        response.content
                    )));
                }
                vm.resume_import(&OwnedValue::Text(response.content))
                    .map_err(|error| {
                        ModuleCommandError::runtime(format!("invalid import result: {error}"))
                    })?;
                continue;
            }
            let reply = replies.get(next_reply).ok_or_else(|| {
                ModuleCommandError::runtime(format!("no reply for import '{}'", request.name))
            })?;
            if reply.name != request.name || reply.arguments != request.arguments {
                return Err(ModuleCommandError::runtime(format!(
                    "import reply {next_reply} does not match request '{}'",
                    request.name
                )));
            }
            vm.resume_import(&reply.result).map_err(|error| {
                ModuleCommandError::runtime(format!("invalid import result: {error}"))
            })?;
            next_reply += 1;
        }
    }
    if next_reply != replies.len() {
        return Err(ModuleCommandError::input(format!(
            "{} unused import replies",
            replies.len() - next_reply
        )));
    }
    let value = vm.result().expect("loop stopped at a result");
    let result = vm
        .resolve(value)
        .map_err(|error| ModuleCommandError::runtime(format!("cannot resolve result: {error}")))?;
    let output = serde_json::json!({
        "result": result,
        "steps": vm.steps(),
        "heap_bytes": vm.heap_bytes(),
    });
    println!("{output}");
    Ok(())
}
