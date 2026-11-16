//! `/goal <condition>`: keeps the agent working, turn after turn, until a condition holds.
//!
//! After each turn a separate checker (a subagent on the same backend that can't edit
//! files) inspects the workspace and decides whether the condition holds, ending its
//! answer with [`MET`] or [`NOT_MET`]. Until it says [`MET`], the agent gets a
//! continuation prompt with the checker's findings, up to [`MAX_TURNS`] turns at a time.
//!
//! The goal pauses when the agent ends a turn with [`BLOCKED`] (only the user can unblock
//! it), on interrupts, after [`MAX_IDLE_TURNS`] turns in a row without a tool call, at the
//! turn limit, and when errors outlast [`RETRY_DELAYS`]. A paused goal continues with the
//! user's next message or `/goal resume`; `/goal pause` holds it until `/goal resume`.
//! The goal is saved with the session, so `/resume` brings it back (paused).

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::{App, AppEvent};
use crate::agent::{AgentEvent, TurnCtx};
use crate::backend;
use crate::session::{self, Entry};

/// Turns a goal may take in a row, including the first, before it is paused.
pub(super) const MAX_TURNS: u32 = 20;
/// Goal turns in a row without a tool call before the goal is paused as stuck.
const MAX_IDLE_TURNS: u32 = 3;
/// Seconds to wait before each automatic retry after a failed turn or check.
const RETRY_DELAYS: [u64; 3] = [60, 300, 900];
/// User messages sent during a goal that the checker is shown, the latest kept.
const MAX_NOTES: usize = 10;
/// The longest condition accepted, in characters.
const MAX_CONDITION_CHARS: usize = 4_000;
const MET: &str = "[goal met]";
const NOT_MET: &str = "[goal not met]";
const BLOCKED: &str = "[goal blocked]";

pub(super) struct Goal {
    pub(super) condition: String,
    /// Turns started for this goal so far, across resumes.
    pub(super) turns: u32,
    /// Turns started since it was set or last resumed; [`MAX_TURNS`] limits these.
    pub(super) run_turns: u32,
    /// No turns start until it's resumed (by a message, or `/goal resume`).
    pub(super) paused: bool,
    /// Paused with `/goal pause`: only `/goal resume` continues it.
    pub(super) held: bool,
    /// Whether the checker is running.
    pub(super) checking: bool,
    /// The checker's latest findings.
    pub(super) last_check: Option<String>,
    /// What the user said while the goal was worked on, which the checker should know.
    pub(super) notes: Vec<String>,
    /// The agent's report from the last turn, kept to retry a failed check.
    report: String,
    /// Goal turns in a row that made no tool call.
    idle_turns: u32,
    /// Automatic retries since the last turn that went through.
    retries: usize,
    /// A scheduled retry: its id and when it's due (Unix seconds).
    pub(super) retry: Option<Retry>,
    retry_ids: u64,
    /// Seconds spent before the current stretch of work.
    elapsed: u64,
    /// When the current stretch of work started (Unix seconds), if it's being worked on.
    since: Option<u64>,
    /// Tokens spent before the current stretch of work.
    tokens: u64,
    /// The session's token count when the current stretch started.
    tokens_base: Option<u64>,
}

#[derive(Clone, Copy)]
pub(super) struct Retry {
    id: u64,
    pub(super) at: u64,
    /// Whether it retries the check rather than the agent's turn.
    check: bool,
}

impl Goal {
    fn new(condition: String) -> Self {
        Goal {
            condition,
            turns: 0,
            run_turns: 0,
            paused: true,
            held: false,
            checking: false,
            last_check: None,
            notes: Vec::new(),
            report: String::new(),
            idle_turns: 0,
            retries: 0,
            retry: None,
            retry_ids: 0,
            elapsed: 0,
            since: None,
            tokens: 0,
            tokens_base: None,
        }
    }

    pub(super) fn from_saved(saved: session::Goal) -> Self {
        Goal {
            turns: saved.turns,
            elapsed: saved.elapsed,
            tokens: saved.tokens,
            last_check: saved.last_check,
            notes: saved.notes,
            ..Goal::new(saved.condition)
        }
    }

    /// Seconds spent on the goal.
    pub(super) fn elapsed(&self) -> u64 {
        self.elapsed + self.since.map_or(0, |s| session::now().saturating_sub(s))
    }

    /// Tokens spent on the goal, given the session's count.
    fn tokens(&self, session_tokens: u64) -> u64 {
        self.tokens
            + self
                .tokens_base
                .map_or(0, |b| session_tokens.saturating_sub(b))
    }

    /// Where it stands, for the status line: `goal 3/20 · 4m`, `goal paused 3/20`, ….
    pub(super) fn label(&self) -> String {
        let state = if self.checking {
            " checking"
        } else if self.held {
            " held"
        } else if self.paused {
            " paused"
        } else if self.retry.is_some() {
            " retrying"
        } else {
            ""
        };
        // A paused goal's run is over; its total says how far it got.
        let mut label = if self.paused {
            format!(
                "goal{state} · {} turn{}",
                self.turns,
                if self.turns == 1 { "" } else { "s" }
            )
        } else {
            format!("goal{state} {}/{MAX_TURNS}", self.run_turns)
        };
        if let Some(retry) = &self.retry {
            let wait = retry.at.saturating_sub(session::now());
            label.push_str(&format!(" in {}", fmt_duration(wait)));
        } else {
            let elapsed = self.elapsed();
            if elapsed >= 60 {
                label.push_str(&format!(" · {}", fmt_duration(elapsed)));
            }
        }
        label
    }
}

/// `45s`, `12m`, `2h 5m`.
fn fmt_duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ if secs % 3600 / 60 == 0 => format!("{}h", secs / 3600),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

fn instructions(condition: &str) -> String {
    format!(
        "<goal>\n{condition}\n</goal>\n\n\
Work toward the goal above, set by the user, until it is fully achieved. It persists across \
turns: after each of your turns a separate checker inspects the workspace (reading files and \
running commands such as tests) and decides whether the goal is met. If it isn't, you'll be \
asked to continue with the checker's findings.\n\n\
How to work:\n\
- Pursue the goal as stated. Don't narrow it, substitute an easier or merely compatible \
version, or redefine success around what already exists. If it can't all be done in one turn, \
make concrete progress toward the real end state and keep going.\n\
- Treat the workspace as the source of truth: inspect its current state rather than relying \
on memory or earlier reports.\n\
- Before ending a turn, verify your work against each requirement of the goal (run the \
relevant tests, builds or checks). Report what you verified, with the commands you ran and \
their results, and say plainly what isn't done yet. The checker judges the actual workspace, \
so claims without evidence don't help.\n\
- Make reasonable assumptions instead of stopping to ask: note them and carry on. Only if you \
truly can't make progress without the user (missing access or credentials, a decision only \
they can make, an external problem you can't get past) end your final message with a line \
containing exactly {BLOCKED} and say what you need. Don't use it because the work is hard, \
slow or uncertain."
    )
}

fn continuation(condition: &str, turn: u32, findings: &str) -> String {
    format!(
        "The checker found the goal isn't met yet (turn {turn} of {MAX_TURNS}). Its findings:\n\n\
<checker>\n{findings}\n</checker>\n\nContinue working toward the goal:\n\n<goal>\n{condition}\n\
</goal>\n\n\
Address the checker's findings, checking them against the workspace since it can be wrong, \
and keep the whole goal in scope. If your last turn didn't make progress, try a different \
approach rather than repeating it. Verify and report as before, and end your final message \
with {BLOCKED} only if you need the user."
    )
}

/// Continues after the user resumed the goal, or a failed turn is retried.
fn resumption(condition: &str, turn: u32, why: &str) -> String {
    format!(
        "{why} Continue working toward the goal (turn {turn} of {MAX_TURNS}):\n\n<goal>\n\
{condition}\n</goal>\n\n\
Check the current state of the workspace first, then keep going. Verify and report as \
before, and end your final message with {BLOCKED} only if you need the user."
    )
}

/// Appended to the user's message when it resumes a paused goal.
fn reminder(condition: &str) -> String {
    format!(
        "\n\n<system-reminder>\nThis message resumes the paused goal below. Take it into \
account, then continue working toward the goal; a checker will inspect the workspace after \
this turn.\n<goal>\n{condition}\n</goal>\n</system-reminder>"
    )
}

fn checker_task(condition: &str, notes: &[String], report: &str) -> String {
    let notes = if notes.is_empty() {
        String::new()
    } else {
        format!(
            "<user-messages>\nWhat the user said while the goal was being worked on. It \
refines the goal: count its requests and constraints as requirements.\n\n{}\n</user-messages>\n\n",
            notes.join("\n---\n")
        )
    };
    format!(
        "Decide whether another agent has achieved the goal below. The goal was set by the \
user; the agent's report is a claim, not evidence. Verify independently against the current \
workspace by reading the relevant files and running read-only commands such as tests, builds \
or linters. Don't modify anything (file edits are blocked).\n\n\
<goal>\n{condition}\n</goal>\n\n{notes}<agent-report>\n{report}\n</agent-report>\n\n\
How to judge:\n\
- Break the goal into its concrete requirements (every named file, command, test, behavior, \
deliverable and constraint) and check each against authoritative evidence: the files \
themselves and the output of commands you ran.\n\
- Match the check to the claim: a narrow check doesn't prove a broad requirement, and passing \
tests only prove what they cover.\n\
- The goal must hold as stated, not a narrower or easier version of it.\n\
- Weak, indirect or missing evidence means the goal isn't met.\n\n\
Reply with a short account of what you checked and found. If the goal isn't met, say \
concretely what is still missing or wrong, so the agent can fix it. End your reply with a line \
containing exactly {MET} or {NOT_MET}."
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
        let (word, rest) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
        match (word.to_ascii_lowercase().as_str(), rest.trim()) {
            ("", _) => {
                let status = self.goal_status();
                self.entries.push(Entry::Info(status));
            }
            ("clear" | "off" | "stop" | "cancel" | "reset" | "none", "") => {
                match self.goal.take() {
                    Some(goal) => {
                        if goal.checking {
                            self.abort_checker();
                        }
                        self.entries
                            .push(Entry::Info(format!("Goal cleared: {}", goal.condition)));
                        self.save_session();
                    }
                    None => self.entries.push(Entry::Info("No goal set.".into())),
                }
            }
            ("pause", "") => match &mut self.goal {
                None => self.entries.push(Entry::Error("No goal to pause.".into())),
                Some(goal) if goal.held => self
                    .entries
                    .push(Entry::Info("The goal is already paused.".into())),
                Some(goal) => {
                    goal.held = true;
                    let checking = goal.checking;
                    self.pause_goal_quietly();
                    if checking {
                        self.abort_checker();
                    }
                    self.entries.push(Entry::Info(
                        "Goal paused. /goal resume to continue, /goal clear to drop it.".into(),
                    ));
                }
            },
            ("resume" | "continue", "")
                if self.goal.is_some()
                    && !self.is_running()
                    && self.refuse_without_backend("/goal resume") => {}
            ("resume" | "continue", "") => match &self.goal {
                None => self.entries.push(Entry::Error("No goal to resume.".into())),
                Some(_) if self.is_running() => self.entries.push(Entry::Error(
                    "The agent is already running (Esc to interrupt first).".into(),
                )),
                Some(goal) if goal.retry.is_some_and(|r| r.check) => {
                    self.activate_goal();
                    self.retry_goal_check();
                }
                Some(_) => {
                    self.activate_goal();
                    self.start_goal_turn("The user resumed the goal.");
                }
            },
            _ if self.is_running() => self.entries.push(Entry::Error(
                "Can't set a goal while the agent is running (Esc to interrupt first).".into(),
            )),
            _ if arg.chars().count() > MAX_CONDITION_CHARS => {
                self.entries.push(Entry::Error(format!(
                    "A goal is limited to {MAX_CONDITION_CHARS} characters (this one has {}). \
Put the details in a file and point the goal at it.",
                    arg.chars().count()
                )))
            }
            _ if self.refuse_without_backend(&format!("/goal {arg}")) => {}
            _ => {
                if let Some(old) = self.goal.take() {
                    self.entries
                        .push(Entry::Info(format!("Replaced goal: {}", old.condition)));
                }
                self.goal = Some(Goal::new(arg.to_string()));
                self.activate_goal();
                let goal = self.goal.as_mut().unwrap();
                goal.turns += 1;
                goal.run_turns += 1;
                self.send_prompt_as(format!("/goal {arg}"), instructions(arg));
            }
        }
    }

    /// What `/goal` shows.
    fn goal_status(&self) -> String {
        let Some(goal) = &self.goal else {
            return "No goal set. Usage: /goal <condition> (then /goal pause, resume, clear)"
                .into();
        };
        let state = if goal.checking {
            "being checked"
        } else if goal.held {
            "paused (/goal resume to continue)"
        } else if goal.paused {
            "paused (send a message or /goal resume to continue)"
        } else if goal.retry.is_some() {
            "waiting to retry (/goal resume to retry now)"
        } else {
            "active"
        };
        let mut out = format!(
            "Goal, {state}:\n{}\n\n{} turn{} ({} of {MAX_TURNS} in this run), {}, {} tokens.",
            goal.condition,
            goal.turns,
            if goal.turns == 1 { "" } else { "s" },
            goal.run_turns,
            fmt_duration(goal.elapsed()),
            super::fmt_tokens(goal.tokens(self.goal_session_tokens())),
        );
        match &goal.last_check {
            Some(check) => out.push_str(&format!("\nLast check:\n{}", check.trim_end())),
            None => out.push_str("\nNot checked yet."),
        }
        out
    }

    /// The tokens the goal is charged for: uncached input plus output.
    fn goal_session_tokens(&self) -> u64 {
        self.input_tokens.saturating_sub(self.cached_tokens) + self.output_tokens
    }

    /// Unpauses the goal and starts its clock and token count.
    fn activate_goal(&mut self) {
        let tokens = self.goal_session_tokens();
        let Some(goal) = self.goal.as_mut() else {
            return;
        };
        if goal.paused {
            goal.run_turns = 0;
            goal.idle_turns = 0;
            goal.retries = 0;
        }
        goal.paused = false;
        goal.held = false;
        goal.retry = None;
        goal.since.get_or_insert_with(session::now);
        goal.tokens_base.get_or_insert(tokens);
    }

    /// Pauses the goal without saying so, stopping its clock and token count.
    fn pause_goal_quietly(&mut self) {
        let tokens = self.goal_session_tokens();
        let Some(goal) = self.goal.as_mut() else {
            return;
        };
        goal.paused = true;
        goal.checking = false;
        goal.retry = None;
        goal.elapsed = goal.elapsed();
        goal.since = None;
        goal.tokens = goal.tokens(tokens);
        goal.tokens_base = None;
        self.save_session();
    }

    /// Pauses the goal, with the reason shown in the transcript.
    pub(super) fn pause_goal(&mut self, reason: &str) {
        if self.goal.as_ref().is_some_and(|g| !g.paused) {
            self.pause_goal_quietly();
            self.entries.push(Entry::Info(format!(
                "Goal paused ({reason}). Send a message or /goal resume to continue, \
/goal clear to drop it."
            )));
        }
    }

    fn abort_checker(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    /// The goal as saved with the session.
    pub(super) fn saved_goal(&self) -> Option<session::Goal> {
        let goal = self.goal.as_ref()?;
        Some(session::Goal {
            condition: goal.condition.clone(),
            turns: goal.turns,
            elapsed: goal.elapsed(),
            tokens: goal.tokens(self.goal_session_tokens()),
            last_check: goal.last_check.clone(),
            notes: goal.notes.clone(),
        })
    }

    /// Brings back a resumed session's goal, paused.
    pub(super) fn restore_goal(&mut self, saved: Option<session::Goal>) {
        self.goal = saved.map(Goal::from_saved);
        if let Some(goal) = &self.goal {
            self.entries.push(Entry::Info(format!(
                "Goal restored, paused: {}\nSend a message or /goal resume to continue, \
/goal clear to drop it.",
                goal.condition
            )));
        }
    }

    /// Keeps a message the user sent while the goal is being worked on (or resumed by
    /// it), so the checker knows about it.
    pub(super) fn note_for_goal(&mut self, text: &str) {
        if let Some(goal) = self.goal.as_mut().filter(|g| !g.held) {
            goal.notes.push(text.to_string());
            if goal.notes.len() > MAX_NOTES {
                goal.notes.remove(0);
            }
        }
    }

    /// A message sent while the goal is paused (not held) or waiting to retry resumes
    /// it: returns what to add to the message, a reminder of the goal.
    pub(super) fn resume_goal_with_message(&mut self) -> Option<String> {
        let goal = self.goal.as_ref()?;
        if !(goal.paused || goal.retry.is_some()) || goal.held || goal.checking {
            return None;
        }
        let condition = goal.condition.clone();
        self.activate_goal();
        let goal = self.goal.as_mut().unwrap();
        goal.turns += 1;
        goal.run_turns += 1;
        self.entries.push(Entry::Info("Goal resumed.".into()));
        Some(reminder(&condition))
    }

    /// Called when a turn ends with nothing queued: pauses the goal or starts the checker.
    pub(super) fn after_goal_turn(&mut self, failed: bool) {
        if self
            .goal
            .as_ref()
            .is_none_or(|g| g.paused || g.retry.is_some())
        {
            return;
        }
        let start = self
            .checkpoints
            .last()
            .map_or(0, |t| t.entries_len)
            .min(self.entries.len());
        let turn = &self.entries[start..];
        if failed {
            let error = turn
                .iter()
                .rev()
                .find_map(|e| match e {
                    Entry::Error(text) => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "the turn failed".into());
            self.schedule_goal_retry(&error, false);
            return;
        }
        let report = turn
            .iter()
            .rev()
            .find_map(|e| match e {
                Entry::Assistant(text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let used_tools = turn.iter().any(|e| matches!(e, Entry::Tool { .. }));
        let goal = self.goal.as_mut().unwrap();
        goal.retries = 0;
        goal.idle_turns = if used_tools { 0 } else { goal.idle_turns + 1 };
        if verdict(&report) == Some(BLOCKED) {
            self.pause_goal("the agent needs your input");
            return;
        }
        if goal.idle_turns >= MAX_IDLE_TURNS {
            self.pause_goal(&format!(
                "{MAX_IDLE_TURNS} turns in a row without using a tool, so it looks stuck"
            ));
            return;
        }
        goal.report = report;
        self.start_goal_check();
    }

    /// Runs the checker as its own task: its steps show in the transcript and its tool
    /// calls follow the session's permission rules, with file edits denied.
    fn start_goal_check(&mut self) {
        let Some(goal) = self.goal.as_mut() else {
            return;
        };
        goal.checking = true;
        let task = checker_task(&goal.condition, &goal.notes, &goal.report);
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
            self.entries.push(Entry::Error(answer.clone()));
            self.schedule_goal_retry(&answer, true);
            return;
        }
        let answer = answer.trim_end().to_string();
        match verdict(&answer) {
            Some(MET) => {
                let tokens = self.goal_session_tokens();
                let goal = self.goal.take().unwrap();
                self.entries.push(Entry::Info(format!(
                    "✓ Goal achieved after {} turn{}, {}, {} tokens (confirmed by the \
checker): {}",
                    goal.turns,
                    if goal.turns == 1 { "" } else { "s" },
                    fmt_duration(goal.elapsed()),
                    super::fmt_tokens(goal.tokens(tokens)),
                    goal.condition,
                )));
                self.entries.push(Entry::Note(answer));
                self.save_session();
            }
            Some(NOT_MET) => {
                goal.last_check = Some(answer.clone());
                self.entries
                    .push(Entry::Info("✗ Checker: goal not met yet.".into()));
                self.entries.push(Entry::Note(answer.clone()));
                self.continue_goal(&answer);
            }
            _ => {
                goal.last_check = Some(answer.clone());
                self.entries.push(Entry::Note(answer));
                self.pause_goal("the checker gave no verdict");
            }
        }
    }

    fn continue_goal(&mut self, findings: &str) {
        let Some(goal) = self.goal.as_mut() else {
            return;
        };
        if goal.run_turns >= MAX_TURNS {
            self.pause_goal(&format!("{MAX_TURNS} turns without being achieved"));
            return;
        }
        goal.turns += 1;
        goal.run_turns += 1;
        let mut prompt = continuation(&goal.condition, goal.run_turns, findings.trim());
        let mut shown = format!(
            "Continue toward the goal (turn {} of {MAX_TURNS}).",
            goal.run_turns
        );
        // Messages typed while the checker ran go along with its findings.
        let queued = std::mem::take(&mut *self.queued.lock().unwrap());
        if !queued.is_empty() {
            shown = queued.join("\n\n");
            prompt = format!("{shown}\n\n<system-reminder>\n{prompt}\n</system-reminder>");
        }
        self.send_prompt_as(shown, prompt);
    }

    /// Starts a goal turn that isn't answering the checker.
    fn start_goal_turn(&mut self, why: &str) {
        let Some(goal) = self.goal.as_mut() else {
            return;
        };
        goal.turns += 1;
        goal.run_turns += 1;
        let prompt = resumption(&goal.condition, goal.run_turns, why);
        let shown = format!(
            "Continue toward the goal (turn {} of {MAX_TURNS}).",
            goal.run_turns
        );
        self.send_prompt_as(shown, prompt);
    }

    /// After a failed turn or check, tries again later, or pauses once retries run out.
    fn schedule_goal_retry(&mut self, error: &str, check: bool) {
        let Some(goal) = self.goal.as_mut() else {
            return;
        };
        let Some(&delay) = RETRY_DELAYS.get(goal.retries) else {
            let what = if check { "check" } else { "turn" };
            self.pause_goal(&format!(
                "the {what} failed {} times in a row",
                RETRY_DELAYS.len() + 1
            ));
            return;
        };
        goal.retries += 1;
        goal.retry_ids += 1;
        let id = goal.retry_ids;
        goal.retry = Some(Retry {
            id,
            at: session::now() + delay,
            check,
        });
        let error = error.lines().next().unwrap_or("").trim();
        self.entries.push(Entry::Info(format!(
            "Goal still active: the {} failed ({error}). Retrying in {} ({}/{}); send a \
message or /goal resume to retry now, /goal pause to stop.",
            if check { "check" } else { "turn" },
            fmt_duration(delay),
            goal.retries,
            RETRY_DELAYS.len(),
        )));
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            let _ = tx.send(AppEvent::GoalRetry(id));
        });
    }

    /// A scheduled retry is due: runs it unless it was overtaken.
    pub(super) fn on_goal_retry(&mut self, id: u64) {
        let Some(retry) = self.goal.as_ref().and_then(|g| g.retry) else {
            return;
        };
        if retry.id != id || self.is_running() {
            return;
        }
        self.goal.as_mut().unwrap().retry = None;
        if retry.check {
            self.retry_goal_check();
        } else {
            self.start_goal_turn("The last turn ended with an error; it has been retried.");
        }
    }

    fn retry_goal_check(&mut self) {
        self.entries
            .push(Entry::Info("Retrying the goal check.".into()));
        self.start_goal_check();
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

    #[test]
    fn durations_read_naturally() {
        assert_eq!(fmt_duration(45), "45s");
        assert_eq!(fmt_duration(720), "12m");
        assert_eq!(fmt_duration(7200), "2h");
        assert_eq!(fmt_duration(7500), "2h 5m");
    }
}
