# tamale

A small Claude Code–style coding agent for the terminal, written in pure Rust with
[rig](https://rig.rs) and [ratatui](https://ratatui.rs). Inference comes from the Anthropic
API or from your Claude Code or Codex sign-in.

## Setup

You need [Rust](https://rustup.rs). Then install tamale and pick where inference comes from:

```sh
git clone https://github.com/bigmah/tamale.git && cd tamale
cargo install --path . --locked

export ANTHROPIC_API_KEY=sk-ant-...  # then run: tamale
tamale --backend claude-code         # or use your Claude Code sign-in
tamale --backend codex               # or your Codex (ChatGPT) sign-in
```

The directory you launch tamale in becomes its workspace. The sign-in backends need the
[Claude Code](https://docs.claude.com/en/docs/claude-code) or
[Codex](https://github.com/openai/codex) CLI installed.

**tamale's default permission mode is `auto`, which runs tools without asking.** Press
`Shift+Tab` or set [`mode = "ask"`](docs/permissions.md) if you want to approve them.

### Use it from your phone

[tamale-hub](docs/remote-access.md) makes a session reachable from a phone or another computer,
with no port forwarding or VPN:

1. Make a key at [tamale-hub.com](https://tamale-hub.com). It's made in your browser.
2. Run `export TAMALE_HUB_KEY=thk_…` on the machine, then `/remote` in tamale.
3. Scan the key's QR code on your phone, and tap a session to drive it.

**Anyone with the key can run commands on your machines**, so treat it like an SSH key.

### Host your own hub

You need a server with a public IP, a domain whose DNS has an API, and Caddy for TLS. The
[self-hosting guide](docs/self-hosting.md) has every step; in short:

1. Point `hub.example` and `*.hub.example` at the server.
2. Run Caddy with a DNS plugin, for the wildcard certificate, proxying to `127.0.0.1:8787`.
3. Build the hub and run it under systemd:

   ```sh
   cargo build -p tamale-hub --release --locked
   tamale-hub serve --public-url https://hub.example --data-dir /var/lib/tamale-hub
   ```

4. Make a key, and point tamale at your hub with `export TAMALE_HUB_URL=wss://hub.example`:

   ```sh
   tamale-hub key new mine --public-url https://hub.example --data-dir /var/lib/tamale-hub
   ```

## Usage

```sh
tamale                           # interactive TUI
tamale -p "summarize src/"       # one-shot, non-interactive
tamale -p "fix the tests" --yes  # one-shot, approve calls that would prompt
tamale --continue                # resume the latest session in this directory
tamale --resume                  # pick a saved session to resume
tamale --model claude-opus-5-5
tamale --effort low              # less thorough, faster, cheaper
tamale --config dev.toml         # add settings from a TOML file
tamale --add-dir ../shared       # add another directory to the agent's scope
```

In the TUI, `/help` lists every command and key. `Esc` interrupts the agent, `/undo` reverts
the last turn, `/diff` reviews your changes and `/web` opens a web UI.

## Docs

| | |
| --- | --- |
| [Backends](docs/backends.md) | The Anthropic API, Claude Code and Codex, and several logins |
| [Configuration](docs/configuration.md) | Settings files, `AGENTS.md`, extra directories, context and retries |
| [Permissions](docs/permissions.md) | Permission modes and allow/deny rules |
| [Tools](docs/tools.md) | The agent's tools, web search and subagents |
| [Sessions and undo](docs/sessions.md) | Saving, resuming, `/undo` and `/rewind` |
| [Keys and commands](docs/commands.md) | Every key and slash command |
| [Diff viewer](docs/diff-viewer.md) | Reviewing changes and asking about code |
| [Browser and web UI](docs/web.md) | `/browser`, `/web` and artifacts |
| [Remote access](docs/remote-access.md) | Using tamale-hub, and what its encryption covers |
| [Self-hosting](docs/self-hosting.md) | Running your own tamale-hub |
| [Development](docs/development.md) | Building, testing, benchmarks and the code layout |
