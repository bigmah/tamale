use super::view::{render_entry, render_quiet};
use super::*;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::text::Line;

#[test]
fn paste_inserts_multiline_unicode_at_cursor_without_submitting() {
    let client = anthropic::Client::new("test-key").unwrap();
    let (mut app, _rx) = App::new_with_channel(client, test_settings("test-model"));
    app.input = "before after".into();
    app.cursor = 7;
    app.on_event(Event::Paste("café\r\n世界\r/paste\n".into()));
    assert_eq!(app.input, "before café\n世界\n/paste\nafter");
    assert_eq!(app.cursor, "before café\n世界\n/paste\n".len());
    assert!(app.history.is_empty());
    assert!(!app.is_running());

    app.on_app_event(AppEvent::ClipboardPasted(Ok("more\r\n".into())));
    assert_eq!(app.input, "before café\n世界\n/paste\nmore\nafter");
    let input = app.input.clone();
    app.on_app_event(AppEvent::ClipboardPasted(Err("unavailable".into())));
    assert_eq!(app.input, input);
    assert!(matches!(app.entries.last(), Some(Entry::Error(_))));
}

#[test]
fn paste_targets_picker_filter_and_ignores_help() {
    let client = anthropic::Client::new("test-key").unwrap();
    let (mut app, _rx) = App::new_with_channel(client, test_settings("test-model"));
    app.picker = Some(ModelPicker {
        filter: "claude-".into(),
        selected: 3,
    });
    app.on_event(Event::Paste("sonnet".into()));
    let picker = app.picker.as_ref().unwrap();
    assert_eq!(picker.filter, "claude-sonnet");
    assert_eq!(picker.selected, 0);
    assert!(app.input.is_empty());

    app.picker = None;
    app.resume_picker = Some(ResumePicker {
        sessions: vec![],
        filter: String::new(),
        selected: 1,
    });
    app.on_event(Event::Paste("fix\r\nbug".into()));
    assert_eq!(app.resume_picker.as_ref().unwrap().filter, "fix bug");
    assert!(app.input.is_empty());
    app.resume_picker = None;
    app.help_scroll = Some(0);
    app.on_event(Event::Paste("ignored".into()));
    assert!(app.input.is_empty());
    app.help_scroll = None;
    app.effort_picker = Some(0);
    app.on_event(Event::Paste("ignored".into()));
    assert!(app.input.is_empty());
}

#[test]
fn copy_preserves_reply_text_and_selects_conversation_messages() {
    let client = anthropic::Client::new("test-key").unwrap();
    let (mut app, _rx) = App::new_with_channel(client, test_settings("test-model"));
    assert_eq!(app.copy_text(false), None);
    assert_eq!(app.copy_text(true), None);
    app.entries.push(Entry::User("show code".into()));
    app.entries.push(Entry::Assistant("old reply".into()));
    let reply = "```rust\nlet café = 1;\n```\n";
    app.entries.push(Entry::Assistant(reply.into()));
    app.entries.push(Entry::Info("status".into()));
    app.entries.push(Entry::Assistant(String::new()));
    assert_eq!(app.copy_text(false).as_deref(), Some(reply));
    let all = app.copy_text(true).unwrap();
    assert!(all.contains("You:\nshow code\n\nAssistant:\nold reply"));
    assert!(all.contains(reply));
    assert!(!all.contains("status"));
}

#[test]
fn thinking_command_toggles_and_accepts_explicit_modes() {
    let client = anthropic::Client::new("test-key").unwrap();
    let (mut app, _rx) = App::new_with_channel(client, test_settings("claude-sonnet-4-6"));
    assert_eq!(app.settings.model.thinking, agent::Thinking::Adaptive);
    app.command("/thinking");
    assert_eq!(app.settings.model.thinking, agent::Thinking::Off);
    app.command("/thinking");
    assert_eq!(app.settings.model.thinking, agent::Thinking::Adaptive);
    app.command("/thinking off");
    assert_eq!(app.settings.model.thinking, agent::Thinking::Off);
    app.command("/thinking on");
    assert_eq!(app.settings.model.thinking, agent::Thinking::Adaptive);
    app.command("/thinking typo");
    assert_eq!(app.settings.model.thinking, agent::Thinking::Adaptive);
    assert!(matches!(app.entries.last(), Some(Entry::Error(_))));
}

#[tokio::test]
async fn sessions_are_saved_and_resumed() {
    let dir = std::env::temp_dir().join(format!("harness-resume-test-{}", session::new_id()));
    // Only this test saves sessions (the others never have history).
    unsafe { std::env::set_var("HARNESS_SESSIONS_DIR", &dir) };
    let client = anthropic::Client::new("test-key").unwrap();
    let (mut app, _rx) = App::new_with_channel(client, test_settings("claude-sonnet-4-6"));
    app.command("/resume");
    assert!(app.resume_picker.is_none());

    app.entries.push(Entry::User("fix the bug\nmore".into()));
    app.entries.push(Entry::Assistant("done".into()));
    app.history = vec![Message::user("fix the bug"), Message::assistant("done")];
    app.context_tokens = 1234;
    app.save_session();
    let first = app.session_id.clone();

    app.command("/clear");
    assert_ne!(app.session_id, first);
    assert!(app.history.is_empty());

    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    app.command("/resume");
    let picker = app.resume_picker.as_ref().expect("picker opens");
    assert_eq!(picker.sessions.len(), 1);
    assert_eq!(picker.sessions[0].title, "fix the bug");
    app.on_event(key(KeyCode::Enter));
    assert!(app.resume_picker.is_none());
    assert_eq!(app.session_id, first);
    assert_eq!(app.history.len(), 2);
    assert_eq!(app.context_tokens, 1234);
    assert!(
        app.entries
            .iter()
            .any(|e| matches!(e, Entry::User(t) if t.starts_with("fix")))
    );
    assert!(matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("Resumed")));

    // The current session is excluded, and numeric/id arguments work.
    app.command("/resume 1");
    assert!(matches!(app.entries.last(), Some(Entry::Error(_))));
    app.command("/clear");
    app.command(&format!("/resume {}", &first[..first.len() - 2]));
    assert_eq!(app.session_id, first);

    app.save_session();
    app.command("/clear");
    let second = app.session_id.clone();
    app.entries.push(Entry::User("another session".into()));
    app.history = vec![Message::user("another session")];
    for n in 0..100 {
        app.context_tokens = n;
        app.save_session();
    }
    app.finish_saves().await;
    assert_eq!(session::load(&first).unwrap().history.len(), 2);
    let latest = session::load(&second).unwrap();
    assert_eq!(latest.context_tokens, 99);
    assert_eq!(latest.history, app.history);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mouse_wheel_scrolls_chat_help_and_pickers() {
    use crossterm::event::MouseEvent;
    let client = anthropic::Client::new("test-key").unwrap();
    let (mut app, _rx) = App::new_with_channel(client, test_settings("claude-sonnet-4-6"));
    assert!(
        app.settings.ui.mouse,
        "wheel scrolling must work without /mouse on"
    );
    let wheel = |kind| {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    };
    app.on_event(wheel(MouseEventKind::ScrollUp));
    app.on_event(wheel(MouseEventKind::ScrollUp));
    assert_eq!(app.scroll_back, 2 * WHEEL_LINES);
    app.on_event(wheel(MouseEventKind::ScrollDown));
    assert_eq!(app.scroll_back, WHEEL_LINES);
    // Pasting must leave the viewport alone and never submit the prompt; copying
    // must still return the full reply while scrolled up.
    app.entries
        .push(Entry::Assistant("A reply worth copying".into()));
    app.on_event(Event::Paste("first line\r\nsecond line".into()));
    assert_eq!(app.input, "first line\nsecond line");
    assert_eq!(app.scroll_back, WHEEL_LINES);
    assert!(app.history.is_empty());
    assert_eq!(
        app.copy_text(false).as_deref(),
        Some("A reply worth copying")
    );
    app.on_event(wheel(MouseEventKind::ScrollDown));
    app.on_event(wheel(MouseEventKind::ScrollDown));
    assert_eq!(app.scroll_back, 0);

    app.command("/help");
    app.on_event(wheel(MouseEventKind::ScrollDown));
    assert_eq!(app.help_scroll, Some(WHEEL_LINES));
    assert_eq!(app.scroll_back, 0);
    app.help_scroll = None;

    app.command("/effort");
    app.on_event(wheel(MouseEventKind::ScrollDown));
    assert_eq!(app.effort_picker, Some(1));
    app.on_event(wheel(MouseEventKind::ScrollUp));
    app.on_event(wheel(MouseEventKind::ScrollUp));
    assert_eq!(app.effort_picker, Some(0));
}

#[test]
fn effort_command_opens_picker_and_arrows_select() {
    let client = anthropic::Client::new("test-key").unwrap();
    let (mut app, _rx) = App::new_with_channel(client, test_settings("claude-sonnet-4-6"));
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    app.command("/effort");
    assert_eq!(app.effort_picker, Some(0));
    app.on_event(key(KeyCode::Down));
    app.on_event(key(KeyCode::Down));
    app.on_event(key(KeyCode::Enter));
    assert_eq!(app.effort_picker, None);
    assert_eq!(app.settings.model.effort, Some(agent::Effort::Medium));

    // Reopening starts on the current level; Esc cancels without changes.
    app.command("/effort");
    assert_eq!(app.effort_picker, Some(2));
    app.on_event(key(KeyCode::Up));
    app.on_event(key(KeyCode::Esc));
    assert_eq!(app.settings.model.effort, Some(agent::Effort::Medium));

    // Number keys jump straight to a level.
    app.command("/effort");
    app.on_event(key(KeyCode::Char('6')));
    assert_eq!(app.settings.model.effort, Some(agent::Effort::Max));

    app.command("/effort default");
    assert_eq!(app.settings.model.effort, None);
}

#[test]
fn quiet_mode_collapses_tool_calls_and_verbose_toggles() {
    let client = anthropic::Client::new("test-key").unwrap();
    let (mut app, _rx) = App::new_with_channel(client, test_settings("claude-sonnet-4-6"));
    assert!(!app.settings.ui.verbose);
    let tool = |n: usize| Entry::Tool {
        name: "bash".into(),
        summary: format!("cmd {n}"),
        description: format!("Step {n}"),
        output: Some(("line a\nline b".into(), false)),
    };
    let entries: Vec<Entry> = (0..7).map(tool).collect();
    let text = |lines: &[Line]| -> Vec<String> { lines.iter().map(|l| l.to_string()).collect() };

    let mut quiet = Vec::new();
    render_quiet(&entries, &mut quiet);
    let quiet = text(&quiet);
    assert!(quiet[0].contains("2 earlier steps"));
    assert!(
        quiet
            .iter()
            .any(|l| l.contains("Step 6") && l.contains("bash cmd 6"))
    );
    assert!(
        !quiet
            .iter()
            .any(|l| l.contains("cmd 1") || l.contains("line a"))
    );

    let mut verbose = Vec::new();
    for e in &entries {
        render_entry(e, true, &mut verbose);
    }
    assert!(text(&verbose).iter().any(|l| l.contains("line a")));

    app.command("/verbose");
    assert!(app.settings.ui.verbose);
    app.command("/verbose off");
    assert!(!app.settings.ui.verbose);
    app.command("/verbose on");
    assert!(app.settings.ui.verbose);
}

fn test_settings(model: &str) -> Settings {
    let mut settings = Settings::default();
    settings.model.name = model.into();
    settings
}

#[test]
fn configured_settings_reach_status_and_runtime_commands() {
    let client = anthropic::Client::new("test-key").unwrap();
    let mut settings = test_settings("configured-model");
    settings.model.context_window = Some(500_000);
    settings.model.thinking = agent::Thinking::Off;
    settings.model.effort = Some(agent::Effort::Low);
    settings.ui.mouse = false;
    settings.ui.verbose = true;
    let (mut app, _rx) = App::new_with_channel(client, settings);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    for expected in ["configured-model", "500k", "thinking off", "low", "verbose"] {
        assert!(text.contains(expected), "missing {expected} in {text}");
    }
    assert!(!app.settings.ui.mouse);
    app.command("/verbose off");
    app.command("/thinking on");
    app.command("/effort high");
    app.command("/model another-model");
    assert!(!app.settings.ui.verbose);
    let turn = app.settings.turn_config(0);
    assert_eq!(turn.reasoning.thinking, agent::Thinking::Adaptive);
    assert_eq!(turn.reasoning.effort, Some(agent::Effort::High));
    assert_eq!(turn.context_window, 500_000);
}

/// Run with `cargo test --release perf_transcript -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn perf_transcript() {
    use std::time::Instant;
    let client = anthropic::Client::new("test-key").unwrap();
    let (mut app, _rx) = App::new_with_channel(client, test_settings("test-model"));
    for i in 0..1000 {
        app.entries.push(Entry::User(format!(
            "Request {i}: inspect the implementation"
        )));
        app.entries.push(Entry::Assistant(
            "# Findings\nThe implementation uses `cached data` and **styled text**. ".repeat(10),
        ));
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    let start = Instant::now();
    terminal.draw(|f| app.draw(f)).unwrap();
    println!("transcript cold draw: {:?}", start.elapsed());
    let start = Instant::now();
    for _ in 0..100 {
        terminal.draw(|f| app.draw(f)).unwrap();
    }
    println!(
        "transcript unchanged draw: {:?}/frame",
        start.elapsed() / 100
    );
    let start = Instant::now();
    for _ in 0..100 {
        app.on_event(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));
        terminal.draw(|f| app.draw(f)).unwrap();
    }
    println!("transcript typing: {:?}/frame", start.elapsed() / 100);
    app.task = Some(tokio::spawn(std::future::pending()));
    app.on_app_event(AppEvent::Agent(
        app.turn,
        AgentEvent::TextDelta("a long streamed response with `code`\n".repeat(5000)),
    ));
    terminal.draw(|f| app.draw(f)).unwrap();
    let start = Instant::now();
    for _ in 0..100 {
        app.on_app_event(AppEvent::Agent(
            app.turn,
            AgentEvent::TextDelta("more streamed `code`\n".into()),
        ));
        terminal.draw(|f| app.draw(f)).unwrap();
    }
    println!("transcript streaming: {:?}/frame", start.elapsed() / 100);
    app.task.take().unwrap().abort();
}

#[tokio::test]
async fn agent_events_refresh_cached_text_tools_retries_and_clear() {
    let client = anthropic::Client::new("test-key").unwrap();
    let (mut app, _rx) = App::new_with_channel(client, test_settings("test-model"));
    app.task = Some(tokio::spawn(std::future::pending()));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    let mut screen = |app: &mut App| {
        terminal.draw(|frame| app.draw(frame)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    app.on_agent(app.turn, AgentEvent::TextDelta("partial response".into()));
    assert!(screen(&mut app).contains("partial response"));
    app.on_agent(
        app.turn,
        AgentEvent::TextDelta("\n```rust\nlet x = 1;\n```".into()),
    );
    assert!(screen(&mut app).contains("let x = 1;"));
    app.on_agent(
        app.turn,
        AgentEvent::Retry {
            attempt: 1,
            delay: std::time::Duration::ZERO,
            error: "try again".into(),
        },
    );
    let retried = screen(&mut app);
    assert!(!retried.contains("partial response"));
    assert!(retried.contains("try again"));
    app.on_agent(
        app.turn,
        AgentEvent::ToolCall {
            name: "bash".into(),
            summary: "test".into(),
            description: "Run tests".into(),
        },
    );
    assert!(screen(&mut app).contains("Run tests"));
    app.on_agent(
        app.turn,
        AgentEvent::ToolResult {
            output: "test output".into(),
            is_error: true,
        },
    );
    app.command("/verbose on");
    assert!(screen(&mut app).contains("test output"));
    app.on_agent(app.turn, AgentEvent::TextDelta("final response".into()));
    assert!(screen(&mut app).contains("final response"));
    app.on_agent(app.turn, AgentEvent::Done);
    app.command("/clear");
    let cleared = screen(&mut app);
    assert!(!cleared.contains("final response"));
    assert!(cleared.contains("Conversation cleared"));
}
