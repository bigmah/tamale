//! What the `/web` UI shows of the session, and the input it sends back, and the
//! terminal's screen while it is served.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::{App, SPINNER, WebRequest};
use crate::permissions::Approval;
use crate::session::Entry;
use crate::web;

impl App {
    pub fn take_web_request(&mut self) -> Option<WebRequest> {
        self.web_request.take()
    }

    /// Asks to serve the web UI to the local network, as `/web`, ctrl+o and the Web tab do.
    pub(super) fn open_web(&mut self) {
        self.web_request = Some(WebRequest::Open { lan: true });
    }

    /// Asks to connect to the hub and show its code, as `/remote` and the Remote tab do.
    pub(super) fn open_remote(&mut self) {
        self.web_request = Some(WebRequest::Remote { on: true });
    }

    /// `/remote <key>`: uses a key from the hub's site for this run, for when tamale was
    /// started without `TAMALE_HUB_KEY`, then connects. The conversation carries on.
    pub(super) fn set_hub_key(&mut self, key: &str) {
        if self.hub_viewers.is_some() {
            self.entries.push(Entry::Error(
                "Already connected to the hub; /remote off first to use another key.".into(),
            ));
            return;
        }
        if tamale_hub_protocol::decode_key(key).is_none() {
            self.entries.push(Entry::Error(format!(
                "That isn't a key (thk_…). Make one at {}",
                self.settings.hub.site()
            )));
            return;
        }
        self.settings.hub.key = Some(key.to_string());
        self.settings.hub.old_token = false;
        self.entries.push(Entry::Info(
            "Using that hub key for this session. Set TAMALE_HUB_KEY in your shell profile to \
have it next time."
                .into(),
        ));
        self.open_remote();
    }

    /// Whether there's a hub key to connect with.
    pub(super) fn hub_key_set(&self) -> bool {
        self.settings.hub.key_bytes().is_ok()
    }

    /// Reports the outcome of a `/web` or `/remote` request.
    pub fn on_web(&mut self, result: Result<String, String>) {
        self.entries.push(match result {
            Ok(message) => Entry::Info(message),
            Err(message) => Entry::Error(message),
        });
    }

    pub fn web_view(&self) -> web::View<'_> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let unix = |at: std::time::Instant| now.saturating_sub(at.elapsed()).as_secs();
        let busy = self.busy_since.filter(|_| self.is_running());
        let since = busy.map(unix);
        // A step from an earlier run started before this one: the run's start is its start.
        let step_since = busy.map(|busy| unix(self.step_since.map_or(busy, |step| step.max(busy))));
        let step = self.activity_step();
        let cwd = &self.workspace.cwd;
        let status = web::Status {
            project: cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| cwd.display().to_string()),
            cwd: cwd.display().to_string(),
            dirs: self
                .workspace
                .additional_dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect(),
            model: self.backend.model_label(&self.settings.model.name),
            backend: self.backend_label(),
            signed_in: self.backend.signed_in(),
            mode: self.permissions.lock().unwrap().mode.as_str(),
            effort: self.settings.model.effort.map(|e| e.as_str()),
            thinking: self.settings.model.thinking.as_str(),
            running: self.is_running(),
            compacting: self.compacting,
            since,
            step_since,
            step: if self.is_running() {
                step.label
            } else {
                String::new()
            },
            step_kind: step.kind.as_str(),
            steps: step.steps,
            sub_steps: step.sub_steps,
            context: self.context_tokens,
            window: self.settings.model.context_window(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cost: self.cost,
            cost_partial: self.cost_partial,
            goal: self.goal.as_ref().map(|g| g.condition.clone()),
            queued: self.queued_messages(),
        };
        let approval = self.approval.as_ref().map(|p| web::Approval {
            name: p.name.clone(),
            summary: p.summary.clone(),
            description: p.description.clone(),
            always: p.always.clone(),
        });
        let question = self.question.as_ref().map(|p| web::Questions {
            description: p.description.clone(),
            questions: p.questions.clone(),
        });
        let mut roots = vec![self.workspace.cwd.clone()];
        roots.extend(self.workspace.additional_dirs.iter().cloned());
        web::View {
            status,
            entries: &self.entries,
            approval,
            question,
            roots,
            artifact_dir: web::artifact_dir(&self.session_id),
        }
    }

    /// A prompt or slash command from the web UI, handled as if typed here.
    pub fn web_send(&mut self, text: String) {
        let text = text.trim().to_string();
        if self.question.is_some() && !text.is_empty() && !text.starts_with('/') {
            // There's no picker there, so a message answers the agent's questions.
            self.answer_questions_with(text);
        } else if !text.is_empty() {
            self.plain_prompts = false;
            self.submit_text(text);
        }
    }

    /// The same from another terminal driving this session, which wants ordinary replies.
    pub fn terminal_send(&mut self, text: String) {
        let text = text.trim().to_string();
        if self.question.is_some() && !text.is_empty() && !text.starts_with('/') {
            self.answer_questions_with(text);
        } else if !text.is_empty() {
            self.plain_prompts = true;
            self.submit_text(text);
        }
    }

    /// Starts or ends the web session: while it lasts, the terminal shows
    /// [`Self::draw_web_session`] instead of the TUI.
    pub fn set_web_session(&mut self, on: bool) {
        self.web_session = on;
    }

    pub fn is_web_session(&self) -> bool {
        self.web_session
    }

    /// Whether prompts ask for replies written for the web UI: during a web session, or
    /// while a browser or phone is watching through the hub from behind the TUI.
    pub(super) fn writes_for_web(&self) -> bool {
        !self.plain_prompts
            && (self.web_session || (self.hub_viewers.is_some() && self.web_viewers > 0))
    }

    /// What the footer's hub indicator shows: `None` when not connected to a hub.
    pub fn set_hub_viewers(&mut self, viewers: Option<usize>) {
        self.hub_viewers = viewers;
    }

    pub fn set_web_viewers(&mut self, viewers: usize) {
        self.web_viewers = viewers;
    }

    pub fn set_hub_live(&mut self, live: bool) {
        self.hub_live = live;
    }

    /// Whether the session is reachable through the hub right now.
    pub(super) fn remote_on(&self) -> bool {
        self.hub_viewers.is_some() && self.hub_live
    }

    pub fn settings_hub(&self) -> &crate::config::HubSettings {
        &self.settings.hub
    }

    /// What the hub's session list shows of this session besides its status.
    pub fn hub_label(&self) -> web::Label {
        let cwd = self.workspace.cwd.display().to_string();
        let cwd = match std::env::var("HOME") {
            Ok(home) if !home.is_empty() && cwd.starts_with(&home) => {
                format!("~{}", &cwd[home.len()..])
            }
            _ => cwd,
        };
        web::Label {
            host: web::hostname(),
            cwd,
            model: self.backend.model_label(&self.settings.model.name),
        }
    }

    /// The terminal's screen during a web session: where the session is, what it's doing,
    /// and the few keys that work here.
    pub fn draw_web_session(
        &self,
        frame: &mut Frame,
        url: &str,
        remote: Option<web::HubState<'_>>,
        tabs: usize,
    ) {
        let dim = Style::new().fg(Color::DarkGray);
        let key = |k: &'static str| Span::from(k).fg(Color::Cyan).bold();
        let hub = remote.is_some();
        // Esc only stops serving what the TUI wouldn't show is still open (see main.rs).
        let esc = if hub && url.starts_with("http://127.0.0.1") {
            " back to the session   "
        } else {
            " stop serving, back to the terminal   "
        };
        let state = if self.approval.is_some() {
            Line::from(vec![
                Span::from("● ").fg(Color::Yellow),
                Span::from("Waiting for approval in the browser").fg(Color::Yellow),
            ])
        } else if self.question.is_some() {
            Line::from(vec![
                Span::from("● ").fg(Color::Yellow),
                Span::from("The agent asked a question; reply in the browser").fg(Color::Yellow),
            ])
        } else if self.is_running() {
            let elapsed = self.busy_since.map_or(0, |s| s.elapsed().as_secs());
            Line::from(vec![
                Span::from(format!("{} ", SPINNER[self.tick % SPINNER.len()])).fg(Color::Magenta),
                Span::from("Working").fg(Color::Magenta),
                Span::styled(format!("  {elapsed}s"), dim),
            ])
        } else {
            Line::from(vec![
                Span::from("○ ").fg(Color::Green),
                Span::styled("Idle", dim),
            ])
        };
        let tabs = match tabs {
            0 => "No browser tab connected".to_string(),
            1 => "1 browser tab connected".to_string(),
            n => format!("{n} browser tabs connected"),
        };
        let mut lines = vec![
            Line::from(vec![
                Span::from("🫔 tamale").bold(),
                Span::styled("  ·  web session", dim),
            ]),
            Line::default(),
            Line::styled("This session is running in your browser at", dim),
            Line::from(Span::from(url.to_string()).fg(Color::Blue).underlined()),
            Line::default(),
        ];
        if let Some(web::HubState::Live(remote)) = remote {
            lines.extend([
                Line::styled(
                    "and anywhere through the hub, on browsers holding this key, at",
                    dim,
                ),
                Line::from(Span::from(remote.to_string()).fg(Color::Blue).underlined()),
                Line::default(),
            ]);
            lines.extend(qr_lines(remote));
            lines.push(Line::default());
        } else if let Some(web::HubState::Connecting(problem)) = remote {
            // The conversation is hidden behind this screen, so what's wrong shows here.
            lines.push(match problem {
                Some(problem) => Line::from(Span::from(problem).fg(Color::Red)),
                None => Line::styled("Connecting to the hub…", dim),
            });
            lines.push(Line::default());
        } else if !url.starts_with("http://127.0.0.1") {
            // Served to the network: a code to scan beats typing the token on a phone.
            lines.push(Line::styled(
                "Scan to open it on a device on this network (anyone on it who sees the link can use it)",
                dim,
            ));
            lines.extend(qr_lines(url));
            lines.push(Line::default());
        }
        let mut keys = vec![
            key("o"),
            Span::styled(" open in browser   ", dim),
            key("c"),
            Span::styled(" copy link   ", dim),
            key("esc"),
            Span::styled(esc, dim),
        ];
        if hub {
            keys.extend([key("x"), Span::styled(" turn remote off   ", dim)]);
        }
        keys.extend([
            key("ctrl+c"),
            Span::styled(
                if self.detachable && self.is_running() {
                    " detach"
                } else {
                    " quit"
                },
                dim,
            ),
        ]);
        lines.extend([
            state,
            Line::styled(
                format!(
                    "{tabs} · {} · ${:.2}",
                    self.backend.model_label(&self.settings.model.name),
                    self.cost
                ),
                dim,
            ),
            Line::default(),
            Line::from(keys),
        ]);
        let area = frame.area();
        let body = Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false });
        let height = body.line_count(area.width) as u16;
        let [_, middle, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .areas(area);
        frame.render_widget(body, middle);
    }

    pub fn web_interrupt(&mut self) {
        self.interrupt();
    }

    /// Answers from the web UI or the iPhone app's question picker.
    pub fn web_answer(&mut self, answers: Vec<crate::tools::ask::Answer>) {
        self.answer_questions(answers);
    }

    pub fn web_approve(&mut self, choice: web::Choice, reason: Option<String>) {
        if self.approval.is_none() {
            return;
        }
        match choice {
            web::Choice::Yes => self.answer(Approval::Allow),
            web::Choice::Always => self.allow_always(),
            web::Choice::No => self.answer(Approval::Deny { reason }),
        }
    }
}

/// `text` as a QR code in half-block characters, dark on light so it scans on any theme.
fn qr_lines(text: &str) -> Vec<Line<'static>> {
    let Ok(code) = qrcode::QrCode::new(text) else {
        return Vec::new();
    };
    let width = code.width();
    let colors = code.to_colors();
    // A quiet zone of two modules on each side, which scanners need.
    let quiet = 2;
    let dark = |x: usize, y: usize| {
        x >= quiet
            && y >= quiet
            && x - quiet < width
            && y - quiet < width
            && colors[(y - quiet) * width + x - quiet] == qrcode::Color::Dark
    };
    let size = width + 2 * quiet;
    let style = Style::new().fg(Color::Black).bg(Color::White);
    (0..size)
        .step_by(2)
        .map(|y| {
            let row: String = (0..size)
                .map(|x| match (dark(x, y), dark(x, y + 1)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                })
                .collect();
            Line::styled(row, style)
        })
        .collect()
}
