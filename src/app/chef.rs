//! The tamale chef in the app (see [`crate::chef`]): the kitchen it shows beside the chat
//! and in the web UI, `/chef` to go to it, and every session's side of a chef's side
//! questions.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use rig_core::completion::{AssistantContent, Message};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

use super::view::truncate_chars;
use super::{App, AppEvent};
use crate::agent::{self, AgentEvent, TurnCtx};
use crate::backend;
use crate::chef;
use crate::permissions::Approval;
use crate::session::Entry;
use crate::web::{Kitchen, KitchenSession};

/// How often the kitchen is looked at again.
const KITCHEN_REFRESH: Duration = Duration::from_secs(8);
/// The kitchen panel's width, and the narrowest terminal that has room for it.
const PANEL_WIDTH: u16 = 36;
const PANEL_MIN_TERMINAL: u16 = 100;
/// The chef's colour.
pub(super) const CHEF: Color = Color::Yellow;

/// What a chef knows of its kitchen.
#[derive(Default)]
pub(super) struct ChefState {
    kitchen: Kitchen,
    fetching: bool,
}

impl App {
    /// Makes this session a tamale chef. Its kitchen is looked at from now on.
    pub fn set_chef(&mut self) {
        self.chef = Some(ChefState {
            kitchen: Kitchen {
                loading: true,
                ..Kitchen::default()
            },
            fetching: false,
        });
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(KITCHEN_REFRESH);
            loop {
                every.tick().await;
                if tx.send(AppEvent::KitchenTick).is_err() {
                    return;
                }
            }
        });
    }

    pub fn is_chef(&self) -> bool {
        self.chef.is_some()
    }

    /// Whether the footer offers the chef: in a session that can switch to it.
    pub(super) fn shows_chef_tab(&self) -> bool {
        cfg!(unix) && self.chef.is_none() && self.detachable
    }

    /// What the chef's tools reach sessions with, as the settings are now (a key set with
    /// `/remote <key>` counts from the next turn).
    pub(super) fn kitchen_tools(&self) -> Option<Arc<chef::Kitchen>> {
        self.chef
            .as_ref()
            .map(|_| chef::Kitchen::new(self.settings.hub.clone()))
    }

    /// The kitchen as the web UI's panel shows it.
    pub(super) fn kitchen_view(&self) -> Option<Kitchen> {
        self.chef.as_ref().map(|chef| chef.kitchen.clone())
    }

    /// Looks at the hub's list again, unless a look is under way.
    pub fn refresh_kitchen(&mut self) {
        let Some(state) = &mut self.chef else {
            return;
        };
        if let Err(problem) = self.settings.hub.key_bytes() {
            state.kitchen = Kitchen {
                problem: Some(problem),
                ..Kitchen::default()
            };
            return;
        }
        if state.fetching {
            return;
        }
        state.fetching = true;
        #[cfg(unix)]
        {
            let hub = self.settings.hub.clone();
            let own = crate::web::sid().to_string();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let result = crate::remote::sessions(&hub).await.map(|sessions| {
                    sessions
                        .into_iter()
                        .filter(|s| s.sid != own)
                        .map(|s| KitchenSession {
                            id: chef::short_id(&s.sid),
                            state: if !s.online {
                                "offline"
                            } else if s.approval {
                                "approval"
                            } else if s.busy {
                                "working"
                            } else {
                                "idle"
                            },
                            host: s.host,
                            cwd: s.cwd,
                            title: s.title,
                            model: s.model,
                            viewers: s.viewers,
                        })
                        .collect()
                });
                let _ = tx.send(AppEvent::Kitchen(result));
            });
        }
    }

    pub(super) fn on_kitchen(&mut self, result: Result<Vec<KitchenSession>, String>) {
        let Some(state) = &mut self.chef else {
            return;
        };
        state.fetching = false;
        state.kitchen = match result {
            Ok(mut sessions) => {
                // What needs the user first, then what's busy, then the rest.
                let rank = |s: &KitchenSession| match s.state {
                    "approval" => 0,
                    "working" => 1,
                    "idle" => 2,
                    _ => 3,
                };
                sessions.sort_by_key(rank);
                Kitchen {
                    sessions,
                    problem: None,
                    loading: false,
                }
            }
            // A hiccup keeps what was there, saying why it may be stale.
            Err(problem) => Kitchen {
                problem: Some(problem),
                loading: false,
                ..std::mem::take(&mut state.kitchen)
            },
        };
    }

    /// `/chef`: goes to this machine's chef, starting one if there isn't one.
    pub(super) fn chef_command(&mut self) {
        if self.is_chef() {
            self.entries.push(Entry::Info(
                "This is the chef. Ask it about your sessions; /sessions goes back to one.".into(),
            ));
            return;
        }
        #[cfg(unix)]
        {
            if !self.detachable {
                self.entries.push(Entry::Error(
                    "This session runs in this terminal (--foreground), so it can't switch. \
                     Leave it and run: tamale chef"
                        .into(),
                ));
                return;
            }
            let running = crate::background::list()
                .into_iter()
                .find(|s| s.chef && s.id != std::process::id());
            if let Some(chef) = running {
                self.switch_request = Some(chef.id);
                return;
            }
            if self.chef_starting {
                return;
            }
            self.chef_starting = true;
            self.entries
                .push(Entry::Info("🫔 Starting the tamale chef…".into()));
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let started = crate::background::client::start_server(["chef"])
                    .await
                    .map_err(|e| format!("Couldn't start the chef: {e:#}"));
                let _ = tx.send(AppEvent::ChefStarted(started));
            });
        }
        #[cfg(not(unix))]
        self.entries.push(Entry::Error(
            "The chef needs background sessions, which this platform doesn't have.".into(),
        ));
    }

    #[cfg(unix)]
    pub(super) fn on_chef_started(&mut self, started: Result<u32, String>) {
        self.chef_starting = false;
        match started {
            Ok(id) => self.switch_request = Some(id),
            Err(e) => self.entries.push(Entry::Error(e)),
        }
    }

    /// Answers a chef's side question from this session's context, without adding to its
    /// conversation: a separate run over a copy of the history, which may read but not change
    /// anything. The transcript notes that it was asked. Resolves to the answer and whether
    /// it's an error.
    pub fn side_question(&mut self, question: String) -> BoxFuture<'static, (String, bool)> {
        self.entries.push(Entry::Info(format!(
            "{}{}",
            chef::ASKED_PREFIX,
            truncate_chars(question.lines().next().unwrap_or(""), 200)
        )));
        if !self.backend.signed_in() || matches!(self.backend, backend::Backend::Unset) {
            return async {
                (
                    "This session has no backend to answer with.".to_string(),
                    true,
                )
            }
            .boxed();
        }
        let mut history = self.history.clone();
        settle(&mut history);
        let prompt = chef::side_question_prompt(&question);
        agent::push_user_text(&mut history, prompt.clone());
        let turn = backend::Turn {
            model: self.settings.model.name.clone(),
            history,
            prompt,
            thread: None,
            config: self.settings.turn_config(self.context_tokens),
            workspace: self.workspace.clone(),
        };
        let policy = self.permissions.lock().unwrap().look_only();
        let ctx = TurnCtx::new(Arc::new(Mutex::new(policy)), None);
        let backend = self.backend.clone();
        async move {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let run = backend.run(turn, ctx, tx);
            let mut last = None;
            let mut errors = Vec::new();
            let collect = async {
                while let Some(event) = rx.recv().await {
                    match event {
                        AgentEvent::History(history) => last = Some(history),
                        AgentEvent::Error(e) => errors.push(e),
                        AgentEvent::Approve { reply, .. } => {
                            let _ = reply.send(Approval::Deny {
                                reason: Some(
                                    "A side question can only look; answer with what you can \
                                     read."
                                        .into(),
                                ),
                            });
                        }
                        _ => {}
                    }
                }
            };
            tokio::join!(run, collect);
            let answer = last.as_deref().map(final_text).unwrap_or_default();
            match (answer.is_empty(), errors.is_empty()) {
                (false, _) => (answer, false),
                (true, false) => (errors.join("; "), true),
                (true, true) => ("It finished without an answer.".into(), true),
            }
        }
        .boxed()
    }

    /// The chef's welcome, above an empty conversation.
    pub(super) fn chef_banner_height(&self, area: Rect) -> u16 {
        let started = self.entries.iter().any(|e| matches!(e, Entry::User(_)));
        if self.chef.is_none() || started || area.height < 22 {
            0
        } else {
            BANNER_HEIGHT.min(area.height / 2 + 4)
        }
    }

    pub(super) fn draw_chef_banner(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let hat = Style::new().fg(Color::White);
        let band = Style::new().fg(CHEF);
        let dim = Style::new().fg(Color::DarkGray);
        let mut lines = vec![
            Line::default(),
            Line::from(Span::styled("▄▄███▄▄", hat)),
            Line::from(Span::styled("▄█████████▄", hat)),
            Line::from(Span::styled("▀███████▀", hat)),
            Line::from(Span::styled("▐▀▀▀▀▀▀▀▌", band)),
            Line::from(Span::styled("▀▀▀▀▀▀▀", band)),
            Line::default(),
            Line::from(vec![
                Span::styled("t a m a l e", Style::new().fg(Color::White).bold()),
                Span::raw("   "),
                Span::styled("c h e f", Style::new().fg(CHEF).bold()),
            ]),
            Line::from(Span::styled(
                "Ask about any of your sessions, on any of your machines.",
                dim,
            )),
            Line::default(),
        ];
        let kitchen = self.chef.as_ref().map(|c| &c.kitchen);
        lines.push(match kitchen {
            Some(k) if k.loading => Line::from(Span::styled("Looking around the kitchen…", dim)),
            Some(Kitchen {
                problem: Some(problem),
                sessions,
                ..
            }) if sessions.is_empty() => {
                Line::from(Span::styled(problem.clone(), Style::new().fg(Color::Red)))
            }
            Some(k) => Line::from(kitchen_summary(&k.sessions)),
            None => Line::default(),
        });
        lines.push(Line::default());
        // One block, centered as a whole, so the suggestions line up.
        let widest = IDEAS.iter().map(|idea| idea.width() + 2).max().unwrap_or(0);
        for (i, idea) in IDEAS.iter().enumerate() {
            let idea = format!("“{idea}”");
            let pad = widest - idea.width();
            lines.push(Line::from(vec![
                Span::styled(if i == 0 { "try  " } else { "     " }, dim),
                Span::styled(idea, Style::new().fg(Color::Gray).italic()),
                Span::raw(" ".repeat(pad)),
            ]));
        }
        let lines: Vec<Line> = lines.into_iter().map(Line::centered).collect();
        frame.render_widget(Paragraph::new(lines), area);
    }

    /// Splits the chat's area for the kitchen panel, on a terminal wide enough for it.
    pub(super) fn chef_panel_split(&self, area: Rect, full_width: u16) -> (Rect, Option<Rect>) {
        if self.chef.is_none() || full_width < PANEL_MIN_TERMINAL {
            return (area, None);
        }
        let [chat, gap, panel] = Layout::horizontal([
            Constraint::Min(40),
            Constraint::Length(1),
            Constraint::Length(PANEL_WIDTH),
        ])
        .areas(area);
        let _ = gap;
        (chat, Some(panel))
    }

    pub(super) fn draw_kitchen(&self, frame: &mut Frame, area: Rect) {
        let Some(state) = &self.chef else {
            return;
        };
        let kitchen = &state.kitchen;
        let footer = if kitchen.loading {
            " looking… ".to_string()
        } else if kitchen.problem.is_some() && !kitchen.sessions.is_empty() {
            " ⚠ may be stale ".to_string()
        } else {
            let n = kitchen.sessions.len();
            format!(" {n} session{} ", if n == 1 { "" } else { "s" })
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::DarkGray))
            .title(Line::from(vec![
                Span::raw(" 🫔 "),
                Span::styled("Kitchen ", Style::new().fg(CHEF).bold()),
            ]))
            .title_bottom(
                Line::from(Span::styled(footer, Style::new().fg(Color::DarkGray))).right_aligned(),
            );
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let width = inner.width.saturating_sub(1) as usize;
        let dim = Style::new().fg(Color::DarkGray);
        let mut lines: Vec<Line> = Vec::new();
        if kitchen.sessions.is_empty() {
            let text = match (&kitchen.problem, kitchen.loading) {
                (_, true) => "Looking around…".to_string(),
                (Some(problem), _) => problem.clone(),
                (None, _) => "No other sessions on the hub. Turn on /remote in a session to \
                              bring it into the kitchen."
                    .to_string(),
            };
            lines.push(Line::default());
            for row in textwrap(&text, width) {
                lines.push(Line::from(Span::styled(format!(" {row}"), dim)));
            }
        }
        for session in &kitchen.sessions {
            let (icon, label, color) = match session.state {
                "working" => ("◉", "working", Color::Yellow),
                "approval" => ("◆", "needs you", Color::LightRed),
                "idle" => ("●", "idle", Color::Green),
                _ => ("○", "offline", Color::DarkGray),
            };
            let offline = session.state == "offline";
            let host_style = if offline {
                dim
            } else {
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD)
            };
            let head_left = format!(" {icon} ");
            let host = truncate_chars(
                if session.host.is_empty() {
                    "?"
                } else {
                    &session.host
                },
                width.saturating_sub(head_left.width() + session.id.width() + 2),
            );
            let pad = width.saturating_sub(head_left.width() + host.width() + session.id.width());
            lines.push(Line::from(vec![
                Span::styled(head_left, Style::new().fg(color)),
                Span::styled(host, host_style),
                Span::raw(" ".repeat(pad)),
                Span::styled(session.id.clone(), dim),
            ]));
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    truncate_start(&session.cwd, width.saturating_sub(3)),
                    Style::new().fg(if offline {
                        Color::DarkGray
                    } else {
                        Color::Cyan
                    }),
                ),
            ]));
            let title = if session.title.is_empty() {
                "nothing asked yet".to_string()
            } else {
                session.title.clone()
            };
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    truncate_chars(&title, width.saturating_sub(3)),
                    Style::new().fg(Color::Gray).italic(),
                ),
            ]));
            let mut meta = vec![
                Span::raw("   "),
                Span::styled(label, Style::new().fg(color)),
            ];
            if session.viewers > 0 {
                meta.push(Span::styled(format!(" · {} viewing", session.viewers), dim));
            }
            lines.push(Line::from(meta));
            lines.push(Line::default());
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// The input box's title in a chef.
    pub(super) fn chef_input_title(&self) -> Option<Line<'static>> {
        self.chef.as_ref().map(|_| {
            Line::from(vec![
                Span::raw(" 🫔 "),
                Span::styled("chef ", Style::new().fg(CHEF).bold()),
            ])
        })
    }
}

const BANNER_HEIGHT: u16 = 18;

/// What the welcome suggests asking.
const IDEAS: [&str; 3] = [
    "What is everyone working on?",
    "Which sessions need me right now?",
    "Ask all sessions whether their tests pass",
];

fn kitchen_summary(sessions: &[KitchenSession]) -> Vec<Span<'static>> {
    let count = |state| sessions.iter().filter(|s| s.state == state).count();
    let n = sessions.len();
    if n == 0 {
        return vec![Span::styled(
            "No other sessions on the hub yet.",
            Style::new().fg(Color::DarkGray),
        )];
    }
    let mut spans = vec![Span::styled(
        format!("{n} session{}", if n == 1 { "" } else { "s" }),
        Style::new().fg(Color::White).bold(),
    )];
    for (state, label, color) in [
        ("working", "working", Color::Yellow),
        ("approval", "need you", Color::LightRed),
        ("idle", "idle", Color::Green),
        ("offline", "offline", Color::DarkGray),
    ] {
        let k = count(state);
        if k > 0 {
            spans.push(Span::styled("  ·  ", Style::new().fg(Color::DarkGray)));
            spans.push(Span::styled(format!("{k} {label}"), Style::new().fg(color)));
        }
    }
    spans
}

/// The end of `s`, at most `max` columns, with an ellipsis if cut.
fn truncate_start(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out: Vec<char> = Vec::new();
    let mut width = 1;
    for c in s.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if width + w > max {
            break;
        }
        width += w;
        out.push(c);
    }
    out.reverse();
    format!("…{}", out.into_iter().collect::<String>())
}

/// `text` in rows of at most `width` columns, broken at spaces.
fn textwrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        if !row.is_empty() && row.width() + 1 + word.width() > width {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(word);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// Drops a tool call still waiting for its result from the end of `history`, so a copy
/// taken mid-turn is a conversation a model can continue.
fn settle(history: &mut Vec<Message>) {
    while let Some(Message::Assistant { content, .. }) = history.last() {
        if content
            .iter()
            .any(|c| matches!(c, AssistantContent::ToolCall(_)))
        {
            history.pop();
        } else {
            break;
        }
    }
}

/// The text of the last reply in `history`.
fn final_text(history: &[Message]) -> String {
    match history.last() {
        Some(Message::Assistant { content, .. }) => content
            .iter()
            .filter_map(|c| match c {
                AssistantContent::Text(t) => Some(t.text.trim()),
                _ => None,
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_taken_mid_turn_loses_its_unanswered_tool_call() {
        let mut history = vec![
            Message::user("hi"),
            Message::Assistant {
                id: None,
                content: vec![AssistantContent::text("Looking.")],
            },
            Message::Assistant {
                id: None,
                content: vec![AssistantContent::tool_call(
                    "t1",
                    "bash",
                    serde_json::json!({}),
                )],
            },
        ];
        settle(&mut history);
        assert_eq!(history.len(), 2);
        assert_eq!(final_text(&history), "Looking.");
    }

    #[test]
    fn text_wraps_at_spaces_and_paths_keep_their_end() {
        assert_eq!(textwrap("a bb ccc dddd", 8), vec!["a bb ccc", "dddd"]);
        assert_eq!(truncate_start("~/dev/projects/tamale", 10), "…ts/tamale");
    }
}
