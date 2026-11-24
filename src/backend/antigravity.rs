//! The Antigravity backend: inference comes from a signed-in Antigravity CLI (`agy`,
//! Google), run headlessly with `--input-format stream-json --output-format stream-json`
//! for each turn.
//!
//! The run uses a custom agent that tamale writes to a private temporary directory, which
//! `agy` finds through `--add-dir`. The agent's body replaces Antigravity's system prompt
//! with tamale's, and its tool list leaves out Antigravity's own tools (shell, file edits,
//! browser, subagents, …). The same directory holds an MCP config that points at the
//! [`mcp`] bridge, so tool calls get the same permission prompts and checkpoints as on the
//! other backends. Nothing is written to the user's repository or Antigravity settings.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rig_core::completion::message::{ToolResultContent, UserContent};
use rig_core::completion::{AssistantContent, Message, Usage};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::Turn;
use super::cli::{self, Cli, Outcome, seed, without_prompt};
use super::mcp::{self, Bridge, ToolResults, Tools};
use crate::agent::{self, AgentEvent, Effort, TurnCtx};
use crate::tools;

#[cfg(test)]
mod tests;

/// The custom agent tamale runs `agy` with.
const AGENT: &str = "tamale";
/// How Antigravity names the bridge's tools: `mcp_<server>_<tool>`.
const TOOL_PREFIX: &str = "mcp_tamale_";
/// How long checking the sign-in may take.
const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
/// Effort levels from lowest to highest, as Antigravity names them in model slugs.
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// The model used while tamale's model setting is a Claude model Antigravity doesn't serve.
const DEFAULT_MODEL: &str = "gemini-4-neon";

/// Whether an Antigravity login can be used.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub logged_in: bool,
}

/// The Antigravity CLI's login. Antigravity keeps one login (in the system keychain), so
/// unlike Claude Code and Codex there's one per machine.
#[derive(Clone)]
pub struct Antigravity {
    program: PathBuf,
    pub account: Account,
}

impl Antigravity {
    /// Finds the CLI and reads its sign-in state. Being signed out is not an error here;
    /// callers decide whether to sign in.
    pub async fn connect(program: PathBuf) -> Result<Self> {
        let mut antigravity = Self::signed_out(program);
        // The model list is read alongside, for the status bar's model.
        let lister = antigravity.clone();
        let (status, _) = futures::join!(antigravity.refresh(), lister.catalog());
        status?;
        Ok(antigravity)
    }

    /// A login that hasn't been checked yet.
    pub fn signed_out(program: PathBuf) -> Self {
        Self {
            program,
            account: Account::default(),
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        crate::secure::withhold_secrets(&mut cmd);
        // So a video's scene can narrate itself with the key saved in the web UI too.
        cmd.envs(crate::web::gemini_env());
        cmd
    }

    /// Whether both are signed in (there's only one Antigravity login).
    pub fn same_account(&self, other: &Antigravity) -> bool {
        self.account.logged_in && other.account.logged_in
    }

    /// Checks the sign-in through Antigravity's ACP server, which says "Authentication
    /// required" when signed out. (A headless `agy -p` run would start a browser sign-in
    /// and wait for it instead.)
    pub async fn refresh(&mut self) -> Result<()> {
        let mut child = self
            .command()
            .arg("acp")
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "couldn't run `{} acp`. Is the Antigravity CLI installed? Set \
backend.antigravity_path if `agy` isn't on PATH",
                    self.program.display()
                )
            })?;
        let mut stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let requests = [
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": { "protocolVersion": 1, "clientCapabilities": {} } }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "session/list", "params": {} }),
        ];
        let check = async {
            for request in requests {
                stdin.write_all(format!("{request}\n").as_bytes()).await?;
            }
            stdin.flush().await?;
            let mut lines = BufReader::new(stdout).lines();
            while let Some(line) = lines.next_line().await? {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message["id"] == 2 {
                    return Ok(message);
                }
            }
            bail!("`agy acp` exited without answering")
        };
        let reply = tokio::time::timeout(STATUS_TIMEOUT, check)
            .await
            .context("`agy acp` didn't answer in time")??;
        let _ = child.kill().await;
        self.account = account(&reply)?;
        Ok(())
    }

    /// Runs the interactive sign-in in this terminal, then re-reads the sign-in state.
    pub async fn login(&mut self) -> Result<()> {
        let status = cli::sign_in(self.login_command(), "agy acp --login").await?;
        self.finish_login(status.success(), &status.to_string())
            .await
    }

    pub fn login_command(&self) -> Command {
        let mut command = self.command();
        command.args(["acp", "--login"]);
        command
    }

    pub async fn finish_login(&mut self, success: bool, status: &str) -> Result<()> {
        self.refresh().await?;
        cli::signed_in::<Self>("agy acp --login", success, status, self.account.logged_in)
    }

    /// Where inference comes from.
    pub fn label(&self) -> String {
        if self.account.logged_in {
            "Antigravity".into()
        } else {
            "Antigravity (signed out)".into()
        }
    }

    /// Models as (slug, display name), from `agy models`.
    pub async fn list_models(&self) -> Result<Vec<(String, String)>> {
        self.catalog().await.map(|models| models.to_vec())
    }

    /// The model a turn will use, for the status bar.
    pub fn model_label(&self, name: &str) -> String {
        let catalog = cached_catalog(&self.program);
        model(name, catalog.as_deref())
            .unwrap_or("antigravity default")
            .to_string()
    }

    /// `agy models`, read once per process.
    async fn catalog(&self) -> Result<Arc<[(String, String)]>> {
        if let Some(models) = cached_catalog(&self.program) {
            return Ok(models);
        }
        let output = self
            .command()
            .arg("models")
            .stdin(Stdio::null())
            .output()
            .await
            .context("couldn't run `agy models`")?;
        anyhow::ensure!(
            output.status.success(),
            "`agy models` {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let models: Arc<[(String, String)]> =
            parse_models(&String::from_utf8_lossy(&output.stdout)).into();
        catalogs()
            .lock()
            .unwrap()
            .insert(self.program.clone(), models.clone());
        Ok(models)
    }
}

/// Model lists by executable, as (slug, display name).
type Catalogs = std::sync::Mutex<HashMap<PathBuf, Arc<[(String, String)]>>>;

fn catalogs() -> &'static Catalogs {
    static CATALOGS: OnceLock<Catalogs> = OnceLock::new();
    CATALOGS.get_or_init(Default::default)
}

fn cached_catalog(program: &Path) -> Option<Arc<[(String, String)]>> {
    catalogs().lock().unwrap().get(program).cloned()
}

/// The sign-in state from the ACP reply to `session/list`.
fn account(reply: &Value) -> Result<Account> {
    if reply.get("result").is_some() {
        return Ok(Account { logged_in: true });
    }
    let message = reply["error"]["message"].as_str().unwrap_or_default();
    if message.to_lowercase().contains("auth") {
        return Ok(Account { logged_in: false });
    }
    bail!("`agy acp` couldn't check the sign-in: {message}")
}

/// `agy models` output: a slug and a display name per line, separated by a tab.
fn parse_models(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (slug, name) = line.split_once('\t').unwrap_or((line, line));
            let slug = slug.trim();
            (!slug.is_empty() && !slug.contains(' '))
                .then(|| (slug.to_string(), name.trim().to_string()))
        })
        .collect()
}

/// The model to ask Antigravity for: tamale's model setting when Antigravity knows it (a
/// slug like `gemini-3.8-flash-high`, or one without the effort, `gemini-3.8-flash`).
/// Claude models have to be one of its slugs (`claude-opus-5-5-high`): otherwise it's
/// tamale's Anthropic model setting (the default), and [`DEFAULT_MODEL`] is used when
/// Antigravity offers it (else Antigravity picks). Any other name is passed on, so a typo
/// fails loudly instead of quietly running another model.
pub fn model<'a>(name: &'a str, catalog: Option<&[(String, String)]>) -> Option<&'a str> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let claude = super::is_claude_model(name);
    let known = |name: &str, family: bool| {
        catalog.is_some_and(|models| {
            let base = format!("{name}-");
            models
                .iter()
                .any(|(slug, _)| slug == name || (family && slug.starts_with(&base)))
        })
    };
    if known(name, !claude) || !claude {
        Some(name)
    } else {
        known(DEFAULT_MODEL, true).then_some(DEFAULT_MODEL)
    }
}

/// The `--effort` to pass with `model`. A slug that names its effort can't take another;
/// otherwise the closest level the model has, at most the one asked for.
fn effort(
    model: Option<&str>,
    effort: Option<Effort>,
    catalog: Option<&[(String, String)]>,
) -> Option<&'static str> {
    let wanted = effort?.as_str();
    let wanted_rank = EFFORTS.iter().position(|e| *e == wanted)?;
    let Some(model) = model else {
        // Antigravity's default model goes up to `xhigh`.
        return Some(EFFORTS[wanted_rank.min(3)]);
    };
    if EFFORTS.iter().any(|e| model.ends_with(&format!("-{e}"))) {
        return None;
    }
    let base = format!("{model}-");
    let offered: Vec<usize> = catalog
        .into_iter()
        .flatten()
        .filter_map(|(slug, _)| slug.strip_prefix(&base))
        .filter_map(|level| EFFORTS.iter().position(|e| *e == level))
        .collect();
    if offered.is_empty() {
        return Some(wanted);
    }
    offered
        .iter()
        .copied()
        .filter(|&rank| rank <= wanted_rank)
        .max()
        .or_else(|| offered.iter().copied().min())
        .map(|rank| EFFORTS[rank])
}

impl Cli for Antigravity {
    const NAME: &'static str = "Antigravity";
    const SIGN_IN_ERRORS: &'static [&'static str] = &[
        "authentication",
        "unauthenticated",
        "log in",
        "login",
        "401",
    ];

    /// Runs one turn: the whole tool loop happens inside one `agy` run.
    async fn run_turn(
        &self,
        mut turn: Turn,
        ctx: TurnCtx,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        // As with Claude Code: the same check as the API backend before each turn, so the
        // transcript and saved history shrink too.
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
        let results = ToolResults::default();
        let mut tools = cli::tools(&turn, Some(ctx), tx);
        tools.results = Some(results.clone());
        let mut recorder = Recorder::new(turn.history.clone(), Some(results), tx);
        let outcome = self
            .exchange(&turn, prompt, turn.thread.clone(), tools, |update| {
                recorder.apply(update)
            })
            .await;
        recorder.finish();
        outcome?.into_result::<Self>()
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
        let prompt = match turn.thread {
            Some(_) => request,
            None => seed(history, &request),
        };
        let mut summary = String::new();
        let mut usage = Usage::new();
        let mut before = None;
        let tools = cli::tools(turn, None, tx);
        let outcome = self
            .exchange(
                turn,
                prompt,
                turn.thread.clone(),
                tools,
                |update| match update {
                    Update::Usage { usage: u, .. } => {
                        before.get_or_insert(agent::context_size(&u) - u.output_tokens);
                        usage += u;
                    }
                    Update::Text { text, .. } => summary.push_str(&text),
                    _ => {}
                },
            )
            .await?;
        outcome.into_result::<Self>()?;
        cli::summarized(summary.trim(), before.unwrap_or_default(), usage, tx)
    }
}

impl Antigravity {
    /// Runs `agy` once with the tool bridge, feeding each update to `on`.
    async fn exchange(
        &self,
        turn: &Turn,
        prompt: String,
        resume: Option<String>,
        tools: Tools,
        mut on: impl FnMut(Update),
    ) -> Result<Outcome> {
        let chef = tools.chef;
        let bridge = Bridge::start(tools).await?;
        let setup = Setup::write(
            &agent::instructions(&turn.workspace, &turn.config),
            &bridge,
            chef,
        )?;
        let catalog = self.catalog().await.ok();
        let command = self.args(turn, resume.as_deref(), &setup, catalog.as_deref());
        // The prompt goes in as a stream-json message on stdin: a replayed conversation can
        // exceed argument limits, and closing stdin ends the run after this turn.
        let input = format!(
            "{}\n",
            json!({ "event": "user", "message": { "content": prompt } })
        );
        let mut parser = Parser::default();
        let mut outcome = None;
        let exit = cli::stream(command, &turn.workspace.cwd, input, |event| {
            for update in parser.parse(&event) {
                match update {
                    Update::Done(done) => outcome = Some(done),
                    update => on(update),
                }
            }
        })
        .await?;
        drop(setup);
        match outcome {
            Some(outcome) => Ok(outcome),
            None => Err(exit.unfinished(Self::NAME).await),
        }
    }

    fn args(
        &self,
        turn: &Turn,
        resume: Option<&str>,
        setup: &Setup,
        catalog: Option<&[(String, String)]>,
    ) -> Command {
        let mut cmd = self.command();
        cmd.args([
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            // tamale's agent: its prompt and no built-in tools.
            "--agent",
            AGENT,
            // A prompt that starts with `/` is a message, not an Antigravity command.
            "--disable-slash-commands",
            // A plain mode (not `plan`, whatever the user's settings say).
            "--mode",
            "accept-edits",
            // tamale asks for approval itself, so Antigravity never has to.
            "--settings",
            &settings_json(),
        ]);
        cmd.arg("--add-dir").arg(&setup.dir);
        let model = model(&turn.model, catalog);
        if let Some(model) = model {
            cmd.args(["--model", model]);
        }
        if let Some(effort) = effort(model, turn.config.reasoning.effort, catalog) {
            cmd.args(["--effort", effort]);
        }
        if let Some(id) = resume {
            cmd.args(["--conversation", id]);
        }
        cmd
    }
}

/// Settings for the run only (never saved): calls to tamale's tools need no approval in
/// Antigravity, as tamale asks itself.
fn settings_json() -> String {
    json!({ "permissions": { "allow": [format!("mcp({}/*)", mcp::SERVER)] } }).to_string()
}

/// The agent and MCP config for one run, in a private temporary directory that's removed
/// when the run ends. The MCP config holds the bridge's token: Antigravity doesn't expand
/// environment variables in it, so it's in a file only this user can read.
struct Setup {
    dir: PathBuf,
}

impl Setup {
    fn write(instructions: &str, bridge: &Bridge, chef: bool) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "tamale-agy-{}-{}",
            std::process::id(),
            crate::session::new_id()
        ));
        let setup = Self { dir };
        let agents = setup.dir.join(".agents");
        let agent_dir = agents.join("agents").join(AGENT);
        for dir in [&setup.dir, &agents, &agents.join("agents"), &agent_dir] {
            crate::secure::private_dir(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        crate::secure::write_private(&agent_dir.join("agent.md"), agent_file(instructions))
            .context("writing the Antigravity agent")?;
        let tools: Vec<String> = tools::definitions_for(chef)
            .into_iter()
            .map(|d| d.name)
            .collect();
        crate::secure::write_private(
            &agents.join("mcp_config.json"),
            mcp_config(&bridge.url, &bridge.token, &tools).to_string(),
        )
        .context("writing the Antigravity MCP config")?;
        Ok(setup)
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The custom agent: tamale's instructions as the system prompt, and none of
/// Antigravity's tools (MCP tools are added to every agent). Even `finish` is left out:
/// Antigravity streams its arguments as response text.
fn agent_file(instructions: &str) -> String {
    format!(
        "---\nname: {AGENT}\ndescription: tamale's agent; its tools come from tamale over MCP.\n\
tools: []\nmainAgent: true\nsubagent: false\n---\n{instructions}\n"
    )
}

/// Points Antigravity at the bridge. Every tool is eager: the model sees it directly,
/// rather than through Antigravity's tool for calling MCP tools.
fn mcp_config(url: &str, token: &str, tools: &[String]) -> Value {
    let eager: serde_json::Map<String, Value> = tools
        .iter()
        .map(|name| (name.clone(), json!({ "eager": true })))
        .collect();
    json!({ "mcpServers": { mcp::SERVER: {
        "serverUrl": url,
        "headers": { "Authorization": format!("Bearer {token}") },
        // Tool calls can wait on an approval prompt or a long command.
        "timeoutSeconds": 86400,
        "tools": eager,
    } } })
}

/// Antigravity usage JSON. Its input count leaves out cached tokens, as tamale's does;
/// its output count includes thinking.
fn usage(v: &Value) -> Usage {
    let get = |key: &str| v[key].as_u64().unwrap_or(0);
    let mut u = Usage::new();
    u.input_tokens = get("input_tokens");
    u.cached_input_tokens = get("cache_read_tokens");
    u.output_tokens = get("output_tokens");
    u.total_tokens = agent::context_size(&u);
    u
}

/// A tool step's output without the times Antigravity puts before it.
fn tool_output(output: &str) -> String {
    let mut rest = output;
    for prefix in ["Created At: ", "Completed At: "] {
        if rest.starts_with(prefix) {
            rest = rest.split_once('\n').map_or("", |(_, after)| after);
        }
    }
    rest.to_string()
}

/// What a line of `agy`'s stream means for the turn.
#[derive(Debug, PartialEq)]
enum Update {
    Thread(String),
    /// Streamed text of a response step.
    Text {
        step: u64,
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    ToolResult {
        id: String,
        name: String,
        output: String,
        is_error: bool,
    },
    /// One request's usage; `request` is false for Antigravity's own side requests (its
    /// checkpoints), which count in the totals but aren't the conversation's size.
    Usage {
        usage: Usage,
        request: bool,
    },
    Done(Outcome),
}

#[derive(Default)]
struct Parser {
    thread: Option<String>,
    /// Steps of tamale's tools that have been announced.
    started: HashSet<u64>,
}

impl Parser {
    fn parse(&mut self, event: &Value) -> Vec<Update> {
        let mut updates = Vec::new();
        match event["event"].as_str() {
            Some("init") => self.thread(&event["conversation_id"], &mut updates),
            Some("step_update") => self.step(&event["step_update"], &mut updates),
            Some("result") => {
                let result = &event["result"];
                self.thread(&result["conversation_id"], &mut updates);
                let status = result["status"].as_str().unwrap_or_default();
                let is_error = status != "SUCCESS";
                let text = match result["error"].as_str().filter(|e| !e.trim().is_empty()) {
                    Some(error) => error.to_string(),
                    None if is_error => format!("Antigravity ended the turn: {status}"),
                    None => String::new(),
                };
                updates.push(Update::Done(Outcome { is_error, text }));
            }
            _ => {}
        }
        updates
    }

    fn thread(&mut self, id: &Value, updates: &mut Vec<Update>) {
        if let Some(id) = id.as_str().filter(|id| !id.is_empty())
            && self.thread.as_deref() != Some(id)
        {
            self.thread = Some(id.to_string());
            updates.push(Update::Thread(id.to_string()));
        }
    }

    fn step(&mut self, step: &Value, updates: &mut Vec<Update>) {
        self.thread(&step["conversation_id"], updates);
        let index = step["step_index"].as_u64().unwrap_or_default();
        let done = step["state"] == "DONE";
        match step["step_type"].as_str().unwrap_or_default() {
            "agent_response" => {
                if let Some(text) = step["text_delta"].as_str().filter(|t| !t.is_empty()) {
                    updates.push(Update::Text {
                        step: index,
                        text: text.to_string(),
                    });
                }
            }
            "tool" => self.tool(index, step, done, updates),
            _ => {}
        }
        if done && step["usage"].is_object() {
            updates.push(Update::Usage {
                usage: usage(&step["usage"]),
                request: step["step_type"] != "checkpoint",
            });
        }
    }

    fn tool(&mut self, index: u64, step: &Value, done: bool, updates: &mut Vec<Update>) {
        let info = &step["tool_info"];
        let full = step["tool_name"].as_str().or(info["name"].as_str());
        let Some(name) = full.and_then(|n| n.strip_prefix(TOOL_PREFIX)) else {
            return;
        };
        let id = format!("agy-{}-{index}", self.thread.as_deref().unwrap_or_default());
        if self.started.insert(index) {
            updates.push(Update::ToolCall {
                id: id.clone(),
                name: name.to_string(),
                args: match &info["parameters"] {
                    Value::Null => json!({}),
                    args => args.clone(),
                },
            });
        }
        if done {
            let (output, is_error) = match info["error"]["message"].as_str() {
                Some(message) => (message.to_string(), true),
                None => (
                    tool_output(info["output"].as_str().unwrap_or_default()),
                    false,
                ),
            };
            updates.push(Update::ToolResult {
                id,
                name: name.to_string(),
                output,
                is_error,
            });
        }
    }
}

/// Turns stream updates into agent events and tamale's own history, so sessions, the
/// transcript and undo work as with the API backend.
struct Recorder<'a> {
    history: Vec<Message>,
    /// Tool outputs as tamale's tools returned them (the stream's may be shortened).
    results: Option<ToolResults>,
    /// The response step being streamed, and its text so far.
    text: Option<(u64, String)>,
    /// Calls still waiting for their result. History is only reported without any, so
    /// a snapshot never ends in an unanswered call.
    pending: HashSet<String>,
    /// Whether the last thing shown was assistant text (consecutive responses are
    /// separated by a blank line).
    after_text: bool,
    tx: &'a mpsc::UnboundedSender<AgentEvent>,
}

impl<'a> Recorder<'a> {
    fn new(
        history: Vec<Message>,
        results: Option<ToolResults>,
        tx: &'a mpsc::UnboundedSender<AgentEvent>,
    ) -> Self {
        Self {
            history,
            results,
            text: None,
            pending: HashSet::new(),
            after_text: false,
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
            Update::Thread(id) => self.send(AgentEvent::Thread(id)),
            Update::Text { step, text } => {
                if self.text.as_ref().is_some_and(|(s, _)| *s != step) {
                    self.flush_text();
                }
                if self.text.is_none() && self.after_text {
                    self.send(AgentEvent::TextDelta("\n\n".into()));
                }
                self.send(AgentEvent::TextDelta(text.clone()));
                self.text
                    .get_or_insert((step, String::new()))
                    .1
                    .push_str(&text);
            }
            Update::ToolCall { id, name, args } => {
                self.flush_text();
                self.after_text = false;
                self.send(AgentEvent::ToolCall {
                    name: name.clone(),
                    summary: tools::summarize(&name, &args),
                    description: tools::describe(&name, &args),
                });
                self.pending.insert(id.clone());
                self.push_assistant(AssistantContent::tool_call(id, name, args));
            }
            Update::ToolResult {
                id,
                name,
                output,
                is_error,
            } => {
                self.after_text = false;
                self.pending.remove(&id);
                let (output, is_error) = self.result(&name).unwrap_or((output, is_error));
                self.send(AgentEvent::ToolResult {
                    output: output.clone(),
                    is_error,
                });
                let result = UserContent::tool_result_from_wire(
                    &id,
                    name,
                    vec![ToolResultContent::text(output)],
                );
                self.history.push(Message::User {
                    content: vec![result],
                });
                self.snapshot();
            }
            Update::Usage { usage, request } => self.send(if request {
                AgentEvent::Usage(usage)
            } else {
                AgentEvent::SubagentUsage(usage)
            }),
            Update::Done(_) => {}
        }
    }

    /// The output tamale's tool `name` returned, when it's the next one recorded.
    fn result(&self, name: &str) -> Option<(String, bool)> {
        let mut results = self.results.as_ref()?.lock().unwrap();
        if results.front()?.name != name {
            return None;
        }
        results.pop_front().map(|r| (r.output, r.is_error))
    }

    /// Records the text streamed so far (at the end of a response, or of the run).
    fn flush_text(&mut self) {
        let Some((_, text)) = self.text.take() else {
            return;
        };
        let text = text.trim_end();
        if text.trim().is_empty() {
            return;
        }
        self.after_text = true;
        self.push_assistant(AssistantContent::text(text));
        self.snapshot();
    }

    fn finish(&mut self) {
        self.flush_text();
    }

    /// Adds to the newest assistant message, or starts one after the prompt or results.
    fn push_assistant(&mut self, item: AssistantContent) {
        match self.history.last_mut() {
            Some(Message::Assistant { content, .. }) => content.push(item),
            _ => self.history.push(Message::Assistant {
                id: None,
                content: vec![item],
            }),
        }
    }
}
