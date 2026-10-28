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
    let mut print_prompt = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-m" | "--model" => model_name = args.next().context("--model needs a value")?,
            "-p" | "--print" => print_prompt = Some(args.next().context("--print needs a prompt")?),
            "-h" | "--help" => {
                println!(
                    "usage: harness [--model <anthropic-model-id>] [-p <prompt>]\n\n\
                     -p runs one prompt non-interactively (write/edit/bash are denied).\n\
                     Requires ANTHROPIC_API_KEY. HARNESS_MODEL sets the default model."
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
    let model = client.completion_model(model_name.clone());

    if let Some(prompt) = print_prompt {
        return print_mode(model, prompt).await;
    }

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    let result = run(&mut terminal, App::new_with_channel(model, model_name)).await;
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

async fn print_mode<M>(model: M, prompt: String) -> Result<()>
where
    M: rig_core::completion::CompletionModel + Clone + Send + Sync + 'static,
{
    use agent::{AgentEvent, Approval};
    use std::io::Write;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let history = vec![rig_core::completion::Message::user(prompt)];
    tokio::spawn(agent::run(model, history, Default::default(), tx));

    let mut failed = false;
    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::TextDelta(t) => {
                print!("{t}");
                std::io::stdout().flush()?;
            }
            AgentEvent::ToolCall { name, summary } => eprintln!("\n⏺ {name}({summary})"),
            AgentEvent::ApprovalRequest { reply, .. } => {
                let _ = reply.send(Approval::Deny);
            }
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

async fn run<M>(
    terminal: &mut ratatui::DefaultTerminal,
    (mut app, mut rx): (App<M>, mpsc::UnboundedReceiver<app::TurnEvent>),
) -> Result<()>
where
    M: rig_core::completion::CompletionModel + Clone + Send + Sync + 'static,
{
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(80));

    while !app.should_quit {
        terminal.draw(|f| app.draw(f))?;
        tokio::select! {
            Some(ev) = events.next() => app.on_event(ev?),
            Some(ev) = rx.recv() => app.on_agent(ev),
            _ = tick.tick(), if app.is_running() => app.on_tick(),
        }
    }
    Ok(())
}
