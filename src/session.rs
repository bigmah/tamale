//! Saved conversations, so a closed session can be picked back up with `/resume`.
//!
//! Each session is one JSON file in `$XDG_DATA_HOME/harness/sessions` (by default
//! `~/.local/share/harness/sessions`), rewritten whenever the conversation changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rig_core::completion::Message;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

/// A line of the visible transcript, saved so a resumed session looks like it did.
#[derive(Clone, Serialize, Deserialize)]
pub enum Entry {
    User(String),
    Assistant(String),
    Tool {
        name: String,
        summary: String,
        description: String,
        output: Option<(String, bool)>,
    },
    Info(String),
    Error(String),
}

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// Working directory the session was started in.
    pub cwd: PathBuf,
    /// Unix seconds.
    pub created: u64,
    pub updated: u64,
    pub model: String,
    /// The first user message, used as the session's label.
    pub title: String,
    pub context_tokens: u64,
    pub history: Vec<Message>,
    pub entries: Vec<Entry>,
}

/// The listing fields of a session; the (possibly large) history is skipped when parsing.
#[derive(Clone, Deserialize)]
pub struct Meta {
    pub id: String,
    pub cwd: PathBuf,
    pub updated: u64,
    pub model: String,
    pub title: String,
    /// Number of user prompts in the transcript (filled in by `list`).
    #[serde(skip)]
    pub prompts: usize,
}

#[derive(Deserialize)]
struct MetaWithEntries {
    #[serde(flatten)]
    meta: Meta,
    /// Each entry is `{"Variant": data}`; only the variant name is kept.
    #[serde(default)]
    entries: Vec<HashMap<String, IgnoredAny>>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A new, unique, chronologically sortable session id.
pub fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:x}-{:x}", std::process::id())
}

pub fn dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("HARNESS_SESSIONS_DIR") {
        return Some(PathBuf::from(dir));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("harness").join("sessions"))
}

fn path_in(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// Writes the session atomically (temp file + rename) so a crash never leaves a torn file.
pub fn save(session: &Session) -> Result<()> {
    let dir = dir().context("can't determine the sessions directory (HOME not set)")?;
    save_in(&dir, session)
}

fn save_in(dir: &Path, session: &Session) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = path_in(dir, &session.id);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(session)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn load(id: &str) -> Result<Session> {
    let dir = dir().context("can't determine the sessions directory (HOME not set)")?;
    load_in(&dir, id)
}

fn load_in(dir: &Path, id: &str) -> Result<Session> {
    let path = path_in(dir, id);
    let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&data).with_context(|| format!("parsing {}", path.display()))
}

/// Saved sessions, most recently updated first. With `cwd`, only sessions started there.
pub fn list(cwd: Option<&Path>) -> Vec<Meta> {
    dir().map(|d| list_in(&d, cwd)).unwrap_or_default()
}

fn list_in(dir: &Path, cwd: Option<&Path>) -> Vec<Meta> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Meta> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| {
            let data = std::fs::read(&p).ok()?;
            let parsed: MetaWithEntries = serde_json::from_slice(&data).ok()?;
            let mut meta = parsed.meta;
            meta.prompts = parsed
                .entries
                .iter()
                .filter(|e| e.contains_key("User"))
                .count();
            Some(meta)
        })
        .filter(|m| cwd.is_none_or(|c| m.cwd == c))
        .collect();
    out.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id)));
    out
}

/// Human-friendly age of a timestamp, e.g. "just now", "5m ago", "3d ago".
pub fn ago(then: u64) -> String {
    let secs = now().saturating_sub(then);
    match secs {
        0..60 => "just now".into(),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        86_400..2_592_000 => format!("{}d ago", secs / 86_400),
        _ => format!("{}mo ago", secs / 2_592_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::AssistantContent;
    use rig_core::completion::message::{ToolResultContent, UserContent};

    fn session(id: &str, cwd: &str, updated: u64) -> Session {
        let call =
            AssistantContent::tool_call("call_1", "bash", serde_json::json!({"command": "ls"}));
        Session {
            id: id.into(),
            cwd: cwd.into(),
            created: updated,
            updated,
            model: "claude-test".into(),
            title: format!("title {id}"),
            context_tokens: 42,
            history: vec![
                Message::user("hi"),
                Message::Assistant {
                    id: None,
                    content: vec![call],
                },
                Message::User {
                    content: vec![UserContent::tool_result(
                        "call_1",
                        "bash",
                        vec![ToolResultContent::text("file.txt")],
                    )],
                },
            ],
            entries: vec![
                Entry::User("hi".into()),
                Entry::Tool {
                    name: "bash".into(),
                    summary: "ls".into(),
                    description: "List files".into(),
                    output: Some(("file.txt".into(), false)),
                },
            ],
        }
    }

    #[test]
    fn save_load_and_list_round_trip() {
        let dir = std::env::temp_dir().join(format!("harness-sessions-test-{}", new_id()));
        save_in(&dir, &session("a", "/proj", 10)).unwrap();
        save_in(&dir, &session("b", "/proj", 20)).unwrap();
        save_in(&dir, &session("c", "/other", 30)).unwrap();

        let loaded = load_in(&dir, "a").unwrap();
        assert_eq!(loaded.history, session("a", "/proj", 10).history);
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.context_tokens, 42);

        let ids = |v: Vec<Meta>| v.into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids(list_in(&dir, None)), ["c", "b", "a"]);
        assert_eq!(ids(list_in(&dir, Some(Path::new("/proj")))), ["b", "a"]);
        assert_eq!(list_in(&dir, None)[0].prompts, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
