use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use rig_core::completion::message::ReasoningContent;
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamFinal, StreamingCompletionResponse,
    ToolCallDeltaContent, ToolInputEnd, UnparseableToolInput,
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
    omit_final: bool,
}

impl CompletionModel for Model {
    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        panic!("tamale should stream requests")
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        self.requests.lock().unwrap().push(request);
        let mut response = self.responses.lock().unwrap().pop_front().unwrap();
        if !self.omit_final
            && !response
                .iter()
                .any(|c| matches!(c, RawStreamingChoice::FinalResponse(_)))
        {
            response.push(final_response(FinishReason::Stop));
        }
        Ok(StreamingCompletionResponse::stream(
            "anthropic",
            Box::pin(futures::stream::iter(response.into_iter().map(Ok))),
        ))
    }
}

fn final_response(reason: FinishReason) -> RawStreamingChoice {
    RawStreamingChoice::FinalResponse(
        StreamFinal::new(
            "anthropic",
            Usage {
                output_tokens: 10,
                ..Usage::default()
            },
        )
        .with_finish_reason(reason),
    )
}

#[tokio::test]
async fn token_limits_continue_text_thinking_and_empty_responses() {
    let signed = ReasoningContent::Text {
        text: String::new(),
        signature: Some("signed-state".into()),
    };
    let model = Model::new(vec![
        vec![
            RawStreamingChoice::Message("Part one. ".into()),
            final_response(FinishReason::Length),
        ],
        vec![
            RawStreamingChoice::Reasoning {
                id: "thinking-1".into(),
                provider_id: None,
                content: signed.clone(),
            },
            final_response(FinishReason::Length),
        ],
        vec![final_response(FinishReason::Length)],
        vec![RawStreamingChoice::Message("Finished.".into())],
    ]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut history = vec![Message::user("Complete the long task")];
    run_inner(
        model.clone(),
        &mut history,
        Settings::default().turn_config(0),
        &tx,
    )
    .await
    .unwrap();

    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().all(|r| r.max_tokens == Some(16000)));
    assert!(history.iter().any(|message| matches!(message, Message::Assistant { content, .. }
        if content.iter().any(|c| matches!(c, AssistantContent::Reasoning(r) if r.content == vec![signed.clone()])))));
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Continuing(_)))
            .count(),
        3
    );
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Part one. Finished.");
    let output: u64 = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Usage(u) => Some(u.output_tokens),
            _ => None,
        })
        .sum();
    assert_eq!(output, 40);
}

#[tokio::test]
async fn truncated_tools_are_not_executed_or_replayed() {
    // Even a complete call from a cut-off message must be reissued. Include
    // an incomplete JSON call to exercise Rig's in-band parsing error too.
    let model = Model::new(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "tool-1",
                "unknown_test_tool".into(),
                json!({}),
            )),
            RawStreamingChoice::ToolCallDelta {
                id: "tool-2".into(),
                content: ToolCallDeltaContent::Name("write_file".into()),
            },
            RawStreamingChoice::ToolCallDelta {
                id: "tool-2".into(),
                content: ToolCallDeltaContent::Delta("{\"path\":\"unfinished".into()),
            },
            RawStreamingChoice::ToolInputEnd(ToolInputEnd::new(
                "tool-2",
                UnparseableToolInput::Error,
            )),
            RawStreamingChoice::Reasoning {
                id: "thinking-1".into(),
                provider_id: None,
                content: ReasoningContent::Text {
                    text: "unfinished thinking".into(),
                    signature: None,
                },
            },
            final_response(FinishReason::Length),
        ],
        vec![RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            "tool-3",
            "unknown_test_tool".into(),
            json!({}),
        ))],
        vec![RawStreamingChoice::Message("Finished.".into())],
    ]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut history = vec![Message::user("Do the task")];
    run_inner(
        model.clone(),
        &mut history,
        Settings::default().turn_config(0),
        &tx,
    )
    .await
    .unwrap();

    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCall { .. }))
            .count(),
        1
    );
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        !requests[1]
            .chat_history
            .iter()
            .any(|m| matches!(m, Message::Assistant { .. }))
    );
    assert!(!history.iter().any(|m| matches!(m, Message::Assistant { content, .. }
        if content.iter().any(|c| matches!(c, AssistantContent::ToolCall(tc) if tc.id == "tool-1" || tc.id == "tool-2")))));
}

#[tokio::test]
async fn empty_truncation_after_tools_keeps_tool_results_first() {
    let model = Model::new(vec![
        vec![RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            "tool-1",
            "unknown_test_tool".into(),
            json!({}),
        ))],
        vec![final_response(FinishReason::Length)],
        vec![RawStreamingChoice::Message("Finished".into())],
    ]);
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut history = vec![Message::user("Task")];
    run_inner(
        model.clone(),
        &mut history,
        Settings::default().turn_config(0),
        &tx,
    )
    .await
    .unwrap();
    let requests = model.requests.lock().unwrap();
    let Some(Message::User { content }) = requests[2].chat_history.last() else {
        panic!("missing tool results");
    };
    assert!(matches!(content.first(), Some(UserContent::ToolResult(_))));
    assert!(matches!(content.last(), Some(UserContent::Text(_))));
}

#[tokio::test]
async fn compaction_continues_truncated_summary_and_counts_all_usage() {
    let model = Model::new(vec![
        vec![
            RawStreamingChoice::Message("Task: implement. ".into()),
            final_response(FinishReason::Length),
        ],
        vec![RawStreamingChoice::Message("Next: verify.".into())],
    ]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let history = compact_history(
        &model,
        "system",
        &[],
        &[Message::user("Task")],
        None,
        Settings::default().turn_config(0),
        &tx,
    )
    .await
    .unwrap();
    assert!(
        serde_json::to_string(&history)
            .unwrap()
            .contains("Task: implement. Next: verify.")
    );
    assert!(
        model
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.max_tokens == Some(8000))
    );
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { usage, .. } if usage.output_tokens == 20))
    );
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::TextDelta(_))));
}

#[tokio::test]
async fn context_limit_compacts_and_resumes_the_task() {
    let model = Model::new(vec![
        vec![
            RawStreamingChoice::Message("Progress. ".into()),
            final_response(FinishReason::Other("model_context_window_exceeded".into())),
        ],
        vec![RawStreamingChoice::Message(
            "Summary with remaining work".into(),
        )],
        vec![RawStreamingChoice::Message("Finished.".into())],
    ]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut history = vec![Message::user("Task")];
    run_inner(
        model.clone(),
        &mut history,
        Settings::default().turn_config(0),
        &tx,
    )
    .await
    .unwrap();
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].max_tokens, Some(8000));
    assert!(
        serde_json::to_string(&requests[2].chat_history)
            .unwrap()
            .contains("Summary with remaining work")
    );
    assert!(
        std::iter::from_fn(|| rx.try_recv().ok())
            .any(|e| matches!(e, AgentEvent::Compacted { .. }))
    );
}

#[tokio::test]
async fn empty_end_turn_recovers_once_then_reports_failure() {
    for recover in [true, false] {
        let second = if recover {
            vec![RawStreamingChoice::Message("Finished.".into())]
        } else {
            vec![]
        };
        let model = Model::new(vec![vec![], second]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        run(
            model.clone(),
            vec![Message::user("Task")],
            Settings::default().turn_config(0),
            tx,
        )
        .await;
        assert_eq!(model.requests.lock().unwrap().len(), 2);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(
            events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
            !recover
        );
        assert!(matches!(events.last(), Some(AgentEvent::Done)));
    }
}

#[tokio::test]
async fn pause_turn_replays_the_assistant_without_adding_user_input() {
    let model = Model::new(vec![
        vec![
            RawStreamingChoice::Message("Progress".into()),
            final_response(FinishReason::Other("pause_turn".into())),
        ],
        vec![RawStreamingChoice::Message("Finished".into())],
    ]);
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut history = vec![Message::user("Task")];
    run_inner(
        model.clone(),
        &mut history,
        Settings::default().turn_config(0),
        &tx,
    )
    .await
    .unwrap();
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(matches!(
        requests[1].chat_history.last(),
        Some(Message::Assistant { .. })
    ));
}

#[tokio::test]
async fn refusal_and_unknown_stop_reasons_report_errors_without_running_tools() {
    for reason in [
        FinishReason::ContentFilter,
        FinishReason::Other("unexpected_reason".into()),
    ] {
        let model = Model::new(vec![vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "tool-1",
                "unknown_test_tool".into(),
                json!({}),
            )),
            final_response(reason),
        ]]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        run(
            model.clone(),
            vec![Message::user("Task")],
            Settings::default().turn_config(0),
            tx,
        )
        .await;
        assert_eq!(model.requests.lock().unwrap().len(), 1);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Error(_))));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCall { .. } | AgentEvent::Retry { .. }))
        );
        assert!(matches!(events.last(), Some(AgentEvent::Done)));
    }
}

#[tokio::test]
async fn incomplete_streams_retry_instead_of_looking_finished() {
    let mut model = Model::new(vec![vec![RawStreamingChoice::Message("Partial".into())]; 2]);
    model.omit_final = true;
    let mut settings = Settings::default();
    settings.agent.max_retries = 1;
    let (tx, mut rx) = mpsc::unbounded_channel();
    run(
        model.clone(),
        vec![Message::user("Task")],
        settings.turn_config(0),
        tx,
    )
    .await;
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Retry { .. })));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Error(e) if e.contains("final response")))
    );
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::History(_))));
}

#[derive(Clone)]
struct StallingModel {
    calls: Arc<std::sync::atomic::AtomicU32>,
    stall_opening: bool,
}

impl CompletionModel for StallingModel {
    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        unreachable!()
    }

    async fn stream(
        &self,
        _: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        if self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            == 0
        {
            if self.stall_opening {
                return std::future::pending().await;
            }
            return Ok(StreamingCompletionResponse::stream(
                "anthropic",
                Box::pin(
                    futures::stream::iter([Ok(RawStreamingChoice::Message("Partial".into()))])
                        .chain(futures::stream::pending()),
                ),
            ));
        }
        Ok(StreamingCompletionResponse::stream(
            "anthropic",
            Box::pin(futures::stream::iter([
                Ok(RawStreamingChoice::Message("Recovered".into())),
                Ok(final_response(FinishReason::Stop)),
            ])),
        ))
    }
}

#[tokio::test]
async fn stalled_opening_and_stream_retry_and_recover() {
    for stall_opening in [true, false] {
        let model = StallingModel {
            calls: Arc::default(),
            stall_opening,
        };
        let mut settings = Settings::default();
        settings.agent.request_timeout_secs = 1;
        settings.agent.max_retries = 1;
        let (tx, mut rx) = mpsc::unbounded_channel();
        run(
            model.clone(),
            vec![Message::user("Task")],
            settings.turn_config(0),
            tx,
        )
        .await;
        assert_eq!(model.calls.load(std::sync::atomic::Ordering::Relaxed), 2);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Retry { error, .. } if error.contains("timeout")))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Recovered"))
        );
        assert!(!events.iter().any(|e| matches!(e, AgentEvent::Error(_))));
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
        panic!("tamale should stream requests")
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
