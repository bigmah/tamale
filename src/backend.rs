//! Where inference comes from. The agent's tools, permissions, checkpoints, history and
//! UI are the same for every backend; only the model calls differ.
//!
//! - [`Backend::Api`]: the Anthropic API with `ANTHROPIC_API_KEY` (the agent loop in
//!   [`crate::agent`]).
//! - [`Backend::ClaudeCode`]: a signed-in Claude Code CLI run headlessly with `-p`, which
//!   calls tamale's tools through a local MCP bridge.
//! - [`Backend::Codex`]: a signed-in Codex CLI run headlessly with `codex exec`, through
//!   the same bridge.

pub mod claude_code;
pub mod codex;
pub mod logins;
mod mcp;
mod subagent;

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
pub use subagent::Subagent;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Api,
    ClaudeCode,
    Codex,
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
        }
    }

    /// The CLI behind a login backend, for messages.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Api => "Anthropic API",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    pub fn parse_setting(s: &str) -> Result<Self> {
        Self::parse(s)
            .with_context(|| format!("unknown backend: {s} (use api, claude-code or codex)"))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendSettings {
    pub kind: Kind,
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
}

impl Backend {
    /// Sets up the configured backend. A signed-out CLI is returned as is, so the caller
    /// can offer to sign in.
    pub async fn connect(settings: &BackendSettings) -> Result<Self> {
        match settings.kind {
            Kind::Api => {
                if std::env::var("ANTHROPIC_API_KEY").is_err() {
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
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Self::Api(_) => Kind::Api,
            Self::ClaudeCode(_) => Kind::ClaudeCode,
            Self::Codex(_) => Kind::Codex,
        }
    }

    /// Where inference comes from, for the status bar.
    pub fn label(&self) -> String {
        match self {
            Self::Api(_) => "Anthropic API".into(),
            Self::ClaudeCode(cc) => cc.label(),
            Self::Codex(codex) => codex.label(),
        }
    }

    /// The label plus the signed-in account, to tell logins apart.
    pub fn label_with_account(&self) -> String {
        match self {
            Self::Api(_) => self.label(),
            Self::ClaudeCode(cc) => cc.label_with_account(),
            Self::Codex(codex) => codex.label_with_account(),
        }
    }

    pub fn email(&self) -> Option<&str> {
        match self {
            Self::Api(_) => None,
            Self::ClaudeCode(cc) => cc.account.email.as_deref(),
            Self::Codex(codex) => codex.account.email.as_deref(),
        }
    }

    /// The directory a CLI login lives in; `None` is the CLI's default.
    pub fn login_dir(&self) -> Option<&Path> {
        match self {
            Self::Api(_) => None,
            Self::ClaudeCode(cc) => cc.config_dir.as_deref(),
            Self::Codex(codex) => codex.home.as_deref(),
        }
    }

    /// Whether both are the same login (the same CLI and directory), signed in or not.
    pub fn same_login(&self, other: &Backend) -> bool {
        self.kind() == other.kind() && self.login_dir() == other.login_dir()
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
            Self::ClaudeCode(cc) => cc.login().await,
            Self::Codex(codex) => codex.login().await,
        }
    }

    /// The model a turn will use, for the status bar.
    pub fn model_label(&self, name: &str) -> String {
        match self {
            Self::Codex(_) => codex::model(name).unwrap_or("codex default").to_string(),
            _ => name.to_string(),
        }
    }

    /// Whether requests can be made: the API backend checked its key at startup.
    pub fn signed_in(&self) -> bool {
        match self {
            Self::Api(_) => true,
            Self::ClaudeCode(cc) => cc.account.logged_in,
            Self::Codex(codex) => codex.account.logged_in,
        }
    }

    pub async fn run(self, turn: Turn, ctx: TurnCtx, tx: mpsc::UnboundedSender<AgentEvent>) {
        match self {
            Self::Api(client) => {
                let model = api_model(&client, &turn.model);
                agent::run(model, turn.history, turn.config, turn.workspace, ctx, tx).await
            }
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
                    turn.history,
                    instructions,
                    turn.config,
                    turn.workspace,
                    tx,
                )
                .await
            }
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
            Self::ClaudeCode(_) => Ok(Vec::new()),
            Self::Codex(codex) => codex.list_models().await.map_err(|e| format!("{e:#}")),
        }
    }
}

fn api_model(client: &anthropic::Client, name: &str) -> anthropic::completion::CompletionModel {
    client
        .completion_model(name.to_string())
        .with_prompt_caching()
}
