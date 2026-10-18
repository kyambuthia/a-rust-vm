use super::*;

#[test]
fn registry_exposes_vm_tools_in_stable_order() {
    let registry = vm_tool_registry().unwrap();
    let names = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        vec![
            "compile_program",
            "disassemble_program",
            "inspect_vm",
            "reset_vm",
            "run_program",
            "step_vm",
            "trace_program"
        ]
    );
}

#[test]
fn compile_tool_validates_and_formats_a_program() {
    let mut registry = vm_tool_registry().unwrap();
    let result = registry.execute(&ToolCall::new(
        "compile-1",
        "compile_program",
        program_arguments("PUSH 2\nPUSH 3\nADD\nHALT"),
    ));

    assert_eq!(
        result.content,
        "instructions=4\n00 PUSH 2\n01 PUSH 3\n02 ADD\n03 HALT"
    );
    assert!(!result.is_error);
}

#[test]
fn trace_tool_reports_instruction_and_stack_state() {
    let mut registry = vm_tool_registry().unwrap();
    let result = registry.execute(&ToolCall::new(
        "trace-1",
        "trace_program",
        program_arguments("PUSH 2\nPUSH 3\nADD\nHALT"),
    ));

    assert_eq!(
        result.content,
        "ip=0 instruction=PUSH 2 stack=[2]\nip=1 instruction=PUSH 3 stack=[2, 3]\nip=2 instruction=ADD stack=[5]\nip=3 instruction=HALT result=5 stack=[5]"
    );
    assert!(!result.is_error);
}

#[test]
fn vm_tools_share_state() {
    let mut registry = vm_tool_registry().unwrap();
    let run = ToolCall::new(
        "run-1",
        "run_program",
        program_arguments("PUSH 2\nPUSH 3\nADD\nHALT"),
    );
    let result = registry.execute(&run);
    assert_eq!(result.content, "result=5 ip=4 stack=[5]");
    assert!(!result.is_error);

    let inspect = ToolCall::new("inspect-1", "inspect_vm", ToolArguments::new());
    let result = registry.execute(&inspect);
    assert_eq!(result.content, "loaded=true ip=4 halted=true stack=[5]");
}

#[test]
fn malformed_program_is_reported_as_a_tool_error() {
    let mut registry = vm_tool_registry().unwrap();
    let call = ToolCall::new(
        "run-1",
        "run_program",
        program_arguments("PUSH 2\nWHAT\nHALT"),
    );

    let result = registry.execute(&call);
    assert!(result.is_error);
    assert!(result.content.contains("unknown instruction 'WHAT'"));
}

#[test]
fn unexpected_tool_arguments_are_rejected() {
    let mut registry = vm_tool_registry().unwrap();
    let call = ToolCall::new(
        "inspect-1",
        "inspect_vm",
        [("unexpected".to_owned(), ToolValue::Boolean(true))]
            .into_iter()
            .collect(),
    );

    let result = registry.execute(&call);
    assert!(result.is_error);
    assert_eq!(result.content, "unexpected argument: unexpected");
}
