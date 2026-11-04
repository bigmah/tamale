//! Driving a session on another machine: its transcript, approvals and a prompt, in place of
//! this session's, which goes on behind it. Picked from the `/sessions` list.

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use serde_json::json;
use unicode_width::UnicodeWidthStr;

use super::transcript::Transcript;
use super::view::{fmt_tokens, wrap_input};
use super::{App, AppEvent, WHEEL_LINES, activity};
use crate::remote::{Approval, Connection, Event as RemoteEvent, HubSession, Incoming, Status};
use crate::session::Entry;

/// How long a new approval ignores keys, so typing can't answer it by accident.
const KEY_GUARD: Duration = Duration::from_millis(300);

enum Link {
    Connecting(Option<String>),
    Live,
    Stopped(String),
}

struct Prompt {
    approval: Approval,
    shown: Instant,
    /// What to tell the model, once `n` has been pressed.
    reason: Option<String>,
}

pub(super) struct RemoteView {
    connection: Connection,
    generation: u64,
    session: HubSession,
    link: Link,
    entries: Vec<Entry>,
    transcript: Transcript,
    status: Status,
    prompt: Option<Prompt>,
    scroll_back: usize,
    /// This session's draft, while the prompt belongs to the other one.
    pub(super) draft: (String, usize),
}

impl App {
    pub(super) fn open_remote_view(&mut self, session: HubSession) {
        if !session.e2e {
            self.entries.push(Entry::Error(format!(
                "{} isn't encrypted ([hub] e2e = false there), so it only opens in a browser.",
                session.host
            )));
            return;
        }
        self.remote_generation += 1;
        let generation = self.remote_generation;
        let tx = self.tx.clone();
        let connection = Connection::open(&self.settings.hub, &session.sid, move |event| {
            let _ = tx.send(AppEvent::Remote(generation, event));
        });
        let connection = match connection {
            Ok(connection) => connection,
            Err(message) => {
                self.entries.push(Entry::Error(message));
                return;
            }
        };
        let draft = (std::mem::take(&mut self.input), std::mem::take(&mut self.cursor));
        self.remote_view = Some(RemoteView {
            connection,
            generation,
            session,
            link: Link::Connecting(None),
            entries: Vec::new(),
            transcript: Transcript::default(),
            status: Status::default(),
            prompt: None,
            scroll_back: 0,
            draft,
        });
    }

    /// Back to this session; the other goes on without this terminal.
    fn close_remote_view(&mut self) {
        if let Some(view) = self.remote_view.take() {
            (self.input, self.cursor) = view.draft;
        }
    }

    pub(super) fn on_remote_event(&mut self, generation: u64, event: RemoteEvent) {
        let Some(view) = self
            .remote_view
            .as_mut()
            .filter(|view| view.generation == generation)
        else {
            return;
        };
        match event {
            RemoteEvent::Connecting(why) => view.link = Link::Connecting(why),
            RemoteEvent::Live => view.link = Link::Live,
            RemoteEvent::Stopped(why) => view.link = Link::Stopped(why),
            RemoteEvent::Message(Incoming::Entries { from, items }) => {
                let from = from.min(view.entries.len());
                view.entries.truncate(from);
                view.entries.extend(items);
                view.transcript.invalidate_from(from);
            }
            RemoteEvent::Message(Incoming::Status(status)) => view.status = *status,
            RemoteEvent::Message(Incoming::Approval(approval)) => {
                let same = |prompt: &Prompt| Some(&prompt.approval) == approval.as_ref();
                if !view.prompt.as_ref().is_some_and(same) {
                    view.prompt = approval.map(|approval| Prompt {
                        approval,
                        shown: Instant::now(),
                        reason: None,
                    });
                }
            }
        }
    }

    /// Input while driving another session.
    pub(super) fn on_remote_input(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.on_remote_key(key),
            Event::Paste(text) => {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                match self.remote_view.as_mut().and_then(|v| v.prompt.as_mut()) {
                    Some(Prompt { reason: Some(reason), .. }) => {
                        reason.push_str(&text.replace('\n', " "));
                    }
                    Some(_) => {}
                    None => self.insert_str(&text),
                }
            }
            Event::Mouse(mouse) => {
                if let Some(view) = &mut self.remote_view {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => view.scroll_back += WHEEL_LINES,
                        MouseEventKind::ScrollDown => {
                            view.scroll_back = view.scroll_back.saturating_sub(WHEEL_LINES);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn on_remote_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if ctrl && key.code == KeyCode::Char('c') {
            self.close_remote_view();
            return;
        }
        let Some(view) = &mut self.remote_view else {
            return;
        };
        let live = matches!(view.link, Link::Live);
        if let Some(prompt) = &mut view.prompt {
            on_prompt_key(prompt, &view.connection, key, ctrl, live);
            return;
        }
        match key.code {
            KeyCode::Esc if view.status.running && live => {
                view.connection.send(json!({ "t": "interrupt" }));
            }
            KeyCode::Esc => self.close_remote_view(),
            KeyCode::Enter if !alt && !shift => self.send_to_remote(),
            KeyCode::PageUp => view.scroll_back += 10,
            KeyCode::PageDown => view.scroll_back = view.scroll_back.saturating_sub(10),
            KeyCode::Up if shift => view.scroll_back += 1,
            KeyCode::Down if shift => view.scroll_back = view.scroll_back.saturating_sub(1),
            KeyCode::Up => self.cursor_up(),
            KeyCode::Down => {
                // There are no tabs to go down to here.
                self.cursor_down();
                self.tab_focus = None;
            }
            _ => self.on_edit_key(key, ctrl, alt, shift),
        }
    }

    fn send_to_remote(&mut self) {
        let text = self.expand_pastes(self.input.trim());
        if text.is_empty() {
            return;
        }
        // Means this terminal's list, not one the other machine would open out of sight.
        if text == "/sessions" {
            self.close_remote_view();
            self.input.clear();
            self.cursor = 0;
            self.open_sessions_picker();
            return;
        }
        let Some(view) = &mut self.remote_view else {
            return;
        };
        // Kept in the prompt until it can go.
        if !matches!(view.link, Link::Live) {
            return;
        }
        view.connection.send(json!({ "t": "send", "text": text }));
        view.scroll_back = 0;
        self.prompts.add(&text);
        self.input.clear();
        self.cursor = 0;
        self.prune_pastes();
    }

    pub(super) fn draw_remote_view(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let local_waiting = self.waiting_for_user();
        let (rows, cursor) = wrap_input(
            &self.input,
            self.cursor,
            area.width.saturating_sub(4).max(1) as usize,
        );
        let input_height = (rows.len() as u16).clamp(1, 10) + 2;
        let verbose = self.settings.ui.verbose;
        let Some(view) = &mut self.remote_view else {
            return;
        };
        let queued = view.status.queued.len().min(3) as u16;
        let [chat_area, queued_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(queued),
            Constraint::Length(input_height),
            Constraint::Length(2),
        ])
        .areas(area);

        view.draw_chat(frame, chat_area, verbose);
        let waiting: Vec<Line> = view
            .status
            .queued
            .iter()
            .take(queued as usize)
            .map(|text| Line::from(format!(" queued: {text}")).fg(Color::DarkGray))
            .collect();
        frame.render_widget(Paragraph::new(waiting), queued_area);

        let visible = input_height.saturating_sub(2) as usize;
        let skip = (cursor.0 + 1).saturating_sub(visible);
        let lines: Vec<Line> = rows
            .into_iter()
            .enumerate()
            .skip(skip)
            .map(|(i, row)| {
                let prefix = if i == 0 { "› " } else { "  " };
                Line::from(vec![Span::from(prefix).fg(Color::Blue), Span::from(row)])
            })
            .collect();
        let live = matches!(view.link, Link::Live);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(if live { Color::Blue } else { Color::DarkGray }))
            .title(format!(" ⇄ {} ", session_name(&view.session)).fg(Color::Blue));
        frame.render_widget(Paragraph::new(lines).block(block), input_area);
        if view.prompt.is_none() {
            frame.set_cursor_position(Position::new(
                input_area.x + 3 + cursor.1 as u16,
                input_area.y + 1 + (cursor.0 - skip) as u16,
            ));
        }
        view.draw_status(frame, status_area, local_waiting);
        view.draw_prompt(frame, chat_area);
    }
}

fn on_prompt_key(prompt: &mut Prompt, connection: &Connection, key: KeyEvent, ctrl: bool, live: bool) {
    if key.code == KeyCode::Esc && prompt.reason.is_none() {
        if live {
            connection.send(json!({ "t": "interrupt" }));
        }
        return;
    }
    if prompt.shown.elapsed() < KEY_GUARD || !live {
        return;
    }
    let approve = |choice: &str, reason: &str| {
        connection.send(json!({ "t": "approve", "choice": choice, "reason": reason }));
    };
    if let Some(reason) = &mut prompt.reason {
        match key.code {
            KeyCode::Esc => prompt.reason = None,
            KeyCode::Enter => approve("no", reason.trim()),
            KeyCode::Backspace => {
                reason.pop();
            }
            KeyCode::Char(c) if !ctrl => reason.push(c),
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Char('y') => approve("yes", ""),
        KeyCode::Char('a') if !prompt.approval.always.is_empty() => approve("always", ""),
        KeyCode::Char('n') => prompt.reason = Some(String::new()),
        _ => {}
    }
}

/// What a session goes by: where it runs, and what it's about once it's about something.
pub(super) fn session_name(session: &HubSession) -> String {
    let place = [session.host.as_str(), session.cwd.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(":");
    if place.is_empty() {
        session.sid.chars().take(8).collect()
    } else {
        place
    }
}

impl RemoteView {
    fn draw_chat(&mut self, frame: &mut Frame, area: Rect, verbose: bool) {
        if area.is_empty() {
            return;
        }
        self.transcript.sync(&self.entries, area.width, verbose);
        let running = self.status.running;
        let (label, palette) = if self.status.compacting {
            ("Compacting…", activity::COMPACTING)
        } else if self.prompt.is_some() {
            ("Waiting for approval…", activity::WAITING)
        } else {
            ("Thinking…", activity::THINKING)
        };
        let elapsed = self
            .status
            .since
            .map(|since| Duration::from_secs(crate::session::now().saturating_sub(since)))
            .unwrap_or_default();
        let spinner = activity::line(label, palette, elapsed, self.prompt.is_none());
        let spinner_rows = if running {
            Paragraph::new(spinner.clone())
                .wrap(Wrap { trim: false })
                .line_count(area.width)
        } else {
            0
        };
        let rows = self.transcript.rows();
        let max_scroll = (rows + spinner_rows).saturating_sub(area.height as usize);
        self.scroll_back = self.scroll_back.min(max_scroll);
        let offset = max_scroll - self.scroll_back;
        let (mut lines, local) = self.transcript.visible(offset.min(rows), area.height as usize);
        if running {
            lines.push(spinner);
        }
        let local = if offset > rows { (offset - rows) as u16 } else { local };
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((local, 0)),
            area,
        );
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect, local_waiting: bool) {
        let status = &self.status;
        let (state, color) = match &self.link {
            Link::Live => ("● connected".to_string(), Color::Green),
            Link::Connecting(None) => ("connecting…".to_string(), Color::Yellow),
            Link::Connecting(Some(why)) => (format!("{why} Retrying…"), Color::Yellow),
            Link::Stopped(why) => (why.clone(), Color::Red),
        };
        let top = Line::from(vec![
            Span::from(format!(" {state}")).fg(color),
            Span::from(format!(
                " · {} · {} · ctx {}/{} · ${:.2}",
                if status.model.is_empty() { &self.session.model } else { &status.model },
                status.mode,
                fmt_tokens(status.context),
                fmt_tokens(status.window),
                status.cost,
            ))
            .fg(Color::DarkGray),
        ]);
        let hint = if self.prompt.is_some() {
            " y yes · n no · esc interrupt · ctrl+c back to this session"
        } else if status.running {
            " esc to interrupt · ctrl+c back to this session"
        } else {
            " esc back to this session"
        };
        let mut bottom = vec![Span::from(hint).fg(Color::DarkGray)];
        if local_waiting {
            bottom.push(Span::from("  · this session needs approval").fg(Color::Yellow));
        }
        frame.render_widget(Paragraph::new(vec![top, Line::from(bottom)]), area);
    }

    fn draw_prompt(&self, frame: &mut Frame, area: Rect) {
        let Some(prompt) = &self.prompt else {
            return;
        };
        let approval = &prompt.approval;
        let mut lines = vec![Line::from(approval.description.as_str().bold())];
        for line in approval.summary.lines().take(8) {
            lines.push(Line::from(format!("    {line}")).fg(Color::Yellow));
        }
        lines.push(Line::default());
        let mut choices = vec![Span::from("y").fg(Color::Cyan).bold(), Span::from(" yes   ")];
        if !approval.always.is_empty() {
            choices.push(Span::from("a").fg(Color::Cyan).bold());
            choices.push(Span::from(format!(" always: {}   ", approval.always.join(", "))));
        }
        choices.push(Span::from("n").fg(Color::Cyan).bold());
        choices.push(Span::from(" no"));
        lines.push(Line::from(choices));
        const ASK: &str = "  Tell the model: ";
        if let Some(reason) = &prompt.reason {
            lines.push(Line::from(vec![
                Span::from(ASK).fg(Color::DarkGray),
                Span::from(reason.as_str()),
            ]));
        }
        let width = area.width.saturating_sub(4).max(20).min(area.width);
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        let height = (paragraph.line_count(width.saturating_sub(2)) as u16 + 2).min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.bottom().saturating_sub(height),
            width,
            height,
        );
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Yellow))
            .title(format!(" Approve on {} ", self.session.host).bold());
        frame.render_widget(Clear, popup);
        frame.render_widget(paragraph.block(block), popup);
        if let Some(reason) = &prompt.reason {
            let x = popup.x + 1 + ASK.len() as u16 + reason.width() as u16;
            let y = popup.bottom().saturating_sub(2);
            if x < popup.right() {
                frame.set_cursor_position(Position::new(x, y));
            }
        }
    }
}
