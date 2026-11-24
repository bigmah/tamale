//! The Claude Code backend: inference comes from a signed-in `claude` CLI, run headlessly
//! with `-p` for each turn.
//!
//! Claude Code's built-in tools, settings, hooks, skills and CLAUDE.md loading are turned
//! off. tamale's system prompt and tools are passed in instead (the tools through the
//! [`mcp`] bridge), so tool calls get the same permission prompts and checkpoints as on
//! the API backend. The stream-json output is translated into the same agent events and
//! history.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result};
use rig_core::completion::message::{ToolResultContent, UserContent};
use rig_core::completion::{AssistantContent, Message, Usage};
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::mpsc;

use super::Turn;
use super::cli::{self, Cli, Outcome, capitalize, seed, without_prompt};
use super::mcp::{self, Bridge, Tools};
use crate::agent::{self, AgentEvent, Reasoning, Thinking, TurnCtx};
use crate::tools;

#[cfg(test)]
mod tests;

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
        crate::secure::withhold_secrets(&mut cmd);
        // So a video's scene can narrate itself with the key saved in the web UI too.
        cmd.envs(crate::web::gemini_env());
        if let Some(dir) = &self.config_dir {
            cmd.env("CLAUDE_CONFIG_DIR", dir);
        }
        cmd
    }

    /// Signs this login out (`claude auth logout`), removing its stored credentials.
    pub async fn logout(&self) -> Result<()> {
        let mut command = self.command();
        command.args(["auth", "logout"]);
        cli::sign_out(command, "claude auth logout").await
    }

    /// Whether both are signed in to the same account and organization.
    pub fn same_account(&self, other: &ClaudeCode) -> bool {
        let (a, b) = (&self.account, &other.account);
        a.logged_in && b.logged_in && a.email == b.email && a.org_id == b.org_id
    }

    /// The label plus the account's email, to tell logins apart.
    pub fn label_with_account(&self) -> String {
        let account = &self.account;
        cli::label_with_account(self.label(), account.logged_in, account.email.as_deref())
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
        let status = cli::sign_in(self.login_command(), "claude auth login").await?;
        self.finish_login(status.success(), &status.to_string())
            .await
    }

    pub fn login_command(&self) -> Command {
        let mut command = self.command();
        command.args(["auth", "login"]);
        command
    }

    pub async fn finish_login(&mut self, success: bool, status: &str) -> Result<()> {
        self.refresh().await?;
        cli::signed_in::<Self>("claude auth login", success, status, self.account.logged_in)
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
}

impl Cli for ClaudeCode {
    const NAME: &'static str = "Claude Code";
    const SIGN_IN_ERRORS: &'static [&'static str] = &["login", "log in", "authenticat"];

    /// Runs one turn: the whole tool loop happens inside one `claude -p` run.
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
        let run = Run {
            prompt,
            resume: turn.thread.clone(),
            persist: true,
            max_tokens: turn.config.limits.max_tokens,
            reasoning: turn.config.reasoning,
        };
        let mut recorder = Recorder::new(turn.history.clone(), tx);
        let tools = cli::tools(&turn, Some(ctx), tx);
        let outcome = self
            .exchange(&turn, run, tools, |update| recorder.apply(update))
            .await?;
        outcome.into_result::<Self>()
    }

    async fn summarize(
        &self,
        turn: &Turn,
        history: &[Message],
        instructions: Option<&str>,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Vec<Message>> {
        let _ = tx.send(AgentEvent::Compacting);
        let request = cli::summary_request(instructions);
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
        let mut summary = String::new();
        let mut usage = Usage::new();
        let mut before = None;
        let tools = cli::tools(turn, None, tx);
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
        outcome.into_result::<Self>()?;
        cli::summarized(&summary, before.unwrap_or_default(), usage, tx)
    }
}

impl ClaudeCode {
    /// Runs `claude -p` once with the tool bridge, feeding each stream update to `on`.
    async fn exchange(
        &self,
        turn: &Turn,
        run: Run,
        tools: Tools,
        mut on: impl FnMut(Update),
    ) -> Result<Outcome> {
        let bridge = Bridge::start(tools).await?;
        let command = self.args(turn, &run, &bridge);
        let mut parser = Parser::default();
        let mut outcome = None;
        let exit = cli::stream(
            command,
            &turn.workspace.cwd,
            run.prompt,
            |event| match parser.parse(&event) {
                Some(Update::Done(done)) => outcome = Some(done),
                Some(update) => on(update),
                None => {}
            },
        )
        .await?;
        match outcome {
            Some(outcome) => Ok(outcome),
            None => Err(exit.unfinished(Self::NAME).await),
        }
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
            &agent::instructions(&turn.workspace, &turn.config),
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
        cmd.env(mcp::TOKEN_ENV, &bridge.token)
            .env("CLAUDE_CODE_MAX_OUTPUT_TOKENS", run.max_tokens.to_string())
            // Tool calls can wait on an approval prompt or a long command.
            .env("MCP_TOOL_TIMEOUT", "86400000")
            // Newer versions also abort a call after 300s without progress (0 disables).
            .env("CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT", "0");
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
