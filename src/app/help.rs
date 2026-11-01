use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};

const COMMANDS: &[(&str, &str)] = &[
    (
        "/help",
        "Show this help (scroll with ↑/↓, close with Esc or q)",
    ),
    ("/model", "Pick a model from a list (type to filter)"),
    (
        "/add-dir [path]",
        "Add a directory to the agent's scope; omit the path to list scoped directories",
    ),
    (
        "/model <id>",
        "Switch directly to a model, e.g. /model claude-opus-5-5",
    ),
    (
        "/goal <condition>",
        "Keep working, turn after turn, until the condition holds (/goal shows it; clear, resume)",
    ),
    (
        "/compact [focus]",
        "Summarize the conversation to free up context (optional focus instructions)",
    ),
    (
        "/effort",
        "Pick an effort level from a list (↑/↓ to choose)",
    ),
    (
        "/effort <level>",
        "Set effort directly: low, medium, high, xhigh, max, or default",
    ),
    (
        "/thinking [mode]",
        "Toggle adaptive thinking (on by default); /thinking on|off to set",
    ),
    (
        "/verbose [on|off]",
        "Toggle verbose output (full tool calls and their output; off by default)",
    ),
    (
        "/mouse [on|off]",
        "Toggle mouse wheel scrolling (on by default; hold Shift/Option to select text)",
    ),
    (
        "/copy [all]",
        "Copy the latest reply, or all user/assistant messages, to the clipboard",
    ),
    (
        "/paste",
        "Insert clipboard text into the prompt without sending",
    ),
    (
        "/resume [n|id]",
        "Pick a saved session from this directory to continue (or resume the nth / by id)",
    ),
    (
        "/undo",
        "Restore files the last turn changed and remove it from the conversation",
    ),
    (
        "/rewind",
        "Pick a turn to return to; restores files from it and every later turn",
    ),
    (
        "/diff [args]",
        "Open the Diff tab (or press → with an empty prompt); args go to git diff, e.g. /diff main",
    ),
    (
        "/permissions [mode]",
        "Show the permission mode and rules, or set ask, accept-edits or auto",
    ),
    (
        "/backend [kind]",
        "Pick where inference comes from, incl. each Claude Code or Codex login or adding one (or /backend api|claude-code|codex)",
    ),
    (
        "/login",
        "Sign in to Claude Code or Codex (their backends; the footer shows where inference comes from)",
    ),
    (
        "/browser [off]",
        "Use tamale from a browser tab too (keys in either switch to it); /browser off stops it",
    ),
    (
        "/web [local|off]",
        "Open a chat-first web UI on your Wi-Fi with a code browser (/code there); /web local keeps it to this machine, /web off stops it (ctrl+o or the Web tab opens it too)",
    ),
    (
        "/artifacts",
        "List this session's saved HTML artifacts (in /web: browse every session's)",
    ),
    ("/clear", "Clear the conversation and start fresh"),
    ("/quit, /exit", "Quit tamale"),
];

const KEYS: &[(&str, &[(&str, &str)])] = &[
    (
        "Prompt",
        &[
            ("Enter", "Send message (while the agent runs: queue it for the next step)"),
            ("Alt+Enter / Shift+Enter", "Insert newline"),
            ("↑ / ↓", "Move between lines; at the top/bottom, recall earlier prompts"),
            ("Ctrl+J", "Insert newline"),
            ("←/→, Home/End", "Move cursor"),
            ("Ctrl+A / Ctrl+E", "Start / end of line"),
            ("Ctrl+U", "Delete to start of line"),
            ("Ctrl+W, Alt+Backspace", "Delete previous word"),
            ("Backspace / Delete", "Delete character"),
            (
                "Ctrl+V / Shift+Insert",
                "Paste clipboard text without sending",
            ),
            ("Terminal paste (Cmd+V)", "Pastes multi-line text as-is"),
        ],
    ),
    (
        "Conversation",
        &[
            ("PgUp / PgDn", "Scroll chat by 10 lines"),
            ("Shift+↑ / Shift+↓", "Scroll chat by 1 line"),
            (
                "Mouse wheel",
                "Scroll chat by 3 lines (also scrolls help and pickers)",
            ),
            (
                "↓ (below the prompt)",
                "Focus the tabs: ←/→ pick Web (the web UI, like /web) or Diff, Enter/↓ opens it; ↑ or Esc back",
            ),
            ("Ctrl+O / click Web", "Open the web UI on your Wi-Fi, like /web"),
            ("v / V (in a diff)", "Select lines; Enter quotes them into the prompt, y copies"),
            ("Esc", "Interrupt the agent (unsent queued messages return to the input)"),
            (
                "Shift+Tab",
                "Cycle permission mode: ask → accept-edits → auto",
            ),
            ("Ctrl+C", "Clear input / interrupt agent / quit"),
            ("Ctrl+D", "Quit (when input is empty)"),
            (
                "Select + terminal copy (Cmd+C)",
                "Hold Shift/Option while dragging to select, then copy normally",
            ),
            ("Ctrl+Shift+C", "Copy the latest assistant reply"),
        ],
    ),
    (
        "Model picker",
        &[
            ("Type", "Filter models by id or name"),
            ("↑/↓, Ctrl+P/Ctrl+N", "Move selection"),
            ("Enter", "Switch to selected model"),
            ("Esc", "Cancel"),
        ],
    ),
    (
        "Resume picker",
        &[
            ("Type", "Filter sessions by title"),
            ("↑/↓, Ctrl+P/Ctrl+N", "Move selection"),
            ("Enter", "Resume selected session"),
            ("Esc", "Cancel"),
        ],
    ),
    (
        "Approval prompt",
        &[
            ("y / Enter on Yes", "Run the tool call"),
            ("a", "Run it and allow the shown rule for this session"),
            (
                "n",
                "Deny it, then type what to tell the model (Enter sends)",
            ),
            ("Esc", "Deny and interrupt the agent"),
        ],
    ),
    (
        "Rewind picker",
        &[
            ("↑/↓, j/k", "Move selection (most recent turn first)"),
            ("Enter", "Rewind to before the selected turn"),
            ("Esc / q", "Cancel"),
        ],
    ),
    (
        "Effort picker",
        &[
            ("↑/↓, j/k, Ctrl+P/Ctrl+N", "Move selection"),
            ("1–6", "Jump to a level"),
            ("Enter", "Apply selected level"),
            ("Esc / q", "Cancel"),
        ],
    ),
    (
        "Help",
        &[
            ("↑/↓ or j/k", "Scroll by 1 line"),
            ("PgUp / PgDn, Space", "Scroll by a page"),
            ("Home/End or g/G", "Jump to top / bottom"),
            ("Esc / q", "Close help"),
        ],
    ),
];

const TOOLS: &[(&str, &str)] = &[
    ("read_file", "Read a file"),
    ("list_dir", "List a directory"),
    ("write_file", "Create/overwrite a file"),
    ("edit_file", "Replace exact text in a file"),
    ("grep", "Search file contents (regex or literal)"),
    ("glob", "Find files by glob pattern"),
    ("web_search", "Search the web (DuckDuckGo or Brave)"),
    ("web_fetch", "Read a web page as text"),
    ("bash", "Run a shell command"),
];

pub(super) fn help_lines() -> Vec<Line<'static>> {
    fn heading(lines: &mut Vec<Line<'static>>, title: &'static str) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(title.bold().fg(Color::Magenta)));
    }
    fn rows(lines: &mut Vec<Line<'static>>, items: &[(&'static str, &'static str)], color: Color) {
        let width = items
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0);
        for (key, desc) in items {
            lines.push(Line::from(vec![
                Span::from(format!("  {key:<width$}  ")).fg(color),
                Span::from(*desc),
            ]));
        }
    }

    let mut lines = Vec::new();
    heading(&mut lines, "Commands");
    rows(&mut lines, COMMANDS, Color::Cyan);
    for (section, items) in KEYS {
        heading(&mut lines, section);
        rows(&mut lines, items, Color::Yellow);
    }
    heading(&mut lines, "Tools the agent can use");
    rows(&mut lines, TOOLS, Color::Green);
    heading(&mut lines, "Options");
    rows(
        &mut lines,
        &[
            (
                "--add-dir <path>",
                "Include another directory in the agent's scope (repeatable)",
            ),
            (
                "--config <path>",
                "Additional TOML settings (after user and project files)",
            ),
            (
                "--model <id>",
                "Anthropic model to use (or set TAMALE_MODEL)",
            ),
            (
                "--effort <level>",
                "Effort level: low|medium|high|xhigh|max (or set TAMALE_EFFORT)",
            ),
            (
                "--thinking <mode>",
                "Thinking: adaptive|off (default: adaptive; or set TAMALE_THINKING)",
            ),
            (
                "-c, --continue",
                "Resume the most recent session in this directory",
            ),
            ("-r, --resume", "Pick a saved session to resume on startup"),
            ("-p <prompt>", "Run one prompt non-interactively and exit"),
            (
                "-y, --yes",
                "With -p, run calls that would need approval (deny rules still apply)",
            ),
        ],
        Color::Cyan,
    );
    lines
}
