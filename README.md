# harness

A small Claude Code–style coding agent for the terminal, written in pure Rust with
[rig](https://rig.rs) (Anthropic API only) and [ratatui](https://ratatui.rs).

## Usage

```sh
export ANTHROPIC_API_KEY=sk-ant-...
cargo run --release                          # interactive TUI
cargo run --release -- --model claude-opus-5-5
cargo run --release -- --effort low          # less thorough, faster, cheaper
cargo run --release -- --thinking off        # disable adaptive thinking
cargo run --release -- -p "summarize src/"   # one-shot, non-interactive
cargo run --release -- --continue            # resume the latest session in this directory
cargo run --release -- --resume              # pick a saved session to resume
```

`HARNESS_MODEL` sets the default model (otherwise `claude-opus-5-5`).
`HARNESS_EFFORT` sets the default effort level (otherwise the API default).
`HARNESS_THINKING` sets the thinking mode (`adaptive` or `on`, the default; `off` to disable).
`HARNESS_CONTEXT_WINDOW` overrides the context window size (default: 1M tokens for Claude 5, Opus 4.6+ and Sonnet 4.6; 200k for older models).

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

## Sessions

Every interactive conversation is saved as JSON in `~/.local/share/harness/sessions/`
(`$XDG_DATA_HOME/harness/sessions`, or `HARNESS_SESSIONS_DIR`) after each prompt, tool step
and on exit. It includes the full model history (so the agent picks up with all context) and
the visible transcript.

- `/resume` opens a picker of sessions started in the current directory (most recent first,
  type to filter). `/resume <n>` resumes the nth most recent, `/resume <id>` a session by id
  (or unique id prefix) from any directory.
- `-c`/`--continue` resumes the latest session in this directory on startup; `-r`/`--resume`
  opens the picker.
- A resumed session keeps the current model (use `/model` to switch); further messages are
  saved to the same session. `/clear` starts a new session (the old one stays resumable).
- `-p` runs are not saved.

## Project instructions

These files are appended to the system prompt if present (duplicates skipped):

1. `~/.config/harness/AGENTS.md` (or `$XDG_CONFIG_HOME/harness/AGENTS.md`)
2. `AGENTS.md` and `CLAUDE.md` in every directory from the git root down to the working directory

## Tools

`read_file`, `list_dir`, `write_file`, `edit_file`, `bash`.

Every tool call carries a short model-written `description` ("Find where tool calls are
rendered"). The chat shows that as the step's label, with the raw call dimmed after it, so a
run of tool calls reads as a sequence of steps. Long runs are collapsed to the last 5 steps.
`/verbose` shows every step, the full call and its output.

All tools run immediately without asking for confirmation, in both the TUI and `-p` mode.
The agent can modify files and run any shell command in the working directory.

## Keys

- `Enter` send · `Alt/Shift+Enter` or `Ctrl+J` newline
- `Esc` interrupt the agent
- `PgUp/PgDn`, `Shift+↑/↓`, mouse wheel scroll (a scrollbar and "↓ N more lines" badge show while scrolled up)
- `/mouse [on|off]` toggles mouse capture (on by default). While on, hold Shift (Option in
  iTerm2/Terminal.app) to select text; turn it off to restore native selection.
- `Ctrl+C` clear input / interrupt / quit
- `/verbose [on|off]` toggles verbose output. Off by default: each step shows as one line
  (its description) with no output, and only the last 5 steps of a run are shown. Verbose shows
  every step with the full call and an output preview.
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
- `/resume [n|id]` continues a saved session (see [Sessions](#sessions))
- `/clear` resets the conversation, `/quit` (or `/exit`) quits

## Layout

- `src/main.rs` – CLI args, terminal setup, event loop
- `src/agent.rs` – agent loop: streams completions via rig, runs tools
- `src/tools.rs` – tool definitions and implementations
- `src/app.rs` – TUI state, key handling, rendering
- `src/session.rs` – saving, listing and loading sessions
