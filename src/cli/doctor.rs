//! `arvm doctor`: platform capabilities and readiness.

pub(crate) fn run_doctor(arguments: &[String]) {
    if arguments.iter().any(|argument| argument != "--json") || arguments.len() > 1 {
        eprintln!("usage: arvm doctor [--json]");
        std::process::exit(2);
    }
    let info = a_rust_vm::protocol::SystemInfo::current();
    if arguments
        .first()
        .is_some_and(|argument| argument == "--json")
    {
        println!(
            "{}",
            serde_json::to_string_pretty(&info).expect("system info is serializable")
        );
        return;
    }
    println!(
        "{} {} · API {}",
        info.product, info.version, info.api_version
    );
    println!("workspace control plane: available (in-memory)");
    println!("bytecode executor: available");
    println!("Wasm/WASI executor: not installed");
    println!("native sandbox executor: not installed");
}
