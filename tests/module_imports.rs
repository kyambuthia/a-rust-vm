use a_rust_vm::{
    module::{
        Function, Import, MODULE_VERSION, Module, ModuleVm, ModuleVmError, Op, OwnedValue, Value,
    },
    module_assembly::assemble_module,
    module_format::parse_module,
};

fn program() -> Module {
    Module::with_imports(
        MODULE_VERSION,
        vec![Function::new(
            0,
            0,
            vec![Op::Const(21), Op::Import(0), Op::Return],
        )],
        0,
        vec![],
        0,
        vec![Import {
            name: "math.double".into(),
            arity: 1,
        }],
    )
    .unwrap()
}

#[test]
fn import_requires_an_explicit_grant_and_host_result() {
    let program = program();
    let mut denied = ModuleVm::new(&program);
    denied.step().unwrap();
    assert_eq!(denied.step().unwrap_err(), ModuleVmError::ImportDenied);
    assert_eq!(denied.steps(), 1);
    assert!(denied.pending_import().unwrap().is_none());

    let mut vm =
        ModuleVm::with_inputs_and_imports(&program, vec![], 1024, &["math.double".into()]).unwrap();
    vm.step().unwrap();
    let step = vm.step().unwrap();
    assert_eq!(step.instruction, Op::Import(0));
    assert_eq!(vm.steps(), 1);
    let request = vm.pending_import().unwrap().unwrap();
    assert_eq!(request.name, "math.double");
    assert_eq!(request.arguments, vec![OwnedValue::Integer(21)]);
    assert_eq!(vm.step().unwrap_err(), ModuleVmError::ImportPending);
    vm.resume_import(&OwnedValue::Integer(42)).unwrap();
    assert_eq!(vm.steps(), 2);
    assert!(vm.pending_import().unwrap().is_none());
    assert_eq!(vm.run().unwrap(), Value::Integer(42));
}

#[test]
fn failed_host_result_keeps_request_pending() {
    let program = program();
    let mut vm =
        ModuleVm::with_inputs_and_imports(&program, vec![], 0, &["math.double".into()]).unwrap();
    vm.step().unwrap();
    vm.step().unwrap();
    assert_eq!(
        vm.resume_import(&OwnedValue::Text("large".into())),
        Err(ModuleVmError::HeapLimit)
    );
    assert_eq!(
        vm.pending_import().unwrap().unwrap().arguments,
        vec![OwnedValue::Integer(21)]
    );
    vm.resume_import(&OwnedValue::Integer(42)).unwrap();
    assert_eq!(vm.run().unwrap(), Value::Integer(42));
}

#[test]
fn import_declarations_are_validated_in_both_source_formats() {
    assert!(
        Module::with_imports(
            MODULE_VERSION,
            vec![Function::new(0, 0, vec![Op::Import(0), Op::Return])],
            0,
            vec![],
            0,
            vec![]
        )
        .is_err()
    );
    let image = br#"{"version":2,"entry":0,"imports":[{"name":"math.double","arity":1}],"functions":[{"arity":0,"locals":0,"code":[{"const":21},{"import":0},"return"]}]}"#;
    assert_eq!(parse_module(image).unwrap(), program());
    let source = "entry main\nimport math.double 1\nfn main 0 0\n const 21\n import math.double\n return\nend\n";
    assert_eq!(assemble_module(source).unwrap(), program());
}
