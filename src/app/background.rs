//! `/sessions`: the other sessions running in the background, to join or stop from here.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph};

use super::view::tilde;
use super::{App, AppEvent};
use crate::background::{self, State, Status};
use crate::session::{self, Entry};

/// Ticks between looking at what the sessions are up to: about a second.
const REFRESH_TICKS: usize = 20;

#[derive(Default)]
pub(super) struct SessionsPicker {
    pub(super) sessions: Vec<Status>,
    pub(super) selected: usize,
    /// A session whose turn would be cut short, waiting for a second `x`.
    pub(super) confirming: Option<u32>,
    /// Sessions told to stop that haven't gone yet.
    pub(super) stopping: Vec<u32>,
}

impl SessionsPicker {
    fn selected_id(&self) -> Option<u32> {
        self.sessions.get(self.selected).map(|s| s.id)
    }

    /// Takes in a new listing, staying on the same session if it's still there.
    pub(super) fn update(&mut self, sessions: Vec<Status>) {
        let selected = self.selected_id();
        self.sessions = sessions;
        self.selected = selected
            .and_then(|id| self.sessions.iter().position(|s| s.id == id))
            .unwrap_or(self.selected)
            .min(self.sessions.len().saturating_sub(1));
        let alive = |id: u32, sessions: &[Status]| sessions.iter().any(|s| s.id == id);
        if self.confirming.is_some_and(|id| !alive(id, &self.sessions)) {
            self.confirming = None;
        }
        let sessions = &self.sessions;
        self.stopping.retain(|id| alive(*id, sessions));
    }
}

impl App {
    /// The other open sessions: those from this directory first, then the latest first.
    fn other_sessions(&self) -> Vec<Status> {
        let mut sessions: Vec<Status> = background::list()
            .into_iter()
            .filter(|s| s.id != std::process::id())
            .collect();
        sessions.sort_by_key(|s| s.cwd != self.workspace.cwd);
        sessions
    }

    pub(super) fn open_sessions_picker(&mut self) {
        let sessions = self.other_sessions();
        if sessions.is_empty() {
            self.entries
                .push(Entry::Info("No other sessions are open.".into()));
            return;
        }
        self.sessions_picker = Some(SessionsPicker {
            sessions,
            ..SessionsPicker::default()
        });
    }

    /// Keeps an open picker current, since sessions go on while it's up.
    pub(super) fn refresh_sessions_picker(&mut self) {
        if self.sessions_picker.is_some() && self.tick.is_multiple_of(REFRESH_TICKS) {
            let sessions = self.other_sessions();
            if let Some(picker) = &mut self.sessions_picker {
                picker.update(sessions);
            }
        }
    }

    pub(super) fn on_sessions_picker_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(picker) = &mut self.sessions_picker else {
            return;
        };
        let last = picker.sessions.len().saturating_sub(1);
        // Anything but the second `x` calls off a stop.
        let confirming = picker.confirming.take();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.sessions_picker = None,
            KeyCode::Char('c') if ctrl => self.sessions_picker = None,
            KeyCode::Up | KeyCode::Char('k') => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Char('n') if ctrl => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Enter => {
                if let Some(id) = picker.selected_id() {
                    self.sessions_picker = None;
                    self.join_session(id);
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                let Some(session) = picker.sessions.get(picker.selected) else {
                    return;
                };
                let id = session.id;
                if picker.stopping.contains(&id) {
                    return;
                }
                if session.state != State::Idle && confirming != Some(id) {
                    picker.confirming = Some(id);
                    return;
                }
                picker.stopping.push(id);
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = background::client::stop(id)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(AppEvent::SessionStopped(id, result));
                });
            }
            _ => {}
        }
    }

    fn join_session(&mut self, id: u32) {
        if self.detachable {
            self.switch_request = Some(id);
        } else {
            self.entries.push(Entry::Error(format!(
                "This session runs in this terminal (--foreground), so it can't switch. Leave it \
                 and run: tamale attach {id}"
            )));
        }
    }

    pub(super) fn on_session_stopped(&mut self, id: u32, result: Result<(), String>) {
        match result {
            Ok(()) => self.entries.push(Entry::Info(format!(
                "Stopped session {id}; /resume can pick it back up."
            ))),
            Err(e) => self.entries.push(Entry::Error(e)),
        }
        let sessions = self.other_sessions();
        if let Some(picker) = &mut self.sessions_picker {
            picker.stopping.retain(|stopping| *stopping != id);
            picker.update(sessions);
        }
    }

    /// Where the terminal should go next, since the last time this was asked.
    pub fn take_switch_request(&mut self) -> Option<u32> {
        self.switch_request.take()
    }

    /// Whether leaving would lose nothing worth coming back to.
    pub fn is_blank_session(&self) -> bool {
        self.history.is_empty() && !self.is_running()
    }

    pub(super) fn draw_sessions_picker(&self, frame: &mut Frame, area: Rect) {
        let Some(picker) = &self.sessions_picker else {
            return;
        };
        let height = 2 * picker.sessions.len().max(1) as u16 + 2;
        let popup = area.centered(Constraint::Percentage(80), Constraint::Max(height));
        let hint = if picker.confirming.is_some() {
            Line::from(" x again to stop it mid-turn · any other key cancels ").fg(Color::Yellow)
        } else {
            Line::from(" ↑/↓ move · enter join · x stop · esc close ")
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Background sessions ".bold())
            .title_bottom(hint.right_aligned());
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);
        if picker.sessions.is_empty() {
            frame.render_widget(
                Paragraph::new(" No other sessions are open.".fg(Color::DarkGray)),
                inner,
            );
            return;
        }
        let items: Vec<ListItem> = picker
            .sessions
            .iter()
            .map(|s| {
                let (label, color) = if picker.stopping.contains(&s.id) {
                    ("stopping…", Color::DarkGray)
                } else {
                    match s.state {
                        State::Running => ("running", Color::Green),
                        State::NeedsApproval => ("needs approval", Color::Yellow),
                        State::Idle => ("idle", Color::DarkGray),
                    }
                };
                let attached = if s.attached { " · attached" } else { "" };
                let title = if s.title.is_empty() {
                    "(new session)"
                } else {
                    &s.title
                };
                ListItem::new(vec![
                    Line::from(vec![Span::from(title.to_string())]),
                    Line::from(vec![
                        Span::from(label).fg(color),
                        Span::from(format!(
                            " · {}{attached} · {} · {}",
                            session::ago(s.since),
                            tilde(&s.cwd),
                            s.id
                        ))
                        .fg(Color::DarkGray),
                    ]),
                ])
            })
            .collect();
        let list = List::new(items)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(picker.selected));
        frame.render_stateful_widget(list, inner, &mut state);
    }
}
