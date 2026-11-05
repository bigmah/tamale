//! The code browser's reads: the workspace's files, one file highlighted, and a text
//! search. Paths come from the page as a root index and a path relative to that root,
//! and are only read if they resolve inside it.

use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;
use serde_json::{Value, json};

use super::render;

/// Files listed per root; more are counted but not sent.
const MAX_FILES: usize = 25_000;
/// Larger files aren't shown.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MATCHES: usize = 400;
const MAX_MATCHES_PER_FILE: usize = 30;
/// Characters of a matching line sent around the match.
const MATCH_CONTEXT: usize = 160;
/// `git diff` here gives whole files, for expanding unchanged lines. Past this many lines a
/// file keeps only those near its changes.
const MAX_DIFF_FILE_LINES: usize = 4_000;
/// Unchanged lines kept around each change in such a file.
const DIFF_CONTEXT: usize = 3;
/// Files past this much diff are listed with their counts, without their lines.
const MAX_DIFF_BYTES: usize = 6 * 1024 * 1024;

/// Walks a root the way the agent's tools do: ignore files apply and `.git` is skipped,
/// but other dotfiles (`.github`, `.gitignore`) are shown.
fn walk(root: &Path) -> impl Iterator<Item = PathBuf> {
    WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<_> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    Some(parts.join("/"))
}

fn root_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

/// Every root's files, sorted.
pub fn tree(roots: &[PathBuf]) -> Value {
    let roots: Vec<Value> = roots
        .iter()
        .map(|root| {
            let mut files: Vec<String> = Vec::new();
            let mut total = 0usize;
            for path in walk(root) {
                total += 1;
                if files.len() < MAX_FILES
                    && let Some(rel) = relative(root, &path)
                {
                    files.push(rel);
                }
            }
            files.sort_unstable();
            json!({
                "name": root_name(root),
                "path": root.display().to_string(),
                "files": files,
                "total": total,
            })
        })
        .collect();
    json!({ "t": "tree", "roots": roots })
}

/// The file at `rel` in root `root`, if it is inside that root.
pub fn resolve(roots: &[PathBuf], root: usize, rel: &str) -> Result<PathBuf, String> {
    let base = roots
        .get(root)
        .ok_or("That folder is no longer in the workspace.")?;
    let rel = Path::new(rel);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("Only files inside the workspace can be opened.".into());
    }
    let path = base.join(rel);
    // Symlinks may still point elsewhere.
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("Couldn't open it: {e}"))?;
    let base = base.canonicalize().unwrap_or_else(|_| base.clone());
    if !canonical.starts_with(&base) {
        return Err("Only files inside the workspace can be opened.".into());
    }
    Ok(canonical)
}

/// One file, highlighted line by line.
pub fn open(roots: &[PathBuf], root: usize, rel: &str) -> Value {
    let reply = |body: Value| {
        let mut reply = json!({ "t": "file", "root": root, "path": rel });
        if let (Some(reply), Value::Object(body)) = (reply.as_object_mut(), body) {
            reply.extend(body);
        }
        reply
    };
    let read = resolve(roots, root, rel).and_then(|path| {
        let metadata = std::fs::metadata(&path).map_err(|e| format!("Couldn't open it: {e}"))?;
        // Reading a pipe that made it into a repository would never finish.
        if !metadata.is_file() {
            return Err("Not a regular file.".into());
        }
        let size = metadata.len();
        if size > MAX_FILE_BYTES {
            return Err(format!(
                "Too large to show here ({:.1} MB).",
                size as f64 / 1_048_576.0
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("Couldn't read it: {e}"))?;
        if bytes[..bytes.len().min(8192)].contains(&0) {
            return Err("Binary file.".into());
        }
        Ok((path, String::from_utf8_lossy(&bytes).into_owned()))
    });
    match read {
        Ok((path, text)) => {
            let (lines, syntax) = render::highlight_file(&path, &text);
            reply(json!({ "lines": lines, "syntax": syntax, "bytes": text.len() }))
        }
        Err(error) => reply(json!({ "error": error })),
    }
}

/// Lines containing `query` across every root: case-insensitive unless the query has a
/// capital letter.
pub fn grep(roots: &[PathBuf], query: &str) -> Value {
    let case_sensitive = query.chars().any(char::is_uppercase);
    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    let mut matches = Vec::new();
    let mut truncated = false;
    if !needle.is_empty() {
        'roots: for (index, root) in roots.iter().enumerate() {
            for path in walk(root) {
                if matches.len() >= MAX_MATCHES {
                    truncated = true;
                    break 'roots;
                }
                if std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_FILE_BYTES) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Some(rel) = relative(root, &path) else {
                    continue;
                };
                let mut in_file = 0;
                for (n, line) in text.lines().enumerate() {
                    let haystack = if case_sensitive {
                        std::borrow::Cow::Borrowed(line)
                    } else {
                        std::borrow::Cow::Owned(line.to_lowercase())
                    };
                    // Lowercasing can change byte lengths; fall back to the line start.
                    let Some(at) = haystack.find(&needle) else {
                        continue;
                    };
                    let at = if haystack.len() == line.len() { at } else { 0 };
                    let (text, col) = excerpt(line, at);
                    matches.push(json!({
                        "root": index, "path": rel, "line": n + 1, "text": text, "col": col,
                    }));
                    in_file += 1;
                    if in_file >= MAX_MATCHES_PER_FILE || matches.len() >= MAX_MATCHES {
                        break;
                    }
                }
            }
        }
    }
    json!({ "t": "grep", "query": query, "matches": matches, "truncated": truncated })
}

/// Up to `MATCH_CONTEXT` characters of `line` around byte `at`, trimmed of indentation,
/// and where the match starts in it (in characters).
fn excerpt(line: &str, at: usize) -> (String, usize) {
    let start = line.len() - line.trim_start().len();
    let mut from = start.min(at);
    // Keep the match visible on long lines.
    let before = line[from..at].chars().count();
    if before > MATCH_CONTEXT / 2 {
        let skip = before - MATCH_CONTEXT / 3;
        from += line[from..]
            .chars()
            .take(skip)
            .map(char::len_utf8)
            .sum::<usize>();
    }
    let text: String = line[from..].chars().take(MATCH_CONTEXT).collect();
    let col = line[from..at].chars().count();
    (text.trim_end().to_string(), col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!("tamale-web-{}", crate::session::new_id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"Hello\");\n}\n",
        )
        .unwrap();
        std::fs::write(root.join(".gitignore"), "target\n").unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref").unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn tree_lists_files_but_not_git_internals() {
        let root = scratch();
        let tree = tree(std::slice::from_ref(&root));
        let files = &tree["roots"][0]["files"];
        assert_eq!(files, &json!([".gitignore", "src/main.rs"]));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolve_rejects_paths_outside_the_root() {
        let root = scratch();
        let roots = [root.clone()];
        assert!(resolve(&roots, 0, "src/main.rs").is_ok());
        assert!(resolve(&roots, 0, "../etc/passwd").is_err());
        assert!(resolve(&roots, 0, "/etc/passwd").is_err());
        assert!(resolve(&roots, 1, "src/main.rs").is_err());
        let opened = open(&roots, 0, "src/main.rs");
        assert_eq!(opened["lines"].as_array().unwrap().len(), 3);
        assert_eq!(opened["syntax"], "Rust");
        assert!(open(&roots, 0, "nope.rs")["error"].is_string());
        assert_eq!(open(&roots, 0, "src")["error"], "Not a regular file.");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grep_is_smart_case() {
        let root = scratch();
        let roots = [root.clone()];
        let found = grep(&roots, "hello");
        assert_eq!(found["matches"][0]["line"], 2);
        assert_eq!(found["matches"][0]["text"], "println!(\"Hello\");");
        assert_eq!(found["matches"][0]["col"], 10);
        assert_eq!(grep(&roots, "HELLO")["matches"], json!([]));
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// The first root's uncommitted changes, as the terminal's `/diff` shows them.
pub fn diff(roots: &[PathBuf]) -> Value {
    let Some(root) = roots.first() else {
        return json!({ "t": "diff", "error": "This session has no working directory." });
    };
    let target = crate::diff::Target::parse("");
    match crate::diff::load(root, &target) {
        Ok(files) => diff_reply(&target.label(), &files),
        Err(e) => json!({ "t": "diff", "error": format!("{e:#}") }),
    }
}

/// `files` as the `diff` message sends them.
pub(super) fn diff_reply(label: &str, files: &[crate::diff::FileDiff]) -> Value {
    let mut budget = MAX_DIFF_BYTES;
    let files: Vec<Value> = files
        .iter()
        .map(|file| {
            let hunks = diff_hunks(file);
            let size = hunks.to_string().len();
            let omitted = size > budget;
            if !omitted {
                budget -= size;
            }
            json!({
                "path": file.path,
                "old_path": file.old_path,
                "status": file.status.letter().to_string(),
                "binary": file.binary,
                "mode_change": file.mode_change,
                "added": file.added,
                "removed": file.removed,
                "omitted": omitted,
                "hunks": if omitted { json!([]) } else { hunks },
            })
        })
        .collect();
    json!({ "t": "diff", "label": label, "files": files })
}

/// A file's hunks, its long unchanged stretches dropped if it's a big one. Each line is
/// `{k, o, n, t, e}`: its kind, old and new line numbers, text, and the characters to
/// emphasize, as the terminal's viewer does.
fn diff_hunks(file: &crate::diff::FileDiff) -> Value {
    use crate::diff::{DiffLine, Kind, pair_changes};

    let lines: Vec<&DiffLine> = file.hunks.iter().flat_map(|h| &h.lines).collect();
    let texts: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
    let emphasis = pair_changes(&lines, &texts);
    let line_json = |i: usize| {
        let line = lines[i];
        let kind = match line.kind {
            Kind::Context => " ",
            Kind::Added => "+",
            Kind::Removed => "-",
            Kind::NoNewline => "\\",
        };
        let mut value = json!({ "k": kind, "t": line.text });
        if let Some(old) = line.old {
            value["o"] = old.into();
        }
        if let Some(new) = line.new {
            value["n"] = new.into();
        }
        if let Some((start, end)) = emphasis[i] {
            value["e"] = json!([start, end]);
        }
        value
    };
    let mut runs: Vec<Vec<Value>> = Vec::new();
    if lines.len() <= MAX_DIFF_FILE_LINES {
        runs.push((0..lines.len()).map(line_json).collect());
    } else {
        let mut keep = vec![false; lines.len()];
        for (i, line) in lines.iter().enumerate() {
            if matches!(line.kind, Kind::Added | Kind::Removed) {
                let end = (i + DIFF_CONTEXT + 1).min(lines.len());
                keep[i.saturating_sub(DIFF_CONTEXT)..end].fill(true);
            }
        }
        for i in 1..lines.len() {
            // `\\ No newline at end of file` belongs to the line before it.
            if lines[i].kind == Kind::NoNewline {
                keep[i] = keep[i - 1];
            }
        }
        let mut open = false;
        for i in 0..lines.len() {
            if keep[i] {
                if !open {
                    runs.push(Vec::new());
                }
                runs.last_mut().unwrap().push(line_json(i));
            }
            open = keep[i];
        }
    }
    runs.retain(|run| !run.is_empty());
    Value::Array(runs.into_iter().map(|lines| json!({ "lines": lines })).collect())
}

#[cfg(test)]
mod diff_tests {
    use super::*;
    use crate::diff::{DiffLine, FileDiff, Hunk, Kind, Status};

    /// `count` unchanged lines, with line `changed` (from 0) an addition.
    fn file(count: usize, changed: usize) -> FileDiff {
        let lines = (0..count)
            .map(|i| DiffLine {
                kind: if i == changed { Kind::Added } else { Kind::Context },
                old: (i != changed).then_some(i as u32 + 1),
                new: Some(i as u32 + 1),
                text: format!("line {i}"),
            })
            .collect();
        FileDiff {
            path: "a.rs".into(),
            old_path: None,
            status: Status::Modified,
            binary: false,
            mode_change: None,
            hunks: vec![Hunk { header: String::new(), section: String::new(), lines }],
            added: 1,
            removed: 0,
        }
    }

    fn runs(reply: &Value) -> Vec<usize> {
        reply["files"][0]["hunks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["lines"].as_array().unwrap().len())
            .collect()
    }

    #[test]
    fn an_ordinary_file_comes_whole_so_the_app_can_unfold_it() {
        assert_eq!(runs(&diff_reply("", &[file(100, 50)])), [100]);
    }

    #[test]
    fn a_big_file_keeps_only_whats_near_its_changes() {
        let mut big = file(MAX_DIFF_FILE_LINES + 1, 50);
        big.hunks[0].lines[3000].kind = Kind::Removed;
        big.hunks[0].lines[3000].new = None;
        assert_eq!(runs(&diff_reply("", &[big])), [7, 7]);
    }

    #[test]
    fn past_the_budget_a_file_keeps_its_counts_but_not_its_lines() {
        let mut huge = file(10, 5);
        huge.hunks[0].lines[0].text = "x".repeat(MAX_DIFF_BYTES);
        let reply = diff_reply("", &[huge, file(10, 5)]);
        assert_eq!(reply["files"][0]["omitted"], true);
        assert_eq!(reply["files"][0]["added"], 1);
        assert!(reply["files"][0]["hunks"].as_array().unwrap().is_empty());
        assert_eq!(reply["files"][1]["omitted"], false, "the budget wasn't spent on what wasn't sent");
    }
}
