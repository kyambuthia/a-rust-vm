//! `arvm run|check|disassemble|trace`: assembly programs on the VM.

use std::io::{self, Read};

use a_rust_vm::Vm;
use a_rust_vm::program::Program;

pub(crate) fn run_program_command(command: &str, arguments: &[String]) {
    let json = arguments.iter().any(|argument| argument == "--json");
    let paths = arguments
        .iter()
        .filter(|argument| argument.as_str() != "--json")
        .collect::<Vec<_>>();
    if paths.len() != 1 || (command != "run" && json) {
        eprintln!(
            "usage: arvm {command} <file|->{}",
            if command == "run" { " [--json]" } else { "" }
        );
        std::process::exit(2);
    }
    let path = paths[0];
    let source = if path.as_str() == "-" {
        let mut source = String::new();
        io::stdin()
            .read_to_string(&mut source)
            .unwrap_or_else(|error| {
                eprintln!("cannot read program from stdin: {error}");
                std::process::exit(2);
            });
        source
    } else {
        std::fs::read_to_string(path).unwrap_or_else(|error| {
            eprintln!("cannot read program '{path}': {error}");
            std::process::exit(2);
        })
    };
    let program = source.parse::<Program>().unwrap_or_else(|error| {
        eprintln!("invalid program: {error}");
        std::process::exit(2);
    });
    match command {
        "check" => println!(
            "valid: {} instructions, max stack depth {}",
            program.instructions().len(),
            program.max_stack_depth()
        ),
        "disassemble" => println!("{}", program.disassemble()),
        "trace" => {
            let entries = Vm::new().trace(&program).unwrap_or_else(|error| {
                eprintln!("VM error: {error:?}");
                std::process::exit(1);
            });
            for entry in entries {
                match entry.result {
                    Some(result) => println!(
                        "ip={} instruction=HALT result={result} stack={:?}",
                        entry.instruction_pointer, entry.stack
                    ),
                    None => println!(
                        "ip={} instruction={} stack={:?}",
                        entry.instruction_pointer, entry.instruction, entry.stack
                    ),
                }
            }
        }
        "run" => {
            let result = Vm::new().run_program(&program).unwrap_or_else(|error| {
                eprintln!("VM error: {error:?}");
                std::process::exit(1);
            });
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "result": result,
                        "instructions": program.instructions().len(),
                        "max_stack_depth": program.max_stack_depth()
                    })
                );
            } else {
                println!("{result}");
            }
        }
        _ => unreachable!("command validated by caller"),
    }
}
