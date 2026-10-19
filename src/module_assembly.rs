//! Text source for validated module programs.
//!
//! `entry main`, `inputs 1`, `const key {"text":"answer"}`,
//! `fn main 0 0`, instructions, and `end` form a complete program.
//! Function names and labels resolve in a second pass, including forward calls.

use std::{collections::BTreeMap, fmt};

use crate::module::{Function, Import, MODULE_VERSION, Module, Op, OwnedValue};
use crate::module_format::MAX_MODULE_SOURCE_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssemblyError {
    pub line: Option<usize>,
    pub message: String,
}

impl AssemblyError {
    fn at(line: usize, message: impl Into<String>) -> Self {
        Self {
            line: Some(line),
            message: message.into(),
        }
    }

    fn general(message: impl Into<String>) -> Self {
        Self {
            line: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for AssemblyError {}

struct SourceFunction {
    name: String,
    line: usize,
    arity: usize,
    locals: usize,
    labels: BTreeMap<String, usize>,
    code: Vec<(usize, String, Option<String>)>,
}

fn identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..index],
            _ => {}
        }
    }
    line
}

fn number<T: std::str::FromStr>(text: &str, line: usize, kind: &str) -> Result<T, AssemblyError> {
    text.parse()
        .map_err(|_| AssemblyError::at(line, format!("{kind} must be a number")))
}

/// Compile human-readable module assembly through the VM's full validator.
pub fn assemble_module(source: &str) -> Result<Module, AssemblyError> {
    if source.len() > MAX_MODULE_SOURCE_BYTES {
        return Err(AssemblyError::general(format!(
            "module source exceeds {MAX_MODULE_SOURCE_BYTES} bytes"
        )));
    }
    let mut entry = None;
    let mut input_count = None;
    let mut constants = Vec::new();
    let mut constant_names = BTreeMap::new();
    let mut imports = Vec::new();
    let mut import_names = BTreeMap::new();
    let mut functions: Vec<SourceFunction> = Vec::new();
    let mut function_names = BTreeMap::new();
    let mut current: Option<SourceFunction> = None;
    for (offset, raw) in source.lines().enumerate() {
        let line_number = offset + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if let Some(function) = current.as_mut() {
            if line == "end" {
                let function = current.take().expect("function exists");
                functions.push(function);
            } else if let Some(label) = line.strip_suffix(':') {
                if !identifier(label)
                    || function
                        .labels
                        .insert(label.to_owned(), function.code.len())
                        .is_some()
                {
                    return Err(AssemblyError::at(line_number, "invalid or duplicate label"));
                }
            } else {
                if fields.len() > 2 {
                    return Err(AssemblyError::at(
                        line_number,
                        "instruction accepts at most one operand",
                    ));
                }
                function.code.push((
                    line_number,
                    fields[0].to_owned(),
                    fields.get(1).map(|s| (*s).to_owned()),
                ));
            }
            continue;
        }
        match fields.as_slice() {
            ["entry", name] if identifier(name) && entry.is_none() => {
                entry = Some((*name).to_owned())
            }
            ["inputs", count] if input_count.is_none() => {
                input_count = Some(number(count, line_number, "input count")?)
            }
            ["fn", name, arity, locals] if identifier(name) => {
                if function_names
                    .insert((*name).to_owned(), functions.len())
                    .is_some()
                {
                    return Err(AssemblyError::at(line_number, "duplicate function name"));
                }
                current = Some(SourceFunction {
                    name: (*name).to_owned(),
                    line: line_number,
                    arity: number(arity, line_number, "arity")?,
                    locals: number(locals, line_number, "local count")?,
                    labels: BTreeMap::new(),
                    code: Vec::new(),
                });
            }
            ["import", name, arity] => {
                if import_names
                    .insert((*name).to_owned(), imports.len())
                    .is_some()
                {
                    return Err(AssemblyError::at(line_number, "duplicate import name"));
                }
                imports.push(Import {
                    name: (*name).to_owned(),
                    arity: number(arity, line_number, "import arity")?,
                });
            }
            ["const", name, ..] if identifier(name) => {
                if constant_names.contains_key(*name) {
                    return Err(AssemblyError::at(line_number, "duplicate constant name"));
                }
                let json = line
                    .strip_prefix("const")
                    .expect("matched declaration")
                    .trim_start()
                    .strip_prefix(name)
                    .expect("matched constant name")
                    .trim_start();
                if json.is_empty() {
                    return Err(AssemblyError::at(
                        line_number,
                        "constant needs a JSON value",
                    ));
                }
                let value: OwnedValue = serde_json::from_str(json).map_err(|e| {
                    AssemblyError::at(line_number, format!("invalid constant JSON: {e}"))
                })?;
                constant_names.insert((*name).to_owned(), constants.len());
                constants.push(value);
            }
            _ => {
                return Err(AssemblyError::at(
                    line_number,
                    "expected entry, inputs, const, import, or fn declaration",
                ));
            }
        }
    }
    if let Some(function) = current {
        return Err(AssemblyError::general(format!(
            "function '{}' is missing end",
            function.name
        )));
    }
    let entry = entry.ok_or_else(|| AssemblyError::general("missing entry declaration"))?;
    let entry = *function_names
        .get(&entry)
        .ok_or_else(|| AssemblyError::general("entry names an unknown function"))?;
    let mut compiled = Vec::with_capacity(functions.len());
    for function in &functions {
        let mut code = Vec::with_capacity(function.code.len());
        for (line, mnemonic, operand) in &function.code {
            let op = match (mnemonic.as_str(), operand.as_deref()) {
                ("const", Some(value)) => Op::Const(number(value, *line, "integer constant")?),
                ("const_value", Some(name)) => {
                    Op::ConstValue(*constant_names.get(name).ok_or_else(|| {
                        AssemblyError::at(*line, format!("unknown constant '{name}'"))
                    })?)
                }
                ("input", Some(value)) => Op::Input(number(value, *line, "input index")?),
                ("local_get", Some(value)) => Op::LocalGet(number(value, *line, "local index")?),
                ("local_take", Some(value)) => Op::LocalTake(number(value, *line, "local index")?),
                ("local_set", Some(value)) => Op::LocalSet(number(value, *line, "local index")?),
                ("jump", Some(label)) => {
                    Op::Jump(*function.labels.get(label).ok_or_else(|| {
                        AssemblyError::at(*line, format!("unknown label '{label}'"))
                    })?)
                }
                ("jump_if_zero", Some(label)) => {
                    Op::JumpIfZero(*function.labels.get(label).ok_or_else(|| {
                        AssemblyError::at(*line, format!("unknown label '{label}'"))
                    })?)
                }
                ("call", Some(name)) => Op::Call(*function_names.get(name).ok_or_else(|| {
                    AssemblyError::at(*line, format!("unknown function '{name}'"))
                })?),
                ("import", Some(name)) => {
                    Op::Import(*import_names.get(name).ok_or_else(|| {
                        AssemblyError::at(*line, format!("unknown import '{name}'"))
                    })?)
                }
                ("add", None) => Op::Add,
                ("sub", None) => Op::Sub,
                ("mul", None) => Op::Mul,
                ("div", None) => Op::Div,
                ("eq", None) => Op::Eq,
                ("lt", None) => Op::Lt,
                ("list_new", None) => Op::ListNew,
                ("list_push", None) => Op::ListPush,
                ("list_get", None) => Op::ListGet,
                ("list_len", None) => Op::ListLen,
                ("map_new", None) => Op::MapNew,
                ("map_insert", None) => Op::MapInsert,
                ("map_get", None) => Op::MapGet,
                ("return", None) => Op::Return,
                _ => {
                    return Err(AssemblyError::at(
                        *line,
                        format!("unknown instruction or wrong operand: {mnemonic}"),
                    ));
                }
            };
            code.push(op);
        }
        compiled.push(Function::new(function.arity, function.locals, code));
    }
    Module::with_imports(
        MODULE_VERSION,
        compiled,
        entry,
        constants,
        input_count.unwrap_or(0),
        imports,
    )
    .map_err(|e| {
        let line = e
            .function
            .and_then(|index| functions.get(index))
            .map(|function| {
                e.instruction
                    .and_then(|index| function.code.get(index))
                    .map_or(function.line, |instruction| instruction.0)
            });
        AssemblyError {
            line,
            message: format!("invalid module: {e}"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{ModuleVm, Value};

    #[test]
    fn resolves_forward_functions_labels_and_named_constants() {
        let source = r#"
            entry main
            inputs 1
            const   key   {"text":"answer#1"}
            fn main 0 1
              input 0
              jump body
              const 0
            body:
              call double
              local_set 0
              map_new
              const_value key
              local_get 0
              map_insert
              return
            end
            fn double 1 1
              local_get 0
              const 2
              mul
              return
            end
        "#;
        let module = assemble_module(source).unwrap();
        assert_eq!(module.functions().len(), 2);
        assert_eq!(module.functions()[0].code()[1], Op::Jump(3));
        assert_eq!(module.functions()[0].code()[3], Op::Call(1));
        let mut vm = ModuleVm::with_inputs(&module, vec![OwnedValue::Integer(21)], 1024).unwrap();
        let result = vm.run().unwrap();
        assert!(matches!(result, Value::Heap(_)));
        assert_eq!(
            vm.resolve(result).unwrap(),
            OwnedValue::Map(BTreeMap::from([(
                "answer#1".to_owned(),
                OwnedValue::Integer(42)
            )]))
        );
    }

    #[test]
    fn rejects_unknown_symbols_and_invalid_programs() {
        let source = "entry main\nfn main 0 0\n jump missing\n return\nend";
        let error = assemble_module(source).unwrap_err();
        assert_eq!(error.line, Some(3));
        assert!(error.message.contains("unknown label"));
        let source = "entry main\nfn main 0 0\n add\n return\nend";
        let error = assemble_module(source).unwrap_err();
        assert_eq!(error.line, Some(3));
        assert!(error.message.contains("invalid module"));
        let source = "entry main\nfn main 0 0\n const 1\n";
        assert!(
            assemble_module(source)
                .unwrap_err()
                .message
                .contains("missing end")
        );
    }
}
