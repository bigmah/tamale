# Sessions and undo

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
