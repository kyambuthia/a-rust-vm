use std::{fs, process::Command};

#[test]
fn module_command_loads_validates_and_runs_external_images() {
    let directory = std::env::temp_dir().join(format!("arvm-module-cli-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let image = directory.join("module.json");
    let inputs = directory.join("input.json");
    fs::write(
        &image,
        r#"{
            "version": 2,
            "entry": 0,
            "input_count": 1,
            "constants": [{"text":"answer"}],
            "functions": [{"arity":0,"locals":0,"code":[
                "map_new",{"const_value":0},{"input":0},"map_insert","return"
            ]}]
        }"#,
    )
    .unwrap();
    fs::write(&inputs, r#"[{"integer":42}]"#).unwrap();

    let check = Command::new(env!("CARGO_BIN_EXE_arvm"))
        .args(["module", "check", image.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(check.status.success(), "{check:?}");
    assert!(String::from_utf8_lossy(&check.stdout).contains("5 instructions"));

    let run = Command::new(env!("CARGO_BIN_EXE_arvm"))
        .args([
            "module",
            "run",
            image.to_str().unwrap(),
            "--input",
            inputs.to_str().unwrap(),
            "--heap-bytes",
            "1024",
        ])
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    let output: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(output["result"]["map"]["answer"]["integer"], 42);
    assert_eq!(output["steps"], 5);

    let too_small = Command::new(env!("CARGO_BIN_EXE_arvm"))
        .args([
            "module",
            "run",
            image.to_str().unwrap(),
            "--input",
            inputs.to_str().unwrap(),
            "--heap-bytes",
            "0",
        ])
        .output()
        .unwrap();
    assert_eq!(too_small.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&too_small.stderr).contains("HeapLimit"));

    fs::remove_dir_all(directory).unwrap();
}
