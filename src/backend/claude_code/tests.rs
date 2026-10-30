use serde_json::json;

use super::*;
use crate::config::Settings;
use crate::permissions::{Mode, Policy};
use crate::workspace::Workspace;

fn parse_all(lines: &[Value]) -> Vec<Update> {
    let mut parser = Parser::default();
    lines.iter().filter_map(|l| parser.parse(l)).collect()
}

#[test]
fn parses_session_text_usage_and_result() {
    let updates = parse_all(&[
        json!({"type": "system", "subtype": "init", "session_id": "s1", "parent_tool_use_id": null}),
        json!({"type": "stream_event", "event": {"type": "message_start", "message": {"usage": {
            "input_tokens": 2, "cache_creation_input_tokens": 300, "cache_read_input_tokens": 50}}}}),
        json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Hel"}}}),
        json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 1,
            "delta": {"type": "thinking_delta", "thinking": "hmm"}}}),
        json!({"type": "stream_event", "event": {"type": "message_delta", "usage": {"output_tokens": 7}}}),
        json!({"type": "rate_limit_event"}),
        json!({"type": "result", "subtype": "success", "is_error": false, "result": "Hello"}),
    ]);
    let mut expected = Usage::new();
    expected.input_tokens = 2;
    expected.cache_creation_input_tokens = 300;
    expected.cached_input_tokens = 50;
    expected.output_tokens = 7;
    expected.total_tokens = 359;
    assert_eq!(
        updates,
        vec![
            Update::Session("s1".into()),
            Update::Text("Hel".into()),
            Update::Usage(expected),
            Update::Done(Outcome {
                is_error: false,
                text: "Hello".into()
            }),
        ]
    );
}

#[test]
fn ignores_subagent_events_and_reads_error_lists() {
    let updates = parse_all(&[
        json!({"type": "assistant", "parent_tool_use_id": "toolu_1",
            "message": {"id": "m", "content": [{"type": "text", "text": "sub"}]}}),
        json!({"type": "user", "message": {"content": [{"type": "text", "text": "not a result"}]}}),
        json!({"type": "result", "is_error": true, "errors": ["boom", "again"]}),
    ]);
    assert_eq!(
        updates,
        vec![Update::Done(Outcome {
            is_error: true,
            text: "boom\nagain".into()
        })]
    );
}

#[test]
fn sign_in_errors_point_to_login() {
    let error = Outcome {
        is_error: true,
        text: "Not logged in · Please run /login".into(),
    }
    .into_result()
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Run /login to sign in to Claude Code.")
    );
    let error = Outcome {
        is_error: true,
        text: "Overloaded".into(),
    }
    .into_result()
    .unwrap_err();
    assert_eq!(error.to_string(), "Overloaded");
}

#[test]
fn recorder_builds_history_and_events() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut recorder = Recorder::new(vec![Message::user("find it")], &tx);
    // One response, sent as one message per content block.
    recorder.apply(Update::Assistant {
        id: "msg_1".into(),
        content: vec![json!({"type": "text", "text": "Looking."})],
    });
    recorder.apply(Update::Assistant {
        id: "msg_1".into(),
        content: vec![
            json!({"type": "tool_use", "id": "toolu_1", "name": "mcp__tamale__grep",
            "input": {"pattern": "fn seed", "description": "Find seed"}}),
        ],
    });
    recorder.apply(Update::ToolResults(vec![json!({"type": "tool_result",
        "tool_use_id": "toolu_1", "content": [{"type": "text", "text": "src/a.rs:1"}]})]));
    recorder.apply(Update::Assistant {
        id: "msg_2".into(),
        content: vec![
            json!({"type": "thinking", "thinking": "x", "signature": "s"}),
            json!({"type": "text", "text": "src/a.rs"}),
        ],
    });

    assert_eq!(recorder.history.len(), 4);
    let Message::Assistant { content, .. } = &recorder.history[1] else {
        panic!("expected assistant message");
    };
    assert_eq!(content.len(), 2);
    let AssistantContent::ToolCall(call) = &content[1] else {
        panic!("expected tool call");
    };
    assert_eq!(call.function.name, "grep");
    let Message::User { content } = &recorder.history[2] else {
        panic!("expected tool results");
    };
    let UserContent::ToolResult(result) = &content[0] else {
        panic!("expected tool result");
    };
    assert_eq!(result.name, "grep");
    assert_eq!(
        result.provider.as_ref().map(|p| p.call_id.as_str()),
        Some("toolu_1")
    );
    let Message::Assistant { content, .. } = &recorder.history[3] else {
        panic!("expected final answer");
    };
    assert!(matches!(&content[..], [AssistantContent::Text(t)] if t.text == "src/a.rs"));

    let mut calls = 0;
    let mut results = Vec::new();
    let mut snapshots = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            AgentEvent::History(h) => snapshots.push(h.len()),
            AgentEvent::ToolCall {
                name, description, ..
            } => {
                assert_eq!((name.as_str(), description.as_str()), ("grep", "Find seed"));
                calls += 1;
            }
            AgentEvent::ToolResult { output, is_error } => results.push((output, is_error)),
            _ => {}
        }
    }
    assert_eq!(calls, 1);
    assert_eq!(results, vec![("src/a.rs:1".to_string(), false)]);
    // Text before the call, after its result, and the answer; never mid-call.
    assert_eq!(snapshots, vec![2, 3, 4]);
}

#[test]
fn replays_earlier_conversation_without_the_new_prompt() {
    let mut history = vec![
        Message::user("first"),
        Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::text("checking"),
                AssistantContent::tool_call("t1", "read_file", json!({"path": "a.rs"})),
            ],
        },
        Message::User {
            content: vec![UserContent::tool_result_from_wire(
                "t1",
                "read_file",
                vec![ToolResultContent::text("x".repeat(SEED_RESULT_CHARS + 10))],
            )],
        },
    ];
    agent::push_user_text(&mut history, "next".into());

    let prior = without_prompt(&history, "next");
    assert_eq!(prior.len(), 3);
    let Message::User { content } = &prior[2] else {
        panic!("tool results stay");
    };
    assert_eq!(content.len(), 1);

    let prompt = seed(&prior, "next");
    assert!(prompt.starts_with("<previous_conversation>"));
    assert!(prompt.contains("[user]\nfirst\n"));
    assert!(prompt.contains("[assistant]\nchecking\n"));
    assert!(prompt.contains(r#"[tool call] read_file {"path":"a.rs"}"#));
    assert!(prompt.contains("… [shortened]"));
    assert!(prompt.ends_with("</previous_conversation>\n\nnext"));

    assert_eq!(without_prompt(&[Message::user("only")], "only").len(), 0);
    assert_eq!(seed(&[], "only"), "only");
}

#[test]
fn labels_describe_the_account() {
    let mut cc = ClaudeCode {
        program: "claude".into(),
        account: Account::default(),
    };
    assert_eq!(cc.label(), "Claude Code (signed out)");
    cc.account = serde_json::from_value(json!({"loggedIn": true, "authMethod": "claude.ai",
        "subscriptionType": "max", "email": "a@b.c"}))
    .unwrap();
    assert_eq!(cc.label(), "Claude Code · Max");
    cc.account.subscription_type = None;
    cc.account.auth_method = Some("api_key".into());
    assert_eq!(cc.label(), "Claude Code · API key");
}

fn unrestricted() -> TurnCtx {
    let mut policy = Policy::new(&Settings::default().permissions, std::env::temp_dir());
    policy.mode = Mode::Auto;
    TurnCtx {
        permissions: policy.shared(),
        checkpoint: None,
    }
}

async fn rpc(url: &str, body: Value) -> (reqwest::StatusCode, String) {
    let response = reqwest::Client::new()
        .post(url)
        .header("accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .unwrap();
    (response.status(), response.text().await.unwrap())
}

#[tokio::test]
async fn bridge_serves_tamale_tools_over_mcp() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let dir = std::env::temp_dir().join(format!("tamale-mcp-{}", crate::session::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("note.txt"), "pumpernickel\n").unwrap();
    let bridge = Bridge::start(Tools {
        ctx: Some(Mutex::new(unrestricted())),
        workspace: Workspace {
            cwd: dir.clone(),
            additional_dirs: vec![],
        },
        tx,
    })
    .await
    .unwrap();

    let (status, body) = rpc(
        &bridge.url,
        json!({"jsonrpc": "2.0", "id": 1,
        "method": "initialize", "params": {"protocolVersion": "2025-11-25"}}),
    )
    .await;
    assert_eq!(status, 200);
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");

    let (status, _) = rpc(
        &bridge.url,
        json!({"jsonrpc": "2.0",
        "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(status, 202);

    let (_, body) = rpc(
        &bridge.url,
        json!({"jsonrpc": "2.0", "id": 2, "method": "server/discover"}),
    )
    .await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["error"]["code"], -32601);

    let (_, body) = rpc(
        &bridge.url,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
    )
    .await;
    let body: Value = serde_json::from_str(&body).unwrap();
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"read_file") && names.contains(&"bash"));

    let path = dir.join("note.txt");
    let (_, body) = rpc(
        &bridge.url,
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "read_file", "arguments": {"path": path}}}),
    )
    .await;
    let data = body
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .expect("an SSE data line");
    let reply: Value = serde_json::from_str(data).unwrap();
    assert_eq!(reply["id"], 4);
    assert_eq!(reply["result"]["isError"], false);
    assert!(
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("pumpernickel")
    );

    // Wrong token: not found, and no tool runs.
    let wrong = bridge.url.rsplit_once('/').unwrap().0.to_string() + "/nope";
    let (status, _) = rpc(
        &wrong,
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, 404);
    assert!(rx.try_recv().is_err(), "the bridge reports nothing itself");

    drop(bridge);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn bridge_refuses_calls_without_a_turn() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let bridge = Bridge::start(Tools {
        ctx: None,
        workspace: Workspace::current().unwrap(),
        tx,
    })
    .await
    .unwrap();
    let (_, body) = rpc(
        &bridge.url,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "bash", "arguments": {"command": "touch should-not-exist"}}}),
    )
    .await;
    assert!(body.contains(r#""isError":true"#));
    assert!(!std::path::Path::new("should-not-exist").exists());
}

/// Runs against the installed, signed-in `claude` CLI:
/// `cargo test live_claude_code -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn live_claude_code_continues_and_compacts() {
    let cc = ClaudeCode::connect("claude".into()).await.unwrap();
    assert!(
        cc.account.logged_in,
        "sign in with `claude auth login` first"
    );
    let settings = Settings::default();
    let turn = |history: Vec<Message>, prompt: &str, thread: Option<String>| Turn {
        model: "claude-haiku-5-5".into(),
        history,
        prompt: prompt.into(),
        thread,
        config: settings.turn_config(0),
        workspace: Workspace::current().unwrap(),
    };
    async fn drive(
        future: impl std::future::Future<Output = ()>,
        mut rx: mpsc::UnboundedReceiver<AgentEvent>,
    ) -> (Vec<Message>, Option<String>, String) {
        future.await;
        let (mut history, mut thread, mut text) = (Vec::new(), None, String::new());
        while let Ok(event) = rx.try_recv() {
            match event {
                AgentEvent::History(h) => history = h,
                AgentEvent::Thread(id) => thread = Some(id),
                AgentEvent::TextDelta(t) => text.push_str(&t),
                AgentEvent::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        (history, thread, text)
    }

    let mut history = vec![Message::user(
        "Remember the word 'quince'. Reply with just OK.",
    )];
    let (tx, rx) = mpsc::unbounded_channel();
    let first = turn(
        history.clone(),
        "Remember the word 'quince'. Reply with just OK.",
        None,
    );
    let (h, thread, _) = drive(cc.clone().run(first, unrestricted(), tx), rx).await;
    history = h;
    assert!(thread.is_some());

    // Continue the same Claude Code session.
    agent::push_user_text(&mut history, "What word? One word.".into());
    let (tx, rx) = mpsc::unbounded_channel();
    let second = turn(history.clone(), "What word? One word.", thread.clone());
    let (h, _, text) = drive(cc.clone().run(second, unrestricted(), tx), rx).await;
    assert!(text.to_lowercase().contains("quince"), "{text}");

    // Compact, then a fresh session sees only the summary.
    let (tx, rx) = mpsc::unbounded_channel();
    let (summary, _, _) = drive(cc.clone().compact(turn(h, "", thread), None, tx), rx).await;
    assert_eq!(summary.len(), 1);
    let mut history = summary;
    agent::push_user_text(
        &mut history,
        "Which word did I ask you to remember? One word.".into(),
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let third = turn(
        history,
        "Which word did I ask you to remember? One word.",
        None,
    );
    let (_, _, text) = drive(cc.run(third, unrestricted(), tx), rx).await;
    assert!(text.to_lowercase().contains("quince"), "{text}");
}
