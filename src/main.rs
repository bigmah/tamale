mod agent;
mod app;
mod cli;
mod clipboard;
mod config;
mod session;
mod tools;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    EventStream,
};
use crossterm::execute;
use futures::StreamExt;
use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::providers::anthropic;
use tokio::sync::mpsc;

use crate::app::App;

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
    let mut settings = config::Settings::load(&std::env::current_dir()?, cli.config.as_deref())?;
    settings.apply_env(|name| std::env::var(name).ok())?;
    cli.apply(&mut settings);
    settings.validate()?;

    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        bail!("ANTHROPIC_API_KEY is not set");
    }
    let client = anthropic::Client::from_env().context("creating Anthropic client")?;
    if let Some(prompt) = cli.print_prompt {
        let config = settings.turn_config(0);
        let model = client
            .completion_model(settings.model.name)
            .with_prompt_caching();
        return print_mode(model, config, prompt).await;
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
    let (mut app, rx) = App::new_with_channel(client, settings);
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

async fn print_mode<M>(model: M, config: agent::TurnConfig, prompt: String) -> Result<()>
where
    M: rig_core::completion::CompletionModel + Clone + Send + Sync + 'static,
{
    use agent::AgentEvent;
    use std::io::Write;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let history = vec![rig_core::completion::Message::user(prompt)];
    tokio::spawn(agent::run(model, history, config, tx));

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
