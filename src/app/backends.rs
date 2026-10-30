//! Choosing where inference comes from: `/backend`, its picker, and Claude Code logins
//! (`/login`, adding and removing accounts).

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, AppEvent};
use crate::backend::{self, Backend, ClaudeCode, logins};
use crate::session::Entry;

pub(super) struct Picker {
    pub selected: usize,
    /// Still checking which Claude Code logins exist.
    pub loading: bool,
}

/// A row of the backend picker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Choice {
    Api,
    /// An index into `App::logins`.
    Login(usize),
    AddLogin,
}

/// A Claude Code sign-in for the event loop to run (it needs the terminal).
pub struct LoginRequest {
    pub cc: ClaudeCode,
    /// A new account from the picker; its directory is removed if sign-in fails.
    pub(super) adding: bool,
}

impl App {
    /// Footer label; with several Claude Code logins, the account tells them apart.
    pub(super) fn backend_label(&self) -> String {
        match &self.backend {
            Backend::ClaudeCode(cc) if self.logins.len() > 1 => cc.label_with_account(),
            backend => backend.label(),
        }
    }

    pub(super) fn backend_choices(&self) -> Vec<Choice> {
        let mut choices = vec![Choice::Api];
        choices.extend((0..self.logins.len()).map(Choice::Login));
        choices.push(Choice::AddLogin);
        choices
    }

    pub(super) fn is_current(&self, choice: Choice) -> bool {
        match (choice, &self.backend) {
            (Choice::Api, Backend::Api(_)) => true,
            (Choice::Login(i), Backend::ClaudeCode(cc)) => {
                self.logins.get(i).map(|l| &l.config_dir) == Some(&cc.config_dir)
            }
            _ => false,
        }
    }

    fn current_choice(&self) -> usize {
        self.backend_choices()
            .into_iter()
            .position(|c| self.is_current(c))
            .unwrap_or(0)
    }

    /// `/login`: signs in to the current Claude Code login again (or switches its account).
    pub(super) fn login(&mut self) {
        match &self.backend {
            Backend::Api(_) => self.entries.push(Entry::Info(
                "This session uses the Anthropic API with ANTHROPIC_API_KEY. To use a Claude \
Code sign-in instead, pick one with /backend."
                    .into(),
            )),
            Backend::ClaudeCode(_) if self.is_running() => self.entries.push(Entry::Error(
                "Can't sign in while the agent is running (Esc to interrupt first).".into(),
            )),
            Backend::ClaudeCode(cc) => {
                self.login_request = Some(LoginRequest {
                    cc: cc.clone(),
                    adding: false,
                })
            }
        }
    }

    /// `/backend <kind>` switches directly (`claude-code` is the default login);
    /// without an argument it opens the picker. The conversation carries over.
    pub(super) fn switch_backend(&mut self, arg: Option<&str>) {
        let Some(arg) = arg else {
            self.open_backend_picker();
            return;
        };
        let kind = match backend::Kind::parse_setting(arg) {
            Ok(kind) => kind,
            Err(e) => {
                self.entries.push(Entry::Error(e.to_string()));
                return;
            }
        };
        if self.refuse_while_running() {
            return;
        }
        let current = match &self.backend {
            Backend::Api(_) => kind == backend::Kind::Api,
            Backend::ClaudeCode(cc) => kind == backend::Kind::ClaudeCode && cc.config_dir.is_none(),
        };
        if current {
            self.entries.push(Entry::Info(format!(
                "Already using {}.",
                self.backend_label()
            )));
            return;
        }
        let mut settings = self.settings.backend.clone();
        settings.kind = kind;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = Backend::connect(&settings)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::Backend(result));
        });
    }

    fn refuse_while_running(&mut self) -> bool {
        if self.is_running() {
            self.entries.push(Entry::Error(
                "Can't switch backends while the agent is running (Esc to interrupt first).".into(),
            ));
        }
        self.is_running()
    }

    /// Makes `backend` current. A Claude Code session from before doesn't hold the turns
    /// made since, so the next turn starts a new one.
    fn set_backend(&mut self, backend: Backend) {
        if let Backend::ClaudeCode(cc) = &backend {
            self.remember_login(cc.clone());
        }
        self.settings.backend.kind = backend.kind();
        self.backend = backend;
        self.thread = None;
        self.models = None;
    }

    fn remember_login(&mut self, cc: ClaudeCode) {
        match self
            .logins
            .iter_mut()
            .find(|l| l.config_dir == cc.config_dir)
        {
            Some(known) => *known = cc,
            None => self.logins.push(cc),
        }
    }

    pub(super) fn on_backend(&mut self, result: Result<Backend, String>) {
        match result {
            Ok(backend) => self.use_backend(backend),
            Err(e) => self
                .entries
                .push(Entry::Error(format!("Couldn't switch backends: {e}"))),
        }
    }

    fn use_backend(&mut self, backend: Backend) {
        self.set_backend(backend);
        let note = if self.is_running() {
            " (takes effect on your next message)"
        } else {
            ""
        };
        self.entries.push(Entry::Info(format!(
            "Switched to {}{note}. The conversation continues.",
            self.backend_label()
        )));
        if let Backend::ClaudeCode(cc) = &self.backend
            && !cc.account.logged_in
        {
            self.entries.push(Entry::Info(
                "Claude Code isn't signed in; starting sign-in…".into(),
            ));
            self.login_request = Some(LoginRequest {
                cc: cc.clone(),
                adding: false,
            });
        }
    }

    pub fn take_login_request(&mut self) -> Option<LoginRequest> {
        self.login_request.take()
    }

    pub fn on_login(&mut self, request: LoginRequest, result: anyhow::Result<()>) {
        let LoginRequest { cc, adding } = request;
        if let Err(e) = result {
            self.entries.push(Entry::Error(format!("{e:#}")));
            if adding {
                tokio::spawn(async move {
                    let _ = logins::remove(&cc).await;
                });
            } else {
                // Keep what the refresh learned (e.g. still signed in to the old account).
                self.remember_login(cc.clone());
                if self.is_current_login(&cc) {
                    self.backend = Backend::ClaudeCode(cc);
                }
            }
            return;
        }
        if adding && let Some(existing) = self.logins.iter().find(|l| l.same_account(&cc)).cloned()
        {
            tokio::spawn(async move {
                let _ = logins::remove(&cc).await;
            });
            self.entries.push(Entry::Info(format!(
                "That account is already signed in ({}).",
                existing.label_with_account()
            )));
            if !self.is_current_login(&existing) {
                self.use_backend(Backend::ClaudeCode(existing));
            }
            return;
        }
        let who = cc
            .account
            .email
            .as_deref()
            .map(|email| format!(" as {email}"))
            .unwrap_or_default();
        self.entries.push(Entry::Info(format!(
            "Signed in to Claude Code{who} ({}).",
            cc.label()
        )));
        if adding || self.is_current_login(&cc) {
            self.set_backend(Backend::ClaudeCode(cc));
        } else {
            self.remember_login(cc);
        }
    }

    fn is_current_login(&self, cc: &ClaudeCode) -> bool {
        matches!(&self.backend, Backend::ClaudeCode(current) if current.config_dir == cc.config_dir)
    }

    // -------------------------------------------------------- picker

    pub(super) fn open_backend_picker(&mut self) {
        self.backend_picker = Some(Picker {
            selected: self.current_choice(),
            loading: true,
        });
        let settings = self.settings.backend.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = logins::list(&settings).await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::Logins(result));
        });
    }

    pub(super) fn on_logins(&mut self, result: Result<Vec<ClaudeCode>, String>) {
        let selected = self
            .backend_picker
            .as_ref()
            .and_then(|p| self.backend_choices().get(p.selected).copied());
        match result {
            Ok(list) => {
                self.logins = list;
                // The current login stays listed even if it was signed out meanwhile.
                if let Backend::ClaudeCode(cc) = &self.backend {
                    match self.logins.iter().find(|l| l.config_dir == cc.config_dir) {
                        Some(fresh) => self.backend = Backend::ClaudeCode(fresh.clone()),
                        None => self.logins.push(cc.clone()),
                    }
                }
            }
            // Without the CLI, only the API is available.
            Err(e) => self.entries.push(Entry::Error(format!(
                "Couldn't list Claude Code logins: {e}"
            ))),
        }
        let choices = self.backend_choices();
        let current = self.current_choice();
        if let Some(picker) = &mut self.backend_picker {
            picker.loading = false;
            // Keep a moved selection on the same kind of row; otherwise the current one.
            picker.selected = match selected {
                Some(Choice::Api) | None => current,
                Some(other) => choices.iter().position(|c| *c == other).unwrap_or(current),
            };
        }
    }

    pub(super) fn on_backend_picker_key(&mut self, key: KeyEvent, ctrl: bool) {
        let choices = self.backend_choices();
        let Some(picker) = &mut self.backend_picker else {
            return;
        };
        let last = choices.len() - 1;
        let selected = picker.selected.min(last);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.backend_picker = None,
            KeyCode::Char('c') if ctrl => self.backend_picker = None,
            KeyCode::Up | KeyCode::Char('k') => picker.selected = selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => picker.selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => picker.selected = (selected + 1).min(last),
            KeyCode::Char('n') if ctrl => picker.selected = (selected + 1).min(last),
            KeyCode::Home | KeyCode::Char('g') => picker.selected = 0,
            KeyCode::End | KeyCode::Char('G') => picker.selected = last,
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) <= last => {
                self.backend_picker = None;
                self.choose(choices[c as usize - '1' as usize]);
            }
            KeyCode::Enter => {
                self.backend_picker = None;
                self.choose(choices[selected]);
            }
            KeyCode::Delete | KeyCode::Backspace | KeyCode::Char('d') => {
                if let Choice::Login(i) = choices[selected] {
                    self.remove_login(i);
                }
            }
            _ => {}
        }
    }

    pub(super) fn scroll_backend_picker(&mut self, up: bool) {
        let last = self.backend_choices().len() - 1;
        if let Some(picker) = &mut self.backend_picker {
            picker.selected = if up {
                picker.selected.saturating_sub(1)
            } else {
                (picker.selected + 1).min(last)
            };
        }
    }

    fn choose(&mut self, choice: Choice) {
        match choice {
            Choice::Api => self.switch_backend(Some(backend::Kind::Api.as_str())),
            Choice::Login(i) => {
                if self.refuse_while_running() {
                    return;
                }
                if self.is_current(choice) {
                    self.entries.push(Entry::Info(format!(
                        "Already using {}.",
                        self.backend_label()
                    )));
                    return;
                }
                let cc = self.logins[i].clone();
                self.use_backend(Backend::ClaudeCode(cc));
            }
            Choice::AddLogin => {
                if self.refuse_while_running() {
                    return;
                }
                match logins::new_dir() {
                    Ok(dir) => {
                        let program = self.settings.backend.claude_path.clone();
                        self.login_request = Some(LoginRequest {
                            cc: ClaudeCode::signed_out(program, Some(dir)),
                            adding: true,
                        });
                    }
                    Err(e) => self
                        .entries
                        .push(Entry::Error(format!("Couldn't add an account: {e:#}"))),
                }
            }
        }
    }

    fn remove_login(&mut self, i: usize) {
        let cc = self.logins[i].clone();
        if !logins::is_added(cc.config_dir.as_deref()) {
            self.entries.push(Entry::Error(
                "Only accounts added here can be removed here. To sign this one out, run \
`claude auth logout`."
                    .into(),
            ));
            return;
        }
        if self.is_current_login(&cc) {
            self.entries.push(Entry::Error(
                "That's the current backend; switch to another one before removing it.".into(),
            ));
            return;
        }
        self.logins.remove(i);
        if let Some(picker) = &mut self.backend_picker {
            picker.selected = picker.selected.min(self.logins.len() + 1);
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let label = cc.label_with_account();
            let result = logins::remove(&cc)
                .await
                .map(|()| label)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::LoginRemoved(result));
        });
    }

    pub(super) fn on_login_removed(&mut self, result: Result<String, String>) {
        self.entries.push(match result {
            Ok(label) => Entry::Info(format!("Removed {label}.")),
            Err(e) => Entry::Error(format!("Couldn't remove the account: {e}")),
        });
    }
}
