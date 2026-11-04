# Sessions and undo

## Sessions

Every interactive conversation is saved as JSON in `~/.local/share/tamale/sessions/`
(`$XDG_DATA_HOME/tamale/sessions`, or `TAMALE_SESSIONS_DIR`) after each prompt, tool step
and on exit. It includes the full model history (so the agent picks up with all context) and
the visible transcript. Only you can read that directory, since whatever secrets went through
a conversation are in it.

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

## Background sessions

An interactive session runs in a background process of its own, and the terminal is only a
window onto it. So work carries on when the terminal goes away, even if you close it.

- `Ctrl+C` during a turn detaches: tamale exits and the agent keeps going. `Esc` still
  interrupts. `Ctrl+C` while idle clears the input, then ends the session as it always has.
  `/detach` leaves at any time.
- `tamale` always starts a new session, and mentions any open ones in the current directory that
  no terminal is showing. `tamale --continue` goes straight to the latest one, or if there isn't
  one, resumes the latest saved session.
- `tamale ls` lists open sessions and what they're doing. `tamale attach [<id>]` goes back to one
  and `tamale kill <id>` ends one (it stays saved, so `--continue` can resume it).
- `/sessions`, or the **Sessions** tab in the footer, lists the other open sessions inside the TUI.
  `Enter` joins one, leaving the current session running (a session with nothing in it just closes),
  and `x` stops one: twice if it's mid-turn. It stays saved either way. With a hub key set, it also
  lists your sessions on other machines, which `x` stops too; see [Remote access](remote-access.md#driving-a-session-from-a-terminal).
- One terminal shows a session at a time; attaching from another takes it over.
- An approval prompt waits while you're away, and `tamale ls` shows `needs approval`. With
  `[permissions] mode = "auto"`, nothing waits.
- A session with nothing running and no terminal or web tab open exits after 24 hours. It stays
  saved.
- `/resume` refuses a session that's still open elsewhere, since two processes would overwrite
  each other's saves.
- Sessions end when the machine restarts. The latest save survives, and `--continue` resumes
  from there.
- `--foreground` keeps the session in the terminal's own process: nothing keeps running after it
  exits, and `Ctrl+C` interrupts a turn instead of detaching.

Each one is `tamale --server`, with files in `.servers/` under the sessions directory: `<id>.sock`,
a socket only you can reach; `<id>.json`, what `tamale ls` shows; and `<id>.log`, its stderr, kept
if it dies.

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
