use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use rig_core::completion::message::{ToolResultContent, UserContent};
use rig_core::completion::{
    AssistantContent, CompletionError, CompletionModel, Message, ToolDefinition, Usage,
};
use rig_core::streaming::StreamedAssistantContent;
use tokio::sync::mpsc;

use crate::tools;

const MAX_TOKENS: u64 = 16_000;
const COMPACT_MAX_TOKENS: u64 = 8_000;
const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;
/// Auto-compact once the context is this full (percent of the window).
const AUTO_COMPACT_PERCENT: u64 = 80;
const MAX_RETRIES: u32 = 5;
/// Max characters read from a single instructions file.
const MAX_INSTRUCTIONS_CHARS: usize = 20_000;

const COMPACT_PROMPT: &str = "The conversation is getting long and will be replaced by a summary. \
Do not call any tools. Write a detailed summary of the conversation so far that lets you continue \
the work without the original messages. Include:\n\
1. The user's requests and goals, including any constraints or preferences they stated.\n\
2. Key technical context: files, functions, commands and decisions that matter.\n\
3. Changes made so far (file paths and what changed) and their current state (built? tested?).\n\
4. Errors you hit and how they were resolved.\n\
5. Anything in progress and the concrete next steps, if any.\n\
Be specific (exact paths, names, snippets where essential) but concise. \
Respond with the summary only.";

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

/// Anthropic's effort setting (`output_config.effort`): trades thoroughness for tokens and
/// latency. Not every model supports every level; the API rejects unsupported ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// Parses a level name; `None` for unknown names.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|e| e.as_str().eq_ignore_ascii_case(s.trim()))
    }

    fn params(effort: Option<Self>) -> Option<serde_json::Value> {
        effort.map(|e| serde_json::json!({ "output_config": { "effort": e.as_str() } }))
    }
}

#[derive(Clone, Copy)]
pub struct Config {
    pub context_window: u64,
    /// Context size of the most recent request, used to decide when to auto-compact.
    pub context_tokens: u64,
    /// `None` leaves the API default.
    pub effort: Option<Effort>,
}

impl Config {
    fn should_compact(&self) -> bool {
        self.context_tokens >= self.context_window * AUTO_COMPACT_PERCENT / 100
    }
}

/// Context window of a model in tokens. `HARNESS_CONTEXT_WINDOW` overrides it.
pub fn context_window(_model: &str) -> u64 {
    std::env::var("HARNESS_CONTEXT_WINDOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CONTEXT_WINDOW)
}

/// Total tokens a request occupied in the context window (prompt incl. cache, plus output).
pub fn context_size(u: &Usage) -> u64 {
    u.input_tokens + u.cached_input_tokens + u.cache_creation_input_tokens + u.output_tokens
}

pub fn system_prompt() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut prompt = format!(
        "You are a coding agent running in the user's terminal. You help with software \
engineering tasks: reading, writing and editing code, running commands, and answering questions \
about the codebase.\n\n\
Guidelines:\n\
- Use the tools to inspect the codebase before making changes. Prefer edit_file for targeted edits.\n\
- Keep responses concise; the UI is a terminal. Use plain text or light markdown.\n\
- Match the existing code style. Verify your work (build/test) when practical.\n\
- Don't take destructive actions (deleting files, force pushes) unless asked.\n\n\
Environment:\n- Working directory: {}\n- Platform: {}\n",
        cwd.display(),
        std::env::consts::OS
    );

    let files = instruction_files(&cwd);
    if !files.is_empty() {
        prompt.push_str(
            "\n# Project instructions\n\nThe user provided these instruction files. \
Follow them; they take precedence over the default guidelines above.\n",
        );
        for (path, text) in files {
            prompt.push_str(&format!(
                "\n<instructions path=\"{}\">\n{}\n</instructions>\n",
                path.display(),
                text.trim()
            ));
        }
    }
    prompt
}

/// Instruction files to include in the system prompt, most general first: the global
/// `~/.config/harness/AGENTS.md`, then `AGENTS.md`/`CLAUDE.md` from the git root down to `cwd`.
pub fn instruction_files(cwd: &Path) -> Vec<(PathBuf, String)> {
    let mut candidates = Vec::new();
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(dir) = config_dir {
        candidates.push(dir.join("harness").join("AGENTS.md"));
    }

    // Walk up to the enclosing git repository root; outside a repo only use cwd.
    let ancestors: Vec<&Path> = cwd.ancestors().collect();
    let dirs = match ancestors.iter().position(|d| d.join(".git").exists()) {
        Some(root) => &ancestors[..=root],
        None => &ancestors[..1.min(ancestors.len())],
    };
    for dir in dirs.iter().rev() {
        candidates.push(dir.join("AGENTS.md"));
        candidates.push(dir.join("CLAUDE.md"));
    }

    let mut out: Vec<(PathBuf, String)> = Vec::new();
    for path in candidates {
        let Ok(mut text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Skip duplicates (e.g. CLAUDE.md symlinked to AGENTS.md).
        if text.trim().is_empty() || out.iter().any(|(_, t)| t == &text) {
            continue;
        }
        if text.len() > MAX_INSTRUCTIONS_CHARS {
            let mut cut = MAX_INSTRUCTIONS_CHARS;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("\n... (truncated)");
        }
        out.push((path, text));
    }
    out
}

/// Runs the agent loop until the model stops calling tools.
pub async fn run<M>(
    model: M,
    mut history: Vec<Message>,
    config: Config,
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
    effort: Option<Effort>,
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
            effort,
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
    mut config: Config,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) -> anyhow::Result<()>
where
    M: CompletionModel + Clone,
{
    let preamble = system_prompt();
    let tool_defs = tools::definitions();

    loop {
        if config.should_compact() && history.len() > 1 {
            *history = compact_history(
                &model,
                &preamble,
                &tool_defs,
                history,
                None,
                config.effort,
                tx,
            )
            .await?;
            config.context_tokens = 0;
            let _ = tx.send(AgentEvent::History(history.clone()));
        }

        let (choice, usage) = request(
            &model,
            &preamble,
            &tool_defs,
            history,
            MAX_TOKENS,
            config.effort,
            true,
            tx,
        )
        .await?;
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
    effort: Option<Effort>,
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

    let (choice, usage) = request(
        model,
        preamble,
        tool_defs,
        &messages,
        COMPACT_MAX_TOKENS,
        effort,
        false,
        tx,
    )
    .await?;
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

/// One completion request, retried with exponential backoff on transient errors.
async fn request<M>(
    model: &M,
    preamble: &str,
    tool_defs: &[ToolDefinition],
    history: &[Message],
    max_tokens: u64,
    effort: Option<Effort>,
    stream_text: bool,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) -> anyhow::Result<(Vec<AssistantContent>, Usage)>
where
    M: CompletionModel + Clone,
{
    let (prompt, rest) = history
        .split_last()
        .ok_or_else(|| anyhow::anyhow!("empty history"))?;

    let mut attempt = 0;
    loop {
        let result = async {
            let mut stream = model
                .completion_request(prompt.clone())
                .preamble(preamble.to_string())
                .messages(rest.to_vec())
                .tools(tool_defs.to_vec())
                .max_tokens(max_tokens)
                .additional_params_opt(Effort::params(effort))
                .stream()
                .await?;
            while let Some(item) = stream.next().await {
                if let StreamedAssistantContent::Text(t) = item?
                    && stream_text
                {
                    let _ = tx.send(AgentEvent::TextDelta(t.text));
                }
            }
            Ok::<_, CompletionError>((stream.choice.to_vec(), stream.usage()))
        }
        .await;

        match result {
            Ok(r) => return Ok(r),
            Err(e) if attempt < MAX_RETRIES && is_retryable(&e) => {
                attempt += 1;
                let delay = retry_delay(&e, attempt);
                let _ = tx.send(AgentEvent::Retry {
                    attempt,
                    delay,
                    error: short_error(&e),
                });
                tokio::time::sleep(delay).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn is_retryable(e: &CompletionError) -> bool {
    if let Some(status) = e.provider_response_status() {
        let code = status.as_u16();
        if matches!(code, 408 | 409 | 429) || code >= 500 {
            return true;
        }
        // Some providers send error envelopes with a 2xx status; fall through to the body.
        if !status.is_success() {
            return false;
        }
    }
    if let Some(body) = e.provider_response_body() {
        return [
            "overloaded_error",
            "rate_limit_error",
            "api_error",
            "timeout_error",
        ]
        .iter()
        .any(|k| body.contains(k));
    }
    // No HTTP response at all: connection resets, timeouts, broken streams.
    matches!(
        e,
        CompletionError::HttpError(_) | CompletionError::ProviderError(_)
    )
}

fn retry_delay(e: &CompletionError, attempt: u32) -> Duration {
    let retry_after = e
        .provider_response_headers()
        .and_then(|h| h.get("retry-after"))
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if let Some(secs) = retry_after {
        return Duration::from_secs(secs.clamp(1, 120));
    }
    let base = Duration::from_secs(1 << (attempt - 1).min(5)).min(Duration::from_secs(30));
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis() % 500)
        .unwrap_or(0);
    base + Duration::from_millis(jitter as u64)
}

fn short_error(e: &CompletionError) -> String {
    if let Ok(Some(json)) = e.provider_response_json()
        && let Some(msg) = json.pointer("/error/message").and_then(|m| m.as_str())
    {
        return match e.provider_response_status() {
            Some(s) if !s.is_success() => format!("{} {msg}", s.as_u16()),
            _ => msg.to_string(),
        };
    }
    let s = e.to_string();
    match s.char_indices().nth(200) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}
