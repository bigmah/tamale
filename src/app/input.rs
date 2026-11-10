use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::Position;

use super::{App, AppEvent, EFFORT_CHOICES, FooterTab, WHEEL_LINES};

/// Pastes with at least this many lines show collapsed in the prompt.
const PASTE_COLLAPSE_LINES: usize = 10;
/// Pastes with at least this many characters show collapsed in the prompt.
const PASTE_COLLAPSE_CHARS: usize = 1000;
use crate::clipboard;
use crate::session::Entry;

impl App {
    pub fn on_event(&mut self, event: Event) {
        #[cfg(unix)]
        if self.remote_view.is_some() {
            self.on_remote_input(event);
            return;
        }
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
            Event::Paste(text) => {
                // Pasting lands in the prompt, so the keyboard goes back there too.
                self.tab_focus = None;
                self.on_paste(&text)
            }
            Event::Mouse(mouse) if self.diff_open && !self.waiting_for_user() => {
                self.on_diff_mouse(mouse)
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => self.on_wheel(true),
                MouseEventKind::ScrollDown => self.on_wheel(false),
                MouseEventKind::Down(MouseButton::Left) if !self.waiting_for_user() => {
                    let at = Position::new(mouse.column, mouse.row);
                    let tab = self.tab_areas.iter().find(|(_, area)| area.contains(at));
                    if let Some(&(tab, _)) = tab {
                        self.open_tab(tab);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// The footer's tabs as they're drawn; Diff only exists inside a git repository.
    pub(super) fn footer_tabs(&self) -> Vec<FooterTab> {
        let mut tabs = vec![FooterTab::Sessions, FooterTab::Remote, FooterTab::Web];
        if self.diff_stats().is_some() {
            tabs.push(FooterTab::Diff);
        }
        if self.shows_chef_tab() {
            tabs.push(FooterTab::Chef);
        }
        tabs
    }

    fn open_tab(&mut self, tab: FooterTab) {
        match tab {
            FooterTab::Sessions => {
                self.tab_focus = None;
                self.command("/sessions");
            }
            FooterTab::Remote => self.open_remote(),
            FooterTab::Web => self.open_web(),
            FooterTab::Diff => self.show_diff(),
            FooterTab::Chef => {
                self.tab_focus = None;
                self.chef_command();
            }
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
        if self.question.is_some() {
            self.on_question_key(key, ctrl);
            return;
        }

        if self.diff_open {
            self.on_diff_key(key, ctrl);
            return;
        }

        #[cfg(unix)]
        if self.sessions_picker.is_some() {
            self.on_sessions_picker_key(key, ctrl);
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

        if let Some(tab) = self.tab_focus {
            // ←/→ only highlight a tab and only Enter opens it, so nothing opens by surprise,
            // not even when ↓ is held down to get through the prompt history.
            let tabs = self.footer_tabs();
            let at = tabs.iter().position(|&t| t == tab).unwrap_or(0);
            match key.code {
                KeyCode::Left => self.tab_focus = Some(tabs[at.saturating_sub(1)]),
                KeyCode::Right => self.tab_focus = Some(tabs[(at + 1).min(tabs.len() - 1)]),
                KeyCode::Enter => self.open_tab(tab),
                // The Chef tab is a switch, as Remote is: x turns it off.
                KeyCode::Char('x') | KeyCode::Delete
                    if tab == FooterTab::Chef && self.chef_on() == Some(true) =>
                {
                    self.stop_chef()
                }
                KeyCode::Up | KeyCode::Esc => self.tab_focus = None,
                KeyCode::Down => {}
                // Anything else goes back to the prompt and acts there.
                _ => {
                    self.tab_focus = None;
                    return self.on_key(key);
                }
            }
            return;
        }

        match key.code {
            KeyCode::Char('c') if ctrl => {
                if self.is_running() {
                    if self.detachable {
                        self.detach_requested = true;
                    } else {
                        self.interrupt();
                    }
                } else if !self.input.is_empty() {
                    self.input.clear();
                    self.cursor = 0;
                    self.prune_pastes();
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => self.should_quit = true,
            KeyCode::Char('o') if ctrl => self.open_web(),
            KeyCode::Esc if self.is_running() => self.interrupt(),
            KeyCode::BackTab => {
                let mut policy = self.permissions.lock().unwrap();
                policy.mode = policy.mode.next();
            }
            KeyCode::Enter if !alt && !shift => self.submit(),
            KeyCode::PageUp => self.scroll_back += 10,
            KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(10),
            KeyCode::Up if shift => self.scroll_back += 1,
            KeyCode::Down if shift => self.scroll_back = self.scroll_back.saturating_sub(1),
            KeyCode::Up => self.cursor_up(),
            KeyCode::Down => self.cursor_down(),
            _ => self.on_edit_key(key, ctrl, alt, shift),
        }
    }

    /// The keys that edit the prompt, whichever session it's for.
    pub(super) fn on_edit_key(&mut self, key: KeyEvent, ctrl: bool, alt: bool, shift: bool) {
        match key.code {
            KeyCode::Enter if alt || shift => self.insert_str("\n"),
            KeyCode::Char('j') if ctrl => self.insert_str("\n"),
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
                if let Some(prev) = self.paste_before_cursor().or_else(|| self.prev_boundary()) {
                    self.input.drain(prev..self.cursor);
                    self.cursor = prev;
                }
            }
            KeyCode::Delete => {
                if let Some(next) = self.paste_after_cursor().or_else(|| self.next_boundary()) {
                    self.input.drain(self.cursor..next);
                }
            }
            KeyCode::Left => self.cursor = self.prev_boundary().unwrap_or(self.cursor),
            KeyCode::Right => self.cursor = self.next_boundary().unwrap_or(self.cursor),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            _ => {}
        }
    }

    /// Moves to the line above, or recalls the previous prompt from the first line.
    pub(super) fn cursor_up(&mut self) {
        let start = self.line_start();
        if start > 0 {
            self.cursor = self.column_in_line(start - 1, self.input[start..self.cursor].chars().count());
        } else if let Some(prompt) = self.prompts.prev(&self.input) {
            self.input = prompt;
            // At the start, so ↑ again keeps going back through multi-line prompts.
            self.cursor = 0;
        }
    }

    /// Moves to the line below, or recalls the next prompt (then the draft) from the last line.
    pub(super) fn cursor_down(&mut self) {
        let end = self.line_end();
        if end < self.input.len() {
            let column = self.input[self.line_start()..self.cursor].chars().count();
            self.cursor = self.column_in_line(end + 1, column);
        } else if let Some(prompt) = self.prompts.next() {
            self.input = prompt;
            self.cursor = self.input.len();
        } else {
            // Past the last line (and the newest prompt): down to the footer's tabs.
            self.tab_focus = Some(FooterTab::Sessions);
        }
    }

    /// The byte offset of `column` (in characters) on the line containing byte `at`,
    /// clamped to the line's end.
    fn column_in_line(&self, at: usize, column: usize) -> usize {
        let start = self.input[..at].rfind('\n').map_or(0, |i| i + 1);
        let end = self.input[start..].find('\n').map_or(self.input.len(), |i| start + i);
        self.input[start..end]
            .char_indices()
            .nth(column)
            .map_or(end, |(i, _)| start + i)
    }

    pub(super) fn insert_str(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub(super) fn on_paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.question.is_some() {
            self.on_question_paste(&text);
        } else if let Some(approval) = &mut self.approval {
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
            && !self.diff_open
        {
            self.insert_paste(text);
        }
    }

    /// Inserts pasted text, collapsing a big one to a placeholder that expands when it's sent.
    fn insert_paste(&mut self, text: String) {
        let lines = text.trim_end_matches('\n').matches('\n').count();
        if lines < PASTE_COLLAPSE_LINES && text.chars().count() < PASTE_COLLAPSE_CHARS {
            self.insert_str(&text);
            return;
        }
        self.paste_count += 1;
        let placeholder = if lines > 0 {
            format!("[Pasted text #{} +{lines} lines]", self.paste_count)
        } else {
            format!("[Pasted text #{} {} chars]", self.paste_count, text.chars().count())
        };
        self.insert_str(&placeholder);
        self.pastes.push((placeholder, text));
    }

    /// `input` with each paste placeholder replaced by the text it stands for.
    pub(super) fn expand_pastes(&self, input: &str) -> String {
        self.pastes
            .iter()
            .fold(input.to_string(), |text, (placeholder, paste)| {
                text.replacen(placeholder, paste, 1)
            })
    }

    /// Forgets pastes whose placeholder is no longer in the prompt (or a stashed draft).
    pub(super) fn prune_pastes(&mut self) {
        #[cfg(unix)]
        let draft = self.remote_view.as_ref().map(|view| view.draft.0.clone());
        #[cfg(not(unix))]
        let draft: Option<String> = None;
        let input = &self.input;
        self.pastes.retain(|(placeholder, _)| {
            input.contains(placeholder.as_str())
                || draft.as_ref().is_some_and(|d| d.contains(placeholder.as_str()))
        });
    }

    /// The placeholder ending at the cursor, if any, so Backspace removes it whole.
    fn paste_before_cursor(&self) -> Option<usize> {
        let before = &self.input[..self.cursor];
        self.pastes
            .iter()
            .find(|(placeholder, _)| before.ends_with(placeholder.as_str()))
            .map(|(placeholder, _)| self.cursor - placeholder.len())
    }

    /// The placeholder starting at the cursor, if any, so Delete removes it whole.
    fn paste_after_cursor(&self) -> Option<usize> {
        let after = &self.input[self.cursor..];
        self.pastes
            .iter()
            .find(|(placeholder, _)| after.starts_with(placeholder.as_str()))
            .map(|(placeholder, _)| self.cursor + placeholder.len())
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
        self.write_clipboard(text);
    }

    pub(super) fn write_clipboard(&self, text: String) {
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
