//! Typed settings and TOML layering. Environment variables and CLI flags are
//! applied once at startup; the agent and UI receive the resolved settings.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer};

use crate::agent::{self, Effort, Reasoning, Thinking};

#[cfg(test)]
mod tests;

pub const DEFAULT_TOML: &str = include_str!("../config/default.toml");

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub model: ModelSettings,
    pub agent: AgentSettings,
    pub ui: UiSettings,
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
        if let Some(dir) = user_dir {
            read_layer(&mut layers, &dir.join("config.toml"), false)?;
        }
        for dir in project_dirs(cwd) {
            read_layer(&mut layers, &dir.join("harness.toml"), false)?;
        }
        if let Some(path) = explicit {
            read_layer(&mut layers, path, true)?;
        }
        layers.try_into().context("resolving settings")
    }

    /// Inject the environment lookup so tests don't mutate process-wide variables.
    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) -> Result<()> {
        if let Some(value) = get("HARNESS_MODEL") {
            self.model.name = value;
        }
        if let Some(value) = get("HARNESS_EFFORT").filter(|s| !s.trim().is_empty()) {
            self.model.effort = Effort::parse_setting(&value).context("HARNESS_EFFORT")?;
        }
        if let Some(value) = get("HARNESS_THINKING").filter(|s| !s.trim().is_empty()) {
            self.model.thinking = Thinking::parse_setting(&value).context("HARNESS_THINKING")?;
        }
        if let Some(value) = get("HARNESS_CONTEXT_WINDOW") {
            self.model.context_window = Some(
                value
                    .parse()
                    .context("HARNESS_CONTEXT_WINDOW must be a positive integer")?,
            );
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
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
        }
    }
}

pub fn user_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|dir| dir.join("harness"))
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

fn read_layer(base: &mut toml::Value, path: &Path, required: bool) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if !required && error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let overlay = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
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
