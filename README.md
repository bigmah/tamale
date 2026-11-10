# tamale 🫔

**A coding agent you can drive from your terminal, browser or phone.**

tamale is a full agent harness: tools, permission prompts, undo, sessions and compaction
are built in. Inference comes from whichever backend you choose: an Anthropic API key, or
your existing [Claude Code](https://docs.claude.com/en/docs/claude-code) or
[Codex](https://github.com/openai/codex) sign-in.

Start a session in the terminal, then pick it up from the web or the iOS app through an
end-to-end encrypted relay. Longer answers arrive as HTML artifacts on web and iOS, and
every UI has a diff viewer for reviewing what changed.

Written entirely in Rust with [rig](https://rig.rs) and [ratatui](https://ratatui.rs)
(outside of the iOS app, of course).

## Install

macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/bigmah/tamale/releases/latest/download/install.sh | sh
```

Or build from source with [Rust](https://rustup.rs):

```sh
git clone https://github.com/bigmah/tamale.git && cd tamale
cargo install --path . --locked
```

Then just run it. It's a single CLI with no arguments needed:

```sh
tamale
```

## Remote usage

[tamale-hub](docs/remote-access.md) is a small relay that makes your sessions reachable from a
phone or another computer, with no port forwarding or VPN. Sessions are end-to-end encrypted;
the hub only moves opaque bytes.

1. **Get a key** at [tamale-hub.com](https://tamale-hub.com). It's made in your browser, and the
   hub never sees it.
2. **Set it** on each machine that runs tamale, then run `/remote` in tamale:

   ```sh
   export TAMALE_HUB_KEY=thk_…
   ```

3. **Scan** the key's QR code on your phone (web or the [iOS app](docs/ios.md)) and tap a
   session to view or drive it.

> [!WARNING]
> Anyone with the key can run commands on your machines. Treat it like an SSH key.

Want your own hub? See the [self-hosting guide](docs/self-hosting.md).

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
| [The chef](docs/chef.md) | One agent that knows what all your sessions are doing |
| [Self-hosting](docs/self-hosting.md) | Running your own tamale-hub |
| [iOS app](docs/ios.md) | The iPhone app |
| [Development](docs/development.md) | Building, testing, benchmarks and the code layout |
