//! End-to-end checks that drive the real `arvm` binary with a scripted
//! model process, so tool, permission, session, and cancellation behaviour
//! is tested the way a user meets it.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A throwaway workspace, state directory, and config directory.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("arvm-cli-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for directory in ["workspace", "state", "config"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        Self { root }
    }

    fn workspace(&self) -> PathBuf {
        self.root.join("workspace")
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.workspace().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.workspace().join(path)).unwrap()
    }

    /// A model that emits `first` (JSON event lines) on the first step and
    /// answers with `after` once the request carries a tool result.
    fn model(&self, first: &[&str], after: &str) -> PathBuf {
        let quote = |line: &str| format!("'{}'", line.replace('\'', r"'\''"));
        let first = first
            .iter()
            .map(|line| format!("  printf '%s\\n' {}\n", quote(line)))
            .collect::<String>();
        let after = quote(&format!(r#"{{"type":"text","text":"{after}"}}"#));
        let script = format!(
            "#!/bin/sh\nrequest=$(cat)\ncase \"$request\" in\n*tool_call_id*)\n  printf '%s\\n' {after}\n  ;;\n*)\n{first}  ;;\nesac\nprintf '%s\\n' '{{\"type\":\"done\"}}'\n"
        );
        let path = self.root.join("model.sh");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn command(&self, model: &Path, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_arvm"));
        command
            .args(arguments)
            .current_dir(self.workspace())
            .env("A_RVM_MODEL_PROGRAM", model)
            .env("A_RVM_STATE_DIR", self.root.join("state"))
            .env("A_RVM_CONFIG_DIR", self.root.join("config"))
            .env_remove("OPENROUTER_API_KEY")
            .stdin(Stdio::null());
        command
    }

    fn ask(&self, model: &Path, arguments: &[&str]) -> Output {
        let mut all = vec!["ask"];
        all.extend_from_slice(arguments);
        self.command(model, &all).output().unwrap()
    }

    /// The `--json` events of a run, one value per line.
    fn events(output: &Output) -> Vec<serde_json::Value> {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn tool_results(output: &Output) -> Vec<String> {
        Self::events(output)
            .into_iter()
            .filter(|event| event["type"] == "tool_result")
            .map(|event| event["content"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn session(&self, id: &str) -> serde_json::Value {
        let path = self.root.join("state/sessions").join(format!("{id}.json"));
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> String {
    serde_json::json!({"type": "tool_call", "id": id, "name": name, "arguments": arguments})
        .to_string()
}

#[test]
fn edit_file_with_auto_changes_the_file_and_records_the_session() {
    let fixture = Fixture::new("edit");
    fixture.write("a.txt", "say hello\n");
    let call = tool_call(
        "c1",
        "edit_file",
        serde_json::json!({"path": "a.txt", "old_string": "hello", "new_string": "goodbye"}),
    );
    let model = fixture.model(&[&call], "edited");

    let output = fixture.ask(&model, &["--auto", "--json", "--session", "s1", "edit it"]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.read("a.txt"), "say goodbye\n");
    let results = Fixture::tool_results(&output);
    assert!(results[0].contains("-hello\n+goodbye"), "{results:?}");
    let session = fixture.session("s1");
    assert_eq!(session["schema"], 2);
    assert_eq!(session["turns"].as_array().unwrap().len(), 1);
    let mode = fs::metadata(fixture.root.join("state/sessions/s1.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn write_file_rules_govern_edits_and_plain_ask_refuses_guarded_tools() {
    let fixture = Fixture::new("rules");
    fixture.write("secrets.txt", "hello secret\n");
    let call = tool_call(
        "c1",
        "edit_file",
        serde_json::json!({"path": "secrets.txt", "old_string": "hello", "new_string": "x"}),
    );
    let model = fixture.model(&[&call], "done");

    let denied = fixture.ask(
        &model,
        &[
            "--auto",
            "--json",
            "--rule",
            "deny write_file:secrets",
            "go",
        ],
    );
    assert!(Fixture::tool_results(&denied)[0].contains("permission rule denies"));
    let refused = fixture.ask(&model, &["--json", "go"]);
    assert!(Fixture::tool_results(&refused)[0].contains("one-shot mode denies"));
    assert_eq!(fixture.read("secrets.txt"), "hello secret\n");
}

#[test]
fn read_only_commands_run_without_approval_and_others_still_ask() {
    let fixture = Fixture::new("read-only");
    fixture.write("notes.txt", "visible\n");
    let read = tool_call(
        "c1",
        "run_command",
        serde_json::json!({"command": "cat notes.txt"}),
    );
    let write = tool_call(
        "c2",
        "run_command",
        serde_json::json!({"command": "touch made.txt"}),
    );
    let model = fixture.model(&[&read, &write], "done");

    let output = fixture.ask(&model, &["--json", "go"]);

    let results = Fixture::tool_results(&output);
    assert!(results[0].contains("visible"), "{results:?}");
    assert!(results[1].contains("one-shot mode denies"), "{results:?}");
    assert!(!fixture.workspace().join("made.txt").exists());
}

#[test]
fn writes_inside_git_are_refused_even_with_auto() {
    let fixture = Fixture::new("git");
    fixture.write(".git/config", "[core]\n");
    let call = tool_call(
        "c1",
        "write_file",
        serde_json::json!({"path": ".git/config", "content": "[core]\n\tfsmonitor = ./x\n"}),
    );
    let model = fixture.model(&[&call], "done");

    let output = fixture.ask(&model, &["--auto", "--json", "go"]);

    let results = Fixture::tool_results(&output);
    assert!(
        results[0].contains("refusing to write inside .git"),
        "{results:?}"
    );
    assert_eq!(fixture.read(".git/config"), "[core]\n");
}

#[test]
fn secrets_in_tool_output_never_reach_the_screen_or_the_session() {
    let fixture = Fixture::new("redact");
    let secret = "sk-or-v1-0123456789abcdef0123456789abcdef";
    fixture.write(".env", &format!("OPENROUTER_API_KEY={secret}\n"));
    let call = tool_call("c1", "read_file", serde_json::json!({"path": ".env"}));
    let model = fixture.model(&[&call], "read");

    let output = fixture.ask(&model, &["--json", "--session", "s2", "go"]);

    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
    assert!(Fixture::tool_results(&output)[0].contains("[redacted]"));
    let session = fs::read_to_string(fixture.root.join("state/sessions/s2.json")).unwrap();
    assert!(!session.contains(secret));
}

#[test]
fn several_tool_calls_in_one_step_are_recorded_calls_first() {
    let fixture = Fixture::new("batch");
    fixture.write("a.txt", "A\n");
    fixture.write("b.txt", "B\n");
    let first = tool_call("r1", "read_file", serde_json::json!({"path": "a.txt"}));
    let second = tool_call("r2", "read_file", serde_json::json!({"path": "b.txt"}));
    let model = fixture.model(&[&first, &second], "both");

    let output = fixture.ask(&model, &["--json", "--session", "s3", "go"]);

    assert_eq!(Fixture::tool_results(&output), ["A\n", "B\n"]);
    let layout = fixture.session("s3")["conversation"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| {
            if let Some(call) = message.get("tool_call").filter(|call| !call.is_null()) {
                format!("call {}", call["id"].as_str().unwrap())
            } else if let Some(id) = message["tool_call_id"].as_str() {
                format!("result {id}")
            } else {
                message["role"].as_str().unwrap().to_owned()
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        layout,
        [
            "user",
            "call r1",
            "call r2",
            "result r1",
            "result r2",
            "assistant"
        ]
    );
}

#[test]
fn ctrl_c_cancels_a_running_command_and_keeps_the_session() {
    let fixture = Fixture::new("cancel");
    let call = tool_call(
        "c1",
        "run_command",
        serde_json::json!({"command": "sleep 30 && echo never"}),
    );
    let model = fixture.model(&[&call], "done");
    let started = Instant::now();
    let mut child = fixture
        .command(&model, &["ask", "--auto", "--session", "s4", "go"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Wait until arvm reports the command, then interrupt it once.
    let stdout = child.stdout.take().unwrap();
    let (seen_sender, seen) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        let mut text = String::new();
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if line.contains("[tool] run_command") {
                let _ = seen_sender.send(());
            }
            text.push_str(&line);
            text.push('\n');
        }
        text
    });
    seen.recv_timeout(Duration::from_secs(20))
        .expect("arvm should start the command");
    std::thread::sleep(Duration::from_millis(200));
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let status = child.wait().unwrap();
    let stdout = reader.join().unwrap();

    assert!(status.success(), "{status:?}: {stdout}");
    assert!(started.elapsed() < Duration::from_secs(25));
    assert!(stdout.contains("command cancelled"), "{stdout}");
    let events = fixture.session("s4")["turns"][0]["events"].clone();
    assert!(
        events
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "cancelled"),
        "{events}"
    );
}
