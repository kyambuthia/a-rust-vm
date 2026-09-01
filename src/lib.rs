//! A small stack-based virtual machine.
//!
//! Programs are represented as a sequence of [`Instruction`] values. The VM
//! evaluates them from left to right and leaves the result on its value stack.

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
}

/// A simple integer stack virtual machine.
#[derive(Debug, Default)]
pub struct Vm {
    stack: Vec<i32>,
    instruction_pointer: usize,
}

impl Vm {
    /// Create an empty VM.
    pub fn new() -> Self {
        Self::default()
    }

    /// Execute a program and return the value at the top of the stack when it
    /// reaches [`Instruction::Halt`].
    ///
    /// Each call starts with a clean stack and instruction pointer, so a VM
    /// can safely be reused for multiple programs.
    pub fn run(&mut self, program: &[Instruction]) -> Result<i32, VmError> {
        self.stack.clear();
        self.instruction_pointer = 0;

        while let Some(&instruction) = program.get(self.instruction_pointer) {
            self.instruction_pointer += 1;

            match instruction {
                Instruction::Push(value) => self.stack.push(value),
                Instruction::Add => {
                    self.binary_operation("add", |lhs, rhs| lhs.checked_add(rhs))?
                }
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
                    return self.stack.last().copied().ok_or(VmError::EmptyStack);
                }
            }
        }

        Err(VmError::MissingHalt)
    }

    /// Inspect the current value stack.
    pub fn stack(&self) -> &[i32] {
        &self.stack
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

#[cfg(test)]
mod tests {
    use super::{Instruction, Vm, VmError};

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
}
