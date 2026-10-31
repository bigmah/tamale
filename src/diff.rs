//! Loading and parsing `git diff` output for the `/diff` viewer.

#[cfg(test)]
mod tests;

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Untracked files larger than this are shown as binary instead of read in full.
const MAX_UNTRACKED_BYTES: u64 = 1 << 20;
/// The hash of git's empty tree, used as the base before the first commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Added,
    Deleted,
    Modified,
    Renamed,
    Untracked,
}

impl Status {
    pub fn letter(self) -> char {
        match self {
            Status::Added => 'A',
            Status::Deleted => 'D',
            Status::Modified => 'M',
            Status::Renamed => 'R',
            Status::Untracked => 'U',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file`
    NoNewline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: Kind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// The `@@ -a,b +c,d @@` header, without the trailing section heading.
    pub header: String,
    /// The function or section git found for the hunk, if any.
    pub section: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// The path before a rename.
    pub old_path: Option<String>,
    pub status: Status,
    pub binary: bool,
    /// `(old, new)` file modes when they changed.
    pub mode_change: Option<(String, String)>,
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub removed: usize,
}

impl FileDiff {
    fn new(path: String, status: Status) -> Self {
        Self {
            path,
            old_path: None,
            status,
            binary: false,
            mode_change: None,
            hunks: Vec::new(),
            added: 0,
            removed: 0,
        }
    }
}

/// What a diff compares, from the `/diff` arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Arguments passed to `git diff`; empty compares the working tree with `HEAD`.
    pub args: Vec<String>,
}

impl Target {
    pub fn parse(args: &str) -> Self {
        Self {
            args: args.split_whitespace().map(str::to_string).collect(),
        }
    }

    pub fn label(&self) -> String {
        if self.args.is_empty() {
            "working tree vs HEAD".into()
        } else {
            format!("git diff {}", self.args.join(" "))
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .output()
        .context("couldn't run git")
}

/// Runs `git diff` in `dir` and parses it. Paths are relative to the repository root.
/// The default target also lists untracked files as additions.
pub fn load(dir: &Path, target: &Target) -> Result<Vec<FileDiff>> {
    let inside = git(dir, &["rev-parse", "--is-inside-work-tree"])?;
    if !inside.status.success() {
        bail!("{} is not inside a git repository", dir.display());
    }
    let root = git(dir, &["rev-parse", "--show-toplevel"])?;
    let root = String::from_utf8_lossy(&root.stdout).trim().to_string();
    let root = Path::new(&root);

    let mut args = vec![
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--find-renames",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        // Whole files, so the viewer can expand unchanged lines; it hides most of them.
        "--unified=1000000000",
    ];
    let has_head = git(root, &["rev-parse", "--verify", "--quiet", "HEAD"])?
        .status
        .success();
    if target.args.is_empty() {
        args.push(if has_head { "HEAD" } else { EMPTY_TREE });
    } else {
        args.extend(target.args.iter().map(String::as_str));
    }
    let output = git(root, &args)?;
    if !output.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut files = parse(&String::from_utf8_lossy(&output.stdout));

    if target.args.is_empty() {
        let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        for path in String::from_utf8_lossy(&untracked.stdout).split('\0') {
            if !path.is_empty() {
                files.push(untracked_file(root, path));
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// An untracked file shown as entirely added.
fn untracked_file(root: &Path, path: &str) -> FileDiff {
    let mut file = FileDiff::new(path.to_string(), Status::Untracked);
    let full = root.join(path);
    let too_big = std::fs::metadata(&full).is_ok_and(|m| m.len() > MAX_UNTRACKED_BYTES);
    let bytes = if too_big {
        None
    } else {
        std::fs::read(&full).ok()
    };
    match bytes {
        Some(bytes) if !bytes.contains(&0) => {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<DiffLine> = text
                .lines()
                .enumerate()
                .map(|(i, line)| DiffLine {
                    kind: Kind::Added,
                    old: None,
                    new: Some(i as u32 + 1),
                    text: line.to_string(),
                })
                .collect();
            file.added = lines.len();
            if !lines.is_empty() {
                file.hunks.push(Hunk {
                    header: format!("@@ -0,0 +1,{} @@", lines.len()),
                    section: String::new(),
                    lines,
                });
            }
        }
        _ => file.binary = true,
    }
    file
}

/// Parses unified diff output from `git diff` with `a/` and `b/` prefixes.
pub fn parse(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let (mut old_line, mut new_line) = (0u32, 0u32);
    let mut in_hunk = false;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.push(FileDiff::new(header_path(rest), Status::Modified));
            in_hunk = false;
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if in_hunk {
            let (kind, text) = match line.as_bytes().first() {
                Some(b'+') => (Kind::Added, &line[1..]),
                Some(b'-') => (Kind::Removed, &line[1..]),
                Some(b' ') => (Kind::Context, &line[1..]),
                Some(b'\\') => (Kind::NoNewline, line),
                // Some tools strip the space from empty context lines.
                None => (Kind::Context, ""),
                _ => {
                    in_hunk = false;
                    (Kind::Context, "")
                }
            };
            if in_hunk {
                let (old, new) = match kind {
                    Kind::Context => {
                        old_line += 1;
                        new_line += 1;
                        (Some(old_line - 1), Some(new_line - 1))
                    }
                    Kind::Added => {
                        file.added += 1;
                        new_line += 1;
                        (None, Some(new_line - 1))
                    }
                    Kind::Removed => {
                        file.removed += 1;
                        old_line += 1;
                        (Some(old_line - 1), None)
                    }
                    Kind::NoNewline => (None, None),
                };
                if let Some(hunk) = file.hunks.last_mut() {
                    hunk.lines.push(DiffLine {
                        kind,
                        old,
                        new,
                        text: text.to_string(),
                    });
                }
                continue;
            }
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            let (range, section) = rest.split_once(" @@").unwrap_or((rest, ""));
            let mut parts = range.split_whitespace();
            old_line = range_start(parts.next().unwrap_or(""), '-');
            new_line = range_start(parts.next().unwrap_or(""), '+');
            file.hunks.push(Hunk {
                header: format!("@@ {range} @@"),
                section: section.trim().to_string(),
                lines: Vec::new(),
            });
            in_hunk = true;
        } else if line.starts_with("new file mode") {
            file.status = Status::Added;
        } else if line.starts_with("deleted file mode") {
            file.status = Status::Deleted;
        } else if let Some(mode) = line.strip_prefix("old mode ") {
            file.mode_change = Some((mode.to_string(), String::new()));
        } else if let Some(mode) = line.strip_prefix("new mode ") {
            if let Some((_, new)) = &mut file.mode_change {
                *new = mode.to_string();
            }
        } else if let Some(from) = line.strip_prefix("rename from ") {
            file.status = Status::Renamed;
            file.old_path = Some(unquote(from));
        } else if let Some(to) = line.strip_prefix("rename to ") {
            file.path = unquote(to);
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            file.binary = true;
        } else if let Some(path) = line.strip_prefix("+++ ") {
            if let Some(path) = strip_side(path, "b/") {
                file.path = path;
            }
        } else if let Some(path) = line.strip_prefix("--- ")
            && file.status == Status::Deleted
            && let Some(path) = strip_side(path, "a/")
        {
            file.path = path;
        }
    }
    files
}

/// The start line of a `-a,b` or `+c,d` range.
fn range_start(range: &str, sign: char) -> u32 {
    range
        .trim_start_matches(sign)
        .split(',')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// A `--- a/path` or `+++ b/path` path, or `None` for `/dev/null`.
fn strip_side(path: &str, prefix: &str) -> Option<String> {
    let path = unquote(path.trim_end_matches('\t'));
    path.strip_prefix(prefix).map(str::to_string)
}

/// The path from `a/<path> b/<path>`. Renamed files get their paths from later lines.
fn header_path(rest: &str) -> String {
    if let Some(quoted) = rest.strip_prefix('"') {
        // `"a/x y" "b/x y"`: take the second quoted path.
        if let Some(i) = quoted.find("\" ") {
            let second = unquote(&quoted[i + 2..]);
            return second.strip_prefix("b/").unwrap_or(&second).to_string();
        }
    }
    // Both halves are the same for non-renames, which makes paths with spaces unambiguous.
    if rest.len() >= 5 && (rest.len() - 5).is_multiple_of(2) {
        let n = (rest.len() - 5) / 2;
        if rest.is_char_boundary(2 + n) && rest.is_char_boundary(n + 5) {
            let (a, b) = (&rest[2..2 + n], &rest[n + 5..]);
            if a == b {
                return a.to_string();
            }
        }
    }
    let b = rest.rsplit_once(" b/").map_or(rest, |(_, b)| b);
    b.to_string()
}

/// Undoes git's C-style quoting of unusual paths, e.g. `"a/tab\there"`.
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return s.to_string();
    };
    let mut bytes = Vec::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => bytes.push(b'\n'),
            Some('t') => bytes.push(b'\t'),
            Some('r') => bytes.push(b'\r'),
            Some('a') => bytes.push(7),
            Some('b') => bytes.push(8),
            Some('f') => bytes.push(12),
            Some('v') => bytes.push(11),
            Some(d @ '0'..='7') => {
                let mut value = d.to_digit(8).unwrap();
                for _ in 0..2 {
                    if let Some(d) = chars.clone().next().and_then(|c| c.to_digit(8)) {
                        value = value * 8 + d;
                        chars.next();
                    }
                }
                bytes.push(value as u8);
            }
            Some(other) => {
                let mut buf = [0; 4];
                bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Splits paired removed/added text into `(prefix, changed, suffix)` character ranges for
/// both sides, so the viewer can highlight just the part of a line that changed.
/// Returns `None` when the lines have too little in common for highlighting to help.
pub fn inline_change(old: &str, new: &str) -> Option<((usize, usize), (usize, usize))> {
    let old: Vec<char> = old.chars().collect();
    let new: Vec<char> = new.chars().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    let common = prefix + suffix;
    let longest = old.len().max(new.len());
    if common == 0 || common * 4 < longest {
        return None;
    }
    Some(((prefix, old.len() - suffix), (prefix, new.len() - suffix)))
}
