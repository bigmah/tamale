use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("tamale-config-test-{}", crate::session::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn layers_merge_fields_from_user_project_explicit_env_and_cli() {
    let fixture = Fixture::new();
    fixture.write(
        "personal/config.toml",
        "[model]\nname = 'personal'\neffort = 'high'\n[ui]\nverbose = true\nmouse = false",
    );
    // A .git file also marks worktree roots. Never read config above that root.
    fixture.write("tamale.toml", "[agent]\nmax_tokens = 99");
    fixture.write("project/.git", "gitdir: elsewhere");
    fixture.write(
        "project/tamale.toml",
        "[model]\nname = 'project'\n[agent]\nmax_tokens = 4096",
    );
    fixture.write(
        "project/nested/tamale.toml",
        "[model]\nname = 'nested'\neffort = 'default'\n[ui]\nverbose = false",
    );
    let cwd = fixture.0.join("project/nested");
    let explicit = fixture.write("extra.toml", "[model]\nname = 'explicit'\nthinking = 'off'");
    let mut settings =
        Settings::load_from(&cwd, Some(&fixture.0.join("personal")), Some(&explicit)).unwrap();
    assert_eq!(settings.model.name, "explicit");
    assert_eq!(
        settings.model.effort, None,
        "default resets an inherited effort"
    );
    assert_eq!(settings.model.thinking, Thinking::Off);
    assert_eq!(settings.agent.max_tokens, 4096);
    assert_eq!(
        settings.agent.compact_max_tokens, 8000,
        "omitted fields retain defaults"
    );
    assert!(!settings.ui.verbose, "false overrides true");
    assert!(
        !settings.ui.mouse,
        "unmentioned fields survive later sections"
    );

    settings
        .apply_env(|key| match key {
            "TAMALE_MODEL" => Some("environment".into()),
            "TAMALE_EFFORT" => Some("medium".into()),
            "TAMALE_THINKING" => Some("on".into()),
            "TAMALE_CONTEXT_WINDOW" => Some("300000".into()),
            _ => None,
        })
        .unwrap();
    assert_eq!(settings.model.name, "environment");
    assert_eq!(settings.model.effort, Some(Effort::Medium));
    assert_eq!(settings.model.thinking, Thinking::Adaptive);
    assert_eq!(settings.model.context_window(), 300000);

    let cli = crate::cli::Cli::parse(["--model", "cli", "--effort", "default"].map(String::from))
        .unwrap();
    cli.apply(&mut settings);
    settings.validate().unwrap();
    assert_eq!(settings.model.name, "cli");
    assert_eq!(settings.model.effort, None);
    assert_eq!(settings.model.thinking, Thinking::Adaptive);
    assert_eq!(settings.model.context_window(), 300000);
    assert_eq!(settings.turn_config(123).limits.max_tokens, 4096);
}

#[test]
fn nearest_project_wins_and_non_repo_discovery_stays_in_cwd() {
    let fixture = Fixture::new();
    fixture.write("tamale.toml", "[model]\nname = 'parent'");
    fixture.write("project/.git", "gitdir: elsewhere");
    fixture.write("project/tamale.toml", "[model]\nname = 'root'");
    fixture.write("project/nested/tamale.toml", "[model]\nname = 'nearest'");
    let cwd = fixture.0.join("project/nested/deep");
    std::fs::create_dir_all(&cwd).unwrap();
    let settings = Settings::load_from(&cwd, None, None).unwrap();
    assert_eq!(settings.model.name, "nearest");
    assert_eq!(
        project_dirs(&cwd),
        vec![
            fixture.0.join("project"),
            fixture.0.join("project/nested"),
            cwd.clone()
        ]
    );

    let outside = fixture.0.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let settings = Settings::load_from(&outside, None, None).unwrap();
    assert_eq!(settings.model.name, Settings::default().model.name);
    assert_eq!(project_dirs(&outside), vec![outside.as_path()]);
}

#[test]
fn explicit_files_are_required_and_errors_identify_the_file() {
    let fixture = Fixture::new();
    let missing = fixture.0.join("missing.toml");
    let error = Settings::load_from(&fixture.0, None, Some(&missing)).unwrap_err();
    assert!(format!("{error:#}").contains("missing.toml"));
    let error = Settings::load_from(&fixture.0, None, Some(&fixture.0)).unwrap_err();
    assert!(format!("{error:#}").contains("reading"));

    for (text, message) in [
        ("[model", "parsing"),
        ("mdoel = 'typo'", "unknown field"),
        ("[model]\nthnking = 'off'", "unknown field"),
        ("[ui]\nmouse = 'false'", "boolean"),
        ("[model]\neffort = 'typo'", "unknown effort"),
        ("[model]\nthinking = 'typo'", "unknown thinking"),
        ("[model]\nname = ' '", "model.name"),
        ("[model]\ncontext_window = 0", "model.context_window"),
        ("[agent]\nmax_tokens = 0", "agent.max_tokens"),
        (
            "[agent]\ncompact_max_tokens = 0",
            "agent.compact_max_tokens",
        ),
        (
            "[agent]\nauto_compact_percent = 0",
            "agent.auto_compact_percent",
        ),
        (
            "[agent]\nauto_compact_percent = 101",
            "agent.auto_compact_percent",
        ),
        ("[agent]\nmax_retries = -1", "u32"),
        (
            "[agent]\nrequest_timeout_secs = 0",
            "agent.request_timeout_secs",
        ),
    ] {
        let path = fixture.write("invalid.toml", text);
        let error = Settings::load_from(&fixture.0, None, Some(&path)).unwrap_err();
        let error = format!("{error:#}");
        assert!(
            error.contains("invalid.toml") && error.contains(message),
            "{text}: {error}"
        );
    }
}

#[test]
fn invalid_environment_is_reported_and_blank_optional_modes_are_ignored() {
    for (key, value) in [
        ("TAMALE_EFFORT", "typo"),
        ("TAMALE_THINKING", "typo"),
        ("TAMALE_CONTEXT_WINDOW", "bad"),
    ] {
        let error = Settings::default()
            .apply_env(|name| (name == key).then(|| value.into()))
            .unwrap_err();
        assert!(format!("{error:#}").contains(key));
    }
    let mut settings = Settings::default();
    settings.model.effort = Some(Effort::High);
    settings.model.thinking = Thinking::Off;
    settings
        .apply_env(|key| matches!(key, "TAMALE_EFFORT" | "TAMALE_THINKING").then(|| " ".into()))
        .unwrap();
    assert_eq!(settings.model.effort, Some(Effort::High));
    assert_eq!(settings.model.thinking, Thinking::Off);
    settings
        .apply_env(|key| (key == "TAMALE_CONTEXT_WINDOW").then(|| "0".into()))
        .unwrap();
    assert!(settings.validate().is_err());
}

#[test]
fn model_changes_recompute_inferred_windows_and_keep_explicit_overrides() {
    let mut settings = Settings::default();
    settings.validate().unwrap();
    assert_eq!(settings.model.context_window(), 1_000_000);
    settings.model.name = "unknown-model".into();
    assert_eq!(settings.model.context_window(), 200_000);
    settings.model.context_window = Some(500_000);
    settings.model.name = "claude-opus-5-5".into();
    let turn = settings.turn_config(42);
    assert_eq!(turn.context_window, 500_000);
    assert_eq!(turn.context_tokens, 42);
}

#[test]
fn permission_rules_accumulate_across_layers_and_invalid_rules_name_the_file() {
    use crate::permissions::{Decision, Mode, Policy};
    use serde_json::json;

    let fixture = Fixture::new();
    fixture.write(
        "personal/config.toml",
        "[permissions]\nmode = 'auto'\ndeny = ['bash:git push*']",
    );
    fixture.write(
        "project/tamale.toml",
        "[permissions]\nmode = 'accept-edits'\nallow = ['bash:cargo *']\ndeny = ['write_file:.env*']",
    );
    let cwd = fixture.0.join("project");
    let settings = Settings::load_from(&cwd, Some(&fixture.0.join("personal")), None).unwrap();
    assert_eq!(settings.permissions.mode, Mode::AcceptEdits);
    assert_eq!(settings.permissions.deny.len(), 2);
    let policy = Policy::new(&settings.permissions, cwd.clone());
    let bash = |command: &str| policy.check("bash", &json!({ "command": command }));
    assert!(matches!(bash("git push"), Decision::Deny(_)));
    assert_eq!(bash("cargo test"), Decision::Allow);
    assert!(matches!(bash("make"), Decision::Ask { .. }));
    assert_eq!(Settings::default().permissions.mode, Mode::Auto);

    for (text, expected) in [
        ("[permissions]\nmode = 'yolo'", "unknown permission mode"),
        ("[permissions]\nallow = ['shell:ls']", "unknown tool"),
        ("[permissions]\ndeny = ['bash:[oops']", "invalid pattern"),
    ] {
        let path = fixture.write("bad.toml", text);
        let error = format!(
            "{:#}",
            Settings::load_from(&cwd, None, Some(&path)).unwrap_err()
        );
        assert!(
            error.contains("bad.toml") && error.contains(expected),
            "{error}"
        );
    }
}

#[test]
fn backend_comes_from_files_and_environment() {
    use crate::backend::Kind;
    let dir = Fixture::new();
    let explicit = dir.write(
        "backend.toml",
        "[backend]\nkind = \"claude-code\"\nclaude_path = \"/opt/claude\"\n",
    );
    let settings = Settings::load_from(&dir.0, None, Some(&explicit)).unwrap();
    assert_eq!(settings.backend.kind, Kind::ClaudeCode);
    assert_eq!(settings.backend.claude_path, PathBuf::from("/opt/claude"));
    assert_eq!(Settings::default().backend.kind, Kind::Api);
    assert_eq!(
        Settings::default().backend.codex_path,
        PathBuf::from("codex")
    );
    let codex = dir.write(
        "codex.toml",
        "[backend]\nkind = \"codex\"\ncodex_homes = [\"~/.codex-work\"]\n",
    );
    let settings = Settings::load_from(&dir.0, None, Some(&codex)).unwrap();
    assert_eq!(settings.backend.kind, Kind::Codex);
    assert_eq!(
        settings.backend.codex_homes,
        [PathBuf::from("~/.codex-work")]
    );

    let mut settings = Settings::default();
    settings
        .apply_env(|key| (key == "TAMALE_BACKEND").then(|| "claude-code".into()))
        .unwrap();
    assert_eq!(settings.backend.kind, Kind::ClaudeCode);
    let error = Settings::default()
        .apply_env(|key| (key == "TAMALE_BACKEND").then(|| "openai".into()))
        .unwrap_err();
    assert!(format!("{error:#}").contains("TAMALE_BACKEND"));

    let bad = dir.write("bad.toml", "[backend]\nkind = \"local\"\n");
    assert!(Settings::load_from(&dir.0, None, Some(&bad)).is_err());
}

#[test]
fn the_hub_key_only_comes_from_the_environment_and_the_site() {
    let settings = |vars: &[(&str, &str)]| {
        let mut settings = Settings::default();
        settings
            .apply_env(|name| {
                vars.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            })
            .unwrap();
        settings.hub
    };
    let hub = settings(&[]);
    assert_eq!(hub.url, "wss://tamale-hub.com");
    assert_eq!(
        hub.key_bytes().unwrap_err(),
        "Set TAMALE_HUB_KEY to use the hub. Make a key at https://tamale-hub.com"
    );
    // The message points at the hub in use.
    let hub = settings(&[("TAMALE_HUB_URL", "ws://hub.localhost:8787/")]);
    assert!(hub.key_bytes().unwrap_err().ends_with("http://hub.localhost:8787"));
    // Someone who set up the old tokens is told, rather than just asked for a key.
    let hub = settings(&[("TAMALE_HUB_TOKEN", "tha_old")]);
    assert!(hub.key_bytes().unwrap_err().contains("TAMALE_HUB_TOKEN has been replaced"));

    let key = tamale_hub_protocol::encode_key(&[5; 32]);
    let hub = settings(&[("TAMALE_HUB_KEY", &format!(" {key}\n")), ("TAMALE_HUB_TOKEN", "tha_old")]);
    assert_eq!(hub.key_bytes().unwrap(), [5; 32]);
    let hub = settings(&[("TAMALE_HUB_KEY", "tha_old")]);
    assert!(hub.key_bytes().unwrap_err().contains("isn't a key"));
    // Never from a settings file.
    assert!(toml::from_str::<HubSettings>("key = \"thk_x\"\n").is_err());
}
