# tamale

A small Claude Code–style coding agent for the terminal, written in pure Rust with
[rig](https://rig.rs) and [ratatui](https://ratatui.rs). Inference comes from the Anthropic
API or from your Claude Code or Codex sign-in (see [Backends](#backends)).

## Install

```sh
cargo install --path .
```

This builds a release binary and installs it as `~/.cargo/bin/tamale`, so `tamale` works from
any directory; the directory you launch it in becomes the workspace. The installed copy doesn't
track the repo: after changing the code, run `cargo install --path .` again to update it.
`cargo uninstall tamale` removes it.

To skip reinstalling, symlink the build output instead, and every `cargo build --release` updates
the `tamale` command (a broken build then breaks it too):

```sh
ln -sf "$PWD/target/release/tamale" ~/.cargo/bin/tamale
```

During development, `cargo run --release -- <args>` works in place of `tamale <args>`.

## Usage

```sh
export ANTHROPIC_API_KEY=sk-ant-...
tamale                       # interactive TUI
tamale --backend claude-code # use your Claude Code sign-in instead of an API key
tamale --backend codex       # use your Codex (ChatGPT) sign-in
tamale --model claude-opus-5-5
tamale --effort low          # less thorough, faster, cheaper
tamale --thinking off        # disable adaptive thinking
tamale -p "summarize src/"   # one-shot, non-interactive
tamale -p "fix the tests" --yes  # one-shot, approve calls that would prompt
tamale --continue            # resume the latest session in this directory
tamale --resume              # pick a saved session to resume
tamale --config dev.toml     # add settings from a TOML file
tamale --add-dir ../shared --add-dir ~/dev/api
```

## Backends

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

### Codex

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

## Configuration

Defaults live in [`config/default.toml`](config/default.toml) and are embedded in the
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
max_tokens = 16000
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

## Sessions

Every interactive conversation is saved as JSON in `~/.local/share/tamale/sessions/`
(`$XDG_DATA_HOME/tamale/sessions`, or `TAMALE_SESSIONS_DIR`) after each prompt, tool step
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
- Additional directories are saved with the session and restored on resume. Directories
  passed with `--add-dir` also apply to resumed sessions. A directory that no longer
  exists is skipped with an error message.

Session writes run on a background worker, coalesce pending updates to each session,
and drain before exit. Resume sees pending updates immediately. A disposable `.index/`
inside the sessions directory caches listing metadata; older sessions are indexed on
first listing, and changed files or missing/corrupt indexes are reparsed automatically.

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

## Tools

| Tool | Purpose |
| --- | --- |
| `read_file`, `list_dir` | Read numbered lines and list a directory |
| `write_file`, `edit_file` | Create files or replace exact text |
| `grep` | Search contents with a regex or literal, file glob filter, case folding, and context lines |
| `glob` | Find files by pattern, e.g. `**/*.rs` or `src/*.{rs,toml}` |
| `web_search` | Search the web and return titles, URLs, and snippets |
| `web_fetch` | Read a URL as numbered text, with extracted links and line pagination |
| `bash` | Run builds, tests, git, and other shell commands |
| `agent` | Hand a self-contained task to a subagent and get back its report |

`grep` and `glob` are built into the binary; no `rg` installation is needed. They
respect `.gitignore`, `.ignore`, and global Git ignores, skip hidden files, and
don't follow directory symlinks. Set `include_hidden: true` to include hidden files,
or `respect_ignore: false` to search ignored files; `.git` is always excluded.
Patterns with `/` are relative to `path` (default `.`); patterns without `/` match
basenames at any depth. `grep` supports `output_mode: "content"` (default),
`"files_with_matches"`, or `"count"` (matching lines per file). Its regex syntax is
[Rust regex](https://docs.rs/regex/latest/regex/#syntax), without lookaround or
backreferences; `fixed_strings: true` searches literal text. It skips binary files
and files over 2 MiB. Both tools default to 200 results (maximum 2000), bound output
and search time, and report when results are limited or files cannot be read.

Web search uses DuckDuckGo's HTML results without an API key. DuckDuckGo may rate
limit or return a bot challenge; this produces an explicit tool error. For the
[Brave Search API](https://api-dashboard.search.brave.com/documentation/quickstart),
set a key before starting the agent:

```sh
export BRAVE_SEARCH_API_KEY=...
```

When set, the key selects Brave; provider errors are returned directly. Search
defaults to 5 results (maximum 20). `web_fetch` supports HTTP(S) HTML, plain text,
JSON, and XML, follows up to 5 redirects, and returns 200 lines by default (maximum
2000); use `offset` and `limit` to read more. Web requests time out after 30 seconds
and downloads are capped at 2 MiB. It doesn't execute JavaScript, authenticate to
sites, or extract PDFs. Search queries go to the selected provider and fetched URLs
go to their destination servers. Web content is marked as untrusted reference data,
and the agent is instructed to cite source URLs.

### Subagents

The `agent` tool starts a subagent: a fresh agent on the current backend (API, Claude
Code or Codex) and model, with the same system prompt and tools, but none of the
conversation. The model writes it a standalone task; the subagent works until done and
only its final report comes back as the tool result, so the files and output it went
through don't fill the main context. The model is told to use it for broad searches,
investigations across many files, or well-described side tasks, not for a few calls it
could make itself.

- Its steps appear in the chat as they happen, marked `↳`; its text and history stay
  out of the conversation.
- It shares the turn's permission mode, rules and approval prompts, and its file
  changes are part of the turn's checkpoint, so `/undo` reverts them too. Starting one
  needs no approval itself; `deny = ["agent"]` turns subagents off.
- Subagents can't start subagents, and they run one at a time. Their usage counts in
  the session totals but not in the context meter. Esc stops them with the turn.

Every tool call carries a short model-written `description` ("Find where tool calls are
rendered"). The chat shows that as the step's label, with the raw call dimmed after it, so a
run of tool calls reads as a sequence of steps. Long runs are collapsed to the last 5 steps.
`/verbose` shows every step, the full call and its output.

`bash` runs `sh -c` in its own process group with stdout and stderr on one pipe, so their
lines stay in order. A timeout (`timeout_secs`, default 120) stops the command and every
process it started (SIGTERM, then SIGKILL after 2 seconds) and returns the output printed so
far; interrupting the turn with `Esc` kills the group too. Background processes a finished
command leaves running (`server &`) keep running; their later output isn't captured.

Output over 30,000 characters keeps its first third and last two thirds (where compiler
errors and test summaries usually are), with a marker in between. The full output is
saved to `~/.cache/tamale/out/` (`$XDG_CACHE_HOME/tamale/out`) and the marker gives its
path, so the agent can `read_file` or `grep` it. Saved outputs older than 7 days are deleted.

## Permissions

The permission mode decides which tool calls wait for your approval:

| Mode | Asks before |
| --- | --- |
| `ask` | `write_file`, `edit_file` and `bash` |
| `accept-edits` | `bash` |
| `auto` (default) | nothing |

Reading, searching and web tools never ask. `Shift+Tab` cycles the modes, the status bar
shows the current one, and `/permissions [mode]` shows the rules or sets the mode.

The approval popup offers **yes**, **yes, and don't ask again this session** for a
suggested rule (e.g. `bash:cargo test*` or `edit_file:src/*`), and **no**, after which
you can type what to tell the model instead. `Esc` denies the call and interrupts the
agent. Keys pressed in the first 300 ms are ignored, so typing can't answer by accident.

Rules in `[permissions]` are `tool` or `tool:glob`. The glob matches the command for
`bash`, the path for file tools (as given and relative to the working directory) and the
URL or query for web tools; `*` also matches `/`.

- **Deny** rules are checked first and apply in every mode, including `auto` and `--yes`.
  For `bash` they match the whole command and each part of a compound command; for file
  tools they also match the file name, so `write_file:.env*` blocks `config/.env.local`.
- **Allow** rules skip the prompt. Every part of a compound command (`&&`, `||`, `;`,
  `|`, `&`, newlines) must be allowed. Commands with substitution (`$(…)`, backticks) or
  file redirection never match an allow rule (`2>&1` and `/dev/null` are fine), and
  paths containing `..` never match one. A bare `bash` rule allows every command.

In `-p` mode nothing can be approved interactively: calls that would prompt are denied
and the model is told why, unless `--yes` is passed.

Rules are a guard against mistakes, not a sandbox: an allowed command like `cargo test`
still runs arbitrary build scripts. A project's `tamale.toml` can also change the mode
and add allow rules, so review it in repositories you don't trust.

## Checkpoints and undo

Before the agent's first write to a file in a turn, the file's original contents (or
the fact that it didn't exist) are saved under the session's directory,
`sessions/<id>/checkpoints/<turn>/`.

- `/undo` restores the files changed in the last turn, removes that turn from the
  conversation, and puts its prompt back in the input box.
- `/rewind` opens a picker of turns (most recent first, with the number of files and
  commands in each); `Enter` returns to before the selected turn, restoring files from
  it and every later turn.
- Shell commands can't be reverted. Their effects stay, and the undo message says how
  many commands ran (e.g. "2 files restored; 1 shell command not reverted").
- After compaction, earlier turns can still restore files, but the conversation isn't
  rewound because those messages have been summarized.
- Checkpoints are saved with the session, so `/undo` works after `/resume`. `-p` runs
  aren't saved and have no checkpoints.

## Keys

- `Enter` send · `Alt/Shift+Enter` or `Ctrl+J` newline
- `↑/↓` move between lines of the input; on the first/last line they recall earlier
  prompts (past the newest, your unsent draft comes back). Prompts are remembered per
  working directory across sessions in `sessions/.prompts.jsonl`.
- **Typing while the agent works**: `Enter` queues the message (shown above the input as
  `⧗ queued`). It's delivered to the model with the next tool result, on every backend,
  and appears in the chat at that point; `/undo` puts it back in the input box with the
  turn's prompt. Messages still queued when the turn ends are sent as the next prompt.
  Slash commands still run immediately.
- `→` with an empty prompt switches to the Diff tab (see [Diff viewer](#diff-viewer))
- `Esc` interrupt the agent; queued messages that weren't delivered return to the input box
- `Shift+Tab` cycle the permission mode (see [Permissions](#permissions))
- `PgUp/PgDn`, `Shift+↑/↓`, mouse wheel scroll (a scrollbar and "↓ N more lines" badge show while scrolled up)
- Mouse wheel scrolling is **on by default**, alongside copy/paste. Hold your
  terminal's mouse override modifier while dragging to select text (usually Shift;
  Option in iTerm2), then use its copy shortcut (`Cmd+C` on macOS, usually
  `Ctrl+Shift+C` on Linux). `/copy` copies the latest reply without selecting it.
- Paste with your terminal's shortcut (`Cmd+V` on macOS, usually `Ctrl+Shift+V` on Linux),
  `Ctrl+V`, `Shift+Insert`, or `/paste`. Multi-line text is inserted without sending;
  press Enter when ready.
- `/copy` copies the latest assistant reply, preserving Markdown and code. `/copy all`
  copies all user and assistant messages. `Ctrl+Shift+C` also copies the latest reply
  if your terminal forwards that shortcut to the app.
- `/mouse [on|off]` toggles mouse capture for wheel scrolling. `/mouse off` allows
  selection without a modifier; `/mouse on` restores wheel scrolling. Clipboard
  commands and bracketed paste work in either mode.
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
- `/goal <condition>` keeps the agent working until the condition holds, e.g.
  `/goal cargo test passes with no warnings`. After each turn a separate checker (a
  subagent on the same backend and model) verifies the condition by reading files and
  running commands, and answers `[goal met]` or `[goal not met]`. Its steps show in the
  transcript (marked `↳`), and its tool calls follow your permission mode and rules, with
  `write_file` and `edit_file` always denied. Until it confirms, the agent gets a
  continuation prompt with the checker's findings, up to 20 turns. The agent can end a turn
  with `[goal blocked]` when it needs you. Interrupts (Esc), errors (the agent's or the
  checker's), `[goal blocked]` and the turn limit pause the goal; `/goal resume` continues
  it, `/goal clear` drops it, and `/goal` alone shows it. The goal is shown above the input
  while it is set. Messages you queue during a goal are sent before the next check.
  `/clear` and `/resume` drop the goal.
- `/backend` opens a backend picker; `/backend api|claude-code|codex` switches directly, keeping the conversation
- `/login` signs in to Claude Code or Codex (with their backends; see [Backends](#backends))
- `/add-dir [path]` adds a directory to the agent's scope, or lists the current scope
- `/resume [n|id]` continues a saved session (see [Sessions](#sessions))
- `/undo` and `/rewind` restore files and rewind the conversation (see [Checkpoints](#checkpoints-and-undo))
- `/diff [args]` opens the diff viewer (see [Diff viewer](#diff-viewer))
- `/browser [off]` serves tamale to a browser tab (see [Browser](#browser))
- `/permissions [mode]` shows the mode and rules, or sets `ask`, `accept-edits` or `auto`
- `/clear` resets the conversation, `/quit` (or `/exit`) quits

Direct clipboard commands use `pbcopy`/`pbpaste` on macOS, PowerShell on Windows,
or `wl-copy`/`wl-paste`, `xclip`, or `xsel` on Linux (install one of those on Linux).
Your terminal's native copy/paste shortcuts also work without these utilities, including over SSH.

## Diff viewer

The chat and the diff viewer are two tabs. The right end of the footer shows
`Chat  Diff 3 +40 −12 →` (changed files and lines, kept up to date in the background);
press `→` with an empty prompt to switch to the Diff tab, and `Esc`, `q` or `←` to come
back. The viewer keeps its place between visits. `/diff` also opens it.

The Diff tab is a full-screen view of your uncommitted changes: a file tree of changed
files on the left (status letter and `+added −removed` per file, directories folded
into single rows like `src/app/`) and the selected file's diff on the right, with old
and new line numbers, tinted added/removed lines and the changed part of edited lines
highlighted.

- `/diff` compares the working tree (staged and unstaged) with `HEAD` and includes
  untracked files; before the first commit it compares with an empty tree.
- `/diff <args>` passes the arguments to `git diff` instead, e.g. `/diff --staged`,
  `/diff main` or `/diff main...feature` (untracked files aren't included).
- The view refreshes after each tool result and when a turn ends, so it can stay
  open while the agent works; `r` refreshes by hand.

Keys: `Tab` switches between the file tree and the diff. In the tree, `↑/↓` (`j/k`)
select a file, `←/→` (`h/l`) fold or unfold directories and `Enter` opens the diff. In
the diff, `j/k` (`↑/↓`) move a line cursor (the view follows), `←/→` pan long lines
(`←` at the left edge returns to the tree) and `g/G` jump to the top or bottom.
Anywhere: `PgUp/PgDn`/`Space`/`Ctrl+U/D` page through the diff, `n`/`p` jump between
hunks (past the last hunk, `n` moves to the next file), `]`/`[` move between files, and
`q` or `Esc` returns to the chat. `←` also leads back: from the diff to the tree, up
through parent directories, and from the top of the tree to the chat. The mouse wheel
scrolls whichever pane it's over, and clicking a tree row selects it.

**Unchanged lines** more than 3 lines from a change fold into `↕ 25 unchanged lines`
rows. With the cursor on one (marked `⏎ expand`), `Enter` (or `e`) reveals 20 more lines
next to the changes; small gaps open fully. Clicking a row expands it too. `E` shows the
whole file, and pressing it again goes back to just the changes. Expanded lines stay
open across refreshes.

**Asking about code**: in the diff, `v` or `V` starts a vim-style visual line
selection that `j/k` (and `PgUp/PgDn`, `g/G`, `n/p`) extend. `Enter` (or `a`) returns to
the chat with the selected lines quoted in the prompt, as the file path, line numbers
and a `diff` block, ready for your question; nothing is sent until you press `Enter`
again. `y` copies the quote to the clipboard instead, and `Esc` cancels the selection.

## Browser

`/browser` lets you use the session from a browser tab as well. Everything works there
as it does in the terminal, because the browser shows the same screen: tamale serves a
page that runs [xterm.js](https://xtermjs.org) and sends it the output it would write to
a terminal. tamale prints the address and opens it in your default browser.

The tab and the terminal take turns. Opening the tab moves the session to it, and the
terminal shows a notice; pressing a key in the terminal moves the session back, and a key
or click in the tab moves it there again. One tab is connected at a time: opening the
address in another tab disconnects the first. `/browser` again shows the address, and
`/browser off` stops serving (closing tamale does too).

- Cmd shortcuts stay with the browser (Cmd+C copies a selection and Cmd+V pastes into
  the prompt); other keys go to tamale. Browsers keep a few Ctrl shortcuts, like Ctrl+W,
  for themselves. The mouse wheel scrolls and clicks work as in the terminal; drag to
  select text.
- `/login` still signs in through the terminal tamale runs in.
- `/copy` and `/paste` use the clipboard of the machine tamale runs on.

The server only listens on 127.0.0.1, and each address has a random token. Requests
without it are refused, as are pages from other sites (checked with the `Origin` and
`Host` headers), so only you can drive the session. Treat the address like a password:
anyone with it and access to your machine can run the agent.

## Layout

- `src/main.rs` – startup, terminal setup, event loop and one-shot output
- `src/browser.rs`, `src/browser/` – `/browser`: the local server, page and key translation
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
- `src/app.rs` – conversation state and agent event handling
- `src/app/{input,commands,sessions,view,help}.rs` – key/clipboard input, commands and pickers,
  session actions, rendering and help content
- `src/app/{approval,rewind}.rs` – the approval popup, `/undo` and the rewind picker
- `src/app/diff.rs` – the `/diff` viewer: file tree, diff rendering and navigation
- `src/app/prompts.rs` – prompt history for ↑/↓ recall
- `src/clipboard.rs` – system clipboard access
- `src/session.rs` – saving, listing and loading sessions
