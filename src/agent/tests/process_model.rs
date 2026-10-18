use super::*;

#[test]
fn process_model_streams_json_events_without_a_shell_wrapper() {
    let mut model = super::ProcessModel::new("/bin/sh").with_arguments([
        "-c",
        "read request; printf '%s\\n' '{\"type\":\"text_delta\",\"text\":\"hello \"}' '{\"type\":\"text_delta\",\"text\":\"world\"}' '{\"type\":\"done\"}'",
    ]);
    let request = super::ModelRequest {
        system_prompt: String::new(),
        prompt: "say hello".to_owned(),
        tool_results: Vec::new(),
        tools: Vec::new(),
        conversation: Vec::new(),
        current_turn: Vec::new(),
        route: RouteRequest::default(),
    };
    let mut chunks = Vec::new();

    let response = model
        .respond_stream(&request, &mut |event| match event {
            super::ModelStreamEvent::TextDelta { content } => chunks.push(content),
        })
        .unwrap();

    assert_eq!(response, ModelResponse::Text("hello world".to_owned()));
    assert_eq!(chunks, vec!["hello ".to_owned(), "world".to_owned()]);
}

#[cfg(unix)]
#[test]
fn process_model_terminates_a_child_that_exceeds_its_deadline() {
    let mut model = super::ProcessModel::new("/bin/sh")
        .with_arguments(["-c", "read request; sleep 5"])
        .with_timeout(std::time::Duration::from_millis(100));
    let request = super::ModelRequest {
        system_prompt: String::new(),
        prompt: "never finish".to_owned(),
        tool_results: Vec::new(),
        tools: Vec::new(),
        conversation: Vec::new(),
        current_turn: Vec::new(),
        route: RouteRequest::default(),
    };

    let error = model.respond(&request).unwrap_err();

    assert_eq!(error.message, "model process timed out");
}
