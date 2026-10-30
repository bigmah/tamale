use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};

use super::{App, AppEvent, EFFORT_CHOICES, WHEEL_LINES};
use crate::clipboard;
use crate::session::Entry;

impl App {
    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
            Event::Paste(text) => self.on_paste(&text),
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => self.on_wheel(true),
                MouseEventKind::ScrollDown => self.on_wheel(false),
                _ => {}
            },
            _ => {}
        }
    }

    pub(super) fn on_wheel(&mut self, up: bool) {
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
        } else if self.backend_picker.is_some() {
            self.scroll_backend_picker(up);
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

    pub(super) fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if (ctrl && matches!(key.code, KeyCode::Char('v' | 'V')))
            || (shift && key.code == KeyCode::Insert)
        {
            self.paste_clipboard();
            return;
        }
        if ctrl && shift && matches!(key.code, KeyCode::Char('c' | 'C')) {
            self.copy_clipboard(false);
            return;
        }

        // A waiting tool call takes the keyboard from everything else.
        if self.approval.is_some() {
            self.on_approval_key(key, ctrl);
            return;
        }

        if self.picker.is_some() {
            self.on_picker_key(key, ctrl);
            return;
        }

        if self.rewind_picker.is_some() {
            self.on_rewind_picker_key(key, ctrl);
            return;
        }

        if self.effort_picker.is_some() {
            self.on_effort_picker_key(key, ctrl);
            return;
        }

        if self.backend_picker.is_some() {
            self.on_backend_picker_key(key, ctrl);
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
            KeyCode::BackTab => {
                let mut policy = self.permissions.lock().unwrap();
                policy.mode = policy.mode.next();
            }
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

    pub(super) fn insert_str(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub(super) fn on_paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if let Some(approval) = &mut self.approval {
            if let Some(reason) = &mut approval.reason {
                reason.push_str(&text.replace('\n', " "));
            }
        } else if let Some(picker) = &mut self.picker {
            picker.filter.push_str(&text.replace('\n', " "));
            picker.selected = 0;
        } else if let Some(picker) = &mut self.resume_picker {
            picker.filter.push_str(&text.replace('\n', " "));
            picker.selected = 0;
        } else if self.help_scroll.is_none()
            && self.effort_picker.is_none()
            && self.backend_picker.is_none()
            && self.rewind_picker.is_none()
        {
            self.insert_str(&text);
        }
    }

    pub(super) fn paste_clipboard(&self) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = clipboard::read().await.map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::ClipboardPasted(result));
        });
    }

    pub(super) fn copy_text(&self, all: bool) -> Option<String> {
        if all {
            let messages: Vec<_> = self
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::User(text) => Some(format!("You:\n{text}")),
                    Entry::Assistant(text) => Some(format!("Assistant:\n{text}")),
                    _ => None,
                })
                .collect();
            (!messages.is_empty()).then(|| messages.join("\n\n"))
        } else {
            self.entries.iter().rev().find_map(|entry| match entry {
                Entry::Assistant(text) if !text.is_empty() => Some(text.clone()),
                _ => None,
            })
        }
    }

    pub(super) fn copy_clipboard(&mut self, all: bool) {
        let Some(text) = self.copy_text(all) else {
            self.entries
                .push(Entry::Info("Nothing to copy yet.".into()));
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = clipboard::write(&text).await.map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::ClipboardCopied(result));
        });
    }

    pub(super) fn prev_boundary(&self) -> Option<usize> {
        self.input[..self.cursor]
            .char_indices()
            .last()
            .map(|(i, _)| i)
    }

    pub(super) fn next_boundary(&self) -> Option<usize> {
        self.input[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }

    pub(super) fn line_start(&self) -> usize {
        self.input[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    pub(super) fn line_end(&self) -> usize {
        self.input[self.cursor..]
            .find('\n')
            .map_or(self.input.len(), |i| self.cursor + i)
    }

    pub(super) fn delete_word(&mut self) {
        let before = &self.input[..self.cursor];
        let trimmed = before.trim_end();
        let start = trimmed
            .rfind(char::is_whitespace)
            .map_or(0, |i| i + trimmed[i..].chars().next().unwrap().len_utf8());
        self.input.drain(start..self.cursor);
        self.cursor = start;
    }

    // --------------------------------------------------------------- agent
}
