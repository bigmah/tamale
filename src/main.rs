mod agent;
mod app;
mod backend;
mod browser;
mod checkpoint;
mod cli;
mod clipboard;
mod config;
mod diff;
mod permissions;
mod session;
mod tools;
mod web;
mod workspace;

use std::time::Duration;

use anyhow::{Result, bail};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::app::{App, BrowserRequest, WebRequest};
use crate::backend::Backend;

/// Shortest time between syncs of the web UI.
const WEB_FRAME: Duration = Duration::from_millis(40);

fn main() -> Result<()> {
    let cli = cli::Cli::parse(std::env::args().skip(1))?;
    if cli.help {
        println!("{}", cli::HELP);
        return Ok(());
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(start(cli))
}

async fn start(cli: cli::Cli) -> Result<()> {
    let mut workspace = workspace::Workspace::current()?;
    for dir in &cli.additional_dirs {
        workspace.add_dir(dir)?;
    }
    let mut settings = config::Settings::load(&workspace.cwd, cli.config.as_deref())?;
    settings.apply_env(|name| std::env::var(name).ok())?;
    cli.apply(&mut settings);
    settings.validate()?;

    let mut backend = Backend::connect(&settings.backend).await?;
    if !backend.signed_in() {
        let (name, login) = match &backend {
            Backend::Codex(_) => ("Codex", "codex login"),
            _ => ("Claude Code", "claude auth login"),
        };
        if cli.print_prompt.is_some() {
            bail!("{name} isn't signed in. Run `{login}` (or start tamale without -p to sign in).");
        }
        eprintln!("{name} isn't signed in; starting `{login}`…");
        backend.login().await?;
    }
    if let Some(prompt) = cli.print_prompt {
        let permissions = permissions::Policy::new(&settings.permissions, workspace.cwd.clone());
        let ctx =
            agent::TurnCtx::new(permissions.shared(), None).with_subagent(backend::Subagent {
                backend: backend.clone(),
                model: settings.model.name.clone(),
                config: settings.turn_config(0),
            });
        let turn = backend::Turn {
            model: settings.model.name.clone(),
            history: vec![rig_core::completion::Message::user(prompt.clone())],
            prompt,
            thread: None,
            config: settings.turn_config(0),
            workspace,
        };
        return print_mode(backend, turn, ctx, cli.yes).await;
    }

    // Restore terminal input modes even if we panic.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            std::io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture
        );
        hook(info);
    }));
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    if settings.ui.mouse {
        execute!(std::io::stdout(), EnableMouseCapture)?;
    }
    let (mut app, rx) = App::new_with_channel(backend, settings, workspace);
    app.load_prompt_history();
    app.preload_diff();
    match cli.resume {
        Some(cli::Resume::Latest) => match session::list(Some(&std::env::current_dir()?)).first() {
            Some(latest) => app.resume(&latest.id),
            None => app.resume_command(None),
        },
        Some(cli::Resume::Pick) => app.resume_command(None),
        None => {}
    }
    let result = run(&mut terminal, (app, rx)).await;
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    );
    ratatui::restore();
    result
}

async fn print_mode(
    backend: Backend,
    turn: backend::Turn,
    ctx: agent::TurnCtx,
    yes: bool,
) -> Result<()> {
    use agent::AgentEvent;
    use std::io::Write;

    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(backend.run(turn, ctx, tx));

    let mut failed = false;
    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::TextDelta(t) => {
                print!("{t}");
                std::io::stdout().flush()?;
            }
            AgentEvent::ToolCall {
                name,
                summary,
                description,
            } => eprintln!(
                "\n⏺ {description}  ({name}: {})",
                summary.lines().next().unwrap_or("")
            ),
            AgentEvent::Retry {
                attempt,
                delay,
                error,
            } => eprintln!(
                "\n⟳ {error}; retrying in {:.1}s (attempt {attempt})",
                delay.as_secs_f64()
            ),
            AgentEvent::Approve { name, reply, .. } => {
                let answer = if yes {
                    permissions::Approval::Allow
                } else {
                    eprintln!(
                        "  denied: {name} needs approval (allow it in [permissions] or pass --yes)"
                    );
                    permissions::Approval::Deny {
                        reason: Some(
                            "this non-interactive run only allows calls permitted by the \
[permissions] settings; it can't prompt for approval."
                                .into(),
                        ),
                    }
                };
                let _ = reply.send(answer);
            }
            AgentEvent::Compacting => eprintln!("\n⏺ compacting conversation…"),
            AgentEvent::Continuing(reason) => eprintln!("\n⟳ {reason}"),
            AgentEvent::Error(e) => {
                eprintln!("\nerror: {e}");
                failed = true;
            }
            AgentEvent::Done => break,
            _ => {}
        }
    }
    println!();
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn suspend_terminal() {
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    );
    ratatui::restore();
}

fn resume_terminal(mouse: bool) -> Result<ratatui::DefaultTerminal> {
    let terminal = ratatui::init();
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    if mouse {
        execute!(std::io::stdout(), EnableMouseCapture)?;
    }
    Ok(terminal)
}

/// Where the TUI is in use; the other screen shows a notice.
#[derive(Clone, Copy, PartialEq)]
enum Surface {
    Terminal,
    Browser,
}

struct Browser {
    server: browser::Server,
    /// Present while a tab is connected.
    terminal: Option<browser::WebTerminal>,
}

async fn browser_event(browser: &mut Option<Browser>) -> Option<browser::ClientEvent> {
    match browser {
        Some(browser) => browser.server.events.recv().await,
        None => std::future::pending().await,
    }
}

async fn web_event(web: &mut Option<web::Server>) -> Option<web::WebEvent> {
    match web {
        Some(web) => web.events.recv().await,
        None => std::future::pending().await,
    }
}

/// Starts or stops serving the web UI, as `/web` asked.
async fn web_request(app: &mut App, web: &mut Option<web::Server>, request: WebRequest) {
    match request {
        WebRequest::Open { lan } => {
            // Switching between local and LAN serving restarts on the other address.
            if web.as_ref().is_some_and(|w| w.lan != lan) {
                *web = None;
            }
            if web.is_none() {
                let started = match web::Server::start(lan).await {
                    // Off any network, this machine can still use it.
                    Err(e) if lan => web::Server::start(false).await.inspect(|_| {
                        app.on_web(Ok(format!(
                            "Serving the web UI to this machine only ({e:#})."
                        )))
                    }),
                    started => started,
                };
                match started {
                    Ok(server) => *web = Some(server),
                    Err(e) => app.on_web(Err(format!("Couldn't serve the web UI: {e:#}"))),
                }
            }
            // The terminal shows the address instead of the conversation from now on.
            if let Some(server) = web {
                browser::open(&server.url);
                app.set_web_session(true);
            }
        }
        WebRequest::Remote { on: true } => {
            // Before anything opens: without a key there's nothing to connect with.
            let hub = app.settings_hub().clone();
            let key = match hub.key_bytes() {
                Ok(key) => key,
                Err(message) => {
                    app.on_web(Err(message));
                    return;
                }
            };
            if web.is_none() {
                match web::Server::start(false).await {
                    Ok(server) => *web = Some(server),
                    Err(e) => {
                        app.on_web(Err(format!("Couldn't serve the web UI: {e:#}")));
                        return;
                    }
                }
            }
            if let Some(server) = web {
                if server.hub_url().is_none() {
                    web::e2e::remove_old_pairing_key();
                    server.connect_hub(
                        &hub.url,
                        &tamale_hub_protocol::auth_value(&key),
                        app.hub_label(),
                        hub.e2e.then(|| web::e2e::Keys::from_key(&key)),
                    );
                }
                app.set_web_session(true);
                app.set_hub_viewers(Some(server.remote_tabs()));
            }
        }
        WebRequest::Remote { on: false } => {
            let disconnected = web.as_mut().is_some_and(|w| w.disconnect_hub());
            app.set_hub_viewers(None);
            app.on_web(if disconnected {
                Ok("Disconnected from the hub.".into())
            } else {
                Err("Not connected to a hub.".into())
            });
        }
        WebRequest::Close => {
            app.set_web_session(false);
            app.set_hub_viewers(None);
            app.on_web(match web.take() {
                Some(_) => Ok("Stopped serving the web UI; the session continues here.".into()),
                None => Err("The web UI isn't being served.".into()),
            })
        }
    }
}

/// With `[hub] connect = "auto"`, makes the session reachable through the hub from the
/// start, without taking over the terminal the way `/web remote` does.
async fn connect_hub_on_start(app: &mut App, web: &mut Option<web::Server>) {
    let hub = app.settings_hub().clone();
    if hub.connect != config::HubConnect::Auto {
        return;
    }
    let key = match hub.key_bytes() {
        Ok(key) => key,
        Err(message) => {
            app.on_web(Err(message));
            return;
        }
    };
    match web::Server::start(false).await {
        Ok(mut server) => {
            web::e2e::remove_old_pairing_key();
            server.connect_hub(
                &hub.url,
                &tamale_hub_protocol::auth_value(&key),
                app.hub_label(),
                hub.e2e.then(|| web::e2e::Keys::from_key(&key)),
            );
            app.set_hub_viewers(Some(0));
            *web = Some(server);
        }
        Err(e) => app.on_web(Err(format!("Couldn't serve the web UI for the hub: {e:#}"))),
    }
}

/// Keys pressed in the terminal during a web session, which only has a few.
async fn web_session_key(app: &mut App, web: &mut Option<web::Server>, event: &Event) {
    let Event::Key(key) = event else { return };
    if key.kind == KeyEventKind::Release {
        return;
    }
    let Some(server) = web else { return };
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Char('o') => browser::open(&server.url),
        KeyCode::Char('c') => {
            let url = server.url.clone();
            tokio::spawn(async move { clipboard::write(&url).await });
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            web_request(app, web, WebRequest::Close).await;
        }
        _ => {}
    }
}

async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    (mut app, mut rx): (App, mpsc::UnboundedReceiver<app::AppEvent>),
) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut next_frame = tokio::time::Instant::now();
    let mut browser: Option<Browser> = None;
    let mut surface = Surface::Terminal;
    let mut web: Option<web::Server> = None;
    connect_hub_on_start(&mut app, &mut web).await;
    // The web UI syncs at most every `WEB_FRAME`, since it renders markdown to HTML.
    let mut web_dirty = false;
    let mut next_web = tokio::time::Instant::now();

    let result = async {
        while !app.should_quit {
            if let Some(mut request) = app.take_login_request() {
                // The sign-in prompt needs the real terminal (and its keyboard input).
                if let Some(Browser {
                    server,
                    terminal: Some(web),
                }) = &mut browser
                {
                    web.draw(|f| {
                        browser::draw_notice(f, "Finish signing in in the terminal tamale runs in.")
                    })?;
                    server.flush(web);
                }
                drop(events);
                let mouse = app.mouse_enabled();
                suspend_terminal();
                let result = request.login.login().await;
                *terminal = resume_terminal(mouse)?;
                events = EventStream::new();
                app.on_login(request, result);
                dirty = true;
            }
            if let Some(request) = app.take_browser_request() {
                match request {
                    BrowserRequest::Open => {
                        if browser.is_none() {
                            match browser::Server::start().await {
                                Ok(server) => {
                                    browser = Some(Browser {
                                        server,
                                        terminal: None,
                                    })
                                }
                                Err(e) => app.on_browser(Err(format!(
                                    "Couldn't serve tamale to a browser: {e:#}"
                                ))),
                            }
                        }
                        if let Some(Browser { server, .. }) = &browser {
                            browser::open(&server.url);
                            app.on_browser(Ok(format!(
                                "Serving tamale at {} (opening it in your browser). Keys pressed \
                                 in the tab or here switch between them; /browser off stops it.",
                                server.url
                            )));
                        }
                    }
                    BrowserRequest::Close => {
                        app.on_browser(match browser.take() {
                            Some(_) => Ok("Stopped serving tamale to the browser.".into()),
                            None => Err("tamale isn't being served to a browser.".into()),
                        });
                        surface = Surface::Terminal;
                    }
                }
                dirty = true;
            }
            if let Some(request) = app.take_web_request() {
                web_request(&mut app, &mut web, request).await;
                dirty = true;
            }
            web_dirty |= dirty;
            tokio::select! {
                Some(ev) = events.next() => {
                    let ev = ev?;
                    if web.is_some() && app.is_web_session() {
                        web_session_key(&mut app, &mut web, &ev).await;
                    } else if surface == Surface::Terminal {
                        app.on_event(ev);
                    } else if browser::takes_over(&ev) {
                        surface = Surface::Terminal;
                    }
                    dirty = true;
                }
                Some(ev) = browser_event(&mut browser) => {
                    let Some(browser) = &mut browser else { continue };
                    match ev {
                        browser::ClientEvent::Connected(cols, rows) => {
                            browser.terminal = Some(browser::terminal(cols, rows));
                            surface = Surface::Browser;
                        }
                        browser::ClientEvent::Resize(cols, rows) => {
                            if let Some(web) = &mut browser.terminal {
                                web.backend_mut().resize(cols, rows);
                            }
                        }
                        browser::ClientEvent::Input(_) if web.is_some() && app.is_web_session() => {}
                        browser::ClientEvent::Input(ev) => {
                            if surface == Surface::Browser {
                                app.on_event(ev);
                            } else if browser::takes_over(&ev) {
                                surface = Surface::Browser;
                            }
                        }
                        browser::ClientEvent::Disconnected => {
                            browser.terminal = None;
                            surface = Surface::Terminal;
                        }
                    }
                    dirty = true;
                }
                Some(ev) = web_event(&mut web) => {
                    match ev {
                        web::WebEvent::Connected(id) => {
                            if let Some(web) = &mut web {
                                web.sync(app.web_view());
                                web.welcome(id);
                            }
                        }
                        web::WebEvent::Disconnected => {}
                        web::WebEvent::Hub(result) => app.on_web(result),
                        web::WebEvent::Send(text) => app.web_send(text),
                        web::WebEvent::Interrupt => app.web_interrupt(),
                        web::WebEvent::Approve(choice, reason) => app.web_approve(choice, reason),
                    }
                    dirty = true;
                }
                Some(ev) = rx.recv() => {
                    app.on_app_event(ev);
                    // Preserve event order while amortizing a burst of streamed tokens
                    // over one frame. Bound the batch so keyboard input stays responsive.
                    for _ in 0..255 {
                        let Ok(ev) = rx.try_recv() else { break };
                        app.on_app_event(ev);
                    }
                    dirty = true;
                }
                _ = tick.tick(), if app.is_busy() => {
                    app.on_tick();
                    dirty = true;
                }
                _ = tokio::time::sleep_until(next_web), if web_dirty && web.is_some() => {
                    if let Some(web) = &mut web {
                        web.sync(app.web_view());
                    }
                    web_dirty = false;
                    next_web = tokio::time::Instant::now() + WEB_FRAME;
                }
                _ = tokio::time::sleep_until(next_frame), if dirty => {
                    if let Some(w) = &web
                        && w.hub_url().is_some() {
                            app.set_hub_viewers(Some(w.remote_tabs()));
                        }
                    let session = web
                        .as_ref()
                        .filter(|_| app.is_web_session())
                        .map(|w| (w.url.as_str(), w.hub_state(), w.tabs()));
                    draw(terminal, &mut browser, surface, session, &mut app)?;
                    dirty = false;
                    next_frame = tokio::time::Instant::now() + Duration::from_millis(16);
                }
            }
        }
        Ok(())
    }
    .await;
    app.save_session();
    app.finish_saves().await;
    result
}

/// Draws the TUI on the screen in use and a notice on the other. During a web session
/// (its address and open tabs in `session`) the terminal shows only that.
fn draw(
    terminal: &mut ratatui::DefaultTerminal,
    browser: &mut Option<Browser>,
    surface: Surface,
    session: Option<(&str, Option<web::HubState<'_>>, usize)>,
    app: &mut App,
) -> Result<()> {
    if let Some((url, remote, tabs)) = session {
        terminal.draw(|f| app.draw_web_session(f, url, remote, tabs))?;
        if let Some(Browser {
            server,
            terminal: Some(web),
        }) = browser
        {
            web.draw(|f| browser::draw_notice(f, "tamale is in use in the web UI (/web)."))?;
            server.flush(web);
        }
        return Ok(());
    }
    let Some(Browser {
        server,
        terminal: Some(web),
    }) = browser
    else {
        terminal.draw(|f| app.draw(f))?;
        return Ok(());
    };
    if surface == Surface::Browser {
        web.draw(|f| app.draw(f))?;
        terminal.draw(|f| {
            browser::draw_notice(
                f,
                "tamale is in use in a browser tab. Press any key to use it here.",
            )
        })?;
    } else {
        terminal.draw(|f| app.draw(f))?;
        web.draw(|f| {
            browser::draw_notice(
                f,
                "tamale is in use in the terminal. Press any key or click to use it here.",
            )
        })?;
    }
    server.flush(web);
    Ok(())
}
