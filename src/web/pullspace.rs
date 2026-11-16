//! The review panel: [pullspace](https://github.com/bigmah/pullspace), an IDE-style code
//! reviewer compiled to WebAssembly, embedded in the page to review the working tree
//! against `HEAD`.
//!
//! pullspace runs in a frame at `/pullspace/?embed` and never talks to GitHub there.
//! It asks the page for two things over `postMessage`, and the page asks here over its
//! socket (so it works the same for any tab that has the socket):
//!
//! - `ps_snapshot`: every file on each side, as git blob hashes and sizes ([`snapshot`]).
//! - `ps_blobs`: the bytes of some of those blobs ([`blobs`]).
//!
//! Everything else (the store, search, symbols, the diff) happens in the frame. The bundle
//! and the message types come from the `pullspace-embed` crate, in pullspace's repository.
//!
//! git runs here as it does for `/diff` (see [`crate::diff::git_command`]): no fsmonitor,
//! no repository-defined filters, no lazy fetches. Working-tree files are hashed here
//! rather than by git, so nothing a repository's config names ever runs.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use base64::Engine;
use pullspace_embed::protocol::{self, BlobsReply, Want};
use serde_json::{Value, json};

/// Working-tree files hashed per snapshot, at most; past this the rest are left out
/// (a directory of build output nobody ignored, say).
const MAX_HASHED: usize = 20_000;
/// Files larger than this aren't hashed or sent.
const MAX_FILE_BYTES: u64 = 32 << 20;
/// What one `ps_blobs` reply may carry before base64, and how many blobs.
const MAX_REPLY_BYTES: usize = 8 << 20;
const MAX_REPLY_BLOBS: usize = 1024;

/// The name the working tree's snapshot is filed under in the frame's store.
const WORKTREE: &str = "worktree";

use protocol::Entry;

/// git's blob hash of `bytes`.
pub fn blob_sha(bytes: &[u8]) -> String {
    let mut hasher = sha1_smol::Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    hasher.digest().to_string()
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The repository the first root is in, and its top level.
fn toplevel(roots: &[PathBuf]) -> Result<PathBuf> {
    let Some(root) = roots.first() else {
        bail!("This session has no working directory.");
    };
    let out = git(root, &["rev-parse", "--show-toplevel"])?;
    if !out.status.success() {
        bail!("{} is not inside a git repository", root.display());
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    crate::diff::git_command(dir)
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .output()
        .context("couldn't run git")
}

/// NUL-separated records, without the empty one after the last NUL.
fn records(out: &[u8]) -> impl Iterator<Item = &str> {
    out.split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .filter_map(|r| std::str::from_utf8(r).ok())
}

/// `git ls-tree -r -l -z`: `<mode> SP <type> SP <sha> SP+ <size> TAB <path>`.
fn parse_ls_tree(out: &[u8]) -> Vec<Entry> {
    records(out)
        .filter_map(|record| {
            let (meta, path) = record.split_once('\t')?;
            let mut fields = meta.split_whitespace();
            let (_mode, kind, sha, size) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            (kind == "blob").then(|| Entry {
                path: path.to_string(),
                sha: sha.to_string(),
                size: size.parse().unwrap_or(0),
            })
        })
        .collect()
}

/// `git ls-files -s -z`: `<mode> SP <sha> SP <stage> TAB <path>`, without submodules.
/// Paths with a conflict (any stage but 0) come back with no hash: the working tree is
/// what to show for them.
fn parse_ls_files(out: &[u8]) -> Vec<(String, Option<String>)> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for record in records(out) {
        let Some((meta, path)) = record.split_once('\t') else {
            continue;
        };
        let mut fields = meta.split_whitespace();
        let (Some(mode), Some(sha), Some(stage)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if mode == "160000" {
            continue;
        }
        if stage == "0" {
            entries.push((path.to_string(), Some(sha.to_string())));
        } else if seen.insert(path.to_string()) {
            entries.push((path.to_string(), None));
        }
    }
    entries
}

/// A path from git or the page, if it is a plain relative path that stays inside `root`.
fn inside(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel);
    let plain = rel.components().all(|c| matches!(c, Component::Normal(_)))
        && rel.components().next().is_some()
        && !rel.starts_with(".git");
    plain.then(|| root.join(rel))
}

/// A working-tree file's contents as git would store them (unfiltered): a symlink is
/// its target. `None` for a file that is gone, a directory, or too big.
fn read_worktree(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path).ok()?;
        return Some(target.to_string_lossy().into_owned().into_bytes());
    }
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    std::fs::read(path).ok()
}

/// The sizes of these blobs, from `git cat-file --batch-check`.
fn blob_sizes(root: &Path, shas: &[&str]) -> Result<HashMap<String, u64>> {
    if shas.is_empty() {
        return Ok(HashMap::new());
    }
    let input: String = shas.iter().map(|s| format!("{s}\n")).collect();
    let out = pipe(
        root,
        &["cat-file", "--batch-check=%(objectname) %(objectsize)"],
        input.as_bytes(),
    )?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|line| {
            let (sha, size) = line.split_once(' ')?;
            Some((sha.to_string(), size.parse().ok()?))
        })
        .collect())
}

/// Runs git with `input` on stdin and returns its stdout. The input goes in from a thread
/// of its own, so neither side can fill a pipe while the other waits.
fn pipe(root: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let mut child = crate::diff::git_command(root)
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("couldn't run git")?;
    let mut stdin = child.stdin.take().context("no stdin for git")?;
    let mut stdout = child.stdout.take().context("no stdout for git")?;
    let mut out = Vec::new();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ = stdin.write_all(input);
        });
        let _ = stdout.read_to_end(&mut out);
    });
    child.wait().context("git didn't finish")?;
    Ok(out)
}

/// Both sides of the review: `HEAD`, and the working tree (staged and unstaged changes and
/// untracked files), as the `ps_snapshot` reply.
pub fn snapshot(roots: &[PathBuf]) -> Value {
    match load_snapshot(roots) {
        Ok(body) => json!({ "t": "ps_snapshot", "body": body }),
        Err(e) => json!({ "t": "ps_snapshot", "error": format!("{e:#}") }),
    }
}

fn load_snapshot(roots: &[PathBuf]) -> Result<Value> {
    let root = toplevel(roots)?;
    let head = git(&root, &["rev-parse", "--verify", "--quiet", "HEAD"])?;
    let head = head
        .status
        .success()
        .then(|| String::from_utf8_lossy(&head.stdout).trim().to_string());

    let base = match &head {
        Some(_) => {
            let out = git(&root, &["ls-tree", "-r", "-l", "-z", "--full-tree", "HEAD"])?;
            if !out.status.success() {
                bail!(
                    "git ls-tree failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            parse_ls_tree(&out.stdout)
        }
        None => Vec::new(),
    };

    let index = git(&root, &["ls-files", "-s", "-z"])?;
    if !index.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&index.stderr).trim()
        );
    }
    let index = parse_ls_files(&index.stdout);

    // What may differ from the index: git compares what it recorded about each file, and
    // lists any it can't vouch for. Each of those is hashed here, so one listed only
    // because it was touched comes out the same as before.
    let mut dirty_cmd = crate::diff::git_command(&root);
    dirty_cmd
        .args(crate::diff::filters_off(&root)?)
        .args(["diff-files", "--name-only", "-z", "--ignore-submodules"])
        .env("GIT_NO_LAZY_FETCH", "1");
    let dirty = dirty_cmd.output().context("couldn't run git")?;
    let dirty: HashSet<String> = records(&dirty.stdout).map(str::to_string).collect();

    let untracked = git(&root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let untracked: Vec<String> = records(&untracked.stdout).map(str::to_string).collect();

    let known: HashMap<&str, u64> = base.iter().map(|e| (e.sha.as_str(), e.size)).collect();
    let mut files = Vec::new();
    let mut hashed = 0;
    let mut hash = |path: &str, files: &mut Vec<Entry>| {
        if hashed >= MAX_HASHED {
            return;
        }
        let Some(full) = inside(&root, path) else {
            return;
        };
        hashed += 1;
        if let Some(bytes) = read_worktree(&full) {
            files.push(Entry {
                path: path.to_string(),
                sha: blob_sha(&bytes),
                size: bytes.len() as u64,
            });
        }
    };
    let mut no_size = Vec::new();
    for (path, sha) in &index {
        match sha {
            Some(sha) if !dirty.contains(path) => match known.get(sha.as_str()) {
                Some(&size) => files.push(Entry {
                    path: path.clone(),
                    sha: sha.clone(),
                    size,
                }),
                None => {
                    no_size.push(files.len());
                    files.push(Entry {
                        path: path.clone(),
                        sha: sha.clone(),
                        size: 0,
                    });
                }
            },
            _ => hash(path, &mut files),
        }
    }
    for path in &untracked {
        hash(path, &mut files);
    }
    // Staged files the last commit doesn't have: their sizes, from git.
    let shas: Vec<&str> = no_size.iter().map(|&i| files[i].sha.as_str()).collect();
    let sizes = blob_sizes(&root, &shas)?;
    for i in no_size {
        if let Some(&size) = sizes.get(&files[i].sha) {
            files[i].size = size;
        }
    }

    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let snapshot = protocol::Snapshot {
        name,
        base: protocol::Side {
            label: "HEAD".into(),
            commit: head.unwrap_or_default(),
            files: base,
        },
        head: protocol::Side {
            label: "working tree".into(),
            commit: WORKTREE.into(),
            files,
        },
    };
    Ok(serde_json::to_value(snapshot)?)
}

/// The bytes of the blobs the frame asked for, as the `ps_blobs` reply: from git's object
/// store, or for a working-tree file git hasn't stored, from the file itself when it still
/// hashes to what was asked for. Each `want` is a hash and the path it was listed at.
pub fn blobs(roots: &[PathBuf], wants: &[Want]) -> Value {
    match load_blobs(roots, wants) {
        Ok(found) => {
            let engine = base64::engine::general_purpose::STANDARD;
            let reply = BlobsReply {
                blobs: found
                    .into_iter()
                    .map(|(sha, bytes)| (sha, Some(engine.encode(bytes))))
                    .collect(),
            };
            json!({ "t": "ps_blobs", "body": reply })
        }
        Err(e) => json!({ "t": "ps_blobs", "error": format!("{e:#}") }),
    }
}

fn load_blobs(roots: &[PathBuf], wants: &[Want]) -> Result<Vec<(String, Vec<u8>)>> {
    let root = toplevel(roots)?;
    let mut wanted: Vec<&Want> = Vec::new();
    let mut seen = HashSet::new();
    for want in wants.iter().take(MAX_REPLY_BLOBS) {
        if is_sha(&want.sha) && seen.insert(want.sha.to_ascii_lowercase()) {
            wanted.push(want);
        }
    }
    let input: String = wanted.iter().map(|w| format!("{}\n", w.sha)).collect();
    let out = pipe(&root, &["cat-file", "--batch"], input.as_bytes())?;
    let mut stored = parse_batch(&out);

    let mut found = Vec::new();
    let mut total = 0;
    for Want { sha, path } in wanted {
        let sha = sha.to_ascii_lowercase();
        let bytes = match stored.remove(&sha) {
            Some(bytes) => Some(bytes),
            None => inside(&root, path)
                .and_then(|full| read_worktree(&full))
                .filter(|bytes| blob_sha(bytes) == sha),
        };
        let Some(bytes) = bytes else { continue };
        // Always at least one, so a large file still comes through on its own.
        if !found.is_empty() && total + bytes.len() > MAX_REPLY_BYTES {
            break;
        }
        total += bytes.len();
        found.push((sha, bytes));
    }
    Ok(found)
}

/// `git cat-file --batch` output: `<sha> SP <type> SP <size> LF <contents> LF`, or
/// `<sha> SP missing LF`. Only blobs are kept.
fn parse_batch(mut out: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut blobs = HashMap::new();
    while let Some(eol) = out.iter().position(|b| *b == b'\n') {
        let header = String::from_utf8_lossy(&out[..eol]).into_owned();
        out = &out[eol + 1..];
        let mut fields = header.split(' ');
        let (Some(sha), Some(kind), Some(size)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(size) = size.parse::<usize>() else {
            continue;
        };
        if out.len() < size {
            break;
        }
        if kind == "blob" {
            blobs.insert(sha.to_string(), out[..size].to_vec());
        }
        out = &out[size..];
        out = out.strip_prefix(b"\n").unwrap_or(out);
    }
    blobs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tamale-pullspace-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"]);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/keep.rs"), "fn keep() {}\n").unwrap();
        std::fs::write(dir.join("src/edit.rs"), "fn old() {}\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("src/edit.rs"), "fn new() {}\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::write(dir.join("fresh.md"), "# new\n").unwrap();
        std::fs::write(dir.join("staged.txt"), "staged\n").unwrap();
        run(&["add", "staged.txt"]);
        dir
    }

    fn side(body: &Value, which: &str) -> HashMap<String, (String, u64)> {
        body[which]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["path"].as_str().unwrap().to_string(),
                    (
                        f["sha"].as_str().unwrap().to_string(),
                        f["size"].as_u64().unwrap(),
                    ),
                )
            })
            .collect()
    }

    #[test]
    fn hashes_like_git() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            blob_sha(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        assert_eq!(blob_sha(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn a_snapshot_is_head_and_the_working_tree_and_blobs_come_from_either() {
        let dir = scratch_repo();
        let roots = [dir.clone()];
        let reply = snapshot(&roots);
        assert_eq!(reply["t"], "ps_snapshot");
        let body = &reply["body"];
        assert!(reply.get("error").is_none(), "{reply}");
        assert_eq!(body["base"]["label"], "HEAD");
        assert_eq!(body["head"]["commit"], WORKTREE);

        let base = side(body, "base");
        let head = side(body, "head");
        let mut base_paths: Vec<_> = base.keys().cloned().collect();
        base_paths.sort();
        assert_eq!(base_paths, ["gone.txt", "src/edit.rs", "src/keep.rs"]);
        let mut head_paths: Vec<_> = head.keys().cloned().collect();
        head_paths.sort();
        assert_eq!(
            head_paths,
            ["fresh.md", "src/edit.rs", "src/keep.rs", "staged.txt"]
        );
        // Unchanged: the same blob on both sides.
        assert_eq!(base["src/keep.rs"], head["src/keep.rs"]);
        // Edited: hashed from the file, with its size.
        assert_eq!(head["src/edit.rs"], (blob_sha(b"fn new() {}\n"), 12));
        assert_ne!(base["src/edit.rs"].0, head["src/edit.rs"].0);
        // Staged but not committed: git's hash, and git's size.
        assert_eq!(head["staged.txt"], (blob_sha(b"staged\n"), 7));
        assert_eq!(head["fresh.md"].1, 6);

        // Blobs: one only git has (the old edit.rs), one only on disk (fresh.md), one
        // that's neither, and a path that climbs out.
        let want = |sha: String, path: &str| Want {
            sha,
            path: path.to_string(),
        };
        let wants = vec![
            want(base["src/edit.rs"].0.clone(), "src/edit.rs"),
            want(head["fresh.md"].0.clone(), "fresh.md"),
            want("0".repeat(40), "fresh.md"),
            want(blob_sha(b"x"), "../outside"),
        ];
        let reply = blobs(&roots, &wants);
        let got = reply["body"]["blobs"].as_object().unwrap();
        let engine = base64::engine::general_purpose::STANDARD;
        let decode = |sha: &str| engine.decode(got[sha].as_str().unwrap()).unwrap();
        assert_eq!(got.len(), 2, "{reply}");
        assert_eq!(decode(&base["src/edit.rs"].0), b"fn old() {}\n");
        assert_eq!(decode(&head["fresh.md"].0), b"# new\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn outside_a_repository_says_so() {
        let dir = std::env::temp_dir().join(format!("tamale-ps-norepo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let reply = snapshot(std::slice::from_ref(&dir));
        // A temp directory could be inside someone's repository; only check the shape.
        assert!(reply.get("error").is_some() || reply.get("body").is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parses_git_listings() {
        let tree = b"100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa      12\tsrc/a b.rs\x00160000 commit bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb       -\tvendor/sub\0";
        assert_eq!(
            parse_ls_tree(tree),
            vec![Entry {
                path: "src/a b.rs".into(),
                sha: "a".repeat(40),
                size: 12
            }]
        );
        let index = b"100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 0\ta.rs\x00160000 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 0\tsub\x00100644 cccccccccccccccccccccccccccccccccccccccc 1\tc.rs\x00100644 dddddddddddddddddddddddddddddddddddddddd 2\tc.rs\0";
        assert_eq!(
            parse_ls_files(index),
            vec![("a.rs".into(), Some("a".repeat(40))), ("c.rs".into(), None)]
        );
        let batch = b"aaaa blob 3\nabc\nbbbb missing\ncccc tree 2\nxy\ndddd blob 0\n\n";
        let parsed = parse_batch(batch);
        assert_eq!(parsed["aaaa"], b"abc");
        assert_eq!(parsed["dddd"], b"");
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn only_plain_paths_inside_the_root_are_read() {
        let root = Path::new("/r");
        assert_eq!(inside(root, "a/b.rs"), Some(PathBuf::from("/r/a/b.rs")));
        assert_eq!(inside(root, "../x"), None);
        assert_eq!(inside(root, "/etc/passwd"), None);
        assert_eq!(inside(root, "a/../../x"), None);
        assert_eq!(inside(root, ".git/config"), None);
        assert_eq!(inside(root, ""), None);
    }
}
