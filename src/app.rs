use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::widgets::{List, ListItem, ListState};
use rig_core::client::{CompletionClient, ModelListingClient};
use rig_core::completion::Message;
use rig_core::providers::anthropic;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::agent::{self, AgentEvent};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TOOL_PREVIEW_LINES: usize = 6;

const COMMANDS: &[(&str, &str)] = &[
    (
        "/help",
        "Show this help (scroll with ↑/↓, close with Esc or q)",
    ),
    ("/model", "Pick a model from a list (type to filter)"),
    (
        "/model <id>",
        "Switch directly to a model, e.g. /model claude-opus-4-8",
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
        "Model picker",
        &[
            ("Type", "Filter models by id or name"),
            ("↑/↓, Ctrl+P/Ctrl+N", "Move selection"),
            ("Enter", "Switch to selected model"),
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
    ("bash", "Run a shell command"),
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

pub enum AppEvent {
    /// An agent event tagged with the turn it belongs to.
    Agent(u64, AgentEvent),
    Models(Result<Vec<ModelInfo>, String>),
}

#[derive(Clone)]
pub struct ModelInfo {
    id: String,
    name: String,
}

/// Used when the live model list can't be fetched.
const FALLBACK_MODELS: &[(&str, &str)] = &[
    (anthropic::completion::CLAUDE_OPUS_4_8, "Claude Opus 4.8"),
    (anthropic::completion::CLAUDE_OPUS_4_7, "Claude Opus 4.7"),
    (anthropic::completion::CLAUDE_OPUS_4_6, "Claude Opus 4.6"),
    (
        anthropic::completion::CLAUDE_SONNET_4_6,
        "Claude Sonnet 4.6",
    ),
    (anthropic::completion::CLAUDE_HAIKU_4_5, "Claude Haiku 4.5"),
];

/// Choices shown in the effort picker, in order. `None` means "API default".
const EFFORT_CHOICES: [(Option<agent::Effort>, &str); 6] = [
    (None, "Let the API decide (omit the setting)"),
    (Some(agent::Effort::Low), "Fastest and cheapest, least thorough"),
    (Some(agent::Effort::Medium), "Balanced speed and thoroughness"),
    (Some(agent::Effort::High), "Thorough"),
    (Some(agent::Effort::XHigh), "Very thorough"),
    (Some(agent::Effort::Max), "Maximum thoroughness, slowest"),
];

fn effort_label(effort: Option<agent::Effort>) -> &'static str {
    effort.map_or("default", |e| e.as_str())
}

#[derive(Default)]
struct ModelPicker {
    filter: String,
    selected: usize,
}

pub struct App {
    client: anthropic::Client,
    model_name: String,
    reasoning: agent::Reasoning,
    tx: mpsc::UnboundedSender<AppEvent>,
    /// `None` until the list has been fetched.
    models: Option<Vec<ModelInfo>>,
    picker: Option<ModelPicker>,
    /// Selected row of the effort picker; `Some` while it is open.
    effort_picker: Option<usize>,
    history: Vec<Message>,
    entries: Vec<Entry>,
    input: String,
    cursor: usize,
    scroll_back: usize,
    turn: u64,
    task: Option<JoinHandle<()>>,
    /// Scroll offset of the help popup; `Some` while it is open.
    help_scroll: Option<usize>,
    help_page: usize,
    /// Size of the context window used by the most recent request.
    context_tokens: u64,
    /// Session totals. `input_tokens` includes cache reads and writes.
    input_tokens: u64,
    cached_tokens: u64,
    output_tokens: u64,
    /// Whether the running task is currently compacting the conversation.
    compacting: bool,
    /// Assistant entry created by the in-flight request, dropped if the request is retried.
    attempt_entry: Option<usize>,
    tick: usize,
    pub should_quit: bool,
}

impl App {
    pub fn new_with_channel(
        client: anthropic::Client,
        model_name: String,
        reasoning: agent::Reasoning,
    ) -> (Self, mpsc::UnboundedReceiver<AppEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self::new(client, model_name, reasoning, tx), rx)
    }

    fn new(
        client: anthropic::Client,
        model_name: String,
        reasoning: agent::Reasoning,
        tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        Self {
            client,
            model_name,
            reasoning,
            tx,
            models: None,
            picker: None,
            effort_picker: None,
            history: Vec::new(),
            entries: vec![Entry::Info(
                "Welcome! Type a request and press Enter. /help for commands.".into(),
            )],
            input: String::new(),
            cursor: 0,
            scroll_back: 0,
            turn: 0,
            task: None,
            help_scroll: None,
            help_page: 10,
            context_tokens: 0,
            input_tokens: 0,
            cached_tokens: 0,
            output_tokens: 0,
            compacting: false,
            attempt_entry: None,
            tick: 0,
            should_quit: false,
        }
    }

    pub fn is_running(&self) -> bool {
        self.task.is_some()
    }

    /// Whether something on screen is animating (spinners).
    pub fn is_busy(&self) -> bool {
        self.is_running() || (self.picker.is_some() && self.models.is_none())
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

        if self.picker.is_some() {
            self.on_picker_key(key, ctrl);
            return;
        }

        if self.effort_picker.is_some() {
            self.on_effort_picker_key(key, ctrl);
            return;
        }

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
        agent::push_user_text(&mut self.history, text);

        let config = agent::Config {
            context_window: agent::context_window(&self.model_name),
            context_tokens: self.context_tokens,
            reasoning: self.reasoning,
        };
        let atx = self.start_turn();
        self.task = Some(tokio::spawn(agent::run(
            self.model(),
            self.history.clone(),
            config,
            atx,
        )));
    }

    fn compact(&mut self, instructions: Option<String>) {
        if self.is_running() {
            self.entries.push(Entry::Error(
                "Can't compact while the agent is running (Esc to interrupt first).".into(),
            ));
            return;
        }
        if self.history.is_empty() {
            self.entries
                .push(Entry::Info("Nothing to compact yet.".into()));
            return;
        }
        let atx = self.start_turn();
        self.task = Some(tokio::spawn(agent::compact(
            self.model(),
            self.history.clone(),
            instructions,
            self.reasoning,
            atx,
        )));
    }

    fn model(&self) -> anthropic::completion::CompletionModel {
        self.client
            .completion_model(self.model_name.clone())
            .with_prompt_caching()
    }

    /// Starts a new turn and returns the sender the agent task should report to.
    fn start_turn(&mut self) -> mpsc::UnboundedSender<AgentEvent> {
        self.turn += 1;
        self.compacting = false;
        self.attempt_entry = None;
        let turn = self.turn;
        let (atx, mut arx) = mpsc::unbounded_channel();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = arx.recv().await {
                if tx.send(AppEvent::Agent(turn, ev)).is_err() {
                    break;
                }
            }
        });
        atx
    }

    // --------------------------------------------------------- model picker

    fn open_picker(&mut self) {
        let mut picker = ModelPicker::default();
        if let Some(models) = &self.models {
            picker.selected = models
                .iter()
                .position(|m| m.id == self.model_name)
                .unwrap_or(0);
        } else {
            let client = self.client.clone();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let result = client
                    .list_models()
                    .await
                    .map(|list| {
                        list.data
                            .into_iter()
                            .map(|m| ModelInfo {
                                name: m.name.unwrap_or_else(|| m.id.clone()),
                                id: m.id,
                            })
                            .collect()
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send(AppEvent::Models(result));
            });
        }
        self.picker = Some(picker);
    }

    fn on_models(&mut self, result: Result<Vec<ModelInfo>, String>) {
        let models = match result {
            Ok(models) if !models.is_empty() => models,
            other => {
                if let Err(e) = other {
                    self.entries.push(Entry::Error(format!(
                        "Couldn't fetch model list ({e}); showing built-in list."
                    )));
                }
                FALLBACK_MODELS
                    .iter()
                    .map(|(id, name)| ModelInfo {
                        id: id.to_string(),
                        name: name.to_string(),
                    })
                    .collect()
            }
        };
        if let Some(picker) = &mut self.picker {
            picker.selected = models
                .iter()
                .position(|m| m.id == self.model_name)
                .unwrap_or(0);
        }
        self.models = Some(models);
    }

    /// Models matching the picker's filter.
    fn filtered_models(&self) -> Vec<&ModelInfo> {
        let filter = self
            .picker
            .as_ref()
            .map(|p| p.filter.to_lowercase())
            .unwrap_or_default();
        self.models
            .iter()
            .flatten()
            .filter(|m| {
                m.id.to_lowercase().contains(&filter) || m.name.to_lowercase().contains(&filter)
            })
            .collect()
    }

    fn on_picker_key(&mut self, key: KeyEvent, ctrl: bool) {
        let count = self.filtered_models().len();
        let Some(picker) = &mut self.picker else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Char('c') if ctrl => self.picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Char('p') if ctrl => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Char('n') if ctrl => {
                picker.selected = (picker.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Home => picker.selected = 0,
            KeyCode::End => picker.selected = count.saturating_sub(1),
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.selected = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                picker.filter.push(c);
                picker.selected = 0;
            }
            KeyCode::Enter => {
                let selected = picker.selected;
                if let Some(id) = self.filtered_models().get(selected).map(|m| m.id.clone()) {
                    self.picker = None;
                    self.set_model(id);
                }
            }
            _ => {}
        }
    }

    fn set_model(&mut self, id: String) {
        if id == self.model_name {
            return;
        }
        let note = if self.is_running() {
            " (takes effect on your next message)"
        } else {
            ""
        };
        self.entries
            .push(Entry::Info(format!("Switched model to {id}{note}.")));
        self.model_name = id;
    }

    // -------------------------------------------------------- effort picker

    fn open_effort_picker(&mut self) {
        let selected = EFFORT_CHOICES
            .iter()
            .position(|(e, _)| *e == self.reasoning.effort)
            .unwrap_or(0);
        self.effort_picker = Some(selected);
    }

    fn on_effort_picker_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(selected) = self.effort_picker else {
            return;
        };
        let last = EFFORT_CHOICES.len() - 1;
        self.effort_picker = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => None,
            KeyCode::Char('c') if ctrl => None,
            KeyCode::Up | KeyCode::Char('k') => Some(selected.saturating_sub(1)),
            KeyCode::Char('p') if ctrl => Some(selected.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => Some((selected + 1).min(last)),
            KeyCode::Char('n') if ctrl => Some((selected + 1).min(last)),
            KeyCode::Home | KeyCode::Char('g') => Some(0),
            KeyCode::End | KeyCode::Char('G') => Some(last),
            KeyCode::Char(c @ '1'..='9') => {
                let i = c as usize - '1' as usize;
                if i <= last {
                    self.effort_picker = None;
                    self.apply_effort(EFFORT_CHOICES[i].0);
                    return;
                }
                Some(selected)
            }
            KeyCode::Enter => {
                self.effort_picker = None;
                self.apply_effort(EFFORT_CHOICES[selected].0);
                return;
            }
            _ => Some(selected),
        };
    }

    fn apply_effort(&mut self, effort: Option<agent::Effort>) {
        if effort == self.reasoning.effort {
            return;
        }
        let note = if self.is_running() {
            " (takes effect on your next message)"
        } else {
            ""
        };
        self.reasoning.effort = effort;
        self.entries.push(Entry::Info(format!(
            "Effort set to {}{note}.",
            effort_label(effort)
        )));
    }

    fn set_effort(&mut self, arg: Option<&str>) {
        let levels = agent::Effort::ALL.map(|e| e.as_str()).join(", ");
        let Some(arg) = arg else {
            self.open_effort_picker();
            return;
        };
        let effort = match arg.to_ascii_lowercase().as_str() {
            "default" | "auto" | "none" | "reset" => None,
            other => match agent::Effort::parse(other) {
                Some(e) => Some(e),
                None => {
                    self.entries.push(Entry::Error(format!(
                        "Unknown effort level: {arg} (use {levels}, or default)"
                    )));
                    return;
                }
            },
        };
        self.apply_effort(effort);
    }

    fn set_thinking(&mut self, arg: Option<&str>) {
        let thinking = match arg {
            None => match self.reasoning.thinking {
                agent::Thinking::Adaptive => agent::Thinking::Off,
                agent::Thinking::Off => agent::Thinking::Adaptive,
            },
            Some(arg) => match agent::Thinking::parse(arg) {
                Some(thinking) => thinking,
                None => {
                    self.entries.push(Entry::Error(format!(
                        "Unknown thinking mode: {arg} (use adaptive, on, off)"
                    )));
                    return;
                }
            },
        };
        self.reasoning.thinking = thinking;
        let note = if self.is_running() {
            " (takes effect on your next message)"
        } else {
            ""
        };
        self.entries.push(Entry::Info(format!(
            "Thinking set to {}{note}.",
            thinking.as_str()
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
                self.context_tokens = 0;
                self.entries.clear();
                self.entries
                    .push(Entry::Info("Conversation cleared.".into()));
            }
            "/help" => self.help_scroll = Some(0),
            "/thinking" => self.set_thinking(cmd.split_whitespace().nth(1)),
            "/compact" => {
                let rest = cmd["/compact".len()..].trim();
                self.compact((!rest.is_empty()).then(|| rest.to_string()));
            }
            "/model" | "/models" => match cmd.split_whitespace().nth(1) {
                Some(id) => self.set_model(id.to_string()),
                None => self.open_picker(),
            },
            "/effort" => self.set_effort(cmd.split_whitespace().nth(1)),
            other => self.entries.push(Entry::Error(format!(
                "Unknown command: {other} (type /help to see all commands)"
            ))),
        }
    }

    fn interrupt(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            self.entries.push(Entry::Info("Interrupted.".into()));
        }
    }

    pub fn on_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Agent(turn, ev) => self.on_agent(turn, ev),
            AppEvent::Models(result) => self.on_models(result),
        }
    }

    fn on_agent(&mut self, turn: u64, event: AgentEvent) {
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
                _ => {
                    self.attempt_entry = Some(self.entries.len());
                    self.entries.push(Entry::Assistant(delta));
                }
            },
            AgentEvent::ToolCall { name, summary } => {
                self.attempt_entry = None;
                self.entries.push(Entry::Tool {
                    name,
                    summary,
                    output: None,
                })
            }
            AgentEvent::Retry {
                attempt,
                delay,
                error,
            } => {
                if let Some(i) = self.attempt_entry.take()
                    && matches!(self.entries.get(i), Some(Entry::Assistant(_)))
                {
                    self.entries.remove(i);
                }
                self.entries.push(Entry::Info(format!(
                    "⟳ {error} — retrying in {:.1}s (attempt {attempt})",
                    delay.as_secs_f64()
                )));
            }
            AgentEvent::Compacting => {
                self.compacting = true;
                self.attempt_entry = None;
                self.entries
                    .push(Entry::Info("Compacting conversation…".into()));
            }
            AgentEvent::Compacted {
                usage,
                before,
                after,
            } => {
                self.compacting = false;
                self.add_usage(&usage);
                self.context_tokens = after;
                self.entries.push(Entry::Info(format!(
                    "Conversation compacted ({} → {} tokens).",
                    fmt_tokens(before),
                    fmt_tokens(after)
                )));
            }
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
            AgentEvent::History(h) => {
                self.attempt_entry = None;
                self.history = h;
            }
            AgentEvent::Usage(u) => {
                self.add_usage(&u);
                self.context_tokens = agent::context_size(&u);
            }
            AgentEvent::Error(e) => self.entries.push(Entry::Error(e)),
            AgentEvent::Done => {
                self.task = None;
                self.compacting = false;
            }
        }
    }

    fn add_usage(&mut self, u: &rig_core::completion::Usage) {
        self.input_tokens += u.input_tokens + u.cached_input_tokens + u.cache_creation_input_tokens;
        self.cached_tokens += u.cached_input_tokens;
        self.output_tokens += u.output_tokens;
    }

    // -------------------------------------------------------------- render

    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let input_width = area.width.saturating_sub(4).max(1) as usize;
        let (rows, cursor) = wrap_input(&self.input, self.cursor, input_width);
        let input_height = (rows.len() as u16).clamp(1, 10) + 2;

        let [chat_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .areas(area);

        self.draw_chat(frame, chat_area);
        self.draw_input(frame, input_area, rows, cursor);
        self.draw_status(frame, status_area);
        if self.help_scroll.is_some() {
            self.draw_help(frame, chat_area);
        }
        if self.picker.is_some() {
            self.draw_picker(frame, chat_area);
        }
        if self.effort_picker.is_some() {
            self.draw_effort_picker(frame, chat_area);
        }
    }

    fn draw_effort_picker(&self, frame: &mut Frame, area: Rect) {
        let Some(selected) = self.effort_picker else {
            return;
        };
        let label_width = EFFORT_CHOICES
            .iter()
            .map(|(e, _)| effort_label(*e).len())
            .max()
            .unwrap_or(0);
        let items: Vec<ListItem> = EFFORT_CHOICES
            .iter()
            .enumerate()
            .map(|(i, (effort, desc))| {
                let current = *effort == self.reasoning.effort;
                ListItem::new(Line::from(vec![
                    Span::from(if current { "● " } else { "  " }).fg(Color::Green),
                    Span::from(format!("{} ", i + 1)).fg(Color::DarkGray),
                    Span::from(format!("{:<label_width$}  ", effort_label(*effort))).bold(),
                    Span::from(*desc).fg(Color::DarkGray),
                ]))
            })
            .collect();
        let width = (items.iter().map(ListItem::width).max().unwrap_or(0) + 6).max(48) as u16;
        let height = EFFORT_CHOICES.len() as u16 + 2;
        let popup = area.centered(Constraint::Max(width), Constraint::Length(height));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Select effort ".bold())
            .title_bottom(Line::from(" ↑/↓ move · enter select · esc cancel ").right_aligned());
        frame.render_widget(Clear, popup);
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(selected));
        frame.render_stateful_widget(list, popup, &mut state);
    }

    fn draw_picker(&self, frame: &mut Frame, area: Rect) {
        let Some(picker) = &self.picker else {
            return;
        };
        let popup = area.centered(Constraint::Max(70), Constraint::Max(20));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Select model ".bold())
            .title_bottom(
                Line::from(" type to filter · ↑/↓ move · enter select · esc cancel ")
                    .right_aligned(),
            );
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);

        let [filter_area, list_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::from(" Filter: ").fg(Color::DarkGray),
                Span::from(picker.filter.as_str()),
            ])),
            filter_area,
        );
        frame.set_cursor_position(Position::new(
            filter_area.x + 9 + picker.filter.width() as u16,
            filter_area.y,
        ));

        if self.models.is_none() {
            frame.render_widget(
                Paragraph::new(
                    format!(" {} Loading models…", SPINNER[self.tick % SPINNER.len()])
                        .fg(Color::Yellow),
                ),
                list_area,
            );
            return;
        }
        let models = self.filtered_models();
        if models.is_empty() {
            frame.render_widget(
                Paragraph::new(" No matching models".fg(Color::DarkGray)),
                list_area,
            );
            return;
        }
        let id_width = models.iter().map(|m| m.id.len()).max().unwrap_or(0);
        let items: Vec<ListItem> = models
            .iter()
            .map(|m| {
                let current = m.id == self.model_name;
                ListItem::new(Line::from(vec![
                    Span::from(if current { "● " } else { "  " }).fg(Color::Green),
                    Span::from(format!("{:<id_width$}  ", m.id)),
                    Span::from(m.name.as_str()).fg(Color::DarkGray),
                ]))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(picker.selected));
        frame.render_stateful_widget(list, list_area, &mut state);
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
        if self.is_running() {
            lines.push(Line::from(
                format!(
                    "{} {}",
                    SPINNER[self.tick % SPINNER.len()],
                    if self.compacting {
                        "Compacting…"
                    } else {
                        "Thinking…"
                    }
                )
                .fg(Color::Yellow),
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
        let hint = if self.picker.is_some() {
            "type to filter · ↑/↓ move · enter select · esc cancel"
        } else if self.effort_picker.is_some() {
            "↑/↓ move · 1–6 jump · enter select · esc cancel"
        } else if self.help_scroll.is_some() {
            "↑/↓ j/k scroll · pgup/pgdn page · esc close"
        } else if self.is_running() {
            "esc to interrupt"
        } else {
            "enter send · alt+enter newline · /model · /help"
        };
        let window = agent::context_window(&self.model_name);
        let pct = self.context_tokens * 100 / window.max(1);
        let ctx_color = match pct {
            0..60 => Color::DarkGray,
            60..80 => Color::Yellow,
            _ => Color::Red,
        };
        let cached = if self.input_tokens > 0 {
            format!(
                " ({}% cached)",
                self.cached_tokens * 100 / self.input_tokens
            )
        } else {
            String::new()
        };
        let line = Line::from(vec![
            Span::from(format!(" {} ", self.model_name)).fg(Color::Magenta),
            Span::from(format!("· thinking {} ", self.reasoning.thinking.as_str()))
                .fg(Color::Magenta),
            Span::from(match self.reasoning.effort {
                Some(e) => format!("· {} ", e.as_str()),
                None => String::new(),
            })
            .fg(Color::Magenta),
            Span::from(format!(
                "· ctx {}/{} {pct}% ",
                fmt_tokens(self.context_tokens),
                fmt_tokens(window)
            ))
            .fg(ctx_color),
            Span::from(format!(
                "· ↑{}{cached} ↓{} ",
                fmt_tokens(self.input_tokens),
                fmt_tokens(self.output_tokens)
            ))
            .fg(Color::DarkGray),
            Span::from(format!("· {hint}")).fg(Color::DarkGray),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }
}

/// Compact token count, e.g. 950, 12.3k, 1.2M.
fn fmt_tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", n as f64 / 1e3),
        10_000..1_000_000 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1e6),
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
            (
                "--effort <level>",
                "Effort level: low|medium|high|xhigh|max (or set HARNESS_EFFORT)",
            ),
            (
                "--thinking <mode>",
                "Thinking: adaptive|off (default: adaptive; or set HARNESS_THINKING)",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_command_toggles_and_accepts_explicit_modes() {
        let client = anthropic::Client::new("test-key").unwrap();
        let (mut app, _rx) = App::new_with_channel(
            client,
            "claude-sonnet-4-6".into(),
            agent::Reasoning::default(),
        );
        assert_eq!(app.reasoning.thinking, agent::Thinking::Adaptive);
        app.command("/thinking");
        assert_eq!(app.reasoning.thinking, agent::Thinking::Off);
        app.command("/thinking");
        assert_eq!(app.reasoning.thinking, agent::Thinking::Adaptive);
        app.command("/thinking off");
        assert_eq!(app.reasoning.thinking, agent::Thinking::Off);
        app.command("/thinking on");
        assert_eq!(app.reasoning.thinking, agent::Thinking::Adaptive);
        app.command("/thinking typo");
        assert_eq!(app.reasoning.thinking, agent::Thinking::Adaptive);
        assert!(matches!(app.entries.last(), Some(Entry::Error(_))));
    }

    #[test]
    fn effort_command_opens_picker_and_arrows_select() {
        let client = anthropic::Client::new("test-key").unwrap();
        let (mut app, _rx) = App::new_with_channel(
            client,
            "claude-sonnet-4-6".into(),
            agent::Reasoning::default(),
        );
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        app.command("/effort");
        assert_eq!(app.effort_picker, Some(0));
        app.on_event(key(KeyCode::Down));
        app.on_event(key(KeyCode::Down));
        app.on_event(key(KeyCode::Enter));
        assert_eq!(app.effort_picker, None);
        assert_eq!(app.reasoning.effort, Some(agent::Effort::Medium));

        // Reopening starts on the current level; Esc cancels without changes.
        app.command("/effort");
        assert_eq!(app.effort_picker, Some(2));
        app.on_event(key(KeyCode::Up));
        app.on_event(key(KeyCode::Esc));
        assert_eq!(app.reasoning.effort, Some(agent::Effort::Medium));

        // Number keys jump straight to a level.
        app.command("/effort");
        app.on_event(key(KeyCode::Char('6')));
        assert_eq!(app.reasoning.effort, Some(agent::Effort::Max));

        app.command("/effort default");
        assert_eq!(app.reasoning.effort, None);
    }
}
