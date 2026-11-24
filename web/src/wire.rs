//! The messages tamale's server sends this page and the ones it takes back (see
//! `src/web.rs` and `src/web/wire.rs` in tamale). Reading is lenient, as there: the session
//! may be an older or a newer tamale, so a missing field takes its default and an unknown
//! one is ignored.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The session's state besides its entries, as the top bar shows it.
#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct Status {
    pub project: String,
    pub cwd: String,
    pub dirs: Vec<String>,
    pub model: String,
    pub backend: String,
    pub signed_in: bool,
    pub mode: String,
    pub effort: Option<String>,
    pub thinking: String,
    pub running: bool,
    pub compacting: bool,
    pub since: Option<u64>,
    pub step_since: Option<u64>,
    pub step: String,
    pub step_kind: String,
    pub steps: usize,
    pub sub_steps: usize,
    pub context: u64,
    pub window: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
    pub cost_partial: bool,
    pub goal: Option<String>,
    pub goal_state: Option<String>,
    pub queued: Vec<String>,
    pub chef: bool,
}

/// A tool call waiting for an answer.
#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct Approval {
    pub name: String,
    pub summary: String,
    pub description: String,
    pub always: Vec<String>,
}

/// An artifact in an assistant's reply.
#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct Artifact {
    pub id: Option<String>,
    pub title: String,
    pub done: bool,
    pub hash: Option<String>,
}

/// One entry of the conversation.
#[derive(Deserialize, PartialEq, Clone, Debug)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Entry {
    User {
        #[serde(default)]
        text: String,
    },
    Assistant {
        #[serde(default)]
        html: String,
        #[serde(default)]
        md: String,
        #[serde(default)]
        artifacts: Vec<Artifact>,
    },
    Note {
        #[serde(default)]
        html: String,
        #[serde(default)]
        md: String,
    },
    Info {
        #[serde(default)]
        text: String,
    },
    Error {
        #[serde(default)]
        text: String,
    },
    Tool {
        #[serde(default)]
        name: String,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        error: bool,
        #[serde(default)]
        cut: bool,
    },
    /// A kind this page doesn't know, from a newer tamale: shown as nothing.
    #[serde(other)]
    Unknown,
}

impl Entry {
    pub fn kind(&self) -> &'static str {
        match self {
            Entry::User { .. } => "user",
            Entry::Assistant { .. } => "assistant",
            Entry::Note { .. } => "note",
            Entry::Info { .. } => "info",
            Entry::Error { .. } => "error",
            Entry::Tool { .. } => "tool",
            Entry::Unknown => "unknown",
        }
    }

    pub fn is_tool(&self) -> bool {
        matches!(self, Entry::Tool { .. })
    }
}

/// One of the agent's questions.
#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct Question {
    pub question: String,
    pub header: String,
    pub options: Vec<Choice>,
    #[serde(alias = "multiSelect")]
    pub multi_select: bool,
}

#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct Choice {
    pub label: String,
    pub description: String,
}

/// The agent's questions, waiting for answers.
#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct Questions {
    pub description: String,
    pub questions: Vec<Question>,
}

/// One answer, as `answer` sends it.
#[derive(Serialize, Deserialize, PartialEq, Default, Clone, Debug)]
pub struct Answer {
    pub chosen: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
}

/// A chef's view of the other sessions.
#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct Kitchen {
    pub sessions: Vec<KitchenSession>,
    pub problem: Option<String>,
    pub loading: bool,
}

#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct KitchenSession {
    pub id: String,
    pub host: String,
    pub cwd: String,
    pub title: String,
    pub model: String,
    pub state: String,
    pub viewers: usize,
}

/// A model the picker lists.
#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct Model {
    pub id: String,
    pub name: String,
}

/// Saved artifacts of one session, for the gallery.
#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct SavedSession {
    pub current: bool,
    pub title: String,
    pub updated: f64,
    pub items: Vec<SavedArtifact>,
}

#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct SavedArtifact {
    pub url: String,
    pub title: String,
    pub file: String,
    pub modified: f64,
}

/// The voice assistant's settings, as tamale keeps them.
#[derive(Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct GeminiSettings {
    pub key: Option<String>,
    pub model: Option<String>,
    pub voice: Option<String>,
    /// `env` or `saved`: where the key came from.
    pub from: Option<String>,
    /// The environment variable tamale reads a key from.
    pub env: Option<String>,
    pub error: Option<String>,
}

/// Reads `value` as `T`, or its default if it isn't one.
pub fn read<T: for<'de> Deserialize<'de> + Default>(value: Option<&Value>) -> T {
    value
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// Reads `value` as `T` if it's there and isn't null.
pub fn read_opt<T: for<'de> Deserialize<'de>>(value: Option<&Value>) -> Option<T> {
    match value {
        None | Some(Value::Null) => None,
        Some(v) => serde_json::from_value(v.clone()).ok(),
    }
}

/// The entries of an `entries` message, leaving out any that don't read.
pub fn entries(value: Option<&Value>) -> Vec<Entry> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| serde_json::from_value(item.clone()).unwrap_or(Entry::Unknown))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_tamale_sends() {
        let e: Entry =
            serde_json::from_str(r#"{"kind":"tool","name":"bash","output":"ok"}"#).unwrap();
        assert!(matches!(e, Entry::Tool { ref name, output: Some(_), .. } if name == "bash"));
        let e: Entry = serde_json::from_str(r#"{"kind":"newer_thing"}"#).unwrap();
        assert_eq!(e, Entry::Unknown);
        let s: Status = serde_json::from_str(r#"{"model":"m","running":true,"x":1}"#).unwrap();
        assert!(s.running);
        let q: Questions = serde_json::from_str(
            r#"{"description":"d","questions":[{"question":"q","options":[{"label":"a"}],"multiSelect":true}]}"#,
        )
        .unwrap();
        assert!(q.questions[0].multi_select);
    }
}
