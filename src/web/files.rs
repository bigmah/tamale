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
