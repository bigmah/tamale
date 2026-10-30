//! Conversation state and agent event handling. Input, commands, sessions and drawing
//! live in child modules so each responsibility can be changed independently.

mod approval;
mod backends;
mod commands;
mod help;
mod input;
mod rewind;
mod sessions;
#[cfg(test)]
mod tests;
mod transcript;
mod view;

use rig_core::completion::Message;
use rig_core::providers::anthropic;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::agent::{self, AgentEvent};
use crate::backend::{self, Backend};
use crate::checkpoint::{self, HistoryMark, Recorder};
use crate::config::Settings;
use crate::permissions::{self, Policy};
use crate::session::{self, Entry};
use crate::workspace::Workspace;
use view::fmt_tokens;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TOOL_PREVIEW_LINES: usize = 6;
/// In quiet mode, only the last few calls of a consecutive run of tool calls are shown.
const QUIET_TOOL_CALLS: usize = 5;
/// In quiet mode, the raw call shown after a step's description is truncated to this many characters.
const QUIET_SUMMARY_CHARS: usize = 50;
/// Lines scrolled per mouse wheel notch.
const WHEEL_LINES: usize = 3;

pub enum AppEvent {
    /// An agent event tagged with the turn it belongs to.
    Agent(u64, AgentEvent),
    Models(Result<Vec<ModelInfo>, String>),
    ClipboardCopied(Result<(), String>),
    ClipboardPasted(Result<String, String>),
    SessionSaved(String, Result<(), String>),
    /// A backend requested with `/backend` is ready (or couldn't be set up).
    Backend(Result<Backend, String>),
    /// Claude Code logins found for the backend picker.
    Logins(Result<Vec<backend::ClaudeCode>, String>),
    /// An added Claude Code account was removed (its label), or why it couldn't be.
    LoginRemoved(Result<String, String>),
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
    backend: Backend,
    /// The backend's own conversation for `history` (a Claude Code session), cleared
    /// whenever `history` is rewritten so the next turn starts a fresh one.
    thread: Option<String>,
    /// Claude Code logins known so far (the backend picker refreshes them).
    logins: Vec<backend::ClaudeCode>,
    /// Set by `/login` and the picker; the event loop suspends the TUI to run the sign-in.
    login_request: Option<backends::LoginRequest>,
    settings: Settings,
    tx: mpsc::UnboundedSender<AppEvent>,
    /// `None` until the list has been fetched.
    models: Option<Vec<ModelInfo>>,
    picker: Option<ModelPicker>,
    /// Selected row of the effort picker; `Some` while it is open.
    effort_picker: Option<usize>,
    backend_picker: Option<backends::Picker>,
    resume_picker: Option<ResumePicker>,
    rewind_picker: Option<rewind::Picker>,
    /// The tool call waiting for the user's approval.
    approval: Option<approval::Prompt>,
    /// Shared with running turns, so mode changes apply to the next tool call.
    permissions: permissions::Shared,
    /// Turns that can be undone, oldest first.
    checkpoints: Vec<checkpoint::Turn>,
    /// Id of the current session, used as its file name when saving.
    session_id: String,
    session_created: u64,
    /// Whether a save failure has been reported (to avoid repeating it on every save).
    save_error_shown: bool,
    history: Vec<Message>,
    entries: Vec<Entry>,
    transcript: transcript::Transcript,
    workspace: Workspace,
    /// CLI directories also apply when choosing a saved session later.
    startup_dirs: Vec<std::path::PathBuf>,
    session_writer: Option<session::Writer>,
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
        backend: Backend,
        settings: Settings,
        workspace: Workspace,
    ) -> (Self, mpsc::UnboundedReceiver<AppEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self::new(backend, settings, workspace, tx), rx)
    }

    fn new(
        backend: Backend,
        settings: Settings,
        workspace: Workspace,
        tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        let permissions = Policy::new(&settings.permissions, workspace.cwd.clone()).shared();
        let logins = match &backend {
            Backend::ClaudeCode(cc) => vec![cc.clone()],
            Backend::Api(_) => Vec::new(),
        };
        Self {
            backend,
            thread: None,
            logins,
            login_request: None,
            settings,
            tx,
            models: None,
            picker: None,
            effort_picker: None,
            backend_picker: None,
            resume_picker: None,
            rewind_picker: None,
            approval: None,
            permissions,
            checkpoints: Vec::new(),
            session_id: session::new_id(),
            session_created: session::now(),
            save_error_shown: false,
            history: Vec::new(),
            entries: vec![Entry::Info(
                "Welcome! Type a request and press Enter. /help for commands.".into(),
            )],
            transcript: transcript::Transcript::default(),
            startup_dirs: workspace.additional_dirs.clone(),
            workspace,
            session_writer: None,
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

    pub fn mouse_enabled(&self) -> bool {
        self.settings.ui.mouse
    }

    pub fn is_running(&self) -> bool {
        self.task.is_some()
    }

    /// Whether something on screen is animating (spinners).
    pub fn is_busy(&self) -> bool {
        self.is_running()
            || (self.picker.is_some() && self.models.is_none())
            || self.backend_picker.as_ref().is_some_and(|p| p.loading)
    }

    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

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

        let ctx = self.begin_turn(text.clone());
        let turn = self.backend_turn(text);
        let atx = self.start_turn();
        self.task = Some(tokio::spawn(self.backend.clone().run(turn, ctx, atx)));
    }

    fn backend_turn(&self, prompt: String) -> backend::Turn {
        backend::Turn {
            model: self.settings.model.name.clone(),
            history: self.history.clone(),
            prompt,
            thread: self.thread.clone(),
            config: self.settings.turn_config(self.context_tokens),
            workspace: self.workspace.clone(),
        }
    }

    /// Adds the prompt to the conversation and starts a checkpoint for the turn.
    fn begin_turn(&mut self, text: String) -> agent::TurnCtx {
        let turn = self.checkpoints.last().map_or(1, |t| t.turn + 1);
        self.checkpoints.push(checkpoint::Turn {
            turn,
            prompt: text.clone(),
            entries_len: self.entries.len(),
            history: Some(HistoryMark {
                len: self.history.len(),
                merged: match self.history.last() {
                    Some(Message::User { content }) => Some(content.len()),
                    _ => None,
                },
            }),
        });
        self.entries.push(Entry::User(text.clone()));
        // Consecutive user turns (e.g. after an interrupt) are merged into one message.
        agent::push_user_text(&mut self.history, text);
        self.save_session();
        agent::TurnCtx {
            permissions: self.permissions.clone(),
            checkpoint: session::checkpoint_dir(&self.session_id, turn).map(Recorder::new),
        }
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
        let turn = self.backend_turn(String::new());
        let atx = self.start_turn();
        self.task = Some(tokio::spawn(self.backend.clone().compact(
            turn,
            instructions,
            atx,
        )));
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

    fn interrupt(&mut self) {
        self.approval = None;
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
            AppEvent::Backend(result) => self.on_backend(result),
            AppEvent::Logins(result) => self.on_logins(result),
            AppEvent::LoginRemoved(result) => self.on_login_removed(result),
            AppEvent::SessionSaved(id, result) => self.on_session_saved(&id, result),
            AppEvent::ClipboardPasted(Ok(text)) => self.on_paste(&text),
            AppEvent::ClipboardCopied(Ok(())) => self
                .entries
                .push(Entry::Info("Copied to clipboard.".into())),
            AppEvent::ClipboardPasted(Err(error)) | AppEvent::ClipboardCopied(Err(error)) => {
                self.entries
                    .push(Entry::Error(format!("Clipboard: {error}")));
            }
        }
    }

    fn on_agent(&mut self, turn: u64, event: AgentEvent) {
        if turn != self.turn {
            return;
        }
        if matches!(event, AgentEvent::Compacted { .. }) {
            // Earlier messages become a summary, even if the turn was just interrupted
            // (its summary snapshot may still arrive). Undo can still restore files.
            for turn in &mut self.checkpoints {
                turn.history = None;
            }
            self.thread = None;
        }
        if let AgentEvent::Thread(id) = event {
            // Kept even after an interrupt: the session holds what was said so far.
            self.thread = Some(id);
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
                Some(Entry::Assistant(text)) => {
                    text.push_str(&delta);
                    self.transcript.append_to(self.entries.len() - 1);
                }
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
                    self.transcript.invalidate_from(i);
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
            AgentEvent::Continuing(reason) => {
                self.attempt_entry = None;
                self.entries.push(Entry::Info(reason));
            }
            AgentEvent::Approve {
                name,
                summary,
                description,
                always,
                reply,
            } => {
                self.approval = Some(approval::Prompt::new(
                    name,
                    summary,
                    description,
                    always,
                    reply,
                ))
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
                if let Some((i, Entry::Tool { output: slot, .. })) = self
                    .entries
                    .iter_mut()
                    .enumerate()
                    .rev()
                    .find(|(_, e)| matches!(e, Entry::Tool { output: None, .. }))
                {
                    *slot = Some((output, is_error));
                    self.transcript.invalidate_from(i);
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
            AgentEvent::Thread(_) => {}
            AgentEvent::Done => {
                self.task = None;
                self.approval = None;
                self.compacting = false;
                self.save_session();
            }
        }
    }

    fn add_usage(&mut self, u: &rig_core::completion::Usage) {
        self.input_tokens += u.input_tokens + u.cached_input_tokens + u.cache_creation_input_tokens;
        self.cached_tokens += u.cached_input_tokens;
        self.output_tokens += u.output_tokens;
        match agent::cost(&self.settings.model.name, u) {
            Some(c) => self.cost += c,
            None => self.cost_partial = true,
        }
    }
}
