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

#[test]
fn module_command_runs_text_assembly() {
    let directory =
        std::env::temp_dir().join(format!("arvm-module-assembly-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("program.arvm");
    fs::write(&source, "entry main\nfn main 0 0\n const 21\n call twice\n return\nend\nfn twice 1 1\n local_get 0\n const 2\n mul\n return\nend\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_arvm"))
        .args(["module", "run", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"]["integer"], 42);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn module_command_replays_only_matching_import_responses() {
    let directory = std::env::temp_dir().join(format!("arvm-module-replay-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("program.arvm");
    let replies = directory.join("replies.json");
    fs::write(&source, "entry main\nimport math.double 1\nfn main 0 0\n const 21\n import math.double\n return\nend\n").unwrap();
    fs::write(
        &replies,
        r#"[{"name":"math.double","arguments":[{"integer":21}],"result":{"integer":42}}]"#,
    )
    .unwrap();
    let denied = Command::new(env!("CARGO_BIN_EXE_arvm"))
        .args(["module", "run", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(denied.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("ImportDenied"));
    let run = Command::new(env!("CARGO_BIN_EXE_arvm"))
        .args([
            "module",
            "run",
            source.to_str().unwrap(),
            "--import-replies",
            replies.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    let output: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(output["result"]["integer"], 42);
    assert_eq!(output["steps"], 3);
    fs::write(
        &replies,
        r#"[{"name":"math.double","arguments":[{"integer":20}],"result":{"integer":42}}]"#,
    )
    .unwrap();
    let mismatch = Command::new(env!("CARGO_BIN_EXE_arvm"))
        .args([
            "module",
            "run",
            source.to_str().unwrap(),
            "--import-replies",
            replies.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(mismatch.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("does not match"));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn module_live_read_requires_grant_and_stays_in_workspace() {
    let directory = std::env::temp_dir().join(format!("arvm-module-live-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("program.arvm");
    fs::write(&source, "entry main\nconst path {\"text\":\"note.txt\"}\nimport workspace.read_file 1\nfn main 0 0\n const_value path\n import workspace.read_file\n return\nend\n").unwrap();
    fs::write(directory.join("note.txt"), "workspace content").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_arvm"))
        .args([
            "module",
            "run",
            source.to_str().unwrap(),
            "--allow-import",
            "workspace.read_file",
            "--workspace",
            directory.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    let output: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(output["result"]["text"], "workspace content");

    let escaped = directory.join("escape.arvm");
    fs::write(&escaped, "entry main\nconst path {\"text\":\"../outside.txt\"}\nimport workspace.read_file 1\nfn main 0 0\n const_value path\n import workspace.read_file\n return\nend\n").unwrap();
    let denied = Command::new(env!("CARGO_BIN_EXE_arvm"))
        .args([
            "module",
            "run",
            escaped.to_str().unwrap(),
            "--allow-import",
            "workspace.read_file",
            "--workspace",
            directory.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(denied.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("cannot contain '..'"));
    fs::remove_dir_all(directory).unwrap();
}
