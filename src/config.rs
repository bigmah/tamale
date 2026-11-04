//! Typed settings and TOML layering. Environment variables and CLI flags are
//! applied once at startup; the agent and UI receive the resolved settings.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer};

use crate::agent::{self, Effort, Reasoning, Thinking};
use crate::backend::{self, BackendSettings};
use crate::permissions::{Mode, PermissionSettings};

#[cfg(test)]
mod tests;

pub const DEFAULT_TOML: &str = include_str!("../config/default.toml");

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub backend: BackendSettings,
    pub model: ModelSettings,
    pub agent: AgentSettings,
    pub ui: UiSettings,
    pub permissions: PermissionSettings,
    #[serde(default)]
    pub hub: HubSettings,
    /// What project files asked for and aren't allowed to set, to tell the user.
    #[serde(skip)]
    pub ignored: Vec<String>,
}

/// `[hub]`: a tamale-hub to make the session reachable through (`/remote`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HubSettings {
    /// The hub's agent socket base.
    pub url: String,
    pub connect: HubConnect,
    /// End-to-end encrypt relayed channels, so the hub only moves opaque bytes. Turning it
    /// off is for debugging.
    pub e2e: bool,
    /// The key, as the hub's site made it. Only from `TAMALE_HUB_KEY`, never a settings
    /// file; tamale never makes one (see `single_key.md`).
    #[serde(skip)]
    pub key: Option<String>,
    /// Whether `TAMALE_HUB_TOKEN`, which keys replaced, is still set.
    #[serde(skip)]
    pub old_token: bool,
}

impl HubSettings {
    /// The key's bytes, or what to tell someone who has to set one.
    pub fn key_bytes(&self) -> Result<[u8; 32], String> {
        let site = self.site();
        match &self.key {
            Some(key) => tamale_hub_protocol::decode_key(key).ok_or_else(|| {
                format!("TAMALE_HUB_KEY isn't a key (thk_…). Make one at {site}")
            }),
            None if self.old_token => Err(format!(
                "TAMALE_HUB_TOKEN has been replaced: set TAMALE_HUB_KEY instead. Make a key at {site}"
            )),
            None => Err(format!(
                "Set TAMALE_HUB_KEY to use the hub. Make a key at {site}"
            )),
        }
    }

    /// The hub's site, from its agent socket's base.
    pub fn site(&self) -> String {
        self.url
            .trim_end_matches('/')
            .replacen("wss://", "https://", 1)
            .replacen("ws://", "http://", 1)
    }
}

impl Default for HubSettings {
    fn default() -> Self {
        Self {
            url: "wss://tamale-hub.com".into(),
            connect: HubConnect::default(),
            e2e: true,
            key: None,
            old_token: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HubConnect {
    /// Only with `/remote`.
    #[default]
    Manual,
    /// On startup, whenever `url` and the token are set.
    Auto,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSettings {
    pub name: String,
    #[serde(deserialize_with = "deserialize_effort")]
    pub effort: Option<Effort>,
    #[serde(deserialize_with = "deserialize_thinking")]
    pub thinking: Thinking,
    pub context_window: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSettings {
    pub max_tokens: u64,
    pub compact_max_tokens: u64,
    pub auto_compact_percent: u8,
    pub max_retries: u32,
    pub request_timeout_secs: u64,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSettings {
    pub verbose: bool,
    pub mouse: bool,
}

impl Default for Settings {
    fn default() -> Self {
        toml::from_str(DEFAULT_TOML).expect("bundled default settings must be valid")
    }
}

impl ModelSettings {
    pub fn reasoning(&self) -> Reasoning {
        Reasoning {
            effort: self.effort,
            thinking: self.thinking,
        }
    }

    pub fn context_window(&self) -> u64 {
        self.context_window
            .unwrap_or_else(|| agent::models::context_window(&self.name))
    }
}

impl Settings {
    pub fn load(cwd: &Path, explicit: Option<&Path>) -> Result<Self> {
        Self::load_from(cwd, user_config_dir().as_deref(), explicit)
    }

    fn load_from(cwd: &Path, user_dir: Option<&Path>, explicit: Option<&Path>) -> Result<Self> {
        let mut layers: toml::Value = toml::from_str(DEFAULT_TOML)?;
        let mut ignored = Vec::new();
        if let Some(dir) = user_dir {
            read_layer(&mut layers, &dir.join("config.toml"), false, None)?;
        }
        for dir in project_dirs(cwd) {
            let path = dir.join("tamale.toml");
            read_layer(&mut layers, &path, false, Some(&mut ignored))?;
        }
        if let Some(path) = explicit {
            read_layer(&mut layers, path, true, None)?;
        }
        let mut settings: Self = layers.try_into().context("resolving settings")?;
        settings.ignored = ignored;
        Ok(settings)
    }

    /// Inject the environment lookup so tests don't mutate process-wide variables.
    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) -> Result<()> {
        if let Some(value) = get("TAMALE_BACKEND").filter(|s| !s.trim().is_empty()) {
            self.backend.kind = backend::Kind::parse_setting(&value).context("TAMALE_BACKEND")?;
        }
        if let Some(value) = get("TAMALE_MODEL") {
            self.model.name = value;
        }
        if let Some(value) = get("TAMALE_EFFORT").filter(|s| !s.trim().is_empty()) {
            self.model.effort = Effort::parse_setting(&value).context("TAMALE_EFFORT")?;
        }
        if let Some(value) = get("TAMALE_THINKING").filter(|s| !s.trim().is_empty()) {
            self.model.thinking = Thinking::parse_setting(&value).context("TAMALE_THINKING")?;
        }
        if let Some(value) = get("TAMALE_HUB_URL").filter(|s| !s.trim().is_empty()) {
            self.hub.url = value.trim().to_string();
        }
        if let Some(value) = get("TAMALE_HUB_KEY").filter(|s| !s.trim().is_empty()) {
            self.hub.key = Some(value.trim().to_string());
        }
        self.hub.old_token = get("TAMALE_HUB_TOKEN").is_some_and(|s| !s.trim().is_empty());
        if let Some(value) = get("TAMALE_CONTEXT_WINDOW") {
            self.model.context_window = Some(
                value
                    .parse()
                    .context("TAMALE_CONTEXT_WINDOW must be a positive integer")?,
            );
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.backend.claude_path.as_os_str().is_empty(),
            "backend.claude_path must not be empty"
        );
        ensure!(
            !self.backend.codex_path.as_os_str().is_empty(),
            "backend.codex_path must not be empty"
        );
        ensure!(
            !self.model.name.trim().is_empty(),
            "model.name must not be empty"
        );
        ensure!(
            self.model.context_window != Some(0),
            "model.context_window must be positive"
        );
        ensure!(
            self.agent.max_tokens > 0,
            "agent.max_tokens must be positive"
        );
        ensure!(
            self.agent.compact_max_tokens > 0,
            "agent.compact_max_tokens must be positive"
        );
        ensure!(
            self.agent.request_timeout_secs > 0,
            "agent.request_timeout_secs must be positive"
        );
        ensure!(
            (1..=100).contains(&self.agent.auto_compact_percent),
            "agent.auto_compact_percent must be between 1 and 100"
        );
        Ok(())
    }

    pub fn turn_config(&self, context_tokens: u64) -> agent::TurnConfig {
        agent::TurnConfig {
            context_window: self.model.context_window(),
            context_tokens,
            reasoning: self.model.reasoning(),
            limits: self.agent,
            web_ui: false,
        }
    }
}

pub fn user_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|dir| dir.join("tamale"))
}

/// Project layers run from the enclosing git root down to cwd. Outside a
/// repository, only cwd is used, matching project instruction discovery.
pub fn project_dirs(cwd: &Path) -> Vec<&Path> {
    let ancestors: Vec<_> = cwd.ancestors().collect();
    let root = ancestors
        .iter()
        .position(|dir| dir.join(".git").exists())
        .unwrap_or(0);
    ancestors[..=root].iter().rev().copied().collect()
}

/// Merges the file at `path` into `base`. With `ignored` it's a project's file, which isn't
/// trusted with everything (see [`restrict_project_layer`]); what it can't set is noted there.
fn read_layer(
    base: &mut toml::Value,
    path: &Path,
    required: bool,
    ignored: Option<&mut Vec<String>>,
) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if !required && error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let mut overlay =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if let Some(ignored) = ignored {
        let refused = restrict_project_layer(&mut overlay, base);
        if !refused.is_empty() {
            ignored.push(format!(
                "Ignored {} in {}: a project's tamale.toml can't set that. Your own config.toml \
or --config can.",
                refused.join(", "),
                path.display()
            ));
        }
    }
    merge(base, overlay);
    // Validate each file's schema so typos and type errors identify their source.
    let settings: Settings = base
        .clone()
        .try_into()
        .with_context(|| format!("invalid settings in {}", path.display()))?;
    settings
        .validate()
        .with_context(|| format!("invalid settings in {}", path.display()))?;
    Ok(())
}

/// The `[backend]` settings that say what tamale runs: the CLIs, and the directories those CLIs
/// load their own settings, hooks and MCP servers from.
const BACKEND_PROGRAMS: [&str; 4] = [
    "claude_path",
    "claude_config_dirs",
    "codex_path",
    "codex_homes",
];

/// Takes out of a project's file what a repository doesn't get to decide, and returns what it
/// took. A repository is someone else's code until you've read it, and starting tamale in one
/// mustn't run a program it chose (`backend`), hand this session and the key's auth value to
/// a hub it chose (`hub`), or skip the approvals you asked for (`permissions`). It can still
/// ask for more caution: a stricter mode, and deny rules.
fn restrict_project_layer(overlay: &mut toml::Value, base: &toml::Value) -> Vec<String> {
    let mut refused = Vec::new();
    let Some(file) = overlay.as_table_mut() else {
        return refused;
    };
    if let Some(hub) = file.remove("hub") {
        match hub.as_table() {
            Some(table) => refused.extend(table.keys().map(|key| format!("hub.{key}"))),
            None => refused.push("hub".to_string()),
        }
    }
    if let Some(backend) = file.get_mut("backend").and_then(toml::Value::as_table_mut) {
        for key in BACKEND_PROGRAMS {
            if backend.remove(key).is_some() {
                refused.push(format!("backend.{key}"));
            }
        }
    }
    if let Some(permissions) = file
        .get_mut("permissions")
        .and_then(toml::Value::as_table_mut)
    {
        if permissions.remove("allow").is_some() {
            refused.push("permissions.allow".to_string());
        }
        let mode = |value: Option<&toml::Value>| value?.as_str().and_then(Mode::parse);
        // An unknown mode is left for validation to report.
        if let (Some(asked), Some(current)) = (
            mode(permissions.get("mode")),
            mode(base.get("permissions").and_then(|p| p.get("mode"))),
        ) && asked.looser_than(current)
        {
            permissions.remove("mode");
            refused.push(format!(
                "permissions.mode = \"{}\" (you use {})",
                asked.as_str(),
                current.as_str()
            ));
        }
    }
    refused
}

fn merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(previous) => merge(previous, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        // Rule lists accumulate, so a project file can't drop personal deny rules.
        (toml::Value::Array(base), toml::Value::Array(overlay)) => base.extend(overlay),
        (base, overlay) => *base = overlay,
    }
}

fn deserialize_effort<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Effort>, D::Error> {
    let value = String::deserialize(deserializer)?;
    Effort::parse_setting(&value).map_err(serde::de::Error::custom)
}

fn deserialize_thinking<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Thinking, D::Error> {
    let value = String::deserialize(deserializer)?;
    Thinking::parse_setting(&value).map_err(serde::de::Error::custom)
}
