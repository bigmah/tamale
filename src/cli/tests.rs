use super::*;
use crate::backend;

fn parse(args: &[&str]) -> Result<Cli> {
    Cli::parse(args.iter().map(|s| s.to_string()))
}

#[test]
fn parses_startup_options_and_keeps_last_value() {
    let cli = parse(&[
        "--config",
        "settings.toml",
        "-m",
        "first",
        "--model",
        "last",
        "-e",
        "HIGH",
        "--thinking",
        "off",
        "-c",
        "-r",
        "-p",
        "a prompt",
    ])
    .unwrap();
    assert_eq!(cli.config, Some(PathBuf::from("settings.toml")));
    assert_eq!(cli.print_prompt.as_deref(), Some("a prompt"));
    assert_eq!(cli.resume, Some(Resume::Pick));
    let mut settings = Settings::default();
    cli.apply(&mut settings).unwrap();
    assert_eq!(settings.model.name, "last");
    assert_eq!(settings.model.effort, Some(Effort::High));
    assert_eq!(settings.model.thinking, Thinking::Off);
}

#[test]
fn omitted_flags_preserve_settings_and_default_resets_effort() {
    let mut settings = Settings::default();
    settings.model.name = "configured-model".into();
    settings.model.effort = Some(Effort::High);
    settings.model.thinking = Thinking::Off;
    parse(&[]).unwrap().apply(&mut settings).unwrap();
    assert_eq!(settings.model.name, "configured-model");
    assert_eq!(settings.model.effort, Some(Effort::High));
    assert_eq!(settings.model.thinking, Thinking::Off);
    parse(&["--effort", "default"])
        .unwrap()
        .apply(&mut settings)
        .unwrap();
    assert_eq!(settings.model.effort, None);
}

#[test]
fn reports_missing_values_unknown_options_and_invalid_modes() {
    for flag in [
        "--add-dir",
        "--config",
        "--model",
        "--effort",
        "--thinking",
        "--print",
    ] {
        assert!(parse(&[flag]).unwrap_err().to_string().contains("needs"));
    }
    assert!(parse(&["--unknown"]).is_err());
    assert!(parse(&["--effort", "typo"]).is_err());
    assert!(parse(&["--thinking", "typo"]).is_err());
    assert!(parse(&["--help"]).unwrap().help);
}

#[test]
fn additional_directories_are_repeatable_and_work_with_print_and_resume() {
    let cli = parse(&[
        "--add-dir",
        "../shared",
        "--add-dir",
        "/tmp/path with spaces",
        "-c",
        "-p",
        "check both",
    ])
    .unwrap();
    assert_eq!(
        cli.additional_dirs,
        [
            PathBuf::from("../shared"),
            PathBuf::from("/tmp/path with spaces")
        ]
    );
    assert_eq!(cli.resume, Some(Resume::Latest));
    assert_eq!(cli.print_prompt.as_deref(), Some("check both"));
    assert!(!cli.yes);
    assert!(parse(&["-p", "go", "--yes"]).unwrap().yes);
    assert!(parse(&["-y"]).unwrap().yes);
}

#[test]
fn backend_flag_selects_where_inference_comes_from() {
    let mut settings = Settings::default();
    parse(&["--backend", "claude-code"])
        .unwrap()
        .apply(&mut settings)
        .unwrap();
    assert_eq!(settings.backend.kind, backend::Kind::ClaudeCode);
    parse(&["-b", "codex"])
        .unwrap()
        .apply(&mut settings)
        .unwrap();
    assert_eq!(settings.backend.kind, backend::Kind::Codex);
    parse(&["-b", "api"]).unwrap().apply(&mut settings).unwrap();
    assert_eq!(settings.backend.kind, backend::Kind::Api);
    parse(&[]).unwrap().apply(&mut settings).unwrap();
    assert_eq!(settings.backend.kind, backend::Kind::Api);
    parse(&["-b", "DeepSeek"])
        .unwrap()
        .apply(&mut settings)
        .unwrap();
    assert_eq!(settings.backend.kind, backend::Kind::Compat);
    assert_eq!(settings.backend.provider, "deepseek");
    assert!(parse(&["--backend"]).is_err());
    let error = parse(&["--backend", "gpt"])
        .unwrap()
        .apply(&mut settings)
        .unwrap_err();
    assert!(format!("{error:#}").contains("kimi"), "{error:#}");
}

#[test]
fn parses_background_subcommands() {
    assert_eq!(parse(&["ls"]).unwrap().command, Some(Command::List));
    assert_eq!(
        parse(&["attach"]).unwrap().command,
        Some(Command::Attach(None))
    );
    assert_eq!(
        parse(&["attach", "42"]).unwrap().command,
        Some(Command::Attach(Some(42)))
    );
    assert_eq!(
        parse(&["kill", "42"]).unwrap().command,
        Some(Command::Kill(42))
    );
    assert!(parse(&["kill"]).is_err());
    assert!(parse(&["attach", "nope"]).is_err());
    assert!(parse(&["ls", "extra"]).is_err());
    assert!(parse(&["--foreground", "ls"]).is_err());
}

#[test]
fn parses_foreground_and_server() {
    assert!(parse(&["--foreground"]).unwrap().foreground);
    assert!(parse(&["--server"]).unwrap().server);
}

#[test]
fn parses_chef_and_chef_off() {
    let chef = parse(&["chef"]).unwrap();
    assert!(chef.chef && !chef.chef_off);
    for word in ["off", "stop"] {
        let off = parse(&["chef", word]).unwrap();
        assert!(off.chef_off && !off.chef, "{word}");
    }
}

#[test]
fn parses_update_and_version() {
    assert_eq!(parse(&["update"]).unwrap().command, Some(Command::Update));
    assert_eq!(parse(&["upgrade"]).unwrap().command, Some(Command::Update));
    assert!(parse(&["update", "now"]).is_err());
    assert!(parse(&["--version"]).unwrap().version);
    assert!(parse(&["-V"]).unwrap().version);
}
