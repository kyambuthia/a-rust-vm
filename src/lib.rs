//! A small stack-based virtual machine.
//!
//! Programs are represented as a sequence of [`Instruction`] values. The VM
//! evaluates them from left to right and leaves the result on its value stack.

pub mod agent;
pub mod apps;
pub mod artifacts;
#[cfg(not(target_arch = "wasm32"))]
pub mod content_store;
pub mod control_plane;
#[cfg(not(target_arch = "wasm32"))]
pub mod control_plane_store;
#[cfg(not(target_arch = "wasm32"))]
pub mod format_readers;
pub mod guest_tools;
pub mod jobs;
#[cfg(not(target_arch = "wasm32"))]
pub mod mcp;
#[cfg(not(target_arch = "wasm32"))]
pub mod openrouter;
pub mod permissions;
#[cfg(not(target_arch = "wasm32"))]
pub mod persistent_model;
pub mod program;
pub mod protocol;
pub mod runtime;
pub mod session;
pub mod session_snapshot;
pub mod skills;
pub mod workspace;

#[cfg(not(target_arch = "wasm32"))]
pub mod anon_session;
#[cfg(not(target_arch = "wasm32"))]
pub mod server;
#[cfg(not(target_arch = "wasm32"))]
pub mod session_snapshot_store;

/// The instructions understood by the VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instruction {
    /// Push an integer onto the value stack.
    Push(i32),
    /// Pop two values and push their sum.
    Add,
    /// Pop two values and push the first value minus the second.
    Sub,
    /// Pop two values and push their product.
    Mul,
    /// Pop two values and push the first value divided by the second.
    Div,
    /// Duplicate the value at the top of the stack.
    Dup,
    /// Discard the value at the top of the stack.
    Pop,
    /// Swap the two values at the top of the stack.
    Swap,
    /// Copy the second value from the top onto the top of the stack.
    Over,
    /// Pop two values and push 1 if they are equal, else 0.
    Eq,
    /// Pop two values and push 1 if the first is less than the second, else 0.
    Lt,
    /// Pop one value and push its arithmetic negation.
    Neg,
    /// Pop one value and push 1 if it is zero, else 0.
    Not,
    /// Jump unconditionally to the instruction at the given index.
    Jmp(usize),
    /// Pop a condition and jump to the given index if it is zero.
    Jz(usize),
    /// Stop execution and return the value at the top of the stack.
    Halt,
}

impl Instruction {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Push(_) => "PUSH",
            Self::Add => "ADD",
            Self::Sub => "SUB",
            Self::Mul => "MUL",
            Self::Div => "DIV",
            Self::Dup => "DUP",
            Self::Pop => "POP",
            Self::Swap => "SWAP",
            Self::Over => "OVER",
            Self::Eq => "EQ",
            Self::Lt => "LT",
            Self::Neg => "NEG",
            Self::Not => "NOT",
            Self::Jmp(_) => "JMP",
            Self::Jz(_) => "JZ",
            Self::Halt => "HALT",
        }
    }
}

impl std::fmt::Display for Instruction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Push(value) => write!(formatter, "PUSH {value}"),
            Self::Jmp(target) => write!(formatter, "JMP {target}"),
            Self::Jz(target) => write!(formatter, "JZ {target}"),
            instruction => formatter.write_str(instruction.name()),
        }
    }
}

/// The result of executing one instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    /// An instruction executed and the VM can continue.
    Executed { instruction: Instruction },
    /// `Halt` executed and produced a result.
    Halted { result: i32 },
}

/// A deterministic snapshot captured after one instruction executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEntry {
    pub instruction_pointer: usize,
    pub instruction: Instruction,
    pub stack: Vec<i32>,
    pub result: Option<i32>,
}

/// Errors that can occur while executing a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmError {
    /// An instruction needed more values than were available on the stack.
    StackUnderflow {
        operation: &'static str,
        needed: usize,
        available: usize,
    },
    /// The program attempted to divide by zero.
    DivisionByZero,
    /// An arithmetic operation exceeded the range of an `i32`.
    IntegerOverflow { operation: &'static str },
    /// `Halt` was not found before the program ended.
    MissingHalt,
    /// `Halt` was reached without a result on the stack.
    EmptyStack,
    /// A jump targeted an instruction index outside the program.
    InvalidJump { target: usize },
    /// Execution exceeded the bounded step budget (loops must terminate).
    StepLimitExceeded { limit: usize },
    /// The VM was stepped after it had already halted.
    AlreadyHalted,
}

/// Upper bound on executed instructions for one `run` or `trace` call.
///
/// Jumps make non-termination expressible, so the direct-execution paths
/// fail closed instead of looping forever. Guest processes are bounded
/// separately by the scheduler tick limits.
pub const MAX_VM_STEPS: usize = 1_000_000;

/// A simple integer stack virtual machine.
#[derive(Debug, Default)]
pub struct Vm {
    stack: Vec<i32>,
    instruction_pointer: usize,
    halted: bool,
}

impl Vm {
    /// Create an empty VM.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset the VM to its initial state.
    pub fn reset(&mut self) {
        self.stack.clear();
        self.instruction_pointer = 0;
        self.halted = false;
    }

    /// Execute a program and return the value at the top of the stack when it
    /// reaches [`Instruction::Halt`].
    ///
    /// Each call starts with a clean stack and instruction pointer, so a VM
    /// can safely be reused for multiple programs.
    pub fn run(&mut self, program: &[Instruction]) -> Result<i32, VmError> {
        self.reset();

        for _ in 0..MAX_VM_STEPS {
            match self.step(program)? {
                StepResult::Executed { .. } => {}
                StepResult::Halted { result } => return Ok(result),
            }
        }
        Err(VmError::StepLimitExceeded {
            limit: MAX_VM_STEPS,
        })
    }

    /// Execute a program that was validated at its input boundary.
    pub fn run_program(&mut self, program: &program::Program) -> Result<i32, VmError> {
        self.run(program.instructions())
    }

    /// Execute a validated program and return a complete, inspectable trace.
    pub fn trace(&mut self, program: &program::Program) -> Result<Vec<TraceEntry>, VmError> {
        self.reset();
        let mut entries = Vec::with_capacity(program.instructions().len());
        for _ in 0..MAX_VM_STEPS {
            let instruction_pointer = self.instruction_pointer;
            let step = self.step(program.instructions())?;
            let (instruction, result) = match step {
                StepResult::Executed { instruction } => (instruction, None),
                StepResult::Halted { result } => (Instruction::Halt, Some(result)),
            };
            entries.push(TraceEntry {
                instruction_pointer,
                instruction,
                stack: self.stack.clone(),
                result,
            });
            if result.is_some() {
                return Ok(entries);
            }
        }
        Err(VmError::StepLimitExceeded {
            limit: MAX_VM_STEPS,
        })
    }

    /// Execute exactly one instruction from the current instruction pointer.
    pub fn step(&mut self, program: &[Instruction]) -> Result<StepResult, VmError> {
        if self.halted {
            return Err(VmError::AlreadyHalted);
        }

        let instruction = *program
            .get(self.instruction_pointer)
            .ok_or(VmError::MissingHalt)?;
        self.instruction_pointer += 1;

        match instruction {
            Instruction::Push(value) => self.stack.push(value),
            Instruction::Add => self.binary_operation("add", |lhs, rhs| lhs.checked_add(rhs))?,
            Instruction::Sub => {
                self.binary_operation("subtract", |lhs, rhs| lhs.checked_sub(rhs))?
            }
            Instruction::Mul => {
                self.binary_operation("multiply", |lhs, rhs| lhs.checked_mul(rhs))?
            }
            Instruction::Div => {
                let (lhs, rhs) = self.pop_binary_operands("divide")?;

                if rhs == 0 {
                    return Err(VmError::DivisionByZero);
                }

                let result = lhs.checked_div(rhs).ok_or(VmError::IntegerOverflow {
                    operation: "divide",
                })?;
                self.stack.push(result);
            }
            Instruction::Dup => {
                let value = self.stack.last().copied().ok_or(VmError::StackUnderflow {
                    operation: "duplicate",
                    needed: 1,
                    available: 0,
                })?;
                self.stack.push(value);
            }
            Instruction::Pop => {
                self.stack.pop().ok_or(VmError::StackUnderflow {
                    operation: "pop",
                    needed: 1,
                    available: 0,
                })?;
            }
            Instruction::Swap => {
                if self.stack.len() < 2 {
                    return Err(VmError::StackUnderflow {
                        operation: "swap",
                        needed: 2,
                        available: self.stack.len(),
                    });
                }
                let len = self.stack.len();
                self.stack.swap(len - 1, len - 2);
            }
            Instruction::Over => {
                if self.stack.len() < 2 {
                    return Err(VmError::StackUnderflow {
                        operation: "over",
                        needed: 2,
                        available: self.stack.len(),
                    });
                }
                let value = self.stack[self.stack.len() - 2];
                self.stack.push(value);
            }
            Instruction::Eq => {
                let (lhs, rhs) = self.pop_binary_operands("equal")?;
                self.stack.push(i32::from(lhs == rhs));
            }
            Instruction::Lt => {
                let (lhs, rhs) = self.pop_binary_operands("less-than")?;
                self.stack.push(i32::from(lhs < rhs));
            }
            Instruction::Neg => {
                let value = self.stack.pop().ok_or(VmError::StackUnderflow {
                    operation: "negate",
                    needed: 1,
                    available: 0,
                })?;
                let result = value.checked_neg().ok_or(VmError::IntegerOverflow {
                    operation: "negate",
                })?;
                self.stack.push(result);
            }
            Instruction::Not => {
                let value = self.stack.pop().ok_or(VmError::StackUnderflow {
                    operation: "not",
                    needed: 1,
                    available: 0,
                })?;
                self.stack.push(i32::from(value == 0));
            }
            Instruction::Jmp(target) => {
                if target >= program.len() {
                    return Err(VmError::InvalidJump { target });
                }
                self.instruction_pointer = target;
            }
            Instruction::Jz(target) => {
                if target >= program.len() {
                    return Err(VmError::InvalidJump { target });
                }
                let condition = self.stack.pop().ok_or(VmError::StackUnderflow {
                    operation: "jump-if-zero",
                    needed: 1,
                    available: 0,
                })?;
                if condition == 0 {
                    self.instruction_pointer = target;
                }
            }
            Instruction::Halt => {
                let result = self.stack.last().copied().ok_or(VmError::EmptyStack)?;
                self.halted = true;
                return Ok(StepResult::Halted { result });
            }
        }

        Ok(StepResult::Executed { instruction })
    }

    /// Inspect the current value stack.
    pub fn stack(&self) -> &[i32] {
        &self.stack
    }

    /// Return the index of the next instruction to execute.
    pub fn instruction_pointer(&self) -> usize {
        self.instruction_pointer
    }

    /// Return whether the VM has executed `Halt`.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    fn binary_operation<F>(
        &mut self,
        operation: &'static str,
        operation_fn: F,
    ) -> Result<(), VmError>
    where
        F: FnOnce(i32, i32) -> Option<i32>,
    {
        let (lhs, rhs) = self.pop_binary_operands(operation)?;
        let result = operation_fn(lhs, rhs).ok_or(VmError::IntegerOverflow { operation })?;
        self.stack.push(result);
        Ok(())
    }

    fn pop_binary_operands(&mut self, operation: &'static str) -> Result<(i32, i32), VmError> {
        if self.stack.len() < 2 {
            return Err(VmError::StackUnderflow {
                operation,
                needed: 2,
                available: self.stack.len(),
            });
        }

        let rhs = self.stack.pop().expect("length checked above");
        let lhs = self.stack.pop().expect("length checked above");
        Ok((lhs, rhs))
    }
}

/// Run the demo bytecode when this library is loaded as a WebAssembly module.
///
/// This intentionally exposes a small C-compatible boundary. Later we can
/// replace it with an API for sending complete bytecode programs from
/// JavaScript.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn run_demo() -> i32 {
    let program = [
        Instruction::Push(2),
        Instruction::Push(3),
        Instruction::Push(4),
        Instruction::Mul,
        Instruction::Add,
        Instruction::Halt,
    ];

    Vm::new()
        .run(&program)
        .expect("the demo bytecode should always execute successfully")
}

/// Execute one binary operation supplied by JavaScript.
///
/// Operation codes are `0 = add`, `1 = subtract`, `2 = multiply`, and
/// `3 = divide`.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn run_binary(lhs: i32, rhs: i32, operation: i32) -> i32 {
    let instruction = match operation {
        0 => Instruction::Add,
        1 => Instruction::Sub,
        2 => Instruction::Mul,
        3 => Instruction::Div,
        _ => panic!("unknown operation code: {operation}"),
    };

    let program = [
        Instruction::Push(lhs),
        Instruction::Push(rhs),
        instruction,
        Instruction::Halt,
    ];

    Vm::new()
        .run(&program)
        .expect("the browser should validate the operation inputs")
}

#[cfg(target_arch = "wasm32")]
struct DebugSession {
    program: Vec<Instruction>,
    vm: Vm,
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static DEBUG_SESSION: std::cell::RefCell<Option<DebugSession>> = const { std::cell::RefCell::new(None) };
    static PROGRAM_BUILDER: std::cell::RefCell<Vec<Instruction>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(target_arch = "wasm32")]
fn debug_error_code(error: VmError) -> i32 {
    match error {
        VmError::MissingHalt => -2,
        VmError::StackUnderflow { .. } => -3,
        VmError::DivisionByZero => -4,
        VmError::IntegerOverflow { .. } => -5,
        VmError::EmptyStack => -6,
        VmError::AlreadyHalted => -7,
        VmError::InvalidJump { .. } => -10,
        VmError::StepLimitExceeded { .. } => -11,
    }
}

/// Load a binary program into the browser debugger session.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn debug_load_binary(lhs: i32, rhs: i32, operation: i32) -> i32 {
    let instruction = match operation {
        0 => Instruction::Add,
        1 => Instruction::Sub,
        2 => Instruction::Mul,
        3 => Instruction::Div,
        _ => return -8,
    };

    DEBUG_SESSION.with(|session| {
        *session.borrow_mut() = Some(DebugSession {
            program: vec![
                Instruction::Push(lhs),
                Instruction::Push(rhs),
                instruction,
                Instruction::Halt,
            ],
            vm: Vm::new(),
        });
    });

    0
}

/// Begin loading an arbitrary program through the allocation-free Wasm ABI.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn debug_program_begin() {
    PROGRAM_BUILDER.with(|builder| builder.borrow_mut().clear());
}

/// Append an instruction. Codes are `0 = PUSH`, `1 = ADD`, `2 = SUB`,
/// `3 = MUL`, `4 = DIV`, `5 = HALT`, `6 = DUP`, `7 = POP`, `8 = EQ`,
/// `9 = LT`, `10 = JMP`, `11 = JZ`, `12 = SWAP`, `13 = OVER`,
/// `14 = NEG`, and `15 = NOT`; PUSH uses `operand` as the value and
/// JMP/JZ use it as the target instruction index.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn debug_program_push(opcode: i32, operand: i32) -> i32 {
    let instruction = match opcode {
        0 => Instruction::Push(operand),
        1 => Instruction::Add,
        2 => Instruction::Sub,
        3 => Instruction::Mul,
        4 => Instruction::Div,
        5 => Instruction::Halt,
        6 => Instruction::Dup,
        7 => Instruction::Pop,
        8 => Instruction::Eq,
        9 => Instruction::Lt,
        10 => {
            let Ok(target) = usize::try_from(operand) else {
                return -10;
            };
            Instruction::Jmp(target)
        }
        11 => {
            let Ok(target) = usize::try_from(operand) else {
                return -10;
            };
            Instruction::Jz(target)
        }
        12 => Instruction::Swap,
        13 => Instruction::Over,
        14 => Instruction::Neg,
        15 => Instruction::Not,
        _ => return -8,
    };
    PROGRAM_BUILDER.with(|builder| {
        let mut builder = builder.borrow_mut();
        if builder.len() >= program::MAX_PROGRAM_INSTRUCTIONS {
            return -9;
        }
        builder.push(instruction);
        0
    })
}

/// Validate and install the program assembled with `debug_program_push`.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn debug_program_finish() -> i32 {
    PROGRAM_BUILDER.with(|builder| {
        let instructions = builder.borrow().clone();
        let Ok(program) = program::Program::new(instructions) else {
            return -9;
        };
        DEBUG_SESSION.with(|session| {
            *session.borrow_mut() = Some(DebugSession {
                program: program.instructions().to_vec(),
                vm: Vm::new(),
            });
        });
        0
    })
}

/// Execute one instruction in the browser debugger session.
///
/// Returns `0` when execution can continue, `1` when the VM halted, and a
/// negative value for an execution error.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn debug_step() -> i32 {
    DEBUG_SESSION.with(|session| {
        let mut session = session.borrow_mut();
        let Some(session) = session.as_mut() else {
            return -1;
        };

        match session.vm.step(&session.program) {
            Ok(StepResult::Executed { .. }) => 0,
            Ok(StepResult::Halted { .. }) => 1,
            Err(error) => debug_error_code(error),
        }
    })
}

/// Reset the browser debugger session without unloading its program.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn debug_reset() -> i32 {
    DEBUG_SESSION.with(|session| {
        let mut session = session.borrow_mut();
        let Some(session) = session.as_mut() else {
            return -1;
        };
        session.vm.reset();
        0
    })
}

/// Return the next instruction index in the browser debugger session.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn debug_instruction_pointer() -> i32 {
    DEBUG_SESSION.with(|session| {
        session
            .borrow()
            .as_ref()
            .map(|session| session.vm.instruction_pointer() as i32)
            .unwrap_or(-1)
    })
}

/// Return the number of values on the browser debugger stack.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn debug_stack_len() -> i32 {
    DEBUG_SESSION.with(|session| {
        session
            .borrow()
            .as_ref()
            .map(|session| session.vm.stack().len() as i32)
            .unwrap_or(-1)
    })
}

/// Return a value from the browser debugger stack by index.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn debug_stack_at(index: i32) -> i32 {
    if index < 0 {
        return 0;
    }

    DEBUG_SESSION.with(|session| {
        session
            .borrow()
            .as_ref()
            .and_then(|session| session.vm.stack().get(index as usize).copied())
            .unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use super::{Instruction, MAX_VM_STEPS, StepResult, Vm, VmError};

    #[test]
    fn adds_two_values() {
        let mut vm = Vm::new();

        let result = vm.run(&[
            Instruction::Push(2),
            Instruction::Push(3),
            Instruction::Add,
            Instruction::Halt,
        ]);

        assert_eq!(result, Ok(5));
        assert_eq!(vm.stack(), &[5]);
    }

    #[test]
    fn evaluates_a_compound_expression() {
        let mut vm = Vm::new();

        let result = vm.run(&[
            Instruction::Push(2),
            Instruction::Push(3),
            Instruction::Push(4),
            Instruction::Mul,
            Instruction::Add,
            Instruction::Halt,
        ]);

        assert_eq!(result, Ok(14));
    }

    #[test]
    fn preserves_operand_order_for_subtraction_and_division() {
        let mut vm = Vm::new();

        assert_eq!(
            vm.run(&[
                Instruction::Push(10),
                Instruction::Push(3),
                Instruction::Sub,
                Instruction::Halt,
            ]),
            Ok(7)
        );

        assert_eq!(
            vm.run(&[
                Instruction::Push(20),
                Instruction::Push(4),
                Instruction::Div,
                Instruction::Halt,
            ]),
            Ok(5)
        );
    }

    #[test]
    fn reports_stack_underflow() {
        let mut vm = Vm::new();

        assert_eq!(
            vm.run(&[Instruction::Push(1), Instruction::Add, Instruction::Halt]),
            Err(VmError::StackUnderflow {
                operation: "add",
                needed: 2,
                available: 1,
            })
        );
    }

    #[test]
    fn reports_division_by_zero() {
        let mut vm = Vm::new();

        assert_eq!(
            vm.run(&[
                Instruction::Push(10),
                Instruction::Push(0),
                Instruction::Div,
                Instruction::Halt,
            ]),
            Err(VmError::DivisionByZero)
        );
    }

    #[test]
    fn reports_missing_halt() {
        let mut vm = Vm::new();

        assert_eq!(vm.run(&[Instruction::Push(1)]), Err(VmError::MissingHalt));
    }

    #[test]
    fn trace_captures_post_instruction_stack_and_halt_result() {
        let program: crate::program::Program = "PUSH 2\nPUSH 3\nADD\nHALT".parse().unwrap();
        let entries = Vm::new().trace(&program).unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[2].stack, vec![5]);
        assert_eq!(entries[3].result, Some(5));
    }

    #[test]
    fn reports_empty_result() {
        let mut vm = Vm::new();

        assert_eq!(vm.run(&[Instruction::Halt]), Err(VmError::EmptyStack));
    }

    #[test]
    fn reports_integer_overflow() {
        let mut vm = Vm::new();

        assert_eq!(
            vm.run(&[
                Instruction::Push(i32::MAX),
                Instruction::Push(1),
                Instruction::Add,
                Instruction::Halt,
            ]),
            Err(VmError::IntegerOverflow { operation: "add" })
        );
    }

    #[test]
    fn steps_through_a_program() {
        let program = [
            Instruction::Push(2),
            Instruction::Push(3),
            Instruction::Add,
            Instruction::Halt,
        ];
        let mut vm = Vm::new();

        assert_eq!(
            vm.step(&program),
            Ok(StepResult::Executed {
                instruction: Instruction::Push(2)
            })
        );
        assert_eq!(vm.instruction_pointer(), 1);
        assert_eq!(vm.stack(), &[2]);

        assert_eq!(
            vm.step(&program),
            Ok(StepResult::Executed {
                instruction: Instruction::Push(3)
            })
        );
        assert_eq!(
            vm.step(&program),
            Ok(StepResult::Executed {
                instruction: Instruction::Add
            })
        );
        assert_eq!(vm.stack(), &[5]);
        assert_eq!(vm.step(&program), Ok(StepResult::Halted { result: 5 }));
        assert!(vm.is_halted());
    }

    #[test]
    fn reset_rewinds_a_debug_session() {
        let program = [Instruction::Push(7), Instruction::Halt];
        let mut vm = Vm::new();

        vm.step(&program).unwrap();
        assert_eq!(vm.stack(), &[7]);
        vm.reset();

        assert_eq!(vm.instruction_pointer(), 0);
        assert!(vm.stack().is_empty());
        assert!(!vm.is_halted());
    }

    #[test]
    fn rejects_stepping_after_halt() {
        let program = [Instruction::Push(7), Instruction::Halt];
        let mut vm = Vm::new();

        vm.run(&program).unwrap();

        assert_eq!(vm.step(&program), Err(VmError::AlreadyHalted));
    }

    #[test]
    fn duplicates_discards_and_compares_values() {
        let mut vm = Vm::new();

        assert_eq!(
            vm.run(&[
                Instruction::Push(9),
                Instruction::Dup,
                Instruction::Add,
                Instruction::Halt,
            ]),
            Ok(18)
        );
        assert_eq!(
            vm.run(&[
                Instruction::Push(9),
                Instruction::Push(1),
                Instruction::Pop,
                Instruction::Halt,
            ]),
            Ok(9)
        );
        assert_eq!(
            vm.run(&[
                Instruction::Push(3),
                Instruction::Push(3),
                Instruction::Eq,
                Instruction::Halt,
            ]),
            Ok(1)
        );
        assert_eq!(
            vm.run(&[
                Instruction::Push(3),
                Instruction::Push(4),
                Instruction::Eq,
                Instruction::Halt,
            ]),
            Ok(0)
        );
        assert_eq!(
            vm.run(&[
                Instruction::Push(2),
                Instruction::Push(5),
                Instruction::Lt,
                Instruction::Halt,
            ]),
            Ok(1)
        );
        assert_eq!(
            vm.run(&[
                Instruction::Push(5),
                Instruction::Push(2),
                Instruction::Lt,
                Instruction::Halt,
            ]),
            Ok(0)
        );
    }

    #[test]
    fn executes_a_countdown_loop_to_zero() {
        // 0: PUSH 5 | 1: DUP | 2: JZ 6 | 3: PUSH 1 | 4: SUB | 5: JMP 1 | 6: HALT
        let program = [
            Instruction::Push(5),
            Instruction::Dup,
            Instruction::Jz(6),
            Instruction::Push(1),
            Instruction::Sub,
            Instruction::Jmp(1),
            Instruction::Halt,
        ];

        assert_eq!(Vm::new().run(&program), Ok(0));
    }

    #[test]
    fn rejects_an_out_of_range_jump_at_runtime() {
        let program = [Instruction::Jmp(99)];

        assert_eq!(
            Vm::new().step(&program),
            Err(VmError::InvalidJump { target: 99 })
        );
    }

    #[test]
    fn bounds_execution_of_non_terminating_loops() {
        let program = [Instruction::Jmp(0)];

        assert_eq!(
            Vm::new().run(&program),
            Err(VmError::StepLimitExceeded {
                limit: MAX_VM_STEPS
            })
        );
    }

    #[test]
    fn swaps_and_copies_second_stack_values() {
        let mut vm = Vm::new();

        assert_eq!(
            vm.run(&[
                Instruction::Push(1),
                Instruction::Push(2),
                Instruction::Swap,
                Instruction::Halt,
            ]),
            Ok(1)
        );
        assert_eq!(
            vm.run(&[
                Instruction::Push(1),
                Instruction::Push(2),
                Instruction::Over,
                Instruction::Halt,
            ]),
            Ok(1)
        );
        assert_eq!(
            Vm::new().step(&[Instruction::Push(1), Instruction::Swap]),
            Ok(StepResult::Executed {
                instruction: Instruction::Push(1)
            })
        );
        assert_eq!(
            Vm::new().run(&[Instruction::Push(1), Instruction::Swap, Instruction::Halt]),
            Err(VmError::StackUnderflow {
                operation: "swap",
                needed: 2,
                available: 1,
            })
        );
        assert_eq!(
            Vm::new().run(&[Instruction::Push(1), Instruction::Over, Instruction::Halt]),
            Err(VmError::StackUnderflow {
                operation: "over",
                needed: 2,
                available: 1,
            })
        );
    }

    #[test]
    fn accumulates_the_sum_of_one_to_five() {
        // acc=0, n=5; each pass adds n into acc and decrements n.
        // 00 PUSH 0 | 01 PUSH 5 | 02 DUP | 03 JZ 14 | 04 SWAP | 05 OVER
        // 06 ADD | 07 SWAP | 08 DUP | 09 PUSH 1 | 10 SUB | 11 SWAP
        // 12 POP | 13 JMP 2 | 14 POP | 15 HALT
        let program = [
            Instruction::Push(0),
            Instruction::Push(5),
            Instruction::Dup,
            Instruction::Jz(14),
            Instruction::Swap,
            Instruction::Over,
            Instruction::Add,
            Instruction::Swap,
            Instruction::Dup,
            Instruction::Push(1),
            Instruction::Sub,
            Instruction::Swap,
            Instruction::Pop,
            Instruction::Jmp(2),
            Instruction::Pop,
            Instruction::Halt,
        ];

        assert_eq!(Vm::new().run(&program), Ok(15));
    }

    #[test]
    fn negates_and_logically_negates_values() {
        let mut vm = Vm::new();

        assert_eq!(
            vm.run(&[Instruction::Push(5), Instruction::Neg, Instruction::Halt]),
            Ok(-5)
        );
        assert_eq!(
            vm.run(&[
                Instruction::Push(i32::MIN),
                Instruction::Neg,
                Instruction::Halt,
            ]),
            Err(VmError::IntegerOverflow {
                operation: "negate"
            })
        );
        assert_eq!(
            vm.run(&[Instruction::Push(0), Instruction::Not, Instruction::Halt]),
            Ok(1)
        );
        assert_eq!(
            vm.run(&[Instruction::Push(42), Instruction::Not, Instruction::Halt]),
            Ok(0)
        );
        assert_eq!(
            vm.run(&[Instruction::Neg, Instruction::Halt]),
            Err(VmError::StackUnderflow {
                operation: "negate",
                needed: 1,
                available: 0,
            })
        );
        assert_eq!(
            vm.run(&[Instruction::Not, Instruction::Halt]),
            Err(VmError::StackUnderflow {
                operation: "not",
                needed: 1,
                available: 0,
            })
        );
    }
}
