//! Command-line parsing. Persistent defaults belong in `config`, not here.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::agent::{Effort, Thinking};
use crate::config::Settings;

#[cfg(test)]
mod tests;

pub const HELP: &str = "usage: tamale [--add-dir <path>] [--config <path>] [--model <id>] [--effort <level>] [--thinking <mode>] [-c | -r] [-p <prompt> [--yes]]

--add-dir <path>   Include another directory in the agent's scope (repeatable).
--config <path>    Add a TOML settings file after user and project settings.
-m, --model <id>  Anthropic model ID.
-e, --effort      low, medium, high, xhigh, max, or default (API decides).
--thinking        adaptive (on) or off.
-c, --continue    Resume the most recent session in this directory.
-r, --resume      Pick a saved session to resume.
-p, --print       Run one prompt non-interactively and exit.
-y, --yes         With -p, run tool calls that would otherwise need approval.
-h, --help        Show this help.

Settings: built-in defaults < ~/.config/tamale/config.toml < project tamale.toml
          < --config < TAMALE_* environment variables < CLI flags.
Requires ANTHROPIC_API_KEY. See config/default.toml for all settings.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    Latest,
    Pick,
}

#[derive(Default, Debug)]
pub struct Cli {
    pub additional_dirs: Vec<PathBuf>,
    pub config: Option<PathBuf>,
    pub print_prompt: Option<String>,
    pub resume: Option<Resume>,
    /// Approve every call that would prompt in `-p` mode (deny rules still apply).
    pub yes: bool,
    pub help: bool,
    model: Option<String>,
    // Outer None means no flag; Some(None) explicitly resets the API default.
    effort: Option<Option<Effort>>,
    thinking: Option<Thinking>,
}

impl Cli {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = args.into_iter();
        let mut cli = Self::default();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--add-dir" => cli
                    .additional_dirs
                    .push(args.next().context("--add-dir needs a path")?.into()),
                "--config" => {
                    cli.config = Some(args.next().context("--config needs a path")?.into())
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
                "-c" | "--continue" => cli.resume = Some(Resume::Latest),
                "-r" | "--resume" => cli.resume = Some(Resume::Pick),
                "-p" | "--print" => {
                    cli.print_prompt = Some(args.next().context("--print needs a prompt")?)
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

    pub fn apply(&self, settings: &mut Settings) {
        if let Some(model) = &self.model {
            settings.model.name = model.clone();
        }
        if let Some(effort) = self.effort {
            settings.model.effort = effort;
        }
        if let Some(thinking) = self.thinking {
            settings.model.thinking = thinking;
        }
    }
}
