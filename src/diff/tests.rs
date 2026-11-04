use super::*;

const SAMPLE: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,4 @@ fn main() {
 one
-two
+TWO
 three
@@ -10,2 +10,3 @@
 ten
+ten and a half
 eleven
\\ No newline at end of file
diff --git a/new file.txt b/new file.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new file.txt
@@ -0,0 +1 @@
+hello
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
--- not a header
diff --git a/old name.rs b/new name.rs
similarity index 90%
rename from old name.rs
rename to new name.rs
diff --git a/logo.png b/logo.png
index 5555555..6666666 100644
Binary files a/logo.png and b/logo.png differ
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
";

#[test]
fn parses_hunks_line_numbers_and_file_kinds() {
    let files = parse(SAMPLE);
    let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "src/lib.rs",
            "new file.txt",
            "gone.txt",
            "new name.rs",
            "logo.png",
            "run.sh"
        ]
    );

    let lib = &files[0];
    assert_eq!(lib.status, Status::Modified);
    assert_eq!((lib.added, lib.removed), (2, 1));
    assert_eq!(lib.hunks.len(), 2);
    assert_eq!(lib.hunks[0].header, "@@ -1,4 +1,4 @@");
    assert_eq!(lib.hunks[0].section, "fn main() {");
    let two = &lib.hunks[0].lines[1];
    assert_eq!(
        (two.kind, two.old, two.new, two.text.as_str()),
        (Kind::Removed, Some(2), None, "two")
    );
    let three = &lib.hunks[0].lines[3];
    assert_eq!((three.old, three.new), (Some(3), Some(3)));
    let half = &lib.hunks[1].lines[1];
    assert_eq!((half.kind, half.new), (Kind::Added, Some(11)));
    assert_eq!(lib.hunks[1].lines[3].kind, Kind::NoNewline);

    assert_eq!(files[1].status, Status::Added);
    assert_eq!(files[1].added, 1);
    assert_eq!(files[2].status, Status::Deleted);
    // A removed line that looks like a `---` header stays content.
    assert_eq!(files[2].hunks[0].lines[0].text, "-- not a header");
    assert_eq!(files[3].status, Status::Renamed);
    assert_eq!(files[3].old_path.as_deref(), Some("old name.rs"));
    assert!(files[4].binary);
    assert_eq!(
        files[5].mode_change,
        Some(("100644".into(), "100755".into()))
    );
}

#[test]
fn unquotes_unusual_paths() {
    assert_eq!(unquote("\"a/tab\\there\""), "a/tab\there");
    assert_eq!(unquote("\"caf\\303\\251\""), "café");
    assert_eq!(unquote("plain"), "plain");
    assert_eq!(header_path("\"a/x\\ty\" \"b/x\\ty\""), "x\ty");
}

#[test]
fn inline_change_finds_the_changed_middle() {
    assert_eq!(
        inline_change("let x = 1;", "let x = 42;"),
        Some(((8, 9), (8, 10)))
    );
    assert_eq!(inline_change("abc", "xyz"), None);
}

#[test]
fn loads_tracked_and_untracked_changes_from_a_repo() {
    let dir = std::env::temp_dir().join(format!("tamale-diff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let run = |args: &[&str]| {
        let out = git(&dir, args).unwrap();
        assert!(out.status.success(), "{:?}", out);
    };
    run(&["init", "-q"]);
    // Before the first commit, staged files diff against the empty tree.
    std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
    run(&["add", "a.txt"]);
    let files = load(&dir, &Target::parse("")).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].status, Status::Added);

    run(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "init",
    ]);
    std::fs::write(dir.join("a.txt"), "one\n2\n").unwrap();
    std::fs::write(dir.join("sub/new.txt"), "fresh\n").unwrap();
    // Paths are relative to the repository root even from a subdirectory.
    let files = load(&dir.join("sub"), &Target::parse("")).unwrap();
    let summary: Vec<_> = files
        .iter()
        .map(|f| (f.path.as_str(), f.status, f.added, f.removed))
        .collect();
    assert_eq!(
        summary,
        [
            ("a.txt", Status::Modified, 1, 1),
            ("sub/new.txt", Status::Untracked, 1, 0)
        ]
    );
    // Explicit arguments skip untracked files.
    let staged = load(&dir, &Target::parse("--staged")).unwrap();
    assert!(staged.is_empty());
    assert!(load(&dir, &Target::parse("no-such-ref")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A scratch repository with a committed `f.txt`, since changed, and a script that notes it ran.
struct Scratch {
    root: std::path::PathBuf,
    repo: std::path::PathBuf,
    ran: std::path::PathBuf,
    marker: std::path::PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("tamale-diff-{}", crate::session::new_id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let (ran, marker) = (root.join("ran"), root.join("marker.sh"));
        std::fs::write(
            &marker,
            format!("#!/bin/sh\necho \"$1\" >> {}\nexit 0\n", ran.display()),
        )
        .unwrap();
        run_git(&root, &["init", "-q", "repo"]);
        let scratch = Self {
            root,
            repo,
            ran,
            marker,
        };
        run_git(
            &scratch.root,
            &["-C", "repo", "config", "user.email", "a@b.c"],
        );
        run_git(&scratch.root, &["-C", "repo", "config", "user.name", "a"]);
        scratch.commit_a_change(&scratch.repo);
        scratch
    }

    fn commit_a_change(&self, repo: &Path) {
        std::fs::write(repo.join("f.txt"), "one\n").unwrap();
        run_git(repo, &["add", "-A"]);
        run_git(repo, &["commit", "-qm", "first"]);
        std::fs::write(repo.join("f.txt"), "two and more\n").unwrap();
    }

    /// `sh <marker> <what>`, as a repository's config would ask for it.
    fn command(&self, what: &str) -> String {
        format!("sh {} {what}", self.marker.display())
    }

    fn set(&self, repo: &Path, key: &str, value: &str) {
        run_git(repo, &["config", key, value]);
    }

    fn what_ran(&self) -> String {
        std::fs::read_to_string(&self.ran).unwrap_or_default()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `git clone` doesn't copy a `.git/config`, but an unpacked archive brings one. tamale loads
/// the diff on its own, at startup, so it mustn't run anything that config names.
#[test]
fn loading_a_diff_runs_nothing_a_repositorys_config_names() {
    let s = Scratch::new();
    std::fs::write(
        s.repo.join(".gitattributes"),
        "f.txt filter=plain\n*.txt diff=driver\n",
    )
    .unwrap();
    s.set(&s.repo, "core.fsmonitor", &s.command("fsmonitor"));
    s.set(&s.repo, "diff.external", &s.command("external"));
    s.set(&s.repo, "diff.driver.command", &s.command("driver"));
    s.set(&s.repo, "diff.driver.textconv", &s.command("textconv"));
    s.set(&s.repo, "filter.plain.clean", &s.command("clean"));
    s.set(&s.repo, "filter.plain.process", &s.command("process"));
    // Forced on, so a filter that's gone would fail the whole diff rather than just not run.
    s.set(&s.repo, "filter.plain.required", "true");
    let found = load(&s.repo, &Target::parse("")).unwrap();
    assert_eq!(s.what_ran(), "");
    assert!(
        found.iter().any(|f| f.path == "f.txt"),
        "{:?}",
        found.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    // The same for a diff someone asked for by name.
    load(&s.repo, &Target::parse("HEAD")).unwrap();
    assert_eq!(s.what_ran(), "");
}

#[test]
fn a_filter_from_an_included_config_doesnt_run_either() {
    let s = Scratch::new();
    std::fs::write(s.repo.join(".gitattributes"), "f.txt filter=included\n").unwrap();
    let extra = s.root.join("extra.cfg");
    std::fs::write(
        &extra,
        format!(
            "[filter \"included\"]\n\tclean = {}\n",
            s.command("included")
        ),
    )
    .unwrap();
    s.set(&s.repo, "include.path", &extra.display().to_string());
    load(&s.repo, &Target::parse("")).unwrap();
    assert_eq!(s.what_ran(), "");
}

#[test]
fn only_a_repositorys_own_filters_are_turned_off() {
    let listing = "global\0filter.lfs.clean\0global\0filter.lfs.process\0local\0filter.evil.clean\0\
                   local\0filter.evil.process\0worktree\0filter.a.b.smudge\0command\0filter.mine.clean\0\
                   system\0filter.sys.clean\0local\0filter.nosubsection\0";
    assert_eq!(repository_filters(listing), ["evil", "a.b"]);
    assert!(repository_filters("").is_empty());
}

/// `-c` splits at the first `=`, so this name couldn't be turned off; it isn't run instead.
#[test]
fn a_filter_whose_name_cant_be_turned_off_stops_the_diff() {
    let s = Scratch::new();
    std::fs::write(s.repo.join(".gitattributes"), "f.txt filter=with=equals\n").unwrap();
    s.set(&s.repo, "filter.with=equals.clean", &s.command("equals"));
    assert!(load(&s.repo, &Target::parse("")).is_err());
    assert_eq!(s.what_ran(), "");
}

/// A submodule's status is a `git status` of its own, under its own config.
#[test]
fn a_submodules_config_runs_nothing_either() {
    let s = Scratch::new();
    let sub = s.repo.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    run_git(&sub, &["init", "-q"]);
    s.set(&sub, "user.email", "a@b.c");
    s.set(&sub, "user.name", "a");
    s.commit_a_change(&sub);
    run_git(&s.repo, &["add", "sub"]);
    run_git(&s.repo, &["commit", "-qm", "with a submodule"]);
    std::fs::write(sub.join(".gitattributes"), "f.txt filter=inner\n").unwrap();
    s.set(&sub, "core.fsmonitor", &s.command("sub-fsmonitor"));
    s.set(&sub, "filter.inner.clean", &s.command("sub-clean"));
    std::fs::write(sub.join("f.txt"), "changed again, and longer\n").unwrap();
    load(&s.repo, &Target::parse("")).unwrap();
    assert_eq!(s.what_ran(), "");
}

/// A partial clone fetches blobs it's missing, which would run its config's `core.sshCommand`.
#[test]
fn the_working_tree_diff_never_fetches() {
    let s = Scratch::new();
    let blob = Command::new("git")
        .arg("-C")
        .arg(&s.repo)
        .args(["rev-parse", "HEAD:f.txt"])
        .output()
        .unwrap();
    let blob = String::from_utf8(blob.stdout).unwrap();
    let blob = blob.trim();
    let object = s
        .repo
        .join(".git/objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    std::fs::remove_file(object).unwrap();
    s.set(&s.repo, "core.repositoryformatversion", "1");
    s.set(&s.repo, "extensions.partialClone", "origin");
    s.set(&s.repo, "remote.origin.url", "ssh://example.invalid/x");
    s.set(&s.repo, "remote.origin.promisor", "true");
    s.set(&s.repo, "core.sshCommand", &s.command("ssh"));
    let _ = load(&s.repo, &Target::parse(""));
    assert_eq!(s.what_ran(), "");
}
