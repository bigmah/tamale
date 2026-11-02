use crossterm::event::{DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEvent};
use crossterm::execute;

use super::{
    App, AppEvent, BrowserRequest, EFFORT_CHOICES, FALLBACK_MODELS, ModelInfo, ModelPicker,
    WebRequest, effort_label,
};
use crate::session::Entry;
use crate::{agent, permissions, session};

impl App {
    /// With no argument, show the current scope. A running turn keeps its snapshot.
    pub(super) fn add_dir(&mut self, arg: &str) {
        if arg.is_empty() {
            let mut scope = format!("Working directory: {}", self.workspace.cwd.display());
            if self.workspace.additional_dirs.is_empty() {
                scope.push_str("\nNo additional directories. Usage: /add-dir <path>");
            } else {
                scope.push_str("\nAdditional directories:");
                for dir in &self.workspace.additional_dirs {
                    scope.push_str(&format!("\n  {}", dir.display()));
                }
            }
            self.entries.push(Entry::Info(scope));
            return;
        }
        // The remainder is one path, so spaces work with or without enclosing quotes.
        let path = arg
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| arg.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(arg);
        match self.workspace.add_dir(std::path::Path::new(path)) {
            Ok(true) => {
                let dir = self.workspace.additional_dirs.last().unwrap();
                let note = if self.is_running() {
                    " (takes effect on your next message)"
                } else {
                    ""
                };
                self.entries.push(Entry::Info(format!(
                    "Added directory to scope: {}{note}.",
                    dir.display()
                )));
                self.save_session();
            }
            Ok(false) => self.entries.push(Entry::Info(format!(
                "Directory is already in scope: {path}"
            ))),
            Err(error) => self
                .entries
                .push(Entry::Error(format!("Couldn't add directory: {error:#}"))),
        }
    }

    /// Lists the HTML artifacts this session's `/web` replies saved, newest first.
    pub(super) fn list_artifacts(&mut self) {
        let Some(dir) = crate::web::artifact_dir(&self.session_id) else {
            self.entries.push(Entry::Error(
                "Can't determine the sessions directory (HOME not set).".into(),
            ));
            return;
        };
        let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "html"))
            .map(|e| {
                let modified = e.metadata().and_then(|m| m.modified());
                (modified.unwrap_or(std::time::UNIX_EPOCH), e.path())
            })
            .collect();
        if files.is_empty() {
            self.entries.push(Entry::Info(
                "No artifacts saved for this session yet. Replies in /web save theirs here.".into(),
            ));
            return;
        }
        files.sort_by(|a, b| b.0.cmp(&a.0));
        let mut text = format!("Artifacts saved in {}:", dir.display());
        for (_, path) in &files {
            text.push_str(&format!("\n  {}", path.display()));
        }
        self.entries.push(Entry::Info(text));
    }

    pub(super) fn open_picker(&mut self) {
        let mut picker = ModelPicker::default();
        if let Some(models) = &self.models {
            picker.selected = models
                .iter()
                .position(|m| m.id == self.settings.model.name)
                .unwrap_or(0);
        } else {
            let backend = self.backend.clone();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let result = backend.list_models().await.map(|list| {
                    list.into_iter()
                        .map(|(id, name)| ModelInfo { id, name })
                        .collect()
                });
                let _ = tx.send(AppEvent::Models(result));
            });
        }
        self.picker = Some(picker);
    }

    pub(super) fn on_models(&mut self, result: Result<Vec<ModelInfo>, String>) {
        let models = match result {
            Ok(models) if !models.is_empty() => models,
            other => {
                if let Err(e) = other {
                    self.entries.push(Entry::Error(format!(
                        "Couldn't fetch model list ({e}); showing built-in list."
                    )));
                }
                FALLBACK_MODELS
                    .iter()
                    .map(|(id, name)| ModelInfo {
                        id: id.to_string(),
                        name: name.to_string(),
                    })
                    .collect()
            }
        };
        if let Some(picker) = &mut self.picker {
            picker.selected = models
                .iter()
                .position(|m| m.id == self.settings.model.name)
                .unwrap_or(0);
        }
        self.models = Some(models);
    }

    /// Models matching the picker's filter.
    pub(super) fn filtered_models(&self) -> Vec<&ModelInfo> {
        let filter = self
            .picker
            .as_ref()
            .map(|p| p.filter.to_lowercase())
            .unwrap_or_default();
        self.models
            .iter()
            .flatten()
            .filter(|m| {
                m.id.to_lowercase().contains(&filter) || m.name.to_lowercase().contains(&filter)
            })
            .collect()
    }

    pub(super) fn on_picker_key(&mut self, key: KeyEvent, ctrl: bool) {
        let count = self.filtered_models().len();
        let Some(picker) = &mut self.picker else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Char('c') if ctrl => self.picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Char('p') if ctrl => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Char('n') if ctrl => {
                picker.selected = (picker.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Home => picker.selected = 0,
            KeyCode::End => picker.selected = count.saturating_sub(1),
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.selected = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                picker.filter.push(c);
                picker.selected = 0;
            }
            KeyCode::Enter => {
                let selected = picker.selected;
                if let Some(id) = self.filtered_models().get(selected).map(|m| m.id.clone()) {
                    self.picker = None;
                    self.set_model(id);
                }
            }
            _ => {}
        }
    }

    pub(super) fn set_model(&mut self, id: String) {
        if id == self.settings.model.name {
            return;
        }
        let note = if self.is_running() {
            " (takes effect on your next message)"
        } else {
            ""
        };
        self.entries
            .push(Entry::Info(format!("Switched model to {id}{note}.")));
        self.settings.model.name = id;
    }

    // -------------------------------------------------------- effort picker

    pub(super) fn open_effort_picker(&mut self) {
        let selected = EFFORT_CHOICES
            .iter()
            .position(|(e, _)| *e == self.settings.model.effort)
            .unwrap_or(0);
        self.effort_picker = Some(selected);
    }

    pub(super) fn on_effort_picker_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(selected) = self.effort_picker else {
            return;
        };
        let last = EFFORT_CHOICES.len() - 1;
        self.effort_picker = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => None,
            KeyCode::Char('c') if ctrl => None,
            KeyCode::Up | KeyCode::Char('k') => Some(selected.saturating_sub(1)),
            KeyCode::Char('p') if ctrl => Some(selected.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => Some((selected + 1).min(last)),
            KeyCode::Char('n') if ctrl => Some((selected + 1).min(last)),
            KeyCode::Home | KeyCode::Char('g') => Some(0),
            KeyCode::End | KeyCode::Char('G') => Some(last),
            KeyCode::Char(c @ '1'..='9') => {
                let i = c as usize - '1' as usize;
                if i <= last {
                    self.effort_picker = None;
                    self.apply_effort(EFFORT_CHOICES[i].0);
                    return;
                }
                Some(selected)
            }
            KeyCode::Enter => {
                self.effort_picker = None;
                self.apply_effort(EFFORT_CHOICES[selected].0);
                return;
            }
            _ => Some(selected),
        };
    }

    pub(super) fn apply_effort(&mut self, effort: Option<agent::Effort>) {
        if effort == self.settings.model.effort {
            return;
        }
        let note = if self.is_running() {
            " (takes effect on your next message)"
        } else {
            ""
        };
        self.settings.model.effort = effort;
        self.entries.push(Entry::Info(format!(
            "Effort set to {}{note}.",
            effort_label(effort)
        )));
    }

    pub(super) fn set_effort(&mut self, arg: Option<&str>) {
        let Some(arg) = arg else {
            self.open_effort_picker();
            return;
        };
        let effort = match agent::Effort::parse_setting(arg) {
            Ok(effort) => effort,
            Err(error) => {
                self.entries.push(Entry::Error(error.to_string()));
                return;
            }
        };
        self.apply_effort(effort);
    }

    pub(super) fn set_thinking(&mut self, arg: Option<&str>) {
        let thinking = match arg {
            None => match self.settings.model.thinking {
                agent::Thinking::Adaptive => agent::Thinking::Off,
                agent::Thinking::Off => agent::Thinking::Adaptive,
            },
            Some(arg) => match agent::Thinking::parse(arg) {
                Some(thinking) => thinking,
                None => {
                    self.entries.push(Entry::Error(format!(
                        "Unknown thinking mode: {arg} (use adaptive, on, off)"
                    )));
                    return;
                }
            },
        };
        self.settings.model.thinking = thinking;
        let note = if self.is_running() {
            " (takes effect on your next message)"
        } else {
            ""
        };
        self.entries.push(Entry::Info(format!(
            "Thinking set to {}{note}.",
            thinking.as_str()
        )));
    }

    pub(super) fn set_verbose(&mut self, arg: Option<&str>) {
        let verbose = match arg.map(str::to_ascii_lowercase).as_deref() {
            None => !self.settings.ui.verbose,
            Some("on" | "true" | "yes" | "1") => true,
            Some("off" | "false" | "no" | "0") => false,
            Some(_) => {
                self.entries.push(Entry::Error(format!(
                    "Unknown verbose mode: {} (use on, off)",
                    arg.unwrap_or_default()
                )));
                return;
            }
        };
        self.settings.ui.verbose = verbose;
        self.entries.push(Entry::Info(format!(
            "Verbose output {}.",
            if verbose { "on" } else { "off" }
        )));
    }

    pub(super) fn set_mouse(&mut self, arg: Option<&str>) {
        let mouse = match arg.map(str::to_ascii_lowercase).as_deref() {
            None => !self.settings.ui.mouse,
            Some("on" | "true" | "yes" | "1") => true,
            Some("off" | "false" | "no" | "0") => false,
            Some(_) => {
                self.entries.push(Entry::Error(format!(
                    "Unknown mouse mode: {} (use on, off)",
                    arg.unwrap_or_default()
                )));
                return;
            }
        };
        let result = if mouse {
            execute!(std::io::stdout(), EnableMouseCapture)
        } else {
            execute!(std::io::stdout(), DisableMouseCapture)
        };
        if let Err(e) = result {
            self.entries
                .push(Entry::Error(format!("Couldn't change mouse mode: {e}")));
            return;
        }
        self.settings.ui.mouse = mouse;
        self.entries.push(Entry::Info(if mouse {
            "Mouse scrolling on (hold Shift/Option to select text).".into()
        } else {
            "Mouse scrolling off (native text selection restored).".into()
        }));
    }

    /// Without an argument, shows the mode and rules.
    pub(super) fn set_permission_mode(&mut self, arg: Option<&str>) {
        let Some(arg) = arg else {
            let summary = self.permissions.lock().unwrap().describe();
            self.entries.push(Entry::Info(summary));
            return;
        };
        match permissions::Mode::parse(arg) {
            Some(mode) => self.apply_permission_mode(mode),
            None => self.entries.push(Entry::Error(format!(
                "Unknown permission mode: {arg} (use ask, accept-edits, auto)"
            ))),
        }
    }

    pub(super) fn apply_permission_mode(&mut self, mode: permissions::Mode) {
        self.permissions.lock().unwrap().mode = mode;
        self.entries.push(Entry::Info(format!(
            "Permission mode: {} ({}).",
            mode.as_str(),
            mode.describe()
        )));
    }

    pub fn take_browser_request(&mut self) -> Option<BrowserRequest> {
        self.browser_request.take()
    }

    /// Reports the outcome of a `/browser` request.
    pub fn on_browser(&mut self, result: Result<String, String>) {
        self.entries.push(match result {
            Ok(message) => Entry::Info(message),
            Err(message) => Entry::Error(message),
        });
    }

    pub(super) fn command(&mut self, cmd: &str) {
        match cmd.split_whitespace().next().unwrap_or("") {
            "/quit" | "/exit" => self.should_quit = true,
            "/clear" if self.is_running() => self.entries.push(Entry::Error(
                "Can't clear while the agent is running (Esc to interrupt first).".into(),
            )),
            "/clear" => {
                self.save_session();
                self.history.clear();
                self.thread = None;
                self.checkpoints.clear();
                self.goal = None;
                self.permissions.lock().unwrap().clear_session();
                self.context_tokens = 0;
                self.entries.clear();
                self.transcript.invalidate_from(0);
                self.session_id = session::new_id();
                self.session_created = session::now();
                self.entries
                    .push(Entry::Info("Conversation cleared.".into()));
            }
            "/resume" | "/continue" => self.resume_command(cmd.split_whitespace().nth(1)),
            "/add-dir" => self.add_dir(cmd["/add-dir".len()..].trim()),
            "/goal" => self.goal_command(cmd["/goal".len()..].trim()),
            "/help" => self.help_scroll = Some(0),
            "/thinking" => self.set_thinking(cmd.split_whitespace().nth(1)),
            "/verbose" => self.set_verbose(cmd.split_whitespace().nth(1)),
            "/mouse" => self.set_mouse(cmd.split_whitespace().nth(1)),
            "/copy" => match cmd.split_whitespace().nth(1) {
                None => self.copy_clipboard(false),
                Some("all") => self.copy_clipboard(true),
                Some(_) => self.entries.push(Entry::Error("Usage: /copy [all]".into())),
            },
            "/paste" => self.paste_clipboard(),
            "/compact" => {
                let rest = cmd["/compact".len()..].trim();
                self.compact((!rest.is_empty()).then(|| rest.to_string()));
            }
            "/model" | "/models" => match cmd.split_whitespace().nth(1) {
                Some(id) => self.set_model(id.to_string()),
                None => self.open_picker(),
            },
            "/effort" => self.set_effort(cmd.split_whitespace().nth(1)),
            "/undo" => self.undo(),
            "/login" => self.login(),
            "/backend" => self.switch_backend(cmd.split_whitespace().nth(1)),
            "/rewind" => self.open_rewind_picker(),
            "/diff" => self.open_diff(cmd["/diff".len()..].trim()),
            "/permissions" => self.set_permission_mode(cmd.split_whitespace().nth(1)),
            "/browser" => match cmd.split_whitespace().nth(1) {
                None | Some("on") => self.browser_request = Some(BrowserRequest::Open),
                Some("off") => self.browser_request = Some(BrowserRequest::Close),
                Some(_) => self
                    .entries
                    .push(Entry::Error("Usage: /browser [on|off]".into())),
            },
            "/artifacts" => self.list_artifacts(),
            "/web" => match cmd.split_whitespace().nth(1) {
                None | Some("on" | "lan") => self.open_web(),
                Some("local") => self.web_request = Some(WebRequest::Open { lan: false }),
                Some("off") => self.web_request = Some(WebRequest::Close),
                Some("remote") => match cmd.split_whitespace().nth(2) {
                    // Pairing is the code on the screen this opens.
                    None | Some("on" | "pair") => {
                        self.web_request = Some(WebRequest::Remote { on: true })
                    }
                    Some("off") => self.web_request = Some(WebRequest::Remote { on: false }),
                    Some("rekey") => self.web_request = Some(WebRequest::Rekey),
                    Some(_) => self
                        .entries
                        .push(Entry::Error("Usage: /web remote [off|pair|rekey]".into())),
                },
                Some(_) => self
                    .entries
                    .push(Entry::Error("Usage: /web [local|remote|off]".into())),
            },
            other => self.entries.push(Entry::Error(format!(
                "Unknown command: {other} (type /help to see all commands)"
            ))),
        }
    }
}
