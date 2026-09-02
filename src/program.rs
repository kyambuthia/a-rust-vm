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

        let mut depth = 0usize;
        let mut max_stack_depth = 0usize;
        for (index, instruction) in instructions.iter().enumerate() {
            match instruction {
                Instruction::Push(_) => {
                    depth += 1;
                    max_stack_depth = max_stack_depth.max(depth);
                }
                Instruction::Add | Instruction::Sub | Instruction::Mul | Instruction::Div => {
                    if depth < 2 {
                        return Err(ProgramError::new(
                            Some(index + 1),
                            format!("{} requires two stack values", instruction.name()),
                        ));
                    }
                    depth -= 1;
                }
                Instruction::Halt => {
                    if depth == 0 {
                        return Err(ProgramError::new(
                            Some(index + 1),
                            "HALT requires a result on the stack",
                        ));
                    }
                    if index + 1 != instructions.len() {
                        return Err(ProgramError::new(
                            Some(index + 2),
                            "instructions after HALT are unreachable",
                        ));
                    }
                }
            }
        }
        if !matches!(instructions.last(), Some(Instruction::Halt)) {
            return Err(ProgramError::new(None, "program must end with HALT"));
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
                "ADD" | "SUB" | "MUL" | "DIV" | "HALT" if fields.len() != 1 => {
                    return Err(ProgramError::new(
                        Some(source_line),
                        format!("{opcode} does not accept operands"),
                    ));
                }
                "ADD" => Instruction::Add,
                "SUB" => Instruction::Sub,
                "MUL" => Instruction::Mul,
                "DIV" => Instruction::Div,
                "HALT" => Instruction::Halt,
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
}
