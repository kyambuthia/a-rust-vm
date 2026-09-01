use a_rust_vm::{Instruction, Vm};

fn main() {
    let program = [
        Instruction::Push(2),
        Instruction::Push(3),
        Instruction::Push(4),
        Instruction::Mul,
        Instruction::Add,
        Instruction::Halt,
    ];

    println!("Executing bytecode: {program:?}");

    let mut vm = Vm::new();
    match vm.run(&program) {
        Ok(result) => println!("Result: {result}"),
        Err(error) => {
            eprintln!("VM error: {error:?}");
            std::process::exit(1);
        }
    }
}
