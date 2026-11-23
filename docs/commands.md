# Keys and commands

- `Enter` send · `Alt/Shift+Enter` or `Ctrl+J` newline
- `↑/↓` move between lines of the input; on the first/last line they recall earlier
  prompts (past the newest, your unsent draft comes back). Prompts are remembered per
  working directory across sessions in `sessions/.prompts.jsonl`.
- **Typing while the agent works**: `Enter` queues the message (shown above the input as
  `⧗ queued`). It's delivered to the model with the next tool result, on every backend,
  and appears in the chat at that point; `/undo` puts it back in the input box with the
  turn's prompt. Messages still queued when the turn ends are sent as the next prompt.
  Slash commands still run immediately.
- `↓` past the end of the prompt focuses the footer's tabs: **Sessions**, **Remote**, **Web** and **Diff**. `←/→` picks one, `Enter` opens it, and `↑` or `Esc` goes back. Clicking one works too
- `Esc` interrupt the agent; queued messages that weren't delivered return to the input box
- `Shift+Tab` cycle the permission mode (see [Permissions](permissions.md))
- `PgUp/PgDn`, `Shift+↑/↓`, mouse wheel scroll (a scrollbar and "↓ N more lines" badge show while scrolled up)
- Mouse wheel scrolling is **on by default**, alongside copy/paste. Hold your
  terminal's mouse override modifier while dragging to select text (usually Shift;
  Option in iTerm2), then use its copy shortcut (`Cmd+C` on macOS, usually
  `Ctrl+Shift+C` on Linux). `/copy` copies the latest reply without selecting it.
- Paste with your terminal's shortcut (`Cmd+V` on macOS, usually `Ctrl+Shift+V` on Linux),
  `Ctrl+V`, `Shift+Insert`, or `/paste`. Multi-line text is inserted without sending;
  press Enter when ready. A big paste (10+ lines or 1000+ characters) shows as
  `[Pasted text #1 +42 lines]` and is sent in full; Backspace removes it whole.
- `/copy` copies the latest assistant reply, preserving Markdown and code. `/copy all`
  copies all user and assistant messages. `Ctrl+Shift+C` also copies the latest reply
  if your terminal forwards that shortcut to the app.
- `/mouse [on|off]` toggles mouse capture for wheel scrolling. `/mouse off` allows
  selection without a modifier; `/mouse on` restores wheel scrolling. Clipboard
  commands and bracketed paste work in either mode.
- `Ctrl+C` during a turn detaches and leaves it running (see
  [Background sessions](sessions.md#background-sessions)); otherwise clears the input, then quits.
  With `--foreground` it interrupts a turn instead
- `/detach` leaves the session running in the background, whether a turn is running or not
- `/verbose [on|off]` toggles verbose output. Off by default: each step shows as one line
  (its description) with no output, and only the last 5 steps of a run are shown. Verbose shows
  every step with the full call and an output preview.
- `/help` opens a scrollable popup listing all commands, keys and tools
- `/model` opens a model picker (the backend's live list: the Anthropic API's, Codex's, Antigravity's or a
  provider's; type to filter, Enter to switch), in the terminal and in the web UI alike.
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
  subagent on the same backend and model) verifies the condition requirement by
  requirement, by reading files and running commands rather than trusting the agent's
  report, and answers `[goal met]` or `[goal not met]`. Its steps show in the transcript
  (marked `↳`), and its tool calls follow your permission mode and rules, with
  `write_file` and `edit_file` always denied. Until it confirms, the agent gets a
  continuation prompt with the checker's findings. Write conditions the checker can
  verify: one measurable end state (a test or build that passes, a file that exists), and
  any constraints that matter ("without changing the public API"). Conditions are limited
  to 4,000 characters; put longer specs in a file and point the goal at it.
  - The goal pauses when the agent ends a turn with `[goal blocked]` (it needs you), on
    interrupts (Esc), after 3 turns in a row without a tool call (it looks stuck), after
    20 turns without success, and when the checker gives no verdict. Failed turns and
    checks are retried after 1, 5 and 15 minutes before the goal pauses.
  - A paused goal continues when you send a message (the message is the next turn, with
    the goal recalled for the agent) or with `/goal resume`. `/goal pause` holds it until
    `/goal resume`, so you can talk about other things in between.
  - `/goal` alone shows the goal, its turns, time, tokens and the checker's last findings.
    `/goal clear` (or `stop`, `off`, `cancel`) drops it; setting a new goal replaces it.
  - The goal is shown above the input (and in the web UI) with its state, turn count and
    time. Messages you type during a goal reach the agent mid-turn, or go along with the
    next continuation when typed while the checker runs. The checker is shown them too,
    and counts their requests as part of the goal.
  - The goal is saved with the session: `/resume` (and `tamale --continue`) brings it back
    paused. `/clear` drops it.
- `/backend` opens a backend picker; `/backend api|claude-code|codex|antigravity|<provider>` (e.g. `deepseek`) switches directly, keeping the conversation
- `/login` signs in to Claude Code, Codex or Antigravity (with their backends; see [Backends](backends.md))
- `/add-dir [path]` adds a directory to the agent's scope; without a path it opens a file tree
  browser to pick one, and `/add-dir list` lists the current scope
- `/chef` goes to the tamale chef, which looks after all your sessions (see [The chef](chef.md)),
  starting it if it's off; `/chef off` stops it wherever it runs. The footer's **Chef** tab says
  **on** or **off**: `Enter` goes to it, and `x` on it stops it
- `/resume [n|id]` continues a saved session (see [Sessions](sessions.md#sessions))
- `/undo` and `/rewind` restore files and rewind the conversation (see [Checkpoints](sessions.md#checkpoints-and-undo))
- `/diff [args]` opens the diff viewer (see [Diff viewer](diff-viewer.md))
- `/browser [off]` serves tamale to a browser tab (see [Browser](web.md#browser))
- `/web [off]` opens a chat-first web UI with pullspace for the code (see [Web UI](web.md#web-ui))
- `/remote [off]` makes the session reachable through a tamale-hub (see [Remote access](remote-access.md))
- `/permissions [mode]` shows the mode and rules, or sets `ask`, `accept-edits` or `auto`
- `/clear` resets the conversation, `/quit` (or `/exit`) quits. In the web UI, `/quit` and the
  ⏻ button in the top bar close the session, after asking

Direct clipboard commands use `pbcopy`/`pbpaste` on macOS, PowerShell on Windows,
or `wl-copy`/`wl-paste`, `xclip`, or `xsel` on Linux (install one of those on Linux).
Your terminal's native copy/paste shortcuts also work without these utilities, including over SSH.
