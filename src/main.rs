mod agent;
mod app;
mod backend;
mod checkpoint;
mod cli;
mod clipboard;
mod config;
mod permissions;
mod session;
mod tools;
mod workspace;

use std::time::Duration;

use anyhow::{Result, bail};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    EventStream,
};
use crossterm::execute;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::app::App;
use crate::backend::Backend;

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

async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    (mut app, mut rx): (App, mpsc::UnboundedReceiver<app::AppEvent>),
) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(80));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut next_frame = tokio::time::Instant::now();

    let result = async {
        while !app.should_quit {
            if let Some(mut request) = app.take_login_request() {
                // The sign-in prompt needs the real terminal (and its keyboard input).
                drop(events);
                let mouse = app.mouse_enabled();
                suspend_terminal();
                let result = request.login.login().await;
                *terminal = resume_terminal(mouse)?;
                events = EventStream::new();
                app.on_login(request, result);
                dirty = true;
            }
            tokio::select! {
                Some(ev) = events.next() => {
                    app.on_event(ev?);
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
                _ = tokio::time::sleep_until(next_frame), if dirty => {
                    terminal.draw(|f| app.draw(f))?;
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
