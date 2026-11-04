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

/// git in `dir`. A repository's own config can name programs for git to run, and an unpacked
/// archive brings its `.git/config` along. tamale loads the diff unasked, at startup and after
/// each turn, so here git runs none of them: see also [`filters_off`] and `--ignore-submodules`.
fn git_command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotePath=false", "-c", "core.fsmonitor=false"]);
    command
}

fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    git_command(dir)
        .args(args)
        .output()
        .context("couldn't run git")
}

/// The filters a repository's own config defines, from `git config --show-scope --name-only
/// -z`: scopes and keys, each ended by a NUL. Global and system ones (git-lfs's, usually) are
/// the user's own, and stay on.
fn repository_filters(listing: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut fields = listing.split('\0');
    while let (Some(scope), Some(key)) = (fields.next(), fields.next()) {
        if matches!(scope, "local" | "worktree")
            && let Some((name, _)) = key
                .strip_prefix("filter.")
                .and_then(|key| key.rsplit_once('.'))
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names
}

/// Arguments for git that turn off the filters a repository's own config defines, which it
/// would otherwise run on changed files. Files they'd clean show as they are instead.
fn filters_off(dir: &Path) -> Result<Vec<String>> {
    let listed = git(
        dir,
        &[
            "config",
            "--show-scope",
            "--includes",
            "--name-only",
            "-z",
            "--get-regexp",
            r"^filter\.",
        ],
    )?;
    // 1 means there are none. Anything else (a git too old for `--show-scope`) can't be trusted
    // to have listed them.
    if !matches!(listed.status.code(), Some(0 | 1)) {
        bail!(
            "couldn't list this repository's git filters: {}",
            String::from_utf8_lossy(&listed.stderr).trim()
        );
    }
    let listing = String::from_utf8(listed.stdout).context("a git filter's name isn't UTF-8")?;
    let mut args = Vec::new();
    for name in repository_filters(&listing) {
        // `-c` splits at the first `=`, so this one couldn't be turned off.
        if name.contains('=') {
            bail!("this repository's git config defines a filter tamale can't turn off: {name:?}");
        }
        for (variable, value) in [
            ("clean", ""),
            ("smudge", ""),
            ("process", ""),
            ("required", "false"),
        ] {
            args.push("-c".to_string());
            args.push(format!("filter.{name}.{variable}={value}"));
        }
    }
    Ok(args)
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
        // A submodule's status is a `git status` of its own, under its own config.
        "--ignore-submodules=dirty",
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
    let mut diff = git_command(root);
    diff.args(filters_off(root)?).args(&args);
    if target.args.is_empty() {
        // A partial clone would fetch a blob it's missing, through whatever its config says.
        // That's fine when someone asks for a diff of history, which is why it's only here.
        diff.env("GIT_NO_LAZY_FETCH", "1");
    }
    let output = diff.output().context("couldn't run git")?;
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
