//! `/goal <condition>`: keeps the agent working, turn after turn, until a condition holds.
//!
//! After each turn a separate checker (a subagent on the same backend that can't edit
//! files) inspects the workspace and decides whether the condition holds, ending its
//! answer with [`MET`] or [`NOT_MET`]. Until it says [`MET`], the agent gets a
//! continuation prompt with the checker's findings, up to [`MAX_TURNS`] turns. The agent
//! can end a turn with [`BLOCKED`] when only the user can unblock it. Errors, interrupts,
//! [`BLOCKED`] and the turn limit pause the goal; `/goal resume` picks it up again.

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::App;
use crate::agent::{AgentEvent, TurnCtx};
use crate::backend;
use crate::session::Entry;

/// Turns a goal may take, including the first, before it is paused.
pub(super) const MAX_TURNS: u32 = 20;
const MET: &str = "[goal met]";
const NOT_MET: &str = "[goal not met]";
const BLOCKED: &str = "[goal blocked]";

pub(super) struct Goal {
    pub(super) condition: String,
    /// Turns started for this goal so far.
    pub(super) turns: u32,
    /// Set by interrupts, errors and [`BLOCKED`]; no turns start until resumed.
    pub(super) paused: bool,
    /// Whether the checker is running.
    pub(super) checking: bool,
}

fn instructions(condition: &str) -> String {
    format!(
        "<goal>\n{condition}\n</goal>\n\n\
Work toward the goal above until it is fully achieved. After each of your turns a separate \
checker inspects the workspace and decides whether the goal is met; if it isn't, you'll be \
asked to continue with the checker's findings. Verify your own work (e.g. run the relevant \
tests or checks) before ending a turn. If you can't make progress without the user (missing \
information, a decision only they can make, or a problem you can't get past), end your final \
message with a line containing exactly {BLOCKED} and say what you need."
    )
}

fn continuation(condition: &str, turn: u32, findings: &str) -> String {
    format!(
        "The checker found the goal isn't met yet (turn {turn} of {MAX_TURNS}). Its findings:\n\n\
<checker>\n{findings}\n</checker>\n\nContinue working toward the goal:\n\n<goal>\n{condition}\n\
</goal>\n\nEnd your final message with {BLOCKED} if you need the user."
    )
}

fn checker_task(condition: &str, report: &str) -> String {
    format!(
        "Decide whether another agent has achieved the goal below. Don't take its report on \
trust: verify independently by reading the relevant files and running read-only commands \
such as tests, builds or linters. Don't modify anything (file edits are blocked).\n\n\
<goal>\n{condition}\n</goal>\n\n<agent-report>\n{report}\n</agent-report>\n\n\
Reply with a short account of what you checked and found. If the goal isn't met, say \
concretely what is still missing or wrong. End your reply with a line containing exactly \
{MET} or {NOT_MET}."
    )
}

/// The marker the text's last line ends with, if any.
fn verdict(text: &str) -> Option<&'static str> {
    let last = text.trim_end().lines().last()?;
    let last = last.trim_matches(|c: char| c.is_whitespace() || "`*_.".contains(c));
    let last = last.to_ascii_lowercase();
    [MET, NOT_MET, BLOCKED]
        .into_iter()
        .find(|m| last.ends_with(m))
}

impl App {
    pub(super) fn goal_command(&mut self, arg: &str) {
        match arg {
            "" => self.entries.push(Entry::Info(match &self.goal {
                None => {
                    "No goal set. Usage: /goal <condition> (then /goal clear, /goal resume)".into()
                }
                Some(goal) => format!(
                    "Goal{} ({} of {MAX_TURNS} turns): {}",
                    if goal.paused { ", paused" } else { "" },
                    goal.turns,
                    goal.condition
                ),
            })),
            "clear" | "off" | "stop" => match self.goal.take() {
                Some(goal) => self
                    .entries
                    .push(Entry::Info(format!("Goal cleared: {}", goal.condition))),
                None => self.entries.push(Entry::Info("No goal set.".into())),
            },
            "resume" | "continue" if self.is_running() && self.goal.is_some() => self.entries.push(
                Entry::Error("The agent is already running (Esc to interrupt first).".into()),
            ),
            "resume" | "continue" => match &mut self.goal {
                None => self.entries.push(Entry::Error("No goal to resume.".into())),
                Some(goal) => {
                    goal.paused = false;
                    goal.turns = 0;
                    self.continue_goal("(None yet: the goal was resumed.)");
                }
            },
            _ if self.is_running() => {
                self.entries.push(Entry::Error(
                    "Can't set a goal while the agent is running (Esc to interrupt first).".into(),
                ));
            }
            condition if self.refuse_without_backend(&format!("/goal {condition}")) => {}
            condition => {
                self.goal = Some(Goal {
                    condition: condition.to_string(),
                    turns: 1,
                    paused: false,
                    checking: false,
                });
                self.send_prompt_as(format!("/goal {condition}"), instructions(condition));
            }
        }
    }

    /// Pauses the goal, with the reason shown in the transcript.
    pub(super) fn pause_goal(&mut self, reason: &str) {
        if let Some(goal) = self.goal.as_mut().filter(|g| !g.paused) {
            goal.paused = true;
            goal.checking = false;
            self.entries.push(Entry::Info(format!(
                "Goal paused ({reason}). /goal resume to continue, /goal clear to drop it."
            )));
        }
    }

    /// Called when a turn ends with nothing queued: pauses the goal or starts the checker.
    pub(super) fn after_goal_turn(&mut self, failed: bool) {
        if self.goal.as_ref().is_none_or(|g| g.paused) {
            return;
        }
        if failed {
            self.pause_goal("the turn failed");
            return;
        }
        let start = self.checkpoints.last().map_or(0, |t| t.entries_len);
        let report = self.entries[start.min(self.entries.len())..]
            .iter()
            .rev()
            .find_map(|e| match e {
                Entry::Assistant(text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        if verdict(&report) == Some(BLOCKED) {
            self.pause_goal("the agent needs your input");
            return;
        }
        self.start_goal_check(&report);
    }

    /// Runs the checker as its own task: its steps show in the transcript and its tool
    /// calls follow the session's permission rules, with file edits denied.
    fn start_goal_check(&mut self, report: &str) {
        let Some(goal) = self.goal.as_mut() else {
            return;
        };
        goal.checking = true;
        let task = checker_task(&goal.condition, report);
        self.entries.push(Entry::Info(
            "Checking the goal with a separate agent…".into(),
        ));
        let checker = backend::Subagent {
            backend: self.backend.clone(),
            model: self.settings.model.name.clone(),
            config: self.settings.turn_config(0),
        };
        let policy = self.permissions.lock().unwrap().read_only();
        let ctx = TurnCtx::new(Arc::new(Mutex::new(policy)), None);
        let workspace = self.workspace.clone();
        let atx = self.start_turn();
        self.task = Some(tokio::spawn(async move {
            let (answer, failed) = checker
                .run(&json!({ "prompt": task }), ctx, &workspace, &atx)
                .await;
            let _ = atx.send(AgentEvent::GoalChecked { answer, failed });
        }));
    }

    /// The checker's answer: finishes the goal, or continues it with the findings.
    pub(super) fn on_goal_checked(&mut self, answer: String, failed: bool) {
        self.task = None;
        let Some(goal) = self.goal.as_mut().filter(|g| g.checking) else {
            return;
        };
        goal.checking = false;
        if failed {
            self.entries.push(Entry::Error(answer));
            self.pause_goal("the checker failed");
            return;
        }
        match verdict(&answer) {
            Some(MET) => {
                let goal = self.goal.take().unwrap();
                self.entries.push(Entry::Info(format!(
                    "✓ Goal achieved after {} turn{} (confirmed by the checker): {}",
                    goal.turns,
                    if goal.turns == 1 { "" } else { "s" },
                    goal.condition,
                )));
                self.entries
                    .push(Entry::Note(answer.trim_end().to_string()));
            }
            Some(NOT_MET) => {
                self.entries
                    .push(Entry::Info("✗ Checker: goal not met yet.".into()));
                self.entries
                    .push(Entry::Note(answer.trim_end().to_string()));
                self.continue_goal(&answer);
            }
            _ => {
                self.entries.push(Entry::Note(answer));
                self.pause_goal("the checker gave no verdict");
            }
        }
    }

    fn continue_goal(&mut self, findings: &str) {
        let Some(goal) = self.goal.as_mut() else {
            return;
        };
        if goal.turns >= MAX_TURNS {
            goal.paused = true;
            self.entries.push(Entry::Info(format!(
                "Goal paused after {MAX_TURNS} turns without being achieved. /goal resume for \
{MAX_TURNS} more, /goal clear to drop it."
            )));
            return;
        }
        goal.turns += 1;
        let prompt = continuation(&goal.condition, goal.turns, findings.trim());
        self.send_prompt(prompt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_reads_the_last_line_only() {
        assert_eq!(verdict("All tests pass.\n\n[goal met]\n"), Some(MET));
        assert_eq!(verdict("Done.\n**[Goal Met]**"), Some(MET));
        assert_eq!(verdict("Done. `[goal met]`."), Some(MET));
        assert_eq!(verdict("2 tests fail.\n[goal not met]"), Some(NOT_MET));
        assert_eq!(
            verdict("I need the API key.\n[goal blocked]"),
            Some(BLOCKED)
        );
        assert_eq!(
            verdict("I'll write [goal met] when done.\nStill working."),
            None
        );
        assert_eq!(verdict(""), None);
    }
}
