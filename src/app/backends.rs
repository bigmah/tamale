//! Choosing where inference comes from: `/backend`, its picker, OpenAI-compatible
//! providers, and Claude Code and Codex logins (`/login`, adding and removing accounts).

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, AppEvent};
use crate::backend::{self, Backend, Kind, logins};
use crate::session::Entry;

pub(super) struct Picker {
    pub selected: usize,
    /// Still checking which CLI logins exist.
    pub loading: bool,
}

/// A row of the backend picker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Choice {
    Api,
    /// An index into `[backend.providers]`.
    Provider(usize),
    /// An index into `App::logins`.
    Login(usize),
    /// Sign in to another account of a CLI.
    AddLogin(Kind),
}

/// Kinds of CLI login the picker can add.
const LOGIN_KINDS: [Kind; 2] = [Kind::ClaudeCode, Kind::Codex];

/// A row's identity: [`Choice::Login`] indexes shift when the list is refreshed.
#[derive(Clone, PartialEq, Eq, Debug)]
enum ChoiceKey {
    Api,
    Provider(String),
    Login(Kind, Option<std::path::PathBuf>),
    AddLogin(Kind),
}

/// A CLI sign-in for the event loop to run (it needs the terminal).
pub struct LoginRequest {
    pub login: Backend,
    /// A new account from the picker; its directory is removed if sign-in fails.
    pub(super) adding: bool,
}

impl App {
    /// Footer label; with several logins of the same CLI, the account tells them apart.
    pub(super) fn backend_label(&self) -> String {
        let kind = self.backend.kind();
        let logins = self.logins.iter().filter(|l| l.kind() == kind).count();
        if matches!(kind, Kind::ClaudeCode | Kind::Codex) && logins > 1 {
            self.backend.label_with_account()
        } else {
            self.backend.label()
        }
    }

    /// The API, the providers with a key, each CLI's logins, then a row to add an
    /// account of each CLI.
    pub(super) fn backend_choices(&self) -> Vec<Choice> {
        let mut choices = vec![Choice::Api];
        choices.extend(
            self.settings
                .backend
                .providers
                .iter()
                .enumerate()
                .filter(|(_, (name, provider))| provider.key_set() || self.is_provider(name))
                .map(|(i, _)| Choice::Provider(i)),
        );
        for kind in LOGIN_KINDS {
            choices.extend(
                (0..self.logins.len())
                    .filter(|&i| self.logins[i].kind() == kind)
                    .map(Choice::Login),
            );
        }
        choices.extend(LOGIN_KINDS.map(Choice::AddLogin));
        choices
    }

    pub(super) fn is_current(&self, choice: Choice) -> bool {
        match choice {
            Choice::Api => matches!(self.backend, Backend::Api(_)),
            Choice::Provider(i) => self.provider_name(i).is_some_and(|n| self.is_provider(n)),
            Choice::Login(i) => self
                .logins
                .get(i)
                .is_some_and(|l| l.same_login(&self.backend)),
            Choice::AddLogin(_) => false,
        }
    }

    /// Whether the current backend is the provider called `name`.
    fn is_provider(&self, name: &str) -> bool {
        matches!(&self.backend, Backend::Compat(c) if c.name == name)
    }

    pub(super) fn provider_name(&self, i: usize) -> Option<&str> {
        self.settings
            .backend
            .providers
            .keys()
            .nth(i)
            .map(String::as_str)
    }

    /// The row to start on: the current backend, or with none yet, the first signed-in
    /// login (or the API when there's a key to use it with).
    fn current_choice(&self) -> usize {
        let choices = self.backend_choices();
        let first_login = || {
            choices
                .iter()
                .position(|c| matches!(*c, Choice::Login(i) if self.logins[i].signed_in()))
        };
        choices
            .iter()
            .position(|&c| self.is_current(c))
            .or_else(|| {
                let unset = matches!(self.backend, Backend::Unset);
                (unset && self.settings.backend.missing_key().is_some())
                    .then(first_login)
                    .flatten()
            })
            .unwrap_or(0)
    }

    /// At startup without a backend: says why and opens the picker, unless another
    /// picker (`--resume`) is already open; the first prompt opens it then.
    pub fn ask_for_backend(&mut self) {
        let key = self
            .settings
            .backend
            .missing_key()
            .unwrap_or_else(|| "ANTHROPIC_API_KEY".into());
        self.entries.push(Entry::Info(format!(
            "{key} isn't set. Choose a backend to get started (/backend)."
        )));
        if self.resume_picker.is_none() {
            self.open_backend_picker();
        }
    }

    /// With no backend yet, opens the picker instead of starting a turn, and puts `text`
    /// back in the input to send afterwards. Returns whether it did.
    pub(super) fn refuse_without_backend(&mut self, text: &str) -> bool {
        if !matches!(self.backend, Backend::Unset) {
            return false;
        }
        self.entries
            .push(Entry::Info("Choose a backend first.".into()));
        if self.input.is_empty() {
            self.input = text.to_string();
            self.cursor = self.input.len();
        }
        self.open_backend_picker();
        true
    }

    /// `/login`: signs in to the current CLI login again (or switches its account).
    pub(super) fn login(&mut self) {
        match &self.backend {
            Backend::Unset => self.open_backend_picker(),
            Backend::Api(_) => self.entries.push(Entry::Info(
                "This session uses the Anthropic API with ANTHROPIC_API_KEY. To use a Claude \
Code or Codex sign-in instead, pick one with /backend."
                    .into(),
            )),
            Backend::Compat(compat) => self.entries.push(Entry::Info(format!(
                "{}. To use a Claude Code or Codex sign-in instead, pick one with /backend.",
                compat.no_login()
            ))),
            _ if self.is_running() => self.entries.push(Entry::Error(
                "Can't sign in while the agent is running (Esc to interrupt first).".into(),
            )),
            login => {
                self.login_request = Some(LoginRequest {
                    login: login.clone(),
                    adding: false,
                })
            }
        }
    }

    /// `/backend <kind>` switches directly (`claude-code` and `codex` are the default logins,
    /// a provider's name its API); without an argument it opens the picker. The
    /// conversation carries over.
    pub(super) fn switch_backend(&mut self, arg: Option<&str>) {
        let Some(arg) = arg else {
            self.open_backend_picker();
            return;
        };
        let mut settings = self.settings.backend.clone();
        if let Err(e) = settings.select(arg) {
            self.entries.push(Entry::Error(e.to_string()));
            return;
        }
        if self.refuse_while_running() {
            return;
        }
        let kind = settings.kind;
        let current = match &self.backend {
            Backend::Unset => false,
            Backend::Compat(c) => kind == Kind::Compat && c.name == settings.provider,
            backend => backend.kind() == kind && backend.login_dir().is_none(),
        };
        if current {
            self.entries.push(Entry::Info(format!(
                "Already using {}.",
                self.backend_label()
            )));
            return;
        }
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

    /// Makes `backend` current. A CLI session from before doesn't hold the turns made
    /// since, so the next turn starts a new one.
    fn set_backend(&mut self, backend: Backend) {
        if matches!(backend.kind(), Kind::ClaudeCode | Kind::Codex) {
            self.remember_login(backend.clone());
        }
        if let Some(model) = backend.model_after(&self.backend, &self.settings.model.name) {
            self.entries
                .push(Entry::Info(format!("Switched model to {model}.")));
            self.settings.model.name = model;
        }
        self.settings.backend.kind = backend.kind();
        if let Backend::Compat(compat) = &backend {
            self.settings.backend.provider = compat.name.clone();
        }
        self.backend = backend;
        self.thread = None;
        self.models = None;
    }

    fn remember_login(&mut self, login: Backend) {
        match self.logins.iter_mut().find(|l| l.same_login(&login)) {
            Some(known) => *known = login,
            None => self.logins.push(login),
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
        if !self.backend.signed_in() {
            self.entries.push(Entry::Info(format!(
                "{} isn't signed in; starting sign-in…",
                self.backend.kind().cli_name()
            )));
            self.login_request = Some(LoginRequest {
                login: self.backend.clone(),
                adding: false,
            });
        }
    }

    pub fn take_login_request(&mut self) -> Option<LoginRequest> {
        self.login_request.take()
    }

    pub fn on_login(&mut self, request: LoginRequest, result: anyhow::Result<()>) {
        let LoginRequest { login, adding } = request;
        if let Err(e) = result {
            self.entries.push(Entry::Error(format!("{e:#}")));
            if adding {
                tokio::spawn(async move {
                    let _ = logins::remove(&login).await;
                });
            } else {
                // Keep what the refresh learned (e.g. still signed in to the old account).
                self.remember_login(login.clone());
                if login.same_login(&self.backend) {
                    self.backend = login;
                }
            }
            return;
        }
        if adding
            && let Some(existing) = self.logins.iter().find(|l| l.same_account(&login)).cloned()
        {
            tokio::spawn(async move {
                let _ = logins::remove(&login).await;
            });
            self.entries.push(Entry::Info(format!(
                "That account is already signed in ({}).",
                existing.label_with_account()
            )));
            if !existing.same_login(&self.backend) {
                self.use_backend(existing);
            }
            return;
        }
        let who = login
            .email()
            .map(|email| format!(" as {email}"))
            .unwrap_or_default();
        self.entries.push(Entry::Info(format!(
            "Signed in to {}{who} ({}).",
            login.kind().cli_name(),
            login.label()
        )));
        if adding || login.same_login(&self.backend) {
            self.set_backend(login);
        } else {
            self.remember_login(login);
        }
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
            let (logins, errors) = logins::list(&settings).await;
            let _ = tx.send(AppEvent::Logins(logins, errors));
        });
    }

    pub(super) fn on_logins(&mut self, list: Vec<Backend>, errors: Vec<String>) {
        let selected = self.backend_picker.as_ref().and_then(|p| {
            self.backend_choices()
                .get(p.selected)
                .map(|&c| self.choice_key(c))
        });
        for error in errors {
            self.entries
                .push(Entry::Error(format!("Couldn't list logins of {error}")));
        }
        // A CLI that couldn't be listed keeps the logins known from before.
        let listed: Vec<Kind> = list.iter().map(Backend::kind).collect();
        self.logins.retain(|l| !listed.contains(&l.kind()));
        self.logins.extend(list);
        // The current login stays listed even if it was signed out meanwhile.
        if matches!(self.backend.kind(), Kind::ClaudeCode | Kind::Codex) {
            match self.logins.iter().find(|l| l.same_login(&self.backend)) {
                Some(fresh) => self.backend = fresh.clone(),
                None => self.logins.push(self.backend.clone()),
            }
        }
        let current = self.current_choice();
        // Keep a moved selection on the same row; otherwise the current one.
        let moved = match selected {
            Some(ChoiceKey::Api) | None => None,
            Some(key) => self
                .backend_choices()
                .into_iter()
                .position(|c| self.choice_key(c) == key),
        };
        if let Some(picker) = &mut self.backend_picker {
            picker.loading = false;
            picker.selected = moved.unwrap_or(current);
        }
    }

    /// What a row stands for, stable across refreshes of the login list.
    fn choice_key(&self, choice: Choice) -> ChoiceKey {
        match choice {
            Choice::Api => ChoiceKey::Api,
            Choice::Provider(i) => {
                ChoiceKey::Provider(self.provider_name(i).unwrap_or_default().to_string())
            }
            Choice::Login(i) => ChoiceKey::Login(
                self.logins[i].kind(),
                self.logins[i].login_dir().map(Into::into),
            ),
            Choice::AddLogin(kind) => ChoiceKey::AddLogin(kind),
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
            Choice::Provider(i) => {
                if let Some(name) = self.provider_name(i).map(str::to_string) {
                    self.switch_backend(Some(&name));
                }
            }
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
                let login = self.logins[i].clone();
                self.use_backend(login);
            }
            Choice::AddLogin(kind) => {
                if self.refuse_while_running() {
                    return;
                }
                match logins::new_login(kind, &self.settings.backend) {
                    Ok(login) => {
                        self.login_request = Some(LoginRequest {
                            login,
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
        let login = self.logins[i].clone();
        if !logins::is_added(&login) {
            let logout = match login.kind() {
                Kind::Codex => "codex logout",
                _ => "claude auth logout",
            };
            self.entries.push(Entry::Error(format!(
                "Only accounts added here can be removed here. To sign this one out, run \
`{logout}`."
            )));
            return;
        }
        if login.same_login(&self.backend) {
            self.entries.push(Entry::Error(
                "That's the current backend; switch to another one before removing it.".into(),
            ));
            return;
        }
        self.logins.remove(i);
        let last = self.backend_choices().len() - 1;
        if let Some(picker) = &mut self.backend_picker {
            picker.selected = picker.selected.min(last);
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let label = login.label_with_account();
            let result = logins::remove(&login)
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
