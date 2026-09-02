//! A small stack-based virtual machine.
//!
//! Programs are represented as a sequence of [`Instruction`] values. The VM
//! evaluates them from left to right and leaves the result on its value stack.

pub mod agent;
pub mod guest_tools;
pub mod jobs;
#[cfg(not(target_arch = "wasm32"))]
pub mod persistent_model;
pub mod runtime;
pub mod session;
pub mod workspace;

#[cfg(not(target_arch = "wasm32"))]
pub mod server;

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
    /// Stop execution and return the value at the top of the stack.
    Halt,
}

/// The result of executing one instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    /// An instruction executed and the VM can continue.
    Executed { instruction: Instruction },
    /// `Halt` executed and produced a result.
    Halted { result: i32 },
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
    /// The VM was stepped after it had already halted.
    AlreadyHalted,
}

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

        loop {
            match self.step(program)? {
                StepResult::Executed { .. } => {}
                StepResult::Halted { result } => return Ok(result),
            }
        }
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
    use super::{Instruction, StepResult, Vm, VmError};

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
}
