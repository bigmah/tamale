//! Where inference comes from. The agent's tools, permissions, checkpoints, history and
//! UI are the same for every backend; only the model calls differ.
//!
//! - [`Backend::Api`]: the Anthropic API with `ANTHROPIC_API_KEY` (the agent loop in
//!   [`crate::agent`]).
//! - [`Backend::ClaudeCode`]: a signed-in Claude Code CLI run headlessly with `-p`, which
//!   calls tamale's tools through a local MCP bridge.
//! - [`Backend::Codex`]: a signed-in Codex CLI run headlessly with `codex exec`, through
//!   the same bridge.
//! - [`Backend::Compat`]: any OpenAI-compatible API (DeepSeek, Kimi, Gemini, OpenRouter,
//!   …) with its own API key, through the same agent loop.
//! - [`Backend::Unset`]: none chosen yet, while the TUI asks which to use.

pub mod claude_code;
pub mod codex;
pub mod compat;
pub mod logins;
mod mcp;
mod subagent;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rig_core::client::{CompletionClient, ModelListingClient, ProviderClient};
use rig_core::completion::Message;
use rig_core::providers::anthropic;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::agent::{self, AgentEvent, TurnConfig, TurnCtx};
use crate::workspace::Workspace;
pub use claude_code::ClaudeCode;
pub use codex::Codex;
pub use compat::{Compat, ProviderSettings};
pub use subagent::Subagent;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Api,
    ClaudeCode,
    Codex,
    /// An OpenAI-compatible provider, named in [`BackendSettings::provider`].
    Compat,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "api" | "anthropic" => Some(Self::Api),
            "claude-code" | "claude_code" | "claude" | "cc" => Some(Self::ClaudeCode),
            "codex" | "codex-cli" | "codex_cli" => Some(Self::Codex),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Compat => "openai-compatible",
        }
    }

    /// The CLI behind a login backend, for messages.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Api => "Anthropic API",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Compat => "OpenAI-compatible API",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(from = "RawBackendSettings")]
pub struct BackendSettings {
    pub kind: Kind,
    /// With [`Kind::Compat`], the name of the provider in `providers`.
    pub provider: String,
    /// OpenAI-compatible providers by name, selected with their name as `kind`.
    pub providers: BTreeMap<String, ProviderSettings>,
    /// The Claude Code executable, for the `claude-code` backend.
    pub claude_path: PathBuf,
    /// More Claude Code config directories (`CLAUDE_CONFIG_DIR`), each with its own
    /// login, offered in the backend picker when signed in.
    pub claude_config_dirs: Vec<PathBuf>,
    /// The Codex executable, for the `codex` backend.
    pub codex_path: PathBuf,
    /// More Codex home directories (`CODEX_HOME`), each with its own login, offered in
    /// the backend picker when signed in.
    pub codex_homes: Vec<PathBuf>,
}

/// `[backend]` as written: `kind` is a backend or a provider's name.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBackendSettings {
    kind: String,
    claude_path: PathBuf,
    claude_config_dirs: Vec<PathBuf>,
    codex_path: PathBuf,
    codex_homes: Vec<PathBuf>,
    #[serde(default)]
    providers: BTreeMap<String, ProviderSettings>,
}

impl From<RawBackendSettings> for BackendSettings {
    fn from(raw: RawBackendSettings) -> Self {
        let (kind, provider) = parse_selection(&raw.kind);
        Self {
            kind,
            provider,
            providers: raw.providers,
            claude_path: raw.claude_path,
            claude_config_dirs: raw.claude_config_dirs,
            codex_path: raw.codex_path,
            codex_homes: raw.codex_homes,
        }
    }
}

/// A backend's name, or else a provider's (checked against the providers later).
fn parse_selection(s: &str) -> (Kind, String) {
    match Kind::parse(s) {
        Some(kind) => (kind, String::new()),
        None => (Kind::Compat, s.trim().to_ascii_lowercase()),
    }
}

impl BackendSettings {
    /// Selects a backend by name: `api`, `claude-code`, `codex` or a provider.
    pub fn select(&mut self, name: &str) -> Result<()> {
        let (kind, provider) = parse_selection(name);
        if kind == Kind::Compat {
            self.provider_settings(&provider)?;
        }
        self.kind = kind;
        self.provider = provider;
        Ok(())
    }

    /// The provider called `name`, or an error naming the backends there are.
    pub fn provider_settings(&self, name: &str) -> Result<&ProviderSettings> {
        self.providers.get(name).with_context(|| {
            let names: Vec<&str> = self.providers.keys().map(String::as_str).collect();
            format!(
                "unknown backend: {name} (use api, claude-code, codex or a provider: {})",
                names.join(", ")
            )
        })
    }

    /// The environment variable the configured backend needs and doesn't have, if any.
    pub fn missing_key(&self) -> Option<String> {
        match self.kind {
            Kind::Api => (!api_key_set()).then(|| "ANTHROPIC_API_KEY".to_string()),
            Kind::Compat => self
                .providers
                .get(&self.provider)
                .filter(|p| !p.key_set())
                .map(|p| p.api_key_env.clone()),
            Kind::ClaudeCode | Kind::Codex => None,
        }
    }
}

/// Whether `name` is an Anthropic model, which only the Anthropic backends serve.
pub fn is_claude_model(name: &str) -> bool {
    let name = name.trim();
    name.is_empty() || name.starts_with("claude") || name.starts_with("anthropic/")
}

/// Everything a turn needs; the API backend ignores `prompt` and `thread`.
pub struct Turn {
    pub model: String,
    /// The conversation, ending with the newest prompt.
    pub history: Vec<Message>,
    /// The newest prompt on its own (the last text of `history`).
    pub prompt: String,
    /// A backend-side conversation that already holds `history` (a Claude Code session
    /// or Codex thread).
    pub thread: Option<String>,
    pub config: TurnConfig,
    pub workspace: Workspace,
}

#[derive(Clone)]
pub enum Backend {
    Api(anthropic::Client),
    ClaudeCode(ClaudeCode),
    Codex(Codex),
    Compat(Compat),
    /// Nothing to run turns with yet: the API was configured without a key, and the TUI
    /// asks for a backend. Its kind stays [`Kind::Api`], as in the settings.
    Unset,
}

/// Whether the API backend can be used.
pub fn api_key_set() -> bool {
    std::env::var("ANTHROPIC_API_KEY").is_ok()
}

impl Backend {
    /// Sets up the configured backend. A signed-out CLI is returned as is, so the caller
    /// can offer to sign in.
    pub async fn connect(settings: &BackendSettings) -> Result<Self> {
        match settings.kind {
            Kind::Api => {
                if !api_key_set() {
                    bail!(
                        "ANTHROPIC_API_KEY is not set (or use your Claude Code sign-in with \
the claude-code backend)"
                    );
                }
                Ok(Self::Api(
                    anthropic::Client::from_env().context("creating Anthropic client")?,
                ))
            }
            Kind::ClaudeCode => Ok(Self::ClaudeCode(
                ClaudeCode::connect(settings.claude_path.clone(), None).await?,
            )),
            Kind::Codex => Ok(Self::Codex(
                Codex::connect(settings.codex_path.clone(), None).await?,
            )),
            Kind::Compat => {
                let provider = settings.provider_settings(&settings.provider)?;
                Ok(Self::Compat(Compat::connect(&settings.provider, provider)?))
            }
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Self::Api(_) | Self::Unset => Kind::Api,
            Self::ClaudeCode(_) => Kind::ClaudeCode,
            Self::Codex(_) => Kind::Codex,
            Self::Compat(_) => Kind::Compat,
        }
    }

    /// Where inference comes from, for the status bar.
    pub fn label(&self) -> String {
        match self {
            Self::Api(_) => "Anthropic API".into(),
            Self::Unset => "no backend".into(),
            Self::ClaudeCode(cc) => cc.label(),
            Self::Codex(codex) => codex.label(),
            Self::Compat(compat) => compat.label.clone(),
        }
    }

    /// The label plus the signed-in account, to tell logins apart.
    pub fn label_with_account(&self) -> String {
        match self {
            Self::Api(_) | Self::Compat(_) | Self::Unset => self.label(),
            Self::ClaudeCode(cc) => cc.label_with_account(),
            Self::Codex(codex) => codex.label_with_account(),
        }
    }

    pub fn email(&self) -> Option<&str> {
        match self {
            Self::Api(_) | Self::Compat(_) | Self::Unset => None,
            Self::ClaudeCode(cc) => cc.account.email.as_deref(),
            Self::Codex(codex) => codex.account.email.as_deref(),
        }
    }

    /// The directory a CLI login lives in; `None` is the CLI's default.
    pub fn login_dir(&self) -> Option<&Path> {
        match self {
            Self::Api(_) | Self::Compat(_) | Self::Unset => None,
            Self::ClaudeCode(cc) => cc.config_dir.as_deref(),
            Self::Codex(codex) => codex.home.as_deref(),
        }
    }

    /// Whether both are the same login (the same CLI and directory, or the same
    /// provider), signed in or not.
    pub fn same_login(&self, other: &Backend) -> bool {
        match (self, other) {
            (Self::Compat(a), Self::Compat(b)) => a.name == b.name,
            _ => self.kind() == other.kind() && self.login_dir() == other.login_dir(),
        }
    }

    /// The model to switch to on becoming the backend after `previous`, when the
    /// current one (`name`) is another provider's.
    pub fn model_after(&self, previous: &Backend, name: &str) -> Option<String> {
        match self {
            Self::Compat(compat) if !self.same_login(previous) => {
                Some(compat.default_model.clone()).filter(|m| m != name)
            }
            Self::Compat(_) => None,
            _ if matches!(previous, Self::Compat(_)) && !is_claude_model(name) => {
                Some(crate::config::Settings::default().model.name)
            }
            _ => None,
        }
    }

    /// Whether both are signed in to the same account of the same CLI.
    pub fn same_account(&self, other: &Backend) -> bool {
        match (self, other) {
            (Self::ClaudeCode(a), Self::ClaudeCode(b)) => a.same_account(b),
            (Self::Codex(a), Self::Codex(b)) => a.same_account(b),
            _ => false,
        }
    }

    /// Runs a CLI's interactive sign-in in this terminal.
    pub async fn login(&mut self) -> Result<()> {
        match self {
            Self::Api(_) => {
                bail!("the API backend uses ANTHROPIC_API_KEY; there's nothing to sign in to")
            }
            Self::Compat(compat) => bail!("{}", compat.no_login()),
            Self::Unset => bail!("no backend is selected; pick one with /backend"),
            Self::ClaudeCode(cc) => cc.login().await,
            Self::Codex(codex) => codex.login().await,
        }
    }

    /// The interactive sign-in `login` runs, for running it in another process's terminal.
    #[cfg(unix)]
    pub fn login_command(&self) -> Result<tokio::process::Command> {
        match self {
            Self::Api(_) => {
                bail!("the API backend uses ANTHROPIC_API_KEY; there's nothing to sign in to")
            }
            Self::Compat(compat) => bail!("{}", compat.no_login()),
            Self::Unset => bail!("no backend is selected; pick one with /backend"),
            Self::ClaudeCode(cc) => Ok(cc.login_command()),
            Self::Codex(codex) => codex.login_command(),
        }
    }

    /// Takes in the outcome of a `login_command`, and how it exited.
    #[cfg(unix)]
    pub async fn finish_login(&mut self, success: bool, status: &str) -> Result<()> {
        match self {
            Self::Api(_) | Self::Compat(_) | Self::Unset => Ok(()),
            Self::ClaudeCode(cc) => cc.finish_login(success, status).await,
            Self::Codex(codex) => codex.finish_login(success, status).await,
        }
    }

    /// The model a turn will use, for the status bar.
    pub fn model_label(&self, name: &str) -> String {
        match self {
            Self::Codex(_) => codex::model(name).unwrap_or("codex default").to_string(),
            Self::Compat(compat) => compat.model(name).to_string(),
            _ => name.to_string(),
        }
    }

    /// Whether requests can be made: the API backend checked its key at startup.
    pub fn signed_in(&self) -> bool {
        match self {
            Self::Api(_) | Self::Compat(_) => true,
            Self::Unset => false,
            Self::ClaudeCode(cc) => cc.account.logged_in,
            Self::Codex(codex) => codex.account.logged_in,
        }
    }

    pub async fn run(self, turn: Turn, ctx: TurnCtx, tx: mpsc::UnboundedSender<AgentEvent>) {
        match self {
            Self::Api(client) => {
                let model = api_model(&client, &turn.model);
                let history = signed_reasoning_only(turn.history);
                agent::run(model, history, turn.config, turn.workspace, ctx, tx).await
            }
            Self::Compat(compat) => {
                let model = compat.completion_model(&turn.model);
                let config = compat_config(&compat, turn.config);
                agent::run(model, turn.history, config, turn.workspace, ctx, tx).await
            }
            Self::Unset => no_backend(&tx),
            Self::ClaudeCode(cc) => cc.run(turn, ctx, tx).await,
            Self::Codex(codex) => codex.run(turn, ctx, tx).await,
        }
    }

    pub async fn compact(
        self,
        turn: Turn,
        instructions: Option<String>,
        tx: mpsc::UnboundedSender<AgentEvent>,
    ) {
        match self {
            Self::Api(client) => {
                let model = api_model(&client, &turn.model);
                agent::compact(
                    model,
                    signed_reasoning_only(turn.history),
                    instructions,
                    turn.config,
                    turn.workspace,
                    tx,
                )
                .await
            }
            Self::Compat(compat) => {
                let model = compat.completion_model(&turn.model);
                let config = compat_config(&compat, turn.config);
                agent::compact(
                    model,
                    turn.history,
                    instructions,
                    config,
                    turn.workspace,
                    tx,
                )
                .await
            }
            Self::Unset => no_backend(&tx),
            Self::ClaudeCode(cc) => cc.compact(turn, instructions, tx).await,
            Self::Codex(codex) => codex.compact(turn, instructions, tx).await,
        }
    }

    /// Available models as (id, display name). Empty means "use the built-in list".
    pub async fn list_models(&self) -> Result<Vec<(String, String)>, String> {
        match self {
            Self::Api(client) => client
                .list_models()
                .await
                .map(|list| {
                    list.data
                        .into_iter()
                        .map(|m| (m.id.clone(), m.name.unwrap_or(m.id)))
                        .collect()
                })
                .map_err(|e| e.to_string()),
            // Claude Code has no model listing; it accepts the same model ids.
            Self::ClaudeCode(_) | Self::Unset => Ok(Vec::new()),
            Self::Codex(codex) => codex.list_models().await.map_err(|e| format!("{e:#}")),
            Self::Compat(compat) => compat.list_models().await.map_err(|e| format!("{e:#}")),
        }
    }
}

fn no_backend(tx: &mpsc::UnboundedSender<AgentEvent>) {
    let _ = tx.send(AgentEvent::Error(
        "No backend is selected; pick one with /backend.".into(),
    ));
    let _ = tx.send(AgentEvent::Done);
}

fn api_model(client: &anthropic::Client, name: &str) -> anthropic::completion::CompletionModel {
    client
        .completion_model(name.to_string())
        .with_prompt_caching()
}

/// A turn's settings for an OpenAI-compatible provider: its reasoning parameters and
/// output limit.
fn compat_config(compat: &Compat, mut config: TurnConfig) -> TurnConfig {
    config.reasoning.dialect = agent::Dialect::OpenAi;
    config.limits.max_tokens = compat.max_tokens(config.limits.max_tokens);
    config.limits.compact_max_tokens = compat.max_tokens(config.limits.compact_max_tokens);
    config
}

/// Drops reasoning without a signature (from another provider), which Anthropic rejects.
fn signed_reasoning_only(mut history: Vec<Message>) -> Vec<Message> {
    use rig_core::completion::{AssistantContent, message::ReasoningContent};
    for message in &mut history {
        if let Message::Assistant { content, .. } = message {
            let mut kept = content.clone();
            kept.retain_mut(|c| match c {
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
            // A message has to keep some content.
            if !kept.is_empty() {
                *content = kept;
            }
        }
    }
    history
}
