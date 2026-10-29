use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use crossterm::execute;
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
use crate::session::{self, Entry};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TOOL_PREVIEW_LINES: usize = 6;
/// In quiet mode, only the last few calls of a consecutive run of tool calls are shown.
const QUIET_TOOL_CALLS: usize = 5;
/// In quiet mode, the raw call shown after a step's description is truncated to this many characters.
const QUIET_SUMMARY_CHARS: usize = 50;
/// Lines scrolled per mouse wheel notch.
const WHEEL_LINES: usize = 3;

const COMMANDS: &[(&str, &str)] = &[
    (
        "/help",
        "Show this help (scroll with ↑/↓, close with Esc or q)",
    ),
    ("/model", "Pick a model from a list (type to filter)"),
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
        "Toggle mouse wheel scrolling (on by default; off restores native text selection)",
    ),
    (
        "/resume [n|id]",
        "Pick a saved session from this directory to continue (or resume the nth / by id)",
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
            ("Mouse wheel", "Scroll chat by 3 lines (also scrolls help and pickers)"),
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
    ("bash", "Run a shell command"),
];

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
    ("claude-opus-5-5", "Claude Opus 5.5"),
    ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
    ("claude-haiku-5-5", "Claude Haiku 5.5"),
    ("claude-fable-5-1", "Claude Fable 5.1"),
    (anthropic::completion::CLAUDE_OPUS_4_8, "Claude Opus 4.8"),
    (
        anthropic::completion::CLAUDE_SONNET_4_6,
        "Claude Sonnet 4.6",
    ),
    (anthropic::completion::CLAUDE_HAIKU_4_5, "Claude Haiku 4.5"),
];

/// Choices shown in the effort picker, in order. `None` means "API default".
const EFFORT_CHOICES: [(Option<agent::Effort>, &str); 6] = [
    (None, "Let the API decide (omit the setting)"),
    (
        Some(agent::Effort::Low),
        "Fastest and cheapest, least thorough",
    ),
    (
        Some(agent::Effort::Medium),
        "Balanced speed and thoroughness",
    ),
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

struct ResumePicker {
    sessions: Vec<session::Meta>,
    filter: String,
    selected: usize,
}

impl ResumePicker {
    fn filtered(&self) -> Vec<&session::Meta> {
        let filter = self.filter.to_lowercase();
        self.sessions
            .iter()
            .filter(|s| s.title.to_lowercase().contains(&filter) || s.id.starts_with(&filter))
            .collect()
    }
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
    resume_picker: Option<ResumePicker>,
    /// Id of the current session, used as its file name when saving.
    session_id: String,
    session_created: u64,
    /// Whether a save failure has been reported (to avoid repeating it on every save).
    save_error_shown: bool,
    /// Show every tool call with its output preview instead of a compact summary.
    verbose: bool,
    /// Whether mouse capture (wheel scrolling) is enabled.
    mouse: bool,
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
    /// Estimated session cost in USD; `cost_partial` is set if some usage had unknown pricing.
    cost: f64,
    cost_partial: bool,
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
            resume_picker: None,
            session_id: session::new_id(),
            session_created: session::now(),
            save_error_shown: false,
            verbose: false,
            mouse: true,
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
            cost: 0.0,
            cost_partial: false,
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
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => self.on_wheel(true),
                MouseEventKind::ScrollDown => self.on_wheel(false),
                _ => {}
            },
            _ => {}
        }
    }

    fn on_wheel(&mut self, up: bool) {
        if self.picker.is_some() {
            let count = self.filtered_models().len();
            if let Some(picker) = &mut self.picker {
                picker.selected = if up {
                    picker.selected.saturating_sub(1)
                } else {
                    (picker.selected + 1).min(count.saturating_sub(1))
                };
            }
        } else if let Some(picker) = &mut self.resume_picker {
            let count = picker.filtered().len();
            picker.selected = if up {
                picker.selected.saturating_sub(1)
            } else {
                (picker.selected + 1).min(count.saturating_sub(1))
            };
        } else if let Some(selected) = self.effort_picker {
            self.effort_picker = Some(if up {
                selected.saturating_sub(1)
            } else {
                (selected + 1).min(EFFORT_CHOICES.len() - 1)
            });
        } else if let Some(scroll) = self.help_scroll {
            // Clamped to the real maximum when drawn.
            self.help_scroll = Some(if up {
                scroll.saturating_sub(WHEEL_LINES)
            } else {
                scroll.saturating_add(WHEEL_LINES)
            });
        } else if up {
            // Clamped to the real maximum when drawn.
            self.scroll_back = self.scroll_back.saturating_add(WHEEL_LINES);
        } else {
            self.scroll_back = self.scroll_back.saturating_sub(WHEEL_LINES);
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

        if self.resume_picker.is_some() {
            self.on_resume_picker_key(key, ctrl);
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
        self.save_session();

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

    fn set_verbose(&mut self, arg: Option<&str>) {
        let verbose = match arg.map(str::to_ascii_lowercase).as_deref() {
            None => !self.verbose,
            Some("on" | "true" | "yes" | "1") => true,
            Some("off" | "false" | "no" | "0") => false,
            Some(_) => {
                self.entries.push(Entry::Error(format!(
                    "Unknown verbose mode: {} (use on, off)",
                    arg.unwrap_or_default()
                )));
                return;
            }
        };
        self.verbose = verbose;
        self.entries.push(Entry::Info(format!(
            "Verbose output {}.",
            if verbose { "on" } else { "off" }
        )));
    }

    fn set_mouse(&mut self, arg: Option<&str>) {
        let mouse = match arg.map(str::to_ascii_lowercase).as_deref() {
            None => !self.mouse,
            Some("on" | "true" | "yes" | "1") => true,
            Some("off" | "false" | "no" | "0") => false,
            Some(_) => {
                self.entries.push(Entry::Error(format!(
                    "Unknown mouse mode: {} (use on, off)",
                    arg.unwrap_or_default()
                )));
                return;
            }
        };
        let result = if mouse {
            execute!(std::io::stdout(), EnableMouseCapture)
        } else {
            execute!(std::io::stdout(), DisableMouseCapture)
        };
        if let Err(e) = result {
            self.entries
                .push(Entry::Error(format!("Couldn't change mouse mode: {e}")));
            return;
        }
        self.mouse = mouse;
        self.entries.push(Entry::Info(if mouse {
            "Mouse scrolling on (hold Shift/Option to select text).".into()
        } else {
            "Mouse scrolling off (native text selection restored).".into()
        }));
    }

    fn command(&mut self, cmd: &str) {
        match cmd.split_whitespace().next().unwrap_or("") {
            "/quit" | "/exit" => self.should_quit = true,
            "/clear" if self.is_running() => self.entries.push(Entry::Error(
                "Can't clear while the agent is running (Esc to interrupt first).".into(),
            )),
            "/clear" => {
                self.save_session();
                self.history.clear();
                self.context_tokens = 0;
                self.entries.clear();
                self.session_id = session::new_id();
                self.session_created = session::now();
                self.entries
                    .push(Entry::Info("Conversation cleared.".into()));
            }
            "/resume" | "/continue" => self.resume_command(cmd.split_whitespace().nth(1)),
            "/help" => self.help_scroll = Some(0),
            "/thinking" => self.set_thinking(cmd.split_whitespace().nth(1)),
            "/verbose" => self.set_verbose(cmd.split_whitespace().nth(1)),
            "/mouse" => self.set_mouse(cmd.split_whitespace().nth(1)),
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

    // -------------------------------------------------------------- sessions

    /// Writes the current conversation to disk (no-op until something has been said).
    pub fn save_session(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let title = self
            .entries
            .iter()
            .find_map(|e| match e {
                Entry::User(t) => Some(truncate_chars(t.lines().next().unwrap_or(""), 80)),
                _ => None,
            })
            .unwrap_or_default();
        let saved = session::Session {
            id: self.session_id.clone(),
            cwd: std::env::current_dir().unwrap_or_default(),
            created: self.session_created,
            updated: session::now(),
            model: self.model_name.clone(),
            title,
            context_tokens: self.context_tokens,
            history: self.history.clone(),
            entries: self.entries.clone(),
        };
        match session::save(&saved) {
            Ok(()) => self.save_error_shown = false,
            Err(e) if !self.save_error_shown => {
                self.save_error_shown = true;
                self.entries
                    .push(Entry::Error(format!("Couldn't save session: {e:#}")));
            }
            Err(_) => {}
        }
    }

    /// Saved sessions from this directory, excluding the current one.
    fn resumable_sessions(&self) -> Vec<session::Meta> {
        let cwd = std::env::current_dir().unwrap_or_default();
        session::list(Some(&cwd))
            .into_iter()
            .filter(|m| m.id != self.session_id)
            .collect()
    }

    /// `/resume` opens a picker; `/resume <n>` resumes the nth most recent session from this
    /// directory and `/resume <id>` a session by id (or unique id prefix) from anywhere.
    pub fn resume_command(&mut self, arg: Option<&str>) {
        if self.is_running() {
            self.entries.push(Entry::Error(
                "Can't resume while the agent is running (Esc to interrupt first).".into(),
            ));
            return;
        }
        let sessions = self.resumable_sessions();
        let Some(arg) = arg else {
            if sessions.is_empty() {
                self.entries.push(Entry::Info(
                    "No saved sessions for this directory yet.".into(),
                ));
            } else {
                self.resume_picker = Some(ResumePicker {
                    sessions,
                    filter: String::new(),
                    selected: 0,
                });
            }
            return;
        };
        if let Ok(n) = arg.parse::<usize>() {
            match n.checked_sub(1).and_then(|i| sessions.get(i)) {
                Some(m) => {
                    let id = m.id.clone();
                    self.resume(&id);
                }
                None => self.entries.push(Entry::Error(format!(
                    "No session #{n} (this directory has {} saved).",
                    sessions.len()
                ))),
            }
            return;
        }
        let matches: Vec<String> = session::list(None)
            .into_iter()
            .filter(|m| m.id.starts_with(arg))
            .map(|m| m.id)
            .collect();
        match matches.as_slice() {
            [id] => {
                let id = id.clone();
                self.resume(&id);
            }
            [] => self
                .entries
                .push(Entry::Error(format!("No saved session matches {arg}."))),
            _ => self.entries.push(Entry::Error(format!(
                "{arg} matches {} sessions; use a longer id.",
                matches.len()
            ))),
        }
    }

    /// Replaces the conversation with a saved session; later saves go to that session.
    pub fn resume(&mut self, id: &str) {
        if self.is_running() {
            return;
        }
        let saved = match session::load(id) {
            Ok(s) => s,
            Err(e) => {
                self.entries
                    .push(Entry::Error(format!("Couldn't resume session: {e:#}")));
                return;
            }
        };
        self.save_session();
        self.turn += 1;
        self.history = saved.history;
        self.entries = saved.entries;
        self.context_tokens = saved.context_tokens;
        self.session_id = saved.id;
        self.session_created = saved.created;
        self.scroll_back = 0;
        self.attempt_entry = None;

        let mut note = format!(
            "Resumed session “{}” (last active {}).",
            saved.title,
            session::ago(saved.updated)
        );
        let cwd = std::env::current_dir().unwrap_or_default();
        if saved.cwd != cwd {
            note.push_str(&format!(
                " It was started in {}; tools now run in {}.",
                saved.cwd.display(),
                cwd.display()
            ));
        }
        if saved.model != self.model_name {
            note.push_str(&format!(
                " It used {}; continuing with {} (/model to switch).",
                saved.model, self.model_name
            ));
        }
        self.entries.push(Entry::Info(note));
    }

    fn on_resume_picker_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(picker) = &mut self.resume_picker else {
            return;
        };
        let count = picker.filtered().len();
        let last = count.saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.resume_picker = None,
            KeyCode::Char('c') if ctrl => self.resume_picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Char('n') if ctrl => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Home => picker.selected = 0,
            KeyCode::End => picker.selected = last,
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.selected = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                picker.filter.push(c);
                picker.selected = 0;
            }
            KeyCode::Enter => {
                let id = picker.filtered().get(picker.selected).map(|m| m.id.clone());
                if let Some(id) = id {
                    self.resume_picker = None;
                    self.resume(&id);
                }
            }
            _ => {}
        }
    }

    fn interrupt(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            self.entries.push(Entry::Info("Interrupted.".into()));
            self.save_session();
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
                self.save_session();
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
            AgentEvent::ToolCall {
                name,
                summary,
                description,
            } => {
                self.attempt_entry = None;
                self.entries.push(Entry::Tool {
                    name,
                    summary,
                    description,
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
                self.save_session();
            }
            AgentEvent::Usage(u) => {
                self.add_usage(&u);
                self.context_tokens = agent::context_size(&u);
            }
            AgentEvent::Error(e) => self.entries.push(Entry::Error(e)),
            AgentEvent::Done => {
                self.task = None;
                self.compacting = false;
                self.save_session();
            }
        }
    }

    fn add_usage(&mut self, u: &rig_core::completion::Usage) {
        self.input_tokens += u.input_tokens + u.cached_input_tokens + u.cache_creation_input_tokens;
        self.cached_tokens += u.cached_input_tokens;
        self.output_tokens += u.output_tokens;
        match agent::cost(&self.model_name, u) {
            Some(c) => self.cost += c,
            None => self.cost_partial = true,
        }
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
        if self.resume_picker.is_some() {
            self.draw_resume_picker(frame, chat_area);
        }
    }

    fn draw_resume_picker(&self, frame: &mut Frame, area: Rect) {
        let Some(picker) = &self.resume_picker else {
            return;
        };
        let popup = area.centered(Constraint::Percentage(80), Constraint::Max(20));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Resume session ".bold())
            .title_bottom(
                Line::from(" type to filter · ↑/↓ move · enter resume · esc cancel ")
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

        let sessions = picker.filtered();
        if sessions.is_empty() {
            frame.render_widget(
                Paragraph::new(" No matching sessions".fg(Color::DarkGray)),
                list_area,
            );
            return;
        }
        let ages: Vec<String> = sessions.iter().map(|m| session::ago(m.updated)).collect();
        let age_width = ages.iter().map(|a| a.width()).max().unwrap_or(0);
        let items: Vec<ListItem> = sessions
            .iter()
            .zip(&ages)
            .map(|(m, age)| {
                let s = if m.prompts == 1 { "" } else { "s" };
                ListItem::new(Line::from(vec![
                    Span::from(format!("{age:>age_width$}  ")).fg(Color::DarkGray),
                    Span::from(if m.title.is_empty() {
                        "(untitled)".to_string()
                    } else {
                        m.title.clone()
                    }),
                    Span::from(format!("  {} prompt{s} · {}", m.prompts, m.model))
                        .fg(Color::DarkGray),
                ]))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(picker.selected));
        frame.render_stateful_widget(list, list_area, &mut state);
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
        if self.verbose {
            for entry in &self.entries {
                render_entry(entry, true, &mut lines);
                lines.push(Line::default());
            }
        } else {
            render_quiet(&self.entries, &mut lines);
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
        let offset = max_scroll - self.scroll_back;
        frame.render_widget(para.scroll((offset.min(u16::MAX as usize) as u16, 0)), area);
        if self.scroll_back > 0 {
            let mut state = ScrollbarState::new(max_scroll).position(offset);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                area,
                &mut state,
            );
            let hint = format!(" ↓ {} more lines ", self.scroll_back);
            let width = (hint.width() as u16).min(area.width);
            let hint_area = Rect::new(
                area.right().saturating_sub(width + 1),
                area.bottom().saturating_sub(1),
                width,
                1,
            );
            frame.render_widget(Clear, hint_area);
            frame.render_widget(
                Paragraph::new(hint).style(Style::new().fg(Color::Black).bg(Color::Yellow)),
                hint_area,
            );
        }
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
        let hint = if self.picker.is_some() || self.resume_picker.is_some() {
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
            Span::from(if self.verbose { "· verbose " } else { "" }).fg(Color::Magenta),
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
            Span::from(format!(
                "· ${:.2}{} ",
                self.cost,
                if self.cost_partial { "+" } else { "" }
            ))
            .fg(Color::Green),
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

/// Compact chat rendering: each tool call is one line showing what the step is for (its
/// description) without output, and long runs of consecutive steps are collapsed to the
/// most recent few.
fn render_quiet<'a>(entries: &'a [Entry], lines: &mut Vec<Line<'a>>) {
    let mut i = 0;
    while i < entries.len() {
        if !matches!(entries[i], Entry::Tool { .. }) {
            render_entry(&entries[i], false, lines);
            lines.push(Line::default());
            i += 1;
            continue;
        }
        let end = entries[i..]
            .iter()
            .position(|e| !matches!(e, Entry::Tool { .. }))
            .map_or(entries.len(), |n| i + n);
        let hidden = (end - i).saturating_sub(QUIET_TOOL_CALLS);
        if hidden > 0 {
            let s = if hidden == 1 { "" } else { "s" };
            lines.push(Line::from(
                format!("  … {hidden} earlier step{s} (/verbose to show)").fg(Color::DarkGray),
            ));
        }
        for entry in &entries[i + hidden..end] {
            render_entry(entry, false, lines);
        }
        lines.push(Line::default());
        i = end;
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn render_entry<'a>(entry: &'a Entry, verbose: bool, lines: &mut Vec<Line<'a>>) {
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
            description,
            output,
        } => {
            let dot_color = match output {
                None => Color::Yellow,
                Some((_, true)) => Color::Red,
                Some((_, false)) => Color::Green,
            };
            let summary = summary.lines().next().unwrap_or("");
            let mut spans = vec![
                Span::from("⏺ ").fg(dot_color),
                Span::from(description.as_str()),
            ];
            if verbose {
                lines.push(Line::from(spans));
                lines.push(Line::from(vec![
                    Span::from("  ").fg(Color::DarkGray),
                    Span::from(name.as_str()).fg(Color::Gray).bold(),
                    Span::from(format!("({summary})")).fg(Color::DarkGray),
                ]));
            } else {
                let detail = truncate_chars(summary, QUIET_SUMMARY_CHARS);
                spans.push(Span::from(format!("  {name} {detail}")).fg(Color::DarkGray));
                lines.push(Line::from(spans));
            }
            if !verbose {
                return;
            }
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
    fn sessions_are_saved_and_resumed() {
        let dir = std::env::temp_dir().join(format!("harness-resume-test-{}", session::new_id()));
        // Only this test saves sessions (the others never have history).
        unsafe { std::env::set_var("HARNESS_SESSIONS_DIR", &dir) };
        let client = anthropic::Client::new("test-key").unwrap();
        let (mut app, _rx) = App::new_with_channel(
            client,
            "claude-sonnet-4-6".into(),
            agent::Reasoning::default(),
        );
        app.command("/resume");
        assert!(app.resume_picker.is_none());

        app.entries.push(Entry::User("fix the bug\nmore".into()));
        app.entries.push(Entry::Assistant("done".into()));
        app.history = vec![Message::user("fix the bug"), Message::assistant("done")];
        app.context_tokens = 1234;
        app.save_session();
        let first = app.session_id.clone();

        app.command("/clear");
        assert_ne!(app.session_id, first);
        assert!(app.history.is_empty());

        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        app.command("/resume");
        let picker = app.resume_picker.as_ref().expect("picker opens");
        assert_eq!(picker.sessions.len(), 1);
        assert_eq!(picker.sessions[0].title, "fix the bug");
        app.on_event(key(KeyCode::Enter));
        assert!(app.resume_picker.is_none());
        assert_eq!(app.session_id, first);
        assert_eq!(app.history.len(), 2);
        assert_eq!(app.context_tokens, 1234);
        assert!(
            app.entries
                .iter()
                .any(|e| matches!(e, Entry::User(t) if t.starts_with("fix")))
        );
        assert!(matches!(app.entries.last(), Some(Entry::Info(t)) if t.contains("Resumed")));

        // The current session is excluded, and numeric/id arguments work.
        app.command("/resume 1");
        assert!(matches!(app.entries.last(), Some(Entry::Error(_))));
        app.command("/clear");
        app.command(&format!("/resume {}", &first[..first.len() - 2]));
        assert_eq!(app.session_id, first);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mouse_wheel_scrolls_chat_help_and_pickers() {
        use crossterm::event::MouseEvent;
        let client = anthropic::Client::new("test-key").unwrap();
        let (mut app, _rx) = App::new_with_channel(
            client,
            "claude-sonnet-4-6".into(),
            agent::Reasoning::default(),
        );
        let wheel = |kind| {
            Event::Mouse(MouseEvent {
                kind,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            })
        };
        app.on_event(wheel(MouseEventKind::ScrollUp));
        app.on_event(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll_back, 2 * WHEEL_LINES);
        app.on_event(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll_back, WHEEL_LINES);
        app.on_event(wheel(MouseEventKind::ScrollDown));
        app.on_event(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll_back, 0);

        app.command("/help");
        app.on_event(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.help_scroll, Some(WHEEL_LINES));
        assert_eq!(app.scroll_back, 0);
        app.help_scroll = None;

        app.command("/effort");
        app.on_event(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.effort_picker, Some(1));
        app.on_event(wheel(MouseEventKind::ScrollUp));
        app.on_event(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.effort_picker, Some(0));
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

    #[test]
    fn quiet_mode_collapses_tool_calls_and_verbose_toggles() {
        let client = anthropic::Client::new("test-key").unwrap();
        let (mut app, _rx) = App::new_with_channel(
            client,
            "claude-sonnet-4-6".into(),
            agent::Reasoning::default(),
        );
        assert!(!app.verbose);
        let tool = |n: usize| Entry::Tool {
            name: "bash".into(),
            summary: format!("cmd {n}"),
            description: format!("Step {n}"),
            output: Some(("line a\nline b".into(), false)),
        };
        let entries: Vec<Entry> = (0..7).map(tool).collect();
        let text =
            |lines: &[Line]| -> Vec<String> { lines.iter().map(|l| l.to_string()).collect() };

        let mut quiet = Vec::new();
        render_quiet(&entries, &mut quiet);
        let quiet = text(&quiet);
        assert!(quiet[0].contains("2 earlier steps"));
        assert!(quiet.iter().any(|l| l.contains("Step 6") && l.contains("bash cmd 6")));
        assert!(
            !quiet
                .iter()
                .any(|l| l.contains("cmd 1") || l.contains("line a"))
        );

        let mut verbose = Vec::new();
        for e in &entries {
            render_entry(e, true, &mut verbose);
        }
        assert!(text(&verbose).iter().any(|l| l.contains("line a")));

        app.command("/verbose");
        assert!(app.verbose);
        app.command("/verbose off");
        assert!(!app.verbose);
        app.command("/verbose on");
        assert!(app.verbose);
    }
}
