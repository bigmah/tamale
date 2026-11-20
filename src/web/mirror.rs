//! What the tabs were last sent, so that a sync carries only what changed, and the
//! entries as they're sent.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::artifacts::{Artifacts, content_hash};
use super::{View, relay, render, wire};
use crate::session::Entry;

/// Tool output sent per call; the rest is cut.
pub(super) const MAX_TOOL_OUTPUT: usize = 24_000;
/// Entries sent per `older` request.
const OLDER_PAGE: usize = 100;
/// The most entry JSON sent in one page of history, so one page stays quick on a phone.
pub(super) const MAX_PAGE_BYTES: usize = 512 * 1024;

/// What the tabs were last sent, to send only what changed.
#[derive(Default)]
pub(super) struct Mirror {
    /// Each entry's content hash and its JSON.
    entries: Vec<(u64, String)>,
    /// What a hub's session list shows, as of the last update.
    pub(super) hub_status: relay::HubStatus,
    status: String,
    approval: String,
    question: String,
    kitchen: String,
    pub(super) artifacts: Arc<Mutex<Artifacts>>,
}

impl Mirror {
    pub(super) fn update(&mut self, view: &View) -> Vec<String> {
        let mut out = Vec::new();
        let keys: Vec<u64> = view.entries.iter().map(entry_key).collect();
        let same = keys
            .iter()
            .zip(&self.entries)
            .take_while(|(key, (sent, _))| *key == sent)
            .count();
        if same < keys.len() || same < self.entries.len() {
            self.entries.truncate(same);
            for (key, entry) in keys[same..].iter().zip(&view.entries[same..]) {
                let entry = wire_entry(entry, &self.artifacts, view.artifact_dir.as_deref());
                let json = json!(entry).to_string();
                self.entries.push((*key, json));
            }
            out.push(self.entries_message(same));
        }
        let status = json!({ "t": "status", "status": view.status }).to_string();
        if status != self.status {
            out.push(status.clone());
            self.status = status;
        }
        let approval = json!({ "t": "approval", "approval": view.approval }).to_string();
        if approval != self.approval {
            out.push(approval.clone());
            self.approval = approval;
        }
        let question = json!({ "t": "question", "question": view.question }).to_string();
        if question != self.question {
            out.push(question.clone());
            self.question = question;
        }
        // Only a chef has one, so other sessions' pages never hear of it.
        let kitchen = match &view.kitchen {
            Some(kitchen) => json!({ "t": "kitchen", "kitchen": kitchen }).to_string(),
            None => String::new(),
        };
        if kitchen != self.kitchen {
            if !kitchen.is_empty() {
                out.push(kitchen.clone());
            }
            self.kitchen = kitchen;
        }
        self.hub_status = relay::HubStatus {
            busy: view.status.running || view.status.compacting,
            approval: view.approval.is_some() || view.question.is_some(),
            title: view
                .entries
                .iter()
                .find_map(|e| match e {
                    Entry::User(text) => {
                        Some(text.lines().next().unwrap_or("").chars().take(80).collect())
                    }
                    _ => None,
                })
                .unwrap_or_default(),
            model: view.status.model.clone(),
        };
        out
    }

    /// All a new tab needs. With `tail`, only that many of the latest entries, after a
    /// `loading` message saying how big they are so the tab can show its progress.
    pub(super) fn full(&self, tail: Option<usize>) -> Vec<String> {
        let from = tail.map_or(0, |count| self.page_start(self.entries.len(), count));
        let entries = self.entries_message(from);
        let loading = tail.map(|_| json!({ "t": "loading", "bytes": entries.len() }).to_string());
        loading
            .into_iter()
            .chain([
                entries,
                self.status.clone(),
                self.approval.clone(),
                self.question.clone(),
                self.kitchen.clone(),
            ])
            .filter(|m| !m.is_empty())
            .collect()
    }

    /// The entries from `from` on, replacing whatever the tab has from there.
    fn entries_message(&self, from: usize) -> String {
        format!(
            r#"{{"t":"entries","from":{from},"items":[{}]}}"#,
            self.items(from, self.entries.len())
        )
    }

    /// The page of entries before `before`, for a tab scrolling back. It says which `before`
    /// it answers, since the tab's history may have been replaced in the meantime.
    pub(super) fn older(&self, before: usize) -> String {
        let before = before.min(self.entries.len());
        let from = self.page_start(before, OLDER_PAGE);
        format!(
            r#"{{"t":"older","before":{before},"from":{from},"items":[{}]}}"#,
            self.items(from, before)
        )
    }

    fn items(&self, from: usize, to: usize) -> String {
        let items: Vec<&str> = self.entries[from..to]
            .iter()
            .map(|(_, j)| j.as_str())
            .collect();
        items.join(",")
    }

    /// Where a page ending at `end` starts: up to `count` entries, fewer if they'd pass
    /// [`MAX_PAGE_BYTES`], but always at least one.
    fn page_start(&self, end: usize, count: usize) -> usize {
        let mut start = end;
        let mut bytes = 0;
        while start > 0 && end - start < count {
            bytes += self.entries[start - 1].1.len();
            if bytes > MAX_PAGE_BYTES && start < end {
                break;
            }
            start -= 1;
        }
        start
    }
}

fn entry_key(entry: &Entry) -> u64 {
    let mut h = DefaultHasher::new();
    match entry {
        Entry::User(t) => (0u8, t).hash(&mut h),
        Entry::Assistant(t) => (1u8, t).hash(&mut h),
        Entry::Tool {
            name,
            summary,
            description,
            output,
        } => (2u8, name, summary, description, output).hash(&mut h),
        Entry::Info(t) => (3u8, t).hash(&mut h),
        Entry::Note(t) => (4u8, t).hash(&mut h),
        Entry::Error(t) => (5u8, t).hash(&mut h),
    }
    h.finish()
}

/// `entry` as an `entries` message carries it, its finished artifacts registered so they
/// can be asked for.
pub(super) fn wire_entry(
    entry: &Entry,
    artifacts: &Mutex<Artifacts>,
    dir: Option<&std::path::Path>,
) -> wire::Entry {
    match entry {
        Entry::User(text) => wire::Entry::User { text: text.clone() },
        Entry::Assistant(text) => {
            let mut artifacts = artifacts.lock().unwrap();
            let mut register = |html: &str| artifacts.register(html, dir);
            let html = render::markdown_with(text, Some(&mut register));
            // For native clients, which render markdown themselves: the text without its
            // artifacts, and the artifacts to put where their markers are.
            let (md, blocks) = render::split_artifacts(text);
            let artifacts = blocks
                .iter()
                .map(|(body, done)| wire::Artifact {
                    id: done.then(|| {
                        let url = register(body);
                        url.trim_start_matches("/artifact/").to_string()
                    }),
                    title: render::artifact_title(body).unwrap_or_else(|| "Artifact".into()),
                    done: *done,
                    hash: done.then(|| content_hash(body)),
                })
                .collect();
            wire::Entry::Assistant {
                html,
                md,
                artifacts,
            }
        }
        Entry::Note(text) => wire::Entry::Note {
            html: render::markdown(text),
            md: text.clone(),
        },
        Entry::Info(text) => wire::Entry::Info { text: text.clone() },
        Entry::Error(text) => wire::Entry::Error { text: text.clone() },
        Entry::Tool {
            name,
            summary,
            description,
            output,
        } => {
            let (output, error, cut) = match output {
                Some((text, error)) => {
                    let (text, cut) = clip(text, MAX_TOOL_OUTPUT);
                    (Some(text.to_string()), *error, cut)
                }
                None => (None, false, false),
            };
            wire::Entry::Tool {
                name: name.clone(),
                summary: summary.clone(),
                description: description.clone(),
                output,
                error,
                cut,
            }
        }
    }
}

/// At most `max` bytes of `text`, cut at a character boundary, and whether it was cut.
pub(super) fn clip(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}
