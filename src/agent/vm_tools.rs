//! Tools that let an agent compile, run, step, trace, and inspect VM programs.

use super::{
    Tool, ToolArguments, ToolError, ToolRegistry, ToolSpec, ensure_arguments, required_text,
};
use crate::{Instruction, StepResult, Vm, VmError};

#[derive(Debug, Default)]
struct VmToolState {
    program: Vec<Instruction>,
    vm: Vm,
}

/// Create the first set of tools that expose the VM to an agent.
pub fn vm_tool_registry() -> Result<ToolRegistry, ToolError> {
    let state = std::rc::Rc::new(std::cell::RefCell::new(VmToolState::default()));
    let mut registry = ToolRegistry::default();

    registry.register(CompileProgramTool)?;
    registry.register(TraceProgramTool)?;
    registry.register(RunProgramTool::new(state.clone()))?;
    registry.register(StepVmTool::new(state.clone()))?;
    registry.register(InspectVmTool::new(state.clone()))?;
    registry.register(DisassembleProgramTool::new(state.clone()))?;
    registry.register(ResetVmTool::new(state))?;

    Ok(registry)
}

type SharedVmToolState = std::rc::Rc<std::cell::RefCell<VmToolState>>;

struct RunProgramTool {
    state: SharedVmToolState,
}

struct CompileProgramTool;

impl Tool for CompileProgramTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "compile_program",
            "Validate a newline-delimited A/RVM program and show its instructions.",
            r#"{"type":"object","properties":{"program":{"type":"string"}},"required":["program"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["program"])?;
        let source = required_text(arguments, "program")?;
        let program = parse_program(source)?;
        Ok(format_program(&program))
    }
}

struct TraceProgramTool;

impl Tool for TraceProgramTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "trace_program",
            "Execute a newline-delimited A/RVM program and return every instruction with its stack.",
            r#"{"type":"object","properties":{"program":{"type":"string"}},"required":["program"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["program"])?;
        let source = required_text(arguments, "program")?;
        let program = parse_program(source)?;
        let program = crate::program::Program::new(program)
            .map_err(|error| ToolError::new(error.to_string()))?;
        let mut vm = Vm::new();
        let trace = vm.trace(&program).map_err(vm_tool_error)?;
        Ok(trace
            .into_iter()
            .map(|entry| match entry.result {
                Some(result) => format!(
                    "ip={} instruction=HALT result={result} stack={:?}",
                    entry.instruction_pointer, entry.stack
                ),
                None => format!(
                    "ip={} instruction={} stack={:?}",
                    entry.instruction_pointer,
                    format_instruction(entry.instruction),
                    entry.stack
                ),
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

impl RunProgramTool {
    fn new(state: SharedVmToolState) -> Self {
        Self { state }
    }
}

impl Tool for RunProgramTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "run_program",
            "Load and execute a newline-delimited A/RVM program.",
            r#"{"type":"object","properties":{"program":{"type":"string"}},"required":["program"]}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &["program"])?;
        let source = required_text(arguments, "program")?;
        let program = parse_program(source)?;
        let mut state = self.state.borrow_mut();
        state.program = program;
        let program = state.program.clone();
        let result = state.vm.run(&program).map_err(vm_tool_error)?;
        Ok(format!(
            "result={result} ip={} stack={:?}",
            state.vm.instruction_pointer(),
            state.vm.stack()
        ))
    }
}

struct StepVmTool {
    state: SharedVmToolState,
}

impl StepVmTool {
    fn new(state: SharedVmToolState) -> Self {
        Self { state }
    }
}

impl Tool for StepVmTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "step_vm",
            "Execute one instruction from the loaded program.",
            r#"{"type":"object","properties":{}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &[])?;
        let mut state = self.state.borrow_mut();
        let program = state.program.clone();
        let result = state.vm.step(&program).map_err(vm_tool_error)?;
        match result {
            StepResult::Executed { instruction } => Ok(format!(
                "executed={} ip={} stack={:?}",
                format_instruction(instruction),
                state.vm.instruction_pointer(),
                state.vm.stack()
            )),
            StepResult::Halted { result } => Ok(format!(
                "halted result={result} ip={} stack={:?}",
                state.vm.instruction_pointer(),
                state.vm.stack()
            )),
        }
    }
}

struct InspectVmTool {
    state: SharedVmToolState,
}

impl InspectVmTool {
    fn new(state: SharedVmToolState) -> Self {
        Self { state }
    }
}

impl Tool for InspectVmTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "inspect_vm",
            "Inspect the loaded program's instruction pointer and value stack.",
            r#"{"type":"object","properties":{}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &[])?;
        let state = self.state.borrow();
        Ok(format!(
            "loaded={} ip={} halted={} stack={:?}",
            !state.program.is_empty(),
            state.vm.instruction_pointer(),
            state.vm.is_halted(),
            state.vm.stack()
        ))
    }
}

struct DisassembleProgramTool {
    state: SharedVmToolState,
}

impl DisassembleProgramTool {
    fn new(state: SharedVmToolState) -> Self {
        Self { state }
    }
}

impl Tool for DisassembleProgramTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "disassemble_program",
            "Show the loaded program with instruction offsets.",
            r#"{"type":"object","properties":{}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &[])?;
        let state = self.state.borrow();
        if state.program.is_empty() {
            return Err(ToolError::new("no program is loaded"));
        }

        Ok(state
            .program
            .iter()
            .enumerate()
            .map(|(index, instruction)| format!("{index:02} {}", format_instruction(*instruction)))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

struct ResetVmTool {
    state: SharedVmToolState,
}

impl ResetVmTool {
    fn new(state: SharedVmToolState) -> Self {
        Self { state }
    }
}

impl Tool for ResetVmTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "reset_vm",
            "Reset the loaded VM to its initial state.",
            r#"{"type":"object","properties":{}}"#,
        )
    }

    fn execute(&mut self, arguments: &ToolArguments) -> Result<String, ToolError> {
        ensure_arguments(arguments, &[])?;
        let mut state = self.state.borrow_mut();
        state.vm.reset();
        Ok("vm reset".to_owned())
    }
}

pub(crate) fn parse_program(source: &str) -> Result<Vec<Instruction>, ToolError> {
    source
        .parse::<crate::program::Program>()
        .map(|program| program.instructions().to_vec())
        .map_err(|error| ToolError::new(error.to_string()))
}

fn format_instruction(instruction: Instruction) -> String {
    instruction.to_string()
}

fn format_program(program: &[Instruction]) -> String {
    format!(
        "instructions={}\n{}",
        program.len(),
        program
            .iter()
            .enumerate()
            .map(|(index, instruction)| format!("{index:02} {}", format_instruction(*instruction)))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn vm_tool_error(error: VmError) -> ToolError {
    ToolError::new(format!("vm error: {error:?}"))
}
