//! Validated VM programs and the textual A/RVM assembly format.

use std::fmt;
use std::str::FromStr;

use crate::Instruction;

/// Default upper bound for a program accepted at an external boundary.
pub const MAX_PROGRAM_INSTRUCTIONS: usize = 65_536;

/// Immutable bytecode that has passed structural and stack validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    instructions: Vec<Instruction>,
    max_stack_depth: usize,
}

impl Program {
    pub fn new(instructions: Vec<Instruction>) -> Result<Self, ProgramError> {
        if instructions.is_empty() {
            return Err(ProgramError::new(None, "program is empty"));
        }
        if instructions.len() > MAX_PROGRAM_INSTRUCTIONS {
            return Err(ProgramError::new(
                None,
                format!(
                    "program has {} instructions; limit is {MAX_PROGRAM_INSTRUCTIONS}",
                    instructions.len()
                ),
            ));
        }
        // Reject out-of-range jump targets anywhere in the program,
        // including unreachable instructions, so invalid targets fail
        // closed at the validation boundary rather than at runtime.
        // Likewise, instructions after HALT stay unreachable by
        // construction; both checks precede the end-with-HALT check to
        // preserve the historical error precedence.
        for (index, instruction) in instructions.iter().enumerate() {
            if matches!(instruction, Instruction::Halt) && index + 1 != instructions.len() {
                return Err(ProgramError::new(
                    Some(index + 2),
                    "instructions after HALT are unreachable",
                ));
            }
            let target = match instruction {
                Instruction::Jmp(target) | Instruction::Jz(target) => *target,
                _ => continue,
            };
            if target >= instructions.len() {
                return Err(ProgramError::new(
                    Some(index + 1),
                    format!(
                        "{} target {target} is outside the program",
                        instruction.name()
                    ),
                ));
            }
        }
        if !matches!(instructions.last(), Some(Instruction::Halt)) {
            return Err(ProgramError::new(None, "program must end with HALT"));
        }

        // Static stack validation is flow-sensitive because jumps create
        // branches and loops. Walk the reachable control-flow graph with an
        // explicit worklist, tracking the stack depth at each instruction.
        // Joins must agree on one depth so every execution has an
        // unambiguous stack; runtime checks remain the authority for
        // underflow on unvalidated paths.
        let mut depth_at: Vec<Option<usize>> = vec![None; instructions.len()];
        let mut max_stack_depth = 0usize;
        let mut worklist = vec![(0usize, 0usize)];
        while let Some((index, depth)) = worklist.pop() {
            if let Some(known) = depth_at[index] {
                if known != depth {
                    return Err(ProgramError::new(
                        Some(index + 1),
                        format!(
                            "ambiguous stack depth at instruction {}: joined paths disagree ({known} vs {depth})",
                            index + 1
                        ),
                    ));
                }
                continue;
            }
            depth_at[index] = Some(depth);
            let instruction = instructions[index];
            // Minimum stack depth required before this instruction.
            let required = match instruction {
                Instruction::Push(_) | Instruction::Jmp(_) => 0,
                Instruction::Dup | Instruction::Pop | Instruction::Jz(_) => 1,
                Instruction::Halt => 1,
                Instruction::Add
                | Instruction::Sub
                | Instruction::Mul
                | Instruction::Div
                | Instruction::Eq
                | Instruction::Lt => 2,
            };
            if depth < required {
                return Err(ProgramError::new(
                    Some(index + 1),
                    format!("{} requires {required} stack values", instruction.name()),
                ));
            }
            // Depth after this instruction executes.
            let next_depth = match instruction {
                Instruction::Push(_) | Instruction::Dup => depth + 1,
                Instruction::Jmp(_) | Instruction::Halt => depth,
                Instruction::Pop
                | Instruction::Jz(_)
                | Instruction::Add
                | Instruction::Sub
                | Instruction::Mul
                | Instruction::Div
                | Instruction::Eq
                | Instruction::Lt => depth - 1,
            };
            max_stack_depth = max_stack_depth.max(next_depth);
            match instruction {
                Instruction::Push(_)
                | Instruction::Dup
                | Instruction::Pop
                | Instruction::Add
                | Instruction::Sub
                | Instruction::Mul
                | Instruction::Div
                | Instruction::Eq
                | Instruction::Lt => {
                    worklist.push((index + 1, next_depth));
                }
                Instruction::Jmp(target) => {
                    worklist.push((target, next_depth));
                }
                Instruction::Jz(target) => {
                    // Explore the fallthrough first so joins report the
                    // target path against the linear path, matching the
                    // order a reader follows through the program.
                    worklist.push((target, next_depth));
                    worklist.push((index + 1, next_depth));
                }
                Instruction::Halt => {}
            }
        }

        Ok(Self {
            instructions,
            max_stack_depth,
        })
    }

    pub fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }

    pub fn max_stack_depth(&self) -> usize {
        self.max_stack_depth
    }

    pub fn disassemble(&self) -> String {
        self.instructions
            .iter()
            .enumerate()
            .map(|(index, instruction)| format!("{index:04}  {instruction}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl FromStr for Program {
    type Err = ProgramError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let mut instructions = Vec::new();
        for (line_index, raw_line) in source.lines().enumerate() {
            let source_line = line_index + 1;
            let line = raw_line
                .split_once('#')
                .map_or(raw_line, |(code, _)| code)
                .trim();
            if line.is_empty() {
                continue;
            }
            let fields = line.split_whitespace().collect::<Vec<_>>();
            let opcode = fields[0].to_ascii_uppercase();
            let instruction = match opcode.as_str() {
                "PUSH" if fields.len() == 2 => {
                    Instruction::Push(fields[1].parse().map_err(|_| {
                        ProgramError::new(Some(source_line), "PUSH operand must be an i32")
                    })?)
                }
                "PUSH" => {
                    return Err(ProgramError::new(
                        Some(source_line),
                        "PUSH requires exactly one operand",
                    ));
                }
                "ADD" | "SUB" | "MUL" | "DIV" | "DUP" | "POP" | "EQ" | "LT" | "HALT"
                    if fields.len() != 1 =>
                {
                    return Err(ProgramError::new(
                        Some(source_line),
                        format!("{opcode} does not accept operands"),
                    ));
                }
                "ADD" => Instruction::Add,
                "SUB" => Instruction::Sub,
                "MUL" => Instruction::Mul,
                "DIV" => Instruction::Div,
                "DUP" => Instruction::Dup,
                "POP" => Instruction::Pop,
                "EQ" => Instruction::Eq,
                "LT" => Instruction::Lt,
                "HALT" => Instruction::Halt,
                "JMP" | "JZ" if fields.len() == 2 => {
                    let target: usize = fields[1].parse().map_err(|_| {
                        ProgramError::new(
                            Some(source_line),
                            format!("{opcode} target must be an instruction index"),
                        )
                    })?;
                    if opcode.as_str() == "JMP" {
                        Instruction::Jmp(target)
                    } else {
                        Instruction::Jz(target)
                    }
                }
                "JMP" | "JZ" => {
                    return Err(ProgramError::new(
                        Some(source_line),
                        format!("{opcode} requires exactly one target index"),
                    ));
                }
                _ => {
                    return Err(ProgramError::new(
                        Some(source_line),
                        format!("unknown instruction '{}'", fields[0]),
                    ));
                }
            };
            instructions.push(instruction);
        }
        Self::new(instructions)
    }
}

impl fmt::Display for Program {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, instruction) in self.instructions.iter().enumerate() {
            if index > 0 {
                formatter.write_str("\n")?;
            }
            write!(formatter, "{instruction}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramError {
    pub line: Option<usize>,
    pub message: String,
}

impl ProgramError {
    fn new(line: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for ProgramError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(formatter, "line {line}: {}", self.message),
            None => formatter.write_str(&self.message),
        }
    }
}

impl std::error::Error for ProgramError {}

#[cfg(test)]
mod tests {
    use super::Program;

    #[test]
    fn parses_comments_and_disassembles_with_offsets() {
        let program: Program = "# multiply\nPUSH 6\nPUSH 7\nMUL\nHALT\n".parse().unwrap();
        assert_eq!(program.max_stack_depth(), 2);
        assert_eq!(
            program.disassemble(),
            "0000  PUSH 6\n0001  PUSH 7\n0002  MUL\n0003  HALT"
        );
    }

    #[test]
    fn rejects_structurally_invalid_programs_before_execution() {
        assert!(
            "PUSH 1\nADD\nHALT"
                .parse::<Program>()
                .unwrap_err()
                .to_string()
                .contains("line 2")
        );
        assert!(
            "PUSH 1"
                .parse::<Program>()
                .unwrap_err()
                .to_string()
                .contains("end with HALT")
        );
        assert!(
            "PUSH 1\nHALT\nPUSH 2"
                .parse::<Program>()
                .unwrap_err()
                .to_string()
                .contains("after HALT")
        );
    }

    #[test]
    fn parses_control_flow_and_disassembles_targets() {
        let program: Program = "PUSH 5\nDUP\nJZ 6\nPUSH 1\nSUB\nJMP 1\nHALT"
            .parse()
            .unwrap();
        assert_eq!(
            program.disassemble(),
            "0000  PUSH 5\n0001  DUP\n0002  JZ 6\n0003  PUSH 1\n0004  SUB\n0005  JMP 1\n0006  HALT"
        );
        assert_eq!(program.max_stack_depth(), 2);
    }

    #[test]
    fn rejects_out_of_range_jump_targets() {
        assert!(
            "PUSH 1\nJMP 9\nHALT"
                .parse::<Program>()
                .unwrap_err()
                .to_string()
                .contains("outside the program")
        );
        assert!(
            "JZ".parse::<Program>()
                .unwrap_err()
                .to_string()
                .contains("exactly one target index")
        );
    }

    #[test]
    fn rejects_branches_that_disagree_on_stack_depth() {
        // The fallthrough path reaches HALT with depth 1 while the JZ path
        // arrives with depth 0: the join is ambiguous, so validation fails.
        assert!(
            "PUSH 1\nJZ 3\nPUSH 2\nHALT"
                .parse::<Program>()
                .unwrap_err()
                .to_string()
                .contains("ambiguous stack depth")
        );
    }
}
