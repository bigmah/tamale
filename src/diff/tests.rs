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
