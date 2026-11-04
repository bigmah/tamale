//! Earlier prompts for ↑/↓ recall in the input box. Prompts are kept per working directory
//! in `<sessions>/.prompts.jsonl`, so recall works across sessions and restarts.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session;

/// Prompts loaded for recall.
const MAX_PROMPTS: usize = 1000;
/// The file is trimmed to its newer half once it grows past this.
const MAX_FILE_BYTES: u64 = 4 << 20;

#[derive(Default)]
pub struct Prompts {
    /// Oldest first.
    list: Vec<String>,
    /// Position while browsing; `None` when editing a new prompt.
    index: Option<usize>,
    /// The unsent input from before browsing started, restored past the newest prompt.
    draft: String,
    /// Where new prompts are appended, and the directory they belong to.
    file: Option<(PathBuf, PathBuf)>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    cwd: PathBuf,
    text: String,
}

impl Prompts {
    /// Loads earlier prompts from `cwd` and saves new ones there. Without a sessions
    /// directory, history lasts for this run only.
    pub fn load(cwd: &Path) -> Self {
        let Some(path) = session::dir().map(|d| d.join(".prompts.jsonl")) else {
            return Self::default();
        };
        Self::load_from(path, cwd)
    }

    fn load_from(path: PathBuf, cwd: &Path) -> Self {
        let content = fs::read_to_string(&path).unwrap_or_default();
        if content.len() as u64 > MAX_FILE_BYTES {
            // Keep the newer half; a failed trim just leaves the file as it was.
            let lines: Vec<&str> = content.lines().collect();
            let kept = lines[lines.len() / 2..].join("\n") + "\n";
            let tmp = path.with_extension(format!("jsonl.{}.tmp", session::new_id()));
            if crate::secure::write_private(&tmp, kept).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
        let mut list: Vec<String> = content
            .lines()
            .filter_map(|line| serde_json::from_str::<Saved>(line).ok())
            .filter(|saved| saved.cwd == cwd)
            .map(|saved| saved.text)
            .collect();
        list.dedup();
        let skip = list.len().saturating_sub(MAX_PROMPTS);
        list.drain(..skip);
        Self {
            list,
            file: Some((path, cwd.to_path_buf())),
            ..Self::default()
        }
    }

    /// Records a sent prompt and stops browsing.
    pub fn add(&mut self, text: &str) {
        self.index = None;
        self.draft.clear();
        if self.list.last().is_some_and(|last| last == text) {
            return;
        }
        self.list.push(text.to_string());
        if let Some((path, cwd)) = &self.file {
            let saved = Saved {
                cwd: cwd.clone(),
                text: text.to_string(),
            };
            if let Ok(line) = serde_json::to_string(&saved) {
                let _ = path.parent().map(fs::create_dir_all);
                let _ = crate::secure::append_private(path, format!("{line}\n"));
            }
        }
    }

    /// The prompt before the one shown; `current` is the input, kept as the draft when
    /// browsing starts. `None` at the oldest prompt.
    pub fn prev(&mut self, current: &str) -> Option<String> {
        let index = match self.index {
            None => {
                self.draft = current.to_string();
                self.list.len()
            }
            Some(i) => i,
        };
        let index = index.checked_sub(1)?;
        self.index = Some(index);
        Some(self.list[index].clone())
    }

    /// The prompt after the one shown, or the draft past the newest. `None` when not browsing.
    pub fn next(&mut self) -> Option<String> {
        let index = self.index? + 1;
        if index >= self.list.len() {
            self.index = None;
            return Some(std::mem::take(&mut self.draft));
        }
        self.index = Some(index);
        Some(self.list[index].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browses_back_and_forward_and_restores_the_draft() {
        let mut prompts = Prompts::default();
        prompts.add("one");
        prompts.add("two");
        prompts.add("two");
        assert_eq!(prompts.next(), None);
        assert_eq!(prompts.prev("draft").as_deref(), Some("two"));
        assert_eq!(prompts.prev("two").as_deref(), Some("one"));
        assert_eq!(prompts.prev("one"), None);
        assert_eq!(prompts.next().as_deref(), Some("two"));
        assert_eq!(prompts.next().as_deref(), Some("draft"));
        assert_eq!(prompts.next(), None);
    }

    #[test]
    fn persists_prompts_per_directory() {
        let dir = std::env::temp_dir().join(format!("tamale-prompts-{}", session::new_id()));
        let path = dir.join(".prompts.jsonl");
        let here = Path::new("/work/a");
        let mut a = Prompts::load_from(path.clone(), here);
        a.add("first\nwith newline");
        Prompts::load_from(path.clone(), Path::new("/work/b")).add("elsewhere");
        a.add("second");

        let mut loaded = Prompts::load_from(path, here);
        assert_eq!(loaded.prev("").as_deref(), Some("second"));
        assert_eq!(loaded.prev("").as_deref(), Some("first\nwith newline"));
        assert_eq!(loaded.prev(""), None);
        let _ = fs::remove_dir_all(dir);
    }
}
