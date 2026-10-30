//! The `agent` tool: hands a self-contained task to a fresh agent on the current backend.
//!
//! The subagent starts with an empty conversation and the same system prompt and tools
//! (except `agent`), and shares the parent turn's permissions and checkpoint, so its calls
//! get the same rules, approval prompts and `/undo`. Its tool calls are shown in the
//! transcript as they happen; only its final answer goes back to the parent model.

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::completion::{AssistantContent, Message};
use serde_json::Value;
use tokio::sync::mpsc;

use super::{Backend, Turn};
use crate::agent::{AgentEvent, TurnConfig, TurnCtx};
use crate::workspace::Workspace;

/// What the subagent is told about its situation, before the task.
const PREAMBLE: &str = "You were started as a subagent by another agent, which handed you \
the task below. You can't ask it or the user questions, so make reasonable assumptions and \
note them. Work on the task with your tools, then reply with a concise, self-contained \
report of what you found or did (with file paths and line numbers where useful): that \
final reply is the only thing the other agent sees.";

/// Marks a subagent's steps in the transcript.
const STEP_PREFIX: &str = "↳ ";

/// Everything needed to start subagents during a turn.
#[derive(Clone)]
pub struct Subagent {
    pub backend: Backend,
    pub model: String,
    pub config: TurnConfig,
}

impl Subagent {
    /// Runs the task to completion and returns the final answer as a tool result. Boxed:
    /// the subagent's own tool calls lead back here (and are refused there).
    pub fn run<'a>(
        &'a self,
        args: &'a Value,
        ctx: TurnCtx,
        workspace: &'a Workspace,
        tx: &'a mpsc::UnboundedSender<AgentEvent>,
    ) -> BoxFuture<'a, (String, bool)> {
        self.run_inner(args, ctx, workspace, tx).boxed()
    }

    async fn run_inner(
        &self,
        args: &Value,
        ctx: TurnCtx,
        workspace: &Workspace,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> (String, bool) {
        let task = args
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if task.is_empty() {
            return ("Error: `prompt` must describe the task.".into(), true);
        }
        let prompt = format!("{PREAMBLE}\n\n<task>\n{task}\n</task>");
        let turn = Turn {
            model: self.model.clone(),
            history: vec![Message::user(prompt.clone())],
            prompt,
            thread: None,
            config: self.config,
            workspace: workspace.clone(),
        };
        let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();
        let run = self.backend.clone().run(turn, ctx, sub_tx);
        let mut outcome = Outcome::default();
        let forward = async {
            while let Some(event) = sub_rx.recv().await {
                outcome.observe(event, tx);
            }
        };
        tokio::join!(run, forward);
        outcome.result()
    }
}

#[derive(Default)]
struct Outcome {
    history: Vec<Message>,
    errors: Vec<String>,
}

impl Outcome {
    /// Passes the subagent's steps and approval requests on to the parent turn, and keeps
    /// what's needed for the result.
    fn observe(&mut self, event: AgentEvent, tx: &mpsc::UnboundedSender<AgentEvent>) {
        let forwarded = match event {
            AgentEvent::ToolCall {
                name,
                summary,
                description,
            } => AgentEvent::ToolCall {
                name,
                summary,
                description: format!("{STEP_PREFIX}{description}"),
            },
            AgentEvent::Approve {
                name,
                summary,
                description,
                always,
                reply,
            } => AgentEvent::Approve {
                name,
                summary,
                description: format!("{STEP_PREFIX}{description}"),
                always,
                reply,
            },
            event @ AgentEvent::ToolResult { .. } => event,
            // Counted in the session totals, but not as the parent's context size.
            AgentEvent::Usage(usage) => AgentEvent::SubagentUsage(usage),
            AgentEvent::History(history) => {
                self.history = history;
                return;
            }
            AgentEvent::Error(error) => {
                self.errors.push(error);
                return;
            }
            // Its text is returned at the end; the rest is the subagent's own business.
            _ => return,
        };
        let _ = tx.send(forwarded);
    }

    fn result(self) -> (String, bool) {
        let answer = match self.history.last() {
            Some(Message::Assistant { content, .. }) => content
                .iter()
                .filter_map(|c| match c {
                    AssistantContent::Text(t) => Some(t.text.trim()),
                    _ => None,
                })
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n"),
            _ => String::new(),
        };
        match (answer.is_empty(), self.errors.is_empty()) {
            (false, true) => (answer, false),
            (false, false) => (
                format!(
                    "{answer}\n\n(The subagent also reported: {})",
                    self.errors.join("; ")
                ),
                false,
            ),
            (true, false) => (
                format!("Error: the subagent failed: {}", self.errors.join("; ")),
                true,
            ),
            (true, true) => (
                "Error: the subagent finished without a final answer.".into(),
                true,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use rig_core::completion::Usage;
    use serde_json::json;

    use super::*;
    use crate::config::Settings;
    use crate::permissions::Policy;

    #[test]
    fn forwards_steps_and_returns_the_final_answer() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut outcome = Outcome::default();
        outcome.observe(
            AgentEvent::ToolCall {
                name: "grep".into(),
                summary: "fn main".into(),
                description: "Find the entry point".into(),
            },
            &tx,
        );
        outcome.observe(
            AgentEvent::ToolResult {
                output: "src/main.rs:1".into(),
                is_error: false,
            },
            &tx,
        );
        outcome.observe(AgentEvent::Usage(Usage::new()), &tx);
        outcome.observe(AgentEvent::TextDelta("Found".into()), &tx);
        outcome.observe(
            AgentEvent::History(vec![
                Message::user("task"),
                Message::assistant("It's in src/main.rs."),
            ]),
            &tx,
        );
        outcome.observe(AgentEvent::Done, &tx);

        assert!(
            matches!(rx.try_recv(), Ok(AgentEvent::ToolCall { description, .. })
            if description == "↳ Find the entry point")
        );
        assert!(matches!(rx.try_recv(), Ok(AgentEvent::ToolResult { .. })));
        assert!(matches!(rx.try_recv(), Ok(AgentEvent::SubagentUsage(_))));
        assert!(
            rx.try_recv().is_err(),
            "text and history stay with the subagent"
        );
        assert_eq!(outcome.result(), ("It's in src/main.rs.".into(), false));
    }

    #[test]
    fn failures_become_error_results() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut outcome = Outcome::default();
        outcome.observe(AgentEvent::Error("overloaded".into()), &tx);
        assert_eq!(
            outcome.result(),
            ("Error: the subagent failed: overloaded".into(), true)
        );
        // A question with no answer yet isn't an answer.
        let mut outcome = Outcome::default();
        outcome.observe(AgentEvent::History(vec![Message::user("task")]), &tx);
        assert!(outcome.result().1);
    }

    #[tokio::test]
    async fn subagents_cant_nest_and_need_a_prompt() {
        let settings = Settings::default();
        let workspace = Workspace::current().unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let permissions = Policy::new(&settings.permissions, workspace.cwd.clone()).shared();
        let mut ctx = TurnCtx::new(permissions.clone(), None);
        let (output, is_error) = ctx
            .call_tool("agent", json!({"prompt": "look around"}), &workspace, &tx)
            .await;
        assert!(
            is_error && output.contains("can't start subagents"),
            "{output}"
        );

        let subagent = Subagent {
            backend: Backend::Api(rig_core::providers::anthropic::Client::new("k").unwrap()),
            model: settings.model.name.clone(),
            config: settings.turn_config(0),
        };
        let mut ctx = TurnCtx::new(permissions, None).with_subagent(subagent);
        let (output, is_error) = ctx
            .call_tool("agent", json!({"prompt": " "}), &workspace, &tx)
            .await;
        assert!(is_error && output.contains("`prompt`"), "{output}");
    }
}
