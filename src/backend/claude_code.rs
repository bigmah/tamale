//! The Claude Code backend: inference comes from a signed-in `claude` CLI, run headlessly
//! with `-p` for each turn.
//!
//! Claude Code's built-in tools, settings, hooks, skills and CLAUDE.md loading are turned
//! off. tamale's system prompt and tools are passed in instead (the tools through the
//! [`mcp`] bridge), so tool calls get the same permission prompts and checkpoints as on
//! the API backend. The stream-json output is translated into the same agent events and
//! history.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use rig_core::completion::message::{ToolResultContent, UserContent};
use rig_core::completion::{AssistantContent, Message, Usage};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

use super::Turn;
use super::mcp::{self, Bridge, Tools};
use crate::agent::{self, AgentEvent, Reasoning, Thinking, TurnCtx};
use crate::tools;

#[cfg(test)]
mod tests;

/// Tool results longer than this are shortened when replaying a conversation as text.
const SEED_RESULT_CHARS: usize = 2_000;
/// Lines of stderr kept for error messages.
const STDERR_LINES: usize = 20;

/// `claude auth status --json`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    #[serde(default)]
    pub logged_in: bool,
    pub auth_method: Option<String>,
    pub subscription_type: Option<String>,
    pub email: Option<String>,
    pub org_id: Option<String>,
}

/// One Claude Code login. Each login lives in its own Claude Code config directory.
#[derive(Clone)]
pub struct ClaudeCode {
    program: PathBuf,
    /// `CLAUDE_CONFIG_DIR` for this login; `None` is Claude Code's default (or the
    /// variable tamale was started with).
    pub config_dir: Option<PathBuf>,
    pub account: Account,
}

impl ClaudeCode {
    /// Finds the CLI and reads its sign-in state. Being signed out is not an error here;
    /// callers decide whether to sign in.
    pub async fn connect(program: PathBuf, config_dir: Option<PathBuf>) -> Result<Self> {
        let mut cc = Self::signed_out(program, config_dir);
        cc.refresh().await?;
        Ok(cc)
    }

    /// A login that hasn't been checked yet (e.g. a new directory about to be signed in).
    pub fn signed_out(program: PathBuf, config_dir: Option<PathBuf>) -> Self {
        Self {
            program,
            config_dir,
            account: Account::default(),
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        // An API key in the environment takes precedence over the Claude Code sign-in;
        // the API backend is the way to use one.
        cmd.env_remove("ANTHROPIC_API_KEY");
        if let Some(dir) = &self.config_dir {
            cmd.env("CLAUDE_CONFIG_DIR", dir);
        }
        cmd
    }

    /// Signs this login out (`claude auth logout`), removing its stored credentials.
    pub async fn logout(&self) -> Result<()> {
        let status = self
            .command()
            .args(["auth", "logout"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .context("couldn't run `claude auth logout`")?;
        anyhow::ensure!(status.success(), "`claude auth logout` {status}");
        Ok(())
    }

    /// Whether both are signed in to the same account and organization.
    pub fn same_account(&self, other: &ClaudeCode) -> bool {
        let (a, b) = (&self.account, &other.account);
        a.logged_in && b.logged_in && a.email == b.email && a.org_id == b.org_id
    }

    /// The label plus the account's email, to tell logins apart.
    pub fn label_with_account(&self) -> String {
        match self.account.email.as_deref() {
            Some(email) if self.account.logged_in => format!("{} · {email}", self.label()),
            _ => self.label(),
        }
    }

    pub async fn refresh(&mut self) -> Result<()> {
        let output = self
            .command()
            .args(["auth", "status", "--json"])
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| {
                format!(
                    "couldn't run `{} auth status`. Is Claude Code installed? Set \
backend.claude_path if it isn't on PATH",
                    self.program.display()
                )
            })?;
        self.account = serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "unexpected `claude auth status` output (Claude Code may be too old): {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })?;
        Ok(())
    }

    /// Runs the interactive `claude auth login` in this terminal, then re-reads the
    /// sign-in state.
    pub async fn login(&mut self) -> Result<()> {
        let mut child = self
            .command()
            .args(["auth", "login"])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .context("couldn't run `claude auth login`")?;
        // Ctrl+C is meant for the sign-in, which shares tamale's process group.
        let status = loop {
            tokio::select! {
                status = child.wait() => break status?,
                _ = tokio::signal::ctrl_c() => {}
            }
        };
        self.refresh().await?;
        if !status.success() {
            let still = if self.account.logged_in {
                "; still signed in to the previous account"
            } else {
                ""
            };
            bail!("Claude Code sign-in didn't complete (`claude auth login` {status}){still}");
        }
        if !self.account.logged_in {
            bail!("Claude Code sign-in didn't complete");
        }
        Ok(())
    }

    /// Where inference comes from, e.g. "Claude Code · Max".
    pub fn label(&self) -> String {
        let account = &self.account;
        if !account.logged_in {
            return "Claude Code (signed out)".into();
        }
        match (
            account
                .subscription_type
                .as_deref()
                .filter(|s| !s.is_empty()),
            account.auth_method.as_deref(),
        ) {
            (Some(plan), _) => format!("Claude Code · {}", capitalize(plan)),
            (None, Some("api_key")) => "Claude Code · API key".into(),
            _ => "Claude Code".into(),
        }
    }

    /// Runs one turn: the whole tool loop happens inside one `claude -p` run.
    pub async fn run(self, turn: Turn, ctx: TurnCtx, tx: mpsc::UnboundedSender<AgentEvent>) {
        if let Err(e) = self.run_turn(turn, ctx, &tx).await {
            let _ = tx.send(AgentEvent::Error(format!("{e:#}")));
        }
        let _ = tx.send(AgentEvent::Done);
    }

    /// Summarizes the history (the `/compact` command).
    pub async fn compact(
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

    async fn run_turn(
        &self,
        mut turn: Turn,
        ctx: TurnCtx,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        // Claude Code compacts within a run; this mirrors the API backend's check before
        // each turn so the transcript and saved history shrink too.
        if turn.config.should_compact() && turn.history.len() > 1 {
            let prior = without_prompt(&turn.history, &turn.prompt);
            let mut history = self.summarize(&turn, &prior, None, tx).await?;
            agent::push_user_text(&mut history, turn.prompt.clone());
            turn.history = history;
            turn.thread = None;
            let _ = tx.send(AgentEvent::History(turn.history.clone()));
        }
        let prompt = match turn.thread {
            Some(_) => turn.prompt.clone(),
            None => seed(&without_prompt(&turn.history, &turn.prompt), &turn.prompt),
        };
        let tools = Tools {
            ctx: Some(Mutex::new(ctx)),
            workspace: turn.workspace.clone(),
            tx: tx.clone(),
        };
        let run = Run {
            prompt,
            resume: turn.thread.clone(),
            persist: true,
            max_tokens: turn.config.limits.max_tokens,
            reasoning: turn.config.reasoning,
        };
        let mut recorder = Recorder::new(turn.history.clone(), tx);
        let outcome = self
            .exchange(&turn, run, tools, |update| recorder.apply(update))
            .await?;
        outcome.into_result()
    }

    /// Asks the model to summarize `history` and returns the replacement history. A
    /// continued session summarizes from its own context; otherwise the history is replayed.
    async fn summarize(
        &self,
        turn: &Turn,
        history: &[Message],
        instructions: Option<&str>,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Vec<Message>> {
        let _ = tx.send(AgentEvent::Compacting);
        let mut request = agent::COMPACT_PROMPT.to_string();
        if let Some(extra) = instructions.filter(|s| !s.trim().is_empty()) {
            request.push_str(&format!(
                "\n\nAdditional instructions from the user: {extra}"
            ));
        }
        let run = Run {
            prompt: match turn.thread {
                Some(_) => request,
                None => seed(history, &request),
            },
            resume: turn.thread.clone(),
            // The session is replaced by the summary, so the request isn't recorded in it.
            persist: false,
            max_tokens: turn.config.limits.compact_max_tokens,
            reasoning: turn.config.reasoning,
        };
        // The tools stay listed (the history may contain calls to them) but can't run.
        let tools = Tools {
            ctx: None,
            workspace: turn.workspace.clone(),
            tx: tx.clone(),
        };
        let mut summary = String::new();
        let mut usage = Usage::new();
        let mut before = None;
        let outcome = self
            .exchange(turn, run, tools, |update| match update {
                Update::Usage(u) => {
                    before.get_or_insert(agent::context_size(&u) - u.output_tokens);
                    usage += u;
                }
                Update::Assistant { content, .. } => {
                    for block in content {
                        if block["type"] == "text"
                            && let Some(text) = block["text"].as_str()
                        {
                            summary.push_str(text);
                        }
                    }
                }
                _ => {}
            })
            .await?;
        outcome.into_result()?;
        if summary.trim().is_empty() {
            bail!("the model returned an empty summary");
        }
        let _ = tx.send(AgentEvent::Compacted {
            before: before.unwrap_or_default(),
            after: usage.output_tokens,
            usage,
        });
        Ok(agent::summary_history(&summary))
    }

    /// Runs `claude -p` once with the tool bridge, feeding each stream update to `on`.
    async fn exchange(
        &self,
        turn: &Turn,
        run: Run,
        tools: Tools,
        mut on: impl FnMut(Update),
    ) -> Result<Outcome> {
        let bridge = Bridge::start(tools).await?;
        let mut child = self
            .args(turn, &run, &bridge)
            .current_dir(&turn.workspace.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("couldn't start {}", self.program.display()))?;

        // The prompt goes through stdin: a replayed conversation can exceed argument limits.
        let mut stdin = child.stdin.take().context("no stdin")?;
        let prompt = run.prompt;
        let _writer = AbortOnDrop(tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
        }));
        let mut stderr = stderr_tail(child.stderr.take().context("no stderr")?);

        let mut lines = BufReader::new(child.stdout.take().context("no stdout")?).lines();
        let mut parser = Parser::default();
        let mut outcome = None;
        while let Some(line) = lines.next_line().await? {
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match parser.parse(&event) {
                Some(Update::Done(done)) => outcome = Some(done),
                Some(update) => on(update),
                None => {}
            }
        }
        let status = child.wait().await?;
        if let Some(outcome) = outcome {
            return Ok(outcome);
        }
        let stderr = (&mut stderr.0).await.unwrap_or_default();
        let detail = if stderr.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", stderr.trim())
        };
        bail!("Claude Code exited ({status}) before finishing the response{detail}")
    }

    fn args(&self, turn: &Turn, run: &Run, bridge: &Bridge) -> Command {
        let mut cmd = self.command();
        cmd.args([
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            // No built-in tools, settings, hooks, skills or CLAUDE.md: tamale supplies
            // the system prompt and tools.
            "--tools",
            "",
            "--setting-sources",
            "",
            "--disable-slash-commands",
            "--strict-mcp-config",
            "--mcp-config",
            &bridge.config(),
            // tamale asks for approval itself, so Claude Code never prompts.
            "--allowedTools",
            mcp::SERVER_RULE,
            // Re-render the system prompt on resume (added directories change it).
            "--system-prompt-snapshot",
            "off",
            "--model",
            &turn.model,
            "--system-prompt",
            &agent::system_prompt(&turn.workspace, turn.config.web_ui),
        ]);
        if let Some(effort) = run.reasoning.effort {
            cmd.args(["--effort", effort.as_str()]);
        }
        if let Some(id) = &run.resume {
            cmd.args(["--resume", id]);
        }
        if !run.persist {
            cmd.arg("--no-session-persistence");
        }
        cmd.env("CLAUDE_CODE_MAX_OUTPUT_TOKENS", run.max_tokens.to_string())
            // Tool calls can wait on an approval prompt or a long command.
            .env("MCP_TOOL_TIMEOUT", "86400000");
        if run.reasoning.thinking == Thinking::Off {
            cmd.env("MAX_THINKING_TOKENS", "0");
        }
        cmd
    }
}

struct Run {
    prompt: String,
    /// Claude Code session to continue.
    resume: Option<String>,
    persist: bool,
    max_tokens: u64,
    reasoning: Reasoning,
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

pub(super) fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The final `result` record of a run.
#[derive(Debug, PartialEq)]
struct Outcome {
    is_error: bool,
    text: String,
}

impl Outcome {
    fn into_result(self) -> Result<()> {
        if !self.is_error {
            return Ok(());
        }
        let text = if self.text.trim().is_empty() {
            "Claude Code reported an error".to_string()
        } else {
            self.text.trim().to_string()
        };
        let lower = text.to_lowercase();
        if lower.contains("login") || lower.contains("log in") || lower.contains("authenticat") {
            bail!("{text}\nRun /login to sign in to Claude Code.");
        }
        bail!("{text}")
    }
}

/// What a stream-json line means for the turn.
#[derive(Debug, PartialEq)]
enum Update {
    Session(String),
    Text(String),
    /// Usage of one model response, sent when it finishes.
    Usage(Usage),
    /// Completed content blocks. Claude Code sends a response's blocks as separate
    /// messages that share its `id`.
    Assistant {
        id: String,
        content: Vec<Value>,
    },
    ToolResults(Vec<Value>),
    Done(Outcome),
}

#[derive(Default)]
struct Parser {
    /// Usage reported when the current response started (the prompt side).
    started: Option<Usage>,
    /// Whether text was streamed this turn, so the next text block starts a new paragraph.
    texted: bool,
}

impl Parser {
    fn parse(&mut self, event: &Value) -> Option<Update> {
        // Subagent traffic; there are none without built-in tools, but don't mix it in.
        if !event["parent_tool_use_id"].is_null() {
            return None;
        }
        match event["type"].as_str()? {
            "system" if event["subtype"] == "init" => {
                Some(Update::Session(event["session_id"].as_str()?.to_string()))
            }
            "stream_event" => {
                let inner = &event["event"];
                match inner["type"].as_str()? {
                    "message_start" => {
                        self.started = Some(usage(&inner["message"]["usage"], Usage::new()));
                        None
                    }
                    "content_block_start"
                        if inner["content_block"]["type"] == "text" && self.texted =>
                    {
                        Some(Update::Text("\n\n".into()))
                    }
                    "content_block_delta" if inner["delta"]["type"] == "text_delta" => {
                        self.texted = true;
                        Some(Update::Text(inner["delta"]["text"].as_str()?.to_string()))
                    }
                    "message_delta" => {
                        let base = self.started.take().unwrap_or_default();
                        Some(Update::Usage(usage(&inner["usage"], base)))
                    }
                    _ => None,
                }
            }
            "assistant" => Some(Update::Assistant {
                id: event["message"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                content: event["message"]["content"].as_array()?.clone(),
            }),
            "user" => {
                let results: Vec<Value> = event["message"]["content"]
                    .as_array()?
                    .iter()
                    .filter(|block| block["type"] == "tool_result")
                    .cloned()
                    .collect();
                (!results.is_empty()).then_some(Update::ToolResults(results))
            }
            "result" => {
                let text = match event["result"].as_str() {
                    Some(text) => text.to_string(),
                    None => event["errors"]
                        .as_array()
                        .map(|errors| {
                            errors
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default(),
                };
                Some(Update::Done(Outcome {
                    is_error: event["is_error"].as_bool().unwrap_or(false),
                    text,
                }))
            }
            _ => None,
        }
    }
}

/// Anthropic usage JSON; fields missing from `v` keep their value in `base`.
fn usage(v: &Value, base: Usage) -> Usage {
    let get = |key: &str, default: u64| v.get(key).and_then(Value::as_u64).unwrap_or(default);
    let mut u = base;
    u.input_tokens = get("input_tokens", base.input_tokens);
    u.output_tokens = get("output_tokens", base.output_tokens);
    u.cached_input_tokens = get("cache_read_input_tokens", base.cached_input_tokens);
    u.cache_creation_input_tokens = get(
        "cache_creation_input_tokens",
        base.cache_creation_input_tokens,
    );
    u.total_tokens = agent::context_size(&u);
    u
}

/// Turns stream updates into agent events and tamale's own history, so sessions, the
/// transcript and undo work as with the API backend.
struct Recorder<'a> {
    history: Vec<Message>,
    /// Tool names by call id, for the results.
    names: HashMap<String, String>,
    /// Id of the response the last assistant message came from.
    response: Option<String>,
    /// Calls still waiting for their result. History is only reported without any, so
    /// a snapshot never ends in an unanswered call (which the API rejects).
    pending: HashSet<String>,
    tx: &'a mpsc::UnboundedSender<AgentEvent>,
}

impl<'a> Recorder<'a> {
    fn new(history: Vec<Message>, tx: &'a mpsc::UnboundedSender<AgentEvent>) -> Self {
        Self {
            history,
            names: HashMap::new(),
            response: None,
            pending: HashSet::new(),
            tx,
        }
    }

    fn send(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }

    fn snapshot(&self) {
        if self.pending.is_empty() {
            self.send(AgentEvent::History(self.history.clone()));
        }
    }

    fn apply(&mut self, update: Update) {
        match update {
            Update::Session(id) => self.send(AgentEvent::Thread(id)),
            Update::Text(text) => self.send(AgentEvent::TextDelta(text)),
            Update::Usage(usage) => self.send(AgentEvent::Usage(usage)),
            Update::Assistant { id, content } => self.on_assistant(id, content),
            Update::ToolResults(results) => self.on_tool_results(results),
            Update::Done(_) => {}
        }
    }

    fn on_assistant(&mut self, id: String, blocks: Vec<Value>) {
        let mut content = Vec::new();
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or_default();
                    if !text.trim().is_empty() {
                        content.push(AssistantContent::text(text));
                    }
                }
                Some("tool_use") => {
                    let call_id = block["id"].as_str().unwrap_or_default().to_string();
                    let name = tool_name(block["name"].as_str().unwrap_or_default());
                    let args = block["input"].clone();
                    self.send(AgentEvent::ToolCall {
                        name: name.clone(),
                        summary: tools::summarize(&name, &args),
                        description: tools::describe(&name, &args),
                    });
                    self.names.insert(call_id.clone(), name.clone());
                    self.pending.insert(call_id.clone());
                    content.push(AssistantContent::tool_call(call_id, name, args));
                }
                // Thinking stays in Claude Code's session; tamale doesn't display it.
                _ => {}
            }
        }
        if content.is_empty() {
            return;
        }
        let same_response = self.response.as_ref() == Some(&id);
        match self.history.last_mut() {
            Some(Message::Assistant { content: last, .. }) if same_response => {
                last.extend(content);
            }
            _ => self.history.push(Message::Assistant { id: None, content }),
        }
        self.response = Some(id);
        self.snapshot();
    }

    fn on_tool_results(&mut self, results: Vec<Value>) {
        let mut content = Vec::new();
        for result in results {
            let call_id = result["tool_use_id"].as_str().unwrap_or_default();
            self.pending.remove(call_id);
            let name = self.names.get(call_id).cloned().unwrap_or_default();
            let output = result_text(&result["content"]);
            let is_error = result["is_error"].as_bool().unwrap_or(false);
            self.send(AgentEvent::ToolResult {
                output: output.clone(),
                is_error,
            });
            content.push(UserContent::tool_result_from_wire(
                call_id,
                name,
                vec![ToolResultContent::text(output)],
            ));
        }
        self.history.push(Message::User { content });
        self.response = None;
        self.snapshot();
    }
}

fn tool_name(name: &str) -> String {
    name.strip_prefix(mcp::TOOL_PREFIX)
        .unwrap_or(name)
        .to_string()
}

/// A tool result's content: a string or a list of text blocks.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
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

/// The prompt for a new CLI session (Claude Code or Codex), with any earlier conversation
/// replayed as a transcript (after `/resume`, `/undo`, compaction, or a backend switch).
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
