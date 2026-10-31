//! What the `/web` UI shows of the session, and the input it sends back.

use super::{App, BrowserRequest};
use crate::permissions::Approval;
use crate::session::Entry;
use crate::web;

impl App {
    pub fn take_web_request(&mut self) -> Option<BrowserRequest> {
        self.web_request.take()
    }

    /// Reports the outcome of a `/web` request.
    pub fn on_web(&mut self, result: Result<String, String>) {
        self.entries.push(match result {
            Ok(message) => Entry::Info(message),
            Err(message) => Entry::Error(message),
        });
    }

    pub fn web_view(&self) -> web::View<'_> {
        let since = self.busy_since.filter(|_| self.is_running()).map(|since| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            now.saturating_sub(since.elapsed()).as_secs()
        });
        let cwd = &self.workspace.cwd;
        let status = web::Status {
            project: cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| cwd.display().to_string()),
            cwd: cwd.display().to_string(),
            dirs: self
                .workspace
                .additional_dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect(),
            model: self.backend.model_label(&self.settings.model.name),
            backend: self.backend_label(),
            signed_in: self.backend.signed_in(),
            mode: self.permissions.lock().unwrap().mode.as_str(),
            effort: self.settings.model.effort.map(|e| e.as_str()),
            thinking: self.settings.model.thinking.as_str(),
            running: self.is_running(),
            compacting: self.compacting,
            since,
            context: self.context_tokens,
            window: self.settings.model.context_window(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cost: self.cost,
            cost_partial: self.cost_partial,
            goal: self.goal.as_ref().map(|g| g.condition.clone()),
            queued: self.queued_messages(),
        };
        let approval = self.approval.as_ref().map(|p| web::Approval {
            name: p.name.clone(),
            summary: p.summary.clone(),
            description: p.description.clone(),
            always: p.always.clone(),
        });
        let mut roots = vec![self.workspace.cwd.clone()];
        roots.extend(self.workspace.additional_dirs.iter().cloned());
        web::View {
            status,
            entries: &self.entries,
            approval,
            roots,
        }
    }

    /// A prompt or slash command from the web UI, handled as if typed here.
    pub fn web_send(&mut self, text: String) {
        let text = text.trim().to_string();
        if !text.is_empty() {
            self.submit_text(text);
        }
    }

    pub fn web_interrupt(&mut self) {
        self.interrupt();
    }

    pub fn web_approve(&mut self, choice: web::Choice, reason: Option<String>) {
        if self.approval.is_none() {
            return;
        }
        match choice {
            web::Choice::Yes => self.answer(Approval::Allow),
            web::Choice::Always => self.allow_always(),
            web::Choice::No => self.answer(Approval::Deny { reason }),
        }
    }
}
