use serde_json::json;

use super::*;

fn parse_all(lines: &[Value]) -> (Vec<Update>, Parser) {
    let mut parser = Parser::default();
    let updates = lines.iter().filter_map(|l| parser.parse(l)).collect();
    (updates, parser)
}

fn tool_item(id: &str, server: &str, extra: Value) -> Value {
    let mut item = json!({"id": id, "type": "mcp_tool_call", "server": server,
        "tool": "read_file", "arguments": {"path": "a.rs"}});
    item.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    item
}

#[test]
fn parses_thread_text_tools_and_usage() {
    let (updates, parser) = parse_all(&[
        json!({"type": "thread.started", "thread_id": "t1"}),
        json!({"type": "turn.started"}),
        json!({"type": "item.completed", "item": {"id": "i0", "type": "error",
            "message": "ignoring an unknown setting"}}),
        json!({"type": "item.completed", "item": {"id": "i1", "type": "reasoning", "text": "hmm"}}),
        json!({"type": "item.completed", "item": {"id": "i2", "type": "agent_message",
            "text": "Looking."}}),
        json!({"type": "item.started", "item": tool_item("i3", "tamale", json!({"status": "in_progress"}))}),
        json!({"type": "item.completed", "item": tool_item("i3", "tamale", json!({"status": "completed",
            "result": {"content": [{"type": "text", "text": "fn main"}, {"type": "text", "text": "{}"}]},
            "error": null}))}),
        // Codex's own tools and other servers aren't tamale's.
        json!({"type": "item.started", "item": tool_item("i4", "other", json!({}))}),
        json!({"type": "item.completed", "item": {"id": "i5", "type": "command_execution"}}),
        json!({"type": "turn.completed", "usage": {"input_tokens": 1000,
            "cached_input_tokens": 800, "cache_write_input_tokens": 0, "output_tokens": 50,
            "reasoning_output_tokens": 10}}),
    ]);
    let mut usage = Usage::new();
    usage.input_tokens = 200;
    usage.cached_input_tokens = 800;
    usage.output_tokens = 50;
    usage.total_tokens = 1050;
    assert_eq!(
        updates,
        vec![
            Update::Thread("t1".into()),
            Update::Text("Looking.".into()),
            Update::ToolCall {
                id: "i3".into(),
                name: "read_file".into(),
                args: json!({"path": "a.rs"}),
            },
            Update::ToolResult {
                id: "i3".into(),
                output: "fn main\n{}".into(),
                is_error: false,
            },
            Update::Usage(usage),
        ]
    );
    assert!(parser.completed && parser.error.is_none());
}

#[test]
fn reports_tool_errors_unannounced_calls_and_failed_turns() {
    let mut parser = Parser::default();
    let failed = tool_item(
        "i1",
        "tamale",
        json!({"status": "failed",
        "result": null, "error": {"message": "denied"}}),
    );
    let event = json!({"type": "item.completed", "item": failed});
    // A call that only shows up finished is announced first, then its result.
    assert!(matches!(
        parser.parse(&event),
        Some(Update::ToolCall { .. })
    ));
    assert_eq!(
        parser.parse(&event),
        Some(Update::ToolResult {
            id: "i1".into(),
            output: "denied".into(),
            is_error: true,
        })
    );
    assert_eq!(
        parser.parse(&json!({"type": "error", "message": "stream disconnected"})),
        None
    );
    assert_eq!(
        parser.parse(&json!({"type": "turn.failed", "error": {}})),
        Some(Update::Done(Outcome {
            is_error: true,
            text: "stream disconnected".into()
        }))
    );
}

#[test]
fn sign_in_errors_point_to_login() {
    let error = Outcome {
        is_error: true,
        text: "unexpected status 401 Unauthorized".into(),
    }
    .into_result()
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Run /login to sign in to Codex.")
    );
    let error = Outcome {
        is_error: true,
        text: "Rate limited".into(),
    }
    .into_result()
    .unwrap_err();
    assert_eq!(error.to_string(), "Rate limited");
}

#[test]
fn claude_models_leave_the_choice_to_codex() {
    assert_eq!(model("claude-opus-5-5"), None);
    assert_eq!(model("anthropic/claude-sonnet-5-5"), None);
    assert_eq!(model(" "), None);
    assert_eq!(model("gpt-6.1-sol"), Some("gpt-6.1-sol"));
}

#[test]
fn config_strings_are_quoted_for_toml() {
    let quoted = toml_string("say \"hi\"\n\\ok");
    let parsed: toml::Table = toml::from_str(&format!("v = {quoted}")).unwrap();
    assert_eq!(parsed["v"].as_str(), Some("say \"hi\"\n\\ok"));
}

#[test]
fn reads_the_account_from_the_id_token() {
    let claims = json!({"email": "me@home.dev", "https://api.openai.com/auth": {
        "chatgpt_plan_type": "pro", "chatgpt_account_id": "acct-1"}});
    let payload = base64url_encode(claims.to_string().as_bytes());
    let auth = json!({"auth_mode": "chatgpt", "tokens": {
        "id_token": format!("e30.{payload}.sig"), "account_id": "acct-1"}});
    let mut codex = Codex::signed_out("codex".into(), None);
    codex.account.logged_in = true;
    read_auth(&auth, &mut codex.account);
    assert_eq!(codex.account.email.as_deref(), Some("me@home.dev"));
    assert_eq!(codex.account.plan.as_deref(), Some("pro"));
    assert_eq!(codex.label(), "Codex · Pro");
    assert_eq!(codex.label_with_account(), "Codex · Pro · me@home.dev");

    let mut other = codex.clone();
    other.home = Some("/tmp/elsewhere".into());
    assert!(codex.same_account(&other));
    other.account.email = Some("me@work.dev".into());
    assert!(!codex.same_account(&other));

    codex.account.auth_method = Some("api_key".into());
    assert_eq!(codex.label(), "Codex · API key");
    codex.account.logged_in = false;
    assert_eq!(codex.label(), "Codex (signed out)");
}

fn base64url_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(CHARS[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

#[test]
fn finds_the_last_request_in_the_session_file() {
    let home = std::env::temp_dir().join(format!("tamale-codex-test-{}", std::process::id()));
    let day = home.join("sessions/2026/10/30");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::create_dir_all(home.join("sessions/2026/09/01")).unwrap();
    let record = |input: u64| {
        json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
            "last_token_usage": {"input_tokens": input, "cached_input_tokens": 100,
                "output_tokens": 5}}}})
        .to_string()
    };
    std::fs::write(
        day.join("rollout-2026-10-30T10-00-00-thread-1.jsonl"),
        [
            record(300),
            r#"{"type":"response_item"}"#.into(),
            record(400),
        ]
        .join("\n"),
    )
    .unwrap();
    let last = last_request_usage(&home, "thread-1").unwrap();
    assert_eq!((last.input_tokens, last.cached_input_tokens), (300, 100));
    assert!(last_request_usage(&home, "thread-2").is_none());
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn turn_usage_is_split_so_the_last_part_is_the_last_request() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let codex = Codex::signed_out("codex".into(), Some("/nonexistent".into()));
    let mut total = Usage::new();
    total.input_tokens = 500;
    total.output_tokens = 20;
    // Without a session file the total is reported as is.
    codex.report_usage(total, Some("t"), &tx);
    assert!(matches!(rx.try_recv(), Ok(AgentEvent::Usage(u)) if u.input_tokens == 500));
    assert!(rx.try_recv().is_err());
}

#[test]
fn recorder_builds_history_from_messages_and_parallel_calls() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut recorder = Recorder::new(vec![Message::user("go")], &tx);
    recorder.apply(Update::Text("First.".into()));
    recorder.apply(Update::Text("Second.".into()));
    for id in ["a", "b"] {
        recorder.apply(Update::ToolCall {
            id: id.into(),
            name: "read_file".into(),
            args: json!({"path": id}),
        });
    }
    recorder.apply(Update::ToolResult {
        id: "a".into(),
        output: "A".into(),
        is_error: false,
    });
    recorder.apply(Update::ToolResult {
        id: "b".into(),
        output: "B".into(),
        is_error: true,
    });
    recorder.apply(Update::Text("Done.".into()));

    let history = &recorder.history;
    assert_eq!(history.len(), 4, "{history:?}");
    assert!(matches!(&history[1], Message::Assistant { content, .. } if content.len() == 4));
    assert!(matches!(&history[2], Message::User { content } if content.len() == 2));
    assert!(matches!(&history[3], Message::Assistant { content, .. } if content.len() == 1));

    let mut texts = Vec::new();
    let mut snapshots = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            AgentEvent::TextDelta(t) => texts.push(t),
            AgentEvent::History(h) => snapshots.push(h.len()),
            _ => {}
        }
    }
    assert_eq!(texts, ["First.", "\n\nSecond.", "Done."]);
    // Never while a call is waiting for its result.
    assert_eq!(snapshots, [2, 2, 3, 4]);
}

/// Runs real turns with the signed-in Codex CLI (uses the account's quota):
/// `cargo test live_codex -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn live_codex_uses_tools_continues_and_compacts() {
    use crate::config::Settings;
    use crate::permissions::{Mode, Policy};
    use crate::workspace::Workspace;

    let codex = Codex::connect("codex".into(), None).await.unwrap();
    assert!(codex.account.logged_in, "sign in with `codex login` first");
    let settings = Settings::default();
    let (logins, errors) = super::super::logins::list(&settings.backend).await;
    assert!(errors.is_empty(), "{errors:?}");
    assert!(
        logins
            .iter()
            .any(|l| l.kind() == super::super::Kind::Codex && l.signed_in())
    );
    println!(
        "logins: {:?}",
        logins.iter().map(|l| l.label()).collect::<Vec<_>>()
    );
    println!("models: {:?}", codex.list_models().await.unwrap());
    let turn = |history: Vec<Message>, prompt: &str, thread: Option<String>| Turn {
        model: settings.model.name.clone(),
        history,
        prompt: prompt.into(),
        thread,
        config: settings.turn_config(0),
        workspace: Workspace::current().unwrap(),
    };
    let ctx = || {
        let mut policy = Policy::new(&settings.permissions, std::env::temp_dir());
        policy.mode = Mode::Auto;
        TurnCtx {
            permissions: policy.shared(),
            checkpoint: None,
        }
    };
    async fn drive(
        future: impl std::future::Future<Output = ()>,
        mut rx: mpsc::UnboundedReceiver<AgentEvent>,
    ) -> (Vec<Message>, Option<String>, String, Vec<Usage>) {
        future.await;
        let (mut history, mut thread, mut text, mut usage) =
            (Vec::new(), None, String::new(), Vec::new());
        while let Ok(event) = rx.try_recv() {
            match event {
                AgentEvent::History(h) => history = h,
                AgentEvent::Thread(id) => thread = Some(id),
                AgentEvent::TextDelta(t) => text.push_str(&t),
                AgentEvent::Usage(u) => usage.push(u),
                AgentEvent::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        (history, thread, text, usage)
    }

    let prompt = "Use read_file on Cargo.toml, then reply with only the package name. Also \
remember the word 'quince'.";
    let (tx, rx) = mpsc::unbounded_channel();
    let first = turn(vec![Message::user(prompt)], prompt, None);
    let (mut history, thread, text, usage) = drive(codex.clone().run(first, ctx(), tx), rx).await;
    assert!(text.contains("tamale"), "{text}");
    assert!(thread.is_some());
    assert!(
        matches!(&history[1], Message::Assistant { content, .. }
            if content.iter().any(|c| matches!(c, AssistantContent::ToolCall(_)))),
        "{history:?}"
    );
    // Two requests (the call, then the answer): the last is reported on its own.
    assert_eq!(usage.len(), 2, "{usage:?}");

    // Continue the same Codex thread.
    agent::push_user_text(&mut history, "What word? One word.".into());
    let (tx, rx) = mpsc::unbounded_channel();
    let second = turn(history.clone(), "What word? One word.", thread.clone());
    let (h, _, text, _) = drive(codex.clone().run(second, ctx(), tx), rx).await;
    assert!(text.to_lowercase().contains("quince"), "{text}");

    // Compact, then a fresh thread sees only the summary.
    let (tx, rx) = mpsc::unbounded_channel();
    let (summary, _, _, _) = drive(codex.clone().compact(turn(h, "", thread), None, tx), rx).await;
    assert_eq!(summary.len(), 1);
    let mut history = summary;
    let question = "Which word did I ask you to remember? One word.";
    agent::push_user_text(&mut history, question.into());
    let (tx, rx) = mpsc::unbounded_channel();
    let (_, _, text, _) = drive(codex.run(turn(history, question, None), ctx(), tx), rx).await;
    assert!(text.to_lowercase().contains("quince"), "{text}");
}
