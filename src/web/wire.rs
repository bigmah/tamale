//! The `/web` messages that tamale reads as well as writes: a session's status, the tool
//! call waiting for approval, and its entries. The server sends them to every client,
//! and `crate::remote` reads them from a session on another machine. Both use these
//! types, so neither side can change a field alone. (The page reads the same JSON in
//! `app.js`; the iPhone app is checked against samples of it, see `fixtures`.)
//!
//! The session at the other end may be an older or a newer tamale, so reading is lenient:
//! a missing field takes its default and an unknown one is ignored.

use serde::{Deserialize, Serialize};

use crate::session;

/// The session's state besides its entries, as the page's status bar shows it.
#[derive(Serialize, Deserialize, PartialEq, Default, Clone, Debug)]
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
    /// Unix seconds when the current run began.
    pub since: Option<u64>,
    /// Unix seconds when the run's current step (model request, tool call, wait) began.
    pub step_since: Option<u64>,
    /// What the run is doing now (a tool's description, "Thinking", …), its kind (think,
    /// write, tool, subagent, wait, compact; see `app::activity`), its tool calls so far
    /// and its subagent's.
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
    /// Where the goal stands, as the TUI shows it: `goal 3/20 · 4m`, `goal paused 3/20`, ….
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_state: Option<String>,
    pub queued: Vec<String>,
    /// Whether this session is a tamale chef (see `crate::chef`).
    pub chef: bool,
    /// Whether it answers a chef's side questions (`ask`); older tamales don't.
    pub asks: bool,
}

/// A tool call waiting for an answer.
#[derive(Serialize, Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct Approval {
    pub name: String,
    pub summary: String,
    pub description: String,
    pub always: Vec<String>,
}

/// One entry of the conversation, as an `entries` message carries it.
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Entry {
    User {
        #[serde(default)]
        text: String,
    },
    Assistant {
        /// The reply as the page shows it, its artifacts embedded.
        #[serde(default)]
        html: String,
        /// For clients that render markdown themselves: the reply without its artifacts,
        /// and the artifacts to put where their markers are (`artifact_marker`).
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
        /// `None` until the call has finished.
        #[serde(default)]
        output: Option<String>,
        /// Whether `output` is an error.
        #[serde(default)]
        error: bool,
        /// Whether `output` is only the start of a long one.
        #[serde(default)]
        cut: bool,
    },
}

/// An artifact in an assistant's reply.
#[derive(Serialize, Deserialize, PartialEq, Default, Clone, Debug)]
#[serde(default)]
pub struct Artifact {
    /// What to ask for its document by, once it's finished.
    pub id: Option<String>,
    pub title: String,
    /// Whether its block has closed, rather than still being written.
    pub done: bool,
    pub hash: Option<String>,
}

impl Entry {
    /// The entry as a transcript holds it. Each artifact goes back where its marker is,
    /// as the kind of block the transcript shows as one line with its title; its document
    /// stays on the other machine.
    pub fn into_session(self) -> session::Entry {
        match self {
            Self::User { text } => session::Entry::User(text),
            Self::Assistant { md, artifacts, .. } => {
                let mut text = md;
                for (n, artifact) in artifacts.iter().enumerate() {
                    let title = match artifact.title.as_str() {
                        "" => "Artifact",
                        title => title,
                    };
                    let title = title.replace('\\', "\\\\").replace('"', "\\\"");
                    let block = format!("```openui\ntitle = \"{title}\"\n```");
                    text = text.replacen(&super::artifact_marker(n), &block, 1);
                }
                session::Entry::Assistant(text)
            }
            Self::Note { md, .. } => session::Entry::Note(md),
            Self::Info { text } => session::Entry::Info(text),
            Self::Error { text } => session::Entry::Error(text),
            Self::Tool {
                name,
                summary,
                description,
                output,
                error,
                ..
            } => session::Entry::Tool {
                name,
                summary,
                description,
                output: output.map(|output| (output, error)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    /// `entry` as the server sends it and another tamale then reads it.
    fn there_and_back(entry: &session::Entry) -> session::Entry {
        let sent = super::super::mirror::wire_entry(entry, &Mutex::default(), None);
        let read: Entry = serde_json::from_str(&serde_json::to_string(&sent).unwrap()).unwrap();
        assert_eq!(read, sent);
        read.into_session()
    }

    #[test]
    fn entries_read_back_as_they_were_sent() {
        let tool = |output| session::Entry::Tool {
            name: "bash".into(),
            summary: "cargo test".into(),
            description: "Run the tests".into(),
            output,
        };
        for entry in [
            session::Entry::User("Why does login fail?".into()),
            session::Entry::Assistant("It drops the **cookie**.\n\n```rust\nfn x() {}\n```".into()),
            session::Entry::Note("Compacted *3 turns*.".into()),
            session::Entry::Info("Welcome!".into()),
            session::Entry::Error("The backend timed out.".into()),
            tool(None),
            tool(Some(("ok".into(), false))),
            tool(Some(("1 failed".into(), true))),
        ] {
            // Compared as they're saved: a transcript's entries have no `==` of their own.
            assert_eq!(json!(there_and_back(&entry)), json!(entry));
        }
    }

    #[test]
    fn an_artifact_comes_back_as_its_title_where_it_was() {
        let reply = "Here:\n\n```openui\ntitle = \"Login flow\"\nroot = Card([])\n```\n\nDone.";
        let session::Entry::Assistant(text) =
            there_and_back(&session::Entry::Assistant(reply.into()))
        else {
            panic!("not a reply");
        };
        assert_eq!(
            text,
            "Here:\n\n```openui\ntitle = \"Login flow\"\n```\n\nDone."
        );
    }

    #[test]
    fn a_status_reads_back_as_it_was_sent() {
        let status = Status {
            project: "tamale".into(),
            mode: "ask".into(),
            effort: Some("high".into()),
            running: true,
            since: Some(1_700_000_000),
            step_kind: "tool".into(),
            cost: 0.42,
            goal_state: Some("goal 2/20 · 4m".into()),
            queued: vec!["then run the tests".into()],
            asks: true,
            ..Status::default()
        };
        let sent = serde_json::to_string(&status).unwrap();
        assert_eq!(serde_json::from_str::<Status>(&sent).unwrap(), status);
    }

    #[test]
    fn what_another_version_sends_still_reads() {
        let status: Status =
            serde_json::from_str(r#"{"model":"m","running":true,"not_yet_known":1}"#).unwrap();
        assert_eq!(
            (status.model.as_str(), status.running, status.asks),
            ("m", true, false)
        );
        let entry: Entry = serde_json::from_str(r#"{"kind":"tool","name":"bash"}"#).unwrap();
        assert!(matches!(
            entry.into_session(),
            session::Entry::Tool { name, output: None, .. } if name == "bash"
        ));
        assert!(serde_json::from_str::<Entry>(r#"{"kind":"not_yet_known"}"#).is_err());
    }
}
