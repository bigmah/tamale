# Backends

The backend decides where inference comes from; everything else is the same. Tools,
permission prompts, checkpoints and `/undo`, sessions, compaction and usage totals work
identically, and the footer shows the backend next to the working directory
(`cwd: ~/dev/app · via Claude Code · Max`).

| Backend | Selected with | Authenticates with |
| --- | --- | --- |
| `api` (default) | `kind = "api"` | `ANTHROPIC_API_KEY` |
| `claude-code` | `kind = "claude-code"`, `--backend claude-code`, `TAMALE_BACKEND` | The Claude Code CLI's sign-in (Claude subscription or Console account) |
| `codex` | `kind = "codex"`, `--backend codex`, `TAMALE_BACKEND` | The Codex CLI's sign-in (ChatGPT plan or OpenAI API key) |

`/backend` opens a picker to switch mid-session (↑/↓ or j/k to move, 1–9 to jump, Enter to
switch, Esc to cancel; the current backend is marked with ●). `/backend api`,
`/backend claude-code` and `/backend codex` (your default login of each CLI) switch
directly. Codex logins work like Claude Code logins below (see [Codex](#codex)).

**Several Claude Code logins.** Each signed-in Claude Code account is its own row in the
picker, e.g. `Claude Code · Max · me@home.dev` and `Claude Code · Team · me@work.dev`.
With more than one, the footer names the account too. The picker lists:

- your default login (Claude Code's default, or `CLAUDE_CONFIG_DIR` if tamale was started
  with it),
- `~/.claude`, when that isn't the default and is signed in,
- config directories listed in `backend.claude_config_dirs` that are signed in,
- accounts added with **Sign in to another Claude Code account…**: tamale creates a config
  directory under `~/.local/share/tamale/claude-code/` (`$XDG_DATA_HOME`), runs
  `claude auth login` for it and switches to it. Signing in to an account that's already
  listed switches to that one instead. `d` (or Delete) removes an added account: it
  signs it out and deletes its directory. The current backend can't be removed.

Claude Code keeps each login in its own config directory, so tamale runs each one with
its `CLAUDE_CONFIG_DIR`; sessions and settings of different logins stay separate.
The conversation carries over in both directions: Claude Code turns are recorded in the
same history the API backend uses, and a switch to Claude Code replays the conversation
to a new Claude Code session. Switching to Claude Code while signed out starts the sign-in;
switching to the API needs `ANTHROPIC_API_KEY`. Resumed sessions keep the current backend.

The `claude-code` backend runs `claude -p` headlessly for each turn, so it needs the
[Claude Code CLI](https://docs.claude.com/en/docs/claude-code) installed (set
`backend.claude_path` if `claude` isn't on `PATH`).

- **Signing in**: at startup tamale checks `claude auth status`. If you're signed out, it
  runs `claude auth login` in the terminal before opening the TUI (`-p` runs exit with
  instructions instead). `/login` signs in again or switches accounts from inside the TUI.
  `ANTHROPIC_API_KEY` is removed from Claude Code's environment so the sign-in is used;
  use the `api` backend for API keys.
- **Tools**: Claude Code's built-in tools, settings files, hooks, skills, plugins' MCP
  servers and CLAUDE.md loading are turned off. tamale passes its own system prompt and
  serves its own tools to Claude Code over a local MCP server (127.0.0.1, random URL
  token, one per turn). Every call therefore goes through tamale's permission rules,
  approval popup and checkpoints.
- **Conversations**: each tamale session keeps a Claude Code session and continues it
  with `--resume`. After `/undo`, `/rewind`, `/resume`, `/clear`, compaction or a backend
  switch a fresh
  Claude Code session starts, and the earlier conversation is replayed to it as a
  transcript (long tool results shortened). These sessions also appear in Claude Code's
  own `/resume` list.
- **Settings**: `--model`/`/model`, `--effort`/`/effort` and `agent.max_tokens` are passed
  through; `/thinking off` sets `MAX_THINKING_TOKENS=0`. Claude Code handles its own
  retries and in-run context management; tamale still auto-compacts between turns.
  The model picker shows the built-in model list (Claude Code has no model listing).

## Codex

The `codex` backend runs `codex exec --json` headlessly for each turn, so it needs the
[Codex CLI](https://github.com/openai/codex) installed (set `backend.codex_path` if
`codex` isn't on `PATH`). It works like the `claude-code` backend:

- **Signing in**: tamale checks `codex login status` and runs `codex login` when you're
  signed out (at startup, on `/login`, or when switching to it). Each Codex login lives in
  its own `CODEX_HOME`; the picker lists your default login, `~/.codex`, the homes in
  `backend.codex_homes`, and accounts added with **Sign in to another Codex account…**
  (kept under `~/.local/share/tamale/codex/`). The footer shows the plan, e.g.
  `via Codex · Plus`.
- **Tools**: Codex runs with `--ignore-user-config` and `--ignore-rules`, a read-only
  sandbox, no AGENTS.md, skills, web search, shell or other built-in tools it allows
  turning off, and tamale's system prompt in place of Codex's instructions. tamale's
  tools are served over the same local MCP server, pre-approved on Codex's side, so
  every call goes through tamale's own permission rules, approval popup and checkpoints.
  Codex's own subagents are turned off through a patched copy of its model catalog
  (`model_catalog_json`, written to the temp directory), so models use tamale's `agent`
  tool instead. Codex's `apply_patch` remains listed, blocked by the read-only sandbox.
- **Conversations**: each tamale session keeps a Codex thread and continues it with
  `codex exec resume`; after anything that rewrites the history a new thread starts with
  the conversation replayed, as with Claude Code.
- **Models**: `/model` lists Codex's model catalog (`codex debug models`). While
  `model.name` is a Claude model (the default), Codex uses its own default model and the
  footer says `codex default`. `--effort`/`/effort` sets `model_reasoning_effort`;
  `/thinking` and `agent.max_tokens` don't apply.
- **Context and usage**: Codex reports usage per turn; tamale reads the last request's
  size from Codex's session file for the context meter. Codex compacts its own thread,
  so tamale doesn't auto-compact before Codex turns (`/compact` still works). Costs
  aren't estimated for OpenAI models.
