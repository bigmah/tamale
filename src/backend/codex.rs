//! The Codex backend: inference comes from a signed-in `codex` CLI (OpenAI), run
//! headlessly with `codex exec --json` for each turn.
//!
//! As with Claude Code, Codex's own tools, user config, rules, AGENTS.md and skills are
//! turned off and its sandbox is read-only. tamale's system prompt replaces Codex's
//! instructions and tamale's tools are offered through the [`mcp`] bridge, so tool calls
//! get the same permission prompts and checkpoints as on the other backends.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use rig_core::completion::message::{ToolResultContent, UserContent};
use rig_core::completion::{AssistantContent, Message, Usage};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc};

use super::Turn;
use super::claude_code::{AbortOnDrop, capitalize, seed, stderr_tail, without_prompt};
use super::mcp::{self, Bridge, Tools};
use crate::agent::{self, AgentEvent, Reasoning, TurnCtx};
use crate::tools;

#[cfg(test)]
mod tests;

/// Codex features that add tools or instructions of their own.
const DISABLED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "view_image",
    "image_generation",
    "multi_agent",
    "apps",
    "plugins",
    "browser_use",
    "computer_use",
    "in_app_browser",
    "goals",
    "sleep_tool",
    "tool_suggest",
    "skill_search",
    "memories",
    "hooks",
];

/// Who a Codex login is signed in as: `codex login status`, plus the account details
/// in the login's `auth.json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub logged_in: bool,
    /// `chatgpt` or `api_key`.
    pub auth_method: Option<String>,
    /// The ChatGPT plan (`plus`, `pro`, …).
    pub plan: Option<String>,
    pub email: Option<String>,
    pub account_id: Option<String>,
}

/// One Codex login. Each login lives in its own Codex home directory (`CODEX_HOME`).
#[derive(Clone)]
pub struct Codex {
    program: PathBuf,
    /// `CODEX_HOME` for this login; `None` is Codex's default (or the variable tamale
    /// was started with).
    pub home: Option<PathBuf>,
    pub account: Account,
}

impl Codex {
    /// Finds the CLI and reads its sign-in state. Being signed out is not an error here;
    /// callers decide whether to sign in.
    pub async fn connect(program: PathBuf, home: Option<PathBuf>) -> Result<Self> {
        let mut codex = Self::signed_out(program, home);
        codex.refresh().await?;
        Ok(codex)
    }

    /// A login that hasn't been checked yet (e.g. a new directory about to be signed in).
    pub fn signed_out(program: PathBuf, home: Option<PathBuf>) -> Self {
        Self {
            program,
            home,
            account: Account::default(),
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        if let Some(home) = &self.home {
            cmd.env("CODEX_HOME", home);
        }
        cmd
    }

    /// The login's directory, wherever it comes from.
    fn home_dir(&self) -> Option<PathBuf> {
        self.home
            .clone()
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))
    }

    /// Signs this login out (`codex logout`), removing its stored credentials.
    pub async fn logout(&self) -> Result<()> {
        let status = self
            .command()
            .arg("logout")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .context("couldn't run `codex logout`")?;
        anyhow::ensure!(status.success(), "`codex logout` {status}");
        Ok(())
    }

    /// Whether both are signed in to the same account.
    pub fn same_account(&self, other: &Codex) -> bool {
        let (a, b) = (&self.account, &other.account);
        a.logged_in
            && b.logged_in
            && a.email.is_some()
            && a.email == b.email
            && a.account_id == b.account_id
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
            .args(["login", "status"])
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| {
                format!(
                    "couldn't run `{} login status`. Is Codex installed? Set \
backend.codex_path if it isn't on PATH",
                    self.program.display()
                )
            })?;
        let mut account = Account {
            logged_in: output.status.success(),
            ..Account::default()
        };
        if account.logged_in {
            let text = String::from_utf8_lossy(&output.stdout).to_string()
                + &String::from_utf8_lossy(&output.stderr);
            if text.contains("API key") {
                account.auth_method = Some("api_key".into());
            } else if text.contains("ChatGPT") {
                account.auth_method = Some("chatgpt".into());
            }
            if let Some(auth) = self
                .home_dir()
                .and_then(|home| std::fs::read(home.join("auth.json")).ok())
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            {
                read_auth(&auth, &mut account);
            }
        }
        self.account = account;
        Ok(())
    }

    /// Runs the interactive `codex login` in this terminal, then re-reads the sign-in
    /// state.
    pub async fn login(&mut self) -> Result<()> {
        if let Some(home) = &self.home {
            // Codex refuses a CODEX_HOME that doesn't exist.
            std::fs::create_dir_all(home)
                .with_context(|| format!("creating {}", home.display()))?;
        }
        let mut child = self
            .command()
            .arg("login")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .context("couldn't run `codex login`")?;
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
            bail!("Codex sign-in didn't complete (`codex login` {status}){still}");
        }
        if !self.account.logged_in {
            bail!("Codex sign-in didn't complete");
        }
        Ok(())
    }

    /// Where inference comes from, e.g. "Codex · Plus".
    pub fn label(&self) -> String {
        let account = &self.account;
        if !account.logged_in {
            return "Codex (signed out)".into();
        }
        match (
            account.plan.as_deref().filter(|s| !s.is_empty()),
            account.auth_method.as_deref(),
        ) {
            (_, Some("api_key")) => "Codex · API key".into(),
            (Some(plan), _) => format!("Codex · {}", capitalize(plan)),
            _ => "Codex".into(),
        }
    }

    /// Models from Codex's catalog as (id, display name).
    pub async fn list_models(&self) -> Result<Vec<(String, String)>> {
        let output = self
            .command()
            .args(["debug", "models"])
            .stdin(Stdio::null())
            .output()
            .await
            .context("couldn't run `codex debug models`")?;
        anyhow::ensure!(
            output.status.success(),
            "`codex debug models` {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let catalog: Value = serde_json::from_slice(&output.stdout)
            .context("unexpected `codex debug models` output")?;
        Ok(catalog["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["visibility"] == "list")
            .filter_map(|m| {
                let id = m["slug"].as_str()?.to_string();
                let name = m["display_name"].as_str().unwrap_or(&id).to_string();
                Some((id, name))
            })
            .collect())
    }

    /// Runs one turn: the whole tool loop happens inside one `codex exec` run.
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

    // Unlike the other backends there's no compaction check before a turn: Codex
    // compacts its own thread, and only reports usage summed over a whole turn.
    async fn run_turn(
        &self,
        turn: Turn,
        ctx: TurnCtx,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<()> {
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
            reasoning: turn.config.reasoning,
        };
        let mut recorder = Recorder::new(turn.history.clone(), tx);
        let mut thread = turn.thread.clone();
        let mut usage = None;
        let outcome = self
            .exchange(&turn, run, tools, |update| match update {
                Update::Usage(u) => usage = Some(u),
                Update::Thread(id) => {
                    thread = Some(id.clone());
                    recorder.apply(Update::Thread(id));
                }
                update => recorder.apply(update),
            })
            .await?;
        if let Some(total) = usage {
            self.report_usage(total, thread.as_deref(), tx);
        }
        outcome.into_result()
    }

    /// Codex only reports a turn's total usage. The context meter needs the size of the
    /// last request, which Codex records in its session file; when that can be read, the
    /// total is reported as two parts so the last one is that request.
    fn report_usage(
        &self,
        total: Usage,
        thread: Option<&str>,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let last = thread
            .zip(self.home_dir())
            .and_then(|(id, home)| last_request_usage(&home, id))
            .filter(|last| {
                last.input_tokens <= total.input_tokens
                    && last.cached_input_tokens <= total.cached_input_tokens
                    && last.output_tokens <= total.output_tokens
            });
        let Some(last) = last else {
            let _ = tx.send(AgentEvent::Usage(total));
            return;
        };
        let mut rest = Usage::new();
        rest.input_tokens = total.input_tokens - last.input_tokens;
        rest.cached_input_tokens = total.cached_input_tokens - last.cached_input_tokens;
        rest.cache_creation_input_tokens = total
            .cache_creation_input_tokens
            .saturating_sub(last.cache_creation_input_tokens);
        rest.output_tokens = total.output_tokens - last.output_tokens;
        rest.total_tokens = agent::context_size(&rest);
        if rest.total_tokens > 0 {
            let _ = tx.send(AgentEvent::Usage(rest));
        }
        let _ = tx.send(AgentEvent::Usage(last));
    }

    /// Asks the model to summarize `history` and returns the replacement history. A
    /// continued thread summarizes from its own context; otherwise the history is replayed.
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
            // The thread is replaced by the summary, so a new one isn't saved. (A resumed
            // thread still records the request, but it's dropped after compacting.)
            persist: false,
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
        let outcome = self
            .exchange(turn, run, tools, |update| match update {
                Update::Usage(u) => usage = u,
                Update::Text(text) => {
                    if !summary.is_empty() {
                        summary.push_str("\n\n");
                    }
                    summary.push_str(&text);
                }
                _ => {}
            })
            .await?;
        outcome.into_result()?;
        if summary.trim().is_empty() {
            bail!("the model returned an empty summary");
        }
        let _ = tx.send(AgentEvent::Compacted {
            before: agent::context_size(&usage) - usage.output_tokens,
            after: usage.output_tokens,
            usage,
        });
        Ok(agent::summary_history(&summary))
    }

    /// Runs `codex exec` once with the tool bridge, feeding each event to `on`.
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
        if parser.completed {
            return Ok(Outcome {
                is_error: false,
                text: String::new(),
            });
        }
        if let Some(error) = parser.error {
            return Ok(Outcome {
                is_error: true,
                text: error,
            });
        }
        let stderr = (&mut stderr.0).await.unwrap_or_default();
        let detail = if stderr.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", stderr.trim())
        };
        bail!("Codex exited ({status}) before finishing the response{detail}")
    }

    fn args(&self, turn: &Turn, run: &Run, bridge: &Bridge) -> Command {
        let mut cmd = self.command();
        cmd.arg("exec");
        if let Some(id) = &run.resume {
            cmd.args(["resume", id]);
        }
        cmd.args([
            "--json",
            "--skip-git-repo-check",
            // No user config, rules, AGENTS.md, skills or built-in tools: tamale supplies
            // the instructions and tools.
            "--ignore-user-config",
            "--ignore-rules",
        ]);
        for feature in DISABLED_FEATURES {
            cmd.args(["--disable", feature]);
        }
        let mut config = vec![
            (
                "instructions",
                toml_string(&agent::system_prompt(&turn.workspace)),
            ),
            ("project_doc_max_bytes", "0".into()),
            ("skills.bundled.enabled", "false".into()),
            ("include_environment_context", "false".into()),
            ("include_permissions_instructions", "false".into()),
            ("include_apps_instructions", "false".into()),
            ("include_collaboration_mode_instructions", "false".into()),
            ("web_search", toml_string("disabled")),
            // Anything left of Codex's own tooling can't change files or run commands,
            // and never waits for an approval nobody can give.
            ("sandbox_mode", toml_string("read-only")),
            ("approval_policy", toml_string("never")),
            ("mcp_servers.tamale.url", toml_string(&bridge.url)),
            // tamale asks for approval itself, so Codex never prompts.
            (
                "mcp_servers.tamale.default_tools_approval_mode",
                toml_string("approve"),
            ),
            // Tool calls can wait on an approval prompt or a long command.
            ("mcp_servers.tamale.tool_timeout_sec", "86400".into()),
        ];
        if let Some(effort) = run.reasoning.effort {
            config.push(("model_reasoning_effort", toml_string(effort.as_str())));
        }
        for (key, value) in config {
            cmd.arg("-c").arg(format!("{key}={value}"));
        }
        if let Some(model) = model(&turn.model) {
            cmd.args(["-m", model]);
        }
        if !run.persist {
            cmd.arg("--ephemeral");
        }
        // Read the prompt from stdin.
        cmd.arg("-");
        cmd
    }
}

/// The model to ask Codex for: tamale's model setting, unless it names a Claude model
/// (the default), in which case Codex picks.
pub fn model(name: &str) -> Option<&str> {
    let name = name.trim();
    (!name.is_empty() && !name.starts_with("claude") && !name.starts_with("anthropic/"))
        .then_some(name)
}

struct Run {
    prompt: String,
    /// Codex thread to continue.
    resume: Option<String>,
    persist: bool,
    reasoning: Reasoning,
}

/// A TOML string literal for a `-c key=value` override. JSON string escapes are valid
/// TOML basic-string escapes.
fn toml_string(s: &str) -> String {
    Value::from(s).to_string()
}

/// Fills in the account details from a login's `auth.json`. The email and plan are in
/// the ID token's claims.
fn read_auth(auth: &Value, account: &mut Account) {
    if let Some(mode) = auth["auth_mode"].as_str() {
        account
            .auth_method
            .get_or_insert_with(|| mode.to_ascii_lowercase());
    }
    let tokens = &auth["tokens"];
    account.account_id = tokens["account_id"].as_str().map(str::to_string);
    let Some(claims) = tokens["id_token"].as_str().and_then(jwt_claims) else {
        return;
    };
    account.email = claims["email"].as_str().map(str::to_string);
    let openai = &claims["https://api.openai.com/auth"];
    account.plan = openai["chatgpt_plan_type"].as_str().map(str::to_string);
    if account.account_id.is_none() {
        account.account_id = openai["chatgpt_account_id"].as_str().map(str::to_string);
    }
}

/// The (unverified) claims of a JWT.
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&base64url(payload)?).ok()
}

fn base64url(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let (mut bits, mut acc) = (0u32, 0u32);
    for c in input.bytes().take_while(|&c| c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Usage of the last request of `thread`, from the newest `token_count` record in its
/// session file (`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-…-<thread>.jsonl`).
fn last_request_usage(home: &Path, thread: &str) -> Option<Usage> {
    let file = find_session_file(&home.join("sessions"), thread)?;
    let text = std::fs::read_to_string(file).ok()?;
    text.lines()
        .rev()
        .filter(|line| line.contains("\"token_count\""))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|record| {
            let last = &record["payload"]["info"]["last_token_usage"];
            last.is_object().then(|| usage(last))
        })
}

/// Searches the date directories newest first; a resumed thread keeps its first file.
fn find_session_file(dir: &Path, thread: &str) -> Option<PathBuf> {
    let suffix = format!("{thread}.jsonl");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort_by(|a, b| b.cmp(a));
    entries.iter().find_map(|path| {
        if path.is_dir() {
            find_session_file(path, thread)
        } else {
            path.file_name()?
                .to_str()?
                .ends_with(&suffix)
                .then(|| path.clone())
        }
    })
}

/// Codex usage JSON. Its input count includes cached tokens; tamale's doesn't.
fn usage(v: &Value) -> Usage {
    let get = |key: &str| v[key].as_u64().unwrap_or(0);
    let mut u = Usage::new();
    u.cached_input_tokens = get("cached_input_tokens");
    u.input_tokens = get("input_tokens").saturating_sub(u.cached_input_tokens);
    u.cache_creation_input_tokens = get("cache_write_input_tokens");
    u.output_tokens = get("output_tokens");
    u.total_tokens = agent::context_size(&u);
    u
}

/// How a run ended.
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
            "Codex reported an error".to_string()
        } else {
            self.text.trim().to_string()
        };
        let lower = text.to_lowercase();
        if lower.contains("login")
            || lower.contains("log in")
            || lower.contains("401")
            || lower.contains("unauthorized")
        {
            bail!("{text}\nRun /login to sign in to Codex.");
        }
        bail!("{text}")
    }
}

/// What a `codex exec --json` line means for the turn.
#[derive(Debug, PartialEq)]
enum Update {
    Thread(String),
    /// A complete assistant message (Codex doesn't stream text).
    Text(String),
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    /// Usage summed over the turn.
    Usage(Usage),
    Done(Outcome),
}

#[derive(Default)]
struct Parser {
    /// Calls to tamale's tools that have been announced.
    started: HashSet<String>,
    /// The last error reported, in case the run ends without finishing the turn.
    error: Option<String>,
    /// Whether the turn finished (`turn.completed`, which carries only usage).
    completed: bool,
}

impl Parser {
    fn parse(&mut self, event: &Value) -> Option<Update> {
        match event["type"].as_str()? {
            "thread.started" => Some(Update::Thread(event["thread_id"].as_str()?.to_string())),
            "item.started" => self.tool_call(&event["item"]),
            "item.completed" => {
                let item = &event["item"];
                match item["type"].as_str()? {
                    "agent_message" => {
                        let text = item["text"].as_str()?;
                        (!text.trim().is_empty()).then(|| Update::Text(text.to_string()))
                    }
                    "mcp_tool_call" if item["server"] == mcp::SERVER => {
                        let id = item["id"].as_str()?.to_string();
                        if !self.started.remove(&id) {
                            // Finished without being announced: report the call itself
                            // first; the caller asks for the result next.
                            self.started.insert(id.clone());
                            return self.tool_call(item);
                        }
                        let (output, is_error) = match item["error"]["message"].as_str() {
                            Some(message) => (message.to_string(), true),
                            None => (
                                result_text(&item["result"]["content"]),
                                item["status"] == "failed",
                            ),
                        };
                        Some(Update::ToolResult {
                            id,
                            output,
                            is_error,
                        })
                    }
                    _ => None,
                }
            }
            "turn.completed" => {
                self.completed = true;
                Some(Update::Usage(usage(&event["usage"])))
            }
            "turn.failed" => Some(Update::Done(Outcome {
                is_error: true,
                text: event["error"]["message"]
                    .as_str()
                    .or(self.error.as_deref())
                    .unwrap_or_default()
                    .to_string(),
            })),
            "error" => {
                self.error = event["message"].as_str().map(str::to_string);
                None
            }
            _ => None,
        }
    }

    fn tool_call(&mut self, item: &Value) -> Option<Update> {
        if item["type"] != "mcp_tool_call" || item["server"] != mcp::SERVER {
            return None;
        }
        let id = item["id"].as_str()?.to_string();
        self.started.insert(id.clone());
        Some(Update::ToolCall {
            id,
            name: item["tool"].as_str()?.to_string(),
            args: item["arguments"].clone(),
        })
    }
}

/// MCP result content: text blocks.
fn result_text(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Turns Codex events into agent events and tamale's own history, so sessions, the
/// transcript and undo work as with the API backend.
struct Recorder<'a> {
    history: Vec<Message>,
    /// Tool names by call id, for the results.
    names: std::collections::HashMap<String, String>,
    /// Calls still waiting for their result. History is only reported without any, so
    /// a snapshot never ends in an unanswered call.
    pending: HashSet<String>,
    /// Whether the last thing shown was assistant text (consecutive messages are
    /// separated by a blank line).
    after_text: bool,
    tx: &'a mpsc::UnboundedSender<AgentEvent>,
}

impl<'a> Recorder<'a> {
    fn new(history: Vec<Message>, tx: &'a mpsc::UnboundedSender<AgentEvent>) -> Self {
        Self {
            history,
            names: Default::default(),
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
            Update::Text(text) => {
                let shown = if self.after_text {
                    format!("\n\n{text}")
                } else {
                    text.clone()
                };
                self.send(AgentEvent::TextDelta(shown));
                self.after_text = true;
                self.push_assistant(AssistantContent::text(text));
                self.snapshot();
            }
            Update::ToolCall { id, name, args } => {
                self.after_text = false;
                self.send(AgentEvent::ToolCall {
                    name: name.clone(),
                    summary: tools::summarize(&name, &args),
                    description: tools::describe(&name, &args),
                });
                self.names.insert(id.clone(), name.clone());
                self.pending.insert(id.clone());
                self.push_assistant(AssistantContent::tool_call(id, name, args));
            }
            Update::ToolResult {
                id,
                output,
                is_error,
            } => {
                self.after_text = false;
                self.pending.remove(&id);
                let name = self.names.get(&id).cloned().unwrap_or_default();
                self.send(AgentEvent::ToolResult {
                    output: output.clone(),
                    is_error,
                });
                let result = UserContent::tool_result_from_wire(
                    &id,
                    name,
                    vec![ToolResultContent::text(output)],
                );
                // Results of parallel calls share one message.
                match self.history.last_mut() {
                    Some(Message::User { content })
                        if content
                            .iter()
                            .all(|c| matches!(c, UserContent::ToolResult(_))) =>
                    {
                        content.push(result)
                    }
                    _ => self.history.push(Message::User {
                        content: vec![result],
                    }),
                }
                self.snapshot();
            }
            Update::Usage(usage) => self.send(AgentEvent::Usage(usage)),
            Update::Done(_) => {}
        }
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
