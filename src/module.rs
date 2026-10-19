//! Validated multi-function programs for the next A/RVM execution format.
//!
//! The legacy assembly VM remains available through crate::program. A module
//! is validated as a whole before its instructions can be executed.

use std::fmt;

pub const MODULE_VERSION: u16 = 1;
pub const MAX_FUNCTIONS: usize = 1_024;
pub const MAX_MODULE_INSTRUCTIONS: usize = 65_536;
pub const MAX_LOCALS_PER_FUNCTION: usize = 256;
pub const MAX_OPERAND_STACK: usize = 1_024;
pub const MAX_CALL_FRAMES: usize = 128;
pub const MAX_LIVE_SLOTS: usize = 16_384;
pub const MAX_MODULE_STEPS: usize = 1_000_000;
pub const MAX_MODULE_VALIDATION_VISITS: usize = 1_000_000;

/// An instruction in a validated module function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Const(i64),
    LocalGet(usize),
    LocalSet(usize),
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Lt,
    Jump(usize),
    JumpIfZero(usize),
    Call(usize),
    Return,
}

/// A function body and its fixed local-slot layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    arity: usize,
    local_count: usize,
    code: Vec<Op>,
}

impl Function {
    pub fn new(arity: usize, local_count: usize, code: Vec<Op>) -> Self {
        Self {
            arity,
            local_count,
            code,
        }
    }

    pub fn arity(&self) -> usize {
        self.arity
    }

    pub fn local_count(&self) -> usize {
        self.local_count
    }

    pub fn code(&self) -> &[Op] {
        &self.code
    }
}

/// A complete module that passed structural and control-flow validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    version: u16,
    functions: Vec<Function>,
    entry: usize,
    max_operand_stack: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleError {
    pub function: Option<usize>,
    pub instruction: Option<usize>,
    pub message: String,
}

impl ModuleError {
    fn new(
        function: Option<usize>,
        instruction: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            function,
            instruction,
            message: message.into(),
        }
    }
}

impl fmt::Display for ModuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.function, self.instruction) {
            (Some(function), Some(instruction)) => {
                write!(
                    formatter,
                    "function {function} instruction {instruction}: {}",
                    self.message
                )
            }
            (Some(function), None) => write!(formatter, "function {function}: {}", self.message),
            _ => formatter.write_str(&self.message),
        }
    }
}

impl std::error::Error for ModuleError {}

impl Module {
    pub fn new(version: u16, functions: Vec<Function>, entry: usize) -> Result<Self, ModuleError> {
        if version != MODULE_VERSION {
            return Err(ModuleError::new(
                None,
                None,
                format!("unsupported module version {version}"),
            ));
        }
        if functions.is_empty() || functions.len() > MAX_FUNCTIONS {
            return Err(ModuleError::new(
                None,
                None,
                format!("function count must be between 1 and {MAX_FUNCTIONS}"),
            ));
        }
        if entry >= functions.len() {
            return Err(ModuleError::new(
                None,
                None,
                format!("entry function {entry} is missing"),
            ));
        }
        if functions[entry].arity != 0 {
            return Err(ModuleError::new(
                Some(entry),
                None,
                "entry function must have no parameters",
            ));
        }
        let mut total_instructions = 0usize;
        for (function_index, function) in functions.iter().enumerate() {
            if function.local_count > MAX_LOCALS_PER_FUNCTION
                || function.arity > function.local_count
            {
                return Err(ModuleError::new(
                    Some(function_index),
                    None,
                    format!(
                        "local count must include parameters and be at most {MAX_LOCALS_PER_FUNCTION}"
                    ),
                ));
            }
            if function.code.is_empty() {
                return Err(ModuleError::new(
                    Some(function_index),
                    None,
                    "function is empty",
                ));
            }
            total_instructions = total_instructions.saturating_add(function.code.len());
            if total_instructions > MAX_MODULE_INSTRUCTIONS {
                return Err(ModuleError::new(
                    None,
                    None,
                    format!("module exceeds {MAX_MODULE_INSTRUCTIONS} instructions"),
                ));
            }
            if !matches!(function.code.last(), Some(Op::Return)) {
                return Err(ModuleError::new(
                    Some(function_index),
                    None,
                    "function must end with RETURN",
                ));
            }
            for (ip, op) in function.code.iter().enumerate() {
                match op {
                    Op::LocalGet(slot) | Op::LocalSet(slot) if *slot >= function.local_count => {
                        return Err(ModuleError::new(
                            Some(function_index),
                            Some(ip),
                            format!("local slot {slot} is out of range"),
                        ));
                    }
                    Op::Jump(target) | Op::JumpIfZero(target) if *target >= function.code.len() => {
                        return Err(ModuleError::new(
                            Some(function_index),
                            Some(ip),
                            format!("branch target {target} is out of range"),
                        ));
                    }
                    Op::Call(target) if *target >= functions.len() => {
                        return Err(ModuleError::new(
                            Some(function_index),
                            Some(ip),
                            format!("function {target} is missing"),
                        ));
                    }
                    _ => {}
                }
            }
        }
        let mut validation_visits = MAX_MODULE_VALIDATION_VISITS;
        let max_operand_stack = functions
            .iter()
            .enumerate()
            .map(|(index, function)| {
                validate_function(index, function, &functions, &mut validation_visits)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            version,
            functions,
            entry,
            max_operand_stack,
        })
    }

    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    pub fn version(&self) -> u16 {
        self.version
    }

    pub fn entry(&self) -> usize {
        self.entry
    }

    pub fn max_operand_stack(&self, function: usize) -> Option<usize> {
        self.max_operand_stack.get(function).copied()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Initialized([u64; 4]);

impl Initialized {
    fn parameters(arity: usize) -> Self {
        let mut result = Self([0; 4]);
        for slot in 0..arity {
            result.mark(slot);
        }
        result
    }

    fn mark(&mut self, slot: usize) {
        self.0[slot / 64] |= 1u64 << (slot % 64);
    }

    fn contains(self, slot: usize) -> bool {
        self.0[slot / 64] & (1u64 << (slot % 64)) != 0
    }

    fn intersection(self, other: Self) -> Self {
        let mut result = self;
        for (word, other_word) in result.0.iter_mut().zip(other.0) {
            *word &= other_word;
        }
        result
    }
}

fn validate_function(
    index: usize,
    function: &Function,
    functions: &[Function],
    visits_remaining: &mut usize,
) -> Result<usize, ModuleError> {
    // At a control-flow join, local initialization is the intersection of
    // incoming paths. Revisit a successor if that intersection shrinks.
    let mut states: Vec<Option<(usize, Initialized)>> = vec![None; function.code.len()];
    let mut worklist = vec![(0usize, 0usize, Initialized::parameters(function.arity))];
    let mut max_stack = 0usize;
    while let Some((ip, depth, incoming)) = worklist.pop() {
        if *visits_remaining == 0 {
            return Err(ModuleError::new(
                Some(index),
                Some(ip),
                "module validation visit limit exceeded",
            ));
        }
        *visits_remaining -= 1;
        let merged = match states[ip] {
            Some((known_depth, known)) => {
                if known_depth != depth {
                    return Err(ModuleError::new(
                        Some(index),
                        Some(ip),
                        format!("stack depth at join differs: {known_depth} and {depth}"),
                    ));
                }
                let merged = known.intersection(incoming);
                if merged == known {
                    continue;
                }
                merged
            }
            None => incoming,
        };
        states[ip] = Some((depth, merged));
        let op = function.code[ip];
        let required = match op {
            Op::Const(_) | Op::LocalGet(_) | Op::Jump(_) => 0,
            Op::LocalSet(_) | Op::JumpIfZero(_) | Op::Return => 1,
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Eq | Op::Lt => 2,
            Op::Call(target) => functions[target].arity,
        };
        if depth < required {
            return Err(ModuleError::new(
                Some(index),
                Some(ip),
                format!("instruction needs {required} operands, has {depth}"),
            ));
        }
        if let Op::LocalGet(slot) = op
            && !merged.contains(slot)
        {
            return Err(ModuleError::new(
                Some(index),
                Some(ip),
                format!("local slot {slot} may be uninitialized"),
            ));
        }
        if matches!(op, Op::Return) && depth != 1 {
            return Err(ModuleError::new(
                Some(index),
                Some(ip),
                format!("RETURN needs exactly one operand, has {depth}"),
            ));
        }
        let next_depth = match op {
            Op::Const(_) | Op::LocalGet(_) => depth + 1,
            Op::LocalSet(_) | Op::JumpIfZero(_) => depth - 1,
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Eq | Op::Lt => depth - 1,
            Op::Call(_) => depth - required + 1,
            Op::Jump(_) | Op::Return => depth,
        };
        max_stack = max_stack.max(next_depth);
        if max_stack > MAX_OPERAND_STACK {
            return Err(ModuleError::new(
                Some(index),
                Some(ip),
                format!("operand stack exceeds {MAX_OPERAND_STACK} values"),
            ));
        }
        let mut outgoing = merged;
        if let Op::LocalSet(slot) = op {
            outgoing.mark(slot);
        }
        match op {
            Op::Return => {}
            Op::Jump(target) => worklist.push((target, next_depth, outgoing)),
            Op::JumpIfZero(target) => {
                worklist.push((target, next_depth, outgoing));
                worklist.push((ip + 1, next_depth, outgoing));
            }
            _ => worklist.push((ip + 1, next_depth, outgoing)),
        }
    }
    Ok(max_stack)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleVmError {
    AlreadyFinished,
    OperandStackUnderflow,
    UninitializedLocal,
    DivisionByZero,
    IntegerOverflow,
    OperandStackLimit,
    CallFrameLimit,
    LiveSlotLimit,
    StepLimit,
}

impl fmt::Display for ModuleVmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for ModuleVmError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleStep {
    pub function: usize,
    pub instruction_pointer: usize,
    pub instruction: Op,
    pub result: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    function: usize,
    ip: usize,
    locals: Vec<Option<i64>>,
    stack: Vec<i64>,
}

/// An owned debugger view of one call frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameSnapshot {
    pub function: usize,
    pub instruction_pointer: usize,
    pub locals: Vec<Option<i64>>,
    pub stack: Vec<i64>,
}

/// A stepwise, bounded executor for a validated module.
#[derive(Debug)]
pub struct ModuleVm<'a> {
    module: &'a Module,
    frames: Vec<Frame>,
    steps: usize,
    result: Option<i64>,
}

impl<'a> ModuleVm<'a> {
    pub fn new(module: &'a Module) -> Self {
        let entry = module.entry;
        Self {
            module,
            frames: vec![Frame {
                function: entry,
                ip: 0,
                locals: vec![None; module.functions[entry].local_count],
                stack: Vec::new(),
            }],
            steps: 0,
            result: None,
        }
    }

    pub fn result(&self) -> Option<i64> {
        self.result
    }

    pub fn steps(&self) -> usize {
        self.steps
    }

    pub fn frame_depth(&self) -> usize {
        self.frames.len()
    }

    pub fn frames(&self) -> Vec<FrameSnapshot> {
        self.frames
            .iter()
            .map(|frame| FrameSnapshot {
                function: frame.function,
                instruction_pointer: frame.ip,
                locals: frame.locals.clone(),
                stack: frame.stack.clone(),
            })
            .collect()
    }

    pub fn run(&mut self) -> Result<i64, ModuleVmError> {
        while self.result.is_none() {
            self.step()?;
        }
        Ok(self.result.expect("result checked above"))
    }

    pub fn step(&mut self) -> Result<ModuleStep, ModuleVmError> {
        if self.result.is_some() {
            return Err(ModuleVmError::AlreadyFinished);
        }
        if self.steps >= MAX_MODULE_STEPS {
            return Err(ModuleVmError::StepLimit);
        }
        let frame_index = self.frames.len() - 1;
        let function = self.frames[frame_index].function;
        let instruction_pointer = self.frames[frame_index].ip;
        let instruction = self.module.functions[function].code[instruction_pointer];
        match instruction {
            Op::Const(value) => {
                self.check_push()?;
                self.frames[frame_index].stack.push(value);
                self.frames[frame_index].ip += 1;
            }
            Op::LocalGet(slot) => {
                self.check_push()?;
                let value = self.frames[frame_index].locals[slot]
                    .ok_or(ModuleVmError::UninitializedLocal)?;
                self.frames[frame_index].stack.push(value);
                self.frames[frame_index].ip += 1;
            }
            Op::LocalSet(slot) => {
                let frame = &mut self.frames[frame_index];
                let value = *frame
                    .stack
                    .last()
                    .ok_or(ModuleVmError::OperandStackUnderflow)?;
                frame.stack.pop();
                frame.locals[slot] = Some(value);
                frame.ip += 1;
            }
            Op::Add => self.binary(|a, b| a.checked_add(b))?,
            Op::Sub => self.binary(|a, b| a.checked_sub(b))?,
            Op::Mul => self.binary(|a, b| a.checked_mul(b))?,
            Op::Div => {
                let stack = &self.frames[frame_index].stack;
                if stack.len() < 2 {
                    return Err(ModuleVmError::OperandStackUnderflow);
                }
                if stack[stack.len() - 1] == 0 {
                    return Err(ModuleVmError::DivisionByZero);
                }
                self.binary(|a, b| a.checked_div(b))?;
            }
            Op::Eq => self.binary(|a, b| Some(i64::from(a == b)))?,
            Op::Lt => self.binary(|a, b| Some(i64::from(a < b)))?,
            Op::Jump(target) => self.frames[frame_index].ip = target,
            Op::JumpIfZero(target) => {
                let frame = &mut self.frames[frame_index];
                let condition = *frame
                    .stack
                    .last()
                    .ok_or(ModuleVmError::OperandStackUnderflow)?;
                frame.stack.pop();
                frame.ip = if condition == 0 { target } else { frame.ip + 1 };
            }
            Op::Call(target) => {
                let callee = &self.module.functions[target];
                let arity = callee.arity;
                let caller = &self.frames[frame_index];
                let stack_len = caller.stack.len();
                if stack_len < arity {
                    return Err(ModuleVmError::OperandStackUnderflow);
                }
                if self.frames.len() >= MAX_CALL_FRAMES {
                    return Err(ModuleVmError::CallFrameLimit);
                }
                let live = self.live_slots();
                if live - arity + callee.local_count > MAX_LIVE_SLOTS {
                    return Err(ModuleVmError::LiveSlotLimit);
                }
                let args = caller.stack[stack_len - arity..].to_vec();
                let mut locals = vec![None; callee.local_count];
                for (slot, value) in args.into_iter().enumerate() {
                    locals[slot] = Some(value);
                }
                self.frames[frame_index].stack.truncate(stack_len - arity);
                self.frames[frame_index].ip += 1;
                self.frames.push(Frame {
                    function: target,
                    ip: 0,
                    locals,
                    stack: Vec::new(),
                });
            }
            Op::Return => {
                let value = *self.frames[frame_index]
                    .stack
                    .last()
                    .ok_or(ModuleVmError::OperandStackUnderflow)?;
                if frame_index == 0 {
                    self.result = Some(value);
                } else {
                    let caller = &self.frames[frame_index - 1];
                    if caller.stack.len() >= MAX_OPERAND_STACK {
                        return Err(ModuleVmError::OperandStackLimit);
                    }
                    self.frames.pop();
                    self.frames[frame_index - 1].stack.push(value);
                }
            }
        }
        self.steps += 1;
        Ok(ModuleStep {
            function,
            instruction_pointer,
            instruction,
            result: self.result,
        })
    }

    fn live_slots(&self) -> usize {
        self.frames
            .iter()
            .map(|frame| frame.locals.len() + frame.stack.len())
            .sum()
    }

    fn check_push(&self) -> Result<(), ModuleVmError> {
        let frame = self.frames.last().expect("running VM has a frame");
        if frame.stack.len() >= MAX_OPERAND_STACK {
            return Err(ModuleVmError::OperandStackLimit);
        }
        if self.live_slots() >= MAX_LIVE_SLOTS {
            return Err(ModuleVmError::LiveSlotLimit);
        }
        Ok(())
    }

    fn binary(
        &mut self,
        operation: impl FnOnce(i64, i64) -> Option<i64>,
    ) -> Result<(), ModuleVmError> {
        let frame = self.frames.last_mut().expect("running VM has a frame");
        let len = frame.stack.len();
        if len < 2 {
            return Err(ModuleVmError::OperandStackUnderflow);
        }
        let lhs = frame.stack[len - 2];
        let rhs = frame.stack[len - 1];
        let value = operation(lhs, rhs).ok_or(ModuleVmError::IntegerOverflow)?;
        frame.stack.truncate(len - 2);
        frame.stack.push(value);
        frame.ip += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(arity: usize, locals: usize, code: Vec<Op>) -> Function {
        Function::new(arity, locals, code)
    }

    fn module(functions: Vec<Function>) -> Module {
        Module::new(MODULE_VERSION, functions, 0).unwrap()
    }

    #[test]
    fn executes_recursive_calls_with_local_parameters() {
        let program = module(vec![
            function(0, 0, vec![Op::Const(5), Op::Call(1), Op::Return]),
            function(
                1,
                1,
                vec![
                    Op::LocalGet(0),
                    Op::Const(2),
                    Op::Lt,
                    Op::JumpIfZero(6),
                    Op::Const(1),
                    Op::Return,
                    Op::LocalGet(0),
                    Op::LocalGet(0),
                    Op::Const(1),
                    Op::Sub,
                    Op::Call(1),
                    Op::Mul,
                    Op::Return,
                ],
            ),
        ]);
        assert_eq!(program.max_operand_stack(1), Some(3));
        let mut vm = ModuleVm::new(&program);
        assert_eq!(vm.step().unwrap().instruction, Op::Const(5));
        assert_eq!(vm.step().unwrap().instruction, Op::Call(1));
        assert_eq!(vm.frames().len(), 2);
        assert_eq!(vm.frames()[1].locals, vec![Some(5)]);
        assert_eq!(vm.run(), Ok(120));
        assert_eq!(vm.result(), Some(120));
        assert_eq!(vm.frame_depth(), 1);
        assert_eq!(vm.step(), Err(ModuleVmError::AlreadyFinished));
    }

    #[test]
    fn locals_are_scoped_to_each_call_frame() {
        let program = module(vec![
            function(
                0,
                1,
                vec![
                    Op::Const(9),
                    Op::LocalSet(0),
                    Op::Const(3),
                    Op::Call(1),
                    Op::LocalGet(0),
                    Op::Add,
                    Op::Return,
                ],
            ),
            function(
                1,
                1,
                vec![Op::LocalGet(0), Op::Const(2), Op::Mul, Op::Return],
            ),
        ]);
        assert_eq!(ModuleVm::new(&program).run(), Ok(15));
    }

    #[test]
    fn rejects_uninitialized_locals_on_any_branch() {
        let error = Module::new(
            MODULE_VERSION,
            vec![function(
                0,
                1,
                vec![
                    Op::Const(0),
                    Op::JumpIfZero(4),
                    Op::Const(1),
                    Op::LocalSet(0),
                    Op::LocalGet(0),
                    Op::Return,
                ],
            )],
            0,
        )
        .unwrap_err();
        assert_eq!(error.function, Some(0));
        assert_eq!(error.instruction, Some(4));
        assert!(error.message.contains("uninitialized"));
    }

    #[test]
    fn accepts_locals_initialized_on_both_branches() {
        let program = module(vec![function(
            0,
            1,
            vec![
                Op::Const(0),
                Op::JumpIfZero(5),
                Op::Const(7),
                Op::LocalSet(0),
                Op::Jump(7),
                Op::Const(8),
                Op::LocalSet(0),
                Op::LocalGet(0),
                Op::Return,
            ],
        )]);
        assert_eq!(ModuleVm::new(&program).run(), Ok(8));
    }

    #[test]
    fn rejects_inconsistent_stack_depth_and_invalid_structure() {
        let error = Module::new(
            MODULE_VERSION,
            vec![function(
                0,
                0,
                vec![
                    Op::Const(0),
                    Op::JumpIfZero(4),
                    Op::Jump(5),
                    Op::Const(99),
                    Op::Const(1),
                    Op::Const(2),
                    Op::Return,
                ],
            )],
            0,
        )
        .unwrap_err();
        assert!(error.message.contains("stack depth at join"));

        let invalid_jump = Module::new(
            MODULE_VERSION,
            vec![function(0, 0, vec![Op::Jump(2), Op::Return])],
            0,
        )
        .unwrap_err();
        assert!(invalid_jump.message.contains("branch target"));

        let invalid_call = Module::new(
            MODULE_VERSION,
            vec![function(0, 0, vec![Op::Call(1), Op::Return])],
            0,
        )
        .unwrap_err();
        assert!(invalid_call.message.contains("missing"));

        assert!(Module::new(MODULE_VERSION + 1, vec![], 0).is_err());
    }

    #[test]
    fn runtime_traps_leave_the_current_instruction_unchanged() {
        let program = module(vec![function(
            0,
            0,
            vec![Op::Const(9), Op::Const(0), Op::Div, Op::Return],
        )]);
        let mut vm = ModuleVm::new(&program);
        assert_eq!(vm.step().unwrap().instruction, Op::Const(9));
        assert_eq!(vm.step().unwrap().instruction, Op::Const(0));
        let frames = vm.frames.clone();
        let steps = vm.steps();
        assert_eq!(vm.step(), Err(ModuleVmError::DivisionByZero));
        assert_eq!(vm.frames, frames);
        assert_eq!(vm.steps(), steps);
        assert_eq!(vm.step(), Err(ModuleVmError::DivisionByZero));

        let overflow = module(vec![function(
            0,
            0,
            vec![Op::Const(i64::MAX), Op::Const(1), Op::Add, Op::Return],
        )]);
        assert_eq!(
            ModuleVm::new(&overflow).run(),
            Err(ModuleVmError::IntegerOverflow)
        );
    }

    #[test]
    fn recursion_is_bounded_by_frames_and_live_slots() {
        let recursive = module(vec![function(0, 0, vec![Op::Call(0), Op::Return])]);
        assert_eq!(
            ModuleVm::new(&recursive).run(),
            Err(ModuleVmError::CallFrameLimit)
        );

        let wide = module(vec![function(
            0,
            MAX_LOCALS_PER_FUNCTION,
            vec![Op::Call(0), Op::Return],
        )]);
        assert_eq!(
            ModuleVm::new(&wide).run(),
            Err(ModuleVmError::LiveSlotLimit)
        );
    }

    #[test]
    fn generated_module_inputs_validate_without_panicking() {
        let mut state = 0x7265_6c65_6173_6573_u64;
        for _ in 0..512 {
            let mut code = Vec::new();
            for _ in 0..16 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let op = match state % 12 {
                    0 => Op::Const(state as i64),
                    1 => Op::LocalGet((state >> 8) as usize % 4),
                    2 => Op::LocalSet((state >> 8) as usize % 4),
                    3 => Op::Add,
                    4 => Op::Sub,
                    5 => Op::Mul,
                    6 => Op::Div,
                    7 => Op::Eq,
                    8 => Op::Lt,
                    9 => Op::Jump((state >> 8) as usize % 20),
                    10 => Op::JumpIfZero((state >> 8) as usize % 20),
                    _ => Op::Call((state >> 8) as usize % 3),
                };
                code.push(op);
            }
            code.push(Op::Return);
            let _ = Module::new(MODULE_VERSION, vec![function(0, 2, code)], 0);
        }
    }

    #[test]
    fn validation_has_an_explicit_work_budget() {
        let functions = vec![function(0, 0, vec![Op::Const(1), Op::Return])];
        let mut visits = 0;
        let error = validate_function(0, &functions[0], &functions, &mut visits).unwrap_err();
        assert!(error.message.contains("visit limit"));
    }
}
