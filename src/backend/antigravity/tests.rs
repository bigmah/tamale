use serde_json::json;

use super::*;

fn parse_all(lines: &[Value]) -> Vec<Update> {
    let mut parser = Parser::default();
    lines.iter().flat_map(|l| parser.parse(l)).collect()
}

fn step(index: u64, state: &str, kind: &str, extra: Value) -> Value {
    let mut step = json!({"conversation_id": "c1", "step_index": index, "state": state,
        "step_type": kind});
    step.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({"event": "step_update", "step_update": step})
}

fn agy_usage(input: u64, cached: u64, output: u64) -> Value {
    json!({"input_tokens": input, "output_tokens": output, "thinking_tokens": 1,
        "cache_read_tokens": cached, "total_tokens": input + output})
}

fn tamale_usage(input: u64, cached: u64, output: u64) -> Usage {
    let mut u = Usage::new();
    u.input_tokens = input;
    u.cached_input_tokens = cached;
    u.output_tokens = output;
    u.total_tokens = input + cached + output;
    u
}

#[test]
fn parses_thread_text_tools_and_usage() {
    let updates = parse_all(&[
        json!({"event": "init", "conversation_id": "c1", "init": {"cwd": "/w",
            "tools": ["run_command"], "permission_mode": "request-review"}}),
        step(0, "DONE", "user_input", json!({})),
        // The request that called the tool: no text, just usage.
        step(
            1,
            "DONE",
            "agent_response",
            json!({"usage": agy_usage(2000, 0, 30)}),
        ),
        step(
            2,
            "ACTIVE",
            "tool",
            json!({"tool_name": "mcp_tamale_read_file",
            "tool_info": {"name": "mcp_tamale_read_file", "parameters": {"path": "a.rs"}}}),
        ),
        step(
            2,
            "DONE",
            "tool",
            json!({"tool_name": "mcp_tamale_read_file",
            "tool_info": {"name": "mcp_tamale_read_file", "parameters": {"path": "a.rs"},
                "output": "Created At: 2026-01-01T00:00:00Z\nCompleted At: 2026-01-01T00:00:01Z\nfn main() {}"}}),
        ),
        // Antigravity's own tools aren't tamale's.
        step(
            3,
            "DONE",
            "tool",
            json!({"tool_name": "finish", "tool_info": {"name": "finish"}}),
        ),
        step(
            4,
            "ACTIVE",
            "agent_response",
            json!({"text_delta": "It's "}),
        ),
        step(
            4,
            "DONE",
            "agent_response",
            json!({"text_delta": "main.\n",
            "usage": agy_usage(40, 2000, 5)}),
        ),
        // A side request that isn't the conversation's size.
        step(
            5,
            "DONE",
            "checkpoint",
            json!({"usage": agy_usage(100, 0, 7)}),
        ),
        json!({"event": "result", "result": {"conversation_id": "c1", "status": "SUCCESS",
            "response": "It's main.\n", "num_turns": 1, "usage": agy_usage(2140, 2000, 42)}}),
    ]);
    let id = "agy-c1-2".to_string();
    assert_eq!(
        updates,
        vec![
            Update::Thread("c1".into()),
            Update::Usage {
                usage: tamale_usage(2000, 0, 30),
                request: true
            },
            Update::ToolCall {
                id: id.clone(),
                name: "read_file".into(),
                args: json!({"path": "a.rs"}),
            },
            Update::ToolResult {
                id,
                name: "read_file".into(),
                output: "fn main() {}".into(),
                is_error: false,
            },
            Update::Text {
                step: 4,
                text: "It's ".into()
            },
            Update::Text {
                step: 4,
                text: "main.\n".into()
            },
            Update::Usage {
                usage: tamale_usage(40, 2000, 5),
                request: true
            },
            Update::Usage {
                usage: tamale_usage(100, 0, 7),
                request: false
            },
            Update::Done(Outcome {
                is_error: false,
                text: String::new()
            }),
        ]
    );
}

#[test]
fn reports_tool_errors_unannounced_calls_and_failed_runs() {
    let updates = parse_all(&[
        // Finished without an ACTIVE step: the call is announced first.
        step(
            2,
            "DONE",
            "tool",
            json!({"tool_name": "mcp_tamale_shell",
            "tool_info": {"name": "mcp_tamale_shell", "parameters": {"command": "ls"},
                "error": {"type": "MCP_ERROR", "message": "connection refused"}}}),
        ),
        json!({"event": "result", "result": {"conversation_id": "", "status": "ERROR",
            "response": "", "error": "authentication failed or timed out"}}),
    ]);
    assert_eq!(
        updates,
        vec![
            Update::Thread("c1".into()),
            Update::ToolCall {
                id: "agy-c1-2".into(),
                name: "shell".into(),
                args: json!({"command": "ls"}),
            },
            Update::ToolResult {
                id: "agy-c1-2".into(),
                name: "shell".into(),
                output: "connection refused".into(),
                is_error: true,
            },
            Update::Done(Outcome {
                is_error: true,
                text: "authentication failed or timed out".into()
            }),
        ]
    );
    let interrupted = parse_all(&[json!({"event": "result",
        "result": {"status": "INTERRUPTED", "response": ""}})]);
    assert_eq!(
        interrupted,
        vec![Update::Done(Outcome {
            is_error: true,
            text: "Antigravity ended the turn: INTERRUPTED".into()
        })]
    );
}

#[test]
fn sign_in_errors_point_to_login() {
    let error = Outcome {
        is_error: true,
        text: "authentication failed or timed out".into(),
    }
    .into_result::<Antigravity>()
    .unwrap_err()
    .to_string();
    assert!(error.contains("/login"), "{error}");
    let other = Outcome {
        is_error: true,
        text: "model is overloaded".into(),
    }
    .into_result::<Antigravity>()
    .unwrap_err()
    .to_string();
    assert!(!other.contains("/login"), "{other}");
}

#[test]
fn reads_the_sign_in_from_acp() {
    let signed_in = json!({"jsonrpc": "2.0", "id": 2, "result": {"sessions": []}});
    assert!(account(&signed_in).unwrap().logged_in);
    let signed_out = json!({"jsonrpc": "2.0", "id": 2,
        "error": {"code": -32000, "message": "Authentication required"}});
    assert!(!account(&signed_out).unwrap().logged_in);
    let broken = json!({"jsonrpc": "2.0", "id": 2,
        "error": {"code": -32601, "message": "Method not found"}});
    assert!(account(&broken).is_err());
}

fn catalog() -> Vec<(String, String)> {
    parse_models(
        "gemini-4-neon-high\tGemini 4 Neon (High)\n\
gemini-4-neon-low\tGemini 4 Neon (Low)\n\
gemini-3.8-flash-high\tGemini 3.8 Flash (High)\n\
gemini-3.8-flash-medium\tGemini 3.8 Flash (Medium)\n\
claude-opus-5-5-high\tClaude Opus 5.5 (High)\n\
gpt-oss-120b-medium\tGPT-OSS 120B (Medium)\n\n",
    )
}

#[test]
fn parses_the_model_list() {
    let models = catalog();
    assert_eq!(models.len(), 6);
    assert_eq!(
        models[0],
        (
            "gemini-4-neon-high".to_string(),
            "Gemini 4 Neon (High)".to_string()
        )
    );
}

#[test]
fn models_antigravity_knows_are_passed_on() {
    let models = catalog();
    let catalog = Some(models.as_slice());
    // tamale's Anthropic model setting (the default) lets Antigravity pick its own, even
    // when Antigravity serves that model too.
    assert_eq!(model("claude-opus-5-5", None), None);
    assert_eq!(model("claude-opus-5-5", catalog), None);
    assert_eq!(model("claude-sonnet-4-5", catalog), None);
    assert_eq!(model("", catalog), None);
    // One of its Claude slugs, as `/model` lists them.
    assert_eq!(
        model("claude-opus-5-5-high", catalog),
        Some("claude-opus-5-5-high")
    );
    // Other models: a slug, or one without the effort.
    assert_eq!(
        model("gemini-4-neon-low", catalog),
        Some("gemini-4-neon-low")
    );
    assert_eq!(model("gemini-3.8-flash", catalog), Some("gemini-3.8-flash"));
    // Anything else fails loudly in Antigravity rather than running another model.
    assert_eq!(model("gemini-9", catalog), Some("gemini-9"));
}

#[test]
fn effort_fits_the_model() {
    let models = catalog();
    let catalog = Some(models.as_slice());
    // A slug names its own effort.
    assert_eq!(
        effort(Some("gemini-4-neon-high"), Some(Effort::Low), catalog),
        None
    );
    assert_eq!(
        effort(Some("gemini-3.8-flash"), Some(Effort::Medium), catalog),
        Some("medium")
    );
    // The closest level the model has, at most the one asked for…
    assert_eq!(
        effort(Some("gemini-3.8-flash"), Some(Effort::Max), catalog),
        Some("high")
    );
    // …or its lowest.
    assert_eq!(
        effort(Some("gemini-3.8-flash"), Some(Effort::Low), catalog),
        Some("medium")
    );
    assert_eq!(effort(None, Some(Effort::Max), catalog), Some("xhigh"));
    assert_eq!(effort(None, None, catalog), None);
}

#[test]
fn strips_the_times_antigravity_adds_to_tool_output() {
    assert_eq!(
        tool_output("Created At: x\nCompleted At: y\nline 1\nline 2"),
        "line 1\nline 2"
    );
    assert_eq!(tool_output("plain"), "plain");
    assert_eq!(tool_output("Created At: x"), "");
}

#[test]
fn agent_and_mcp_config_replace_antigravitys_prompt_and_tools() {
    let agent = agent_file("You are tamale.");
    assert!(agent.starts_with("---\nname: tamale\n"), "{agent}");
    assert!(agent.contains("\ntools: []\n"), "{agent}");
    assert!(agent.ends_with("---\nYou are tamale.\n"), "{agent}");

    let config = mcp_config(
        "http://127.0.0.1:1/mcp",
        "secret",
        &["read_file".into(), "shell".into()],
    );
    let server = &config["mcpServers"]["tamale"];
    assert_eq!(server["serverUrl"], "http://127.0.0.1:1/mcp");
    assert_eq!(server["headers"]["Authorization"], "Bearer secret");
    assert_eq!(server["tools"]["shell"]["eager"], true);
    assert_eq!(server["tools"]["read_file"]["eager"], true);

    let settings: Value = serde_json::from_str(&settings_json()).unwrap();
    assert_eq!(settings["permissions"]["allow"], json!(["mcp(tamale/*)"]));
}

#[tokio::test]
async fn setup_is_private_and_removed_after_the_run() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let bridge = Bridge::start(Tools {
        ctx: None,
        workspace: crate::workspace::Workspace::current().unwrap(),
        tx,
        chef: false,
        results: None,
    })
    .await
    .unwrap();
    let setup = Setup::write("prompt", &bridge, false).unwrap();
    let dir = setup.dir.clone();
    let config = dir.join(".agents/mcp_config.json");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains(&bridge.token));
    assert!(dir.join(".agents/agents/tamale/agent.md").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&config), 0o600);
    }
    drop(setup);
    assert!(!dir.exists());
}

#[test]
fn recorder_streams_text_and_builds_history() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let results = ToolResults::default();
    let mut recorder = Recorder::new(vec![Message::user("go")], Some(results.clone()), &tx);
    recorder.apply(Update::Text {
        step: 1,
        text: "First".into(),
    });
    recorder.apply(Update::Text {
        step: 1,
        text: ".\n".into(),
    });
    recorder.apply(Update::Text {
        step: 2,
        text: "Second.".into(),
    });
    recorder.apply(Update::ToolCall {
        id: "a".into(),
        name: "read_file".into(),
        args: json!({"path": "a"}),
    });
    // The tool's own output wins over the stream's.
    results.lock().unwrap().push_back(mcp::ToolResult {
        name: "read_file".into(),
        output: "full output".into(),
        is_error: false,
    });
    recorder.apply(Update::ToolResult {
        id: "a".into(),
        name: "read_file".into(),
        output: "shortened".into(),
        is_error: false,
    });
    recorder.apply(Update::Text {
        step: 4,
        text: "Done.".into(),
    });
    recorder.finish();

    let history = &recorder.history;
    assert_eq!(history.len(), 4, "{history:?}");
    assert!(matches!(&history[1], Message::Assistant { content, .. } if content.len() == 3));
    assert!(matches!(&history[2], Message::User { content }
        if matches!(&content[0], UserContent::ToolResult(r)
            if r.content.iter().any(|c| matches!(c, ToolResultContent::Text(t) if t.text == "full output")))));
    assert!(matches!(&history[3], Message::Assistant { content, .. }
        if matches!(&content[0], AssistantContent::Text(t) if t.text == "Done.")));
    assert!(results.lock().unwrap().is_empty());

    let mut texts = Vec::new();
    let mut snapshots = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            AgentEvent::TextDelta(t) => texts.push(t),
            AgentEvent::History(h) => snapshots.push(h.len()),
            _ => {}
        }
    }
    assert_eq!(texts, ["First", ".\n", "\n\n", "Second.", "Done."]);
    // Never while a call is waiting for its result.
    assert_eq!(snapshots, [2, 2, 3, 4]);
}

/// Runs real turns with the signed-in Antigravity CLI (uses the account's quota):
/// `cargo test live_antigravity -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn live_antigravity_uses_tools_continues_and_compacts() {
    use crate::config::Settings;
    use crate::permissions::{Mode, Policy};
    use crate::workspace::Workspace;

    let antigravity = Antigravity::connect("agy".into()).await.unwrap();
    assert!(antigravity.account.logged_in, "sign in with `agy` first");
    let settings = Settings::default();
    let (logins, errors) = super::super::logins::list(&settings.backend).await;
    assert!(errors.is_empty(), "{errors:?}");
    assert!(
        logins
            .iter()
            .any(|l| l.kind() == super::super::Kind::Antigravity && l.signed_in())
    );
    println!("models: {:?}", antigravity.list_models().await.unwrap());
    // A small model keeps the quota used down.
    let model = std::env::var("TAMALE_AGY_MODEL").unwrap_or("gemini-3.8-flash-low".into());
    let turn = |history: Vec<Message>, prompt: &str, thread: Option<String>| Turn {
        model: model.clone(),
        history,
        prompt: prompt.into(),
        thread,
        config: settings.turn_config(0),
        workspace: Workspace::current().unwrap(),
    };
    let ctx = || {
        let mut policy = Policy::new(&settings.permissions, std::env::temp_dir());
        policy.mode = Mode::Auto;
        TurnCtx::new(policy.shared(), None)
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
    let (mut history, thread, text, usage) =
        drive(antigravity.clone().run(first, ctx(), tx), rx).await;
    println!("first: {text:?}, usage: {usage:?}");
    assert!(text.contains("tamale"), "{text}");
    assert!(thread.is_some());
    assert!(
        history
            .iter()
            .any(|m| matches!(m, Message::Assistant { content, .. }
            if content.iter().any(|c| matches!(c, AssistantContent::ToolCall(_))))),
        "{history:?}"
    );
    assert!(usage.len() >= 2, "{usage:?}");

    // Continue the same Antigravity conversation.
    agent::push_user_text(&mut history, "What word? One word.".into());
    let (tx, rx) = mpsc::unbounded_channel();
    let second = turn(history.clone(), "What word? One word.", thread.clone());
    let (h, _, text, _) = drive(antigravity.clone().run(second, ctx(), tx), rx).await;
    println!("second: {text:?}");
    assert!(text.to_lowercase().contains("quince"), "{text}");

    // Compact, then a fresh conversation sees only the summary.
    let (tx, rx) = mpsc::unbounded_channel();
    let (summary, _, _, _) = drive(
        antigravity.clone().compact(turn(h, "", thread), None, tx),
        rx,
    )
    .await;
    assert_eq!(summary.len(), 1);
    let mut history = summary;
    let question = "Which word did I ask you to remember? One word.";
    agent::push_user_text(&mut history, question.into());
    let (tx, rx) = mpsc::unbounded_channel();
    let (_, _, text, _) = drive(
        antigravity.run(turn(history, question, None), ctx(), tx),
        rx,
    )
    .await;
    println!("third: {text:?}");
    assert!(text.to_lowercase().contains("quince"), "{text}");
}
