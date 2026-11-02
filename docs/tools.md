# Tools

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

## Subagents

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
