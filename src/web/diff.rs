//! The workspace's uncommitted changes, as the `diff` and `diff_stat` messages send them:
//! what the terminal's `/diff` shows, for the iPhone app's diff view and the count on the
//! page's code button.

use std::path::PathBuf;

use serde_json::{Value, json};

/// `git diff` here gives whole files, for expanding unchanged lines. Past this many lines a
/// file keeps only those near its changes.
const MAX_DIFF_FILE_LINES: usize = 4_000;
/// Unchanged lines kept around each change in such a file.
const DIFF_CONTEXT: usize = 3;
/// Files past this much diff are listed with their counts, without their lines.
const MAX_DIFF_BYTES: usize = 6 * 1024 * 1024;

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

/// The size of the first root's uncommitted changes, for the page's Changes button: the
/// counts alone, so keeping them current costs little on a slow connection.
pub fn diff_stat(roots: &[PathBuf]) -> Value {
    let files = roots
        .first()
        .and_then(|root| crate::diff::load(root, &crate::diff::Target::parse("")).ok())
        .unwrap_or_default();
    diff_stat_reply(&files)
}

fn diff_stat_reply(files: &[crate::diff::FileDiff]) -> Value {
    json!({
        "t": "diff_stat",
        "files": files.len(),
        "added": files.iter().map(|f| f.added).sum::<usize>(),
        "removed": files.iter().map(|f| f.removed).sum::<usize>(),
    })
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
    Value::Array(
        runs.into_iter()
            .map(|lines| json!({ "lines": lines }))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffLine, FileDiff, Hunk, Kind, Status};

    /// `count` unchanged lines, with line `changed` (from 0) an addition.
    fn file(count: usize, changed: usize) -> FileDiff {
        let lines = (0..count)
            .map(|i| DiffLine {
                kind: if i == changed {
                    Kind::Added
                } else {
                    Kind::Context
                },
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
            hunks: vec![Hunk {
                header: String::new(),
                section: String::new(),
                lines,
            }],
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
    fn the_stat_adds_up_every_file_without_sending_lines() {
        let mut a = file(10, 5);
        a.added = 3;
        a.removed = 1;
        let mut b = file(10, 5);
        b.added = 4;
        b.removed = 2;
        let reply = diff_stat_reply(&[a, b]);
        assert_eq!(
            reply,
            json!({ "t": "diff_stat", "files": 2, "added": 7, "removed": 3 })
        );
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
        assert_eq!(
            reply["files"][1]["omitted"], false,
            "the budget wasn't spent on what wasn't sent"
        );
    }
}
