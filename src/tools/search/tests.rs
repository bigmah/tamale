use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static ID: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "tamale-search-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        for (path, text) in [
            (".gitignore", "ignored.rs\n"),
            (".ignore", "other.rs\n"),
            ("src/main.rs", "before\nHello needle\nneedle again\nafter\n"),
            ("src/nested/lib.rs", "NEEDLE\na.b\naxb\n"),
            ("notes.txt", "needle\n"),
            ("ignored.rs", "needle\n"),
            ("other.rs", "needle\n"),
            (".hidden.rs", "needle\n"),
            (".git/config", "needle\n"),
        ] {
            std::fs::write(root.join(path), text).unwrap();
        }
        std::fs::write(root.join("binary.rs"), b"needle\0binary").unwrap();
        Self(root)
    }

    async fn run(&self, name: &str, mut args: Value) -> String {
        args["path"] = json!(self.0);
        // Exercise dispatch and deserialization, including the UI's extra description field.
        args["description"] = json!("Search fixture files");
        crate::tools::execute(name, args).await.unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn grep_respects_ignores_hidden_files_and_binary_detection() {
    let fixture = Fixture::new();
    let out = fixture.run("grep", json!({"pattern": "needle"})).await;
    assert!(out.contains("main.rs:2:Hello needle"), "{out}");
    assert!(out.contains("notes.txt:1:needle"));
    for excluded in [
        "ignored.rs",
        "other.rs",
        ".hidden.rs",
        ".git/config",
        "binary.rs",
        "lib.rs",
    ] {
        assert!(!out.contains(excluded), "{out}");
    }
    let out = fixture
        .run(
            "grep",
            json!({"pattern": "needle", "include_hidden": true, "respect_ignore": false}),
        )
        .await;
    assert!(
        out.contains("ignored.rs:1:")
            && out.contains("other.rs:1:")
            && out.contains(".hidden.rs:1:"),
        "{out}"
    );
    assert!(!out.contains(".git/config"));
}

#[tokio::test]
async fn grep_handles_filters_modes_literals_and_case() {
    let fixture = Fixture::new();
    let out = fixture.run("grep", json!({"pattern": "needle", "glob": "src/**/*.rs", "ignore_case": true, "output_mode": "count"})).await;
    assert!(
        out.contains("main.rs:2\n") && out.contains("lib.rs:1\n"),
        "{out}"
    );
    assert!(!out.contains("notes.txt"));
    let out = fixture
        .run(
            "grep",
            json!({"pattern": "a.b", "fixed_strings": true, "glob": "*.rs"}),
        )
        .await;
    assert!(out.contains("lib.rs:2:a.b"), "{out}");
    assert!(!out.contains("axb"));
    let out = fixture
        .run(
            "grep",
            json!({"pattern": "needle", "output_mode": "files_with_matches"}),
        )
        .await;
    assert!(out.lines().any(|l| l.ends_with("main.rs")));
    assert!(!out.contains("Hello needle"));
}

#[tokio::test]
async fn context_overlaps_are_merged_and_results_are_limited() {
    let fixture = Fixture::new();
    let out = fixture
        .run(
            "grep",
            json!({"pattern": "needle", "glob": "main.rs", "context": 1}),
        )
        .await;
    assert!(
        out.contains("main.rs-1-before") && out.contains("main.rs-4-after"),
        "{out}"
    );
    assert_eq!(out.matches("main.rs:2:").count(), 1);
    assert_eq!(out.matches("main.rs:3:").count(), 1);
    let out = fixture
        .run(
            "grep",
            json!({"pattern": "needle", "glob": "main.rs", "max_results": 1}),
        )
        .await;
    assert!(out.contains("Results limited"), "{out}");
    assert_eq!(out.matches("main.rs:").count(), 1);
}

#[tokio::test]
async fn glob_matches_relative_paths_and_nested_basenames() {
    let fixture = Fixture::new();
    let out = fixture.run("glob", json!({"pattern": "*.rs"})).await;
    assert!(
        out.contains("src/main.rs") && out.contains("src/nested/lib.rs"),
        "{out}"
    );
    assert!(!out.contains("ignored.rs") && !out.contains(".hidden.rs"));
    let out = fixture
        .run("glob", json!({"pattern": "src/*.{rs,toml}"}))
        .await;
    assert!(out.contains("main.rs"));
    assert!(!out.contains("lib.rs"));
    let out = fixture
        .run("glob", json!({"pattern": "**/*.rs", "max_results": 1}))
        .await;
    assert!(out.contains("Results limited"));
    let out = fixture.run("glob", json!({"pattern": "*.missing"})).await;
    assert_eq!(out, "No files found.");
}

#[tokio::test]
async fn invalid_patterns_and_missing_paths_are_errors() {
    let fixture = Fixture::new();
    assert!(
        grep(json!({"path": fixture.0, "pattern": "("}))
            .await
            .is_err()
    );
    assert!(
        glob(json!({"path": fixture.0, "pattern": "["}))
            .await
            .is_err()
    );
    assert!(
        grep(json!({"path": fixture.0.join("missing"), "pattern": "test"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn explicit_file_search_and_large_file_skip() {
    let fixture = Fixture::new();
    let out =
        grep(json!({"path": fixture.0.join("src/main.rs"), "pattern": "needle", "glob": "*.rs"}))
            .await
            .unwrap();
    assert!(out.contains("main.rs:2:"));
    let large = fixture.0.join("large.rs");
    File::create(&large)
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    let out = grep(json!({"path": large, "pattern": "needle"}))
        .await
        .unwrap();
    assert!(out.contains("Skipped 1 files over 2 MiB"), "{out}");
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_directories_are_not_followed() {
    let fixture = Fixture::new();
    std::os::unix::fs::symlink(&fixture.0, fixture.0.join("src/loop")).unwrap();
    let out = fixture.run("glob", json!({"pattern": "**/*.rs"})).await;
    assert!(!out.contains("loop") && !out.contains("limited"), "{out}");
}
