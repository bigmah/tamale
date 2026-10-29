mod agent;
mod app;
mod tools;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, EventStream};
use crossterm::execute;
use futures::StreamExt;
use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::providers::anthropic;
use tokio::sync::mpsc;

use crate::app::App;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut model_name = std::env::var("HARNESS_MODEL")
        .unwrap_or_else(|_| anthropic::completion::CLAUDE_SONNET_4_6.into());
    let mut effort = match std::env::var("HARNESS_EFFORT") {
        Ok(v) if !v.trim().is_empty() => parse_effort(&v)?,
        _ => None,
    };
    let mut thinking = match std::env::var("HARNESS_THINKING") {
        Ok(v) if !v.trim().is_empty() => parse_thinking(&v)?,
        _ => agent::Thinking::default(),
    };
    let mut print_prompt = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-m" | "--model" => model_name = args.next().context("--model needs a value")?,
            "-e" | "--effort" => {
                effort = parse_effort(&args.next().context("--effort needs a value")?)?
            }
            "--thinking" => {
                thinking = parse_thinking(&args.next().context("--thinking needs a value")?)?
            }
            "-p" | "--print" => print_prompt = Some(args.next().context("--print needs a prompt")?),
            "-h" | "--help" => {
                println!(
                    "usage: harness [--model <anthropic-model-id>] [--effort <level>] [--thinking <mode>] [-p <prompt>]\n\n\
                     -p runs one prompt non-interactively and exits.\n\
                     --effort is one of low, medium, high, xhigh, max (default: API default).\n\
                     --thinking is adaptive (on) or off (default: adaptive).\n\
                     Requires ANTHROPIC_API_KEY. HARNESS_MODEL, HARNESS_EFFORT and HARNESS_THINKING set defaults."
                );
                return Ok(());
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        bail!("ANTHROPIC_API_KEY is not set");
    }
    let client = anthropic::Client::from_env().context("creating Anthropic client")?;
    let reasoning = agent::Reasoning { thinking, effort };

    if let Some(prompt) = print_prompt {
        let config = agent::Config {
            context_window: agent::context_window(&model_name),
            context_tokens: 0,
            reasoning,
        };
        let model = client.completion_model(model_name).with_prompt_caching();
        return print_mode(model, config, prompt).await;
    }

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    let result = run(
        &mut terminal,
        App::new_with_channel(client, model_name, reasoning),
    )
    .await;
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

fn parse_thinking(s: &str) -> Result<agent::Thinking> {
    agent::Thinking::parse(s)
        .with_context(|| format!("unknown thinking mode: {s} (use adaptive, on, off)"))
}

fn parse_effort(s: &str) -> Result<Option<agent::Effort>> {
    if matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "default" | "auto" | "none"
    ) {
        return Ok(None);
    }
    agent::Effort::parse(s)
        .map(Some)
        .with_context(|| format!("unknown effort level: {s} (use low, medium, high, xhigh, max)"))
}

async fn print_mode<M>(model: M, config: agent::Config, prompt: String) -> Result<()>
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
            AgentEvent::ToolCall { name, summary } => eprintln!("\n⏺ {name}({summary})"),
            AgentEvent::Retry {
                attempt,
                delay,
                error,
            } => eprintln!(
                "\n⟳ {error}; retrying in {:.1}s (attempt {attempt})",
                delay.as_secs_f64()
            ),
            AgentEvent::Compacting => eprintln!("\n⏺ compacting conversation…"),
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

    while !app.should_quit {
        terminal.draw(|f| app.draw(f))?;
        tokio::select! {
            Some(ev) = events.next() => app.on_event(ev?),
            Some(ev) = rx.recv() => app.on_app_event(ev),
            _ = tick.tick(), if app.is_busy() => app.on_tick(),
        }
    }
    Ok(())
}
