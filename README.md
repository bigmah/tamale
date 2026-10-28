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

| Tool         | Approval |
|--------------|----------|
| `read_file`  | no       |
| `list_dir`   | no       |
| `write_file` | yes      |
| `edit_file`  | yes      |
| `bash`       | yes      |

When asked to approve: `y` allow once, `a` always allow that tool for the session, `n` deny.
In `-p` mode, tools that need approval are denied.

## Keys

- `Enter` send · `Alt/Shift+Enter` or `Ctrl+J` newline
- `Esc` interrupt the agent
- `PgUp/PgDn`, `Shift+↑/↓` scroll
- `Ctrl+C` clear input / interrupt / quit
- `/help` opens a scrollable popup listing all commands, keys and tools
- `/clear` resets the conversation, `/quit` (or `/exit`) quits

## Layout

- `src/main.rs` – CLI args, terminal setup, event loop
- `src/agent.rs` – agent loop: streams completions via rig, runs tools, asks for approval
- `src/tools.rs` – tool definitions and implementations
- `src/app.rs` – TUI state, key handling, rendering
