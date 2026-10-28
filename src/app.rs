use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use rig_core::completion::message::UserContent;
use rig_core::completion::{CompletionModel, Message};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use unicode_width::UnicodeWidthChar;

use crate::agent::{self, AgentEvent, Approval};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TOOL_PREVIEW_LINES: usize = 6;

const COMMANDS: &[(&str, &str)] = &[
    (
        "/help",
        "Show this help (scroll with ↑/↓, close with Esc or q)",
    ),
    ("/clear", "Clear the conversation and start fresh"),
    ("/quit, /exit", "Quit harness"),
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
            ("Paste", "Pastes multi-line text as-is"),
        ],
    ),
    (
        "Conversation",
        &[
            ("PgUp / PgDn", "Scroll chat by 10 lines"),
            ("Shift+↑ / Shift+↓", "Scroll chat by 1 line"),
            ("Esc", "Interrupt the agent"),
            ("Ctrl+C", "Clear input / interrupt agent / quit"),
            ("Ctrl+D", "Quit (when input is empty)"),
        ],
    ),
    (
        "Tool approval",
        &[
            ("y / Enter", "Allow this call once"),
            ("a", "Always allow this tool for the session"),
            ("n / Esc", "Deny this call"),
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
    ("read_file", "Read a file (no approval)"),
    ("list_dir", "List a directory (no approval)"),
    ("write_file", "Create/overwrite a file (asks first)"),
    ("edit_file", "Replace exact text in a file (asks first)"),
    ("bash", "Run a shell command (asks first)"),
];

enum Entry {
    User(String),
    Assistant(String),
    Tool {
        name: String,
        summary: String,
        output: Option<(String, bool)>,
    },
    Info(String),
    Error(String),
}

struct PendingApproval {
    name: String,
    summary: String,
    reply: oneshot::Sender<Approval>,
}

pub type TurnEvent = (u64, AgentEvent);

pub struct App<M> {
    model: M,
    model_name: String,
    tx: mpsc::UnboundedSender<TurnEvent>,
    history: Vec<Message>,
    always_allowed: Arc<Mutex<HashSet<String>>>,
    entries: Vec<Entry>,
    input: String,
    cursor: usize,
    scroll_back: usize,
    turn: u64,
    task: Option<JoinHandle<()>>,
    approval: Option<PendingApproval>,
    /// Scroll offset of the help popup; `Some` while it is open.
    help_scroll: Option<usize>,
    help_page: usize,
    input_tokens: u64,
    output_tokens: u64,
    tick: usize,
    pub should_quit: bool,
}

impl<M> App<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
{
    pub fn new_with_channel(
        model: M,
        model_name: String,
    ) -> (Self, mpsc::UnboundedReceiver<TurnEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self::new(model, model_name, tx), rx)
    }

    fn new(model: M, model_name: String, tx: mpsc::UnboundedSender<TurnEvent>) -> Self {
        Self {
            model,
            model_name,
            tx,
            history: Vec::new(),
            always_allowed: Arc::default(),
            entries: vec![Entry::Info(
                "Welcome! Type a request and press Enter. /help for commands.".into(),
            )],
            input: String::new(),
            cursor: 0,
            scroll_back: 0,
            turn: 0,
            task: None,
            approval: None,
            help_scroll: None,
            help_page: 10,
            input_tokens: 0,
            output_tokens: 0,
            tick: 0,
            should_quit: false,
        }
    }

    pub fn is_running(&self) -> bool {
        self.task.is_some()
    }

    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    // ---------------------------------------------------------------- input

    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
            Event::Paste(text) => self.insert_str(&text.replace("\r\n", "\n").replace('\r', "\n")),
            _ => {}
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if let Some(scroll) = self.help_scroll {
            let page = self.help_page.max(1);
            self.help_scroll = match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => None,
                KeyCode::Char('c') if ctrl => None,
                KeyCode::Up | KeyCode::Char('k') => Some(scroll.saturating_sub(1)),
                KeyCode::Down | KeyCode::Char('j') => Some(scroll + 1),
                KeyCode::PageUp => Some(scroll.saturating_sub(page)),
                KeyCode::PageDown | KeyCode::Char(' ') => Some(scroll + page),
                KeyCode::Home | KeyCode::Char('g') => Some(0),
                // Clamped to the real maximum when drawn.
                KeyCode::End | KeyCode::Char('G') => Some(usize::MAX),
                _ => Some(scroll),
            };
            return;
        }

        if let Some(pending) = self.approval.take() {
            let choice = match key.code {
                KeyCode::Char('y') | KeyCode::Enter => Some(Approval::Once),
                KeyCode::Char('a') => Some(Approval::Always),
                KeyCode::Char('n') | KeyCode::Esc => Some(Approval::Deny),
                KeyCode::Char('c') if ctrl => {
                    drop(pending);
                    self.interrupt();
                    return;
                }
                _ => None,
            };
            match choice {
                Some(c) => {
                    let _ = pending.reply.send(c);
                }
                None => self.approval = Some(pending),
            }
            return;
        }

        match key.code {
            KeyCode::Char('c') if ctrl => {
                if self.is_running() {
                    self.interrupt();
                } else if !self.input.is_empty() {
                    self.input.clear();
                    self.cursor = 0;
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => self.should_quit = true,
            KeyCode::Esc if self.is_running() => self.interrupt(),
            KeyCode::Enter if alt || shift => self.insert_str("\n"),
            KeyCode::Char('j') if ctrl => self.insert_str("\n"),
            KeyCode::Enter => self.submit(),
            KeyCode::Char('a') if ctrl => self.cursor = self.line_start(),
            KeyCode::Char('e') if ctrl => self.cursor = self.line_end(),
            KeyCode::Char('u') if ctrl => {
                let start = self.line_start();
                self.input.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Backspace if alt || ctrl => self.delete_word(),
            KeyCode::Char(c) if !ctrl => self.insert_str(&c.to_string()),
            // Many terminals send ^H (Ctrl+H) for Backspace instead of DEL.
            KeyCode::Backspace | KeyCode::Char('h') => {
                if let Some(prev) = self.prev_boundary() {
                    self.input.drain(prev..self.cursor);
                    self.cursor = prev;
                }
            }
            KeyCode::Delete => {
                if let Some(next) = self.next_boundary() {
                    self.input.drain(self.cursor..next);
                }
            }
            KeyCode::Left => self.cursor = self.prev_boundary().unwrap_or(self.cursor),
            KeyCode::Right => self.cursor = self.next_boundary().unwrap_or(self.cursor),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::PageUp => self.scroll_back += 10,
            KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(10),
            KeyCode::Up if shift => self.scroll_back += 1,
            KeyCode::Down if shift => self.scroll_back = self.scroll_back.saturating_sub(1),
            _ => {}
        }
    }

    fn insert_str(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.input[..self.cursor]
            .char_indices()
            .last()
            .map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.input[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }

    fn line_start(&self) -> usize {
        self.input[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.input[self.cursor..]
            .find('\n')
            .map_or(self.input.len(), |i| self.cursor + i)
    }

    fn delete_word(&mut self) {
        let before = &self.input[..self.cursor];
        let trimmed = before.trim_end();
        let start = trimmed
            .rfind(char::is_whitespace)
            .map_or(0, |i| i + trimmed[i..].chars().next().unwrap().len_utf8());
        self.input.drain(start..self.cursor);
        self.cursor = start;
    }

    // --------------------------------------------------------------- agent

    fn submit(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() || (self.is_running() && !text.starts_with('/')) {
            return;
        }
        self.input.clear();
        self.cursor = 0;
        self.scroll_back = 0;

        if text.starts_with('/') {
            self.command(&text);
            return;
        }

        self.entries.push(Entry::User(text.clone()));
        // Consecutive user turns (e.g. after an interrupt) are merged into one message.
        match self.history.last_mut() {
            Some(Message::User { content }) => content.push(UserContent::text(text)),
            _ => self.history.push(Message::user(text)),
        }

        self.turn += 1;
        let turn = self.turn;
        let (atx, mut arx) = mpsc::unbounded_channel();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = arx.recv().await {
                if tx.send((turn, ev)).is_err() {
                    break;
                }
            }
        });
        self.task = Some(tokio::spawn(agent::run(
            self.model.clone(),
            self.history.clone(),
            self.always_allowed.clone(),
            atx,
        )));
    }

    fn command(&mut self, cmd: &str) {
        match cmd.split_whitespace().next().unwrap_or("") {
            "/quit" | "/exit" => self.should_quit = true,
            "/clear" if self.is_running() => self.entries.push(Entry::Error(
                "Can't clear while the agent is running (Esc to interrupt first).".into(),
            )),
            "/clear" => {
                self.history.clear();
                self.entries.clear();
                self.entries
                    .push(Entry::Info("Conversation cleared.".into()));
            }
            "/help" => self.help_scroll = Some(0),
            other => self.entries.push(Entry::Error(format!(
                "Unknown command: {other} (type /help to see all commands)"
            ))),
        }
    }

    fn interrupt(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            self.approval = None;
            self.entries.push(Entry::Info("Interrupted.".into()));
        }
    }

    pub fn on_agent(&mut self, (turn, event): TurnEvent) {
        if turn != self.turn {
            return;
        }
        if !self.is_running() {
            // Turn was interrupted; only keep consistent history snapshots.
            if let AgentEvent::History(h) = event {
                self.history = h;
            }
            return;
        }
        match event {
            AgentEvent::TextDelta(delta) => match self.entries.last_mut() {
                Some(Entry::Assistant(text)) => text.push_str(&delta),
                _ => self.entries.push(Entry::Assistant(delta)),
            },
            AgentEvent::ToolCall { name, summary } => self.entries.push(Entry::Tool {
                name,
                summary,
                output: None,
            }),
            AgentEvent::ToolResult { output, is_error } => {
                if let Some(Entry::Tool { output: slot, .. }) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, Entry::Tool { output: None, .. }))
                {
                    *slot = Some((output, is_error));
                }
            }
            AgentEvent::ApprovalRequest {
                name,
                summary,
                reply,
            } => {
                self.approval = Some(PendingApproval {
                    name,
                    summary,
                    reply,
                })
            }
            AgentEvent::History(h) => self.history = h,
            AgentEvent::Usage(u) => {
                self.input_tokens += u.input_tokens;
                self.output_tokens += u.output_tokens;
            }
            AgentEvent::Error(e) => self.entries.push(Entry::Error(e)),
            AgentEvent::Done => {
                self.task = None;
                self.approval = None;
            }
        }
    }

    // -------------------------------------------------------------- render

    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let input_width = area.width.saturating_sub(4).max(1) as usize;
        let (rows, cursor) = wrap_input(&self.input, self.cursor, input_width);
        let input_height = if self.approval.is_some() {
            4
        } else {
            (rows.len() as u16).clamp(1, 10) + 2
        };

        let [chat_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .areas(area);

        self.draw_chat(frame, chat_area);
        if let Some(pending) = &self.approval {
            draw_approval(frame, input_area, pending);
        } else {
            self.draw_input(frame, input_area, rows, cursor);
        }
        self.draw_status(frame, status_area);
        if self.help_scroll.is_some() {
            self.draw_help(frame, chat_area);
        }
    }

    fn draw_help(&mut self, frame: &mut Frame, area: Rect) {
        let popup = area.centered(Constraint::Percentage(80), Constraint::Percentage(90));
        let lines = help_lines();
        let inner_height = popup.height.saturating_sub(2) as usize;
        let max_scroll = lines.len().saturating_sub(inner_height);
        let scroll = self.help_scroll.unwrap_or(0).min(max_scroll);
        self.help_scroll = Some(scroll);
        self.help_page = inner_height.saturating_sub(1);

        let position = if max_scroll == 0 {
            "all".to_string()
        } else {
            format!("{}%", scroll * 100 / max_scroll)
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Help ".bold())
            .title_bottom(
                Line::from(format!(" ↑/↓ scroll · esc close · {position} ")).right_aligned(),
            );

        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .scroll((scroll as u16, 0)),
            popup,
        );
        if max_scroll > 0 {
            let mut state = ScrollbarState::new(max_scroll).position(scroll);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                popup.inner(Margin::new(0, 1)),
                &mut state,
            );
        }
    }

    fn draw_chat(&mut self, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        for entry in &self.entries {
            render_entry(entry, &mut lines);
            lines.push(Line::default());
        }
        if self.is_running() && self.approval.is_none() {
            lines.push(Line::from(
                format!("{} Thinking…", SPINNER[self.tick % SPINNER.len()]).fg(Color::Yellow),
            ));
        }

        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        let total = para.line_count(area.width);
        let max_scroll = total.saturating_sub(area.height as usize);
        self.scroll_back = self.scroll_back.min(max_scroll);
        let offset = (max_scroll - self.scroll_back).min(u16::MAX as usize) as u16;
        frame.render_widget(para.scroll((offset, 0)), area);
    }

    fn draw_input(&self, frame: &mut Frame, area: Rect, rows: Vec<String>, cursor: (usize, usize)) {
        let visible = area.height.saturating_sub(2) as usize;
        let skip = (cursor.0 + 1).saturating_sub(visible);
        let lines: Vec<Line> = rows
            .into_iter()
            .enumerate()
            .skip(skip)
            .map(|(i, row)| {
                let prefix = if i == 0 { "› " } else { "  " };
                Line::from(vec![Span::from(prefix).fg(Color::Cyan), Span::from(row)])
            })
            .collect();
        let border = if self.is_running() {
            Color::DarkGray
        } else {
            Color::Cyan
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border));
        frame.render_widget(Paragraph::new(lines).block(block), area);
        frame.set_cursor_position(Position::new(
            area.x + 3 + cursor.1 as u16,
            area.y + 1 + (cursor.0 - skip) as u16,
        ));
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let hint = if self.help_scroll.is_some() {
            "↑/↓ j/k scroll · pgup/pgdn page · esc close"
        } else if self.approval.is_some() {
            "y allow · a always · n deny"
        } else if self.is_running() {
            "esc to interrupt"
        } else {
            "enter send · alt+enter newline · /help"
        };
        let line = Line::from(vec![
            Span::from(format!(" {} ", self.model_name)).fg(Color::Magenta),
            Span::from(format!(
                "· {}↑ {}↓ tokens ",
                self.input_tokens, self.output_tokens
            ))
            .fg(Color::DarkGray),
            Span::from(format!("· {hint}")).fg(Color::DarkGray),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }
}

fn help_lines() -> Vec<Line<'static>> {
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
                "--model <id>",
                "Anthropic model to use (or set HARNESS_MODEL)",
            ),
            ("-p <prompt>", "Run one prompt non-interactively and exit"),
        ],
        Color::Cyan,
    );
    lines
}

fn render_entry<'a>(entry: &'a Entry, lines: &mut Vec<Line<'a>>) {
    match entry {
        Entry::User(text) => {
            for (i, l) in text.lines().enumerate() {
                let prefix = if i == 0 { "› " } else { "  " };
                lines.push(Line::from(vec![
                    Span::from(prefix).fg(Color::Cyan).bold(),
                    Span::from(l).bold(),
                ]));
            }
        }
        Entry::Assistant(text) => render_markdown(text, lines),
        Entry::Tool {
            name,
            summary,
            output,
        } => {
            let dot_color = match output {
                None => Color::Yellow,
                Some((_, true)) => Color::Red,
                Some((_, false)) => Color::Green,
            };
            let summary = summary.lines().next().unwrap_or("");
            lines.push(Line::from(vec![
                Span::from("⏺ ").fg(dot_color),
                Span::from(name.as_str()).bold(),
                Span::from(format!("({summary})")).fg(Color::Gray),
            ]));
            if let Some((out, is_error)) = output {
                let style = if *is_error {
                    Style::new().fg(Color::Red)
                } else {
                    Style::new().fg(Color::DarkGray)
                };
                let all: Vec<&str> = out.trim_end().lines().collect();
                for (i, l) in all.iter().take(TOOL_PREVIEW_LINES).enumerate() {
                    let prefix = if i == 0 { "  ⎿ " } else { "    " };
                    lines.push(Line::from(vec![
                        Span::from(prefix).fg(Color::DarkGray),
                        Span::styled(*l, style),
                    ]));
                }
                if all.len() > TOOL_PREVIEW_LINES {
                    lines.push(Line::from(
                        format!("    … +{} lines", all.len() - TOOL_PREVIEW_LINES)
                            .fg(Color::DarkGray),
                    ));
                }
            }
        }
        Entry::Info(text) => {
            for l in text.lines() {
                lines.push(Line::from(l.fg(Color::DarkGray).italic()));
            }
        }
        Entry::Error(text) => {
            for l in text.lines() {
                lines.push(Line::from(l.fg(Color::Red)));
            }
        }
    }
}

/// Very small markdown styling: code fences, headings, bullets and `inline code`.
fn render_markdown<'a>(text: &'a str, lines: &mut Vec<Line<'a>>) {
    let mut in_code = false;
    for l in text.lines() {
        if l.trim_start().starts_with("```") {
            in_code = !in_code;
            lines.push(Line::from(l.fg(Color::DarkGray)));
            continue;
        }
        if in_code {
            lines.push(Line::from(
                Span::from(format!("  {l}")).fg(Color::LightYellow),
            ));
            continue;
        }
        if let Some(h) = l.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim_start();
            lines.push(Line::from(h.bold().add_modifier(Modifier::UNDERLINED)));
            continue;
        }
        let mut spans = Vec::new();
        for (i, part) in l.split('`').enumerate() {
            if i % 2 == 1 {
                spans.push(part.fg(Color::LightYellow));
            } else {
                for (j, p) in part.split("**").enumerate() {
                    spans.push(if j % 2 == 1 { p.bold() } else { Span::from(p) });
                }
            }
        }
        lines.push(Line::from(spans));
    }
}

fn draw_approval(frame: &mut Frame, area: Rect, pending: &PendingApproval) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Yellow))
        .title(format!(" Allow {}? ", pending.name).bold());
    let summary = pending.summary.replace('\n', " ⏎ ");
    let lines = vec![
        Line::from(summary.fg(Color::White)),
        Line::from(vec![
            "[y]".fg(Color::Green).bold(),
            " yes   ".into(),
            "[a]".fg(Color::Cyan).bold(),
            format!(" always allow {}   ", pending.name).into(),
            "[n]".fg(Color::Red).bold(),
            " no".into(),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Wraps input text to `width` columns, returning visual rows and the
/// (row, col) of the byte offset `cursor`.
fn wrap_input(input: &str, cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let mut rows = vec![String::new()];
    let mut col = 0;
    let mut pos = None;
    for (i, ch) in input.char_indices() {
        if ch == '\n' {
            if i == cursor {
                pos = Some((rows.len() - 1, col));
            }
            rows.push(String::new());
            col = 0;
            continue;
        }
        let w = ch.width().unwrap_or(0);
        if col + w > width {
            rows.push(String::new());
            col = 0;
        }
        if i == cursor {
            pos = Some((rows.len() - 1, col));
        }
        rows.last_mut().unwrap().push(ch);
        col += w;
    }
    let pos = pos.unwrap_or_else(|| {
        if col >= width {
            rows.push(String::new());
            (rows.len() - 1, 0)
        } else {
            (rows.len() - 1, col)
        }
    });
    (rows, pos)
}
