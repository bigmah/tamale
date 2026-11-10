use super::view::{render_entry, render_quiet};
use super::*;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::text::Line;

#[test]
fn paste_inserts_multiline_unicode_at_cursor_without_submitting() {
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
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
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
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
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
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
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("claude-sonnet-4-6"),
        Workspace::current().unwrap(),
    );
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
async fn add_dir_lists_scope_and_accepts_spaces_and_quotes_during_a_turn() {
    let root = std::env::temp_dir().join(format!("tamale-add-dir-{}", session::new_id()));
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::create_dir_all(root.join("shared files")).unwrap();
    std::fs::create_dir_all(root.join("other files")).unwrap();
    let workspace = Workspace {
        cwd: root.join("project"),
        additional_dirs: Vec::new(),
    };
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(client, test_settings("test-model"), workspace);
    app.command("/add-dir");
    assert!(matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("Usage: /add-dir")));
    app.command("/add-dir ../shared files");
    assert_eq!(
        app.workspace.additional_dirs,
        [root.join("shared files").canonicalize().unwrap()]
    );
    app.command("/add-dir \"../shared files\"");
    assert!(matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("already in scope")));
    assert_eq!(app.workspace.additional_dirs.len(), 1);
    app.command("/add-dir ../missing");
    assert!(matches!(app.entries.last(), Some(Entry::Error(_))));
    assert_eq!(app.workspace.additional_dirs.len(), 1);

    let in_flight = app.workspace.clone();
    app.task = Some(tokio::spawn(std::future::pending()));
    app.command("/add-dir '../other files'");
    assert!(matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("next message")));
    assert_eq!(in_flight.additional_dirs.len(), 1);
    assert_eq!(app.workspace.additional_dirs.len(), 2);
    app.interrupt();
    app.command("/clear");
    assert_eq!(app.workspace.additional_dirs.len(), 2);
    app.command("/add-dir");
    assert!(
        matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("shared files") && t.contains("other files"))
    );
    assert_eq!(app.workspace.cwd, root.join("project"));
    std::fs::remove_dir_all(root).unwrap();
}

/// Tests that save sessions point `TAMALE_SESSIONS_DIR` at their own directory; this
/// keeps them from running at the same time.
static SESSIONS_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn sessions_are_saved_and_resumed() {
    let _env = SESSIONS_ENV.lock().await;
    let dir = std::env::temp_dir().join(format!("tamale-resume-test-{}", session::new_id()));
    for name in ["startup", "shared", "later", "removed"] {
        std::fs::create_dir_all(dir.join(name)).unwrap();
    }
    unsafe { std::env::set_var("TAMALE_SESSIONS_DIR", &dir) };
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let mut workspace = Workspace::current().unwrap();
    workspace.add_dir(&dir.join("startup")).unwrap();
    let (mut app, _rx) =
        App::new_with_channel(client, test_settings("claude-sonnet-4-6"), workspace);
    app.command("/resume");
    assert!(app.resume_picker.is_none());

    app.entries.push(Entry::User("fix the bug\nmore".into()));
    app.entries.push(Entry::Assistant("done".into()));
    app.history = vec![Message::user("fix the bug"), Message::assistant("done")];
    app.context_tokens = 1234;
    app.save_session();
    let first = app.session_id.clone();
    // Adding a directory after a prompt immediately updates the saved scope.
    app.command(&format!("/add-dir {}", dir.join("shared").display()));
    app.command(&format!("/add-dir {}", dir.join("removed").display()));
    std::fs::remove_dir(dir.join("removed")).unwrap();

    app.command("/clear");
    assert_ne!(app.session_id, first);
    assert!(app.history.is_empty());
    app.command(&format!("/add-dir {}", dir.join("later").display()));

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
    assert_eq!(
        app.workspace.additional_dirs,
        [
            dir.join("startup").canonicalize().unwrap(),
            dir.join("shared").canonicalize().unwrap()
        ]
    );
    assert!(app.entries.iter().any(|e| matches!(e, Entry::Error(t) if t.contains("Couldn't restore scoped directory") && t.contains("removed"))));
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
    assert_eq!(
        session::load(&first).unwrap().additional_dirs,
        app.workspace.additional_dirs
    );
    let latest = session::load(&second).unwrap();
    assert_eq!(latest.context_tokens, 99);
    assert_eq!(latest.history, app.history);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn footer_tabs_open_by_click_or_by_down_arrows_enter() {
    use super::{FooterTab, WebRequest};
    use crossterm::event::{MouseButton, MouseEvent};
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("claude-sonnet-4-6"),
        Workspace::current().unwrap(),
    );
    app.settings.hub.key = Some(tamale_hub_protocol::encode_key(&[1; 32]));
    let lines = screen_lines(&mut app, 100, 30);
    let footer = &lines[28];
    assert!(footer.starts_with(" ≡ Sessions  ⇄ Remote off │ ◉ Web "), "{footer}");

    let click = |column, row| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    app.on_event(click(26, 28));
    assert!(
        app.take_web_request().is_none(),
        "a click elsewhere does nothing"
    );
    app.on_event(click(30, 28));
    assert!(matches!(
        app.take_web_request(),
        Some(WebRequest::Open { lan: true })
    ));
    app.on_event(click(15, 28));
    assert!(matches!(
        app.take_web_request(),
        Some(WebRequest::Remote { on: true })
    ));

    // In the prompt ← and → move the cursor; ↓ past its last line focuses the first tab.
    type_text(&mut app, "hi");
    app.on_event(key(KeyCode::Left));
    assert!(app.take_web_request().is_none());
    assert_eq!(app.cursor, 1);
    app.on_event(key(KeyCode::Down));
    assert_eq!(app.tab_focus, Some(FooterTab::Sessions));
    // Holding ↓ through the history doesn't open anything.
    app.on_event(key(KeyCode::Down));
    assert_eq!(app.tab_focus, Some(FooterTab::Sessions));
    assert!(app.sessions_picker.is_none());
    // → only highlights Remote; Enter opens it.
    app.on_event(key(KeyCode::Right));
    assert_eq!(app.tab_focus, Some(FooterTab::Remote));
    assert!(app.take_web_request().is_none());
    app.on_event(key(KeyCode::Down));
    assert!(app.take_web_request().is_none());
    app.on_event(key(KeyCode::Enter));
    assert!(matches!(
        app.take_web_request(),
        Some(WebRequest::Remote { on: true })
    ));
    // Web works the same way, and without a repository it's the last one.
    app.on_event(key(KeyCode::Right));
    assert_eq!(app.tab_focus, Some(FooterTab::Web));
    assert!(app.take_web_request().is_none());
    app.on_event(key(KeyCode::Enter));
    assert!(matches!(
        app.take_web_request(),
        Some(WebRequest::Open { lan: true })
    ));
    app.on_event(key(KeyCode::Right));
    assert_eq!(app.tab_focus, Some(FooterTab::Web), "nothing further right");
    for _ in 0..3 {
        app.on_event(key(KeyCode::Left));
    }
    assert_eq!(app.tab_focus, Some(FooterTab::Sessions), "nothing further left");
    // ↑, Esc and typing all go back to the prompt; typing still types.
    app.on_event(key(KeyCode::Up));
    assert_eq!(app.tab_focus, None);
    app.on_event(key(KeyCode::Down));
    app.on_event(key(KeyCode::Esc));
    assert_eq!(app.tab_focus, None);
    app.on_event(key(KeyCode::Down));
    type_text(&mut app, "!");
    assert_eq!(app.tab_focus, None);
    assert_eq!(app.input, "h!i");
    app.input.clear();
    app.cursor = 0;

    app.on_event(Event::Key(KeyEvent::new(
        KeyCode::Char('o'),
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(
        app.take_web_request(),
        Some(WebRequest::Open { lan: true })
    ));
    app.command("/web");
    assert!(matches!(
        app.take_web_request(),
        Some(WebRequest::Open { lan: true })
    ));
    app.command("/web local");
    assert!(matches!(
        app.take_web_request(),
        Some(WebRequest::Open { lan: false })
    ));
    for command in ["/remote", "/remote on"] {
        app.command(command);
        assert!(matches!(
            app.take_web_request(),
            Some(WebRequest::Remote { on: true })
        ));
    }
    app.command("/remote off");
    assert!(matches!(
        app.take_web_request(),
        Some(WebRequest::Remote { on: false })
    ));
    // A key given here is used for this run, and connects, keeping the conversation.
    let key = tamale_hub_protocol::encode_key(&[7; 32]);
    app.command(&format!("/remote {key}"));
    assert_eq!(app.settings.hub.key_bytes(), Ok([7; 32]));
    assert!(matches!(
        app.take_web_request(),
        Some(WebRequest::Remote { on: true })
    ));
    app.command("/remote thk_nope");
    assert!(app.take_web_request().is_none());
    assert_eq!(app.settings.hub.key_bytes(), Ok([7; 32]));
    // Not while connected with another one.
    app.set_hub_viewers(Some(0));
    app.command(&format!("/remote {}", tamale_hub_protocol::encode_key(&[8; 32])));
    assert!(app.take_web_request().is_none());
    assert_eq!(app.settings.hub.key_bytes(), Ok([7; 32]));
    app.set_hub_viewers(None);
    // Keys come from the hub's site, so `pair` and `rekey` went with the pairing key.
    for command in ["/remote nonsense", "/remote pair", "/remote rekey", "/web remote"] {
        app.command(command);
        assert!(app.take_web_request().is_none(), "{command}");
    }
}

#[test]
fn the_remote_tab_says_whether_the_hub_is_connected_and_stays_clickable() {
    use super::WebRequest;
    use crossterm::event::{MouseButton, MouseEvent};
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("claude-sonnet-4-6"),
        Workspace::current().unwrap(),
    );
    let footer = |app: &mut App| screen_lines(app, 100, 30)[28].clone();
    // Without a key, it says so before anyone counts on reaching the session.
    assert!(footer(&mut app).contains("⇄ Remote no key"));
    app.settings.hub.key = Some(tamale_hub_protocol::encode_key(&[1; 32]));
    assert!(footer(&mut app).contains("⇄ Remote off"));
    // Still being tried isn't reachable yet.
    app.set_hub_viewers(Some(0));
    assert!(footer(&mut app).contains("⇄ Remote connecting"));
    app.set_hub_live(true);
    let on = footer(&mut app);
    assert!(on.starts_with(" ≡ Sessions  ⇄ Remote on │ ◉ Web "), "{on}");
    // The narrower tab is still where a click lands, on it and on Web beside it.
    let click = |column| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: 28,
            modifiers: KeyModifiers::NONE,
        })
    };
    app.on_event(click(15));
    assert!(matches!(
        app.take_web_request(),
        Some(WebRequest::Remote { on: true })
    ));
    app.on_event(click(28));
    assert!(matches!(
        app.take_web_request(),
        Some(WebRequest::Open { .. })
    ));
    app.set_hub_viewers(Some(2));
    assert!(footer(&mut app).contains("⇄ Remote on · 2 viewing"));
    app.set_hub_viewers(None);
    assert!(footer(&mut app).contains("⇄ Remote off"), "a stale live doesn't count");
}

#[test]
fn the_web_session_screen_shows_what_became_of_the_hub() {
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (app, _rx) = App::new_with_channel(
        client,
        test_settings("claude-sonnet-4-6"),
        Workspace::current().unwrap(),
    );
    let screen_at = |url: &str, remote| {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 60)).unwrap();
        terminal
            .draw(|f| app.draw_web_session(f, url, remote, 0))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..60)
            .map(|y| {
                (0..100)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let screen = |remote| screen_at("http://127.0.0.1:1/?t=x", remote);
    use crate::web::HubState;
    let refused = screen(Some(HubState::Connecting(Some(
        "The hub refused this session".into(),
    ))));
    assert!(refused.contains("The hub refused this session"), "{refused}");
    assert!(!refused.contains("anywhere through the hub"));
    assert!(screen(Some(HubState::Connecting(None))).contains("Connecting to the hub"));
    let live = screen(Some(HubState::Live("https://hub.example/open/ab")));
    assert!(live.contains("https://hub.example/open/ab"), "{live}");
    assert!(live.contains("back to the session"), "esc says what it does");
    assert!(live.contains("turn remote off"), "{live}");
    let local = screen(None);
    assert!(!local.contains("hub"));
    assert!(local.contains("stop serving"), "{local}");
    assert!(!local.contains("turn remote off"), "{local}");
    // On the network, esc still closes it: the TUI wouldn't show it's open.
    let lan = screen_at(
        "http://192.168.1.2:1/?t=x",
        Some(HubState::Live("https://hub.example/open/ab")),
    );
    assert!(lan.contains("stop serving"), "{lan}");
}

#[test]
fn mouse_wheel_scrolls_chat_help_and_pickers() {
    use crossterm::event::MouseEvent;
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("claude-sonnet-4-6"),
        Workspace::current().unwrap(),
    );
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
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("claude-sonnet-4-6"),
        Workspace::current().unwrap(),
    );
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
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("claude-sonnet-4-6"),
        Workspace::current().unwrap(),
    );
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
    render_quiet(&entries, 80, &mut quiet);
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
        render_entry(e, true, 80, &mut verbose);
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
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let mut settings = test_settings("configured-model");
    settings.model.context_window = Some(500_000);
    settings.model.thinking = agent::Thinking::Off;
    settings.model.effort = Some(agent::Effort::Low);
    settings.ui.mouse = false;
    settings.ui.verbose = true;
    let (mut app, _rx) = App::new_with_channel(client, settings, Workspace::current().unwrap());
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
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
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
async fn continuation_keeps_completed_text_when_the_next_request_retries() {
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    app.task = Some(tokio::spawn(std::future::pending()));
    app.on_agent(
        app.turn,
        AgentEvent::TextDelta("Completed first chunk".into()),
    );
    app.on_agent(app.turn, AgentEvent::Continuing("Continuing…".into()));
    assert!(app.is_running());
    app.on_agent(
        app.turn,
        AgentEvent::TextDelta("Failed second chunk".into()),
    );
    app.on_agent(
        app.turn,
        AgentEvent::Retry {
            attempt: 1,
            delay: std::time::Duration::ZERO,
            error: "timeout".into(),
        },
    );
    assert!(
        app.entries
            .iter()
            .any(|e| matches!(e, Entry::Assistant(t) if t == "Completed first chunk"))
    );
    assert!(
        !app.entries
            .iter()
            .any(|e| matches!(e, Entry::Assistant(t) if t == "Failed second chunk"))
    );
    app.task.take().unwrap().abort();
}

#[tokio::test]
async fn agent_events_refresh_cached_text_tools_retries_and_clear() {
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
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

#[tokio::test]
async fn undo_and_rewind_restore_files_and_conversation() {
    let _env = SESSIONS_ENV.lock().await;
    let root = std::env::temp_dir().join(format!("tamale-undo-test-{}", session::new_id()));
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    unsafe { std::env::set_var("TAMALE_SESSIONS_DIR", root.join("sessions")) };
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let workspace = Workspace {
        cwd: work.clone(),
        additional_dirs: Vec::new(),
    };
    let (mut app, _rx) = App::new_with_channel(client, test_settings("test-model"), workspace);
    app.command("/undo");
    assert!(matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("Nothing to undo")));

    let existing = work.join("main.rs");
    let created = work.join("new.rs");
    std::fs::write(&existing, "original").unwrap();
    // What the agent does during a turn: snapshot, then write.
    let first = app.begin_turn("edit main".into()).checkpoint.unwrap();
    first.lock().unwrap().before_write(&existing).unwrap();
    std::fs::write(&existing, "edited").unwrap();
    first.lock().unwrap().command_ran().unwrap();
    app.history.push(Message::assistant("edited main"));
    app.entries.push(Entry::Assistant("edited main".into()));
    let second = app.begin_turn("add a file".into()).checkpoint.unwrap();
    second.lock().unwrap().before_write(&created).unwrap();
    std::fs::write(&created, "new").unwrap();
    app.history.push(Message::assistant("added"));
    app.entries.push(Entry::Assistant("added".into()));

    app.command("/undo");
    assert!(!created.exists());
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "edited");
    assert_eq!(app.history.len(), 2);
    assert_eq!(app.input, "add a file");
    assert!(matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("1 file restored")));
    assert_eq!(app.checkpoints.len(), 1);

    // An interrupted turn's follow-up prompt merges into the same user message.
    app.input.clear();
    app.history.push(Message::user("interrupted"));
    app.begin_turn("follow-up".into());
    app.command("/rewind");
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    app.on_event(key(KeyCode::Down));
    app.on_event(key(KeyCode::Enter));
    assert!(app.rewind_picker.is_none());
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "original");
    assert!(app.history.is_empty());
    assert_eq!(app.input, "edit main");
    let note = app
        .entries
        .iter()
        .rev()
        .find_map(|e| match e {
            Entry::Info(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        note.contains("1 file restored; 1 shell command not reverted"),
        "{note}"
    );
    assert!(app.checkpoints.is_empty());

    // Rewinding only the follow-up keeps the interrupted prompt.
    app.input.clear();
    app.history = vec![Message::user("interrupted")];
    app.begin_turn("follow-up".into());
    app.command("/undo");
    assert_eq!(app.history, [Message::user("interrupted")]);

    // After compaction, only files are restored.
    app.begin_turn("later".into());
    app.on_agent(
        app.turn,
        AgentEvent::Compacted {
            usage: Default::default(),
            before: 10,
            after: 1,
        },
    );
    let len = app.history.len();
    app.command("/undo");
    assert_eq!(app.history.len(), len);
    assert!(matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("not rewound")));
    app.finish_saves().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn approval_prompt_answers_and_mode_cycles() {
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let open = |app: &mut App, ready: bool| {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let mut prompt = approval::Prompt::new(
            "bash".into(),
            "cargo test".into(),
            "Run tests".into(),
            vec!["bash:cargo test*".into()],
            reply,
        );
        if ready {
            prompt.shown -= std::time::Duration::from_secs(1);
        }
        app.approval = Some(prompt);
        answer
    };

    // Keys typed right as the prompt opens don't answer it.
    let mut answer = open(&mut app, false);
    app.on_event(key(KeyCode::Char('y')));
    assert!(app.approval.is_some() && answer.try_recv().is_err());

    let mut answer = open(&mut app, true);
    app.on_event(key(KeyCode::Char('a')));
    assert!(matches!(
        answer.try_recv(),
        Ok(permissions::Approval::Allow)
    ));
    assert!(app.approval.is_none());
    let decision = app
        .permissions
        .lock()
        .unwrap()
        .check("bash", &serde_json::json!({"command": "cargo test -q"}));
    assert_eq!(decision, permissions::Decision::Allow);

    let mut answer = open(&mut app, true);
    app.on_event(key(KeyCode::Char('n')));
    app.on_event(Event::Paste("try ".into()));
    for c in "make".chars() {
        app.on_event(key(KeyCode::Char(c)));
    }
    assert!(app.input.is_empty());
    app.on_event(key(KeyCode::Enter));
    assert!(matches!(
        answer.try_recv(),
        Ok(permissions::Approval::Deny { reason: Some(r) }) if r == "try make"
    ));

    // Esc closes the prompt, which denies the call.
    let mut answer = open(&mut app, true);
    app.on_event(key(KeyCode::Esc));
    assert!(app.approval.is_none());
    assert!(answer.try_recv().is_err());

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
    let screen =
        |app: &mut App, terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
            terminal.draw(|frame| app.draw(frame)).unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
        };
    let _answer = open(&mut app, true);
    let text = screen(&mut app, &mut terminal);
    assert!(
        text.contains("Allow bash?") && text.contains("cargo test") && text.contains("Run tests")
    );
    app.approval = None;

    assert!(screen(&mut app, &mut terminal).contains(" auto "));
    app.on_event(Event::Key(KeyEvent::new(
        KeyCode::BackTab,
        KeyModifiers::SHIFT,
    )));
    assert!(screen(&mut app, &mut terminal).contains(" ask "));
    app.on_event(Event::Key(KeyEvent::new(
        KeyCode::BackTab,
        KeyModifiers::SHIFT,
    )));
    assert!(screen(&mut app, &mut terminal).contains(" accept-edits "));
    app.command("/permissions auto");
    assert_eq!(
        app.permissions.lock().unwrap().mode,
        permissions::Mode::Auto
    );
    app.command("/permissions");
    assert!(
        matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("Permission mode: auto"))
    );
}

#[test]
fn status_shows_where_inference_comes_from_even_with_a_long_cwd() {
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let mut workspace = Workspace::current().unwrap();
    workspace.cwd = std::path::PathBuf::from(format!("/{}", "deep/".repeat(40)));
    let (mut app, _rx) = App::new_with_channel(client, test_settings("test-model"), workspace);
    app.settings.hub.key = Some(tamale_hub_protocol::encode_key(&[1; 32]));
    let backend = ratatui::backend::TestBackend::new(80, 12);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buffer = terminal.backend().buffer();
    let row: String = (0..80)
        .map(|x| buffer[(x, 10)].symbol().to_string())
        .collect();
    assert!(row.starts_with(" ≡ Sessions  ⇄ Remote off │ ◉ Web "), "the tabs stay: {row}");
    assert!(row.contains("…"), "{row}");
    assert!(row.ends_with(" · via Anthropic API "), "{row}");

    // Too narrow for all of it: the most used tabs and the keys' hint are what's left.
    let backend = ratatui::backend::TestBackend::new(34, 12);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y| (0..34).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>();
    assert!(row(10).starts_with(" ≡ Sessions  ⇄ Remote off"), "{}", row(10));
    assert!(row(11).ends_with("shift+tab mode · ↓ tabs · /help "), "{}", row(11));

    // Stats give way whole, not cut off by the hint, and the cost outlasts the usage.
    let backend = ratatui::backend::TestBackend::new(100, 12);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = (0..100).map(|x| buffer[(x, 11)].symbol().to_string()).collect::<String>();
    assert!(row.contains("· $0.00 "), "{row}");
    assert!(!row.contains("↑0"), "{row}");
}

fn login(email: &str, dir: Option<&str>) -> Backend {
    let mut cc = backend::ClaudeCode::signed_out("claude".into(), dir.map(Into::into));
    cc.account = serde_json::from_value(serde_json::json!({
        "loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "max",
        "email": email, "orgId": format!("org-{email}"),
    }))
    .unwrap();
    Backend::ClaudeCode(cc)
}

fn codex_login(email: &str, dir: Option<&str>) -> Backend {
    let mut codex = backend::Codex::signed_out("codex".into(), dir.map(Into::into));
    codex.account = backend::codex::Account {
        logged_in: true,
        auth_method: Some("chatgpt".into()),
        plan: Some("plus".into()),
        email: Some(email.into()),
        account_id: Some(format!("acct-{email}")),
    };
    Backend::Codex(codex)
}

#[tokio::test]
async fn backend_switches_keep_the_conversation_and_drop_the_old_thread() {
    use backend::Kind;
    use backends::{Choice, LoginRequest};
    let client = anthropic::Client::new("test-key").unwrap();
    let (mut app, _rx) = App::new_with_channel(
        Backend::Api(client.clone()),
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let personal = login("me@home.dev", None);
    let work = login("me@work.dev", Some("/tmp/tamale-test-work"));

    // The picker opens on the current backend while logins are checked.
    app.command("/backend");
    assert!(app.backend_picker.as_ref().unwrap().loading);
    let adds = [
        Choice::AddLogin(Kind::ClaudeCode),
        Choice::AddLogin(Kind::Codex),
    ];
    assert_eq!(app.backend_choices(), [&[Choice::Api][..], &adds].concat());
    app.on_app_event(AppEvent::Logins(
        vec![personal.clone(), work.clone()],
        vec![],
    ));
    let picker = app.backend_picker.as_ref().unwrap();
    assert!(!picker.loading);
    assert_eq!(picker.selected, 0);
    assert_eq!(
        app.backend_choices(),
        [
            &[Choice::Api, Choice::Login(0), Choice::Login(1)][..],
            &adds
        ]
        .concat()
    );
    app.on_key(key(KeyCode::Char('x')));
    assert!(app.input.is_empty(), "keys don't reach the prompt");
    app.on_key(key(KeyCode::End));
    app.on_key(key(KeyCode::Down));
    assert_eq!(
        app.backend_picker.as_ref().unwrap().selected,
        4,
        "stops at the last row"
    );

    // Each login is its own backend; the footer names the account.
    app.history = vec![Message::user("earlier")];
    app.thread = Some("old-session".into());
    app.models = Some(vec![]);
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Enter));
    assert!(app.backend_picker.is_none());
    assert!(app.is_current(Choice::Login(0)));
    assert!(app.thread.is_none() && app.models.is_none());
    assert_eq!(app.history.len(), 1);
    assert_eq!(app.backend_label(), "Claude Code · Max · me@home.dev");
    assert!(
        matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("conversation continues"))
    );

    // Logins tamale didn't add can't be removed from the picker.
    app.command("/backend");
    app.on_app_event(AppEvent::Logins(
        vec![personal.clone(), work.clone()],
        vec![],
    ));
    assert_eq!(app.backend_picker.as_ref().unwrap().selected, 1);
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Char('d')));
    assert!(
        matches!(app.entries.last(), Some(Entry::Error(e)) if e.contains("claude auth logout"))
    );
    assert_eq!(app.logins.len(), 2);
    app.on_key(key(KeyCode::Char('3')));
    assert!(app.is_current(Choice::Login(1)));

    // Adding an account that's already known switches to it instead of duplicating it.
    let request = LoginRequest {
        login: login("me@home.dev", Some("/tmp/tamale-test-new")),
        adding: true,
    };
    app.on_login(request, Ok(()));
    assert!(
        matches!(&app.entries[app.entries.len() - 2], Entry::Info(t) if t.contains("already signed in"))
    );
    assert!(app.is_current(Choice::Login(0)));
    assert_eq!(app.logins.len(), 2);

    // A new account is added and used.
    let request = LoginRequest {
        login: login("me@side.dev", Some("/tmp/tamale-test-side")),
        adding: true,
    };
    app.on_login(request, Ok(()));
    assert_eq!(app.logins.len(), 3);
    assert!(app.is_current(Choice::Login(2)));
    assert!(app.take_login_request().is_none());

    // Codex logins are listed after Claude Code's; the same email on another CLI is
    // another account. Listing one CLI keeps the other's logins.
    let codex = codex_login("me@home.dev", None);
    app.command("/backend");
    app.on_app_event(AppEvent::Logins(
        vec![codex.clone()],
        vec!["Claude Code: boom".into()],
    ));
    assert!(
        matches!(&app.entries[app.entries.len() - 1], Entry::Error(e) if e.contains("Claude Code: boom"))
    );
    assert_eq!(app.logins.len(), 4);
    let choices = app.backend_choices();
    assert_eq!(choices[4], Choice::Login(3));
    app.on_key(key(KeyCode::Char('5')));
    assert!(app.is_current(Choice::Login(3)));
    assert_eq!(app.settings.backend.kind, Kind::Codex);
    assert_eq!(app.backend_label(), "Codex · Plus");
    app.command("/backend codex");
    assert!(matches!(app.entries.last(), Some(Entry::Info(t)) if t.starts_with("Already using")));
    app.command("/backend");
    app.on_key(key(KeyCode::Char('d')));
    assert!(matches!(app.entries.last(), Some(Entry::Error(e)) if e.contains("codex logout")));
    app.on_key(key(KeyCode::Esc));
    app.command("/backend");
    app.on_key(key(KeyCode::Char('4')));
    assert!(app.is_current(Choice::Login(2)));

    // Direct switches.
    app.command("/backend gpt");
    assert!(matches!(app.entries.last(), Some(Entry::Error(_))));
    app.on_app_event(AppEvent::Backend(
        Err("ANTHROPIC_API_KEY is not set".into()),
    ));
    assert!(matches!(app.entries.last(), Some(Entry::Error(e)) if e.contains("Couldn't switch")));
    assert!(app.is_current(Choice::Login(2)));
    app.on_app_event(AppEvent::Backend(Ok(Backend::Api(client))));
    assert_eq!(app.settings.backend.kind, backend::Kind::Api);
    assert_eq!(app.backend_label(), "Anthropic API");
    app.command("/backend api");
    assert!(matches!(app.entries.last(), Some(Entry::Info(t)) if t.starts_with("Already using")));
}

#[tokio::test]
async fn without_a_backend_prompts_open_the_picker_and_stay_in_the_input() {
    use backend::Kind;
    use backends::Choice;

    let (mut app, _rx) = App::new_with_channel(
        Backend::Unset,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    assert!(app.logins.is_empty());
    assert_eq!(app.backend_label(), "no backend");
    assert!(!app.is_current(Choice::Api));

    app.ask_for_backend();
    assert!(matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("Choose a backend")));
    assert!(app.backend_picker.is_some());
    app.on_event(key(KeyCode::Esc));
    assert!(app.backend_picker.is_none());

    // A prompt doesn't start a turn or join the history; it waits in the input.
    type_text(&mut app, "hello");
    app.on_event(key(KeyCode::Enter));
    assert!(!app.is_running());
    assert!(app.history.is_empty());
    assert_eq!(app.input, "hello");
    assert!(app.backend_picker.is_some());
    app.on_event(key(KeyCode::Esc));

    // Neither does a goal, and none is left behind.
    app.input.clear();
    app.cursor = 0;
    app.command("/goal ship it");
    assert!(app.goal.is_none());
    assert!(!app.is_running());
    assert_eq!(app.input, "/goal ship it");
    app.on_event(key(KeyCode::Esc));

    // The API row is a real switch, not "already using".
    let before = app.entries.len();
    app.command("/backend api");
    assert_eq!(app.entries.len(), before);

    // Choosing a backend makes it current.
    let client = anthropic::Client::new("test-key").unwrap();
    app.on_app_event(AppEvent::Backend(Ok(Backend::Api(client))));
    assert!(app.is_current(Choice::Api));
    assert_eq!(app.settings.backend.kind, Kind::Api);
    assert_eq!(app.backend_label(), "Anthropic API");
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn type_text(app: &mut App, text: &str) {
    app.on_event(Event::Paste(text.into()));
}

#[test]
fn up_and_down_move_between_lines_then_recall_prompts() {
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    app.prompts.add("first");
    app.prompts.add("multi\nline");
    type_text(&mut app, "draft");

    app.on_event(key(KeyCode::Up));
    assert_eq!(app.input, "multi\nline");
    assert_eq!(app.cursor, 0);
    app.on_event(key(KeyCode::Up));
    assert_eq!(app.input, "first");
    app.on_event(key(KeyCode::Up));
    assert_eq!(app.input, "first", "stays at the oldest prompt");
    app.on_event(key(KeyCode::Down));
    assert_eq!(app.input, "multi\nline");
    assert_eq!(app.cursor, app.input.len());
    // Inside a multi-line prompt, ↑ moves up a line (keeping the column) before recalling.
    app.on_event(key(KeyCode::Up));
    assert_eq!(app.input, "multi\nline");
    assert_eq!(app.cursor, "line".len());
    app.on_event(key(KeyCode::Down));
    assert_eq!(app.cursor, "multi\nline".len());
    app.on_event(key(KeyCode::Down));
    assert_eq!(app.input, "draft", "the unsent draft comes back");
}

#[tokio::test]
async fn messages_typed_during_a_turn_are_queued_delivered_and_restored() {
    let _env = SESSIONS_ENV.lock().await;
    let root = std::env::temp_dir().join(format!("tamale-steer-test-{}", session::new_id()));
    unsafe { std::env::set_var("TAMALE_SESSIONS_DIR", root.join("sessions")) };
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    let _ = app.begin_turn("do the thing".into());
    app.task = Some(tokio::spawn(std::future::pending()));

    type_text(&mut app, "also check the docs");
    app.on_event(key(KeyCode::Enter));
    assert!(app.input.is_empty());
    assert_eq!(app.queued_messages(), ["also check the docs"]);
    assert!(screen(&mut app).contains("queued also check the docs"));

    // The turn takes the queue with the next tool result (what `TurnCtx::call_tool` does).
    let mut output = "tool output".to_string();
    let delivered = std::mem::take(&mut *app.queued.lock().unwrap());
    agent::deliver_steering(&mut output, &delivered);
    app.on_agent(
        app.turn,
        AgentEvent::ToolCall {
            name: "bash".into(),
            summary: "ls".into(),
            description: "List files".into(),
        },
    );
    app.on_agent(app.turn, AgentEvent::Steered(delivered));
    app.on_agent(
        app.turn,
        AgentEvent::ToolResult {
            output,
            is_error: false,
        },
    );
    assert!(matches!(&app.entries[app.entries.len() - 2],
        Entry::Tool { output: Some((out, false)), .. } if out == "tool output"));
    assert!(matches!(app.entries.last(), Some(Entry::User(t)) if t == "also check the docs"));
    assert_eq!(
        app.checkpoints.last().unwrap().prompt,
        "do the thing\n\nalso check the docs"
    );
    assert!(!screen(&mut app).contains("queued"));

    // Undelivered messages go back to the input box on interrupt.
    type_text(&mut app, "never mind");
    app.on_event(key(KeyCode::Enter));
    type_text(&mut app, "half-typed");
    app.on_event(key(KeyCode::Esc));
    assert!(!app.is_running());
    assert!(app.queued_messages().is_empty());
    assert_eq!(app.input, "never mind\n\nhalf-typed");
    app.finish_saves().await;
    unsafe { std::env::remove_var("TAMALE_SESSIONS_DIR") };
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn messages_left_in_the_queue_are_sent_when_the_turn_ends() {
    let _env = SESSIONS_ENV.lock().await;
    let root = std::env::temp_dir().join(format!("tamale-queue-test-{}", session::new_id()));
    unsafe { std::env::set_var("TAMALE_SESSIONS_DIR", root.join("sessions")) };
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    let _ = app.begin_turn("first".into());
    app.task = Some(tokio::spawn(std::future::pending()));
    app.queued.lock().unwrap().push("second".into());
    app.queued.lock().unwrap().push("third".into());
    app.on_agent(app.turn, AgentEvent::Done);

    assert!(app.is_running(), "the queued messages start the next turn");
    assert!(app.queued_messages().is_empty());
    assert!(matches!(app.entries.last(), Some(Entry::User(t)) if t == "second\n\nthird"));
    assert_eq!(app.checkpoints.len(), 2);
    app.task.take().unwrap().abort();
    app.finish_saves().await;
    unsafe { std::env::remove_var("TAMALE_SESSIONS_DIR") };
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn goals_continue_until_the_checker_confirms_them() {
    let _env = SESSIONS_ENV.lock().await;
    let root = std::env::temp_dir().join(format!("tamale-goal-test-{}", session::new_id()));
    unsafe { std::env::set_var("TAMALE_SESSIONS_DIR", root.join("sessions")) };
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    let reply = |app: &mut App, text: &str| {
        app.on_agent(app.turn, AgentEvent::TextDelta(text.into()));
        app.on_agent(app.turn, AgentEvent::Done);
    };
    let checked = |app: &mut App, answer: &str| {
        app.on_agent(
            app.turn,
            AgentEvent::GoalChecked {
                answer: answer.into(),
                failed: false,
            },
        );
    };

    app.command("/goal all tests pass");
    assert!(app.is_running());
    assert!(matches!(app.entries.last(), Some(Entry::User(t)) if t == "/goal all tests pass"));
    assert!(format!("{:?}", app.history).contains("Work toward the goal above"));
    assert!(screen(&mut app).contains("goal 1/20 all tests pass"));

    // The end of a turn starts the checker, which doesn't add to the conversation.
    reply(&mut app, "All tests pass now.");
    assert!(app.is_running());
    assert!(app.goal.as_ref().unwrap().checking);
    assert_eq!(app.checkpoints.len(), 1);
    assert!(screen(&mut app).contains("goal checking 1/20"));
    // Its steps show in the transcript.
    app.on_agent(
        app.turn,
        AgentEvent::ToolCall {
            name: "bash".into(),
            summary: "cargo test".into(),
            description: "↳ Run the tests".into(),
        },
    );

    // Not met: the agent continues with the checker's findings.
    checked(&mut app, "`parse` still fails.\n[goal not met]");
    assert!(app.is_running());
    assert_eq!(app.checkpoints.len(), 2);
    assert!(matches!(app.entries.last(), Some(Entry::User(t))
        if t.contains("turn 2 of 20") && t.contains("`parse` still fails.")));
    assert!(format!("{:?}", app.history).contains("`parse` still fails."));

    // Blocked pauses without checking; resume continues.
    reply(&mut app, "Which database should I use?\n[goal blocked]");
    assert!(!app.is_running());
    assert!(app.goal.as_ref().unwrap().paused);
    app.command("/goal resume");
    assert!(app.is_running());

    // An interrupt pauses too, also while checking.
    reply(&mut app, "Done.");
    assert!(app.goal.as_ref().unwrap().checking);
    app.interrupt();
    let goal = app.goal.as_ref().unwrap();
    assert!(goal.paused && !goal.checking);
    app.command("/goal resume");

    reply(&mut app, "All 42 tests pass.");
    checked(&mut app, "Ran cargo test: 42 passed.\n\n**[goal met]**");
    assert!(!app.is_running());
    assert!(app.goal.is_none());
    let n = app.entries.len();
    assert!(matches!(&app.entries[n - 2], Entry::Info(t) if t.starts_with("✓ Goal achieved")));
    assert!(matches!(&app.entries[n - 1], Entry::Note(t) if t.contains("42 passed")));
    assert!(!screen(&mut app).contains("◎ goal"));

    // Errors pause instead of looping, from the agent or the checker.
    app.command("/goal ship it");
    app.on_agent(app.turn, AgentEvent::Error("overloaded".into()));
    app.on_agent(app.turn, AgentEvent::Done);
    assert!(!app.is_running());
    assert!(app.goal.as_ref().unwrap().paused);
    app.command("/goal resume");
    reply(&mut app, "Shipped.");
    app.on_agent(
        app.turn,
        AgentEvent::GoalChecked {
            answer: "Error: the subagent failed: overloaded".into(),
            failed: true,
        },
    );
    assert!(!app.is_running());
    assert!(app.goal.as_ref().unwrap().paused);
    app.command("/goal clear");
    assert!(app.goal.is_none());

    app.finish_saves().await;
    unsafe { std::env::remove_var("TAMALE_SESSIONS_DIR") };
    let _ = std::fs::remove_dir_all(root);
}

fn screen(app: &mut App) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn screen_lines(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

#[tokio::test]
async fn diff_viewer_browses_files_hunks_and_follows_edits() {
    let work = std::env::temp_dir().join(format!("tamale-diff-view-{}", session::new_id()));
    std::fs::create_dir_all(work.join("src/app")).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    git(&["init", "-q"]);
    let body: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    std::fs::write(work.join("src/app/view.rs"), &body).unwrap();
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    std::fs::write(
        work.join("src/app/view.rs"),
        body.replace("line 3\n", "line three\n")
            .replace("line 30\n", "line 30\nline 30.5\n"),
    )
    .unwrap();
    std::fs::write(work.join("notes.txt"), "new\n").unwrap();

    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let workspace = Workspace {
        cwd: work.clone(),
        additional_dirs: Vec::new(),
    };
    let (mut app, mut rx) = App::new_with_channel(client, test_settings("test-model"), workspace);
    app.command("/diff");
    assert!(app.is_busy());
    let event = rx.recv().await.unwrap();
    app.on_app_event(event);
    assert!(!app.is_busy());

    let lines = screen_lines(&mut app, 110, 24);
    let text = lines.join("\n");
    println!("{text}");
    assert!(text.contains("working tree vs HEAD · 2 files +3 −1"), "{text}");
    assert!(text.contains("src/app/"), "directory chains are merged:\n{text}");
    assert!(text.contains("U notes.txt"), "{text}");
    // The first file in tree order (directories first) is shown.
    assert!(text.contains("view.rs modified"), "{text}");
    assert!(text.contains("@@ -1,6 +1,6 @@"), "{text}");
    assert!(text.contains("  3     - line 3"), "{text}");
    assert!(text.contains("      3 + line three"), "{text}");
    assert!(text.contains(" 2 hunks "), "{text}");
    // Changed lines are tinted across the pane, not just under their text.
    let row = lines.iter().position(|l| l.contains("+ line three")).unwrap() as u16;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert_eq!(terminal.backend().buffer()[(100, row)].bg, diff::ADDED_BG);

    // In a short pane, n walks the hunks and then moves on to the next file.
    let small = screen_lines(&mut app, 110, 12).join("\n");
    assert!(small.contains("hunk 1/2"), "{small}");
    app.on_event(key(KeyCode::Char('n')));
    assert!(screen_lines(&mut app, 110, 12).join("\n").contains("hunk 2/2"));
    app.on_event(key(KeyCode::Char('n')));
    let text = screen_lines(&mut app, 110, 24).join("\n");
    assert!(text.contains("notes.txt untracked"), "{text}");
    assert!(text.contains("+ new"), "{text}");

    // Collapsing a directory hides its files but keeps the shown diff.
    app.on_event(key(KeyCode::Home));
    app.on_event(key(KeyCode::Left));
    let text = screen_lines(&mut app, 110, 24).join("\n");
    assert!(text.contains("▸ src/app/") && !text.contains("M view.rs"), "{text}");

    // Edits show up after a refresh, and the selected file stays selected.
    std::fs::write(work.join("README.md"), "hello\nworld\n").unwrap();
    app.on_event(key(KeyCode::Char('r')));
    let event = rx.recv().await.unwrap();
    app.on_app_event(event);
    let text = screen_lines(&mut app, 110, 24).join("\n");
    assert!(text.contains("3 files +4 −1"), "{text}");
    assert!(text.contains("notes.txt untracked"), "{text}");

    app.on_event(key(KeyCode::Char('q')));
    assert!(!app.diff_open);
    let _ = std::fs::remove_dir_all(&work);
}

#[tokio::test]
async fn diff_outside_a_repository_reports_an_error() {
    let work = std::env::temp_dir().join(format!("tamale-no-repo-{}", session::new_id()));
    std::fs::create_dir_all(&work).unwrap();
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let workspace = Workspace {
        cwd: work.clone(),
        additional_dirs: Vec::new(),
    };
    let (mut app, mut rx) = App::new_with_channel(client, test_settings("test-model"), workspace);
    app.command("/diff");
    let event = rx.recv().await.unwrap();
    app.on_app_event(event);
    assert!(!app.diff_open);
    assert!(matches!(app.entries.last(), Some(Entry::Error(e)) if e.contains("not inside a git repository")));
    // Without a repository there is no Diff tab.
    assert!(app.tab_spans().is_none());
    let _ = std::fs::remove_dir_all(&work);
}

#[tokio::test]
async fn arrow_keys_switch_between_chat_and_diff_tabs() {
    let work = std::env::temp_dir().join(format!("tamale-diff-tabs-{}", session::new_id()));
    std::fs::create_dir_all(work.join("src")).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(work.join("top.txt"), "one\n").unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\n").unwrap();

    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let workspace = Workspace {
        cwd: work.clone(),
        additional_dirs: Vec::new(),
    };
    let (mut app, mut rx) = App::new_with_channel(client, test_settings("test-model"), workspace);
    app.preload_diff();
    let event = rx.recv().await.unwrap();
    app.on_app_event(event);
    assert!(!app.diff_open, "preloading stays on the chat");
    let footer = screen_lines(&mut app, 100, 30)[28].clone();
    assert!(footer.contains(" ◉ Web  ± Diff 2 +2 −0 "), "{footer}");

    // In the prompt → only moves the cursor, even when empty; from the tabs it reaches Diff.
    type_text(&mut app, "hi");
    app.on_event(key(KeyCode::Left));
    app.on_event(key(KeyCode::Right));
    assert!(!app.diff_open);
    assert_eq!(app.cursor, 2);
    app.input.clear();
    app.cursor = 0;
    app.on_event(key(KeyCode::Right));
    assert!(!app.diff_open);
    app.on_event(key(KeyCode::Down));
    // → highlights Diff, last of the tabs, without opening it; Enter opens it.
    for _ in 0..4 {
        app.on_event(key(KeyCode::Right));
    }
    assert_eq!(app.tab_focus, Some(super::FooterTab::Diff));
    assert!(!app.diff_open);
    app.on_event(key(KeyCode::Enter));
    assert!(app.diff_open);
    let event = rx.recv().await.unwrap();
    app.on_app_event(event);
    let text = screen_lines(&mut app, 100, 30).join("\n");
    assert!(text.contains(" ←  Chat  Diff "), "{text}");

    // ← walks out of the diff pane, up the tree, then back to the chat.
    app.on_event(key(KeyCode::Tab));
    app.on_event(key(KeyCode::Left));
    app.on_event(key(KeyCode::Left));
    assert!(app.diff_open, "first ← selects the parent directory");
    app.on_event(key(KeyCode::Left));
    assert!(app.diff_open, "then folds it");
    app.on_event(key(KeyCode::Left));
    assert!(!app.diff_open);
    // Back on the chat Diff stays highlighted, so Enter reopens it with the selection kept.
    assert_eq!(app.tab_focus, Some(super::FooterTab::Diff));
    app.on_event(key(KeyCode::Enter));
    let event = rx.recv().await.unwrap();
    app.on_app_event(event);
    let text = screen_lines(&mut app, 100, 30).join("\n");
    assert!(text.contains("▸ src/"), "{text}");
    let _ = std::fs::remove_dir_all(&work);
}

#[tokio::test]
async fn diff_viewer_expands_unchanged_lines() {
    let work = std::env::temp_dir().join(format!("tamale-diff-expand-{}", session::new_id()));
    std::fs::create_dir_all(&work).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    git(&["init", "-q"]);
    let mut body: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    body.insert_str(0, "fn main() {\n");
    std::fs::write(work.join("a.rs"), &body).unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    std::fs::write(
        work.join("a.rs"),
        body.replace("line 3\n", "line three\n")
            .replace("line 35\n", "line thirty-five\n"),
    )
    .unwrap();

    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let workspace = Workspace {
        cwd: work.clone(),
        additional_dirs: Vec::new(),
    };
    let (mut app, mut rx) = App::new_with_channel(client, test_settings("test-model"), workspace);
    app.command("/diff");
    let event = rx.recv().await.unwrap();
    app.on_app_event(event);
    app.on_event(key(KeyCode::Tab));
    // n moves the cursor to the next hunk, whose row holds the gap above it.
    app.on_event(key(KeyCode::Char('n')));
    let text = screen_lines(&mut app, 110, 60).join("\n");
    // Lines 8–32 are hidden between the changes, 40–41 after them.
    assert!(
        text.contains("↕ 25 unchanged lines  @@ -33,7 +33,7 @@ line 31  ⏎ expand"),
        "{text}"
    );
    assert!(text.contains("↕ 2 unchanged lines"), "{text}");
    assert!(!text.contains("line 17"), "{text}");

    // A large gap opens next to the changes on both sides first.
    app.on_event(key(KeyCode::Enter));
    let text = screen_lines(&mut app, 110, 60).join("\n");
    assert!(text.contains("line 16") && text.contains("line 22"), "{text}");
    assert!(text.contains("↕ 5 unchanged lines  @@") && !text.contains("line 19"), "{text}");
    app.on_event(key(KeyCode::Char('n')));
    app.on_event(key(KeyCode::Enter));
    let text = screen_lines(&mut app, 110, 60).join("\n");
    assert!(text.contains(" 20  20   line 19"), "{text}");
    assert!(text.contains("↕ 2 unchanged lines"), "{text}");
    // The cursor moves onto a gap with j/k too.
    app.on_event(key(KeyCode::Char('G')));
    let text = screen_lines(&mut app, 110, 60).join("\n");
    assert!(text.contains("↕ 2 unchanged lines  ⏎ expand"), "{text}");

    // Expansions survive a reload; E toggles the whole file and back.
    app.on_event(key(KeyCode::Char('r')));
    let event = rx.recv().await.unwrap();
    app.on_app_event(event);
    assert!(screen_lines(&mut app, 110, 60).join("\n").contains("line 17"));
    app.on_event(key(KeyCode::Char('E')));
    let text = screen_lines(&mut app, 110, 60).join("\n");
    assert!(text.contains("line 17") && !text.contains("↕"), "{text}");
    app.on_event(key(KeyCode::Char('E')));
    let text = screen_lines(&mut app, 110, 60).join("\n");
    assert!(!text.contains("line 17") && text.contains("↕ 25 unchanged lines"), "{text}");
    let _ = std::fs::remove_dir_all(&work);
}

#[tokio::test]
async fn visual_selection_in_the_diff_quotes_lines_into_the_prompt() {
    let work = std::env::temp_dir().join(format!("tamale-diff-visual-{}", session::new_id()));
    std::fs::create_dir_all(&work).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(work.join("a.rs"), "one\ntwo\nthree\nfour\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    std::fs::write(work.join("a.rs"), "one\n2\nthree\nfour\n").unwrap();

    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let workspace = Workspace {
        cwd: work.clone(),
        additional_dirs: Vec::new(),
    };
    let (mut app, mut rx) = App::new_with_channel(client, test_settings("test-model"), workspace);
    app.command("/diff");
    let event = rx.recv().await.unwrap();
    app.on_app_event(event);
    app.on_event(key(KeyCode::Tab));
    // Rows: header, one, -two, +2, three, four. Select from "one" through "+2".
    app.on_event(key(KeyCode::Char('j')));
    app.on_event(key(KeyCode::Char('V')));
    app.on_event(key(KeyCode::Char('j')));
    app.on_event(key(KeyCode::Char('j')));
    let text = screen_lines(&mut app, 100, 30).join("\n");
    assert!(text.contains("-- VISUAL LINE --"), "{text}");
    assert!(text.contains("3 rows selected"), "{text}");
    // Esc leaves visual mode rather than the tab.
    app.on_event(key(KeyCode::Esc));
    assert!(app.diff_open);
    assert!(!screen_lines(&mut app, 100, 30).join("\n").contains("VISUAL"));

    app.on_event(key(KeyCode::Char('v')));
    app.on_event(key(KeyCode::Char('k')));
    app.on_event(key(KeyCode::Char('k')));
    app.input = "Context first".into();
    app.on_event(key(KeyCode::Enter));
    assert!(!app.diff_open, "asking returns to the chat");
    assert_eq!(
        app.input,
        "Context first\n\nIn `a.rs` (lines 1–2):\n```diff\n one\n-two\n+2\n```\n\n"
    );
    assert_eq!(app.cursor, app.input.len());
    assert!(app.history.is_empty(), "nothing is sent until you add a question");
    let _ = std::fs::remove_dir_all(&work);
}

#[tokio::test]
async fn web_sessions_ask_for_artifacts_but_show_the_prompt_as_typed() {
    let _env = SESSIONS_ENV.lock().await;
    let root = std::env::temp_dir().join(format!("tamale-web-test-{}", session::new_id()));
    unsafe { std::env::set_var("TAMALE_SESSIONS_DIR", root.join("sessions")) };
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    let last_prompt = |app: &App| match app.history.last() {
        Some(Message::User { content }) => format!("{:?}", content.iter().last()),
        _ => String::new(),
    };

    app.set_web_session(true);
    app.web_send("how does sync work?".into());
    assert!(matches!(app.entries.last(), Some(Entry::User(t)) if t == "how does sync work?"));
    assert!(last_prompt(&app).contains("html artifact"));
    app.task.take().unwrap().abort();
    app.on_agent(app.turn, AgentEvent::Done);

    app.set_web_session(false);
    app.web_send("and now?".into());
    assert!(!last_prompt(&app).contains("html artifact"));
    app.task.take().unwrap().abort();
    app.on_agent(app.turn, AgentEvent::Done);

    // Back in the TUI with a phone still watching through the hub, it gets artifacts too.
    app.set_hub_viewers(Some(1));
    app.set_web_viewers(1);
    app.web_send("from the phone".into());
    assert!(last_prompt(&app).contains("html artifact"));
    app.task.take().unwrap().abort();
    app.on_agent(app.turn, AgentEvent::Done);

    // Another terminal driving it gets ordinary replies, even with the phone there.
    app.terminal_send("from a terminal".into());
    assert!(!last_prompt(&app).contains("html artifact"));
    app.task.take().unwrap().abort();
    app.on_agent(app.turn, AgentEvent::Done);

    // And the phone gets its artifacts back with its next prompt.
    app.web_send("from the phone again".into());
    assert!(last_prompt(&app).contains("html artifact"));
    app.task.take().unwrap().abort();
    app.finish_saves().await;
    unsafe { std::env::remove_var("TAMALE_SESSIONS_DIR") };
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_reply_cut_off_inside_a_code_block_continues_in_the_same_entry() {
    let (mut app, _rx) = App::new_with_channel(
        Backend::Api(anthropic::Client::new("test-key").unwrap()),
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    app.task = Some(tokio::spawn(std::future::pending()));
    let delta =
        |app: &mut App, text: &str| app.on_agent(app.turn, AgentEvent::TextDelta(text.into()));
    delta(
        &mut app,
        "Here.\n\n```html artifact\n<title>T</title>\n<p>he",
    );
    app.on_agent(app.turn, AgentEvent::Continuing("Continuing…".into()));
    delta(&mut app, "llo</p>\n```");
    app.on_agent(
        app.turn,
        AgentEvent::Retry {
            attempt: 1,
            delay: std::time::Duration::ZERO,
            error: "timeout".into(),
        },
    );
    delta(&mut app, "llo</p>\n```\n");
    let replies: Vec<&String> = app
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::Assistant(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(
        replies,
        ["Here.\n\n```html artifact\n<title>T</title>\n<p>hello</p>\n```\n"]
    );

    // Cut off outside a code block, the continuation is a reply of its own.
    app.on_agent(app.turn, AgentEvent::Continuing("Continuing…".into()));
    assert!(matches!(app.entries.last(), Some(Entry::Info(_))));
}

#[tokio::test]
async fn ctrl_c_detaches_a_running_turn_only_on_a_server() {
    let ctrl_c = || Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    app.set_detachable();

    // Running: leaves the turn going.
    app.task = Some(tokio::spawn(std::future::pending()));
    app.on_event(ctrl_c());
    assert!(app.take_detach_request());
    assert!(!app.take_detach_request());
    assert!(app.is_running() && !app.should_quit);

    // Also from an approval prompt, which stays up for whoever comes back.
    let (reply, _answer) = tokio::sync::oneshot::channel();
    app.approval = Some(approval::Prompt::new(
        "bash".into(),
        "cargo test".into(),
        "Run tests".into(),
        vec![],
        reply,
    ));
    app.on_event(ctrl_c());
    assert!(app.take_detach_request());
    assert!(app.approval.is_some() && app.is_running());
    app.approval = None;
    app.task.take().unwrap().abort();

    // Idle: clears the input, then quits, since nothing would be left running.
    app.input = "draft".into();
    app.on_event(ctrl_c());
    assert!(app.input.is_empty() && !app.take_detach_request() && !app.should_quit);
    app.on_event(ctrl_c());
    assert!(app.should_quit && !app.take_detach_request());
}

#[tokio::test]
async fn ctrl_c_interrupts_a_running_turn_in_the_foreground() {
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    app.task = Some(tokio::spawn(std::future::pending()));
    app.on_event(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
    assert!(!app.is_running());
    assert!(!app.take_detach_request() && !app.should_quit);

    type_text(&mut app, "/detach");
    app.on_event(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    assert!(!app.take_detach_request());
    assert!(matches!(app.entries.last(), Some(Entry::Error(_))));
}

#[test]
fn startup_mentions_only_sessions_there_are_to_go_back_to() {
    use crate::background::{State, Status};
    let status = |id, cwd: &str, title: &str, attached| Status {
        id,
        cwd: cwd.into(),
        session_id: format!("s{id}"),
        title: title.into(),
        state: State::Idle,
        attached,
        since: 0,
        sid: String::new(),
        chef: false,
    };
    let note = |sessions: &[Status]| {
        background::open_sessions_note(sessions, std::path::Path::new("/project"))
    };
    let elsewhere = status(1, "/other", "elsewhere", false);
    let attached = status(2, "/project", "attached", true);
    let empty = status(3, "/project", "", false);
    assert_eq!(note(&[elsewhere, attached, empty]), None);

    let waiting = status(4, "/project", "fix the bug", false);
    let one = note(std::slice::from_ref(&waiting)).unwrap();
    assert!(one.contains("(idle)") && one.contains("fix the bug") && one.contains("/sessions"));

    let many = note(&[waiting, status(5, "/project", "another", false)]).unwrap();
    assert!(many.starts_with("2 other sessions") && many.contains("/sessions"));
}

#[tokio::test]
async fn the_startup_note_stays_out_of_saves_but_rewinding_still_lines_up() {
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    let before = app.entries.len();
    app.entries.push(Entry::Info("1 open".into()));
    app.open_sessions_note = Some("1 open".into());
    app.begin_turn_as("hi".into(), "hi".into());
    let (entries, checkpoints) = app.entries_to_save();
    assert_eq!(entries.len(), app.entries.len() - 1);
    assert!(!entries.iter().any(|e| matches!(e, Entry::Info(t) if t == "1 open")));
    assert_eq!(checkpoints[0].entries_len, app.checkpoints[0].entries_len - 1);
    assert!(before <= checkpoints[0].entries_len);
}

#[tokio::test]
async fn sessions_picker_joins_stops_and_keeps_its_place() {
    use crate::background::{State, Status};
    let status = |id, state| Status {
        id,
        cwd: "/project".into(),
        session_id: format!("s{id}"),
        title: format!("title {id}"),
        state,
        attached: false,
        since: 0,
        sid: format!("sid{id}"),
        chef: false,
    };
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    app.set_detachable();
    let open = |app: &mut App| {
        app.sessions_picker = Some(background::SessionsPicker {
            sessions: vec![status(1, State::Running), status(2, State::Idle)],
            ..Default::default()
        });
    };

    // A session mid-turn takes a second `x`, and anything else calls that off. The stop
    // itself goes to a session that isn't there, which only reports back.
    open(&mut app);
    app.on_event(key(KeyCode::Char('x')));
    let picker = app.sessions_picker.as_ref().unwrap();
    assert_eq!(picker.confirming, Some(background::Row::Local(1)));
    assert!(picker.stopping.is_empty());
    app.on_event(key(KeyCode::Down));
    app.on_event(key(KeyCode::Up));
    let picker = app.sessions_picker.as_ref().unwrap();
    assert_eq!(picker.confirming, None);
    app.on_event(key(KeyCode::Char('x')));
    app.on_event(key(KeyCode::Char('x')));
    assert_eq!(app.sessions_picker.as_ref().unwrap().stopping, [1]);

    // An idle one goes at the first.
    app.on_event(key(KeyCode::Down));
    app.on_event(key(KeyCode::Char('x')));
    assert_eq!(app.sessions_picker.as_ref().unwrap().stopping, [1, 2]);

    // The selection follows its session through a refresh, and a gone one is forgotten.
    let picker = app.sessions_picker.as_mut().unwrap();
    picker.update(vec![status(3, State::Idle), status(2, State::Idle)]);
    assert_eq!(picker.selected, 1);
    assert_eq!(picker.stopping, [2]);
    picker.update(vec![]);
    assert_eq!(picker.selected, 0);
    let empty = screen(&mut app);
    assert!(empty.contains("No other sessions are open"), "{empty}");

    // Enter closes the picker and asks to go there.
    open(&mut app);
    app.on_event(key(KeyCode::Down));
    let screen_text = screen(&mut app);
    assert!(screen_text.contains("title 2"), "{screen_text}");
    app.on_event(key(KeyCode::Enter));
    assert!(app.sessions_picker.is_none());
    assert_eq!(app.take_switch_request(), Some(2));
    assert_eq!(app.take_switch_request(), None);
    assert!(app.is_blank_session());

    // In the foreground there's no switching, only directions.
    app.detachable = false;
    open(&mut app);
    app.on_event(key(KeyCode::Enter));
    assert_eq!(app.take_switch_request(), None);
    assert!(matches!(app.entries.last(), Some(Entry::Error(_))));
}

#[cfg(unix)]
#[tokio::test]
async fn sessions_tab_lists_both_kinds_and_drives_a_remote_one() {
    use crate::background::{State, Status};
    use crate::remote::{Approval, Event as RemoteEvent, HubSession, Incoming, Status as Remote};
    let hub_session = |sid: &str, host: &str, title: &str| HubSession {
        sid: sid.into(),
        host: host.into(),
        cwd: "~/x".into(),
        title: title.into(),
        e2e: true,
        online: true,
        ..Default::default()
    };
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let mut settings = test_settings("test-model");
    settings.hub.key = Some(tamale_hub_protocol::encode_key(&[1; 32]));
    // Nothing listens there, which only means retrying.
    settings.hub.url = "ws://127.0.0.1:1".into();
    let (mut app, _rx) = App::new_with_channel(client, settings, Workspace::current().unwrap());

    // The tab comes first, by click or by arrows.
    let footer = screen_lines(&mut app, 100, 30)[28].clone();
    assert!(footer.contains("≡ Sessions"), "{footer}");
    let column = footer.chars().position(|c| c == '≡').unwrap() as u16;
    app.on_event(Event::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column,
        row: 28,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(app.sessions_picker.is_some());
    app.sessions_picker = None;
    app.on_event(key(KeyCode::Down));
    assert_eq!(app.tab_focus, Some(super::FooterTab::Sessions));
    app.on_event(key(KeyCode::Enter));
    assert_eq!(app.tab_focus, None);

    // Both kinds, and a session on this machine isn't listed twice.
    let picker = app.sessions_picker.as_mut().unwrap();
    picker.sessions = vec![Status {
        id: 1,
        cwd: "/project".into(),
        session_id: "s1".into(),
        title: "local one".into(),
        state: State::Idle,
        attached: false,
        since: 0,
        sid: "aa".into(),
        chef: false,
    }];
    app.on_app_event(AppEvent::HubSessions(Ok(vec![
        hub_session("aa", "this-box", "local one"),
        hub_session("bb", "far-box", "far away"),
        hub_session(crate::web::sid(), "this-box", "this very session"),
    ])));
    let shown = screen(&mut app);
    assert!(shown.contains("This machine") && shown.contains("local one"), "{shown}");
    assert!(shown.contains("far away") && shown.contains("far-box"), "{shown}");
    assert!(!shown.contains("this-box") && !shown.contains("this very"), "{shown}");

    // A draft waits while the other session has the prompt.
    app.input = "my draft".into();
    app.cursor = app.input.len();
    app.on_event(key(KeyCode::Down));
    app.on_event(key(KeyCode::Enter));
    assert!(app.remote_view.is_some());
    assert!(app.sessions_picker.is_none());
    assert_eq!(app.input, "");
    let generation = app.remote_generation;

    // What the session sends shows; what an old connection sends doesn't.
    let entries = |from, items| RemoteEvent::Message(Incoming::Entries { from, items });
    app.on_app_event(AppEvent::Remote(generation, RemoteEvent::Live));
    app.on_app_event(AppEvent::Remote(
        generation,
        entries(0, vec![Entry::User("hello there".into()), Entry::Assistant("general".into())]),
    ));
    app.on_app_event(AppEvent::Remote(
        generation - 1,
        entries(0, vec![Entry::User("stale".into())]),
    ));
    let shown = screen(&mut app);
    assert!(shown.contains("hello there") && shown.contains("general"), "{shown}");
    assert!(!shown.contains("stale"), "{shown}");
    assert!(shown.contains("connected") && shown.contains("far-box"), "{shown}");
    assert!(app.entries.iter().all(|e| !matches!(e, Entry::User(_))), "this session's are its own");
    app.on_app_event(AppEvent::Remote(
        generation,
        entries(1, vec![Entry::Assistant("kenobi".into())]),
    ));
    let shown = screen(&mut app);
    assert!(shown.contains("kenobi") && !shown.contains("general"), "{shown}");

    // Sent text leaves the prompt; approvals show and take the keyboard.
    type_text(&mut app, "do it");
    app.on_event(key(KeyCode::Enter));
    assert_eq!(app.input, "");
    app.on_app_event(AppEvent::Remote(
        generation,
        RemoteEvent::Message(Incoming::Approval(Some(Approval {
            name: "bash".into(),
            summary: "rm -rf target".into(),
            description: "Run a command".into(),
            always: Vec::new(),
        }))),
    ));
    let shown = screen(&mut app);
    assert!(shown.contains("rm -rf target") && shown.contains("Approve on far-box"), "{shown}");
    type_text(&mut app, "x");
    assert_eq!(app.input, "", "typing goes to the prompt's keys");

    // Esc interrupts a working session and only then comes back; ctrl+c always does.
    app.on_app_event(AppEvent::Remote(
        generation,
        RemoteEvent::Message(Incoming::Approval(None)),
    ));
    app.on_app_event(AppEvent::Remote(
        generation,
        RemoteEvent::Message(Incoming::Status(Box::new(Remote {
            running: true,
            ..Default::default()
        }))),
    ));
    app.on_event(key(KeyCode::Esc));
    assert!(app.remote_view.is_some(), "esc interrupts while it works");
    app.on_event(Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    assert!(app.remote_view.is_none());
    assert!(!app.should_quit, "leaving it doesn't quit this session");
    assert_eq!(app.input, "my draft");

    // A session that isn't encrypted isn't opened.
    app.open_remote_view(HubSession {
        e2e: false,
        ..hub_session("cc", "old-box", "")
    });
    assert!(app.remote_view.is_none());
    assert!(matches!(app.entries.last(), Some(Entry::Error(e)) if e.contains("old-box")));

    // `x` stops one through the hub: a second `x` if it's working, never an offline one.
    app.open_sessions_picker();
    app.sessions_picker.as_mut().unwrap().sessions.clear();
    let busy = HubSession {
        busy: true,
        ..hub_session("dd", "busy-box", "working")
    };
    let offline = HubSession {
        online: false,
        ..hub_session("ee", "gone-box", "")
    };
    app.on_app_event(AppEvent::HubSessions(Ok(vec![busy.clone(), offline])));
    app.on_event(key(KeyCode::Char('x')));
    let picker = app.sessions_picker.as_ref().unwrap();
    assert_eq!(picker.confirming, Some(background::Row::Hub("dd".into())));
    assert!(picker.closing.is_empty());
    app.on_event(key(KeyCode::Char('x')));
    assert_eq!(app.sessions_picker.as_ref().unwrap().closing, ["dd"]);
    assert!(screen(&mut app).contains("stopping…"));
    app.on_event(key(KeyCode::Down));
    app.on_event(key(KeyCode::Char('x')));
    assert!(matches!(app.entries.last(), Some(Entry::Error(e)) if e.contains("offline")));

    // Once it's gone it leaves the list, and the selection stays in it.
    app.on_app_event(AppEvent::HubSessionClosed(busy, Ok(())));
    let picker = app.sessions_picker.as_ref().unwrap();
    assert!(picker.closing.is_empty());
    assert_eq!((picker.rows().len(), picker.selected), (1, 0));
    assert!(matches!(app.entries.last(), Some(Entry::Info(e)) if e.contains("“working”")));
}

#[tokio::test]
async fn question_prompt_picks_toggles_types_and_submits() {
    use crate::tools::ask::{self, Answer, Reply};
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let questions = ask::parse(&serde_json::json!({
        "questions": [
            {
                "question": "Which database?",
                "header": "DB",
                "options": [
                    { "label": "Postgres", "description": "The safe choice" },
                    { "label": "SQLite" }
                ]
            },
            {
                "question": "Which features?",
                "header": "Features",
                "options": [{ "label": "Auth" }, { "label": "Logs" }, { "label": "Metrics" }],
                "multi_select": true
            }
        ]
    }))
    .unwrap();
    let open = |app: &mut App, questions: Vec<ask::Question>, ready: bool| {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let mut prompt = question::Prompt::new("Ask about the setup".into(), questions, reply);
        if ready {
            prompt.shown -= std::time::Duration::from_secs(1);
        }
        app.question = Some(prompt);
        answer
    };

    // Keys typed right as the questions open don't answer them.
    let mut answer = open(&mut app, questions.clone(), false);
    app.on_event(key(KeyCode::Char('1')));
    assert!(app.question.as_ref().unwrap().answers[0].is_empty());
    assert!(answer.try_recv().is_err());

    let mut answer = open(&mut app, questions.clone(), true);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    let screen =
        |app: &mut App, terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
            terminal.draw(|frame| app.draw(frame)).unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
        };
    let text = screen(&mut app, &mut terminal);
    assert!(text.contains("Which database?") && text.contains("The safe choice"), "{text}");
    assert!(text.contains("Type something else") && text.contains("Submit"));
    assert!(text.contains("answer the agent's questions"));

    // A single choice moves on to the next question.
    app.on_event(key(KeyCode::Down));
    app.on_event(key(KeyCode::Enter));
    assert_eq!(app.question.as_ref().unwrap().current, 1);
    // Multiple choice: toggle two, untoggle one, and type something else too.
    app.on_event(key(KeyCode::Char(' ')));
    app.on_event(key(KeyCode::Char('3')));
    app.on_event(key(KeyCode::Char('2')));
    app.on_event(key(KeyCode::Char('2')));
    app.on_event(key(KeyCode::Char('4')));
    assert!(app.question.as_ref().unwrap().typing);
    app.on_event(Event::Paste("rate ".into()));
    for c in "limits".chars() {
        app.on_event(key(KeyCode::Char(c)));
    }
    assert!(app.input.is_empty());
    app.on_event(key(KeyCode::Enter));
    // The review page lists the answers; Enter sends them.
    let text = screen(&mut app, &mut terminal);
    assert!(text.contains("Review your answers") && text.contains("Auth, Metrics"), "{text}");
    app.on_event(key(KeyCode::Enter));
    assert!(app.question.is_none());
    assert_eq!(
        answer.try_recv().unwrap(),
        Reply::Answers(vec![
            Answer { chosen: vec!["SQLite".into()], other: None },
            Answer {
                chosen: vec!["Auth".into(), "Metrics".into()],
                other: Some("rate limits".into()),
            },
        ])
    );

    // A single question is sent as soon as it's answered, here in the user's own words.
    let mut answer = open(&mut app, questions[..1].to_vec(), true);
    app.on_event(key(KeyCode::Char('3')));
    for c in "MySQL".chars() {
        app.on_event(key(KeyCode::Char(c)));
    }
    app.on_event(key(KeyCode::Enter));
    assert_eq!(
        answer.try_recv().unwrap(),
        Reply::Answers(vec![Answer { chosen: vec![], other: Some("MySQL".into()) }])
    );

    // A message from the web UI answers in words; slash commands don't.
    let mut answer = open(&mut app, questions.clone(), true);
    app.web_send("/help".into());
    assert!(app.question.is_some());
    app.web_send("whatever is simplest".into());
    assert_eq!(answer.try_recv().unwrap(), Reply::Text("whatever is simplest".into()));

    // The iPhone app's picker sends one answer per question, each saying something.
    let mut answer = open(&mut app, questions.clone(), true);
    let view = app.web_view();
    assert_eq!(view.question.as_ref().unwrap().questions, questions);
    let unit = |label: &str| Answer { chosen: vec![label.into()], other: None };
    app.web_answer(vec![unit("SQLite")]);
    app.web_answer(vec![unit("SQLite"), Answer::default()]);
    assert!(app.question.is_some());
    app.web_answer(vec![unit("SQLite"), unit("Auth")]);
    assert_eq!(answer.try_recv().unwrap(), Reply::Answers(vec![unit("SQLite"), unit("Auth")]));

    // Esc dismisses the questions (and interrupts the turn).
    let mut answer = open(&mut app, questions, true);
    app.on_event(key(KeyCode::Esc));
    assert!(app.question.is_none());
    assert!(answer.try_recv().is_err());
}

#[tokio::test]
async fn big_pastes_collapse_in_the_prompt_and_expand_when_sent() {
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    let wall: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    app.input = "look: ".into();
    app.cursor = app.input.len();
    app.on_event(Event::Paste(wall.clone()));
    assert_eq!(app.input, "look: [Pasted text #1 +19 lines]");
    assert_eq!(app.cursor, app.input.len());

    // A long single line collapses too, with its length.
    app.on_event(Event::Paste(" ".into()));
    let long = "x".repeat(1500);
    app.on_event(Event::Paste(long.clone()));
    assert_eq!(
        app.input,
        "look: [Pasted text #1 +19 lines] [Pasted text #2 1500 chars]"
    );

    // Backspace removes a placeholder whole.
    app.on_event(key(KeyCode::Backspace));
    assert_eq!(app.input, "look: [Pasted text #1 +19 lines] ");
    app.on_event(key(KeyCode::Backspace));
    assert_eq!(app.expand_pastes(&app.input), format!("look: {wall}"));

    // Small pastes go in as they are.
    app.on_event(Event::Paste(" small\npaste".into()));
    assert_eq!(app.input, "look: [Pasted text #1 +19 lines] small\npaste");

    // Sending expands it; the prompt and stored pastes are cleared.
    app.on_event(key(KeyCode::Enter));
    assert!(app.input.is_empty());
    assert!(app.pastes.is_empty());
    assert_eq!(
        app.prompts.prev(""),
        Some(format!("look: {wall} small\npaste"))
    );
}

#[tokio::test]
async fn a_chef_shows_its_kitchen_beside_a_welcome() {
    let client = Backend::Api(anthropic::Client::new("test-key").unwrap());
    let (mut app, _rx) = App::new_with_channel(
        client,
        test_settings("test-model"),
        Workspace::current().unwrap(),
    );
    app.set_chef();
    let session = |id: &str, host: &str, cwd: &str, title: &str, state| crate::web::KitchenSession {
        id: id.into(),
        host: host.into(),
        cwd: cwd.into(),
        title: title.into(),
        model: "claude-opus-5-5".into(),
        state,
        viewers: 0,
    };
    app.on_app_event(AppEvent::Kitchen(Ok(vec![
        session("a1b2c3", "laptop", "~/dev/api", "add rate limits to the public API", "working"),
        session("d4e5f6", "vps-1", "~/web", "fix the login page", "idle"),
        session("0a9b8c", "laptop", "~/dev/tamale", "", "approval"),
    ])));
    let lines = screen_lines(&mut app, 120, 40);
    let text = lines.join("\n");
    if std::env::var_os("SHOW_SCREEN").is_some() {
        println!("{text}");
    }
    assert!(text.contains("Kitchen"), "{text}");
    assert!(text.contains("c h e f"), "{text}");
    // What needs you comes first.
    let approval = text.find("0a9b8c").unwrap();
    let working = text.find("a1b2c3").unwrap();
    assert!(approval < working, "{text}");
    assert!(text.contains("needs you"));
    assert!(text.contains("3 sessions"));

    // Too narrow for the panel: the chat keeps the width.
    let narrow = screen_lines(&mut app, 80, 30).join("\n");
    assert!(!narrow.contains("Kitchen"), "{narrow}");

    // Its turns use the chef's tools.
    let ctx = app.begin_turn("what's everyone doing?".into());
    assert!(ctx.chef.is_some());
    assert!(app.backend_turn(String::new()).config.chef);
}
