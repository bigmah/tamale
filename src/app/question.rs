//! The popup showing the model's `ask_user` questions: pick options, or type an answer.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use tokio::sync::oneshot;
use unicode_width::UnicodeWidthStr;

use super::App;
use crate::tools::ask::{Answer, Question, Reply};

/// Keys typed this soon after the popup opens are ignored, so text being typed into the
/// prompt can't answer it by accident.
const KEY_GUARD: Duration = Duration::from_millis(300);
const OTHER_LABEL: &str = "Type something else…";
const OTHER_PREFIX: &str = "Other: ";

pub(super) struct Prompt {
    pub(super) description: String,
    pub(super) questions: Vec<Question>,
    pub(super) answers: Vec<Answer>,
    /// The highlighted row of each question: its options, then "type something else".
    rows: Vec<usize>,
    /// The question shown; `questions.len()` is the review page.
    pub(super) current: usize,
    /// Whether keys go to the current question's typed answer.
    pub(super) typing: bool,
    reply: Option<oneshot::Sender<Reply>>,
    pub(super) shown: Instant,
}

impl Prompt {
    pub(super) fn new(
        description: String,
        questions: Vec<Question>,
        reply: oneshot::Sender<Reply>,
    ) -> Self {
        let n = questions.len();
        Self {
            description,
            questions,
            answers: vec![Answer::default(); n],
            rows: vec![0; n],
            current: 0,
            typing: false,
            reply: Some(reply),
            shown: Instant::now(),
        }
    }

    fn reviewing(&self) -> bool {
        self.current >= self.questions.len()
    }

    /// The row index of "type something else" for question `i`.
    fn other_row(&self, i: usize) -> usize {
        self.questions[i].options.len()
    }

    fn other_text(&mut self) -> &mut String {
        let i = self.current;
        self.answers[i].other.get_or_insert_with(String::new)
    }

    /// Stops typing; an empty typed answer counts as none.
    fn stop_typing(&mut self) {
        self.typing = false;
        if let Some(answer) = self.answers.get_mut(self.current)
            && answer.other.as_deref().is_some_and(|o| o.trim().is_empty())
        {
            answer.other = None;
        }
    }

    fn start_typing(&mut self) {
        let i = self.current;
        self.rows[i] = self.other_row(i);
        self.other_text();
        self.typing = true;
    }

    /// Picks (single choice) or toggles (multiple choice) row `row` of the current question.
    /// Returns whether a single choice was made, which moves on.
    fn choose(&mut self, row: usize) -> bool {
        let i = self.current;
        self.rows[i] = row;
        if row == self.other_row(i) {
            self.start_typing();
            return false;
        }
        let label = self.questions[i].options[row].label.clone();
        let answer = &mut self.answers[i];
        if self.questions[i].multi_select {
            if let Some(at) = answer.chosen.iter().position(|l| *l == label) {
                answer.chosen.remove(at);
            } else {
                answer.chosen.push(label);
                // Keep the options' order.
                let order =
                    |l: &String| self.questions[i].options.iter().position(|o| o.label == *l);
                answer.chosen.sort_by_key(order);
            }
            false
        } else {
            answer.chosen = vec![label];
            answer.other = None;
            true
        }
    }

    /// Moves to question `to`, or the review page past the last one.
    fn go(&mut self, to: usize) {
        self.stop_typing();
        self.current = to.min(self.questions.len());
    }

    /// The first question without an answer.
    fn unanswered(&self) -> Option<usize> {
        self.answers.iter().position(Answer::is_empty)
    }

    /// After answering the current question: the next one, the review page, or (a single
    /// question) done. Returns whether the answers are ready to send.
    fn advance(&mut self) -> bool {
        self.stop_typing();
        if self.questions.len() == 1 {
            return !self.answers[0].is_empty();
        }
        self.current += 1;
        false
    }
}

impl App {
    pub(super) fn on_question_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(prompt) = &mut self.question else {
            return;
        };
        let ctrl_c = ctrl && key.code == KeyCode::Char('c');
        if ctrl_c && self.detachable {
            // The questions wait for whoever comes back.
            self.detach_requested = true;
            return;
        }
        if key.code == KeyCode::Esc && !prompt.typing || ctrl_c {
            self.interrupt();
            return;
        }
        if prompt.shown.elapsed() < KEY_GUARD {
            return;
        }
        let last = prompt.questions.len();
        if prompt.typing {
            let send = match key.code {
                KeyCode::Esc => {
                    prompt.stop_typing();
                    false
                }
                KeyCode::Enter => {
                    let i = prompt.current;
                    if !prompt.questions[i].multi_select
                        && prompt.answers[i]
                            .other
                            .as_deref()
                            .is_some_and(|o| !o.trim().is_empty())
                    {
                        prompt.answers[i].chosen.clear();
                    }
                    prompt.advance()
                }
                KeyCode::Backspace => {
                    prompt.other_text().pop();
                    false
                }
                KeyCode::Up => {
                    prompt.stop_typing();
                    let i = prompt.current;
                    prompt.rows[i] = prompt.rows[i].saturating_sub(1);
                    false
                }
                KeyCode::Tab => {
                    prompt.go(prompt.current + 1);
                    false
                }
                KeyCode::BackTab => {
                    prompt.go(prompt.current.saturating_sub(1));
                    false
                }
                KeyCode::Char(c) if !ctrl => {
                    prompt.other_text().push(c);
                    false
                }
                _ => false,
            };
            if send {
                self.send_answers();
            }
            return;
        }
        if prompt.reviewing() {
            match key.code {
                KeyCode::Enter => match prompt.unanswered() {
                    Some(i) => prompt.go(i),
                    None => self.send_answers(),
                },
                KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab | KeyCode::Backspace => {
                    prompt.go(last - 1)
                }
                KeyCode::Char(c @ '1'..='9') => {
                    let i = c as usize - '1' as usize;
                    if i < last {
                        prompt.go(i);
                    }
                }
                _ => {}
            }
            return;
        }
        let i = prompt.current;
        let other = prompt.other_row(i);
        let multi = prompt.questions[i].multi_select;
        let send = match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                prompt.rows[i] = prompt.rows[i].saturating_sub(1);
                false
            }
            KeyCode::Down | KeyCode::Char('j') => {
                prompt.rows[i] = (prompt.rows[i] + 1).min(other);
                false
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                prompt.go(i.saturating_sub(1));
                false
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                prompt.go(i + 1);
                false
            }
            KeyCode::Char(c @ '1'..='9') => {
                let row = c as usize - '1' as usize;
                row <= other && prompt.choose(row) && prompt.advance()
            }
            KeyCode::Char(' ') => {
                let row = prompt.rows[i];
                prompt.choose(row) && prompt.advance()
            }
            KeyCode::Enter => {
                let row = prompt.rows[i];
                if row == other {
                    prompt.start_typing();
                    false
                } else if multi {
                    // Enter on a multiple-choice question confirms it, taking the highlighted
                    // option if none is picked yet.
                    if prompt.answers[i].is_empty() {
                        prompt.choose(row);
                    }
                    prompt.advance()
                } else {
                    prompt.choose(row) && prompt.advance()
                }
            }
            _ => false,
        };
        if send {
            self.send_answers();
        }
    }

    pub(super) fn on_question_paste(&mut self, text: &str) {
        if let Some(prompt) = &mut self.question
            && prompt.typing
        {
            prompt.other_text().push_str(&text.replace('\n', " "));
        }
    }

    fn send_answers(&mut self) {
        if let Some(mut prompt) = self.question.take()
            && let Some(reply) = prompt.reply.take()
        {
            let _ = reply.send(Reply::Answers(prompt.answers));
        }
    }

    /// Answers the questions from elsewhere (the web UI or the iPhone app), if there's one
    /// answer per question and each says something.
    pub(super) fn answer_questions(&mut self, answers: Vec<Answer>) {
        let Some(prompt) = &self.question else {
            return;
        };
        if answers.len() != prompt.questions.len() || answers.iter().any(Answer::is_empty) {
            return;
        }
        if let Some(mut prompt) = self.question.take()
            && let Some(reply) = prompt.reply.take()
        {
            let _ = reply.send(Reply::Answers(answers));
        }
    }

    /// Answers the questions with a message, e.g. sent from the web UI.
    pub(super) fn answer_questions_with(&mut self, text: String) {
        if let Some(mut prompt) = self.question.take()
            && let Some(reply) = prompt.reply.take()
        {
            let _ = reply.send(Reply::Text(text));
        }
    }

    pub(super) fn draw_question(&self, frame: &mut Frame, area: Rect) {
        let Some(prompt) = &self.question else {
            return;
        };
        let width = area.width.saturating_sub(4).max(20).min(area.width);
        let dim = Style::new().fg(Color::DarkGray);
        let accent = Style::new().fg(Color::Cyan);
        let count = prompt.questions.len();
        let mut lines = Vec::new();

        if count > 1 {
            let mut tabs = vec![Span::from(" ")];
            for (i, q) in prompt.questions.iter().enumerate() {
                let label = if q.header.is_empty() {
                    format!("Q{}", i + 1)
                } else {
                    q.header.clone()
                };
                let mark = if prompt.answers[i].is_empty() {
                    "□"
                } else {
                    "✔"
                };
                let text = format!(" {mark} {label} ");
                tabs.push(if i == prompt.current {
                    Span::styled(text, Style::new().fg(Color::Black).bg(Color::Cyan).bold())
                } else if prompt.answers[i].is_empty() {
                    Span::from(text)
                } else {
                    Span::styled(text, Style::new().fg(Color::Green))
                });
                tabs.push(Span::from(" "));
            }
            let submit = " Submit ";
            tabs.push(if prompt.reviewing() {
                Span::styled(submit, Style::new().fg(Color::Black).bg(Color::Cyan).bold())
            } else {
                Span::styled(submit, dim)
            });
            lines.push(Line::from(tabs));
            lines.push(Line::default());
        }

        let mut cursor = None;
        if prompt.reviewing() {
            lines.push(Line::from("Review your answers".bold()));
            lines.push(Line::default());
            for (i, (q, a)) in prompt.questions.iter().zip(&prompt.answers).enumerate() {
                lines.push(Line::from(vec![
                    Span::styled(format!(" {}. ", i + 1), dim),
                    Span::from(q.question.as_str()),
                ]));
                let mut parts = a.chosen.clone();
                if let Some(o) = a.other.as_deref().filter(|o| !o.trim().is_empty()) {
                    parts.push(format!("“{}”", o.trim()));
                }
                lines.push(if parts.is_empty() {
                    Line::from(Span::styled(
                        "    → not answered yet",
                        Style::new().fg(Color::Yellow),
                    ))
                } else {
                    Line::from(vec![
                        Span::styled("    → ", dim),
                        Span::styled(parts.join(", "), Style::new().fg(Color::Green)),
                    ])
                });
            }
            lines.push(Line::default());
            let label = match prompt.unanswered() {
                Some(_) => "› Answer the rest",
                None => "› Submit answers",
            };
            lines.push(Line::from(Span::styled(label, accent.bold())));
        } else {
            let i = prompt.current;
            let q = &prompt.questions[i];
            let answer = &prompt.answers[i];
            lines.push(Line::from(q.question.as_str().bold()));
            if q.multi_select {
                lines.push(Line::from(Span::styled("Pick any that apply.", dim)));
            }
            lines.push(Line::default());
            let selected = prompt.rows[i];
            for (row, option) in q.options.iter().enumerate() {
                let on = answer.chosen.contains(&option.label);
                let here = row == selected;
                let mut spans = vec![Span::styled(if here { "› " } else { "  " }, accent)];
                if q.multi_select {
                    spans.push(Span::from(if on { "[x] " } else { "[ ] " }));
                }
                let text = format!("{}. {}", row + 1, option.label);
                spans.push(if here {
                    Span::styled(text, accent.bold())
                } else if on {
                    Span::styled(text, Style::new().fg(Color::Green))
                } else {
                    Span::from(text)
                });
                if on && !q.multi_select {
                    spans.push(Span::styled(" ✔", Style::new().fg(Color::Green)));
                }
                lines.push(Line::from(spans));
                if !option.description.is_empty() {
                    let indent = if q.multi_select { "         " } else { "     " };
                    lines.push(Line::from(vec![
                        Span::from(indent),
                        Span::styled(option.description.as_str(), dim),
                    ]));
                }
            }
            let row = q.options.len();
            let here = row == selected;
            let mut spans = vec![Span::styled(if here { "› " } else { "  " }, accent)];
            if q.multi_select {
                let on = answer
                    .other
                    .as_deref()
                    .is_some_and(|o| !o.trim().is_empty());
                spans.push(Span::from(if on { "[x] " } else { "[ ] " }));
            }
            let number = format!("{}. ", row + 1);
            let style = if here { accent.bold() } else { Style::new() };
            match (&answer.other, prompt.typing) {
                (Some(text), _) if prompt.typing || !text.is_empty() => {
                    let prefix_width = spans.iter().map(|s| s.content.width()).sum::<usize>()
                        + number.width()
                        + OTHER_PREFIX.width();
                    if prompt.typing {
                        cursor = Some((lines.len(), prefix_width + text.width()));
                    }
                    spans.push(Span::styled(number, style));
                    spans.push(Span::styled(OTHER_PREFIX, style));
                    spans.push(Span::from(text.as_str()));
                }
                _ => {
                    spans.push(Span::styled(number, style));
                    spans.push(Span::styled(OTHER_LABEL, if here { style } else { dim }));
                }
            }
            lines.push(Line::from(spans));
        }

        let hint = if prompt.typing {
            " type your answer · enter confirm · esc back "
        } else if prompt.reviewing() {
            " enter submit · ←/1–9 change an answer · esc interrupt "
        } else if prompt.questions[prompt.current].multi_select {
            " ↑/↓ move · space toggle · enter confirm · ←/→ questions · esc interrupt "
        } else if count > 1 {
            " ↑/↓ move · enter select · ←/→ questions · esc interrupt "
        } else {
            " ↑/↓ move · enter select · esc interrupt "
        };
        let title = if prompt.description.is_empty() {
            " Question ".to_string()
        } else {
            format!(" {} ", prompt.description)
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(accent)
            .title(title.bold())
            .title_bottom(Line::from(hint).right_aligned());
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        let height = (paragraph.line_count(width.saturating_sub(2)) as u16 + 2).min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.bottom().saturating_sub(height),
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        frame.render_widget(paragraph.block(block), popup);
        if let Some((row, column)) = cursor {
            // Exact when the earlier lines didn't wrap, which is the common case.
            let x = popup.x + 1 + column as u16;
            let y = popup.y + 1 + row as u16;
            if x < popup.right() && y < popup.bottom().saturating_sub(1) {
                frame.set_cursor_position(Position::new(x, y));
            }
        }
    }
}
