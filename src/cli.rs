//! Command-line parsing. Persistent defaults belong in `config`, not here.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::agent::{Effort, Thinking};
use crate::config::Settings;

#[cfg(test)]
mod tests;

pub const HELP: &str = "usage: tamale [--backend <kind>] [--add-dir <path>] [--config <path>] [--model <id>] [--effort <level>] [--thinking <mode>] [-c | -r] [--foreground] [-p <prompt> [--yes]]
       tamale chef [off]
       tamale ls | attach [<id>] | kill <id>
       tamale update

chef              Talk to the tamale chef, which looks after all your sessions, on every
                  machine with your hub key (or go back to it if it's running).
chef off          Stop the chef, wherever it runs (it stays saved).
ls                List the sessions running in the background.
attach [<id>]     Go back to a session running in the background.
kill <id>         End a session running in the background.
update            Install the latest release of tamale, if this one is older.

-b, --backend      api (ANTHROPIC_API_KEY), claude-code (your Claude Code sign-in),
                   codex (your Codex sign-in), or an OpenAI-compatible provider with its
                   own key: deepseek, kimi, gemini, openrouter, xai, groq, mistral, zai,
                   openai, ollama, or one you add under [backend.providers].
--add-dir <path>   Include another directory in the agent's scope (repeatable).
--config <path>    Add a TOML settings file after user and project settings.
-m, --model <id>  Model ID (Anthropic, a Codex model on codex, or the provider's).
-e, --effort      low, medium, high, xhigh, max, or default (API decides).
--thinking        adaptive (on) or off.
-c, --continue    Go back to the most recent session in this directory, running or saved.
-r, --resume      Pick a saved session to resume.
--foreground      Run the session in this terminal's process, so it can't detach.
-p, --print       Run one prompt non-interactively and exit.
-y, --yes         With -p, run tool calls that would otherwise need approval.
-V, --version     Print tamale's version.
-h, --help        Show this help.

Settings: built-in defaults < ~/.config/tamale/config.toml < project tamale.toml
          < --config < TAMALE_* environment variables < CLI flags.
The api backend requires ANTHROPIC_API_KEY (without it, tamale asks which backend to use);
claude-code requires the `claude` CLI and
codex the `codex` CLI (tamale offers to sign in when they aren't). See config/default.toml for all settings.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    Latest,
    Pick,
}

/// A subcommand, which works on sessions running in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    List,
    Attach(Option<u32>),
    Kill(u32),
    /// Install the latest release (see `update`).
    Update,
}

#[derive(Default, Debug)]
pub struct Cli {
    pub command: Option<Command>,
    /// Keep the session in this process instead of starting a server for it.
    pub foreground: bool,
    /// Be the server a client started (see `background`).
    pub server: bool,
    pub additional_dirs: Vec<PathBuf>,
    pub config: Option<PathBuf>,
    pub print_prompt: Option<String>,
    pub resume: Option<Resume>,
    /// Be (or go back to) the tamale chef rather than a session in a project.
    pub chef: bool,
    /// `tamale chef off`: stop the chef, wherever it runs.
    pub chef_off: bool,
    /// Approve every call that would prompt in `-p` mode (deny rules still apply).
    pub yes: bool,
    pub help: bool,
    pub version: bool,
    backend: Option<String>,
    model: Option<String>,
    // Outer None means no flag; Some(None) explicitly resets the API default.
    effort: Option<Option<Effort>>,
    thinking: Option<Thinking>,
}

impl Cli {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = args.into_iter().peekable();
        let mut cli = Self {
            ..Self::default()
        };
        let session_id = |arg: &str| {
            arg.parse::<u32>()
                .with_context(|| format!("{arg:?} isn't a session id; `tamale ls` lists them"))
        };
        match args.peek().map(String::as_str) {
            Some("ls" | "list") => {
                args.next();
                cli.command = Some(Command::List);
            }
            Some("attach") => {
                args.next();
                let id = args.next().map(|id| session_id(&id)).transpose()?;
                cli.command = Some(Command::Attach(id));
            }
            Some("kill") => {
                args.next();
                let id = args.next().context("kill needs a session id")?;
                cli.command = Some(Command::Kill(session_id(&id)?));
            }
            Some("update" | "upgrade") => {
                args.next();
                cli.command = Some(Command::Update);
            }
            Some("chef") => {
                args.next();
                if matches!(args.peek().map(String::as_str), Some("off" | "stop")) {
                    args.next();
                    cli.chef_off = true;
                } else {
                    cli.chef = true;
                }
            }
            _ => {}
        }
        if cli.command.is_some()
            && let Some(extra) = args.next()
        {
            bail!("unexpected argument: {extra}");
        }
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--add-dir" => cli
                    .additional_dirs
                    .push(args.next().context("--add-dir needs a path")?.into()),
                "--config" => {
                    cli.config = Some(args.next().context("--config needs a path")?.into())
                }
                "-b" | "--backend" => {
                    cli.backend = Some(args.next().context("--backend needs a value")?)
                }
                "-m" | "--model" => cli.model = Some(args.next().context("--model needs a value")?),
                "-e" | "--effort" => {
                    cli.effort = Some(Effort::parse_setting(
                        &args.next().context("--effort needs a value")?,
                    )?)
                }
                "--thinking" => {
                    cli.thinking = Some(Thinking::parse_setting(
                        &args.next().context("--thinking needs a value")?,
                    )?)
                }
                "-y" | "--yes" => cli.yes = true,
                "--foreground" => cli.foreground = true,
                "--server" => cli.server = true,
                "-c" | "--continue" => cli.resume = Some(Resume::Latest),
                "-r" | "--resume" => cli.resume = Some(Resume::Pick),
                "-p" | "--print" => {
                    cli.print_prompt = Some(args.next().context("--print needs a prompt")?)
                }
                "-V" | "--version" => {
                    cli.version = true;
                    return Ok(cli);
                }
                "-h" | "--help" => {
                    cli.help = true;
                    return Ok(cli);
                }
                other => bail!("unknown argument: {other}"),
            }
        }
        Ok(cli)
    }

    pub fn apply(&self, settings: &mut Settings) -> Result<()> {
        if let Some(backend) = &self.backend {
            settings.backend.select(backend).context("--backend")?;
        }
        if let Some(model) = &self.model {
            settings.model.name = model.clone();
        }
        if let Some(effort) = self.effort {
            settings.model.effort = effort;
        }
        if let Some(thinking) = self.thinking {
            settings.model.thinking = thinking;
        }
        Ok(())
    }
}
