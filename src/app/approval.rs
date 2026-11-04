//! The popup asking whether a tool call may run.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use tokio::sync::oneshot;
use unicode_width::UnicodeWidthStr;

use super::App;
use crate::permissions::Approval;
use crate::session::Entry;

/// Keys typed this soon after the popup opens are ignored, so text being typed into the
/// prompt can't answer it by accident.
const KEY_GUARD: Duration = Duration::from_millis(300);
/// Lines of the call shown in the popup.
const MAX_CALL_LINES: usize = 12;

pub(super) struct Prompt {
    pub(super) name: String,
    pub(super) summary: String,
    pub(super) description: String,
    pub(super) always: Vec<String>,
    reply: Option<oneshot::Sender<Approval>>,
    selected: usize,
    /// `Some` while typing what to tell the model.
    pub(super) reason: Option<String>,
    pub(super) shown: Instant,
}

#[derive(Clone, Copy, PartialEq)]
enum Choice {
    Yes,
    Always,
    No,
}

impl Prompt {
    pub(super) fn new(
        name: String,
        summary: String,
        description: String,
        always: Vec<String>,
        reply: oneshot::Sender<Approval>,
    ) -> Self {
        Self {
            name,
            summary,
            description,
            always,
            reply: Some(reply),
            selected: 0,
            reason: None,
            shown: Instant::now(),
        }
    }

    fn choices(&self) -> Vec<Choice> {
        let mut choices = vec![Choice::Yes];
        if !self.always.is_empty() {
            choices.push(Choice::Always);
        }
        choices.push(Choice::No);
        choices
    }

    fn label(&self, choice: Choice) -> String {
        match choice {
            Choice::Yes => "Yes".into(),
            Choice::Always => format!(
                "Yes, and don't ask again this session for {}",
                self.always.join(", ")
            ),
            Choice::No => "No, and tell the model what to do instead".into(),
        }
    }
}

impl App {
    pub(super) fn on_approval_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(prompt) = &mut self.approval else {
            return;
        };
        let ctrl_c = ctrl && key.code == KeyCode::Char('c');
        if ctrl_c && self.detachable {
            // The prompt waits for whoever comes back.
            self.detach_requested = true;
            return;
        }
        if key.code == KeyCode::Esc && prompt.reason.is_none() || ctrl_c {
            self.interrupt();
            return;
        }
        if prompt.shown.elapsed() < KEY_GUARD {
            return;
        }
        if let Some(reason) = &mut prompt.reason {
            match key.code {
                KeyCode::Esc => prompt.reason = None,
                KeyCode::Enter => {
                    let reason = reason.trim().to_string();
                    self.answer(Approval::Deny {
                        reason: (!reason.is_empty()).then_some(reason),
                    });
                }
                KeyCode::Backspace => {
                    reason.pop();
                }
                KeyCode::Char(c) if !ctrl => reason.push(c),
                _ => {}
            }
            return;
        }
        let choices = prompt.choices();
        let last = choices.len() - 1;
        let choice = match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                prompt.selected = prompt.selected.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                prompt.selected = (prompt.selected + 1).min(last);
                None
            }
            KeyCode::Enter => Some(choices[prompt.selected]),
            KeyCode::Char('y') => Some(Choice::Yes),
            KeyCode::Char('a') if choices.contains(&Choice::Always) => Some(Choice::Always),
            KeyCode::Char('n') => Some(Choice::No),
            KeyCode::Char(c @ '1'..='3') => choices.get(c as usize - '1' as usize).copied(),
            _ => None,
        };
        match choice {
            Some(Choice::Yes) => self.answer(Approval::Allow),
            Some(Choice::Always) => self.allow_always(),
            Some(Choice::No) => {
                prompt.selected = last;
                prompt.reason = Some(String::new());
            }
            None => {}
        }
    }

    /// Allows the pending call and, for the rest of the session, calls like it.
    pub(super) fn allow_always(&mut self) {
        let Some(prompt) = &self.approval else {
            return;
        };
        let rules = prompt.always.clone();
        if !rules.is_empty() {
            self.permissions.lock().unwrap().allow_for_session(&rules);
            self.entries.push(Entry::Info(format!(
                "Allowed for this session: {}",
                rules.join(", ")
            )));
        }
        self.answer(Approval::Allow);
    }

    pub(super) fn answer(&mut self, approval: Approval) {
        if let Some(reply) = self.approval.take().and_then(|mut p| p.reply.take()) {
            let _ = reply.send(approval);
        }
    }

    pub(super) fn draw_approval(&self, frame: &mut Frame, area: Rect) {
        let Some(prompt) = &self.approval else {
            return;
        };
        let width = area.width.saturating_sub(4).max(20).min(area.width);
        let call_style = Style::new().fg(Color::Yellow);
        let mut lines = vec![Line::from(prompt.description.as_str().bold())];
        let call: Vec<&str> = prompt.summary.lines().collect();
        for (i, l) in call.iter().take(MAX_CALL_LINES).enumerate() {
            let prefix = if i == 0 && prompt.name == "bash" {
                "  $ "
            } else {
                "    "
            };
            lines.push(Line::from(vec![
                Span::from(prefix).fg(Color::DarkGray),
                Span::styled(*l, call_style),
            ]));
        }
        if call.len() > MAX_CALL_LINES {
            lines.push(Line::from(
                format!("    … +{} lines", call.len() - MAX_CALL_LINES).fg(Color::DarkGray),
            ));
        }
        lines.push(Line::default());
        for (i, choice) in prompt.choices().into_iter().enumerate() {
            let text = format!("{}. {}", i + 1, prompt.label(choice));
            lines.push(if i == prompt.selected {
                Line::from(vec![
                    Span::from("› ").fg(Color::Cyan),
                    text.fg(Color::Cyan).bold(),
                ])
            } else {
                Line::from(format!("  {text}"))
            });
        }
        let mut cursor_line = None;
        if let Some(reason) = &prompt.reason {
            lines.push(Line::default());
            cursor_line = Some(lines.len());
            lines.push(Line::from(vec![
                Span::from("  Tell the model: ").fg(Color::DarkGray),
                Span::from(reason.as_str()),
            ]));
        }
        let hint = if prompt.reason.is_some() {
            " enter deny (empty: just deny) · esc back "
        } else {
            " ↑/↓ move · enter select · y/a/n · esc interrupt "
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Yellow))
            .title(format!(" Allow {}? ", prompt.name).bold())
            .title_bottom(Line::from(hint).right_aligned());
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        let height = (paragraph.line_count(width.saturating_sub(2)) as u16 + 2).min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.bottom().saturating_sub(height),
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        frame.render_widget(paragraph.block(block), popup);
        if let (Some(row), Some(reason)) = (cursor_line, &prompt.reason) {
            let x = popup.x + 1 + "  Tell the model: ".len() as u16 + reason.width() as u16;
            // Exact when the earlier lines didn't wrap, which is the common case.
            let y = popup.y + 1 + row as u16;
            if x < popup.right() && y < popup.bottom().saturating_sub(1) {
                frame.set_cursor_position(Position::new(x, y));
            }
        }
    }
}
