//! Load and run validated module VM images from files or standard input.

use std::{
    fmt,
    fs::File,
    io::{self, Read},
};

use a_rust_vm::{
    module::{MAX_MODULE_HEAP_BYTES, ModuleVm, OwnedValue},
    module_format::{MAX_MODULE_SOURCE_BYTES, parse_module},
};

const MAX_MODULE_INPUT_BYTES: usize = 8 * 1024 * 1024;
const USAGE: &str = "usage: arvm module check <file|->\n       arvm module run <file|-> [--input <file|->] [--heap-bytes <n>]";

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
    if source_path == "-" && input_path == Some("-") {
        return Err(ModuleCommandError::input(
            "module and input cannot both use standard input",
        ));
    }
    let source = read_limited(source_path, MAX_MODULE_SOURCE_BYTES)?;
    let module =
        parse_module(&source).map_err(|error| ModuleCommandError::input(error.to_string()))?;
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
    let mut vm = ModuleVm::with_inputs(&module, inputs, heap_limit)
        .map_err(|error| ModuleCommandError::input(format!("invalid module input: {error}")))?;
    let value = vm
        .run()
        .map_err(|error| ModuleCommandError::runtime(format!("module VM error: {error}")))?;
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
