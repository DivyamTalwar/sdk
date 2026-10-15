use super::*;
use crate::{
    OperationId, Result,
    conversation::Limits,
    model::{
        Model, ModelContent, ModelDispatch, ModelEvent, ModelMessage, ModelProvider, ModelRequest,
        ModelRole, PreparedModelRequest,
    },
};
use futures::StreamExt as _;
use serde_json::json;

fn request() -> Result<(PreparedModelRequest, ModelDispatch)> {
    let request = PreparedModelRequest::prepare(
        ModelRequest {
            model: Model::new("scripted".into(), "test".into(), "1".into(), json!({}))?,
            messages: vec![
                ModelMessage {
                    role: ModelRole::System,
                    content: ModelContent::Text("instructions".into()),
                },
                ModelMessage {
                    role: ModelRole::User,
                    content: ModelContent::Text("prompt".into()),
                },
            ],
            tools: vec![crate::tool::ToolDefinition {
                name: "read".into(),
                revision: "1".into(),
                description: "read a file".into(),
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
                projection_schema: crate::tool::json_projection_schema(json!({"type":"object"})),
            }],
            max_output_tokens: Some(64),
        },
        Limits::default(),
        None,
    )?;
    let dispatch = ModelDispatch {
        operation_id: OperationId::new(),
        step: 0,
        request_digest: request.request_digest(),
    };
    Ok((request, dispatch))
}

#[tokio::test]
async fn text_replay_records_exact_request_and_exhausts() -> Result<()> {
    let provider = ScriptedModelProvider::new([ScriptedTurn::Text("answer".into())]);
    let (request, dispatch) = request()?;
    let expected = request.request().clone();
    let count = provider.count_tokens(&request)?;
    assert_eq!(count.request_digest, request.request_digest());
    assert_eq!(count.message_tokens.len(), 2);
    let events = provider
        .generate(request.clone(), dispatch)
        .collect::<Vec<_>>()
        .await;
    assert_eq!(
        events.into_iter().collect::<Result<Vec<_>>>()?,
        vec![
            ModelEvent::Content {
                delta: "answer".into()
            },
            ModelEvent::Completed {
                metadata: serde_json::Value::Null
            },
        ]
    );
    assert_eq!(provider.requests()?, vec![expected]);
    assert_eq!(provider.dispatches()?, vec![dispatch]);
    assert_eq!(provider.remaining()?, 0);
    let events = provider
        .generate(request, dispatch)
        .collect::<Vec<_>>()
        .await;
    assert!(
        matches!(events.as_slice(), [Err(crate::Error::Invalid(message))]
        if message == "scripted model exhausted")
    );
    assert_eq!(provider.requests()?.len(), 2);
    Ok(())
}

#[tokio::test]
async fn exact_events_preserve_tools_errors_and_missing_completion() -> Result<()> {
    let turns: Vec<ScriptedTurn> = serde_json::from_value(json!([
        [{"kind":"reasoning","delta":"thinking"},
         {"kind":"tool_call","call_id":"call","name":"read","arguments":{"path":"a"}},
         {"kind":"completed","metadata":{"tokens":3}}],
        [{"kind":"content","delta":"partial"},{"error":"offline"}],
        []
    ]))
    .map_err(|e| crate::Error::Invalid(e.to_string()))?;
    let provider = ScriptedModelProvider::new(turns);
    let (request, dispatch) = request()?;
    let events = provider
        .generate(request.clone(), dispatch)
        .collect::<Vec<_>>()
        .await;
    let events = events.into_iter().collect::<Result<Vec<_>>>()?;
    assert!(matches!(events.as_slice(), [ModelEvent::Reasoning { .. },
        ModelEvent::ToolCall { arguments, .. }, ModelEvent::Completed { metadata }]
        if arguments == &json!({"path":"a"}) && metadata == &json!({"tokens":3})));
    let events = provider
        .generate(request.clone(), dispatch)
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(events.as_slice(), [Ok(ModelEvent::Content { .. }),
        Err(crate::Error::Storage(message))] if message == "offline"));
    assert!(
        provider
            .generate(request, dispatch)
            .collect::<Vec<_>>()
            .await
            .is_empty()
    );
    assert_eq!(provider.remaining()?, 0);
    Ok(())
}

#[tokio::test]
async fn mismatched_dispatch_does_not_consume_script() -> Result<()> {
    let provider = ScriptedModelProvider::new([ScriptedTurn::Text("answer".into())]);
    let (request, mut dispatch) = request()?;
    dispatch.request_digest = [0; 32];
    let events = provider
        .generate(request, dispatch)
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(events.as_slice(), [Err(crate::Error::Invalid(_))]));
    assert_eq!(provider.remaining()?, 1);
    assert!(provider.requests()?.is_empty());
    Ok(())
}

#[test]
fn malformed_script_events_are_rejected() {
    for event in [
        json!({"kind":"content","delta":"x","typo":true}),
        json!({"error":"x","typo":true}),
        json!({"kind":"typo"}),
    ] {
        assert!(serde_json::from_value::<ScriptedEvent>(event).is_err());
    }
}
