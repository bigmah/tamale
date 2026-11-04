//! Saved conversations, so a closed session can be picked back up with `/resume`.
//!
//! Each session is one JSON file in `$XDG_DATA_HOME/tamale/sessions` (by default
//! `~/.local/share/tamale/sessions`), rewritten whenever the conversation changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
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
    /// Model-written text outside the conversation, such as a goal checker's verdict.
    Note(String),
    Error(String),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// Working directory the session was started in.
    pub cwd: PathBuf,
    /// Extra task directories. Older sessions used only cwd.
    #[serde(default)]
    pub additional_dirs: Vec<PathBuf>,
    /// Unix seconds.
    pub created: u64,
    pub updated: u64,
    pub model: String,
    /// The first user message, used as the session's label.
    pub title: String,
    pub context_tokens: u64,
    pub history: Vec<Message>,
    pub entries: Vec<Entry>,
    /// Turns `/undo` and `/rewind` can return to; file snapshots live in `checkpoint_dir`.
    #[serde(default)]
    pub checkpoints: Vec<crate::checkpoint::Turn>,
}

/// The listing fields of a session; the (possibly large) history is skipped when parsing.
#[derive(Clone, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub cwd: PathBuf,
    pub updated: u64,
    pub model: String,
    pub title: String,
    /// Number of user prompts in the transcript (filled in by `list`).
    #[serde(default)]
    pub prompts: usize,
}

/// Disposable listing cache. Fingerprints ensure an edited/replaced session is
/// reparsed; the session JSON remains the source of truth.
#[derive(Serialize, Deserialize)]
struct Index {
    len: u64,
    modified: SystemTime,
    meta: Meta,
}

fn write_index(dir: &Path, path: &Path, meta: Meta, stat: &std::fs::Metadata) -> Result<()> {
    let index = Index {
        len: stat.len(),
        modified: stat.modified()?,
        meta,
    };
    let index_dir = dir.join(".index");
    std::fs::create_dir_all(&index_dir)?;
    let path = index_dir.join(path.file_name().context("missing session file name")?);
    let tmp = path.with_extension(format!("json.{}.tmp", new_id()));
    crate::secure::write_private(&tmp, serde_json::to_vec(&index)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// One ordered writer owns all session writes. Pending snapshots for the same
/// session are coalesced, and closing the queue drains it before exit.
pub struct Writer {
    queue: Arc<(Mutex<Pending>, Condvar)>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Default)]
struct Pending {
    sessions: HashMap<String, Arc<Session>>,
    closing: bool,
}

impl From<&Session> for Meta {
    fn from(saved: &Session) -> Self {
        Self {
            id: saved.id.clone(),
            cwd: saved.cwd.clone(),
            updated: saved.updated,
            model: saved.model.clone(),
            title: saved.title.clone(),
            prompts: saved
                .entries
                .iter()
                .filter(|e| matches!(e, Entry::User(_)))
                .count(),
        }
    }
}

impl Writer {
    pub fn start(events: tokio::sync::mpsc::UnboundedSender<crate::app::AppEvent>) -> Self {
        let queue = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let worker_queue = queue.clone();
        let task = tokio::task::spawn_blocking(move || {
            let (lock, ready) = &*worker_queue;
            loop {
                let saved = {
                    let mut pending = lock.lock().unwrap();
                    while pending.sessions.is_empty() && !pending.closing {
                        pending = ready.wait(pending).unwrap();
                    }
                    let Some(saved) = pending.sessions.values().next().cloned() else {
                        break;
                    };
                    saved
                };
                let result = save(&saved).map_err(|e| format!("{e:#}"));
                let mut pending = lock.lock().unwrap();
                if pending
                    .sessions
                    .get(&saved.id)
                    .is_some_and(|latest| Arc::ptr_eq(latest, &saved))
                {
                    pending.sessions.remove(&saved.id);
                }
                drop(pending);
                let _ = events.send(crate::app::AppEvent::SessionSaved(saved.id.clone(), result));
            }
        });
        Self {
            queue,
            task: Some(task),
        }
    }

    pub fn save(&self, saved: Session) {
        let (lock, ready) = &*self.queue;
        lock.lock()
            .unwrap()
            .sessions
            .insert(saved.id.clone(), Arc::new(saved));
        ready.notify_one();
    }

    /// Includes in-flight writes, so immediate clear/resume sees the newest snapshot.
    pub fn pending(&self) -> Vec<Arc<Session>> {
        self.queue
            .0
            .lock()
            .unwrap()
            .sessions
            .values()
            .cloned()
            .collect()
    }

    pub async fn finish(mut self) {
        self.close();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    fn close(&self) {
        self.queue.0.lock().unwrap().closing = true;
        self.queue.1.notify_one();
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.close();
    }
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
    if let Some(dir) = std::env::var_os("TAMALE_SESSIONS_DIR") {
        return Some(PathBuf::from(dir));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("tamale").join("sessions"))
}

/// Closes up the places tamale keeps conversations, snapshots of files, tool output and CLI
/// logins, which hold whatever secrets went through them, including what earlier versions left
/// there open. Best effort: a read-only home shouldn't stop tamale starting.
pub fn make_storage_private() {
    if let Some(sessions) = dir() {
        let _ = crate::secure::private_dir(&sessions);
    }
    let others = [
        crate::tools::spill_dir(),
        crate::backend::logins::added_dir(crate::backend::Kind::ClaudeCode),
        crate::backend::logins::added_dir(crate::backend::Kind::Codex),
    ];
    for other in others.into_iter().flatten().filter(|dir| dir.is_dir()) {
        let _ = crate::secure::private_dir(&other);
    }
}

/// File snapshots for one turn of a session: `<sessions>/<id>/checkpoints/<turn>/`.
pub fn checkpoint_dir(id: &str, turn: u64) -> Option<PathBuf> {
    Some(dir()?.join(id).join("checkpoints").join(turn.to_string()))
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
    crate::secure::private_dir(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = path_in(dir, &session.id);
    let tmp = path.with_extension("json.tmp");
    crate::secure::write_private(&tmp, serde_json::to_vec(session)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
    // Cache failures must never prevent saving/resuming the conversation.
    if let Ok(stat) = std::fs::metadata(&path) {
        let _ = write_index(dir, &path, Meta::from(session), &stat);
    }
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
            let stat = std::fs::metadata(&p).ok()?;
            if let Ok(data) = std::fs::read(dir.join(".index").join(p.file_name()?))
                && let Ok(index) = serde_json::from_slice::<Index>(&data)
                && index.len == stat.len()
                && Some(index.modified) == stat.modified().ok()
                && Some(index.meta.id.as_str()) == p.file_stem()?.to_str()
            {
                return Some(index.meta);
            }
            let data = std::fs::read(&p).ok()?;
            let parsed: MetaWithEntries = serde_json::from_slice(&data).ok()?;
            let mut meta = parsed.meta;
            meta.prompts = parsed
                .entries
                .iter()
                .filter(|e| e.contains_key("User"))
                .count();
            let _ = write_index(dir, &p, meta.clone(), &stat);
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
            additional_dirs: Vec::new(),
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
            checkpoints: Vec::new(),
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

    #[cfg(unix)]
    #[test]
    fn saved_sessions_are_this_users_alone() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let dir = std::env::temp_dir().join(format!("tamale-sessions-private-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        // As an earlier version left it.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let session = Session {
            id: "a".into(),
            cwd: "/x".into(),
            additional_dirs: Vec::new(),
            created: 1,
            updated: 1,
            model: "m".into(),
            title: "t".into(),
            context_tokens: 0,
            history: Vec::new(),
            entries: Vec::new(),
            checkpoints: Vec::new(),
        };
        save_in(&dir, &session).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path_in(&dir, "a")), 0o600);
        assert_eq!(mode(&dir.join(".index").join("a.json")), 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn save_load_and_list_round_trip() {
        let dir = std::env::temp_dir().join(format!("tamale-sessions-test-{}", new_id()));
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

    #[test]
    fn additional_directories_round_trip_and_old_sessions_default_to_empty() {
        let mut saved = session("a", "/proj", 10);
        saved.additional_dirs = vec![PathBuf::from("/shared"), PathBuf::from("/other project")];
        let mut json = serde_json::to_value(&saved).unwrap();
        let loaded: Session = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(loaded.additional_dirs, saved.additional_dirs);
        json.as_object_mut().unwrap().remove("additional_dirs");
        let legacy: Session = serde_json::from_value(json).unwrap();
        assert!(legacy.additional_dirs.is_empty());
    }

    #[test]
    fn listing_rebuilds_missing_stale_and_corrupt_indexes() {
        let dir = std::env::temp_dir().join(format!("tamale-index-test-{}", new_id()));
        let mut saved = session("a", "/proj", 10);
        save_in(&dir, &saved).unwrap();
        let index = dir.join(".index/a.json");
        std::fs::remove_file(&index).unwrap();
        assert_eq!(list_in(&dir, None)[0].prompts, 1);
        assert!(
            index.exists(),
            "legacy sessions get an index on first listing"
        );
        std::fs::write(&index, "broken cache").unwrap();
        assert_eq!(list_in(&dir, None)[0].title, saved.title);
        saved.title = "Changed outside the app".into();
        saved.entries.push(Entry::User("another prompt".into()));
        std::fs::write(path_in(&dir, "a"), serde_json::to_vec(&saved).unwrap()).unwrap();
        let listed = list_in(&dir, None);
        assert_eq!(listed[0].title, saved.title);
        assert_eq!(listed[0].prompts, 2);
        std::fs::write(path_in(&dir, "a"), "broken session").unwrap();
        assert!(list_in(&dir, None).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn index_write_failures_do_not_prevent_saving_or_listing() {
        let dir = std::env::temp_dir().join(format!("tamale-index-failure-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".index"), "not a directory").unwrap();
        save_in(&dir, &session("a", "/proj", 10)).unwrap();
        assert_eq!(load_in(&dir, "a").unwrap().id, "a");
        assert_eq!(list_in(&dir, None)[0].id, "a");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod perf_tests {
    use super::*;

    /// Run with `cargo test --release perf_session_listing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn perf_session_listing() {
        let dir = std::env::temp_dir().join(format!("tamale-list-perf-{}", new_id()));
        let mut saved = Session {
            id: String::new(),
            cwd: "/perf-project".into(),
            additional_dirs: Vec::new(),
            created: 1,
            updated: 1,
            model: "test".into(),
            title: "Performance fixture".into(),
            context_tokens: 0,
            history: vec![Message::user("x".repeat(1_000_000))],
            entries: vec![Entry::User("y".repeat(1_000_000))],
            checkpoints: Vec::new(),
        };
        for i in 0..30 {
            saved.id = format!("session-{i}");
            save_in(&dir, &saved).unwrap();
        }
        let start = std::time::Instant::now();
        for _ in 0..10 {
            assert_eq!(list_in(&dir, Some(Path::new("/perf-project"))).len(), 30);
        }
        println!("list 30 sessions (60 MB): {:?}/list", start.elapsed() / 10);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
