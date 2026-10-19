use super::*;

#[test]
fn scripted_agent_emits_tool_and_completion_events() {
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new(
            "run-1",
            "run_program",
            program_arguments("PUSH 8\nPUSH 5\nMUL\nHALT"),
        )),
        ModelResponse::Text("The VM result is 40.".to_owned()),
    ]);
    let mut agent = Agent::new(model, vm_tool_registry().unwrap());

    let events = agent.run("multiply 8 by 5").unwrap();

    assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
    assert!(matches!(events[1], AgentEvent::ToolCall(_)));
    assert!(matches!(events[2], AgentEvent::ToolResult(ref result) if !result.is_error));
    assert_eq!(
        events[3],
        AgentEvent::AssistantText {
            content: "The VM result is 40.".to_owned()
        }
    );
    assert_eq!(events[4], AgentEvent::Done);
}

#[test]
fn streaming_agent_emits_text_deltas_and_completion() {
    let model = ScriptedModel::new([ModelResponse::Text("ready".to_owned())]);
    let mut agent = Agent::new(model, vm_tool_registry().unwrap());
    let mut events = Vec::new();

    agent
        .run_streaming("say ready", |event| events.push(event))
        .unwrap();

    assert_eq!(
        events,
        vec![
            AgentEvent::UserMessage {
                content: "say ready".to_owned()
            },
            AgentEvent::AssistantDelta {
                content: "ready".to_owned()
            },
            AgentEvent::Done,
        ]
    );
}

#[test]
fn agent_sends_project_prompt_and_bounded_prior_tool_history() {
    let requests = Rc::new(RefCell::new(Vec::new()));
    let model = RecordingModel {
        requests: requests.clone(),
        responses: [
            ModelResponse::ToolCall(ToolCall::new(
                "inspect-1",
                "inspect_vm",
                ToolArguments::new(),
            )),
            ModelResponse::Text("The VM is empty.".to_owned()),
            ModelResponse::Text("The previous VM was empty.".to_owned()),
        ]
        .into_iter()
        .collect(),
    };
    let mut agent = Agent::new(model, vm_tool_registry().unwrap())
        .with_system_prompt("workspace rules")
        .with_context_limits(8, 1024);

    agent.run("inspect the VM").unwrap();
    agent.run("what did we learn?").unwrap();

    let requests = requests.borrow();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].system_prompt, "workspace rules");
    assert!(requests[0].conversation.is_empty());
    assert_eq!(requests[1].current_turn.len(), 3);
    assert_eq!(requests[1].current_turn[0].role, "user");
    assert_eq!(
        requests[1].current_turn[1]
            .tool_call
            .as_ref()
            .map(|call| call.name.as_str()),
        Some("inspect_vm")
    );
    assert_eq!(
        requests[1].current_turn[2].tool_call_id.as_deref(),
        Some("inspect-1")
    );
    assert_eq!(requests[2].prompt, "what did we learn?");
    assert!(
        requests[2]
            .conversation
            .iter()
            .any(|message| message.role == "tool" && message.content.contains("loaded=false"))
    );
    assert!(requests[2].conversation.len() <= 8);
    assert!(agent.conversation().len() <= 8);
}

#[test]
fn model_boundary_serializes_tool_arguments_as_json_objects() {
    let call = ToolCall::new("run-1", "run_program", program_arguments("PUSH 2\nHALT"));

    let json = serde_json::to_string(&call).unwrap();

    assert_eq!(
        json,
        r#"{"id":"run-1","name":"run_program","arguments":{"program":"PUSH 2\nHALT"}}"#
    );
}

#[cfg(unix)]
#[test]
fn agent_stops_after_the_configured_step_limit() {
    let model = ScriptedModel::new([ModelResponse::ToolCall(ToolCall::new(
        "inspect-1",
        "inspect_vm",
        ToolArguments::new(),
    ))]);
    let mut agent = Agent::new(model, vm_tool_registry().unwrap()).with_max_steps(1);

    let error = agent.run("inspect").unwrap_err();

    assert_eq!(error, super::AgentError::StepLimitExceeded { limit: 1 });
}

#[test]
fn repeated_identical_tool_call_stops_before_second_execution() {
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new("probe-1", "inspect_vm", ToolArguments::new())),
        ModelResponse::ToolCall(ToolCall::new("probe-2", "inspect_vm", ToolArguments::new())),
        ModelResponse::Text("unreachable".to_owned()),
    ]);
    let mut agent = Agent::new(model, vm_tool_registry().unwrap()).with_max_steps(4);

    let events = agent.run("probe").unwrap();

    let tool_calls = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolCall(_)))
        .count();
    let repeats = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::RepeatedToolCall { .. }))
        .count();
    assert_eq!(tool_calls, 1);
    assert_eq!(repeats, 1);
    assert!(events.iter().any(|event| matches!(event, AgentEvent::Done)));
}

#[test]
fn pattern_loop_of_two_tools_stops_with_pattern_error() {
    let empty_a = ToolCall::new("a-1", "inspect_vm", ToolArguments::new());
    let tagged_b = ToolCall::new(
        "b-1",
        "inspect_vm",
        [("tag".to_owned(), ToolValue::Text("b".to_owned()))]
            .into_iter()
            .collect(),
    );
    let empty_a_again = ToolCall::new("a-2", "inspect_vm", ToolArguments::new());
    let tagged_b_again = ToolCall::new(
        "b-2",
        "inspect_vm",
        [("tag".to_owned(), ToolValue::Text("b".to_owned()))]
            .into_iter()
            .collect(),
    );
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(empty_a),
        ModelResponse::ToolCall(tagged_b),
        ModelResponse::ToolCall(empty_a_again),
        ModelResponse::ToolCall(tagged_b_again),
        ModelResponse::Text("unreachable".to_owned()),
    ]);
    let mut agent = Agent::new(model, vm_tool_registry().unwrap())
        .with_max_steps(10)
        .with_max_tool_calls(32);

    let error = agent.run("probe").unwrap_err();

    assert_eq!(error, super::AgentError::PatternLoopDetected { period: 2 });
}

#[test]
fn tool_call_cap_ends_turn_before_extra_execution() {
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new("a-1", "inspect_vm", ToolArguments::new())),
        ModelResponse::ToolCall(ToolCall::new(
            "b-1",
            "inspect_vm",
            [("tag".to_owned(), ToolValue::Text("b".to_owned()))]
                .into_iter()
                .collect(),
        )),
        ModelResponse::ToolCall(ToolCall::new(
            "c-1",
            "inspect_vm",
            [("tag".to_owned(), ToolValue::Text("c".to_owned()))]
                .into_iter()
                .collect(),
        )),
        ModelResponse::Text("unreachable".to_owned()),
    ]);
    let mut agent = Agent::new(model, vm_tool_registry().unwrap())
        .with_max_steps(10)
        .with_max_tool_calls(2);

    let error = agent.run("probe").unwrap_err();

    assert_eq!(error, super::AgentError::ToolCallLimitExceeded { limit: 2 });
}

#[test]
fn pre_cancelled_agent_finishes_without_calling_the_model() {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let model = ScriptedModel::new([ModelResponse::Text("should not run".to_owned())]);
    let mut agent = Agent::new(model, vm_tool_registry().unwrap()).with_cancel_token(cancel);

    let events = agent.run("ping").unwrap();

    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Cancelled))
    );
    assert!(events.iter().any(|event| matches!(event, AgentEvent::Done)));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::AssistantText { .. }))
    );
}

#[test]
fn expired_turn_timeout_stops_before_model_call() {
    let model = ScriptedModel::new([ModelResponse::Text("hi".to_owned())]);
    let mut agent =
        Agent::new(model, vm_tool_registry().unwrap()).with_turn_timeout(std::time::Duration::ZERO);

    assert_eq!(
        agent.run("ping").unwrap_err(),
        super::AgentError::TurnTimeoutExceeded
    );
}

#[test]
fn different_tool_arguments_are_not_treated_as_repeats() {
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new("probe-1", "inspect_vm", ToolArguments::new())),
        ModelResponse::ToolCall(ToolCall::new(
            "probe-2",
            "inspect_vm",
            [("tag".to_owned(), ToolValue::Text("b".to_owned()))]
                .into_iter()
                .collect(),
        )),
        ModelResponse::Text("done".to_owned()),
    ]);
    let mut agent = Agent::new(model, vm_tool_registry().unwrap()).with_max_steps(4);

    let events = agent.run("probe").unwrap();

    let tool_calls = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolCall(_)))
        .count();
    assert_eq!(tool_calls, 2);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::RepeatedToolCall { .. }))
    );
}

#[test]
fn unknown_tool_calls_return_explicit_errors() {
    let mut registry = ToolRegistry::default();
    let result = registry.execute(&ToolCall::new(
        "missing-1",
        "missing_tool",
        ToolArguments::new(),
    ));

    assert!(result.is_error);
    assert_eq!(result.content, "unknown tool: missing_tool");
}

#[test]
fn denied_workspace_write_emits_permission_and_does_not_write() {
    let root =
        std::env::temp_dir().join(format!("a-rust-vm-agent-permission-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new(
            "write-1",
            "write_file",
            [
                ("path".to_owned(), ToolValue::Text("blocked.txt".to_owned())),
                ("content".to_owned(), ToolValue::Text("nope".to_owned())),
            ]
            .into_iter()
            .collect(),
        )),
        ModelResponse::Text("The write was not approved.".to_owned()),
    ]);
    let mut agent = Agent::new(
        model,
        crate::workspace::workspace_tool_registry(&root).unwrap(),
    );

    let events = agent
        .run_with_approval("write a file", |_| PermissionDecision::Deny {
            reason: "test denial".to_owned(),
        })
        .unwrap();

    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::PermissionRequested(_)))
    );
    assert!(events.iter().any(|event| {
        matches!(event, AgentEvent::ToolResult(result) if result.is_error && result.content == "test denial")
    }));
    assert!(!root.join("blocked.txt").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn allow_rule_executes_guarded_tool_without_prompting() {
    let root =
        std::env::temp_dir().join(format!("a-rust-vm-agent-allow-rule-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new(
            "write-1",
            "write_file",
            [
                ("path".to_owned(), ToolValue::Text("notes.txt".to_owned())),
                ("content".to_owned(), ToolValue::Text("allowed".to_owned())),
            ]
            .into_iter()
            .collect(),
        )),
        ModelResponse::Text("done".to_owned()),
    ]);
    let policy = crate::permissions::PermissionPolicy::with_rules(vec![
        crate::permissions::parse_permission_rule("allow write_file:notes.txt").unwrap(),
    ]);
    let mut agent = Agent::new(
        model,
        crate::workspace::workspace_tool_registry(&root).unwrap(),
    )
    .with_permission_policy(policy);
    let mut prompts = 0;

    let events = agent
        .run_with_approval("write a file", |_| {
            prompts += 1;
            PermissionDecision::Allow
        })
        .unwrap();

    assert_eq!(prompts, 0);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::PermissionRequested(_)))
    );
    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt")).unwrap(),
        "allowed"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn deny_rule_blocks_guarded_tool_without_prompting() {
    let root =
        std::env::temp_dir().join(format!("a-rust-vm-agent-deny-rule-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new(
            "write-1",
            "write_file",
            [
                ("path".to_owned(), ToolValue::Text("secrets.txt".to_owned())),
                ("content".to_owned(), ToolValue::Text("blocked".to_owned())),
            ]
            .into_iter()
            .collect(),
        )),
        ModelResponse::Text("done".to_owned()),
    ]);
    let policy = crate::permissions::PermissionPolicy::with_rules(vec![
        crate::permissions::parse_permission_rule("deny write_file:secrets").unwrap(),
    ]);
    let mut agent = Agent::new(
        model,
        crate::workspace::workspace_tool_registry(&root).unwrap(),
    )
    .with_permission_policy(policy);
    let mut prompts = 0;

    let events = agent
        .run_with_approval("write a file", |_| {
            prompts += 1;
            PermissionDecision::Allow
        })
        .unwrap();

    assert_eq!(prompts, 0);
    assert!(events.iter().any(|event| {
        matches!(event, AgentEvent::ToolResult(result) if result.is_error && result.content.contains("permission rule denies"))
    }));
    assert!(!root.join("secrets.txt").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn allow_rule_does_not_override_dangerous_command_refusal() {
    let root = std::env::temp_dir().join(format!(
        "a-rust-vm-agent-dangerous-command-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new(
            "command-1",
            "run_command",
            [("command".to_owned(), ToolValue::Text("rm -rf /".to_owned()))]
                .into_iter()
                .collect(),
        )),
        ModelResponse::Text("done".to_owned()),
    ]);
    let policy = crate::permissions::PermissionPolicy::with_rules(vec![
        crate::permissions::parse_permission_rule("allow run_command").unwrap(),
    ]);
    let mut agent = Agent::new(
        model,
        crate::workspace::workspace_tool_registry(&root).unwrap(),
    )
    .with_permission_policy(policy);
    let mut prompts = 0;

    let events = agent
        .run_with_approval("run a command", |_| {
            prompts += 1;
            PermissionDecision::Allow
        })
        .unwrap();

    assert_eq!(prompts, 0);
    assert!(events.iter().any(|event| {
        matches!(event, AgentEvent::ToolResult(result) if result.is_error && result.content.contains("refusing dangerous command"))
    }));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn host_allow_is_remembered_for_the_same_tool_and_target() {
    let root =
        std::env::temp_dir().join(format!("a-rust-vm-agent-remember-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let write = |id: &str| {
        ModelResponse::ToolCall(ToolCall::new(
            id,
            "write_file",
            [
                ("path".to_owned(), ToolValue::Text("notes.txt".to_owned())),
                ("content".to_owned(), ToolValue::Text("updated".to_owned())),
            ]
            .into_iter()
            .collect(),
        ))
    };
    let model = ScriptedModel::new([
        write("write-1"),
        write("write-2"),
        ModelResponse::Text("done".to_owned()),
    ]);
    let mut agent = Agent::new(
        model,
        crate::workspace::workspace_tool_registry(&root).unwrap(),
    )
    .with_max_steps(6);
    let mut prompts = 0;

    let events = agent
        .run_with_approval("write twice", |_| {
            prompts += 1;
            PermissionDecision::Allow
        })
        .unwrap();

    assert_eq!(prompts, 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::PermissionRequested(_)))
            .count(),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn host_deny_is_never_remembered() {
    let root = std::env::temp_dir().join(format!(
        "a-rust-vm-agent-no-remember-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let write = |id: &str, content: &str| {
        ModelResponse::ToolCall(ToolCall::new(
            id,
            "write_file",
            [
                ("path".to_owned(), ToolValue::Text("notes.txt".to_owned())),
                ("content".to_owned(), ToolValue::Text(content.to_owned())),
            ]
            .into_iter()
            .collect(),
        ))
    };
    let model = ScriptedModel::new([
        write("write-1", "first"),
        write("write-2", "second"),
        ModelResponse::Text("done".to_owned()),
    ]);
    let mut agent = Agent::new(
        model,
        crate::workspace::workspace_tool_registry(&root).unwrap(),
    )
    .with_max_steps(6);
    let mut prompts = 0;

    agent
        .run_with_approval("write twice", |_| {
            prompts += 1;
            PermissionDecision::Deny {
                reason: "test denial".to_owned(),
            }
        })
        .unwrap();

    assert_eq!(prompts, 2);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tool_output_secrets_are_redacted_before_events_and_context() {
    let root = std::env::temp_dir().join(format!("a-rvm-redact-turn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join(".env"),
        "DB_PASSWORD=hunter2hunter\nKEY=sk-or-v1-0123456789abcdef0123\n",
    )
    .unwrap();
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new(
            "read-1",
            "read_file",
            [("path".to_owned(), ToolValue::Text(".env".to_owned()))]
                .into_iter()
                .collect(),
        )),
        ModelResponse::Text("done".to_owned()),
    ]);
    let mut agent = Agent::new(
        model,
        crate::workspace::workspace_tool_registry(&root).unwrap(),
    );

    let events = agent.run("read the env file").unwrap();

    let result = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolResult(result) => Some(result.content.clone()),
            _ => None,
        })
        .unwrap();
    assert!(result.contains("DB_PASSWORD=[redacted]"), "{result}");
    assert!(!result.contains("hunter2hunter"));
    assert!(!result.contains("0123456789abcdef0123"));
    let context = format!("{:?}", agent.conversation());
    assert!(!context.contains("hunter2hunter"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn usage_accumulates_and_summarises() {
    let mut total = Usage::default();
    total.accumulate(&Usage {
        input_tokens: 1200,
        output_tokens: 56,
        cached_tokens: 800,
        cost_micro_usd: Some(120),
        latency_ms: 2100,
        ..Default::default()
    });
    total.accumulate(&Usage {
        input_tokens: 1000,
        output_tokens: 4,
        latency_ms: 900,
        ..Default::default()
    });
    assert_eq!(
        total.summary(),
        "2,200 in (800 cached) · 60 out · $0.000120 · 3.0 s"
    );
    assert_eq!(Usage::default().summary(), "0 in · 0 out · 0.0 s");
}

#[test]
fn trimmed_history_keeps_a_stable_start_and_never_opens_with_a_tool_result() {
    let replies = (0..12).map(|turn| ModelResponse::Text(format!("answer {turn}")));
    let mut agent = Agent::new(ScriptedModel::new(replies), vm_tool_registry().unwrap())
        .with_context_limits(8, 100_000);
    let mut starts = Vec::new();
    for turn in 0..12 {
        agent.run(format!("question {turn}")).unwrap();
        let conversation = agent.conversation();
        assert!(conversation.len() <= 8);
        assert_eq!(conversation[0].role, "user");
        starts.push(conversation[0].content.clone());
    }
    // Each trim drops to half the budget, so the first message changes only
    // every few turns instead of every turn once the budget is full.
    let changes = starts.windows(2).filter(|pair| pair[0] != pair[1]).count();
    assert!(
        changes <= 4,
        "first message changed {changes} times: {starts:?}"
    );

    let call = ToolCall::new("c1", "read_file", ToolArguments::new());
    let history = vec![
        ConversationMessage::new("user", "q"),
        ConversationMessage::assistant_tool_call(&call),
        ConversationMessage::tool_result(&ToolResult {
            id: "c1".to_owned(),
            name: "read_file".to_owned(),
            content: "x".repeat(10),
            is_error: false,
        }),
        ConversationMessage::new("assistant", "done"),
        ConversationMessage::new("user", "next"),
        ConversationMessage::new("assistant", "ok"),
    ];
    let kept = crate::agent::turn::newest_within(&history, 4, 100_000);
    assert_eq!(
        kept[0].content, "next",
        "a leading tool result must be dropped"
    );
    assert_eq!(kept.len(), 2);
}

#[cfg(unix)]
#[test]
fn read_only_commands_skip_prompts_unless_a_rule_speaks() {
    let root =
        std::env::temp_dir().join(format!("a-rust-vm-agent-read-only-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.txt"), "hello").unwrap();
    let command = |id: &str, text: &str| {
        ModelResponse::ToolCall(ToolCall::new(
            id,
            "run_command",
            [("command".to_owned(), ToolValue::Text(text.to_owned()))]
                .into_iter()
                .collect(),
        ))
    };
    let run = |rules: &[&str], text: &str| {
        let model = ScriptedModel::new([
            command("run-1", text),
            ModelResponse::Text("done".to_owned()),
        ]);
        let policy = crate::permissions::PermissionPolicy::with_rules(
            rules
                .iter()
                .map(|rule| crate::permissions::parse_permission_rule(rule).unwrap())
                .collect(),
        );
        let mut agent = Agent::new(
            model,
            crate::workspace::workspace_tool_registry(&root).unwrap(),
        )
        .with_permission_policy(policy);
        let mut prompts = 0;
        let events = agent
            .run_with_approval("go", |_| {
                prompts += 1;
                PermissionDecision::Deny {
                    reason: "declined".to_owned(),
                }
            })
            .unwrap();
        let output = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ToolResult(result) => Some(result.content.clone()),
                _ => None,
            })
            .unwrap();
        (prompts, output)
    };

    let (prompts, output) = run(&[], "cat notes.txt");
    assert_eq!(prompts, 0);
    assert!(output.contains("hello"), "{output}");

    // Not read-only: asks as before.
    let (prompts, output) = run(&[], "touch made.txt");
    assert_eq!((prompts, output.as_str()), (1, "declined"));
    assert!(!root.join("made.txt").exists());

    // An explicit ask rule restores the prompt; a deny rule refuses.
    let (prompts, _) = run(&["ask run_command"], "cat notes.txt");
    assert_eq!(prompts, 1);
    let (prompts, output) = run(&["deny run_command:cat"], "cat notes.txt");
    assert_eq!(prompts, 0);
    assert!(output.contains("permission rule denies"), "{output}");
    std::fs::remove_dir_all(root).unwrap();
}
