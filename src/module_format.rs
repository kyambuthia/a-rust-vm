//! Bounded JSON image for loading validated module VM programs.
//!
//! Every decoded image passes through `Module::with_values`; deserialization
//! never constructs an executable module directly.
//!
//! Instructions are lowercase names (`"add"`) or single-key operands
//! (`{"const": 42}`). Values use explicit variants (`{"integer": 42}`,
//! `{"text": "key"}`, `{"list": [...]}`, `{"map": {...}}`). A complete image:
//!
//! ```json
//! {"version":2,"entry":0,"functions":[
//!   {"arity":0,"locals":0,"code":[{"const":42},"return"]}
//! ]}
//! ```

use std::fmt;

use serde::Deserialize;

use crate::module::{Function, Module, ModuleError, Op, OwnedValue};

pub const MAX_MODULE_SOURCE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleImage {
    version: u16,
    entry: usize,
    #[serde(default)]
    input_count: usize,
    #[serde(default)]
    constants: Vec<OwnedValue>,
    functions: Vec<FunctionImage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FunctionImage {
    arity: usize,
    locals: usize,
    code: Vec<Op>,
}

#[derive(Debug)]
pub enum ModuleSourceError {
    TooLarge,
    Json(serde_json::Error),
    Invalid(ModuleError),
}

impl fmt::Display for ModuleSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(
                formatter,
                "module source exceeds {MAX_MODULE_SOURCE_BYTES} bytes"
            ),
            Self::Json(error) => write!(formatter, "invalid module JSON: {error}"),
            Self::Invalid(error) => write!(formatter, "invalid module: {error}"),
        }
    }
}

impl std::error::Error for ModuleSourceError {}

/// Parse a typed module image and validate all functions and constants.
pub fn parse_module(source: &[u8]) -> Result<Module, ModuleSourceError> {
    if source.len() > MAX_MODULE_SOURCE_BYTES {
        return Err(ModuleSourceError::TooLarge);
    }
    let image: ModuleImage = serde_json::from_slice(source).map_err(ModuleSourceError::Json)?;
    let functions = image
        .functions
        .into_iter()
        .map(|function| Function::new(function.arity, function.locals, function.code))
        .collect();
    Module::with_values(
        image.version,
        functions,
        image.entry,
        image.constants,
        image.input_count,
    )
    .map_err(ModuleSourceError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{ModuleVm, Value};

    #[test]
    fn loads_and_executes_typed_module_image() {
        let source = br#"{
            "version": 2,
            "entry": 0,
            "input_count": 1,
            "constants": [{"text":"answer"}],
            "functions": [{
                "arity": 0,
                "locals": 0,
                "code": ["map_new", {"const_value":0}, {"input":0}, "map_insert", "return"]
            }]
        }"#;
        let module = parse_module(source).unwrap();
        let mut vm = ModuleVm::with_inputs(&module, vec![OwnedValue::Integer(42)], 1024).unwrap();
        let result = vm.run().unwrap();
        assert!(matches!(result, Value::Heap(_)));
        assert_eq!(
            vm.resolve(result).unwrap(),
            OwnedValue::Map(std::collections::BTreeMap::from([(
                "answer".to_owned(),
                OwnedValue::Integer(42),
            )]))
        );
    }

    #[test]
    fn rejects_oversized_malformed_and_invalid_images() {
        assert!(matches!(
            parse_module(&vec![b' '; MAX_MODULE_SOURCE_BYTES + 1]),
            Err(ModuleSourceError::TooLarge)
        ));
        assert!(matches!(
            parse_module(br#"{"version":2,"unknown":1}"#),
            Err(ModuleSourceError::Json(_))
        ));
        assert!(matches!(
            parse_module(br#"{"version":2,"entry":0,"functions":[{"arity":0,"locals":0,"code":[{"jump":9},"return"]}]}"#),
            Err(ModuleSourceError::Invalid(_))
        ));
        assert!(matches!(
            parse_module(br#"{"version":3,"entry":0,"functions":[{"arity":0,"locals":0,"code":[{"const":1},"return"]}]}"#),
            Err(ModuleSourceError::Invalid(_))
        ));
    }
}
