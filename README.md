# harness

A small Claude Code–style coding agent for the terminal, written in pure Rust with
[rig](https://rig.rs) (Anthropic API only) and [ratatui](https://ratatui.rs).

## Usage

```sh
export ANTHROPIC_API_KEY=sk-ant-...
cargo run --release                          # interactive TUI
cargo run --release -- --model claude-opus-4-8
cargo run --release -- -p "summarize src/"   # one-shot, non-interactive
```

`HARNESS_MODEL` sets the default model (otherwise `claude-sonnet-4-6`).

## Tools

`read_file`, `list_dir`, `write_file`, `edit_file`, `bash`.

All tools run immediately without asking for confirmation, in both the TUI and `-p` mode.
The agent can modify files and run any shell command in the working directory.

## Keys

- `Enter` send · `Alt/Shift+Enter` or `Ctrl+J` newline
- `Esc` interrupt the agent
- `PgUp/PgDn`, `Shift+↑/↓` scroll
- `Ctrl+C` clear input / interrupt / quit
- `/help` opens a scrollable popup listing all commands, keys and tools
- `/model` opens a model picker (live list from the Anthropic API; type to filter, Enter to switch).
  `/model <id>` switches directly. The conversation is kept when switching.
- `/clear` resets the conversation, `/quit` (or `/exit`) quits

## Layout

- `src/main.rs` – CLI args, terminal setup, event loop
- `src/agent.rs` – agent loop: streams completions via rig, runs tools
- `src/tools.rs` – tool definitions and implementations
- `src/app.rs` – TUI state, key handling, rendering
