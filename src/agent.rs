//! Agent turns and conversation compaction.

pub mod models;
mod prompt;
mod reasoning;
mod request;
#[cfg(test)]
mod tests;

use std::time::Duration;

use rig_core::completion::message::{ReasoningContent, ToolResultContent, UserContent};
use rig_core::completion::{
    AssistantContent, CompletionModel, FinishReason, Message, ToolDefinition, Usage,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::checkpoint::Recorder;
use crate::permissions::{self, Approval, Decision};
use crate::workspace::Workspace;
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
    /// The latest tool call needs the user's approval. `always` lists the session rules
    /// "always allow" would add (empty if none can cover the call).
    Approve {
        name: String,
        summary: String,
        description: String,
        always: Vec<String>,
        reply: oneshot::Sender<Approval>,
    },
    /// A request failed transiently and will be retried. Text streamed during the failed
    /// attempt should be discarded.
    Retry {
        attempt: u32,
        delay: Duration,
        error: String,
    },
    /// The response stopped early; another request will continue the same task.
    Continuing(String),
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

/// Per-turn tool state: what may run without asking, and where file snapshots go.
pub struct TurnCtx {
    pub permissions: permissions::Shared,
    /// `None` when the turn isn't checkpointed (e.g. `-p` runs, which aren't saved).
    pub checkpoint: Option<Recorder>,
}

impl TurnCtx {
    /// Asks the policy (and, if needed, the user) whether the call may run. `Err` is the
    /// tool result explaining why not.
    async fn authorize(
        &self,
        name: &str,
        args: &Value,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<(), String> {
        let decision = self.permissions.lock().unwrap().check(name, args);
        let always = match decision {
            Decision::Allow => return Ok(()),
            Decision::Deny(rule) => {
                return Err(format!(
                    "Blocked by the permission rule `{rule}`. Don't retry this call; \
use another approach or ask the user."
                ));
            }
            Decision::Ask { always } => always,
        };
        let (reply, answer) = oneshot::channel();
        let _ = tx.send(AgentEvent::Approve {
            name: name.into(),
            summary: tools::summarize(name, args),
            description: tools::describe(name, args),
            always,
            reply,
        });
        match answer.await {
            Ok(Approval::Allow) => Ok(()),
            Ok(Approval::Deny {
                reason: Some(reason),
            }) => Err(format!("The user denied this tool call and said: {reason}")),
            Ok(Approval::Deny { reason: None }) | Err(_) => Err(
                "The user denied this tool call. Don't retry it; adjust your approach or ask \
the user how to proceed."
                    .into(),
            ),
        }
    }

    /// Snapshots files a call is about to change.
    fn before_execute(
        &mut self,
        name: &str,
        args: &Value,
        workspace: &Workspace,
    ) -> anyhow::Result<()> {
        let Some(recorder) = &mut self.checkpoint else {
            return Ok(());
        };
        if let Some(path) = tools::write_target(name, args) {
            recorder.before_write(&workspace.cwd.join(path))?;
        } else if name == "bash" {
            // Commands can't be reverted; counting them lets /undo say so.
            let _ = recorder.command_ran();
        }
        Ok(())
    }
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

/// Runs until the model finishes, continuing truncated responses and tool steps.
pub async fn run<M>(
    model: M,
    mut history: Vec<Message>,
    config: TurnConfig,
    workspace: Workspace,
    mut ctx: TurnCtx,
    tx: mpsc::UnboundedSender<AgentEvent>,
) where
    M: CompletionModel + Clone,
{
    if let Err(e) = run_inner(model, &mut history, config, &workspace, &mut ctx, &tx).await {
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
    workspace: Workspace,
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
            &system_prompt(&workspace),
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
    workspace: &Workspace,
    ctx: &mut TurnCtx,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) -> anyhow::Result<()>
where
    M: CompletionModel + Clone,
{
    let preamble = system_prompt(workspace);
    let tool_defs = tools::definitions();
    let mut force_compact = false;
    let mut empty_responses = 0;

    loop {
        if force_compact || (config.should_compact() && history.len() > 1) {
            *history =
                compact_history(&model, &preamble, &tool_defs, history, None, config, tx).await?;
            config.context_tokens = 0;
            force_compact = false;
            let _ = tx.send(AgentEvent::History(history.clone()));
        }

        let response = request(&model, &preamble, &tool_defs, history, config, true, tx).await?;
        config.context_tokens = context_size(&response.usage);
        let _ = tx.send(AgentEvent::Usage(response.usage));
        let continuation = continuation(response.finish_reason.as_ref())?;

        let content: Vec<AssistantContent> = response
            .choice
            .into_iter()
            .filter(|c| !matches!(c, AssistantContent::Text(t) if t.text.trim().is_empty()))
            .collect();
        if let Some(continuation) = continuation {
            force_compact = matches!(continuation, Continuation::ContextLimit);
            append_continuation(history, content, continuation.prompt());
            let _ = tx.send(AgentEvent::History(history.clone()));
            let _ = tx.send(AgentEvent::Continuing(continuation.status().into()));
            continue;
        }

        let calls: Vec<_> = content
            .iter()
            .filter_map(|c| match c {
                AssistantContent::ToolCall(tc) => Some(tc.clone()),
                _ => None,
            })
            .collect();

        if calls.is_empty()
            && !content
                .iter()
                .any(|c| matches!(c, AssistantContent::Text(_)))
        {
            empty_responses += 1;
            if empty_responses > 1 {
                anyhow::bail!("the model repeatedly returned no answer or tool calls");
            }
            append_continuation(
                history,
                content,
                Some(
                    "Your last response contained no answer or tool call. Continue the user's \
task using tools as needed. If the task is complete, provide the final answer.",
                ),
            );
            let _ = tx.send(AgentEvent::History(history.clone()));
            let _ = tx.send(AgentEvent::Continuing(
                "Model returned no answer; continuing…".into(),
            ));
            continue;
        }
        empty_responses = 0;
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

            let result = match ctx.authorize(&name, &args, tx).await {
                Err(denied) => Err(denied),
                Ok(()) => match ctx.before_execute(&name, &args, workspace) {
                    Err(e) => Err(format!(
                        "Error: couldn't save a checkpoint, so the call was not run: {e:#}"
                    )),
                    Ok(()) => tools::execute(&name, args)
                        .await
                        .map_err(|e| format!("Error: {e:#}")),
                },
            };
            let (output, is_error) = match result {
                Ok(out) => (out, false),
                Err(out) => (out, true),
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

    let mut usage = Usage::default();
    let mut before = None;
    let mut summary = String::new();
    loop {
        let response = request(model, preamble, tool_defs, &messages, config, false, tx).await?;
        before.get_or_insert(context_size(&response.usage) - response.usage.output_tokens);
        usage += response.usage;
        for content in &response.choice {
            if let AssistantContent::Text(t) = content {
                summary.push_str(&t.text);
            }
        }
        match continuation(response.finish_reason.as_ref())? {
            Some(Continuation::ContextLimit) => {
                anyhow::bail!("context window filled while compacting; original history was kept");
            }
            Some(reason) => {
                append_continuation(
                    &mut messages,
                    response.choice,
                    Some(
                        "Continue the conversation summary from where it was cut off. \
Respond with the remaining summary only; do not call tools or resume the task yet.",
                    ),
                );
                let _ = tx.send(AgentEvent::Continuing(reason.status().into()));
            }
            None => break,
        }
    }
    if summary.trim().is_empty() {
        anyhow::bail!("the model returned an empty summary");
    }

    let _ = tx.send(AgentEvent::Compacted {
        before: before.unwrap_or_default(),
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

enum Continuation {
    TokenLimit,
    ContextLimit,
    Paused,
}

impl Continuation {
    fn status(&self) -> &'static str {
        match self {
            Self::TokenLimit => "Response reached its token limit; continuing…",
            Self::ContextLimit => "Context window filled; compacting and continuing…",
            Self::Paused => "Model paused its turn; continuing…",
        }
    }

    fn prompt(&self) -> Option<&'static str> {
        match self {
            Self::TokenLimit | Self::ContextLimit => Some(
                "Your previous response was cut off at a token or context limit. Continue \
the user's task from where you left off without repeating completed work. No tool calls \
from the cut-off response were executed; reissue any needed calls in smaller steps. \
Keep working until the task is complete or you encounter a concrete blocker.",
            ),
            // Anthropic resumes paused server tools with the assistant content as-is.
            Self::Paused => None,
        }
    }
}

fn continuation(reason: Option<&FinishReason>) -> anyhow::Result<Option<Continuation>> {
    Ok(match reason {
        Some(FinishReason::Length) => Some(Continuation::TokenLimit),
        Some(FinishReason::Other(reason)) if reason == "pause_turn" => Some(Continuation::Paused),
        Some(FinishReason::Other(reason)) if reason == "model_context_window_exceeded" => {
            Some(Continuation::ContextLimit)
        }
        Some(FinishReason::ContentFilter) => anyhow::bail!("the model declined this request"),
        Some(FinishReason::Other(reason)) => {
            anyhow::bail!("the model stopped unexpectedly: {reason}");
        }
        _ => None,
    })
}

/// Keep usable output, but never replay or execute a possibly truncated tool call.
/// Unsigned thinking can also be incomplete and is rejected by Anthropic on replay.
fn append_continuation(
    history: &mut Vec<Message>,
    mut content: Vec<AssistantContent>,
    prompt: Option<&str>,
) {
    content.retain_mut(|c| match c {
        AssistantContent::ToolCall(_) => false,
        AssistantContent::Text(t) => !t.text.trim().is_empty(),
        AssistantContent::Reasoning(r) => {
            r.content.retain(|block| match block {
                ReasoningContent::Text { signature, .. } => {
                    signature.as_ref().is_some_and(|s| !s.is_empty())
                }
                _ => true,
            });
            !r.content.is_empty()
        }
        _ => true,
    });
    if !content.is_empty() {
        history.push(Message::Assistant { id: None, content });
    }
    if let Some(prompt) = prompt {
        // When no assistant content survived, merge into the last user message.
        // Tool results must stay first in that message for Anthropic's API.
        push_user_text(history, prompt.into());
    }
}

/// Appends text as a user turn, merging into a trailing user message (consecutive user
/// messages aren't allowed).
pub fn push_user_text(history: &mut Vec<Message>, text: String) {
    match history.last_mut() {
        Some(Message::User { content }) => content.push(UserContent::text(text)),
        _ => history.push(Message::user(text)),
    }
}
