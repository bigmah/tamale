# Development

## Building

`cargo install --path . --locked` builds a release binary and installs it as
`~/.cargo/bin/tamale`, so `tamale` works from any directory. `--locked` builds with the versions in
`Cargo.lock`; without it, `cargo install` ignores the lockfile and resolves the newest compatible
versions of every dependency, which can differ from what the repo is tested with (or need a newer
Rust than you have). The installed copy doesn't track the repo: after changing the code, run it
again to update it. `cargo uninstall tamale` removes it.

To skip reinstalling, symlink the build output instead, and every `cargo build --release` updates
the `tamale` command (a broken build then breaks it too):

```sh
ln -sf "$PWD/target/release/tamale" ~/.cargo/bin/tamale
```

During development, `cargo run --release -- <args>` works in place of `tamale <args>`.

The built browser client is checked in; see [changing the client](web.md#browser) to rebuild it.

## Testing the hub

`cargo test -p tamale-hub` runs the hub in-process with a fake agent through the whole ticket,
cookie, tunnel and socket flow, and the rejections. Against a deployed hub:

```sh
TAMALE_HUB_URL=wss://tamale-hub.com TAMALE_HUB_KEY=thk_… cargo test -p tamale real_hub -- --ignored
```

## Layout

- `src/main.rs` – startup, terminal setup, event loop and one-shot output
- `src/browser.rs`, `src/browser/` – `/browser`: the local server, the page, the backend
  that records changed cells, and input translation; `dist/` holds the built client
- `src/web.rs`, `src/web/` – `/web`: the server and session sync, markdown and code
  rendering, workspace file reads, and the page itself (HTML, CSS, JavaScript)
- `src/app/web.rs` – what the web UI shows of the session, and its input back to the app
- `web/protocol` – the messages between tamale and its browser client
- `web/client` – the browser client (Rust, compiled to WebAssembly by `web/build.sh`)
- `src/cli.rs` – command-line parsing and overrides
- `src/config.rs`, `config/default.toml` – typed settings, defaults, file discovery and merging
- `src/backend.rs` – backend selection (API, Claude Code or Codex) and dispatch
- `src/backend/claude_code.rs` – running `claude -p`, sign-in, stream translation and replay
- `src/backend/codex.rs` – running `codex exec`, sign-in, event translation and usage
- `src/backend/logins.rs` – the Claude Code and Codex logins offered in the picker
- `src/backend/mcp.rs` – the local MCP server that offers tamale's tools to Claude Code and Codex
- `src/backend/subagent.rs` – the `agent` tool: running a subagent on the current backend
- `src/agent.rs` – agent turns, tool execution and compaction
- `src/agent/{request,reasoning,prompt,models}.rs` – streaming/retries, reasoning settings,
  instruction loading and model metadata/cost estimates
- `src/tools.rs` – tool definitions and implementations
- `src/permissions.rs` – permission modes and allow/deny rule matching
- `src/checkpoint.rs` – file snapshots for `/undo` and `/rewind`
- `src/diff.rs` – running and parsing `git diff` for the diff viewer
- `src/tools/search.rs`, `src/tools/web.rs` – built-in repository search and web tools
- `src/tools/shell.rs` – running `bash` commands in a process group
- `src/chef.rs`, `src/app/chef.rs` – the tamale chef: its tools and prompt, reaching sessions
  through the hub, its kitchen panel and every session's side of a side question
- `src/app.rs` – conversation state and agent event handling
- `src/app/{input,commands,sessions,view,help}.rs` – key/clipboard input, commands and pickers,
  session actions, rendering and help content
- `src/app/{approval,rewind}.rs` – the approval popup, `/undo` and the rewind picker
- `src/app/diff.rs` – the `/diff` viewer: file tree, diff rendering and navigation
- `src/app/prompts.rs` – prompt history for ↑/↓ recall
- `src/clipboard.rs` – system clipboard access
- `src/session.rs` – saving, listing and loading sessions

## Performance

The TUI caches styled transcript lines and wrapped row offsets, renders only the
visible slice, and updates the unfinished Markdown line as text streams in. Token
bursts share redraws capped near 60 fps; the idle screen does not redraw. Web tools
reuse HTTP clients and connection pools. `--help` skips async runtime initialization.

Reproduce the local CPU/I/O benchmarks without an API key or network requests:

```sh
cargo test --release perf_ -- --ignored --nocapture --test-threads=1
```

On the development Mac, a 1,000-turn transcript at 120×40 went from about 25 ms to
0.11 ms per typing/redraw frame. Streaming into a 5,000-line response takes about
0.09 ms per frame. Listing 30 sessions totaling 60 MB went from about 7.6 ms to
0.5 ms with cached metadata. These measure local rendering/listing work; API and
terminal display latency are separate. Initial layout and terminal resizing still
process the transcript once.
