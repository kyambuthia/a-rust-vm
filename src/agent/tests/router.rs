use super::*;

#[test]
fn router_selects_explicit_and_profile_routes() {
    let mut router = ModelRouter::new();
    router
        .register_model(
            "default",
            ModelCapabilities::STREAMING_TOOLS,
            ScriptedModel::default(),
        )
        .unwrap();
    router
        .register_model(
            "fast",
            ModelCapabilities::STREAMING_TOOLS,
            ScriptedModel::default(),
        )
        .unwrap();
    router
        .register_model(
            "strong",
            ModelCapabilities::STREAMING_TOOLS,
            ScriptedModel::default(),
        )
        .unwrap();
    router.set_default("default").unwrap();
    router.set_fast("fast").unwrap();
    router.set_strong("strong").unwrap();

    assert_eq!(
        router.select(&RouteRequest::default()).unwrap().model,
        "default"
    );
    assert_eq!(
        router
            .select(&RouteRequest {
                mode: ModelMode::Fast,
                ..RouteRequest::default()
            })
            .unwrap()
            .model,
        "fast"
    );
    assert_eq!(
        router
            .select(&RouteRequest {
                requested_model: Some("strong".to_owned()),
                ..RouteRequest::default()
            })
            .unwrap()
            .model,
        "strong"
    );
}

#[test]
fn router_rejects_routes_missing_required_capabilities() {
    let mut router = ModelRouter::new();
    router
        .register_model(
            "text-only",
            ModelCapabilities {
                streaming: true,
                tools: false,
            },
            ScriptedModel::default(),
        )
        .unwrap();
    router.set_default("text-only").unwrap();

    let error = router
        .select(&RouteRequest {
            requires_tools: true,
            ..RouteRequest::default()
        })
        .unwrap_err();

    assert_eq!(
        error,
        super::RouteError::MissingCapability {
            model: "text-only".to_owned(),
            capability: "tool calls"
        }
    );
}

#[test]
fn router_falls_back_when_primary_fails_before_output() {
    let mut router = ModelRouter::new();
    router
        .register_model("primary", ModelCapabilities::STREAMING_TOOLS, FailingModel)
        .unwrap();
    router
        .register_model(
            "fallback",
            ModelCapabilities::STREAMING_TOOLS,
            ScriptedModel::new([ModelResponse::Text("fallback response".to_owned())]),
        )
        .unwrap();
    router.set_default("primary").unwrap();
    router.set_fallback("fallback").unwrap();

    let response = router
        .respond(&model_request(RouteRequest::default()))
        .unwrap();

    assert_eq!(
        response,
        ModelResponse::Text("fallback response".to_owned())
    );
}

#[test]
fn router_does_not_fallback_after_streaming_output() {
    let mut router = ModelRouter::new();
    router
        .register_model(
            "primary",
            ModelCapabilities::STREAMING_TOOLS,
            PartialFailureModel,
        )
        .unwrap();
    router
        .register_model(
            "fallback",
            ModelCapabilities::STREAMING_TOOLS,
            ScriptedModel::new([ModelResponse::Text("should not run".to_owned())]),
        )
        .unwrap();
    router.set_default("primary").unwrap();
    router.set_fallback("fallback").unwrap();
    let mut chunks = Vec::new();

    let error = router
        .respond_stream(&model_request(RouteRequest::default()), &mut |event| {
            if let ModelStreamEvent::TextDelta { content } = event {
                chunks.push(content);
            }
        })
        .unwrap_err();

    assert_eq!(chunks, vec!["partial"]);
    assert_eq!(error.message, "stream interrupted");
}
