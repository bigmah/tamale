# Diff viewer

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
