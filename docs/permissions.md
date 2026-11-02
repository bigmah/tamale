# Permissions

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
