use crossterm::event::{KeyCode, KeyEvent};

use super::view::truncate_chars;
use super::{App, ResumePicker};
use crate::session::{self, Entry};

impl App {
    /// Writes the current conversation to disk (no-op until something has been said).
    pub fn save_session(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let title = self
            .entries
            .iter()
            .find_map(|e| match e {
                Entry::User(t) => Some(truncate_chars(t.lines().next().unwrap_or(""), 80)),
                _ => None,
            })
            .unwrap_or_default();
        let saved = session::Session {
            id: self.session_id.clone(),
            cwd: self.workspace.cwd.clone(),
            additional_dirs: self.workspace.additional_dirs.clone(),
            created: self.session_created,
            updated: session::now(),
            model: self.settings.model.name.clone(),
            title,
            context_tokens: self.context_tokens,
            history: self.history.clone(),
            entries: self.entries.clone(),
            checkpoints: self.checkpoints.clone(),
        };
        self.session_writer
            .get_or_insert_with(|| session::Writer::start(self.tx.clone()))
            .save(saved);
    }

    pub(super) fn on_session_saved(&mut self, id: &str, result: Result<(), String>) {
        if id != self.session_id {
            return;
        }
        match result {
            Ok(()) => self.save_error_shown = false,
            Err(e) if !self.save_error_shown => {
                self.save_error_shown = true;
                self.entries
                    .push(Entry::Error(format!("Couldn't save session: {e:#}")));
            }
            Err(_) => {}
        }
    }

    pub async fn finish_saves(&mut self) {
        if let Some(writer) = self.session_writer.take() {
            writer.finish().await;
        }
    }

    /// Saved sessions from this directory, excluding the current one.
    pub(super) fn resumable_sessions(&self) -> Vec<session::Meta> {
        self.saved_sessions(Some(&self.workspace.cwd))
            .into_iter()
            .filter(|m| m.id != self.session_id)
            .collect()
    }

    fn saved_sessions(&self, cwd: Option<&std::path::Path>) -> Vec<session::Meta> {
        let pending = self
            .session_writer
            .as_ref()
            .map(|writer| writer.pending())
            .unwrap_or_default();
        let mut sessions = session::list(cwd);
        if !pending.is_empty() {
            for saved in pending {
                sessions.retain(|meta| meta.id != saved.id);
                if cwd.is_none_or(|cwd| saved.cwd == cwd) {
                    sessions.push(session::Meta::from(saved.as_ref()));
                }
            }
            sessions.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id)));
        }
        sessions
    }

    /// `/resume` opens a picker; `/resume <n>` resumes the nth most recent session from this
    /// directory and `/resume <id>` a session by id (or unique id prefix) from anywhere.
    pub fn resume_command(&mut self, arg: Option<&str>) {
        if self.is_running() {
            self.entries.push(Entry::Error(
                "Can't resume while the agent is running (Esc to interrupt first).".into(),
            ));
            return;
        }
        let sessions = self.resumable_sessions();
        let Some(arg) = arg else {
            if sessions.is_empty() {
                self.entries.push(Entry::Info(
                    "No saved sessions for this directory yet.".into(),
                ));
            } else {
                self.resume_picker = Some(ResumePicker {
                    sessions,
                    filter: String::new(),
                    selected: 0,
                });
            }
            return;
        };
        if let Ok(n) = arg.parse::<usize>() {
            match n.checked_sub(1).and_then(|i| sessions.get(i)) {
                Some(m) => {
                    let id = m.id.clone();
                    self.resume(&id);
                }
                None => self.entries.push(Entry::Error(format!(
                    "No session #{n} (this directory has {} saved).",
                    sessions.len()
                ))),
            }
            return;
        }
        let matches: Vec<String> = self
            .saved_sessions(None)
            .into_iter()
            .filter(|m| m.id.starts_with(arg))
            .map(|m| m.id)
            .collect();
        match matches.as_slice() {
            [id] => {
                let id = id.clone();
                self.resume(&id);
            }
            [] => self
                .entries
                .push(Entry::Error(format!("No saved session matches {arg}."))),
            _ => self.entries.push(Entry::Error(format!(
                "{arg} matches {} sessions; use a longer id.",
                matches.len()
            ))),
        }
    }

    /// Replaces the conversation with a saved session; later saves go to that session.
    pub fn resume(&mut self, id: &str) {
        if self.is_running() {
            return;
        }
        let pending = self
            .session_writer
            .as_ref()
            .and_then(|writer| writer.pending().into_iter().find(|saved| saved.id == id));
        let saved = match pending.map_or_else(|| session::load(id), |saved| Ok((*saved).clone())) {
            Ok(s) => s,
            Err(e) => {
                self.entries
                    .push(Entry::Error(format!("Couldn't resume session: {e:#}")));
                return;
            }
        };
        self.save_session();
        self.turn += 1;
        self.history = saved.history;
        self.entries = saved.entries;
        self.checkpoints = saved.checkpoints;
        self.permissions.lock().unwrap().clear_session();
        self.workspace.additional_dirs.clear();
        for dir in self
            .startup_dirs
            .clone()
            .into_iter()
            .chain(saved.additional_dirs)
        {
            if let Err(error) = self.workspace.add_dir(&dir) {
                self.entries.push(Entry::Error(format!(
                    "Couldn't restore scoped directory {}: {error:#}",
                    dir.display()
                )));
            }
        }
        self.transcript.invalidate_from(0);
        self.context_tokens = saved.context_tokens;
        self.session_id = saved.id;
        self.session_created = saved.created;
        self.scroll_back = 0;
        self.attempt_entry = None;

        let mut note = format!(
            "Resumed session “{}” (last active {}).",
            saved.title,
            session::ago(saved.updated)
        );
        if saved.cwd != self.workspace.cwd {
            note.push_str(&format!(
                " It was started in {}; tools now run in {}.",
                saved.cwd.display(),
                self.workspace.cwd.display()
            ));
        }
        if saved.model != self.settings.model.name {
            note.push_str(&format!(
                " It used {}; continuing with {} (/model to switch).",
                saved.model, self.settings.model.name
            ));
        }
        self.entries.push(Entry::Info(note));
    }

    pub(super) fn on_resume_picker_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(picker) = &mut self.resume_picker else {
            return;
        };
        let count = picker.filtered().len();
        let last = count.saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.resume_picker = None,
            KeyCode::Char('c') if ctrl => self.resume_picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Char('n') if ctrl => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Home => picker.selected = 0,
            KeyCode::End => picker.selected = last,
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.selected = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                picker.filter.push(c);
                picker.selected = 0;
            }
            KeyCode::Enter => {
                let id = picker.filtered().get(picker.selected).map(|m| m.id.clone());
                if let Some(id) = id {
                    self.resume_picker = None;
                    self.resume(&id);
                }
            }
            _ => {}
        }
    }
}
