use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use rig_core::completion::message::ReasoningContent;
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamFinal, StreamingCompletionResponse,
};
use serde_json::json;

use super::*;

#[derive(Clone, Default)]
struct Model {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    responses: Arc<Mutex<VecDeque<Vec<RawStreamingChoice>>>>,
}

impl CompletionModel for Model {
    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        panic!("the harness should stream requests")
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        self.requests.lock().unwrap().push(request);
        let mut response = self.responses.lock().unwrap().pop_front().unwrap();
        response.push(RawStreamingChoice::FinalResponse(StreamFinal::new(
            "anthropic",
            Usage::default(),
        )));
        Ok(StreamingCompletionResponse::stream(
            "anthropic",
            Box::pin(futures::stream::iter(response.into_iter().map(Ok))),
        ))
    }
}

impl Model {
    fn new(responses: Vec<Vec<RawStreamingChoice>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
            ..Self::default()
        }
    }
}

#[test]
fn thinking_defaults_on_and_parses_modes() {
    assert_eq!(Reasoning::default().thinking, Thinking::Adaptive);
    for name in ["adaptive", "on", " ON "] {
        assert_eq!(Thinking::parse(name), Some(Thinking::Adaptive));
    }
    for name in ["off", "disabled", " OFF "] {
        assert_eq!(Thinking::parse(name), Some(Thinking::Off));
    }
    assert_eq!(Thinking::parse("typo"), None);
}

#[tokio::test]
async fn tool_followup_preserves_thinking_and_effort() {
    // Cover normal, signature-only, and redacted thinking. All carry provider
    // state that must survive even though the UI only receives answer text.
    let blocks = [
        ReasoningContent::Text {
            text: "Check the tool result.".into(),
            signature: Some("signed-thinking".into()),
        },
        ReasoningContent::Text {
            text: String::new(),
            signature: Some("signature-only".into()),
        },
        ReasoningContent::Redacted {
            data: "opaque-data".into(),
        },
    ];
    let mut first: Vec<_> = blocks
        .iter()
        .enumerate()
        .map(|(i, content)| RawStreamingChoice::Reasoning {
            id: format!("thinking-{i}").into(),
            provider_id: None,
            content: content.clone(),
        })
        .collect();
    // An unknown tool exercises the result loop without filesystem side effects.
    first.push(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
        "tool-1",
        "unknown_test_tool".into(),
        json!({}),
    )));
    let model = Model::new(vec![
        first,
        vec![RawStreamingChoice::Message("Done".into())],
    ]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut history = vec![Message::user("Do the task")];
    run_inner(
        model.clone(),
        &mut history,
        Config {
            context_window: DEFAULT_CONTEXT_WINDOW,
            context_tokens: 0,
            reasoning: Reasoning {
                effort: Some(Effort::High),
                ..Reasoning::default()
            },
        },
        &tx,
    )
    .await
    .unwrap();

    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert_eq!(
            request.additional_params,
            Some(json!({
                "thinking": { "type": "adaptive" },
                "output_config": { "effort": "high" },
            }))
        );
    }
    let content = requests[1]
        .chat_history
        .iter()
        .find_map(|message| match message {
            Message::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .expect("missing assistant turn before tool results");
    let replayed: Vec<_> = content
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Reasoning(r) => Some(r.content.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(replayed, blocks);
    let mut text = String::new();
    while let Ok(event) = rx.try_recv() {
        if let AgentEvent::TextDelta(delta) = event {
            text.push_str(&delta);
        }
    }
    assert_eq!(text, "Done");
}

#[tokio::test]
async fn compaction_keeps_thinking_setting() {
    for thinking in [Thinking::Adaptive, Thinking::Off] {
        let reasoning = Reasoning {
            thinking,
            effort: None,
        };
        let model = Model::new(vec![
            vec![RawStreamingChoice::Message("Summary".into())],
            vec![RawStreamingChoice::Message("Summary".into())],
            vec![RawStreamingChoice::Message("Done".into())],
        ]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut history = vec![Message::user("Task"), Message::assistant("Progress")];
        compact(model.clone(), history.clone(), None, reasoning, tx.clone()).await;
        assert!(
            std::iter::from_fn(|| rx.try_recv().ok())
                .any(|e| matches!(e, AgentEvent::Compacted { .. }))
        );
        // A full context forces automatic compaction before the next completion.
        run_inner(
            model.clone(),
            &mut history,
            Config {
                context_window: DEFAULT_CONTEXT_WINDOW,
                context_tokens: DEFAULT_CONTEXT_WINDOW,
                reasoning,
            },
            &tx,
        )
        .await
        .unwrap();
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].max_tokens, Some(COMPACT_MAX_TOKENS));
        assert_eq!(requests[1].max_tokens, Some(COMPACT_MAX_TOKENS));
        assert_eq!(requests[2].max_tokens, Some(MAX_TOKENS));
        for request in requests.iter() {
            assert_eq!(
                request.additional_params,
                Some(json!({
                    "thinking": { "type": if thinking == Thinking::Adaptive { "adaptive" } else { "disabled" } },
                }))
            );
        }
    }
}
