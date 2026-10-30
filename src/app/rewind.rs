//! `/undo` and `/rewind`: restore checkpointed files and truncate the conversation.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState};
use rig_core::completion::Message;

use super::App;
use super::view::truncate_chars;
use crate::checkpoint;
use crate::session::{self, Entry};

pub(super) struct Picker {
    /// Row 0 is the most recent turn.
    selected: usize,
    /// Files changed and commands run per turn, newest first.
    activity: Vec<(usize, usize)>,
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

impl App {
    fn can_rewind(&mut self) -> bool {
        if self.is_running() {
            self.entries.push(Entry::Error(
                "Can't rewind while the agent is running (Esc to interrupt first).".into(),
            ));
            return false;
        }
        if self.checkpoints.is_empty() {
            self.entries
                .push(Entry::Info("Nothing to undo yet.".into()));
            return false;
        }
        true
    }

    pub(super) fn undo(&mut self) {
        if self.can_rewind() {
            self.rewind_to(self.checkpoints.len() - 1);
        }
    }

    pub(super) fn open_rewind_picker(&mut self) {
        if !self.can_rewind() {
            return;
        }
        let activity = self
            .checkpoints
            .iter()
            .rev()
            .map(|t| {
                session::checkpoint_dir(&self.session_id, t.turn)
                    .map_or((0, 0), |dir| checkpoint::summary(&dir))
            })
            .collect();
        self.rewind_picker = Some(Picker {
            selected: 0,
            activity,
        });
    }

    pub(super) fn on_rewind_picker_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(picker) = &mut self.rewind_picker else {
            return;
        };
        let last = self.checkpoints.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.rewind_picker = None,
            KeyCode::Char('c') if ctrl => self.rewind_picker = None,
            KeyCode::Up | KeyCode::Char('k') => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Char('n') if ctrl => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Home | KeyCode::Char('g') => picker.selected = 0,
            KeyCode::End | KeyCode::Char('G') => picker.selected = last,
            KeyCode::Enter => {
                let index = last - picker.selected.min(last);
                self.rewind_picker = None;
                self.rewind_to(index);
            }
            _ => {}
        }
    }

    /// Returns to just before turn `index`: restores files changed in it and every later
    /// turn, and drops those turns from the conversation.
    pub(super) fn rewind_to(&mut self, index: usize) {
        let turns = self.checkpoints.split_off(index);
        let Some(first) = turns.first().cloned() else {
            return;
        };
        let dirs: Vec<_> = turns
            .iter()
            .filter_map(|t| session::checkpoint_dir(&self.session_id, t.turn))
            .collect();
        let restored = checkpoint::restore(&dirs);
        for dir in &dirs {
            checkpoint::discard(dir);
        }

        let prompt = truncate_chars(first.prompt.lines().next().unwrap_or(""), 60);
        let mut note = match first.history {
            Some(mark) => {
                self.history.truncate(mark.len);
                if let Some(n) = mark.merged
                    && let Some(Message::User { content }) = self.history.last_mut()
                {
                    content.truncate(n);
                }
                self.entries.truncate(first.entries_len);
                self.transcript.invalidate_from(0);
                format!("Rewound to before “{prompt}”")
            }
            None => format!(
                "Restored files from before “{prompt}”. The conversation was compacted since, \
so it was not rewound"
            ),
        };
        note.push_str(&match (restored.files, restored.commands) {
            (0, 0) => "; no files had changed.".to_string(),
            (files, 0) => format!("; {} restored.", plural(files, "file")),
            (files, commands) => format!(
                "; {} restored; {} not reverted.",
                plural(files, "file"),
                plural(commands, "shell command")
            ),
        });
        if self.input.is_empty() && first.history.is_some() {
            self.input = first.prompt.clone();
            self.cursor = self.input.len();
            note.push_str(" Your prompt is back in the input box.");
        }
        self.entries.push(Entry::Info(note));
        for error in restored.errors {
            self.entries
                .push(Entry::Error(format!("Couldn't restore a file: {error}")));
        }
        // Ignore late events from an interrupted turn that was just removed.
        self.turn += 1;
        self.attempt_entry = None;
        self.scroll_back = 0;
        self.save_session();
    }

    pub(super) fn draw_rewind_picker(&self, frame: &mut Frame, area: Rect) {
        let Some(picker) = &self.rewind_picker else {
            return;
        };
        let items: Vec<ListItem> = self
            .checkpoints
            .iter()
            .rev()
            .zip(&picker.activity)
            .map(|(turn, (files, commands))| {
                let mut activity = Vec::new();
                if *files > 0 {
                    activity.push(plural(*files, "file"));
                }
                if *commands > 0 {
                    activity.push(plural(*commands, "command"));
                }
                ListItem::new(Line::from(vec![
                    Span::from(truncate_chars(turn.prompt.lines().next().unwrap_or(""), 70)),
                    Span::from(if activity.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", activity.join(" · "))
                    })
                    .fg(Color::DarkGray),
                ]))
            })
            .collect();
        let popup = area.centered(Constraint::Percentage(80), Constraint::Max(20));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Rewind to before… ".bold())
            .title_bottom(
                Line::from(" ↑/↓ move · enter rewind (files and conversation) · esc cancel ")
                    .right_aligned(),
            );
        frame.render_widget(Clear, popup);
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(picker.selected));
        frame.render_stateful_widget(list, popup, &mut state);
    }
}
