//! Agent turns and conversation compaction.

pub mod models;
mod prompt;
mod reasoning;
mod request;
#[cfg(test)]
mod tests;

use std::time::Duration;

use rig_core::completion::message::{ToolResultContent, UserContent};
use rig_core::completion::{AssistantContent, CompletionModel, Message, ToolDefinition, Usage};
use tokio::sync::mpsc;

use crate::{config::AgentSettings, tools};
pub use models::{context_size, cost};
use prompt::{COMPACT_PROMPT, system_prompt};
pub use reasoning::{Effort, Reasoning, Thinking};
use request::request;

pub enum AgentEvent {
    TextDelta(String),
    ToolCall {
        name: String,
        /// Raw arguments, condensed (path or command).
        summary: String,
        /// What the step is for, in words (see `tools::describe`).
        description: String,
    },
    ToolResult {
        output: String,
        is_error: bool,
    },
    /// A request failed transiently and will be retried. Text streamed during the failed
    /// attempt should be discarded.
    Retry {
        attempt: u32,
        delay: Duration,
        error: String,
    },
    Compacting,
    /// The history was replaced by a summary. `before` is the context size that was summarized,
    /// `after` the size of the summary (tokens).
    Compacted {
        usage: Usage,
        before: u64,
        after: u64,
    },
    /// A consistent snapshot of the conversation, safe to resume from.
    History(Vec<Message>),
    Usage(Usage),
    Error(String),
    Done,
}

#[derive(Clone, Copy)]
pub struct TurnConfig {
    pub context_window: u64,
    /// Context size of the most recent request, used to decide when to auto-compact.
    pub context_tokens: u64,
    pub reasoning: Reasoning,
    pub limits: AgentSettings,
}

impl TurnConfig {
    fn should_compact(&self) -> bool {
        // Widen before multiplying so large context-window overrides can't overflow.
        self.context_tokens as u128 * 100
            >= self.context_window as u128 * self.limits.auto_compact_percent as u128
    }
}

/// Runs the agent loop until the model stops calling tools.
pub async fn run<M>(
    model: M,
    mut history: Vec<Message>,
    config: TurnConfig,
    tx: mpsc::UnboundedSender<AgentEvent>,
) where
    M: CompletionModel + Clone,
{
    if let Err(e) = run_inner(model, &mut history, config, &tx).await {
        let _ = tx.send(AgentEvent::Error(format!("{e:#}")));
    }
    let _ = tx.send(AgentEvent::Done);
}

/// Summarizes the history (the `/compact` command).
pub async fn compact<M>(
    model: M,
    history: Vec<Message>,
    instructions: Option<String>,
    config: TurnConfig,
    tx: mpsc::UnboundedSender<AgentEvent>,
) where
    M: CompletionModel + Clone,
{
    let result = async {
        if history.is_empty() {
            anyhow::bail!("nothing to compact");
        }
        let history = compact_history(
            &model,
            &system_prompt(),
            &tools::definitions(),
            &history,
            instructions.as_deref(),
            config,
            &tx,
        )
        .await?;
        let _ = tx.send(AgentEvent::History(history));
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let _ = tx.send(AgentEvent::Error(format!("compaction failed: {e:#}")));
    }
    let _ = tx.send(AgentEvent::Done);
}

async fn run_inner<M>(
    model: M,
    history: &mut Vec<Message>,
    mut config: TurnConfig,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) -> anyhow::Result<()>
where
    M: CompletionModel + Clone,
{
    let preamble = system_prompt();
    let tool_defs = tools::definitions();

    loop {
        if config.should_compact() && history.len() > 1 {
            *history =
                compact_history(&model, &preamble, &tool_defs, history, None, config, tx).await?;
            config.context_tokens = 0;
            let _ = tx.send(AgentEvent::History(history.clone()));
        }

        let (choice, usage) =
            request(&model, &preamble, &tool_defs, history, config, true, tx).await?;
        config.context_tokens = context_size(&usage);
        let _ = tx.send(AgentEvent::Usage(usage));

        let content: Vec<AssistantContent> = choice
            .into_iter()
            .filter(|c| !matches!(c, AssistantContent::Text(t) if t.text.trim().is_empty()))
            .collect();
        if content.is_empty() {
            let _ = tx.send(AgentEvent::History(history.clone()));
            return Ok(());
        }

        let calls: Vec<_> = content
            .iter()
            .filter_map(|c| match c {
                AssistantContent::ToolCall(tc) => Some(tc.clone()),
                _ => None,
            })
            .collect();
        history.push(Message::Assistant { id: None, content });

        if calls.is_empty() {
            let _ = tx.send(AgentEvent::History(history.clone()));
            return Ok(());
        }

        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            let name = call.function.name.clone();
            let args = call.function.arguments.clone();
            let _ = tx.send(AgentEvent::ToolCall {
                name: name.clone(),
                summary: tools::summarize(&name, &args),
                description: tools::describe(&name, &args),
            });

            let (output, is_error) = match tools::execute(&name, args).await {
                Ok(out) => (out, false),
                Err(e) => (format!("Error: {e:#}"), true),
            };
            let _ = tx.send(AgentEvent::ToolResult {
                output: output.clone(),
                is_error,
            });
            results.push(UserContent::tool_result_for(
                call.id.clone(),
                call.provider.clone(),
                name,
                vec![ToolResultContent::text(output)],
            ));
        }
        history.push(Message::User { content: results });
        let _ = tx.send(AgentEvent::History(history.clone()));
    }
}

/// Asks the model to summarize `history` and returns the replacement history.
///
/// Uses the same preamble and tools as normal requests so the cached prefix is reused (and
/// because Anthropic requires tool definitions when the history contains tool calls).
async fn compact_history<M>(
    model: &M,
    preamble: &str,
    tool_defs: &[ToolDefinition],
    history: &[Message],
    instructions: Option<&str>,
    config: TurnConfig,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) -> anyhow::Result<Vec<Message>>
where
    M: CompletionModel + Clone,
{
    let _ = tx.send(AgentEvent::Compacting);
    let mut prompt = COMPACT_PROMPT.to_string();
    if let Some(extra) = instructions.filter(|s| !s.trim().is_empty()) {
        prompt.push_str(&format!(
            "\n\nAdditional instructions from the user: {extra}"
        ));
    }
    let mut messages = history.to_vec();
    push_user_text(&mut messages, prompt);

    let (choice, usage) = request(model, preamble, tool_defs, &messages, config, false, tx).await?;
    let summary: String = choice
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if summary.trim().is_empty() {
        anyhow::bail!("the model returned an empty summary");
    }

    let _ = tx.send(AgentEvent::Compacted {
        before: context_size(&usage) - usage.output_tokens,
        after: usage.output_tokens,
        usage,
    });
    Ok(vec![Message::user(format!(
        "This session continues a previous conversation that was compacted to save context. \
Summary of the conversation so far:\n\n{}\n\n\
Continue from where it left off; if a task was in progress, carry on with it.",
        summary.trim()
    ))])
}

/// Appends text as a user turn, merging into a trailing user message (consecutive user
/// messages aren't allowed).
pub fn push_user_text(history: &mut Vec<Message>, text: String) {
    match history.last_mut() {
        Some(Message::User { content }) => content.push(UserContent::text(text)),
        _ => history.push(Message::user(text)),
    }
}
