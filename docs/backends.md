# Backends

The backend decides where inference comes from; everything else is the same. Tools,
permission prompts, checkpoints and `/undo`, sessions, compaction and usage totals work
identically, and the footer shows the backend next to the working directory
(`cwd: ~/dev/app · via Claude Code · Max`).

| Backend | Selected with | Authenticates with |
| --- | --- | --- |
| `api` (default) | `kind = "api"`, `--backend api`, `TAMALE_BACKEND` | `ANTHROPIC_API_KEY` |
| `claude-code` | `kind = "claude-code"`, `--backend claude-code`, `TAMALE_BACKEND` | The Claude Code CLI's sign-in (Claude subscription or Console account) |
| `codex` | `kind = "codex"`, `--backend codex`, `TAMALE_BACKEND` | The Codex CLI's sign-in (ChatGPT plan or OpenAI API key) |
| a provider, e.g. `deepseek` | `kind = "deepseek"`, `--backend deepseek`, `TAMALE_BACKEND` | The provider's API key, e.g. `DEEPSEEK_API_KEY` (see [below](#openai-compatible-providers)) |

`/backend` opens a picker to switch mid-session (↑/↓ or j/k to move, 1–9 to jump, Enter to
switch, Esc to cancel; the current backend is marked with ●). `/backend api`,
`/backend claude-code` and `/backend codex` (your default login of each CLI) switch
directly. Codex logins work like Claude Code logins below (see [Codex](#codex)).

If the `api` backend is selected and `ANTHROPIC_API_KEY` isn't set, tamale opens with this
picker so you can choose (`-p` runs exit with an error instead). To skip it next time, set
`kind` in `~/.config/tamale/config.toml` or `TAMALE_BACKEND`.

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
  serves its own tools to Claude Code over a local MCP server (127.0.0.1, one per turn,
  with a random token it's given in its environment). Every call therefore goes through tamale's permission rules,
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

## OpenAI-compatible providers

Most providers serve OpenAI's **Chat Completions** API (`POST {base_url}/chat/completions`),
so one backend covers them all. They run through the same agent loop as the Anthropic API.
These are built in, keyed by the environment variable shown:

| Name | Provider | Key | Default model |
| --- | --- | --- | --- |
| `deepseek` | DeepSeek | `DEEPSEEK_API_KEY` | `deepseek-chat` |
| `kimi` | Kimi (Moonshot) | `MOONSHOT_API_KEY` | `kimi-k2.5` |
| `gemini` | Gemini | `GEMINI_API_KEY` | `gemini-2.5-pro` |
| `openrouter` | OpenRouter | `OPENROUTER_API_KEY` | `moonshotai/kimi-k2` |
| `xai` | xAI | `XAI_API_KEY` | `grok-4` |
| `groq` | Groq | `GROQ_API_KEY` | `openai/gpt-oss-120b` |
| `mistral` | Mistral | `MISTRAL_API_KEY` | `mistral-large-latest` |
| `zai` | Z.ai (GLM) | `ZAI_API_KEY` | `glm-4.6` |
| `openai` | OpenAI API | `OPENAI_API_KEY` | `gpt-5` |
| `ollama` | Ollama on localhost | none | `qwen3-coder` |

Select one with `--backend kimi`, `TAMALE_BACKEND=kimi`, `kind = "kimi"` or `/backend kimi`.
The `/backend` picker lists every provider whose key is set, plus keyless ones like
`ollama`. If the configured provider's key is missing, tamale opens the picker instead
(`-p` runs exit with an error).

- **Models**: while `model.name` is a Claude model, a provider uses its default model, and
  switching to a provider switches to its default (switching back to an Anthropic backend
  returns to the default Claude model). `/model` lists the provider's models from its
  `/models` endpoint; `--model` or `TAMALE_MODEL` pick one at startup.
- **Settings**: `--effort`/`/effort` is sent as `reasoning_effort` (`xhigh` and `max` as
  `high`), and only when set, since not every provider accepts it. `/thinking` doesn't apply.
  `max_tokens` on a provider caps `agent.max_tokens` for APIs with a lower output limit.
- **Context and cost**: context windows are known for the default models' families;
  set `model.context_window` for others (unknown models assume 200k). Costs aren't
  estimated.
- **Switching**: the conversation carries over in both directions. Reasoning from other
  providers is dropped when replaying to the Anthropic API, which only accepts its own.

Override any field of a built-in provider, or add your own, in
`~/.config/tamale/config.toml`:

```toml
[backend.providers.deepseek]
model = "deepseek-reasoner"
max_tokens = 32768

[backend.providers.together]
label = "Together"
base_url = "https://api.together.xyz/v1"
api_key_env = "TOGETHER_API_KEY"
model = "moonshotai/Kimi-K2-Instruct-0905"

[backend.providers.vllm]
base_url = "http://localhost:8000/v1"
api_key_env = ""                   # no key
model = "Qwen/Qwen3-Coder-30B-A3B-Instruct"
```

A project's `tamale.toml` can't set `backend.providers`: a repository must not be able to
send your API keys somewhere else.
