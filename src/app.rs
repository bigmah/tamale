//! Conversation state and agent event handling. Input, commands, sessions and drawing
//! live in child modules so each responsibility can be changed independently.

mod activity;
mod approval;
#[cfg(unix)]
mod background;
mod backends;
mod commands;
mod diff;
mod goal;
mod help;
mod input;
mod prompts;
#[cfg(unix)]
mod remote;
mod rewind;
mod sessions;
#[cfg(test)]
mod tests;
mod markdown;
mod transcript;
mod view;
mod web;

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
    /// CLI logins found for the backend picker, and CLIs that couldn't be listed.
    Logins(Vec<Backend>, Vec<String>),
    /// An added CLI account was removed (its label), or why it couldn't be.
    LoginRemoved(Result<String, String>),
    /// A `/diff` load finished (tagged with the viewer's load generation).
    Diff(u64, Result<Vec<crate::diff::FileDiff>, String>),
    /// A session stopped from `/sessions` has gone (its id), or why it hasn't.
    #[cfg(unix)]
    SessionStopped(u32, Result<(), String>),
    /// The hub's list of sessions, for an open `/sessions`.
    #[cfg(unix)]
    HubSessions(Result<Vec<crate::remote::HubSession>, String>),
    /// News of the session being driven, tagged with which time it was opened.
    #[cfg(unix)]
    Remote(u64, crate::remote::Event),
}

/// What `/browser` and `/web` ask the event loop to do.
pub enum BrowserRequest {
    Open,
    Close,
}

/// The footer's tabs: what's reached most often comes first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterTab {
    Sessions,
    Remote,
    Web,
    Diff,
}

/// What `/web` and `/remote` asked of the event loop.
pub enum WebRequest {
    /// Serve the web UI; with `lan`, to other devices on the local network too.
    Open { lan: bool },
    /// Connect to (or with `on: false` disconnect from) the `[hub]` tamale-hub.
    Remote { on: bool },
    Close,
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
    /// The backend's own conversation for `history` (a Claude Code session or Codex
    /// thread), cleared
    /// whenever `history` is rewritten so the next turn starts a fresh one.
    thread: Option<String>,
    /// CLI logins (Claude Code and Codex) known so far; the backend picker refreshes them.
    logins: Vec<Backend>,
    /// Set by `/login` and the picker; the event loop suspends the TUI to run the sign-in.
    login_request: Option<backends::LoginRequest>,
    /// Set by `/browser`; the event loop starts or stops serving the TUI to a browser.
    browser_request: Option<BrowserRequest>,
    /// Set by `/web`; the event loop starts or stops serving the web UI.
    web_request: Option<WebRequest>,
    /// Where the footer's tabs were last drawn, so a click on one opens it.
    tab_areas: Vec<(FooterTab, ratatui::layout::Rect)>,
    /// The footer tab picked with ←/→ after ↓ past the prompt's last line moved the
    /// keyboard there; Enter opens it. `None` while the keyboard is in the prompt.
    tab_focus: Option<FooterTab>,
    settings: Settings,
    tx: mpsc::UnboundedSender<AppEvent>,
    /// `None` until the list has been fetched.
    models: Option<Vec<ModelInfo>>,
    picker: Option<ModelPicker>,
    /// Selected row of the effort picker; `Some` while it is open.
    effort_picker: Option<usize>,
    backend_picker: Option<backends::Picker>,
    resume_picker: Option<ResumePicker>,
    #[cfg(unix)]
    sessions_picker: Option<background::SessionsPicker>,
    /// A session on another machine being driven, in place of this one on screen.
    #[cfg(unix)]
    remote_view: Option<remote::RemoteView>,
    #[cfg(unix)]
    remote_generation: u64,
    /// The session the terminal should go to.
    #[cfg(unix)]
    switch_request: Option<u32>,
    rewind_picker: Option<rewind::Picker>,
    /// The `/diff` viewer, while it is open.
    diff: Option<diff::Viewer>,
    /// Whether the Diff tab is showing instead of the chat.
    diff_open: bool,
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
    /// The startup note about other open sessions, true only then, so it's never saved.
    open_sessions_note: Option<String>,
    history: Vec<Message>,
    entries: Vec<Entry>,
    transcript: transcript::Transcript,
    workspace: Workspace,
    /// CLI directories also apply when choosing a saved session later.
    startup_dirs: Vec<std::path::PathBuf>,
    session_writer: Option<session::Writer>,
    input: String,
    cursor: usize,
    /// Messages typed while the agent runs, shared with the running turn (see `agent::Steering`).
    queued: agent::Steering,
    /// Earlier prompts for ↑/↓ recall, oldest first.
    prompts: prompts::Prompts,
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
    /// The `/goal` being worked toward, if any.
    goal: Option<goal::Goal>,
    /// Whether the running turn reported an error.
    turn_failed: bool,
    /// Assistant entry created by the in-flight request, dropped if the request is retried.
    attempt_entry: Option<usize>,
    tick: usize,
    /// When the current run began, for the activity animation and elapsed time.
    busy_since: Option<std::time::Instant>,
    /// Whether `/web` has taken over: replies are written for the web UI (see
    /// `web::ARTIFACT_PROMPT`) and the terminal only shows the session's address.
    web_session: bool,
    /// Viewers connected through a tamale-hub, while connected to one.
    hub_viewers: Option<usize>,
    /// Whether the hub connection is up, rather than still being tried.
    hub_live: bool,
    /// How many of those viewers show artifacts.
    web_viewers: usize,
    /// Whether the last prompt came from another terminal, so isn't written for the web UI.
    plain_prompts: bool,
    /// A reply being continued in place after a token limit cut it off inside a code
    /// block: its entry and its length before the continuation.
    joining: Option<(usize, usize)>,
    pub should_quit: bool,
    /// Running as a server: a client can leave while the session goes on.
    detachable: bool,
    detach_requested: bool,
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
            Backend::Api(_) | Backend::Unset => Vec::new(),
            login => vec![login.clone()],
        };
        Self {
            backend,
            thread: None,
            logins,
            login_request: None,
            browser_request: None,
            web_request: None,
            tab_areas: Vec::new(),
            tab_focus: None,
            settings,
            tx,
            models: None,
            picker: None,
            effort_picker: None,
            backend_picker: None,
            resume_picker: None,
            #[cfg(unix)]
            sessions_picker: None,
            #[cfg(unix)]
            remote_view: None,
            #[cfg(unix)]
            remote_generation: 0,
            #[cfg(unix)]
            switch_request: None,
            rewind_picker: None,
            diff: None,
            diff_open: false,
            approval: None,
            permissions,
            checkpoints: Vec::new(),
            session_id: session::new_id(),
            session_created: session::now(),
            save_error_shown: false,
            open_sessions_note: None,
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
            queued: agent::Steering::default(),
            prompts: prompts::Prompts::default(),
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
            goal: None,
            turn_failed: false,
            attempt_entry: None,
            tick: 0,
            busy_since: None,
            web_session: false,
            hub_viewers: None,
            hub_live: false,
            web_viewers: 0,
            plain_prompts: false,
            joining: None,
            should_quit: false,
            detachable: false,
            detach_requested: false,
        }
    }

    pub fn mouse_enabled(&self) -> bool {
        self.settings.ui.mouse
    }

    /// Loads earlier prompts from this directory for ↑ recall, and saves new ones.
    pub fn load_prompt_history(&mut self) {
        self.prompts = prompts::Prompts::load(&self.workspace.cwd);
    }

    pub fn is_running(&self) -> bool {
        self.task.is_some()
    }

    #[cfg(unix)]
    pub fn set_detachable(&mut self) {
        self.detachable = true;
    }

    /// Whether the terminal should be let go of, since the last time this was asked.
    #[cfg(unix)]
    pub fn take_detach_request(&mut self) -> bool {
        std::mem::take(&mut self.detach_requested)
    }

    /// What Ctrl+C does when it isn't clearing anything: leaves a turn running if it can,
    /// and otherwise ends the session.
    pub fn quit_or_detach(&mut self) {
        if self.detachable && self.is_running() {
            self.detach_requested = true;
        } else {
            self.should_quit = true;
        }
    }

    /// What `tamale ls` shows about this session: its directory, id, title and state.
    #[cfg(unix)]
    pub fn background_status(
        &self,
    ) -> (std::path::PathBuf, String, String, crate::background::State) {
        use crate::background::State;
        let state = if self.approval.is_some() {
            State::NeedsApproval
        } else if self.is_running() {
            State::Running
        } else {
            State::Idle
        };
        (
            self.workspace.cwd.clone(),
            self.session_id.clone(),
            self.session_title(),
            state,
        )
    }

    /// Whether something on screen is animating (spinners).
    pub fn is_busy(&self) -> bool {
        #[cfg(unix)]
        if self.sessions_picker.is_some() || self.remote_view.is_some() {
            return true;
        }
        self.is_running()
            || (self.picker.is_some() && self.models.is_none())
            || self.backend_picker.as_ref().is_some_and(|p| p.loading)
            || self.diff_loading()
    }

    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        #[cfg(unix)]
        self.refresh_sessions_picker();
    }

    fn submit(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.input.clear();
        self.cursor = 0;
        self.plain_prompts = false;
        self.submit_text(text);
    }

    /// Sends a prompt or runs a slash command, typed here or in the web UI.
    fn submit_text(&mut self, text: String) {
        self.prompts.add(&text);
        self.scroll_back = 0;

        if text.starts_with('/') {
            self.command(&text);
            return;
        }
        if self.is_running() {
            // Delivered with the next tool result, or sent when the turn ends.
            self.queued.lock().unwrap().push(text);
            return;
        }
        self.send_prompt(text);
    }

    /// Messages still queued, oldest first.
    fn queued_messages(&self) -> Vec<String> {
        self.queued.lock().unwrap().clone()
    }

    /// Sends messages queued during a turn that ended before they could be delivered.
    fn send_queued(&mut self) {
        let queued = std::mem::take(&mut *self.queued.lock().unwrap());
        if !queued.is_empty() {
            self.send_prompt(queued.join("\n\n"));
        }
    }

    fn send_prompt(&mut self, text: String) {
        let prompt = if self.writes_for_web() {
            format!("{text}{}", crate::web::ARTIFACT_PROMPT)
        } else {
            text.clone()
        };
        self.send_prompt_as(text, prompt);
    }

    /// Sends `prompt` to the model while showing (and keeping for `/undo`) `shown`.
    fn send_prompt_as(&mut self, shown: String, prompt: String) {
        if self.refuse_without_backend(&shown) {
            return;
        }
        let ctx = self.begin_turn_as(shown, prompt.clone());
        let turn = self.backend_turn(prompt);
        let atx = self.start_turn();
        self.task = Some(tokio::spawn(self.backend.clone().run(turn, ctx, atx)));
    }

    fn backend_turn(&self, prompt: String) -> backend::Turn {
        backend::Turn {
            model: self.settings.model.name.clone(),
            history: self.history.clone(),
            prompt,
            thread: self.thread.clone(),
            config: agent::TurnConfig {
                web_ui: self.writes_for_web(),
                ..self.settings.turn_config(self.context_tokens)
            },
            workspace: self.workspace.clone(),
        }
    }

    /// Adds the prompt to the conversation and starts a checkpoint for the turn.
    #[cfg(test)]
    fn begin_turn(&mut self, text: String) -> agent::TurnCtx {
        self.begin_turn_as(text.clone(), text)
    }

    fn begin_turn_as(&mut self, shown: String, text: String) -> agent::TurnCtx {
        let turn = self.checkpoints.last().map_or(1, |t| t.turn + 1);
        self.checkpoints.push(checkpoint::Turn {
            turn,
            prompt: shown.clone(),
            entries_len: self.entries.len(),
            history: Some(HistoryMark {
                len: self.history.len(),
                merged: match self.history.last() {
                    Some(Message::User { content }) => Some(content.len()),
                    _ => None,
                },
            }),
        });
        self.entries.push(Entry::User(shown));
        // Consecutive user turns (e.g. after an interrupt) are merged into one message.
        agent::push_user_text(&mut self.history, text);
        self.save_session();
        // A fresh queue per turn: an interrupted turn's task can't take later messages.
        self.queued = agent::Steering::default();
        agent::TurnCtx::new(
            self.permissions.clone(),
            session::checkpoint_dir(&self.session_id, turn).map(Recorder::new),
        )
        .with_subagent(backend::Subagent {
            backend: self.backend.clone(),
            model: self.settings.model.name.clone(),
            config: self.settings.turn_config(0),
        })
        .with_steering(self.queued.clone())
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
        self.turn_failed = false;
        self.attempt_entry = None;
        self.joining = None;
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
            self.pause_goal("interrupted");
            self.save_session();
        }
        // Undelivered messages go back to the input box rather than being lost.
        let queued = std::mem::take(&mut *self.queued.lock().unwrap());
        if !queued.is_empty() {
            let mut text = queued.join("\n\n");
            if !self.input.trim().is_empty() {
                text.push_str("\n\n");
                text.push_str(&self.input);
            }
            self.input = text;
            self.cursor = self.input.len();
        }
    }

    pub fn on_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Agent(turn, ev) => self.on_agent(turn, ev),
            AppEvent::Models(result) => self.on_models(result),
            AppEvent::Backend(result) => self.on_backend(result),
            AppEvent::Logins(logins, errors) => self.on_logins(logins, errors),
            AppEvent::LoginRemoved(result) => self.on_login_removed(result),
            AppEvent::Diff(generation, result) => self.on_diff(generation, result),
            AppEvent::SessionSaved(id, result) => self.on_session_saved(&id, result),
            #[cfg(unix)]
            AppEvent::SessionStopped(id, result) => self.on_session_stopped(id, result),
            #[cfg(unix)]
            AppEvent::HubSessions(result) => self.on_hub_sessions(result),
            #[cfg(unix)]
            AppEvent::Remote(generation, event) => self.on_remote_event(generation, event),
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
        if let AgentEvent::Steered(messages) = event {
            // Shown even if the turn was just interrupted: the model has already seen them.
            self.attempt_entry = None;
            let text = messages.join("\n\n");
            if let Some(turn) = self.checkpoints.last_mut() {
                // `/undo` puts these back in the input box with the turn's prompt.
                turn.prompt.push_str("\n\n");
                turn.prompt.push_str(&text);
            }
            self.entries.push(Entry::User(text));
            self.save_session();
            return;
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
            AgentEvent::TextDelta(delta) if self.joining.is_some() => {
                let (i, _) = self.joining.unwrap();
                if let Some(Entry::Assistant(text)) = self.entries.get_mut(i) {
                    text.push_str(&delta);
                }
                if i + 1 == self.entries.len() {
                    self.transcript.append_to(i);
                } else {
                    self.transcript.invalidate_from(i);
                }
            }
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
                self.joining = None;
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
                // A failed attempt at continuing a joined reply drops only what it added.
                if let Some((i, len)) = self.joining
                    && let Some(Entry::Assistant(text)) = self.entries.get_mut(i)
                    && text.len() > len
                {
                    text.truncate(len);
                    self.transcript.invalidate_from(i);
                }
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
                // A reply cut off inside a code block (usually an artifact) continues in the
                // same entry, so the block is whole; see `agent::CONTINUE_IN_FENCE`.
                match self.entries.last() {
                    Some(Entry::Assistant(text)) if agent::ends_in_open_fence(text) => {
                        self.joining = Some((self.entries.len() - 1, text.len()));
                    }
                    _ => self.entries.push(Entry::Info(reason)),
                }
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
            AgentEvent::ToolResult { mut output, is_error } => {
                // Delivered user messages are shown as their own entries.
                if let Some(i) = output.find(agent::STEERING_MARKER) {
                    output.truncate(i);
                }
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
                // An open diff viewer follows the agent's edits.
                self.reload_diff();
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
            AgentEvent::SubagentUsage(u) => self.add_usage(&u),
            AgentEvent::Error(e) => {
                self.turn_failed = true;
                self.entries.push(Entry::Error(e))
            }
            AgentEvent::Thread(_) | AgentEvent::Steered(_) => {}
            AgentEvent::GoalChecked { answer, failed } => {
                self.save_session();
                self.on_goal_checked(answer, failed);
            }
            AgentEvent::Done => {
                self.task = None;
                self.approval = None;
                self.compacting = false;
                self.save_session();
                self.reload_diff();
                self.send_queued();
                if !self.is_running() {
                    self.after_goal_turn(self.turn_failed);
                }
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
