//! `/sessions`: the other sessions on this machine, to join or stop from here, and those on
//! other machines through the hub, to drive from here (see `remote.rs`).

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState};

use super::view::tilde;
use super::{App, AppEvent};
use super::remote::session_name;
use crate::background::{self, State, Status};
use crate::remote::{self, HubSession};
use crate::session::{self, Entry};

/// Ticks between looking at what the sessions are up to: about a second.
const REFRESH_TICKS: usize = 20;
/// The same for the hub, which is further away: about five seconds.
const HUB_REFRESH_TICKS: usize = 100;

/// What's known of the sessions on other machines.
#[derive(Default)]
pub(super) enum HubList {
    #[default]
    Loading,
    /// Why there's no asking: no key, or a problem with it. Says what to do about it.
    Unavailable(String),
    Failed(String),
    Loaded(Vec<HubSession>),
}

/// A row that can be picked.
#[derive(Clone, PartialEq)]
enum Row {
    Local(u32),
    Hub(String),
}

#[derive(Default)]
pub(super) struct SessionsPicker {
    pub(super) sessions: Vec<Status>,
    pub(super) selected: usize,
    /// A session whose turn would be cut short, waiting for a second `x`.
    pub(super) confirming: Option<u32>,
    /// Sessions told to stop that haven't gone yet.
    pub(super) stopping: Vec<u32>,
    pub(super) hub: HubList,
    /// Whether a look at the hub is under way.
    pub(super) hub_loading: bool,
}

impl SessionsPicker {
    /// The hub's sessions to show: a session on this machine is reached directly instead.
    fn hub_sessions(&self) -> Vec<&HubSession> {
        let HubList::Loaded(sessions) = &self.hub else {
            return Vec::new();
        };
        sessions
            .iter()
            .filter(|h| !self.sessions.iter().any(|s| s.sid == h.sid))
            .collect()
    }

    fn rows(&self) -> Vec<Row> {
        self.sessions
            .iter()
            .map(|s| Row::Local(s.id))
            .chain(self.hub_sessions().iter().map(|h| Row::Hub(h.sid.clone())))
            .collect()
    }

    /// Runs `change`, staying on the same session if it's still there.
    fn keeping_place(&mut self, change: impl FnOnce(&mut Self)) {
        let selected = self.rows().get(self.selected).cloned();
        change(self);
        let rows = self.rows();
        self.selected = selected
            .and_then(|row| rows.iter().position(|r| *r == row))
            .unwrap_or(self.selected)
            .min(rows.len().saturating_sub(1));
    }

    /// Takes in a new listing of this machine's sessions.
    pub(super) fn update(&mut self, sessions: Vec<Status>) {
        self.keeping_place(|picker| picker.sessions = sessions);
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
        self.sessions_picker = Some(SessionsPicker {
            sessions: self.other_sessions(),
            ..SessionsPicker::default()
        });
        self.refresh_hub_sessions();
    }

    /// Asks the hub what's there, unless it's being asked or there's no key to ask with.
    fn refresh_hub_sessions(&mut self) {
        let hub = self.settings.hub.clone();
        let Some(picker) = &mut self.sessions_picker else {
            return;
        };
        if let Err(message) = hub.key_bytes() {
            picker.hub = HubList::Unavailable(message);
            return;
        }
        if picker.hub_loading {
            return;
        }
        picker.hub_loading = true;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(AppEvent::HubSessions(remote::sessions(&hub).await));
        });
    }

    pub(super) fn on_hub_sessions(&mut self, result: Result<Vec<HubSession>, String>) {
        let Some(picker) = &mut self.sessions_picker else {
            return;
        };
        picker.hub_loading = false;
        let list = match result {
            Ok(mut sessions) => {
                sessions.retain(|s| s.sid != crate::web::sid());
                HubList::Loaded(sessions)
            }
            Err(message) => HubList::Failed(message),
        };
        picker.keeping_place(|picker| picker.hub = list);
    }

    /// Keeps an open picker current, since sessions go on while it's up.
    pub(super) fn refresh_sessions_picker(&mut self) {
        if self.sessions_picker.is_some() && self.tick.is_multiple_of(REFRESH_TICKS) {
            let sessions = self.other_sessions();
            if let Some(picker) = &mut self.sessions_picker {
                picker.update(sessions);
            }
        }
        if self.sessions_picker.is_some() && self.tick.is_multiple_of(HUB_REFRESH_TICKS) {
            self.refresh_hub_sessions();
        }
    }

    pub(super) fn on_sessions_picker_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(picker) = &mut self.sessions_picker else {
            return;
        };
        let rows = picker.rows();
        let last = rows.len().saturating_sub(1);
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
                match rows.get(picker.selected) {
                    Some(Row::Local(id)) => {
                        let id = *id;
                        self.sessions_picker = None;
                        self.join_session(id);
                    }
                    Some(Row::Hub(sid)) => {
                        let session = picker.hub_sessions().into_iter().find(|h| h.sid == *sid).cloned();
                        self.sessions_picker = None;
                        if let Some(session) = session {
                            self.open_remote_view(session);
                        }
                    }
                    None => {}
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
        let dim = Color::DarkGray;
        let heading = |text: &'static str| ListItem::new(Line::from(text.bold().fg(Color::Blue)));
        let note = |text: String, color| ListItem::new(Line::from(format!("  {text}")).fg(color));
        let mut items = vec![heading(" This machine")];
        let mut rows = Vec::new();
        if picker.sessions.is_empty() {
            items.push(note("No other sessions are open.".into(), dim));
        }
        for s in &picker.sessions {
            let (label, color) = if picker.stopping.contains(&s.id) {
                ("stopping…", dim)
            } else {
                match s.state {
                    State::Running => ("running", Color::Green),
                    State::NeedsApproval => ("needs approval", Color::Yellow),
                    State::Idle => ("idle", dim),
                }
            };
            let attached = if s.attached { " · attached" } else { "" };
            let detail = format!(
                " · {}{attached} · {} · {}",
                session::ago(s.since),
                tilde(&s.cwd),
                s.id
            );
            rows.push(items.len());
            items.push(session_item(&s.title, label, color, detail));
        }
        items.push(heading(" Through the hub"));
        let hub_sessions = picker.hub_sessions();
        match &picker.hub {
            HubList::Loading => items.push(note("Looking…".into(), dim)),
            HubList::Unavailable(message) => items.push(note(message.clone(), dim)),
            HubList::Failed(message) => items.push(note(message.clone(), Color::Red)),
            HubList::Loaded(_) if hub_sessions.is_empty() => {
                items.push(note("No sessions there right now.".into(), dim));
            }
            HubList::Loaded(_) => {}
        }
        for h in &hub_sessions {
            let (label, color) = if !h.online {
                ("offline", dim)
            } else if h.approval {
                ("needs approval", Color::Yellow)
            } else if h.busy {
                ("running", Color::Green)
            } else {
                ("idle", dim)
            };
            let viewing = match h.viewers {
                0 => String::new(),
                n => format!(" · {n} viewing"),
            };
            let detail = format!(" · {} · {}{viewing}", session_name(h), h.model);
            rows.push(items.len());
            items.push(session_item(&h.title, label, color, detail));
        }

        let height = items.iter().map(|i| i.height() as u16).sum::<u16>() + 2;
        let popup = area.centered(Constraint::Percentage(80), Constraint::Max(height));
        let on_hub = picker.selected >= picker.sessions.len();
        let hint = if picker.confirming.is_some() {
            Line::from(" x again to stop it mid-turn · any other key cancels ").fg(Color::Yellow)
        } else if on_hub && !rows.is_empty() {
            Line::from(" ↑/↓ move · enter drive it from here · esc close ")
        } else {
            Line::from(" ↑/↓ move · enter join · x stop · esc close ")
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Sessions ".bold())
            .title_bottom(hint.right_aligned());
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);
        let list = List::new(items)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(rows.get(picker.selected).copied());
        frame.render_stateful_widget(list, inner, &mut state);
    }
}

fn session_item(title: &str, label: &'static str, color: Color, detail: String) -> ListItem<'static> {
    let title = if title.is_empty() { "(new session)" } else { title };
    ListItem::new(vec![
        Line::from(title.to_string()),
        Line::from(vec![
            Span::from(label).fg(color),
            Span::from(detail).fg(Color::DarkGray),
        ]),
    ])
}
