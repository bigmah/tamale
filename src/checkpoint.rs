//! File snapshots for `/undo` and `/rewind`.
//!
//! Before the agent's first write to a path in a turn, the file's original bytes (or the
//! fact that it didn't exist) are saved under `sessions/<id>/checkpoints/<turn>/`. Shell
//! commands are only counted: their side effects can't be reverted.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const MANIFEST: &str = "manifest.json";

/// A user turn that can be rewound, saved with the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub turn: u64,
    /// The prompt, put back in the input box when the turn is undone.
    pub prompt: String,
    /// Transcript length before the prompt.
    pub entries_len: usize,
    /// `None` once the conversation has been compacted after this turn started.
    pub history: Option<HistoryMark>,
}

/// Where the conversation stood before the prompt was added.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryMark {
    pub len: usize,
    /// When the prompt was merged into a trailing user message, that message's content
    /// length before the merge.
    pub merged: Option<usize>,
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    /// In snapshot order. `blob` is `None` if the file didn't exist.
    files: Vec<SavedFile>,
    commands: usize,
}

#[derive(Serialize, Deserialize)]
struct SavedFile {
    path: PathBuf,
    blob: Option<String>,
}

/// Records one turn's checkpoint as the agent works.
pub struct Recorder {
    dir: PathBuf,
    manifest: Manifest,
    seen: HashSet<PathBuf>,
}

impl Recorder {
    /// Any leftover checkpoint in `dir` (from a turn number that was undone) is replaced.
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::remove_dir_all(&dir);
        Self {
            dir,
            manifest: Manifest::default(),
            seen: HashSet::new(),
        }
    }

    /// Saves the file's current contents if this is the turn's first write to `path`
    /// (an absolute path).
    pub fn before_write(&mut self, path: &Path) -> Result<()> {
        if self.seen.contains(path) {
            return Ok(());
        }
        let blob = match std::fs::read(path) {
            Ok(bytes) => {
                std::fs::create_dir_all(&self.dir)
                    .with_context(|| format!("creating {}", self.dir.display()))?;
                let name = format!("{}.bin", self.manifest.files.len());
                crate::secure::write_private(&self.dir.join(&name), bytes)
                    .with_context(|| format!("saving a checkpoint of {}", path.display()))?;
                Some(name)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("reading {} to checkpoint it", path.display()));
            }
        };
        self.manifest.files.push(SavedFile {
            path: path.to_path_buf(),
            blob,
        });
        self.seen.insert(path.to_path_buf());
        self.write_manifest()
    }

    pub fn command_ran(&mut self) -> Result<()> {
        self.manifest.commands += 1;
        self.write_manifest()
    }

    fn write_manifest(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let path = self.dir.join(MANIFEST);
        let tmp = self.dir.join(format!("{MANIFEST}.tmp"));
        crate::secure::write_private(&tmp, serde_json::to_vec(&self.manifest)?)?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }
}

fn read_manifest(dir: &Path) -> Result<Manifest> {
    match std::fs::read(dir.join(MANIFEST)) {
        Ok(data) => serde_json::from_slice(&data)
            .with_context(|| format!("parsing {}", dir.join(MANIFEST).display())),
        // The turn never wrote a file or ran a command.
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Manifest::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", dir.join(MANIFEST).display())),
    }
}

/// Files changed and commands run in a recorded turn, for the rewind picker.
pub fn summary(dir: &Path) -> (usize, usize) {
    read_manifest(dir).map_or((0, 0), |m| (m.files.len(), m.commands))
}

#[derive(Debug, Default)]
pub struct Restored {
    /// Distinct paths put back (rewritten or removed).
    pub files: usize,
    pub commands: usize,
    pub errors: Vec<String>,
}

/// Restores checkpoints, newest first, so each file ends up as it was before the oldest
/// turn in `dirs`. Failures are collected rather than stopping the rest.
pub fn restore(dirs: &[PathBuf]) -> Restored {
    let mut restored = Restored::default();
    let mut paths = HashSet::new();
    for dir in dirs.iter().rev() {
        let manifest = match read_manifest(dir) {
            Ok(m) => m,
            Err(e) => {
                restored.errors.push(format!("{e:#}"));
                continue;
            }
        };
        restored.commands += manifest.commands;
        // Within a turn, a path is saved once; reverse order still matters for aliases.
        for file in manifest.files.iter().rev() {
            match restore_file(dir, file) {
                Ok(()) => {
                    paths.insert(file.path.clone());
                }
                Err(e) => restored.errors.push(format!("{e:#}")),
            }
        }
    }
    restored.files = paths.len();
    restored
}

fn restore_file(dir: &Path, file: &SavedFile) -> Result<()> {
    match &file.blob {
        Some(blob) => {
            let bytes = std::fs::read(dir.join(blob))
                .with_context(|| format!("reading the checkpoint of {}", file.path.display()))?;
            if let Some(parent) = file.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&file.path, bytes)
                .with_context(|| format!("restoring {}", file.path.display()))
        }
        None => match std::fs::remove_file(&file.path) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", file.path.display()))
            }
            _ => Ok(()),
        },
    }
}

pub fn discard(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_files_across_turns_to_their_earliest_state() {
        let root =
            std::env::temp_dir().join(format!("tamale-checkpoint-{}", crate::session::new_id()));
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let existing = work.join("existing.txt");
        let created = work.join("new/created.txt");
        std::fs::write(&existing, "original").unwrap();

        let first = root.join("checkpoints/1");
        let mut turn = Recorder::new(first.clone());
        turn.before_write(&existing).unwrap();
        std::fs::write(&existing, "turn 1").unwrap();
        // Later writes in the same turn keep the first snapshot.
        turn.before_write(&existing).unwrap();
        std::fs::write(&existing, "turn 1 again").unwrap();
        turn.command_ran().unwrap();

        let second = root.join("checkpoints/2");
        let mut turn = Recorder::new(second.clone());
        turn.before_write(&existing).unwrap();
        turn.before_write(&created).unwrap();
        std::fs::write(&existing, "turn 2").unwrap();
        std::fs::create_dir_all(created.parent().unwrap()).unwrap();
        std::fs::write(&created, "new").unwrap();
        assert_eq!(summary(&second), (2, 0));

        let undone = restore(std::slice::from_ref(&second));
        assert_eq!((undone.files, undone.commands), (2, 0));
        assert!(undone.errors.is_empty());
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "turn 1 again");
        assert!(!created.exists());

        // A turn with no recorded activity restores nothing.
        let empty = restore(&[root.join("checkpoints/never")]);
        assert_eq!((empty.files, empty.commands), (0, 0));

        std::fs::write(&existing, "turn 2").unwrap();
        let rewound = restore(&[first.clone(), second]);
        assert_eq!((rewound.files, rewound.commands), (2, 1));
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "original");

        // Reusing a turn number starts a clean checkpoint.
        Recorder::new(first.clone());
        assert_eq!(summary(&first), (0, 0));
        std::fs::remove_dir_all(root).unwrap();
    }
}
