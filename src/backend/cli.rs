//! What the Claude Code, Codex, and Antigravity backends share. Each runs a signed-in CLI
//! headlessly for a turn, and a conversation the CLI doesn't hold is replayed to it as text.
//! What differs is each CLI's protocol, its events and its account, which stay in
//! `claude_code`, `codex`, and `antigravity`.

use std::collections::VecDeque;
use std::path::Path;
use std::process::{ExitStatus, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use rig_core::completion::message::{ToolResultContent, UserContent};
use rig_core::completion::{AssistantContent, Message, Usage};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

use super::Turn;
use super::mcp::Tools;
use crate::agent::{self, AgentEvent, TurnCtx};

#[cfg(test)]
mod tests;

/// Tool results longer than this are shortened when replaying a conversation as text.
const SEED_RESULT_CHARS: usize = 2_000;
/// Lines of stderr kept for error messages.
const STDERR_LINES: usize = 20;

/// A CLI backend, as far as running turns through it goes.
pub(super) trait Cli: Sized {
    /// What the CLI is called in messages.
    const NAME: &'static str;
    /// What a failed run's error says, in lowercase, when the CLI isn't signed in.
    const SIGN_IN_ERRORS: &'static [&'static str];

    /// Runs one turn: the whole tool loop happens inside one run of the CLI.
    async fn run_turn(
        &self,
        turn: Turn,
        ctx: TurnCtx,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<()>;

    /// Asks the model to summarize `history` and returns the replacement history. A
    /// continued session summarizes from its own context; otherwise the history is replayed.
    async fn summarize(
        &self,
        turn: &Turn,
        history: &[Message],
        instructions: Option<&str>,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Vec<Message>>;

    /// Runs one turn and reports how it went.
    async fn run(self, turn: Turn, ctx: TurnCtx, tx: mpsc::UnboundedSender<AgentEvent>) {
        if let Err(e) = self.run_turn(turn, ctx, &tx).await {
            let _ = tx.send(AgentEvent::Error(format!("{e:#}")));
        }
        let _ = tx.send(AgentEvent::Done);
    }

    /// Summarizes the history (the `/compact` command).
    async fn compact(
        self,
        turn: Turn,
        instructions: Option<String>,
        tx: mpsc::UnboundedSender<AgentEvent>,
    ) {
        let result = async {
            if turn.history.is_empty() {
                bail!("nothing to compact");
            }
            let history = self
                .summarize(&turn, &turn.history, instructions.as_deref(), &tx)
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
}

/// What tool calls can do during a run of `turn`. Without a `ctx` the tools stay listed
/// (the history may contain calls to them) but can't run, as while compacting.
pub(super) fn tools(
    turn: &Turn,
    ctx: Option<TurnCtx>,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) -> Tools {
    Tools {
        ctx: ctx.map(Mutex::new),
        workspace: turn.workspace.clone(),
        tx: tx.clone(),
        chef: turn.config.chef,
        results: None,
    }
}

/// How a run ended, as the CLI reported it.
#[derive(Debug, PartialEq)]
pub(super) struct Outcome {
    pub(super) is_error: bool,
    pub(super) text: String,
}

impl Outcome {
    /// The run's error, if it failed. One that reads like not being signed in points to
    /// `/login`.
    pub(super) fn into_result<C: Cli>(self) -> Result<()> {
        if !self.is_error {
            return Ok(());
        }
        let name = C::NAME;
        let text = if self.text.trim().is_empty() {
            format!("{name} reported an error")
        } else {
            self.text.trim().to_string()
        };
        let lower = text.to_lowercase();
        if C::SIGN_IN_ERRORS.iter().any(|sign| lower.contains(sign)) {
            bail!("{text}\nRun /login to sign in to {name}.");
        }
        bail!("{text}")
    }
}

/// How a CLI's process ended, for when it stopped without reporting an outcome.
pub(super) struct Exit {
    status: ExitStatus,
    stderr: AbortOnDrop<String>,
}

impl Exit {
    /// The error for a run of `name` that ended without saying how it went, with the end
    /// of what it wrote to stderr.
    pub(super) async fn unfinished(mut self, name: &str) -> anyhow::Error {
        let stderr = (&mut self.stderr.0).await.unwrap_or_default();
        let detail = if stderr.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", stderr.trim())
        };
        anyhow!(
            "{name} exited ({}) before finishing the response{detail}",
            self.status
        )
    }
}

/// Runs a CLI once in `cwd`, feeding each JSON line of its output to `on`.
pub(super) async fn stream(
    mut command: Command,
    cwd: &Path,
    prompt: String,
    mut on: impl FnMut(Value),
) -> Result<Exit> {
    let mut child = command
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            let program = Path::new(command.as_std().get_program());
            format!("couldn't start {}", program.display())
        })?;

    // The prompt goes through stdin: a replayed conversation can exceed argument limits.
    let mut stdin = child.stdin.take().context("no stdin")?;
    let _writer = AbortOnDrop(tokio::spawn(async move {
        let _ = stdin.write_all(prompt.as_bytes()).await;
    }));
    let stderr = stderr_tail(child.stderr.take().context("no stderr")?);

    let mut lines = BufReader::new(child.stdout.take().context("no stdout")?).lines();
    while let Some(line) = lines.next_line().await? {
        if let Ok(event) = serde_json::from_str::<Value>(&line) {
            on(event);
        }
    }
    let status = child.wait().await?;
    Ok(Exit { status, stderr })
}

/// Aborts a helper task when the run ends (or is interrupted).
pub(super) struct AbortOnDrop<T>(pub(super) JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Collects the last lines of a CLI's stderr, for error messages.
pub(super) fn stderr_tail(stderr: tokio::process::ChildStderr) -> AbortOnDrop<String> {
    AbortOnDrop(tokio::spawn(async move {
        let mut tail = VecDeque::new();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tail.len() == STDERR_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
        Vec::from(tail).join("\n")
    }))
}

/// The request that asks the model for a summary of the conversation, with the user's own
/// instructions for it.
pub(super) fn summary_request(instructions: Option<&str>) -> String {
    let mut request = agent::COMPACT_PROMPT.to_string();
    if let Some(extra) = instructions.filter(|s| !s.trim().is_empty()) {
        request.push_str(&format!(
            "\n\nAdditional instructions from the user: {extra}"
        ));
    }
    request
}

/// The history that replaces a conversation summarized as `summary`, reported with the
/// context it took up `before` and the `usage` of summarizing it.
pub(super) fn summarized(
    summary: &str,
    before: u64,
    usage: Usage,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) -> Result<Vec<Message>> {
    if summary.trim().is_empty() {
        bail!("the model returned an empty summary");
    }
    let _ = tx.send(AgentEvent::Compacted {
        before,
        after: usage.output_tokens,
        usage,
    });
    Ok(agent::summary_history(summary))
}

/// The history before the newest prompt (which is the last text of the last user message).
pub(super) fn without_prompt(history: &[Message], prompt: &str) -> Vec<Message> {
    let mut prior = history.to_vec();
    if let Some(Message::User { content }) = prior.last_mut()
        && matches!(content.last(), Some(UserContent::Text(t)) if t.text == prompt)
    {
        content.pop();
        if content.is_empty() {
            prior.pop();
        }
    }
    prior
}

/// The prompt for a new CLI session, with any earlier conversation replayed as a
/// transcript (after `/resume`, `/undo`, compaction, or a backend switch).
pub(super) fn seed(history: &[Message], prompt: &str) -> String {
    if history.is_empty() {
        return prompt.to_string();
    }
    let mut out = String::from(
        "<previous_conversation>\nThis conversation started before this session. The \
transcript so far (long tool results shortened):\n",
    );
    for message in history {
        match message {
            Message::User { content } => {
                for item in content {
                    match item {
                        UserContent::Text(t) => out.push_str(&format!("\n[user]\n{}\n", t.text)),
                        UserContent::ToolResult(r) => {
                            let text: Vec<&str> = r
                                .content
                                .iter()
                                .filter_map(|c| match c {
                                    ToolResultContent::Text(t) => Some(t.text.as_str()),
                                    _ => None,
                                })
                                .collect();
                            out.push_str(&format!(
                                "[tool result]\n{}\n",
                                shorten(&text.join("\n"), SEED_RESULT_CHARS)
                            ));
                        }
                        _ => {}
                    }
                }
            }
            Message::Assistant { content, .. } => {
                for item in content {
                    match item {
                        AssistantContent::Text(t) => {
                            out.push_str(&format!("\n[assistant]\n{}\n", t.text))
                        }
                        AssistantContent::ToolCall(call) => out.push_str(&format!(
                            "[tool call] {} {}\n",
                            call.function.name, call.function.arguments
                        )),
                        _ => {}
                    }
                }
            }
            Message::System { .. } => {}
        }
    }
    out.push_str("</previous_conversation>\n\n");
    out.push_str(prompt);
    out
}

fn shorten(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}… [shortened]", &text[..i]),
        None => text.to_string(),
    }
}

/// Runs a CLI's interactive sign-in (`what`, as typed) in this terminal, and says how it
/// exited.
pub(super) async fn sign_in(mut command: Command, what: &str) -> Result<ExitStatus> {
    let mut child = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("couldn't run `{what}`"))?;
    // Ctrl+C is meant for the sign-in, which shares tamale's process group.
    loop {
        tokio::select! {
            status = child.wait() => break Ok(status?),
            _ = tokio::signal::ctrl_c() => {}
        }
    }
}

/// Whether a sign-in to `C` went through, from how its command (`what`) exited and
/// whether the login is signed in now.
pub(super) fn signed_in<C: Cli>(
    what: &str,
    success: bool,
    status: &str,
    logged_in: bool,
) -> Result<()> {
    let name = C::NAME;
    if !success {
        let still = if logged_in {
            "; still signed in to the previous account"
        } else {
            ""
        };
        bail!("{name} sign-in didn't complete (`{what}` {status}){still}");
    }
    if !logged_in {
        bail!("{name} sign-in didn't complete");
    }
    Ok(())
}

/// Runs a CLI's sign-out (`what`, as typed), which removes the login's stored credentials.
pub(super) async fn sign_out(mut command: Command, what: &str) -> Result<()> {
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .with_context(|| format!("couldn't run `{what}`"))?;
    anyhow::ensure!(status.success(), "`{what}` {status}");
    Ok(())
}

/// A login's label plus its account's email, to tell logins apart.
pub(super) fn label_with_account(label: String, logged_in: bool, email: Option<&str>) -> String {
    match email {
        Some(email) if logged_in => format!("{label} · {email}"),
        _ => label,
    }
}

pub(super) fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
