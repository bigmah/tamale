# harness

A small Claude Code–style coding agent for the terminal, written in pure Rust with
[rig](https://rig.rs) (Anthropic API only) and [ratatui](https://ratatui.rs).

## Usage

```sh
export ANTHROPIC_API_KEY=sk-ant-...
cargo run --release                          # interactive TUI
cargo run --release -- --model claude-opus-4-8
cargo run --release -- --effort low          # less thorough, faster, cheaper
cargo run --release -- --thinking off        # disable adaptive thinking
cargo run --release -- -p "summarize src/"   # one-shot, non-interactive
```

`HARNESS_MODEL` sets the default model (otherwise `claude-sonnet-4-6`).
`HARNESS_EFFORT` sets the default effort level (otherwise the API default).
`HARNESS_THINKING` sets the thinking mode (`adaptive` or `on`, the default; `off` to disable).
`HARNESS_CONTEXT_WINDOW` overrides the context window size (default 200k tokens).

Adaptive thinking is **on by default**: Claude decides when and how much to reason,
with `--effort` or `/effort` controlling thoroughness. The setting applies to tool
follow-ups and compaction as well as normal replies. Thinking blocks and their
signatures are preserved in conversation history; thinking text is not displayed.
Use `--thinking off` or `/thinking off` for models that don't support adaptive
thinking (e.g. Haiku 4.5). See [Anthropic's thinking documentation](https://platform.claude.com/docs/en/build-with-claude/thinking).

## Context, caching and retries

- **Prompt caching** is always on: the system prompt, tool definitions and latest message are
  marked with Anthropic cache breakpoints, so each request mostly reads the conversation from cache.
- The status bar shows the **context usage** of the last request against the window
  (yellow at 60%, red at 80%) and session totals with the cache hit rate.
- **Compaction**: `/compact [focus]` replaces the conversation with a model-written summary.
  It also runs automatically before a request once the context is 80% full (also mid-turn).
- **Retries**: rate limits (429), overload (529), 5xx and network errors are retried up to 5 times
  with exponential backoff (honoring `Retry-After`).

## Project instructions

These files are appended to the system prompt if present (duplicates skipped):

1. `~/.config/harness/AGENTS.md` (or `$XDG_CONFIG_HOME/harness/AGENTS.md`)
2. `AGENTS.md` and `CLAUDE.md` in every directory from the git root down to the working directory

## Tools

`read_file`, `list_dir`, `write_file`, `edit_file`, `bash`.

All tools run immediately without asking for confirmation, in both the TUI and `-p` mode.
The agent can modify files and run any shell command in the working directory.

## Keys

- `Enter` send · `Alt/Shift+Enter` or `Ctrl+J` newline
- `Esc` interrupt the agent
- `PgUp/PgDn`, `Shift+↑/↓` scroll
- `Ctrl+C` clear input / interrupt / quit
- `/verbose [on|off]` toggles verbose output. Off by default: tool calls show as one line
  each with no output, and only the last 3 calls of a run are shown. Verbose shows every call
  with an output preview.
- `/help` opens a scrollable popup listing all commands, keys and tools
- `/model` opens a model picker (live list from the Anthropic API; type to filter, Enter to switch).
  `/model <id>` switches directly. The conversation is kept when switching.
- `/effort` opens an effort picker (↑/↓ or j/k to move, 1–6 to jump, Enter to apply, Esc to
  cancel; the current level is marked with ●). `/effort <level>` sets it directly (`low`,
  `medium`, `high`, `xhigh`, `max`, or `default` to omit it). It is sent as `output_config.effort`; models that don't support it
  (e.g. Haiku 4.5) or a given level reject the request with an error. Shown in the status bar when set.
- `/thinking` toggles adaptive thinking (on by default). `/thinking on` (or `adaptive`)
  and `/thinking off` set it explicitly. Shown in the status bar; changes during a run
  take effect on the next message.
- `/compact [focus]` summarizes the conversation to free context
- `/clear` resets the conversation, `/quit` (or `/exit`) quits

## Layout

- `src/main.rs` – CLI args, terminal setup, event loop
- `src/agent.rs` – agent loop: streams completions via rig, runs tools
- `src/tools.rs` – tool definitions and implementations
- `src/app.rs` – TUI state, key handling, rendering
