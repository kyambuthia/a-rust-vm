use super::*;

#[test]
fn subagent_delegates_a_bounded_tool_turn() {
    let model = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new(
            "run-1",
            "run_program",
            program_arguments("PUSH 2\nPUSH 3\nADD\nHALT"),
        )),
        ModelResponse::Text("The VM result is 5.".to_owned()),
    ]);
    let result = run_subagent(
        model,
        vm_tool_registry().unwrap(),
        "add two and three",
        SubagentConfig::default(),
    )
    .unwrap();

    assert_eq!(result.tool_calls, 1);
    assert_eq!(result.assistant_text, "The VM result is 5.");
    assert!(result.events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolResult(result) if result.content.contains("result=5")
    )));
}

#[test]
fn subagent_rejects_empty_prompts_and_enforces_tool_limits() {
    let empty = run_subagent(
        ScriptedModel::default(),
        vm_tool_registry().unwrap(),
        "   ",
        SubagentConfig::default(),
    );
    assert_eq!(empty.unwrap_err(), super::SubagentError::EmptyPrompt);

    let limited = run_subagent(
        ScriptedModel::new([
            ModelResponse::ToolCall(ToolCall::new("probe-1", "inspect_vm", ToolArguments::new())),
            ModelResponse::ToolCall(ToolCall::new(
                "probe-2",
                "inspect_vm",
                [("tag".to_owned(), ToolValue::Text("b".to_owned()))]
                    .into_iter()
                    .collect(),
            )),
            ModelResponse::Text("unreachable".to_owned()),
        ]),
        vm_tool_registry().unwrap(),
        "probe the VM",
        SubagentConfig {
            max_tool_calls: 1,
            ..SubagentConfig::default()
        },
    );
    assert_eq!(
        limited.unwrap_err(),
        super::SubagentError::Agent(super::AgentError::ToolCallLimitExceeded { limit: 1 })
    );
}

#[test]
fn subagent_uses_an_isolated_session_and_prompt() {
    let requests = Rc::new(RefCell::new(Vec::new()));
    let model = RecordingModel {
        requests: requests.clone(),
        responses: [ModelResponse::Text("child reply".to_owned())]
            .into_iter()
            .collect(),
    };
    let result = run_subagent(
        model,
        vm_tool_registry().unwrap(),
        "child task",
        SubagentConfig {
            system_prompt: Some("child contract".to_owned()),
            max_transcript_chars: 5,
            ..SubagentConfig::default()
        },
    )
    .unwrap();

    assert_eq!(result.assistant_text, "child");
    let requests = requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].prompt, "child task");
    assert_eq!(requests[0].system_prompt, "child contract");
    assert!(requests[0].conversation.is_empty());
}

#[test]
fn delegation_tool_runs_an_isolated_child_turn() {
    let delegate = SubagentTool::new(
        || ScriptedModel::new([ModelResponse::Text("child answer".to_owned())]),
        || vm_tool_registry().unwrap(),
        SubagentConfig::default(),
    );
    let mut registry = ToolRegistry::default();
    registry.register(delegate).unwrap();
    let parent = ScriptedModel::new([
        ModelResponse::ToolCall(ToolCall::new(
            "delegate-1",
            "delegate_subagent",
            [("task".to_owned(), ToolValue::Text("child task".to_owned()))]
                .into_iter()
                .collect(),
        )),
        ModelResponse::Text("parent done".to_owned()),
    ]);
    let mut agent = Agent::new(parent, registry);

    let events = agent.run("parent task").unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolResult(result)
            if result.content.contains("child answer") && result.content.contains("tool_calls=0")
    )));
}

#[test]
fn delegation_tool_fails_closed_on_empty_tasks() {
    let delegate = SubagentTool::new(
        ScriptedModel::default,
        || vm_tool_registry().unwrap(),
        SubagentConfig::default(),
    );
    let mut registry = ToolRegistry::default();
    registry.register(delegate).unwrap();
    let call = ToolCall::new(
        "delegate-1",
        "delegate_subagent",
        [("task".to_owned(), ToolValue::Text("   ".to_owned()))]
            .into_iter()
            .collect(),
    );

    let result = registry.execute(&call);
    assert!(result.is_error);
    assert_eq!(result.content, "argument 'task' must be non-empty text");
}

#[test]
fn delegation_tool_propagates_child_tool_limits() {
    let delegate = SubagentTool::new(
        || {
            ScriptedModel::new([
                ModelResponse::ToolCall(ToolCall::new(
                    "probe-1",
                    "inspect_vm",
                    ToolArguments::new(),
                )),
                ModelResponse::ToolCall(ToolCall::new(
                    "probe-2",
                    "inspect_vm",
                    ToolArguments::new(),
                )),
            ])
        },
        || vm_tool_registry().unwrap(),
        SubagentConfig {
            max_tool_calls: 1,
            ..SubagentConfig::default()
        },
    );
    let mut registry = ToolRegistry::default();
    registry.register(delegate).unwrap();
    let call = ToolCall::new(
        "delegate-1",
        "delegate_subagent",
        [("task".to_owned(), ToolValue::Text("probe".to_owned()))]
            .into_iter()
            .collect(),
    );

    let result = registry.execute(&call);
    assert!(result.is_error);
    assert_eq!(
        result.content,
        "subagent turn failed: agent tool call limit exceeded: 1"
    );
}

#[test]
fn delegation_tool_blocks_nested_delegation_at_default_depth() {
    let outer = SubagentTool::new(
        EchoAfterOneToolCall::default,
        nested_child_tools,
        SubagentConfig::default(),
    );
    let parent = EchoAfterOneToolCall::new(parent_call());
    let mut tools = ToolRegistry::default();
    tools.register(outer).unwrap();
    let mut agent = Agent::new(parent, tools);

    let events = agent.run("parent task").unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolResult(result)
            if result.content.contains("subagent delegation depth exceeded: 1")
    )));
    assert_eq!(super::subagent_depth(), 0);
}

#[test]
fn delegation_tool_allows_one_nested_level_when_configured() {
    let delegate = SubagentTool::new(
        || EchoAfterOneToolCall::new(nested_call()),
        || {
            let mut tools = ToolRegistry::default();
            tools
                .register(SubagentTool::new(
                    || ScriptedModel::new([ModelResponse::Text("leaf".to_owned())]),
                    || vm_tool_registry().unwrap(),
                    SubagentConfig {
                        max_depth: 2,
                        ..SubagentConfig::default()
                    },
                ))
                .unwrap();
            tools
        },
        SubagentConfig {
            max_depth: 2,
            ..SubagentConfig::default()
        },
    );
    let parent = EchoAfterOneToolCall::new(parent_call());
    let mut tools = ToolRegistry::default();
    tools.register(delegate).unwrap();
    let mut agent = Agent::new(parent, tools);

    let events = agent.run("parent task").unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolResult(result) if result.content.contains("leaf")
    )));
    assert_eq!(super::subagent_depth(), 0);
}

#[test]
fn delegation_depth_resets_between_sequential_calls() {
    let mut registry = ToolRegistry::default();
    registry
        .register(SubagentTool::new(
            ScriptedModel::default,
            || vm_tool_registry().unwrap(),
            SubagentConfig::default(),
        ))
        .unwrap();
    let call = || {
        ToolCall::new(
            "delegate-1",
            "delegate_subagent",
            [("task".to_owned(), ToolValue::Text("   ".to_owned()))]
                .into_iter()
                .collect(),
        )
    };

    for _ in 0..2 {
        let result = registry.execute(&call());
        assert!(result.is_error);
        assert_eq!(result.content, "argument 'task' must be non-empty text");
    }
    assert_eq!(super::subagent_depth(), 0);
}

#[test]
fn subagent_with_cancel_fails_closed_before_consulting_the_model() {
    use super::run_subagent_with_cancel;
    struct CallCountingModel {
        calls: Rc<std::cell::Cell<usize>>,
    }

    impl Model for CallCountingModel {
        fn respond(&mut self, _request: &ModelRequest) -> Result<ModelResponse, ModelError> {
            self.calls.set(self.calls.get() + 1);
            Ok(ModelResponse::Text("should not run".to_owned()))
        }
    }

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let calls = Rc::new(std::cell::Cell::new(0));
    let result = run_subagent_with_cancel(
        CallCountingModel {
            calls: calls.clone(),
        },
        vm_tool_registry().unwrap(),
        "child task",
        SubagentConfig::default(),
        cancel,
    )
    .unwrap();

    assert_eq!(calls.get(), 0);
    assert!(result.assistant_text.is_empty());
    assert!(
        result
            .events
            .iter()
            .any(|event| matches!(event, AgentEvent::Cancelled))
    );
}

#[test]
fn delegation_tool_shares_registry_cancel_tokens() {
    struct ProbeTool {
        seen: Rc<RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>>,
    }

    impl super::Tool for ProbeTool {
        fn spec(&self) -> super::ToolSpec {
            super::ToolSpec::new("probe", "probe", r#"{"type":"object"}"#)
        }

        fn execute(&mut self, _arguments: &ToolArguments) -> Result<String, super::ToolError> {
            Ok("ok".to_owned())
        }

        fn set_cancel_token(&mut self, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) {
            self.seen.borrow_mut().replace(cancel);
        }
    }

    let seen = Rc::new(RefCell::new(None));
    let mut registry = ToolRegistry::default();
    registry.register(ProbeTool { seen: seen.clone() }).unwrap();
    let token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    registry.set_cancel_token(token.clone());
    assert!(std::sync::Arc::ptr_eq(
        seen.borrow().as_ref().unwrap(),
        &token
    ));
}

#[test]
fn delegation_tool_propagates_parent_cancellation_to_child() {
    struct NeverModel;

    impl Model for NeverModel {
        fn respond(&mut self, _request: &ModelRequest) -> Result<ModelResponse, ModelError> {
            panic!("cancelled child must not consult the model");
        }
    }

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut delegate = SubagentTool::new(
        || NeverModel,
        || vm_tool_registry().unwrap(),
        SubagentConfig::default(),
    )
    .with_cancel_token(cancel.clone());
    assert!(std::sync::Arc::ptr_eq(&delegate.cancel_token(), &cancel));
    let call = ToolCall::new(
        "delegate-1",
        "delegate_subagent",
        [("task".to_owned(), ToolValue::Text("child task".to_owned()))]
            .into_iter()
            .collect(),
    );

    let result = delegate.execute(&call.arguments);
    let content = result.unwrap();
    assert!(content.contains("cancelled"));
    assert_eq!(super::subagent_depth(), 0);
}

#[test]
fn agent_turn_shares_parent_cancel_token_with_delegate_tool() {
    struct CancelOnDispatch {
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        call: ToolCall,
    }

    impl Model for CancelOnDispatch {
        fn respond(&mut self, _request: &ModelRequest) -> Result<ModelResponse, ModelError> {
            self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ModelResponse::ToolCall(self.call.clone()))
        }
    }

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let delegate = SubagentTool::new(
        || NeverRespondModel,
        || vm_tool_registry().unwrap(),
        SubagentConfig::default(),
    );
    let mut registry = ToolRegistry::default();
    registry.register(delegate).unwrap();
    struct NeverRespondModel;

    impl Model for NeverRespondModel {
        fn respond(&mut self, _request: &ModelRequest) -> Result<ModelResponse, ModelError> {
            panic!("cancelled child must not consult the model");
        }
    }
    let parent = CancelOnDispatch {
        cancel: cancel.clone(),
        call: ToolCall::new(
            "delegate-1",
            "delegate_subagent",
            [("task".to_owned(), ToolValue::Text("child task".to_owned()))]
                .into_iter()
                .collect(),
        ),
    };
    let mut agent = Agent::new(parent, registry).with_cancel_token(cancel);

    let events = agent.run("parent task").unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolResult(result) if result.content.contains("cancelled")
    )));
}

#[test]
fn persistent_session_rejects_blank_ids_and_prompts() {
    use super::SubagentSession;
    assert_eq!(
        SubagentSession::new("   ", SubagentConfig::default()).unwrap_err(),
        super::SubagentError::InvalidId
    );
    let mut session = SubagentSession::new("child-1", SubagentConfig::default()).unwrap();
    assert_eq!(session.id(), "child-1");
    assert!(session.history().is_empty());
    let rejected = session.send(ScriptedModel::default(), vm_tool_registry().unwrap(), "   ");
    assert_eq!(rejected.unwrap_err(), super::SubagentError::EmptyPrompt);
    assert!(session.history().is_empty());
}

#[test]
fn turns_that_end_without_an_answer_keep_their_context() {
    let inspect =
        || ModelResponse::ToolCall(ToolCall::new("call-1", "inspect_vm", ToolArguments::new()));

    // Step limit: the prompt, the tool exchange, and the reason survive.
    let mut agent =
        Agent::new(ScriptedModel::new([inspect()]), vm_tool_registry().unwrap()).with_max_steps(1);
    assert!(matches!(
        agent.run("look at the vm"),
        Err(super::AgentError::StepLimitExceeded { limit: 1 })
    ));
    let contents = agent
        .conversation()
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(contents.first(), Some(&"look at the vm"));
    assert!(
        agent
            .conversation()
            .iter()
            .any(|message| message.tool_call_id.as_deref() == Some("call-1"))
    );
    assert!(
        contents
            .last()
            .is_some_and(|last| last.contains("step limit exceeded"))
    );

    // A repeated-call stop is recorded the same way.
    let mut agent = Agent::new(
        ScriptedModel::new([inspect(), inspect()]),
        vm_tool_registry().unwrap(),
    );
    agent.run("again").unwrap();
    assert!(agent.conversation().last().is_some_and(|message| {
        message.role == "assistant" && message.content.contains("repeated identical tool call")
    }));
}

#[test]
fn persistent_session_accumulates_history_across_turns() {
    use super::SubagentSession;
    let requests = Rc::new(RefCell::new(Vec::new()));
    let mut session = SubagentSession::new("child-1", SubagentConfig::default()).unwrap();
    let first = session
        .send(
            RecordingModel {
                requests: requests.clone(),
                responses: [ModelResponse::Text("first reply".to_owned())]
                    .into_iter()
                    .collect(),
            },
            vm_tool_registry().unwrap(),
            "first task",
        )
        .unwrap();
    assert_eq!(first.assistant_text, "first reply");
    assert_eq!(session.history().len(), 2);
    let second = session
        .send(
            RecordingModel {
                requests: requests.clone(),
                responses: [ModelResponse::Text("second reply".to_owned())]
                    .into_iter()
                    .collect(),
            },
            vm_tool_registry().unwrap(),
            "second task",
        )
        .unwrap();
    assert_eq!(second.assistant_text, "second reply");
    assert!(session.history().len() > 2);
    let requests = requests.borrow();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .conversation
            .iter()
            .any(|message| { message.content == "first task" || message.content == "first reply" })
    );
}

#[test]
fn persistent_session_truncates_long_transcripts() {
    use super::SubagentSession;
    let mut session = SubagentSession::new(
        "child-1",
        SubagentConfig {
            max_transcript_chars: 5,
            ..SubagentConfig::default()
        },
    )
    .unwrap();
    let result = session
        .send(
            ScriptedModel::new([ModelResponse::Text("long reply".to_owned())]),
            vm_tool_registry().unwrap(),
            "task",
        )
        .unwrap();
    assert_eq!(result.assistant_text, "long ");
}

#[test]
fn persistent_session_propagates_cancellation() {
    use super::SubagentSession;
    struct CallCountingModel {
        calls: Rc<std::cell::Cell<usize>>,
    }

    impl Model for CallCountingModel {
        fn respond(&mut self, _request: &ModelRequest) -> Result<ModelResponse, ModelError> {
            self.calls.set(self.calls.get() + 1);
            Ok(ModelResponse::Text("should not run".to_owned()))
        }
    }

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let calls = Rc::new(std::cell::Cell::new(0));
    let mut session = SubagentSession::new("child-1", SubagentConfig::default())
        .unwrap()
        .with_cancel_token(cancel);
    let result = session
        .send(
            CallCountingModel {
                calls: calls.clone(),
            },
            vm_tool_registry().unwrap(),
            "task",
        )
        .unwrap();

    assert_eq!(calls.get(), 0);
    assert!(result.assistant_text.is_empty());
    assert!(
        result
            .events
            .iter()
            .any(|event| matches!(event, AgentEvent::Cancelled))
    );
    assert!(
        result
            .events
            .iter()
            .any(|event| matches!(event, AgentEvent::Done))
    );
    assert!(session.history().is_empty());
}

#[test]
fn persistent_session_shares_parent_cancel_token() {
    use super::{Agent, SubagentSession};
    let parent = Agent::new(
        ScriptedModel::new([ModelResponse::Text("parent".to_owned())]),
        vm_tool_registry().unwrap(),
    );
    let parent_token = parent.cancel_token();
    let mut session = SubagentSession::new("child-1", SubagentConfig::default()).unwrap();
    session = session.with_cancel_token(parent_token.clone());
    assert!(std::sync::Arc::ptr_eq(
        &session.cancel_token(),
        &parent_token
    ));

    parent_token.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = session
        .send(
            ScriptedModel::new([ModelResponse::Text("child".to_owned())]),
            vm_tool_registry().unwrap(),
            "task",
        )
        .unwrap();
    assert!(
        result
            .events
            .iter()
            .any(|event| matches!(event, AgentEvent::Cancelled))
    );
}

#[test]
fn persistent_session_cancel_stops_next_send() {
    use super::SubagentSession;
    let mut session = SubagentSession::new("child-1", SubagentConfig::default()).unwrap();
    session.cancel();
    let result = session
        .send(
            ScriptedModel::new([ModelResponse::Text("child".to_owned())]),
            vm_tool_registry().unwrap(),
            "task",
        )
        .unwrap();
    assert!(
        result
            .events
            .iter()
            .any(|event| matches!(event, AgentEvent::Cancelled))
    );
    assert!(session.history().is_empty());
}

#[test]
fn persistent_session_transcript_round_trips_through_store() {
    use super::{SubagentConfig, SubagentSession};
    use crate::session::SessionStore;
    let directory =
        std::env::temp_dir().join(format!("a-rvm-subagent-{}-round-trip", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let store = SessionStore::new(&directory);
    let mut session = SubagentSession::new(
        "child-1",
        SubagentConfig {
            max_transcript_chars: 16,
            ..SubagentConfig::default()
        },
    )
    .unwrap();
    session
        .send(
            ScriptedModel::new([ModelResponse::Text("first reply".to_owned())]),
            vm_tool_registry().unwrap(),
            "first task",
        )
        .unwrap();
    session.save(&store).unwrap();

    let reloaded = SubagentSession::load(&store, "child-1").unwrap();
    assert_eq!(reloaded, session);
    assert_eq!(reloaded.history(), session.history());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn persistent_session_continues_history_after_reload() {
    use super::SubagentSession;
    use crate::session::SessionStore;
    let directory =
        std::env::temp_dir().join(format!("a-rvm-subagent-{}-continue", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let store = SessionStore::new(&directory);
    let mut session = SubagentSession::new("child-1", SubagentConfig::default()).unwrap();
    session
        .send(
            ScriptedModel::new([ModelResponse::Text("first reply".to_owned())]),
            vm_tool_registry().unwrap(),
            "first task",
        )
        .unwrap();
    session.save(&store).unwrap();

    let mut reloaded = SubagentSession::load(&store, "child-1").unwrap();
    let second = reloaded
        .send(
            ScriptedModel::new([ModelResponse::Text("second reply".to_owned())]),
            vm_tool_registry().unwrap(),
            "second task",
        )
        .unwrap();
    assert_eq!(second.assistant_text, "second reply");
    assert!(reloaded.history().len() > session.history().len());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn persistent_session_load_rejects_unknown_and_blank_ids() {
    use super::SubagentSession;
    use crate::session::SessionStore;
    let directory =
        std::env::temp_dir().join(format!("a-rvm-subagent-{}-reject", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let store = SessionStore::new(&directory);
    assert!(SubagentSession::load(&store, "   ").is_err());
    assert!(SubagentSession::load(&store, "missing-child").is_err());
    let session = SubagentSession::new("with space", SubagentConfig::default()).unwrap();
    assert!(session.save(&store).is_err());
}
