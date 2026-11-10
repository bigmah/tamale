//! Writes what the iOS app (`apple/`, see `apple/ios.md`) is tested against: end-to-end
//! encryption vectors and one sample of each `/web` message it reads, into
//! `apple/Fixtures`. The files are checked in; this fails when they're stale, so a change
//! here that would break the app's decoding shows up in the same PR.
//!
//! `UPDATE_FIXTURES=1 cargo test apple_fixtures` rewrites them.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::e2e::{self, testing::Initiator};
use super::*;

const SID: &str = "0123456789abcdef0123456789abcdef";
const NONCE: &[u8; 16] = b"channel-nonce-16";
/// Bigger than one Noise message's piece (65535 - 17 bytes), so it goes in two.
const LONG_COUNT: usize = 32_800;
const ARTIFACT_ID: &str = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
const SAMPLE_DIFF: &str = "\
diff --git a/src/auth.rs b/src/auth.rs
index 1111111..2222222 100644
--- a/src/auth.rs
+++ b/src/auth.rs
@@ -1,4 +1,5 @@
 fn login() {
-    drop(cookie);
+    keep(cookie);
+    log(cookie);
 }
 // end
\\ No newline at end of file
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+hello
diff --git a/logo.png b/logo.png
index 4444444..5555555 100644
Binary files a/logo.png and b/logo.png differ
diff --git a/old/name.rs b/new/name.rs
similarity index 100%
rename from old/name.rs
rename to new/name.rs
old mode 100644
new mode 100755
";

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("apple/Fixtures")
}

/// Compares `value` with the checked-in file, or rewrites it with `UPDATE_FIXTURES` set.
fn check(name: &str, value: &Value) -> Option<String> {
    let path = root().join(name);
    let text = format!("{}\n", serde_json::to_string_pretty(value).unwrap());
    if std::env::var_os("UPDATE_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(old) if old == text => None,
        Ok(_) => Some(format!("{name} is stale")),
        Err(_) => Some(format!("{name} is missing")),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn vectors() -> Value {
    let key = [0x42u8; 32];
    let psk = tamale_hub_protocol::e2e_secret(&key);
    let keys = e2e::Keys::from_key(&key);
    let auth = tamale_hub_protocol::auth_value(&key);
    let initiator_e = [0x11u8; 32];
    let responder_e = [0x22u8; 32];
    let mut app = Initiator::new(&psk, SID, NONCE, Some(&initiator_e));
    let payload = r#"{"v":1}"#;
    let hs1 = app.hs1(payload.as_bytes());
    let (mut tamale, hs2) = keys
        .respond_fixed(SID, NONCE, &hs1, &responder_e)
        .unwrap();
    let mut app = app.finish(&hs2).unwrap();
    let long = "λ".repeat(LONG_COUNT);
    let mut down = Vec::new();
    for plain in [r#"{"t":"status"}"#, "", long.as_str()] {
        let sealed = tamale.seal(plain).unwrap();
        down.push(json!({ "plain": if plain == long { "<long>" } else { plain }, "sealed": sealed }));
    }
    let mut up = Vec::new();
    for plain in [r#"{"t":"send","text":"hi from the app"}"#, long.as_str()] {
        let sealed = app.seal(plain).unwrap();
        up.push(json!({ "plain": if plain == long { "<long>" } else { plain }, "sealed": sealed }));
    }
    let label = r#"{"host":"laptop","cwd":"~/dev/tamale","model":"opus"}"#;
    let status = r#"{"title":"Fix the login bug","model":"opus"}"#;
    json!({
        "key": tamale_hub_protocol::encode_key(&key),
        "auth": auth,
        "id": &content_hash(&auth)[..16],
        "psk": hex(&psk),
        "meta": hex(&keys.meta_key()),
        "mid": keys.mid,
        "sid": SID,
        "nonce": STANDARD.encode(NONCE),
        "initiator_ephemeral": hex(&initiator_e),
        "responder_ephemeral": hex(&responder_e),
        "hs1_payload": payload,
        "hs1": hs1,
        "hs2": hs2,
        "long": { "repeat": "λ", "count": LONG_COUNT },
        "down": down,
        "up": up,
        "sealed": [
            { "kind": "label", "plain": label, "sealed": keys.seal_fixed(SID, "label", label, [1; 12]).unwrap() },
            { "kind": "status", "plain": status, "sealed": keys.seal_fixed(SID, "status", status, [2; 12]).unwrap() },
        ],
        "not_keys": [
            "thk_", "THK_IJBEEQSCIJBEEQSCIJBEEQSCIJBEEQSCIJBEEQSCIJBEEQSCIJBA",
            format!("{}b", &tamale_hub_protocol::encode_key(&key)[..55]),
            "thk_ijbeeqscijbeeqscijbeeqscijbeeqscijbeeqscijbeeqscijb1",
        ],
    })
}

/// One sample of each message the app reads, with the random artifact id made fixed.
fn messages(dir: &std::path::Path) -> Vec<(&'static str, Value)> {
    let artifact = "<!doctype html><title>Login flow</title><p>Steps</p>\n";
    let reply = format!(
        "I traced the bug.\n\n- `auth.rs` drops the cookie\n- **fix**: keep it\n\n```html artifact\n{artifact}```\n\nWant me to apply it?"
    );
    let entries = vec![
        Entry::Info("Welcome! tamale 0.1.2".into()),
        Entry::User("Why does login fail?\n```\nerror: 401\n```".into()),
        Entry::Tool {
            name: "read_file".into(),
            summary: "src/auth.rs".into(),
            description: "Read src/auth.rs".into(),
            output: Some(("fn login() {}".into(), false)),
        },
        Entry::Tool {
            name: "bash".into(),
            summary: "cargo test".into(),
            description: "Run the tests".into(),
            output: Some(("1 failed".into(), true)),
        },
        Entry::Tool {
            name: "grep".into(),
            summary: "cookie".into(),
            description: "Find cookie handling".into(),
            output: None,
        },
        Entry::Assistant(reply),
        Entry::Note("Compacted the conversation: *3 turns* summarised.".into()),
        Entry::Error("The backend timed out.".into()),
        Entry::Assistant("Building:\n\n```html artifact\n<title>Draft</title>\n<p>half".into()),
    ];
    let mirror = Mirror::default();
    let mut v = view(&entries);
    v.status = Status {
        project: "tamale".into(),
        cwd: "~/dev/tamale".into(),
        dirs: vec!["~/dev/shared".into()],
        model: "claude-opus-4".into(),
        backend: "anthropic".into(),
        signed_in: true,
        mode: "default",
        effort: Some("high"),
        thinking: "auto",
        running: true,
        compacting: false,
        since: Some(1_700_000_000),
        step_since: Some(1_700_000_090),
        step: "Run the tests".into(),
        step_kind: "tool",
        steps: 3,
        sub_steps: 0,
        context: 12_345,
        window: 200_000,
        input_tokens: 50_000,
        output_tokens: 4_000,
        cost: 0.42,
        cost_partial: false,
        goal: Some("Fix login".into()),
        queued: vec!["then run the tests".into()],
        chef: false,
        asks: true,
    };
    v.approval = Some(Approval {
        name: "bash".into(),
        summary: "rm -rf target".into(),
        description: "Clean the build".into(),
        always: vec!["bash: rm *".into()],
    });
    v.question = Some(Questions {
        description: "Ask how to fix login".into(),
        questions: crate::tools::ask::parse(&json!({
            "questions": [
                {
                    "question": "How should the cookie be kept?",
                    "header": "Cookie",
                    "options": [
                        { "label": "Refresh it (Recommended)", "description": "Renew it on every request" },
                        { "label": "Longer expiry" }
                    ]
                },
                {
                    "question": "Which tests should I add?",
                    "header": "Tests",
                    "options": [{ "label": "Unit" }, { "label": "Integration" }],
                    "multi_select": true
                }
            ]
        }))
        .unwrap(),
    });
    let mut mirror = mirror;
    let sent = mirror.update(&v);
    let id = mirror.artifacts.lock().unwrap().pages.keys().next().unwrap().clone();
    let fix = |text: &str| -> Value {
        serde_json::from_str(&text.replace(&id, ARTIFACT_ID)).unwrap()
    };
    let (page, hash) = {
        let artifacts = mirror.artifacts.lock().unwrap();
        (artifacts.pages[&id].clone(), artifacts.hashes[&id].clone())
    };

    let saved = dir.join("session-1");
    save_artifact(&saved, artifact).unwrap();
    let store = Mutex::new(Artifacts::default());
    let mut gallery = saved_artifacts(Some(saved.clone()), std::iter::empty(), &store);
    let gallery_id = store.lock().unwrap().pages.keys().next().unwrap().clone();
    for session in gallery["sessions"].as_array_mut().unwrap() {
        session["updated"] = 1_700_000_100.into();
        for item in session["items"].as_array_mut().unwrap() {
            item["modified"] = 1_700_000_100.into();
            // The real name ends in a hash that can change between Rust versions.
            item["file"] = "/home/me/.local/share/tamale/sessions/session-1/login-flow.html".into();
            item["url"] = format!("/artifact/{ARTIFACT_ID}").into();
        }
    }
    gallery["seq"] = 7.into();
    let _ = gallery_id;

    let older = {
        let mut mirror = Mirror::default();
        let history = [Entry::User("The first prompt".into()), Entry::Assistant("And its answer.".into()), Entry::User("Next".into())];
        mirror.update(&view(&history));
        mirror.older(2)
    };
    let entries = fix(&sent[0]);
    let status = fix(&sent[1]);
    let approval = fix(&sent[2]);
    let question = fix(&sent[3]);
    vec![
        ("entries.json", entries),
        ("status.json", status),
        ("approval.json", approval),
        ("approval-none.json", json!({ "t": "approval", "approval": null })),
        ("question.json", question),
        ("question-none.json", json!({ "t": "question", "question": null })),
        ("artifact.json", fix(&artifact_reply(&id, Some(&page), Some(&hash)).to_string())),
        ("artifact-missing.json", artifact_reply("ffff", None, None)),
        ("artifacts.json", gallery),
        ("hs0.json", json!({ "t": "hs0", "n": STANDARD.encode(NONCE) })),
        ("loading.json", json!({ "t": "loading", "bytes": 2048 })),
        ("older.json", fix(&older)),
        ("diff.json", files::diff_reply("working tree vs HEAD", &crate::diff::parse(SAMPLE_DIFF))),
        ("diff-error.json", json!({ "t": "diff", "error": "/tmp is not inside a git repository" })),
    ]
}

fn view(entries: &[Entry]) -> View<'_> {
    View {
        status: Status::default(),
        entries,
        approval: None,
        question: None,
        roots: Vec::new(),
        artifact_dir: None,
        kitchen: None,
    }
}

#[test]
fn apple_fixtures_are_current() {
    let dir = std::env::temp_dir().join(format!("tamale-fixtures-{}", crate::session::new_id()));
    let mut stale = Vec::new();
    stale.extend(check("e2e-vectors.json", &vectors()));
    for (name, value) in messages(&dir) {
        stale.extend(check(&format!("messages/{name}"), &value));
    }
    let _ = std::fs::remove_dir_all(dir);
    assert!(
        stale.is_empty(),
        "{}; run `UPDATE_FIXTURES=1 cargo test apple_fixtures` and check them in",
        stale.join(", ")
    );
}

#[test]
fn fixtures_come_out_the_same_every_time() {
    assert_eq!(vectors(), vectors());
}
