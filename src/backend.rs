//! Where inference comes from. The agent's tools, permissions, checkpoints, history and
//! UI are the same for every backend; only the model calls differ.
//!
//! - [`Backend::Api`]: the Anthropic API with `ANTHROPIC_API_KEY` (the agent loop in
//!   [`crate::agent`]).
//! - [`Backend::ClaudeCode`]: a signed-in Claude Code CLI run headlessly with `-p`, which
//!   calls tamale's tools through a local MCP bridge.

pub mod claude_code;
pub mod logins;
mod mcp;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use rig_core::client::{CompletionClient, ModelListingClient, ProviderClient};
use rig_core::completion::Message;
use rig_core::providers::anthropic;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::agent::{self, AgentEvent, TurnConfig, TurnCtx};
use crate::workspace::Workspace;
pub use claude_code::ClaudeCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Api,
    ClaudeCode,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "api" | "anthropic" => Some(Self::Api),
            "claude-code" | "claude_code" | "claude" | "cc" => Some(Self::ClaudeCode),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::ClaudeCode => "claude-code",
        }
    }

    pub fn parse_setting(s: &str) -> Result<Self> {
        Self::parse(s).with_context(|| format!("unknown backend: {s} (use api or claude-code)"))
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
}

/// Everything a turn needs; the API backend ignores `prompt` and `thread`.
pub struct Turn {
    pub model: String,
    /// The conversation, ending with the newest prompt.
    pub history: Vec<Message>,
    /// The newest prompt on its own (the last text of `history`).
    pub prompt: String,
    /// A backend-side conversation that already holds `history` (a Claude Code session).
    pub thread: Option<String>,
    pub config: TurnConfig,
    pub workspace: Workspace,
}

#[derive(Clone)]
pub enum Backend {
    Api(anthropic::Client),
    ClaudeCode(ClaudeCode),
}

impl Backend {
    /// Sets up the configured backend. A signed-out Claude Code is returned as is, so
    /// the caller can offer to sign in.
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
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Self::Api(_) => Kind::Api,
            Self::ClaudeCode(_) => Kind::ClaudeCode,
        }
    }

    /// Where inference comes from, for the status bar.
    pub fn label(&self) -> String {
        match self {
            Self::Api(_) => "Anthropic API".into(),
            Self::ClaudeCode(cc) => cc.label(),
        }
    }

    /// Whether requests can be made: the API backend checked its key at startup.
    pub fn signed_in(&self) -> bool {
        match self {
            Self::Api(_) => true,
            Self::ClaudeCode(cc) => cc.account.logged_in,
        }
    }

    pub async fn run(self, turn: Turn, ctx: TurnCtx, tx: mpsc::UnboundedSender<AgentEvent>) {
        match self {
            Self::Api(client) => {
                let model = api_model(&client, &turn.model);
                agent::run(model, turn.history, turn.config, turn.workspace, ctx, tx).await
            }
            Self::ClaudeCode(cc) => cc.run(turn, ctx, tx).await,
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
        }
    }
}

fn api_model(client: &anthropic::Client, name: &str) -> anthropic::completion::CompletionModel {
    client
        .completion_model(name.to_string())
        .with_prompt_caching()
}
