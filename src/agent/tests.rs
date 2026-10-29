use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use rig_core::completion::message::ReasoningContent;
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamFinal, StreamingCompletionResponse,
};
use serde_json::json;

use super::models::context_window;
use super::*;
use crate::config::Settings;
use rig_core::completion::CompletionError;

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
    let mut settings = Settings::default();
    settings.agent.max_tokens = 2048;
    settings.model.effort = Some(Effort::High);
    run_inner(model.clone(), &mut history, settings.turn_config(0), &tx)
        .await
        .unwrap();

    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert_eq!(request.max_tokens, Some(2048));
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
async fn manual_and_automatic_compaction_use_configured_limits_and_thinking() {
    for thinking in [Thinking::Adaptive, Thinking::Off] {
        let model = Model::new(vec![
            vec![RawStreamingChoice::Message("Summary".into())],
            vec![RawStreamingChoice::Message("Summary".into())],
            vec![RawStreamingChoice::Message("Done".into())],
        ]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut history = vec![Message::user("Task"), Message::assistant("Progress")];
        let mut settings = Settings::default();
        settings.model.thinking = thinking;
        settings.model.context_window = Some(1000);
        settings.agent.max_tokens = 2048;
        settings.agent.compact_max_tokens = 1024;
        settings.agent.auto_compact_percent = 25;
        compact(
            model.clone(),
            history.clone(),
            None,
            settings.turn_config(0),
            tx.clone(),
        )
        .await;
        assert!(
            std::iter::from_fn(|| rx.try_recv().ok())
                .any(|e| matches!(e, AgentEvent::Compacted { .. }))
        );
        // A custom threshold forces compaction well below the default 80%.
        run_inner(model.clone(), &mut history, settings.turn_config(250), &tx)
            .await
            .unwrap();
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].max_tokens, Some(1024));
        assert_eq!(requests[1].max_tokens, Some(1024));
        assert_eq!(requests[2].max_tokens, Some(2048));
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

#[test]
fn compaction_threshold_handles_boundaries_and_large_windows() {
    let mut settings = Settings::default();
    settings.model.context_window = Some(1000);
    settings.agent.auto_compact_percent = 25;
    assert!(!settings.turn_config(249).should_compact());
    assert!(settings.turn_config(250).should_compact());
    settings.model.context_window = Some(u64::MAX);
    settings.agent.auto_compact_percent = 100;
    assert!(!settings.turn_config(u64::MAX - 1).should_compact());
    assert!(settings.turn_config(u64::MAX).should_compact());
}

#[derive(Clone, Default)]
struct FailingModel(Arc<std::sync::atomic::AtomicU32>);

impl CompletionModel for FailingModel {
    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        panic!("the harness should stream requests")
    }

    async fn stream(
        &self,
        _: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Err(CompletionError::ProviderError("connection reset".into()))
    }
}

#[tokio::test]
async fn configured_retry_limit_controls_actual_attempts() {
    for retries in [0, 1] {
        let model = FailingModel::default();
        let mut settings = Settings::default();
        settings.agent.max_retries = retries;
        let (tx, mut rx) = mpsc::unbounded_channel();
        run(
            model.clone(),
            vec![Message::user("Task")],
            settings.turn_config(0),
            tx,
        )
        .await;
        assert_eq!(
            model.0.load(std::sync::atomic::Ordering::Relaxed),
            retries + 1
        );
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::Retry { .. }))
                .count(),
            retries as usize
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Error(_))));
        assert!(matches!(events.last(), Some(AgentEvent::Done)));
    }
}

#[test]
fn context_windows_match_models_api() {
    for model in [
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
        "claude-fable-5-1",
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "anthropic/claude-opus-5-5",
    ] {
        assert_eq!(context_window(model), 1_000_000, "{model}");
    }
    for model in [
        "claude-opus-4-5-20251101",
        "claude-sonnet-4-5-20250929",
        "claude-haiku-4-5-20251001",
        "some-other-model",
    ] {
        assert_eq!(context_window(model), 200_000, "{model}");
    }
}
