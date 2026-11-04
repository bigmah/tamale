# Configuration

## Configuration

Defaults live in [`config/default.toml`](../config/default.toml) and are embedded in the
binary. Override just the fields you need in a personal or project TOML file:

```toml
[backend]
kind = "api" # or "claude-code" or "codex"
claude_path = "claude"
claude_config_dirs = ["~/.claude-work"] # more Claude Code logins for the picker
codex_path = "codex"
codex_homes = ["~/.codex-work"] # more Codex logins for the picker

[model]
name = "claude-opus-5-5"
effort = "high"
thinking = "adaptive"
# context_window = 1000000 # otherwise inferred from the selected model

[agent]
max_tokens = 64000
compact_max_tokens = 8000
auto_compact_percent = 80
max_retries = 5
request_timeout_secs = 300

[ui]
verbose = false
mouse = true

[permissions]
mode = "accept-edits"
allow = ["bash:cargo *", "bash:git status*", "bash:git diff*"]
deny = ["bash:git push*", "bash:rm -rf *", "write_file:.env*"]
```

Settings are merged field by field, in this order (later values win):

1. Built-in `config/default.toml`
2. `~/.config/tamale/config.toml` (or `$XDG_CONFIG_HOME/tamale/config.toml`)
3. `tamale.toml` in each directory from the enclosing git root down to the working
   directory; outside a repository, only the working directory is checked
4. A file passed with `--config <path>` (relative to the working directory)
5. The `TAMALE_*` environment variables below
6. `--backend`, `--model`, `--effort`, and `--thinking` flags

All fields are optional in override files. `permissions.allow` and `permissions.deny`
lists are combined across layers rather than replaced. `effort = "default"` resets an inherited
effort to the API default. Unknown fields, invalid types, empty model names, zero
token limits and compaction percentages outside 1–100 produce a startup error.
Missing personal/project files are skipped; an explicit `--config` file must exist.

A repository is someone else's code until you've read it, so opening one can't do anything on
its say-so. A project's `tamale.toml` can't set `[hub]` (where this session and your key's auth
value go), `backend.claude_path`, `codex_path`, `claude_config_dirs` or `codex_homes` (what
tamale runs) or `permissions.allow`, and it can make `permissions.mode` stricter but not looser.
tamale says what it ignored when it starts. All of them work from your own `config.toml` or a
file you pass with `--config`.

The same settings apply to interactive and `-p` runs, tool follow-ups, retries and
manual/automatic compaction. Settings load at startup; slash commands change the
current session's settings without rewriting TOML files.

`TAMALE_BACKEND` sets the backend (`api`, `claude-code` or `codex`).
`TAMALE_MODEL` sets the default model (otherwise `claude-opus-5-5`).
`TAMALE_EFFORT` sets the default effort level (otherwise the API default).
`TAMALE_THINKING` sets the thinking mode (`adaptive` or `on`, the default; `off` to disable).
`TAMALE_CONTEXT_WINDOW` overrides the context window size (default: 1M tokens for Claude 5, Opus 4.6+ and Sonnet 4.6; 200k for older models).
API keys (`ANTHROPIC_API_KEY`, `BRAVE_SEARCH_API_KEY`) and storage paths
(`TAMALE_SESSIONS_DIR`, XDG directories) continue to use environment variables.

Adaptive thinking is **on by default**: Claude decides when and how much to reason,
with `--effort` or `/effort` controlling thoroughness. The setting applies to tool
follow-ups and compaction as well as normal replies. Thinking blocks and their
signatures are preserved in conversation history; thinking text is not displayed.
Use `--thinking off` or `/thinking off` for models that don't support adaptive
thinking (e.g. Haiku 4.5). See [Anthropic's thinking documentation](https://platform.claude.com/docs/en/build-with-claude/thinking).

## Project instructions

These files are appended to the system prompt if present (duplicates skipped):

1. `~/.config/tamale/AGENTS.md` (or `$XDG_CONFIG_HOME/tamale/AGENTS.md`)
2. `AGENTS.md` and `CLAUDE.md` in every directory from the git root down to the working directory
3. The same instruction discovery for each additional directory added with `/add-dir`
   or `--add-dir`; project instructions apply within their respective directories

## Additional directories

Use `/add-dir ../shared` to include another directory in the agent's scope, or pass
`--add-dir <path>` at startup (repeat the flag for multiple directories). `/add-dir`
without a path lists the working directory and all added directories.

Relative paths resolve from the original working directory. Absolute paths, `~` and
`~/...` work too; paths containing spaces can be entered as-is or enclosed in quotes
in the slash command. Directories must exist; duplicate paths and symlinks to an
already added directory are skipped.

The agent receives the full directory list and project instructions on each turn,
including compaction. It can read, edit, search and run commands in added directories
using their absolute paths. The original working directory stays the base for relative
tool paths and shell commands. Additions during a run take effect on the next message.
`/clear` keeps the directory scope, and the footer shows the number of extra directories.
This defines task context; tools already run without a filesystem sandbox.

## Context, caching and retries

- **Prompt caching** is always on: the system prompt, tool definitions and latest message are
  marked with Anthropic cache breakpoints, so each request mostly reads the conversation from cache.
- The footer shows the **current working directory** above the status bar.
- The status bar shows the **context usage** of the last request against the window
  (yellow at 60%, red at 80%) and session totals with the cache hit rate.
- **Compaction**: `/compact [focus]` replaces the conversation with a model-written summary.
  It also runs automatically before a request once the context reaches
  `agent.auto_compact_percent` (default 80%, also mid-turn).
- **Retries**: rate limits (429), overload (529), 5xx and network errors are retried up to
  `agent.max_retries` times (default 5) with exponential backoff (honoring `Retry-After`).
- **Long tasks** have no fixed tool-step or total-token limit. `agent.max_tokens` is a
  limit for each response, including thinking, rather than the whole task. Responses
  that hit it continue automatically with the conversation preserved. Calls from a
  cut-off response are discarded and the model is asked to reissue them in smaller
  steps. Compaction summaries also continue if they hit their response limit.
- **Stalled requests**: opening a stream or waiting for its next event times out after
  `agent.request_timeout_secs` (default 300 seconds), then uses the same retry policy.
  This is an inactivity timeout; active responses can run longer. Streams that close
  without a final response are also retried. A blank reply gets one continuation
  attempt before an explicit error. Continuations and retries are shown in the chat.
  Normal final replies end the run; press `Esc` to interrupt ongoing work.
