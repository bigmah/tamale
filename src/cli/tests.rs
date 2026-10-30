use super::*;

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
    cli.apply(&mut settings);
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
    parse(&[]).unwrap().apply(&mut settings);
    assert_eq!(settings.model.name, "configured-model");
    assert_eq!(settings.model.effort, Some(Effort::High));
    assert_eq!(settings.model.thinking, Thinking::Off);
    parse(&["--effort", "default"])
        .unwrap()
        .apply(&mut settings);
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
        .apply(&mut settings);
    assert_eq!(settings.backend.kind, backend::Kind::ClaudeCode);
    parse(&["-b", "api"]).unwrap().apply(&mut settings);
    assert_eq!(settings.backend.kind, backend::Kind::Api);
    parse(&[]).unwrap().apply(&mut settings);
    assert_eq!(settings.backend.kind, backend::Kind::Api);
    assert!(parse(&["--backend"]).is_err());
    assert!(parse(&["--backend", "gpt"]).is_err());
}
