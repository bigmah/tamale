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

### Building the browser apps

tamale serves two apps that run in the browser, both Rust compiled to WebAssembly: the `/web`
page (`web/`, with pullspace in it) and the `/browser` client (`browser/client`). `build.rs`
builds them as part of every `cargo build`, so nothing built is checked in and there's nothing
else to install or run. Each is a cargo build of its own for `wasm32-unknown-unknown`, always in
release, into `target/ui` (shared by every profile); then wasm-bindgen, as a library
(`wasm-bindgen-cli-support`, which must match the `wasm-bindgen` both crates pin), writes its
script, with the JavaScript modules its crates bring folded in, and `src/web.rs` and
`src/browser.rs` embed the results from `OUT_DIR`. `rust-toolchain.toml` has rustup add the
wasm target; the first build takes a minute or two longer for it, and later ones rebuild the apps
only when `web/` or `browser/` change. `cargo check`, clippy and rust-analyzer run `build.rs`
too.

## Testing the hub

`cargo test -p tamale-hub` runs the hub in-process with a fake agent through the whole ticket,
cookie, tunnel and socket flow, and the rejections. Against a deployed hub:

```sh
TAMALE_HUB_URL=wss://tamale-hub.com TAMALE_HUB_KEY=thk_… cargo test -p tamale real_hub -- --ignored
```

## Layout

Starting up and the terminal:

- `src/main.rs` – startup, terminal setup, event loop and one-shot output
- `src/cli.rs` – command-line parsing and overrides
- `src/config.rs`, `config/default.toml` – typed settings, defaults, file discovery and merging
- `src/screen.rs` – where the TUI is shown: this terminal, or the one attached to a background
  session
- `src/background.rs`, `src/background/` – sessions that outlive their terminal: the server
  that owns one, and the client that attaches to it
- `src/workspace.rs` – the working directory and the directories added to it
- `src/session.rs` – saving, listing and loading sessions
- `src/secure.rs` – keeping tamale's secrets from the programs it starts, and its files private
- `src/clipboard.rs` – system clipboard access
- `src/update.rs` – `tamale update`

The app:

- `src/app.rs` – conversation state and agent event handling
- `src/app/{input,commands,sessions,view,help}.rs` – key/clipboard input, commands and pickers,
  session actions, rendering and help content
- `src/app/{markdown,transcript,activity}.rs` – assistant markdown as styled lines, the cached
  transcript, and the indicator of what a run is doing
- `src/app/{approval,question,rewind}.rs` – the approval and `ask_user` popups, `/undo` and the
  rewind picker
- `src/app/{backends,dir_browser,goal}.rs` – `/backend` and `/login`, `/add-dir`'s browser, and
  `/goal`
- `src/app/diff.rs` – the `/diff` viewer: file tree, diff rendering and navigation
- `src/app/prompts.rs` – prompt history for ↑/↓ recall
- `src/app/{background,remote}.rs` – `/sessions`: the other sessions on this machine, and
  driving one on another machine
- `src/syntax.rs` – the syntaxes code is highlighted with, in the terminal and the web UI

The agent:

- `src/agent.rs` – agent turns, tool execution and compaction
- `src/agent/{request,reasoning,prompt,models}.rs` – streaming/retries, reasoning settings,
  instruction loading and model metadata/cost estimates
- `src/backend.rs` – backend selection (API, Claude Code, Codex or Antigravity) and dispatch
- `src/backend/claude_code.rs` – running `claude -p`, its sign-in and stream translation
- `src/backend/codex.rs` – running `codex exec`, its sign-in, event translation and usage
- `src/backend/antigravity.rs` – running `agy` with stream-json and a generated agent, its
  sign-in (checked over ACP), event translation and models
- `src/backend/cli.rs` – what those three share: running a CLI for a turn, signing in and out,
  and replaying a conversation it doesn't hold
- `src/backend/compat.rs` – providers that speak OpenAI's Chat Completions API
- `src/backend/logins.rs` – the Claude Code, Codex and Antigravity logins offered in the picker
- `src/backend/mcp.rs` – the local MCP server that offers tamale's tools to Claude Code, Codex
  and Antigravity
- `src/backend/subagent.rs` – the `agent` tool: running a subagent on the current backend
- `src/tools.rs` – tool definitions and implementations
- `src/tools/search.rs`, `src/tools/web.rs` – built-in repository search and web tools
- `src/tools/shell.rs` – running `bash` commands in a process group
- `src/tools/ask.rs` – the `ask_user` tool
- `src/permissions.rs` – permission modes and allow/deny rule matching
- `src/checkpoint.rs` – file snapshots for `/undo` and `/rewind`
- `src/diff.rs` – running and parsing `git diff` for the diff viewer
- `src/chef.rs`, `src/chef/hub.rs`, `src/app/chef.rs` – the tamale chef: its tools and prompt,
  reaching sessions through the hub, its kitchen panel and every session's side of a side
  question

In a browser:

- `src/browser.rs`, `src/browser/` – `/browser`: the local server, the page, the backend
  that records changed cells, and input translation
- `browser/protocol` – the screen cells and input tamale exchanges with its browser client,
  and with a terminal attached to a background session
- `browser/client` – the browser client (Rust, compiled to WebAssembly by `build.rs`)
- `src/web.rs`, `src/web/` – `/web`: the server and its requests, the page's HTML and CSS,
- `web/` – the `/web` app (Rust and Dioxus, compiled to WebAssembly by `build.rs`, with
  pullspace, the code view, drawn by it from the `pullspace` crate; with
  `--features desktop`, the desktop app that lists your sessions and opens them, bundled by
  `web/bundle-desktop.sh`)
- `src/web/{mirror,render,artifacts}.rs` – what each tab was last sent, markdown and code as
  HTML, and the artifacts a reply ends in
- `src/web/{pullspace,diff}.rs` – reads of the workspace's files, for the code view, and of its
  changes
- `src/web/wire.rs` – the `/web` messages tamale reads as well as writes (status, approval,
  entries), shared with `src/remote.rs`
- `src/web/fixtures.rs` – keeps the samples the iPhone app is tested against current
- `src/app/web.rs` – what the web UI shows of the session, and its input back to the app

Remote access:

- `src/web/relay.rs`, `src/web/e2e.rs` – `/remote`: the session's connection to a tamale-hub,
  and the end-to-end encryption over it
- `src/remote.rs` – driving a session on another machine through the hub
- `hub/` – tamale-hub: `src/main.rs` is its command line, `src/state.rs` its keys,
  `src/serve.rs` and `src/serve/` the relay, and `static/` its page
- `hub/protocol` – the frames tamale and tamale-hub exchange, and what's derived from a key
- `apple/` – the iPhone app (see [`apple/README.md`](../apple/README.md))

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
