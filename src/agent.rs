use futures::StreamExt;
use rig_core::completion::message::{ToolResultContent, UserContent};
use rig_core::completion::{AssistantContent, CompletionModel, Message, Usage};
use rig_core::streaming::StreamedAssistantContent;
use tokio::sync::mpsc;

use crate::tools;

const MAX_TOKENS: u64 = 16_000;

pub enum AgentEvent {
    TextDelta(String),
    ToolCall {
        name: String,
        summary: String,
    },
    ToolResult {
        output: String,
        is_error: bool,
    },
    /// A consistent snapshot of the conversation, safe to resume from.
    History(Vec<Message>),
    Usage(Usage),
    Error(String),
    Done,
}

pub fn system_prompt() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    format!(
        "You are a coding agent running in the user's terminal. You help with software \
engineering tasks: reading, writing and editing code, running commands, and answering questions \
about the codebase.\n\n\
Guidelines:\n\
- Use the tools to inspect the codebase before making changes. Prefer edit_file for targeted edits.\n\
- Keep responses concise; the UI is a terminal. Use plain text or light markdown.\n\
- Match the existing code style. Verify your work (build/test) when practical.\n\
- Don't take destructive actions (deleting files, force pushes) unless asked.\n\n\
Environment:\n- Working directory: {cwd}\n- Platform: {}\n",
        std::env::consts::OS
    )
}

/// Runs the agent loop until the model stops calling tools.
pub async fn run<M>(model: M, mut history: Vec<Message>, tx: mpsc::UnboundedSender<AgentEvent>)
where
    M: CompletionModel + Clone,
{
    if let Err(e) = run_inner(model, &mut history, &tx).await {
        let _ = tx.send(AgentEvent::Error(format!("{e:#}")));
    }
    let _ = tx.send(AgentEvent::Done);
}

async fn run_inner<M>(
    model: M,
    history: &mut Vec<Message>,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) -> anyhow::Result<()>
where
    M: CompletionModel + Clone,
{
    let preamble = system_prompt();
    let tool_defs = tools::definitions();

    loop {
        let (prompt, rest) = history
            .split_last()
            .ok_or_else(|| anyhow::anyhow!("empty history"))?;

        let mut stream = model
            .completion_request(prompt.clone())
            .preamble(preamble.clone())
            .messages(rest.to_vec())
            .tools(tool_defs.clone())
            .max_tokens(MAX_TOKENS)
            .stream()
            .await?;

        while let Some(item) = stream.next().await {
            if let StreamedAssistantContent::Text(t) = item? {
                let _ = tx.send(AgentEvent::TextDelta(t.text));
            }
        }
        let _ = tx.send(AgentEvent::Usage(stream.usage()));

        let content: Vec<AssistantContent> = stream
            .choice
            .iter()
            .filter(|c| !matches!(c, AssistantContent::Text(t) if t.text.trim().is_empty()))
            .cloned()
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
