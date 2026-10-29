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
    ("/clear", "Clear the conversation and start fresh"),
    ("/quit, /exit", "Quit tamale"),
];

const KEYS: &[(&str, &[(&str, &str)])] = &[
    (
        "Prompt",
        &[
            ("Enter", "Send message"),
            ("Alt+Enter / Shift+Enter", "Insert newline"),
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
            ("Esc", "Interrupt the agent"),
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
        ],
        Color::Cyan,
    );
    lines
}
